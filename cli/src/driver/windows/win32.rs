//! Shared Win32 handle ownership, device opening, encoding, and error context.

use anyhow::{Error, Result};

use std::{
    ffi::OsStr,
    io,
    os::windows::ffi::OsStrExt,
    ptr::{null, null_mut},
};

use windows_sys::{
    Win32::{
        Devices::DeviceAndDriverInstallation::{
            HDEVINFO, SetupDiCreateDeviceInfoList, SetupDiDestroyDeviceInfoList,
        },
        Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        },
    },
    core::GUID,
};

pub(super) struct DeviceInfoSet(pub(super) HDEVINFO);

impl DeviceInfoSet {
    pub(super) fn new(class_guid: &GUID) -> Result<Self> {
        let handle = unsafe { SetupDiCreateDeviceInfoList(class_guid, null_mut()) };
        if handle == -1_isize {
            Err(last_os_error("create device-information set"))
        } else {
            Ok(Self(handle))
        }
    }
}

impl Drop for DeviceInfoSet {
    fn drop(&mut self) {
        let _ = unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

pub(super) struct OwnedHandle(pub(super) HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

pub(super) fn open_device(path: &str, access: u32) -> io::Result<OwnedHandle> {
    let path = wide(OsStr::new(path));
    open_device_wide(&path, access)
}

pub(super) fn open_device_wide(path: &[u16], access: u32) -> io::Result<OwnedHandle> {
    if path.last() != Some(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "device path is not null terminated",
        ));
    }
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        Ok(OwnedHandle(handle))
    }
}

pub(super) fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

pub(super) fn last_os_error(operation: &str) -> Error {
    Error::new(io::Error::last_os_error()).context(operation.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unterminated_device_paths_before_calling_windows() {
        for path in [&[][..], &[b'A' as u16][..]] {
            let error = open_device_wide(path, 0).err().expect("invalid path");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn encodes_null_terminated_utf16() {
        assert_eq!(wide(OsStr::new("A😀")), [65, 0xD83D, 0xDE00, 0]);
    }

    #[test]
    fn contextual_os_error_preserves_raw_win32_code() {
        unsafe { windows_sys::Win32::Foundation::SetLastError(5) };
        let error = last_os_error("open target volume");
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().raw_os_error(),
            Some(5)
        );
        let message = format!("{error:#}");
        assert!(message.contains("open target volume"));
        assert!(message.contains("os error 5"));
    }
}
