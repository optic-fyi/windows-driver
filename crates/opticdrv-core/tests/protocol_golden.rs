// Copyright (c) 2026 Wilson Brous, Ayden Yang, Carter Alfers and Andre Hamghalam,
// trading as Optic.fyi. All rights reserved.
// SPDX-License-Identifier: LicenseRef-Optic-Source-Available
//
// Source-available, not open source. You may read this file to inspect how Optic
// uses its kernel privileges. All other rights are reserved: no use, copying,
// modification, redistribution, or use in a competing product without written
// permission from the copyright holders. See the LICENSE file at the repository root.

//! Pins the service <-> driver wire contract. The user-mode service decodes
//! these exact bytes; changing them is a breaking protocol change.

use opticdrv_core::ioctl::{
    DriverStatus, HeartbeatReply, IOCTL_OPTIC_DRIVER_STATUS, IOCTL_OPTIC_HEARTBEAT,
    OPTIC_PROTOCOL_VERSION,
};

// If this fails: the heartbeat IOCTL number changed and deployed services get
// STATUS_INVALID_DEVICE_REQUEST. Bump the protocol on purpose or revert.
#[test]
fn heartbeat_ioctl_number_is_stable() {
    assert_eq!(IOCTL_OPTIC_HEARTBEAT, 0x0022_6000);
}

// If this fails: the status IOCTL number changed; same breakage as above.
#[test]
fn status_ioctl_number_is_stable() {
    assert_eq!(IOCTL_OPTIC_DRIVER_STATUS, 0x0022_6004);
}

// If this fails: the reply layout changed and the service reads garbage.
#[test]
fn reply_golden_bytes() {
    let reply = HeartbeatReply {
        sequence: 3,
        missed: 1,
    };
    let golden: [u8; 16] = [3, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0];
    assert_eq!(reply.to_bytes(), golden);
    assert_eq!(HeartbeatReply::from_bytes(&golden), Ok(reply));
}

// If this fails: the status layout changed and the service reads garbage.
#[test]
fn status_golden_bytes() {
    let status = DriverStatus {
        version: OPTIC_PROTOCOL_VERSION,
        tamper_events: 2,
        sequence: 100,
        missed_total: 5,
    };
    let golden: [u8; 24] = [
        1, 0, 0, 0, 2, 0, 0, 0, 100, 0, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0, 0, 0, 0, 0,
    ];
    assert_eq!(status.to_bytes(), golden);
    assert_eq!(DriverStatus::from_bytes(&golden), Ok(status));
}
