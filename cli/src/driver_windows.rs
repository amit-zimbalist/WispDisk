use std::{
    ffi::{OsStr, c_void},
    fs, io,
    mem::{size_of, zeroed},
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use windows_sys::Win32::{
    Devices::DeviceAndDriverInstallation::{
        DICD_GENERATE_ID, DIF_REGISTERDEVICE, DIF_REMOVE, DIGCF_PRESENT, DIIRFLAG_FORCE_INF,
        DiInstallDriverW, GUID_DEVCLASS_SCSIADAPTER, HDEVINFO, INSTALLFLAG_FORCE,
        INSTALLFLAG_NONINTERACTIVE, SP_DEVINFO_DATA, SPDRP_HARDWAREID, SetupDiCallClassInstaller,
        SetupDiCreateDeviceInfoList, SetupDiCreateDeviceInfoW, SetupDiDestroyDeviceInfoList,
        SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceRegistryPropertyW,
        SetupDiSetDeviceRegistryPropertyW, UpdateDriverForPlugAndPlayDevicesW,
    },
    Foundation::{
        CloseHandle, ERROR_NO_MORE_ITEMS, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    },
    Storage::{
        FileSystem::{
            CreateFileW, DeleteVolumeMountPointW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ,
            FILE_SHARE_WRITE, GetLogicalDrives, GetVolumeNameForVolumeMountPointW,
            IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, OPEN_EXISTING, SetVolumeMountPointW,
        },
        IscsiDisc::{IOCTL_SCSI_GET_ADDRESS, IOCTL_SCSI_MINIPORT, SCSI_ADDRESS, SRB_IO_CONTROL},
    },
    System::{
        IO::DeviceIoControl,
        Ioctl::{
            FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME, FSCTL_UNLOCK_VOLUME, VOLUME_DISK_EXTENTS,
        },
    },
};
use wmi::WMIConnection;

use crate::{
    args::{Command, DriveLetter, MediaKind},
    payload,
    protocol::{
        CAP_CREATE_DELETE, CAP_READ_WRITE, CreateRequest, CreateResponse, DeleteRequest, DiskInfo,
        ListResponse, MAXIMUM_DISK_COUNT, Operation, PROTOCOL_VERSION, ProtocolMediaKind,
        QueryVersionResponse, RequestHeader, ResponseHeader, SRB_SIGNATURE,
    },
};

const ROOT_HARDWARE_ID: &str = r"ROOT\WISPDISK";
const CONTROL_BUFFER_SIZE: usize = 1024;
const CONTROL_TIMEOUT_SECONDS: u32 = 30;
const DEVICE_WAIT: Duration = Duration::from_secs(30);
const RETRY_INTERVAL: Duration = Duration::from_millis(200);
const SCSI_PORT_LIMIT: u32 = 256;
const PHYSICAL_DISK_LIMIT: u32 = 256;
const LOGICAL_SECTOR_SIZE: u32 = 512;
const PARTITION_STYLE_RAW: u16 = 0;
const PARTITION_STYLE_MBR: u16 = 1;

static NEXT_CORRELATION_ID: AtomicU64 = AtomicU64::new(1);

pub(super) fn execute(command: &Command) -> Result<(), String> {
    match command {
        Command::Add {
            letter,
            media,
            size_bytes,
        } => add_disk(*letter, *media, *size_bytes),
        Command::Delete { letter } => delete_disk(*letter),
    }
}

fn add_disk(letter: DriveLetter, media: MediaKind, size_bytes: u64) -> Result<(), String> {
    ensure_drive_letter_available(letter)?;
    let adapter = ensure_adapter()?;
    let version = adapter.query_version()?;
    let required = CAP_CREATE_DELETE | CAP_READ_WRITE;
    if version.capabilities & required != required {
        return Err(format!(
            "loaded driver lacks required capabilities (reported 0x{:08X})",
            version.capabilities
        ));
    }
    if media == MediaKind::Removable && version.capabilities & crate::protocol::CAP_REMOVABLE == 0 {
        return Err("loaded driver does not support removable-media disks".into());
    }

    let request = CreateRequest {
        header: request_header::<CreateRequest>(Operation::CreateDisk),
        size_bytes,
        logical_sector_size: LOGICAL_SECTOR_SIZE,
        media_kind: match media {
            MediaKind::Fixed => ProtocolMediaKind::Fixed as u32,
            MediaKind::Removable => ProtocolMediaKind::Removable as u32,
        },
        requested_drive_letter: u16::from(letter.as_ascii()),
        reserved16: 0,
        reserved32: 0,
    };
    let created: CreateResponse = adapter.send(Operation::CreateDisk, &request)?;

    let result: Result<(), String> = (|| {
        let disk_number = wait_for_physical_disk(
            ScsiLocation {
                port: adapter.port_number,
                path: created.path_id,
                target: created.target_id,
                lun: created.lun,
            },
            DEVICE_WAIT,
        )?;
        verify_physical_disk_location(
            disk_number,
            ScsiLocation {
                port: adapter.port_number,
                path: created.path_id,
                target: created.target_id,
                lun: created.lun,
            },
        )?;
        initialize_and_format(disk_number, letter, media, size_bytes, created.device_id)?;
        wait_for_drive_state(letter, true, DEVICE_WAIT)?;
        verify_mounted_drive(letter, disk_number, expected_location(&adapter, &created))?;
        Ok(())
    })();

    if let Err(error) = result {
        let rollback = adapter.delete_by_id(created.device_id);
        return match rollback {
            Ok(()) => Err(format!(
                "failed to initialize the new disk; the driver allocation was rolled back: {error}"
            )),
            Err(rollback_error) => Err(format!(
                "failed to initialize the new disk: {error}; driver rollback also failed: {rollback_error}"
            )),
        };
    }

    println!(
        "created {} WispDisk {} ({size_bytes} bytes, device id {})",
        media, letter, created.device_id
    );
    Ok(())
}

fn delete_disk(letter: DriveLetter) -> Result<(), String> {
    let mount_point = mount_point(letter);
    if !drive_letter_in_use(letter) {
        return Err(format!("drive {letter} is not mounted"));
    }

    let volume_name = get_volume_name(&mount_point)?;
    let volume = open_device(
        &format!(r"\\.\{}:", char::from(letter.as_ascii())),
        GENERIC_READ | GENERIC_WRITE,
    )
    .map_err(|error| os_error("open target volume", error))?;
    let disk_number = volume_disk_number(&volume)?;
    let address = physical_disk_address(disk_number)?;
    let location = address;

    let mut matched = None;
    for adapter in find_adapters()? {
        if adapter.port_number != address.port {
            continue;
        }
        let matches: Vec<DiskInfo> = adapter
            .list_disks()?
            .into_iter()
            .filter(|disk| {
                disk.path_id == address.path
                    && disk.target_id == address.target
                    && disk.lun == address.lun
            })
            .collect();
        if matches.len() > 1 || (matches.len() == 1 && matched.is_some()) {
            return Err(format!(
                "refusing to delete {letter}: multiple WispDisk LUNs claim its SCSI address"
            ));
        }
        if let Some(disk) = matches.first() {
            matched = Some((adapter, *disk));
        }
    }
    let Some((adapter, disk)) = matched else {
        return Err(format!(
            "refusing to delete {letter}: its PhysicalDrive{disk_number} address is not uniquely owned by WispDisk"
        ));
    };

    volume_control(&volume, FSCTL_LOCK_VOLUME, "lock target volume")?;
    if let Err(error) = volume_control(&volume, FSCTL_DISMOUNT_VOLUME, "dismount target volume") {
        let _ = volume_control(&volume, FSCTL_UNLOCK_VOLUME, "unlock target volume");
        return Err(error);
    }
    if unsafe { DeleteVolumeMountPointW(mount_point.as_ptr()) } == 0 {
        let error = os_error(
            "remove drive-letter mount point",
            io::Error::last_os_error(),
        );
        let _ = volume_control(&volume, FSCTL_UNLOCK_VOLUME, "unlock target volume");
        return Err(error);
    }

    if let Err(error) = adapter.delete_by_id(disk.device_id) {
        let restore_error = restore_mount_and_unlock(&volume, &mount_point, &volume_name).err();
        return match restore_error {
            Some(restore) => Err(format!(
                "driver rejected deletion: {error}; restoring {letter} also failed: {restore}"
            )),
            None => Err(format!(
                "driver rejected deletion; {letter} was restored: {error}"
            )),
        };
    }

    drop(volume);
    wait_for_drive_state(letter, false, DEVICE_WAIT)?;
    wait_for_physical_disk_absent(disk_number, location, DEVICE_WAIT)?;
    println!("deleted WispDisk {letter} (device id {})", disk.device_id);
    Ok(())
}

fn expected_location(adapter: &Adapter, created: &CreateResponse) -> ScsiLocation {
    ScsiLocation {
        port: adapter.port_number,
        path: created.path_id,
        target: created.target_id,
        lun: created.lun,
    }
}

fn ensure_adapter() -> Result<Adapter, String> {
    if let Some(adapter) = find_adapter()? {
        return Ok(adapter);
    }

    let package = extract_driver_package()?;
    install_driver_package(&package)?;
    wait_for_adapter(DEVICE_WAIT)
}

struct DriverPackage {
    inf: PathBuf,
}

fn extract_driver_package() -> Result<DriverPackage, String> {
    let program_data = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    let directory = program_data
        .join("WispDisk")
        .join("Driver")
        .join(env!("CARGO_PKG_VERSION"));
    fs::create_dir_all(&directory)
        .map_err(|error| path_error("create driver-package directory", &directory, error))?;

    write_package_file(&directory.join("WispDisk.sys"), payload::DRIVER_SYS)?;
    write_package_file(&directory.join("WispDisk.inf"), payload::DRIVER_INF)?;
    write_package_file(&directory.join("WispDisk.cat"), payload::DRIVER_CAT)?;
    let inf = directory
        .join("WispDisk.inf")
        .canonicalize()
        .map_err(|error| {
            path_error(
                "resolve extracted INF",
                &directory.join("WispDisk.inf"),
                error,
            )
        })?;
    Ok(DriverPackage { inf })
}

fn write_package_file(path: &Path, contents: &[u8]) -> Result<(), String> {
    if contents.is_empty() {
        return Err(format!("embedded package file {} is empty", path.display()));
    }
    fs::write(path, contents).map_err(|error| path_error("extract driver package", path, error))
}

fn install_driver_package(package: &DriverPackage) -> Result<(), String> {
    let inf = wide(package.inf.as_os_str());
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
        return Err(os_error(
            "stage driver package (is this console elevated, and is the package signed?)",
            io::Error::last_os_error(),
        ));
    }

    let mut created_device = create_root_device_if_missing()?;
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
        let error = os_error("bind and start WispDisk driver", io::Error::last_os_error());
        if let Some(device) = created_device.take() {
            let _ = unsafe { SetupDiCallClassInstaller(DIF_REMOVE, device.set.0, &device.data) };
        }
        return Err(error);
    }
    if reboot_required != 0 {
        return Err(
            "Windows installed the driver package but requires a reboot before it can start".into(),
        );
    }
    Ok(())
}

