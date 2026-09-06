#pragma once

#include "driver.h"

enum class WISPDISK_IO_CONTROL_DISPOSITION {
    Complete,
    Pending,
};

WISPDISK_IO_CONTROL_DISPOSITION WispDiskHandleIoControl(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept;

UCHAR WispDiskHandleExecuteScsi(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept;
