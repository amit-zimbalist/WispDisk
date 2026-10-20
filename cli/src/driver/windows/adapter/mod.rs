//! Adapter discovery and validated WispDisk miniport protocol requests.

mod control_operation;
mod install;
mod reserved_operation;

pub(super) use self::install::install_embedded_driver;

use self::{
    control_operation::{
        ControlOperation, CreateReserved, DeleteReserved, ListState, ProtocolRequest,
        ProtocolResponse, QueryResult, QueryVersion, ReserveOperation,
    },
    reserved_operation::ReservedOperation,
};
use anyhow::{Context, Result, bail};

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

use crate::protocol::{
    CAP_OPERATION_RECOVERY, OperationRequest, OperationResponse, ReserveRequest,
    ReservedCreateRequest, ReservedDeleteRequest, StateResponse,
};

use crate::{
    args::{DriveLetter, MediaKind},
    protocol::{
        MAXIMUM_DISK_COUNT, Operation, PROTOCOL_VERSION, ProtocolMediaKind, QueryVersionResponse,
        RequestHeader, ResponseHeader, SRB_SIGNATURE,
    },
};

use super::{
    DEVICE_WAIT, RETRY_INTERVAL,
    recovery::{DriverRejection, OperationHandle, UnresolvedOutcome, resolve_operation},
    win32::{OwnedHandle, last_os_error, open_device},
};

const CONTROL_BUFFER_SIZE: usize = 1024;
const CONTROL_TIMEOUT_SECONDS: u32 = 30;
const SCSI_PORT_LIMIT: u32 = 256;
const LOGICAL_SECTOR_SIZE: u32 = 512;

static NEXT_CORRELATION_ID: AtomicU64 = AtomicU64::new(1);

pub(super) struct CreatedDisk {
    pub adapter_instance: u64,
    pub device_id: u32,
    pub path_id: u8,
    pub target_id: u8,
    pub lun: u8,
}

impl TryFrom<OperationResponse> for CreatedDisk {
    type Error = anyhow::Error;

    fn try_from(response: OperationResponse) -> Result<Self> {
        if !response.is_valid_created_disk() {
            bail!("driver returned an invalid created disk identity");
        }
        Ok(Self {
            adapter_instance: response.adapter_instance,
            device_id: response.device_id,
            path_id: response.path_id,
            target_id: response.target_id,
            lun: response.lun,
        })
    }
}

pub(super) struct Adapter {
    handle: OwnedHandle,
    pub(super) port_number: u8,
}

