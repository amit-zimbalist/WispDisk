#include "scsi_commands.h"

#include "scsi_response.h"
#include "support/scoped_spin_lock.h"

namespace wispdisk::scsi {

constexpr UCHAR kDirectAccessDevice = 0x00;
constexpr UCHAR kVpdSupportedPages = 0x00;
constexpr UCHAR kVpdUnitSerialNumber = 0x80;
constexpr UCHAR kVpdDeviceIdentification = 0x83;

static ULONG BuildLunList(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _Inout_updates_(8 + (WISPDISK_MAXIMUM_DISK_COUNT * 8)) UCHAR* response
) noexcept {
    ULONG count = 0;
    support::ScopedSpinLock lock{&adapter->LunLock};
    for (const auto& lun : adapter->Luns) {
        if (lun.State == WispDiskLunState::Online) {
            response[8 + (count * 8) + 1] = lun.Lun;
            ++count;
        }
    }
    return count;
}

UCHAR HandleInquiry(
    _In_ const LunView& view,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept {
    if (srb->CdbLength < 6) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }

    const auto* cdb = srb->Cdb;
    const bool enableVitalProductData = (cdb[1] & 0x01U) != 0;
    const UCHAR pageCode = cdb[2];
    const ULONG allocationLength = cdb[4];
    UCHAR response[64]{};
    ULONG responseLength = 0;

    if (!enableVitalProductData) {
        if (pageCode != 0) {
            return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
        }
        response[0] = kDirectAccessDevice;
        response[1] = view.mediaKind == WispDiskMediaRemovable ? 0x80U : 0x00U;
        response[2] = 0x05;
        response[3] = 0x02;
        response[4] = 31;
        static constexpr UCHAR vendor[8] = {'W', 'I', 'S', 'P', 'D', 'I', 'S', 'K'};
        static constexpr UCHAR product[16] = {
            'V', 'I', 'R', 'T', 'U', 'A', 'L', ' ', 'D', 'I', 'S', 'K', ' ', ' ', ' ', ' '
        };
        static constexpr UCHAR revision[4] = {'0', '0', '0', '2'};
        RtlCopyMemory(&response[8], vendor, sizeof(vendor));
        RtlCopyMemory(&response[16], product, sizeof(product));
        RtlCopyMemory(&response[32], revision, sizeof(revision));
        responseLength = 36;
    } else if (pageCode == kVpdSupportedPages) {
        response[0] = kDirectAccessDevice;
        response[1] = kVpdSupportedPages;
        response[3] = 3;
        response[4] = kVpdSupportedPages;
        response[5] = kVpdUnitSerialNumber;
        response[6] = kVpdDeviceIdentification;
        responseLength = 7;
    } else if (pageCode == kVpdUnitSerialNumber) {
        response[0] = kDirectAccessDevice;
        response[1] = kVpdUnitSerialNumber;
        response[3] = 14;
        static constexpr UCHAR prefix[6] = {'W', 'I', 'S', 'P', 'D', 'K'};
        RtlCopyMemory(&response[4], prefix, sizeof(prefix));
        WriteDeviceIdHex(&response[10], view.deviceId);
        responseLength = 18;
    } else if (pageCode == kVpdDeviceIdentification) {
        response[0] = kDirectAccessDevice;
        response[1] = kVpdDeviceIdentification;
        response[4] = 0x02; // ASCII.
        response[5] = 0x01; // T10 vendor identifier, associated with the LUN.
        response[7] = 22;
        static constexpr UCHAR vendor[8] = {'W', 'I', 'S', 'P', 'D', 'I', 'S', 'K'};
        static constexpr UCHAR prefix[6] = {'W', 'I', 'S', 'P', 'D', 'K'};
        RtlCopyMemory(&response[8], vendor, sizeof(vendor));
        RtlCopyMemory(&response[16], prefix, sizeof(prefix));
        WriteDeviceIdHex(&response[22], view.deviceId);
        WriteBigEndian16(&response[2], 26);
        responseLength = 30;
    } else {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }

    if (responseLength > allocationLength) {
        responseLength = allocationLength;
    }
    return CopyScsiResponse(srb, response, responseLength);
}

UCHAR HandleReadCapacity10(
    _In_ const LunView& view,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept {
    UCHAR response[8]{};
    const ULONGLONG blockCount = view.sizeBytes / kLogicalSectorSize;
    const ULONGLONG lastLba = blockCount - 1;
    const ULONG reportedLastLba = lastLba > MAXULONG ? MAXULONG : static_cast<ULONG>(lastLba);
    WriteBigEndian32(&response[0], reportedLastLba);
    WriteBigEndian32(&response[4], kLogicalSectorSize);
    return CopyScsiResponse(srb, response, sizeof(response));
}

UCHAR HandleReadCapacity16(
    _In_ const LunView& view,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept {
    if (srb->CdbLength < 16 || (srb->Cdb[1] & 0x1fU) != SERVICE_ACTION_READ_CAPACITY16) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }
    UCHAR response[32]{};
    WriteBigEndian64(&response[0], (view.sizeBytes / kLogicalSectorSize) - 1);
    WriteBigEndian32(&response[8], kLogicalSectorSize);
    return CopyScsiResponse(srb, response, sizeof(response));
}

UCHAR HandleRequestSense(_Inout_ PSCSI_REQUEST_BLOCK srb) noexcept {
    if (srb->CdbLength < 6) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }
    UCHAR response[kSenseDataLength]{};
    response[0] = SCSI_SENSE_ERRORCODE_FIXED_CURRENT;
    response[7] = 10;
    ULONG responseLength = kSenseDataLength;
    if (responseLength > srb->Cdb[4]) {
        responseLength = srb->Cdb[4];
    }
    return CopyScsiResponse(srb, response, responseLength);
}

UCHAR HandleModeSense(_Inout_ PSCSI_REQUEST_BLOCK srb, _In_ bool tenByte) noexcept {
    const ULONG requiredCdbLength = tenByte ? 10 : 6;
    if (srb->CdbLength < requiredCdbLength) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }

