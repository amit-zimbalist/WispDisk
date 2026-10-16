//! Resolve reserved operations and preserve failures from recovery and rollback.

use std::fmt;

use crate::protocol::{Operation, OperationResponse, OperationState};
use anyhow::{Error, Result, bail};

#[derive(Clone, Copy, Debug)]
pub(super) struct OperationHandle {
    pub instance: u64,
    pub id: u64,
}

impl TryFrom<OperationResponse> for OperationHandle {
    type Error = Error;

    fn try_from(response: OperationResponse) -> Result<Self> {
        if !response.is_valid_reservation() {
            bail!("driver returned an invalid operation reservation");
        }
        Ok(Self {
            instance: response.adapter_instance,
            id: response.operation_id,
        })
    }
}

#[derive(Debug)]
pub(super) struct DriverRejection(pub i32);

impl DriverRejection {
    // STATUS_DEVICE_BUSY leaves the reserved operation available for recovery.
    const STATUS_DEVICE_BUSY: i32 = 0x8000_0011u32 as i32;

    fn is_terminal(&self) -> bool {
        self.0 != Self::STATUS_DEVICE_BUSY
    }
}

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

#[derive(Clone, Copy)]
enum RecoveryAction {
    Submit,
    Query,
}

enum RecoveryProgress {
    Complete(OperationResponse),
    Reserved,
    Pending,
    Invalid(Error),
}

impl OperationHandle {
    fn interpret(&self, reply: OperationResponse, mutation: Operation) -> Result<RecoveryProgress> {
        if !reply.matches_operation(self.instance, self.id, mutation) {
            return Ok(RecoveryProgress::Invalid(Error::msg(
                "operation query returned a different identity",
            )));
        }
        match reply.state {
            state if state == OperationState::Succeeded as u32 && reply.result_status == 0 => {
                Ok(RecoveryProgress::Complete(reply))
            }
            state if state == OperationState::Failed as u32 && reply.result_status < 0 => {
                Err(Error::new(DriverRejection(reply.result_status)))
            }
            state if state == OperationState::Deleted as u32 => bail!(
                "operation {} created device {} which has already been deleted",
                self.id,
                reply.device_id
            ),
            state if state == OperationState::Reserved as u32 => Ok(RecoveryProgress::Reserved),
            state if state == OperationState::Pending as u32 => Ok(RecoveryProgress::Pending),
            _ => Ok(RecoveryProgress::Invalid(Error::msg(
                "driver returned an invalid operation state or result",
            ))),
        }
    }
}

#[derive(Default)]
struct RecoveryErrors {
    submission: Option<Error>,
    query: Option<Error>,
}

impl RecoveryErrors {
    fn record_submission(&mut self, error: Error) -> Result<()> {
        if error
            .downcast_ref::<DriverRejection>()
            .is_some_and(DriverRejection::is_terminal)
        {
            return Err(error);
        }
        self.submission = Some(error);
        Ok(())
    }

    fn record_query(&mut self, error: Error) {
        self.query = Some(error);
    }

    fn unresolved(self, handle: OperationHandle) -> Error {
        let cause = match (self.submission, self.query) {
            (Some(first), Some(last)) => {
                Some(recovery_failed(first, "operation query also failed", last))
            }
            (first, last) => first.or(last),
        };
        cause
            .unwrap_or_else(|| Error::msg("operation remained pending past the recovery deadline"))
            .context(UnresolvedOutcome(handle))
    }
}

/// One recovery attempt's transport and retry schedule.
pub(super) trait RecoveryAttempt {
    fn submit(&mut self) -> Result<OperationResponse>;
    fn query(&mut self) -> Result<OperationResponse>;
    /// Wait before another attempt; return false when the deadline expires.
    fn wait_for_retry(&mut self) -> bool;
}

