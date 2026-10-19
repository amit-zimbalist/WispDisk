//! Discover volume names and mount points without requiring write access.
use super::{
    volume::volume_disk_number,
    win32::{last_os_error, open_device_wide},
};
use anyhow::{Context, Result};
use std::{io, ptr::null_mut};
use windows_sys::Win32::{
    Foundation::{ERROR_MORE_DATA, ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{
        FindFirstVolumeW, FindNextVolumeW, FindVolumeClose, GetVolumePathNamesForVolumeNameW,
    },
};

pub(super) struct VolumeInfo {
    pub(super) name: Vec<u16>,
    pub(super) mount_points: Vec<Vec<u16>>,
}

pub(super) fn volumes_for_disk(number: u32) -> Result<Vec<VolumeInfo>> {
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
            Ok(actual) if actual == number => volumes.push(VolumeInfo {
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
