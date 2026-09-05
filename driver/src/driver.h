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

enum WISPDISK_LUN_STATE : LONG {
    WispDiskLunEmpty = 0,
    WispDiskLunOnline = 1,
    WispDiskLunStopping = 2,
};

typedef struct _WISPDISK_LUN {
    LONG State;
    ULONG DeviceId;
    UCHAR Lun;
    UCHAR Reserved8[3];
    ULONG MediaKind;
    USHORT RequestedDriveLetter;
    USHORT Reserved16;
    ULONGLONG SizeBytes;
    PUCHAR BackingStore;
    ULONG ActiveRequests;
    KEVENT NoActiveRequestsEvent;
} WISPDISK_LUN, *PWISPDISK_LUN;

typedef struct _WISPDISK_ADAPTER_EXTENSION {
    ULONG Signature;
    KSPIN_LOCK LunLock;
    volatile LONG AdapterStopping;
    volatile LONG ManagementBusy;
    KEVENT ManagementIdleEvent;
    PVOID ManagementWorker;
    ULONG NextDeviceId;
    ULONGLONG TotalAllocatedBytes;
    WISPDISK_LUN Luns[WISPDISK_MAXIMUM_DISK_COUNT];
} WISPDISK_ADAPTER_EXTENSION, *PWISPDISK_ADAPTER_EXTENSION;

extern "C" ULONG DriverEntry(_In_ PVOID DriverObject, _In_ PVOID RegistryPath);

VIRTUAL_HW_FIND_ADAPTER WispDiskHwFindAdapter;
HW_INITIALIZE WispDiskHwInitialize;
HW_STARTIO WispDiskHwStartIo;
HW_RESET_BUS WispDiskHwResetBus;
HW_ADAPTER_CONTROL WispDiskHwAdapterControl;
HW_FREE_ADAPTER_RESOURCES WispDiskHwFreeAdapterResources;
HW_WORKITEM WispDiskManagementWorker;
