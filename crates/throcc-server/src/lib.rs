#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result};
use throcc_proto::Fingerprint;
use tracing::Instrument as _;

use crate::database::Database;
use crate::invite::RedemptionLimiter;

pub mod auth;
pub mod control;
pub mod database;
pub mod identity;
pub mod invite;
pub mod permissions;
pub mod session;
pub mod tls;

pub struct State {
    pub database: Database,
    redemption_limiter: Mutex<RedemptionLimiter>,
}

impl State {
    pub fn new(database: Database) -> Self {
        Self {
            database,
            redemption_limiter: Mutex::new(RedemptionLimiter::default()),
        }
    }

    pub fn redemption_limiter(&self) -> MutexGuard<'_, RedemptionLimiter> {
        self.redemption_limiter
            .lock()
            .expect("redemption mutex poisoned")
    }
}

pub struct Server {
    endpoint: quinn::Endpoint,
    fingerprint: Fingerprint,
    state: Arc<State>,
}

impl Server {
    pub fn bind(data_dir: &Path, listen: SocketAddr) -> Result<Self> {
        let identity = identity::load_or_create(data_dir)?;
        let state = Arc::new(State::new(Database::open(data_dir)?));

        let endpoint = quinn::Endpoint::server(tls::server_config(&identity)?, listen)
            .with_context(|| format!("binding UDP {listen}"))?;

        Ok(Self {
            endpoint,
            fingerprint: identity.fingerprint,
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

    /// A fresh code enrolling the first admin, `None` once anyone is enrolled.
    /// Every outstanding invite is invalidated.
    pub fn mint_bootstrap_invite(&self) -> Result<Option<String>> {
        if self.state.database.user_count()? > 0 {
            return Ok(None);
        }
        Ok(Some(self.state.database.replace_invites()?.code))
    }

    pub async fn run(self) -> Result<()> {
        tokio::spawn(maintain_invites(self.state.clone()));

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

async fn maintain_invites(state: Arc<State>) {
    let mut ticker = tokio::time::interval(invite::DECAY_INTERVAL);
    loop {
        ticker.tick().await;
        state.redemption_limiter().decay();
        match state.database.prune_invites() {
            Ok(0) => {}
            Ok(pruned) => tracing::info!(pruned, "pruned expired invites"),
            Err(e) => tracing::warn!(error = ?e, "could not prune invites"),
        }
    }
}
