//! Preserve the primary error and a failed recovery operation without flattening either.

use std::fmt;

use anyhow::Error;

#[derive(Debug)]
struct RecoveryFailure {
    action: String,
    cause: Error,
}

impl fmt::Display for RecoveryFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {:#}", self.action, self.cause)
    }
}

pub(super) fn recovery_failed(primary: Error, action: impl Into<String>, recovery: Error) -> Error {
    primary.context(RecoveryFailure {
        action: action.into(),
        cause: recovery,
    })
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    #[test]
    fn preserves_both_errors_and_reports_both_chains() {
        let primary = Error::new(io::Error::from_raw_os_error(5)).context("initialize disk");
        let recovery = Error::new(io::Error::from_raw_os_error(32)).context("delete allocation");
        let combined = recovery_failed(primary, "driver rollback also failed", recovery);

        assert_eq!(
            combined.downcast_ref::<io::Error>().unwrap().raw_os_error(),
            Some(5)
        );
        let recovery_context = combined.downcast_ref::<RecoveryFailure>().unwrap();
        assert_eq!(
            recovery_context
                .cause
                .downcast_ref::<io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(32)
        );

        let message = format!("{combined:#}");
        for expected in [
            "driver rollback also failed",
            "delete allocation",
            "initialize disk",
            "os error 5",
            "os error 32",
        ] {
            assert!(message.contains(expected), "missing {expected}: {message}");
        }
    }
}
