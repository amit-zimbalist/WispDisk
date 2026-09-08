#pragma once

#include "control_request.h"

namespace wispdisk::management {

void HandleQueryVersion(_In_ const ControlRequest& controlRequest) noexcept;

void HandleListDisks(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _In_ const ControlRequest& controlRequest
) noexcept;

void ProcessCreateRequest(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _In_ const ControlRequest& controlRequest
) noexcept;

void ProcessDeleteRequest(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _In_ const ControlRequest& controlRequest
) noexcept;

} // namespace wispdisk::management
