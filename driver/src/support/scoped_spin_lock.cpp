#include "scoped_spin_lock.h"

namespace wispdisk::support {

// Static analysis does not pair the IRQL transition performed by a C++
// constructor with the matching destructor, so suppress that local warning.
#pragma warning(suppress: 28167)
ScopedSpinLock::ScopedSpinLock(_Inout_ PKSPIN_LOCK spinLock) noexcept
    : spinLock_{spinLock} {
    KeAcquireSpinLock(spinLock_, &oldIrql_);
}

#pragma warning(suppress: 28167)
ScopedSpinLock::~ScopedSpinLock() noexcept {
    KeReleaseSpinLock(spinLock_, oldIrql_);
}

} // namespace wispdisk::support
