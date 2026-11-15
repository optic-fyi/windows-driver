// Copyright (c) 2026 Wilson Brous, Ayden Yang, Carter Alfers and Andre Hamghalam,
// trading as Optic.fyi. All rights reserved.
// SPDX-License-Identifier: LicenseRef-Optic-Source-Available
//
// Source-available, not open source. You may read this file to inspect how Optic
// uses its kernel privileges. All other rights are reserved: no use, copying,
// modification, redistribution, or use in a competing product without written
// permission from the copyright holders. See the LICENSE file at the repository root.

//! Property tests: the heartbeat counters obey their invariants under any
//! interleaving of pings and watchdog ticks.

use opticdrv_core::heartbeat::{Heartbeat, MISSED_THRESHOLD, Tick};
use proptest::prelude::*;

#[derive(Clone, Copy, Debug)]
enum Event {
    Ping,
    Tick,
}

fn events() -> impl Strategy<Value = Vec<Event>> {
    prop::collection::vec(prop_oneof![Just(Event::Ping), Just(Event::Tick)], 0..200)
}

proptest! {
    // If this fails: a tick is classified wrongly (alive without a ping, or
    // missed despite one), so the driver misreports the service's health.
    #[test]
    fn tick_is_alive_iff_a_ping_arrived_since_the_previous_tick(events in events()) {
        let hb = Heartbeat::new();
        let mut pinged = false;
        for event in events {
            match event {
                Event::Ping => { hb.record_ping(); pinged = true; }
                Event::Tick => {
                    let alive = matches!(hb.tick(), Tick::ServiceAlive { .. });
                    prop_assert_eq!(alive, pinged);
                    pinged = false;
                }
            }
        }
    }

    // If this fails: threshold crossings fire more than once per outage, so the
    // tamper count over-reports and the driver spams the event log.
    #[test]
    fn threshold_crossings_are_bounded_by_miss_runs(events in events()) {
        use Event::{Ping, Tick as TickEvent};
        let hb = Heartbeat::new();
        let (mut crossings, mut ticks) = (0u32, 0u32);
        for event in events {
            match event {
                Ping => {
                    hb.record_ping();
                }
                TickEvent => {
                    ticks += 1;
                    if matches!(hb.tick(), Tick::ThresholdCrossed { .. }) {
                        crossings += 1;
                    }
                }
            }
        }
        // Each crossing consumes at least MISSED_THRESHOLD miss ticks.
        prop_assert!(crossings * MISSED_THRESHOLD <= ticks);
        prop_assert_eq!(crossings, hb.status().tamper_events);
    }

    // If this fails: the sequence or missed counters drift, so replies sent to
    // the service contradict what the watchdog logged.
    #[test]
    fn replies_report_exact_tick_and_miss_counts(events in events()) {
        let hb = Heartbeat::new();
        let (mut ticks, mut misses, mut pinged) = (0u64, 0u64, false);
        for event in events {
            match event {
                Event::Ping => {
                    let reply = hb.record_ping();
                    prop_assert_eq!((reply.sequence, reply.missed), (ticks, misses));
                    pinged = true;
                }
                Event::Tick => {
                    ticks += 1;
                    if !pinged { misses += 1; }
                    pinged = false;
                    hb.tick();
                }
            }
        }
        prop_assert!(misses <= ticks);
    }
}
