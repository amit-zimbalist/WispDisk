#include "control_request.h"

namespace wispdisk::management {

static bool HasSrbSignature(
    _In_reads_bytes_(WISPDISK_SRB_SIGNATURE_LENGTH) const UCHAR* signature
) noexcept {
    return RtlCompareMemory(
               signature,
               WISPDISK_SRB_SIGNATURE,
               WISPDISK_SRB_SIGNATURE_LENGTH
           ) == WISPDISK_SRB_SIGNATURE_LENGTH;
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

WISPDISK_RESPONSE_HEADER MakeResponseHeader(
    _In_ ULONG structureSize,
    _In_ WISPDISK_CONTROL_CODE operation,
    _In_ NTSTATUS status,
    _In_ ULONGLONG correlationId
) noexcept {
    return {
        .StructSize = structureSize,
        .ProtocolVersion = WISPDISK_PROTOCOL_VERSION,
        .Operation = static_cast<ULONG>(operation),
        .Status = status,
        .CorrelationId = correlationId,
    };
}

static void PrepareControlCompletion(
    _In_ const ControlRequest& controlRequest,
    _In_ NTSTATUS status,
    _In_ ULONG payloadLength
) noexcept {
    controlRequest.control->ReturnCode = static_cast<ULONG>(status);
    controlRequest.control->Length = payloadLength;
    controlRequest.srb->DataTransferLength = sizeof(SRB_IO_CONTROL) + payloadLength;
    controlRequest.srb->ScsiStatus = SCSISTAT_GOOD;
    controlRequest.srb->SrbStatus = SRB_STATUS_SUCCESS;
}

ControlRequest GetControlRequest(_Inout_ PSCSI_REQUEST_BLOCK srb) noexcept {
    if (srb->DataBuffer == nullptr || srb->DataTransferLength < sizeof(SRB_IO_CONTROL)) {
        return {};
    }

    auto* control = static_cast<PSRB_IO_CONTROL>(srb->DataBuffer);
    if (control->HeaderLength != sizeof(SRB_IO_CONTROL) ||
        !HasSrbSignature(control->Signature) ||
        control->Length > srb->DataTransferLength - sizeof(SRB_IO_CONTROL)) {
        return {};
    }

    return {
        .srb = srb,
        .control = control,
        .payload = reinterpret_cast<PUCHAR>(control) + sizeof(SRB_IO_CONTROL),
    };
}

NTSTATUS ValidateCreateRequest(
    _In_ const ControlRequest& controlRequest,
    _Out_ PWISPDISK_CREATE_REQUEST request
) noexcept {
    WISPDISK_REQUEST_HEADER header{};
    const NTSTATUS status = ReadAndValidateRequestHeader(
        controlRequest.payload,
        controlRequest.control->Length,
        sizeof(*request),
        WispDiskControlCreateDisk,
        &header
    );
    if (!NT_SUCCESS(status)) {
        request->Header = header;
        return status;
    }

    RtlCopyMemory(request, controlRequest.payload, sizeof(*request));
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
    _In_ const ControlRequest& controlRequest,
    _Out_ PWISPDISK_DELETE_REQUEST request
) noexcept {
    WISPDISK_REQUEST_HEADER header{};
    const NTSTATUS status = ReadAndValidateRequestHeader(
        controlRequest.payload,
        controlRequest.control->Length,
        sizeof(*request),
        WispDiskControlDeleteDisk,
        &header
    );
    if (!NT_SUCCESS(status)) {
        request->Header = header;
        return status;
    }

    RtlCopyMemory(request, controlRequest.payload, sizeof(*request));
    if (request->DeviceId == 0 || request->Reserved != 0) {
        return STATUS_INVALID_PARAMETER;
    }
    return STATUS_SUCCESS;
}

void WriteSimpleErrorResponse(
    _In_ const ControlRequest& controlRequest,
    _In_ WISPDISK_CONTROL_CODE operation,
    _In_ NTSTATUS status,
    _In_ ULONGLONG correlationId
) noexcept {
    if (controlRequest.control->Length >= sizeof(WISPDISK_RESPONSE_HEADER)) {
        const WISPDISK_RESPONSE_HEADER response = MakeResponseHeader(
            sizeof(WISPDISK_RESPONSE_HEADER),
            operation,
            status,
            correlationId
        );
        RtlCopyMemory(controlRequest.payload, &response, sizeof(response));
        PrepareControlCompletion(controlRequest, status, sizeof(response));
        return;
    }

    PrepareControlCompletion(controlRequest, status, 0);
}

void WriteCreateResponse(
    _In_ const ControlRequest& controlRequest,
    _In_ const WISPDISK_CREATE_REQUEST& request,
    _In_ NTSTATUS status,
    _In_ ULONG deviceId,
    _In_ UCHAR lun
) noexcept {
    const WISPDISK_CREATE_RESPONSE response{
        .Header = MakeResponseHeader(
            sizeof(WISPDISK_CREATE_RESPONSE),
            WispDiskControlCreateDisk,
            status,
            request.Header.CorrelationId
        ),
        .DeviceId = deviceId,
        .PathId = 0,
        .TargetId = 0,
        .Lun = lun,
    };

    if (controlRequest.control->Length >= sizeof(response)) {
        RtlCopyMemory(controlRequest.payload, &response, sizeof(response));
        PrepareControlCompletion(controlRequest, status, sizeof(response));
        return;
    }

    PrepareControlCompletion(controlRequest, STATUS_BUFFER_TOO_SMALL, 0);
}

void WriteDeleteResponse(
    _In_ const ControlRequest& controlRequest,
    _In_ const WISPDISK_DELETE_REQUEST& request,
    _In_ NTSTATUS status
) noexcept {
    const WISPDISK_RESPONSE_HEADER response = MakeResponseHeader(
        sizeof(WISPDISK_RESPONSE_HEADER),
        WispDiskControlDeleteDisk,
        status,
        request.Header.CorrelationId
    );
    if (controlRequest.control->Length >= sizeof(response)) {
        RtlCopyMemory(controlRequest.payload, &response, sizeof(response));
        PrepareControlCompletion(controlRequest, status, sizeof(response));
        return;
    }

    PrepareControlCompletion(controlRequest, STATUS_BUFFER_TOO_SMALL, 0);
}

} // namespace wispdisk::management
