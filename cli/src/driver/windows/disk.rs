//! Physical disk discovery and SCSI identity verification.

use anyhow::{Context, Result, bail, ensure};

use std::{
    io,
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
    thread,
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    Devices::DeviceAndDriverInstallation::{
        DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, SP_DEVICE_INTERFACE_DATA,
        SP_DEVICE_INTERFACE_DETAIL_DATA_W, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
        SetupDiGetDeviceInterfaceDetailW,
    },
    Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_ITEMS},
    Storage::{
        FileSystem::FILE_DEVICE_DISK,
        IscsiDisc::{IOCTL_SCSI_GET_ADDRESS, SCSI_ADDRESS},
    },
    System::{
        IO::DeviceIoControl,
        Ioctl::{GUID_DEVINTERFACE_DISK, IOCTL_STORAGE_GET_DEVICE_NUMBER, STORAGE_DEVICE_NUMBER},
    },
};

use super::{
    RETRY_INTERVAL,
    disk_interface_notification::DiskInterfaceNotification,
    win32::{DeviceInfoSet, OwnedHandle, last_os_error, open_device, open_device_wide},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ScsiLocation {
    pub(super) port: u8,
    pub(super) path: u8,
    pub(super) target: u8,
    pub(super) lun: u8,
}
pub(super) fn wait_for_physical_disk(location: ScsiLocation, timeout: Duration) -> Result<u32> {
    // Register before the first enumeration so an arrival cannot be lost between
    // checking existing interfaces and beginning the wait.
    let notification = DiskInterfaceNotification::new()?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(number) = find_physical_disk(location)? {
            return Ok(number);
        }
        let now = Instant::now();
        if now >= deadline {
            bail!(
                "new disk at SCSI {}:{}:{}:{} did not appear within 30 seconds",
                location.port,
                location.path,
                location.target,
                location.lun
            );
        }
        // The short fallback timeout covers a device interface that has arrived
        // but cannot quite be opened yet; arrivals otherwise wake this wait early.
        notification.wait((deadline - now).min(RETRY_INTERVAL))?;
    }
}

fn find_physical_disk(expected: ScsiLocation) -> Result<Option<u32>> {
    let raw = unsafe {
        SetupDiGetClassDevsW(
            &GUID_DEVINTERFACE_DISK,
            null(),
            null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    };
    if raw == -1_isize {
        return Err(last_os_error("enumerate present disk interfaces"));
    }
    let set = DeviceInfoSet(raw);
    let mut matched = None;
    let mut index = 0;
    loop {
        let mut interface = SP_DEVICE_INTERFACE_DATA {
            cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
            ..Default::default()
        };
        if unsafe {
            SetupDiEnumDeviceInterfaces(
                set.0,
                null(),
                &GUID_DEVINTERFACE_DISK,
                index,
                &mut interface,
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_NO_MORE_ITEMS as i32) {
                return Ok(matched);
            }
            return Err(error).context("enumerate disk interface");
        }
        index += 1;

        let Ok(path) = disk_interface_path(&set, &interface) else {
            continue;
        };
        let Ok(disk) = open_device_wide(&path, 0) else {
            continue;
        };
        let Ok(actual) = scsi_location(&disk) else {
            continue;
        };
        if actual != expected {
            continue;
        }
        let Ok(number) = storage_device_number(&disk) else {
            continue;
        };
        if let Some(previous) = matched {
            if previous != number {
                bail!(
                    "multiple physical disks reported SCSI {}:{}:{}:{}",
                    expected.port,
                    expected.path,
                    expected.target,
                    expected.lun
                );
            }
        } else {
            matched = Some(number);
        }
    }
}

fn disk_interface_path(
    set: &DeviceInfoSet,
    interface: &SP_DEVICE_INTERFACE_DATA,
) -> Result<Vec<u16>> {
    let mut required = 0;
    let first_result = unsafe {
        SetupDiGetDeviceInterfaceDetailW(set.0, interface, null_mut(), 0, &mut required, null_mut())
    };
    if first_result == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
            return Err(error).context("query disk-interface path size");
        }
    }
    ensure!(
        required >= size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32,
        "SetupAPI returned an invalid disk-interface path size"
    );

    let required_size = required as usize;
    let word_count = required_size.div_ceil(size_of::<usize>());
    let mut storage = vec![0_usize; word_count];
    let detail = storage
        .as_mut_ptr()
        .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
    unsafe {
        (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
    }
    let storage_bytes = storage.len() * size_of::<usize>();
    if unsafe {
        SetupDiGetDeviceInterfaceDetailW(
            set.0,
            interface,
            detail,
            storage_bytes as u32,
            &mut required,
            null_mut(),
        )
    } == 0
    {
        return Err(last_os_error("read disk-interface path"));
    }

    let path = unsafe { std::ptr::addr_of!((*detail).DevicePath).cast::<u16>() };
    let path_offset = path as usize - detail as usize;
    let maximum_units = (storage_bytes - path_offset) / size_of::<u16>();
    let units = unsafe { std::slice::from_raw_parts(path, maximum_units) };
    let Some(terminator) = units.iter().position(|unit| *unit == 0) else {
        bail!("SetupAPI returned an unterminated disk-interface path");
    };
    Ok(units[..=terminator].to_vec())
}

