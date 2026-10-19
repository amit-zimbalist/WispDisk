//! Owned volume operations, drive-letter state, and volume identity.

use anyhow::{Context, Result, bail, ensure};

use std::{
    ffi::OsStr,
    fmt, io,
    mem::size_of,
    ptr::{null, null_mut},
    thread,
    time::{Duration, Instant},
};
use tracing::warn;

use windows_sys::Win32::{
    Foundation::{GENERIC_READ, GENERIC_WRITE},
    Storage::FileSystem::{
        DeleteVolumeMountPointW, GetLogicalDrives, GetVolumeNameForVolumeMountPointW,
        IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, SetVolumeMountPointW,
    },
    System::{
        IO::DeviceIoControl,
        Ioctl::{
            FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME, FSCTL_UNLOCK_VOLUME, VOLUME_DISK_EXTENTS,
        },
    },
};

use crate::args::DriveLetter;

use super::{
    RETRY_INTERVAL,
    disk::{ScsiLocation, verify_physical_disk_location},
    recovery::recovery_failed,
    volume_discovery::VolumeInfo,
    win32::{OwnedHandle, last_os_error, open_device, open_device_wide, wide},
};

pub(super) struct Volume {
    name: Vec<u16>,
    mount_points: Vec<Vec<u16>>,
    handle: OwnedHandle,
    locked: bool,
    removed_mount_points: Vec<Vec<u16>>,
}

impl Volume {
    pub(super) fn open(info: VolumeInfo, expected_disk: u32) -> Result<Self> {
        let mut path = info.name[..info.name.len() - 1].to_vec();
        path.pop();
        path.push(0); // remove the GUID path's trailing backslash
        let handle =
            open_device_wide(&path, GENERIC_READ | GENERIC_WRITE).context("open target volume")?;
        if volume_disk_number(&handle)? != expected_disk {
            bail!("volume changed disk identity before deletion");
        }
        Ok(Self {
            name: info.name,
            mount_points: info.mount_points,
            handle,
            locked: false,
            removed_mount_points: Vec::new(),
        })
    }

