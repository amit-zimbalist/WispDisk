//! Own the identity of an allocation protected by the driver during setup.

use anyhow::{Error, Result};

use crate::args::{DriveLetter, MediaKind};

use super::super::adapter::{Adapter, CreatedDisk};

trait ProvisioningDriver {
    fn finish_provisioning(&self, instance: u64, device_id: u32) -> Result<()>;
}

impl ProvisioningDriver for Adapter {
    fn finish_provisioning(&self, instance: u64, device_id: u32) -> Result<()> {
        Adapter::finish_provisioning(self, instance, device_id)
    }
}

/// Dropping the guard leaves the allocation protected for explicit recovery.
#[must_use = "provisioning protection must be explicitly finished after successful setup"]
pub(super) struct ProvisioningGuard<'a> {
    driver: &'a dyn ProvisioningDriver,
    created: CreatedDisk,
}

impl<'a> ProvisioningGuard<'a> {
    pub(super) fn create(
        adapter: &'a Adapter,
        letter: DriveLetter,
        media: MediaKind,
        size_bytes: u64,
    ) -> Result<Self> {
        let created = adapter.create_disk(letter, media, size_bytes)?;
        Ok(Self {
            driver: adapter,
            created,
        })
    }

    pub(super) fn created(&self) -> &CreatedDisk {
        &self.created
    }

    pub(super) fn provisioning_failed(&self, error: Error) -> Error {
        error.context(format!(
            "provisioning did not complete; device {} on adapter instance {} remains protected. After confirming Windows storage operations have stopped, use /recover-provisioning /adapter:{} /id:{}",
            self.created.device_id,
            self.created.adapter_instance,
            self.created.adapter_instance,
            self.created.device_id
        ))
    }

    pub(super) fn finish(self) -> Result<()> {
        self.driver
            .finish_provisioning(self.created.adapter_instance, self.created.device_id)
            .map_err(|error| {
                error.context(format!(
                    "could not confirm provisioning completion for device {} on adapter instance {}; inspect /list and use /recover-provisioning if it remains protected",
                    self.created.device_id, self.created.adapter_instance
                ))
            })
    }
}

impl Drop for ProvisioningGuard<'_> {
    fn drop(&mut self) {
        // A Windows storage operation could still be active. Only finish()
        // after confirmed success or explicit recovery may release protection.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, io};

    struct TestDriver {
        calls: RefCell<Vec<(u64, u32)>>,
        fail: bool,
    }

    impl ProvisioningDriver for TestDriver {
        fn finish_provisioning(&self, instance: u64, device_id: u32) -> Result<()> {
            self.calls.borrow_mut().push((instance, device_id));
            if self.fail {
                return Err(io::Error::from_raw_os_error(5).into());
            }
            Ok(())
        }
    }

    fn guard(driver: &TestDriver) -> ProvisioningGuard<'_> {
        ProvisioningGuard {
            driver,
            created: CreatedDisk {
                adapter_instance: 42,
                device_id: 7,
                path_id: 0,
                target_id: 0,
                lun: 1,
            },
        }
    }

    #[test]
    fn dropping_guard_keeps_driver_protection() {
        let driver = TestDriver {
            calls: RefCell::new(Vec::new()),
            fail: false,
        };
        drop(guard(&driver));
        assert!(driver.calls.borrow().is_empty());
    }

    #[test]
    fn provisioning_failure_retains_recovery_identity_without_releasing() {
        let driver = TestDriver {
            calls: RefCell::new(Vec::new()),
            fail: false,
        };
        let guard = guard(&driver);
        let error = guard.provisioning_failed(io::Error::from_raw_os_error(5).into());
        drop(guard);
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().raw_os_error(),
            Some(5)
        );
        let message = format!("{error:#}");
        assert!(message.contains("/recover-provisioning /adapter:42 /id:7"));
        assert!(driver.calls.borrow().is_empty());
    }

    #[test]
    fn finishing_releases_the_exact_allocation_once() {
        let driver = TestDriver {
            calls: RefCell::new(Vec::new()),
            fail: false,
        };
        guard(&driver).finish().unwrap();
        assert_eq!(*driver.calls.borrow(), [(42, 7)]);
    }

    #[test]
    fn failed_finish_preserves_error_and_does_not_retry_on_drop() {
        let driver = TestDriver {
            calls: RefCell::new(Vec::new()),
            fail: true,
        };
        let error = guard(&driver).finish().unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().raw_os_error(),
            Some(5)
        );
        assert!(format!("{error:#}").contains("device 7 on adapter instance 42"));
        assert_eq!(*driver.calls.borrow(), [(42, 7)]);
    }
}
