use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

use superspace_protocol::{ContentHash, TransferChunk, TransferManifest};
use thiserror::Error;

/// Maximum application payload per file chunk.
pub const MAX_CHUNK_SIZE: usize = 256 * 1024;

/// File-transfer validation, I/O, and integrity failures.
#[derive(Debug, Error)]
pub enum TransferError {
    /// Manifest failed protocol-level validation.
    #[error("invalid transfer manifest: {0}")]
    Manifest(#[from] superspace_protocol::ProtocolError),
    /// Filesystem operation failed.
    #[error("file transfer storage failed")]
    Io(#[from] std::io::Error),
    /// Destination does not have enough free bytes before transfer overhead.
    #[error("not enough free disk space")]
    InsufficientSpace,
    /// Chunk references another transfer.
    #[error("chunk belongs to another transfer")]
    WrongTransfer,
    /// Chunk index is outside the manifest.
    #[error("chunk references an unknown file")]
    UnknownEntry,
    /// Chunks must arrive at the receiver's advertised resume offset.
    #[error("chunk offset does not match receiver progress")]
    WrongOffset,
    /// Chunk is empty, too large, or extends beyond the declared length.
    #[error("chunk size is invalid")]
    InvalidChunkSize,
    /// Completed file does not match its announced BLAKE3 digest.
    #[error("transferred file failed its integrity check")]
    HashMismatch,
    /// Not every manifest entry is complete.
    #[error("transfer is incomplete")]
    Incomplete,
    /// Destination name or path is unsafe.
    #[error("transfer destination is unsafe")]
    UnsafeDestination,
}

struct ReceivingEntry {
    partial_path: PathBuf,
    final_path: PathBuf,
    file: Option<File>,
    received: u64,
    expected_size: u64,
    expected_hash: ContentHash,
    complete: bool,
}

/// Stateful receiver for an accepted, integrity-checked file transfer.
pub struct TransferReceiver {
    manifest: TransferManifest,
    incoming_root: PathBuf,
    staging_root: PathBuf,
    entries: Vec<ReceivingEntry>,
}

impl TransferReceiver {
    /// Create an isolated staging directory and preflight free disk space.
    ///
    /// # Errors
    ///
    /// Returns [`TransferError`] for an unsafe manifest, insufficient space, or filesystem failure.
    pub fn begin(
        incoming_root: impl Into<PathBuf>,
        manifest: TransferManifest,
    ) -> Result<Self, TransferError> {
        manifest.validate()?;
        validate_single_component(&manifest.name)?;
        let incoming_root = incoming_root.into();
        fs::create_dir_all(&incoming_root)?;
        let required = manifest
            .entries
            .iter()
            .try_fold(0_u64, |total, entry| total.checked_add(entry.size))
            .ok_or(TransferError::InsufficientSpace)?;
        let margin = 16 * 1024 * 1024_u64;
        if fs2::available_space(&incoming_root)? < required.saturating_add(margin) {
            return Err(TransferError::InsufficientSpace);
        }

        let staging_root = incoming_root.join(format!(".{}.partial", manifest.id));
        fs::create_dir(&staging_root)?;
        let mut entries = Vec::with_capacity(manifest.entries.len());
        for (index, entry) in manifest.entries.iter().enumerate() {
            let final_path = staging_root.join(&entry.relative_path);
            let parent = final_path
                .parent()
                .ok_or(TransferError::UnsafeDestination)?;
            fs::create_dir_all(parent)?;
            let partial_path = final_path.with_extension(format!("superspace-part-{index}"));
            let file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&partial_path)?;
            let mut receiving_entry = ReceivingEntry {
                partial_path,
                final_path,
                file: Some(file),
                received: 0,
                expected_size: entry.size,
                expected_hash: entry.hash,
                complete: false,
            };
            if entry.size == 0 {
                let file = receiving_entry
                    .file
                    .take()
                    .ok_or(TransferError::Incomplete)?;
                file.sync_all()?;
                verify_hash(&receiving_entry.partial_path, receiving_entry.expected_hash)?;
                fs::rename(&receiving_entry.partial_path, &receiving_entry.final_path)?;
                receiving_entry.complete = true;
            }
            entries.push(receiving_entry);
        }
        Ok(Self {
            manifest,
            incoming_root,
            staging_root,
            entries,
        })
    }

    /// Apply one ordered chunk and finalize its file when complete.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong IDs/offsets, invalid sizes, I/O failures, or a hash mismatch.
    pub fn accept(&mut self, chunk: &TransferChunk) -> Result<(), TransferError> {
        if chunk.transfer_id != self.manifest.id {
            return Err(TransferError::WrongTransfer);
        }
        let index = usize::try_from(chunk.entry_index).map_err(|_| TransferError::UnknownEntry)?;
        let entry = self
            .entries
            .get_mut(index)
            .ok_or(TransferError::UnknownEntry)?;
        if entry.complete || chunk.offset != entry.received {
            return Err(TransferError::WrongOffset);
        }
        let chunk_len =
            u64::try_from(chunk.bytes.len()).map_err(|_| TransferError::InvalidChunkSize)?;
        if chunk.bytes.is_empty()
            || chunk.bytes.len() > MAX_CHUNK_SIZE
            || entry.received.saturating_add(chunk_len) > entry.expected_size
        {
            return Err(TransferError::InvalidChunkSize);
        }
        let file = entry.file.as_mut().ok_or(TransferError::Incomplete)?;
        file.write_all(&chunk.bytes)?;
        entry.received += chunk_len;
        if entry.received == entry.expected_size {
            file.sync_all()?;
            drop(entry.file.take());
            verify_hash(&entry.partial_path, entry.expected_hash)?;
            fs::rename(&entry.partial_path, &entry.final_path)?;
            entry.complete = true;
        }
        Ok(())
    }

    /// Current byte offsets, suitable for a transfer-resume message.
    #[must_use]
    pub fn resume_offsets(&self) -> Vec<u64> {
        self.entries.iter().map(|entry| entry.received).collect()
    }

    /// Atomically publish the completed transfer with collision-safe naming.
    ///
    /// # Errors
    ///
    /// Returns [`TransferError::Incomplete`] until every file passes integrity verification.
    pub fn finish(mut self) -> Result<PathBuf, TransferError> {
        if self.entries.iter().any(|entry| !entry.complete) {
            return Err(TransferError::Incomplete);
        }
        self.entries.clear();
        let destination = available_destination(&self.incoming_root, &self.manifest.name);
        fs::rename(&self.staging_root, &destination)?;
        Ok(destination)
    }
}

impl Drop for TransferReceiver {
    fn drop(&mut self) {
        for entry in &mut self.entries {
            drop(entry.file.take());
        }
    }
}

/// Read one bounded source-file chunk at a requested resume offset.
///
/// # Errors
///
/// Returns filesystem errors or [`TransferError::InvalidChunkSize`] for a zero requested length.
pub fn read_transfer_chunk(
    path: impl AsRef<Path>,
    offset: u64,
    requested: usize,
) -> Result<Vec<u8>, TransferError> {
    if requested == 0 {
        return Err(TransferError::InvalidChunkSize);
    }
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0; requested.min(MAX_CHUNK_SIZE)];
    let length = file.read(&mut bytes)?;
    bytes.truncate(length);
    Ok(bytes)
}

