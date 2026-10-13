#pragma once
#include "control_request.h"
#include "support/scoped_spin_lock.h"

namespace wispdisk::management {
// 0x8F0 configures tracing/faults; 0x8F1 reads the last 16 trace entries.
// Both commands are available only in Debug builds.
enum class TracePhase : ULONG { Accepted = 1, Worker = 2, Committed = 3, Response = 4, Complete = 5, BusChange = 6, Rejected = 7 };
inline void TraceLocked(PWISPDISK_ADAPTER_EXTENSION adapter, ULONGLONG operation, ULONG device,
                        TracePhase phase, NTSTATUS status, ULONG srbStatus = 0) noexcept {
#if DBG
    if (adapter->TraceEnabled == 0) return;
    const auto sequence = ++adapter->TraceSequence;
    adapter->Trace[(sequence - 1) % RTL_NUMBER_OF(adapter->Trace)] = {
        .Sequence = sequence, .OperationId = operation,
        .Timestamp = KeQueryPerformanceCounter(nullptr).QuadPart,
        .DeviceId = device, .Phase = static_cast<ULONG>(phase), .Status = status, .SrbStatus = srbStatus,
    };
#else
    UNREFERENCED_PARAMETER(adapter); UNREFERENCED_PARAMETER(operation); UNREFERENCED_PARAMETER(device);
    UNREFERENCED_PARAMETER(phase); UNREFERENCED_PARAMETER(status); UNREFERENCED_PARAMETER(srbStatus);
#endif
}
inline void TraceManagement(PWISPDISK_ADAPTER_EXTENSION adapter, ULONGLONG operation, ULONG device,
                            TracePhase phase, NTSTATUS status, ULONG srbStatus = 0) noexcept {
#if DBG
    support::ScopedSpinLock lock{&adapter->LunLock};
#endif
    TraceLocked(adapter, operation, device, phase, status, srbStatus);
}
inline ULONG TakeFault(PWISPDISK_ADAPTER_EXTENSION adapter) noexcept {
#if DBG
    support::ScopedSpinLock lock{&adapter->LunLock};
    if (adapter->FaultRemaining != 0) { --adapter->FaultRemaining; return adapter->FaultMode; }
#else
    UNREFERENCED_PARAMETER(adapter);
#endif
    return 0;
}
inline void HandleDiagnostics(PWISPDISK_ADAPTER_EXTENSION adapter, const ControlRequest& control) noexcept {
#if DBG
    struct Configuration { WISPDISK_REQUEST_HEADER Header; ULONG Mode; ULONG Count; };
    struct TraceResponse { WISPDISK_RESPONSE_HEADER Header; ULONGLONG Sequence; ULONG Count; ULONG Reserved; WISPDISK_ADAPTER_EXTENSION::TraceEntry Entries[16]; };
    static_assert(sizeof(TraceResponse) == 680);
    const bool configure = control.control->ControlCode == 0x8F0;
    const auto operation = static_cast<WISPDISK_CONTROL_CODE>(control.control->ControlCode);
    WISPDISK_REQUEST_HEADER header{};
    NTSTATUS status = ReadAndValidateRequestHeader(control.payload, control.control->Length,
        configure ? sizeof(Configuration) : sizeof(header), operation, &header);
    if (NT_SUCCESS(status) && control.control->Length < (configure ? sizeof(WISPDISK_RESPONSE_HEADER) : sizeof(TraceResponse))) status = STATUS_BUFFER_TOO_SMALL;
    if (!NT_SUCCESS(status)) { WriteSimpleErrorResponse(control, operation, status, header.CorrelationId); return; }
    support::ScopedSpinLock lock{&adapter->LunLock};
    if (configure) {
        Configuration request{};
        RtlCopyMemory(&request, control.payload, sizeof(request));
        if (request.Mode > 4 || request.Count > 1000) { WriteSimpleErrorResponse(control, operation, STATUS_INVALID_PARAMETER, header.CorrelationId); return; }
        adapter->TraceEnabled = request.Mode != 0;
        adapter->FaultMode = request.Mode;
        adapter->FaultRemaining = request.Mode > 1 ? request.Count : 0;
        WriteSimpleErrorResponse(control, operation, STATUS_SUCCESS, header.CorrelationId);
        return;
    }
    TraceResponse response{.Header = MakeResponseHeader(sizeof(TraceResponse), operation, STATUS_SUCCESS, header.CorrelationId), .Sequence = adapter->TraceSequence};
    response.Count = static_cast<ULONG>(min(adapter->TraceSequence, static_cast<ULONGLONG>(RTL_NUMBER_OF(response.Entries))));
    const auto first = adapter->TraceSequence - response.Count;
    for (ULONG index = 0; index < response.Count; ++index) response.Entries[index] = adapter->Trace[(first + index) % RTL_NUMBER_OF(adapter->Trace)];
    RtlCopyMemory(control.payload, &response, sizeof(response));
    control.control->ReturnCode = STATUS_SUCCESS; control.control->Length = sizeof(response);
    control.srb->DataTransferLength = sizeof(SRB_IO_CONTROL) + sizeof(response);
    control.srb->ScsiStatus = SCSISTAT_GOOD; control.srb->SrbStatus = SRB_STATUS_SUCCESS;
#else
    UNREFERENCED_PARAMETER(adapter);
    WriteSimpleErrorResponse(control, static_cast<WISPDISK_CONTROL_CODE>(control.control->ControlCode), STATUS_NOT_SUPPORTED, 0);
#endif
}
}
