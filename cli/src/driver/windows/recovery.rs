//! Preserve the primary error and a failed recovery operation without flattening either.

use std::fmt;

use crate::protocol::{Operation, OperationResponse, OperationState};
use anyhow::{Error, Result, bail};

#[derive(Clone, Copy, Debug)]
pub(super) struct OperationHandle {
    pub instance: u64,
    pub id: u64,
}

#[derive(Debug)]
pub(super) struct DriverRejection(pub i32);

impl fmt::Display for DriverRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "driver rejected operation with NTSTATUS 0x{:08X}",
            self.0 as u32
        )
    }
}
impl std::error::Error for DriverRejection {}

#[derive(Debug)]
pub(super) struct UnresolvedOutcome(pub OperationHandle);

impl fmt::Display for UnresolvedOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "outcome unresolved for adapter instance {} operation {}; query inventory before starting another mutation",
            self.0.instance, self.0.id
        )
    }
}
impl std::error::Error for UnresolvedOutcome {}

// The same algorithm is used by create and rollback/delete. The pause callback
// provides a bounded deadline in production and deterministic schedules in tests.
pub(super) fn resolve_operation(
    handle: OperationHandle,
    mutation: Operation,
    mut submit: impl FnMut() -> Result<OperationResponse>,
    mut query: impl FnMut() -> Result<OperationResponse>,
    mut pause: impl FnMut() -> bool,
) -> Result<OperationResponse> {
    let mut primary = None;
    let mut query_error = None;
    let mut response = match submit() {
        Ok(response) => Some(response),
        Err(error) => {
            if error
                .downcast_ref::<DriverRejection>()
                .is_some_and(|rejection| rejection.0 as u32 != 0x8000_0011)
            {
                return Err(error);
            }
            primary = Some(error);
            None
        }
    };
    loop {
        let current = match response.take() {
            Some(response) => Ok(response),
            None => query(),
        };
        let mut reserved = false;
        match current {
            Ok(reply)
                if reply.adapter_instance == handle.instance
                    && reply.operation_id == handle.id
                    && reply.mutation == mutation as u32 =>
            {
                match reply.state {
                    state
                        if state == OperationState::Succeeded as u32
                            && reply.result_status == 0 =>
                    {
                        return Ok(reply);
                    }
                    state if state == OperationState::Failed as u32 => {
                        return Err(Error::new(DriverRejection(reply.result_status)));
                    }
                    state if state == OperationState::Deleted as u32 => bail!(
                        "operation {} created device {} which has already been deleted",
                        handle.id,
                        reply.device_id
                    ),
                    state if state == OperationState::Reserved as u32 => reserved = true,
                    state if state == OperationState::Pending as u32 => {}
                    _ => {
                        query_error = Some(Error::msg(
                            "driver returned an invalid operation state or result",
                        ))
                    }
                }
            }
            Ok(_) => {
                query_error = Some(Error::msg("operation query returned a different identity"))
            }
            Err(error) => {
                query_error = Some(error);
            }
        }
        if !pause() {
            let primary = match (primary, query_error) {
                (Some(first), Some(last)) => {
                    Some(recovery_failed(first, "operation query also failed", last))
                }
                (first, last) => first.or(last),
            };
            return Err(primary
                .unwrap_or_else(|| {
                    Error::msg("operation remained pending past the recovery deadline")
                })
                .context(UnresolvedOutcome(handle)));
        }
        if reserved {
            response = match submit() {
                Ok(reply) => Some(reply),
                Err(error) => {
                    if error
                        .downcast_ref::<DriverRejection>()
                        .is_some_and(|rejection| rejection.0 as u32 != 0x8000_0011)
                    {
                        return Err(error);
                    }
                    primary = Some(error);
                    None
                }
            };
        }
    }
}

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

    fn handle() -> OperationHandle {
        OperationHandle {
            instance: 123,
            id: 7,
        }
    }
    fn reply(state: OperationState) -> OperationResponse {
        OperationResponse {
            header: crate::protocol::ResponseHeader {
                struct_size: 64,
                protocol_version: 1,
                operation: Operation::QueryResult as u32,
                status: 0,
                correlation_id: 1,
            },
            adapter_instance: 123,
            operation_id: 7,
            mutation: Operation::CreateDisk as u32,
            state: state as u32,
            result_status: 0,
            device_id: 1,
            path_id: 0,
            target_id: 0,
            lun: 0,
            reserved8: 0,
            reserved32: 0,
        }
    }
    fn lost_response() -> Error {
        Error::new(io::Error::from_raw_os_error(1117))
    }

    #[test]
    fn lost_create_response_recovers_original_disk_without_resubmitting() {
        let mut submissions = 0;
        let result = resolve_operation(
            handle(),
            Operation::CreateDisk,
            || {
                submissions += 1;
                Err(lost_response())
            },
            || Ok(reply(OperationState::Succeeded)),
            || true,
        )
        .unwrap();
        assert_eq!(result.device_id, 1);
        assert_eq!(submissions, 1);
    }

    #[test]
    fn only_reserved_operations_can_be_resubmitted() {
        let mut submissions = 0;
        let result = resolve_operation(
            handle(),
            Operation::CreateDisk,
            || {
                submissions += 1;
                if submissions == 1 {
                    Err(lost_response())
                } else {
                    Ok(reply(OperationState::Succeeded))
                }
            },
            || Ok(reply(OperationState::Reserved)),
            || true,
        )
        .unwrap();
        assert_eq!(submissions, 2);
        assert_eq!(result.operation_id, handle().id);
    }

    #[test]
    fn pending_operation_is_polled_without_resubmitting() {
        let mut submissions = 0;
        let mut queries = 0;
        resolve_operation(
            handle(),
            Operation::CreateDisk,
            || {
                submissions += 1;
                Ok(reply(OperationState::Pending))
            },
            || {
                queries += 1;
                Ok(reply(if queries < 3 {
                    OperationState::Pending
                } else {
                    OperationState::Succeeded
                }))
            },
            || true,
        )
        .unwrap();
        assert_eq!(submissions, 1);
        assert_eq!(queries, 3);
    }

    #[test]
    fn unreachable_query_preserves_transport_error_and_reports_unresolved_identity() {
        let error = resolve_operation(
            handle(),
            Operation::CreateDisk,
            || Err(lost_response()),
            || Err(Error::msg("query unavailable")),
            || false,
        )
        .unwrap_err();
        assert!(error.downcast_ref::<UnresolvedOutcome>().is_some());
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().raw_os_error(),
            Some(1117)
        );
        assert!(format!("{error:#}").contains("adapter instance 123 operation 7"));
    }

    #[test]
    fn wrong_adapter_instance_cannot_be_adopted() {
        let mut other = reply(OperationState::Succeeded);
        other.adapter_instance = 456;
        let error = resolve_operation(
            handle(),
            Operation::CreateDisk,
            || Err(lost_response()),
            || Ok(other),
            || false,
        )
        .unwrap_err();
        assert!(error.downcast_ref::<UnresolvedOutcome>().is_some());
    }

    #[test]
    fn explicit_rejection_is_not_retried() {
        let mut queries = 0;
        let error = resolve_operation(
            handle(),
            Operation::CreateDisk,
            || Err(Error::new(DriverRejection(0xC000_000Du32 as i32))),
            || {
                queries += 1;
                Ok(reply(OperationState::Reserved))
            },
            || true,
        )
        .unwrap_err();
        assert!(error.downcast_ref::<DriverRejection>().is_some());
        assert_eq!(queries, 0);
    }

    #[test]
    fn terminal_failure_and_deleted_create_are_not_reexecuted() {
        for state in [OperationState::Failed, OperationState::Deleted] {
            let mut result = reply(state);
            result.result_status = 0xC000_009Au32 as i32;
            let mut submissions = 0;
            assert!(
                resolve_operation(
                    handle(),
                    Operation::CreateDisk,
                    || {
                        submissions += 1;
                        Err(lost_response())
                    },
                    || Ok(result),
                    || true
                )
                .is_err()
            );
            assert_eq!(submissions, 1);
        }
    }

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
