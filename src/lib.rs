//! # opticdrv
//!
//! Non-PnP KMDF driver that exposes `\\.\OpticDrv` and supervises the
//! Optic user-mode service:
//!
//! * the service pings [`IOCTL_OPTIC_HEARTBEAT`][hb] at least every 10 s and
//!   queries [`IOCTL_OPTIC_DRIVER_STATUS`][st] for supervision counters;
//! * a periodic WDF timer fires every 10 s; three consecutive misses persist
//!   a tamper record and write an event-log entry.
//!
//! [hb]: opticdrv_core::ioctl::IOCTL_OPTIC_HEARTBEAT
//! [st]: opticdrv_core::ioctl::IOCTL_OPTIC_DRIVER_STATUS
//!
//! Only the service may talk to the device: the device ACL admits
//! `LocalSystem` and Administrators, and every request is additionally
//! checked for the installed service image path.
//!
//! Design notes, including what this driver deliberately does NOT do, live in
//! `README.md` ("Supervision design").

#![no_std]

extern crate alloc;

extern crate wdk_panic;

use core::{
    mem::size_of,
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};

use opticdrv_core::{
    heartbeat::{Heartbeat, Tick},
    ioctl::{HEARTBEAT_INTERVAL_MS, IOCTL_OPTIC_DRIVER_STATUS, IOCTL_OPTIC_HEARTBEAT},
    wide::ascii_to_utf16,
};
use wdk::println;
use wdk_alloc::WdkAllocator;
use wdk_sys::{
    _WDF_DRIVER_INIT_FLAGS, _WDF_EXECUTION_LEVEL, _WDF_IO_QUEUE_DISPATCH_TYPE,
    _WDF_SYNCHRONIZATION_SCOPE, _WDF_TRI_STATE, DRIVER_OBJECT, HANDLE, IO_ERROR_LOG_PACKET,
    KEY_READ, KEY_WRITE, NTSTATUS, PCUNICODE_STRING, PEPROCESS, PUNICODE_STRING, PVOID, ULONG,
    UNICODE_STRING, WDF_DRIVER_CONFIG, WDF_IO_QUEUE_CONFIG, WDF_NO_HANDLE,
    WDF_NO_OBJECT_ATTRIBUTES, WDF_OBJECT_ATTRIBUTES, WDF_TIMER_CONFIG, WDFDEVICE, WDFDRIVER,
    WDFKEY, WDFOBJECT, WDFQUEUE, WDFREQUEST, WDFTIMER, call_unsafe_wdf_function_binding,
    ntddk::{
        ExFreePool, IoAllocateErrorLogEntry, IoWriteErrorLogEntry, ObfDereferenceObject,
        PsLookupProcessByProcessId, SeLocateProcessImageName,
    },
};

#[global_allocator]
static GLOBAL_ALLOCATOR: WdkAllocator = WdkAllocator;

/// Shared by the IOCTL path and the watchdog timer.
static HEARTBEAT: Heartbeat = Heartbeat::new();

/// The `DRIVER_OBJECT` from entry, for error-log entries. Written once at
/// entry, before any IOCTL or timer can run; valid for driver lifetime.
static DRIVER_OBJECT_PTR: AtomicPtr<DRIVER_OBJECT> = AtomicPtr::new(core::ptr::null_mut());

/// The `WDFDRIVER` handle, for registry access from the timer path. Written
/// once at entry; valid for driver lifetime.
static WDF_DRIVER_HANDLE: AtomicUsize = AtomicUsize::new(0);

const STATUS_SUCCESS: NTSTATUS = 0;
/// `0xC0000010` as a signed `NTSTATUS`.
const STATUS_INVALID_DEVICE_REQUEST: NTSTATUS = -1_073_741_808;
/// `0xC0000022` as a signed `NTSTATUS`.
const STATUS_ACCESS_DENIED: NTSTATUS = -1_073_741_790;
/// `FILE_DEVICE_SECURE_OPEN`.
const FILE_DEVICE_SECURE_OPEN: ULONG = 0x0000_0100;

/// Driver-specific event code logged when the service goes unreachable.
/// Visible in Event Viewer under the `opticdrv` source.
const OPTIC_EVENT_SERVICE_UNREACHABLE: NTSTATUS = 0x0000_1001;

/// Tail of the service's NT image path. The volume prefix varies per machine
/// (`\Device\HarddiskVolumeN\...`), so only this lowercase tail is compared,
/// ASCII case-insensitive.
const SERVICE_IMAGE_SUFFIX: &str = "\\program files\\optic\\optic.exe";

