//! Clipboard synchronization coordinator joining platform, storage, and network state.

use std::io::Cursor;
use std::path::PathBuf;

use superspace_network::{ApplyDecision, ReplicationError, ReplicationLedger};
use superspace_platform::{ClipboardBackend, ClipboardError, ClipboardMonitor, ClipboardValue};
use superspace_protocol::{
    ClipboardContent, ClipboardEvent, ClipboardFormat, ContentHash, DeviceId, TransferId,
};
use superspace_storage::{
    BlobStore, ClipboardEntry, ClipboardKind, ClipboardSource, ClipboardStore, StorageError,
};
use thiserror::Error;
use uuid::Uuid;

/// Payloads larger than this use the content-addressed blob transfer path.
pub const MAX_INLINE_CLIPBOARD_BYTES: usize = 384 * 1024;
const MAX_DECODED_IMAGE_BYTES: usize = 128 * 1024 * 1024;

/// End-to-end clipboard capture, conflict resolution, persistence, and OS application.
pub struct ClipboardSync<B> {
    local_device: DeviceId,
    monitor: ClipboardMonitor<B>,
    ledger: ReplicationLedger,
    history: ClipboardStore,
    blobs: BlobStore,
}

impl<B: ClipboardBackend> ClipboardSync<B> {
    /// Compose independently testable platform and persistence services.
    #[must_use]
    pub fn new(
        local_device: DeviceId,
        physical_millis: u64,
        backend: B,
        history: ClipboardStore,
        blobs: BlobStore,
    ) -> Self {
        Self {
            local_device,
            monitor: ClipboardMonitor::new(backend),
            ledger: ReplicationLedger::new(local_device, physical_millis),
            history,
            blobs,
        }
    }

    /// Capture a physical local copy, persist it, and queue it for every enabled peer.
    ///
    /// # Errors
    ///
    /// Returns platform, encoding, storage, or replication failures. Native file lists require a
    /// transfer manifest and must enter through [`Self::record_local_files`].
    pub fn poll_local(
        &mut self,
        now_millis: u64,
        peers: impl IntoIterator<Item = DeviceId>,
        expires_at: i64,
    ) -> Result<Option<ClipboardEvent>, SyncError> {
        let Some(observation) = self.monitor.poll()? else {
            return Ok(None);
        };
        if matches!(observation.value, ClipboardValue::Files(_)) {
            return Err(SyncError::FilesRequireTransfer);
        }
        let encoded = encode_value(&observation.value)?;
        let event = self.local_event(now_millis, &encoded);
        self.persist(&event, &encoded.bytes, None)?;
        self.ledger.record_local(&event, peers, expires_at)?;
        Ok(Some(event))
    }

    /// Record copied files after the transfer service has created their manifest.
    ///
    /// # Errors
    ///
    /// Returns validation, storage, platform, or replication failures.
    pub fn record_local_files(
        &mut self,
        paths: Vec<PathBuf>,
        transfer_id: TransferId,
        now_millis: u64,
        peers: impl IntoIterator<Item = DeviceId>,
        expires_at: i64,
    ) -> Result<ClipboardEvent, SyncError> {
        let value = ClipboardValue::Files(paths);
        value.digest()?;
        let bytes = encode_paths(&value);
        let event = ClipboardEvent {
            id: Uuid::new_v4(),
            origin: self.local_device,
            timestamp: self.ledger.next_timestamp(now_millis),
            format: ClipboardFormat::Files,
            content: ClipboardContent::Transfer { transfer_id },
        };
        self.persist(&event, &bytes, None)?;
        self.ledger.record_local(&event, peers, expires_at)?;
        Ok(event)
    }

