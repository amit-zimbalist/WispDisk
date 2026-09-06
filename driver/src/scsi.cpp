#include "driver_internal.h"

namespace {

struct WISPDISK_LUN_VIEW {
    PWISPDISK_LUN Lun;
    PUCHAR BackingStore;
    ULONGLONG SizeBytes;
    ULONG MediaKind;
    ULONG DeviceId;
};

constexpr UCHAR kDirectAccessDevice = 0x00;
constexpr UCHAR kVpdSupportedPages = 0x00;
constexpr UCHAR kVpdUnitSerialNumber = 0x80;
constexpr UCHAR kVpdDeviceIdentification = 0x83;
constexpr ULONG kSenseDataLength = 18;

ULONG ReadBigEndian32(_In_reads_(4) const UCHAR* value) noexcept {
    return (static_cast<ULONG>(value[0]) << 24) |
           (static_cast<ULONG>(value[1]) << 16) |
           (static_cast<ULONG>(value[2]) << 8) |
           static_cast<ULONG>(value[3]);
}

ULONGLONG ReadBigEndian64(_In_reads_(8) const UCHAR* value) noexcept {
    ULONGLONG result = 0;
    for (ULONG index = 0; index < 8; ++index) {
        result = (result << 8) | value[index];
    }
    return result;
}

void WriteBigEndian16(_Out_writes_(2) UCHAR* destination, USHORT value) noexcept {
    destination[0] = static_cast<UCHAR>(value >> 8);
    destination[1] = static_cast<UCHAR>(value);
}

void WriteBigEndian32(_Out_writes_(4) UCHAR* destination, ULONG value) noexcept {
    destination[0] = static_cast<UCHAR>(value >> 24);
    destination[1] = static_cast<UCHAR>(value >> 16);
    destination[2] = static_cast<UCHAR>(value >> 8);
    destination[3] = static_cast<UCHAR>(value);
}

void WriteBigEndian64(_Out_writes_(8) UCHAR* destination, ULONGLONG value) noexcept {
    for (LONG index = 7; index >= 0; --index) {
        destination[index] = static_cast<UCHAR>(value);
        value >>= 8;
    }
}

UCHAR SetSense(
    _Inout_ PSCSI_REQUEST_BLOCK srb,
    _In_ UCHAR senseKey,
    _In_ UCHAR additionalSenseCode,
    _In_ UCHAR additionalSenseQualifier = 0
) noexcept {
    srb->DataTransferLength = 0;
    srb->ScsiStatus = SCSISTAT_CHECK_CONDITION;

    if (srb->SenseInfoBuffer != nullptr && srb->SenseInfoBufferLength >= kSenseDataLength) {
        auto* sense = static_cast<PUCHAR>(srb->SenseInfoBuffer);
        RtlZeroMemory(sense, kSenseDataLength);
        sense[0] = SCSI_SENSE_ERRORCODE_FIXED_CURRENT;
        sense[2] = senseKey;
        sense[7] = 10;
        sense[12] = additionalSenseCode;
        sense[13] = additionalSenseQualifier;
        return SRB_STATUS_ERROR | SRB_STATUS_AUTOSENSE_VALID;
    }

    return SRB_STATUS_ERROR;
}

UCHAR CopyScsiResponse(
    _Inout_ PSCSI_REQUEST_BLOCK srb,
    _In_reads_bytes_(responseLength) const void* response,
    _In_ ULONG responseLength
) noexcept {
    if (responseLength != 0 && srb->DataBuffer == nullptr) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }

    const ULONG available = srb->DataTransferLength;
    const ULONG transferred = responseLength < available ? responseLength : available;
    if (transferred != 0) {
        RtlCopyMemory(srb->DataBuffer, response, transferred);
    }
    srb->DataTransferLength = transferred;
    return responseLength > available ? SRB_STATUS_DATA_OVERRUN : SRB_STATUS_SUCCESS;
}

bool AcquireLunForIo(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _In_ PSCSI_REQUEST_BLOCK srb,
    _Out_ WISPDISK_LUN_VIEW* view
) noexcept {
    RtlZeroMemory(view, sizeof(*view));
    if (srb->PathId != 0 || srb->TargetId != 0 || srb->Lun >= kMaximumDiskCount) {
        return false;
    }

    bool acquired = false;
    KIRQL oldIrql;
    KeAcquireSpinLock(&adapter->LunLock, &oldIrql);
    auto* lun = &adapter->Luns[srb->Lun];
    if (lun->State == WispDiskLunOnline && lun->BackingStore != nullptr) {
        if (lun->ActiveRequests++ == 0) {
            KeClearEvent(&lun->NoActiveRequestsEvent);
        }
        view->Lun = lun;
        view->BackingStore = lun->BackingStore;
        view->SizeBytes = lun->SizeBytes;
        view->MediaKind = lun->MediaKind;
        view->DeviceId = lun->DeviceId;
        acquired = true;
    }
    KeReleaseSpinLock(&adapter->LunLock, oldIrql);
    return acquired;
}

