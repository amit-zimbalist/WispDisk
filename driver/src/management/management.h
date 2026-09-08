#pragma once

#include "driver.h"

enum class WispDiskIoControlDisposition {
    Complete,
    Pending,
};

[[nodiscard]] WispDiskIoControlDisposition WispDiskHandleIoControl(
    _In_ PWISPDISK_ADAPTER_EXTENSION adapter,
    _Inout_ PSCSI_REQUEST_BLOCK srb
) noexcept;

HW_WORKITEM WispDiskManagementWorker;