struct CreatedDevice {
    set: DeviceInfoSet,
    data: SP_DEVINFO_DATA,
}

fn create_root_device_if_missing() -> Result<Option<CreatedDevice>, String> {
    if root_device_exists()? {
        return Ok(None);
    }

    let set = DeviceInfoSet::new()?;
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
        return Err(os_error(
            "create WispDisk root device",
            io::Error::last_os_error(),
        ));
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
        return Err(os_error(
            "set WispDisk root-device hardware id",
            io::Error::last_os_error(),
        ));
    }
    if unsafe { SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set.0, &data) } == 0 {
        return Err(os_error(
            "register WispDisk root device",
            io::Error::last_os_error(),
        ));
    }

    Ok(Some(CreatedDevice { set, data }))
}

fn root_device_exists() -> Result<bool, String> {
    let raw = unsafe {
        SetupDiGetClassDevsW(
            &GUID_DEVCLASS_SCSIADAPTER,
            null(),
            null_mut(),
            DIGCF_PRESENT,
        )
    };
    if raw == -1_isize {
        return Err(os_error(
            "enumerate SCSI adapters",
            io::Error::last_os_error(),
        ));
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
            return Err(os_error("enumerate SCSI adapter", error));
        }
        if device_has_hardware_id(&set, &data, ROOT_HARDWARE_ID)? {
            return Ok(true);
        }
        index += 1;
    }
}

