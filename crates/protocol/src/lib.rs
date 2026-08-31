//! Versioned types shared by Superspace peers.

mod clock;
mod message;

pub use clock::HybridTimestamp;
pub use message::{
    BlobChunk, ClipboardContent, ClipboardEvent, ClipboardFormat, ContentHash, DeviceId,
    DeviceInfo, Message, PROTOCOL_VERSION, ProtocolError, TransferChunk, TransferEntry, TransferId,
    TransferManifest,
};
