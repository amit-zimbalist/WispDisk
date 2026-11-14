//! Storage WMI provisioning, kept private behind the add-flow entry point.

use anyhow::{Context, Result, anyhow, bail, ensure};

use std::{thread, time::Instant};
use tracing::info;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use wmi::WMIConnection;

use crate::{
    args::{DriveLetter, FileSystem, MediaKind},
    protocol::{DISK_STATE_PROVISIONING, StateResponse},
};

use super::super::{
    DEVICE_WAIT, RETRY_INTERVAL,
    adapter::{Adapter, CreatedDisk},
    volume::{Volume, ensure_drive_letter_available, volume_device_path},
    win32::open_device_wide,
};
use widestring::U16CString;

use super::{ScsiLocation, verify_physical_disk_location};

const PARTITION_STYLE_RAW: u16 = 0;
const PARTITION_STYLE_MBR: u16 = 1;
const MBR_TYPE_FAT16: u16 = 4;
const MBR_TYPE_IFS: u16 = 7;
const MBR_TYPE_FAT32: u16 = 12;

pub(super) fn initialize_and_format(
    target: &ProvisioningTarget<'_>,
    letter: DriveLetter,
    media: MediaKind,
    expected_size: u64,
    file_system: FileSystem,
) -> Result<()> {
    ensure_drive_letter_available(letter)?;

    let disk_number = target.disk_number;
    let letter = char::from(letter.as_ascii()).to_ascii_uppercase();
    let label = volume_label(file_system, target.created.device_id);

    info!(disk_number, "Connect to Storage WMI");
    let storage = StorageWmi::connect()?;

    info!(disk_number, "Query new disk");
    let disk = storage.get_disk(disk_number)?;

    ensure!(
        matches_allocation_serial(disk.serial_number.as_deref(), target.created),
        "WMI disk identity does not match the created allocation; refusing to provision it"
    );

    if disk.is_system || disk.is_boot {
        bail!("refusing to initialize a boot or system disk");
    }

    if disk.size != expected_size {
        bail!(
            "disk size changed before initialization: expected {}, got {}",
            expected_size,
            disk.size
        );
    }

    if disk.is_offline {
        info!(disk_number, "Bring disk online");
        target.verify()?;
        storage.online_disk(&disk)?;
    }

    if disk.is_read_only {
        info!(disk_number, "Make disk writable");
        target.verify()?;
        storage.set_disk_read_only(&disk, false)?;
    }

    let (partition, drive_letter_assigned) = match disk.partition_style {
        PARTITION_STYLE_RAW => {
            info!(disk_number, "Initialize MBR");
            target.verify()?;
            storage.initialize_mbr(&disk)?;
            info!(disk_number, %letter, "Create partition and assign drive letter");
            target.verify()?;
            let partition = storage.create_max_partition(&disk, letter, file_system)?;
            (partition, true)
        }
        PARTITION_STYLE_MBR if media == MediaKind::Removable => {
            info!(disk_number, "Prepare removable partition");
            target.verify()?;
            storage.get_or_create_removable_partition(&disk, letter, file_system)?
        }
        style => {
            bail!(
                "refusing to prepare PhysicalDrive{} because WMI reports unexpected partition style {style}",
                disk.number
            );
        }
    };

    ensure!(
        partition.disk_number == disk_number,
        "WMI returned a partition on a different disk; refusing to provision it"
    );
    if !drive_letter_assigned {
        info!(disk_number, %letter, "Assign drive letter");
        target.verify()?;
        storage.assign_drive_letter(&partition, letter)?;
    }

    info!(%letter, "Wait for volume");
    let volume = storage.wait_for_volume(&partition)?;

    info!(disk_number, "Verify volume and allocation before format");
    volume.verify_partition(&partition)?;
    target.verify()?;

    info!(disk_number, %letter, %file_system, "Quick-format volume");
    storage.format_volume(
        &volume,
        &label,
        true, // quick
        true, // force
        file_system,
    )?;
    info!(disk_number, %letter, %file_system, "Volume formatted");

    Ok(())
}

