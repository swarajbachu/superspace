use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use quinn::{Connection, RecvStream, SendStream};
use superspace_protocol::{
    ClipboardEvent, ContentHash, DeviceId, DeviceInfo, Message, PROTOCOL_VERSION, TransferManifest,
};
use thiserror::Error;

use crate::{
    BlobReceiver, BlobTransferError, FrameError, MAX_CHUNK_SIZE, read_blob_chunk, read_frame,
    write_frame,
};
use crate::{TransferError, TransferProgress, TransferReceiver, read_transfer_chunk};

const MAX_DEVICE_NAME_CHARS: usize = 128;
const MAX_PLATFORM_CHARS: usize = 64;

/// Exchange authenticated device metadata on an outgoing connection.
///
/// The expected ID comes from the trusted-device record associated with the pinned certificate,
/// preventing a valid paired certificate from claiming another installation's identity.
///
/// # Errors
/// Returns a framing, stream, identity, or protocol-negotiation failure.
pub async fn exchange_hello_outgoing(
    connection: &Connection,
    local: &DeviceInfo,
    expected_peer: DeviceId,
) -> Result<DeviceInfo, PeerSessionError> {
    validate_local_info(local)?;
    let (mut send, mut receive) = connection
        .open_bi()
        .await
        .map_err(|_| PeerSessionError::Stream)?;
    write_frame(&mut send, &Message::Hello(local.clone())).await?;
    send.finish().map_err(|_| PeerSessionError::Stream)?;
    let Message::Hello(remote) = read_frame(&mut receive).await? else {
        return Err(PeerSessionError::UnexpectedMessage);
    };
    validate_remote_info(&remote, local.id, expected_peer)?;
    Ok(remote)
}

/// Exchange authenticated device metadata on an accepted connection.
///
/// # Errors
/// Returns a framing, stream, identity, or protocol-negotiation failure.
pub async fn exchange_hello_incoming(
    connection: &Connection,
    local: &DeviceInfo,
    expected_peer: DeviceId,
) -> Result<DeviceInfo, PeerSessionError> {
    validate_local_info(local)?;
    let (mut send, mut receive) = connection
        .accept_bi()
        .await
        .map_err(|_| PeerSessionError::Stream)?;
    let Message::Hello(remote) = read_frame(&mut receive).await? else {
        return Err(PeerSessionError::UnexpectedMessage);
    };
    validate_remote_info(&remote, local.id, expected_peer)?;
    write_frame(&mut send, &Message::Hello(local.clone())).await?;
    send.finish().map_err(|_| PeerSessionError::Stream)?;
    Ok(remote)
}

/// Offer one clipboard event and wait until the peer confirms durable handling.
///
/// # Errors
/// Returns a framing, stream, or mismatched-acknowledgement failure.
pub async fn offer_clipboard(
    connection: &Connection,
    event: &ClipboardEvent,
) -> Result<(), PeerSessionError> {
    let (mut send, mut receive) = connection
        .open_bi()
        .await
        .map_err(|_| PeerSessionError::Stream)?;
    write_frame(&mut send, &Message::Clipboard(event.clone())).await?;
    send.finish().map_err(|_| PeerSessionError::Stream)?;
    match read_frame(&mut receive).await? {
        Message::Acknowledge { id } if id == event.id => Ok(()),
        Message::Acknowledge { .. } => Err(PeerSessionError::MismatchedAcknowledgement),
        _ => Err(PeerSessionError::UnexpectedMessage),
    }
}

/// Accepted clipboard event whose response remains unacknowledged.
///
/// Keeping acknowledgement explicit lets the caller fetch and verify a referenced blob or file
/// transfer and durably apply the event before the sender drops it from offline reconciliation.
pub struct ClipboardOffer {
    /// Untrusted event received over the mutually authenticated connection.
    pub event: ClipboardEvent,
    response: SendStream,
}