fn device_has_hardware_id(
    set: &DeviceInfoSet,
    data: &SP_DEVINFO_DATA,
    expected: &str,
) -> Result<bool, String> {
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
        if error.raw_os_error() == Some(13) {
            return Ok(false);
        }
        return Err(os_error("read adapter hardware id", error));
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

struct DeviceInfoSet(HDEVINFO);

impl DeviceInfoSet {
    fn new() -> Result<Self, String> {
        let handle = unsafe { SetupDiCreateDeviceInfoList(&GUID_DEVCLASS_SCSIADAPTER, null_mut()) };
        if handle == -1_isize {
            Err(os_error(
                "create device-information set",
                io::Error::last_os_error(),
            ))
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

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

struct Adapter {
    handle: OwnedHandle,
    port_number: u8,
}

impl Adapter {
    fn query_version(&self) -> Result<QueryVersionResponse, String> {
        let request = request_header::<RequestHeader>(Operation::QueryVersion);
        let response: QueryVersionResponse = self.send(Operation::QueryVersion, &request)?;
        if response.version_major != 0 || response.version_minor < 2 {
            return Err(format!(
                "unsupported loaded driver version {}.{}",
                response.version_major, response.version_minor
            ));
        }
        if response.maximum_disk_count as usize > MAXIMUM_DISK_COUNT {
            return Err("driver reports a disk-count limit larger than the shared ABI".into());
        }
        Ok(response)
    }

    fn list_disks(&self) -> Result<Vec<DiskInfo>, String> {
        let request = request_header::<RequestHeader>(Operation::ListDisks);
        let response: ListResponse = self.send(Operation::ListDisks, &request)?;
        let count = response.disk_count as usize;
        if count > response.disks.len() {
            return Err("driver returned an invalid disk count".into());
        }
        Ok(response.disks[..count].to_vec())
    }

    fn delete_by_id(&self, device_id: u32) -> Result<(), String> {
        let request = DeleteRequest {
            header: request_header::<DeleteRequest>(Operation::DeleteDisk),
            device_id,
            reserved: 0,
        };
        let _: ResponseHeader = self.send(Operation::DeleteDisk, &request)?;
        Ok(())
    }

    fn send<Request: Copy, Response: Copy + HasResponseHeader>(
        &self,
        operation: Operation,
        request: &Request,
    ) -> Result<Response, String> {
        if size_of::<SRB_IO_CONTROL>() + size_of::<Response>() > CONTROL_BUFFER_SIZE {
            return Err("internal IOCTL buffer is too small for the protocol response".into());
        }

        let mut storage = [0_u64; CONTROL_BUFFER_SIZE / size_of::<u64>()];
        let buffer = storage.as_mut_ptr().cast::<u8>();
        let correlation_id = request_correlation_id(request)?;
        let control = SRB_IO_CONTROL {
            HeaderLength: size_of::<SRB_IO_CONTROL>() as u32,
            Signature: SRB_SIGNATURE,
            Timeout: CONTROL_TIMEOUT_SECONDS,
            ControlCode: operation as u32,
            ReturnCode: 0,
            Length: (CONTROL_BUFFER_SIZE - size_of::<SRB_IO_CONTROL>()) as u32,
        };
        unsafe {
            buffer.cast::<SRB_IO_CONTROL>().write(control);
            buffer
                .add(size_of::<SRB_IO_CONTROL>())
                .copy_from_nonoverlapping(
                    request as *const Request as *const u8,
                    size_of::<Request>(),
                );
        }

        let mut returned = 0;
        let result = unsafe {
            DeviceIoControl(
                self.handle.0,
                IOCTL_SCSI_MINIPORT,
                buffer.cast::<c_void>(),
                CONTROL_BUFFER_SIZE as u32,
                buffer.cast::<c_void>(),
                CONTROL_BUFFER_SIZE as u32,
                &mut returned,
                null_mut(),
            )
        };
        if result == 0 {
            return Err(os_error(
                "send WispDisk miniport control request",
                io::Error::last_os_error(),
            ));
        }
        if returned < (size_of::<SRB_IO_CONTROL>() + size_of::<ResponseHeader>()) as u32 {
            return Err(format!(
                "driver returned a truncated control response ({returned} bytes)"
            ));
        }

        let returned_control = unsafe { buffer.cast::<SRB_IO_CONTROL>().read() };
        let header = unsafe {
            buffer
                .add(size_of::<SRB_IO_CONTROL>())
                .cast::<ResponseHeader>()
                .read_unaligned()
        };
        validate_response_header(operation, correlation_id, &returned_control, &header)?;
        if returned < (size_of::<SRB_IO_CONTROL>() + size_of::<Response>()) as u32
            || returned_control.Length < size_of::<Response>() as u32
        {
            return Err(
                "driver returned a shorter response than its success header promised".into(),
            );
        }
        let response = unsafe {
            buffer
                .add(size_of::<SRB_IO_CONTROL>())
                .cast::<Response>()
                .read_unaligned()
        };
        let response_header = response.response_header();
        if response_header.struct_size != size_of::<Response>() as u32 {
            return Err(format!(
                "driver returned an unexpected response size {} (expected {})",
                response_header.struct_size,
                size_of::<Response>()
            ));
        }
        Ok(response)
    }
}

trait HasResponseHeader {
    fn response_header(&self) -> &ResponseHeader;
}

macro_rules! response_header {
    ($type:ty, $field:ident) => {
        impl HasResponseHeader for $type {
            fn response_header(&self) -> &ResponseHeader {
                &self.$field
            }
        }
    };
}

impl HasResponseHeader for ResponseHeader {
    fn response_header(&self) -> &ResponseHeader {
        self
    }
}

response_header!(QueryVersionResponse, header);
response_header!(CreateResponse, header);
response_header!(ListResponse, header);

fn request_header<T>(operation: Operation) -> RequestHeader {
    RequestHeader {
        struct_size: size_of::<T>() as u32,
        protocol_version: PROTOCOL_VERSION,
        operation: operation as u32,
        flags: 0,
        correlation_id: NEXT_CORRELATION_ID.fetch_add(1, Ordering::Relaxed),
    }
}

fn request_correlation_id<T: Copy>(request: &T) -> Result<u64, String> {
    if size_of::<T>() < size_of::<RequestHeader>() {
        return Err("internal protocol request is smaller than its header".into());
    }
    let header = unsafe {
        (request as *const T)
            .cast::<RequestHeader>()
            .read_unaligned()
    };
    Ok(header.correlation_id)
}

fn validate_response_header(
    operation: Operation,
    correlation_id: u64,
    control: &SRB_IO_CONTROL,
    header: &ResponseHeader,
) -> Result<(), String> {
    if control.HeaderLength != size_of::<SRB_IO_CONTROL>() as u32
        || control.Signature != SRB_SIGNATURE
    {
        return Err("driver returned an invalid SRB control header".into());
    }
    if header.protocol_version != PROTOCOL_VERSION
        || header.operation != operation as u32
        || header.correlation_id != correlation_id
    {
        return Err("driver response did not match the request".into());
    }
    let status = header.status;
    if status < 0 || control.ReturnCode & 0x8000_0000 != 0 {
        return Err(format!(
            "driver rejected {:?} with NTSTATUS 0x{:08X}",
            operation, status as u32
        ));
    }
    Ok(())
}

fn find_adapter() -> Result<Option<Adapter>, String> {
    Ok(find_adapters()?.into_iter().next())
}

fn find_adapters() -> Result<Vec<Adapter>, String> {
    let mut adapters = Vec::new();
    for port in 0..SCSI_PORT_LIMIT {
        let Ok(handle) = open_device(&format!(r"\\.\Scsi{port}:"), GENERIC_READ | GENERIC_WRITE)
        else {
            continue;
        };
        let Ok(port_number) = u8::try_from(port) else {
            continue;
        };
        let adapter = Adapter {
            handle,
            port_number,
        };
        if adapter.query_version().is_ok() {
            adapters.push(adapter);
        }
    }
    Ok(adapters)
}

fn wait_for_adapter(timeout: Duration) -> Result<Adapter, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(adapter) = find_adapter()? {
            return Ok(adapter);
        }
        if Instant::now() >= deadline {
            return Err(
                "driver was installed, but its SCSI adapter did not appear within 30 seconds"
                    .into(),
            );
        }
        thread::sleep(RETRY_INTERVAL);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScsiLocation {
    port: u8,
    path: u8,
    target: u8,
    lun: u8,
}

fn wait_for_physical_disk(location: ScsiLocation, timeout: Duration) -> Result<u32, String> {
    let deadline = Instant::now() + timeout;
    loop {
        for number in 0..PHYSICAL_DISK_LIMIT {
            if physical_disk_address(number).ok() == Some(location) {
                return Ok(number);
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "new disk at SCSI {}:{}:{}:{} did not appear within 30 seconds",
                location.port, location.path, location.target, location.lun
            ));
        }
        thread::sleep(RETRY_INTERVAL);
    }
}

fn wait_for_physical_disk_absent(
    number: u32,
    location: ScsiLocation,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if physical_disk_address(number).ok() != Some(location) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "deleted disk at SCSI {}:{}:{}:{} remained present for 30 seconds",
                location.port, location.path, location.target, location.lun
            ));
        }
        thread::sleep(RETRY_INTERVAL);
    }
}

