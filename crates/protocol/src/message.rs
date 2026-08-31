use std::fmt;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::HybridTimestamp;

/// Current peer protocol version.
pub const PROTOCOL_VERSION: u16 = 1;

/// Stable device identity.
pub type DeviceId = Uuid;
/// Stable file-transfer identity.
pub type TransferId = Uuid;

/// BLAKE3 digest used for content verification and deduplication.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    /// Hash content bytes.
    #[must_use]
    pub fn digest(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// Access the digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hexadecimal representation used for content-addressed filenames.
    #[must_use]
    pub fn to_hex(self) -> String {
        blake3::Hash::from_bytes(self.0).to_hex().to_string()
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "ContentHash({})",
            blake3::Hash::from_bytes(self.0).to_hex()
        )
    }
}

/// Public peer metadata announced after authentication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    /// Stable identity.
    pub id: DeviceId,
    /// User-selected display name.
    pub name: String,
    /// Operating system identifier.
    pub platform: String,
    /// Protocol versions accepted by the device.
    pub protocol_versions: Vec<u16>,
}

/// Clipboard formats synchronized by Superspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardFormat {
    /// UTF-8 plain text.
    Text,
    /// UTF-8 HTML fragment.
    Html,
    /// Rich Text Format data.
    Rtf,
    /// PNG image bytes.
    Png,
    /// A transfer manifest whose completed entries become local file URLs.
    Files,
}

/// Clipboard payload, inline for small content and hash-addressed for large content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "storage", rename_all = "snake_case")]
pub enum ClipboardContent {
    /// Inline bytes carried by the event.
    Inline {
        /// Uncompressed payload.
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
    },
    /// Blob transferred separately.
    Blob {
        /// Expected payload digest.
        hash: ContentHash,
        /// Expected byte length.
        size: u64,
    },
    /// A file transfer that must complete before the clipboard is applied.
    Transfer {
        /// Associated transfer.
        transfer_id: TransferId,
    },
}

/// One ordered clipboard event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardEvent {
    /// Globally unique event identity.
    pub id: Uuid,
    /// Device where the physical copy occurred.
    pub origin: DeviceId,
    /// Deterministic ordering timestamp.
    pub timestamp: HybridTimestamp,
    /// Clipboard representation.
    pub format: ClipboardFormat,
    /// Data or transfer reference.
    pub content: ClipboardContent,
}

/// A safe relative path included in a file transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferEntry {
    /// Slash-separated path relative to the transfer root.
    pub relative_path: String,
    /// Uncompressed file length.
    pub size: u64,
    /// Complete-file digest.
    pub hash: ContentHash,
}

impl TransferEntry {
    /// Validate that an entry cannot escape its destination root.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::UnsafePath`] for empty, absolute, or traversing paths.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let path = Path::new(&self.relative_path);
        if self.relative_path.is_empty()
            || path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(ProtocolError::UnsafePath(self.relative_path.clone()));
        }
        Ok(())
    }
}

/// Metadata sent before streaming transfer chunks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferManifest {
    /// Transfer identity.
    pub id: TransferId,
    /// Device that initiated the transfer.
    pub origin: DeviceId,
    /// Top-level display name.
    pub name: String,
    /// Files in deterministic path order.
    pub entries: Vec<TransferEntry>,
}

/// One bounded, ordered piece of a transferred file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferChunk {
    /// Owning transfer.
    pub transfer_id: TransferId,
    /// Index into [`TransferManifest::entries`].
    pub entry_index: u32,
    /// Byte offset where this chunk starts.
    pub offset: u64,
    /// Raw file bytes.
    #[serde(with = "serde_bytes")]
    pub bytes: Vec<u8>,
}

/// One bounded segment of a content-addressed clipboard blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlobChunk {
    /// Digest announced by the clipboard event.
    pub hash: ContentHash,
    /// Byte offset where this segment begins.
    pub offset: u64,
    /// Raw segment bytes.
    #[serde(with = "serde_bytes")]
    pub bytes: Vec<u8>,
    /// True only for the segment ending at the announced blob length.
    pub complete: bool,
}

impl TransferManifest {
    /// Validate all untrusted manifest fields before touching the filesystem.
    ///
    /// # Errors
    ///
    /// Returns an error when the name or entry list is empty or an entry path is unsafe.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.name.trim().is_empty() {
            return Err(ProtocolError::MissingName);
        }
        if self.entries.is_empty() {
            return Err(ProtocolError::EmptyTransfer);
        }
        for entry in &self.entries {
            entry.validate()?;
        }
        Ok(())
    }
}

/// Top-level authenticated peer message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    /// Announces peer metadata and negotiates protocol compatibility.
    Hello(DeviceInfo),
    /// Offers a clipboard event.
    Clipboard(ClipboardEvent),
    /// Requests a content-addressed clipboard blob from a resumable byte offset.
    BlobRequest {
        /// Requested content digest.
        hash: ContentHash,
        /// Receiver's verified partial byte count.
        offset: u64,
    },
    /// Streams one bounded clipboard blob segment.
    BlobChunk(BlobChunk),
    /// Announces a file transfer.
    TransferOffer(TransferManifest),
    /// Streams one file segment after an accepted offer.
    TransferChunk(TransferChunk),
    /// Requests continuation from receiver-persisted byte offsets.
    TransferResume {
        /// Transfer being resumed.
        id: TransferId,
        /// One byte offset per manifest entry.
        offsets: Vec<u64>,
    },
    /// Acknowledges durable receipt of an event or transfer.
    Acknowledge {
        /// Event or transfer identifier.
        id: Uuid,
    },
    /// Cancels an in-progress transfer.
    CancelTransfer {
        /// Transfer to cancel.
        id: TransferId,
    },
}

/// Rejection raised before untrusted protocol data reaches a service.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProtocolError {
    /// A transfer entry attempted to escape its destination.
    #[error("unsafe transfer path: {0}")]
    UnsafePath(String),
    /// Transfer display name was empty.
    #[error("transfer name is required")]
    MissingName,
    /// Transfer included no files.
    #[error("transfer has no entries")]
    EmptyTransfer,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_is_deterministic() {
        assert_eq!(
            ContentHash::digest(b"superspace"),
            ContentHash::digest(b"superspace")
        );
        assert_ne!(
            ContentHash::digest(b"superspace"),
            ContentHash::digest(b"tinyspace")
        );
    }

    #[test]
    fn transfer_rejects_traversal_and_absolute_paths() {
        for path in ["../secret", "/etc/passwd", "folder/../../secret", ""] {
            let entry = TransferEntry {
                relative_path: path.into(),
                size: 0,
                hash: ContentHash::digest(&[]),
            };
            assert!(matches!(
                entry.validate(),
                Err(ProtocolError::UnsafePath(_))
            ));
        }
    }

    #[test]
    fn message_wire_shape_is_versionable() {
        let message = Message::Acknowledge { id: Uuid::nil() };
        let json = serde_json::to_string(&message).expect("serialize message");
        assert_eq!(
            json,
            r#"{"type":"acknowledge","id":"00000000-0000-0000-0000-000000000000"}"#
        );
    }
}
