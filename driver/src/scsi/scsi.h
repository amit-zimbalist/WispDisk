#pragma once

#include "storage/lun_io_lease.h"

namespace wispdisk::scsi {

using storage::LunView;

} // namespace wispdisk::scsi

[[nodiscard]] UCHAR WispDiskHandleExecuteScsi(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _Inout_ PSCSI_REQUEST_BLOCK srb) noexcept;