    /// Process an authenticated remote event whose content is inline.
    ///
    /// # Errors
    ///
    /// Returns decoding, platform, storage, or protocol validation failures.
    pub fn receive(
        &mut self,
        event: &ClipboardEvent,
        now_millis: u64,
    ) -> Result<SyncOutcome, SyncError> {
        match &event.content {
            ClipboardContent::Inline { bytes } => self.apply_bytes(event, bytes, now_millis),
            ClipboardContent::Blob { hash, size } => Ok(SyncOutcome::NeedsBlob {
                hash: *hash,
                size: *size,
            }),
            ClipboardContent::Transfer { transfer_id } => {
                Ok(SyncOutcome::NeedsTransfer { id: *transfer_id })
            }
        }
    }

    /// Verify and apply content fetched through the blob transfer path.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::ContentMismatch`] before touching the OS clipboard if size or digest
    /// differs from the event.
    pub fn receive_blob(
        &mut self,
        event: &ClipboardEvent,
        bytes: &[u8],
        now_millis: u64,
    ) -> Result<SyncOutcome, SyncError> {
        let ClipboardContent::Blob { hash, size } = event.content else {
            return Err(SyncError::UnexpectedContent);
        };
        if u64::try_from(bytes.len()).ok() != Some(size) || ContentHash::digest(bytes) != hash {
            return Err(SyncError::ContentMismatch);
        }
        self.apply_bytes(event, bytes, now_millis)
    }

    /// Apply file URLs only after the matching transfer completed and passed integrity checks.
    ///
    /// # Errors
    ///
    /// Returns validation, platform, storage, or conflict-state failures.
    pub fn receive_files(
        &mut self,
        event: &ClipboardEvent,
        transfer_id: TransferId,
        paths: Vec<PathBuf>,
        now_millis: u64,
    ) -> Result<SyncOutcome, SyncError> {
        if event.format != ClipboardFormat::Files
            || !matches!(
                event.content,
                ClipboardContent::Transfer {
                    transfer_id: expected
                } if expected == transfer_id
            )
        {
            return Err(SyncError::UnexpectedContent);
        }
        let value = ClipboardValue::Files(paths);
        value.digest()?;
        let bytes = encode_paths(&value);
        self.apply_ready(event, &value, &bytes, now_millis)
    }

    /// Offline events still owed to a peer, with expired entries pruned.
    pub fn pending_for(&mut self, peer: DeviceId, now_millis: i64) -> Vec<ClipboardEvent> {
        self.ledger.pending_for(peer, now_millis)
    }

    /// Remove a durably acknowledged event from one peer's replay queue.
    #[must_use]
    pub fn acknowledge(&mut self, peer: DeviceId, event_id: Uuid) -> bool {
        self.ledger.acknowledge(peer, event_id)
    }

    fn local_event(&mut self, now_millis: u64, encoded: &EncodedValue) -> ClipboardEvent {
        let content = if encoded.bytes.len() <= MAX_INLINE_CLIPBOARD_BYTES {
            ClipboardContent::Inline {
                bytes: encoded.bytes.clone(),
            }
        } else {
            ClipboardContent::Blob {
                hash: ContentHash::digest(&encoded.bytes),
                size: encoded.bytes.len() as u64,
            }
        };
        ClipboardEvent {
            id: Uuid::new_v4(),
            origin: self.local_device,
            timestamp: self.ledger.next_timestamp(now_millis),
            format: encoded.format,
            content,
        }
    }

    fn apply_bytes(
        &mut self,
        event: &ClipboardEvent,
        bytes: &[u8],
        now_millis: u64,
    ) -> Result<SyncOutcome, SyncError> {
        let value = decode_value(event.format, bytes)?;
        self.apply_ready(event, &value, bytes, now_millis)
    }

    fn apply_ready(
        &mut self,
        event: &ClipboardEvent,
        value: &ClipboardValue,
        persisted_bytes: &[u8],
        now_millis: u64,
    ) -> Result<SyncOutcome, SyncError> {
        match self.ledger.preview(event) {
            ApplyDecision::Duplicate => {
                self.ledger.receive(event, now_millis);
                Ok(SyncOutcome::Duplicate)
            }
            ApplyDecision::Superseded => {
                self.ledger.receive(event, now_millis);
                self.persist(event, persisted_bytes, Some(value))?;
                Ok(SyncOutcome::Superseded)
            }
            ApplyDecision::Apply => {
                self.monitor.apply_remote(value)?;
                self.persist(event, persisted_bytes, Some(value))?;
                debug_assert_eq!(self.ledger.receive(event, now_millis), ApplyDecision::Apply);
                Ok(SyncOutcome::Applied)
            }
        }
    }

