#pragma once

extern "C" {
#include <ntddk.h>
}

namespace wispdisk::support {

class ScopedSpinLock final {
public:
    _IRQL_requires_max_(DISPATCH_LEVEL)
    explicit ScopedSpinLock(_Inout_ PKSPIN_LOCK spinLock) noexcept;
    ~ScopedSpinLock() noexcept;

    ScopedSpinLock(const ScopedSpinLock&) = delete;
    ScopedSpinLock& operator=(const ScopedSpinLock&) = delete;
    ScopedSpinLock(ScopedSpinLock&&) = delete;
    ScopedSpinLock& operator=(ScopedSpinLock&&) = delete;

    // Passing this live, noncopyable guard ties a locked helper to its lock.
    void AssertProtects(_In_ const KSPIN_LOCK* spinLock) const noexcept {
        NT_ASSERT(spinLock_ == spinLock);
        UNREFERENCED_PARAMETER(spinLock);
    }

private:
    PKSPIN_LOCK spinLock_;
    KIRQL oldIrql_{};
};

} // namespace wispdisk::support
