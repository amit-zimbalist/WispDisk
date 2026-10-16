#![allow(dead_code)]

pub const PROTOCOL_VERSION: u32 = 1;
pub const SRB_SIGNATURE: [u8; 8] = *b"WISPDK01";
pub const MAXIMUM_DISK_COUNT: usize = 16;

pub const CAP_CREATE_DELETE: u32 = 0x0000_0001;
pub const CAP_READ_WRITE: u32 = 0x0000_0002;
pub const CAP_REMOVABLE: u32 = 0x0000_0004;
pub const CAP_OPERATION_RECOVERY: u32 = 0x0000_0008;

#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    QueryVersion = 0x800,
    CreateDisk = 0x801,
    DeleteDisk = 0x802,
    ListDisks = 0x803,
    Reserve = 0x804,
    QueryResult = 0x805,
    CreateReserved = 0x806,
    DeleteReserved = 0x807,
    ListState = 0x808,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtocolMediaKind {
    Fixed = 1,
    Removable = 2,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RequestHeader {
    pub struct_size: u32,
    pub protocol_version: u32,
    pub operation: u32,
    pub flags: u32,
    pub correlation_id: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ResponseHeader {
    pub struct_size: u32,
    pub protocol_version: u32,
    pub operation: u32,
    pub status: i32,
    pub correlation_id: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct QueryVersionResponse {
    pub header: ResponseHeader,
    pub version_major: u32,
    pub version_minor: u32,
    pub capabilities: u32,
    pub maximum_disk_count: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CreateRequest {
    pub header: RequestHeader,
    pub size_bytes: u64,
    pub logical_sector_size: u32,
    pub media_kind: u32,
    pub requested_drive_letter: u16,
    pub reserved16: u16,
    pub reserved32: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CreateResponse {
    pub header: ResponseHeader,
    pub device_id: u32,
    pub path_id: u8,
    pub target_id: u8,
    pub lun: u8,
    pub reserved: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DeleteRequest {
    pub header: RequestHeader,
    pub device_id: u32,
    pub reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DiskInfo {
    pub device_id: u32,
    pub path_id: u8,
    pub target_id: u8,
    pub lun: u8,
    pub reserved8: u8,
    pub size_bytes: u64,
    pub logical_sector_size: u32,
    pub media_kind: u32,
    pub requested_drive_letter: u16,
    pub reserved16: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ListResponse {
    pub header: ResponseHeader,
    pub disk_count: u32,
    pub reserved: u32,
    pub disks: [DiskInfo; MAXIMUM_DISK_COUNT],
}

const _: [(); 24] = [(); size_of::<RequestHeader>()];
const _: [(); 24] = [(); size_of::<ResponseHeader>()];
const _: [(); 40] = [(); size_of::<QueryVersionResponse>()];
const _: [(); 48] = [(); size_of::<CreateRequest>()];
const _: [(); 32] = [(); size_of::<CreateResponse>()];
const _: [(); 32] = [(); size_of::<DeleteRequest>()];
const _: [(); 32] = [(); size_of::<DiskInfo>()];
const _: [(); 544] = [(); size_of::<ListResponse>()];

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ReserveRequest {
    pub header: RequestHeader,
    pub mutation: u32,
    pub reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct OperationRequest {
    pub header: RequestHeader,
    pub adapter_instance: u64,
    pub operation_id: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ReservedCreateRequest {
    pub operation: OperationRequest,
    pub size_bytes: u64,
    pub logical_sector_size: u32,
    pub media_kind: u32,
    pub requested_drive_letter: u16,
    pub reserved16: u16,
    pub reserved32: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ReservedDeleteRequest {
    pub operation: OperationRequest,
    pub device_id: u32,
    pub reserved: u32,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationState {
    Reserved = 1,
    Pending = 2,
    Succeeded = 3,
    Failed = 4,
    Deleted = 5,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct OperationResponse {
    pub header: ResponseHeader,
    pub adapter_instance: u64,
    pub operation_id: u64,
    pub mutation: u32,
    pub state: u32,
    pub result_status: i32,
    pub device_id: u32,
    pub path_id: u8,
    pub target_id: u8,
    pub lun: u8,
    pub reserved8: u8,
    pub reserved32: u32,
}

impl OperationResponse {
    // These checks validate payload meaning; the transport validates the wire
    // header and recovery matches the response to the original request.
    pub fn is_valid_reservation(&self) -> bool {
        self.adapter_instance != 0
            && self.operation_id != 0
            && (self.mutation == Operation::CreateDisk as u32
                || self.mutation == Operation::DeleteDisk as u32)
            && self.state == OperationState::Reserved as u32
    }

    pub fn matches_operation(&self, instance: u64, id: u64, mutation: Operation) -> bool {
        self.adapter_instance == instance
            && self.operation_id == id
            && self.mutation == mutation as u32
    }

    pub fn is_valid_created_disk(&self) -> bool {
        self.adapter_instance != 0
            && self.operation_id != 0
            && self.mutation == Operation::CreateDisk as u32
            && self.state == OperationState::Succeeded as u32
            && self.result_status == 0
            && self.device_id != 0
            && self.path_id == 0
            && self.target_id == 0
            && (self.lun as usize) < MAXIMUM_DISK_COUNT
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DiskState {
    pub disk: DiskInfo,
    pub create_operation_id: u64,
    pub state: u32,
    pub reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct StateResponse {
    pub header: ResponseHeader,
    pub adapter_instance: u64,
    pub allocated_bytes: u64,
    pub disk_count: u32,
    pub pending_operations: u32,
    pub disks: [DiskState; MAXIMUM_DISK_COUNT],
}

impl StateResponse {
    pub fn is_valid(&self) -> bool {
        self.adapter_instance != 0 && (self.disk_count as usize) <= self.disks.len()
    }
}

const _: [(); 32] = [(); size_of::<ReserveRequest>()];
const _: [(); 40] = [(); size_of::<OperationRequest>()];
const _: [(); 64] = [(); size_of::<ReservedCreateRequest>()];
const _: [(); 48] = [(); size_of::<ReservedDeleteRequest>()];
const _: [(); 64] = [(); size_of::<OperationResponse>()];
const _: [(); 48] = [(); size_of::<DiskState>()];
const _: [(); 816] = [(); size_of::<StateResponse>()];