fn physical_disk_address(number: u32) -> Result<ScsiLocation, String> {
    let disk = open_device(&format!(r"\\.\PhysicalDrive{number}"), 0)
        .map_err(|error| os_error(&format!("open PhysicalDrive{number}"), error))?;
    let mut address: SCSI_ADDRESS = unsafe { zeroed() };
    address.Length = size_of::<SCSI_ADDRESS>() as u32;
    let mut returned = 0;
    let result = unsafe {
        DeviceIoControl(
            disk.0,
            IOCTL_SCSI_GET_ADDRESS,
            null(),
            0,
            (&mut address as *mut SCSI_ADDRESS).cast(),
            size_of::<SCSI_ADDRESS>() as u32,
            &mut returned,
            null_mut(),
        )
    };
    if result == 0 || returned < size_of::<SCSI_ADDRESS>() as u32 {
        return Err(os_error(
            &format!("query PhysicalDrive{number} SCSI address"),
            io::Error::last_os_error(),
        ));
    }
    Ok(ScsiLocation {
        port: address.PortNumber,
        path: address.PathId,
        target: address.TargetId,
        lun: address.Lun,
    })
}

fn verify_physical_disk_location(number: u32, expected: ScsiLocation) -> Result<(), String> {
    let actual = physical_disk_address(number)?;
    if actual != expected {
        return Err(format!(
            "PhysicalDrive{number} changed identity before initialization; refusing to format it"
        ));
    }
    Ok(())
}

