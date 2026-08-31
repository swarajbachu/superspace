use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{ClientConfig, Connection, Endpoint, ServerConfig};
use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use rustls::server::WebPkiClientVerifier;
use thiserror::Error;

/// Stable TLS name embedded in every private Superspace transport certificate.
pub const TRANSPORT_SERVER_NAME: &str = "peer.superspace.local";
const ALPN: &[u8] = b"superspace/1";
const MAX_CERTIFICATE_BYTES: usize = 16 * 1024;

/// Persistent self-signed certificate identity stored in the OS credential store.
#[derive(Clone, Eq, PartialEq)]
pub struct TransportIdentity {
    certificate: Vec<u8>,
    private_key: Vec<u8>,
}

impl TransportIdentity {
    /// Generate an Ed25519 identity suitable for TLS 1.3 and QUIC.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError`] when the platform crypto provider cannot generate a key.
    pub fn generate() -> Result<Self, TransportError> {
        let generated = rcgen::generate_simple_self_signed(vec![TRANSPORT_SERVER_NAME.into()])
            .map_err(|_| TransportError::Identity)?;
        Ok(Self {
            certificate: generated.cert.der().to_vec(),
            private_key: generated.signing_key.serialize_der(),
        })
    }

    /// Restore an identity after validating size and its certificate/key relationship.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Identity`] for malformed or mismatched material.
    pub fn from_der(certificate: Vec<u8>, private_key: Vec<u8>) -> Result<Self, TransportError> {
        let identity = Self {
            certificate,
            private_key,
        };
        identity.validate()?;
        Ok(identity)
    }

    /// Certificate bytes exchanged inside verified Noise pairing.
    #[must_use]
    pub fn certificate_der(&self) -> &[u8] {
        &self.certificate
    }

    /// Private PKCS#8 bytes for credential-store persistence.
    #[must_use]
    pub fn private_key_der(&self) -> &[u8] {
        &self.private_key
    }

    /// Short certificate fingerprint used in discovery and diagnostics.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.certificate)
    }

    fn validate(&self) -> Result<(), TransportError> {
        if self.certificate.is_empty()
            || self.certificate.len() > MAX_CERTIFICATE_BYTES
            || self.private_key.is_empty()
        {
            return Err(TransportError::Identity);
        }
        server_crypto(self, &[]).map(|_| ())
    }
}

impl fmt::Debug for TransportIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransportIdentity")
            .field("certificate_fingerprint", &self.fingerprint())
            .field("private_key", &"[REDACTED]")
            .finish()
    }
}

/// Certificate pinned only after the six-digit Noise pairing code is confirmed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerCertificate {
    der: Vec<u8>,
}

impl PeerCertificate {
    /// Validate and pin certificate bytes received in the authenticated pairing payload.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Certificate`] for empty, oversized, or invalid DER.
    pub fn from_der(der: Vec<u8>) -> Result<Self, TransportError> {
        if der.is_empty() || der.len() > MAX_CERTIFICATE_BYTES {
            return Err(TransportError::Certificate);
        }
        let certificate = Self { der };
        let mut roots = RootCertStore::empty();
        roots
            .add(certificate.as_certificate())
            .map_err(|_| TransportError::Certificate)?;
        Ok(certificate)
    }

    /// Exact certificate bytes.
    #[must_use]
    pub fn as_der(&self) -> &[u8] {
        &self.der
    }

    /// Short BLAKE3 fingerprint for user-visible trust management.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.der)
    }

    fn as_certificate(&self) -> CertificateDer<'static> {
        CertificateDer::from(self.der.clone())
    }
}

/// Mutually certificate-pinned QUIC endpoint for trusted-peer traffic.
pub struct QuicEndpoint {
    endpoint: Endpoint,
    identity: TransportIdentity,
}

impl QuicEndpoint {
    /// Bind a UDP endpoint and require incoming clients to present a paired certificate.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError`] for invalid identities, trust roots, or socket binding failures.
    pub fn bind(
        address: SocketAddr,
        identity: TransportIdentity,
        trusted_peers: &[PeerCertificate],
    ) -> Result<Self, TransportError> {
        if trusted_peers.is_empty() {
            return Err(TransportError::NoTrustedPeers);
        }
        identity.validate()?;
        let server_config = server_config(&identity, trusted_peers)?;
        let endpoint = Endpoint::server(server_config, address).map_err(TransportError::Io)?;
        Ok(Self { endpoint, identity })
    }

