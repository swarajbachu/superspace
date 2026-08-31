use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use superspace_protocol::{BlobChunk, ContentHash};
use thiserror::Error;

use crate::MAX_CHUNK_SIZE;

const DISK_MARGIN_BYTES: u64 = 16 * 1024 * 1024;

/// Restart-safe receiver for one content-addressed clipboard blob.
pub struct BlobReceiver {
    expected_hash: ContentHash,
    expected_size: u64,
    received: u64,
    partial_path: PathBuf,
    final_path: PathBuf,
    file: Option<File>,
    complete: bool,
}

impl BlobReceiver {
    /// Open or resume a partial blob beneath a content-addressed root.
    ///
    /// Existing completed content is re-verified. Partial content resumes only when its size does
    /// not exceed the authenticated event metadata.
    ///
    /// # Errors
    ///
    /// Returns storage, disk-capacity, or integrity failures.
    pub fn begin(
        root: impl Into<PathBuf>,
        expected_hash: ContentHash,
        expected_size: u64,
    ) -> Result<Self, BlobTransferError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        let name = expected_hash.to_hex();
        let final_path = root.join(&name);
        let partial_path = root.join(format!(".{name}.partial"));

        if final_path.is_file() {
            if fs::metadata(&final_path)?.len() == expected_size
                && verify_hash(&final_path, expected_hash).is_ok()
            {
                return Ok(Self {
                    expected_hash,
                    expected_size,
                    received: expected_size,
                    partial_path,
                    final_path,
                    file: None,
                    complete: true,
                });
            }
            return Err(BlobTransferError::ExistingContentMismatch);
        }

        let durable_offset = match fs::metadata(&partial_path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error.into()),
        };
        if durable_offset > expected_size {
            return Err(BlobTransferError::InvalidPartial);
        }
        let remaining = expected_size - durable_offset;
        if fs2::available_space(&root)? < remaining.saturating_add(DISK_MARGIN_BYTES) {
            return Err(BlobTransferError::InsufficientSpace);
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&partial_path)?;
        let mut receiver = Self {
            expected_hash,
            expected_size,
            received: durable_offset,
            partial_path,
            final_path,
            file: Some(file),
            complete: false,
        };
        if durable_offset == expected_size {
            receiver.finalize()?;
        }
        Ok(receiver)
    }

    /// Receiver's durable byte offset for the next [`Message::BlobRequest`](superspace_protocol::Message::BlobRequest).
    #[must_use]
    pub const fn resume_offset(&self) -> u64 {
        self.received
    }

    /// Authenticated digest requested from the peer.
    #[must_use]
    pub const fn expected_hash(&self) -> ContentHash {
        self.expected_hash
    }

    /// Whether final content already passed integrity verification.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.complete
    }

    /// Append one ordered, bounded segment and finalize on the authenticated last chunk.
    ///
    /// # Errors
    ///
    /// Returns ID, offset, size, storage, or integrity failures.
    pub fn accept(&mut self, chunk: &BlobChunk) -> Result<(), BlobTransferError> {
        if self.complete || chunk.hash != self.expected_hash {
            return Err(BlobTransferError::WrongBlob);
        }
        if chunk.offset != self.received {
            return Err(BlobTransferError::WrongOffset);
        }
        if chunk.bytes.is_empty() || chunk.bytes.len() > MAX_CHUNK_SIZE {
            return Err(BlobTransferError::InvalidChunk);
        }
        let length =
            u64::try_from(chunk.bytes.len()).map_err(|_| BlobTransferError::InvalidChunk)?;
        let ending = self
            .received
            .checked_add(length)
            .ok_or(BlobTransferError::InvalidChunk)?;
        if ending > self.expected_size || chunk.complete != (ending == self.expected_size) {
            return Err(BlobTransferError::InvalidChunk);
        }
        self.file
            .as_mut()
            .ok_or(BlobTransferError::Incomplete)?
            .write_all(&chunk.bytes)?;
        self.received = ending;
        if chunk.complete {
            self.finalize()?;
        }
        Ok(())
    }

    /// Return the verified content-addressed path.
    ///
    /// # Errors
    ///
    /// Returns [`BlobTransferError::Incomplete`] until every byte and digest has been verified.
    pub fn finish(self) -> Result<PathBuf, BlobTransferError> {
        if self.complete {
            Ok(self.final_path.clone())
        } else {
            Err(BlobTransferError::Incomplete)
        }
    }

    fn finalize(&mut self) -> Result<(), BlobTransferError> {
        if let Some(file) = self.file.take() {
            file.sync_all()?;
        }
        if let Err(error) = verify_hash(&self.partial_path, self.expected_hash) {
            let _ = fs::remove_file(&self.partial_path);
            self.received = 0;
            return Err(error);
        }
        match fs::rename(&self.partial_path, &self.final_path) {
            Ok(()) => {}
            Err(error) if self.final_path.is_file() => {
                verify_hash(&self.final_path, self.expected_hash)?;
                let _ = fs::remove_file(&self.partial_path);
                let _ = error;
            }
            Err(error) => return Err(error.into()),
        }
        self.complete = true;
        Ok(())
    }
}

