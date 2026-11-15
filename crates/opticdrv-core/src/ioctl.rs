// Copyright (c) 2026 Wilson Brous, Ayden Yang, Carter Alfers and Andre Hamghalam,
// trading as Optic.fyi. All rights reserved.
// SPDX-License-Identifier: LicenseRef-Optic-Source-Available
//
// Source-available, not open source. You may read this file to inspect how Optic
// uses its kernel privileges. All other rights are reserved: no use, copying,
// modification, redistribution, or use in a competing product without written
// permission from the copyright holders. See the LICENSE file at the repository root.

//! IOCTL contract between the driver and the user-mode Optic service.
//!
//! The service opens `\\.\OpticDrv` and pings
//! [`IOCTL_OPTIC_HEARTBEAT`] at least once every [`HEARTBEAT_INTERVAL_MS`].
//! [`IOCTL_OPTIC_DRIVER_STATUS`] reports the driver's supervision state.
//! Neither call takes input; each fills a fixed-size output buffer.
//!
//! Access is restricted twice: the device ACL admits only `LocalSystem` and
//! Administrators, and the driver additionally verifies that the calling
//! process runs from the installed service image. Anything else gets
//! `STATUS_ACCESS_DENIED`.

/// `FILE_DEVICE_UNKNOWN`.
pub const FILE_DEVICE_OPTIC: u32 = 0x22;
/// `METHOD_BUFFERED`.
const METHOD_BUFFERED: u32 = 0;
/// `FILE_READ_ACCESS`.
const FILE_READ_ACCESS: u32 = 1;

/// How often the driver expects a heartbeat from the service, in milliseconds.
pub const HEARTBEAT_INTERVAL_MS: u32 = 10_000;

/// Equivalent of the `CTL_CODE` macro from `winioctl.h`.
///
/// Contract: `function` must fit in 12 bits (`< 0x1000`).
///
/// # Panics
/// If `function >= 0x1000` (a compile error when used in a `const`).
#[must_use]
pub const fn ctl_code(device_type: u32, function: u32, method: u32, access: u32) -> u32 {
    assert!(function < 0x1000, "CTL_CODE function is a 12-bit field");
    (device_type << 16) | (access << 14) | (function << 2) | method
}

/// Service -> driver ping. Output: one [`HeartbeatReply`]. No input.
pub const IOCTL_OPTIC_HEARTBEAT: u32 =
    ctl_code(FILE_DEVICE_OPTIC, 0x800, METHOD_BUFFERED, FILE_READ_ACCESS);

/// Service -> driver supervision query. Output: one [`DriverStatus`]. No input.
pub const IOCTL_OPTIC_DRIVER_STATUS: u32 =
    ctl_code(FILE_DEVICE_OPTIC, 0x801, METHOD_BUFFERED, FILE_READ_ACCESS);

/// Protocol version spoken by this driver. It rides in every [`DriverStatus`];
/// the service must log (and distrust the counters of) any other version.
pub const OPTIC_PROTOCOL_VERSION: u32 = 1;

/// Reply to [`IOCTL_OPTIC_HEARTBEAT`].
///
/// Wire format ([`HeartbeatReply::WIRE_SIZE`] bytes, little-endian):
/// `sequence: u64` at offset 0, `missed: u64` at offset 8.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeartbeatReply {
    /// Watchdog ticks (one per [`HEARTBEAT_INTERVAL_MS`]) since the driver loaded.
    pub sequence: u64,
    /// Total ticks since load in which no ping arrived from the service.
    pub missed: u64,
}

/// Status snapshot of the driver's service supervision.
///
/// Wire format ([`DriverStatus::WIRE_SIZE`] bytes, little-endian):
/// `version: u32` at offset 0, `tamper_events: u32` at offset 4,
/// `sequence: u64` at offset 8, `missed_total: u64` at offset 16.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DriverStatus {
    /// [`OPTIC_PROTOCOL_VERSION`] of the answering driver.
    pub version: u32,
    /// Threshold crossings recorded (lifetime total, survives reboots).
    pub tamper_events: u32,
    /// Watchdog ticks since the driver loaded.
    pub sequence: u64,
    /// Total ticks since load in which no ping arrived.
    pub missed_total: u64,
}

/// A buffer whose length does not match the expected wire size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WrongLength {
    /// The length that was supplied.
    pub actual: usize,
}

/// Reads the little-endian `u32` at `at..at + 4` of a reply buffer.
fn le_u32<const N: usize>(bytes: &[u8; N], at: usize) -> u32 {
    let mut word = [0u8; 4];
    word.copy_from_slice(&bytes[at..at + 4]);
    u32::from_le_bytes(word)
}

/// Reads the little-endian `u64` at `at..at + 8` of a reply buffer.
fn le_u64<const N: usize>(bytes: &[u8; N], at: usize) -> u64 {
    let mut word = [0u8; 8];
    word.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(word)
}

impl HeartbeatReply {
    /// Size of the encoded reply in bytes.
    pub const WIRE_SIZE: usize = 16;

    /// Encodes the reply in its wire format.
    #[must_use]
    pub fn to_bytes(self) -> [u8; Self::WIRE_SIZE] {
        let mut out = [0u8; Self::WIRE_SIZE];
        out[..8].copy_from_slice(&self.sequence.to_le_bytes());
        out[8..].copy_from_slice(&self.missed.to_le_bytes());
        out
    }