    /// Bound local UDP address, including an assigned ephemeral port.
    ///
    /// # Errors
    ///
    /// Returns an I/O failure if the socket is unavailable.
    pub fn local_addr(&self) -> Result<SocketAddr, TransportError> {
        self.endpoint.local_addr().map_err(TransportError::Io)
    }

    /// Wait for the next mutually authenticated incoming connection.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError`] when the endpoint closes or the TLS handshake fails.
    pub async fn accept(&self) -> Result<Connection, TransportError> {
        self.endpoint
            .accept()
            .await
            .ok_or(TransportError::Closed)?
            .await
            .map_err(|_| TransportError::Handshake)
    }

    /// Connect to an exact certificate pinned during pairing.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError`] for configuration, connection, or certificate failures.
    pub async fn connect(
        &self,
        address: SocketAddr,
        peer: &PeerCertificate,
    ) -> Result<Connection, TransportError> {
        let config = client_config(&self.identity, peer)?;
        self.endpoint
            .connect_with(config, address, TRANSPORT_SERVER_NAME)
            .map_err(|_| TransportError::Configuration)?
            .await
            .map_err(|_| TransportError::Handshake)
    }

    /// Close all connections and stop UDP processing.
    pub fn close(&self) {
        self.endpoint.close(0_u8.into(), b"shutdown");
    }
}

