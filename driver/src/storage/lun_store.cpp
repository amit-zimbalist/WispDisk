#include "lun_store.h"
#include "support/stor_port_pool_allocation.h"

namespace wispdisk::storage {

LunStore::LunStore(WISPDISK_ADAPTER_EXTENSION& extension) noexcept : extension_{extension} {}

void LunStore::Initialize() const noexcept {
    for (ULONG index = 0; index < kMaximumDiskCount; ++index) {
        extension_.Luns[index].Lun = static_cast<UCHAR>(index);
        KeInitializeEvent(&extension_.Luns[index].NoActiveRequestsEvent, NotificationEvent, TRUE);
    }
}

_Use_decl_annotations_
LunIoLease LunStore::AcquireIo(const SCSI_REQUEST_BLOCK& srb) const noexcept {
    return LunIoLease{extension_, srb};
}

static WISPDISK_DISK_INFO DiskInfo(const WISPDISK_LUN& lun) noexcept {
    return {
        .DeviceId = lun.DeviceId,
        .Lun = lun.Lun,
        .SizeBytes = lun.SizeBytes,
        .LogicalSectorSize = kLogicalSectorSize,
        .MediaKind = lun.MediaKind,
        .RequestedDriveLetter = lun.RequestedDriveLetter,
    };
}

_Use_decl_annotations_
void LunStore::SnapshotOnlineDisks(WISPDISK_LIST_RESPONSE& response) const noexcept {
    support::ScopedSpinLock lock{&extension_.LunLock};
    response.DiskCount = 0;
    for (const auto& lun : extension_.Luns) {
        if (lun.State == WispDiskLunState::Online) {
            response.Disks[response.DiskCount++] = DiskInfo(lun);
        }
    }
}

void LunStore::SnapshotStateLocked(
    WISPDISK_STATE_RESPONSE& response,
    const support::ScopedSpinLock& lock) const noexcept {
    lock.AssertProtects(&extension_.LunLock);
    response.AllocatedBytes = extension_.TotalAllocatedBytes;
    response.DiskCount = 0;
    for (const auto& lun : extension_.Luns) {
        if (lun.State == WispDiskLunState::Empty) {
            continue;
        }
        response.Disks[response.DiskCount++] = {
            .Disk = DiskInfo(lun),
            .CreateOperationId = lun.CreateOperationId,
            .State = static_cast<ULONG>(lun.State),
        };
    }
}

void LunStore::BindCreateOperationLocked(
    UCHAR lun,
    ULONGLONG operation,
    const support::ScopedSpinLock& lock) const noexcept {
    lock.AssertProtects(&extension_.LunLock);
    NT_ASSERT(lun < kMaximumDiskCount);
    extension_.Luns[lun].CreateOperationId = operation;
}

_Use_decl_annotations_
ULONG LunStore::SnapshotOnlineLuns(UCHAR (&luns)[kMaximumDiskCount]) const noexcept {
    support::ScopedSpinLock lock{&extension_.LunLock};
    ULONG count = 0;
    for (const auto& lun : extension_.Luns) {
        if (lun.State == WispDiskLunState::Online) {
            luns[count++] = lun.Lun;
        }
    }
    return count;
}

NTSTATUS LunStore::SelectAvailableLun(ULONGLONG sizeBytes, ULONG& selected) const noexcept {
    support::ScopedSpinLock lock{&extension_.LunLock};
    if (extension_.AdapterStopping != 0) {
        return STATUS_DEVICE_NOT_READY;
    }
    if (extension_.TotalAllocatedBytes > kMaximumTotalDiskBytes - sizeBytes) {
        return STATUS_QUOTA_EXCEEDED;
    }
    for (ULONG index = 0; index < kMaximumDiskCount; ++index) {
        if (extension_.Luns[index].State == WispDiskLunState::Empty) {
            selected = index;
            return STATUS_SUCCESS;
        }
    }
    return STATUS_INSUFFICIENT_RESOURCES;
}

NTSTATUS LunStore::CommitNewLun(
    const WISPDISK_CREATE_REQUEST& request,
    ULONG selected,
    support::StorPortPoolAllocation& allocation,
    ULONG& deviceId) const noexcept {
    support::ScopedSpinLock lock{&extension_.LunLock};
    auto& lun = extension_.Luns[selected];
    if (extension_.AdapterStopping != 0 || lun.State != WispDiskLunState::Empty) {
        return STATUS_DEVICE_NOT_READY;
    }
    if (extension_.NextDeviceId == 0) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    deviceId = extension_.NextDeviceId++;
    lun.DeviceId = deviceId;
    lun.Lun = static_cast<UCHAR>(selected);
    lun.MediaKind = request.MediaKind;
    lun.RequestedDriveLetter = request.RequestedDriveLetter;
    lun.SizeBytes = request.SizeBytes;
    lun.CreateOperationId = 0;
    lun.BackingStore = static_cast<PUCHAR>(allocation.release());
    lun.ActiveRequests = 0;
    KeSetEvent(&lun.NoActiveRequestsEvent, IO_NO_INCREMENT, FALSE);
    lun.State = WispDiskLunState::Online;
    extension_.TotalAllocatedBytes += request.SizeBytes;
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
NTSTATUS LunStore::CreateDisk(const WISPDISK_CREATE_REQUEST& request, ULONG& deviceId, UCHAR& lun)
    const noexcept {
    ULONG selected = kMaximumDiskCount;
    NTSTATUS status = SelectAvailableLun(request.SizeBytes, selected);
    if (!NT_SUCCESS(status)) {
        KdPrint(("WispDisk: create rejected, status=0x%08lX\n", status));
        return status;
    }
    support::StorPortPoolAllocation allocation{
        &extension_,
        static_cast<ULONG>(request.SizeBytes),
        kBackingStoreTag};
    if (!allocation) {
        KdPrint(("WispDisk: backing-store allocation failed, size=%I64u\n", request.SizeBytes));
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(allocation.get(), static_cast<SIZE_T>(request.SizeBytes));
    status = CommitNewLun(request, selected, allocation, deviceId);
    if (!NT_SUCCESS(status)) {
        KdPrint(("WispDisk: create commit failed, status=0x%08lX\n", status));
        return status;
    }
    lun = static_cast<UCHAR>(selected);
    KdPrint(
        ("WispDisk: created disk id=%lu lun=%lu size=%I64u\n",
         deviceId,
         selected,
         request.SizeBytes));
    return STATUS_SUCCESS;
}

PWISPDISK_LUN LunStore::MarkLunStopping(ULONG deviceId) const noexcept {
    support::ScopedSpinLock lock{&extension_.LunLock};
    for (auto& lun : extension_.Luns) {
        if (lun.State == WispDiskLunState::Online && lun.DeviceId == deviceId) {
            lun.State = WispDiskLunState::Stopping;
            return &lun;
        }
    }
    return nullptr;
}

PUCHAR LunStore::DetachBackingStore(WISPDISK_LUN& lun, ULONG deviceId) const noexcept {
    support::ScopedSpinLock lock{&extension_.LunLock};
    if (lun.State != WispDiskLunState::Stopping || lun.DeviceId != deviceId) {
        return nullptr;
    }
    auto* backingStore = lun.BackingStore;
    lun.BackingStore = nullptr;
    return backingStore;
}

void LunStore::FinalizeRemovalLocked(WISPDISK_LUN& lun, const support::ScopedSpinLock& lock)
    const noexcept {
    lock.AssertProtects(&extension_.LunLock);
    extension_.TotalAllocatedBytes = extension_.TotalAllocatedBytes >= lun.SizeBytes
                                         ? extension_.TotalAllocatedBytes - lun.SizeBytes
                                         : 0;
    lun.BackingStore = nullptr;
    lun.SizeBytes = 0;
    lun.DeviceId = 0;
    lun.MediaKind = 0;
    lun.RequestedDriveLetter = 0;
    lun.CreateOperationId = 0;
    lun.State = WispDiskLunState::Empty;
}

static void WaitForLunIo(WISPDISK_LUN& lun) noexcept {
    KeWaitForSingleObject(&lun.NoActiveRequestsEvent, Executive, KernelMode, FALSE, nullptr);
}

_Use_decl_annotations_
void LunStore::DrainAndFreeBacking(WISPDISK_LUN& lun, ULONG deviceId) const noexcept {
    WaitForLunIo(lun);
    auto* backing = DetachBackingStore(lun, deviceId);
    if (backing != nullptr) {
        StorPortFreePool(&extension_, backing);
    }
}

void LunStore::MarkAllLunsStopping() const noexcept {
    support::ScopedSpinLock lock{&extension_.LunLock};
    for (auto& lun : extension_.Luns) {
        if (lun.State == WispDiskLunState::Online) {
            lun.State = WispDiskLunState::Stopping;
        }
    }
}

PWISPDISK_LUN LunStore::FirstStoppingLun() const noexcept {
    support::ScopedSpinLock lock{&extension_.LunLock};
    for (auto& lun : extension_.Luns) {
        if (lun.State == WispDiskLunState::Stopping) {
            return &lun;
        }
    }
    return nullptr;
}

PWISPDISK_LUN LunStore::AcquireLunIo(const SCSI_REQUEST_BLOCK& srb, LunView& view) const noexcept {
    if (srb.PathId != 0 || srb.TargetId != 0 || srb.Lun >= kMaximumDiskCount) {
        return nullptr;
    }
    support::ScopedSpinLock lock{&extension_.LunLock};
    auto& lun = extension_.Luns[srb.Lun];
    if (lun.State != WispDiskLunState::Online || lun.BackingStore == nullptr) {
        return nullptr;
    }
    if (lun.ActiveRequests++ == 0) {
        KeClearEvent(&lun.NoActiveRequestsEvent);
    }
    view = {
        .backingStore = lun.BackingStore,
        .sizeBytes = lun.SizeBytes,
        .mediaKind = lun.MediaKind,
        .deviceId = lun.DeviceId};
    return &lun;
}

void LunStore::ReleaseLunIo(WISPDISK_LUN& lun) const noexcept {
    support::ScopedSpinLock lock{&extension_.LunLock};
    NT_ASSERT(lun.ActiveRequests != 0);
    if (lun.ActiveRequests != 0 && --lun.ActiveRequests == 0) {
        KeSetEvent(&lun.NoActiveRequestsEvent, IO_NO_INCREMENT, FALSE);
    }
}

} // namespace wispdisk::storage