    /// Decodes a reply.
    ///
    /// # Errors
    /// [`WrongLength`] unless `bytes.len() == WIRE_SIZE`: a short or long
    /// buffer means the peer speaks a different protocol version.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, WrongLength> {
        let bytes: &[u8; Self::WIRE_SIZE] = bytes.try_into().map_err(|_| WrongLength {
            actual: bytes.len(),
        })?;
        Ok(Self {
            sequence: le_u64(bytes, 0),
            missed: le_u64(bytes, 8),
        })
    }
}

impl DriverStatus {
    /// Size of the encoded status in bytes.
    pub const WIRE_SIZE: usize = 24;

    /// Encodes the status in its wire format.
    #[must_use]
    pub fn to_bytes(self) -> [u8; Self::WIRE_SIZE] {
        let mut out = [0u8; Self::WIRE_SIZE];
        out[..4].copy_from_slice(&self.version.to_le_bytes());
        out[4..8].copy_from_slice(&self.tamper_events.to_le_bytes());
        out[8..16].copy_from_slice(&self.sequence.to_le_bytes());
        out[16..].copy_from_slice(&self.missed_total.to_le_bytes());
        out
    }

    /// Decodes a status.
    ///
    /// # Errors
    /// [`WrongLength`] unless `bytes.len() == WIRE_SIZE`.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, WrongLength> {
        let bytes: &[u8; Self::WIRE_SIZE] = bytes.try_into().map_err(|_| WrongLength {
            actual: bytes.len(),
        })?;
        Ok(Self {
            version: le_u32(bytes, 0),
            tamper_events: le_u32(bytes, 4),
            sequence: le_u64(bytes, 8),
            missed_total: le_u64(bytes, 16),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // If this fails: the IOCTL code no longer matches what the service sends,
    // so every heartbeat is rejected with STATUS_INVALID_DEVICE_REQUEST.
    #[test]
    fn heartbeat_code_matches_ctl_code_macro() {
        // CTL_CODE(0x22, 0x800, METHOD_BUFFERED, FILE_READ_ACCESS)
        assert_eq!(IOCTL_OPTIC_HEARTBEAT, 0x0022_6000);
    }

    // If this fails: the status IOCTL no longer matches what the service sends.
    #[test]
    fn status_code_matches_ctl_code_macro() {
        // CTL_CODE(0x22, 0x801, METHOD_BUFFERED, FILE_READ_ACCESS)
        assert_eq!(IOCTL_OPTIC_DRIVER_STATUS, 0x0022_6004);
    }

    // If this fails: an out-of-range function number silently corrupts the access bits.
    #[test]
    #[should_panic(expected = "12-bit")]
    fn ctl_code_rejects_wide_function() {
        let _ = ctl_code(FILE_DEVICE_OPTIC, 0x1000, METHOD_BUFFERED, FILE_READ_ACCESS);
    }

    // If this fails: the byte layout the service parses has changed.
    #[test]
    fn reply_encodes_little_endian_fields_in_order() {
        let bytes = HeartbeatReply {
            sequence: 0x0102,
            missed: 0x0304,
        }
        .to_bytes();
        assert_eq!(bytes[..8], [0x02, 0x01, 0, 0, 0, 0, 0, 0]);
        assert_eq!(bytes[8..], [0x04, 0x03, 0, 0, 0, 0, 0, 0]);
    }

    // If this fails: truncated or oversized replies are accepted as valid.
    #[test]
    fn from_bytes_rejects_every_length_but_sixteen() {
        for len in [0, 1, 15, 17, 32] {
            assert_eq!(
                HeartbeatReply::from_bytes(&[0u8; 32][..len]),
                Err(WrongLength { actual: len })
            );
        }
    }

    // If this fails: the status layout the service parses has changed.
    #[test]
    fn status_encodes_fields_in_order() {
        let bytes = DriverStatus {
            version: 1,
            tamper_events: 2,
            sequence: 3,
            missed_total: 4,
        }
        .to_bytes();
        assert_eq!(bytes[..4], [1, 0, 0, 0]);
        assert_eq!(bytes[4..8], [2, 0, 0, 0]);
        assert_eq!(bytes[8..16], [3, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(bytes[16..], [4, 0, 0, 0, 0, 0, 0, 0]);
    }

    // If this fails: malformed status buffers are accepted as valid.
    #[test]
    fn status_from_bytes_rejects_wrong_lengths() {
        for len in [0, 16, 23, 25] {
            assert_eq!(
                DriverStatus::from_bytes(&[0u8; 32][..len]),
                Err(WrongLength { actual: len })
            );
        }
    }

    // If this fails: extreme counter values do not survive the wire.
    #[test]
    fn roundtrip_preserves_extreme_values() {
        for reply in [
            HeartbeatReply {
                sequence: 0,
                missed: 0,
            },
            HeartbeatReply {
                sequence: u64::MAX,
                missed: u64::MAX,
            },
        ] {
            assert_eq!(HeartbeatReply::from_bytes(&reply.to_bytes()), Ok(reply));
        }
        let status = DriverStatus {
            version: u32::MAX,
            tamper_events: u32::MAX,
            sequence: u64::MAX,
            missed_total: u64::MAX,
        };
        assert_eq!(DriverStatus::from_bytes(&status.to_bytes()), Ok(status));
    }
}