/// Accepted request routed from the first message on a peer-created QUIC stream.
pub enum IncomingPeerRequest {
    /// Clipboard event awaiting durable application and acknowledgement.
    Clipboard(ClipboardOffer),
    /// Resumable request for a content-addressed clipboard blob.
    Blob(BlobRequest),
    /// Resumable file or folder transfer offer.
    Transfer(TransferRequest),
}

/// Accepted blob request with the response stream needed to serve it.
pub struct BlobRequest {
    hash: ContentHash,
    offset: u64,
    response: SendStream,
}

/// Accepted file/folder offer with its already-routed QUIC streams.
pub struct TransferRequest {
    manifest: TransferManifest,
    response: SendStream,
    receive: RecvStream,
}

impl TransferRequest {
    /// Authenticated manifest supplied before any destination paths are created.
    #[must_use]
    pub const fn manifest(&self) -> &TransferManifest {
        &self.manifest
    }

    /// Receive, verify, and atomically publish this transfer with progress and cancellation.
    ///
    /// Cancelling preserves bounded partial files for a later resume.
    ///
    /// # Errors
    /// Returns manifest, disk, framing, stream, integrity, or cancellation failures.
    pub async fn receive_with_progress(
        mut self,
        incoming_root: impl Into<PathBuf>,
        cancellation: &TransferCancellation,
        mut on_progress: impl FnMut(TransferProgress),
    ) -> Result<PathBuf, TransferSessionError> {
        let mut receiver = TransferReceiver::begin(incoming_root, self.manifest.clone())?;
        on_progress(receiver.progress());
        write_frame(
            &mut self.response,
            &Message::TransferResume {
                id: self.manifest.id,
                offsets: receiver.resume_offsets(),
            },
        )
        .await?;
        while receiver
            .resume_offsets()
            .iter()
            .zip(&self.manifest.entries)
            .any(|(offset, entry)| *offset < entry.size)
        {
            if cancellation.is_cancelled() {
                write_frame(
                    &mut self.response,
                    &Message::CancelTransfer {
                        id: self.manifest.id,
                    },
                )
                .await?;
                self.response
                    .finish()
                    .map_err(|_| TransferSessionError::Stream)?;
                return Err(TransferSessionError::Cancelled);
            }
            let message = read_frame(&mut self.receive).await?;
            if matches!(message, Message::CancelTransfer { id } if id == self.manifest.id) {
                return Err(TransferSessionError::Cancelled);
            }
            let Message::TransferChunk(chunk) = message else {
                return Err(TransferSessionError::UnexpectedMessage);
            };
            receiver.accept(&chunk)?;
            on_progress(receiver.progress());
        }
        let destination = receiver.finish()?;
        write_frame(
            &mut self.response,
            &Message::Acknowledge {
                id: self.manifest.id,
            },
        )
        .await?;
        self.response
            .finish()
            .map_err(|_| TransferSessionError::Stream)?;
        self.response
            .stopped()
            .await
            .map_err(|_| TransferSessionError::Stream)?;
        Ok(destination)
    }
}

impl BlobRequest {
    /// Stream the requested blob from a content-addressed root.
    ///
    /// # Errors
    /// Returns invalid offset, source I/O, framing, or stream failures.
    pub async fn serve(mut self, blob_root: impl AsRef<Path>) -> Result<(), BlobSessionError> {
        let path = blob_root.as_ref().join(self.hash.to_hex());
        loop {
            let chunk = read_blob_chunk(&path, self.hash, self.offset, MAX_CHUNK_SIZE)?;
            let complete = chunk.complete;
            self.offset = self
                .offset
                .checked_add(
                    u64::try_from(chunk.bytes.len())
                        .map_err(|_| BlobSessionError::UnexpectedMessage)?,
                )
                .ok_or(BlobSessionError::UnexpectedMessage)?;
            write_frame(&mut self.response, &Message::BlobChunk(chunk)).await?;
            if complete {
                break;
            }
        }
        self.response
            .finish()
            .map_err(|_| BlobSessionError::Stream)?;
        Ok(())
    }
}