impl Adapter {
    pub(super) fn query_version(&self) -> Result<QueryVersionResponse> {
        let request = request_header::<QueryVersion>();
        let response: QueryVersionResponse = self.send::<QueryVersion>(&request)?;
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

    pub(super) fn require_recovery(&self) -> Result<()> {
        if self.query_version()?.capabilities & CAP_OPERATION_RECOVERY == 0 {
            bail!(
                "loaded WispDisk driver lacks operation recovery; upgrade the driver before creating or deleting disks"
            );
        }
        Ok(())
    }

    fn reserve_operation(&self, mutation: Operation) -> Result<OperationHandle> {
        self.require_recovery()?;
        let request = ReserveRequest {
            header: request_header::<ReserveOperation>(),
            mutation: mutation as u32,
            reserved: 0,
        };
        let response: OperationResponse = self.send::<ReserveOperation>(&request)?;
        if response.mutation != mutation as u32 {
            bail!("driver returned an invalid operation reservation");
        }
        response.try_into()
    }

    fn query_operation(&self, handle: OperationHandle) -> Result<OperationResponse> {
        self.send::<QueryResult>(&operation_request::<QueryResult>(handle))
    }

    pub(super) fn state(&self) -> Result<StateResponse> {
        let response: StateResponse = self.send::<ListState>(&request_header::<ListState>())?;
        if !response.is_valid() {
            bail!("driver returned invalid inventory");
        }
        Ok(response)
    }

    fn recover<O: ControlOperation<Response = OperationResponse>>(
        &self,
        handle: OperationHandle,
        mutation: Operation,
        request: O::Request,
    ) -> Result<OperationResponse> {
        let mut attempt = ReservedOperation::<O>::new(self, handle, mutation, request);
        resolve_operation(handle, mutation, &mut attempt)
    }

    pub(super) fn create_disk(
        &self,
        letter: DriveLetter,
        media: MediaKind,
        size_bytes: u64,
    ) -> Result<CreatedDisk> {
        let handle = self.reserve_operation(Operation::CreateDisk)?;
        let request = ReservedCreateRequest {
            operation: operation_request::<CreateReserved>(handle),
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
        let response = self.recover::<CreateReserved>(handle, Operation::CreateDisk, request)?;
        CreatedDisk::try_from(response).context(UnresolvedOutcome(handle))
    }

    pub(super) fn delete_by_id(&self, instance: u64, device_id: u32) -> Result<()> {
        let handle = self.reserve_operation(Operation::DeleteDisk)?;
        if handle.instance != instance {
            bail!("adapter changed before deletion; refusing stale device ID {device_id}");
        }
        let request = ReservedDeleteRequest {
            operation: operation_request::<DeleteReserved>(handle),
            device_id,
            reserved: 0,
        };
        self.recover::<DeleteReserved>(handle, Operation::DeleteDisk, request)?;
        let state = self
            .state()
            .map_err(|error| error.context(UnresolvedOutcome(handle)))?;
        if state.adapter_instance != handle.instance
            || state.disks[..state.disk_count as usize]
                .iter()
                .any(|entry| entry.disk.device_id == device_id)
        {
            return Err(anyhow::anyhow!(
                "delete operation {} completed but device {} removal could not be confirmed",
                handle.id,
                device_id
            )
            .context(UnresolvedOutcome(handle)));
        }
        Ok(())
    }

    fn send<O: ControlOperation>(&self, request: &O::Request) -> Result<O::Response> {
        if size_of::<SRB_IO_CONTROL>() + size_of::<O::Response>() > CONTROL_BUFFER_SIZE
            || size_of::<SRB_IO_CONTROL>() + size_of::<O::Request>() > CONTROL_BUFFER_SIZE
        {
            bail!("internal IOCTL buffer is too small for the protocol message");
        }

        let mut storage = [0_u64; CONTROL_BUFFER_SIZE / size_of::<u64>()];
        let buffer = storage.as_mut_ptr().cast::<u8>();
        let correlation_id = request.request_header().correlation_id;
        let control = SRB_IO_CONTROL {
            HeaderLength: size_of::<SRB_IO_CONTROL>() as u32,
            Signature: SRB_SIGNATURE,
            Timeout: CONTROL_TIMEOUT_SECONDS,
            ControlCode: O::CODE as u32,
            ReturnCode: 0,
            Length: (CONTROL_BUFFER_SIZE - size_of::<SRB_IO_CONTROL>()) as u32,
        };
        // SAFETY: storage is aligned for SRB_IO_CONTROL and both writes fit the
        // checked buffer. ProtocolRequest guarantees initialized wire bytes.
        unsafe {
            buffer.cast::<SRB_IO_CONTROL>().write(control);
            buffer
                .add(size_of::<SRB_IO_CONTROL>())
                .copy_from_nonoverlapping(
                    request as *const O::Request as *const u8,
                    size_of::<O::Request>(),
                );
        }

        let mut returned = 0;
        // SAFETY: the owned handle remains live and the synchronous call uses
        // the same writable buffer for input and output for its full duration.
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

        // SAFETY: the minimum returned length was checked above. These wire
        // headers contain only integers; the payload need not be aligned.
        let returned_control = unsafe { buffer.cast::<SRB_IO_CONTROL>().read() };
        let header = unsafe {
            buffer
                .add(size_of::<SRB_IO_CONTROL>())
                .cast::<ResponseHeader>()
                .read_unaligned()
        };
        validate_response_header(O::CODE, correlation_id, &returned_control, &header)?;
        if returned < (size_of::<SRB_IO_CONTROL>() + size_of::<O::Response>()) as u32
            || returned_control.Length < size_of::<O::Response>() as u32
        {
            bail!("driver returned a shorter response than its success header promised");
        }
        // SAFETY: response length and buffer capacity were checked above.
        // ProtocolResponse guarantees all bit patterns are valid for this type.
        let response = unsafe {
            buffer
                .add(size_of::<SRB_IO_CONTROL>())
                .cast::<O::Response>()
                .read_unaligned()
        };
        let response_header = response.response_header();
        if response_header.struct_size != size_of::<O::Response>() as u32 {
            bail!(
                "driver returned an unexpected response size {} (expected {})",
                response_header.struct_size,
                size_of::<O::Response>()
            );
        }
        Ok(response)
    }
}

fn request_header<O: ControlOperation>() -> RequestHeader {
    RequestHeader {
        struct_size: size_of::<O::Request>() as u32,
        protocol_version: PROTOCOL_VERSION,
        operation: O::CODE as u32,
        flags: 0,
        correlation_id: NEXT_CORRELATION_ID.fetch_add(1, Ordering::Relaxed),
    }
}

fn operation_request<O: ControlOperation>(handle: OperationHandle) -> OperationRequest {
    OperationRequest {
        header: request_header::<O>(),
        adapter_instance: handle.instance,
        operation_id: handle.id,
    }
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
        let rejected = if status < 0 {
            status
        } else {
            control.ReturnCode as i32
        };
        bail!(DriverRejection(rejected));
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
    use crate::protocol::CreateResponse;

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
        let first = request_header::<CreateReserved>();
        let second = request_header::<CreateReserved>();
        assert_eq!(first.struct_size, size_of::<ReservedCreateRequest>() as u32);
        assert_eq!(first.protocol_version, PROTOCOL_VERSION);
        assert_eq!(first.operation, Operation::CreateReserved as u32);
        assert_eq!(first.flags, 0);
        assert_ne!(first.correlation_id, second.correlation_id);
        assert_eq!(first.request_header().correlation_id, first.correlation_id);
    }
}
