#include "operation_state.h"
#include "adapter_context.h"
#include "logging.h"

namespace wispdisk::management {

template <typename Request>
static NTSTATUS ReadOperationRequest(
    const WISPDISK_ADAPTER_EXTENSION& adapter,
    const ControlRequest& control,
    Request& request,
    WISPDISK_REQUEST_HEADER& header) noexcept {
    const auto operation = static_cast<WISPDISK_CONTROL_CODE>(control.ioControl->ControlCode);
    NTSTATUS status = control.ReadRequest(operation, request, header);
    if (NT_SUCCESS(status) && control.ioControl->Length < sizeof(WISPDISK_OPERATION_RESPONSE)) {
        status = STATUS_BUFFER_TOO_SMALL;
    }
    if (!NT_SUCCESS(status)) {
        WARN(
            "OperationRequestValidationFailed",
            .AdapterInstance = adapter.AdapterInstance,
            .OperationId = header.CorrelationId,
            .Status = status);
        control.WriteError(operation, status, header.CorrelationId);
    }
    return status;
}

void HandleReserveOperation(
    PWISPDISK_ADAPTER_EXTENSION adapter,
    const ControlRequest& control) noexcept {
    WISPDISK_REQUEST_HEADER header{};
    WISPDISK_RESERVE_REQUEST request{};
    if (!NT_SUCCESS(ReadOperationRequest(*adapter, control, request, header))) {
        return;
    }
    OperationResult result{};
    const NTSTATUS status = request.Reserved == 0
                                ? driver::AdapterContext{*adapter}.Management().ReserveOperation(
                                      request.Mutation,
                                      result)
                                : STATUS_INVALID_PARAMETER;
    if (!NT_SUCCESS(status)) {
        control.WriteError(WispDiskControlReserveOperation, status, header.CorrelationId);
        return;
    }
    WriteOperationResponse(control, result, header.CorrelationId);
}

void HandleQueryOperation(
    PWISPDISK_ADAPTER_EXTENSION adapter,
    const ControlRequest& control) noexcept {
    WISPDISK_REQUEST_HEADER header{};
    WISPDISK_OPERATION_REQUEST request{};
    if (!NT_SUCCESS(ReadOperationRequest(*adapter, control, request, header))) {
        return;
    }
    OperationResult result{};
    const NTSTATUS status = driver::AdapterContext{*adapter}.Management().QueryOperation(
        request.AdapterInstance,
        request.OperationId,
        result);
    if (!NT_SUCCESS(status)) {
        control.WriteError(WispDiskControlQueryOperation, status, header.CorrelationId);
        return;
    }
    WriteOperationResponse(control, result, header.CorrelationId);
}

WispDiskIoControlDisposition HandleReservedCreateControl(
    PWISPDISK_ADAPTER_EXTENSION adapter,
    const ControlRequest& control) noexcept {
    WISPDISK_REQUEST_HEADER header{};
    WISPDISK_RESERVED_CREATE_REQUEST request{};
    if (!NT_SUCCESS(ReadOperationRequest(*adapter, control, request, header))) {
        return WispDiskIoControlDisposition::Complete;
    }
    WISPDISK_CREATE_REQUEST parameters{
        .Header = request.Operation.Header,
        .SizeBytes = request.SizeBytes,
        .LogicalSectorSize = request.LogicalSectorSize,
        .MediaKind = request.MediaKind,
        .RequestedDriveLetter = request.RequestedDriveLetter,
        .Reserved16 = request.Reserved16,
        .Reserved32 = request.Reserved32,
    };
    parameters.Header.StructSize = sizeof(parameters);
    parameters.Header.Operation = WispDiskControlCreateDisk;
    SRB_IO_CONTROL scratch{.Length = sizeof(parameters)};
    const ControlRequest validation{control.srb, &scratch, reinterpret_cast<PUCHAR>(&parameters)};
    WISPDISK_CREATE_REQUEST validated{};
    const NTSTATUS status = ValidateCreateRequest(validation, validated);
    if (!NT_SUCCESS(status)) {
        WARN(
            "ReservedCreateRequestValidationFailed",
            .AdapterInstance = adapter->AdapterInstance,
            .OperationId = request.Operation.OperationId,
            .Status = status,
            .SizeBytes = request.SizeBytes);
        control.WriteError(WispDiskControlCreateReserved, status, header.CorrelationId);
        return WispDiskIoControlDisposition::Complete;
    }
    return driver::AdapterContext{*adapter}.Management().SubmitCreate(
        control,
        request.Operation,
        validated);
}

WispDiskIoControlDisposition HandleReservedDeleteControl(
    PWISPDISK_ADAPTER_EXTENSION adapter,
    const ControlRequest& control) noexcept {
    WISPDISK_REQUEST_HEADER header{};
    WISPDISK_RESERVED_DELETE_REQUEST request{};
    if (!NT_SUCCESS(ReadOperationRequest(*adapter, control, request, header))) {
        return WispDiskIoControlDisposition::Complete;
    }
    WISPDISK_DELETE_REQUEST parameters{
        .Header = request.Operation.Header,
        .DeviceId = request.DeviceId,
        .Reserved = request.Reserved,
    };
    parameters.Header.StructSize = sizeof(parameters);
    parameters.Header.Operation = WispDiskControlDeleteDisk;
    SRB_IO_CONTROL scratch{.Length = sizeof(parameters)};
    const ControlRequest validation{control.srb, &scratch, reinterpret_cast<PUCHAR>(&parameters)};
    WISPDISK_DELETE_REQUEST validated{};
    const NTSTATUS status = ValidateDeleteRequest(validation, validated);
    if (!NT_SUCCESS(status)) {
        WARN(
            "ReservedDeleteRequestValidationFailed",
            .AdapterInstance = adapter->AdapterInstance,
            .OperationId = request.Operation.OperationId,
            .DeviceId = request.DeviceId,
            .Status = status);
        control.WriteError(WispDiskControlDeleteReserved, status, header.CorrelationId);
        return WispDiskIoControlDisposition::Complete;
    }
    return driver::AdapterContext{*adapter}.Management().SubmitDelete(
        control,
        request.Operation,
        validated);
}

void HandleListState(PWISPDISK_ADAPTER_EXTENSION adapter, const ControlRequest& control) noexcept {
    WISPDISK_REQUEST_HEADER header{};
    NTSTATUS status = ReadAndValidateRequestHeader(
        control.payload,
        control.ioControl->Length,
        sizeof(header),
        WispDiskControlListState,
        header);
    if (NT_SUCCESS(status) && control.ioControl->Length < sizeof(WISPDISK_STATE_RESPONSE)) {
        status = STATUS_BUFFER_TOO_SMALL;
    }
    if (!NT_SUCCESS(status)) {
        control.WriteError(WispDiskControlListState, status, header.CorrelationId);
        return;
    }
    WISPDISK_STATE_RESPONSE response{
        .Header = MakeResponseHeader(
            sizeof(WISPDISK_STATE_RESPONSE),
            WispDiskControlListState,
            STATUS_SUCCESS,
            header.CorrelationId)};
    driver::AdapterContext{*adapter}.Management().SnapshotState(response);
    control.WriteResponse(response, STATUS_SUCCESS);
}

} // namespace wispdisk::management
