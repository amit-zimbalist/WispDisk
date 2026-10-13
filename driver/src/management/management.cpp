#include "management.h"

#include "control_request.h"
#include "disk_operations.h"
#include "operation_state.h"
#include "diagnostics.h"
#include "support/scoped_spin_lock.h"

namespace wispdisk::management {

static WispDiskIoControlDisposition QueueManagementRequest(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _In_ const ControlRequest& controlRequest,
    _In_ WISPDISK_CONTROL_CODE operation,
    _In_ ULONGLONG correlationId
) noexcept {
    {
        support::ScopedSpinLock lock{&adapter->LunLock};
        if (adapter->AdapterStopping != 0) {
            WriteSimpleErrorResponse(
                controlRequest,
                operation,
                STATUS_DEVICE_NOT_READY,
                correlationId
            );
            return WispDiskIoControlDisposition::Complete;
        }

        if (InterlockedCompareExchange(&adapter->ManagementBusy, 1, 0) != 0) {
            WriteSimpleErrorResponse(
                controlRequest,
                operation,
                STATUS_DEVICE_BUSY,
                correlationId
            );
            return WispDiskIoControlDisposition::Complete;
        }
        KeClearEvent(&adapter->ManagementIdleEvent);

        if ((operation == WispDiskControlCreateReserved || operation == WispDiskControlDeleteReserved) &&
            !AcceptReservedOperation(adapter, controlRequest)) {
            InterlockedExchange(&adapter->ManagementBusy, 0);
            KeSetEvent(&adapter->ManagementIdleEvent, IO_NO_INCREMENT, FALSE);
            return WispDiskIoControlDisposition::Complete;
        }

        // The worker can run before StorPortQueueWorkItem returns. Publish pending
        // before handing it the SRB; on success only the worker may access it.
        controlRequest.control->ReturnCode = static_cast<ULONG>(STATUS_PENDING);
        controlRequest.srb->SrbStatus = SRB_STATUS_PENDING;
        ULONGLONG operationId = correlationId;
        if (operation == WispDiskControlCreateReserved || operation == WispDiskControlDeleteReserved) {
            WISPDISK_OPERATION_REQUEST identity{};
            RtlCopyMemory(&identity, controlRequest.payload, sizeof(identity));
            operationId = identity.OperationId;
        }
        TraceLocked(adapter, operationId, 0, TracePhase::Accepted, STATUS_PENDING, SRB_STATUS_PENDING);
    }

    const ULONG storStatus = adapter->ManagementWorker == nullptr
        ? STOR_STATUS_INVALID_DEVICE_STATE
        : StorPortQueueWorkItem(
              adapter,
              WispDiskManagementWorker,
              adapter->ManagementWorker,
              controlRequest.srb
          );

    if (storStatus != STOR_STATUS_SUCCESS) {
        support::ScopedSpinLock lock{&adapter->LunLock};
        InterlockedExchange(&adapter->ManagementBusy, 0);
        KeSetEvent(&adapter->ManagementIdleEvent, IO_NO_INCREMENT, FALSE);
        KdPrint((
            "WispDisk: failed to queue management operation=%lu, StorPort status=%lu\n",
            static_cast<ULONG>(operation),
            storStatus
        ));
        if (operation == WispDiskControlCreateReserved || operation == WispDiskControlDeleteReserved) {
            FailReservedOperation(adapter, controlRequest, STATUS_INSUFFICIENT_RESOURCES);
        } else {
            WriteSimpleErrorResponse(
                controlRequest,
                operation,
                STATUS_INSUFFICIENT_RESOURCES,
                correlationId
            );
        }
        return WispDiskIoControlDisposition::Complete;
    }

    return WispDiskIoControlDisposition::Pending;
}

static WispDiskIoControlDisposition HandleCreateControl(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _In_ const ControlRequest& controlRequest
) noexcept {
    WISPDISK_CREATE_REQUEST request{};
    const NTSTATUS status = ValidateCreateRequest(controlRequest, &request);
    if (!NT_SUCCESS(status)) {
        WriteCreateResponse(controlRequest, request, status);
        return WispDiskIoControlDisposition::Complete;
    }
    return QueueManagementRequest(
        adapter,
        controlRequest,
        WispDiskControlCreateDisk,
        request.Header.CorrelationId
    );
}

static WispDiskIoControlDisposition HandleDeleteControl(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _In_ const ControlRequest& controlRequest
) noexcept {
    WISPDISK_DELETE_REQUEST request{};
    const NTSTATUS status = ValidateDeleteRequest(controlRequest, &request);
    if (!NT_SUCCESS(status)) {
        WriteDeleteResponse(controlRequest, request, status);
        return WispDiskIoControlDisposition::Complete;
    }
    return QueueManagementRequest(
        adapter,
        controlRequest,
        WispDiskControlDeleteDisk,
        request.Header.CorrelationId
    );
}

static WispDiskIoControlDisposition DispatchControlRequest(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _In_ const ControlRequest& controlRequest
) noexcept {
    switch (controlRequest.control->ControlCode) {
        case 0x8F0:
        case 0x8F1:
            HandleDiagnostics(adapter, controlRequest);
            return WispDiskIoControlDisposition::Complete;
        case WispDiskControlReserveOperation:
            HandleReserveOperation(adapter, controlRequest);
            return WispDiskIoControlDisposition::Complete;
        case WispDiskControlQueryOperation:
            HandleQueryOperation(adapter, controlRequest);
            return WispDiskIoControlDisposition::Complete;
        case WispDiskControlListState:
            HandleListState(adapter, controlRequest);
            return WispDiskIoControlDisposition::Complete;
        case WispDiskControlCreateReserved:
        case WispDiskControlDeleteReserved: {
            WISPDISK_REQUEST_HEADER header{};
            const auto operation = static_cast<WISPDISK_CONTROL_CODE>(controlRequest.control->ControlCode);
            RtlCopyMemory(&header, controlRequest.payload, min(controlRequest.control->Length, static_cast<ULONG>(sizeof(header))));
            return QueueManagementRequest(adapter, controlRequest, operation, header.CorrelationId);
        }
        case WispDiskControlQueryVersion:
            HandleQueryVersion(controlRequest);
            return WispDiskIoControlDisposition::Complete;

        case WispDiskControlListDisks:
            HandleListDisks(adapter, controlRequest);
            return WispDiskIoControlDisposition::Complete;

        case WispDiskControlCreateDisk:
            return HandleCreateControl(adapter, controlRequest);

        case WispDiskControlDeleteDisk:
            return HandleDeleteControl(adapter, controlRequest);

        default:
            WriteSimpleErrorResponse(
                controlRequest,
                static_cast<WISPDISK_CONTROL_CODE>(controlRequest.control->ControlCode),
                STATUS_INVALID_DEVICE_REQUEST,
                0
            );
            return WispDiskIoControlDisposition::Complete;
    }
}

static void ProcessQueuedRequest(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _In_ const ControlRequest& controlRequest
) noexcept {
    switch (controlRequest.control->ControlCode) {
        case WispDiskControlCreateReserved:
        case WispDiskControlDeleteReserved:
            ProcessReservedOperation(adapter, controlRequest);
            break;
        case WispDiskControlCreateDisk:
            ProcessCreateRequest(adapter, controlRequest);
            break;

        case WispDiskControlDeleteDisk:
            ProcessDeleteRequest(adapter, controlRequest);
            break;

        default:
            WriteSimpleErrorResponse(
                controlRequest,
                static_cast<WISPDISK_CONTROL_CODE>(controlRequest.control->ControlCode),
                STATUS_INVALID_DEVICE_REQUEST,
                0
            );
            break;
    }
}

} // namespace wispdisk::management

