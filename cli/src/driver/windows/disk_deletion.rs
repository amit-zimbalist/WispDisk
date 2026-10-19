//! Coordinate volume preparation and recovery for one verified WispDisk.

use anyhow::Result;

use super::{
    disk::{ScsiLocation, verify_physical_disk_location},
    recovery::recovery_failed,
    volume::Volume,
    volume_discovery::volumes_for_disk,
};

pub(super) struct DiskDeletion {
    volumes: Vec<Volume>,
}

impl DiskDeletion {
    pub(super) fn open(number: u32, location: ScsiLocation) -> Result<Self> {
        let volumes = volumes_for_disk(number)?
            .into_iter()
            .map(|info| Volume::open(info, number))
            .collect::<Result<_>>()?;
        verify_physical_disk_location(number, location)?;
        Ok(Self { volumes })
    }

    pub(super) fn lock(&mut self, force: bool) -> Result<()> {
        for volume in &mut self.volumes {
            volume.lock(force)?;
        }
        Ok(())
    }

    pub(super) fn dismount(&self) -> Result<()> {
        for volume in &self.volumes {
            volume.dismount()?;
        }
        Ok(())
    }

    pub(super) fn remove_mounts(&mut self) -> Result<()> {
        for volume in &mut self.volumes {
            volume.remove_mounts()?;
        }
        Ok(())
    }

    pub(super) fn restore(&mut self) -> Result<()> {
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