impl ClipboardOffer {
    /// Acknowledge durable processing and close the response stream.
    ///
    /// # Errors
    /// Returns a framing or stream failure.
    pub async fn acknowledge(mut self) -> Result<(), PeerSessionError> {
        write_frame(
            &mut self.response,
            &Message::Acknowledge { id: self.event.id },
        )
        .await?;
        self.response
            .finish()
            .map_err(|_| PeerSessionError::Stream)?;
        Ok(())
    }
}

/// Receive the next clipboard offer without acknowledging it prematurely.
///
/// # Errors
/// Returns a framing, stream, or unexpected-message failure.
pub async fn receive_clipboard_offer(
    connection: &Connection,
) -> Result<ClipboardOffer, PeerSessionError> {
    match receive_peer_request(connection).await? {
        IncomingPeerRequest::Clipboard(offer) => Ok(offer),
        IncomingPeerRequest::Blob(_) | IncomingPeerRequest::Transfer(_) => {
            Err(PeerSessionError::UnexpectedMessage)
        }
    }
}

/// Accept and route the next clipboard-control or blob request stream.
///
/// # Errors
/// Returns a framing, stream, or unsupported first-message failure.
pub async fn receive_peer_request(
    connection: &Connection,
) -> Result<IncomingPeerRequest, IncomingRequestError> {
    let (response, mut receive) = connection
        .accept_bi()
        .await
        .map_err(|_| IncomingRequestError::Stream)?;
    match read_frame(&mut receive).await? {
        Message::Clipboard(event) => Ok(IncomingPeerRequest::Clipboard(ClipboardOffer {
            event,
            response,
        })),
        Message::BlobRequest { hash, offset } => Ok(IncomingPeerRequest::Blob(BlobRequest {
            hash,
            offset,
            response,
        })),
        Message::TransferOffer(manifest) => Ok(IncomingPeerRequest::Transfer(TransferRequest {
            manifest,
            response,
            receive,
        })),
        _ => Err(IncomingRequestError::UnexpectedMessage),
    }
}

