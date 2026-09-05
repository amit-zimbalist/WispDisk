#include "driver.h"

namespace {

enum class IO_CONTROL_DISPOSITION {
    Complete,
    Pending,
};

struct WISPDISK_LUN_VIEW {
    PWISPDISK_LUN Lun;
    PUCHAR BackingStore;
    ULONGLONG SizeBytes;
    ULONG MediaKind;
    ULONG DeviceId;
};

constexpr UCHAR kDirectAccessDevice = 0x00;
constexpr UCHAR kVpdSupportedPages = 0x00;
constexpr UCHAR kVpdUnitSerialNumber = 0x80;
constexpr UCHAR kVpdDeviceIdentification = 0x83;
constexpr ULONG kSenseDataLength = 18;

ULONG ReadBigEndian32(_In_reads_(4) const UCHAR* value) noexcept {
    return (static_cast<ULONG>(value[0]) << 24) |
           (static_cast<ULONG>(value[1]) << 16) |
           (static_cast<ULONG>(value[2]) << 8) |
           static_cast<ULONG>(value[3]);
}

ULONGLONG ReadBigEndian64(_In_reads_(8) const UCHAR* value) noexcept {
    ULONGLONG result = 0;
    for (ULONG index = 0; index < 8; ++index) {
        result = (result << 8) | value[index];
    }
    return result;
}

void WriteBigEndian16(_Out_writes_(2) UCHAR* destination, USHORT value) noexcept {
    destination[0] = static_cast<UCHAR>(value >> 8);
    destination[1] = static_cast<UCHAR>(value);
}

void WriteBigEndian32(_Out_writes_(4) UCHAR* destination, ULONG value) noexcept {
    destination[0] = static_cast<UCHAR>(value >> 24);
    destination[1] = static_cast<UCHAR>(value >> 16);
    destination[2] = static_cast<UCHAR>(value >> 8);
    destination[3] = static_cast<UCHAR>(value);
}

void WriteBigEndian64(_Out_writes_(8) UCHAR* destination, ULONGLONG value) noexcept {
    for (LONG index = 7; index >= 0; --index) {
        destination[index] = static_cast<UCHAR>(value);
        value >>= 8;
    }
}

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

UCHAR SetSense(
    _Inout_ PSCSI_REQUEST_BLOCK srb,
    _In_ UCHAR senseKey,
    _In_ UCHAR additionalSenseCode,
    _In_ UCHAR additionalSenseQualifier = 0
) noexcept {
    srb->DataTransferLength = 0;
    srb->ScsiStatus = SCSISTAT_CHECK_CONDITION;

    if (srb->SenseInfoBuffer != nullptr && srb->SenseInfoBufferLength >= kSenseDataLength) {
        auto* sense = static_cast<PUCHAR>(srb->SenseInfoBuffer);
        RtlZeroMemory(sense, kSenseDataLength);
        sense[0] = SCSI_SENSE_ERRORCODE_FIXED_CURRENT;
        sense[2] = senseKey;
        sense[7] = 10;
        sense[12] = additionalSenseCode;
        sense[13] = additionalSenseQualifier;
        return SRB_STATUS_ERROR | SRB_STATUS_AUTOSENSE_VALID;
    }

    return SRB_STATUS_ERROR;
}

UCHAR CopyScsiResponse(
    _Inout_ PSCSI_REQUEST_BLOCK srb,
    _In_reads_bytes_(responseLength) const void* response,
    _In_ ULONG responseLength
) noexcept {
    if (responseLength != 0 && srb->DataBuffer == nullptr) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }

    const ULONG available = srb->DataTransferLength;
    const ULONG transferred = responseLength < available ? responseLength : available;
    if (transferred != 0) {
        RtlCopyMemory(srb->DataBuffer, response, transferred);
    }
    srb->DataTransferLength = transferred;
    return responseLength > available ? SRB_STATUS_DATA_OVERRUN : SRB_STATUS_SUCCESS;
}

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

IO_CONTROL_DISPOSITION QueueManagementRequest(
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
        return IO_CONTROL_DISPOSITION::Complete;
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
        return IO_CONTROL_DISPOSITION::Complete;
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
        return IO_CONTROL_DISPOSITION::Complete;
    }

    control->ReturnCode = static_cast<ULONG>(STATUS_PENDING);
    srb->SrbStatus = SRB_STATUS_PENDING;
    return IO_CONTROL_DISPOSITION::Pending;
}

