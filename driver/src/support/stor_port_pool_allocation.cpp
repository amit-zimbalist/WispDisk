#include "stor_port_pool_allocation.h"

namespace wispdisk::support {

StorPortPoolAllocation::StorPortPoolAllocation(
    _In_ PVOID deviceExtension,
    _In_ ULONG size,
    _In_ ULONG tag
) noexcept
    : deviceExtension_{deviceExtension} {
    PVOID allocation = nullptr;
    if (StorPortAllocatePool(deviceExtension_, size, tag, &allocation) ==
        STOR_STATUS_SUCCESS) {
        allocation_ = allocation;
    }
}

StorPortPoolAllocation::~StorPortPoolAllocation() noexcept {
    if (allocation_ != nullptr) {
        StorPortFreePool(deviceExtension_, allocation_);
    }
}

PVOID StorPortPoolAllocation::get() const noexcept {
    return allocation_;
}

StorPortPoolAllocation::operator bool() const noexcept {
    return allocation_ != nullptr;
}

PVOID StorPortPoolAllocation::release() noexcept {
    const auto allocation = allocation_;
    allocation_ = nullptr;
    return allocation;
}

} // namespace wispdisk::support
