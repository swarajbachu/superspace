//! Peer synchronization state machines, independent of the concrete QUIC transport.

mod replication;

pub use replication::{ApplyDecision, PendingEvent, ReplicationError, ReplicationLedger};
