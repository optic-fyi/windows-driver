//! # opticdrv
//!
//! Non-PnP KMDF driver that exposes `\\.\OpticDrv` and keeps a 10 second
//! heartbeat with the Optic user-mode service:
//!
//! * the service sends [`ioctl::IOCTL_OPTIC_HEARTBEAT`] at least every 10 s;
//! * a periodic WDF timer fires every 10 s and logs whether a ping arrived.
//!
//! It enforces nothing yet.

#![no_std]

extern crate alloc;

extern crate wdk_panic;

use core::mem::size_of;

use opticdrv_core::{
    heartbeat::{Heartbeat, Tick},
    ioctl::{HEARTBEAT_INTERVAL_MS, HeartbeatReply, IOCTL_OPTIC_HEARTBEAT},
    wide::ascii_to_utf16,
};
use wdk::println;
use wdk_alloc::WdkAllocator;
use wdk_sys::{
    _WDF_DRIVER_INIT_FLAGS, _WDF_EXECUTION_LEVEL, _WDF_IO_QUEUE_DISPATCH_TYPE,
    _WDF_SYNCHRONIZATION_SCOPE, _WDF_TRI_STATE, DRIVER_OBJECT, NTSTATUS, PCUNICODE_STRING, PVOID,
    ULONG, UNICODE_STRING, WDF_DRIVER_CONFIG, WDF_IO_QUEUE_CONFIG, WDF_NO_HANDLE,
    WDF_NO_OBJECT_ATTRIBUTES, WDF_OBJECT_ATTRIBUTES, WDF_TIMER_CONFIG, WDFDEVICE, WDFDRIVER,
    WDFOBJECT, WDFQUEUE, WDFREQUEST, WDFTIMER, call_unsafe_wdf_function_binding,
};

#[global_allocator]
static GLOBAL_ALLOCATOR: WdkAllocator = WdkAllocator;

/// Shared by the IOCTL path and the watchdog timer.
static HEARTBEAT: Heartbeat = Heartbeat::new();

const STATUS_SUCCESS: NTSTATUS = 0;
/// `0xC0000010` as a signed `NTSTATUS`.
const STATUS_INVALID_DEVICE_REQUEST: NTSTATUS = -1_073_741_808;
/// `FILE_DEVICE_SECURE_OPEN`.
const FILE_DEVICE_SECURE_OPEN: ULONG = 0x0000_0100;

/// Declares `static $name: [u16; _]` holding `$text` as UTF-16 (no terminator).
macro_rules! wide {
    ($name:ident, $text:literal) => {
        static $name: [u16; $text.len()] = ascii_to_utf16::<{ $text.len() }>($text);
    };
}

wide!(DEVICE_NAME, "\\Device\\OpticDrv");
wide!(SYMLINK_NAME, "\\DosDevices\\OpticDrv");
// SYSTEM and Administrators only: the service runs as LocalSystem.
wide!(DEVICE_SDDL, "D:P(A;;GA;;;SY)(A;;GA;;;BA)");

/// Borrows a static UTF-16 buffer as a `UNICODE_STRING`.
///
/// Contract: the buffer is `'static`, so the returned pointer never dangles;
/// the kernel only reads through it.
fn unicode_string(buffer: &'static [u16]) -> UNICODE_STRING {
    let bytes = u16::try_from(core::mem::size_of_val(buffer))
        .expect("UNICODE_STRING buffers are at most 65535 bytes");
    UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        Buffer: buffer.as_ptr().cast_mut(),
    }
}

/// `WDF_OBJECT_ATTRIBUTES_INIT` (an inline function in the headers, so not bound).
fn object_attributes() -> WDF_OBJECT_ATTRIBUTES {
    WDF_OBJECT_ATTRIBUTES {
        Size: size_of::<WDF_OBJECT_ATTRIBUTES>() as ULONG,
        ExecutionLevel: _WDF_EXECUTION_LEVEL::WdfExecutionLevelInheritFromParent,
        SynchronizationScope: _WDF_SYNCHRONIZATION_SCOPE::WdfSynchronizationScopeInheritFromParent,
        ..WDF_OBJECT_ATTRIBUTES::default()
    }
}