fn verify_mounted_drive(
    letter: DriveLetter,
    expected_disk_number: u32,
    expected_location: ScsiLocation,
) -> Result<(), String> {
    let volume = open_device(
        &format!(r"\\.\{}:", char::from(letter.as_ascii())),
        GENERIC_READ,
    )
    .map_err(|error| os_error("open newly formatted volume", error))?;
    let actual_disk_number = volume_disk_number(&volume)?;
    if actual_disk_number != expected_disk_number {
        return Err(format!(
            "new mount {letter} resolved to PhysicalDrive{actual_disk_number}, not the created PhysicalDrive{expected_disk_number}"
        ));
    }
    verify_physical_disk_location(actual_disk_number, expected_location)
}

fn initialize_and_format(
    disk_number: u32,
    letter: DriveLetter,
    media: MediaKind,
    expected_size: u64,
    device_id: u32,
) -> Result<(), String> {
    ensure_drive_letter_available(letter)?;

    let letter = char::from(letter.as_ascii()).to_ascii_uppercase();
    let label = format!("WispDisk-{device_id:08X}");

    let storage = StorageWmi::connect()?;

    let disk = storage.get_disk(disk_number)?;

    if disk.is_system || disk.is_boot {
        return Err("refusing to initialize a boot or system disk".into());
    }

    if disk.size != expected_size {
        return Err(format!(
            "disk size changed before initialization: expected {}, got {}",
            expected_size, disk.size
        ));
    }

    if disk.is_offline {
        storage.online_disk(&disk)?;
    }

    if disk.is_read_only {
        storage.set_disk_read_only(&disk, false)?;
    }

    let partition = match disk.partition_style {
        PARTITION_STYLE_RAW => {
            storage.initialize_mbr(&disk)?;
            storage.create_max_partition(&disk)?
        }
        PARTITION_STYLE_MBR if media == MediaKind::Removable => {
            storage.get_or_create_removable_partition(&disk)?
        }
        style => {
            return Err(format!(
                "refusing to prepare PhysicalDrive{} because WMI reports unexpected partition style {style}",
                disk.number
            ));
        }
    };

    storage.assign_drive_letter(&partition, letter)?;

    let volume = storage.wait_for_volume(letter)?;

    storage.format_ntfs(
        &volume, &label, true, // quick
        true, // force
    )?;

    Ok(())
}

#[allow(non_camel_case_types)]
#[derive(Deserialize)]
struct MSFT_Disk {
    #[serde(rename = "__Path")]
    path: String,
    #[serde(rename = "Number")]
    number: u32,
    #[serde(rename = "Size")]
    size: u64,
    #[serde(rename = "PartitionStyle")]
    partition_style: u16,
    #[serde(rename = "IsBoot")]
    is_boot: bool,
    #[serde(rename = "IsSystem")]
    is_system: bool,
    #[serde(rename = "IsOffline")]
    is_offline: bool,
    #[serde(rename = "IsReadOnly")]
    is_read_only: bool,
}

#[allow(non_camel_case_types)]
#[derive(Deserialize)]
struct MSFT_Partition {
    #[serde(rename = "__Path")]
    path: String,
    #[serde(rename = "DiskNumber")]
    disk_number: u32,
    #[serde(rename = "PartitionNumber")]
    partition_number: u32,
    #[serde(rename = "DriveLetter")]
    drive_letter: Option<String>,
    #[serde(rename = "Offset")]
    offset: u64,
    #[serde(rename = "Size")]
    size: u64,
    #[serde(rename = "IsBoot")]
    is_boot: bool,
    #[serde(rename = "IsSystem")]
    is_system: bool,
}

#[allow(non_camel_case_types)]
#[derive(Deserialize)]
struct MSFT_Volume {
    #[serde(rename = "__Path")]
    path: String,
    #[serde(rename = "DriveLetter")]
    drive_letter: Option<String>,
}

#[derive(Deserialize)]
struct StorageMethodOutput {
    #[serde(rename = "ReturnValue")]
    return_value: u32,
}

#[derive(Serialize)]
struct SetDiskReadOnlyInput {
    #[serde(rename = "IsReadOnly")]
    is_read_only: bool,
}

#[derive(Serialize)]
struct InitializeDiskInput {
    #[serde(rename = "PartitionStyle")]
    partition_style: u16,
}

#[derive(Serialize)]
struct CreatePartitionInput {
    #[serde(rename = "UseMaximumSize")]
    use_maximum_size: bool,
    #[serde(rename = "AssignDriveLetter")]
    assign_drive_letter: bool,
    #[serde(rename = "MbrType")]
    mbr_type: u16,
}