/// First-message routing failures on an authenticated peer stream.
#[derive(Debug, Error)]
pub enum IncomingRequestError {
    /// Protocol framing failed.
    #[error("incoming peer request frame failed")]
    Frame(#[from] FrameError),
    /// QUIC stream could not be accepted.
    #[error("incoming peer request stream failed")]
    Stream,
    /// Stream began with a message owned by another protocol dispatcher.
    #[error("incoming peer request has an unsupported message")]
    UnexpectedMessage,
}

fn validate_local_info(info: &DeviceInfo) -> Result<(), PeerSessionError> {
    validate_info(info)?;
    Ok(())
}

fn validate_remote_info(
    info: &DeviceInfo,
    local_id: DeviceId,
    expected_peer: DeviceId,
) -> Result<(), PeerSessionError> {
    validate_info(info)?;
    if info.id == local_id || info.id != expected_peer {
        return Err(PeerSessionError::IdentityMismatch);
    }
    Ok(())
}

fn validate_info(info: &DeviceInfo) -> Result<(), PeerSessionError> {
    if info.name.trim().is_empty()
        || info.name.chars().count() > MAX_DEVICE_NAME_CHARS
        || info.platform.trim().is_empty()
        || info.platform.chars().count() > MAX_PLATFORM_CHARS
    {
        return Err(PeerSessionError::InvalidDeviceInfo);
    }
    if !info.protocol_versions.contains(&PROTOCOL_VERSION) {
        return Err(PeerSessionError::IncompatibleProtocol);
    }
    Ok(())
}

/// Authenticated peer control-session failures.
#[derive(Debug, Error)]
pub enum PeerSessionError {
    /// Incoming stream routing failed.
    #[error(transparent)]
    Incoming(#[from] IncomingRequestError),
    /// Protocol framing failed.
    #[error("peer session protocol frame failed")]
    Frame(#[from] FrameError),
    /// QUIC stream could not open, accept, or finish.
    #[error("peer session QUIC stream failed")]
    Stream,
    /// Peer sent an invalid message for the current exchange.
    #[error("peer sent an unexpected control message")]
    UnexpectedMessage,
    /// Authenticated certificate was associated with a different device ID.
    #[error("peer device identity does not match its trusted certificate")]
    IdentityMismatch,
    /// Device metadata was empty or exceeded protocol bounds.
    #[error("peer device metadata is invalid")]
    InvalidDeviceInfo,
    /// No supported protocol version overlaps.
    #[error("peer does not support this protocol version")]
    IncompatibleProtocol,
    /// Acknowledgement did not refer to the offered event.
    #[error("peer acknowledged a different event")]
    MismatchedAcknowledgement,
}

/// Cloneable cooperative cancellation signal for an active transfer session.
#[derive(Clone, Debug, Default)]
pub struct TransferCancellation(Arc<AtomicBool>);

impl TransferCancellation {
    /// Create a signal in the running state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation. Active loops observe this between bounded chunks.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Request and receive one authenticated content-addressed blob over a QUIC bidirectional stream.
///
/// The receiver is consumed so successful return proves its partial file was atomically published.
///
/// # Errors
///
/// Returns stream, protocol, storage, offset, or integrity failures.
pub async fn request_blob(
    connection: &Connection,
    mut receiver: BlobReceiver,
) -> Result<PathBuf, BlobSessionError> {
    if receiver.is_complete() {
        return receiver.finish().map_err(BlobSessionError::Transfer);
    }
    let (mut send, mut receive) = connection
        .open_bi()
        .await
        .map_err(|_| BlobSessionError::Stream)?;
    write_frame(
        &mut send,
        &Message::BlobRequest {
            hash: receiver.expected_hash(),
            offset: receiver.resume_offset(),
        },
    )
    .await?;
    send.finish().map_err(|_| BlobSessionError::Stream)?;
    loop {
        let message = read_frame(&mut receive).await?;
        let Message::BlobChunk(chunk) = message else {
            return Err(BlobSessionError::UnexpectedMessage);
        };
        let complete = chunk.complete;
        receiver.accept(&chunk)?;
        if complete {
            break;
        }
    }
    receiver.finish().map_err(BlobSessionError::Transfer)
}

/// Serve one blob request from a paired peer on its next QUIC bidirectional stream.
///
/// Source paths are derived exclusively from a validated digest, never from peer-supplied path
/// material.
///
/// # Errors
///
/// Returns stream, request-shape, offset, or filesystem failures.
pub async fn serve_blob(
    connection: &Connection,
    blob_root: impl AsRef<Path>,
) -> Result<(), BlobSessionError> {
    match receive_peer_request(connection).await? {
        IncomingPeerRequest::Blob(request) => request.serve(blob_root).await,
        IncomingPeerRequest::Clipboard(_) | IncomingPeerRequest::Transfer(_) => {
            Err(BlobSessionError::UnexpectedMessage)
        }
    }
}

/// Blob request/response session failures.
#[derive(Debug, Error)]
pub enum BlobSessionError {
    /// Incoming stream routing failed.
    #[error(transparent)]
    Incoming(#[from] IncomingRequestError),
    /// Protocol framing failed.
    #[error("clipboard blob protocol frame failed")]
    Frame(#[from] FrameError),
    /// Resumable storage or source reading failed.
    #[error("clipboard blob transfer failed")]
    Transfer(#[from] BlobTransferError),
    /// QUIC stream could not open, accept, or finish.
    #[error("clipboard blob QUIC stream failed")]
    Stream,
    /// Peer sent a message that is not valid in this exchange.
    #[error("clipboard blob peer sent an unexpected message")]
    UnexpectedMessage,
}

/// Offer and stream a file/folder manifest over one authenticated QUIC stream.
///
/// The receiver controls resume offsets. Source paths are joined only after protocol path
/// validation, and every chunk remains bounded by [`MAX_CHUNK_SIZE`].
///
/// # Errors
///
/// Returns manifest, source I/O, peer-resume, framing, stream, or acknowledgement failures.
pub async fn send_transfer(
    connection: &Connection,
    source_root: impl AsRef<Path>,
    manifest: &TransferManifest,
) -> Result<(), TransferSessionError> {
    send_transfer_with_progress(
        connection,
        source_root,
        manifest,
        &TransferCancellation::new(),
        |_| {},
    )
    .await
}

/// Send a transfer with progress snapshots and bidirectional cooperative cancellation.
///
/// # Errors
/// Returns the same validation and transport failures as [`send_transfer`], plus
/// [`TransferSessionError::Cancelled`] when either peer cancels.
#[allow(
    clippy::too_many_lines,
    reason = "streaming state machine is kept contiguous for protocol auditability"
)]
pub async fn send_transfer_with_progress(
    connection: &Connection,
    source_root: impl AsRef<Path>,
    manifest: &TransferManifest,
    cancellation: &TransferCancellation,
    mut on_progress: impl FnMut(TransferProgress),
) -> Result<(), TransferSessionError> {
    manifest.validate().map_err(TransferError::from)?;
    let (mut send, mut receive) = connection
        .open_bi()
        .await
        .map_err(|_| TransferSessionError::Stream)?;
    write_frame(&mut send, &Message::TransferOffer(manifest.clone())).await?;
    let resume_message = read_frame(&mut receive).await?;
    if matches!(resume_message, Message::CancelTransfer { id } if id == manifest.id) {
        return Err(TransferSessionError::Cancelled);
    }
    let Message::TransferResume { id, offsets } = resume_message else {
        return Err(TransferSessionError::UnexpectedMessage);
    };
    if id != manifest.id || offsets.len() != manifest.entries.len() {
        return Err(TransferSessionError::InvalidResume);
    }
    if offsets
        .iter()
        .zip(&manifest.entries)
        .any(|(offset, entry)| *offset > entry.size)
    {
        return Err(TransferSessionError::InvalidResume);
    }
    let total_bytes = manifest.entries.iter().try_fold(0_u64, |total, entry| {
        total
            .checked_add(entry.size)
            .ok_or(TransferError::SizeOverflow)
    })?;
    let mut completed_bytes = offsets.iter().try_fold(0_u64, |total, offset| {
        total
            .checked_add(*offset)
            .ok_or(TransferError::SizeOverflow)
    })?;
    let mut completed_files = offsets
        .iter()
        .zip(&manifest.entries)
        .filter(|(offset, entry)| **offset == entry.size)
        .count();
    on_progress(TransferProgress {
        total_bytes,
        completed_bytes,
        completed_files,
        total_files: manifest.entries.len(),
        current_file: None,
    });
    let mut response = Box::pin(read_frame(&mut receive));
    for (index, (entry, mut offset)) in manifest.entries.iter().zip(offsets).enumerate() {
        let was_incomplete = offset < entry.size;
        let source = source_root.as_ref().join(&entry.relative_path);
        while offset < entry.size {
            if cancellation.is_cancelled() {
                write_frame(&mut send, &Message::CancelTransfer { id: manifest.id }).await?;
                send.finish().map_err(|_| TransferSessionError::Stream)?;
                return Err(TransferSessionError::Cancelled);
            }
            let bytes = read_transfer_chunk(&source, offset, MAX_CHUNK_SIZE)?;
            if bytes.is_empty() {
                return Err(TransferSessionError::SourceChanged);
            }
            let length =
                u64::try_from(bytes.len()).map_err(|_| TransferSessionError::SourceChanged)?;
            let chunk_message = Message::TransferChunk(superspace_protocol::TransferChunk {
                transfer_id: manifest.id,
                entry_index: u32::try_from(index)
                    .map_err(|_| TransferSessionError::InvalidResume)?,
                offset,
                bytes,
            });
            let send_chunk = write_frame(&mut send, &chunk_message);
            tokio::select! {
                result = send_chunk => result?,
                message = &mut response => {
                    return match message? {
                        Message::CancelTransfer { id } if id == manifest.id => Err(TransferSessionError::Cancelled),
                        _ => Err(TransferSessionError::UnexpectedMessage),
                    };
                }
            }
            offset = offset
                .checked_add(length)
                .ok_or(TransferSessionError::SourceChanged)?;
            completed_bytes = completed_bytes
                .checked_add(length)
                .ok_or(TransferError::SizeOverflow)?;
            on_progress(TransferProgress {
                total_bytes,
                completed_bytes,
                completed_files,
                total_files: manifest.entries.len(),
                current_file: Some(entry.relative_path.clone()),
            });
        }
        if was_incomplete {
            completed_files += 1;
            on_progress(TransferProgress {
                total_bytes,
                completed_bytes,
                completed_files,
                total_files: manifest.entries.len(),
                current_file: None,
            });
        }
    }
    send.finish().map_err(|_| TransferSessionError::Stream)?;
    match response.await? {
        Message::Acknowledge { id } if id == manifest.id => Ok(()),
        Message::CancelTransfer { id } if id == manifest.id => Err(TransferSessionError::Cancelled),
        _ => Err(TransferSessionError::UnexpectedMessage),
    }
}

/// Accept the next file/folder offer, stream it into isolated staging, and acknowledge only after
/// every file passes its BLAKE3 digest and the transfer is atomically published.
///
/// # Errors
///
/// Returns framing, stream, manifest, disk, path, offset, or integrity failures.
pub async fn receive_transfer(
    connection: &Connection,
    incoming_root: impl Into<PathBuf>,
) -> Result<PathBuf, TransferSessionError> {
    receive_transfer_with_progress(
        connection,
        incoming_root,
        &TransferCancellation::new(),
        |_| {},
    )
    .await
}

/// Receive a transfer with progress snapshots and resumable cooperative cancellation.
///
/// Cancelling retains verified partial files for a later resume.
///
/// # Errors
/// Returns the same failures as [`receive_transfer`], plus cancellation from either peer.
pub async fn receive_transfer_with_progress(
    connection: &Connection,
    incoming_root: impl Into<PathBuf>,
    cancellation: &TransferCancellation,
    on_progress: impl FnMut(TransferProgress),
) -> Result<PathBuf, TransferSessionError> {
    match receive_peer_request(connection)
        .await
        .map_err(TransferSessionError::Incoming)?
    {
        IncomingPeerRequest::Transfer(request) => {
            request
                .receive_with_progress(incoming_root, cancellation, on_progress)
                .await
        }
        IncomingPeerRequest::Clipboard(_) | IncomingPeerRequest::Blob(_) => {
            Err(TransferSessionError::UnexpectedMessage)
        }
    }
}

/// File/folder QUIC session failures.
#[derive(Debug, Error)]
pub enum TransferSessionError {
    /// Incoming stream routing failed.
    #[error(transparent)]
    Incoming(#[from] IncomingRequestError),
    /// Protocol framing failed.
    #[error("file transfer protocol frame failed")]
    Frame(#[from] FrameError),
    /// Manifest, filesystem, offset, or integrity validation failed.
    #[error("file transfer failed")]
    Transfer(#[from] TransferError),
    /// QUIC stream could not open, accept, or finish.
    #[error("file transfer QUIC stream failed")]
    Stream,
    /// Peer sent a message invalid for the current exchange phase.
    #[error("file transfer peer sent an unexpected message")]
    UnexpectedMessage,
    /// Receiver returned offsets that do not match the offered manifest.
    #[error("file transfer peer returned invalid resume offsets")]
    InvalidResume,
    /// Source file was truncated or replaced during streaming.
    #[error("file transfer source changed while reading")]
    SourceChanged,
    /// Local user or authenticated peer cancelled the transfer.
    #[error("file transfer was cancelled")]
    Cancelled,
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use super::*;
    use crate::{PeerCertificate, QuicEndpoint, TransportIdentity};
    use superspace_protocol::{ClipboardContent, ClipboardFormat, ContentHash, HybridTimestamp};

    #[tokio::test]
    async fn hello_and_clipboard_ack_round_trip_over_pinned_quic() {
        let client_id = uuid::Uuid::new_v4();
        let server_id = uuid::Uuid::new_v4();
        let client_info = DeviceInfo {
            id: client_id,
            name: "Linux Workstation".into(),
            platform: "linux".into(),
            protocol_versions: vec![PROTOCOL_VERSION],
        };
        let server_info = DeviceInfo {
            id: server_id,
            name: "MacBook".into(),
            platform: "macos".into(),
            protocol_versions: vec![PROTOCOL_VERSION],
        };
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
        .expect("server");
        let client = QuicEndpoint::bind(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            client_identity,
            std::slice::from_ref(&server_certificate),
        )
        .expect("client");
        let (server_connection, client_connection) = tokio::join!(
            server.accept(),
            client.connect(server.local_addr().expect("address"), &server_certificate)
        );
        let server_connection = server_connection.expect("server connection");
        let client_connection = client_connection.expect("client connection");

        let (server_remote, client_remote) = tokio::join!(
            exchange_hello_incoming(&server_connection, &server_info, client_id),
            exchange_hello_outgoing(&client_connection, &client_info, server_id)
        );
        assert_eq!(server_remote.expect("server hello"), client_info);
        assert_eq!(client_remote.expect("client hello"), server_info);

        let event = ClipboardEvent {
            id: uuid::Uuid::new_v4(),
            origin: client_id,
            timestamp: HybridTimestamp {
                physical_millis: 42,
                logical: 0,
            },
            format: ClipboardFormat::Text,
            content: ClipboardContent::Inline {
                bytes: b"copied on Linux".to_vec(),
            },
        };
        let (received, sent) = tokio::join!(
            async {
                let offer = receive_clipboard_offer(&server_connection)
                    .await
                    .expect("receive clipboard");
                let received = offer.event.clone();
                offer.acknowledge().await.expect("acknowledge clipboard");
                received
            },
            offer_clipboard(&client_connection, &event)
        );
        sent.expect("offer clipboard");
        assert_eq!(received, event);
    }

    #[test]
    fn hello_validation_rejects_identity_spoofing_and_incompatible_versions() {
        let local = uuid::Uuid::new_v4();
        let expected = uuid::Uuid::new_v4();
        let mut info = DeviceInfo {
            id: uuid::Uuid::new_v4(),
            name: "Peer".into(),
            platform: "linux".into(),
            protocol_versions: vec![PROTOCOL_VERSION],
        };
        assert!(matches!(
            validate_remote_info(&info, local, expected),
            Err(PeerSessionError::IdentityMismatch)
        ));
        info.id = expected;
        info.protocol_versions = vec![PROTOCOL_VERSION + 1];
        assert!(matches!(
            validate_remote_info(&info, local, expected),
            Err(PeerSessionError::IncompatibleProtocol)
        ));
    }

    #[tokio::test]
    async fn blob_resumes_over_mutually_authenticated_quic() {
        let directory = tempfile::tempdir().expect("directory");
        let source_root = directory.path().join("source");
        let destination_root = directory.path().join("destination");
        std::fs::create_dir(&source_root).expect("source root");
        let bytes = vec![42_u8; MAX_CHUNK_SIZE + 73];
        let hash = ContentHash::digest(&bytes);
        std::fs::write(source_root.join(hash.to_hex()), &bytes).expect("source blob");

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
        .expect("server");
        let client = QuicEndpoint::bind(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            client_identity,
            std::slice::from_ref(&server_certificate),
        )
        .expect("client");
        let (server_connection, client_connection) = tokio::join!(
            server.accept(),
            client.connect(server.local_addr().expect("address"), &server_certificate)
        );
        let server_connection = server_connection.expect("server connection");
        let client_connection = client_connection.expect("client connection");

        let mut partial = BlobReceiver::begin(&destination_root, hash, bytes.len() as u64)
            .expect("initial receiver");
        partial
            .accept(&superspace_protocol::BlobChunk {
                hash,
                offset: 0,
                bytes: bytes[..100].to_vec(),
                complete: false,
            })
            .expect("initial partial chunk");
        drop(partial);
        let resumed_blob = BlobReceiver::begin(&destination_root, hash, bytes.len() as u64)
            .expect("resumed receiver");
        assert_eq!(resumed_blob.resume_offset(), 100);
        let (upload_outcome, download_outcome) = tokio::join!(
            serve_blob(&server_connection, &source_root),
            request_blob(&client_connection, resumed_blob)
        );
        upload_outcome.expect("serve");
        let path = download_outcome.expect("receive");
        assert_eq!(std::fs::read(path).expect("read destination"), bytes);
    }

    #[tokio::test]
    async fn folder_transfer_round_trips_over_mutually_authenticated_quic() {
        let directory = tempfile::tempdir().expect("directory");
        let source_root = directory.path().join("shared");
        std::fs::create_dir_all(source_root.join("nested")).expect("source root");
        let bytes = vec![17_u8; MAX_CHUNK_SIZE + 11];
        std::fs::write(source_root.join("nested/data.bin"), &bytes).expect("source file");
        let manifest = TransferManifest {
            id: uuid::Uuid::new_v4(),
            origin: uuid::Uuid::new_v4(),
            name: "Shared Folder".into(),
            entries: vec![superspace_protocol::TransferEntry {
                relative_path: "nested/data.bin".into(),
                size: bytes.len() as u64,
                hash: ContentHash::digest(&bytes),
            }],
        };

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
        .expect("server");
        let client = QuicEndpoint::bind(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            client_identity,
            std::slice::from_ref(&server_certificate),
        )
        .expect("client");
        let (server_connection, client_connection) = tokio::join!(
            server.accept(),
            client.connect(server.local_addr().expect("address"), &server_certificate)
        );
        let server_connection = server_connection.expect("server connection");
        let client_connection = client_connection.expect("client connection");
        let incoming = directory.path().join("incoming");
        let send_cancellation = TransferCancellation::new();
        let receive_cancellation = TransferCancellation::new();
        let mut sent_progress = Vec::new();
        let mut received_progress = Vec::new();
        let (receive_outcome, send_outcome) = tokio::join!(
            receive_transfer_with_progress(
                &server_connection,
                incoming,
                &receive_cancellation,
                |progress| received_progress.push(progress),
            ),
            send_transfer_with_progress(
                &client_connection,
                &source_root,
                &manifest,
                &send_cancellation,
                |progress| sent_progress.push(progress),
            )
        );
        send_outcome.expect("send transfer");
        let destination = receive_outcome.expect("receive transfer");
        assert_eq!(
            std::fs::read(destination.join("nested/data.bin")).expect("destination file"),
            bytes
        );
        assert!(
            (sent_progress.last().expect("send progress").fraction() - 1.0).abs() <= f64::EPSILON
        );
        assert!(
            (received_progress
                .last()
                .expect("receive progress")
                .fraction()
                - 1.0)
                .abs()
                <= f64::EPSILON
        );
    }

    #[test]
    fn cancellation_signal_is_cloneable_and_monotonic() {
        let cancellation = TransferCancellation::new();
        let observer = cancellation.clone();
        assert!(!observer.is_cancelled());
        cancellation.cancel();
        assert!(observer.is_cancelled());
    }
}
