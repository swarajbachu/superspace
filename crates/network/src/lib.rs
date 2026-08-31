//! Peer synchronization state machines, independent of the concrete QUIC transport.

mod blob;
mod discovery;
mod frame;
mod identity;
mod pairing;
mod pairing_session;
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
pub use pairing_session::{
    PairedPeer, PairingPublicInfo, PairingSessionError, pair_incoming, pair_outgoing,
};
pub use replication::{ApplyDecision, PendingEvent, ReplicationError, ReplicationLedger};
pub use session::{
    BlobRequest, BlobSessionError, ClipboardOffer, IncomingPeerRequest, IncomingRequestError,
    PeerSessionError, TransferCancellation, TransferRequest, TransferSessionError,
    exchange_hello_incoming, exchange_hello_outgoing, offer_clipboard, receive_clipboard_offer,
    receive_peer_request, receive_transfer, receive_transfer_with_progress, request_blob,
    send_transfer, send_transfer_with_progress, serve_blob,
};
pub use transfer::{
    MAX_CHUNK_SIZE, PreparedTransfer, TransferError, TransferProgress, TransferReceiver,
    prepare_transfer, read_transfer_chunk,
};
pub use transport::{
    PeerCertificate, QuicEndpoint, TRANSPORT_SERVER_NAME, TransportError, TransportIdentity,
};
