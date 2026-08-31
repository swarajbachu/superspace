use std::path::{Path, PathBuf};

use quinn::Connection;
use superspace_protocol::{Message, TransferManifest};
use thiserror::Error;

use crate::{
    BlobReceiver, BlobTransferError, FrameError, MAX_CHUNK_SIZE, read_blob_chunk, read_frame,
    write_frame,
};
use crate::{TransferError, TransferReceiver, read_transfer_chunk};

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
    let (mut send, mut receive) = connection
        .accept_bi()
        .await
        .map_err(|_| BlobSessionError::Stream)?;
    let Message::BlobRequest { hash, mut offset } = read_frame(&mut receive).await? else {
        return Err(BlobSessionError::UnexpectedMessage);
    };
    let path = blob_root.as_ref().join(hash.to_hex());
    loop {
        let chunk = read_blob_chunk(&path, hash, offset, MAX_CHUNK_SIZE)?;
        let complete = chunk.complete;
        offset = offset
            .checked_add(
                u64::try_from(chunk.bytes.len())
                    .map_err(|_| BlobSessionError::UnexpectedMessage)?,
            )
            .ok_or(BlobSessionError::UnexpectedMessage)?;
        write_frame(&mut send, &Message::BlobChunk(chunk)).await?;
        if complete {
            break;
        }
    }
    send.finish().map_err(|_| BlobSessionError::Stream)?;
    Ok(())
}

/// Blob request/response session failures.
#[derive(Debug, Error)]
pub enum BlobSessionError {
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
    manifest.validate().map_err(TransferError::from)?;
    let (mut send, mut receive) = connection
        .open_bi()
        .await
        .map_err(|_| TransferSessionError::Stream)?;
    write_frame(&mut send, &Message::TransferOffer(manifest.clone())).await?;
    let Message::TransferResume { id, offsets } = read_frame(&mut receive).await? else {
        return Err(TransferSessionError::UnexpectedMessage);
    };
    if id != manifest.id || offsets.len() != manifest.entries.len() {
        return Err(TransferSessionError::InvalidResume);
    }
    for (index, (entry, mut offset)) in manifest.entries.iter().zip(offsets).enumerate() {
        if offset > entry.size {
            return Err(TransferSessionError::InvalidResume);
        }
        let source = source_root.as_ref().join(&entry.relative_path);
        while offset < entry.size {
            let bytes = read_transfer_chunk(&source, offset, MAX_CHUNK_SIZE)?;
            if bytes.is_empty() {
                return Err(TransferSessionError::SourceChanged);
            }
            let length =
                u64::try_from(bytes.len()).map_err(|_| TransferSessionError::SourceChanged)?;
            write_frame(
                &mut send,
                &Message::TransferChunk(superspace_protocol::TransferChunk {
                    transfer_id: manifest.id,
                    entry_index: u32::try_from(index)
                        .map_err(|_| TransferSessionError::InvalidResume)?,
                    offset,
                    bytes,
                }),
            )
            .await?;
            offset = offset
                .checked_add(length)
                .ok_or(TransferSessionError::SourceChanged)?;
        }
    }
    send.finish().map_err(|_| TransferSessionError::Stream)?;
    match read_frame(&mut receive).await? {
        Message::Acknowledge { id } if id == manifest.id => Ok(()),
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
    let (mut send, mut receive) = connection
        .accept_bi()
        .await
        .map_err(|_| TransferSessionError::Stream)?;
    let Message::TransferOffer(manifest) = read_frame(&mut receive).await? else {
        return Err(TransferSessionError::UnexpectedMessage);
    };
    let mut receiver = TransferReceiver::begin(incoming_root, manifest.clone())?;
    write_frame(
        &mut send,
        &Message::TransferResume {
            id: manifest.id,
            offsets: receiver.resume_offsets(),
        },
    )
    .await?;
    while receiver
        .resume_offsets()
        .iter()
        .zip(&manifest.entries)
        .any(|(offset, entry)| *offset < entry.size)
    {
        let Message::TransferChunk(chunk) = read_frame(&mut receive).await? else {
            return Err(TransferSessionError::UnexpectedMessage);
        };
        receiver.accept(&chunk)?;
    }
    let destination = receiver.finish()?;
    write_frame(&mut send, &Message::Acknowledge { id: manifest.id }).await?;
    send.finish().map_err(|_| TransferSessionError::Stream)?;
    Ok(destination)
}

/// File/folder QUIC session failures.
#[derive(Debug, Error)]
pub enum TransferSessionError {
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
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use super::*;
    use crate::{PeerCertificate, QuicEndpoint, TransportIdentity};
    use superspace_protocol::ContentHash;

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
        let (receive_outcome, send_outcome) = tokio::join!(
            receive_transfer(&server_connection, incoming),
            send_transfer(&client_connection, &source_root, &manifest)
        );
        send_outcome.expect("send transfer");
        let destination = receive_outcome.expect("receive transfer");
        assert_eq!(
            std::fs::read(destination.join("nested/data.bin")).expect("destination file"),
            bytes
        );
    }
}
