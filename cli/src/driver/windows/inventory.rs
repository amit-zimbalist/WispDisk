//! Collect and present the driver inventory with Windows disk and mount details.

use std::fmt;

use crate::{
    args::MediaKind,
    protocol::{DiskState, ProtocolMediaKind},
};
use anyhow::Result;
use serde::{Serialize, Serializer};

use super::{
    adapter::{Adapter, adapters},
    disk::{disk_location, find_physical_disk},
    volume::volumes_for_disk,
};

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum InventoryDiskState {
    Online,
    Stopping,
    Unknown,
}

impl From<u32> for InventoryDiskState {
    fn from(state: u32) -> Self {
        match state {
            1 => Self::Online,
            2 => Self::Stopping,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for InventoryDiskState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Online => "online",
            Self::Stopping => "stopping",
            Self::Unknown => "unknown",
        })
    }
}

// Reuse the CLI's media enum and its labels without coupling it to inventory serialization.
fn serialize_media<S: Serializer>(media: &MediaKind, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(media)
}

#[derive(Serialize)]
struct InventoryDisk {
    device_id: u32,
    state: InventoryDiskState,
    size_bytes: u64,
    #[serde(serialize_with = "serialize_media")]
    media: MediaKind,
    requested_letter: String,
    create_operation_id: String,
    path: u8,
    target: u8,
    lun: u8,
    physical_disk: Option<u32>,
    mount_points: Option<Vec<String>>,
}
#[derive(Serialize)]
struct InventoryAdapter {
    adapter_instance: String,
    port: u8,
    allocated_bytes: u64,
    pending_operations: u32,
    disks: Vec<InventoryDisk>,
}

impl InventoryDisk {
    fn collect(adapter: &Adapter, entry: &DiskState) -> Result<Self> {
        let number = find_physical_disk(disk_location(adapter, entry))?;
        Ok(Self {
            device_id: entry.disk.device_id,
            state: entry.state.into(),
            size_bytes: entry.disk.size_bytes,
            media: if entry.disk.media_kind == ProtocolMediaKind::Removable as u32 {
                MediaKind::Removable
            } else {
                MediaKind::Fixed
            },
            requested_letter: char::from_u32(u32::from(entry.disk.requested_drive_letter))
                .unwrap_or('?')
                .to_string(),
            create_operation_id: entry.create_operation_id.to_string(),
            path: entry.disk.path_id,
            target: entry.disk.target_id,
            lun: entry.disk.lun,
            physical_disk: number,
            mount_points: number.and_then(discover_mount_points),
        })
    }
}

impl InventoryAdapter {
    fn collect(adapter: &Adapter) -> Result<Self> {
        adapter.require_recovery()?;
        let state = adapter.state()?;
        let disks = state.disks[..state.disk_count as usize]
            .iter()
            .map(|entry| InventoryDisk::collect(adapter, entry))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            adapter_instance: state.adapter_instance.to_string(),
            port: adapter.port_number,
            allocated_bytes: state.allocated_bytes,
            pending_operations: state.pending_operations,
            disks,
        })
    }
}

impl fmt::Display for InventoryDisk {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "  id {} {} {} bytes {} requested {}: mounts {:?}",
            self.device_id,
            self.state,
            self.size_bytes,
            self.media,
            self.requested_letter,
            self.mount_points
        )
    }
}

impl fmt::Display for InventoryAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "adapter {} (Scsi{}): {} allocated bytes, {} pending operations",
            self.adapter_instance, self.port, self.allocated_bytes, self.pending_operations
        )?;
        for disk in &self.disks {
            write!(formatter, "\n{disk}")?;
        }
        Ok(())
    }
}

// Mount details are optional enrichment: Windows volume discovery failure must
// not prevent listing the driver's authoritative inventory.
fn discover_mount_points(number: u32) -> Option<Vec<String>> {
    let volumes = volumes_for_disk(number).ok()?;
    Some(
        volumes
            .into_iter()
            .flat_map(|volume| volume.mount_points)
            .map(|point| point.to_string_lossy())
            .collect(),
    )
}

fn collect_inventory() -> Result<Vec<InventoryAdapter>> {
    adapters()
        .map(|adapter| InventoryAdapter::collect(&adapter))
        .collect()
}

pub(super) fn list_inventory(json: bool) -> Result<()> {
    let inventory = collect_inventory()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&inventory)?);
    } else if inventory.is_empty() {
        println!("no WispDisk adapters");
    } else {
        for adapter in &inventory {
            println!("{adapter}");
        }
    }
    Ok(())
}
