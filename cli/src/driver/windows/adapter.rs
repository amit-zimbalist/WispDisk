//! Adapter discovery and validated WispDisk miniport protocol requests.

use anyhow::{Result, bail};

use std::{
    ffi::c_void,
    mem::size_of,
    ptr::null_mut,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    Foundation::{GENERIC_READ, GENERIC_WRITE},
    Storage::IscsiDisc::{IOCTL_SCSI_MINIPORT, SRB_IO_CONTROL},
    System::IO::DeviceIoControl,
};

use crate::{
    args::{DriveLetter, MediaKind},
    protocol::{
        CreateRequest, CreateResponse, DeleteRequest, DiskInfo, ListResponse, MAXIMUM_DISK_COUNT,
        Operation, PROTOCOL_VERSION, ProtocolMediaKind, QueryVersionResponse, RequestHeader,
        ResponseHeader, SRB_SIGNATURE,
    },
};

use super::{
    RETRY_INTERVAL,
    win32::{OwnedHandle, last_os_error, open_device},
};

const CONTROL_BUFFER_SIZE: usize = 1024;
const CONTROL_TIMEOUT_SECONDS: u32 = 30;
const SCSI_PORT_LIMIT: u32 = 256;
const LOGICAL_SECTOR_SIZE: u32 = 512;

static NEXT_CORRELATION_ID: AtomicU64 = AtomicU64::new(1);

pub(super) struct Adapter {
    handle: OwnedHandle,
    pub(super) port_number: u8,
}

impl Adapter {
    pub(super) fn query_version(&self) -> Result<QueryVersionResponse> {
        let request = request_header::<RequestHeader>(Operation::QueryVersion);
        let response: QueryVersionResponse = self.send(Operation::QueryVersion, &request)?;
        if response.version_major != 0 || response.version_minor < 2 {
            bail!(
                "unsupported loaded driver version {}.{}",
                response.version_major,
                response.version_minor
            );
        }
        if response.maximum_disk_count as usize > MAXIMUM_DISK_COUNT {
            bail!("driver reports a disk-count limit larger than the shared ABI");
        }
        Ok(response)
    }

    pub(super) fn create_disk(
        &self,
        letter: DriveLetter,
        media: MediaKind,
        size_bytes: u64,
    ) -> Result<CreateResponse> {
        let request = CreateRequest {
            header: request_header::<CreateRequest>(Operation::CreateDisk),
            size_bytes,
            logical_sector_size: LOGICAL_SECTOR_SIZE,
            media_kind: match media {
                MediaKind::Fixed => ProtocolMediaKind::Fixed as u32,
                MediaKind::Removable => ProtocolMediaKind::Removable as u32,
            },
            requested_drive_letter: u16::from(letter.as_ascii()),
            reserved16: 0,
            reserved32: 0,
        };
        self.send(Operation::CreateDisk, &request)
    }

    pub(super) fn list_disks(&self) -> Result<Vec<DiskInfo>> {
        let request = request_header::<RequestHeader>(Operation::ListDisks);
        let response: ListResponse = self.send(Operation::ListDisks, &request)?;
        let count = response.disk_count as usize;
        if count > response.disks.len() {
            bail!("driver returned an invalid disk count");
        }
        Ok(response.disks[..count].to_vec())
    }

    pub(super) fn delete_by_id(&self, device_id: u32) -> Result<()> {
        let request = DeleteRequest {
            header: request_header::<DeleteRequest>(Operation::DeleteDisk),
            device_id,
            reserved: 0,
        };
        let _: ResponseHeader = self.send(Operation::DeleteDisk, &request)?;
        Ok(())
    }