void ReleaseLunForIo(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _In_ PWISPDISK_LUN lun
) noexcept {
    KIRQL oldIrql;
    KeAcquireSpinLock(&adapter->LunLock, &oldIrql);
    NT_ASSERT(lun->ActiveRequests != 0);
    if (lun->ActiveRequests != 0 && --lun->ActiveRequests == 0) {
        KeSetEvent(&lun->NoActiveRequestsEvent, IO_NO_INCREMENT, FALSE);
    }
    KeReleaseSpinLock(&adapter->LunLock, oldIrql);
}

void WriteDeviceIdHex(_Out_writes_(8) UCHAR* destination, ULONG deviceId) noexcept {
    static constexpr UCHAR digits[] = "0123456789ABCDEF";
    for (ULONG index = 0; index < 8; ++index) {
        const ULONG shift = (7 - index) * 4;
        destination[index] = digits[(deviceId >> shift) & 0x0fU];
    }
}

UCHAR HandleInquiry(
    _In_ const WISPDISK_LUN_VIEW& view,
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
        response[1] = view.MediaKind == WispDiskMediaRemovable ? 0x80U : 0x00U;
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
        WriteDeviceIdHex(&response[10], view.DeviceId);
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
        WriteDeviceIdHex(&response[22], view.DeviceId);
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
    _In_ const WISPDISK_LUN_VIEW& view,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept {
    UCHAR response[8]{};
    const ULONGLONG blockCount = view.SizeBytes / kLogicalSectorSize;
    const ULONGLONG lastLba = blockCount - 1;
    const ULONG reportedLastLba = lastLba > MAXULONG ? MAXULONG : static_cast<ULONG>(lastLba);
    WriteBigEndian32(&response[0], reportedLastLba);
    WriteBigEndian32(&response[4], kLogicalSectorSize);
    return CopyScsiResponse(srb, response, sizeof(response));
}

UCHAR HandleReadCapacity16(
    _In_ const WISPDISK_LUN_VIEW& view,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept {
    if (srb->CdbLength < 16 || (srb->Cdb[1] & 0x1fU) != SERVICE_ACTION_READ_CAPACITY16) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }
    UCHAR response[32]{};
    WriteBigEndian64(&response[0], (view.SizeBytes / kLogicalSectorSize) - 1);
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

UCHAR HandleModeSense(_Inout_ PSCSI_REQUEST_BLOCK srb, bool tenByte) noexcept {
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

bool ParseReadWriteCdb(
    _In_ PSCSI_REQUEST_BLOCK srb,
    _Out_ ULONGLONG* logicalBlock,
    _Out_ ULONG* blockCount,
    _Out_ bool* isWrite
) noexcept {
    const UCHAR operation = srb->Cdb[0];
    *logicalBlock = 0;
    *blockCount = 0;
    *isWrite = operation == SCSIOP_WRITE6 || operation == SCSIOP_WRITE ||
               operation == SCSIOP_WRITE12 || operation == SCSIOP_WRITE16;

    switch (operation) {
        case SCSIOP_READ6:
        case SCSIOP_WRITE6:
            if (srb->CdbLength < 6) {
                return false;
            }
            *logicalBlock = (static_cast<ULONGLONG>(srb->Cdb[1] & 0x1fU) << 16) |
                            (static_cast<ULONGLONG>(srb->Cdb[2]) << 8) |
                            srb->Cdb[3];
            *blockCount = srb->Cdb[4] == 0 ? 256 : srb->Cdb[4];
            return true;

        case SCSIOP_READ:
        case SCSIOP_WRITE:
            if (srb->CdbLength < 10) {
                return false;
            }
            *logicalBlock = ReadBigEndian32(&srb->Cdb[2]);
            *blockCount = (static_cast<ULONG>(srb->Cdb[7]) << 8) | srb->Cdb[8];
            return true;

        case SCSIOP_READ12:
        case SCSIOP_WRITE12:
            if (srb->CdbLength < 12) {
                return false;
            }
            *logicalBlock = ReadBigEndian32(&srb->Cdb[2]);
            *blockCount = ReadBigEndian32(&srb->Cdb[6]);
            return true;

        case SCSIOP_READ16:
        case SCSIOP_WRITE16:
            if (srb->CdbLength < 16) {
                return false;
            }
            *logicalBlock = ReadBigEndian64(&srb->Cdb[2]);
            *blockCount = ReadBigEndian32(&srb->Cdb[10]);
            return true;

        default:
            return false;
    }
}

UCHAR HandleReadWrite(
    _In_ const WISPDISK_LUN_VIEW& view,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept {
    ULONGLONG logicalBlock = 0;
    ULONG blockCount = 0;
    bool isWrite = false;
    if (!ParseReadWriteCdb(srb, &logicalBlock, &blockCount, &isWrite)) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }

    if (blockCount == 0) {
        srb->DataTransferLength = 0;
        return SRB_STATUS_SUCCESS;
    }

    const ULONGLONG diskBlocks = view.SizeBytes / kLogicalSectorSize;
    if (logicalBlock >= diskBlocks || blockCount > diskBlocks - logicalBlock) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_ILLEGAL_BLOCK);
    }

    const ULONGLONG byteOffset = logicalBlock * kLogicalSectorSize;
    const ULONGLONG byteCount64 = static_cast<ULONGLONG>(blockCount) * kLogicalSectorSize;
    if (byteCount64 > kMaximumTransferLength || byteCount64 > MAXULONG ||
        srb->DataTransferLength < byteCount64 ||
        srb->DataBuffer == nullptr) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }

    const ULONG byteCount = static_cast<ULONG>(byteCount64);
    auto* requestBuffer = static_cast<PUCHAR>(srb->DataBuffer);
    auto* diskBuffer = view.BackingStore + static_cast<SIZE_T>(byteOffset);
    if (isWrite) {
        RtlCopyMemory(diskBuffer, requestBuffer, byteCount);
    } else {
        RtlCopyMemory(requestBuffer, diskBuffer, byteCount);
    }
    srb->DataTransferLength = byteCount;
    return SRB_STATUS_SUCCESS;
}