pub(super) struct ProvisioningTarget<'a> {
    pub(super) adapter: &'a Adapter,
    pub(super) created: &'a CreatedDisk,
    pub(super) disk_number: u32,
    pub(super) location: ScsiLocation,
}

impl ProvisioningTarget<'_> {
    fn verify(&self) -> Result<()> {
        let state = self.adapter.state()?;
        ensure!(
            matches_created_allocation(&state, self.created),
            "created allocation changed before provisioning; refusing to modify the disk"
        );
        verify_physical_disk_location(self.disk_number, self.location)
    }
}

fn matches_created_allocation(state: &StateResponse, created: &CreatedDisk) -> bool {
    state.adapter_instance == created.adapter_instance
        && state.disks[..state.disk_count as usize]
            .iter()
            .any(|entry| {
                entry.state == DISK_STATE_PROVISIONING
                    && entry.disk.device_id == created.device_id
                    && entry.disk.path_id == created.path_id
                    && entry.disk.target_id == created.target_id
                    && entry.disk.lun == created.lun
            })
}

fn matches_allocation_serial(serial: Option<&str>, created: &CreatedDisk) -> bool {
    let expected = format!(
        "WISPDK{:016X}{:08X}",
        created.adapter_instance, created.device_id
    );
    serial.is_some_and(|serial| serial.trim().eq_ignore_ascii_case(&expected))
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
    #[serde(rename = "SerialNumber")]
    serial_number: Option<String>,
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
    #[serde(rename = "Path")]
    volume_path: String,
}

impl MSFT_Volume {
    fn verify_partition(&self, partition: &MSFT_Partition) -> Result<()> {
        let name = U16CString::from_str(&self.volume_path).context("encode volume path")?;
        let path = volume_device_path(&name)?;
        let handle = open_device_wide(&path, 0).context("open volume for identity verification")?;
        let extent = Volume::single_extent(&handle)?;
        ensure!(
            partition.matches_extent(&extent),
            "volume does not match the created partition; refusing to format"
        );
        Ok(())
    }
}

impl MSFT_Partition {
    fn matches_extent(&self, extent: &windows_sys::Win32::System::Ioctl::DISK_EXTENT) -> bool {
        extent.DiskNumber == self.disk_number
            && u64::try_from(extent.StartingOffset) == Ok(self.offset)
            && u64::try_from(extent.ExtentLength) == Ok(self.size)
    }
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
    #[serde(rename = "DriveLetter")]
    drive_letter: String,
    #[serde(rename = "MbrType")]
    mbr_type: u16,
}

#[derive(Deserialize)]
struct CreatePartitionOutput {
    #[serde(rename = "ReturnValue")]
    return_value: u32,
    #[serde(rename = "CreatedPartition")]
    created_partition: Option<MSFT_Partition>,
}

#[derive(Serialize)]
struct AccessPathInput {
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
    fn connect() -> Result<Self> {
        let connection = WMIConnection::with_namespace_path(r"ROOT\Microsoft\Windows\Storage")
            .context("connect to the Windows Storage WMI provider")?;
        Ok(Self { connection })
    }

    fn exec_method<Class: DeserializeOwned>(
        &self,
        path: &str,
        method: &str,
        input: impl Serialize,
        operation: &str,
    ) -> Result<()> {
        let output: StorageMethodOutput = self
            .connection
            .exec_instance_method::<Class, _>(path, method, input)
            .with_context(|| operation.to_owned())?;
        check_storage_method(operation, output.return_value)
    }

    fn get_disk(&self, disk_number: u32) -> Result<MSFT_Disk> {
        let query = format!(
            "SELECT __Path, Number, Size, SerialNumber, PartitionStyle, IsBoot, IsSystem, IsOffline, IsReadOnly \
             FROM MSFT_Disk WHERE Number = {disk_number}"
        );
        let mut disks: Vec<MSFT_Disk> = self
            .connection
            .raw_query(query)
            .context("query the new disk")?;
        if disks.len() != 1 {
            bail!(
                "expected one WMI disk for PhysicalDrive{disk_number}, found {}",
                disks.len()
            );
        }
        let disk = disks.remove(0);
        if disk.number != disk_number {
            bail!(
                "WMI returned disk {} for PhysicalDrive{disk_number}; refusing to continue",
                disk.number
            );
        }
        Ok(disk)
    }