#[derive(Serialize)]
struct AddAccessPathInput {
    #[serde(rename = "AccessPath")]
    access_path: String,
}

#[derive(Serialize)]
struct RemoveAccessPathInput {
    #[serde(rename = "AccessPath")]
    access_path: String,
}

#[derive(Serialize)]
struct FormatVolumeInput {
    #[serde(rename = "FileSystem")]
    file_system: String,
    #[serde(rename = "FileSystemLabel")]
    file_system_label: String,
    #[serde(rename = "Full")]
    full: bool,
    #[serde(rename = "Force")]
    force: bool,
}

struct StorageWmi {
    connection: WMIConnection,
}

impl StorageWmi {
    fn connect() -> Result<Self, String> {
        let connection = WMIConnection::with_namespace_path(r"ROOT\Microsoft\Windows\Storage")
            .map_err(|error| wmi_error("connect to the Windows Storage WMI provider", error))?;
        Ok(Self { connection })
    }

    fn get_disk(&self, disk_number: u32) -> Result<MSFT_Disk, String> {
        let query = format!(
            "SELECT __Path, Number, Size, PartitionStyle, IsBoot, IsSystem, IsOffline, IsReadOnly \
             FROM MSFT_Disk WHERE Number = {disk_number}"
        );
        let mut disks: Vec<MSFT_Disk> = self
            .connection
            .raw_query(query)
            .map_err(|error| wmi_error("query the new disk", error))?;
        if disks.len() != 1 {
            return Err(format!(
                "expected one WMI disk for PhysicalDrive{disk_number}, found {}",
                disks.len()
            ));
        }
        let disk = disks.remove(0);
        if disk.number != disk_number {
            return Err(format!(
                "WMI returned disk {} for PhysicalDrive{disk_number}; refusing to continue",
                disk.number
            ));
        }
        Ok(disk)
    }

    fn online_disk(&self, disk: &MSFT_Disk) -> Result<(), String> {
        let output: StorageMethodOutput = self
            .connection
            .exec_instance_method::<MSFT_Disk, _>(&disk.path, "Online", ())
            .map_err(|error| wmi_error("bring the new disk online", error))?;
        check_storage_method("bring the new disk online", output.return_value)
    }

    fn set_disk_read_only(&self, disk: &MSFT_Disk, read_only: bool) -> Result<(), String> {
        let input = SetDiskReadOnlyInput {
            is_read_only: read_only,
        };
        let output: StorageMethodOutput = self
            .connection
            .exec_instance_method::<MSFT_Disk, _>(&disk.path, "SetAttributes", input)
            .map_err(|error| wmi_error("set the new disk writable", error))?;
        check_storage_method("set the new disk writable", output.return_value)
    }

    fn initialize_mbr(&self, disk: &MSFT_Disk) -> Result<(), String> {
        if disk.partition_style != PARTITION_STYLE_RAW {
            return Err(format!(
                "refusing to initialize PhysicalDrive{} because WMI reports partition style {} instead of RAW",
                disk.number, disk.partition_style
            ));
        }
        let input = InitializeDiskInput {
            partition_style: PARTITION_STYLE_MBR,
        };
        let output: StorageMethodOutput = self
            .connection
            .exec_instance_method::<MSFT_Disk, _>(&disk.path, "Initialize", input)
            .map_err(|error| wmi_error("initialize the new disk as MBR", error))?;
        check_storage_method("initialize the new disk as MBR", output.return_value)
    }

    fn create_max_partition(&self, disk: &MSFT_Disk) -> Result<MSFT_Partition, String> {
        let input = CreatePartitionInput {
            use_maximum_size: true,
            assign_drive_letter: false,
            mbr_type: 7, // IFS (NTFS or exFAT) in the Storage WMI schema.
        };
        let output: StorageMethodOutput = self
            .connection
            .exec_instance_method::<MSFT_Disk, _>(&disk.path, "CreatePartition", input)
            .map_err(|error| wmi_error("create the WispDisk partition", error))?;
        check_storage_method("create the WispDisk partition", output.return_value)?;

        self.wait_for_partition(disk.number)
    }

    fn get_or_create_removable_partition(
        &self,
        disk: &MSFT_Disk,
    ) -> Result<MSFT_Partition, String> {
        let mut partitions = self.get_partitions(disk.number)?;
        match partitions.len() {
            0 => self.create_max_partition(disk),
            1 => {
                let partition = partitions.remove(0);
                if partition.offset != 0 || partition.size != disk.size {
                    return Err(format!(
                        "refusing to use removable PhysicalDrive{} partition {} because it does not span the whole medium",
                        disk.number, partition.partition_number
                    ));
                }
                if partition.is_boot || partition.is_system {
                    return Err(format!(
                        "refusing to use a boot or system partition on PhysicalDrive{}",
                        disk.number
                    ));
                }
                Ok(partition)
            }
            count => Err(format!(
                "refusing to use removable PhysicalDrive{} because WMI reports {count} partitions",
                disk.number
            )),
        }
    }

