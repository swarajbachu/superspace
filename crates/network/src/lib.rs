//! Peer synchronization state machines, independent of the concrete QUIC transport.

mod blob;
mod discovery;
mod frame;
mod identity;
mod pairing;
mod replication;
mod session;
mod transfer;
mod transport;

pub use blob::{BlobReceiver, BlobTransferError, read_blob_chunk};
pub use discovery::{DiscoveryError, DiscoveryEvent, NearbyDevice, NearbyDiscovery, SERVICE_TYPE};
pub use frame::{FrameError, MAX_FRAME_SIZE, decode_frame, encode_frame, read_frame, write_frame};
pub use identity::{IdentityStoreError, LocalIdentity};
pub use pairing::{
    DeviceKeypair, InitiatorFinish, PairingCode, PairingError, PairingInitiator, PairingResponder,
    ResponderFinish, ResponderReply, SecureChannel,
};
pub use replication::{ApplyDecision, PendingEvent, ReplicationError, ReplicationLedger};
pub use session::{
    BlobSessionError, ClipboardOffer, PeerSessionError, TransferCancellation, TransferSessionError,
    exchange_hello_incoming, exchange_hello_outgoing, offer_clipboard, receive_clipboard_offer,
    receive_transfer, receive_transfer_with_progress, request_blob, send_transfer,
    send_transfer_with_progress, serve_blob,
};
pub use transfer::{
    MAX_CHUNK_SIZE, TransferError, TransferProgress, TransferReceiver, read_transfer_chunk,
};
pub use transport::{
    PeerCertificate, QuicEndpoint, TRANSPORT_SERVER_NAME, TransportError, TransportIdentity,
};
