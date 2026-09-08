#pragma once

extern "C" {
#include <ntddk.h>
#include <storport.h>
}

namespace wispdisk::support {

class StorPortPoolAllocation final {
public:
    StorPortPoolAllocation(
        _In_ PVOID deviceExtension,
        _In_ ULONG size,
        _In_ ULONG tag
    ) noexcept;
    ~StorPortPoolAllocation() noexcept;

    StorPortPoolAllocation(const StorPortPoolAllocation&) = delete;
    StorPortPoolAllocation& operator=(const StorPortPoolAllocation&) = delete;
    StorPortPoolAllocation(StorPortPoolAllocation&&) = delete;
    StorPortPoolAllocation& operator=(StorPortPoolAllocation&&) = delete;

    [[nodiscard]] PVOID get() const noexcept;
    [[nodiscard]] explicit operator bool() const noexcept;
    [[nodiscard]] PVOID release() noexcept;

private:
    PVOID deviceExtension_;
    PVOID allocation_{};
};

} // namespace wispdisk::support