    const UCHAR pageCode = srb->Cdb[2] & 0x3fU;
    if (pageCode != 0x00 && pageCode != 0x08 && pageCode != 0x3f) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }

    UCHAR response[32]{};
    ULONG responseLength = tenByte ? 8 : 4;
    if (pageCode == 0x08 || pageCode == 0x3f) {
        auto* page = &response[responseLength];
        page[0] = 0x08;
        page[1] = 0x12;
        responseLength += 20;
    }

    ULONG allocationLength;
    if (tenByte) {
        WriteBigEndian16(&response[0], static_cast<USHORT>(responseLength - 2));
        allocationLength = (static_cast<ULONG>(srb->Cdb[7]) << 8) | srb->Cdb[8];
    } else {
        response[0] = static_cast<UCHAR>(responseLength - 1);
        allocationLength = srb->Cdb[4];
    }
    if (responseLength > allocationLength) {
        responseLength = allocationLength;
    }
    return CopyScsiResponse(srb, response, responseLength);
}

UCHAR HandleReportLuns(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept {
    if (srb->CdbLength < 12 || srb->PathId != 0 || srb->TargetId != 0) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }

    UCHAR response[8 + (WISPDISK_MAXIMUM_DISK_COUNT * 8)]{};
    const ULONG count = BuildLunList(adapter, response);
    WriteBigEndian32(&response[0], count * 8);

    ULONG responseLength = 8 + (count * 8);
    const ULONG allocationLength = ReadBigEndian32(&srb->Cdb[6]);
    if (responseLength > allocationLength) {
        responseLength = allocationLength;
    }
    return CopyScsiResponse(srb, response, responseLength);
}

} // namespace wispdisk::scsi
