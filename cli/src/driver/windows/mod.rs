//! Windows add/delete orchestration. Platform details live in focused sibling modules.

mod adapter;
mod disk;
mod disk_interface_notification;
mod install;
mod recovery;
mod storage;
mod volume;
mod win32;

use anyhow::{Context, Result, bail};

use std::time::Duration;

use windows_sys::Win32::{
    Foundation::{GENERIC_READ, GENERIC_WRITE},
    Storage::FileSystem::DeleteVolumeMountPointW,
    System::Ioctl::{FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME, FSCTL_UNLOCK_VOLUME},
};

use crate::{
    args::{Command, DriveLetter, MediaKind},
    protocol::{CAP_CREATE_DELETE, CAP_READ_WRITE, CreateResponse, DiskInfo},
    timing,
};

use self::{
    adapter::{Adapter, adapters, wait_for_adapter},
    disk::{
        ScsiLocation, physical_disk_address, verify_physical_disk_location, wait_for_physical_disk,
        wait_for_physical_disk_absent,
    },
    install::install_embedded_driver,
    recovery::recovery_failed,
    storage::initialize_and_format,
    volume::{
        drive_letter_in_use, ensure_drive_letter_available, get_volume_name, mount_point,
        restore_mount_and_unlock, verify_mounted_drive, volume_control, volume_disk_number,
        wait_for_drive_state,
    },
    win32::{last_os_error, open_device},
};

const DEVICE_WAIT: Duration = Duration::from_secs(30);
const RETRY_INTERVAL: Duration = Duration::from_millis(200);

pub(super) fn execute(command: &Command) -> Result<()> {
    match command {
        Command::Add {
            letter,
            media,
            size_bytes,
        } => add_disk(*letter, *media, *size_bytes),
        Command::Delete { letter } => delete_disk(*letter),
    }
}

fn add_disk(letter: DriveLetter, media: MediaKind, size_bytes: u64) -> Result<()> {
    ensure_drive_letter_available(letter)?;
    timing::mark("validate drive letter");
    let adapter = ensure_adapter()?;
    timing::mark("find or install adapter");
    let version = adapter.query_version()?;
    timing::mark("query driver capabilities");
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

    let created = adapter.create_disk(letter, media, size_bytes)?;
    timing::mark("allocate and zero backing store");

    let result: Result<()> = (|| {
        let location = expected_location(&adapter, &created);
        let disk_number = wait_for_physical_disk(location, DEVICE_WAIT)?;
        timing::mark("wait for disk interface");
        verify_physical_disk_location(disk_number, location)?;
        timing::mark("verify disk identity");
        initialize_and_format(
            disk_number,
            letter,
            media,
            size_bytes,
            created.device_id,
            location,
        )?;
        wait_for_drive_state(letter, true, DEVICE_WAIT)?;
        timing::mark("wait for drive letter");
        verify_mounted_drive(letter, disk_number, location)?;
        timing::mark("verify mounted drive");
        Ok(())
    })();

    if let Err(error) = result {
        let rollback = adapter.delete_by_id(created.device_id);
        return match rollback {
            Ok(()) => Err(error.context(
                "failed to initialize the new disk; the driver allocation was rolled back",
            )),
            Err(rollback_error) => Err(recovery_failed(
                error,
                "failed to initialize the new disk; driver rollback also failed",
                rollback_error,
            )),
        };
    }

    println!(
        "created {} WispDisk {} ({size_bytes} bytes, device id {})",
        media, letter, created.device_id
    );
    Ok(())
}

fn delete_disk(letter: DriveLetter) -> Result<()> {
    let mount_point = mount_point(letter);
    if !drive_letter_in_use(letter) {
        bail!("drive {letter} is not mounted");
    }

    let volume_name = get_volume_name(&mount_point)?;
    let volume = open_device(
        &format!(r"\\.\{}:", char::from(letter.as_ascii())),
        GENERIC_READ | GENERIC_WRITE,
    )
    .context("open target volume")?;
    let disk_number = volume_disk_number(&volume)?;
    let address = physical_disk_address(disk_number)?;
    let location = address;

    let mut matched = None;
    for adapter in adapters() {
        if adapter.port_number != address.port {
            continue;
        }
        let matches: Vec<DiskInfo> = adapter
            .list_disks()?
            .into_iter()
            .filter(|disk| {
                disk.path_id == address.path
                    && disk.target_id == address.target
                    && disk.lun == address.lun
            })
            .collect();
        if matches.len() > 1 || (matches.len() == 1 && matched.is_some()) {
            bail!("refusing to delete {letter}: multiple WispDisk LUNs claim its SCSI address");
        }
        if let Some(disk) = matches.first() {
            matched = Some((adapter, *disk));
        }
    }
    let Some((adapter, disk)) = matched else {
        bail!(
            "refusing to delete {letter}: its PhysicalDrive{disk_number} address is not uniquely owned by WispDisk"
        );
    };

    volume_control(&volume, FSCTL_LOCK_VOLUME, "lock target volume")?;
    if let Err(error) = volume_control(&volume, FSCTL_DISMOUNT_VOLUME, "dismount target volume") {
        let _ = volume_control(&volume, FSCTL_UNLOCK_VOLUME, "unlock target volume");
        return Err(error);
    }
    if unsafe { DeleteVolumeMountPointW(mount_point.as_ptr()) } == 0 {
        let error = last_os_error("remove drive-letter mount point");
        let _ = volume_control(&volume, FSCTL_UNLOCK_VOLUME, "unlock target volume");
        return Err(error);
    }

    if let Err(error) = adapter.delete_by_id(disk.device_id) {
        let restore_error = restore_mount_and_unlock(&volume, &mount_point, &volume_name).err();
        return match restore_error {
            Some(restore) => Err(recovery_failed(
                error,
                format!("driver rejected deletion; restoring {letter} also failed"),
                restore,
            )),
            None => Err(error.context(format!("driver rejected deletion; {letter} was restored"))),
        };
    }

    drop(volume);
    wait_for_drive_state(letter, false, DEVICE_WAIT)?;
    wait_for_physical_disk_absent(disk_number, location, DEVICE_WAIT)?;
    println!("deleted WispDisk {letter} (device id {})", disk.device_id);
    Ok(())
}

fn expected_location(adapter: &Adapter, created: &CreateResponse) -> ScsiLocation {
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

    install_embedded_driver()?;
    wait_for_adapter(DEVICE_WAIT)
}
