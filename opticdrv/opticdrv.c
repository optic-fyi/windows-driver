/*
 * OpticDrv: refuses to launch executables on a user-mode-supplied blocklist.
 *
 * One job. The Optic service sends the lowercase basenames of apps that have
 * used up their daily screen time (IOCTL_OPTIC_SET_BLOCKED_IMAGES). A process
 * create notification compares each new process's image basename against that
 * list and fails the creation with STATUS_ACCESS_DENIED on a match.
 *
 * Non-PnP KMDF driver: a single control device \\.\OpticDrv, administrators and
 * SYSTEM only. Loaded as a kernel service by `optic install`.
 *
 * PsSetCreateProcessNotifyRoutineEx requires the image to be linked with
 * /INTEGRITYCHECK (see opticdrv.vcxproj) and signed (test-signing or
 * attestation signing).
 */

#include <ntddk.h>
#include <wdf.h>
#include <wdmsec.h>
#include "ioctl.h"

#define OPTIC_TAG 'ptoO'
#define OPTIC_DEVICE_NAME L"\\Device\\OpticDrv"
#define OPTIC_SYMLINK_NAME L"\\DosDevices\\OpticDrv"

/* The current blocklist. Readers (process-create callback) take the lock shared; the IOCTL takes it exclusive. */
static EX_PUSH_LOCK g_lock;
static OPTIC_BLOCKED_IMAGES g_blocked;

DRIVER_INITIALIZE DriverEntry;
EVT_WDF_DRIVER_UNLOAD OpticEvtUnload;
EVT_WDF_IO_QUEUE_IO_DEVICE_CONTROL OpticEvtIoDeviceControl;

/*
 * True if `basename` equals one of the blocked names, ignoring case.
 * Precondition: g_lock is held (shared or exclusive).
 */
static BOOLEAN IsBlockedLocked(_In_ PCUNICODE_STRING basename) {
    for (unsigned int i = 0; i < g_blocked.count; i++) {
        UNICODE_STRING blocked;
        RtlInitUnicodeString(&blocked, g_blocked.names[i]);
        if (RtlEqualUnicodeString(&blocked, basename, TRUE)) {
            return TRUE;
        }
    }
    return FALSE;
}

/* Final path component of `path` (the whole string if it has no backslash). */
static UNICODE_STRING Basename(_In_ PCUNICODE_STRING path) {
    USHORT chars = path->Length / sizeof(WCHAR);
    USHORT start = chars;
    while (start > 0 && path->Buffer[start - 1] != L'\\') {
        start--;
    }
    UNICODE_STRING result;
    result.Buffer = path->Buffer + start;
    result.Length = (USHORT)((chars - start) * sizeof(WCHAR));
    result.MaximumLength = result.Length;
    return result;
}

/* Runs at PASSIVE_LEVEL in the context of the creating thread, before the new process's first instruction. */
static VOID OpticCreateProcessNotify(
    _Inout_ PEPROCESS Process,
    _In_ HANDLE ProcessId,
    _Inout_opt_ PPS_CREATE_NOTIFY_INFO CreateInfo) {
    UNREFERENCED_PARAMETER(Process);
    UNREFERENCED_PARAMETER(ProcessId);

    /* CreateInfo is NULL when a process exits; ImageFileName can be NULL for some system processes. */
    if (CreateInfo == NULL || CreateInfo->ImageFileName == NULL) {
        return;
    }

    UNICODE_STRING name = Basename(CreateInfo->ImageFileName);

    KeEnterCriticalRegion();
    ExAcquirePushLockShared(&g_lock);
    BOOLEAN blocked = IsBlockedLocked(&name);
    ExReleasePushLockShared(&g_lock);
    KeLeaveCriticalRegion();

    if (blocked) {
        CreateInfo->CreationStatus = STATUS_ACCESS_DENIED;
    }
}