IO_CONTROL_DISPOSITION HandleIoControl(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept {
    PSRB_IO_CONTROL control = nullptr;
    PUCHAR payload = nullptr;
    if (!GetControlBuffer(srb, &control, &payload)) {
        srb->DataTransferLength = 0;
        return IO_CONTROL_DISPOSITION::Complete;
    }

    switch (control->ControlCode) {
        case WispDiskControlQueryVersion:
            HandleQueryVersion(srb, control, payload);
            return IO_CONTROL_DISPOSITION::Complete;

        case WispDiskControlListDisks:
            HandleListDisks(adapter, srb, control, payload);
            return IO_CONTROL_DISPOSITION::Complete;

        case WispDiskControlCreateDisk: {
            WISPDISK_CREATE_REQUEST request{};
            const NTSTATUS status = ValidateCreateRequest(payload, control->Length, &request);
            if (!NT_SUCCESS(status)) {
                SetCreateResponse(srb, control, payload, request, status);
                return IO_CONTROL_DISPOSITION::Complete;
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
                return IO_CONTROL_DISPOSITION::Complete;
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
            return IO_CONTROL_DISPOSITION::Complete;
    }
}

bool AcquireLunForIo(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _In_ PSCSI_REQUEST_BLOCK srb,
    _Out_ WISPDISK_LUN_VIEW* view
) noexcept {
    RtlZeroMemory(view, sizeof(*view));
    if (srb->PathId != 0 || srb->TargetId != 0 || srb->Lun >= kMaximumDiskCount) {
        return false;
    }

    bool acquired = false;
    KIRQL oldIrql;
    KeAcquireSpinLock(&adapter->LunLock, &oldIrql);
    auto* lun = &adapter->Luns[srb->Lun];
    if (lun->State == WispDiskLunOnline && lun->BackingStore != nullptr) {
        if (lun->ActiveRequests++ == 0) {
            KeClearEvent(&lun->NoActiveRequestsEvent);
        }
        view->Lun = lun;
        view->BackingStore = lun->BackingStore;
        view->SizeBytes = lun->SizeBytes;
        view->MediaKind = lun->MediaKind;
        view->DeviceId = lun->DeviceId;
        acquired = true;
    }
    KeReleaseSpinLock(&adapter->LunLock, oldIrql);
    return acquired;
}

void ReleaseLunForIo(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _In_ PWISPDISK_LUN lun
) noexcept {
    KIRQL oldIrql;
    KeAcquireSpinLock(&adapter->LunLock, &oldIrql);
    NT_ASSERT(lun->ActiveRequests != 0);
    if (lun->ActiveRequests != 0 && --lun->ActiveRequests == 0) {
        KeSetEvent(&lun->NoActiveRequestsEvent, IO_NO_INCREMENT, FALSE);
    }
    KeReleaseSpinLock(&adapter->LunLock, oldIrql);
}

void WriteDeviceIdHex(_Out_writes_(8) UCHAR* destination, ULONG deviceId) noexcept {
    static constexpr UCHAR digits[] = "0123456789ABCDEF";
    for (ULONG index = 0; index < 8; ++index) {
        const ULONG shift = (7 - index) * 4;
        destination[index] = digits[(deviceId >> shift) & 0x0fU];
    }
}

UCHAR HandleInquiry(
    _In_ const WISPDISK_LUN_VIEW& view,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept {
    if (srb->CdbLength < 6) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }

    const auto* cdb = srb->Cdb;
    const bool enableVitalProductData = (cdb[1] & 0x01U) != 0;
    const UCHAR pageCode = cdb[2];
    const ULONG allocationLength = cdb[4];
    UCHAR response[64]{};
    ULONG responseLength = 0;

    if (!enableVitalProductData) {
        if (pageCode != 0) {
            return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
        }
        response[0] = kDirectAccessDevice;
        response[1] = view.MediaKind == WispDiskMediaRemovable ? 0x80U : 0x00U;
        response[2] = 0x05;
        response[3] = 0x02;
        response[4] = 31;
        static constexpr UCHAR vendor[8] = {'W', 'I', 'S', 'P', 'D', 'I', 'S', 'K'};
        static constexpr UCHAR product[16] = {
            'V', 'I', 'R', 'T', 'U', 'A', 'L', ' ', 'D', 'I', 'S', 'K', ' ', ' ', ' ', ' '
        };
        static constexpr UCHAR revision[4] = {'0', '0', '0', '2'};
        RtlCopyMemory(&response[8], vendor, sizeof(vendor));
        RtlCopyMemory(&response[16], product, sizeof(product));
        RtlCopyMemory(&response[32], revision, sizeof(revision));
        responseLength = 36;
    } else if (pageCode == kVpdSupportedPages) {
        response[0] = kDirectAccessDevice;
        response[1] = kVpdSupportedPages;
        response[3] = 3;
        response[4] = kVpdSupportedPages;
        response[5] = kVpdUnitSerialNumber;
        response[6] = kVpdDeviceIdentification;
        responseLength = 7;
    } else if (pageCode == kVpdUnitSerialNumber) {
        response[0] = kDirectAccessDevice;
        response[1] = kVpdUnitSerialNumber;
        response[3] = 14;
        static constexpr UCHAR prefix[6] = {'W', 'I', 'S', 'P', 'D', 'K'};
        RtlCopyMemory(&response[4], prefix, sizeof(prefix));
        WriteDeviceIdHex(&response[10], view.DeviceId);
        responseLength = 18;
    } else if (pageCode == kVpdDeviceIdentification) {
        response[0] = kDirectAccessDevice;
        response[1] = kVpdDeviceIdentification;
        response[4] = 0x02; // ASCII.
        response[5] = 0x01; // T10 vendor identifier, associated with the LUN.
        response[7] = 22;
        static constexpr UCHAR vendor[8] = {'W', 'I', 'S', 'P', 'D', 'I', 'S', 'K'};
        static constexpr UCHAR prefix[6] = {'W', 'I', 'S', 'P', 'D', 'K'};
        RtlCopyMemory(&response[8], vendor, sizeof(vendor));
        RtlCopyMemory(&response[16], prefix, sizeof(prefix));
        WriteDeviceIdHex(&response[22], view.DeviceId);
        WriteBigEndian16(&response[2], 26);
        responseLength = 30;
    } else {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }

    if (responseLength > allocationLength) {
        responseLength = allocationLength;
    }
    return CopyScsiResponse(srb, response, responseLength);
}

UCHAR HandleReadCapacity10(
    _In_ const WISPDISK_LUN_VIEW& view,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept {
    UCHAR response[8]{};
    const ULONGLONG blockCount = view.SizeBytes / kLogicalSectorSize;
    const ULONGLONG lastLba = blockCount - 1;
    const ULONG reportedLastLba = lastLba > MAXULONG ? MAXULONG : static_cast<ULONG>(lastLba);
    WriteBigEndian32(&response[0], reportedLastLba);
    WriteBigEndian32(&response[4], kLogicalSectorSize);
    return CopyScsiResponse(srb, response, sizeof(response));
}

UCHAR HandleReadCapacity16(
    _In_ const WISPDISK_LUN_VIEW& view,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept {
    if (srb->CdbLength < 16 || (srb->Cdb[1] & 0x1fU) != SERVICE_ACTION_READ_CAPACITY16) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }
    UCHAR response[32]{};
    WriteBigEndian64(&response[0], (view.SizeBytes / kLogicalSectorSize) - 1);
    WriteBigEndian32(&response[8], kLogicalSectorSize);
    return CopyScsiResponse(srb, response, sizeof(response));
}

UCHAR HandleRequestSense(_Inout_ PSCSI_REQUEST_BLOCK srb) noexcept {
    if (srb->CdbLength < 6) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }
    UCHAR response[kSenseDataLength]{};
    response[0] = SCSI_SENSE_ERRORCODE_FIXED_CURRENT;
    response[7] = 10;
    ULONG responseLength = kSenseDataLength;
    if (responseLength > srb->Cdb[4]) {
        responseLength = srb->Cdb[4];
    }
    return CopyScsiResponse(srb, response, responseLength);
}

UCHAR HandleModeSense(_Inout_ PSCSI_REQUEST_BLOCK srb, bool tenByte) noexcept {
    const ULONG requiredCdbLength = tenByte ? 10 : 6;
    if (srb->CdbLength < requiredCdbLength) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }

    const UCHAR pageCode = srb->Cdb[2] & 0x3fU;
    if (pageCode != 0x00 && pageCode != 0x08 && pageCode != 0x3f) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }

    UCHAR response[32]{};
    ULONG responseLength = tenByte ? 8 : 4;
    if (pageCode == 0x08 || pageCode == 0x3f) {
        auto* page = &response[responseLength];
        page[0] = 0x08;
        page[1] = 0x12;
        responseLength += 20;
    }

    ULONG allocationLength;
    if (tenByte) {
        WriteBigEndian16(&response[0], static_cast<USHORT>(responseLength - 2));
        allocationLength = (static_cast<ULONG>(srb->Cdb[7]) << 8) | srb->Cdb[8];
    } else {
        response[0] = static_cast<UCHAR>(responseLength - 1);
        allocationLength = srb->Cdb[4];
    }
    if (responseLength > allocationLength) {
        responseLength = allocationLength;
    }
    return CopyScsiResponse(srb, response, responseLength);
}

bool ParseReadWriteCdb(
    _In_ PSCSI_REQUEST_BLOCK srb,
    _Out_ ULONGLONG* logicalBlock,
    _Out_ ULONG* blockCount,
    _Out_ bool* isWrite
) noexcept {
    const UCHAR operation = srb->Cdb[0];
    *logicalBlock = 0;
    *blockCount = 0;
    *isWrite = operation == SCSIOP_WRITE6 || operation == SCSIOP_WRITE ||
               operation == SCSIOP_WRITE12 || operation == SCSIOP_WRITE16;

    switch (operation) {
        case SCSIOP_READ6:
        case SCSIOP_WRITE6:
            if (srb->CdbLength < 6) {
                return false;
            }
            *logicalBlock = (static_cast<ULONGLONG>(srb->Cdb[1] & 0x1fU) << 16) |
                            (static_cast<ULONGLONG>(srb->Cdb[2]) << 8) |
                            srb->Cdb[3];
            *blockCount = srb->Cdb[4] == 0 ? 256 : srb->Cdb[4];
            return true;

        case SCSIOP_READ:
        case SCSIOP_WRITE:
            if (srb->CdbLength < 10) {
                return false;
            }
            *logicalBlock = ReadBigEndian32(&srb->Cdb[2]);
            *blockCount = (static_cast<ULONG>(srb->Cdb[7]) << 8) | srb->Cdb[8];
            return true;

        case SCSIOP_READ12:
        case SCSIOP_WRITE12:
            if (srb->CdbLength < 12) {
                return false;
            }
            *logicalBlock = ReadBigEndian32(&srb->Cdb[2]);
            *blockCount = ReadBigEndian32(&srb->Cdb[6]);
            return true;

        case SCSIOP_READ16:
        case SCSIOP_WRITE16:
            if (srb->CdbLength < 16) {
                return false;
            }
            *logicalBlock = ReadBigEndian64(&srb->Cdb[2]);
            *blockCount = ReadBigEndian32(&srb->Cdb[10]);
            return true;

        default:
            return false;
    }
}

UCHAR HandleReadWrite(
    _In_ const WISPDISK_LUN_VIEW& view,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept {
    ULONGLONG logicalBlock = 0;
    ULONG blockCount = 0;
    bool isWrite = false;
    if (!ParseReadWriteCdb(srb, &logicalBlock, &blockCount, &isWrite)) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }

    if (blockCount == 0) {
        srb->DataTransferLength = 0;
        return SRB_STATUS_SUCCESS;
    }

    const ULONGLONG diskBlocks = view.SizeBytes / kLogicalSectorSize;
    if (logicalBlock >= diskBlocks || blockCount > diskBlocks - logicalBlock) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_ILLEGAL_BLOCK);
    }

    const ULONGLONG byteOffset = logicalBlock * kLogicalSectorSize;
    const ULONGLONG byteCount64 = static_cast<ULONGLONG>(blockCount) * kLogicalSectorSize;
    if (byteCount64 > kMaximumTransferLength || byteCount64 > MAXULONG ||
        srb->DataTransferLength < byteCount64 ||
        srb->DataBuffer == nullptr) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }

    const ULONG byteCount = static_cast<ULONG>(byteCount64);
    auto* requestBuffer = static_cast<PUCHAR>(srb->DataBuffer);
    auto* diskBuffer = view.BackingStore + static_cast<SIZE_T>(byteOffset);
    if (isWrite) {
        RtlCopyMemory(diskBuffer, requestBuffer, byteCount);
    } else {
        RtlCopyMemory(requestBuffer, diskBuffer, byteCount);
    }
    srb->DataTransferLength = byteCount;
    return SRB_STATUS_SUCCESS;
}