// The same algorithm is used by create and rollback/delete. The attempt owns
// the retry schedule so tests can exercise recovery without real transport or waits.
pub(super) fn resolve_operation(
    handle: OperationHandle,
    mutation: Operation,
    attempt: &mut impl RecoveryAttempt,
) -> Result<OperationResponse> {
    let mut action = RecoveryAction::Submit;
    let mut errors = RecoveryErrors::default();
    loop {
        let current = match action {
            RecoveryAction::Submit => match attempt.submit() {
                Ok(reply) => Ok(reply),
                Err(error) => {
                    errors.record_submission(error)?;
                    // An ambiguous submission is queried immediately, including
                    // after a resubmission, before checking the recovery deadline.
                    action = RecoveryAction::Query;
                    continue;
                }
            },
            RecoveryAction::Query => attempt.query(),
        };
        action = match current {
            Ok(reply) => match handle.interpret(reply, mutation)? {
                RecoveryProgress::Complete(reply) => return Ok(reply),
                // Only a matching Reserved response permits resubmission.
                RecoveryProgress::Reserved => RecoveryAction::Submit,
                RecoveryProgress::Pending => RecoveryAction::Query,
                RecoveryProgress::Invalid(error) => {
                    errors.record_query(error);
                    RecoveryAction::Query
                }
            },
            Err(error) => {
                errors.record_query(error);
                RecoveryAction::Query
            }
        };
        if !attempt.wait_for_retry() {
            return Err(errors.unresolved(handle));
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

pub(super) fn is_unresolved(error: &Error) -> bool {
    error.downcast_ref::<UnresolvedOutcome>().is_some()
        || error
            .downcast_ref::<RecoveryFailure>()
            .is_some_and(|failure| is_unresolved(&failure.cause))
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, io};

    use super::*;

    #[test]
    fn unresolved_rollback_retains_the_distinct_exit_status() {
        let rollback =
            Error::msg("delete transport unavailable").context(UnresolvedOutcome(handle()));
        let combined = recovery_failed(
            Error::msg("format failed"),
            "rollback also failed",
            rollback,
        );
        assert_eq!(crate::driver::error_exit_code(&combined), 3);
    }

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

    struct ScriptedRecovery {
        submissions: VecDeque<Result<OperationResponse>>,
        queries: VecDeque<Result<OperationResponse>>,
        waits: VecDeque<bool>,
        events: Vec<&'static str>,
    }

    impl ScriptedRecovery {
        fn new(
            submissions: impl IntoIterator<Item = Result<OperationResponse>>,
            queries: impl IntoIterator<Item = Result<OperationResponse>>,
            waits: impl IntoIterator<Item = bool>,
        ) -> Self {
            Self {
                submissions: submissions.into_iter().collect(),
                queries: queries.into_iter().collect(),
                waits: waits.into_iter().collect(),
                events: Vec::new(),
            }
        }

        fn resolve(&mut self) -> Result<OperationResponse> {
            resolve_operation(handle(), Operation::CreateDisk, self)
        }
    }

    impl RecoveryAttempt for ScriptedRecovery {
        fn submit(&mut self) -> Result<OperationResponse> {
            self.events.push("submit");
            self.submissions.pop_front().expect("unexpected submission")
        }

        fn query(&mut self) -> Result<OperationResponse> {
            self.events.push("query");
            self.queries.pop_front().expect("unexpected query")
        }

        fn wait_for_retry(&mut self) -> bool {
            self.events.push("wait");
            self.waits.pop_front().expect("unexpected retry wait")
        }
    }

    #[test]
    fn lost_create_response_recovers_original_disk_without_resubmitting() {
        let mut attempt = ScriptedRecovery::new(
            [Err(lost_response())],
            [Ok(reply(OperationState::Succeeded))],
            [],
        );
        let result = attempt.resolve().unwrap();
        assert_eq!(result.device_id, 1);
        assert_eq!(attempt.events, ["submit", "query"]);
    }

    #[test]
    fn only_reserved_operations_can_be_resubmitted() {
        let mut attempt = ScriptedRecovery::new(
            [Err(lost_response()), Ok(reply(OperationState::Succeeded))],
            [Ok(reply(OperationState::Reserved))],
            [true],
        );
        let result = attempt.resolve().unwrap();
        assert_eq!(result.operation_id, handle().id);
        assert_eq!(attempt.events, ["submit", "query", "wait", "submit"]);
    }

    #[test]
    fn lost_submissions_are_queried_before_waiting_and_reserved_retries_wait() {
        let mut attempt = ScriptedRecovery::new(
            [
                Err(Error::new(DriverRejection(
                    DriverRejection::STATUS_DEVICE_BUSY,
                ))),
                Err(lost_response()),
            ],
            [
                Ok(reply(OperationState::Reserved)),
                Ok(reply(OperationState::Pending)),
                Ok(reply(OperationState::Succeeded)),
            ],
            [true, true],
        );
        attempt.resolve().unwrap();
        assert_eq!(
            attempt.events,
            [
                "submit", "query", "wait", "submit", "query", "wait", "query"
            ]
        );
    }

    #[test]
    fn invalid_replies_are_queried_until_deadline_without_resubmitting() {
        let mut wrong_identity = reply(OperationState::Reserved);
        wrong_identity.operation_id += 1;
        let mut invalid_success = reply(OperationState::Succeeded);
        invalid_success.result_status = -1;
        let mut unknown_state = reply(OperationState::Reserved);
        unknown_state.state = u32::MAX;
        for invalid in [wrong_identity, invalid_success, unknown_state] {
            let mut attempt = ScriptedRecovery::new([Ok(invalid)], [Ok(invalid)], [true, false]);
            let error = attempt.resolve().unwrap_err();
            assert_eq!(attempt.events, ["submit", "wait", "query", "wait"]);
            assert!(error.downcast_ref::<UnresolvedOutcome>().is_some());
            assert!(
                format!("{error:#}").contains(if invalid.operation_id != handle().id {
                    "different identity"
                } else {
                    "invalid operation state or result"
                })
            );
        }
    }

    #[test]
    fn pending_operation_is_polled_without_resubmitting() {
        let mut attempt = ScriptedRecovery::new(
            [Ok(reply(OperationState::Pending))],
            [
                Ok(reply(OperationState::Pending)),
                Ok(reply(OperationState::Pending)),
                Ok(reply(OperationState::Succeeded)),
            ],
            [true, true, true],
        );
        attempt.resolve().unwrap();
        assert_eq!(
            attempt.events,
            ["submit", "wait", "query", "wait", "query", "wait", "query"]
        );
    }

    #[test]
    fn unreachable_query_preserves_transport_error_and_reports_unresolved_identity() {
        let mut attempt = ScriptedRecovery::new(
            [Err(lost_response())],
            [Err(Error::msg("query unavailable"))],
            [false],
        );
        let error = attempt.resolve().unwrap_err();
        assert_eq!(attempt.events, ["submit", "query", "wait"]);
        assert!(error.downcast_ref::<UnresolvedOutcome>().is_some());
        assert_eq!(crate::driver::error_exit_code(&error), 3);
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().raw_os_error(),
            Some(1117)
        );
        assert!(format!("{error:#}").contains("adapter instance 123 operation 7"));
        assert!(format!("{error:#}").contains("query unavailable"));
    }

    #[test]
    fn wrong_adapter_instance_cannot_be_adopted() {
        let mut other = reply(OperationState::Succeeded);
        other.adapter_instance = 456;
        let mut attempt = ScriptedRecovery::new([Err(lost_response())], [Ok(other)], [false]);
        let error = attempt.resolve().unwrap_err();
        assert!(error.downcast_ref::<UnresolvedOutcome>().is_some());
    }

    #[test]
    fn explicit_rejection_is_not_retried() {
        let mut attempt = ScriptedRecovery::new(
            [Err(Error::new(DriverRejection(0xC000_000Du32 as i32)))],
            [],
            [],
        );
        let error = attempt.resolve().unwrap_err();
        assert!(error.downcast_ref::<DriverRejection>().is_some());
        assert_eq!(attempt.events, ["submit"]);
    }

    #[test]
    fn terminal_failure_and_deleted_create_are_not_reexecuted() {
        for state in [OperationState::Failed, OperationState::Deleted] {
            let mut result = reply(state);
            result.result_status = 0xC000_009Au32 as i32;
            let mut attempt = ScriptedRecovery::new([Err(lost_response())], [Ok(result)], []);
            assert!(attempt.resolve().is_err());
            assert_eq!(attempt.events, ["submit", "query"]);
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