    fn online_disk(&self, disk: &MSFT_Disk) -> Result<()> {
        self.exec_method::<MSFT_Disk>(&disk.path, "Online", (), "bring the new disk online")
    }

    fn set_disk_read_only(&self, disk: &MSFT_Disk, read_only: bool) -> Result<()> {
        let input = SetDiskReadOnlyInput {
            is_read_only: read_only,
        };
        self.exec_method::<MSFT_Disk>(
            &disk.path,
            "SetAttributes",
            input,
            "set the new disk writable",
        )
    }

    fn initialize_mbr(&self, disk: &MSFT_Disk) -> Result<()> {
        if disk.partition_style != PARTITION_STYLE_RAW {
            bail!(
                "refusing to initialize PhysicalDrive{} because WMI reports partition style {} instead of RAW",
                disk.number,
                disk.partition_style
            );
        }
        let input = InitializeDiskInput {
            partition_style: PARTITION_STYLE_MBR,
        };
        self.exec_method::<MSFT_Disk>(
            &disk.path,
            "Initialize",
            input,
            "initialize the new disk as MBR",
        )
    }

    fn create_max_partition(
        &self,
        disk: &MSFT_Disk,
        letter: char,
        file_system: FileSystem,
    ) -> Result<MSFT_Partition> {
        let input = CreatePartitionInput {
            use_maximum_size: true,
            drive_letter: letter.to_string(),
            mbr_type: mbr_type(file_system),
        };
        let output: CreatePartitionOutput = self
            .connection
            .exec_instance_method::<MSFT_Disk, _>(&disk.path, "CreatePartition", input)
            .context("create the WispDisk partition")?;
        check_storage_method("create the WispDisk partition", output.return_value)?;
        output.created_partition.ok_or_else(|| {
            anyhow!("Windows Storage WMI created the partition but returned no partition object")
        })
    }

    fn get_or_create_removable_partition(
        &self,
        disk: &MSFT_Disk,
        letter: char,
        file_system: FileSystem,
    ) -> Result<(MSFT_Partition, bool)> {
        let mut partitions = self.get_partitions(disk.number)?;
        match partitions.len() {
            0 => self
                .create_max_partition(disk, letter, file_system)
                .map(|partition| (partition, true)),
            1 => {
                let partition = partitions.remove(0);
                if partition.offset != 0 || partition.size != disk.size {
                    bail!(
                        "refusing to use removable PhysicalDrive{} partition {} because it does not span the whole medium",
                        disk.number,
                        partition.partition_number
                    );
                }
                if partition.is_boot || partition.is_system {
                    bail!(
                        "refusing to use a boot or system partition on PhysicalDrive{}",
                        disk.number
                    );
                }
                Ok((partition, false))
            }
            count => Err(anyhow!(
                "refusing to use removable PhysicalDrive{} because WMI reports {count} partitions",
                disk.number
            )),
        }
    }

    fn assign_drive_letter(&self, partition: &MSFT_Partition, letter: char) -> Result<()> {
        if let Some(current) = wmi_drive_letter(partition.drive_letter.as_deref()) {
            if current.eq_ignore_ascii_case(&letter) {
                return Ok(());
            }
            let input = AccessPathInput {
                access_path: format!("{current}:"),
            };
            self.exec_method::<MSFT_Partition>(
                &partition.path,
                "RemoveAccessPath",
                input,
                "remove the automatically assigned drive letter",
            )?;
        }

        let access_path = format!("{letter}:");
        // AccessPath and AssignDriveLetter are mutually exclusive WMI inputs.
        // Omitting AssignDriveLetter is different from serializing it as false.
        let input = AccessPathInput { access_path };
        self.exec_method::<MSFT_Partition>(
            &partition.path,
            "AddAccessPath",
            input,
            "assign the requested drive letter",
        )
    }

