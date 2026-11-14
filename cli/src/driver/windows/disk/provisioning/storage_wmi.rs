//! Windows Storage WMI objects and operations used during provisioning.

use std::{thread, time::Instant};

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use widestring::U16CString;
use wmi::WMIConnection;

use crate::args::FileSystem;

use super::super::super::{
    DEVICE_WAIT, RETRY_INTERVAL,
    volume::{Volume, volume_device_path},
    win32::open_device_wide,
};

pub(super) const PARTITION_STYLE_RAW: u16 = 0;
pub(super) const PARTITION_STYLE_MBR: u16 = 1;
const MBR_TYPE_FAT16: u16 = 4;
const MBR_TYPE_IFS: u16 = 7;
const MBR_TYPE_FAT32: u16 = 12;

#[allow(non_camel_case_types)]
#[derive(Deserialize)]
pub(super) struct MSFT_Disk {
    #[serde(rename = "__Path")]
    path: String,
    #[serde(rename = "Number")]
    pub(super) number: u32,
    #[serde(rename = "Size")]
    pub(super) size: u64,
    #[serde(rename = "SerialNumber")]
    pub(super) serial_number: Option<String>,
    #[serde(rename = "PartitionStyle")]
    pub(super) partition_style: u16,
    #[serde(rename = "IsBoot")]
    pub(super) is_boot: bool,
    #[serde(rename = "IsSystem")]
    pub(super) is_system: bool,
    #[serde(rename = "IsOffline")]
    pub(super) is_offline: bool,
    #[serde(rename = "IsReadOnly")]
    pub(super) is_read_only: bool,
}

#[allow(non_camel_case_types)]
#[derive(Deserialize)]
pub(super) struct MSFT_Partition {
    #[serde(rename = "__Path")]
    path: String,
    #[serde(rename = "DiskNumber")]
    pub(super) disk_number: u32,
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
pub(super) struct MSFT_Volume {
    #[serde(rename = "__Path")]
    path: String,
    #[serde(rename = "Path")]
    volume_path: String,
}

impl MSFT_Volume {
    pub(super) fn verify_partition_extent(&self, partition: &MSFT_Partition) -> Result<()> {
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

pub(super) struct StorageWmi {
    connection: WMIConnection,
}

impl StorageWmi {
    pub(super) fn connect() -> Result<Self> {
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

    pub(super) fn get_disk(&self, disk_number: u32) -> Result<MSFT_Disk> {
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

    pub(super) fn bring_disk_online(&self, disk: &MSFT_Disk) -> Result<()> {
        self.exec_method::<MSFT_Disk>(&disk.path, "Online", (), "bring the new disk online")
    }

    pub(super) fn make_disk_writable(&self, disk: &MSFT_Disk) -> Result<()> {
        let input = SetDiskReadOnlyInput {
            is_read_only: false,
        };
        self.exec_method::<MSFT_Disk>(
            &disk.path,
            "SetAttributes",
            input,
            "set the new disk writable",
        )
    }

    pub(super) fn initialize_mbr(&self, disk: &MSFT_Disk) -> Result<()> {
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

    pub(super) fn create_max_partition(
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

    pub(super) fn find_removable_partition(
        &self,
        disk: &MSFT_Disk,
    ) -> Result<Option<MSFT_Partition>> {
        let mut partitions = self.get_partitions(disk.number)?;
        match partitions.len() {
            0 => Ok(None),
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
                Ok(Some(partition))
            }
            count => Err(anyhow!(
                "refusing to use removable PhysicalDrive{} because WMI reports {count} partitions",
                disk.number
            )),
        }
    }

    pub(super) fn assign_drive_letter(
        &self,
        partition: &MSFT_Partition,
        letter: char,
    ) -> Result<()> {
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

    pub(super) fn wait_for_volume(&self, partition: &MSFT_Partition) -> Result<MSFT_Volume> {
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

    pub(super) fn quick_format_volume(
        &self,
        volume: &MSFT_Volume,
        label: &str,
        file_system: FileSystem,
    ) -> Result<()> {
        let input = FormatVolumeInput {
            file_system: wmi_file_system(file_system).into(),
            file_system_label: label.into(),
            full: false,
            force: true,
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
    use windows_sys::Win32::System::Ioctl::DISK_EXTENT;

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
    fn maps_file_systems_to_storage_values() {
        assert_eq!(wmi_file_system(FileSystem::Fat), "FAT");
        assert_eq!(wmi_file_system(FileSystem::Fat32), "FAT32");
        assert_eq!(wmi_file_system(FileSystem::Ntfs), "NTFS");

        assert_eq!(mbr_type(FileSystem::Fat), MBR_TYPE_FAT16);
        assert_eq!(mbr_type(FileSystem::Fat32), MBR_TYPE_FAT32);
        assert_eq!(mbr_type(FileSystem::Ntfs), MBR_TYPE_IFS);
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
