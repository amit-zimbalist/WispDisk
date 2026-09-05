#pragma once

#include <ntdef.h>

#define WISPDISK_PROTOCOL_VERSION 1UL
#define WISPDISK_SRB_SIGNATURE "WISPDK01"
#define WISPDISK_SRB_SIGNATURE_LENGTH 8UL
#define WISPDISK_MAXIMUM_DISK_COUNT 16UL

#define WISPDISK_CAP_CREATE_DELETE 0x00000001UL
#define WISPDISK_CAP_READ_WRITE 0x00000002UL
#define WISPDISK_CAP_REMOVABLE 0x00000004UL

typedef enum _WISPDISK_CONTROL_CODE {
    WispDiskControlQueryVersion = 0x800,
    WispDiskControlCreateDisk = 0x801,
    WispDiskControlDeleteDisk = 0x802,
    WispDiskControlListDisks = 0x803,
} WISPDISK_CONTROL_CODE;

typedef enum _WISPDISK_MEDIA_KIND {
    WispDiskMediaFixed = 1,
    WispDiskMediaRemovable = 2,
} WISPDISK_MEDIA_KIND;

typedef struct _WISPDISK_REQUEST_HEADER {
    ULONG StructSize;
    ULONG ProtocolVersion;
    ULONG Operation;
    ULONG Flags;
    ULONGLONG CorrelationId;
} WISPDISK_REQUEST_HEADER, *PWISPDISK_REQUEST_HEADER;

typedef struct _WISPDISK_RESPONSE_HEADER {
    ULONG StructSize;
    ULONG ProtocolVersion;
    ULONG Operation;
    LONG Status;
    ULONGLONG CorrelationId;
} WISPDISK_RESPONSE_HEADER, *PWISPDISK_RESPONSE_HEADER;

typedef struct _WISPDISK_QUERY_VERSION_RESPONSE {
    WISPDISK_RESPONSE_HEADER Header;
    ULONG VersionMajor;
    ULONG VersionMinor;
    ULONG Capabilities;
    ULONG MaximumDiskCount;
} WISPDISK_QUERY_VERSION_RESPONSE, *PWISPDISK_QUERY_VERSION_RESPONSE;

typedef struct _WISPDISK_CREATE_REQUEST {
    WISPDISK_REQUEST_HEADER Header;
    ULONGLONG SizeBytes;
    ULONG LogicalSectorSize;
    ULONG MediaKind;
    USHORT RequestedDriveLetter;
    USHORT Reserved16;
    ULONG Reserved32;
} WISPDISK_CREATE_REQUEST, *PWISPDISK_CREATE_REQUEST;

typedef struct _WISPDISK_CREATE_RESPONSE {
    WISPDISK_RESPONSE_HEADER Header;
    ULONG DeviceId;
    UCHAR PathId;
    UCHAR TargetId;
    UCHAR Lun;
    UCHAR Reserved;
} WISPDISK_CREATE_RESPONSE, *PWISPDISK_CREATE_RESPONSE;

typedef struct _WISPDISK_DELETE_REQUEST {
    WISPDISK_REQUEST_HEADER Header;
    ULONG DeviceId;
    ULONG Reserved;
} WISPDISK_DELETE_REQUEST, *PWISPDISK_DELETE_REQUEST;

typedef struct _WISPDISK_DISK_INFO {
    ULONG DeviceId;
    UCHAR PathId;
    UCHAR TargetId;
    UCHAR Lun;
    UCHAR Reserved8;
    ULONGLONG SizeBytes;
    ULONG LogicalSectorSize;
    ULONG MediaKind;
    USHORT RequestedDriveLetter;
    USHORT Reserved16;
} WISPDISK_DISK_INFO, *PWISPDISK_DISK_INFO;

typedef struct _WISPDISK_LIST_RESPONSE {
    WISPDISK_RESPONSE_HEADER Header;
    ULONG DiskCount;
    ULONG Reserved;
    WISPDISK_DISK_INFO Disks[WISPDISK_MAXIMUM_DISK_COUNT];
} WISPDISK_LIST_RESPONSE, *PWISPDISK_LIST_RESPONSE;

#if defined(__cplusplus)
static_assert(sizeof(WISPDISK_REQUEST_HEADER) == 24, "protocol ABI changed");
static_assert(sizeof(WISPDISK_RESPONSE_HEADER) == 24, "protocol ABI changed");
static_assert(sizeof(WISPDISK_QUERY_VERSION_RESPONSE) == 40, "protocol ABI changed");
static_assert(sizeof(WISPDISK_CREATE_REQUEST) == 48, "protocol ABI changed");
static_assert(sizeof(WISPDISK_CREATE_RESPONSE) == 32, "protocol ABI changed");
static_assert(sizeof(WISPDISK_DELETE_REQUEST) == 32, "protocol ABI changed");
static_assert(sizeof(WISPDISK_DISK_INFO) == 32, "protocol ABI changed");
static_assert(sizeof(WISPDISK_LIST_RESPONSE) == 544, "protocol ABI changed");
#endif
