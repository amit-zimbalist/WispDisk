#pragma once

#include <ntddk.h>

namespace wispdisk::logging {

enum class Level : UCHAR { Error = 2, Warning = 3, Information = 4, Verbose = 5 };

// All fields are copied by ETW before Write returns. Message must be resident
// and null terminated; callers use bounded string literals.
struct Event {
    const char* Message;
    ULONGLONG AdapterInstance{};
    ULONGLONG OperationId{};
    ULONG DeviceId{};
    NTSTATUS Status{STATUS_SUCCESS};
    ULONGLONG SizeBytes{};
    ULONG SrbStatus{};
    ULONG StorPortStatus{};
};

_IRQL_requires_(PASSIVE_LEVEL)
[[nodiscard]] NTSTATUS Initialize() noexcept;
_IRQL_requires_(PASSIVE_LEVEL)
void Shutdown() noexcept;

// Some adapter-control callbacks run at DIRQL. Skip tracing there instead of
// calling ETW above its supported IRQL. Never wait, allocate, or perform file I/O.
void Write(Level level, const Event& event) noexcept;

} // namespace wispdisk::logging

// Optional arguments are designated Event fields, in declaration order.
#define LOG(level, message, ...)                                                                   \
    ::wispdisk::logging::Write(                                                                    \
        ::wispdisk::logging::Level::level,                                                         \
        ::wispdisk::logging::Event{.Message = (message), __VA_ARGS__})
#define INFO(message, ...) LOG(Information, message, __VA_ARGS__)
#define WARN(message, ...) LOG(Warning, message, __VA_ARGS__)
#define ERROR(message, ...) LOG(Error, message, __VA_ARGS__)
#define DEBUG(message, ...) LOG(Verbose, message, __VA_ARGS__)
