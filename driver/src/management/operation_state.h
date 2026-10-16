#pragma once

#include "management.h"
#include "control_request.h"

namespace wispdisk::management {
void HandleReserveOperation(
    PWISPDISK_ADAPTER_EXTENSION adapter,
    const ControlRequest& control) noexcept;
void HandleQueryOperation(
    PWISPDISK_ADAPTER_EXTENSION adapter,
    const ControlRequest& control) noexcept;
void HandleListState(PWISPDISK_ADAPTER_EXTENSION adapter, const ControlRequest& control) noexcept;
[[nodiscard]] WispDiskIoControlDisposition HandleReservedCreateControl(
    PWISPDISK_ADAPTER_EXTENSION adapter,
    const ControlRequest& control) noexcept;
[[nodiscard]] WispDiskIoControlDisposition HandleReservedDeleteControl(
    PWISPDISK_ADAPTER_EXTENSION adapter,
    const ControlRequest& control) noexcept;
} // namespace wispdisk::management
