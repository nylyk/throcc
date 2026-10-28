pub mod identity;
pub mod tls;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use throcc_proto::Fingerprint;
use throcc_proto::transport::{check_datagrams, close};
use tracing::{info, warn};

use crate::identity::ServerIdentity;
pub use crate::tls::Congestion;

pub struct Config {
    pub data_dir: PathBuf,
    pub listen: SocketAddr,
    pub congestion: Congestion,
}

pub struct Server {
    endpoint: quinn::Endpoint,
    identity: ServerIdentity,
}

impl Server {
    /// Binds the UDP listener. Must be called inside a tokio runtime.
    pub fn bind(config: &Config) -> Result<Self> {
        create_private_dir(&config.data_dir)?;
        let identity = ServerIdentity::load_or_create(&config.data_dir)?;
        let server_config =
            tls::server_config(&identity, tls::transport_config(config.congestion)?)?;
        let endpoint = quinn::Endpoint::server(server_config, config.listen)
            .with_context(|| format!("binding {}", config.listen))?;
        Ok(Self { endpoint, identity })
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.endpoint.local_addr()?)
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.identity.fingerprint
    }

    pub async fn run(self) -> Result<()> {
        while let Some(incoming) = self.endpoint.accept().await {
            tokio::spawn(async move {
                match incoming.await {
                    Ok(connection) => handle(connection).await,
                    Err(e) => warn!("handshake failed: {e}"),
                }
            });
        }
        Ok(())
    }
}

async fn handle(connection: quinn::Connection) {
    let peer = connection.remote_address();
    if let Err(e) = check_datagrams(connection.max_datagram_size()) {
        warn!(%peer, "refusing connection: {e}");
        connection.close(close::DATAGRAMS_REQUIRED.into(), e.to_string().as_bytes());
        return;
    }
    info!(%peer, "connected");
    let reason = connection.closed().await;
    info!(%peer, "disconnected: {reason}");
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
