#pragma once

#include "management.h"
#include "control_request.h"

namespace wispdisk::management {
void HandleReserveOperation(PWISPDISK_ADAPTER_EXTENSION adapter, const ControlRequest& control) noexcept;
void HandleQueryOperation(PWISPDISK_ADAPTER_EXTENSION adapter, const ControlRequest& control) noexcept;
void HandleListState(PWISPDISK_ADAPTER_EXTENSION adapter, const ControlRequest& control) noexcept;
// Called under LunLock after the management slot has been acquired. True means
// a new mutation was accepted and must be queued; false means response is ready.
bool AcceptReservedOperation(PWISPDISK_ADAPTER_EXTENSION adapter, const ControlRequest& control) noexcept;
void ProcessReservedOperation(PWISPDISK_ADAPTER_EXTENSION adapter, const ControlRequest& control) noexcept;
void FailReservedOperation(PWISPDISK_ADAPTER_EXTENSION adapter, const ControlRequest& control, NTSTATUS status) noexcept;
}
