#pragma once

#include "scsi.h"

namespace wispdisk::scsi {

constexpr ULONG kSenseDataLength = 18;

[[nodiscard]] ULONG ReadBigEndian32(_In_reads_(4) const UCHAR* value) noexcept;
[[nodiscard]] ULONGLONG ReadBigEndian64(_In_reads_(8) const UCHAR* value) noexcept;

void WriteBigEndian16(_Out_writes_(2) UCHAR* destination, _In_ USHORT value) noexcept;
void WriteBigEndian32(_Out_writes_(4) UCHAR* destination, _In_ ULONG value) noexcept;
void WriteBigEndian64(_Out_writes_(8) UCHAR* destination, _In_ ULONGLONG value) noexcept;
void WriteDeviceIdHex(_Out_writes_(8) UCHAR* destination, _In_ ULONG deviceId) noexcept;

[[nodiscard]] UCHAR SetSense(
    _Inout_ PSCSI_REQUEST_BLOCK srb,
    _In_ UCHAR senseKey,
    _In_ UCHAR additionalSenseCode,
    _In_ UCHAR additionalSenseQualifier = 0
) noexcept;

[[nodiscard]] UCHAR CopyScsiResponse(
    _Inout_ PSCSI_REQUEST_BLOCK srb,
    _In_reads_bytes_(responseLength) const void* response,
    _In_ ULONG responseLength
) noexcept;

} // namespace wispdisk::scsi
