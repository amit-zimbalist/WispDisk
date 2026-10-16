#include "logging.h"

#include <TraceLoggingProvider.h>

namespace wispdisk::logging {
namespace {

// Keep this GUID in sync with cli/src/logging/driver_events.rs. A custom
// TraceLogging provider uses an ordinary ETW session even from kernel mode.
TRACELOGGING_DEFINE_PROVIDER(
    provider,
    "WispDisk.Driver",
    (0x8b6cae34, 0x6a65, 0x4cf1, 0xa5, 0x5b, 0x0c, 0x9a, 0x5c, 0x22, 0x48, 0x96));

bool registered{};

template <UCHAR level> void WriteEvent(const Event& event) noexcept {
    TraceLoggingWrite(
        provider,
        "DriverEvent",
        TraceLoggingLevel(level),
        TraceLoggingKeyword(0x1),
        TraceLoggingString(event.Message, "Message"),
        TraceLoggingUInt64(event.AdapterInstance, "AdapterInstance"),
        TraceLoggingUInt64(event.OperationId, "OperationId"),
        TraceLoggingUInt32(event.DeviceId, "DeviceId"),
        TraceLoggingNTStatus(event.Status, "Status"),
        TraceLoggingUInt64(event.SizeBytes, "SizeBytes"),
        TraceLoggingUInt32(event.SrbStatus, "SrbStatus"),
        TraceLoggingUInt32(event.StorPortStatus, "StorPortStatus"));
}

} // namespace

_Use_decl_annotations_
NTSTATUS Initialize() noexcept {
    if (registered) {
        return STATUS_SUCCESS;
    }
    const NTSTATUS status = TraceLoggingRegister(provider);
    registered = NT_SUCCESS(status);
    INFO("TracingInitialized", .Status = status);
    return status;
}

_Use_decl_annotations_
void Shutdown() noexcept {
    if (registered) {
        INFO("TracingShutdown");
        TraceLoggingUnregister(provider);
        registered = false;
    }
}

void Write(Level level, const Event& event) noexcept {
    if (KeGetCurrentIrql() > DISPATCH_LEVEL) {
        return;
    }
    // Each instantiation has constant event metadata, including its level.
    switch (level) {
        case Level::Error:
            WriteEvent<2>(event);
            break;
        case Level::Warning:
            WriteEvent<3>(event);
            break;
        case Level::Information:
            WriteEvent<4>(event);
            break;
        case Level::Verbose:
            WriteEvent<5>(event);
            break;
    }
}

} // namespace wispdisk::logging
