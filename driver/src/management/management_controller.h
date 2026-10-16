#pragma once

#include "management.h"
#include "control_request.h"
#include "support/scoped_spin_lock.h"

namespace wispdisk::management {

// A copied result, independent of the operation table and its lock.
struct OperationResult {
    ULONGLONG adapterInstance{};
    ULONGLONG operationId{};
    ULONG mutation{};
    WISPDISK_OPERATION_STATE state{};
    NTSTATUS status{};
    ULONG deviceId{};
    UCHAR lun{};
};

class ManagementController final {
public:
    explicit ManagementController(WISPDISK_ADAPTER_EXTENSION& extension) noexcept;

    // Initialize once after adapter storage and its lock are initialized,
    // before admitting requests. Shutdown runs at PASSIVE_LEVEL during teardown.
    _IRQL_requires_max_(DISPATCH_LEVEL)
    [[nodiscard]] bool Initialize() const noexcept;
    // Closes admission, waits for the final worker access, drains disks,
    // and frees the worker before returning.
    _IRQL_requires_(PASSIVE_LEVEL)
    void Shutdown() const noexcept;

    _IRQL_requires_max_(DISPATCH_LEVEL)
    [[nodiscard]] NTSTATUS ReserveOperation(ULONG mutation, OperationResult& result) const noexcept;
    _IRQL_requires_max_(DISPATCH_LEVEL)
    [[nodiscard]] NTSTATUS
    QueryOperation(ULONGLONG instance, ULONGLONG operation, OperationResult& result) const noexcept;
    _IRQL_requires_max_(DISPATCH_LEVEL)
    void SnapshotState(WISPDISK_STATE_RESPONSE& response) const noexcept;

    // Requests have been validated by the protocol layer. A successful queue
    // transfers SRB ownership to the worker; Complete means a response is ready.
    _IRQL_requires_max_(DISPATCH_LEVEL)
    [[nodiscard]] WispDiskIoControlDisposition SubmitCreate(
        const ControlRequest& control,
        const WISPDISK_CREATE_REQUEST& request) const noexcept;
    _IRQL_requires_max_(DISPATCH_LEVEL)
    [[nodiscard]] WispDiskIoControlDisposition SubmitCreate(
        const ControlRequest& control,
        const WISPDISK_OPERATION_REQUEST& identity,
        const WISPDISK_CREATE_REQUEST& request) const noexcept;
    _IRQL_requires_max_(DISPATCH_LEVEL)
    [[nodiscard]] WispDiskIoControlDisposition SubmitDelete(
        const ControlRequest& control,
        const WISPDISK_DELETE_REQUEST& request) const noexcept;
    _IRQL_requires_max_(DISPATCH_LEVEL)
    [[nodiscard]] WispDiskIoControlDisposition SubmitDelete(
        const ControlRequest& control,
        const WISPDISK_OPERATION_REQUEST& identity,
        const WISPDISK_DELETE_REQUEST& request) const noexcept;

    // Entry point for StorPort's PASSIVE_LEVEL worker callback.
    _IRQL_requires_(PASSIVE_LEVEL)
    void ProcessWorkItem(PSCSI_REQUEST_BLOCK srb) const noexcept;

private:
    struct MutationCommand {
        // The submission overloads select one of the four mutation operations.
        WISPDISK_CONTROL_CODE operation;
        WISPDISK_OPERATION_REQUEST identity;
        WISPDISK_CREATE_REQUEST create;
        WISPDISK_DELETE_REQUEST remove;

        [[nodiscard]] bool IsReserved() const noexcept;
        [[nodiscard]] ULONG Mutation() const noexcept;
        [[nodiscard]] ULONGLONG CorrelationId() const noexcept;
    };

    void CloseAdmission() const noexcept;
    _IRQL_requires_(PASSIVE_LEVEL)
    void WaitForIdle() const noexcept;
    _IRQL_requires_(PASSIVE_LEVEL)
    void ReleaseWorker() const noexcept;
    [[nodiscard]] WispDiskIoControlDisposition
    Submit(const ControlRequest& control, const MutationCommand& command) const noexcept;
    [[nodiscard]] bool
    PrepareSubmission(const ControlRequest& control, const MutationCommand& command) const noexcept;
    // Every Locked helper requires a live guard for this adapter's LunLock.
    [[nodiscard]] NTSTATUS
    BeginManagementLocked(const support::ScopedSpinLock& lock) const noexcept;
    void EndManagementLocked(const support::ScopedSpinLock& lock) const noexcept;
    [[nodiscard]] WISPDISK_OPERATION_RECORD*
    FindOperationLocked(ULONGLONG id, const support::ScopedSpinLock& lock) const noexcept;
    [[nodiscard]] OperationResult CopyResultLocked(
        const WISPDISK_OPERATION_RECORD& record,
        const support::ScopedSpinLock& lock) const noexcept;
    [[nodiscard]] NTSTATUS BindOperationLocked(
        const MutationCommand& command,
        bool& accepted,
        OperationResult& result,
        const support::ScopedSpinLock& lock) const noexcept;
    void FailOperationLocked(
        ULONGLONG id,
        NTSTATUS status,
        const ControlRequest& control,
        ULONGLONG correlation,
        const support::ScopedSpinLock& lock) const noexcept;
    void
    MarkCreatesDeletedLocked(ULONG deviceId, const support::ScopedSpinLock& lock) const noexcept;
    void FinalizeDiskRemoval(WISPDISK_LUN& lun, ULONG deviceId) const noexcept;
    [[nodiscard]] bool SnapshotPendingOperation(
        const ControlRequest& control,
        const WISPDISK_OPERATION_REQUEST& identity,
        WISPDISK_OPERATION_RECORD& snapshot) const noexcept;
    void PublishOperationResult(
        const ControlRequest& control,
        const WISPDISK_OPERATION_RECORD& snapshot,
        ULONGLONG correlation) const noexcept;
    _IRQL_requires_(PASSIVE_LEVEL)
    void ProcessReservedRequest(const ControlRequest& control) const noexcept;
    _IRQL_requires_(PASSIVE_LEVEL)
    void ProcessLegacyRequest(const ControlRequest& control) const noexcept;
    _IRQL_requires_(PASSIVE_LEVEL)
    [[nodiscard]] NTSTATUS DeleteDisk(ULONG deviceId) const noexcept;
    _IRQL_requires_(PASSIVE_LEVEL)
    void DrainAndReleaseDisks() const noexcept;

    WISPDISK_ADAPTER_EXTENSION& extension_;
};

void WriteOperationResponse(
    const ControlRequest& control,
    const OperationResult& result,
    ULONGLONG correlation) noexcept;

} // namespace wispdisk::management
