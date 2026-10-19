//! Windows add/delete orchestration. Platform details live in focused sibling modules.

mod adapter;
mod control_operation;
mod disk;
mod disk_deletion;
mod disk_interface_notification;
mod install;
mod inventory;
mod recovery;
mod storage;
mod volume;
mod volume_discovery;
mod volume_finder;
mod win32;

use anyhow::{Context, Result, bail};

use std::time::Duration;
use tracing::{info, warn};

use windows_sys::Win32::Foundation::GENERIC_READ;

use crate::{
    args::{Command, DriveLetter, FileSystem, MediaKind},
    protocol::{CAP_CREATE_DELETE, CAP_READ_WRITE, DiskState},
};

use self::{
    adapter::{Adapter, CreatedDisk, adapters, wait_for_adapter},
    disk::{
        ScsiLocation, physical_disk_address, verify_physical_disk_location, wait_for_physical_disk,
        wait_for_physical_disk_absent,
    },
    disk_deletion::DiskDeletion,
    install::install_embedded_driver,
    inventory::list_inventory,
    recovery::{UnresolvedOutcome, recovery_failed},
    storage::initialize_and_format,
    volume::{
        drive_letter_in_use, ensure_drive_letter_available, get_volume_name, mount_point,
        verify_mounted_drive, volume_disk_number, wait_for_drive_state,
    },
    win32::open_device,
};

const DEVICE_WAIT: Duration = Duration::from_secs(30);
const RETRY_INTERVAL: Duration = Duration::from_millis(200);

pub(super) fn is_unresolved(error: &anyhow::Error) -> bool {
    recovery::is_unresolved(error)
}

pub(super) fn execute(command: &Command) -> Result<()> {
    match command {
        Command::Add {
            letter,
            media,
            size_bytes,
            file_system,
        } => add_disk(*letter, *media, *size_bytes, *file_system),
        Command::Delete { letter, force } => delete_disk(*letter, *force),
        Command::DeleteId { adapter, id, force } => delete_by_identity(*adapter, *id, *force),
        Command::List { json } => list_inventory(*json),
    }
}

fn add_disk(
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

    let result: Result<()> = (|| {
        ensure_drive_letter_available(letter)?;
        let location = expected_location(&adapter, &created);
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
    })();

    if let Err(error) = result {
        warn!(
            adapter_instance = created.adapter_instance,
            device_id = created.device_id,
            error = %format_args!("{error:#}"),
            "Disk setup failed; roll back allocation"
        );
        let deadline = std::time::Instant::now() + DEVICE_WAIT;
        let rollback = loop {
            match delete_allocation(&adapter, created.adapter_instance, created.device_id, false) {
                Err(error)
                    if volume::is_lock_refusal(&error) && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(RETRY_INTERVAL);
                }
                result => break result,
            }
        };
        return match rollback {
            Ok(()) => Err(error.context(
                "failed to initialize the new disk; the driver allocation was rolled back",
            )),
            Err(rollback_error) => Err(recovery_failed(
                error,
                format!(
                    "failed to initialize device {} on adapter instance {}; driver rollback also failed",
                    created.device_id, created.adapter_instance
                ),
                rollback_error,
            )),
        };
    }

    println!(
        "created {} WispDisk {} ({size_bytes} bytes, {file_system}, device id {})",
        media, letter, created.device_id
    );
    Ok(())
}

fn delete_disk(letter: DriveLetter, force: bool) -> Result<()> {
    if !drive_letter_in_use(letter) {
        bail!("drive {letter} is not mounted");
    }
    info!(%letter, "Resolve disk identity");
    let _name = get_volume_name(&mount_point(letter))?;
    let volume = open_device(
        &format!(r"\\.\{}:", char::from(letter.as_ascii())),
        GENERIC_READ,
    )
    .context("open target volume for identity")?;
    let number = volume_disk_number(&volume)?;
    let address = physical_disk_address(number)?;
    drop(volume);
    let mut matched = None;
    for adapter in adapters() {
        if adapter.port_number != address.port {
            continue;
        }
        adapter.require_recovery()?;
        let state = adapter.state()?;
        for entry in &state.disks[..state.disk_count as usize] {
            if entry.disk.path_id == address.path
                && entry.disk.target_id == address.target
                && entry.disk.lun == address.lun
            {
                if matched.is_some() {
                    bail!("multiple WispDisk LUNs claim drive {letter}");
                }
                matched = Some((
                    adapter.port_number,
                    state.adapter_instance,
                    entry.disk.device_id,
                ));
            }
        }
    }
    let (_, instance, id) = matched.context("volume is not uniquely owned by WispDisk")?;
    delete_by_identity(instance, id, force)?;
    info!(%letter, "Wait for drive-letter disappearance");
    wait_for_drive_state(letter, false, DEVICE_WAIT)?;
    Ok(())
}

