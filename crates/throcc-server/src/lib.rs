#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::path::Path;

use anyhow::{Context, Result};
use throcc_proto::Fingerprint;
use tracing::Instrument as _;

pub mod identity;
pub mod session;
pub mod tls;

pub struct Server {
    endpoint: quinn::Endpoint,
    fingerprint: Fingerprint,
}

impl Server {
    pub fn bind(data_dir: &Path, listen: SocketAddr) -> Result<Self> {
        let identity = identity::load_or_create(data_dir)?;
        let endpoint = quinn::Endpoint::server(tls::server_config(&identity)?, listen)
            .with_context(|| format!("binding UDP {listen}"))?;

        Ok(Self {
            endpoint,
            fingerprint: identity.fingerprint,
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

    pub async fn run(self) -> Result<()> {
        while let Some(incoming) = self.endpoint.accept().await {
            tokio::spawn(async move {
                match incoming.await {
                    Ok(connection) => {
                        let span = tracing::info_span!("connection", id = connection.stable_id());
                        session::serve(connection).instrument(span).await
                    }
                    Err(e) => tracing::debug!(error = %e, "handshake failed"),
                }
            });
        }
        Ok(())
    }
}
