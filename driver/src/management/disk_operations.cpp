#include "disk_operations.h"

#include "adapter_context.h"

namespace wispdisk::management {

void HandleQueryVersion(_In_ const ControlRequest& controlRequest) noexcept {
    WISPDISK_REQUEST_HEADER request{};
    const NTSTATUS validation = ReadAndValidateRequestHeader(
        controlRequest.payload,
        controlRequest.ioControl->Length,
        sizeof(request),
        WispDiskControlQueryVersion,
        request);
    if (!NT_SUCCESS(validation)) {
        controlRequest.WriteError(WispDiskControlQueryVersion, validation, request.CorrelationId);
        return;
    }

    if (controlRequest.ioControl->Length < sizeof(WISPDISK_QUERY_VERSION_RESPONSE)) {
        controlRequest.WriteError(
            WispDiskControlQueryVersion,
            STATUS_BUFFER_TOO_SMALL,
            request.CorrelationId);
        return;
    }

    const WISPDISK_QUERY_VERSION_RESPONSE response{
        .Header = MakeResponseHeader(
            sizeof(WISPDISK_QUERY_VERSION_RESPONSE),
            WispDiskControlQueryVersion,
            STATUS_SUCCESS,
            request.CorrelationId),
        .VersionMajor = 0,
        .VersionMinor = 3,
        .Capabilities = WISPDISK_CAP_CREATE_DELETE | WISPDISK_CAP_READ_WRITE |
                        WISPDISK_CAP_REMOVABLE | WISPDISK_CAP_OPERATION_RECOVERY,
        .MaximumDiskCount = kMaximumDiskCount,
    };
    controlRequest.WriteResponse(response, STATUS_SUCCESS);
}

void HandleListDisks(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _In_ const ControlRequest& controlRequest) noexcept {
    WISPDISK_REQUEST_HEADER request{};
    const NTSTATUS validation = ReadAndValidateRequestHeader(
        controlRequest.payload,
        controlRequest.ioControl->Length,
        sizeof(request),
        WispDiskControlListDisks,
        request);
    if (!NT_SUCCESS(validation) ||
        controlRequest.ioControl->Length < sizeof(WISPDISK_LIST_RESPONSE)) {
        const NTSTATUS status = NT_SUCCESS(validation) ? STATUS_BUFFER_TOO_SMALL : validation;
        controlRequest.WriteError(WispDiskControlListDisks, status, request.CorrelationId);
        return;
    }

    WISPDISK_LIST_RESPONSE response{
        .Header = MakeResponseHeader(
            sizeof(WISPDISK_LIST_RESPONSE),
            WispDiskControlListDisks,
            STATUS_SUCCESS,
            request.CorrelationId),
    };
    driver::AdapterContext{*adapter}.Disks().SnapshotOnlineDisks(response);

    controlRequest.WriteResponse(response, STATUS_SUCCESS);
}

} // namespace wispdisk::management