    fn persist(
        &self,
        event: &ClipboardEvent,
        bytes: &[u8],
        decoded: Option<&ClipboardValue>,
    ) -> Result<(), SyncError> {
        let (kind, text, blob_hash) = match event.format {
            ClipboardFormat::Text | ClipboardFormat::Html | ClipboardFormat::Rtf => {
                let text = match decoded {
                    Some(ClipboardValue::Text(text)) => text.clone(),
                    _ => std::str::from_utf8(bytes)
                        .map_err(|_| SyncError::InvalidText)?
                        .to_owned(),
                };
                let kind = match event.format {
                    ClipboardFormat::Text => ClipboardKind::Text,
                    ClipboardFormat::Html => ClipboardKind::Html,
                    ClipboardFormat::Rtf => ClipboardKind::Rtf,
                    _ => unreachable!(),
                };
                (kind, Some(text), None)
            }
            ClipboardFormat::Png => {
                let hash = self.blobs.put(bytes)?;
                (ClipboardKind::Image, None, Some(hash.as_str().to_owned()))
            }
            ClipboardFormat::Files => {
                let hash = self.blobs.put(bytes)?;
                (ClipboardKind::Files, None, Some(hash.as_str().to_owned()))
            }
        };
        self.history.insert(&ClipboardEntry {
            id: event.id,
            kind,
            text,
            blob_hash,
            source: ClipboardSource {
                application_id: None,
                device_id: (event.origin != self.local_device).then_some(event.origin),
            },
            created_at: i64::try_from(event.timestamp.physical_millis).unwrap_or(i64::MAX),
            pinned_at: None,
        })?;
        Ok(())
    }
}

/// Result of receiving an authenticated clipboard event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SyncOutcome {
    /// Newest value was written to the OS clipboard and history.
    Applied,
    /// Event had already been processed.
    Duplicate,
    /// Event was persisted in history but lost deterministic conflict resolution.
    Superseded,
    /// Fetch this exact blob before retrying with [`ClipboardSync::receive_blob`].
    NeedsBlob {
        /// Required digest.
        hash: ContentHash,
        /// Required byte length.
        size: u64,
    },
    /// Finish this transfer before retrying with [`ClipboardSync::receive_files`].
    NeedsTransfer {
        /// Required transfer identity.
        id: TransferId,
    },
}

impl SyncOutcome {
    /// Whether the sender may remove the event from its offline replay queue.
    #[must_use]
    pub const fn should_acknowledge(&self) -> bool {
        matches!(self, Self::Applied | Self::Duplicate | Self::Superseded)
    }
}

struct EncodedValue {
    format: ClipboardFormat,
    bytes: Vec<u8>,
}

fn encode_value(value: &ClipboardValue) -> Result<EncodedValue, SyncError> {
    match value {
        ClipboardValue::Text(text) => Ok(EncodedValue {
            format: ClipboardFormat::Text,
            bytes: text.as_bytes().to_vec(),
        }),
        ClipboardValue::Image {
            width,
            height,
            rgba,
        } => {
            value.digest()?;
            let width = u32::try_from(*width).map_err(|_| SyncError::ImageTooLarge)?;
            let height = u32::try_from(*height).map_err(|_| SyncError::ImageTooLarge)?;
            let mut bytes = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut bytes, width, height);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                encoder
                    .write_header()
                    .and_then(|mut writer| writer.write_image_data(rgba))
                    .map_err(|_| SyncError::InvalidImage)?;
            }
            Ok(EncodedValue {
                format: ClipboardFormat::Png,
                bytes,
            })
        }
        ClipboardValue::Files(_) => Err(SyncError::FilesRequireTransfer),
    }
}

