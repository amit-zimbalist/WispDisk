//! Discover and prepare every volume belonging to one verified WispDisk.
use super::{
    disk::{ScsiLocation, verify_physical_disk_location},
    recovery::recovery_failed,
    volume::{restore_mount_and_unlock, volume_control, volume_disk_number},
    win32::{OwnedHandle, last_os_error, open_device_wide},
};
use anyhow::{Context, Result, bail};
use std::{io, ptr::null_mut};
use windows_sys::Win32::{
    Foundation::{
        ERROR_MORE_DATA, ERROR_NO_MORE_FILES, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
    },
    Storage::FileSystem::{
        DeleteVolumeMountPointW, FindFirstVolumeW, FindNextVolumeW, FindVolumeClose,
        GetVolumePathNamesForVolumeNameW, SetVolumeMountPointW,
    },
    System::Ioctl::{FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME},
};

pub(super) struct DiskVolume {
    name: Vec<u16>,
    pub mount_points: Vec<Vec<u16>>,
}

pub(super) fn volumes_for_disk(number: u32) -> Result<Vec<DiskVolume>> {
    let mut name = vec![0u16; 1024];
    let enumeration = unsafe { FindFirstVolumeW(name.as_mut_ptr(), name.len() as u32) };
    if enumeration == INVALID_HANDLE_VALUE {
        return Err(last_os_error("enumerate volumes"));
    }
    struct Enumeration(windows_sys::Win32::Foundation::HANDLE);
    impl Drop for Enumeration {
        fn drop(&mut self) {
            unsafe {
                FindVolumeClose(self.0);
            }
        }
    }
    let enumeration = Enumeration(enumeration);
    let mut volumes = Vec::new();
    loop {
        let length = name
            .iter()
            .position(|unit| *unit == 0)
            .context("unterminated volume name")?;
        let volume_name = name[..=length].to_vec();
        let mut device_path = name[..length].to_vec();
        if device_path.last() == Some(&(b'\\' as u16)) {
            device_path.pop();
        }
        device_path.push(0);
        let volume =
            open_device_wide(&device_path, 0).context("open volume for disk identity query")?;
        match volume_disk_number(&volume) {
            Ok(actual) if actual == number => volumes.push(DiskVolume {
                name: volume_name,
                mount_points: volume_paths(&name)?,
            }),
            Ok(_) => {}
            // Optical devices can have no media, and some volume types do not
            // implement disk extents. Other failures prevent safe deletion.
            Err(error)
                if error
                    .downcast_ref::<io::Error>()
                    .is_some_and(|error| matches!(error.raw_os_error(), Some(1 | 21))) => {}
            Err(error) => return Err(error),
        }
        name.fill(0);
        if unsafe { FindNextVolumeW(enumeration.0, name.as_mut_ptr(), name.len() as u32) } == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                break;
            }
            return Err(error).context("enumerate next volume");
        }
    }
    Ok(volumes)
}

fn volume_paths(name: &[u16]) -> Result<Vec<Vec<u16>>> {
    let mut required = 0;
    unsafe {
        GetVolumePathNamesForVolumeNameW(name.as_ptr(), null_mut(), 0, &mut required);
    }
    if required == 0 {
        return Err(last_os_error("query volume mount points"));
    }
    let mut paths = vec![0u16; required as usize];
    if unsafe {
        GetVolumePathNamesForVolumeNameW(name.as_ptr(), paths.as_mut_ptr(), required, &mut required)
    } == 0
    {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_MORE_DATA as i32) {
            return Err(error).context("read volume mount points");
        }
        paths.resize(required as usize, 0);
        if unsafe {
            GetVolumePathNamesForVolumeNameW(
                name.as_ptr(),
                paths.as_mut_ptr(),
                required,
                &mut required,
            )
        } == 0
        {
            return Err(last_os_error("reread volume mount points"));
        }
    }
    let mut result = Vec::new();
    for path in paths
        .split(|unit| *unit == 0)
        .take_while(|path| !path.is_empty())
    {
        let mut path = path.to_vec();
        path.push(0);
        result.push(path);
    }
    Ok(result)
}

struct DeletionVolume {
    volume: DiskVolume,
    handle: OwnedHandle,
    locked: bool,
}

pub(super) struct VolumeDeletion {
    volumes: Vec<DeletionVolume>,
    removed: Vec<(Vec<u16>, Vec<u16>)>,
}