UCHAR HandleReportLuns(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept {
    if (srb->CdbLength < 12 || srb->PathId != 0 || srb->TargetId != 0) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }

    UCHAR response[8 + (WISPDISK_MAXIMUM_DISK_COUNT * 8)]{};
    ULONG count = 0;
    KIRQL oldIrql;
    KeAcquireSpinLock(&adapter->LunLock, &oldIrql);
    for (ULONG index = 0; index < kMaximumDiskCount; ++index) {
        if (adapter->Luns[index].State == WispDiskLunOnline) {
            response[8 + (count * 8) + 1] = adapter->Luns[index].Lun;
            ++count;
        }
    }
    KeReleaseSpinLock(&adapter->LunLock, oldIrql);

    WriteBigEndian32(&response[0], count * 8);
    ULONG responseLength = 8 + (count * 8);
    const ULONG allocationLength = ReadBigEndian32(&srb->Cdb[6]);
    if (responseLength > allocationLength) {
        responseLength = allocationLength;
    }
    return CopyScsiResponse(srb, response, responseLength);
}

UCHAR HandleExecuteScsi(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept {
    if (srb->CdbLength == 0) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }

    if (srb->Cdb[0] == SCSIOP_REPORT_LUNS) {
        return HandleReportLuns(adapter, srb);
    }

    WISPDISK_LUN_VIEW view{};
    if (!AcquireLunForIo(adapter, srb, &view)) {
        srb->DataTransferLength = 0;
        return SRB_STATUS_NO_DEVICE;
    }

    UCHAR status;
    switch (srb->Cdb[0]) {
        case SCSIOP_TEST_UNIT_READY:
        case SCSIOP_START_STOP_UNIT:
        case SCSIOP_MEDIUM_REMOVAL:
        case SCSIOP_SYNCHRONIZE_CACHE:
        case SCSIOP_SYNCHRONIZE_CACHE16:
        case SCSIOP_VERIFY:
        case SCSIOP_VERIFY16:
        case SCSIOP_MODE_SELECT:
        case SCSIOP_MODE_SELECT10:
            srb->DataTransferLength = 0;
            status = SRB_STATUS_SUCCESS;
            break;

        case SCSIOP_INQUIRY:
            status = HandleInquiry(view, srb);
            break;

        case SCSIOP_REQUEST_SENSE:
            status = HandleRequestSense(srb);
            break;

        case SCSIOP_READ_CAPACITY:
            status = HandleReadCapacity10(view, srb);
            break;

        case SCSIOP_READ_CAPACITY16:
            status = HandleReadCapacity16(view, srb);
            break;

        case SCSIOP_MODE_SENSE:
            status = HandleModeSense(srb, false);
            break;

        case SCSIOP_MODE_SENSE10:
            status = HandleModeSense(srb, true);
            break;

        case SCSIOP_READ6:
        case SCSIOP_WRITE6:
        case SCSIOP_READ:
        case SCSIOP_WRITE:
        case SCSIOP_READ12:
        case SCSIOP_WRITE12:
        case SCSIOP_READ16:
        case SCSIOP_WRITE16:
            status = HandleReadWrite(view, srb);
            break;

        default:
            status = SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_ILLEGAL_COMMAND);
            break;
    }

    ReleaseLunForIo(adapter, view.Lun);
    return status;
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
            if (HandleIoControl(adapter, srb) == IO_CONTROL_DISPOSITION::Pending) {
                return TRUE;
            }
            if (srb->SrbStatus == SRB_STATUS_PENDING) {
                srb->SrbStatus = SRB_STATUS_INVALID_REQUEST;
            }
            CompleteSrb(deviceExtension, srb, srb->SrbStatus);
            break;

        case SRB_FUNCTION_EXECUTE_SCSI:
            CompleteSrb(deviceExtension, srb, HandleExecuteScsi(adapter, srb));
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
