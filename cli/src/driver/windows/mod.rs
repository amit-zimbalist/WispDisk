//! Windows command dispatch with adapter, disk, and volume components.

mod adapter;
mod disk;
mod inventory;
mod recovery;
mod volume;
mod win32;

use anyhow::Result;
use std::time::Duration;

use crate::args::Command;

use self::{
    disk::{add_disk, delete_by_identity, delete_disk},
    inventory::list_inventory,
};

const DEVICE_WAIT: Duration = Duration::from_secs(30);
const RETRY_INTERVAL: Duration = Duration::from_millis(200);

pub(super) fn is_unresolved(error: &anyhow::Error) -> bool {
    recovery::is_unresolved(error)
}

pub(super) fn execute(command: &Command) -> Result<()> {
    match command {
        Command::Add {
            letter,
            media,
            size_bytes,
            file_system,
        } => add_disk(*letter, *media, *size_bytes, *file_system),
        Command::Delete { letter, force } => delete_disk(*letter, *force),
        Command::DeleteId { adapter, id, force } => delete_by_identity(*adapter, *id, *force),
        Command::List { json } => list_inventory(*json),
    }
}
