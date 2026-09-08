#pragma once

#include "driver.h"

namespace wispdisk::management {

struct ControlRequest {
    PSCSI_REQUEST_BLOCK srb{};
    PSRB_IO_CONTROL control{};
    PUCHAR payload{};

    [[nodiscard]] explicit operator bool() const noexcept {
        return srb != nullptr && control != nullptr && payload != nullptr;
    }
};

[[nodiscard]] ControlRequest GetControlRequest(
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept;

[[nodiscard]] WISPDISK_RESPONSE_HEADER MakeResponseHeader(
    _In_ ULONG structureSize,
    _In_ WISPDISK_CONTROL_CODE operation,
    _In_ NTSTATUS status,
    _In_ ULONGLONG correlationId
) noexcept;

[[nodiscard]] NTSTATUS ReadAndValidateRequestHeader(
    _In_reads_bytes_(payloadLength) const UCHAR* payload,
    _In_ ULONG payloadLength,
    _In_ ULONG requiredLength,
    _In_ WISPDISK_CONTROL_CODE expectedOperation,
    _Out_ PWISPDISK_REQUEST_HEADER header
) noexcept;

[[nodiscard]] NTSTATUS ValidateCreateRequest(
    _In_ const ControlRequest& controlRequest,
    _Out_ PWISPDISK_CREATE_REQUEST request
) noexcept;

[[nodiscard]] NTSTATUS ValidateDeleteRequest(
    _In_ const ControlRequest& controlRequest,
    _Out_ PWISPDISK_DELETE_REQUEST request
) noexcept;

void WriteSimpleErrorResponse(
    _In_ const ControlRequest& controlRequest,
    _In_ WISPDISK_CONTROL_CODE operation,
    _In_ NTSTATUS status,
    _In_ ULONGLONG correlationId
) noexcept;

void WriteCreateResponse(
    _In_ const ControlRequest& controlRequest,
    _In_ const WISPDISK_CREATE_REQUEST& request,
    _In_ NTSTATUS status,
    _In_ ULONG deviceId = 0,
    _In_ UCHAR lun = 0
) noexcept;

void WriteDeleteResponse(
    _In_ const ControlRequest& controlRequest,
    _In_ const WISPDISK_DELETE_REQUEST& request,
    _In_ NTSTATUS status
) noexcept;

} // namespace wispdisk::management
