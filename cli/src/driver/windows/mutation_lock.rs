//! Serialize CLI mutations across processes and user sessions.

use std::{marker::PhantomData, ptr::null, rc::Rc};

use anyhow::{Result, bail};
use windows_sys::Win32::{
    Foundation::{WAIT_ABANDONED, WAIT_OBJECT_0},
    System::Threading::{CreateMutexW, INFINITE, ReleaseMutex, WaitForSingleObject},
};

use super::win32::{OwnedHandle, last_os_error};

pub(super) struct MutationLock {
    handle: OwnedHandle,
    // Mutex ownership is tied to the acquiring thread.
    _thread_bound: PhantomData<Rc<()>>,
}

impl MutationLock {
    pub(super) fn acquire() -> Result<Self> {
        Self::acquire_named(windows_sys::core::w!("Global\\WispDisk-Mutation-v1"))
    }

    fn acquire_named(name: *const u16) -> Result<Self> {
        let handle = unsafe { CreateMutexW(null(), 0, name) };
        if handle.is_null() {
            return Err(last_os_error("open WispDisk mutation lock"));
        }
        let handle = OwnedHandle(handle);
        match unsafe { WaitForSingleObject(handle.0, INFINITE) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Self {
                handle,
                _thread_bound: PhantomData,
            }),
            windows_sys::Win32::Foundation::WAIT_FAILED => {
                Err(last_os_error("wait for WispDisk mutation lock"))
            }
            status => bail!("unexpected WispDisk mutation lock wait result {status}"),
        }
    }
}

impl Drop for MutationLock {
    fn drop(&mut self) {
        unsafe { ReleaseMutex(self.handle.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, thread, time::Duration};

    #[test]
    fn blocks_other_mutations_until_owner_releases() {
        let name = windows_sys::core::w!("Local\\WispDisk-Mutation-test-serialization");
        let owner = MutationLock::acquire_named(name).unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let waiter = thread::spawn(move || {
            ready_tx.send(()).unwrap();
            let _lock = MutationLock::acquire_named(windows_sys::core::w!(
                "Local\\WispDisk-Mutation-test-serialization"
            ))
            .unwrap();
            acquired_tx.send(()).unwrap();
        });
        ready_rx.recv().unwrap();
        assert!(
            acquired_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err()
        );
        drop(owner);
        acquired_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        waiter.join().unwrap();
    }

    #[test]
    fn recovers_an_abandoned_mutex() {
        let owner = thread::spawn(|| {
            let lock = MutationLock::acquire_named(windows_sys::core::w!(
                "Local\\WispDisk-Mutation-test-abandoned"
            ))
            .unwrap();
            let raw = lock.handle.0 as usize;
            std::mem::forget(lock);
            raw
        });
        let raw = owner.join().unwrap();
        let _lock = MutationLock::acquire_named(windows_sys::core::w!(
            "Local\\WispDisk-Mutation-test-abandoned"
        ))
        .unwrap();
        // Close the abandoned owner's handle while the new guard keeps the
        // mutex alive and owns it on this thread.
        drop(OwnedHandle(raw as windows_sys::Win32::Foundation::HANDLE));
    }
}
