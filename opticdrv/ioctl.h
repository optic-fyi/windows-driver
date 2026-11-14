#pragma once

/*
 * IOCTL interface of OpticDrv, shared with the user-mode service.
 * Keep in sync with any user-mode client that sends this IOCTL (none yet).
 */

#define FILE_DEVICE_OPTIC 0x8001

#define OPTIC_IOCTL_BASE 0x800

/* Replace the list of executables the driver refuses to launch. */
#define IOCTL_OPTIC_SET_BLOCKED_IMAGES \
    CTL_CODE(FILE_DEVICE_OPTIC, OPTIC_IOCTL_BASE + 0, METHOD_BUFFERED, FILE_WRITE_ACCESS)

#define OPTIC_MAX_BLOCKED 128
#define OPTIC_MAX_NAME 64

#pragma pack(push, 1)
/*
 * `count` NUL-terminated lowercase executable basenames (e.g. L"chrome.exe").
 * Input length must equal sizeof(OPTIC_BLOCKED_IMAGES) exactly.
 */
typedef struct _OPTIC_BLOCKED_IMAGES {
    unsigned int count;
    wchar_t names[OPTIC_MAX_BLOCKED][OPTIC_MAX_NAME];
} OPTIC_BLOCKED_IMAGES;
#pragma pack(pop)