/// Declares `static $name: [u16; _]` holding `$text` as UTF-16 (no terminator).
macro_rules! wide {
    ($name:ident, $text:literal) => {
        static $name: [u16; $text.len()] = ascii_to_utf16::<{ $text.len() }>($text);
    };
}

wide!(DEVICE_NAME, "\\Device\\OpticDrv");
wide!(SYMLINK_NAME, "\\DosDevices\\OpticDrv");
wide!(REG_TAMPER_EVENTS, "TamperEvents");
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

/// The `Size` field WDF expects for a config struct. Every WDF struct is far
/// below `ULONG::MAX` bytes; the assert proves it at compile time, so the
/// cast below cannot truncate. (This is the pattern from Microsoft's own
/// `sample-kmdf-driver`: clippy cannot evaluate the assert, hence the allow.)
#[allow(clippy::cast_possible_truncation)]
const fn wdf_struct_size<T>() -> ULONG {
    const {
        assert!(size_of::<T>() <= ULONG::MAX as usize);
    }
    size_of::<T>() as ULONG
}

/// `WDF_OBJECT_ATTRIBUTES_INIT` (an inline function in the headers, so not bound).
fn object_attributes() -> WDF_OBJECT_ATTRIBUTES {
    WDF_OBJECT_ATTRIBUTES {
        Size: wdf_struct_size::<WDF_OBJECT_ATTRIBUTES>(),
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
    DRIVER_OBJECT_PTR.store(driver as *mut DRIVER_OBJECT, Ordering::Release);
    let mut config = WDF_DRIVER_CONFIG {
        Size: wdf_struct_size::<WDF_DRIVER_CONFIG>(),
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
            &raw mut config,
            &raw mut wdf_driver,
        )
    };
    if !wdk::nt_success(status) {
        println!("opticdrv: WdfDriverCreate failed: {status:#x}");
        return status;
    }
    WDF_DRIVER_HANDLE.store(wdf_driver as usize, Ordering::Release);

    // Best-effort: a missing key just means no outage was ever recorded.
    HEARTBEAT.seed_tamper_events(read_tamper_events(wdf_driver));

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
    let mut init = unsafe {
        call_unsafe_wdf_function_binding!(WdfControlDeviceInitAllocate, driver, &raw const sddl)
    };
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
    check(unsafe {
        call_unsafe_wdf_function_binding!(WdfDeviceInitAssignName, init, &raw const name)
    })?;

    let mut device: WDFDEVICE = core::ptr::null_mut();
    // SAFETY: `init` is valid (the framework consumes it on success or failure)
    // and `device` is a valid out pointer.
    check(unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceCreate,
            &raw mut init,
            WDF_NO_OBJECT_ATTRIBUTES,
            &raw mut device,
        )
    })?;

    let symlink = unicode_string(&SYMLINK_NAME);
    // SAFETY: `device` was just created; `symlink` outlives the call.
    check(unsafe {
        call_unsafe_wdf_function_binding!(WdfDeviceCreateSymbolicLink, device, &raw const symlink)
    })?;

    let mut queue_config = WDF_IO_QUEUE_CONFIG {
        Size: wdf_struct_size::<WDF_IO_QUEUE_CONFIG>(),
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
            &raw mut queue_config,
            WDF_NO_OBJECT_ATTRIBUTES,
            WDF_NO_HANDLE.cast::<WDFQUEUE>(),
        )
    })?;

    let mut timer_config = WDF_TIMER_CONFIG {
        Size: wdf_struct_size::<WDF_TIMER_CONFIG>(),
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
            &raw mut timer_config,
            &raw mut timer_attributes,
            &raw mut timer,
        )
    })?;

    // Relative due time: negative, in 100 ns units.
    let due_time = -i64::from(HEARTBEAT_INTERVAL_MS) * 10_000;
    // SAFETY: `timer` was just created. A fresh timer is never already
    // running; the return only matters when restarting a live timer.
    let _was_running = unsafe { call_unsafe_wdf_function_binding!(WdfTimerStart, timer, due_time) };

    // SAFETY: `device` is fully configured; this opens it to I/O.
    unsafe {
        call_unsafe_wdf_function_binding!(WdfControlFinishInitializing, device);
    }
    Ok(())
}

