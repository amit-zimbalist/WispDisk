//! Embedded package extraction and root-enumerated adapter installation.

use std::{
    ffi::OsStr,
    fs, io,
    mem::size_of,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
};

use anyhow::{Context, Result, ensure};
use windows_sys::Win32::{
    Devices::DeviceAndDriverInstallation::{
        DICD_GENERATE_ID, DIF_REGISTERDEVICE, DIF_REMOVE, DIGCF_PRESENT, DIIRFLAG_FORCE_INF,
        DiInstallDriverW, GUID_DEVCLASS_SCSIADAPTER, INSTALLFLAG_FORCE, INSTALLFLAG_NONINTERACTIVE,
        SP_DEVINFO_DATA, SPDRP_HARDWAREID, SetupDiCallClassInstaller, SetupDiCreateDeviceInfoW,
        SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceRegistryPropertyW,
        SetupDiSetDeviceRegistryPropertyW, UpdateDriverForPlugAndPlayDevicesW,
    },
    Foundation::{ERROR_INVALID_DATA, ERROR_NO_MORE_ITEMS},
};

use crate::payload;

use super::{
    recovery::recovery_failed,
    win32::{DeviceInfoSet, last_os_error, wide},
};

const ROOT_HARDWARE_ID: &str = r"ROOT\WISPDISK";

pub(super) fn install_embedded_driver() -> Result<()> {
    DriverPackage::extract_embedded()?.install()
}

struct DriverPackage {
    inf: PathBuf,
}

impl DriverPackage {
    fn extract_embedded() -> Result<Self> {
        let program_data = std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        let directory = program_data
            .join("WispDisk")
            .join("Driver")
            .join(env!("CARGO_PKG_VERSION"));
        fs::create_dir_all(&directory).with_context(|| {
            format!("create driver-package directory at {}", directory.display())
        })?;

        write_package_file(&directory.join("WispDisk.sys"), payload::DRIVER_SYS)?;
        write_package_file(&directory.join("WispDisk.inf"), payload::DRIVER_INF)?;
        write_package_file(&directory.join("WispDisk.cat"), payload::DRIVER_CAT)?;
        let inf = directory.join("WispDisk.inf");
        let inf = inf
            .canonicalize()
            .with_context(|| format!("resolve extracted INF at {}", inf.display()))?;
        Ok(Self { inf })
    }

    fn install(&self) -> Result<()> {
        let inf = wide(self.inf.as_os_str());
        let mut reboot_required = 0;
        if unsafe {
            DiInstallDriverW(
                null_mut(),
                inf.as_ptr(),
                DIIRFLAG_FORCE_INF,
                &mut reboot_required,
            )
        } == 0
        {
            return Err(last_os_error(
                "stage driver package (is this console elevated, and is the package signed?)",
            ));
        }

        let created_device = CreatedDevice::create_if_missing()?;
        let hardware_id = wide(OsStr::new(ROOT_HARDWARE_ID));
        let update_result = unsafe {
            UpdateDriverForPlugAndPlayDevicesW(
                null_mut(),
                hardware_id.as_ptr(),
                inf.as_ptr(),
                INSTALLFLAG_FORCE | INSTALLFLAG_NONINTERACTIVE,
                &mut reboot_required,
            )
        };
        if update_result == 0 {
            // Capture the bind error before rollback changes the thread's last error.
            let error = last_os_error("bind and start WispDisk driver");
            if let Some(device) = created_device {
                if let Err(recovery) = device.remove() {
                    return Err(recovery_failed(
                        error,
                        "removing the newly created root device also failed",
                        recovery,
                    ));
                }
            }
            return Err(error);
        }
        ensure!(
            reboot_required == 0,
            "Windows installed the driver package but requires a reboot before it can start"
        );
        Ok(())
    }
}

struct CreatedDevice {
    set: DeviceInfoSet,
    data: SP_DEVINFO_DATA,
}