fn verify_hash(path: &Path, expected: ContentHash) -> Result<(), TransferError> {
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
    if hasher.finalize().as_bytes() != expected.as_bytes() {
        return Err(TransferError::HashMismatch);
    }
    Ok(())
}

fn validate_single_component(name: &str) -> Result<(), TransferError> {
    let mut components = Path::new(name).components();
    let valid =
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();
    if valid {
        Ok(())
    } else {
        Err(TransferError::UnsafeDestination)
    }
}

fn available_destination(root: &Path, name: &str) -> PathBuf {
    let requested = root.join(name);
    if !requested.exists() {
        return requested;
    }
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("Transfer");
    let extension = path.extension().and_then(|value| value.to_str());
    for suffix in 2_u32.. {
        let candidate_name = extension.map_or_else(
            || format!("{stem} {suffix}"),
            |extension| format!("{stem} {suffix}.{extension}"),
        );
        let candidate = root.join(candidate_name);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("unbounded suffix search")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use superspace_protocol::TransferEntry;
    use uuid::Uuid;

    use super::*;

    fn manifest(bytes: &[u8]) -> TransferManifest {
        TransferManifest {
            id: Uuid::new_v4(),
            origin: Uuid::new_v4(),
            name: "Shared Files".into(),
            entries: vec![TransferEntry {
                relative_path: "folder/hello.txt".into(),
                size: bytes.len() as u64,
                hash: ContentHash::digest(bytes),
            }],
        }
    }

    #[test]
    fn receives_verifies_and_publishes_nested_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let bytes = b"copied on mac, pasted on linux";
        let manifest = manifest(bytes);
        let mut receiver =
            TransferReceiver::begin(directory.path(), manifest.clone()).expect("begin");
        receiver
            .accept(&TransferChunk {
                transfer_id: manifest.id,
                entry_index: 0,
                offset: 0,
                bytes: bytes[..10].to_vec(),
            })
            .expect("first chunk");
        assert_eq!(receiver.resume_offsets(), [10]);
        receiver
            .accept(&TransferChunk {
                transfer_id: manifest.id,
                entry_index: 0,
                offset: 10,
                bytes: bytes[10..].to_vec(),
            })
            .expect("final chunk");
        let destination = receiver.finish().expect("finish");
        assert_eq!(
            fs::read(destination.join("folder/hello.txt")).expect("read"),
            bytes
        );
    }

    #[test]
    fn rejects_wrong_offsets_and_corrupt_content() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let expected = b"expected";
        let manifest = manifest(expected);
        let mut receiver =
            TransferReceiver::begin(directory.path(), manifest.clone()).expect("begin");
        let wrong_offset = TransferChunk {
            transfer_id: manifest.id,
            entry_index: 0,
            offset: 2,
            bytes: expected.to_vec(),
        };
        assert!(matches!(
            receiver.accept(&wrong_offset),
            Err(TransferError::WrongOffset)
        ));
        let corrupt = TransferChunk {
            offset: 0,
            bytes: b"corrupt!".to_vec(),
            ..wrong_offset
        };
        assert!(matches!(
            receiver.accept(&corrupt),
            Err(TransferError::HashMismatch)
        ));
    }

    #[test]
    fn collision_names_do_not_overwrite_existing_content() {
        let directory = tempfile::tempdir().expect("temporary directory");
        fs::create_dir(directory.path().join("Shared Files")).expect("existing destination");
        let bytes = b"hello";
        let manifest = manifest(bytes);
        let mut receiver =
            TransferReceiver::begin(directory.path(), manifest.clone()).expect("begin");
        receiver
            .accept(&TransferChunk {
                transfer_id: manifest.id,
                entry_index: 0,
                offset: 0,
                bytes: bytes.to_vec(),
            })
            .expect("chunk");
        assert_eq!(
            receiver
                .finish()
                .expect("finish")
                .file_name()
                .and_then(|value| value.to_str()),
            Some("Shared Files 2")
        );
    }

    #[test]
    fn source_chunk_reader_honors_resume_offset_and_cap() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("source");
        fs::write(&path, b"0123456789").expect("write source");
        assert_eq!(
            read_transfer_chunk(&path, 4, 3).expect("read chunk"),
            b"456"
        );
        assert!(read_transfer_chunk(&path, 0, 0).is_err());
    }

    #[test]
    fn empty_files_complete_without_a_chunk() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let manifest = manifest(b"");
        let receiver =
            TransferReceiver::begin(directory.path(), manifest).expect("begin empty transfer");
        assert_eq!(receiver.resume_offsets(), [0]);
        let destination = receiver.finish().expect("finish empty transfer");
        assert_eq!(
            fs::metadata(destination.join("folder/hello.txt"))
                .expect("empty file metadata")
                .len(),
            0
        );
    }
}
