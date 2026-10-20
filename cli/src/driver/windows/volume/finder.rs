//! Own a Windows volume enumeration and yield its volume names.

use anyhow::{Context, Result};
use std::io;
use widestring::U16CString;
use windows_sys::Win32::{
    Foundation::{ERROR_NO_MORE_FILES, HANDLE, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{FindFirstVolumeW, FindNextVolumeW, FindVolumeClose},
};

use super::super::win32::{last_os_error, wide_from_buffer};

pub(super) struct VolumeFinder {
    handle: HANDLE,
    name: Vec<u16>,
    first: bool,
    finished: bool,
}

impl VolumeFinder {
    pub(super) fn new() -> Result<Self> {
        let mut name = vec![0u16; 1024];
        let handle = unsafe { FindFirstVolumeW(name.as_mut_ptr(), name.len() as u32) };
        if handle == INVALID_HANDLE_VALUE {
            return Err(last_os_error("enumerate volumes"));
        }
        Ok(Self {
            handle,
            name,
            first: true,
            finished: false,
        })
    }
}

impl Iterator for VolumeFinder {
    type Item = Result<U16CString>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        if self.first {
            // FindFirstVolumeW already populated the buffer. Yield that name
            // before advancing the enumeration with FindNextVolumeW.
            self.first = false;
        } else {
            self.name.fill(0);
            if unsafe {
                FindNextVolumeW(self.handle, self.name.as_mut_ptr(), self.name.len() as u32)
            } == 0
            {
                let error = io::Error::last_os_error();
                self.finished = true;
                return if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                    None
                } else {
                    Some(Err(error).context("enumerate next volume"))
                };
            }
        }
        let name = wide_from_buffer(&self.name).context("unterminated volume name");
        if name.is_err() {
            self.finished = true;
        }
        Some(name)
    }
}

impl Drop for VolumeFinder {
    fn drop(&mut self) {
        let _ = unsafe { FindVolumeClose(self.handle) };
    }
}
