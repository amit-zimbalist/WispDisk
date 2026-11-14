//! Coordinate volume preparation and recovery for one verified WispDisk.

use anyhow::Result;
use tracing::info;

use super::super::{
    recovery::recovery_failed,
    volume::{Volume, volumes_for_disk},
};

use super::{ScsiLocation, verify_physical_disk_location};

// Keep these events under the removal target printed in CLI logs.
const REMOVAL_LOG_TARGET: &str = "wispdisk::driver::windows::disk::removal";

pub(super) struct DiskDeletion {
    disk_number: u32,
    volumes: Vec<Volume>,
}

impl DiskDeletion {
    pub(super) fn open(disk_number: u32, location: ScsiLocation) -> Result<Self> {
        let volumes = volumes_for_disk(disk_number)?
            .into_iter()
            .map(|info| Volume::open(info, disk_number))
            .collect::<Result<_>>()?;
        verify_physical_disk_location(disk_number, location)?;
        Ok(Self {
            disk_number,
            volumes,
        })
    }

    // Finish each phase across all volumes before advancing. If a phase fails,
    // the owning volumes keep any locks and removed paths for explicit recovery.
    pub(super) fn prepare(&mut self, force: bool) -> Result<()> {
        let disk_number = self.disk_number;
        info!(target: REMOVAL_LOG_TARGET, disk_number, "Lock target volumes");
        for volume in &mut self.volumes {
            volume.lock(force)?;
        }

        info!(target: REMOVAL_LOG_TARGET, disk_number, "Dismount target volumes");
        for volume in &self.volumes {
            volume.dismount()?;
        }

        info!(target: REMOVAL_LOG_TARGET, disk_number, "Remove volume mount points");
        for volume in &mut self.volumes {
            volume.remove_mounts()?;
        }
        Ok(())
    }

    pub(super) fn restore_mounts(&mut self) -> Result<()> {
        info!(
            target: REMOVAL_LOG_TARGET,
            disk_number = self.disk_number,
            "Restore volume mount points"
        );
        let mut failure = None;
        for volume in &mut self.volumes {
            if let Err(error) = volume.restore_mounts() {
                failure = Some(match failure {
                    Some(first) => {
                        recovery_failed(first, "another mount restoration failed", error)
                    }
                    None => error,
                });
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}
