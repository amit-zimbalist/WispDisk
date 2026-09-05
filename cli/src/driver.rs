use std::{error::Error, fmt};

use crate::{args::Command, payload};

#[cfg(windows)]
#[path = "driver_windows.rs"]
mod imp;

pub fn execute(command: &Command) -> Result<(), DriverError> {
    if !payload::package_is_embedded() {
        return Err(DriverError::PackageNotEmbedded);
    }

    #[cfg(windows)]
    {
        imp::execute(command).map_err(DriverError::Operation)
    }

    #[cfg(not(windows))]
    {
        let _ = command;
        Err(DriverError::UnsupportedPlatform)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum DriverError {
    PackageNotEmbedded,
    #[cfg(not(windows))]
    UnsupportedPlatform,
    Operation(String),
}

impl fmt::Display for DriverError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PackageNotEmbedded => formatter.write_str(
                "no signed driver package is embedded; build with WISPDISK_DRIVER_PACKAGE_DIR set",
            ),
            #[cfg(not(windows))]
            Self::UnsupportedPlatform => {
                formatter.write_str("wispdisk is only supported on Windows")
            }
            Self::Operation(message) => formatter.write_str(message),
        }
    }
}

impl Error for DriverError {}