    fn assign_drive_letter(&self, partition: &MSFT_Partition, letter: char) -> Result<(), String> {
        if let Some(current) = wmi_drive_letter(partition.drive_letter.as_deref()) {
            if current.eq_ignore_ascii_case(&letter) {
                return Ok(());
            }
            let input = RemoveAccessPathInput {
                access_path: format!("{current}:"),
            };
            let output: StorageMethodOutput = self
                .connection
                .exec_instance_method::<MSFT_Partition, _>(
                    &partition.path,
                    "RemoveAccessPath",
                    input,
                )
                .map_err(|error| {
                    wmi_error("remove the automatically assigned drive letter", error)
                })?;
            check_storage_method(
                "remove the automatically assigned drive letter",
                output.return_value,
            )?;
        }

        let access_path = format!("{letter}:");
        // AccessPath and AssignDriveLetter are mutually exclusive WMI inputs.
        // Omitting AssignDriveLetter is different from serializing it as false.
        let input = AddAccessPathInput { access_path };
        let output: StorageMethodOutput = self
            .connection
            .exec_instance_method::<MSFT_Partition, _>(&partition.path, "AddAccessPath", input)
            .map_err(|error| wmi_error("assign the requested drive letter", error))?;
        check_storage_method("assign the requested drive letter", output.return_value)
    }

    fn get_partitions(&self, disk_number: u32) -> Result<Vec<MSFT_Partition>, String> {
        let query = format!(
            "SELECT __Path, DiskNumber, PartitionNumber, DriveLetter, Offset, Size, IsBoot, IsSystem \
             FROM MSFT_Partition WHERE DiskNumber = {disk_number}"
        );
        let partitions: Vec<MSFT_Partition> = self
            .connection
            .raw_query(query)
            .map_err(|error| wmi_error("query the new partition", error))?;
        if partitions
            .iter()
            .any(|partition| partition.disk_number != disk_number)
        {
            return Err("WMI returned a partition belonging to a different disk".into());
        }
        Ok(partitions)
    }

    fn wait_for_partition(&self, disk_number: u32) -> Result<MSFT_Partition, String> {
        let deadline = Instant::now() + DEVICE_WAIT;
        loop {
            let mut partitions = self.get_partitions(disk_number)?;
            match partitions.len() {
                1 => return Ok(partitions.remove(0)),
                count if count > 1 => {
                    return Err(format!(
                        "expected one partition on PhysicalDrive{disk_number}, found {count}"
                    ));
                }
                _ if Instant::now() >= deadline => {
                    return Err(format!(
                        "the partition on PhysicalDrive{disk_number} did not appear in WMI within 30 seconds"
                    ));
                }
                _ => thread::sleep(RETRY_INTERVAL),
            }
        }
    }

    fn wait_for_volume(&self, letter: char) -> Result<MSFT_Volume, String> {
        let deadline = Instant::now() + DEVICE_WAIT;
        loop {
            let volumes: Vec<MSFT_Volume> = self
                .connection
                .raw_query("SELECT __Path, DriveLetter FROM MSFT_Volume")
                .map_err(|error| wmi_error("query the new volume", error))?;
            let mut matches = volumes
                .into_iter()
                .filter(|volume| volume_has_drive_letter(volume, letter));
            if let Some(volume) = matches.next() {
                if matches.next().is_some() {
                    return Err(format!("WMI returned multiple volumes for drive {letter}:"));
                }
                return Ok(volume);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "drive {letter}: did not appear in WMI within 30 seconds"
                ));
            }
            thread::sleep(RETRY_INTERVAL);
        }
    }

    fn format_ntfs(
        &self,
        volume: &MSFT_Volume,
        label: &str,
        quick: bool,
        force: bool,
    ) -> Result<(), String> {
        let input = FormatVolumeInput {
            file_system: "NTFS".into(),
            file_system_label: label.into(),
            full: !quick,
            force,
        };
        let output: StorageMethodOutput = self
            .connection
            .exec_instance_method::<MSFT_Volume, _>(&volume.path, "Format", input)
            .map_err(|error| wmi_error("format the WispDisk volume as NTFS", error))?;
        check_storage_method("format the WispDisk volume as NTFS", output.return_value)
    }
}

fn volume_has_drive_letter(volume: &MSFT_Volume, expected: char) -> bool {
    wmi_drive_letter(volume.drive_letter.as_deref())
        .is_some_and(|actual| actual.eq_ignore_ascii_case(&expected))
}

fn wmi_drive_letter(value: Option<&str>) -> Option<char> {
    let value = value?.trim_matches('\0').trim_end_matches(':');
    let mut characters = value.chars();
    let letter = characters.next()?;
    if characters.next().is_some() || !letter.is_ascii_alphabetic() {
        return None;
    }
    Some(letter.to_ascii_uppercase())
}

fn check_storage_method(operation: &str, return_value: u32) -> Result<(), String> {
    if return_value == 0 {
        Ok(())
    } else {
        Err(format!(
            "{operation}: Windows Storage WMI returned {} ({})",
            return_value,
            storage_status_name(return_value)
        ))
    }
}

fn storage_status_name(status: u32) -> &'static str {
    match status {
        1 => "not supported",
        2 => "unspecified error",
        3 => "timeout",
        4 => "failed",
        5 => "invalid parameter",
        6 => "disk in use",
        7 => "unsupported process architecture",
        4096 => "operation started as an asynchronous job",
        4097 => "size not supported",
        40000 => "not enough free space",
        40001 => "access denied",
        40002 => "insufficient resources",
        40003 => "provider cache out of date",
        40004 => "unexpected I/O error",
        41000 => "disk not initialized",
        41001 => "disk already initialized",
        41002 => "disk read-only",
        41003 => "disk offline",
        41004 => "partition limit reached",
        42002 => "access path already in use",
        42007 => "invalid access path",
        43000 => "invalid allocation unit size",
        43001 => "file system not supported",
        43002 => "quick format unavailable",
        43005 => "allocation unit incompatible with sector size",
        43006 => "volume read-only",
        _ => "unrecognized storage-provider status",
    }
}

fn wmi_error(operation: &str, error: wmi::WMIError) -> String {
    format!("{operation}: {error}")
}