/// Watchdog: once per interval, checks whether the service pinged. On the
/// threshold-crossing tick the outage is persisted and reported.
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
        Tick::ThresholdCrossed {
            sequence,
            missed_total,
            tamper_events,
        } => {
            println!(
                "opticdrv: heartbeat #{sequence}: service UNREACHABLE                  ({missed_total} misses, tamper record #{tamper_events})"
            );
            persist_tamper_events(tamper_events);
            // Saturating: the event id only needs to be unique-ish, and a
            // sequence past u32::MAX must not wrap onto an earlier one.
            write_service_unreachable_event(u32::try_from(sequence).unwrap_or(u32::MAX));
        }
    }
}

/// Handles the service IOCTLs. The caller is verified first: anything but
/// the service gets `STATUS_ACCESS_DENIED`, unknown codes get
/// `STATUS_INVALID_DEVICE_REQUEST`.
extern "C" fn evt_io_device_control(
    _queue: WDFQUEUE,
    request: WDFREQUEST,
    _output_buffer_length: usize,
    _input_buffer_length: usize,
    io_control_code: ULONG,
) {
    if !verify_caller(request) {
        complete(request, STATUS_ACCESS_DENIED, 0);
        return;
    }
    if io_control_code == IOCTL_OPTIC_HEARTBEAT {
        complete_with_bytes(request, &HEARTBEAT.record_ping().to_bytes());
    } else if io_control_code == IOCTL_OPTIC_DRIVER_STATUS {
        complete_with_bytes(request, &HEARTBEAT.status().to_bytes());
    } else {
        complete(request, STATUS_INVALID_DEVICE_REQUEST, 0);
    }
}

/// Copies `bytes` into the request's output buffer and completes it.
/// A too-small caller buffer fails with the framework's status.
fn complete_with_bytes(request: WDFREQUEST, bytes: &[u8]) {
    let mut buffer: PVOID = core::ptr::null_mut();
    // SAFETY: `request` is a live request owned by this callback; the out
    // pointer is valid and the optional length pointer is null.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfRequestRetrieveOutputBuffer,
            request,
            bytes.len(),
            &raw mut buffer,
            core::ptr::null_mut::<usize>(),
        )
    };
    if !wdk::nt_success(status) {
        complete(request, status, 0);
        return;
    }
    // SAFETY: WDF guarantees `buffer` is writable for at least `bytes.len()`
    // bytes (the minimum requested above); `bytes` is a live Rust slice.
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.cast::<u8>(), bytes.len());
    }
    complete(request, STATUS_SUCCESS, bytes.len() as u64);
}

/// True only when the request comes from the installed Optic service: the
/// calling process's NT image path must end with [`SERVICE_IMAGE_SUFFIX`].
/// A renamed copy elsewhere fails the path; running code from the installed
/// image requires injecting into the service process itself. (The
/// short-name/session-id helpers are not exposed by `wdk-sys`, so the full
/// path query is used instead.)
fn verify_caller(request: WDFREQUEST) -> bool {
    // SAFETY: `request` is a live request owned by this callback.
    let pid: u32 =
        unsafe { call_unsafe_wdf_function_binding!(WdfRequestGetRequestorProcessId, request) };
    let mut process: PEPROCESS = core::ptr::null_mut();
    let pid_handle: HANDLE = pid as HANDLE;
    // SAFETY: `pid` came from WDF; on success we own one reference, released below.
    let status = unsafe { PsLookupProcessByProcessId(pid_handle, &raw mut process) };
    if !wdk::nt_success(status) || process.is_null() {
        return false;
    }
    let mut image_name: PUNICODE_STRING = core::ptr::null_mut();
    // SAFETY: `process` is referenced; on success we own the string, freed below.
    let status = unsafe { SeLocateProcessImageName(process, &raw mut image_name) };
    // SAFETY: `image_name` is either null or the owned string from the call above.
    let ok = wdk::nt_success(status) && unsafe { image_path_matches(image_name) };
    if !image_name.is_null() {
        // SAFETY: balances the allocation from `SeLocateProcessImageName`.
        unsafe {
            ExFreePool(image_name.cast());
        }
    }
    // SAFETY: balances the reference from `PsLookupProcessByProcessId`.
    unsafe {
        ObfDereferenceObject(process.cast());
    }
    ok
}

