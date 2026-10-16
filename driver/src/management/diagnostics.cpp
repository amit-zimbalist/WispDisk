#include "diagnostics.h"
#include "logging.h"

namespace wispdisk::management::diagnostics {
namespace {

const char* PhaseName(TracePhase phase) noexcept {
    switch (phase) {
        case TracePhase::Accepted:
            return "ManagementAccepted";
        case TracePhase::Worker:
            return "ManagementWorker";
        case TracePhase::Committed:
            return "ManagementCommitted";
        case TracePhase::Response:
            return "ManagementResponse";
        case TracePhase::Complete:
            return "ManagementComplete";
        case TracePhase::BusChange:
            return "ManagementBusChange";
        case TracePhase::Rejected:
            return "ManagementRejected";
    }
    return "ManagementUnknownPhase";
}

void LogPhase(
    const WISPDISK_ADAPTER_EXTENSION& extension,
    ULONGLONG operation,
    ULONG device,
    TracePhase phase,
    NTSTATUS status,
    ULONG srbStatus) noexcept {
    logging::Write(
        NT_SUCCESS(status) ? logging::Level::Verbose : logging::Level::Warning,
        {.Message = PhaseName(phase),
         .AdapterInstance = extension.AdapterInstance,
         .OperationId = operation,
         .DeviceId = device,
         .Status = status,
         .SrbStatus = srbStatus});
}

} // namespace

void TraceLocked(
    WISPDISK_ADAPTER_EXTENSION& extension,
    const support::ScopedSpinLock& lock,
    ULONGLONG operation,
    ULONG device,
    TracePhase phase,
    NTSTATUS status,
    ULONG srbStatus) noexcept {
    lock.AssertProtects(&extension.LunLock);
    LogPhase(extension, operation, device, phase, status, srbStatus);
#if DBG
    if (extension.TraceEnabled == 0) {
        return;
    }
    const auto sequence = ++extension.TraceSequence;
    extension.Trace[(sequence - 1) % RTL_NUMBER_OF(extension.Trace)] = {
        .Sequence = sequence,
        .OperationId = operation,
        .Timestamp = KeQueryPerformanceCounter(nullptr).QuadPart,
        .DeviceId = device,
        .Phase = static_cast<ULONG>(phase),
        .Status = status,
        .SrbStatus = srbStatus,
    };
#else
    UNREFERENCED_PARAMETER(extension);
    UNREFERENCED_PARAMETER(operation);
    UNREFERENCED_PARAMETER(device);
    UNREFERENCED_PARAMETER(phase);
    UNREFERENCED_PARAMETER(status);
    UNREFERENCED_PARAMETER(srbStatus);
#endif
}

void Trace(
    WISPDISK_ADAPTER_EXTENSION& extension,
    ULONGLONG operation,
    ULONG device,
    TracePhase phase,
    NTSTATUS status,
    ULONG srbStatus) noexcept {
#if DBG
    support::ScopedSpinLock lock{&extension.LunLock};
    TraceLocked(extension, lock, operation, device, phase, status, srbStatus);
#else
    LogPhase(extension, operation, device, phase, status, srbStatus);
#endif
}

Mode TakeFault(WISPDISK_ADAPTER_EXTENSION& extension) noexcept {
#if DBG
    support::ScopedSpinLock lock{&extension.LunLock};
    if (extension.FaultRemaining != 0) {
        --extension.FaultRemaining;
        return static_cast<Mode>(extension.FaultMode);
    }
#else
    UNREFERENCED_PARAMETER(extension);
#endif
    return Mode::Disabled;
}

#if DBG
struct TraceSnapshot {
    ULONGLONG Sequence;
    ULONG Count;
    ULONG Reserved;
    WISPDISK_ADAPTER_EXTENSION::TraceEntry Entries[16];
};

static NTSTATUS Configure(WISPDISK_ADAPTER_EXTENSION& extension, ULONG mode, ULONG count) noexcept {
    constexpr ULONG kMaximumFaultCount = 1000;
    if (mode > static_cast<ULONG>(Mode::DelayWorker) || count > kMaximumFaultCount) {
        return STATUS_INVALID_PARAMETER;
    }
    const auto requestedMode = static_cast<Mode>(mode);
    support::ScopedSpinLock lock{&extension.LunLock};
    extension.TraceEnabled = requestedMode != Mode::Disabled;
    extension.FaultMode = mode;
    extension.FaultRemaining =
        requestedMode != Mode::Disabled && requestedMode != Mode::TraceOnly ? count : 0;
    return STATUS_SUCCESS;
}

static void Snapshot(WISPDISK_ADAPTER_EXTENSION& extension, TraceSnapshot& snapshot) noexcept {
    support::ScopedSpinLock lock{&extension.LunLock};
    snapshot = {
        .Sequence = extension.TraceSequence,
        .Count = static_cast<ULONG>(
            min(extension.TraceSequence, static_cast<ULONGLONG>(RTL_NUMBER_OF(snapshot.Entries)))),
    };
    const auto first = extension.TraceSequence - snapshot.Count;
    for (ULONG index = 0; index < snapshot.Count; ++index) {
        snapshot.Entries[index] = extension.Trace[(first + index) % RTL_NUMBER_OF(extension.Trace)];
    }
}
#endif

void HandleControl(WISPDISK_ADAPTER_EXTENSION& extension, const ControlRequest& control) noexcept {
#if DBG
    struct Configuration {
        WISPDISK_REQUEST_HEADER Header;
        ULONG Mode;
        ULONG Count;
    };
    struct TraceResponse {
        WISPDISK_RESPONSE_HEADER Header;
        TraceSnapshot Trace;
    };
    static_assert(sizeof(TraceResponse) == 680);
    const bool configure = control.ioControl->ControlCode == kConfigureControlCode;
    const auto operation = static_cast<WISPDISK_CONTROL_CODE>(control.ioControl->ControlCode);
    WISPDISK_REQUEST_HEADER header{};
    Configuration request{};
    NTSTATUS status = configure ? control.ReadRequest(operation, request, header)
                                : ReadAndValidateRequestHeader(
                                      control.payload,
                                      control.ioControl->Length,
                                      sizeof(header),
                                      operation,
                                      header);
    if (NT_SUCCESS(status) &&
        control.ioControl->Length <
            (configure ? sizeof(WISPDISK_RESPONSE_HEADER) : sizeof(TraceResponse))) {
        status = STATUS_BUFFER_TOO_SMALL;
    }
    if (!NT_SUCCESS(status)) {
        control.WriteError(operation, status, header.CorrelationId);
        return;
    }
    if (configure) {
        control.WriteError(
            operation,
            Configure(extension, request.Mode, request.Count),
            header.CorrelationId);
        return;
    }
    TraceResponse response{
        .Header = MakeResponseHeader(
            sizeof(TraceResponse),
            operation,
            STATUS_SUCCESS,
            header.CorrelationId)};
    Snapshot(extension, response.Trace);
    control.WriteResponse(response, STATUS_SUCCESS);
#else
    UNREFERENCED_PARAMETER(extension);
    control.WriteError(
        static_cast<WISPDISK_CONTROL_CODE>(control.ioControl->ControlCode),
        STATUS_NOT_SUPPORTED,
        0);
#endif
}

} // namespace wispdisk::management::diagnostics
