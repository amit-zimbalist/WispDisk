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
        GetVolumePathNamesForVolumeNameW,
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

pub(super) struct VolumeDeletion {
    volumes: Vec<(DiskVolume, OwnedHandle)>,
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
            volumes.push((volume, handle));
        }
        verify_physical_disk_location(number, location)?;
        Ok(Self {
            volumes,
            removed: Vec::new(),
        })
    }

    pub(super) fn lock(&self) -> Result<()> {
        for (_, handle) in &self.volumes {
            volume_control(handle, FSCTL_LOCK_VOLUME, "lock target volume")?;
        }
        Ok(())
    }
    pub(super) fn dismount(&self) -> Result<()> {
        for (_, handle) in &self.volumes {
            volume_control(handle, FSCTL_DISMOUNT_VOLUME, "dismount target volume")?;
        }
        Ok(())
    }
    pub(super) fn remove_mounts(&mut self) -> Result<()> {
        for (volume, _) in &self.volumes {
            for point in &volume.mount_points {
                if unsafe { DeleteVolumeMountPointW(point.as_ptr()) } == 0 {
                    return Err(last_os_error("remove volume mount point"));
                }
                self.removed.push((point.clone(), volume.name.clone()));
            }
        }
        Ok(())
    }
    pub(super) fn restore(&self) -> Result<()> {
        let mut failure = None;
        for (volume, handle) in &self.volumes {
            let points: Vec<_> = self
                .removed
                .iter()
                .filter(|(_, name)| name == &volume.name)
                .collect();
            if points.is_empty() {
                // Closing the owned handle releases any acquired lock.
                continue;
            }
            for (point, _) in points {
                if let Err(error) = restore_mount_and_unlock(handle, point, &volume.name) {
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