VOID OpticEvtIoDeviceControl(
    _In_ WDFQUEUE Queue,
    _In_ WDFREQUEST Request,
    _In_ size_t OutputBufferLength,
    _In_ size_t InputBufferLength,
    _In_ ULONG IoControlCode) {
    UNREFERENCED_PARAMETER(Queue);
    UNREFERENCED_PARAMETER(OutputBufferLength);

    NTSTATUS status;

    if (IoControlCode != IOCTL_OPTIC_SET_BLOCKED_IMAGES) {
        WdfRequestComplete(Request, STATUS_INVALID_DEVICE_REQUEST);
        return;
    }
    if (InputBufferLength != sizeof(OPTIC_BLOCKED_IMAGES)) {
        WdfRequestComplete(Request, STATUS_INVALID_BUFFER_SIZE);
        return;
    }

    OPTIC_BLOCKED_IMAGES *input = NULL;
    status = WdfRequestRetrieveInputBuffer(Request, sizeof(OPTIC_BLOCKED_IMAGES), (PVOID *)&input, NULL);
    if (!NT_SUCCESS(status)) {
        WdfRequestComplete(Request, status);
        return;
    }
    if (input->count > OPTIC_MAX_BLOCKED) {
        WdfRequestComplete(Request, STATUS_INVALID_PARAMETER);
        return;
    }

    KeEnterCriticalRegion();
    ExAcquirePushLockExclusive(&g_lock);
    RtlCopyMemory(&g_blocked, input, sizeof(g_blocked));
    /* Force termination so the comparison never reads past a name even if user mode sent garbage. */
    for (unsigned int i = 0; i < OPTIC_MAX_BLOCKED; i++) {
        g_blocked.names[i][OPTIC_MAX_NAME - 1] = L'\0';
    }
    ExReleasePushLockExclusive(&g_lock);
    KeLeaveCriticalRegion();

    WdfRequestComplete(Request, STATUS_SUCCESS);
}

VOID OpticEvtUnload(_In_ WDFDRIVER Driver) {
    UNREFERENCED_PARAMETER(Driver);
    PsSetCreateProcessNotifyRoutineEx(OpticCreateProcessNotify, TRUE);
}

NTSTATUS DriverEntry(_In_ PDRIVER_OBJECT DriverObject, _In_ PUNICODE_STRING RegistryPath) {
    WDF_DRIVER_CONFIG config;
    WDF_DRIVER_CONFIG_INIT(&config, WDF_NO_EVENT_CALLBACK);
    config.DriverInitFlags |= WdfDriverInitNonPnpDriver;
    config.EvtDriverUnload = OpticEvtUnload;

    WDFDRIVER driver;
    NTSTATUS status = WdfDriverCreate(DriverObject, RegistryPath, WDF_NO_OBJECT_ATTRIBUTES, &config, &driver);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    ExInitializePushLock(&g_lock);
    RtlZeroMemory(&g_blocked, sizeof(g_blocked));

    PWDFDEVICE_INIT init = WdfControlDeviceInitAllocate(driver, &SDDL_DEVOBJ_SYS_ALL_ADM_ALL);
    if (init == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    DECLARE_CONST_UNICODE_STRING(deviceName, OPTIC_DEVICE_NAME);
    status = WdfDeviceInitAssignName(init, &deviceName);
    if (!NT_SUCCESS(status)) {
        WdfDeviceInitFree(init);
        return status;
    }

    WDFDEVICE device;
    status = WdfDeviceCreate(&init, WDF_NO_OBJECT_ATTRIBUTES, &device);
    if (!NT_SUCCESS(status)) {
        /* WdfDeviceCreate frees and NULLs `init` itself on failure. */
        return status;
    }

    DECLARE_CONST_UNICODE_STRING(symlink, OPTIC_SYMLINK_NAME);
    status = WdfDeviceCreateSymbolicLink(device, &symlink);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    WDF_IO_QUEUE_CONFIG queueConfig;
    WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(&queueConfig, WdfIoQueueDispatchSequential);
    queueConfig.EvtIoDeviceControl = OpticEvtIoDeviceControl;
    WDFQUEUE queue;
    status = WdfIoQueueCreate(device, &queueConfig, WDF_NO_OBJECT_ATTRIBUTES, &queue);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    WdfControlFinishInitializing(device);

    status = PsSetCreateProcessNotifyRoutineEx(OpticCreateProcessNotify, FALSE);
    if (!NT_SUCCESS(status)) {
        return status;
    }
    return STATUS_SUCCESS;
}
