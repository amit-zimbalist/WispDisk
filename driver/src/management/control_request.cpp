#include "control_request.h"

namespace wispdisk::management {

static bool HasSrbSignature(_In_reads_bytes_(WISPDISK_SRB_SIGNATURE_LENGTH)
                                const UCHAR* signature) noexcept {
    return RtlCompareMemory(signature, WISPDISK_SRB_SIGNATURE, WISPDISK_SRB_SIGNATURE_LENGTH) ==
           WISPDISK_SRB_SIGNATURE_LENGTH;
}

NTSTATUS ReadAndValidateRequestHeader(
    _In_reads_bytes_(payloadLength) const UCHAR* payload,
    _In_ ULONG payloadLength,
    _In_ ULONG requiredLength,
    _In_ WISPDISK_CONTROL_CODE expectedOperation,
    _Out_ WISPDISK_REQUEST_HEADER& header) noexcept {
    if (payloadLength < requiredLength || requiredLength < sizeof(WISPDISK_REQUEST_HEADER)) {
        return STATUS_BUFFER_TOO_SMALL;
    }

    RtlCopyMemory(&header, payload, sizeof(header));
    if (header.StructSize != requiredLength ||
        header.ProtocolVersion != WISPDISK_PROTOCOL_VERSION ||
        header.Operation != static_cast<ULONG>(expectedOperation) || header.Flags != 0) {
        return STATUS_INVALID_PARAMETER;
    }

    return STATUS_SUCCESS;
}

WISPDISK_RESPONSE_HEADER MakeResponseHeader(
    _In_ ULONG structureSize,
    _In_ WISPDISK_CONTROL_CODE operation,
    _In_ NTSTATUS status,
    _In_ ULONGLONG correlationId) noexcept {
    return {
        .StructSize = structureSize,
        .ProtocolVersion = WISPDISK_PROTOCOL_VERSION,
        .Operation = static_cast<ULONG>(operation),
        .Status = status,
        .CorrelationId = correlationId,
    };
}

void ControlRequest::PrepareCompletion(_In_ NTSTATUS status, _In_ ULONG payloadLength)
    const noexcept {
    ioControl->ReturnCode = static_cast<ULONG>(status);
    ioControl->Length = payloadLength;
    srb->DataTransferLength = sizeof(SRB_IO_CONTROL) + payloadLength;
    srb->ScsiStatus = SCSISTAT_GOOD;
    srb->SrbStatus = SRB_STATUS_SUCCESS;
}

ControlRequest ControlRequest::FromSrb(_Inout_opt_ PSCSI_REQUEST_BLOCK requestSrb) noexcept {
    if (requestSrb == nullptr || requestSrb->DataBuffer == nullptr ||
        requestSrb->DataTransferLength < sizeof(SRB_IO_CONTROL)) {
        return {};
    }

    auto* envelope = static_cast<PSRB_IO_CONTROL>(requestSrb->DataBuffer);
    if (envelope->HeaderLength != sizeof(SRB_IO_CONTROL) || !HasSrbSignature(envelope->Signature) ||
        envelope->Length > requestSrb->DataTransferLength - sizeof(SRB_IO_CONTROL)) {
        return {};
    }

    return {
        .srb = requestSrb,
        .ioControl = envelope,
        .payload = reinterpret_cast<PUCHAR>(envelope) + sizeof(SRB_IO_CONTROL),
    };
}

NTSTATUS ValidateCreateRequest(
    _In_ const ControlRequest& controlRequest,
    _Out_ WISPDISK_CREATE_REQUEST& request) noexcept {
    WISPDISK_REQUEST_HEADER header{};
    const NTSTATUS status = controlRequest.ReadRequest(WispDiskControlCreateDisk, request, header);
    if (!NT_SUCCESS(status)) {
        request.Header = header;
        return status;
    }

    if (request.SizeBytes < kMinimumDiskSize || request.SizeBytes > kMaximumDiskSize ||
        request.SizeBytes % kLogicalSectorSize != 0 ||
        request.LogicalSectorSize != kLogicalSectorSize ||
        (request.MediaKind != WispDiskMediaFixed && request.MediaKind != WispDiskMediaRemovable) ||
        request.RequestedDriveLetter < L'A' || request.RequestedDriveLetter > L'Z' ||
        request.Reserved16 != 0 || request.Reserved32 != 0) {
        return STATUS_INVALID_PARAMETER;
    }

    return STATUS_SUCCESS;
}

NTSTATUS ValidateDeleteRequest(
    _In_ const ControlRequest& controlRequest,
    _Out_ WISPDISK_DELETE_REQUEST& request) noexcept {
    WISPDISK_REQUEST_HEADER header{};
    const NTSTATUS status = controlRequest.ReadRequest(WispDiskControlDeleteDisk, request, header);
    if (!NT_SUCCESS(status)) {
        request.Header = header;
        return status;
    }

    if (request.DeviceId == 0 || request.Reserved != 0) {
        return STATUS_INVALID_PARAMETER;
    }
    return STATUS_SUCCESS;
}

void ControlRequest::WriteError(
    _In_ WISPDISK_CONTROL_CODE operation,
    _In_ NTSTATUS status,
    _In_ ULONGLONG correlationId) const noexcept {
    if (ioControl->Length >= sizeof(WISPDISK_RESPONSE_HEADER)) {
        const WISPDISK_RESPONSE_HEADER response =
            MakeResponseHeader(sizeof(WISPDISK_RESPONSE_HEADER), operation, status, correlationId);
        WriteResponse(response, status);
        return;
    }

    PrepareCompletion(status, 0);
}

void WriteCreateResponse(
    _In_ const ControlRequest& controlRequest,
    _In_ const WISPDISK_CREATE_REQUEST& request,
    _In_ NTSTATUS status,
    _In_ ULONG deviceId,
    _In_ UCHAR lun) noexcept {
    const WISPDISK_CREATE_RESPONSE response{
        .Header = MakeResponseHeader(
            sizeof(WISPDISK_CREATE_RESPONSE),
            WispDiskControlCreateDisk,
            status,
            request.Header.CorrelationId),
        .DeviceId = deviceId,
        .PathId = 0,
        .TargetId = 0,
        .Lun = lun,
    };

    controlRequest.WriteResponse(response, status);
}

void WriteDeleteResponse(
    _In_ const ControlRequest& controlRequest,
    _In_ const WISPDISK_DELETE_REQUEST& request,
    _In_ NTSTATUS status) noexcept {
    const WISPDISK_RESPONSE_HEADER response = MakeResponseHeader(
        sizeof(WISPDISK_RESPONSE_HEADER),
        WispDiskControlDeleteDisk,
        status,
        request.Header.CorrelationId);
    controlRequest.WriteResponse(response, status);
}

} // namespace wispdisk::management
