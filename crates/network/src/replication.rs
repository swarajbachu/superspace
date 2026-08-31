use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};

use superspace_protocol::{ClipboardEvent, DeviceId, HybridTimestamp};
use thiserror::Error;
use uuid::Uuid;

/// Whether an inbound event changes the local clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyDecision {
    /// Event is new and newer than the clipboard currently applied.
    Apply,
    /// Event ID was already observed locally or from another peer.
    Duplicate,
    /// Event is valid history but loses deterministic conflict resolution.
    Superseded,
}

/// One event waiting for a peer to acknowledge durable receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingEvent {
    /// Clipboard event to send.
    pub event: ClipboardEvent,
    /// Unix millisecond after which the event may be discarded.
    pub expires_at: i64,
}

/// Replication policy failures.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ReplicationError {
    /// A local event claimed to originate on another device.
    #[error("local clipboard event has a foreign origin")]
    ForeignLocalOrigin,
}

/// Bounded, deterministic clipboard replication ledger.
pub struct ReplicationLedger {
    local_device: DeviceId,
    clock: HybridTimestamp,
    seen: HashSet<Uuid>,
    seen_order: VecDeque<Uuid>,
    current: Option<(HybridTimestamp, DeviceId, Uuid)>,
    pending: HashMap<DeviceId, VecDeque<PendingEvent>>,
    seen_capacity: usize,
}

impl ReplicationLedger {
    /// Create a ledger for one stable device identity.
    #[must_use]
    pub fn new(local_device: DeviceId, physical_millis: u64) -> Self {
        Self {
            local_device,
            clock: HybridTimestamp::new(physical_millis),
            seen: HashSet::new(),
            seen_order: VecDeque::new(),
            current: None,
            pending: HashMap::new(),
            seen_capacity: 20_000,
        }
    }

    /// Record a physical local copy and queue it for selected trusted peers.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::ForeignLocalOrigin`] when the event origin is not this device.
    pub fn record_local(
        &mut self,
        event: &ClipboardEvent,
        peers: impl IntoIterator<Item = DeviceId>,
        expires_at: i64,
    ) -> Result<(), ReplicationError> {
        if event.origin != self.local_device {
            return Err(ReplicationError::ForeignLocalOrigin);
        }
        self.clock = self
            .clock
            .receive(event.timestamp, event.timestamp.physical_millis);
        self.mark_seen(event.id);
        self.current = Some((event.timestamp, event.origin, event.id));
        for peer in peers {
            if peer != self.local_device {
                self.pending
                    .entry(peer)
                    .or_default()
                    .push_back(PendingEvent {
                        event: event.clone(),
                        expires_at,
                    });
            }
        }
        Ok(())
    }

    /// Observe an authenticated remote event and decide whether to apply it.
    ///
    /// Remote events are marked seen even when superseded, so another peer cannot replay them.
    pub fn receive(&mut self, event: &ClipboardEvent, now_millis: u64) -> ApplyDecision {
        self.clock = self.clock.receive(event.timestamp, now_millis);
        let decision = self.preview(event);
        if decision == ApplyDecision::Duplicate {
            return decision;
        }
        self.mark_seen(event.id);
        if decision == ApplyDecision::Apply {
            self.current = Some((event.timestamp, event.origin, event.id));
        }
        decision
    }

    /// Classify an inbound event without mutating replay, clock, or conflict state.
    ///
    /// Coordinators use this before a fallible OS clipboard write, then call [`Self::receive`] only
    /// after the value is successfully applied. Duplicate and superseded events can be committed
    /// immediately.
    #[must_use]
    pub fn preview(&self, event: &ClipboardEvent) -> ApplyDecision {
        if self.seen.contains(&event.id) {
            return ApplyDecision::Duplicate;
        }
        let incoming = (event.timestamp, event.origin, event.id);
        if self
            .current
            .as_ref()
            .is_some_and(|current| compare_event(&incoming, current).is_le())
        {
            ApplyDecision::Superseded
        } else {
            ApplyDecision::Apply
        }
    }

    /// Events still owed to a peer, after pruning expired entries.
    pub fn pending_for(&mut self, peer: DeviceId, now_millis: i64) -> Vec<ClipboardEvent> {
        let queue = self.pending.entry(peer).or_default();
        queue.retain(|pending| pending.expires_at >= now_millis);
        queue.iter().map(|pending| pending.event.clone()).collect()
    }

