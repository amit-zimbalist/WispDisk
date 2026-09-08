#include "disk_operations.h"

#include "support/scoped_spin_lock.h"
#include "support/stor_port_pool_allocation.h"

namespace wispdisk::management {

static void PrepareSuccessfulResponse(
    _In_ const ControlRequest& controlRequest,
    _In_ ULONG payloadLength
) noexcept {
    controlRequest.control->ReturnCode = static_cast<ULONG>(STATUS_SUCCESS);
    controlRequest.control->Length = payloadLength;
    controlRequest.srb->DataTransferLength = sizeof(SRB_IO_CONTROL) + payloadLength;
    controlRequest.srb->ScsiStatus = SCSISTAT_GOOD;
    controlRequest.srb->SrbStatus = SRB_STATUS_SUCCESS;
}

static void SnapshotOnlineLuns(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _Inout_ PWISPDISK_LIST_RESPONSE response
) noexcept {
    support::ScopedSpinLock lock{&adapter->LunLock};
    for (const auto& lun : adapter->Luns) {
        if (lun.State != WispDiskLunState::Online) {
            continue;
        }

        response->Disks[response->DiskCount++] = {
            .DeviceId = lun.DeviceId,
            .PathId = 0,
            .TargetId = 0,
            .Lun = lun.Lun,
            .SizeBytes = lun.SizeBytes,
            .LogicalSectorSize = kLogicalSectorSize,
            .MediaKind = lun.MediaKind,
            .RequestedDriveLetter = lun.RequestedDriveLetter,
        };
    }
}

static NTSTATUS SelectAvailableLun(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _In_ ULONGLONG sizeBytes,
    _Out_ ULONG* selectedLun
) noexcept {
    support::ScopedSpinLock lock{&adapter->LunLock};
    if (adapter->AdapterStopping != 0) {
        return STATUS_DEVICE_NOT_READY;
    }
    if (adapter->TotalAllocatedBytes > kMaximumTotalDiskBytes - sizeBytes) {
        return STATUS_QUOTA_EXCEEDED;
    }

    for (ULONG index = 0; index < kMaximumDiskCount; ++index) {
        if (adapter->Luns[index].State == WispDiskLunState::Empty) {
            *selectedLun = index;
            return STATUS_SUCCESS;
        }
    }
    return STATUS_INSUFFICIENT_RESOURCES;
}

static NTSTATUS CommitNewLun(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _In_ const WISPDISK_CREATE_REQUEST& request,
    _In_ ULONG selectedLun,
    _Inout_ support::StorPortPoolAllocation* allocation,
    _Out_ ULONG* deviceId
) noexcept {
    support::ScopedSpinLock lock{&adapter->LunLock};
    auto* lun = &adapter->Luns[selectedLun];
    if (adapter->AdapterStopping != 0 || lun->State != WispDiskLunState::Empty) {
        return STATUS_DEVICE_NOT_READY;
    }

    *deviceId = adapter->NextDeviceId++;
    if (*deviceId == 0) {
        *deviceId = adapter->NextDeviceId++;
    }

    lun->DeviceId = *deviceId;
    lun->Lun = static_cast<UCHAR>(selectedLun);
    lun->MediaKind = request.MediaKind;
    lun->RequestedDriveLetter = request.RequestedDriveLetter;
    lun->SizeBytes = request.SizeBytes;
    lun->BackingStore = static_cast<PUCHAR>(allocation->release());
    lun->ActiveRequests = 0;
    KeSetEvent(&lun->NoActiveRequestsEvent, IO_NO_INCREMENT, FALSE);
    lun->State = WispDiskLunState::Online;
    adapter->TotalAllocatedBytes += request.SizeBytes;
    return STATUS_SUCCESS;
}

static PWISPDISK_LUN MarkLunStopping(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _In_ ULONG deviceId
) noexcept {
    support::ScopedSpinLock lock{&adapter->LunLock};
    for (auto& lun : adapter->Luns) {
        if (lun.State == WispDiskLunState::Online && lun.DeviceId == deviceId) {
            lun.State = WispDiskLunState::Stopping;
            return &lun;
        }
    }
    return nullptr;
}

static PUCHAR DetachBackingStore(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _Inout_ PWISPDISK_LUN lun,
    _In_ ULONG expectedDeviceId
) noexcept {
    support::ScopedSpinLock lock{&adapter->LunLock};
    if (lun->State != WispDiskLunState::Stopping || lun->DeviceId != expectedDeviceId) {
        return nullptr;
    }

    auto* backingStore = lun->BackingStore;
    if (adapter->TotalAllocatedBytes >= lun->SizeBytes) {
        adapter->TotalAllocatedBytes -= lun->SizeBytes;
    } else {
        adapter->TotalAllocatedBytes = 0;
    }

    lun->BackingStore = nullptr;
    lun->SizeBytes = 0;
    lun->DeviceId = 0;
    lun->MediaKind = 0;
    lun->RequestedDriveLetter = 0;
    lun->State = WispDiskLunState::Empty;
    return backingStore;
}

void HandleQueryVersion(_In_ const ControlRequest& controlRequest) noexcept {
    WISPDISK_REQUEST_HEADER request{};
    const NTSTATUS validation = ReadAndValidateRequestHeader(
        controlRequest.payload,
        controlRequest.control->Length,
        sizeof(request),
        WispDiskControlQueryVersion,
        &request
    );
    if (!NT_SUCCESS(validation)) {
        WriteSimpleErrorResponse(
            controlRequest,
            WispDiskControlQueryVersion,
            validation,
            request.CorrelationId
        );
        return;
    }

    if (controlRequest.control->Length < sizeof(WISPDISK_QUERY_VERSION_RESPONSE)) {
        WriteSimpleErrorResponse(
            controlRequest,
            WispDiskControlQueryVersion,
            STATUS_BUFFER_TOO_SMALL,
            request.CorrelationId
        );
        return;
    }

    const WISPDISK_QUERY_VERSION_RESPONSE response{
        .Header = MakeResponseHeader(
            sizeof(WISPDISK_QUERY_VERSION_RESPONSE),
            WispDiskControlQueryVersion,
            STATUS_SUCCESS,
            request.CorrelationId
        ),
        .VersionMajor = 0,
        .VersionMinor = 2,
        .Capabilities =
            WISPDISK_CAP_CREATE_DELETE | WISPDISK_CAP_READ_WRITE | WISPDISK_CAP_REMOVABLE,
        .MaximumDiskCount = kMaximumDiskCount,
    };
    RtlCopyMemory(controlRequest.payload, &response, sizeof(response));
    PrepareSuccessfulResponse(controlRequest, sizeof(response));
}

void HandleListDisks(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _In_ const ControlRequest& controlRequest
) noexcept {
    WISPDISK_REQUEST_HEADER request{};
    const NTSTATUS validation = ReadAndValidateRequestHeader(
        controlRequest.payload,
        controlRequest.control->Length,
        sizeof(request),
        WispDiskControlListDisks,
        &request
    );
    if (!NT_SUCCESS(validation) ||
        controlRequest.control->Length < sizeof(WISPDISK_LIST_RESPONSE)) {
        const NTSTATUS status = NT_SUCCESS(validation) ? STATUS_BUFFER_TOO_SMALL : validation;
        WriteSimpleErrorResponse(
            controlRequest,
            WispDiskControlListDisks,
            status,
            request.CorrelationId
        );
        return;
    }

    WISPDISK_LIST_RESPONSE response{
        .Header = MakeResponseHeader(
            sizeof(WISPDISK_LIST_RESPONSE),
            WispDiskControlListDisks,
            STATUS_SUCCESS,
            request.CorrelationId
        ),
    };
    SnapshotOnlineLuns(adapter, &response);

    RtlCopyMemory(controlRequest.payload, &response, sizeof(response));
    PrepareSuccessfulResponse(controlRequest, sizeof(response));
}

void ProcessCreateRequest(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _In_ const ControlRequest& controlRequest
) noexcept {
    WISPDISK_CREATE_REQUEST request{};
    NTSTATUS status = ValidateCreateRequest(controlRequest, &request);
    if (!NT_SUCCESS(status)) {
        WriteCreateResponse(controlRequest, request, status);
        return;
    }

    ULONG selectedLun = kMaximumDiskCount;
    status = SelectAvailableLun(adapter, request.SizeBytes, &selectedLun);
    if (!NT_SUCCESS(status)) {
        KdPrint(("WispDisk: create rejected, status=0x%08lX\n", status));
        WriteCreateResponse(controlRequest, request, status);
        return;
    }

    support::StorPortPoolAllocation allocation{
        adapter,
        static_cast<ULONG>(request.SizeBytes),
        kBackingStoreTag
    };
    if (!allocation) {
        KdPrint(("WispDisk: backing-store allocation failed, size=%I64u\n", request.SizeBytes));
        WriteCreateResponse(controlRequest, request, STATUS_INSUFFICIENT_RESOURCES);
        return;
    }
    RtlZeroMemory(allocation.get(), static_cast<SIZE_T>(request.SizeBytes));

    ULONG deviceId = 0;
    status = CommitNewLun(adapter, request, selectedLun, &allocation, &deviceId);
    if (!NT_SUCCESS(status)) {
        KdPrint(("WispDisk: create commit failed, status=0x%08lX\n", status));
        WriteCreateResponse(controlRequest, request, status);
        return;
    }

    KdPrint((
        "WispDisk: created disk id=%lu lun=%lu size=%I64u\n",
        deviceId,
        selectedLun,
        request.SizeBytes
    ));
    StorPortNotification(BusChangeDetected, adapter, static_cast<ULONG>(0));
    WriteCreateResponse(
        controlRequest,
        request,
        STATUS_SUCCESS,
        deviceId,
        static_cast<UCHAR>(selectedLun)
    );
}

void ProcessDeleteRequest(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _In_ const ControlRequest& controlRequest
) noexcept {
    WISPDISK_DELETE_REQUEST request{};
    const NTSTATUS status = ValidateDeleteRequest(controlRequest, &request);
    if (!NT_SUCCESS(status)) {
        WriteDeleteResponse(controlRequest, request, status);
        return;
    }

    auto* selected = MarkLunStopping(adapter, request.DeviceId);
    if (selected == nullptr) {
        KdPrint(("WispDisk: delete requested for unknown disk id=%lu\n", request.DeviceId));
        WriteDeleteResponse(controlRequest, request, STATUS_NO_SUCH_DEVICE);
        return;
    }

    StorPortNotification(BusChangeDetected, adapter, static_cast<ULONG>(0));
    KeWaitForSingleObject(
        &selected->NoActiveRequestsEvent,
        Executive,
        KernelMode,
        FALSE,
        nullptr
    );

    auto* backingStore = DetachBackingStore(adapter, selected, request.DeviceId);
    if (backingStore != nullptr) {
        StorPortFreePool(adapter, backingStore);
    }

    KdPrint(("WispDisk: deleted disk id=%lu\n", request.DeviceId));
    StorPortNotification(BusChangeDetected, adapter, static_cast<ULONG>(0));
    WriteDeleteResponse(controlRequest, request, STATUS_SUCCESS);
}

} // namespace wispdisk::management
