//! Discover volume names and mount points without requiring write access.
use super::super::win32::{last_os_error, multisz_entries, open_device_wide};
use super::{Volume, finder::VolumeFinder, volume_device_path};
use anyhow::{Context, Result};
use std::{io, ptr::null_mut};
use widestring::{U16CStr, U16CString};
use windows_sys::Win32::{
    Foundation::ERROR_MORE_DATA, Storage::FileSystem::GetVolumePathNamesForVolumeNameW,
};

pub(in crate::driver::windows) struct VolumeInfo {
    pub(super) name: U16CString,
    pub(in crate::driver::windows) mount_points: Vec<U16CString>,
}

pub(in crate::driver::windows) fn volumes_for_disk(number: u32) -> Result<Vec<VolumeInfo>> {
    let mut volumes = Vec::new();
    for name in VolumeFinder::new()? {
        let name = name?;
        let device_path = volume_device_path(&name)?;
        let volume =
            open_device_wide(&device_path, 0).context("open volume for disk identity query")?;
        match Volume::disk_number(&volume) {
            Ok(actual) if actual == number => volumes.push(VolumeInfo {
                mount_points: volume_paths(&name)?,
                name,
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
    }
    Ok(volumes)
}

fn volume_paths(name: &U16CStr) -> Result<Vec<U16CString>> {
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
    multisz_entries(&paths)
        .map(|path| U16CString::from_ustr(path).context("encode volume mount point"))
        .collect()
}
