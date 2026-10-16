#pragma once

#include "driver.h"
#include "management/management_controller.h"
#include "storage/lun_store.h"

namespace wispdisk::driver {

// Non-owning lifecycle and dispatch coordinator for StorPort's plain storage.
class AdapterContext final {
public:
    explicit AdapterContext(WISPDISK_ADAPTER_EXTENSION& extension) noexcept;

    [[nodiscard]] management::ManagementController Management() const noexcept;
    [[nodiscard]] storage::LunStore Disks() const noexcept;
    [[nodiscard]] bool IsInitialized() const noexcept;
    _IRQL_requires_(PASSIVE_LEVEL)
    [[nodiscard]] bool Initialize() noexcept;
    [[nodiscard]] bool SetStopping(bool stopping) noexcept;
    _IRQL_requires_max_(DISPATCH_LEVEL)
    [[nodiscard]] BOOLEAN StartIo(PSCSI_REQUEST_BLOCK srb) const noexcept;
    _IRQL_requires_(PASSIVE_LEVEL)
    void ReleaseResources() noexcept;

private:
    void CompleteRequest(PSCSI_REQUEST_BLOCK srb, UCHAR status) const noexcept;
    WISPDISK_ADAPTER_EXTENSION& extension_;
};

} // namespace wispdisk::driver
