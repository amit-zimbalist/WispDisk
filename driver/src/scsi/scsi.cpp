#include "scsi.h"

#include "read_write.h"
#include "scsi_commands.h"
#include "scsi_response.h"
#include "support/scoped_spin_lock.h"

namespace wispdisk::scsi {

class LunIoLease final {
public:
    LunIoLease(
        _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
        _In_ PSCSI_REQUEST_BLOCK srb
    ) noexcept
        : adapter_{adapter} {
        if (srb->PathId != 0 || srb->TargetId != 0 || srb->Lun >= kMaximumDiskCount) {
            return;
        }

        support::ScopedSpinLock lock{&adapter_->LunLock};
        auto* lun = &adapter_->Luns[srb->Lun];
        if (lun->State != WispDiskLunState::Online || lun->BackingStore == nullptr) {
            return;
        }

        if (lun->ActiveRequests++ == 0) {
            KeClearEvent(&lun->NoActiveRequestsEvent);
        }
        view_ = {
            .lun = lun,
            .backingStore = lun->BackingStore,
            .sizeBytes = lun->SizeBytes,
            .mediaKind = lun->MediaKind,
            .deviceId = lun->DeviceId,
        };
    }

    ~LunIoLease() noexcept {
        if (view_.lun == nullptr) {
            return;
        }

        support::ScopedSpinLock lock{&adapter_->LunLock};
        NT_ASSERT(view_.lun->ActiveRequests != 0);
        if (view_.lun->ActiveRequests != 0 && --view_.lun->ActiveRequests == 0) {
            KeSetEvent(&view_.lun->NoActiveRequestsEvent, IO_NO_INCREMENT, FALSE);
        }
    }

    LunIoLease(const LunIoLease&) = delete;
    LunIoLease& operator=(const LunIoLease&) = delete;
    LunIoLease(LunIoLease&&) = delete;
    LunIoLease& operator=(LunIoLease&&) = delete;

    [[nodiscard]] explicit operator bool() const noexcept {
        return view_.lun != nullptr;
    }

    [[nodiscard]] const LunView& view() const noexcept {
        return view_;
    }

private:
    PWISPDISK_ADAPTER_EXTENSION adapter_;
    LunView view_{};
};

static UCHAR DispatchScsiCommand(
    _In_ const LunView& view,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept {
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
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept {
    if (srb->CdbLength == 0) {
        return wispdisk::scsi::SetSense(
            srb,
            SCSI_SENSE_ILLEGAL_REQUEST,
            SCSI_ADSENSE_INVALID_CDB
        );
    }

    if (srb->Cdb[0] == SCSIOP_REPORT_LUNS) {
        return wispdisk::scsi::HandleReportLuns(adapter, srb);
    }

    const wispdisk::scsi::LunIoLease lunLease{adapter, srb};
    if (!lunLease) {
        srb->DataTransferLength = 0;
        return SRB_STATUS_NO_DEVICE;
    }
    return wispdisk::scsi::DispatchScsiCommand(lunLease.view(), srb);
}
