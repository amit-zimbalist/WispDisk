#pragma once

extern "C" {
#include <ntddk.h>
}

namespace wispdisk::support {

class ScopedSpinLock final {
public:
    explicit ScopedSpinLock(_Inout_ PKSPIN_LOCK spinLock) noexcept;
    ~ScopedSpinLock() noexcept;

    ScopedSpinLock(const ScopedSpinLock&) = delete;
    ScopedSpinLock& operator=(const ScopedSpinLock&) = delete;
    ScopedSpinLock(ScopedSpinLock&&) = delete;
    ScopedSpinLock& operator=(ScopedSpinLock&&) = delete;

private:
    PKSPIN_LOCK spinLock_;
    KIRQL oldIrql_{};
};

} // namespace wispdisk::support
