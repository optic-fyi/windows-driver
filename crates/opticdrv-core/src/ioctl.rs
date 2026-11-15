//! IOCTL contract between the driver and the user-mode Optic service.
//!
//! The service opens `\\.\OpticDrv` (`LocalSystem` / Administrators only) and calls
//! `DeviceIoControl(IOCTL_OPTIC_HEARTBEAT)` with no input and a
//! [`HeartbeatReply`]-sized output buffer at least once every
//! [`HEARTBEAT_INTERVAL_MS`].

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

/// A buffer that is not exactly [`HeartbeatReply::WIRE_SIZE`] bytes long.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WrongLength {
    /// The length that was supplied.
    pub actual: usize,
}

/// Reads the little-endian `u64` at `at..at + 8` of a reply buffer.
fn le_u64(bytes: &[u8; HeartbeatReply::WIRE_SIZE], at: usize) -> u64 {
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
    }
}
