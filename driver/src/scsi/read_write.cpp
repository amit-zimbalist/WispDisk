#include "read_write.h"

#include "scsi_response.h"

namespace wispdisk::scsi {

static bool ParseReadWriteCdb(
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
    _In_ const LunView& view,
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

    const ULONGLONG diskBlocks = view.sizeBytes / kLogicalSectorSize;
    if (logicalBlock >= diskBlocks || blockCount > diskBlocks - logicalBlock) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_ILLEGAL_BLOCK);
    }

    const ULONGLONG byteOffset = logicalBlock * kLogicalSectorSize;
    const ULONGLONG byteCount64 = static_cast<ULONGLONG>(blockCount) * kLogicalSectorSize;
    if (byteCount64 > kMaximumTransferLength || byteCount64 > MAXULONG ||
        srb->DataTransferLength < byteCount64 || srb->DataBuffer == nullptr) {
        return SetSense(srb, SCSI_SENSE_ILLEGAL_REQUEST, SCSI_ADSENSE_INVALID_CDB);
    }

    const ULONG byteCount = static_cast<ULONG>(byteCount64);
    auto* requestBuffer = static_cast<PUCHAR>(srb->DataBuffer);
    auto* diskBuffer = view.backingStore + static_cast<SIZE_T>(byteOffset);
    if (isWrite) {
        RtlCopyMemory(diskBuffer, requestBuffer, byteCount);
    } else {
        RtlCopyMemory(requestBuffer, diskBuffer, byteCount);
    }
    srb->DataTransferLength = byteCount;
    return SRB_STATUS_SUCCESS;
}

} // namespace wispdisk::scsi