impl VolumeDeletion {
    pub(super) fn open(number: u32, location: ScsiLocation) -> Result<Self> {
        let mut volumes = Vec::new();
        for volume in volumes_for_disk(number)? {
            let mut path = volume.name[..volume.name.len() - 1].to_vec();
            path.pop();
            path.push(0); // remove the GUID path's trailing backslash
            let handle = open_device_wide(&path, GENERIC_READ | GENERIC_WRITE)
                .context("open target volume")?;
            if volume_disk_number(&handle)? != number {
                bail!("volume changed disk identity before deletion");
            }
            volumes.push(DeletionVolume {
                volume,
                handle,
                locked: false,
            });
        }
        verify_physical_disk_location(number, location)?;
        Ok(Self {
            volumes,
            removed: Vec::new(),
        })
    }

    pub(super) fn lock(&mut self, force: bool) -> Result<()> {
        for target in &mut self.volumes {
            match volume_control(&target.handle, FSCTL_LOCK_VOLUME, "lock target volume") {
                Ok(()) => target.locked = true,
                Err(error) if force => {
                    let name = String::from_utf16_lossy(
                        &target.volume.name[..target.volume.name.len() - 1],
                    );
                    eprintln!(
                        "warning: could not lock volume {name}: {error:#}; /force will attempt to dismount it. Open files may become invalid and unsaved data may be lost."
                    );
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    pub(super) fn dismount(&self) -> Result<()> {
        for target in &self.volumes {
            volume_control(
                &target.handle,
                FSCTL_DISMOUNT_VOLUME,
                "dismount target volume",
            )?;
        }
        Ok(())
    }

    pub(super) fn remove_mounts(&mut self) -> Result<()> {
        for target in &self.volumes {
            for point in &target.volume.mount_points {
                if unsafe { DeleteVolumeMountPointW(point.as_ptr()) } == 0 {
                    return Err(last_os_error("remove volume mount point"));
                }
                self.removed
                    .push((point.clone(), target.volume.name.clone()));
            }
        }
        Ok(())
    }
    pub(super) fn restore(&self) -> Result<()> {
        let mut failure = None;
        for target in &self.volumes {
            let volume = &target.volume;
            let points: Vec<_> = self
                .removed
                .iter()
                .filter(|(_, name)| name == &volume.name)
                .collect();
            if points.is_empty() {
                // Closing the owned handle releases any acquired lock.
                continue;
            }
            let count = points.len();
            for (index, (point, _)) in points.into_iter().enumerate() {
                // A forced dismount may not own a lock. Restore all paths,
                // then release only a lock this deletion actually acquired.
                let restoration = if index + 1 == count && target.locked {
                    restore_mount_and_unlock(&target.handle, point, &volume.name)
                } else if unsafe { SetVolumeMountPointW(point.as_ptr(), volume.name.as_ptr()) } == 0
                {
                    Err(last_os_error("restore volume mount point"))
                } else {
                    Ok(())
                };
                if let Err(error) = restoration {
                    failure = Some(match failure {
                        Some(first) => {
                            recovery_failed(first, "another mount restoration failed", error)
                        }
                        None => error,
                    });
                }
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Threading::CreateEventW;

    fn non_volume_deletion() -> VolumeDeletion {
        // An event gives DeviceIoControl a valid, owned non-volume handle.
        // These tests cannot lock or dismount a real filesystem.
        let handle = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        assert!(!handle.is_null());
        VolumeDeletion {
            volumes: vec![DeletionVolume {
                volume: DiskVolume {
                    name: "test-volume\0".encode_utf16().collect(),
                    mount_points: Vec::new(),
                },
                handle: OwnedHandle(handle),
                locked: false,
            }],
            removed: Vec::new(),
        }
    }

    #[test]
    fn lock_failure_aborts_normal_deletion() {
        let mut deletion = non_volume_deletion();
        assert!(deletion.lock(false).is_err());
        assert!(!deletion.volumes[0].locked);
    }

    #[test]
    fn force_bypasses_lock_failure_but_not_dismount_failure() {
        let mut deletion = non_volume_deletion();
        deletion.lock(true).unwrap();
        assert!(!deletion.volumes[0].locked);
        let error = deletion.dismount().unwrap_err();
        assert!(error.downcast_ref::<io::Error>().is_some());
        assert!(format!("{error:#}").starts_with("dismount target volume:"));
    }

    #[test]
    fn restoration_only_attempts_to_release_an_acquired_lock() {
        let mut deletion = non_volume_deletion();
        // Neither the empty mount point nor the synthetic volume name can
        // identify a real mount. Restoration must fail without changing one.
        deletion
            .removed
            .push((vec![0], deletion.volumes[0].volume.name.clone()));
        let error = deletion.restore().unwrap_err();
        assert!(format!("{error:#}").starts_with("restore volume mount point:"));
        assert!(!format!("{error:#}").contains("unlock"));

        deletion.volumes[0].locked = true;
        let error = deletion.restore().unwrap_err();
        assert!(format!("{error:#}").contains("unlock also failed"));
    }
}
