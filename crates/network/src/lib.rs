//! Peer synchronization state machines, independent of the concrete QUIC transport.

mod blob;
mod discovery;
mod frame;
mod pairing;
mod replication;
mod session;
mod transfer;
mod transport;

pub use blob::{BlobReceiver, BlobTransferError, read_blob_chunk};
pub use discovery::{DiscoveryError, DiscoveryEvent, NearbyDevice, NearbyDiscovery, SERVICE_TYPE};
pub use frame::{FrameError, MAX_FRAME_SIZE, decode_frame, encode_frame, read_frame, write_frame};
pub use pairing::{
    DeviceKeypair, InitiatorFinish, PairingCode, PairingError, PairingInitiator, PairingResponder,
    ResponderFinish, ResponderReply, SecureChannel,
};
pub use replication::{ApplyDecision, PendingEvent, ReplicationError, ReplicationLedger};
pub use session::{
    BlobSessionError, TransferSessionError, receive_transfer, request_blob, send_transfer,
    serve_blob,
};
pub use transfer::{MAX_CHUNK_SIZE, TransferError, TransferReceiver, read_transfer_chunk};
pub use transport::{
    PeerCertificate, QuicEndpoint, TRANSPORT_SERVER_NAME, TransportError, TransportIdentity,
};
