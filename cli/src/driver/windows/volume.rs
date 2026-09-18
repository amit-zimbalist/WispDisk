//! Drive-letter state, volume identity, locking, and mount rollback.

use anyhow::{Context, Result, bail, ensure};

use std::{
    ffi::OsStr,
    mem::size_of,
    ptr::{null, null_mut},
    thread,
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    Foundation::GENERIC_READ,
    Storage::FileSystem::{
        GetLogicalDrives, GetVolumeNameForVolumeMountPointW, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
        SetVolumeMountPointW,
    },
    System::{
        IO::DeviceIoControl,
        Ioctl::{FSCTL_UNLOCK_VOLUME, VOLUME_DISK_EXTENTS},
    },
};

use crate::args::DriveLetter;

use super::{
    RETRY_INTERVAL,
    disk::{ScsiLocation, verify_physical_disk_location},
    recovery::recovery_failed,
    win32::{OwnedHandle, last_os_error, open_device, wide},
};

pub(super) fn verify_mounted_drive(
    letter: DriveLetter,
    expected_disk_number: u32,
    expected_location: ScsiLocation,
) -> Result<()> {
    let volume = open_device(
        &format!(r"\\.\{}:", char::from(letter.as_ascii())),
        GENERIC_READ,
    )
    .context("open newly formatted volume")?;
    let actual_disk_number = volume_disk_number(&volume)?;
    if actual_disk_number != expected_disk_number {
        bail!(
            "new mount {letter} resolved to PhysicalDrive{actual_disk_number}, not the created PhysicalDrive{expected_disk_number}"
        );
    }
    verify_physical_disk_location(actual_disk_number, expected_location)
}

pub(super) fn get_volume_name(mount_point: &[u16]) -> Result<Vec<u16>> {
    let mut volume_name = vec![0_u16; 128];
    if unsafe {
        GetVolumeNameForVolumeMountPointW(
            mount_point.as_ptr(),
            volume_name.as_mut_ptr(),
            volume_name.len() as u32,
        )
    } == 0
    {
        return Err(last_os_error("resolve drive letter to a volume"));
    }
    Ok(volume_name)
}

pub(super) fn volume_disk_number(volume: &OwnedHandle) -> Result<u32> {
    let mut storage = [0_u64; 128];
    let mut returned = 0;
    let result = unsafe {
        DeviceIoControl(
            volume.0,
            IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
            null(),
            0,
            storage.as_mut_ptr().cast(),
            size_of_val(&storage) as u32,
            &mut returned,
            null_mut(),
        )
    };
    if result == 0 {
        return Err(last_os_error("resolve volume to a physical disk"));
    }
    ensure!(
        returned >= size_of::<VOLUME_DISK_EXTENTS>() as u32,
        "volume returned truncated disk extents ({returned} bytes)"
    );
    let extents = unsafe {
        storage
            .as_ptr()
            .cast::<VOLUME_DISK_EXTENTS>()
            .read_unaligned()
    };
    if extents.NumberOfDiskExtents != 1 {
        bail!(
            "refusing to delete a volume spanning {} physical disks",
            extents.NumberOfDiskExtents
        );
    }
    Ok(extents.Extents[0].DiskNumber)
}

pub(super) fn volume_control(volume: &OwnedHandle, code: u32, operation: &str) -> Result<()> {
    let mut returned = 0;
    if unsafe {
        DeviceIoControl(
            volume.0,
            code,
            null(),
            0,
            null_mut(),
            0,
            &mut returned,
            null_mut(),
        )
    } == 0
    {
        Err(last_os_error(operation))
    } else {
        Ok(())
    }
}

pub(super) fn restore_mount_and_unlock(
    volume: &OwnedHandle,
    mount_point: &[u16],
    volume_name: &[u16],
) -> Result<()> {
    let mount_result = unsafe { SetVolumeMountPointW(mount_point.as_ptr(), volume_name.as_ptr()) };
    let mount_error = if mount_result == 0 {
        Some(last_os_error("restore drive-letter mount point"))
    } else {
        None
    };
    let unlock_error = volume_control(volume, FSCTL_UNLOCK_VOLUME, "unlock restored volume").err();
    match (mount_error, unlock_error) {
        (None, None) => Ok(()),
        (Some(error), None) | (None, Some(error)) => Err(error),
        (Some(mount), Some(unlock)) => Err(recovery_failed(mount, "unlock also failed", unlock)),
    }
}

pub(super) fn ensure_drive_letter_available(letter: DriveLetter) -> Result<()> {
    ensure!(
        !drive_letter_in_use(letter),
        "drive letter {letter} is already in use"
    );
    Ok(())
}

pub(super) fn drive_letter_in_use(letter: DriveLetter) -> bool {
    let index = u32::from(letter.as_ascii() - b'A');
    (unsafe { GetLogicalDrives() } & (1_u32 << index)) != 0
}

pub(super) fn wait_for_drive_state(
    letter: DriveLetter,
    expected_present: bool,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if drive_letter_in_use(letter) == expected_present {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "drive {letter} did not {} within 30 seconds",
                if expected_present {
                    "appear"
                } else {
                    "disappear"
                }
            );
        }
        thread::sleep(RETRY_INTERVAL);
    }
}

pub(super) fn mount_point(letter: DriveLetter) -> Vec<u16> {
    wide(OsStr::new(&format!("{}:\\", char::from(letter.as_ascii()))))
}
