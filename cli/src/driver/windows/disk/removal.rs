//! Resolve disk identities and coordinate WispDisk removal.

use anyhow::{Context, Result, bail};
use tracing::info;
use windows_sys::Win32::Foundation::GENERIC_READ;

use crate::args::DriveLetter;

use super::super::{
    DEVICE_WAIT,
    adapter::{Adapter, adapters},
    recovery::{UnresolvedOutcome, recovery_failed},
    volume::{Volume, drive_letter_in_use, get_volume_name, mount_point, wait_for_drive_state},
    win32::open_device,
};

use super::{
    deletion::DiskDeletion, disk_location, physical_disk_address, wait_for_physical_disk,
    wait_for_physical_disk_absent,
};

pub(in crate::driver::windows) fn delete_disk(letter: DriveLetter, force: bool) -> Result<()> {
    if !drive_letter_in_use(letter) {
        bail!("drive {letter} is not mounted");
    }
    info!(%letter, "Resolve disk identity");
    let _name = get_volume_name(&mount_point(letter)?)?;
    let volume = open_device(
        &format!(r"\\.\{}:", char::from(letter.as_ascii())),
        GENERIC_READ,
    )
    .context("open target volume for identity")?;
    let number = Volume::disk_number(&volume)?;
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

pub(in crate::driver::windows) fn delete_by_identity(
    instance: u64,
    id: u32,
    force: bool,
) -> Result<()> {
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

pub(super) fn delete_allocation(
    adapter: &Adapter,
    instance: u64,
    id: u32,
    force: bool,
) -> Result<()> {
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
