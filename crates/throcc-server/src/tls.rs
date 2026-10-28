use std::sync::Arc;

use anyhow::Result;
use quinn::congestion::{BbrConfig, CubicConfig};
use quinn::crypto::rustls::QuicServerConfig;
use quinn::{IdleTimeout, TransportConfig};
use throcc_proto::{ALPN, transport};

use crate::identity::ServerIdentity;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Congestion {
    #[default]
    Cubic,
    Bbr,
}

pub fn server_config(
    identity: &ServerIdentity,
    transport: TransportConfig,
) -> Result<quinn::ServerConfig> {
    let (cert, key) = identity.certificate()?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)?;
    tls.alpn_protocols = vec![ALPN.to_vec()];

    let mut config = quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls)?));
    config.transport_config(Arc::new(transport));
    config.max_incoming(256);
    Ok(config)
}

pub fn transport_config(congestion: Congestion) -> Result<TransportConfig> {
    let mut config = TransportConfig::default();
    config
        .keep_alive_interval(Some(transport::KEEP_ALIVE))
        .max_idle_timeout(Some(IdleTimeout::try_from(transport::IDLE_TIMEOUT)?))
        .datagram_send_buffer_size(transport::SERVER_DATAGRAM_SEND_BUFFER)
        .packet_threshold(transport::REORDER_PACKETS)
        .time_threshold(transport::REORDER_RTT_FACTOR);
    match congestion {
        Congestion::Cubic => config.congestion_controller_factory(Arc::new(CubicConfig::default())),
        Congestion::Bbr => config.congestion_controller_factory(Arc::new(BbrConfig::default())),
    };
    Ok(config)
}
