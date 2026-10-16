#include "scsi.h"
#include "adapter_context.h"

#include "read_write.h"
#include "scsi_commands.h"
#include "scsi_response.h"

namespace wispdisk::scsi {

static UCHAR
DispatchScsiCommand(_In_ const LunView& view, _Inout_ PSCSI_REQUEST_BLOCK srb) noexcept {
    switch (srb->Cdb[0]) {
        case SCSIOP_TEST_UNIT_READY:
        case SCSIOP_START_STOP_UNIT:
        case SCSIOP_MEDIUM_REMOVAL:
        case SCSIOP_SYNCHRONIZE_CACHE:
        case SCSIOP_SYNCHRONIZE_CACHE16:
        case SCSIOP_VERIFY:
        case SCSIOP_VERIFY16:
        case SCSIOP_MODE_SELECT:
        case SCSIOP_MODE_SELECT10:
            srb->DataTransferLength = 0;
            return SRB_STATUS_SUCCESS;

        case SCSIOP_INQUIRY:
            return HandleInquiry(view, srb);

        case SCSIOP_REQUEST_SENSE:
            return HandleRequestSense(srb);

        case SCSIOP_READ_CAPACITY:
            return HandleReadCapacity10(view, srb);

        case SCSIOP_READ_CAPACITY16:
            return HandleReadCapacity16(view, srb);

        case SCSIOP_MODE_SENSE:
            return HandleModeSense(srb, false);

        case SCSIOP_MODE_SENSE10:
            return HandleModeSense(srb, true);

        case SCSIOP_READ6:
        case SCSIOP_WRITE6:
        case SCSIOP_READ:
        case SCSIOP_WRITE:
        case SCSIOP_READ12:
        case SCSIOP_WRITE12:
        case SCSIOP_READ16:
        case SCSIOP_WRITE16:
            return HandleReadWrite(view, srb);

        default:
            return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_ILLEGAL_COMMAND);
    }
}

} // namespace wispdisk::scsi

UCHAR WispDiskHandleExecuteScsi(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _Inout_ PSCSI_REQUEST_BLOCK srb) noexcept {
    if (srb->CdbLength == 0) {
        return wispdisk::scsi::SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }

    if (srb->Cdb[0] == SCSIOP_REPORT_LUNS) {
        return wispdisk::scsi::HandleReportLuns(adapter, srb);
    }

    const auto lunLease = wispdisk::driver::AdapterContext{*adapter}.Disks().AcquireIo(*srb);
    if (!lunLease) {
        srb->DataTransferLength = 0;
        return SRB_STATUS_NO_DEVICE;
    }
    return wispdisk::scsi::DispatchScsiCommand(lunLease.view(), srb);
}
