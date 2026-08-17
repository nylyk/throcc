use std::sync::Arc;

use anyhow::{Context, Result};
use quinn::crypto::rustls::QuicServerConfig;

use crate::identity::ServerIdentity;

pub fn server_config(identity: &ServerIdentity) -> Result<quinn::ServerConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());

    let mut tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .context("selecting TLS 1.3")?
        .with_no_client_auth()
        .with_single_cert(
            vec![identity.certificate.clone()],
            identity.private_key.clone_key(),
        )
        .context("installing the derived certificate")?;
    tls.alpn_protocols = vec![throcc_proto::ALPN.to_vec()];

    let quic = QuicServerConfig::try_from(tls).context("building the QUIC crypto config")?;
    Ok(quinn::ServerConfig::with_crypto(Arc::new(quic)))
}