fn delete_by_identity(instance: u64, id: u32, force: bool) -> Result<()> {
    let mut matched = None;
    for adapter in adapters() {
        adapter.require_recovery()?;
        if adapter.state()?.adapter_instance == instance {
            if matched.is_some() {
                bail!("multiple adapters claim instance {instance}");
            }
            matched = Some(adapter);
        }
    }
    let adapter =
        matched.context("adapter instance is absent or stale; refresh /list before deletion")?;
    delete_allocation(&adapter, instance, id, force)?;
    println!("deleted WispDisk device {id} (adapter instance {instance})");
    Ok(())
}

fn delete_allocation(adapter: &Adapter, instance: u64, id: u32, force: bool) -> Result<()> {
    info!(
        adapter_instance = instance,
        device_id = id,
        "Resolve deletion identity"
    );
    let state = adapter.state()?;
    if state.adapter_instance != instance {
        bail!("adapter instance changed before deletion");
    }
    let entry = state.disks[..state.disk_count as usize]
        .iter()
        .find(|entry| entry.disk.device_id == id)
        .context("device ID is absent; refresh /list")?;
    if entry.state != 1 {
        bail!("device {id} is already stopping; query inventory until removal completes");
    }
    let location = disk_location(adapter, entry);
    let number = wait_for_physical_disk(location, DEVICE_WAIT)?;
    info!(disk_number = number, "Open target volumes");
    let mut deletion = DiskDeletion::open(number, location)?;
    let prepared = (|| {
        info!(disk_number = number, "Lock target volumes");
        deletion.lock(force)?;
        info!(disk_number = number, "Dismount target volumes");
        deletion.dismount()?;
        info!(disk_number = number, "Remove volume mount points");
        deletion.remove_mounts()
    })();
    if let Err(error) = prepared {
        info!(disk_number = number, "Restore volume mount points");
        return match deletion.restore() {
            Ok(()) => Err(error),
            Err(restore) => Err(recovery_failed(
                error,
                "volume restoration also failed",
                restore,
            )),
        };
    }
    if let Err(error) = adapter.delete_by_id(instance, id) {
        // A transport failure is not a rejection. Restore only after positively
        // establishing that the same allocation remains online.
        let still_online = error.downcast_ref::<UnresolvedOutcome>().is_none()
            && adapter.state().ok().is_some_and(|state| {
                state.adapter_instance == instance
                    && state.disks[..state.disk_count as usize]
                        .iter()
                        .any(|entry| entry.disk.device_id == id && entry.state == 1)
            });
        if still_online {
            info!(disk_number = number, "Restore volume mount points");
            return match deletion.restore() {
                Ok(()) => Err(error.context("deletion rejected; original mount points restored")),
                Err(restore) => Err(recovery_failed(
                    error,
                    "mount restoration also failed",
                    restore,
                )),
            };
        }
        return Err(error.context(format!(
            "device {id} removal is not confirmed; inspect /list /json before retrying"
        )));
    }
    drop(deletion);
    info!(
        device_id = id,
        disk_number = number,
        "Wait for device disappearance"
    );
    wait_for_physical_disk_absent(number, location, DEVICE_WAIT).with_context(|| format!("driver allocation for device {id} was removed, but Windows disappearance was not confirmed"))?;
    Ok(())
}

fn disk_location(adapter: &Adapter, entry: &DiskState) -> ScsiLocation {
    ScsiLocation {
        port: adapter.port_number,
        path: entry.disk.path_id,
        target: entry.disk.target_id,
        lun: entry.disk.lun,
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