fn get_volume_name(mount_point: &[u16]) -> Result<Vec<u16>, String> {
    let mut volume_name = vec![0_u16; 128];
    if unsafe {
        GetVolumeNameForVolumeMountPointW(
            mount_point.as_ptr(),
            volume_name.as_mut_ptr(),
            volume_name.len() as u32,
        )
    } == 0
    {
        return Err(os_error(
            "resolve drive letter to a volume",
            io::Error::last_os_error(),
        ));
    }
    Ok(volume_name)
}

fn volume_disk_number(volume: &OwnedHandle) -> Result<u32, String> {
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
    if result == 0 || returned < size_of::<VOLUME_DISK_EXTENTS>() as u32 {
        return Err(os_error(
            "resolve volume to a physical disk",
            io::Error::last_os_error(),
        ));
    }
    let extents = unsafe {
        storage
            .as_ptr()
            .cast::<VOLUME_DISK_EXTENTS>()
            .read_unaligned()
    };
    if extents.NumberOfDiskExtents != 1 {
        return Err(format!(
            "refusing to delete a volume spanning {} physical disks",
            extents.NumberOfDiskExtents
        ));
    }
    Ok(extents.Extents[0].DiskNumber)
}

fn volume_control(volume: &OwnedHandle, code: u32, operation: &str) -> Result<(), String> {
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
        Err(os_error(operation, io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

fn restore_mount_and_unlock(
    volume: &OwnedHandle,
    mount_point: &[u16],
    volume_name: &[u16],
) -> Result<(), String> {
    let mount_result = unsafe { SetVolumeMountPointW(mount_point.as_ptr(), volume_name.as_ptr()) };
    let mount_error = if mount_result == 0 {
        Some(os_error(
            "restore drive-letter mount point",
            io::Error::last_os_error(),
        ))
    } else {
        None
    };
    let unlock_error = volume_control(volume, FSCTL_UNLOCK_VOLUME, "unlock restored volume").err();
    match (mount_error, unlock_error) {
        (None, None) => Ok(()),
        (Some(error), None) | (None, Some(error)) => Err(error),
        (Some(mount), Some(unlock)) => Err(format!("{mount}; {unlock}")),
    }
}

fn ensure_drive_letter_available(letter: DriveLetter) -> Result<(), String> {
    if drive_letter_in_use(letter) {
        Err(format!("drive letter {letter} is already in use"))
    } else {
        Ok(())
    }
}

fn drive_letter_in_use(letter: DriveLetter) -> bool {
    let index = u32::from(letter.as_ascii() - b'A');
    (unsafe { GetLogicalDrives() } & (1_u32 << index)) != 0
}

fn wait_for_drive_state(
    letter: DriveLetter,
    expected_present: bool,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if drive_letter_in_use(letter) == expected_present {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "drive {letter} did not {} within 30 seconds",
                if expected_present {
                    "appear"
                } else {
                    "disappear"
                }
            ));
        }
        thread::sleep(RETRY_INTERVAL);
    }
}

fn open_device(path: &str, access: u32) -> io::Result<OwnedHandle> {
    let path = wide(OsStr::new(path));
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

fn mount_point(letter: DriveLetter) -> Vec<u16> {
    wide(OsStr::new(&format!("{}:\\", char::from(letter.as_ascii()))))
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

fn os_error(operation: &str, error: io::Error) -> String {
    match error.raw_os_error() {
        Some(code) => format!("{operation}: {error} (Win32 error {code})"),
        None => format!("{operation}: {error}"),
    }
}

fn path_error(operation: &str, path: &Path, error: io::Error) -> String {
    format!("{operation} at {}: {error}", path.display())
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
    fn response_validation_rejects_wrong_correlation() {
        let control = SRB_IO_CONTROL {
            HeaderLength: size_of::<SRB_IO_CONTROL>() as u32,
            Signature: SRB_SIGNATURE,
            Timeout: 1,
            ControlCode: Operation::QueryVersion as u32,
            ReturnCode: 0,
            Length: size_of::<QueryVersionResponse>() as u32,
        };
        let header = ResponseHeader {
            struct_size: size_of::<QueryVersionResponse>() as u32,
            protocol_version: PROTOCOL_VERSION,
            operation: Operation::QueryVersion as u32,
            status: 0,
            correlation_id: 99,
        };
        assert!(validate_response_header(Operation::QueryVersion, 100, &control, &header).is_err());
    }

    #[test]
    fn response_validation_rejects_failed_ntstatus() {
        let control = SRB_IO_CONTROL {
            HeaderLength: size_of::<SRB_IO_CONTROL>() as u32,
            Signature: SRB_SIGNATURE,
            Timeout: 1,
            ControlCode: Operation::CreateDisk as u32,
            ReturnCode: 0xC000_000D,
            Length: size_of::<CreateResponse>() as u32,
        };
        let header = ResponseHeader {
            struct_size: size_of::<CreateResponse>() as u32,
            protocol_version: PROTOCOL_VERSION,
            operation: Operation::CreateDisk as u32,
            status: 0xC000_000D_u32 as i32,
            correlation_id: 7,
        };
        assert!(validate_response_header(Operation::CreateDisk, 7, &control, &header).is_err());
    }

    #[test]
    fn parses_wmi_drive_letters() {
        assert_eq!(wmi_drive_letter(Some("z")), Some('Z'));
        assert_eq!(wmi_drive_letter(Some("K:")), Some('K'));
        assert_eq!(wmi_drive_letter(Some("\0")), None);
        assert_eq!(wmi_drive_letter(None), None);
        assert_eq!(wmi_drive_letter(Some("not-a-letter")), None);
    }
}