    fn get_partitions(&self, disk_number: u32) -> Result<Vec<MSFT_Partition>> {
        let query = format!(
            "SELECT __Path, DiskNumber, PartitionNumber, DriveLetter, Offset, Size, IsBoot, IsSystem \
             FROM MSFT_Partition WHERE DiskNumber = {disk_number}"
        );
        let partitions: Vec<MSFT_Partition> = self
            .connection
            .raw_query(query)
            .context("query the new partition")?;
        if partitions
            .iter()
            .any(|partition| partition.disk_number != disk_number)
        {
            bail!("WMI returned a partition belonging to a different disk");
        }
        Ok(partitions)
    }

    fn wait_for_volume(&self, partition: &MSFT_Partition) -> Result<MSFT_Volume> {
        let deadline = Instant::now() + DEVICE_WAIT;
        loop {
            let query = format!(
                "ASSOCIATORS OF {{{}}} WHERE AssocClass = MSFT_PartitionToVolume",
                partition.path
            );
            let volumes: Vec<MSFT_Volume> = self
                .connection
                .raw_query(query)
                .context("query the new volume")?;
            let mut matches = volumes.into_iter();
            if let Some(volume) = matches.next() {
                if matches.next().is_some() {
                    bail!("WMI returned multiple volumes for the created partition");
                }
                return Ok(volume);
            }
            if Instant::now() >= deadline {
                bail!("the created partition's volume did not appear in WMI within 30 seconds");
            }
            thread::sleep(RETRY_INTERVAL);
        }
    }

    fn format_volume(
        &self,
        volume: &MSFT_Volume,
        label: &str,
        quick: bool,
        force: bool,
        file_system: FileSystem,
    ) -> Result<()> {
        let input = FormatVolumeInput {
            file_system: wmi_file_system(file_system).into(),
            file_system_label: label.into(),
            full: !quick,
            force,
        };
        let operation = format!("format the WispDisk volume as {file_system}");
        self.exec_method::<MSFT_Volume>(&volume.path, "Format", input, &operation)
    }
}

fn wmi_file_system(file_system: FileSystem) -> &'static str {
    match file_system {
        FileSystem::Fat => "FAT",
        FileSystem::Fat32 => "FAT32",
        FileSystem::Ntfs => "NTFS",
    }
}

fn mbr_type(file_system: FileSystem) -> u16 {
    match file_system {
        FileSystem::Fat => MBR_TYPE_FAT16,
        FileSystem::Fat32 => MBR_TYPE_FAT32,
        FileSystem::Ntfs => MBR_TYPE_IFS,
    }
}

