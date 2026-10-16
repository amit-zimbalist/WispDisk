#pragma once

extern "C" {
#include <ntddk.h>
#include <ntddscsi.h>
#include <storport.h>
}

#include "wispdisk_protocol.h"

constexpr ULONG kAdapterExtensionSignature = 0x57697370UL; // "Wisp"
constexpr ULONG kMaximumDiskCount = WISPDISK_MAXIMUM_DISK_COUNT;
constexpr ULONG kLogicalSectorSize = 512;
constexpr ULONG kMaximumTransferLength = 1024 * 1024;
constexpr ULONGLONG kMinimumDiskSize = 1024ULL * 1024ULL;
constexpr ULONGLONG kMaximumDiskSize = 256ULL * 1024ULL * 1024ULL;
constexpr ULONGLONG kMaximumTotalDiskBytes = 512ULL * 1024ULL * 1024ULL;
constexpr ULONG kBackingStoreTag = 'psiW';

enum class WispDiskLunState : LONG {
    Empty = 0,
    Online = 1,
    Stopping = 2,
};

struct WISPDISK_LUN {
    // Runtime state changes use the adapter's LunLock. A live I/O lease permits
    // access to immutable online metadata and backing memory outside the lock.
    // Serialized management retains stopping LUNs while draining active I/O.
    WispDiskLunState State;
    ULONG DeviceId;
    UCHAR Lun;
    UCHAR Reserved8[3];
    ULONG MediaKind;
    USHORT RequestedDriveLetter;
    USHORT Reserved16;
    ULONGLONG SizeBytes;
    ULONGLONG CreateOperationId;
    PUCHAR BackingStore;
    ULONG ActiveRequests;
    KEVENT NoActiveRequestsEvent;
};
using PWISPDISK_LUN = WISPDISK_LUN*;

constexpr ULONG kMaximumOperationCount = 128;
struct WISPDISK_OPERATION_RECORD {
    ULONGLONG Id;
    ULONG Mutation;
    WISPDISK_OPERATION_STATE State;
    NTSTATUS Result;
    ULONG DeviceId;
    UCHAR Lun;
    WISPDISK_CREATE_REQUEST Create;
    WISPDISK_DELETE_REQUEST Delete;
};

struct WISPDISK_ADAPTER_EXTENSION {
    ULONG Signature;
    // Serializes admission/idle publication, operation records, LUN state,
    // allocation accounting, and Debug diagnostic state. Initialization is
    // exclusive; allocation, I/O draining, completion, and notification run
    // outside this lock.
    KSPIN_LOCK LunLock;
    // Stop/restart may update this atomically at DIRQL without LunLock.
    // Shutdown also holds LunLock to serialize closing with request admission.
    volatile LONG AdapterStopping;
    // Busy and idle-event changes are coordinated together under LunLock.
    volatile LONG ManagementBusy;
    KEVENT ManagementIdleEvent;
    // Created before requests are admitted; freed after admission is closed
    // and the final worker access has finished.
    PVOID ManagementWorker;
    ULONG NextDeviceId;
    ULONGLONG AdapterInstance; // Immutable from initialization until teardown.
    ULONGLONG NextOperationId;
    WISPDISK_OPERATION_RECORD Operations[kMaximumOperationCount];
    ULONGLONG TotalAllocatedBytes;
    WISPDISK_LUN Luns[WISPDISK_MAXIMUM_DISK_COUNT];
#if DBG
    ULONG TraceEnabled;
    ULONG FaultMode;
    ULONG FaultRemaining;
    ULONGLONG TraceSequence;
    struct TraceEntry {
        ULONGLONG Sequence;
        ULONGLONG OperationId;
        LONGLONG Timestamp;
        ULONG DeviceId;
        ULONG Phase;
        NTSTATUS Status;
        ULONG SrbStatus;
    } Trace[128];
#endif
};
using PWISPDISK_ADAPTER_EXTENSION = WISPDISK_ADAPTER_EXTENSION*;

extern "C" ULONG DriverEntry(_In_ PVOID DriverObject, _In_ PVOID RegistryPath);

VIRTUAL_HW_FIND_ADAPTER WispDiskHwFindAdapter;
HW_INITIALIZE WispDiskHwInitialize;
HW_STARTIO WispDiskHwStartIo;
HW_RESET_BUS WispDiskHwResetBus;
HW_ADAPTER_CONTROL WispDiskHwAdapterControl;
HW_FREE_ADAPTER_RESOURCES WispDiskHwFreeAdapterResources;