UCHAR HandleReportLuns(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept {
    if (srb->CdbLength < 12 || srb->PathId != 0 || srb->TargetId != 0) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }

    UCHAR response[8 + (WISPDISK_MAXIMUM_DISK_COUNT * 8)]{};
    ULONG count = 0;
    KIRQL oldIrql;
    KeAcquireSpinLock(&adapter->LunLock, &oldIrql);
    for (ULONG index = 0; index < kMaximumDiskCount; ++index) {
        if (adapter->Luns[index].State == WispDiskLunOnline) {
            response[8 + (count * 8) + 1] = adapter->Luns[index].Lun;
            ++count;
        }
    }
    KeReleaseSpinLock(&adapter->LunLock, oldIrql);

    WriteBigEndian32(&response[0], count * 8);
    ULONG responseLength = 8 + (count * 8);
    const ULONG allocationLength = ReadBigEndian32(&srb->Cdb[6]);
    if (responseLength > allocationLength) {
        responseLength = allocationLength;
    }
    return CopyScsiResponse(srb, response, responseLength);
}

} // namespace

UCHAR WispDiskHandleExecuteScsi(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept {
    if (srb->CdbLength == 0) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }

    if (srb->Cdb[0] == SCSIOP_REPORT_LUNS) {
        return HandleReportLuns(adapter, srb);
    }

    WISPDISK_LUN_VIEW view{};
    if (!AcquireLunForIo(adapter, srb, &view)) {
        srb->DataTransferLength = 0;
        return SRB_STATUS_NO_DEVICE;
    }

    UCHAR status;
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
            status = SRB_STATUS_SUCCESS;
            break;

        case SCSIOP_INQUIRY:
            status = HandleInquiry(view, srb);
            break;

        case SCSIOP_REQUEST_SENSE:
            status = HandleRequestSense(srb);
            break;

        case SCSIOP_READ_CAPACITY:
            status = HandleReadCapacity10(view, srb);
            break;

        case SCSIOP_READ_CAPACITY16:
            status = HandleReadCapacity16(view, srb);
            break;

        case SCSIOP_MODE_SENSE:
            status = HandleModeSense(srb, false);
            break;

        case SCSIOP_MODE_SENSE10:
            status = HandleModeSense(srb, true);
            break;

        case SCSIOP_READ6:
        case SCSIOP_WRITE6:
        case SCSIOP_READ:
        case SCSIOP_WRITE:
        case SCSIOP_READ12:
        case SCSIOP_WRITE12:
        case SCSIOP_READ16:
        case SCSIOP_WRITE16:
            status = HandleReadWrite(view, srb);
            break;

        default:
            status = SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_ILLEGAL_COMMAND);
            break;
    }

    ReleaseLunForIo(adapter, view.Lun);
    return status;
}
