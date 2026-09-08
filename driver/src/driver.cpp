#include "driver.h"

#include "management/management.h"
#include "scsi/scsi.h"
#include "support/scoped_spin_lock.h"

namespace wispdisk::driver {

void CompleteSrb(
    _In_ PVOID deviceExtension,
    _Inout_ PSCSI_REQUEST_BLOCK srb,
    _In_ UCHAR srbStatus
) noexcept {
    srb->SrbStatus = srbStatus;
    const UCHAR baseStatus = srbStatus & 0x3fU;
    if (baseStatus == SRB_STATUS_SUCCESS || baseStatus == SRB_STATUS_DATA_OVERRUN) {
        srb->ScsiStatus = SCSISTAT_GOOD;
    }
    StorPortNotification(RequestComplete, deviceExtension, srb);
}

void MarkSupported(
    _Inout_opt_ PSCSI_SUPPORTED_CONTROL_TYPE_LIST list,
    _In_ SCSI_ADAPTER_CONTROL_TYPE controlType
) noexcept {
    if (list != nullptr && static_cast<ULONG>(controlType) < list->MaxControlType) {
        list->SupportedTypeList[controlType] = TRUE;
    }
}

SCSI_ADAPTER_CONTROL_STATUS QuerySupportedControlTypes(_Inout_opt_ PVOID parameters) noexcept {
    auto* list = static_cast<PSCSI_SUPPORTED_CONTROL_TYPE_LIST>(parameters);
    MarkSupported(list, ScsiQuerySupportedControlTypes);
    MarkSupported(list, ScsiStopAdapter);
    MarkSupported(list, ScsiRestartAdapter);
    return list == nullptr ? ScsiAdapterControlUnsuccessful : ScsiAdapterControlSuccess;
}

void MarkAllLunsStopping(_Inout_ PWISPDISK_ADAPTER_EXTENSION adapter) noexcept {
    support::ScopedSpinLock lock{&adapter->LunLock};
    for (auto& lun : adapter->Luns) {
        if (lun.State == WispDiskLunState::Online) {
            lun.State = WispDiskLunState::Stopping;
        }
    }
}

void FreeLunResources(_Inout_ PWISPDISK_ADAPTER_EXTENSION adapter) noexcept {
    for (auto& lun : adapter->Luns) {
        if (lun.State == WispDiskLunState::Empty) {
            continue;
        }
        KeWaitForSingleObject(
            &lun.NoActiveRequestsEvent,
            Executive,
            KernelMode,
            FALSE,
            nullptr
        );
        if (lun.BackingStore != nullptr) {
            StorPortFreePool(adapter, lun.BackingStore);
            lun.BackingStore = nullptr;
        }
        lun.State = WispDiskLunState::Empty;
    }
    adapter->TotalAllocatedBytes = 0;
}

} // namespace wispdisk::driver

extern "C"
ULONG DriverEntry(_In_ PVOID driverObject, _In_ PVOID registryPath) {
    KdPrint(("WispDisk: DriverEntry\n"));
    HW_INITIALIZATION_DATA initializationData{
        .HwInitializationDataSize = sizeof(HW_INITIALIZATION_DATA),
        .AdapterInterfaceType = Internal,
        .HwInitialize = WispDiskHwInitialize,
        .HwStartIo = WispDiskHwStartIo,
        .HwFindAdapter = reinterpret_cast<PVOID>(WispDiskHwFindAdapter),
        .HwResetBus = WispDiskHwResetBus,
        .DeviceExtensionSize = sizeof(WISPDISK_ADAPTER_EXTENSION),
        .MapBuffers = STOR_MAP_ALL_BUFFERS_INCLUDING_READ_WRITE,
        .NeedPhysicalAddresses = FALSE,
        .TaggedQueuing = TRUE,
        .AutoRequestSense = TRUE,
        .MultipleRequestPerLu = TRUE,
        .HwAdapterControl = WispDiskHwAdapterControl,
        .HwFreeAdapterResources = WispDiskHwFreeAdapterResources,
        .FeatureSupport =
            STOR_FEATURE_VIRTUAL_MINIPORT | STOR_FEATURE_ADAPTER_NOT_REQUIRE_IO_PORT,
        .SrbTypeFlags = SRB_TYPE_FLAG_SCSI_REQUEST_BLOCK,
        .AddressTypeFlags = ADDRESS_TYPE_FLAG_BTL8,
    };

    const ULONG status = StorPortInitialize(
        driverObject,
        registryPath,
        &initializationData,
        nullptr
    );
    KdPrint(("WispDisk: StorPortInitialize returned 0x%08lX\n", status));
    return status;
}

