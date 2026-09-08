#pragma once

#include "scsi.h"

namespace wispdisk::scsi {

[[nodiscard]] UCHAR HandleReadWrite(
    _In_ const LunView& view,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept;

} // namespace wispdisk::scsi
