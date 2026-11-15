//! Heartbeat bookkeeping, independent of WDF so it is unit-testable.
//!
//! The service pings ([`Heartbeat::record_ping`]); a 10 s watchdog calls
//! [`Heartbeat::tick`] and learns whether at least one ping arrived since the
//! previous tick.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::ioctl::HeartbeatReply;

/// Result of one watchdog interval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick {
    /// At least one ping arrived during the interval.
    ServiceAlive { sequence: u64 },
    /// No ping arrived during the interval.
    ServiceMissed { sequence: u64, missed_total: u64 },
}

/// Lock-free counters shared by the IOCTL path and the timer path.
#[derive(Debug)]
pub struct Heartbeat {
    pings_this_interval: AtomicU32,
    sequence: AtomicU64,
    missed_total: AtomicU64,
}

impl Heartbeat {
    /// A heartbeat with no ticks and no pings recorded.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pings_this_interval: AtomicU32::new(0),
            sequence: AtomicU64::new(0),
            missed_total: AtomicU64::new(0),
        }
    }

    /// Records a ping from the service and returns the current state for the reply.
    pub fn record_ping(&self) -> HeartbeatReply {
        self.pings_this_interval.fetch_add(1, Ordering::AcqRel);
        HeartbeatReply {
            sequence: self.sequence.load(Ordering::Acquire),
            missed: self.missed_total.load(Ordering::Acquire),
        }
    }

    /// Closes the current interval. Call once per heartbeat interval, from one thread.
    pub fn tick(&self) -> Tick {
        let sequence = self.sequence.fetch_add(1, Ordering::AcqRel) + 1;
        if self.pings_this_interval.swap(0, Ordering::AcqRel) > 0 {
            Tick::ServiceAlive { sequence }
        } else {
            let missed_total = self.missed_total.fetch_add(1, Ordering::AcqRel) + 1;
            Tick::ServiceMissed {
                sequence,
                missed_total,
            }
        }
    }
}

impl Default for Heartbeat {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // If this fails: a silent service is reported as alive and nobody notices it died.
    #[test]
    fn tick_without_ping_is_missed() {
        let hb = Heartbeat::new();
        assert_eq!(
            hb.tick(),
            Tick::ServiceMissed {
                sequence: 1,
                missed_total: 1
            }
        );
    }

    // If this fails: a healthy service is reported as missing.
    #[test]
    fn tick_after_ping_is_alive() {
        let hb = Heartbeat::new();
        hb.record_ping();
        assert_eq!(hb.tick(), Tick::ServiceAlive { sequence: 1 });
    }

    // If this fails: one old ping keeps the service "alive" forever.
    #[test]
    fn ping_only_covers_one_interval() {
        let hb = Heartbeat::new();
        hb.record_ping();
        hb.record_ping();
        assert_eq!(hb.tick(), Tick::ServiceAlive { sequence: 1 });
        assert_eq!(
            hb.tick(),
            Tick::ServiceMissed {
                sequence: 2,
                missed_total: 1
            }
        );
    }

    // If this fails: the reply reports stale counters to the service.
    #[test]
    fn reply_reflects_ticks_and_misses() {
        let hb = Heartbeat::new();
        hb.tick();
        hb.tick();
        assert_eq!(
            hb.record_ping(),
            HeartbeatReply {
                sequence: 2,
                missed: 2
            }
        );
    }
}
