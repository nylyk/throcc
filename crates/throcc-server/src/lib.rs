#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context, Result};
use throcc_proto::Fingerprint;
use tracing::Instrument as _;

use crate::database::Database;
use crate::invite::RedemptionLimiter;
use crate::rooms::Registry;

pub mod auth;
pub mod bootstrap;
pub mod control;
pub mod database;
pub mod identity;
pub mod invite;
pub mod perms;
pub mod rooms;
pub mod session;
pub mod sfu;
pub mod tls;

const PRUNE_INTERVAL: Duration = Duration::from_mins(60);

pub struct State {
    pub database: Database,
    redemptions: Mutex<RedemptionLimiter>,
    rooms: Mutex<Registry>,
}

impl State {
    pub fn new(database: Database) -> Self {
        Self {
            database,
            redemptions: Mutex::new(RedemptionLimiter::default()),
            rooms: Mutex::new(Registry::default()),
        }
    }

    pub fn redemptions(&self) -> MutexGuard<'_, RedemptionLimiter> {
        self.redemptions.lock().expect("redemption mutex poisoned")
    }

    pub fn rooms(&self) -> MutexGuard<'_, Registry> {
        self.rooms.lock().expect("room registry mutex poisoned")
    }
}

pub struct Server {
    endpoint: quinn::Endpoint,
    fingerprint: Fingerprint,
    bootstrap_invite: Option<String>,
    state: Arc<State>,
}

impl Server {
    pub fn bind(data_dir: &Path, listen: SocketAddr) -> Result<Self> {
        let identity = identity::load_or_create(data_dir)?;
        let state = Arc::new(State::new(Database::open(data_dir)?));
        let bootstrap_invite = bootstrap::ensure_invite(&state)?;

        let endpoint = quinn::Endpoint::server(tls::server_config(&identity)?, listen)
            .with_context(|| format!("binding UDP {listen}"))?;

        Ok(Self {
            endpoint,
            fingerprint: identity.fingerprint,
            bootstrap_invite,
            state,
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.endpoint
            .local_addr()
            .context("reading the local address")
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }

    /// The code enrolling the first admin, present only while nobody is enrolled.
    pub fn bootstrap_invite(&self) -> Option<&str> {
        self.bootstrap_invite.as_deref()
    }

    pub async fn run(self) -> Result<()> {
        tokio::spawn(prune_invites(self.state.clone()));

        while let Some(incoming) = self.endpoint.accept().await {
            let state = self.state.clone();
            tokio::spawn(async move {
                match incoming.await {
                    Ok(connection) => {
                        let span = tracing::info_span!("connection", id = connection.stable_id());
                        session::serve(connection, state).instrument(span).await
                    }
                    Err(e) => tracing::debug!(error = %e, "handshake failed"),
                }
            });
        }
        Ok(())
    }
}

async fn prune_invites(state: Arc<State>) {
    let mut ticker = tokio::time::interval(PRUNE_INTERVAL);
    loop {
        ticker.tick().await;
        match state.database.prune_invites() {
            Ok(0) => {}
            Ok(pruned) => tracing::info!(pruned, "pruned spent invites"),
            Err(e) => tracing::warn!(error = ?e, "could not prune invites"),
        }
    }
}
