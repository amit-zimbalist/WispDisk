//! Initialize and format the Windows disk for a protected allocation.

mod storage_wmi;

use anyhow::{Result, bail, ensure};
use tracing::info;

use crate::{
    args::{DriveLetter, FileSystem, MediaKind},
    protocol::{DISK_STATE_PROVISIONING, StateResponse},
};

use super::super::{
    adapter::{Adapter, CreatedDisk},
    volume::ensure_drive_letter_available,
};
use super::{ScsiLocation, verify_physical_disk_location};
use storage_wmi::{PARTITION_STYLE_MBR, PARTITION_STYLE_RAW, StorageWmi};

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
        target.verify_allocation_and_location()?;
        storage.bring_disk_online(&disk)?;
    }

    if disk.is_read_only {
        info!(disk_number, "Make disk writable");
        target.verify_allocation_and_location()?;
        storage.make_disk_writable(&disk)?;
    }

    let (partition, drive_letter_assigned) = match disk.partition_style {
        PARTITION_STYLE_RAW => {
            info!(disk_number, "Initialize MBR");
            target.verify_allocation_and_location()?;
            storage.initialize_mbr(&disk)?;
            info!(disk_number, %letter, "Create partition and assign drive letter");
            target.verify_allocation_and_location()?;
            let partition = storage.create_max_partition(&disk, letter, file_system)?;
            (partition, true)
        }
        PARTITION_STYLE_MBR if media == MediaKind::Removable => {
            info!(disk_number, "Prepare removable partition");
            target.verify_allocation_and_location()?;
            match storage.find_removable_partition(&disk)? {
                Some(partition) => (partition, false),
                None => (
                    storage.create_max_partition(&disk, letter, file_system)?,
                    true,
                ),
            }
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
        target.verify_allocation_and_location()?;
        storage.assign_drive_letter(&partition, letter)?;
    }

    info!(%letter, "Wait for volume");
    let volume = storage.wait_for_volume(&partition)?;

    info!(disk_number, "Verify volume and allocation before format");
    volume.verify_partition_extent(&partition)?;
    target.verify_allocation_and_location()?;

    info!(disk_number, %letter, %file_system, "Quick-format volume");
    storage.quick_format_volume(&volume, &label, file_system)?;
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
    fn verify_allocation_and_location(&self) -> Result<()> {
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

fn volume_label(file_system: FileSystem, device_id: u32) -> String {
    match file_system {
        FileSystem::Fat | FileSystem::Fat32 => format!("WD-{device_id:08X}"),
        FileSystem::Ntfs => format!("WispDisk-{device_id:08X}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{DiskInfo, DiskState, ResponseHeader};

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
    fn generates_volume_labels() {
        assert_eq!(volume_label(FileSystem::Fat, 0x1234ABCD), "WD-1234ABCD");
        assert_eq!(volume_label(FileSystem::Fat32, 0x1234ABCD), "WD-1234ABCD");
        assert_eq!(
            volume_label(FileSystem::Ntfs, 0x1234ABCD),
            "WispDisk-1234ABCD"
        );
    }
}