    fn send<Request: Copy, Response: Copy + HasResponseHeader>(
        &self,
        operation: Operation,
        request: &Request,
    ) -> Result<Response> {
        if size_of::<SRB_IO_CONTROL>() + size_of::<Response>() > CONTROL_BUFFER_SIZE {
            bail!("internal IOCTL buffer is too small for the protocol response");
        }

        let mut storage = [0_u64; CONTROL_BUFFER_SIZE / size_of::<u64>()];
        let buffer = storage.as_mut_ptr().cast::<u8>();
        let correlation_id = request_correlation_id(request)?;
        let control = SRB_IO_CONTROL {
            HeaderLength: size_of::<SRB_IO_CONTROL>() as u32,
            Signature: SRB_SIGNATURE,
            Timeout: CONTROL_TIMEOUT_SECONDS,
            ControlCode: operation as u32,
            ReturnCode: 0,
            Length: (CONTROL_BUFFER_SIZE - size_of::<SRB_IO_CONTROL>()) as u32,
        };
        unsafe {
            buffer.cast::<SRB_IO_CONTROL>().write(control);
            buffer
                .add(size_of::<SRB_IO_CONTROL>())
                .copy_from_nonoverlapping(
                    request as *const Request as *const u8,
                    size_of::<Request>(),
                );
        }

        let mut returned = 0;
        let result = unsafe {
            DeviceIoControl(
                self.handle.0,
                IOCTL_SCSI_MINIPORT,
                buffer.cast::<c_void>(),
                CONTROL_BUFFER_SIZE as u32,
                buffer.cast::<c_void>(),
                CONTROL_BUFFER_SIZE as u32,
                &mut returned,
                null_mut(),
            )
        };
        if result == 0 {
            return Err(last_os_error("send WispDisk miniport control request"));
        }
        if returned < (size_of::<SRB_IO_CONTROL>() + size_of::<ResponseHeader>()) as u32 {
            bail!("driver returned a truncated control response ({returned} bytes)");
        }

        let returned_control = unsafe { buffer.cast::<SRB_IO_CONTROL>().read() };
        let header = unsafe {
            buffer
                .add(size_of::<SRB_IO_CONTROL>())
                .cast::<ResponseHeader>()
                .read_unaligned()
        };
        validate_response_header(operation, correlation_id, &returned_control, &header)?;
        if returned < (size_of::<SRB_IO_CONTROL>() + size_of::<Response>()) as u32
            || returned_control.Length < size_of::<Response>() as u32
        {
            bail!("driver returned a shorter response than its success header promised");
        }
        let response = unsafe {
            buffer
                .add(size_of::<SRB_IO_CONTROL>())
                .cast::<Response>()
                .read_unaligned()
        };
        let response_header = response.response_header();
        if response_header.struct_size != size_of::<Response>() as u32 {
            bail!(
                "driver returned an unexpected response size {} (expected {})",
                response_header.struct_size,
                size_of::<Response>()
            );
        }
        Ok(response)
    }
}

trait HasResponseHeader {
    fn response_header(&self) -> &ResponseHeader;
}

macro_rules! response_header {
    ($type:ty, $field:ident) => {
        impl HasResponseHeader for $type {
            fn response_header(&self) -> &ResponseHeader {
                &self.$field
            }
        }
    };
}

impl HasResponseHeader for ResponseHeader {
    fn response_header(&self) -> &ResponseHeader {
        self
    }
}

response_header!(QueryVersionResponse, header);
response_header!(CreateResponse, header);
response_header!(ListResponse, header);

fn request_header<T>(operation: Operation) -> RequestHeader {
    RequestHeader {
        struct_size: size_of::<T>() as u32,
        protocol_version: PROTOCOL_VERSION,
        operation: operation as u32,
        flags: 0,
        correlation_id: NEXT_CORRELATION_ID.fetch_add(1, Ordering::Relaxed),
    }
}

fn request_correlation_id<T: Copy>(request: &T) -> Result<u64> {
    if size_of::<T>() < size_of::<RequestHeader>() {
        bail!("internal protocol request is smaller than its header");
    }
    let header = unsafe {
        (request as *const T)
            .cast::<RequestHeader>()
            .read_unaligned()
    };
    Ok(header.correlation_id)
}

fn validate_response_header(
    operation: Operation,
    correlation_id: u64,
    control: &SRB_IO_CONTROL,
    header: &ResponseHeader,
) -> Result<()> {
    if control.HeaderLength != size_of::<SRB_IO_CONTROL>() as u32
        || control.Signature != SRB_SIGNATURE
    {
        bail!("driver returned an invalid SRB control header");
    }
    if header.protocol_version != PROTOCOL_VERSION
        || header.operation != operation as u32
        || header.correlation_id != correlation_id
    {
        bail!("driver response did not match the request");
    }
    let status = header.status;
    if status < 0 || control.ReturnCode & 0x8000_0000 != 0 {
        bail!(
            "driver rejected {:?} with NTSTATUS 0x{:08X}",
            operation,
            status as u32
        );
    }
    Ok(())
}

