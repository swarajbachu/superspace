use std::future::Future;

use serde::{Deserialize, Serialize};
use superspace_protocol::{DeviceId, PROTOCOL_VERSION};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

use crate::{
    LocalIdentity, PairingCode, PairingError, PairingInitiator, PairingResponder, PeerCertificate,
    TransportError,
};

const MAX_PACKET_BYTES: usize = 65_535;
const MAX_NAME_CHARS: usize = 128;
const MAX_PLATFORM_CHARS: usize = 64;
const ACCEPTED: u8 = 1;
const REJECTED: u8 = 0;
const CONFIRMATION_TEXT: &[u8] = b"superspace pairing confirmed v1";

/// Public metadata cryptographically bound into the Noise XX pairing transcript.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PairingPublicInfo {
    /// Stable installation identifier.
    pub device_id: DeviceId,
    /// User-visible device name.
    pub name: String,
    /// Operating-system identifier.
    pub platform: String,
    /// Supported authenticated peer protocol versions.
    pub protocol_versions: Vec<u16>,
    /// Self-signed TLS certificate pinned after code verification.
    #[serde(with = "serde_bytes")]
    pub certificate_der: Vec<u8>,
}

impl PairingPublicInfo {
    /// Construct public metadata from the stable local identity.
    #[must_use]
    pub fn for_local(identity: &LocalIdentity, name: impl Into<String>) -> Self {
        Self {
            device_id: identity.device_id,
            name: name.into(),
            platform: std::env::consts::OS.into(),
            protocol_versions: vec![PROTOCOL_VERSION],
            certificate_der: identity.transport.certificate_der().to_vec(),
        }
    }

    fn validate(&self, local_id: DeviceId) -> Result<PeerCertificate, PairingSessionError> {
        if self.device_id.is_nil()
            || self.device_id == local_id
            || self.name.trim().is_empty()
            || self.name.chars().count() > MAX_NAME_CHARS
            || self.platform.trim().is_empty()
            || self.platform.chars().count() > MAX_PLATFORM_CHARS
        {
            return Err(PairingSessionError::InvalidMetadata);
        }
        if !self.protocol_versions.contains(&PROTOCOL_VERSION) {
            return Err(PairingSessionError::IncompatibleProtocol);
        }
        PeerCertificate::from_der(self.certificate_der.clone()).map_err(Into::into)
    }
}

/// Fully authenticated peer material ready for durable trusted-device storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairedPeer {
    /// Authenticated public metadata.
    pub info: PairingPublicInfo,
    /// Noise static public key authenticated by XX and the confirmed short code.
    pub noise_public_key: [u8; 32],
    /// TLS certificate authenticated inside the same Noise transcript.
    pub certificate: PeerCertificate,
}

/// Initiate pairing over any reliable bidirectional byte stream.
///
/// The confirmation callback may display the six-digit code and await explicit local approval.
/// Neither side returns peer material unless both callbacks approve the same Noise transcript.
///
/// # Errors
/// Returns bounded framing, cryptographic, metadata, local rejection, or peer rejection failures.
pub async fn pair_outgoing<S, Confirm, Confirmation>(
    stream: &mut S,
    identity: &LocalIdentity,
    local: &PairingPublicInfo,
    confirm: Confirm,
) -> Result<PairedPeer, PairingSessionError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    Confirm: FnOnce(PairingCode) -> Confirmation,
    Confirmation: Future<Output = bool>,
{
    validate_local(local, identity.device_id)?;
    let payload = encode_info(local)?;
    let mut initiator = PairingInitiator::new_with_payload(&identity.noise, payload)?;
    write_packet(stream, &initiator.initial_message()?).await?;
    let response = read_packet(stream).await?;
    let mut finish = initiator.finish(&response)?;
    let approved = confirm(finish.code).await;
    if !approved {
        write_packet(stream, &[REJECTED]).await?;
        return Err(PairingSessionError::Rejected);
    }
    let mut decision = Vec::with_capacity(1 + finish.confirmation.len());
    decision.push(ACCEPTED);
    decision.extend_from_slice(&finish.confirmation);
    write_packet(stream, &decision).await?;
    let response = read_packet(stream).await?;
    let Some((&status, ciphertext)) = response.split_first() else {
        return Err(PairingSessionError::UnexpectedMessage);
    };
    if status == REJECTED {
        return Err(PairingSessionError::PeerRejected);
    }
    if status != ACCEPTED || finish.channel.decrypt(ciphertext)? != CONFIRMATION_TEXT {
        return Err(PairingSessionError::UnexpectedMessage);
    }
    paired_peer(
        identity.device_id,
        &finish.remote_payload,
        finish.remote_static_key,
    )
}

