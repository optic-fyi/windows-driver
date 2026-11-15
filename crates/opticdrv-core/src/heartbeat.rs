// Copyright (c) 2026 Optic.fyi. All rights reserved.
// SPDX-License-Identifier: LicenseRef-Optic-Source-Available
//
// Source-available, not open source. You may read this file to inspect how Optic
// uses its kernel privileges. All other rights are reserved: no use, copying,
// modification, redistribution, or use in a competing product without written
// permission from Optic.fyi. See the LICENSE file at the repository root.

//! Heartbeat bookkeeping, independent of WDF so it is unit-testable.
//!
//! The service pings ([`Heartbeat::record_ping`]); a 10 s watchdog calls
//! [`Heartbeat::tick`] and learns whether at least one ping arrived since the
//! previous tick. [`MISSED_THRESHOLD`] consecutive misses raise one
//! [`Tick::ThresholdCrossed`]; the driver answers it by persisting a tamper
//! record and writing an event-log entry (see the crate root).

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::ioctl::{DriverStatus, HeartbeatReply, OPTIC_PROTOCOL_VERSION};

/// Consecutive missed intervals that declare the service unreachable.
/// Three intervals at 10 s each: a single slow tick never trips it, a dead
/// service is noticed within 30 s.
pub const MISSED_THRESHOLD: u32 = 3;

/// Result of one watchdog interval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick {
    /// At least one ping arrived during the interval.
    ServiceAlive {
        /// Watchdog tick number that closed this interval.
        sequence: u64,
    },
    /// No ping arrived during the interval.
    ServiceMissed {
        /// Watchdog tick number that closed this interval.
        sequence: u64,
        /// Intervals without a ping since the driver loaded, including this one.
        missed_total: u64,
    },
    /// The [`MISSED_THRESHOLD`]th consecutive miss. Fires once per outage:
    /// further misses without an intervening ping are plain [`Tick::ServiceMissed`].
    ThresholdCrossed {
        /// Watchdog tick number that closed this interval.
        sequence: u64,
        /// Intervals without a ping since the driver loaded, including this one.
        missed_total: u64,
        /// Lifetime threshold crossings, including this one and the seeded base.
        tamper_events: u32,
    },
}

/// Lock-free counters shared by the IOCTL path and the timer path.
#[derive(Debug)]
pub struct Heartbeat {
    pings_this_interval: AtomicU32,
    sequence: AtomicU64,
    missed_total: AtomicU64,
    consecutive_missed: AtomicU32,
    tamper_events: AtomicU32,
}

impl Heartbeat {
    /// A heartbeat with no ticks and no pings recorded.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pings_this_interval: AtomicU32::new(0),
            sequence: AtomicU64::new(0),
            missed_total: AtomicU64::new(0),
            consecutive_missed: AtomicU32::new(0),
            tamper_events: AtomicU32::new(0),
        }
    }

    /// Restores the persisted threshold-crossing count after a reboot.
    /// Call once during startup, before the first [`Heartbeat::tick`].
    pub fn seed_tamper_events(&self, persisted: u32) {
        self.tamper_events.store(persisted, Ordering::Release);
    }

    /// Snapshots the supervision state for [`DriverStatus`] replies.
    #[must_use]
    pub fn status(&self) -> DriverStatus {
        DriverStatus {
            version: OPTIC_PROTOCOL_VERSION,
            tamper_events: self.tamper_events.load(Ordering::Acquire),
            sequence: self.sequence.load(Ordering::Acquire),
            missed_total: self.missed_total.load(Ordering::Acquire),
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
            self.consecutive_missed.store(0, Ordering::Release);
            Tick::ServiceAlive { sequence }
        } else {
            let missed_total = self.missed_total.fetch_add(1, Ordering::AcqRel) + 1;
            let consecutive = self.consecutive_missed.fetch_add(1, Ordering::AcqRel) + 1;
            if consecutive == MISSED_THRESHOLD {
                let tamper_events = self.tamper_events.fetch_add(1, Ordering::AcqRel) + 1;
                Tick::ThresholdCrossed {
                    sequence,
                    missed_total,
                    tamper_events,
                }
            } else {
                Tick::ServiceMissed {
                    sequence,
                    missed_total,
                }
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

    // If this fails: the third consecutive miss does not raise the threshold
    // action, so an unreachable service is never recorded.
    #[test]
    fn third_consecutive_miss_crosses_threshold_once() {
        let hb = Heartbeat::new();
        assert!(matches!(hb.tick(), Tick::ServiceMissed { .. }));
        assert!(matches!(hb.tick(), Tick::ServiceMissed { .. }));
        assert_eq!(
            hb.tick(),
            Tick::ThresholdCrossed {
                sequence: 3,
                missed_total: 3,
                tamper_events: 1
            }
        );
        // Same outage, no second crossing.
        assert!(matches!(hb.tick(), Tick::ServiceMissed { .. }));
        assert_eq!(hb.status().tamper_events, 1);
    }

    // If this fails: a recovered service does not reset the outage detector,
    // so the next lone miss looks like a longer outage than it is.
    #[test]
    fn ping_resets_consecutive_count() {
        let hb = Heartbeat::new();
        hb.tick();
        hb.tick();
        hb.record_ping();
        assert!(matches!(hb.tick(), Tick::ServiceAlive { .. }));
        assert!(matches!(hb.tick(), Tick::ServiceMissed { .. }));
        assert!(matches!(hb.tick(), Tick::ServiceMissed { .. }));
        // Only two consecutive misses: no crossing.
        assert_eq!(hb.status().tamper_events, 0);
    }

    // If this fails: the seed is lost and post-reboot reports undercount outages.
    #[test]
    fn seeded_tamper_events_survive_in_status() {
        let hb = Heartbeat::new();
        hb.seed_tamper_events(7);
        assert_eq!(hb.status().tamper_events, 7);
        hb.tick();
        hb.tick();
        assert_eq!(
            hb.tick(),
            Tick::ThresholdCrossed {
                sequence: 3,
                missed_total: 3,
                tamper_events: 8
            }
        );
    }

    // If this fails: the status snapshot disagrees with the tick counters.
    #[test]
    fn status_reports_counters() {
        let hb = Heartbeat::new();
        hb.record_ping();
        hb.tick();
        hb.tick();
        let status = hb.status();
        assert_eq!(status.version, OPTIC_PROTOCOL_VERSION);
        assert_eq!((status.sequence, status.missed_total), (2, 1));
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
