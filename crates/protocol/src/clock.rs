use serde::{Deserialize, Serialize};

/// A hybrid logical timestamp used for deterministic clipboard ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HybridTimestamp {
    /// Unix milliseconds observed by the creator.
    pub physical_millis: u64,
    /// Logical counter for events sharing or preceding that physical time.
    pub logical: u32,
}

impl HybridTimestamp {
    /// Construct the first event at a physical timestamp.
    #[must_use]
    pub const fn new(physical_millis: u64) -> Self {
        Self {
            physical_millis,
            logical: 0,
        }
    }

    /// Advance for a local event observed at `now_millis`.
    #[must_use]
    pub fn tick(self, now_millis: u64) -> Self {
        if now_millis > self.physical_millis {
            Self::new(now_millis)
        } else {
            Self {
                logical: self.logical.saturating_add(1),
                ..self
            }
        }
    }

    /// Merge a remote timestamp and advance for receipt at `now_millis`.
    #[must_use]
    pub fn receive(self, remote: Self, now_millis: u64) -> Self {
        let physical_millis = self
            .physical_millis
            .max(remote.physical_millis)
            .max(now_millis);
        let logical = if physical_millis == self.physical_millis
            && physical_millis == remote.physical_millis
        {
            self.logical.max(remote.logical).saturating_add(1)
        } else if physical_millis == self.physical_millis {
            self.logical.saturating_add(1)
        } else if physical_millis == remote.physical_millis {
            remote.logical.saturating_add(1)
        } else {
            0
        };
        Self {
            physical_millis,
            logical,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_ticks_never_move_backwards() {
        let stamp = HybridTimestamp {
            physical_millis: 100,
            logical: 4,
        };
        assert_eq!(
            stamp.tick(99),
            HybridTimestamp {
                physical_millis: 100,
                logical: 5
            }
        );
        assert_eq!(stamp.tick(101), HybridTimestamp::new(101));
    }

    #[test]
    fn receive_orders_simultaneous_events() {
        let local = HybridTimestamp {
            physical_millis: 100,
            logical: 2,
        };
        let remote = HybridTimestamp {
            physical_millis: 100,
            logical: 7,
        };
        assert_eq!(
            local.receive(remote, 90),
            HybridTimestamp {
                physical_millis: 100,
                logical: 8
            }
        );
    }
}
