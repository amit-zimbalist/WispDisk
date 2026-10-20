//! Create, initialize, and recover a new WispDisk allocation.

use anyhow::{Result, bail};
use tracing::{info, warn};

use crate::{
    args::{DriveLetter, FileSystem, MediaKind},
    protocol::{CAP_CREATE_DELETE, CAP_READ_WRITE},
};

use super::super::{
    DEVICE_WAIT, RETRY_INTERVAL,
    adapter::{Adapter, CreatedDisk, adapters, install_embedded_driver, wait_for_adapter},
    recovery::recovery_failed,
    volume::{self, ensure_drive_letter_available, verify_mounted_drive, wait_for_drive_state},
};

use super::{
    ScsiLocation, provisioning::initialize_and_format, removal::delete_allocation,
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
    let created = adapter.create_disk(letter, media, size_bytes)?;
    info!(
        adapter_instance = created.adapter_instance,
        device_id = created.device_id,
        size_bytes,
        "Disk allocation created"
    );

    if let Err(error) =
        initialize_created_disk(&adapter, &created, letter, media, size_bytes, file_system)
    {
        return Err(rollback_created_disk(&adapter, &created, error));
    }

    println!(
        "created {} WispDisk {} ({size_bytes} bytes, {file_system}, device id {})",
        media, letter, created.device_id
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
        disk_number,
        letter,
        media,
        size_bytes,
        created.device_id,
        location,
        file_system,
    )?;
    info!(%letter, "Wait for drive letter");
    wait_for_drive_state(letter, true, DEVICE_WAIT)?;
    info!(%letter, disk_number, "Verify mounted drive");
    verify_mounted_drive(letter, disk_number, location)?;
    Ok(())
}

fn rollback_created_disk(
    adapter: &Adapter,
    created: &CreatedDisk,
    error: anyhow::Error,
) -> anyhow::Error {
    warn!(
        adapter_instance = created.adapter_instance,
        device_id = created.device_id,
        error = %format_args!("{error:#}"),
        "Disk setup failed; roll back allocation"
    );
    let deadline = std::time::Instant::now() + DEVICE_WAIT;
    let rollback = loop {
        match delete_allocation(adapter, created.adapter_instance, created.device_id, false) {
            Err(error)
                if volume::is_lock_refusal(&error) && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(RETRY_INTERVAL);
            }
            result => break result,
        }
    };
    match rollback {
        Ok(()) => error
            .context("failed to initialize the new disk; the driver allocation was rolled back"),
        Err(rollback_error) => recovery_failed(
            error,
            format!(
                "failed to initialize device {} on adapter instance {}; driver rollback also failed",
                created.device_id, created.adapter_instance
            ),
            rollback_error,
        ),
    }
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