fn decode_value(format: ClipboardFormat, bytes: &[u8]) -> Result<ClipboardValue, SyncError> {
    match format {
        ClipboardFormat::Text | ClipboardFormat::Html | ClipboardFormat::Rtf => {
            let text = std::str::from_utf8(bytes).map_err(|_| SyncError::InvalidText)?;
            if text.is_empty() {
                return Err(SyncError::InvalidText);
            }
            Ok(ClipboardValue::Text(text.to_owned()))
        }
        ClipboardFormat::Png => decode_png(bytes),
        ClipboardFormat::Files => Err(SyncError::UnexpectedContent),
    }
}

fn decode_png(bytes: &[u8]) -> Result<ClipboardValue, SyncError> {
    let limits = png::Limits {
        bytes: MAX_DECODED_IMAGE_BYTES,
    };
    let decoder = png::Decoder::new_with_limits(Cursor::new(bytes), limits);
    let mut reader = decoder.read_info().map_err(|_| SyncError::InvalidImage)?;
    let size = reader
        .output_buffer_size()
        .ok_or(SyncError::ImageTooLarge)?;
    if size > MAX_DECODED_IMAGE_BYTES {
        return Err(SyncError::ImageTooLarge);
    }
    let mut rgba = vec![0; size];
    let info = reader
        .next_frame(&mut rgba)
        .map_err(|_| SyncError::InvalidImage)?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return Err(SyncError::InvalidImage);
    }
    rgba.truncate(info.buffer_size());
    let value = ClipboardValue::Image {
        width: info.width as usize,
        height: info.height as usize,
        rgba,
    };
    value.digest()?;
    Ok(value)
}

fn encode_paths(value: &ClipboardValue) -> Vec<u8> {
    let ClipboardValue::Files(paths) = value else {
        return Vec::new();
    };
    let mut bytes = Vec::new();
    for path in paths {
        let path = path.to_string_lossy();
        bytes.extend_from_slice(&path.len().to_le_bytes());
        bytes.extend_from_slice(path.as_bytes());
    }
    bytes
}

