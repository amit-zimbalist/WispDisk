#include "driver.h"

#include "adapter_context.h"
#include "logging.h"

namespace wispdisk::driver {
namespace {

void MarkSupported(
    _Inout_opt_ PSCSI_SUPPORTED_CONTROL_TYPE_LIST list,
    _In_ SCSI_ADAPTER_CONTROL_TYPE controlType) noexcept {
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

} // namespace
} // namespace wispdisk::driver

extern "C" ULONG DriverEntry(_In_ PVOID driverObject, _In_ PVOID registryPath) {
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
        .HwInitializeTracing = WispDiskHwInitializeTracing,
        .HwCleanupTracing = WispDiskHwCleanupTracing,
        .FeatureSupport = STOR_FEATURE_VIRTUAL_MINIPORT | STOR_FEATURE_ADAPTER_NOT_REQUIRE_IO_PORT,
        .SrbTypeFlags = SRB_TYPE_FLAG_SCSI_REQUEST_BLOCK,
        .AddressTypeFlags = ADDRESS_TYPE_FLAG_BTL8,
    };

    const ULONG status =
        StorPortInitialize(driverObject, registryPath, &initializationData, nullptr);
    wispdisk::logging::Write(
        NT_SUCCESS(status) ? wispdisk::logging::Level::Information
                           : wispdisk::logging::Level::Error,
        {.Message = "DriverInitialized", .Status = static_cast<NTSTATUS>(status)});
    // DriverUnload is not called when DriverEntry fails.
    if (!NT_SUCCESS(status)) {
        wispdisk::logging::Shutdown();
    }
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
    PBOOLEAN again) {
    UNREFERENCED_PARAMETER(hwContext);
    UNREFERENCED_PARAMETER(busInformation);
    UNREFERENCED_PARAMETER(lowerDevice);
    UNREFERENCED_PARAMETER(argumentString);

    if (deviceExtension == nullptr || configInfo == nullptr || again == nullptr) {
        ERROR("AdapterDiscoveryRejected", .Status = STATUS_INVALID_PARAMETER);
        return SP_RETURN_BAD_CONFIG;
    }

    wispdisk::driver::AdapterContext adapter{
        *static_cast<PWISPDISK_ADAPTER_EXTENSION>(deviceExtension)};
    if (!adapter.Initialize()) {
        ERROR("ManagementWorkerInitializationFailed", .Status = STATUS_UNSUCCESSFUL);
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
    INFO(
        "AdapterDiscovered",
        .AdapterInstance =
            static_cast<PWISPDISK_ADAPTER_EXTENSION>(deviceExtension)->AdapterInstance);
    return SP_RETURN_FOUND;
}

_Use_decl_annotations_
BOOLEAN WispDiskHwInitialize(PVOID deviceExtension) {
    if (deviceExtension == nullptr) {
        return FALSE;
    }
    wispdisk::driver::AdapterContext adapter{
        *static_cast<PWISPDISK_ADAPTER_EXTENSION>(deviceExtension)};
    if (!adapter.SetStopping(false)) {
        return FALSE;
    }
    INFO(
        "AdapterInitialized",
        .AdapterInstance =
            static_cast<PWISPDISK_ADAPTER_EXTENSION>(deviceExtension)->AdapterInstance);
    return TRUE;
}

_Use_decl_annotations_
BOOLEAN WispDiskHwStartIo(PVOID deviceExtension, PSCSI_REQUEST_BLOCK srb) {
    if (deviceExtension == nullptr || srb == nullptr) {
        return FALSE;
    }
    const wispdisk::driver::AdapterContext adapter{
        *static_cast<PWISPDISK_ADAPTER_EXTENSION>(deviceExtension)};
    return adapter.StartIo(srb);
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
    PVOID parameters) {
    if (controlType == ScsiQuerySupportedControlTypes) {
        return wispdisk::driver::QuerySupportedControlTypes(parameters);
    }
    if (deviceExtension == nullptr) {
        return ScsiAdapterControlUnsuccessful;
    }
    wispdisk::driver::AdapterContext adapter{
        *static_cast<PWISPDISK_ADAPTER_EXTENSION>(deviceExtension)};
    switch (controlType) {
        case ScsiStopAdapter:
            if (!adapter.SetStopping(true)) {
                return ScsiAdapterControlUnsuccessful;
            }
            INFO(
                "AdapterStopped",
                .AdapterInstance =
                    static_cast<PWISPDISK_ADAPTER_EXTENSION>(deviceExtension)->AdapterInstance);
            return ScsiAdapterControlSuccess;
        case ScsiRestartAdapter:
            if (!adapter.SetStopping(false)) {
                return ScsiAdapterControlUnsuccessful;
            }
            INFO(
                "AdapterRestarted",
                .AdapterInstance =
                    static_cast<PWISPDISK_ADAPTER_EXTENSION>(deviceExtension)->AdapterInstance);
            return ScsiAdapterControlSuccess;
        default:
            return ScsiAdapterControlUnsuccessful;
    }
}

_Use_decl_annotations_
VOID WispDiskHwFreeAdapterResources(PVOID deviceExtension) {
    if (deviceExtension == nullptr) {
        return;
    }
    wispdisk::driver::AdapterContext adapter{
        *static_cast<PWISPDISK_ADAPTER_EXTENSION>(deviceExtension)};
    if (!adapter.IsInitialized()) {
        return;
    }
    INFO(
        "AdapterResourcesReleasing",
        .AdapterInstance =
            static_cast<PWISPDISK_ADAPTER_EXTENSION>(deviceExtension)->AdapterInstance);
    adapter.ReleaseResources();
    INFO(
        "AdapterResourcesReleased",
        .AdapterInstance =
            static_cast<PWISPDISK_ADAPTER_EXTENSION>(deviceExtension)->AdapterInstance);
}

_Use_decl_annotations_
VOID WispDiskHwInitializeTracing(PVOID driverObject, PVOID registryPath) {
    UNREFERENCED_PARAMETER(driverObject);
    UNREFERENCED_PARAMETER(registryPath);
    const NTSTATUS status = wispdisk::logging::Initialize();
    if (!NT_SUCCESS(status)) {
        ERROR("TracingInitializationFailed", .Status = status);
    }
}

_Use_decl_annotations_
VOID WispDiskHwCleanupTracing(PVOID driverObject) {
    UNREFERENCED_PARAMETER(driverObject);
    wispdisk::logging::Shutdown();
}
