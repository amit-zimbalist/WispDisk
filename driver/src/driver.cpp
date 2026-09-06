#include "driver_internal.h"

namespace {

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
    _Inout_ PSCSI_SUPPORTED_CONTROL_TYPE_LIST list,
    _In_ SCSI_ADAPTER_CONTROL_TYPE controlType
) noexcept {
    if (list != nullptr && static_cast<ULONG>(controlType) < list->MaxControlType) {
        list->SupportedTypeList[controlType] = TRUE;
    }
}

} // namespace

extern "C"
ULONG DriverEntry(_In_ PVOID driverObject, _In_ PVOID registryPath) {
    HW_INITIALIZATION_DATA initializationData{};
    initializationData.HwInitializationDataSize = sizeof(initializationData);
    initializationData.AdapterInterfaceType = Internal;
    initializationData.HwInitialize = WispDiskHwInitialize;
    initializationData.HwStartIo = WispDiskHwStartIo;
    initializationData.HwFindAdapter = reinterpret_cast<PVOID>(WispDiskHwFindAdapter);
    initializationData.HwResetBus = WispDiskHwResetBus;
    initializationData.HwAdapterControl = WispDiskHwAdapterControl;
    initializationData.HwFreeAdapterResources = WispDiskHwFreeAdapterResources;
    initializationData.DeviceExtensionSize = sizeof(WISPDISK_ADAPTER_EXTENSION);
    initializationData.MapBuffers = STOR_MAP_ALL_BUFFERS_INCLUDING_READ_WRITE;
    initializationData.NeedPhysicalAddresses = FALSE;
    initializationData.TaggedQueuing = TRUE;
    initializationData.AutoRequestSense = TRUE;
    initializationData.MultipleRequestPerLu = TRUE;
    initializationData.FeatureSupport =
        STOR_FEATURE_VIRTUAL_MINIPORT | STOR_FEATURE_ADAPTER_NOT_REQUIRE_IO_PORT;
    initializationData.SrbTypeFlags = SRB_TYPE_FLAG_SCSI_REQUEST_BLOCK;
    initializationData.AddressTypeFlags = ADDRESS_TYPE_FLAG_BTL8;

    return StorPortInitialize(driverObject, registryPath, &initializationData, nullptr);
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
    return SP_RETURN_FOUND;
}

_Use_decl_annotations_
BOOLEAN WispDiskHwInitialize(PVOID deviceExtension) {
    auto* adapter = static_cast<PWISPDISK_ADAPTER_EXTENSION>(deviceExtension);
    if (adapter == nullptr || adapter->Signature != kAdapterExtensionSignature) {
        return FALSE;
    }
    InterlockedExchange(&adapter->AdapterStopping, 0);
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
            if (WispDiskHandleIoControl(adapter, srb) == WISPDISK_IO_CONTROL_DISPOSITION::Pending) {
                return TRUE;
            }
            if (srb->SrbStatus == SRB_STATUS_PENDING) {
                srb->SrbStatus = SRB_STATUS_INVALID_REQUEST;
            }
            CompleteSrb(deviceExtension, srb, srb->SrbStatus);
            break;

        case SRB_FUNCTION_EXECUTE_SCSI:
            CompleteSrb(deviceExtension, srb, WispDiskHandleExecuteScsi(adapter, srb));
            break;

        case SRB_FUNCTION_PNP:
        case SRB_FUNCTION_POWER:
        case SRB_FUNCTION_FLUSH:
        case SRB_FUNCTION_SHUTDOWN:
            srb->DataTransferLength = 0;
            CompleteSrb(deviceExtension, srb, SRB_STATUS_SUCCESS);
            break;

        default:
            srb->DataTransferLength = 0;
            CompleteSrb(deviceExtension, srb, SRB_STATUS_INVALID_REQUEST);
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
        case ScsiQuerySupportedControlTypes: {
            auto* list = static_cast<PSCSI_SUPPORTED_CONTROL_TYPE_LIST>(parameters);
            MarkSupported(list, ScsiQuerySupportedControlTypes);
            MarkSupported(list, ScsiStopAdapter);
            MarkSupported(list, ScsiRestartAdapter);
            return list == nullptr ? ScsiAdapterControlUnsuccessful : ScsiAdapterControlSuccess;
        }

        case ScsiStopAdapter:
            if (adapter == nullptr || adapter->Signature != kAdapterExtensionSignature) {
                return ScsiAdapterControlUnsuccessful;
            }
            InterlockedExchange(&adapter->AdapterStopping, 1);
            return ScsiAdapterControlSuccess;

        case ScsiRestartAdapter:
            if (adapter == nullptr || adapter->Signature != kAdapterExtensionSignature) {
                return ScsiAdapterControlUnsuccessful;
            }
            InterlockedExchange(&adapter->AdapterStopping, 0);
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

    InterlockedExchange(&adapter->AdapterStopping, 1);
    KeWaitForSingleObject(
        &adapter->ManagementIdleEvent,
        Executive,
        KernelMode,
        FALSE,
        nullptr
    );

    KIRQL oldIrql;
    KeAcquireSpinLock(&adapter->LunLock, &oldIrql);
    for (ULONG index = 0; index < kMaximumDiskCount; ++index) {
        if (adapter->Luns[index].State == WispDiskLunOnline) {
            adapter->Luns[index].State = WispDiskLunStopping;
        }
    }
    KeReleaseSpinLock(&adapter->LunLock, oldIrql);

    for (ULONG index = 0; index < kMaximumDiskCount; ++index) {
        auto* lun = &adapter->Luns[index];
        if (lun->State == WispDiskLunEmpty) {
            continue;
        }
        KeWaitForSingleObject(
            &lun->NoActiveRequestsEvent,
            Executive,
            KernelMode,
            FALSE,
            nullptr
        );
        if (lun->BackingStore != nullptr) {
            StorPortFreePool(adapter, lun->BackingStore);
            lun->BackingStore = nullptr;
        }
        lun->State = WispDiskLunEmpty;
    }

    adapter->TotalAllocatedBytes = 0;
    if (adapter->ManagementWorker != nullptr) {
        StorPortFreeWorker(adapter, adapter->ManagementWorker);
        adapter->ManagementWorker = nullptr;
    }
    adapter->Signature = 0;
}