/// Driver entry point.
///
/// # Safety
/// Called once by the kernel with a valid driver object and registry path.
// SAFETY: "DriverEntry" is the symbol the kernel loader looks up; nothing else exports it.
#[unsafe(export_name = "DriverEntry")]
pub unsafe extern "system" fn driver_entry(
    driver: &mut DRIVER_OBJECT,
    registry_path: PCUNICODE_STRING,
) -> NTSTATUS {
    let mut config = WDF_DRIVER_CONFIG {
        Size: size_of::<WDF_DRIVER_CONFIG>() as ULONG,
        DriverInitFlags: _WDF_DRIVER_INIT_FLAGS::WdfDriverInitNonPnpDriver as ULONG,
        ..WDF_DRIVER_CONFIG::default()
    };
    let mut wdf_driver: WDFDRIVER = core::ptr::null_mut();

    // SAFETY: `driver` and `registry_path` come from the kernel, `config` is a
    // fully initialised WDF_DRIVER_CONFIG, and `wdf_driver` is a valid out pointer.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDriverCreate,
            driver as *mut DRIVER_OBJECT,
            registry_path,
            WDF_NO_OBJECT_ATTRIBUTES,
            &mut config,
            &mut wdf_driver,
        )
    };
    if !wdk::nt_success(status) {
        println!("opticdrv: WdfDriverCreate failed: {status:#x}");
        return status;
    }

    match create_control_device(wdf_driver) {
        Ok(()) => {
            println!("opticdrv: started");
            STATUS_SUCCESS
        }
        Err(status) => {
            println!("opticdrv: control device setup failed: {status:#x}");
            status
        }
    }
}

/// Maps an `NTSTATUS` to `Err` unless it indicates success.
fn check(status: NTSTATUS) -> Result<(), NTSTATUS> {
    if wdk::nt_success(status) {
        Ok(())
    } else {
        Err(status)
    }
}

/// Creates `\Device\OpticDrv`, its DOS symlink, the IOCTL queue and the watchdog timer.
///
/// Postcondition on `Ok`: the device accepts IOCTLs and the timer is running.
/// On `Err` the framework deletes any partially created objects at driver unload.
fn create_control_device(driver: WDFDRIVER) -> Result<(), NTSTATUS> {
    let sddl = unicode_string(&DEVICE_SDDL);
    // SAFETY: `driver` is the handle returned by WdfDriverCreate; `sddl` outlives the call.
    let mut init =
        unsafe { call_unsafe_wdf_function_binding!(WdfControlDeviceInitAllocate, driver, &sddl) };
    if init.is_null() {
        return Err(-1_073_741_670); // STATUS_INSUFFICIENT_RESOURCES (0xC000009A)
    }

    // SAFETY: `init` is the non-null init structure just allocated.
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceInitSetCharacteristics,
            init,
            FILE_DEVICE_SECURE_OPEN,
            0u8
        );
    }

    let name = unicode_string(&DEVICE_NAME);
    // SAFETY: `init` is valid and `name` outlives the call.
    check(unsafe { call_unsafe_wdf_function_binding!(WdfDeviceInitAssignName, init, &name) })?;

    let mut device: WDFDEVICE = core::ptr::null_mut();
    // SAFETY: `init` is valid (the framework consumes it on success or failure)
    // and `device` is a valid out pointer.
    check(unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceCreate,
            &mut init,
            WDF_NO_OBJECT_ATTRIBUTES,
            &mut device,
        )
    })?;

    let symlink = unicode_string(&SYMLINK_NAME);
    // SAFETY: `device` was just created; `symlink` outlives the call.
    check(unsafe {
        call_unsafe_wdf_function_binding!(WdfDeviceCreateSymbolicLink, device, &symlink)
    })?;

    let mut queue_config = WDF_IO_QUEUE_CONFIG {
        Size: size_of::<WDF_IO_QUEUE_CONFIG>() as ULONG,
        DispatchType: _WDF_IO_QUEUE_DISPATCH_TYPE::WdfIoQueueDispatchParallel,
        PowerManaged: _WDF_TRI_STATE::WdfUseDefault,
        DefaultQueue: 1,
        EvtIoDeviceControl: Some(evt_io_device_control),
        ..WDF_IO_QUEUE_CONFIG::default()
    };
    // SAFETY: `device` is valid, `queue_config` is initialised, the optional
    // queue handle out parameter is null.
    check(unsafe {
        call_unsafe_wdf_function_binding!(
            WdfIoQueueCreate,
            device,
            &mut queue_config,
            WDF_NO_OBJECT_ATTRIBUTES,
            WDF_NO_HANDLE.cast::<WDFQUEUE>(),
        )
    })?;

    let mut timer_config = WDF_TIMER_CONFIG {
        Size: size_of::<WDF_TIMER_CONFIG>() as ULONG,
        EvtTimerFunc: Some(evt_watchdog_timer),
        Period: HEARTBEAT_INTERVAL_MS,
        AutomaticSerialization: 0,
        ..WDF_TIMER_CONFIG::default()
    };
    let mut timer_attributes = object_attributes();
    timer_attributes.ParentObject = device as WDFOBJECT;
    let mut timer: WDFTIMER = core::ptr::null_mut();
    // SAFETY: configs are initialised, the parent is the live device, `timer` is a valid out pointer.
    check(unsafe {
        call_unsafe_wdf_function_binding!(
            WdfTimerCreate,
            &mut timer_config,
            &mut timer_attributes,
            &mut timer,
        )
    })?;

    // Relative due time: negative, in 100 ns units.
    let due_time = -i64::from(HEARTBEAT_INTERVAL_MS) * 10_000;
    // SAFETY: `timer` was just created.
    unsafe {
        call_unsafe_wdf_function_binding!(WdfTimerStart, timer, due_time);
    }

    // SAFETY: `device` is fully configured; this opens it to I/O.
    unsafe {
        call_unsafe_wdf_function_binding!(WdfControlFinishInitializing, device);
    }
    Ok(())
}

