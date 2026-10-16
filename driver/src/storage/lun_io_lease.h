#pragma once

#include "driver.h"

namespace wispdisk::storage {

class LunStore;

// This view is usable only while its I/O lease is alive.
struct LunView {
    PUCHAR backingStore{};
    ULONGLONG sizeBytes{};
    ULONG mediaKind{};
    ULONG deviceId{};
};

class LunIoLease final {
public:
    ~LunIoLease() noexcept;
    LunIoLease(const LunIoLease&) = delete;
    LunIoLease& operator=(const LunIoLease&) = delete;
    LunIoLease(LunIoLease&&) = delete;
    LunIoLease& operator=(LunIoLease&&) = delete;

    [[nodiscard]] explicit operator bool() const noexcept;
    [[nodiscard]] const LunView& view() const noexcept;

private:
    friend class LunStore;
    LunIoLease(WISPDISK_ADAPTER_EXTENSION& extension, const SCSI_REQUEST_BLOCK& srb) noexcept;

    WISPDISK_ADAPTER_EXTENSION& extension_;
    LunView view_{};
    PWISPDISK_LUN lun_;
};

} // namespace wispdisk::storage
