use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use quinn::crypto::rustls::QuicClientConfig;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, PeerIncompatible, SignatureScheme};
use throcc_proto::Fingerprint;

use crate::identity::Keystore;
use crate::{Error, Result};

pub struct Connector {
    endpoint: quinn::Endpoint,
    keystore: Keystore,
}

impl Connector {
    pub fn new(keystore: Keystore) -> Result<Self> {
        let endpoint = quinn::Endpoint::client((std::net::Ipv4Addr::UNSPECIFIED, 0).into())
            .map_err(|e| Error::Connect(format!("binding a local UDP socket: {e}")))?;
        Ok(Self { endpoint, keystore })
    }

    pub fn keystore(&self) -> &Keystore {
        &self.keystore
    }

    pub fn keystore_mut(&mut self) -> &mut Keystore {
        &mut self.keystore
    }

    /// Pins the server's key on first contact, and enforces it on every later one.
    pub async fn connect(
        &mut self,
        address: SocketAddr,
        server: &str,
    ) -> Result<quinn::Connection> {
        let pinned = self.keystore.pinned(server);
        let verifier = Arc::new(PinVerifier::new(pinned));

        let mut tls = rustls::ClientConfig::builder_with_provider(verifier.provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .expect("TLS 1.3 is supported by the ring provider")
            .dangerous()
            .with_custom_certificate_verifier(verifier.clone())
            .with_no_client_auth();
        tls.alpn_protocols = vec![throcc_proto::ALPN.to_vec()];

        let mut config = quinn::ClientConfig::new(Arc::new(
            QuicClientConfig::try_from(tls)
                .map_err(|e| Error::Connect(format!("building the QUIC crypto config: {e}")))?,
        ));

        let mut transport = quinn::TransportConfig::default();
        transport.keep_alive_interval(Some(Duration::from_secs(10)));
        config.transport_config(Arc::new(transport));

        let handshake = self
            .endpoint
            .connect_with(config, address, "throcc")
            .map_err(|e| Error::Connect(e.to_string()))?;

        let connection = match handshake.await {
            Ok(connection) => connection,
            Err(e) => {
                if let (Some(pinned), Some(presented)) = (pinned, verifier.presented())
                    && pinned != presented
                {
                    return Err(Error::PinMismatch {
                        server: server.to_string(),
                        pinned: pinned.to_string(),
                        presented: presented.to_string(),
                    });
                }
                return Err(Error::Connect(e.to_string()));
            }
        };

        let presented = verifier
            .presented()
            .ok_or_else(|| Error::Connect("handshake completed without a certificate".into()))?;

        if pinned.is_none() {
            tracing::info!(server, fingerprint = %presented, "pinning on first use");
            self.keystore.pin(server, presented)?;
        }

        Ok(connection)
    }
}

/// Accepts a certificate only if its SPKI hash matches the pin
#[derive(Debug)]
struct PinVerifier {
    pinned: Option<Fingerprint>,
    presented: Mutex<Option<Fingerprint>>,
    provider: Arc<CryptoProvider>,
}

impl PinVerifier {
    fn new(pinned: Option<Fingerprint>) -> Self {
        Self {
            pinned,
            presented: Mutex::new(None),
            provider: Arc::new(rustls::crypto::ring::default_provider()),
        }
    }

    fn presented(&self) -> Option<Fingerprint> {
        *self.presented.lock().expect("verifier mutex poisoned")
    }
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        let presented = Fingerprint::from_cert_der(end_entity).map_err(|e| {
            rustls::Error::InvalidCertificate(rustls::CertificateError::Other(rustls::OtherError(
                Arc::new(e),
            )))
        })?;
        *self.presented.lock().expect("verifier mutex poisoned") = Some(presented);

        match self.pinned {
            None => Ok(ServerCertVerified::assertion()),
            Some(pinned) if pinned == presented => Ok(ServerCertVerified::assertion()),
            Some(_) => Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            )),
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(
            PeerIncompatible::Tls12NotOffered,
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
