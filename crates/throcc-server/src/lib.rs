pub mod auth;
pub mod database;
pub mod identity;
pub mod permissions;
pub mod rooms;
pub mod session;
pub mod tls;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::{Context, Result};
use throcc_proto::Fingerprint;
use throcc_proto::invite::InviteCode;
use throcc_proto::transport::{check_datagrams, close};
use tracing::{info, warn};

use crate::auth::SourceLimiter;
use crate::database::Database;
use crate::identity::ServerIdentity;
use crate::rooms::{Hub, unix_now};
pub use crate::tls::Congestion;

const DATABASE_FILE: &str = "throcc.sqlite";

pub struct Config {
    pub data_dir: PathBuf,
    pub listen: SocketAddr,
    pub congestion: Congestion,
}

/// State shared by every connection.
pub struct Shared {
    pub fingerprint: Fingerprint,
    hub: Mutex<Hub>,
    limiter: Mutex<SourceLimiter>,
}

impl Shared {
    /// Locks the hub. Never hold the guard across an `.await`.
    pub fn hub(&self) -> MutexGuard<'_, Hub> {
        self.hub.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn limiter(&self) -> MutexGuard<'_, SourceLimiter> {
        self.limiter.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub struct Server {
    endpoint: quinn::Endpoint,
    shared: Arc<Shared>,
    bootstrap: Option<InviteCode>,
}

impl Server {
    /// Binds the UDP listener. Must be called inside a tokio runtime.
    pub fn bind(config: &Config) -> Result<Self> {
        create_private_dir(&config.data_dir)?;
        let identity = ServerIdentity::load_or_create(&config.data_dir)?;
        let mut hub = Hub::new(Database::open(&config.data_dir.join(DATABASE_FILE))?)?;
        let bootstrap = if hub.db.user_count()? == 0 {
            hub.db.delete_bootstrap_invites()?;
            Some(hub.mint_invite(None, unix_now())?.0)
        } else {
            None
        };
        let server_config =
            tls::server_config(&identity, tls::transport_config(config.congestion)?)?;
        let endpoint = quinn::Endpoint::server(server_config, config.listen)
            .with_context(|| format!("binding {}", config.listen))?;
        let shared = Arc::new(Shared {
            fingerprint: identity.fingerprint,
            hub: Mutex::new(hub),
            limiter: Mutex::default(),
        });
        Ok(Self {
            endpoint,
            shared,
            bootstrap,
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.endpoint.local_addr()?)
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.shared.fingerprint
    }

    /// The invite that makes its user the first admin. Present only while no user exists.
    pub fn bootstrap_invite(&self) -> Option<&InviteCode> {
        self.bootstrap.as_ref()
    }

    pub fn shared(&self) -> Arc<Shared> {
        self.shared.clone()
    }

    pub async fn run(self) -> Result<()> {
        while let Some(incoming) = self.endpoint.accept().await {
            let shared = self.shared.clone();
            tokio::spawn(async move {
                match incoming.await {
                    Ok(connection) => handle(shared, connection).await,
                    Err(e) => warn!("handshake failed: {e}"),
                }
            });
        }
        Ok(())
    }
}

async fn handle(shared: Arc<Shared>, connection: quinn::Connection) {
    let peer = connection.remote_address();
    if let Err(e) = check_datagrams(connection.max_datagram_size()) {
        warn!(%peer, "refusing connection: {e}");
        connection.close(close::DATAGRAMS_REQUIRED.into(), e.to_string().as_bytes());
        return;
    }
    info!(%peer, "connected");
    session::run(shared, connection).await;
}

fn create_private_dir(path: &std::path::Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(path)
        .with_context(|| format!("creating {}", path.display()))
}
