#include "scsi_response.h"

namespace wispdisk::scsi {

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

void WriteBigEndian16(_Out_writes_(2) UCHAR* destination, _In_ USHORT value) noexcept {
    destination[0] = static_cast<UCHAR>(value >> 8);
    destination[1] = static_cast<UCHAR>(value);
}

void WriteBigEndian32(_Out_writes_(4) UCHAR* destination, _In_ ULONG value) noexcept {
    destination[0] = static_cast<UCHAR>(value >> 24);
    destination[1] = static_cast<UCHAR>(value >> 16);
    destination[2] = static_cast<UCHAR>(value >> 8);
    destination[3] = static_cast<UCHAR>(value);
}

void WriteBigEndian64(_Out_writes_(8) UCHAR* destination, _In_ ULONGLONG value) noexcept {
    for (LONG index = 7; index >= 0; --index) {
        destination[index] = static_cast<UCHAR>(value);
        value >>= 8;
    }
}

void WriteDeviceIdHex(_Out_writes_(8) UCHAR* destination, _In_ ULONG deviceId) noexcept {
    static constexpr UCHAR digits[] = "0123456789ABCDEF";
    for (ULONG index = 0; index < 8; ++index) {
        const ULONG shift = (7 - index) * 4;
        destination[index] = digits[(deviceId >> shift) & 0x0fU];
    }
}

UCHAR SetSense(
    _Inout_ PSCSI_REQUEST_BLOCK srb,
    _In_ UCHAR senseKey,
    _In_ UCHAR additionalSenseCode,
    _In_ UCHAR additionalSenseQualifier
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

} // namespace wispdisk::scsi