// A lazy scan lets creation stop at the first valid adapter, while deletion
// still visits every adapter to establish unique ownership.
pub(super) fn adapters() -> impl Iterator<Item = Adapter> {
    (0..SCSI_PORT_LIMIT).filter_map(|port| {
        let handle =
            open_device(&format!(r"\\.\Scsi{port}:"), GENERIC_READ | GENERIC_WRITE).ok()?;
        let port_number = u8::try_from(port).ok()?;
        let adapter = Adapter {
            handle,
            port_number,
        };
        adapter.query_version().is_ok().then_some(adapter)
    })
}

pub(super) fn wait_for_adapter(timeout: Duration) -> Result<Adapter> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(adapter) = adapters().next() {
            return Ok(adapter);
        }
        if Instant::now() >= deadline {
            bail!("driver was installed, but its SCSI adapter did not appear within 30 seconds");
        }
        thread::sleep(RETRY_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn successful_response<Response>(
        operation: Operation,
        correlation_id: u64,
    ) -> (SRB_IO_CONTROL, ResponseHeader) {
        let control = SRB_IO_CONTROL {
            HeaderLength: size_of::<SRB_IO_CONTROL>() as u32,
            Signature: SRB_SIGNATURE,
            Timeout: 1,
            ControlCode: operation as u32,
            ReturnCode: 0,
            Length: size_of::<Response>() as u32,
        };
        let header = ResponseHeader {
            struct_size: size_of::<Response>() as u32,
            protocol_version: PROTOCOL_VERSION,
            operation: operation as u32,
            status: 0,
            correlation_id,
        };
        (control, header)
    }

    #[test]
    fn response_validation_accepts_matching_success() {
        let (control, header) =
            successful_response::<QueryVersionResponse>(Operation::QueryVersion, 99);
        assert!(validate_response_header(Operation::QueryVersion, 99, &control, &header).is_ok());
    }

    #[test]
    fn response_validation_rejects_wrong_correlation() {
        let (control, header) =
            successful_response::<QueryVersionResponse>(Operation::QueryVersion, 99);
        assert!(validate_response_header(Operation::QueryVersion, 100, &control, &header).is_err());
    }

    #[test]
    fn response_validation_rejects_failed_ntstatus() {
        let (control, header) = successful_response::<CreateResponse>(Operation::CreateDisk, 7);
        let failed_control = SRB_IO_CONTROL {
            ReturnCode: 0xC000_000D,
            ..control
        };
        let failed_header = ResponseHeader {
            status: 0xC000_000D_u32 as i32,
            ..header
        };
        assert!(
            validate_response_header(Operation::CreateDisk, 7, &failed_control, &header).is_err()
        );
        assert!(
            validate_response_header(Operation::CreateDisk, 7, &control, &failed_header).is_err()
        );
    }

    #[test]
    fn response_validation_rejects_invalid_control_header() {
        let (control, header) =
            successful_response::<QueryVersionResponse>(Operation::QueryVersion, 99);
        for invalid_control in [
            SRB_IO_CONTROL {
                HeaderLength: 0,
                ..control
            },
            SRB_IO_CONTROL {
                Signature: [0; 8],
                ..control
            },
        ] {
            assert!(
                validate_response_header(Operation::QueryVersion, 99, &invalid_control, &header)
                    .is_err()
            );
        }
    }

    #[test]
    fn response_validation_rejects_wrong_version_or_operation() {
        let (control, header) =
            successful_response::<QueryVersionResponse>(Operation::QueryVersion, 99);
        for invalid_header in [
            ResponseHeader {
                protocol_version: PROTOCOL_VERSION + 1,
                ..header
            },
            ResponseHeader {
                operation: Operation::CreateDisk as u32,
                ..header
            },
        ] {
            assert!(
                validate_response_header(Operation::QueryVersion, 99, &control, &invalid_header)
                    .is_err()
            );
        }
    }

    #[test]
    fn request_headers_preserve_layout_and_unique_correlation_ids() {
        let first = request_header::<CreateRequest>(Operation::CreateDisk);
        let second = request_header::<CreateRequest>(Operation::CreateDisk);
        assert_eq!(first.struct_size, size_of::<CreateRequest>() as u32);
        assert_eq!(first.protocol_version, PROTOCOL_VERSION);
        assert_eq!(first.operation, Operation::CreateDisk as u32);
        assert_eq!(first.flags, 0);
        assert_ne!(first.correlation_id, second.correlation_id);
        assert_eq!(
            request_correlation_id(&first).unwrap(),
            first.correlation_id
        );
        assert!(request_correlation_id(&0_u32).is_err());
    }
}
