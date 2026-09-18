//! Storage WMI provisioning, kept private behind the add-flow entry point.

use anyhow::{Context, Result, anyhow, bail, ensure};

use std::{thread, time::Instant};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use wmi::WMIConnection;

use crate::{
    args::{DriveLetter, MediaKind},
    timing,
};

use super::{
    DEVICE_WAIT, RETRY_INTERVAL,
    disk::{ScsiLocation, verify_physical_disk_location},
    volume::ensure_drive_letter_available,
};

const PARTITION_STYLE_RAW: u16 = 0;
const PARTITION_STYLE_MBR: u16 = 1;

pub(super) fn initialize_and_format(
    disk_number: u32,
    letter: DriveLetter,
    media: MediaKind,
    expected_size: u64,
    device_id: u32,
    expected_location: ScsiLocation,
) -> Result<()> {
    ensure_drive_letter_available(letter)?;

    let letter = char::from(letter.as_ascii()).to_ascii_uppercase();
    let label = format!("WispDisk-{device_id:08X}");

    let storage = StorageWmi::connect()?;
    timing::mark("connect to Storage WMI");

    let disk = storage.get_disk(disk_number)?;
    timing::mark("query new disk");

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
        storage.online_disk(&disk)?;
        timing::mark("bring disk online");
    }

    if disk.is_read_only {
        storage.set_disk_read_only(&disk, false)?;
        timing::mark("make disk writable");
    }

    let (partition, drive_letter_assigned) = match disk.partition_style {
        PARTITION_STYLE_RAW => {
            storage.initialize_mbr(&disk)?;
            timing::mark("initialize MBR");
            let partition = storage.create_max_partition(&disk, letter)?;
            timing::mark("create partition and assign drive letter");
            (partition, true)
        }
        PARTITION_STYLE_MBR if media == MediaKind::Removable => {
            let prepared = storage.get_or_create_removable_partition(&disk, letter)?;
            timing::mark("prepare removable partition");
            prepared
        }
        style => {
            bail!(
                "refusing to prepare PhysicalDrive{} because WMI reports unexpected partition style {style}",
                disk.number
            );
        }
    };

    if !drive_letter_assigned {
        storage.assign_drive_letter(&partition, letter)?;
        timing::mark("assign drive letter");
    }

    let volume = storage.wait_for_volume(letter)?;
    timing::mark("wait for volume");

    // The WMI calls above can take long enough for a PhysicalDrive number to be
    // reused. Revalidate the stable SCSI address immediately before formatting.
    verify_physical_disk_location(disk_number, expected_location)?;
    timing::mark("revalidate disk identity before format");

    storage.format_ntfs(
        &volume, &label, true, // quick
        true, // force
    )?;
    timing::mark("quick-format NTFS");

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
            "SELECT __Path, Number, Size, PartitionStyle, IsBoot, IsSystem, IsOffline, IsReadOnly \
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

    fn create_max_partition(&self, disk: &MSFT_Disk, letter: char) -> Result<MSFT_Partition> {
        let input = CreatePartitionInput {
            use_maximum_size: true,
            drive_letter: letter.to_string(),
            mbr_type: 7, // IFS (NTFS or exFAT) in the Storage WMI schema.
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
    ) -> Result<(MSFT_Partition, bool)> {
        let mut partitions = self.get_partitions(disk.number)?;
        match partitions.len() {
            0 => self
                .create_max_partition(disk, letter)
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

    fn wait_for_volume(&self, letter: char) -> Result<MSFT_Volume> {
        let deadline = Instant::now() + DEVICE_WAIT;
        loop {
            let query = volume_query(letter);
            let volumes: Vec<MSFT_Volume> = self
                .connection
                .raw_query(query)
                .context("query the new volume")?;
            let mut matches = volumes
                .into_iter()
                .filter(|volume| volume_has_drive_letter(volume, letter));
            if let Some(volume) = matches.next() {
                if matches.next().is_some() {
                    bail!("WMI returned multiple volumes for drive {letter}:");
                }
                return Ok(volume);
            }
            if Instant::now() >= deadline {
                bail!("drive {letter}: did not appear in WMI within 30 seconds");
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
    ) -> Result<()> {
        let input = FormatVolumeInput {
            file_system: "NTFS".into(),
            file_system_label: label.into(),
            full: !quick,
            force,
        };
        self.exec_method::<MSFT_Volume>(
            &volume.path,
            "Format",
            input,
            "format the WispDisk volume as NTFS",
        )
    }
}

fn volume_has_drive_letter(volume: &MSFT_Volume, expected: char) -> bool {
    wmi_drive_letter(volume.drive_letter.as_deref())
        .is_some_and(|actual| actual.eq_ignore_ascii_case(&expected))
}

fn volume_query(letter: char) -> String {
    // MSFT_Volume.DriveLetter is CIM Char16, so the filter is the bare letter;
    // access paths such as AddAccessPath use the separate "R:" spelling.
    format!("SELECT __Path, DriveLetter FROM MSFT_Volume WHERE DriveLetter = '{letter}'")
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

    #[test]
    fn parses_wmi_drive_letters() {
        assert_eq!(wmi_drive_letter(Some("z")), Some('Z'));
        assert_eq!(wmi_drive_letter(Some("K:")), Some('K'));
        assert_eq!(wmi_drive_letter(Some("\0")), None);
        assert_eq!(wmi_drive_letter(None), None);
        assert_eq!(wmi_drive_letter(Some("not-a-letter")), None);
    }

    #[test]
    fn filters_volume_query_by_cim_drive_letter() {
        assert_eq!(
            volume_query('R'),
            "SELECT __Path, DriveLetter FROM MSFT_Volume WHERE DriveLetter = 'R'"
        );
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
