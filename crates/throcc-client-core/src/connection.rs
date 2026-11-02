use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use quinn::crypto::rustls::QuicClientConfig;
use quinn::{IdleTimeout, TransportConfig, VarInt};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use throcc_proto::transport::{self, DatagramError, check_datagrams};
use throcc_proto::{ALPN, DEFAULT_PORT, Fingerprint, SERVER_NAME};

use crate::lock;

const EXPORTER_LABEL: &[u8] = b"throcc/exporter/1";
/// Kept small whatever the share's bitrate: voice queues behind video in this buffer, so a
/// deeper one would delay voice during a share. Media waits for room rather than dropping.
const DATAGRAM_SEND_BUFFER: usize = 64 * 1024;
/// Room for a largest control frame and the start of the next; the control stream is the
/// only one, so this also bounds what a server can leave unread in us.
const CONTROL_WINDOW: u32 = 2 * throcc_proto::framing::MAX_FRAME_BYTES as u32;

pub struct Connected {
    pub connection: quinn::Connection,
    pub fingerprint: Fingerprint,
    /// TLS exporter value for label `throcc/exporter/1`, used for channel binding.
    pub exporter: [u8; 32],
    endpoint: quinn::Endpoint,
}

impl Connected {
    /// Closes the connection and waits until the server has been told, so it retracts
    /// presence at once instead of after the idle timeout.
    pub async fn close(&self) {
        self.connection
            .close(transport::close::NORMAL.into(), b"bye");
        tokio::time::timeout(std::time::Duration::from_secs(1), self.endpoint.wait_idle())
            .await
            .ok();
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("cannot resolve {0}")]
    Resolve(String),
    #[error("the server's key changed: you trusted {expected}, the server now has {actual}")]
    PinMismatch {
        expected: Fingerprint,
        actual: Fingerprint,
    },
    #[error(transparent)]
    Datagrams(#[from] DatagramError),
    #[error("connection failed: {0}")]
    Connection(#[from] quinn::ConnectionError),
    #[error("connection failed: {0}")]
    Setup(String),
}

/// Connects to `address` (`host` or `host:port`) and checks the server's key against `pin`.
/// With no pin, any key is accepted and returned in [`Connected::fingerprint`].
pub async fn connect(address: &str, pin: Option<Fingerprint>) -> Result<Connected, ConnectError> {
    let (config, verifier) = client_config(pin, transport_config())?;
    connect_with(address, config, &verifier).await
}

pub async fn connect_with(
    address: &str,
    config: quinn::ClientConfig,
    verifier: &PinVerifier,
) -> Result<Connected, ConnectError> {
    let remote = resolve(address).await?;
    let bind: SocketAddr = if remote.is_ipv4() {
        ([0, 0, 0, 0], 0).into()
    } else {
        ([0u16; 8], 0).into()
    };
    let mut endpoint =
        quinn::Endpoint::client(bind).map_err(|e| ConnectError::Setup(e.to_string()))?;
    endpoint.set_default_client_config(config);

    let connecting = endpoint
        .connect(remote, SERVER_NAME)
        .map_err(|e| ConnectError::Setup(e.to_string()))?;
    let connection = match connecting.await {
        Ok(connection) => connection,
        Err(e) => {
            if let (Some(expected), Some(actual)) = (verifier.expected, verifier.seen())
                && expected != actual
            {
                return Err(ConnectError::PinMismatch { expected, actual });
            }
            return Err(e.into());
        }
    };
    check_datagrams(connection.max_datagram_size())?;

    let fingerprint = verifier
        .seen()
        .ok_or_else(|| ConnectError::Setup("no server certificate".into()))?;
    let mut exporter = [0u8; 32];
    connection
        .export_keying_material(&mut exporter, EXPORTER_LABEL, b"")
        .map_err(|_| ConnectError::Setup("TLS exporter unavailable".into()))?;
    Ok(Connected {
        connection,
        fingerprint,
        exporter,
        endpoint,
    })
}

pub fn client_config(
    pin: Option<Fingerprint>,
    transport: TransportConfig,
) -> Result<(quinn::ClientConfig, Arc<PinVerifier>), ConnectError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(PinVerifier::new(pin, provider.clone()));
    let mut tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| ConnectError::Setup(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier.clone())
        .with_no_client_auth();
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let crypto = QuicClientConfig::try_from(tls).map_err(|e| ConnectError::Setup(e.to_string()))?;
    let mut config = quinn::ClientConfig::new(Arc::new(crypto));
    config.transport_config(Arc::new(transport));
    Ok((config, verifier))
}

/// The client's transport settings.
pub fn transport_config() -> TransportConfig {
    let mut config = TransportConfig::default();
    config
        .keep_alive_interval(Some(transport::KEEP_ALIVE))
        .datagram_send_buffer_size(DATAGRAM_SEND_BUFFER)
        // The server opens the one control stream; it may open nothing else.
        .max_concurrent_bidi_streams(VarInt::from_u32(1))
        .max_concurrent_uni_streams(VarInt::from_u32(0))
        .stream_receive_window(VarInt::from_u32(CONTROL_WINDOW))
        .receive_window(VarInt::from_u32(CONTROL_WINDOW))
        .packet_threshold(transport::REORDER_PACKETS)
        .time_threshold(transport::REORDER_RTT_FACTOR);
    if let Ok(idle) = IdleTimeout::try_from(transport::IDLE_TIMEOUT) {
        config.max_idle_timeout(Some(idle));
    }
    config
}

async fn resolve(address: &str) -> Result<SocketAddr, ConnectError> {
    let with_port = if address.parse::<SocketAddr>().is_ok() || has_port(address) {
        address.to_owned()
    } else {
        format!("{address}:{DEFAULT_PORT}")
    };
    tokio::net::lookup_host(&with_port)
        .await
        .ok()
        .and_then(|mut addrs| addrs.next())
        .ok_or_else(|| ConnectError::Resolve(address.to_owned()))
}

fn has_port(address: &str) -> bool {
    match address.rsplit_once(':') {
        Some((host, port)) => !host.contains(':') && port.parse::<u16>().is_ok(),
        None => false,
    }
}

/// Accepts a server certificate only by its SPKI hash: no chain, name or expiry checks.
#[derive(Debug)]
pub struct PinVerifier {
    expected: Option<Fingerprint>,
    seen: Mutex<Option<Fingerprint>>,
    provider: Arc<CryptoProvider>,
}

impl PinVerifier {
    fn new(expected: Option<Fingerprint>, provider: Arc<CryptoProvider>) -> Self {
        Self {
            expected,
            seen: Mutex::new(None),
            provider,
        }
    }

    pub fn seen(&self) -> Option<Fingerprint> {
        *lock(&self.seen)
    }
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let cert = webpki::EndEntityCert::try_from(end_entity).map_err(|_| {
            rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding)
        })?;
        let actual = Fingerprint::of_spki(&cert.subject_public_key_info());
        *lock(&self.seen) = Some(actual);
        match self.expected {
            Some(expected) if expected != actual => Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            )),
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::Tls12NotOffered,
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::has_port;

    #[test]
    fn detects_ports() {
        assert!(has_port("voice.example.net:9000"));
        assert!(!has_port("voice.example.net"));
        assert!(!has_port("::1"));
    }
}