fn volume_label(file_system: FileSystem, device_id: u32) -> String {
    match file_system {
        FileSystem::Fat | FileSystem::Fat32 => format!("WD-{device_id:08X}"),
        FileSystem::Ntfs => format!("WispDisk-{device_id:08X}"),
    }
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

fn check_storage_method(operation: &str, return_value: u32) -> Result<()> {
    ensure!(
        return_value == 0,
        "{operation}: Windows Storage WMI returned {} ({})",
        return_value,
        storage_status_name(return_value)
    );
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{DiskInfo, DiskState, ResponseHeader};
    use windows_sys::Win32::System::Ioctl::DISK_EXTENT;

    #[test]
    fn rejects_replacement_allocation_at_the_same_lun() {
        let created = CreatedDisk {
            adapter_instance: 100,
            device_id: 10,
            path_id: 0,
            target_id: 0,
            lun: 2,
        };
        let mut state = StateResponse {
            header: ResponseHeader {
                struct_size: 0,
                protocol_version: 0,
                operation: 0,
                status: 0,
                correlation_id: 0,
            },
            adapter_instance: 100,
            allocated_bytes: 0,
            disk_count: 1,
            pending_operations: 0,
            disks: [DiskState::default(); crate::protocol::MAXIMUM_DISK_COUNT],
        };
        state.disks[0] = DiskState {
            disk: DiskInfo {
                device_id: 10,
                lun: 2,
                ..Default::default()
            },
            state: DISK_STATE_PROVISIONING,
            ..Default::default()
        };
        assert!(matches_created_allocation(&state, &created));
        state.disks[0].disk.device_id = 11;
        assert!(!matches_created_allocation(&state, &created));
        state.disks[0].disk.device_id = 10;
        state.adapter_instance = 101;
        assert!(!matches_created_allocation(&state, &created));
        state.adapter_instance = 100;
        state.disks[0].state = 2;
        assert!(!matches_created_allocation(&state, &created));
    }

    #[test]
    fn disk_serial_identifies_the_adapter_instance_and_device() {
        let created = CreatedDisk {
            adapter_instance: 0x1234,
            device_id: 0xabcd,
            path_id: 0,
            target_id: 0,
            lun: 0,
        };
        assert!(matches_allocation_serial(
            Some("WISPDK00000000000012340000ABCD"),
            &created
        ));
        assert!(!matches_allocation_serial(
            Some("WISPDK00000000000012350000ABCD"),
            &created
        ));
        assert!(!matches_allocation_serial(
            Some("WISPDK00000000000012340000ABCE"),
            &created
        ));
        assert!(!matches_allocation_serial(None, &created));
    }

    #[test]
    fn volume_extent_must_match_the_created_partition() {
        let partition = MSFT_Partition {
            path: String::new(),
            disk_number: 3,
            partition_number: 1,
            drive_letter: None,
            offset: 1048576,
            size: 4096,
            is_boot: false,
            is_system: false,
        };
        let mut extent = DISK_EXTENT {
            DiskNumber: 3,
            StartingOffset: 1048576,
            ExtentLength: 4096,
        };
        assert!(partition.matches_extent(&extent));
        extent.DiskNumber = 4;
        assert!(!partition.matches_extent(&extent));
        extent.DiskNumber = 3;
        extent.StartingOffset = 0;
        assert!(!partition.matches_extent(&extent));
        extent.StartingOffset = 1048576;
        extent.ExtentLength = -1;
        assert!(!partition.matches_extent(&extent));
    }

    #[test]
    fn maps_file_systems_to_storage_values_and_labels() {
        assert_eq!(wmi_file_system(FileSystem::Fat), "FAT");
        assert_eq!(wmi_file_system(FileSystem::Fat32), "FAT32");
        assert_eq!(wmi_file_system(FileSystem::Ntfs), "NTFS");

        assert_eq!(mbr_type(FileSystem::Fat), MBR_TYPE_FAT16);
        assert_eq!(mbr_type(FileSystem::Fat32), MBR_TYPE_FAT32);
        assert_eq!(mbr_type(FileSystem::Ntfs), MBR_TYPE_IFS);

        assert_eq!(volume_label(FileSystem::Fat, 0x1234ABCD), "WD-1234ABCD");
        assert_eq!(volume_label(FileSystem::Fat32, 0x1234ABCD), "WD-1234ABCD");
        assert_eq!(
            volume_label(FileSystem::Ntfs, 0x1234ABCD),
            "WispDisk-1234ABCD"
        );
    }

    #[test]
    fn parses_wmi_drive_letters() {
        assert_eq!(wmi_drive_letter(Some("z")), Some('Z'));
        assert_eq!(wmi_drive_letter(Some("K:")), Some('K'));
        assert_eq!(wmi_drive_letter(Some("\0")), None);
        assert_eq!(wmi_drive_letter(None), None);
        assert_eq!(wmi_drive_letter(Some("not-a-letter")), None);
    }

    #[test]
    fn accepts_successful_storage_method() {
        assert!(check_storage_method("format test volume", 0).is_ok());
    }

    #[test]
    fn storage_failure_preserves_operation_and_provider_status() {
        assert_eq!(
            check_storage_method("format test volume", 43002)
                .unwrap_err()
                .to_string(),
            "format test volume: Windows Storage WMI returned 43002 (quick format unavailable)"
        );
    }

    #[test]
    fn asynchronous_storage_result_is_not_treated_as_completion() {
        assert!(check_storage_method("format test volume", 4096).is_err());
    }
}