impl CreatedDevice {
    fn create_if_missing() -> Result<Option<Self>> {
        if Self::exists()? {
            return Ok(None);
        }

        let set = DeviceInfoSet::new(&GUID_DEVCLASS_SCSIADAPTER)?;
        let class_name = wide(OsStr::new("SCSIAdapter"));
        let mut data = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        if unsafe {
            SetupDiCreateDeviceInfoW(
                set.0,
                class_name.as_ptr(),
                &GUID_DEVCLASS_SCSIADAPTER,
                null(),
                null_mut(),
                DICD_GENERATE_ID,
                &mut data,
            )
        } == 0
        {
            return Err(last_os_error("create WispDisk root device"));
        }

        let mut hardware_id = wide(OsStr::new(ROOT_HARDWARE_ID));
        hardware_id.push(0);
        if unsafe {
            SetupDiSetDeviceRegistryPropertyW(
                set.0,
                &mut data,
                SPDRP_HARDWAREID,
                hardware_id.as_ptr().cast(),
                (hardware_id.len() * size_of::<u16>()) as u32,
            )
        } == 0
        {
            return Err(last_os_error("set WispDisk root-device hardware id"));
        }
        if unsafe { SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set.0, &data) } == 0 {
            return Err(last_os_error("register WispDisk root device"));
        }

        Ok(Some(Self { set, data }))
    }

    fn exists() -> Result<bool> {
        let raw = unsafe {
            SetupDiGetClassDevsW(
                &GUID_DEVCLASS_SCSIADAPTER,
                null(),
                null_mut(),
                DIGCF_PRESENT,
            )
        };
        if raw == -1_isize {
            return Err(last_os_error("enumerate SCSI adapters"));
        }
        let set = DeviceInfoSet(raw);
        let mut index = 0;
        loop {
            let mut data = SP_DEVINFO_DATA {
                cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
                ..Default::default()
            };
            if unsafe { SetupDiEnumDeviceInfo(set.0, index, &mut data) } == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_NO_MORE_ITEMS as i32) {
                    return Ok(false);
                }
                return Err(error).context("enumerate SCSI adapter");
            }
            if device_has_hardware_id(&set, &data, ROOT_HARDWARE_ID)? {
                return Ok(true);
            }
            index += 1;
        }
    }

    // Removal is explicit rollback, never an automatic Drop action on success.
    fn remove(&self) -> Result<()> {
        if unsafe { SetupDiCallClassInstaller(DIF_REMOVE, self.set.0, &self.data) } == 0 {
            return Err(last_os_error("remove newly created WispDisk root device"));
        }
        Ok(())
    }
}

fn write_package_file(path: &Path, contents: &[u8]) -> Result<()> {
    ensure!(
        !contents.is_empty(),
        "embedded package file {} is empty",
        path.display()
    );
    fs::write(path, contents)
        .with_context(|| format!("extract driver package at {}", path.display()))
}

fn device_has_hardware_id(
    set: &DeviceInfoSet,
    data: &SP_DEVINFO_DATA,
    expected: &str,
) -> Result<bool> {
    let mut property_type = 0;
    let mut required = 0;
    let mut buffer = [0_u16; 512];
    let result = unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            set.0,
            data,
            SPDRP_HARDWAREID,
            &mut property_type,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * size_of::<u16>()) as u32,
            &mut required,
        )
    };
    if result == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_INVALID_DATA as i32) {
            return Ok(false);
        }
        return Err(error).context("read adapter hardware id");
    }
    let units = (required as usize / size_of::<u16>()).min(buffer.len());
    Ok(multisz(&buffer[..units]).any(|value| value.eq_ignore_ascii_case(expected)))
}

fn multisz(buffer: &[u16]) -> impl Iterator<Item = String> + '_ {
    buffer
        .split(|unit| *unit == 0)
        .take_while(|entry| !entry.is_empty())
        .map(String::from_utf16_lossy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_windows_multisz() {
        let values = [b'A' as u16, 0, b'B' as u16, b'C' as u16, 0, 0];
        assert_eq!(multisz(&values).collect::<Vec<_>>(), ["A", "BC"]);
    }

    #[test]
    fn rejects_empty_package_files_before_writing() {
        let error = write_package_file(Path::new("unused.sys"), &[]).unwrap_err();
        assert_eq!(
            error.to_string(),
            "embedded package file unused.sys is empty"
        );
    }

    #[test]
    fn package_write_failure_retains_io_error_and_path_context() {
        // An embedded NUL is rejected without creating or overwriting any file.
        let error = write_package_file(Path::new("invalid\0.sys"), &[1]).unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(format!("{error:#}").contains("extract driver package at invalid"));
    }
}
