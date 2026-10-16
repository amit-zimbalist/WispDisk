use std::{error::Error, fmt};

use anyhow::{Result, bail};

use crate::{args::Command, payload};

mod windows;

pub fn execute(command: &Command) -> Result<()> {
    if !payload::package_is_embedded() {
        bail!(DriverError::PackageNotEmbedded);
    }

    windows::execute(command)
}

pub fn error_exit_code(error: &anyhow::Error) -> u8 {
    if windows::is_unresolved(error) {
        return 3;
    }
    1
}

#[derive(Debug, Eq, PartialEq)]
pub enum DriverError {
    PackageNotEmbedded,
}

impl fmt::Display for DriverError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PackageNotEmbedded => formatter.write_str(
                "no signed driver package is embedded; build with WISPDISK_DRIVER_PACKAGE_DIR set",
            ),
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
