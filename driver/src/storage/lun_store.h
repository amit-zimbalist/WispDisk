#pragma once

#include "driver.h"
#include "lun_io_lease.h"
#include "support/scoped_spin_lock.h"

namespace wispdisk::driver {
class AdapterContext;
}
namespace wispdisk::management {
class ManagementController;
}
namespace wispdisk::support {
class StorPortPoolAllocation;
}

namespace wispdisk::storage {

// Non-owning access to the adapter's LUN storage. Mutations are private to the
// management workflow, which coordinates them with operation bookkeeping.
class LunStore final {
public:
    explicit LunStore(WISPDISK_ADAPTER_EXTENSION& extension) noexcept;

    _IRQL_requires_max_(DISPATCH_LEVEL)
    [[nodiscard]] LunIoLease AcquireIo(const SCSI_REQUEST_BLOCK& srb) const noexcept;
    _IRQL_requires_max_(DISPATCH_LEVEL)
    void SnapshotOnlineDisks(WISPDISK_LIST_RESPONSE& response) const noexcept;
    _IRQL_requires_max_(DISPATCH_LEVEL)
    [[nodiscard]] ULONG SnapshotOnlineLuns(UCHAR (&luns)[kMaximumDiskCount]) const noexcept;

private:
    friend class driver::AdapterContext;
    friend class management::ManagementController;
    friend class LunIoLease;

    void Initialize() const noexcept;
    _IRQL_requires_(PASSIVE_LEVEL)
    [[nodiscard]] NTSTATUS
    CreateDisk(const WISPDISK_CREATE_REQUEST& request, ULONG& deviceId, UCHAR& lun) const noexcept;
    [[nodiscard]] NTSTATUS SelectAvailableLun(ULONGLONG sizeBytes, ULONG& selected) const noexcept;
    [[nodiscard]] NTSTATUS CommitNewLun(
        const WISPDISK_CREATE_REQUEST& request,
        ULONG selected,
        support::StorPortPoolAllocation& allocation,
        ULONG& deviceId) const noexcept;
    [[nodiscard]] PWISPDISK_LUN MarkLunStopping(ULONG deviceId) const noexcept;
    [[nodiscard]] PUCHAR DetachBackingStore(WISPDISK_LUN& lun, ULONG deviceId) const noexcept;
    _IRQL_requires_(PASSIVE_LEVEL)
    void DrainAndFreeBacking(WISPDISK_LUN& lun, ULONG deviceId) const noexcept;
    void MarkAllLunsStopping() const noexcept;
    [[nodiscard]] PWISPDISK_LUN FirstStoppingLun() const noexcept;

    // The controller supplies the guard so storage and operation results can
    // be published together without acquiring the shared lock twice.
    void
    FinalizeRemovalLocked(WISPDISK_LUN& lun, const support::ScopedSpinLock& lock) const noexcept;
    void BindCreateOperationLocked(
        UCHAR lun,
        ULONGLONG operation,
        const support::ScopedSpinLock& lock) const noexcept;
    void SnapshotStateLocked(WISPDISK_STATE_RESPONSE& response, const support::ScopedSpinLock& lock)
        const noexcept;

    [[nodiscard]] PWISPDISK_LUN
    AcquireLunIo(const SCSI_REQUEST_BLOCK& srb, LunView& view) const noexcept;
    void ReleaseLunIo(WISPDISK_LUN& lun) const noexcept;

    WISPDISK_ADAPTER_EXTENSION& extension_;
};

} // namespace wispdisk::storage
