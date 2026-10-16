//! Typed control operations and the wire-layout contracts used by transport.

use crate::protocol::{
    Operation, OperationRequest, OperationResponse, QueryVersionResponse, RequestHeader,
    ReserveRequest, ReservedCreateRequest, ReservedDeleteRequest, ResponseHeader, StateResponse,
};

/// Bind an operation code to its wire request and response types.
pub(super) trait ControlOperation {
    type Request: ProtocolRequest;
    type Response: ProtocolResponse;

    const CODE: Operation;
}

pub(super) struct QueryVersion;

impl ControlOperation for QueryVersion {
    type Request = RequestHeader;
    type Response = QueryVersionResponse;

    const CODE: Operation = Operation::QueryVersion;
}

pub(super) struct ReserveOperation;

impl ControlOperation for ReserveOperation {
    type Request = ReserveRequest;
    type Response = OperationResponse;

    const CODE: Operation = Operation::Reserve;
}

pub(super) struct QueryResult;

impl ControlOperation for QueryResult {
    type Request = OperationRequest;
    type Response = OperationResponse;

    const CODE: Operation = Operation::QueryResult;
}

pub(super) struct ListState;

impl ControlOperation for ListState {
    type Request = RequestHeader;
    type Response = StateResponse;

    const CODE: Operation = Operation::ListState;
}

pub(super) struct CreateReserved;

impl ControlOperation for CreateReserved {
    type Request = ReservedCreateRequest;
    type Response = OperationResponse;

    const CODE: Operation = Operation::CreateReserved;
}

pub(super) struct DeleteReserved;

impl ControlOperation for DeleteReserved {
    type Request = ReservedDeleteRequest;
    type Response = OperationResponse;

    const CODE: Operation = Operation::DeleteReserved;
}

/// A request whose complete representation can be sent to the driver.
///
/// # Safety
/// Implementations must use repr(C), contain no uninitialized padding, and
/// return the actual transmitted header from request_header().
pub(super) unsafe trait ProtocolRequest: Copy {
    fn request_header(&self) -> &RequestHeader;
}

/// A response that can be read from the driver buffer.
///
/// # Safety
/// Implementations must use repr(C), accept every bit pattern, and place
/// ResponseHeader at offset zero for initial transport validation.
pub(super) unsafe trait ProtocolResponse: Copy {
    fn response_header(&self) -> &ResponseHeader;
}

// SAFETY: RequestHeader is repr(C), has no padding, and contains only integer fields or headers.
unsafe impl ProtocolRequest for RequestHeader {
    fn request_header(&self) -> &RequestHeader {
        self
    }
}

// SAFETY: ReserveRequest is repr(C), has no padding, and contains only integer fields or headers.
unsafe impl ProtocolRequest for ReserveRequest {
    fn request_header(&self) -> &RequestHeader {
        &self.header
    }
}

// SAFETY: OperationRequest is repr(C), has no padding, and contains only integer fields or headers.
unsafe impl ProtocolRequest for OperationRequest {
    fn request_header(&self) -> &RequestHeader {
        &self.header
    }
}

// SAFETY: ReservedCreateRequest is repr(C), has no padding, and contains only integer fields or headers.
unsafe impl ProtocolRequest for ReservedCreateRequest {
    fn request_header(&self) -> &RequestHeader {
        &self.operation.header
    }
}

// SAFETY: ReservedDeleteRequest is repr(C), has no padding, and contains only integer fields or headers.
unsafe impl ProtocolRequest for ReservedDeleteRequest {
    fn request_header(&self) -> &RequestHeader {
        &self.operation.header
    }
}

// SAFETY: QueryVersionResponse is repr(C), contains only integer aggregates, and starts with ResponseHeader.
unsafe impl ProtocolResponse for QueryVersionResponse {
    fn response_header(&self) -> &ResponseHeader {
        &self.header
    }
}

// SAFETY: OperationResponse is repr(C), contains only integer aggregates, and starts with ResponseHeader.
unsafe impl ProtocolResponse for OperationResponse {
    fn response_header(&self) -> &ResponseHeader {
        &self.header
    }
}

// SAFETY: StateResponse is repr(C), contains only integer aggregates, and starts with ResponseHeader.
unsafe impl ProtocolResponse for StateResponse {
    fn response_header(&self) -> &ResponseHeader {
        &self.header
    }
}