    /// Remove a peer's acknowledged event from its replay queue.
    #[must_use]
    pub fn acknowledge(&mut self, peer: DeviceId, event_id: Uuid) -> bool {
        let Some(queue) = self.pending.get_mut(&peer) else {
            return false;
        };
        let before = queue.len();
        queue.retain(|pending| pending.event.id != event_id);
        queue.len() != before
    }

    /// Generate a monotonically ordered timestamp for the next local event.
    #[must_use]
    pub fn next_timestamp(&mut self, now_millis: u64) -> HybridTimestamp {
        self.clock = self.clock.tick(now_millis);
        self.clock
    }

    fn mark_seen(&mut self, id: Uuid) {
        if !self.seen.insert(id) {
            return;
        }
        self.seen_order.push_back(id);
        while self.seen_order.len() > self.seen_capacity {
            if let Some(oldest) = self.seen_order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
    }
}

fn compare_event(
    left: &(HybridTimestamp, DeviceId, Uuid),
    right: &(HybridTimestamp, DeviceId, Uuid),
) -> Ordering {
    left.cmp(right)
}

#[cfg(test)]
mod tests {
    use superspace_protocol::{ClipboardContent, ClipboardFormat};

    use super::*;

    fn event(origin: DeviceId, millis: u64, logical: u32, id: Uuid) -> ClipboardEvent {
        ClipboardEvent {
            id,
            origin,
            timestamp: HybridTimestamp {
                physical_millis: millis,
                logical,
            },
            format: ClipboardFormat::Text,
            content: ClipboardContent::Inline {
                bytes: b"hello".to_vec(),
            },
        }
    }

    #[test]
    fn remote_events_apply_once_and_are_never_echo_candidates() {
        let local = Uuid::from_u128(1);
        let remote = Uuid::from_u128(2);
        let id = Uuid::from_u128(3);
        let mut ledger = ReplicationLedger::new(local, 0);
        let event = event(remote, 10, 0, id);
        assert_eq!(ledger.receive(&event, 10), ApplyDecision::Apply);
        assert_eq!(ledger.receive(&event, 11), ApplyDecision::Duplicate);
        assert!(ledger.pending_for(remote, 0).is_empty());
    }

    #[test]
    fn simultaneous_events_converge_by_device_then_event_id() {
        let first_device = Uuid::from_u128(1);
        let second_device = Uuid::from_u128(2);
        let first = event(first_device, 10, 0, Uuid::from_u128(8));
        let second = event(second_device, 10, 0, Uuid::from_u128(7));

        let mut left = ReplicationLedger::new(first_device, 0);
        left.record_local(&first, [second_device], 100)
            .expect("record first");
        assert_eq!(left.receive(&second, 10), ApplyDecision::Apply);

        let mut right = ReplicationLedger::new(second_device, 0);
        right
            .record_local(&second, [first_device], 100)
            .expect("record second");
        assert_eq!(right.receive(&first, 10), ApplyDecision::Superseded);
    }

    #[test]
    fn offline_queue_expires_and_acknowledges() {
        let local = Uuid::from_u128(1);
        let peer = Uuid::from_u128(2);
        let first = event(local, 10, 0, Uuid::from_u128(3));
        let second = event(local, 11, 0, Uuid::from_u128(4));
        let mut ledger = ReplicationLedger::new(local, 0);
        ledger
            .record_local(&first, [peer], 15)
            .expect("record first");
        ledger
            .record_local(&second, [peer], 30)
            .expect("record second");
        let pending = ledger.pending_for(peer, 20);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0], second);
        assert!(ledger.acknowledge(peer, second.id));
        assert!(ledger.pending_for(peer, 20).is_empty());
    }

    #[test]
    fn local_event_must_have_local_origin() {
        let mut ledger = ReplicationLedger::new(Uuid::from_u128(1), 0);
        let foreign = event(Uuid::from_u128(2), 10, 0, Uuid::from_u128(3));
        assert_eq!(
            ledger.record_local(&foreign, [], 100),
            Err(ReplicationError::ForeignLocalOrigin)
        );
    }

    #[test]
    fn preview_does_not_consume_an_event_before_a_fallible_apply() {
        let local = Uuid::from_u128(1);
        let remote = Uuid::from_u128(2);
        let incoming = event(remote, 10, 0, Uuid::from_u128(3));
        let mut ledger = ReplicationLedger::new(local, 0);
        assert_eq!(ledger.preview(&incoming), ApplyDecision::Apply);
        assert_eq!(ledger.preview(&incoming), ApplyDecision::Apply);
        assert_eq!(ledger.receive(&incoming, 10), ApplyDecision::Apply);
        assert_eq!(ledger.preview(&incoming), ApplyDecision::Duplicate);
    }
}
