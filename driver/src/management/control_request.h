#pragma once

#include "driver.h"

namespace wispdisk::management {

// Copy/layout requirements only; protocol contents are validated separately.
// Compiler traits avoid pulling user-mode CRT headers into the miniport.
template <typename T>
concept WireStructure = __is_trivially_copyable(T) && __is_standard_layout(T);

struct ControlRequest {
    PSCSI_REQUEST_BLOCK srb{};
    PSRB_IO_CONTROL ioControl{};
    PUCHAR payload{};

    // Returns an empty view when the SRB or its control envelope is invalid.
    [[nodiscard]] static ControlRequest
    FromSrb(_Inout_opt_ PSCSI_REQUEST_BLOCK requestSrb) noexcept;

    template <WireStructure Request>
    [[nodiscard]] NTSTATUS ReadRequest(
        _In_ WISPDISK_CONTROL_CODE expectedOperation,
        _Inout_ Request& request,
        _Out_ WISPDISK_REQUEST_HEADER& header) const noexcept;

    template <WireStructure Response>
    void WriteResponse(_In_ const Response& response, _In_ NTSTATUS status) const noexcept;

    void WriteError(
        _In_ WISPDISK_CONTROL_CODE operation,
        _In_ NTSTATUS status,
        _In_ ULONGLONG correlationId) const noexcept;

    // Finalizes metadata after capacity validation and payload writing.
    // SRB completion and ownership transfer remain explicit in the caller.
    void PrepareCompletion(_In_ NTSTATUS status, _In_ ULONG payloadLength) const noexcept;

    [[nodiscard]] explicit operator bool() const noexcept {
        return srb != nullptr && ioControl != nullptr && payload != nullptr;
    }
};

[[nodiscard]] WISPDISK_RESPONSE_HEADER MakeResponseHeader(
    _In_ ULONG structureSize,
    _In_ WISPDISK_CONTROL_CODE operation,
    _In_ NTSTATUS status,
    _In_ ULONGLONG correlationId) noexcept;

[[nodiscard]] NTSTATUS ReadAndValidateRequestHeader(
    _In_reads_bytes_(payloadLength) const UCHAR* payload,
    _In_ ULONG payloadLength,
    _In_ ULONG requiredLength,
    _In_ WISPDISK_CONTROL_CODE expectedOperation,
    _Out_ WISPDISK_REQUEST_HEADER& header) noexcept;

// Request is a wire structure beginning with WISPDISK_REQUEST_HEADER. Copy it
// only after common validation succeeds; the header remains available on error.
template <WireStructure Request>
[[nodiscard]] NTSTATUS ControlRequest::ReadRequest(
    _In_ WISPDISK_CONTROL_CODE expectedOperation,
    _Inout_ Request& request,
    _Out_ WISPDISK_REQUEST_HEADER& header) const noexcept {
    static_assert(sizeof(Request) >= sizeof(WISPDISK_REQUEST_HEADER));
    static_assert(sizeof(Request) <= MAXULONG);

    header = {};
    const NTSTATUS status = ReadAndValidateRequestHeader(
        payload,
        ioControl->Length,
        static_cast<ULONG>(sizeof(Request)),
        expectedOperation,
        header);
    if (NT_SUCCESS(status)) {
        RtlCopyMemory(&request, payload, sizeof(Request));
    }
    return status;
}

[[nodiscard]] NTSTATUS ValidateCreateRequest(
    _In_ const ControlRequest& controlRequest,
    _Out_ WISPDISK_CREATE_REQUEST& request) noexcept;

[[nodiscard]] NTSTATUS ValidateDeleteRequest(
    _In_ const ControlRequest& controlRequest,
    _Out_ WISPDISK_DELETE_REQUEST& request) noexcept;

// Writes a complete wire response without completing or transferring the SRB.
// Capacity is the validated incoming control length. A short buffer receives
// no payload and STATUS_BUFFER_TOO_SMALL; special error fallbacks stay explicit.
template <WireStructure Response>
void ControlRequest::WriteResponse(_In_ const Response& response, _In_ NTSTATUS status)
    const noexcept {
    static_assert(sizeof(Response) >= sizeof(WISPDISK_RESPONSE_HEADER));
    static_assert(sizeof(Response) <= MAXULONG - sizeof(SRB_IO_CONTROL));

    if (ioControl->Length < sizeof(Response)) {
        PrepareCompletion(STATUS_BUFFER_TOO_SMALL, 0);
        return;
    }
    RtlCopyMemory(payload, &response, sizeof(Response));
    PrepareCompletion(status, static_cast<ULONG>(sizeof(Response)));
}

void WriteCreateResponse(
    _In_ const ControlRequest& controlRequest,
    _In_ const WISPDISK_CREATE_REQUEST& request,
    _In_ NTSTATUS status,
    _In_ ULONG deviceId = 0,
    _In_ UCHAR lun = 0) noexcept;

void WriteDeleteResponse(
    _In_ const ControlRequest& controlRequest,
    _In_ const WISPDISK_DELETE_REQUEST& request,
    _In_ NTSTATUS status) noexcept;

} // namespace wispdisk::management