/// Clipboard coordination failures.
#[derive(Debug, Error)]
pub enum SyncError {
    /// OS clipboard operation or normalized content validation failed.
    #[error("clipboard platform operation failed")]
    Clipboard(#[from] ClipboardError),
    /// Clipboard history operation failed.
    #[error("clipboard history operation failed")]
    Storage(#[from] StorageError),
    /// Blob filesystem operation failed.
    #[error("clipboard blob operation failed")]
    Blob(#[from] std::io::Error),
    /// Replication ledger rejected a foreign local event.
    #[error("clipboard replication state failed")]
    Replication(#[from] ReplicationError),
    /// Native file copy must first create a transfer manifest.
    #[error("copied files require a transfer manifest")]
    FilesRequireTransfer,
    /// Event shape does not match the supplied resolved content.
    #[error("clipboard event content is unexpected")]
    UnexpectedContent,
    /// Downloaded blob differs from its authenticated event metadata.
    #[error("clipboard content failed its integrity check")]
    ContentMismatch,
    /// Text payload is empty or not UTF-8.
    #[error("clipboard text is invalid")]
    InvalidText,
    /// PNG payload is malformed or not normalized RGBA8.
    #[error("clipboard image is invalid")]
    InvalidImage,
    /// Encoded or decoded image exceeds safety limits.
    #[error("clipboard image exceeds size limits")]
    ImageTooLarge,
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use superspace_protocol::HybridTimestamp;

    use super::*;

    #[derive(Clone, Default)]
    struct MemoryBackend(Rc<RefCell<Option<ClipboardValue>>>);

    impl ClipboardBackend for MemoryBackend {
        fn read(&mut self) -> Result<ClipboardValue, ClipboardError> {
            self.0.borrow().clone().ok_or(ClipboardError::Empty)
        }

        fn write(&mut self, value: &ClipboardValue) -> Result<(), ClipboardError> {
            *self.0.borrow_mut() = Some(value.clone());
            Ok(())
        }
    }

    fn coordinator(
        directory: &tempfile::TempDir,
        id: Uuid,
        backend: MemoryBackend,
    ) -> ClipboardSync<MemoryBackend> {
        ClipboardSync::new(
            id,
            0,
            backend,
            ClipboardStore::memory().expect("history"),
            BlobStore::open(directory.path().join(id.to_string())).expect("blobs"),
        )
    }

    #[test]
    fn text_copies_sync_once_and_acknowledge_offline_queue() {
        let directory = tempfile::tempdir().expect("directory");
        let first_id = Uuid::from_u128(1);
        let second_id = Uuid::from_u128(2);
        let first_backend = MemoryBackend::default();
        *first_backend.0.borrow_mut() = Some(ClipboardValue::Text("Mac to Linux".into()));
        let second_backend = MemoryBackend::default();
        let mut first = coordinator(&directory, first_id, first_backend);
        let mut second = coordinator(&directory, second_id, second_backend.clone());
        let event = first
            .poll_local(10, [second_id], 1_000)
            .expect("poll")
            .expect("event");
        assert_eq!(
            second.receive(&event, 11).expect("receive"),
            SyncOutcome::Applied
        );
        assert_eq!(
            second_backend.0.borrow().as_ref(),
            Some(&ClipboardValue::Text("Mac to Linux".into()))
        );
        assert_eq!(
            second.receive(&event, 12).expect("duplicate"),
            SyncOutcome::Duplicate
        );
        assert_eq!(
            first.pending_for(second_id, 20),
            std::slice::from_ref(&event)
        );
        assert!(first.acknowledge(second_id, event.id));
        assert!(first.pending_for(second_id, 20).is_empty());
    }

    #[test]
    fn images_round_trip_as_png_and_do_not_echo() {
        let directory = tempfile::tempdir().expect("directory");
        let first_backend = MemoryBackend::default();
        let image = ClipboardValue::Image {
            width: 1,
            height: 1,
            rgba: vec![10, 20, 30, 255],
        };
        *first_backend.0.borrow_mut() = Some(image.clone());
        let second_backend = MemoryBackend::default();
        let mut first = coordinator(&directory, Uuid::from_u128(1), first_backend);
        let mut second = coordinator(&directory, Uuid::from_u128(2), second_backend.clone());
        let event = first
            .poll_local(10, [Uuid::from_u128(2)], 1_000)
            .expect("poll")
            .expect("event");
        assert_eq!(event.format, ClipboardFormat::Png);
        assert_eq!(
            second.receive(&event, 11).expect("receive"),
            SyncOutcome::Applied
        );
        assert_eq!(second_backend.0.borrow().as_ref(), Some(&image));
        assert!(
            second
                .poll_local(12, [Uuid::from_u128(1)], 1_000)
                .expect("poll")
                .is_none()
        );
    }

    #[test]
    fn unresolved_and_corrupt_blobs_never_touch_the_clipboard() {
        let directory = tempfile::tempdir().expect("directory");
        let backend = MemoryBackend::default();
        let mut sync = coordinator(&directory, Uuid::from_u128(1), backend.clone());
        let expected = b"large text";
        let event = ClipboardEvent {
            id: Uuid::new_v4(),
            origin: Uuid::from_u128(2),
            timestamp: HybridTimestamp::new(10),
            format: ClipboardFormat::Text,
            content: ClipboardContent::Blob {
                hash: ContentHash::digest(expected),
                size: expected.len() as u64,
            },
        };
        assert!(matches!(
            sync.receive(&event, 10).expect("needs blob"),
            SyncOutcome::NeedsBlob { .. }
        ));
        assert!(sync.receive_blob(&event, b"corruption", 11).is_err());
        assert!(backend.0.borrow().is_none());
        assert_eq!(
            sync.receive_blob(&event, expected, 12).expect("apply"),
            SyncOutcome::Applied
        );
    }
}