/// True when the image path ends with [`SERVICE_IMAGE_SUFFIX`], ASCII
/// case-insensitive. A null or short path is false, never a fault.
///
/// # Safety
/// `name` must be null or point to a valid `UNICODE_STRING`.
unsafe fn image_path_matches(name: PUNICODE_STRING) -> bool {
    if name.is_null() {
        return false;
    }
    // SAFETY: `name` is non-null and valid by contract.
    let length = unsafe { (*name).Length as usize / size_of::<u16>() };
    // SAFETY: same as above; the buffer is read next, bounds-checked.
    let buffer = unsafe { (*name).Buffer };
    if buffer.is_null() || length < SERVICE_IMAGE_SUFFIX.len() {
        return false;
    }
    let suffix = SERVICE_IMAGE_SUFFIX.as_bytes();
    let mut i = 0;
    while i < suffix.len() {
        // SAFETY: `length - suffix.len() + i < length`, inside the buffer.
        let unit = unsafe { *buffer.add(length - suffix.len() + i) };
        if unit >= 0x80 || unit.to_le_bytes()[0].to_ascii_lowercase() != suffix[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Reads the persisted threshold-crossing count from the driver's
/// `Parameters` registry key. Best-effort: any failure (missing key on a
/// first install included) yields 0.
fn read_tamper_events(driver: WDFDRIVER) -> u32 {
    let Ok(key) = open_parameters_key(driver) else {
        return 0;
    };
    let name = unicode_string(&REG_TAMPER_EVENTS);
    let mut value: ULONG = 0;
    // SAFETY: `key` is open; `name` outlives the call; `value` is a valid out pointer.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfRegistryQueryULong,
            key,
            &raw const name,
            &raw mut value
        )
    };
    close_key(key);
    if wdk::nt_success(status) { value } else { 0 }
}

/// Persists the threshold-crossing count. Best-effort: failures only log.
/// The driver handle comes from the entry-time stash because the timer path
/// receives no driver handle of its own.
fn persist_tamper_events(tamper_events: u32) {
    let driver = WDF_DRIVER_HANDLE.load(Ordering::Acquire) as WDFDRIVER;
    if driver.is_null() {
        return;
    }
    let key = open_parameters_key(driver).inspect_err(|status| {
        println!("opticdrv: cannot open Parameters key: {status:#x}");
    });
    let Ok(key) = key else {
        return;
    };
    let name = unicode_string(&REG_TAMPER_EVENTS);
    // SAFETY: `key` is open and `name` outlives the call.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfRegistryAssignULong,
            key,
            &raw const name,
            tamper_events
        )
    };
    close_key(key);
    if !wdk::nt_success(status) {
        println!("opticdrv: cannot persist tamper record: {status:#x}");
    }
}

/// Opens (creating if needed) `HKLM\...\Services\opticdrv\Parameters`.
fn open_parameters_key(driver: WDFDRIVER) -> Result<WDFKEY, NTSTATUS> {
    let mut key: WDFKEY = core::ptr::null_mut();
    // SAFETY: `driver` is the live entry-time handle; `key` is a valid out pointer.
    check(unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDriverOpenParametersRegistryKey,
            driver,
            KEY_READ | KEY_WRITE,
            WDF_NO_OBJECT_ATTRIBUTES,
            &raw mut key,
        )
    })?;
    Ok(key)
}

/// Closes a key from [`open_parameters_key`].
fn close_key(key: WDFKEY) {
    // SAFETY: `key` is open and not used after this call.
    unsafe {
        call_unsafe_wdf_function_binding!(WdfRegistryClose, key);
    }
}

/// Writes one `opticdrv` event-log entry for a service outage. Best-effort:
/// allocation failure or a missing driver object silently skips the entry.
/// The I/O manager owns (and frees) the packet after the write call.
fn write_service_unreachable_event(unique: u32) {
    let driver = DRIVER_OBJECT_PTR.load(Ordering::Acquire);
    if driver.is_null() {
        return;
    }
    let size = size_of::<IO_ERROR_LOG_PACKET>();
    let Ok(size_u8) = u8::try_from(size) else {
        return;
    };
    let driver_ptr: PVOID = driver.cast();
    // SAFETY: the driver object is valid for driver lifetime; the size fits `UCHAR`.
    let entry = unsafe { IoAllocateErrorLogEntry(driver_ptr, size_u8) };
    if entry.is_null() {
        return;
    }
    // SAFETY: `entry` is our fresh packet; zeroing `size` bytes is exact.
    unsafe {
        entry.cast::<u8>().write_bytes(0, size);
    }
    // SAFETY: `entry` is a valid packet; the borrow ends before the write call below.
    let packet = unsafe { &mut *entry.cast::<IO_ERROR_LOG_PACKET>() };
    packet.ErrorCode = OPTIC_EVENT_SERVICE_UNREACHABLE;
    packet.UniqueErrorValue = unique;
    // SAFETY: `entry` was just allocated and filled; ownership passes to the I/O manager.
    unsafe {
        IoWriteErrorLogEntry(entry);
    }
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