_Use_decl_annotations_
ULONG WispDiskHwFindAdapter(
    PVOID deviceExtension,
    PVOID hwContext,
    PVOID busInformation,
    PVOID lowerDevice,
    PCHAR argumentString,
    PPORT_CONFIGURATION_INFORMATION configInfo,
    PBOOLEAN again
) {
    UNREFERENCED_PARAMETER(hwContext);
    UNREFERENCED_PARAMETER(busInformation);
    UNREFERENCED_PARAMETER(lowerDevice);
    UNREFERENCED_PARAMETER(argumentString);

    if (deviceExtension == nullptr || configInfo == nullptr || again == nullptr) {
        KdPrint(("WispDisk: invalid adapter discovery parameters\n"));
        return SP_RETURN_BAD_CONFIG;
    }

    auto* adapter = static_cast<PWISPDISK_ADAPTER_EXTENSION>(deviceExtension);
    RtlZeroMemory(adapter, sizeof(*adapter));
    KeInitializeSpinLock(&adapter->LunLock);
    KeInitializeEvent(&adapter->ManagementIdleEvent, NotificationEvent, TRUE);
    adapter->NextDeviceId = 1;
    for (ULONG index = 0; index < kMaximumDiskCount; ++index) {
        adapter->Luns[index].Lun = static_cast<UCHAR>(index);
        KeInitializeEvent(&adapter->Luns[index].NoActiveRequestsEvent, NotificationEvent, TRUE);
    }
    adapter->Signature = kAdapterExtensionSignature;
    if (StorPortInitializeWorker(adapter, &adapter->ManagementWorker) != STOR_STATUS_SUCCESS) {
        adapter->Signature = 0;
        KdPrint(("WispDisk: failed to initialize management worker\n"));
        return SP_RETURN_ERROR;
    }

    // Storport preinitializes this structure; do not zero it here.
    configInfo->VirtualDevice = TRUE;
    configInfo->ScatterGather = TRUE;
    configInfo->Master = TRUE;
    configInfo->CachesData = FALSE;
    configInfo->MaximumTransferLength = kMaximumTransferLength;
    configInfo->NumberOfPhysicalBreaks = 32;
    configInfo->NumberOfBuses = 1;
    configInfo->MaximumNumberOfTargets = 1;
    configInfo->MaximumNumberOfLogicalUnits = kMaximumDiskCount;
    configInfo->AlignmentMask = FILE_LONG_ALIGNMENT;
    *again = FALSE;
    KdPrint(("WispDisk: virtual adapter discovered\n"));
    return SP_RETURN_FOUND;
}

_Use_decl_annotations_
BOOLEAN WispDiskHwInitialize(PVOID deviceExtension) {
    auto* adapter = static_cast<PWISPDISK_ADAPTER_EXTENSION>(deviceExtension);
    if (adapter == nullptr || adapter->Signature != kAdapterExtensionSignature) {
        return FALSE;
    }
    InterlockedExchange(&adapter->AdapterStopping, 0);
    KdPrint(("WispDisk: adapter initialized\n"));
    return TRUE;
}

