//! Adapter-backed transport and deadline for one reserved mutation.

use super::super::recovery::RecoveryAttempt;
use super::{
    Adapter, ControlOperation, DEVICE_WAIT, Operation, OperationHandle, OperationResponse,
    RETRY_INTERVAL,
};
use anyhow::Result;
use std::{thread, time::Instant};
use tracing::{debug, info};

pub(super) struct ReservedOperation<'a, O: ControlOperation> {
    adapter: &'a Adapter,
    request: O::Request,
    handle: OperationHandle,
    deadline: Instant,
}

impl<'a, O: ControlOperation<Response = OperationResponse>> ReservedOperation<'a, O> {
    pub(super) fn new(
        adapter: &'a Adapter,
        handle: OperationHandle,
        mutation: Operation,
        request: O::Request,
    ) -> Self {
        debug!(
            adapter_instance = handle.instance,
            operation_id = handle.id,
            ?mutation,
            "Management operation reserved"
        );
        Self {
            adapter,
            request,
            handle,
            deadline: Instant::now() + DEVICE_WAIT,
        }
    }
}

impl<O: ControlOperation<Response = OperationResponse>> RecoveryAttempt
    for ReservedOperation<'_, O>
{
    fn submit(&mut self) -> Result<OperationResponse> {
        let action = match O::CODE {
            Operation::CreateReserved => "create",
            Operation::DeleteReserved => "delete",
            _ => "management",
        };
        info!(
            adapter_instance = self.handle.instance,
            operation_id = self.handle.id,
            "Send driver {action} request"
        );
        self.adapter.send::<O>(&self.request)
    }

    fn query(&mut self) -> Result<OperationResponse> {
        debug!(
            adapter_instance = self.handle.instance,
            operation_id = self.handle.id,
            "Querying management outcome"
        );
        self.adapter.query_operation(self.handle)
    }

    fn wait_for_retry(&mut self) -> bool {
        let now = Instant::now();
        if now >= self.deadline {
            return false;
        }
        thread::sleep(RETRY_INTERVAL.min(self.deadline - now));
        Instant::now() < self.deadline
    }
}