WispDiskIoControlDisposition WispDiskHandleIoControl(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept {
    const auto controlRequest = wispdisk::management::GetControlRequest(srb);
    if (!controlRequest) {
        srb->DataTransferLength = 0;
        return WispDiskIoControlDisposition::Complete;
    }
    return wispdisk::management::DispatchControlRequest(adapter, controlRequest);
}

_Use_decl_annotations_
VOID WispDiskManagementWorker(PVOID deviceExtension, PVOID context, PVOID worker) {
    UNREFERENCED_PARAMETER(worker);
    auto* adapter = static_cast<PWISPDISK_ADAPTER_EXTENSION>(deviceExtension);
    auto* srb = static_cast<PSCSI_REQUEST_BLOCK>(context);
    const auto controlRequest = srb == nullptr
        ? wispdisk::management::ControlRequest{}
        : wispdisk::management::GetControlRequest(srb);

    if (adapter != nullptr && controlRequest) {
        using namespace wispdisk::management;
        const bool reserved = controlRequest.control->ControlCode == WispDiskControlCreateReserved || controlRequest.control->ControlCode == WispDiskControlDeleteReserved;
        WISPDISK_OPERATION_REQUEST identity{};
        if (reserved) RtlCopyMemory(&identity, controlRequest.payload, sizeof(identity));
        TraceManagement(adapter, identity.OperationId, 0, TracePhase::Worker, STATUS_PENDING);
        const ULONG fault = reserved ? TakeFault(adapter) : 0;
        if (fault == 4) {
            LARGE_INTEGER delay{}; delay.QuadPart = -2000000;
            KeDelayExecutionThread(KernelMode, FALSE, &delay);
        }
        if (fault == 3) {
            wispdisk::support::ScopedSpinLock lock{&adapter->LunLock};
            FailReservedOperation(adapter, controlRequest, STATUS_INSUFFICIENT_RESOURCES);
        } else {
            ProcessQueuedRequest(adapter, controlRequest);
        }
        if (fault == 2 && controlRequest.control->ReturnCode == static_cast<ULONG>(STATUS_SUCCESS)) {
            // Commit/result are retained; only transport completion fails.
            srb->SrbStatus = SRB_STATUS_ERROR;
            srb->ScsiStatus = SCSISTAT_CHECK_CONDITION;
        }
    } else if (srb != nullptr) {
        srb->DataTransferLength = 0;
        srb->ScsiStatus = SCSISTAT_CHECK_CONDITION;
        srb->SrbStatus = SRB_STATUS_INVALID_REQUEST;
    }

    // Copy notification intent before RequestComplete transfers SRB ownership.
    bool busChanged = controlRequest &&
        controlRequest.control->ReturnCode == static_cast<ULONG>(STATUS_SUCCESS) &&
        (controlRequest.control->ControlCode == WispDiskControlCreateDisk ||
         controlRequest.control->ControlCode == WispDiskControlDeleteDisk ||
         controlRequest.control->ControlCode == WispDiskControlCreateReserved ||
         controlRequest.control->ControlCode == WispDiskControlDeleteReserved);
    ULONGLONG operationId = 0;
    ULONG deviceId = 0;
    NTSTATUS resultStatus = STATUS_INVALID_DEVICE_REQUEST;
    if (controlRequest && (controlRequest.control->ControlCode == WispDiskControlCreateReserved || controlRequest.control->ControlCode == WispDiskControlDeleteReserved) &&
        controlRequest.control->Length == sizeof(WISPDISK_OPERATION_RESPONSE)) {
        WISPDISK_OPERATION_RESPONSE response{};
        RtlCopyMemory(&response, controlRequest.payload, sizeof(response));
        operationId = response.OperationId; deviceId = response.DeviceId; resultStatus = response.ResultStatus;
        busChanged = response.State == WispDiskOperationSucceeded && NT_SUCCESS(resultStatus);
    }
    const ULONG completedSrbStatus = srb == nullptr ? SRB_STATUS_INVALID_REQUEST : srb->SrbStatus;
    if (adapter != nullptr) wispdisk::management::TraceManagement(adapter, operationId, deviceId, wispdisk::management::TracePhase::Response, resultStatus, completedSrbStatus);
    if (adapter != nullptr && srb != nullptr) {
        StorPortNotification(RequestComplete, adapter, srb);
    }
    if (adapter != nullptr) {
        wispdisk::management::TraceManagement(adapter, operationId, deviceId, wispdisk::management::TracePhase::Complete, resultStatus, completedSrbStatus);
        if (busChanged) {
            StorPortNotification(BusChangeDetected, adapter, static_cast<ULONG>(0));
            wispdisk::management::TraceManagement(adapter, operationId, deviceId, wispdisk::management::TracePhase::BusChange, resultStatus);
        }
        // Teardown must wait through completion and bus notification. This is
        // the final adapter access by the worker.
        wispdisk::support::ScopedSpinLock lock{&adapter->LunLock};
        InterlockedExchange(&adapter->ManagementBusy, 0);
        KeSetEvent(&adapter->ManagementIdleEvent, IO_NO_INCREMENT, FALSE);
    }
}
