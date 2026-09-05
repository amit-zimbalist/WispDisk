#![allow(dead_code)]

pub const PROTOCOL_VERSION: u32 = 1;
pub const SRB_SIGNATURE: [u8; 8] = *b"WISPDK01";
pub const MAXIMUM_DISK_COUNT: usize = 16;

pub const CAP_CREATE_DELETE: u32 = 0x0000_0001;
pub const CAP_READ_WRITE: u32 = 0x0000_0002;
pub const CAP_REMOVABLE: u32 = 0x0000_0004;

#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    QueryVersion = 0x800,
    CreateDisk = 0x801,
    DeleteDisk = 0x802,
    ListDisks = 0x803,
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
