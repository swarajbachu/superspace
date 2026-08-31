use std::path::{Path, PathBuf};

use quinn::Connection;
use superspace_protocol::Message;
use thiserror::Error;

use crate::{
    BlobReceiver, BlobTransferError, FrameError, MAX_CHUNK_SIZE, read_blob_chunk, read_frame,
    write_frame,
};

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
}
