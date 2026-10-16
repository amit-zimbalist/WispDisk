#include "management.h"

#include "adapter_context.h"
#include "control_request.h"
#include "diagnostics.h"
#include "disk_operations.h"
#include "operation_state.h"

namespace wispdisk::management {

static WispDiskIoControlDisposition
HandleCreateControl(PWISPDISK_ADAPTER_EXTENSION adapter, const ControlRequest& control) noexcept {
    WISPDISK_CREATE_REQUEST request{};
    const NTSTATUS status = ValidateCreateRequest(control, request);
    if (!NT_SUCCESS(status)) {
        WriteCreateResponse(control, request, status);
        return WispDiskIoControlDisposition::Complete;
    }
    return driver::AdapterContext{*adapter}.Management().SubmitCreate(control, request);
}

static WispDiskIoControlDisposition
HandleDeleteControl(PWISPDISK_ADAPTER_EXTENSION adapter, const ControlRequest& control) noexcept {
    WISPDISK_DELETE_REQUEST request{};
    const NTSTATUS status = ValidateDeleteRequest(control, request);
    if (!NT_SUCCESS(status)) {
        WriteDeleteResponse(control, request, status);
        return WispDiskIoControlDisposition::Complete;
    }
    return driver::AdapterContext{*adapter}.Management().SubmitDelete(control, request);
}

static WispDiskIoControlDisposition DispatchControlRequest(
    PWISPDISK_ADAPTER_EXTENSION adapter,
    const ControlRequest& control) noexcept {
    switch (control.ioControl->ControlCode) {
        case diagnostics::kConfigureControlCode:
        case diagnostics::kReadTraceControlCode:
            diagnostics::HandleControl(*adapter, control);
            return WispDiskIoControlDisposition::Complete;
        case WispDiskControlReserveOperation:
            HandleReserveOperation(adapter, control);
            return WispDiskIoControlDisposition::Complete;
        case WispDiskControlQueryOperation:
            HandleQueryOperation(adapter, control);
            return WispDiskIoControlDisposition::Complete;
        case WispDiskControlListState:
            HandleListState(adapter, control);
            return WispDiskIoControlDisposition::Complete;
        case WispDiskControlCreateReserved:
            return HandleReservedCreateControl(adapter, control);
        case WispDiskControlDeleteReserved:
            return HandleReservedDeleteControl(adapter, control);
        case WispDiskControlQueryVersion:
            HandleQueryVersion(control);
            return WispDiskIoControlDisposition::Complete;
        case WispDiskControlListDisks:
            HandleListDisks(adapter, control);
            return WispDiskIoControlDisposition::Complete;
        case WispDiskControlCreateDisk:
            return HandleCreateControl(adapter, control);
        case WispDiskControlDeleteDisk:
            return HandleDeleteControl(adapter, control);
        default:
            control.WriteError(
                static_cast<WISPDISK_CONTROL_CODE>(control.ioControl->ControlCode),
                STATUS_INVALID_DEVICE_REQUEST,
                0);
            return WispDiskIoControlDisposition::Complete;
    }
}

} // namespace wispdisk::management

_Use_decl_annotations_
WispDiskIoControlDisposition
WispDiskHandleIoControl(PWISPDISK_ADAPTER_EXTENSION adapter, PSCSI_REQUEST_BLOCK srb) noexcept {
    const auto control = wispdisk::management::ControlRequest::FromSrb(srb);
    if (!control) {
        srb->DataTransferLength = 0;
        return WispDiskIoControlDisposition::Complete;
    }
    return wispdisk::management::DispatchControlRequest(adapter, control);
}

_Use_decl_annotations_
VOID WispDiskManagementWorker(PVOID deviceExtension, PVOID context, PVOID worker) {
    UNREFERENCED_PARAMETER(worker);
    auto* adapter = static_cast<PWISPDISK_ADAPTER_EXTENSION>(deviceExtension);
    auto* srb = static_cast<PSCSI_REQUEST_BLOCK>(context);
    if (adapter != nullptr) {
        wispdisk::driver::AdapterContext{*adapter}.Management().ProcessWorkItem(srb);
    } else if (srb != nullptr) {
        srb->DataTransferLength = 0;
        srb->ScsiStatus = SCSISTAT_CHECK_CONDITION;
        srb->SrbStatus = SRB_STATUS_INVALID_REQUEST;
    }
}
