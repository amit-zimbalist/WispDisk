#pragma once

#include "scsi.h"

namespace wispdisk::scsi {

[[nodiscard]] UCHAR HandleInquiry(
    _In_ const LunView& view,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept;

[[nodiscard]] UCHAR HandleReadCapacity10(
    _In_ const LunView& view,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept;

[[nodiscard]] UCHAR HandleReadCapacity16(
    _In_ const LunView& view,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept;

[[nodiscard]] UCHAR HandleRequestSense(_Inout_ PSCSI_REQUEST_BLOCK srb) noexcept;

[[nodiscard]] UCHAR HandleModeSense(
    _Inout_ PSCSI_REQUEST_BLOCK srb,
    _In_ bool tenByte
) noexcept;

[[nodiscard]] UCHAR HandleReportLuns(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept;

} // namespace wispdisk::scsi