/// Accept pairing over any reliable bidirectional byte stream.
///
/// # Errors
/// Returns bounded framing, cryptographic, metadata, local rejection, or peer rejection failures.
pub async fn pair_incoming<S, Confirm, Confirmation>(
    stream: &mut S,
    identity: &LocalIdentity,
    local: &PairingPublicInfo,
    confirm: Confirm,
) -> Result<PairedPeer, PairingSessionError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    Confirm: FnOnce(PairingCode) -> Confirmation,
    Confirmation: Future<Output = bool>,
{
    validate_local(local, identity.device_id)?;
    let payload = encode_info(local)?;
    let initial = read_packet(stream).await?;
    let mut responder = PairingResponder::new_with_payload(&identity.noise, payload)?;
    let reply = responder.respond(&initial)?;
    write_packet(stream, &reply.message).await?;
    let approved = confirm(reply.code).await;
    let decision = read_packet(stream).await?;
    let Some((&status, confirmation)) = decision.split_first() else {
        return Err(PairingSessionError::UnexpectedMessage);
    };
    if status == REJECTED {
        return Err(PairingSessionError::PeerRejected);
    }
    if status != ACCEPTED {
        return Err(PairingSessionError::UnexpectedMessage);
    }
    if !approved {
        write_packet(stream, &[REJECTED]).await?;
        return Err(PairingSessionError::Rejected);
    }
    let mut finish = responder.finish_with_payload(confirmation, reply.code)?;
    let ciphertext = finish.channel.encrypt(CONFIRMATION_TEXT)?;
    let mut accepted = Vec::with_capacity(1 + ciphertext.len());
    accepted.push(ACCEPTED);
    accepted.extend_from_slice(&ciphertext);
    write_packet(stream, &accepted).await?;
    paired_peer(
        identity.device_id,
        &finish.remote_payload,
        finish.remote_static_key,
    )
}

fn validate_local(
    local: &PairingPublicInfo,
    expected_id: DeviceId,
) -> Result<(), PairingSessionError> {
    if local.device_id != expected_id {
        return Err(PairingSessionError::InvalidMetadata);
    }
    if local.name.trim().is_empty()
        || local.name.chars().count() > MAX_NAME_CHARS
        || local.platform.trim().is_empty()
        || local.platform.chars().count() > MAX_PLATFORM_CHARS
        || !local.protocol_versions.contains(&PROTOCOL_VERSION)
    {
        return Err(PairingSessionError::InvalidMetadata);
    }
    PeerCertificate::from_der(local.certificate_der.clone())?;
    Ok(())
}

fn paired_peer(
    local_id: DeviceId,
    payload: &[u8],
    remote_static_key: Vec<u8>,
) -> Result<PairedPeer, PairingSessionError> {
    let info = decode_info(payload)?;
    let certificate = info.validate(local_id)?;
    let noise_public_key = remote_static_key
        .try_into()
        .map_err(|_| PairingSessionError::InvalidMetadata)?;
    Ok(PairedPeer {
        info,
        noise_public_key,
        certificate,
    })
}

fn encode_info(info: &PairingPublicInfo) -> Result<Vec<u8>, PairingSessionError> {
    let mut output = Vec::new();
    ciborium::into_writer(info, &mut output).map_err(|_| PairingSessionError::Codec)?;
    if output.len() > MAX_PACKET_BYTES / 2 {
        return Err(PairingSessionError::OversizedPacket);
    }
    Ok(output)
}

fn decode_info(bytes: &[u8]) -> Result<PairingPublicInfo, PairingSessionError> {
    ciborium::from_reader(bytes).map_err(|_| PairingSessionError::Codec)
}

async fn write_packet(
    stream: &mut (impl AsyncWrite + Unpin),
    packet: &[u8],
) -> Result<(), PairingSessionError> {
    let length = u32::try_from(packet.len()).map_err(|_| PairingSessionError::OversizedPacket)?;
    if packet.is_empty() || packet.len() > MAX_PACKET_BYTES {
        return Err(PairingSessionError::OversizedPacket);
    }
    stream.write_all(&length.to_be_bytes()).await?;
    stream.write_all(packet).await?;
    stream.flush().await?;
    Ok(())
}