    pub(super) fn lock(&mut self, force: bool) -> Result<()> {
        match volume_control(&self.handle, FSCTL_LOCK_VOLUME, "lock target volume") {
            Ok(()) => self.locked = true,
            Err(error) if force => {
                warn!(
                    volume = %String::from_utf16_lossy(&self.name[..self.name.len() - 1]),
                    error = %format_args!("{error:#}"),
                    "Could not lock target volume; /force will attempt to dismount it. Open files may become invalid and unsaved data may be lost."
                );
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }

    pub(super) fn dismount(&self) -> Result<()> {
        volume_control(
            &self.handle,
            FSCTL_DISMOUNT_VOLUME,
            "dismount target volume",
        )
    }

    pub(super) fn remove_mounts(&mut self) -> Result<()> {
        for point in &self.mount_points {
            if unsafe { DeleteVolumeMountPointW(point.as_ptr()) } == 0 {
                return Err(last_os_error("remove volume mount point"));
            }
            self.removed_mount_points.push(point.clone());
        }
        Ok(())
    }

    pub(super) fn restore_mounts(&mut self) -> Result<()> {
        let mut failure = None;
        // Restore all removed paths before releasing an acquired lock. With
        // no removed paths, closing the owned handle releases any lock.
        for index in 0..self.removed_mount_points.len() {
            if let Err(error) = self.restore_removed_mount(index) {
                failure = Some(match failure {
                    Some(first) => {
                        recovery_failed(first, "another mount restoration failed", error)
                    }
                    None => error,
                });
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn restore_removed_mount(&mut self, index: usize) -> Result<()> {
        // A forced dismount may not own a lock. Only release our own lock,
        // once, after the last mount restoration attempt.
        let release_lock = index + 1 == self.removed_mount_points.len() && self.locked;
        let mount_error = if unsafe {
            SetVolumeMountPointW(
                self.removed_mount_points[index].as_ptr(),
                self.name.as_ptr(),
            )
        } == 0
        {
            Some(last_os_error(if release_lock {
                "restore drive-letter mount point"
            } else {
                "restore volume mount point"
            }))
        } else {
            None
        };
        let unlock_error = if release_lock {
            self.unlock().err()
        } else {
            None
        };
        match (mount_error, unlock_error) {
            (None, None) => Ok(()),
            (Some(error), None) | (None, Some(error)) => Err(error),
            (Some(mount), Some(unlock)) => {
                Err(recovery_failed(mount, "unlock also failed", unlock))
            }
        }
    }

    fn unlock(&mut self) -> Result<()> {
        volume_control(&self.handle, FSCTL_UNLOCK_VOLUME, "unlock restored volume")?;
        self.locked = false;
        Ok(())
    }
}

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

fn volume_control(volume: &OwnedHandle, code: u32, operation: &str) -> Result<()> {
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
        let error = io::Error::last_os_error();
        if code == FSCTL_LOCK_VOLUME {
            Err(anyhow::Error::new(error).context(VolumeLockFailure))
        } else {
            Err(anyhow::Error::new(error).context(operation.to_owned()))
        }
    } else {
        Ok(())
    }
}

#[derive(Debug)]
struct VolumeLockFailure;
impl fmt::Display for VolumeLockFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("lock target volume")
    }
}
pub(super) fn is_lock_refusal(error: &anyhow::Error) -> bool {
    error.downcast_ref::<VolumeLockFailure>().is_some()
        && error
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.raw_os_error() == Some(5))
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

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Threading::CreateEventW;

    fn non_volume() -> Volume {
        // An event gives DeviceIoControl a valid, owned non-volume handle.
        // These tests cannot lock or dismount a real filesystem.
        let handle = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        assert!(!handle.is_null());
        Volume {
            name: "test-volume\0".encode_utf16().collect(),
            mount_points: Vec::new(),
            handle: OwnedHandle(handle),
            locked: false,
            removed_mount_points: Vec::new(),
        }
    }

    #[test]
    fn lock_failure_aborts_normal_deletion() {
        let mut volume = non_volume();
        assert!(volume.lock(false).is_err());
        assert!(!volume.locked);
    }

    #[test]
    fn force_bypasses_lock_failure_but_not_dismount_failure() {
        let mut volume = non_volume();
        volume.lock(true).unwrap();
        assert!(!volume.locked);
        let error = volume.dismount().unwrap_err();
        assert!(error.downcast_ref::<io::Error>().is_some());
        assert!(format!("{error:#}").starts_with("dismount target volume:"));
    }

    #[test]
    fn restoration_only_attempts_to_release_an_acquired_lock() {
        let mut volume = non_volume();
        // Neither the empty mount point nor the synthetic volume name can
        // identify a real mount. Restoration must fail without changing one.
        volume.removed_mount_points.push(vec![0]);
        let error = volume.restore_mounts().unwrap_err();
        assert!(format!("{error:#}").starts_with("restore volume mount point:"));
        assert!(!format!("{error:#}").contains("unlock"));

        volume.locked = true;
        let error = volume.restore_mounts().unwrap_err();
        assert!(format!("{error:#}").contains("unlock also failed"));
        assert!(volume.locked);
    }

    #[test]
    fn only_access_denied_during_locking_is_a_retryable_lock_refusal() {
        let lock = anyhow::Error::new(io::Error::from_raw_os_error(5)).context(VolumeLockFailure);
        assert!(is_lock_refusal(&lock));
        let opening =
            anyhow::Error::new(io::Error::from_raw_os_error(5)).context("open target volume");
        assert!(!is_lock_refusal(&opening));
        let transport =
            anyhow::Error::new(io::Error::from_raw_os_error(1117)).context(VolumeLockFailure);
        assert!(!is_lock_refusal(&transport));
    }
}
