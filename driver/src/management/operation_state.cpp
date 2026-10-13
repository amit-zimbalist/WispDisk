#include "operation_state.h"
#include "disk_operations.h"
#include "support/scoped_spin_lock.h"

namespace wispdisk::management {

static WISPDISK_OPERATION_RECORD* FindOperation(PWISPDISK_ADAPTER_EXTENSION adapter, ULONGLONG id) noexcept {
    for (auto& record : adapter->Operations) {
        if (record.Id == id && id != 0) return &record;
    }
    return nullptr;
}

static void WriteOperation(PWISPDISK_ADAPTER_EXTENSION adapter, const ControlRequest& control,
                           const WISPDISK_OPERATION_RECORD& record, ULONGLONG correlation) noexcept {
    const WISPDISK_OPERATION_RESPONSE response{
        .Header = MakeResponseHeader(sizeof(WISPDISK_OPERATION_RESPONSE),
            static_cast<WISPDISK_CONTROL_CODE>(control.control->ControlCode), STATUS_SUCCESS, correlation),
        .AdapterInstance = adapter->AdapterInstance,
        .OperationId = record.Id,
        .Mutation = record.Mutation,
        .State = static_cast<ULONG>(record.State),
        .ResultStatus = record.Result,
        .DeviceId = record.DeviceId,
        .Lun = record.Lun,
    };
    RtlCopyMemory(control.payload, &response, sizeof(response));
    control.control->ReturnCode = STATUS_SUCCESS;
    control.control->Length = sizeof(response);
    control.srb->DataTransferLength = sizeof(SRB_IO_CONTROL) + sizeof(response);
    control.srb->ScsiStatus = SCSISTAT_GOOD;
    control.srb->SrbStatus = SRB_STATUS_SUCCESS;
}

static NTSTATUS ReadHeader(const ControlRequest& control, ULONG size, WISPDISK_REQUEST_HEADER* header) noexcept {
    const auto operation = static_cast<WISPDISK_CONTROL_CODE>(control.control->ControlCode);
    NTSTATUS status = ReadAndValidateRequestHeader(control.payload, control.control->Length, size, operation, header);
    if (NT_SUCCESS(status) && control.control->Length < sizeof(WISPDISK_OPERATION_RESPONSE)) status = STATUS_BUFFER_TOO_SMALL;
    if (!NT_SUCCESS(status)) WriteSimpleErrorResponse(control, operation, status, header->CorrelationId);
    return status;
}

void HandleReserveOperation(PWISPDISK_ADAPTER_EXTENSION adapter, const ControlRequest& control) noexcept {
    WISPDISK_REQUEST_HEADER header{};
    if (!NT_SUCCESS(ReadHeader(control, sizeof(WISPDISK_RESERVE_REQUEST), &header))) return;
    WISPDISK_RESERVE_REQUEST request{};
    RtlCopyMemory(&request, control.payload, sizeof(request));
    support::ScopedSpinLock lock{&adapter->LunLock};
    NTSTATUS status = STATUS_SUCCESS;
    WISPDISK_OPERATION_RECORD* selected = nullptr;
    if (request.Reserved != 0 || (request.Mutation != WispDiskControlCreateDisk && request.Mutation != WispDiskControlDeleteDisk)) {
        status = STATUS_INVALID_PARAMETER;
    } else if (adapter->AdapterStopping != 0) {
        status = STATUS_DEVICE_NOT_READY;
    } else {
        for (auto& record : adapter->Operations) {
            if (record.Id == 0) { selected = &record; break; }
            // Never evict accepted work or the identity of a live allocation.
            if (record.State == WispDiskOperationPending ||
                (record.State == WispDiskOperationSucceeded && record.Mutation == WispDiskControlCreateDisk)) continue;
            if (selected == nullptr || record.Id < selected->Id) selected = &record;
        }
        if (selected == nullptr || adapter->NextOperationId == 0) status = STATUS_INSUFFICIENT_RESOURCES;
    }
    if (!NT_SUCCESS(status)) {
        WriteSimpleErrorResponse(control, WispDiskControlReserveOperation, status, header.CorrelationId);
        return;
    }
    *selected = {};
    selected->Id = adapter->NextOperationId++;
    selected->Mutation = request.Mutation;
    selected->State = WispDiskOperationReserved;
    selected->Result = STATUS_PENDING;
    WriteOperation(adapter, control, *selected, header.CorrelationId);
}

void HandleQueryOperation(PWISPDISK_ADAPTER_EXTENSION adapter, const ControlRequest& control) noexcept {
    WISPDISK_REQUEST_HEADER header{};
    if (!NT_SUCCESS(ReadHeader(control, sizeof(WISPDISK_OPERATION_REQUEST), &header))) return;
    WISPDISK_OPERATION_REQUEST request{};
    RtlCopyMemory(&request, control.payload, sizeof(request));
    support::ScopedSpinLock lock{&adapter->LunLock};
    auto* record = FindOperation(adapter, request.OperationId);
    if (request.AdapterInstance != adapter->AdapterInstance || record == nullptr) {
        WriteSimpleErrorResponse(control, WispDiskControlQueryOperation, STATUS_NOT_FOUND, header.CorrelationId);
        return;
    }
    WriteOperation(adapter, control, *record, header.CorrelationId);
}

bool AcceptReservedOperation(PWISPDISK_ADAPTER_EXTENSION adapter, const ControlRequest& control) noexcept {
    const bool create = control.control->ControlCode == WispDiskControlCreateReserved;
    WISPDISK_REQUEST_HEADER header{};
    if (!NT_SUCCESS(ReadHeader(control, create ? sizeof(WISPDISK_RESERVED_CREATE_REQUEST) : sizeof(WISPDISK_RESERVED_DELETE_REQUEST), &header))) return false;
    WISPDISK_OPERATION_REQUEST identity{};
    RtlCopyMemory(&identity, control.payload, sizeof(identity));
    auto* record = FindOperation(adapter, identity.OperationId);
    NTSTATUS status = STATUS_SUCCESS;
    WISPDISK_CREATE_REQUEST createRequest{};
    WISPDISK_DELETE_REQUEST deleteRequest{};
    if (identity.AdapterInstance != adapter->AdapterInstance || record == nullptr) {
        status = STATUS_NOT_FOUND;
    } else if (record->Mutation != static_cast<ULONG>(create ? WispDiskControlCreateDisk : WispDiskControlDeleteDisk)) {
        status = STATUS_INVALID_PARAMETER;
    } else if (create) {
        WISPDISK_RESERVED_CREATE_REQUEST request{};
        RtlCopyMemory(&request, control.payload, sizeof(request));
        createRequest = {
            .Header = request.Operation.Header,
            .SizeBytes = request.SizeBytes, .LogicalSectorSize = request.LogicalSectorSize,
            .MediaKind = request.MediaKind, .RequestedDriveLetter = request.RequestedDriveLetter,
            .Reserved16 = request.Reserved16, .Reserved32 = request.Reserved32,
        };
        createRequest.Header.StructSize = sizeof(createRequest);
        createRequest.Header.Operation = WispDiskControlCreateDisk;
        SRB_IO_CONTROL scratch{};
        scratch.Length = sizeof(createRequest);
        const ControlRequest validation{control.srb, &scratch, reinterpret_cast<PUCHAR>(&createRequest)};
        WISPDISK_CREATE_REQUEST validated{};
        status = ValidateCreateRequest(validation, &validated);
        if (NT_SUCCESS(status) && record->State != WispDiskOperationReserved &&
            (record->Create.SizeBytes != createRequest.SizeBytes || record->Create.MediaKind != createRequest.MediaKind ||
             record->Create.RequestedDriveLetter != createRequest.RequestedDriveLetter ||
             record->Create.LogicalSectorSize != createRequest.LogicalSectorSize)) status = STATUS_INVALID_PARAMETER;
    } else {
        WISPDISK_RESERVED_DELETE_REQUEST request{};
        RtlCopyMemory(&request, control.payload, sizeof(request));
        deleteRequest = {.Header = request.Operation.Header, .DeviceId = request.DeviceId, .Reserved = request.Reserved};
        deleteRequest.Header.StructSize = sizeof(deleteRequest);
        deleteRequest.Header.Operation = WispDiskControlDeleteDisk;
        SRB_IO_CONTROL scratch{};
        scratch.Length = sizeof(deleteRequest);
        const ControlRequest validation{control.srb, &scratch, reinterpret_cast<PUCHAR>(&deleteRequest)};
        WISPDISK_DELETE_REQUEST validated{};
        status = ValidateDeleteRequest(validation, &validated);
        if (NT_SUCCESS(status) && record->State != WispDiskOperationReserved && record->Delete.DeviceId != deleteRequest.DeviceId) status = STATUS_INVALID_PARAMETER;
    }
    if (!NT_SUCCESS(status)) {
        WriteSimpleErrorResponse(control, static_cast<WISPDISK_CONTROL_CODE>(control.control->ControlCode), status, header.CorrelationId);
        return false;
    }
    if (record->State != WispDiskOperationReserved) {
        WriteOperation(adapter, control, *record, header.CorrelationId);
        return false;
    }
    record->Create = createRequest;
    record->Delete = deleteRequest;
    record->State = WispDiskOperationPending;
    return true;
}

void FailReservedOperation(PWISPDISK_ADAPTER_EXTENSION adapter, const ControlRequest& control, NTSTATUS status) noexcept {
    WISPDISK_OPERATION_REQUEST request{};
    RtlCopyMemory(&request, control.payload, sizeof(request));
    auto* record = FindOperation(adapter, request.OperationId);
    if (record != nullptr) {
        record->State = WispDiskOperationFailed;
        record->Result = status;
        WriteOperation(adapter, control, *record, request.Header.CorrelationId);
    }
}

void ProcessReservedOperation(PWISPDISK_ADAPTER_EXTENSION adapter, const ControlRequest& control) noexcept {
    WISPDISK_OPERATION_REQUEST identity{};
    RtlCopyMemory(&identity, control.payload, sizeof(identity));
    WISPDISK_OPERATION_RECORD snapshot{};
    {
        support::ScopedSpinLock lock{&adapter->LunLock};
        auto* record = FindOperation(adapter, identity.OperationId);
        if (record == nullptr) { WriteSimpleErrorResponse(control, static_cast<WISPDISK_CONTROL_CODE>(control.control->ControlCode), STATUS_NOT_FOUND, identity.Header.CorrelationId); return; }
        snapshot = *record;
    }
    // Execute the validated legacy mutation using an owned scratch buffer. The
    // real SRB is completed only once by the outer management worker.
    alignas(8) UCHAR buffer[sizeof(SRB_IO_CONTROL) + sizeof(WISPDISK_CREATE_REQUEST)]{};
    auto* localControl = reinterpret_cast<PSRB_IO_CONTROL>(buffer);
    localControl->Length = sizeof(WISPDISK_CREATE_REQUEST);
    SCSI_REQUEST_BLOCK localSrb{};
    const ControlRequest local{&localSrb, localControl, buffer + sizeof(SRB_IO_CONTROL)};
    if (snapshot.Mutation == WispDiskControlCreateDisk) {
        RtlCopyMemory(local.payload, &snapshot.Create, sizeof(snapshot.Create));
        ProcessCreateRequest(adapter, local);
        WISPDISK_CREATE_RESPONSE response{};
        RtlCopyMemory(&response, local.payload, sizeof(response));
        snapshot.DeviceId = response.DeviceId;
        snapshot.Lun = response.Lun;
    } else {
        RtlCopyMemory(local.payload, &snapshot.Delete, sizeof(snapshot.Delete));
        ProcessDeleteRequest(adapter, local);
        snapshot.DeviceId = snapshot.Delete.DeviceId;
    }
    snapshot.Result = static_cast<NTSTATUS>(localControl->ReturnCode);
    snapshot.State = NT_SUCCESS(snapshot.Result) ? WispDiskOperationSucceeded : WispDiskOperationFailed;
    {
        support::ScopedSpinLock lock{&adapter->LunLock};
        *FindOperation(adapter, identity.OperationId) = snapshot;
        if (NT_SUCCESS(snapshot.Result)) {
            if (snapshot.Mutation == WispDiskControlCreateDisk) {
                adapter->Luns[snapshot.Lun].CreateOperationId = snapshot.Id;
            } else {
                for (auto& record : adapter->Operations) {
                    if (record.Mutation == WispDiskControlCreateDisk && record.DeviceId == snapshot.DeviceId &&
                        record.State == WispDiskOperationSucceeded) record.State = WispDiskOperationDeleted;
                }
            }
        }
        WriteOperation(adapter, control, snapshot, identity.Header.CorrelationId);
    }
}

void HandleListState(PWISPDISK_ADAPTER_EXTENSION adapter, const ControlRequest& control) noexcept {
    WISPDISK_REQUEST_HEADER header{};
    NTSTATUS status = ReadAndValidateRequestHeader(control.payload, control.control->Length, sizeof(header), WispDiskControlListState, &header);
    if (NT_SUCCESS(status) && control.control->Length < sizeof(WISPDISK_STATE_RESPONSE)) status = STATUS_BUFFER_TOO_SMALL;
    if (!NT_SUCCESS(status)) { WriteSimpleErrorResponse(control, WispDiskControlListState, status, header.CorrelationId); return; }
    WISPDISK_STATE_RESPONSE response{.Header = MakeResponseHeader(sizeof(WISPDISK_STATE_RESPONSE), WispDiskControlListState, STATUS_SUCCESS, header.CorrelationId)};
    {
        support::ScopedSpinLock lock{&adapter->LunLock};
        response.AdapterInstance = adapter->AdapterInstance;
        response.AllocatedBytes = adapter->TotalAllocatedBytes;
        for (const auto& record : adapter->Operations) if (record.State == WispDiskOperationPending) ++response.PendingOperations;
        for (const auto& lun : adapter->Luns) {
            if (lun.State == WispDiskLunState::Empty) continue;
            response.Disks[response.DiskCount++] = {
                .Disk = {.DeviceId = lun.DeviceId, .Lun = lun.Lun, .SizeBytes = lun.SizeBytes,
                    .LogicalSectorSize = kLogicalSectorSize, .MediaKind = lun.MediaKind, .RequestedDriveLetter = lun.RequestedDriveLetter},
                .CreateOperationId = lun.CreateOperationId,
                .State = static_cast<ULONG>(lun.State),
            };
        }
    }
    RtlCopyMemory(control.payload, &response, sizeof(response));
    control.control->ReturnCode = STATUS_SUCCESS;
    control.control->Length = sizeof(response);
    control.srb->DataTransferLength = sizeof(SRB_IO_CONTROL) + sizeof(response);
    control.srb->ScsiStatus = SCSISTAT_GOOD;
    control.srb->SrbStatus = SRB_STATUS_SUCCESS;
}
}