async fn read_packet(
    stream: &mut (impl AsyncRead + Unpin),
) -> Result<Vec<u8>, PairingSessionError> {
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length).await?;
    let length = usize::try_from(u32::from_be_bytes(length))
        .map_err(|_| PairingSessionError::OversizedPacket)?;
    if length == 0 || length > MAX_PACKET_BYTES {
        return Err(PairingSessionError::OversizedPacket);
    }
    let mut packet = vec![0; length];
    stream.read_exact(&mut packet).await?;
    Ok(packet)
}

/// Interactive pairing-session failures.
#[derive(Debug, Error)]
pub enum PairingSessionError {
    /// Reliable stream failed.
    #[error("pairing stream failed: {0}")]
    Io(#[from] std::io::Error),
    /// Noise handshake or encrypted confirmation failed.
    #[error("pairing cryptography failed")]
    Pairing(#[from] PairingError),
    /// CBOR public metadata was malformed.
    #[error("pairing metadata encoding failed")]
    Codec,
    /// A packet exceeded strict protocol limits or was empty.
    #[error("pairing packet is invalid or oversized")]
    OversizedPacket,
    /// Peer metadata was invalid or tried to claim the local identity.
    #[error("pairing peer metadata is invalid")]
    InvalidMetadata,
    /// No authenticated protocol version overlaps.
    #[error("pairing peer uses an incompatible protocol")]
    IncompatibleProtocol,
    /// Local user declined the displayed code.
    #[error("pairing was declined locally")]
    Rejected,
    /// Remote user declined the displayed code.
    #[error("pairing was declined by the peer")]
    PeerRejected,
    /// Peer sent a response invalid for this pairing phase.
    #[error("pairing peer sent an unexpected response")]
    UnexpectedMessage,
    /// TLS certificate in authenticated metadata was malformed.
    #[error("pairing transport certificate is invalid")]
    Transport(#[from] TransportError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn both_users_must_confirm_before_peer_material_is_returned() {
        let first = LocalIdentity::generate().expect("first identity");
        let second = LocalIdentity::generate().expect("second identity");
        let first_info = PairingPublicInfo::for_local(&first, "Linux");
        let second_info = PairingPublicInfo::for_local(&second, "Mac");
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("address");
        let (incoming, outgoing) = tokio::join!(
            async {
                let (mut stream, _) = listener.accept().await.expect("accept");
                pair_incoming(&mut stream, &second, &second_info, |_| async { true }).await
            },
            async {
                let mut stream = tokio::net::TcpStream::connect(address)
                    .await
                    .expect("connect");
                pair_outgoing(&mut stream, &first, &first_info, |_| async { true }).await
            }
        );
        let incoming = incoming.expect("incoming pairing");
        let outgoing = outgoing.expect("outgoing pairing");
        assert_eq!(incoming.info, first_info);
        assert_eq!(outgoing.info, second_info);
        assert_eq!(
            incoming.noise_public_key.as_slice(),
            first.noise.public_key()
        );
        assert_eq!(
            outgoing.noise_public_key.as_slice(),
            second.noise.public_key()
        );
    }

    #[tokio::test]
    async fn responder_rejection_is_visible_to_both_sides() {
        let first = LocalIdentity::generate().expect("first identity");
        let second = LocalIdentity::generate().expect("second identity");
        let first_info = PairingPublicInfo::for_local(&first, "Linux");
        let second_info = PairingPublicInfo::for_local(&second, "Mac");
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("address");
        let (incoming, outgoing) = tokio::join!(
            async {
                let (mut stream, _) = listener.accept().await.expect("accept");
                pair_incoming(&mut stream, &second, &second_info, |_| async { false }).await
            },
            async {
                let mut stream = tokio::net::TcpStream::connect(address)
                    .await
                    .expect("connect");
                pair_outgoing(&mut stream, &first, &first_info, |_| async { true }).await
            }
        );
        assert!(matches!(incoming, Err(PairingSessionError::Rejected)));
        assert!(matches!(outgoing, Err(PairingSessionError::PeerRejected)));
    }
}
