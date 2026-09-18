use std::{error::Error, fmt};

use anyhow::{Result, bail};

use crate::{args::Command, payload};

#[cfg(windows)]
mod windows;

pub fn execute(command: &Command) -> Result<()> {
    if !payload::package_is_embedded() {
        bail!(DriverError::PackageNotEmbedded);
    }

    #[cfg(windows)]
    {
        windows::execute(command)
    }

    #[cfg(not(windows))]
    {
        let _ = command;
        bail!(DriverError::UnsupportedPlatform)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum DriverError {
    PackageNotEmbedded,
    #[cfg(not(windows))]
    UnsupportedPlatform,
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
        }
    }
}

impl Error for DriverError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_preserves_typed_driver_errors_and_full_message() {
        let error = anyhow::Error::new(DriverError::PackageNotEmbedded).context("create disk");
        assert_eq!(
            error.downcast_ref::<DriverError>(),
            Some(&DriverError::PackageNotEmbedded)
        );
        let message = format!("{error:#}");
        assert!(message.starts_with("create disk: no signed driver package is embedded"));
    }
}