/// QUIC identity, configuration, socket, and handshake failures.
#[derive(Debug, Error)]
pub enum TransportError {
    /// Certificate and private key are malformed or mismatched.
    #[error("QUIC transport identity is invalid")]
    Identity,
    /// A paired certificate is malformed.
    #[error("paired QUIC certificate is invalid")]
    Certificate,
    /// A listening endpoint must have at least one paired client certificate.
    #[error("QUIC endpoint has no trusted peers")]
    NoTrustedPeers,
    /// TLS or QUIC configuration could not be constructed.
    #[error("QUIC transport configuration failed")]
    Configuration,
    /// UDP socket operation failed.
    #[error("QUIC transport socket failed")]
    Io(#[source] std::io::Error),
    /// Endpoint was closed before accepting a connection.
    #[error("QUIC endpoint is closed")]
    Closed,
    /// Peer certificate or QUIC handshake was rejected.
    #[error("QUIC peer authentication failed")]
    Handshake,
}

fn server_config(
    identity: &TransportIdentity,
    trusted_peers: &[PeerCertificate],
) -> Result<ServerConfig, TransportError> {
    let crypto = server_crypto(identity, trusted_peers)?;
    let quic = QuicServerConfig::try_from(crypto).map_err(|_| TransportError::Configuration)?;
    Ok(ServerConfig::with_crypto(Arc::new(quic)))
}

fn server_crypto(
    identity: &TransportIdentity,
    trusted_peers: &[PeerCertificate],
) -> Result<rustls::ServerConfig, TransportError> {
    let certificate = CertificateDer::from(identity.certificate.clone());
    let key = PrivatePkcs8KeyDer::from(identity.private_key.clone()).into();
    let builder = rustls::ServerConfig::builder();
    let mut crypto = if trusted_peers.is_empty() {
        builder
            .with_no_client_auth()
            .with_single_cert(vec![certificate], key)
    } else {
        let mut roots = RootCertStore::empty();
        for peer in trusted_peers {
            roots
                .add(peer.as_certificate())
                .map_err(|_| TransportError::Certificate)?;
        }
        let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
            .build()
            .map_err(|_| TransportError::Configuration)?;
        builder
            .with_client_cert_verifier(verifier)
            .with_single_cert(vec![certificate], key)
    }
    .map_err(|_| TransportError::Identity)?;
    crypto.alpn_protocols = vec![ALPN.to_vec()];
    Ok(crypto)
}

fn client_config(
    identity: &TransportIdentity,
    peer: &PeerCertificate,
) -> Result<ClientConfig, TransportError> {
    let mut roots = RootCertStore::empty();
    roots
        .add(peer.as_certificate())
        .map_err(|_| TransportError::Certificate)?;
    let certificate = CertificateDer::from(identity.certificate.clone());
    let key = PrivatePkcs8KeyDer::from(identity.private_key.clone()).into();
    let mut crypto = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(vec![certificate], key)
        .map_err(|_| TransportError::Identity)?;
    crypto.alpn_protocols = vec![ALPN.to_vec()];
    let quic = QuicClientConfig::try_from(crypto).map_err(|_| TransportError::Configuration)?;
    Ok(ClientConfig::new(Arc::new(quic)))
}

fn fingerprint(certificate: &[u8]) -> String {
    blake3::hash(certificate).as_bytes()[..8].iter().fold(
        String::with_capacity(16),
        |mut output, byte| {
            use std::fmt::Write as _;
            let _ = write!(output, "{byte:02x}");
            output
        },
    )
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use superspace_protocol::{DeviceInfo, Message, PROTOCOL_VERSION};
    use uuid::Uuid;

    use super::*;
    use crate::{read_frame, write_frame};

    #[test]
    fn private_identity_debug_is_redacted_and_restore_validates() {
        let identity = TransportIdentity::generate().expect("identity");
        let debug = format!("{identity:?}");
        assert!(debug.contains("REDACTED"));
        assert!(!debug.contains(&fingerprint(identity.private_key_der())));
        assert!(
            TransportIdentity::from_der(
                identity.certificate_der().to_vec(),
                identity.private_key_der().to_vec(),
            )
            .is_ok()
        );
        assert!(TransportIdentity::from_der(vec![1], vec![2]).is_err());
    }

    #[tokio::test]
    async fn mutually_pinned_peers_exchange_a_stream() {
        let client_identity = TransportIdentity::generate().expect("client identity");
        let server_identity = TransportIdentity::generate().expect("server identity");
        let client_certificate =
            PeerCertificate::from_der(client_identity.certificate_der().to_vec())
                .expect("client certificate");
        let server_certificate =
            PeerCertificate::from_der(server_identity.certificate_der().to_vec())
                .expect("server certificate");
        let server = QuicEndpoint::bind(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            server_identity,
            std::slice::from_ref(&client_certificate),
        )
        .expect("server endpoint");
        let client = QuicEndpoint::bind(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            client_identity,
            std::slice::from_ref(&server_certificate),
        )
        .expect("client endpoint");
        let server_address = server.local_addr().expect("server address");
        let (accepted, connected) = tokio::join!(
            server.accept(),
            client.connect(server_address, &server_certificate)
        );
        let accepted = accepted.expect("accepted connection");
        let connected = connected.expect("connected");
        let (mut send, mut receive) = connected.open_bi().await.expect("open stream");
        let hello = Message::Hello(DeviceInfo {
            id: Uuid::new_v4(),
            name: "MacBook".into(),
            platform: "macos".into(),
            protocol_versions: vec![PROTOCOL_VERSION],
        });
        write_frame(&mut send, &hello).await.expect("write frame");
        send.finish().expect("finish stream");
        let (mut outgoing, mut incoming) = accepted.accept_bi().await.expect("accept stream");
        assert_eq!(read_frame(&mut incoming).await.expect("read frame"), hello);
        let acknowledgement = Message::Acknowledge { id: Uuid::nil() };
        write_frame(&mut outgoing, &acknowledgement)
            .await
            .expect("write ack");
        outgoing.finish().expect("finish ack");
        assert_eq!(
            read_frame(&mut receive).await.expect("read ack"),
            acknowledgement
        );
    }

    #[tokio::test]
    async fn unpaired_certificate_is_rejected() {
        let trusted_client = TransportIdentity::generate().expect("trusted client identity");
        let server_identity = TransportIdentity::generate().expect("server identity");
        let stranger = TransportIdentity::generate().expect("stranger identity");
        let client_certificate =
            PeerCertificate::from_der(trusted_client.certificate_der().to_vec())
                .expect("client certificate");
        let server_certificate =
            PeerCertificate::from_der(server_identity.certificate_der().to_vec())
                .expect("server certificate");
        let server = QuicEndpoint::bind(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            server_identity,
            &[client_certificate],
        )
        .expect("server endpoint");
        let client = QuicEndpoint::bind(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            stranger,
            std::slice::from_ref(&server_certificate),
        )
        .expect("client endpoint");
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            client.connect(
                server.local_addr().expect("server address"),
                &server_certificate,
            ),
        )
        .await;
        assert!(!matches!(outcome, Ok(Ok(_))));
    }
}
