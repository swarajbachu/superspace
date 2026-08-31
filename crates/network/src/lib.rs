//! Peer synchronization state machines, independent of the concrete QUIC transport.

mod discovery;
mod pairing;
mod replication;
mod transfer;
mod transport;

pub use discovery::{DiscoveryError, DiscoveryEvent, NearbyDevice, NearbyDiscovery, SERVICE_TYPE};
pub use pairing::{
    DeviceKeypair, InitiatorFinish, PairingCode, PairingError, PairingInitiator, PairingResponder,
    ResponderFinish, ResponderReply, SecureChannel,
};
pub use replication::{ApplyDecision, PendingEvent, ReplicationError, ReplicationLedger};
pub use transfer::{MAX_CHUNK_SIZE, TransferError, TransferReceiver, read_transfer_chunk};
pub use transport::{
    PeerCertificate, QuicEndpoint, TRANSPORT_SERVER_NAME, TransportError, TransportIdentity,
};
