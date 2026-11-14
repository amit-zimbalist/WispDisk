//! Create, initialize, and recover a new WispDisk allocation.

use anyhow::{Result, bail};
use tracing::info;

use crate::{
    args::{DriveLetter, FileSystem, MediaKind},
    protocol::{CAP_CREATE_DELETE, CAP_READ_WRITE},
};

use super::super::{
    DEVICE_WAIT,
    adapter::{Adapter, CreatedDisk, adapters, install_embedded_driver, wait_for_adapter},
    volume::{ensure_drive_letter_available, verify_mounted_drive, wait_for_drive_state},
};

use super::{
    ScsiLocation,
    provisioning::{ProvisioningTarget, initialize_and_format},
    provisioning_guard::ProvisioningGuard,
    verify_physical_disk_location, wait_for_physical_disk,
};

pub(in crate::driver::windows) fn add_disk(
    letter: DriveLetter,
    media: MediaKind,
    size_bytes: u64,
    file_system: FileSystem,
) -> Result<()> {
    info!(%letter, "Validate drive letter");
    ensure_drive_letter_available(letter)?;
    info!("Find or install adapter");
    let adapter = ensure_adapter()?;
    info!(port = adapter.port_number, "Query driver capabilities");
    let version = adapter.query_version()?;
    let required = CAP_CREATE_DELETE | CAP_READ_WRITE;
    if version.capabilities & required != required {
        bail!(
            "loaded driver lacks required capabilities (reported 0x{:08X})",
            version.capabilities
        );
    }
    if media == MediaKind::Removable && version.capabilities & crate::protocol::CAP_REMOVABLE == 0 {
        bail!("loaded driver does not support removable-media disks");
    }

    adapter.require_recovery()?;
    adapter.require_provisioning_guard()?;
    let guard = ProvisioningGuard::create(&adapter, letter, media, size_bytes)?;
    let created = guard.created();
    info!(
        adapter_instance = created.adapter_instance,
        device_id = created.device_id,
        size_bytes,
        "Disk allocation created"
    );

    if let Err(error) =
        initialize_created_disk(&adapter, created, letter, media, size_bytes, file_system)
    {
        return Err(guard.provisioning_failed(error));
    }

    let device_id = created.device_id;
    guard.finish()?;

    println!(
        "created {} WispDisk {} ({size_bytes} bytes, {file_system}, device id {})",
        media, letter, device_id
    );
    Ok(())
}

fn initialize_created_disk(
    adapter: &Adapter,
    created: &CreatedDisk,
    letter: DriveLetter,
    media: MediaKind,
    size_bytes: u64,
    file_system: FileSystem,
) -> Result<()> {
    ensure_drive_letter_available(letter)?;
    let location = expected_location(adapter, created);
    info!(device_id = created.device_id, "Wait for disk interface");
    let disk_number = wait_for_physical_disk(location, DEVICE_WAIT)?;
    info!(disk_number, "Verify disk identity");
    verify_physical_disk_location(disk_number, location)?;
    initialize_and_format(
        &ProvisioningTarget {
            adapter,
            created,
            disk_number,
            location,
        },
        letter,
        media,
        size_bytes,
        file_system,
    )?;
    info!(%letter, "Wait for drive letter");
    wait_for_drive_state(letter, true, DEVICE_WAIT)?;
    info!(%letter, disk_number, "Verify mounted drive");
    verify_mounted_drive(letter, disk_number, location)?;
    Ok(())
}

fn expected_location(adapter: &Adapter, created: &CreatedDisk) -> ScsiLocation {
    ScsiLocation {
        port: adapter.port_number,
        path: created.path_id,
        target: created.target_id,
        lun: created.lun,
    }
}

fn ensure_adapter() -> Result<Adapter> {
    if let Some(adapter) = adapters().next() {
        return Ok(adapter);
    }

    info!("Install embedded driver");
    install_embedded_driver()?;
    info!("Wait for adapter");
    wait_for_adapter(DEVICE_WAIT)
}