fn storage_device_number(device: &OwnedHandle) -> Result<u32> {
    let mut number = STORAGE_DEVICE_NUMBER::default();
    let mut returned = 0;
    let result = unsafe {
        DeviceIoControl(
            device.0,
            IOCTL_STORAGE_GET_DEVICE_NUMBER,
            null(),
            0,
            (&mut number as *mut STORAGE_DEVICE_NUMBER).cast(),
            size_of::<STORAGE_DEVICE_NUMBER>() as u32,
            &mut returned,
            null_mut(),
        )
    };
    if result == 0 {
        return Err(last_os_error("query disk-interface device number"));
    }
    ensure!(
        returned >= size_of::<STORAGE_DEVICE_NUMBER>() as u32,
        "disk interface returned a truncated device number ({returned} bytes)"
    );
    if number.DeviceType != FILE_DEVICE_DISK {
        bail!(
            "disk interface reported unexpected device type {}",
            number.DeviceType
        );
    }
    Ok(number.DeviceNumber)
}

pub(super) fn wait_for_physical_disk_absent(
    number: u32,
    location: ScsiLocation,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if physical_disk_address(number).ok() != Some(location) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "deleted disk at SCSI {}:{}:{}:{} remained present for 30 seconds",
                location.port,
                location.path,
                location.target,
                location.lun
            );
        }
        thread::sleep(RETRY_INTERVAL);
    }
}

pub(super) fn physical_disk_address(number: u32) -> Result<ScsiLocation> {
    let disk = open_device(&format!(r"\\.\PhysicalDrive{number}"), 0)
        .with_context(|| format!("open PhysicalDrive{number}"))?;
    scsi_location(&disk).with_context(|| format!("query PhysicalDrive{number} SCSI address"))
}

fn scsi_location(device: &OwnedHandle) -> Result<ScsiLocation> {
    let mut address: SCSI_ADDRESS = unsafe { zeroed() };
    address.Length = size_of::<SCSI_ADDRESS>() as u32;
    let mut returned = 0;
    let result = unsafe {
        DeviceIoControl(
            device.0,
            IOCTL_SCSI_GET_ADDRESS,
            null(),
            0,
            (&mut address as *mut SCSI_ADDRESS).cast(),
            size_of::<SCSI_ADDRESS>() as u32,
            &mut returned,
            null_mut(),
        )
    };
    if result == 0 {
        return Err(last_os_error("query SCSI address"));
    }
    ensure!(
        returned >= size_of::<SCSI_ADDRESS>() as u32,
        "disk returned a truncated SCSI address ({returned} bytes)"
    );
    Ok(ScsiLocation {
        port: address.PortNumber,
        path: address.PathId,
        target: address.TargetId,
        lun: address.Lun,
    })
}

pub(super) fn verify_physical_disk_location(number: u32, expected: ScsiLocation) -> Result<()> {
    let actual = physical_disk_address(number)?;
    if actual != expected {
        bail!(
            "PhysicalDrive{number} changed identity before initialization; refusing to format it"
        );
    }
    Ok(())
}
