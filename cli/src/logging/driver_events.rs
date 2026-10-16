//! Owns one ordinary ETW session, its callback state, and its processing thread.

use std::{
    fmt,
    thread::{self, JoinHandle},
};

use anyhow::{Context, Result, anyhow};
use ferrisetw::{
    EventRecord, SchemaLocator,
    native::EvntraceNativeError,
    parser::Parser,
    provider::Provider,
    trace::{TraceError, TraceTrait, UserTrace, stop_trace_by_name},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tracing::{event, level_filters::LevelFilter, warn};

// Same stable provider GUID as driver/src/logging.cpp.
const DRIVER_GUID: &str = "8b6cae34-6a65-4cf1-a55b-0c9a5c224896";

pub(super) struct DriverEvents {
    name: String,
    session: Option<UserTrace>,
    worker: Option<JoinHandle<Result<(), EtwFailure>>>,
}

impl DriverEvents {
    pub(super) fn start(level: LevelFilter) -> Result<Self> {
        let name = format!(
            "WispDisk-{}-{}",
            std::process::id(),
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        );
        let dispatch = tracing::dispatcher::get_default(Clone::clone);
        let provider = Provider::by_guid(DRIVER_GUID)
            .any(1)
            .level(etw_level(level))
            .add_callback(move |record, schemas| {
                tracing::dispatcher::with_default(&dispatch, || on_event(record, schemas));
            })
            .build();
        // UserTrace is an ordinary provider session. KernelTrace is for system
        // kernel logger providers, not our custom kernel-mode TraceLogging GUID.
        let (session, handle) = UserTrace::new()
            .named(name.clone())
            .enable(provider)
            .start()
            .map_err(EtwFailure)
            .context("start driver ETW session")?;
        let worker = thread::Builder::new()
            .name("wispdisk-etw".into())
            .spawn(move || UserTrace::process_from_handle(handle).map_err(EtwFailure))
            .context("start driver ETW consumer thread")?;
        Ok(Self {
            name,
            session: Some(session),
            worker: Some(worker),
        })
    }

    pub(super) fn finish(&mut self) -> Result<()> {
        let Some(session) = self.session.take() else {
            return Ok(());
        };
        // ferrisetw::stop closes the consumer first, discarding callbacks for
        // buffered events. Stop the session first and let ProcessTrace drain.
        let stopped = stop_trace_by_name(&self.name).map_err(EtwFailure);
        if let Err(error) = stopped {
            // Closing the consumer is a fallback when stopping failed. Only
            // join after a successful stop/close, so cleanup cannot hang on a
            // still-running session. Callback ownership remains in ferrisetw.
            if session.stop().map_err(EtwFailure).is_ok() {
                let _ = self.join();
            } else {
                self.worker.take();
            }
            return Err(error).context("stop driver ETW session");
        }
        let result = self.join();
        drop(session); // CloseTrace and release callback state after the drain.
        result
    }

    fn join(&mut self) -> Result<()> {
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| anyhow!("driver ETW consumer panicked"))?
                .context("process driver ETW events")?;
        }
        Ok(())
    }
}

impl Drop for DriverEvents {
    fn drop(&mut self) {
        if let Err(error) = self.finish() {
            warn!(error = %format_args!("{error:#}"), "Driver tracing cleanup failed");
        }
    }
}

// ferrisetw's error types do not implement std::error::Error. Keep their typed
// value and expose the underlying OS error instead of discarding it in a string.
#[derive(Debug)]
struct EtwFailure(TraceError);

impl fmt::Display for EtwFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ETW error: {:?}", self.0)
    }
}

impl std::error::Error for EtwFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.0 {
            TraceError::EtwNativeError(EvntraceNativeError::IoError(error)) => Some(error),
            _ => None,
        }
    }
}

fn etw_level(level: LevelFilter) -> u8 {
    match level {
        LevelFilter::ERROR => 2,
        LevelFilter::WARN => 3,
        LevelFilter::INFO => 4,
        _ => 5,
    }
}

struct DriverEvent {
    timestamp: String,
    message: String,
    adapter: u64,
    operation: u64,
    device: u32,
    status: i32,
    size_bytes: u64,
    srb_status: u32,
    storport_status: u32,
}

impl DriverEvent {
    fn decode(record: &EventRecord, schemas: &SchemaLocator) -> Result<Self> {
        let schema = schemas
            .event_schema(record)
            .map_err(|e| anyhow!("driver event schema: {e:?}"))?;
        let parser = Parser::create(record, &schema);
        // The default consumer mode converts QPC timestamps to FILETIME.
        // ferrisetw 1.2's timestamp() duplicates the high FILETIME word and
        // loses the low word. Convert the intact raw timestamp centrally.
        let timestamp = event_time(record.raw_timestamp())?.format(&Rfc3339)?;
        macro_rules! field {
            ($name:literal) => {
                parser
                    .try_parse($name)
                    .map_err(|e| anyhow!("driver event field {}: {e:?}", $name))?
            };
        }
        Ok(Self {
            timestamp,
            message: field!("Message"),
            adapter: field!("AdapterInstance"),
            operation: field!("OperationId"),
            device: field!("DeviceId"),
            status: field!("Status"),
            size_bytes: field!("SizeBytes"),
            srb_status: field!("SrbStatus"),
            storport_status: field!("StorPortStatus"),
        })
    }

    fn emit(&self, level: u8) {
        macro_rules! emit {
            ($level:expr) => { event!(target: "wispdisk::driver", $level,
                event_time = self.timestamp.as_str(), adapter_instance = self.adapter,
                operation_id = self.operation, device_id = self.device,
                status = %format_args!("0x{:08X}", self.status as u32), size_bytes = self.size_bytes,
                srb_status = self.srb_status, storport_status = self.storport_status,
                "{}", self.message) };
        }
        match level {
            1 | 2 => emit!(tracing::Level::ERROR),
            3 => emit!(tracing::Level::WARN),
            4 => emit!(tracing::Level::INFO),
            _ => emit!(tracing::Level::DEBUG),
        }
    }
}

fn on_event(record: &EventRecord, schemas: &SchemaLocator) {
    match DriverEvent::decode(record, schemas) {
        Ok(event) => event.emit(record.level()),
        Err(error) => warn!(%error, "Cannot decode driver event"),
    }
}

fn event_time(filetime: i64) -> Result<OffsetDateTime, time::error::ComponentRange> {
    // ETW's default consumer mode supplies 100 ns ticks since 1601-01-01.
    // Do not enable PROCESS_TRACE_MODE_RAW_TIMESTAMP with this conversion.
    const UNIX_EPOCH_FILETIME: i128 = 116_444_736_000_000_000;
    OffsetDateTime::from_unix_timestamp_nanos((i128::from(filetime) - UNIX_EPOCH_FILETIME) * 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_filetime_epoch_and_preserves_low_word_precision() {
        let epoch = 116_444_736_000_000_000;
        assert_eq!(event_time(epoch).unwrap(), OffsetDateTime::UNIX_EPOCH);
        assert_eq!(event_time(epoch + 1).unwrap().unix_timestamp_nanos(), 100);
        assert_eq!(
            event_time(epoch + 10_000).unwrap().unix_timestamp_nanos(),
            1_000_000
        );
        assert_eq!(event_time(epoch - 1).unwrap().unix_timestamp_nanos(), -100);
    }
}
