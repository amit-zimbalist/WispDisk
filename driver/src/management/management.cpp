#include "management.h"

#include "control_request.h"
#include "disk_operations.h"

namespace wispdisk::management {

static WispDiskIoControlDisposition QueueManagementRequest(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _In_ const ControlRequest& controlRequest,
    _In_ WISPDISK_CONTROL_CODE operation,
    _In_ ULONGLONG correlationId
) noexcept {
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

    const ULONG storStatus = adapter->ManagementWorker == nullptr
        ? STOR_STATUS_INVALID_DEVICE_STATE
        : StorPortQueueWorkItem(
              adapter,
              WispDiskManagementWorker,
              adapter->ManagementWorker,
              controlRequest.srb
          );

    if (storStatus != STOR_STATUS_SUCCESS) {
        InterlockedExchange(&adapter->ManagementBusy, 0);
        KeSetEvent(&adapter->ManagementIdleEvent, IO_NO_INCREMENT, FALSE);
        KdPrint((
            "WispDisk: failed to queue management operation=%lu, StorPort status=%lu\n",
            static_cast<ULONG>(operation),
            storStatus
        ));
        WriteSimpleErrorResponse(
            controlRequest,
            operation,
            STATUS_INSUFFICIENT_RESOURCES,
            correlationId
        );
        return WispDiskIoControlDisposition::Complete;
    }

    controlRequest.control->ReturnCode = static_cast<ULONG>(STATUS_PENDING);
    controlRequest.srb->SrbStatus = SRB_STATUS_PENDING;
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
        wispdisk::management::ProcessQueuedRequest(adapter, controlRequest);
    } else if (srb != nullptr) {
        srb->DataTransferLength = 0;
        srb->ScsiStatus = SCSISTAT_CHECK_CONDITION;
        srb->SrbStatus = SRB_STATUS_INVALID_REQUEST;
    }

    if (adapter != nullptr) {
        InterlockedExchange(&adapter->ManagementBusy, 0);
        KeSetEvent(&adapter->ManagementIdleEvent, IO_NO_INCREMENT, FALSE);
    }
    if (adapter != nullptr && srb != nullptr) {
        StorPortNotification(RequestComplete, adapter, srb);
    }
}
