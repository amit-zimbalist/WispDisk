#include "management_controller.h"

#include "diagnostics.h"
#include "logging.h"
#include "storage/lun_store.h"

namespace wispdisk::management {

using diagnostics::TracePhase;

bool ManagementController::MutationCommand::IsReserved() const noexcept {
    return operation == WispDiskControlCreateReserved || operation == WispDiskControlDeleteReserved;
}

ULONG ManagementController::MutationCommand::Mutation() const noexcept {
    switch (operation) {
        case WispDiskControlCreateDisk:
        case WispDiskControlCreateReserved:
            return WispDiskControlCreateDisk;
        case WispDiskControlDeleteDisk:
        case WispDiskControlDeleteReserved:
            return WispDiskControlDeleteDisk;
        default:
            NT_ASSERT(false);
            return 0;
    }
}

ULONGLONG ManagementController::MutationCommand::CorrelationId() const noexcept {
    return Mutation() == WispDiskControlCreateDisk ? create.Header.CorrelationId
                                                   : remove.Header.CorrelationId;
}

ManagementController::ManagementController(WISPDISK_ADAPTER_EXTENSION& extension) noexcept
    : extension_{extension} {}

_Use_decl_annotations_
bool ManagementController::Initialize() const noexcept {
    KeInitializeEvent(&extension_.ManagementIdleEvent, NotificationEvent, TRUE);
    extension_.ManagementBusy = 0;
    extension_.NextOperationId = 1;
    return StorPortInitializeWorker(&extension_, &extension_.ManagementWorker) ==
           STOR_STATUS_SUCCESS;
}

void ManagementController::CloseAdmission() const noexcept {
    support::ScopedSpinLock lock{&extension_.LunLock};
    InterlockedExchange(&extension_.AdapterStopping, 1);
}

_Use_decl_annotations_
void ManagementController::Shutdown() const noexcept {
    CloseAdmission();
    DEBUG("ManagementIdleWaitStarted", .AdapterInstance = extension_.AdapterInstance);
    WaitForIdle();
    DEBUG("ManagementIdleWaitCompleted", .AdapterInstance = extension_.AdapterInstance);
    DEBUG("ShutdownDiskDrainStarted", .AdapterInstance = extension_.AdapterInstance);
    DrainAndReleaseDisks();
    DEBUG("ShutdownDiskDrainCompleted", .AdapterInstance = extension_.AdapterInstance);
    ReleaseWorker();
}

OperationResult ManagementController::CopyResultLocked(
    const WISPDISK_OPERATION_RECORD& record,
    const support::ScopedSpinLock& lock) const noexcept {
    lock.AssertProtects(&extension_.LunLock);
    return {
        .adapterInstance = extension_.AdapterInstance,
        .operationId = record.Id,
        .mutation = record.Mutation,
        .state = record.State,
        .status = record.Result,
        .deviceId = record.DeviceId,
        .lun = record.Lun,
    };
}

WISPDISK_OPERATION_RECORD* ManagementController::FindOperationLocked(
    ULONGLONG id,
    const support::ScopedSpinLock& lock) const noexcept {
    lock.AssertProtects(&extension_.LunLock);
    if (id == 0) {
        return nullptr;
    }
    for (auto& record : extension_.Operations) {
        if (record.Id == id) {
            return &record;
        }
    }
    return nullptr;
}

_Use_decl_annotations_
NTSTATUS
ManagementController::ReserveOperation(ULONG mutation, OperationResult& result) const noexcept {
    result = {};
    support::ScopedSpinLock lock{&extension_.LunLock};
    if (mutation != WispDiskControlCreateDisk && mutation != WispDiskControlDeleteDisk) {
        WARN(
            "OperationMutationRejected",
            .AdapterInstance = extension_.AdapterInstance,
            .Status = STATUS_INVALID_PARAMETER);
        return STATUS_INVALID_PARAMETER;
    }
    if (extension_.AdapterStopping != 0) {
        DEBUG(
            "OperationReservationStopping",
            .AdapterInstance = extension_.AdapterInstance,
            .Status = STATUS_DEVICE_NOT_READY);
        return STATUS_DEVICE_NOT_READY;
    }
    WISPDISK_OPERATION_RECORD* selected = nullptr;
    for (auto& candidate : extension_.Operations) {
        if (candidate.Id == 0) {
            selected = &candidate;
            break;
        }
        // Pending work and live create identities are never evicted.
        if (candidate.State == WispDiskOperationPending ||
            (candidate.State == WispDiskOperationSucceeded &&
             candidate.Mutation == WispDiskControlCreateDisk)) {
            continue;
        }
        if (selected == nullptr || candidate.Id < selected->Id) {
            selected = &candidate;
        }
    }
    if (selected == nullptr || extension_.NextOperationId == 0) {
        WARN(
            "OperationReservationExhausted",
            .AdapterInstance = extension_.AdapterInstance,
            .Status = STATUS_INSUFFICIENT_RESOURCES);
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    *selected = {
        .Id = extension_.NextOperationId++,
        .Mutation = mutation,
        .State = WispDiskOperationReserved,
        .Result = STATUS_PENDING,
    };
    result = CopyResultLocked(*selected, lock);
    DEBUG(
        "OperationReserved",
        .AdapterInstance = extension_.AdapterInstance,
        .OperationId = result.operationId);
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
NTSTATUS ManagementController::QueryOperation(
    ULONGLONG instance,
    ULONGLONG operation,
    OperationResult& result) const noexcept {
    result = {};
    support::ScopedSpinLock lock{&extension_.LunLock};
    const auto* record = FindOperationLocked(operation, lock);
    if (instance != extension_.AdapterInstance || record == nullptr) {
        return STATUS_NOT_FOUND;
    }
    result = CopyResultLocked(*record, lock);
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
void ManagementController::SnapshotState(WISPDISK_STATE_RESPONSE& response) const noexcept {
    support::ScopedSpinLock lock{&extension_.LunLock};
    response.AdapterInstance = extension_.AdapterInstance;
    response.PendingOperations = 0;
    for (const auto& record : extension_.Operations) {
        if (record.State == WispDiskOperationPending) {
            ++response.PendingOperations;
        }
    }
    storage::LunStore{extension_}.SnapshotStateLocked(response, lock);
}

NTSTATUS
ManagementController::BeginManagementLocked(const support::ScopedSpinLock& lock) const noexcept {
    lock.AssertProtects(&extension_.LunLock);
    if (extension_.AdapterStopping != 0) {
        return STATUS_DEVICE_NOT_READY;
    }
    if (InterlockedCompareExchange(&extension_.ManagementBusy, 1, 0) != 0) {
        return STATUS_DEVICE_BUSY;
    }
    KeClearEvent(&extension_.ManagementIdleEvent);
    return STATUS_SUCCESS;
}

void ManagementController::EndManagementLocked(const support::ScopedSpinLock& lock) const noexcept {
    lock.AssertProtects(&extension_.LunLock);
    InterlockedExchange(&extension_.ManagementBusy, 0);
    KeSetEvent(&extension_.ManagementIdleEvent, IO_NO_INCREMENT, FALSE);
}

_Use_decl_annotations_
void ManagementController::WaitForIdle() const noexcept {
    KeWaitForSingleObject(&extension_.ManagementIdleEvent, Executive, KernelMode, FALSE, nullptr);
    // An idle worker may still be releasing the lock after signaling the event.
    support::ScopedSpinLock lock{&extension_.LunLock};
}

NTSTATUS ManagementController::BindOperationLocked(
    const MutationCommand& command,
    bool& accepted,
    OperationResult& result,
    const support::ScopedSpinLock& lock) const noexcept {
    lock.AssertProtects(&extension_.LunLock);
    accepted = false;
    auto* record = FindOperationLocked(command.identity.OperationId, lock);
    if (command.identity.AdapterInstance != extension_.AdapterInstance || record == nullptr) {
        return STATUS_NOT_FOUND;
    }
    if (record->Mutation != command.Mutation()) {
        return STATUS_INVALID_PARAMETER;
    }
    if (record->State != WispDiskOperationReserved) {
        if (command.Mutation() == WispDiskControlCreateDisk) {
            if (record->Create.SizeBytes != command.create.SizeBytes ||
                record->Create.MediaKind != command.create.MediaKind ||
                record->Create.RequestedDriveLetter != command.create.RequestedDriveLetter ||
                record->Create.LogicalSectorSize != command.create.LogicalSectorSize) {
                return STATUS_INVALID_PARAMETER;
            }
        } else if (record->Delete.DeviceId != command.remove.DeviceId) {
            return STATUS_INVALID_PARAMETER;
        }
        result = CopyResultLocked(*record, lock);
        return STATUS_SUCCESS;
    }
    record->Create = command.create;
    record->Delete = command.remove;
    if (command.Mutation() == WispDiskControlDeleteDisk) {
        record->DeviceId = command.remove.DeviceId;
    }
    record->State = WispDiskOperationPending;
    accepted = true;
    return STATUS_SUCCESS;
}

void WriteOperationResponse(
    const ControlRequest& control,
    const OperationResult& result,
    ULONGLONG correlation) noexcept {
    const WISPDISK_OPERATION_RESPONSE response{
        .Header = MakeResponseHeader(
            sizeof(WISPDISK_OPERATION_RESPONSE),
            static_cast<WISPDISK_CONTROL_CODE>(control.ioControl->ControlCode),
            STATUS_SUCCESS,
            correlation),
        .AdapterInstance = result.adapterInstance,
        .OperationId = result.operationId,
        .Mutation = result.mutation,
        .State = static_cast<ULONG>(result.state),
        .ResultStatus = result.status,
        .DeviceId = result.deviceId,
        .Lun = result.lun,
    };
    control.WriteResponse(response, STATUS_SUCCESS);
}

void ManagementController::FailOperationLocked(
    ULONGLONG id,
    NTSTATUS status,
    const ControlRequest& control,
    ULONGLONG correlation,
    const support::ScopedSpinLock& lock) const noexcept {
    lock.AssertProtects(&extension_.LunLock);
    auto* record = FindOperationLocked(id, lock);
    if (record == nullptr) {
        control.WriteError(
            static_cast<WISPDISK_CONTROL_CODE>(control.ioControl->ControlCode),
            STATUS_NOT_FOUND,
            correlation);
        return;
    }
    record->State = WispDiskOperationFailed;
    record->Result = status;
    diagnostics::TraceLocked(
        extension_,
        lock,
        record->Id,
        record->DeviceId,
        TracePhase::Rejected,
        status);
    WriteOperationResponse(control, CopyResultLocked(*record, lock), correlation);
}

_Use_decl_annotations_
WispDiskIoControlDisposition ManagementController::SubmitCreate(
    const ControlRequest& control,
    const WISPDISK_CREATE_REQUEST& request) const noexcept {
    return Submit(control, {.operation = WispDiskControlCreateDisk, .create = request});
}

_Use_decl_annotations_
WispDiskIoControlDisposition ManagementController::SubmitCreate(
    const ControlRequest& control,
    const WISPDISK_OPERATION_REQUEST& identity,
    const WISPDISK_CREATE_REQUEST& request) const noexcept {
    return Submit(
        control,
        {.operation = WispDiskControlCreateReserved, .identity = identity, .create = request});
}

_Use_decl_annotations_
WispDiskIoControlDisposition ManagementController::SubmitDelete(
    const ControlRequest& control,
    const WISPDISK_DELETE_REQUEST& request) const noexcept {
    return Submit(control, {.operation = WispDiskControlDeleteDisk, .remove = request});
}

_Use_decl_annotations_
WispDiskIoControlDisposition ManagementController::SubmitDelete(
    const ControlRequest& control,
    const WISPDISK_OPERATION_REQUEST& identity,
    const WISPDISK_DELETE_REQUEST& request) const noexcept {
    return Submit(
        control,
        {.operation = WispDiskControlDeleteReserved, .identity = identity, .remove = request});
}

bool ManagementController::PrepareSubmission(
    const ControlRequest& control,
    const MutationCommand& command) const noexcept {
    const ULONGLONG correlation = command.CorrelationId();
    const ULONGLONG operationId = command.IsReserved() ? command.identity.OperationId : correlation;
    support::ScopedSpinLock lock{&extension_.LunLock};
    NTSTATUS status = BeginManagementLocked(lock);
    if (!NT_SUCCESS(status)) {
        DEBUG(
            "ManagementAdmissionRejected",
            .AdapterInstance = extension_.AdapterInstance,
            .OperationId = operationId,
            .Status = status);
        control.WriteError(command.operation, status, correlation);
        return false;
    }
    if (command.IsReserved()) {
        bool accepted = false;
        OperationResult replay{};
        status = BindOperationLocked(command, accepted, replay, lock);
        if (!NT_SUCCESS(status) || !accepted) {
            if (!NT_SUCCESS(status)) {
                WARN(
                    status == STATUS_NOT_FOUND ? "OperationIdentityRejected"
                                               : "OperationParametersRejected",
                    .AdapterInstance = extension_.AdapterInstance,
                    .OperationId = operationId,
                    .Status = status);
                control.WriteError(command.operation, status, correlation);
            } else {
                DEBUG(
                    "OperationReplayed",
                    .AdapterInstance = extension_.AdapterInstance,
                    .OperationId = replay.operationId,
                    .DeviceId = replay.deviceId,
                    .Status = replay.status);
                WriteOperationResponse(control, replay, correlation);
            }
            EndManagementLocked(lock);
            return false;
        }
    }
    // The worker can run before queueing returns. Publish pending first;
    // after successful queueing only the worker may touch this SRB.
    control.ioControl->ReturnCode = static_cast<ULONG>(STATUS_PENDING);
    control.srb->SrbStatus = SRB_STATUS_PENDING;
    diagnostics::TraceLocked(
        extension_,
        lock,
        operationId,
        0,
        TracePhase::Accepted,
        STATUS_PENDING,
        SRB_STATUS_PENDING);
    return true;
}

WispDiskIoControlDisposition ManagementController::Submit(
    const ControlRequest& control,
    const MutationCommand& command) const noexcept {
    NT_ASSERT(control.ioControl->ControlCode == static_cast<ULONG>(command.operation));
    if (!PrepareSubmission(control, command)) {
        return WispDiskIoControlDisposition::Complete;
    }
    const ULONG storStatus = extension_.ManagementWorker == nullptr
                                 ? STOR_STATUS_INVALID_DEVICE_STATE
                                 : StorPortQueueWorkItem(
                                       &extension_,
                                       WispDiskManagementWorker,
                                       extension_.ManagementWorker,
                                       control.srb);
    if (storStatus != STOR_STATUS_SUCCESS) {
        support::ScopedSpinLock lock{&extension_.LunLock};
        ERROR(
            "ManagementQueueFailed",
            .AdapterInstance = extension_.AdapterInstance,
            .OperationId =
                command.IsReserved() ? command.identity.OperationId : command.CorrelationId(),
            .Status = STATUS_INSUFFICIENT_RESOURCES,
            .StorPortStatus = storStatus);
        if (command.IsReserved()) {
            FailOperationLocked(
                command.identity.OperationId,
                STATUS_INSUFFICIENT_RESOURCES,
                control,
                command.CorrelationId(),
                lock);
        } else {
            control.WriteError(
                command.operation,
                STATUS_INSUFFICIENT_RESOURCES,
                command.CorrelationId());
        }
        EndManagementLocked(lock);
        return WispDiskIoControlDisposition::Complete;
    }
    return WispDiskIoControlDisposition::Pending;
}

void ManagementController::MarkCreatesDeletedLocked(
    ULONG deviceId,
    const support::ScopedSpinLock& lock) const noexcept {
    lock.AssertProtects(&extension_.LunLock);
    for (auto& record : extension_.Operations) {
        if (record.Mutation == WispDiskControlCreateDisk && record.DeviceId == deviceId &&
            record.State == WispDiskOperationSucceeded) {
            record.State = WispDiskOperationDeleted;
        }
    }
}

_Use_decl_annotations_
NTSTATUS ManagementController::DeleteDisk(ULONG deviceId) const noexcept {
    const storage::LunStore disks{extension_};
    auto* lun = disks.MarkLunStopping(deviceId);
    if (lun == nullptr) {
        WARN(
            "DeleteUnknownDisk",
            .AdapterInstance = extension_.AdapterInstance,
            .DeviceId = deviceId,
            .Status = STATUS_NO_SUCH_DEVICE);
        return STATUS_NO_SUCH_DEVICE;
    }
    disks.DrainAndFreeBacking(*lun, deviceId);
    FinalizeDiskRemoval(*lun, deviceId);
    INFO("DiskDeleted", .AdapterInstance = extension_.AdapterInstance, .DeviceId = deviceId);
    return STATUS_SUCCESS;
}

void ManagementController::FinalizeDiskRemoval(WISPDISK_LUN& lun, ULONG deviceId) const noexcept {
    support::ScopedSpinLock lock{&extension_.LunLock};
    // Removal becomes visible with the corresponding create record marked
    // Deleted, only after the backing memory has actually been freed.
    MarkCreatesDeletedLocked(deviceId, lock);
    storage::LunStore{extension_}.FinalizeRemovalLocked(lun, lock);
}

_Use_decl_annotations_
void ManagementController::ProcessLegacyRequest(const ControlRequest& control) const noexcept {
    if (control.ioControl->ControlCode == WispDiskControlCreateDisk) {
        WISPDISK_CREATE_REQUEST request{};
        NTSTATUS status = ValidateCreateRequest(control, request);
        ULONG deviceId = 0;
        UCHAR lun = 0;
        if (NT_SUCCESS(status)) {
            status = storage::LunStore{extension_}.CreateDisk(request, deviceId, lun);
        }
        WriteCreateResponse(control, request, status, deviceId, lun);
    } else {
        WISPDISK_DELETE_REQUEST request{};
        NTSTATUS status = ValidateDeleteRequest(control, request);
        if (NT_SUCCESS(status)) {
            status = DeleteDisk(request.DeviceId);
        }
        WriteDeleteResponse(control, request, status);
    }
}

bool ManagementController::SnapshotPendingOperation(
    const ControlRequest& control,
    const WISPDISK_OPERATION_REQUEST& identity,
    WISPDISK_OPERATION_RECORD& snapshot) const noexcept {
    support::ScopedSpinLock lock{&extension_.LunLock};
    const auto* record = FindOperationLocked(identity.OperationId, lock);
    if (identity.AdapterInstance != extension_.AdapterInstance || record == nullptr ||
        record->State != WispDiskOperationPending) {
        control.WriteError(
            static_cast<WISPDISK_CONTROL_CODE>(control.ioControl->ControlCode),
            STATUS_NOT_FOUND,
            identity.Header.CorrelationId);
        return false;
    }
    snapshot = *record;
    return true;
}

_Use_decl_annotations_
void ManagementController::ProcessReservedRequest(const ControlRequest& control) const noexcept {
    WISPDISK_OPERATION_REQUEST identity{};
    RtlCopyMemory(&identity, control.payload, sizeof(identity));
    WISPDISK_OPERATION_RECORD snapshot{};
    if (!SnapshotPendingOperation(control, identity, snapshot)) {
        return;
    }
    if (snapshot.Mutation == WispDiskControlCreateDisk) {
        snapshot.Result = storage::LunStore{extension_}.CreateDisk(
            snapshot.Create,
            snapshot.DeviceId,
            snapshot.Lun);
    } else {
        snapshot.Result = DeleteDisk(snapshot.Delete.DeviceId);
        snapshot.DeviceId = snapshot.Delete.DeviceId;
    }
    snapshot.State =
        NT_SUCCESS(snapshot.Result) ? WispDiskOperationSucceeded : WispDiskOperationFailed;
    PublishOperationResult(control, snapshot, identity.Header.CorrelationId);
}

void ManagementController::PublishOperationResult(
    const ControlRequest& control,
    const WISPDISK_OPERATION_RECORD& snapshot,
    ULONGLONG correlation) const noexcept {
    support::ScopedSpinLock lock{&extension_.LunLock};
    auto* record = FindOperationLocked(snapshot.Id, lock);
    if (record == nullptr) {
        control.WriteError(
            static_cast<WISPDISK_CONTROL_CODE>(control.ioControl->ControlCode),
            STATUS_NOT_FOUND,
            correlation);
        return;
    }
    if (NT_SUCCESS(snapshot.Result) && snapshot.Mutation == WispDiskControlCreateDisk) {
        storage::LunStore{extension_}.BindCreateOperationLocked(snapshot.Lun, snapshot.Id, lock);
    }
    *record = snapshot;
    diagnostics::TraceLocked(
        extension_,
        lock,
        snapshot.Id,
        snapshot.DeviceId,
        NT_SUCCESS(snapshot.Result) ? TracePhase::Committed : TracePhase::Rejected,
        snapshot.Result);
    WriteOperationResponse(control, CopyResultLocked(snapshot, lock), correlation);
}

_Use_decl_annotations_
void ManagementController::ProcessWorkItem(PSCSI_REQUEST_BLOCK srb) const noexcept {
    const auto control = ControlRequest::FromSrb(srb);
    if (control) {
        const bool reserved = control.ioControl->ControlCode == WispDiskControlCreateReserved ||
                              control.ioControl->ControlCode == WispDiskControlDeleteReserved;
        WISPDISK_OPERATION_REQUEST identity{};
        if (reserved) {
            RtlCopyMemory(&identity, control.payload, sizeof(identity));
        }
        diagnostics::Trace(extension_, identity.OperationId, 0, TracePhase::Worker, STATUS_PENDING);
        const auto fault =
            reserved ? diagnostics::TakeFault(extension_) : diagnostics::Mode::Disabled;
        if (fault == diagnostics::Mode::DelayWorker) {
            LARGE_INTEGER delay{.QuadPart = -2000000};
            KeDelayExecutionThread(KernelMode, FALSE, &delay);
        }
        if (fault == diagnostics::Mode::RejectOperation) {
            support::ScopedSpinLock lock{&extension_.LunLock};
            FailOperationLocked(
                identity.OperationId,
                STATUS_INSUFFICIENT_RESOURCES,
                control,
                identity.Header.CorrelationId,
                lock);
        } else if (reserved) {
            ProcessReservedRequest(control);
        } else if (
            control.ioControl->ControlCode == WispDiskControlCreateDisk ||
            control.ioControl->ControlCode == WispDiskControlDeleteDisk) {
            ProcessLegacyRequest(control);
        } else {
            control.WriteError(
                static_cast<WISPDISK_CONTROL_CODE>(control.ioControl->ControlCode),
                STATUS_INVALID_DEVICE_REQUEST,
                0);
        }
        if (fault == diagnostics::Mode::FailResponseTransport &&
            control.ioControl->ReturnCode == static_cast<ULONG>(STATUS_SUCCESS)) {
            srb->SrbStatus = SRB_STATUS_ERROR;
            srb->ScsiStatus = SCSISTAT_CHECK_CONDITION;
        }
    } else if (srb != nullptr) {
        srb->DataTransferLength = 0;
        srb->ScsiStatus = SCSISTAT_CHECK_CONDITION;
        srb->SrbStatus = SRB_STATUS_INVALID_REQUEST;
    }

    // Copy all metadata before completion transfers SRB ownership to StorPort.
    bool busChanged = control &&
                      control.ioControl->ReturnCode == static_cast<ULONG>(STATUS_SUCCESS) &&
                      (control.ioControl->ControlCode == WispDiskControlCreateDisk ||
                       control.ioControl->ControlCode == WispDiskControlDeleteDisk ||
                       control.ioControl->ControlCode == WispDiskControlCreateReserved ||
                       control.ioControl->ControlCode == WispDiskControlDeleteReserved);
    ULONGLONG operationId = 0;
    ULONG deviceId = 0;
    NTSTATUS resultStatus = STATUS_INVALID_DEVICE_REQUEST;
    if (control &&
        (control.ioControl->ControlCode == WispDiskControlCreateReserved ||
         control.ioControl->ControlCode == WispDiskControlDeleteReserved) &&
        control.ioControl->Length == sizeof(WISPDISK_OPERATION_RESPONSE)) {
        WISPDISK_OPERATION_RESPONSE response{};
        RtlCopyMemory(&response, control.payload, sizeof(response));
        operationId = response.OperationId;
        deviceId = response.DeviceId;
        resultStatus = response.ResultStatus;
        busChanged = response.State == WispDiskOperationSucceeded && NT_SUCCESS(resultStatus);
    }
    const ULONG srbStatus = srb == nullptr ? SRB_STATUS_INVALID_REQUEST : srb->SrbStatus;
    diagnostics::Trace(
        extension_,
        operationId,
        deviceId,
        TracePhase::Response,
        resultStatus,
        srbStatus);
    if (srb != nullptr) {
        StorPortNotification(RequestComplete, &extension_, srb);
    }
    diagnostics::Trace(
        extension_,
        operationId,
        deviceId,
        TracePhase::Complete,
        resultStatus,
        srbStatus);
    if (busChanged) {
        StorPortNotification(BusChangeDetected, &extension_, static_cast<ULONG>(0));
        diagnostics::Trace(extension_, operationId, deviceId, TracePhase::BusChange, resultStatus);
    }
    // This scope performs the worker's final access to adapter storage.
    support::ScopedSpinLock lock{&extension_.LunLock};
    EndManagementLocked(lock);
}

_Use_decl_annotations_
void ManagementController::DrainAndReleaseDisks() const noexcept {
    const storage::LunStore disks{extension_};
    disks.MarkAllLunsStopping();
    while (auto* lun = disks.FirstStoppingLun()) {
        disks.DrainAndFreeBacking(*lun, lun->DeviceId);
        FinalizeDiskRemoval(*lun, lun->DeviceId);
    }
}

_Use_decl_annotations_
void ManagementController::ReleaseWorker() const noexcept {
    if (extension_.ManagementWorker != nullptr) {
        DEBUG("ManagementWorkerReleaseStarted", .AdapterInstance = extension_.AdapterInstance);
        // The idle callback can still be unwinding inside StorPort.
        ULONG status;
        do {
            status = StorPortFreeWorker(&extension_, extension_.ManagementWorker);
            if (status == STOR_STATUS_BUSY) {
                LARGE_INTEGER delay{.QuadPart = -10000};
                KeDelayExecutionThread(KernelMode, FALSE, &delay);
            }
        } while (status == STOR_STATUS_BUSY);
        if (status != STOR_STATUS_SUCCESS) {
            ERROR(
                "ManagementWorkerReleaseFailed",
                .AdapterInstance = extension_.AdapterInstance,
                .StorPortStatus = status);
        } else {
            DEBUG(
                "ManagementWorkerReleaseCompleted",
                .AdapterInstance = extension_.AdapterInstance);
        }
        extension_.ManagementWorker = nullptr;
    }
}

} // namespace wispdisk::management
