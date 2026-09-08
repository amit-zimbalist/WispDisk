#pragma once

#include "driver.h"

namespace wispdisk::scsi {

struct LunView {
    PWISPDISK_LUN lun{};
    PUCHAR backingStore{};
    ULONGLONG sizeBytes{};
    ULONG mediaKind{};
    ULONG deviceId{};
};

} // namespace wispdisk::scsi

[[nodiscard]] UCHAR WispDiskHandleExecuteScsi(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept;
