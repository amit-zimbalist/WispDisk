#pragma once

#include "control_request.h"
#include "support/scoped_spin_lock.h"

namespace wispdisk::management::diagnostics {

inline constexpr ULONG kConfigureControlCode = 0x8F0;
inline constexpr ULONG kReadTraceControlCode = 0x8F1;

// Numeric values are part of the Debug diagnostic control interface.
enum class Mode : ULONG {
    Disabled = 0,
    TraceOnly = 1,
    FailResponseTransport = 2,
    RejectOperation = 3,
    DelayWorker = 4,
};

enum class TracePhase : ULONG {
    Accepted = 1,
    Worker = 2,
    Committed = 3,
    Response = 4,
    Complete = 5,
    BusChange = 6,
    Rejected = 7,
};

void Trace(
    WISPDISK_ADAPTER_EXTENSION& extension,
    ULONGLONG operation,
    ULONG device,
    TracePhase phase,
    NTSTATUS status,
    ULONG srbStatus = 0) noexcept;
// Internal callers supply the guard for the shared adapter lock.
void TraceLocked(
    WISPDISK_ADAPTER_EXTENSION& extension,
    const support::ScopedSpinLock& lock,
    ULONGLONG operation,
    ULONG device,
    TracePhase phase,
    NTSTATUS status,
    ULONG srbStatus = 0) noexcept;
[[nodiscard]] Mode TakeFault(WISPDISK_ADAPTER_EXTENSION& extension) noexcept;
void HandleControl(WISPDISK_ADAPTER_EXTENSION& extension, const ControlRequest& control) noexcept;

} // namespace wispdisk::management::diagnostics
