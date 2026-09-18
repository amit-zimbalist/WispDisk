//! Owns a disk-interface arrival subscription and its callback event.

use anyhow::{Result, bail};

use std::{
    ffi::c_void,
    mem::size_of,
    ptr::{null, null_mut},
    time::Duration,
};

use windows_sys::Win32::{
    Devices::DeviceAndDriverInstallation::{
        CM_NOTIFY_ACTION, CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL, CM_NOTIFY_EVENT_DATA,
        CM_NOTIFY_FILTER, CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE, CM_Register_Notification,
        CM_Unregister_Notification, CR_SUCCESS, HCMNOTIFICATION,
    },
    Foundation::{ERROR_SUCCESS, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::{
        Ioctl::GUID_DEVINTERFACE_DISK,
        Threading::{CreateEventW, SetEvent, WaitForSingleObject},
    },
};

use super::win32::{OwnedHandle, last_os_error};

pub(super) struct DiskInterfaceNotification {
    handle: HCMNOTIFICATION,
    event: OwnedHandle,
}

impl DiskInterfaceNotification {
    pub(super) fn new() -> Result<Self> {
        let event = unsafe { CreateEventW(null(), 0, 0, null()) };
        if event.is_null() {
            return Err(last_os_error("create disk-arrival notification event"));
        }
        let event = OwnedHandle(event);

        let mut filter = CM_NOTIFY_FILTER {
            cbSize: size_of::<CM_NOTIFY_FILTER>() as u32,
            FilterType: CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE,
            ..Default::default()
        };
        filter.u.DeviceInterface.ClassGuid = GUID_DEVINTERFACE_DISK;

        let mut handle = null_mut();
        let status = unsafe {
            CM_Register_Notification(
                &filter,
                event.0 as *const c_void,
                Some(disk_interface_notification),
                &mut handle,
            )
        };
        if status != CR_SUCCESS {
            bail!(
                "register for disk-interface arrival notifications: Configuration Manager error {status}"
            );
        }
        Ok(Self { handle, event })
    }

    pub(super) fn wait(&self, timeout: Duration) -> Result<()> {
        let milliseconds = timeout.as_millis().min(u128::from(u32::MAX)) as u32;
        match unsafe { WaitForSingleObject(self.event.0, milliseconds) } {
            WAIT_OBJECT_0 | WAIT_TIMEOUT => Ok(()),
            _ => Err(last_os_error("wait for disk-interface arrival")),
        }
    }
}

impl Drop for DiskInterfaceNotification {
    fn drop(&mut self) {
        // Unregister before the event field is dropped: in-flight callbacks
        // must finish while their event-handle context is still valid.
        if !self.handle.is_null() {
            let _ = unsafe { CM_Unregister_Notification(self.handle) };
        }
    }
}

unsafe extern "system" fn disk_interface_notification(
    _notification: HCMNOTIFICATION,
    context: *const c_void,
    action: CM_NOTIFY_ACTION,
    _event_data: *const CM_NOTIFY_EVENT_DATA,
    _event_data_size: u32,
) -> u32 {
    if action == CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL && !context.is_null() {
        let _ = unsafe { SetEvent(context as HANDLE) };
    }
    ERROR_SUCCESS
}
