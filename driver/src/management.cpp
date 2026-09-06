#include "driver_internal.h"

namespace {

bool HasSrbSignature(_In_reads_bytes_(WISPDISK_SRB_SIGNATURE_LENGTH) const UCHAR* signature) noexcept {
    return RtlCompareMemory(
               signature,
               WISPDISK_SRB_SIGNATURE,
               WISPDISK_SRB_SIGNATURE_LENGTH
           ) == WISPDISK_SRB_SIGNATURE_LENGTH;
}

bool GetControlBuffer(
    _Inout_ PSCSI_REQUEST_BLOCK srb,
    _Out_ PSRB_IO_CONTROL* control,
    _Out_ PUCHAR* payload
) noexcept {
    *control = nullptr;
    *payload = nullptr;
    if (srb->DataBuffer == nullptr || srb->DataTransferLength < sizeof(SRB_IO_CONTROL)) {
        return false;
    }

    auto* candidate = static_cast<PSRB_IO_CONTROL>(srb->DataBuffer);
    if (candidate->HeaderLength != sizeof(SRB_IO_CONTROL) ||
        !HasSrbSignature(candidate->Signature) ||
        candidate->Length > srb->DataTransferLength - sizeof(SRB_IO_CONTROL)) {
        return false;
    }

    *control = candidate;
    *payload = reinterpret_cast<PUCHAR>(candidate) + sizeof(SRB_IO_CONTROL);
    return true;
}

NTSTATUS ReadAndValidateRequestHeader(
    _In_reads_bytes_(payloadLength) const UCHAR* payload,
    _In_ ULONG payloadLength,
    _In_ ULONG requiredLength,
    _In_ WISPDISK_CONTROL_CODE expectedOperation,
    _Out_ PWISPDISK_REQUEST_HEADER header
) noexcept {
    if (payloadLength < requiredLength || requiredLength < sizeof(WISPDISK_REQUEST_HEADER)) {
        return STATUS_BUFFER_TOO_SMALL;
    }

    RtlCopyMemory(header, payload, sizeof(*header));
    if (header->StructSize != requiredLength ||
        header->ProtocolVersion != WISPDISK_PROTOCOL_VERSION ||
        header->Operation != static_cast<ULONG>(expectedOperation) ||
        header->Flags != 0) {
        return STATUS_INVALID_PARAMETER;
    }

    return STATUS_SUCCESS;
}

void InitializeResponseHeader(
    _Out_ PWISPDISK_RESPONSE_HEADER header,
    _In_ ULONG structureSize,
    _In_ WISPDISK_CONTROL_CODE operation,
    _In_ NTSTATUS status,
    _In_ ULONGLONG correlationId
) noexcept {
    header->StructSize = structureSize;
    header->ProtocolVersion = WISPDISK_PROTOCOL_VERSION;
    header->Operation = static_cast<ULONG>(operation);
    header->Status = status;
    header->CorrelationId = correlationId;
}

void PrepareControlCompletion(
    _Inout_ PSCSI_REQUEST_BLOCK srb,
    _Inout_ PSRB_IO_CONTROL control,
    _In_ NTSTATUS status,
    _In_ ULONG payloadLength
) noexcept {
    control->ReturnCode = static_cast<ULONG>(status);
    control->Length = payloadLength;
    srb->DataTransferLength = sizeof(SRB_IO_CONTROL) + payloadLength;
    srb->ScsiStatus = SCSISTAT_GOOD;
    srb->SrbStatus = SRB_STATUS_SUCCESS;
}

void WriteSimpleErrorResponse(
    _Inout_ PSCSI_REQUEST_BLOCK srb,
    _Inout_ PSRB_IO_CONTROL control,
    _Inout_updates_bytes_(control->Length) PUCHAR payload,
    _In_ WISPDISK_CONTROL_CODE operation,
    _In_ NTSTATUS status,
    _In_ ULONGLONG correlationId
) noexcept {
    if (control->Length >= sizeof(WISPDISK_RESPONSE_HEADER)) {
        WISPDISK_RESPONSE_HEADER response{};
        InitializeResponseHeader(
            &response,
            sizeof(response),
            operation,
            status,
            correlationId
        );
        RtlCopyMemory(payload, &response, sizeof(response));
        PrepareControlCompletion(srb, control, status, sizeof(response));
    } else {
        PrepareControlCompletion(srb, control, status, 0);
    }
}

void HandleQueryVersion(
    _Inout_ PSCSI_REQUEST_BLOCK srb,
    _Inout_ PSRB_IO_CONTROL control,
    _Inout_updates_bytes_(control->Length) PUCHAR payload
) noexcept {
    WISPDISK_REQUEST_HEADER request{};
    const NTSTATUS validation = ReadAndValidateRequestHeader(
        payload,
        control->Length,
        sizeof(request),
        WispDiskControlQueryVersion,
        &request
    );
    if (!NT_SUCCESS(validation)) {
        WriteSimpleErrorResponse(
            srb,
            control,
            payload,
            WispDiskControlQueryVersion,
            validation,
            request.CorrelationId
        );
        return;
    }

    if (control->Length < sizeof(WISPDISK_QUERY_VERSION_RESPONSE)) {
        WriteSimpleErrorResponse(
            srb,
            control,
            payload,
            WispDiskControlQueryVersion,
            STATUS_BUFFER_TOO_SMALL,
            request.CorrelationId
        );
        return;
    }

    WISPDISK_QUERY_VERSION_RESPONSE response{};
    InitializeResponseHeader(
        &response.Header,
        sizeof(response),
        WispDiskControlQueryVersion,
        STATUS_SUCCESS,
        request.CorrelationId
    );
    response.VersionMajor = 0;
    response.VersionMinor = 2;
    response.Capabilities =
        WISPDISK_CAP_CREATE_DELETE | WISPDISK_CAP_READ_WRITE | WISPDISK_CAP_REMOVABLE;
    response.MaximumDiskCount = kMaximumDiskCount;
    RtlCopyMemory(payload, &response, sizeof(response));
    PrepareControlCompletion(srb, control, STATUS_SUCCESS, sizeof(response));
}

void HandleListDisks(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _Inout_ PSCSI_REQUEST_BLOCK srb,
    _Inout_ PSRB_IO_CONTROL control,
    _Inout_updates_bytes_(control->Length) PUCHAR payload
) noexcept {
    WISPDISK_REQUEST_HEADER request{};
    const NTSTATUS validation = ReadAndValidateRequestHeader(
        payload,
        control->Length,
        sizeof(request),
        WispDiskControlListDisks,
        &request
    );
    if (!NT_SUCCESS(validation) || control->Length < sizeof(WISPDISK_LIST_RESPONSE)) {
        const NTSTATUS status = NT_SUCCESS(validation) ? STATUS_BUFFER_TOO_SMALL : validation;
        WriteSimpleErrorResponse(
            srb,
            control,
            payload,
            WispDiskControlListDisks,
            status,
            request.CorrelationId
        );
        return;
    }

    WISPDISK_LIST_RESPONSE response{};
    InitializeResponseHeader(
        &response.Header,
        sizeof(response),
        WispDiskControlListDisks,
        STATUS_SUCCESS,
        request.CorrelationId
    );

    KIRQL oldIrql;
    KeAcquireSpinLock(&adapter->LunLock, &oldIrql);
    for (ULONG lunIndex = 0; lunIndex < kMaximumDiskCount; ++lunIndex) {
        const auto* lun = &adapter->Luns[lunIndex];
        if (lun->State != WispDiskLunOnline) {
            continue;
        }

        auto* info = &response.Disks[response.DiskCount++];
        info->DeviceId = lun->DeviceId;
        info->PathId = 0;
        info->TargetId = 0;
        info->Lun = lun->Lun;
        info->SizeBytes = lun->SizeBytes;
        info->LogicalSectorSize = kLogicalSectorSize;
        info->MediaKind = lun->MediaKind;
        info->RequestedDriveLetter = lun->RequestedDriveLetter;
    }
    KeReleaseSpinLock(&adapter->LunLock, oldIrql);

    RtlCopyMemory(payload, &response, sizeof(response));
    PrepareControlCompletion(srb, control, STATUS_SUCCESS, sizeof(response));
}

NTSTATUS ValidateCreateRequest(
    _In_reads_bytes_(payloadLength) const UCHAR* payload,
    _In_ ULONG payloadLength,
    _Out_ PWISPDISK_CREATE_REQUEST request
) noexcept {
    WISPDISK_REQUEST_HEADER header{};
    NTSTATUS status = ReadAndValidateRequestHeader(
        payload,
        payloadLength,
        sizeof(*request),
        WispDiskControlCreateDisk,
        &header
    );
    if (!NT_SUCCESS(status)) {
        request->Header = header;
        return status;
    }

    RtlCopyMemory(request, payload, sizeof(*request));
    if (request->SizeBytes < kMinimumDiskSize ||
        request->SizeBytes > kMaximumDiskSize ||
        request->SizeBytes % kLogicalSectorSize != 0 ||
        request->LogicalSectorSize != kLogicalSectorSize ||
        (request->MediaKind != WispDiskMediaFixed &&
         request->MediaKind != WispDiskMediaRemovable) ||
        request->RequestedDriveLetter < L'A' ||
        request->RequestedDriveLetter > L'Z' ||
        request->Reserved16 != 0 ||
        request->Reserved32 != 0) {
        return STATUS_INVALID_PARAMETER;
    }

    return STATUS_SUCCESS;
}

NTSTATUS ValidateDeleteRequest(
    _In_reads_bytes_(payloadLength) const UCHAR* payload,
    _In_ ULONG payloadLength,
    _Out_ PWISPDISK_DELETE_REQUEST request
) noexcept {
    WISPDISK_REQUEST_HEADER header{};
    NTSTATUS status = ReadAndValidateRequestHeader(
        payload,
        payloadLength,
        sizeof(*request),
        WispDiskControlDeleteDisk,
        &header
    );
    if (!NT_SUCCESS(status)) {
        request->Header = header;
        return status;
    }

    RtlCopyMemory(request, payload, sizeof(*request));
    if (request->DeviceId == 0 || request->Reserved != 0) {
        return STATUS_INVALID_PARAMETER;
    }
    return STATUS_SUCCESS;
}

void SetCreateResponse(
    _Inout_ PSCSI_REQUEST_BLOCK srb,
    _Inout_ PSRB_IO_CONTROL control,
    _Inout_updates_bytes_(control->Length) PUCHAR payload,
    _In_ const WISPDISK_CREATE_REQUEST& request,
    _In_ NTSTATUS status,
    _In_ ULONG deviceId = 0,
    _In_ UCHAR lun = 0
) noexcept {
    WISPDISK_CREATE_RESPONSE response{};
    InitializeResponseHeader(
        &response.Header,
        sizeof(response),
        WispDiskControlCreateDisk,
        status,
        request.Header.CorrelationId
    );
    response.DeviceId = deviceId;
    response.PathId = 0;
    response.TargetId = 0;
    response.Lun = lun;

    if (control->Length >= sizeof(response)) {
        RtlCopyMemory(payload, &response, sizeof(response));
        PrepareControlCompletion(srb, control, status, sizeof(response));
    } else {
        PrepareControlCompletion(srb, control, STATUS_BUFFER_TOO_SMALL, 0);
    }
}

void SetDeleteResponse(
    _Inout_ PSCSI_REQUEST_BLOCK srb,
    _Inout_ PSRB_IO_CONTROL control,
    _Inout_updates_bytes_(control->Length) PUCHAR payload,
    _In_ const WISPDISK_DELETE_REQUEST& request,
    _In_ NTSTATUS status
) noexcept {
    WISPDISK_RESPONSE_HEADER response{};
    InitializeResponseHeader(
        &response,
        sizeof(response),
        WispDiskControlDeleteDisk,
        status,
        request.Header.CorrelationId
    );
    if (control->Length >= sizeof(response)) {
        RtlCopyMemory(payload, &response, sizeof(response));
        PrepareControlCompletion(srb, control, status, sizeof(response));
    } else {
        PrepareControlCompletion(srb, control, STATUS_BUFFER_TOO_SMALL, 0);
    }
}

void ProcessCreateRequest(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _Inout_ PSCSI_REQUEST_BLOCK srb,
    _Inout_ PSRB_IO_CONTROL control,
    _Inout_updates_bytes_(control->Length) PUCHAR payload
) noexcept {
    WISPDISK_CREATE_REQUEST request{};
    NTSTATUS status = ValidateCreateRequest(payload, control->Length, &request);
    if (!NT_SUCCESS(status)) {
        SetCreateResponse(srb, control, payload, request, status);
        return;
    }

    ULONG selectedLun = kMaximumDiskCount;
    KIRQL oldIrql;
    KeAcquireSpinLock(&adapter->LunLock, &oldIrql);
    if (adapter->AdapterStopping != 0 ||
        adapter->TotalAllocatedBytes > kMaximumTotalDiskBytes - request.SizeBytes) {
        status = adapter->AdapterStopping != 0 ? STATUS_DEVICE_NOT_READY : STATUS_QUOTA_EXCEEDED;
    } else {
        for (ULONG index = 0; index < kMaximumDiskCount; ++index) {
            if (adapter->Luns[index].State == WispDiskLunEmpty) {
                selectedLun = index;
                break;
            }
        }
        if (selectedLun == kMaximumDiskCount) {
            status = STATUS_INSUFFICIENT_RESOURCES;
        }
    }
    KeReleaseSpinLock(&adapter->LunLock, oldIrql);

    if (!NT_SUCCESS(status)) {
        SetCreateResponse(srb, control, payload, request, status);
        return;
    }

    PVOID allocation = nullptr;
    const ULONG allocationStatus = StorPortAllocatePool(
        adapter,
        static_cast<ULONG>(request.SizeBytes),
        kBackingStoreTag,
        &allocation
    );
    if (allocationStatus != STOR_STATUS_SUCCESS || allocation == nullptr) {
        SetCreateResponse(
            srb,
            control,
            payload,
            request,
            STATUS_INSUFFICIENT_RESOURCES
        );
        return;
    }
    RtlZeroMemory(allocation, static_cast<SIZE_T>(request.SizeBytes));

    ULONG deviceId = 0;
    KeAcquireSpinLock(&adapter->LunLock, &oldIrql);
    auto* lun = &adapter->Luns[selectedLun];
    if (adapter->AdapterStopping != 0 || lun->State != WispDiskLunEmpty) {
        status = STATUS_DEVICE_NOT_READY;
    } else {
        deviceId = adapter->NextDeviceId++;
        if (deviceId == 0) {
            deviceId = adapter->NextDeviceId++;
        }
        lun->DeviceId = deviceId;
        lun->Lun = static_cast<UCHAR>(selectedLun);
        lun->MediaKind = request.MediaKind;
        lun->RequestedDriveLetter = request.RequestedDriveLetter;
        lun->SizeBytes = request.SizeBytes;
        lun->BackingStore = static_cast<PUCHAR>(allocation);
        lun->ActiveRequests = 0;
        KeSetEvent(&lun->NoActiveRequestsEvent, IO_NO_INCREMENT, FALSE);
        lun->State = WispDiskLunOnline;
        adapter->TotalAllocatedBytes += request.SizeBytes;
    }
    KeReleaseSpinLock(&adapter->LunLock, oldIrql);

    if (!NT_SUCCESS(status)) {
        StorPortFreePool(adapter, allocation);
        SetCreateResponse(srb, control, payload, request, status);
        return;
    }

    StorPortNotification(BusChangeDetected, adapter, static_cast<ULONG>(0));
    SetCreateResponse(
        srb,
        control,
        payload,
        request,
        STATUS_SUCCESS,
        deviceId,
        static_cast<UCHAR>(selectedLun)
    );
}

void ProcessDeleteRequest(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _Inout_ PSCSI_REQUEST_BLOCK srb,
    _Inout_ PSRB_IO_CONTROL control,
    _Inout_updates_bytes_(control->Length) PUCHAR payload
) noexcept {
    WISPDISK_DELETE_REQUEST request{};
    NTSTATUS status = ValidateDeleteRequest(payload, control->Length, &request);
    if (!NT_SUCCESS(status)) {
        SetDeleteResponse(srb, control, payload, request, status);
        return;
    }

    PWISPDISK_LUN selected = nullptr;
    KIRQL oldIrql;
    KeAcquireSpinLock(&adapter->LunLock, &oldIrql);
    for (ULONG index = 0; index < kMaximumDiskCount; ++index) {
        auto* lun = &adapter->Luns[index];
        if (lun->State == WispDiskLunOnline && lun->DeviceId == request.DeviceId) {
            lun->State = WispDiskLunStopping;
            selected = lun;
            break;
        }
    }
    KeReleaseSpinLock(&adapter->LunLock, oldIrql);

    if (selected == nullptr) {
        SetDeleteResponse(srb, control, payload, request, STATUS_NO_SUCH_DEVICE);
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

    PUCHAR backingStore = nullptr;
    KeAcquireSpinLock(&adapter->LunLock, &oldIrql);
    if (selected->State == WispDiskLunStopping && selected->DeviceId == request.DeviceId) {
        backingStore = selected->BackingStore;
        if (adapter->TotalAllocatedBytes >= selected->SizeBytes) {
            adapter->TotalAllocatedBytes -= selected->SizeBytes;
        } else {
            adapter->TotalAllocatedBytes = 0;
        }
        selected->BackingStore = nullptr;
        selected->SizeBytes = 0;
        selected->DeviceId = 0;
        selected->MediaKind = 0;
        selected->RequestedDriveLetter = 0;
        selected->State = WispDiskLunEmpty;
    }
    KeReleaseSpinLock(&adapter->LunLock, oldIrql);

    if (backingStore != nullptr) {
        StorPortFreePool(adapter, backingStore);
    }
    StorPortNotification(BusChangeDetected, adapter, static_cast<ULONG>(0));
    SetDeleteResponse(srb, control, payload, request, STATUS_SUCCESS);
}

WISPDISK_IO_CONTROL_DISPOSITION QueueManagementRequest(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _Inout_ PSCSI_REQUEST_BLOCK srb,
    _Inout_ PSRB_IO_CONTROL control,
    _Inout_updates_bytes_(control->Length) PUCHAR payload,
    _In_ WISPDISK_CONTROL_CODE operation,
    _In_ ULONGLONG correlationId
) noexcept {
    if (adapter->AdapterStopping != 0) {
        WriteSimpleErrorResponse(
            srb,
            control,
            payload,
            operation,
            STATUS_DEVICE_NOT_READY,
            correlationId
        );
        return WISPDISK_IO_CONTROL_DISPOSITION::Complete;
    }

    if (InterlockedCompareExchange(&adapter->ManagementBusy, 1, 0) != 0) {
        WriteSimpleErrorResponse(
            srb,
            control,
            payload,
            operation,
            STATUS_DEVICE_BUSY,
            correlationId
        );
        return WISPDISK_IO_CONTROL_DISPOSITION::Complete;
    }
    KeClearEvent(&adapter->ManagementIdleEvent);

    const ULONG storStatus = adapter->ManagementWorker == nullptr
        ? STOR_STATUS_INVALID_DEVICE_STATE
        : StorPortQueueWorkItem(
              adapter,
              WispDiskManagementWorker,
              adapter->ManagementWorker,
              srb
          );

    if (storStatus != STOR_STATUS_SUCCESS) {
        InterlockedExchange(&adapter->ManagementBusy, 0);
        KeSetEvent(&adapter->ManagementIdleEvent, IO_NO_INCREMENT, FALSE);
        WriteSimpleErrorResponse(
            srb,
            control,
            payload,
            operation,
            STATUS_INSUFFICIENT_RESOURCES,
            correlationId
        );
        return WISPDISK_IO_CONTROL_DISPOSITION::Complete;
    }

    control->ReturnCode = static_cast<ULONG>(STATUS_PENDING);
    srb->SrbStatus = SRB_STATUS_PENDING;
    return WISPDISK_IO_CONTROL_DISPOSITION::Pending;
}

} // namespace

WISPDISK_IO_CONTROL_DISPOSITION WispDiskHandleIoControl(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept {
    PSRB_IO_CONTROL control = nullptr;
    PUCHAR payload = nullptr;
    if (!GetControlBuffer(srb, &control, &payload)) {
        srb->DataTransferLength = 0;
        return WISPDISK_IO_CONTROL_DISPOSITION::Complete;
    }

    switch (control->ControlCode) {
        case WispDiskControlQueryVersion:
            HandleQueryVersion(srb, control, payload);
            return WISPDISK_IO_CONTROL_DISPOSITION::Complete;

        case WispDiskControlListDisks:
            HandleListDisks(adapter, srb, control, payload);
            return WISPDISK_IO_CONTROL_DISPOSITION::Complete;

        case WispDiskControlCreateDisk: {
            WISPDISK_CREATE_REQUEST request{};
            const NTSTATUS status = ValidateCreateRequest(payload, control->Length, &request);
            if (!NT_SUCCESS(status)) {
                SetCreateResponse(srb, control, payload, request, status);
                return WISPDISK_IO_CONTROL_DISPOSITION::Complete;
            }
            return QueueManagementRequest(
                adapter,
                srb,
                control,
                payload,
                WispDiskControlCreateDisk,
                request.Header.CorrelationId
            );
        }

        case WispDiskControlDeleteDisk: {
            WISPDISK_DELETE_REQUEST request{};
            const NTSTATUS status = ValidateDeleteRequest(payload, control->Length, &request);
            if (!NT_SUCCESS(status)) {
                SetDeleteResponse(srb, control, payload, request, status);
                return WISPDISK_IO_CONTROL_DISPOSITION::Complete;
            }
            return QueueManagementRequest(
                adapter,
                srb,
                control,
                payload,
                WispDiskControlDeleteDisk,
                request.Header.CorrelationId
            );
        }

        default:
            WriteSimpleErrorResponse(
                srb,
                control,
                payload,
                static_cast<WISPDISK_CONTROL_CODE>(control->ControlCode),
                STATUS_INVALID_DEVICE_REQUEST,
                0
            );
            return WISPDISK_IO_CONTROL_DISPOSITION::Complete;
    }
}

_Use_decl_annotations_
VOID WispDiskManagementWorker(PVOID deviceExtension, PVOID context, PVOID worker) {
    UNREFERENCED_PARAMETER(worker);
    auto* adapter = static_cast<PWISPDISK_ADAPTER_EXTENSION>(deviceExtension);
    auto* srb = static_cast<PSCSI_REQUEST_BLOCK>(context);
    PSRB_IO_CONTROL control = nullptr;
    PUCHAR payload = nullptr;

    if (adapter != nullptr && srb != nullptr && GetControlBuffer(srb, &control, &payload)) {
        if (control->ControlCode == WispDiskControlCreateDisk) {
            ProcessCreateRequest(adapter, srb, control, payload);
        } else if (control->ControlCode == WispDiskControlDeleteDisk) {
            ProcessDeleteRequest(adapter, srb, control, payload);
        } else {
            WriteSimpleErrorResponse(
                srb,
                control,
                payload,
                static_cast<WISPDISK_CONTROL_CODE>(control->ControlCode),
                STATUS_INVALID_DEVICE_REQUEST,
                0
            );
        }
    } else if (srb != nullptr) {
        srb->DataTransferLength = 0;
        srb->ScsiStatus = SCSISTAT_CHECK_CONDITION;
        srb->SrbStatus = SRB_STATUS_INVALID_REQUEST;
    }

    if (adapter != nullptr) {
        InterlockedExchange(&adapter->ManagementBusy, 0);
        KeSetEvent(&adapter->ManagementIdleEvent, IO_NO_INCREMENT, FALSE);
    }
    if (adapter != nullptr && srb != nullptr) {
        StorPortNotification(RequestComplete, adapter, srb);
    }
}