/// Read one blob chunk at a receiver-advertised resume offset.
///
/// # Errors
///
/// Returns I/O, invalid offset, or zero-length request failures.
pub fn read_blob_chunk(
    path: impl AsRef<Path>,
    hash: ContentHash,
    offset: u64,
    requested: usize,
) -> Result<BlobChunk, BlobTransferError> {
    if requested == 0 {
        return Err(BlobTransferError::InvalidChunk);
    }
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    if offset >= size {
        return Err(BlobTransferError::WrongOffset);
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0; requested.min(MAX_CHUNK_SIZE)];
    let length = file.read(&mut bytes)?;
    bytes.truncate(length);
    let ending = offset + u64::try_from(length).map_err(|_| BlobTransferError::InvalidChunk)?;
    Ok(BlobChunk {
        hash,
        offset,
        bytes,
        complete: ending == size,
    })
}

/// Resumable blob transfer failures.
#[derive(Debug, Error)]
pub enum BlobTransferError {
    /// Filesystem operation failed.
    #[error("blob transfer storage failed")]
    Io(#[from] std::io::Error),
    /// Destination lacks transfer bytes plus safety margin.
    #[error("not enough free disk space for clipboard blob")]
    InsufficientSpace,
    /// Existing partial file exceeds authenticated size.
    #[error("partial clipboard blob is invalid")]
    InvalidPartial,
    /// Existing final content conflicts with the requested digest or size.
    #[error("existing clipboard blob does not match")]
    ExistingContentMismatch,
    /// Segment references another content digest or arrives after completion.
    #[error("clipboard blob segment references the wrong content")]
    WrongBlob,
    /// Segment does not start at the durable resume offset.
    #[error("clipboard blob segment has the wrong offset")]
    WrongOffset,
    /// Segment is empty, oversized, crosses the expected end, or mislabels completion.
    #[error("clipboard blob segment has an invalid size")]
    InvalidChunk,
    /// Complete bytes do not match the authenticated digest.
    #[error("clipboard blob failed its integrity check")]
    HashMismatch,
    /// Blob has not completed.
    #[error("clipboard blob transfer is incomplete")]
    Incomplete,
}

fn verify_hash(path: &Path, expected: ContentHash) -> Result<(), BlobTransferError> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let length = file.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        hasher.update(&buffer[..length]);
    }
    if hasher.finalize().as_bytes() == expected.as_bytes() {
        Ok(())
    } else {
        Err(BlobTransferError::HashMismatch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resumes_across_receiver_restarts_and_publishes_verified_content() {
        let directory = tempfile::tempdir().expect("directory");
        let bytes = b"a clipboard image larger than one protocol frame";
        let hash = ContentHash::digest(bytes);
        let source = directory.path().join("source");
        fs::write(&source, bytes).expect("source");
        let blob_root = directory.path().join("blobs");
        {
            let mut receiver =
                BlobReceiver::begin(&blob_root, hash, bytes.len() as u64).expect("begin");
            receiver
                .accept(&BlobChunk {
                    hash,
                    offset: 0,
                    bytes: bytes[..10].to_vec(),
                    complete: false,
                })
                .expect("first chunk");
            assert_eq!(receiver.resume_offset(), 10);
        }
        let mut receiver =
            BlobReceiver::begin(&blob_root, hash, bytes.len() as u64).expect("resume");
        assert_eq!(receiver.resume_offset(), 10);
        let chunk = read_blob_chunk(&source, hash, 10, MAX_CHUNK_SIZE).expect("source chunk");
        receiver.accept(&chunk).expect("final chunk");
        let path = receiver.finish().expect("finish");
        assert_eq!(fs::read(path).expect("read"), bytes);
    }

    #[test]
    fn corruption_is_deleted_and_never_published() {
        let directory = tempfile::tempdir().expect("directory");
        let expected = b"expected";
        let hash = ContentHash::digest(expected);
        let mut receiver =
            BlobReceiver::begin(directory.path(), hash, expected.len() as u64).expect("begin");
        assert!(matches!(
            receiver.accept(&BlobChunk {
                hash,
                offset: 0,
                bytes: b"corrupt!".to_vec(),
                complete: true,
            }),
            Err(BlobTransferError::HashMismatch)
        ));
        assert!(!directory.path().join(hash.to_hex()).exists());
        assert_eq!(receiver.resume_offset(), 0);
    }

    #[test]
    fn wrong_offsets_and_false_completion_are_rejected() {
        let directory = tempfile::tempdir().expect("directory");
        let bytes = b"1234";
        let hash = ContentHash::digest(bytes);
        let mut receiver =
            BlobReceiver::begin(directory.path(), hash, bytes.len() as u64).expect("begin");
        assert!(matches!(
            receiver.accept(&BlobChunk {
                hash,
                offset: 1,
                bytes: bytes.to_vec(),
                complete: true,
            }),
            Err(BlobTransferError::WrongOffset)
        ));
        assert!(matches!(
            receiver.accept(&BlobChunk {
                hash,
                offset: 0,
                bytes: bytes.to_vec(),
                complete: false,
            }),
            Err(BlobTransferError::InvalidChunk)
        ));
    }
}
