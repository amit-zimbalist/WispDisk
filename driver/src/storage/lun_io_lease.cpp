#include "lun_io_lease.h"
#include "lun_store.h"

namespace wispdisk::storage {

LunIoLease::LunIoLease(
    WISPDISK_ADAPTER_EXTENSION& extension,
    const SCSI_REQUEST_BLOCK& srb) noexcept
    : extension_{extension}, lun_{LunStore{extension}.AcquireLunIo(srb, view_)} {}

LunIoLease::~LunIoLease() noexcept {
    if (lun_ != nullptr) {
        LunStore{extension_}.ReleaseLunIo(*lun_);
    }
}

LunIoLease::operator bool() const noexcept {
    return lun_ != nullptr;
}
const LunView& LunIoLease::view() const noexcept {
    return view_;
}

} // namespace wispdisk::storage
