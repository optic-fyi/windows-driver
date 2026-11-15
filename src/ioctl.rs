//! IOCTL contract between the driver and the user-mode Optic service.
//!
//! The service opens `\\.\OpticDrv` (LocalSystem / Administrators only) and calls
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
/// Contract: `function` must fit in 12 bits (`< 0x1000`); panics otherwise
/// (at compile time when used in a `const`).
#[must_use]
pub const fn ctl_code(device_type: u32, function: u32, method: u32, access: u32) -> u32 {
    assert!(function < 0x1000, "CTL_CODE function is a 12-bit field");
    (device_type << 16) | (access << 14) | (function << 2) | method
}

/// Service -> driver ping. Output: one [`HeartbeatReply`]. No input.
pub const IOCTL_OPTIC_HEARTBEAT: u32 =
    ctl_code(FILE_DEVICE_OPTIC, 0x800, METHOD_BUFFERED, FILE_READ_ACCESS);

/// Reply to [`IOCTL_OPTIC_HEARTBEAT`]. Little-endian, 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeartbeatReply {
    /// Watchdog ticks (one per [`HEARTBEAT_INTERVAL_MS`]) since the driver loaded.
    pub sequence: u64,
    /// Total ticks since load in which no ping arrived from the service.
    pub missed: u64,
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

    // If this fails: the wire format changed and the service reads garbage.
    #[test]
    fn reply_is_sixteen_bytes() {
        assert_eq!(core::mem::size_of::<HeartbeatReply>(), 16);
    }

    // If this fails: an out-of-range function number silently corrupts the access bits.
    #[test]
    #[should_panic(expected = "12-bit")]
    fn ctl_code_rejects_wide_function() {
        let _ = ctl_code(FILE_DEVICE_OPTIC, 0x1000, METHOD_BUFFERED, FILE_READ_ACCESS);
    }
}