_Use_decl_annotations_
BOOLEAN WispDiskHwStartIo(PVOID deviceExtension, PSCSI_REQUEST_BLOCK srb) {
    if (deviceExtension == nullptr || srb == nullptr) {
        return FALSE;
    }

    auto* adapter = static_cast<PWISPDISK_ADAPTER_EXTENSION>(deviceExtension);
    switch (srb->Function) {
        case SRB_FUNCTION_IO_CONTROL:
            if (WispDiskHandleIoControl(adapter, srb) ==
                WispDiskIoControlDisposition::Pending) {
                return TRUE;
            }
            if (srb->SrbStatus == SRB_STATUS_PENDING) {
                srb->SrbStatus = SRB_STATUS_INVALID_REQUEST;
            }
            wispdisk::driver::CompleteSrb(deviceExtension, srb, srb->SrbStatus);
            break;

        case SRB_FUNCTION_EXECUTE_SCSI:
            wispdisk::driver::CompleteSrb(
                deviceExtension,
                srb,
                WispDiskHandleExecuteScsi(adapter, srb)
            );
            break;

        case SRB_FUNCTION_PNP:
        case SRB_FUNCTION_POWER:
        case SRB_FUNCTION_FLUSH:
        case SRB_FUNCTION_SHUTDOWN:
            srb->DataTransferLength = 0;
            wispdisk::driver::CompleteSrb(deviceExtension, srb, SRB_STATUS_SUCCESS);
            break;

        default:
            srb->DataTransferLength = 0;
            wispdisk::driver::CompleteSrb(
                deviceExtension,
                srb,
                SRB_STATUS_INVALID_REQUEST
            );
            break;
    }

    return TRUE;
}

_Use_decl_annotations_
BOOLEAN WispDiskHwResetBus(PVOID deviceExtension, ULONG pathId) {
    UNREFERENCED_PARAMETER(deviceExtension);
    UNREFERENCED_PARAMETER(pathId);
    return TRUE;
}

_Use_decl_annotations_
SCSI_ADAPTER_CONTROL_STATUS WispDiskHwAdapterControl(
    PVOID deviceExtension,
    SCSI_ADAPTER_CONTROL_TYPE controlType,
    PVOID parameters
) {
    auto* adapter = static_cast<PWISPDISK_ADAPTER_EXTENSION>(deviceExtension);

    switch (controlType) {
        case ScsiQuerySupportedControlTypes:
            return wispdisk::driver::QuerySupportedControlTypes(parameters);

        case ScsiStopAdapter:
            if (adapter == nullptr || adapter->Signature != kAdapterExtensionSignature) {
                return ScsiAdapterControlUnsuccessful;
            }
            InterlockedExchange(&adapter->AdapterStopping, 1);
            KdPrint(("WispDisk: adapter stopped\n"));
            return ScsiAdapterControlSuccess;

        case ScsiRestartAdapter:
            if (adapter == nullptr || adapter->Signature != kAdapterExtensionSignature) {
                return ScsiAdapterControlUnsuccessful;
            }
            InterlockedExchange(&adapter->AdapterStopping, 0);
            KdPrint(("WispDisk: adapter restarted\n"));
            return ScsiAdapterControlSuccess;

        default:
            return ScsiAdapterControlUnsuccessful;
    }
}

_Use_decl_annotations_
VOID WispDiskHwFreeAdapterResources(PVOID deviceExtension) {
    auto* adapter = static_cast<PWISPDISK_ADAPTER_EXTENSION>(deviceExtension);
    if (adapter == nullptr || adapter->Signature != kAdapterExtensionSignature) {
        return;
    }

    KdPrint(("WispDisk: releasing adapter resources\n"));
    InterlockedExchange(&adapter->AdapterStopping, 1);
    KeWaitForSingleObject(
        &adapter->ManagementIdleEvent,
        Executive,
        KernelMode,
        FALSE,
        nullptr
    );

    wispdisk::driver::MarkAllLunsStopping(adapter);
    wispdisk::driver::FreeLunResources(adapter);
    if (adapter->ManagementWorker != nullptr) {
        StorPortFreeWorker(adapter, adapter->ManagementWorker);
        adapter->ManagementWorker = nullptr;
    }
    adapter->Signature = 0;
    KdPrint(("WispDisk: adapter resources released\n"));
}
