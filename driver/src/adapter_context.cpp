#include "adapter_context.h"
#include "scsi/scsi.h"

namespace wispdisk::driver {

AdapterContext::AdapterContext(WISPDISK_ADAPTER_EXTENSION& extension) noexcept
    : extension_{extension} {}

management::ManagementController AdapterContext::Management() const noexcept {
    return management::ManagementController{extension_};
}

storage::LunStore AdapterContext::Disks() const noexcept {
    return storage::LunStore{extension_};
}

bool AdapterContext::IsInitialized() const noexcept {
    return extension_.Signature == kAdapterExtensionSignature;
}

_Use_decl_annotations_
bool AdapterContext::Initialize() noexcept {
    RtlZeroMemory(&extension_, sizeof(extension_));
    KeInitializeSpinLock(&extension_.LunLock);
    extension_.NextDeviceId = 1;
    LARGE_INTEGER instanceTime{};
    KeQuerySystemTimePrecise(&instanceTime);
    static volatile LONG64 lastInstance = 0;
    LONG64 previous;
    LONG64 instance;
    do {
        previous = lastInstance;
        instance = instanceTime.QuadPart > previous ? instanceTime.QuadPart : previous + 1;
    } while (InterlockedCompareExchange64(&lastInstance, instance, previous) != previous);
    extension_.AdapterInstance = static_cast<ULONGLONG>(instance);
    Disks().Initialize();
    extension_.Signature = kAdapterExtensionSignature;
    if (!Management().Initialize()) {
        extension_.Signature = 0;
        return false;
    }
    return true;
}

bool AdapterContext::SetStopping(bool stopping) noexcept {
    if (!IsInitialized()) {
        return false;
    }
    // ScsiStopAdapter/ScsiRestartAdapter may run at DIRQL. They must not
    // acquire our DISPATCH_LEVEL spin lock; management shutdown closes
    // admission under that lock during teardown.
    InterlockedExchange(&extension_.AdapterStopping, stopping ? 1 : 0);
    return true;
}

void AdapterContext::CompleteRequest(PSCSI_REQUEST_BLOCK srb, UCHAR status) const noexcept {
    srb->SrbStatus = status;
    const UCHAR baseStatus = status & 0x3fU;
    if (baseStatus == SRB_STATUS_SUCCESS || baseStatus == SRB_STATUS_DATA_OVERRUN) {
        srb->ScsiStatus = SCSISTAT_GOOD;
    }
    StorPortNotification(RequestComplete, &extension_, srb);
}

_Use_decl_annotations_
BOOLEAN AdapterContext::StartIo(PSCSI_REQUEST_BLOCK srb) const noexcept {
    switch (srb->Function) {
        case SRB_FUNCTION_IO_CONTROL:
            if (WispDiskHandleIoControl(&extension_, srb) ==
                WispDiskIoControlDisposition::Pending) {
                return TRUE;
            }
            if (srb->SrbStatus == SRB_STATUS_PENDING) {
                srb->SrbStatus = SRB_STATUS_INVALID_REQUEST;
            }
            CompleteRequest(srb, srb->SrbStatus);
            break;
        case SRB_FUNCTION_EXECUTE_SCSI:
            CompleteRequest(srb, WispDiskHandleExecuteScsi(&extension_, srb));
            break;
        case SRB_FUNCTION_PNP:
        case SRB_FUNCTION_POWER:
        case SRB_FUNCTION_FLUSH:
        case SRB_FUNCTION_SHUTDOWN:
            srb->DataTransferLength = 0;
            CompleteRequest(srb, SRB_STATUS_SUCCESS);
            break;
        default:
            srb->DataTransferLength = 0;
            CompleteRequest(srb, SRB_STATUS_INVALID_REQUEST);
            break;
    }
    return TRUE;
}

_Use_decl_annotations_
void AdapterContext::ReleaseResources() noexcept {
    if (!IsInitialized()) {
        return;
    }
    Management().Shutdown();
    extension_.Signature = 0;
}

} // namespace wispdisk::driver