/// Watchdog: once per interval, logs whether the service pinged.
extern "C" fn evt_watchdog_timer(_timer: WDFTIMER) {
    match HEARTBEAT.tick() {
        Tick::ServiceAlive { sequence } => {
            println!("opticdrv: heartbeat #{sequence}: service alive");
        }
        Tick::ServiceMissed {
            sequence,
            missed_total,
        } => {
            println!("opticdrv: heartbeat #{sequence}: service MISSED ({missed_total} total)");
        }
    }
}

/// Handles `IOCTL_OPTIC_HEARTBEAT`; every other code is rejected.
extern "C" fn evt_io_device_control(
    _queue: WDFQUEUE,
    request: WDFREQUEST,
    _output_buffer_length: usize,
    _input_buffer_length: usize,
    io_control_code: ULONG,
) {
    if io_control_code != IOCTL_OPTIC_HEARTBEAT {
        complete(request, STATUS_INVALID_DEVICE_REQUEST, 0);
        return;
    }

    let mut buffer: PVOID = core::ptr::null_mut();
    // SAFETY: `request` is a live request owned by this callback; the out
    // pointer is valid and the optional length pointer is null.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfRequestRetrieveOutputBuffer,
            request,
            HeartbeatReply::WIRE_SIZE,
            &mut buffer,
            core::ptr::null_mut::<usize>(),
        )
    };
    if !wdk::nt_success(status) {
        complete(request, status, 0);
        return;
    }

    let reply = HEARTBEAT.record_ping().to_bytes();
    // SAFETY: WDF guarantees `buffer` is writable for at least
    // `HeartbeatReply::WIRE_SIZE` bytes (the minimum requested above); a byte
    // array has alignment 1, so no alignment requirement applies.
    unsafe {
        buffer
            .cast::<[u8; HeartbeatReply::WIRE_SIZE]>()
            .write(reply)
    };
    complete(request, STATUS_SUCCESS, HeartbeatReply::WIRE_SIZE as u64);
}

/// Completes `request` with `status` and `information` bytes transferred.
fn complete(request: WDFREQUEST, status: NTSTATUS, information: u64) {
    // SAFETY: every caller passes a live request it has not completed yet.
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfRequestCompleteWithInformation,
            request,
            status,
            information,
        );
    }
}
