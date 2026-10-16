//! Adapter-backed transport and deadline for one reserved mutation.

use super::super::recovery::RecoveryAttempt;
use super::{
    Adapter, ControlOperation, DEVICE_WAIT, Operation, OperationHandle, OperationResponse,
    RETRY_INTERVAL,
};
use crate::timing;
use anyhow::Result;
use std::{thread, time::Instant};

pub(super) struct ReservedOperation<'a, O: ControlOperation> {
    adapter: &'a Adapter,
    request: O::Request,
    handle: OperationHandle,
    mutation: Operation,
    deadline: Instant,
}

impl<'a, O: ControlOperation<Response = OperationResponse>> ReservedOperation<'a, O> {
    pub(super) fn new(
        adapter: &'a Adapter,
        handle: OperationHandle,
        mutation: Operation,
        request: O::Request,
    ) -> Self {
        Self {
            adapter,
            request,
            handle,
            mutation,
            deadline: Instant::now() + DEVICE_WAIT,
        }
    }
}

impl<O: ControlOperation<Response = OperationResponse>> RecoveryAttempt
    for ReservedOperation<'_, O>
{
    fn submit(&mut self) -> Result<OperationResponse> {
        timing::measure(
            if self.mutation == Operation::CreateDisk {
                "send driver create request"
            } else {
                "send driver delete request"
            },
            || self.adapter.send::<O>(&self.request),
        )
    }

    fn query(&mut self) -> Result<OperationResponse> {
        timing::measure("query management outcome", || {
            self.adapter.query_operation(self.handle)
        })
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
