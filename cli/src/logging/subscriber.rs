//! Formatting and ownership of the dedicated stdout writer.

use std::fmt::{self, Write};

use anyhow::Result;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tracing::{
    Event, Subscriber as TracingSubscriber,
    field::{Field, Visit},
    level_filters::LevelFilter,
};
use tracing_appender::non_blocking::{ErrorCounter, WorkerGuard};
use tracing_subscriber::{
    EnvFilter,
    fmt::{FmtContext, FormatEvent, FormatFields, format::Writer},
    registry::LookupSpan,
};

pub(super) struct Subscriber {
    _writer: WorkerGuard,
    dropped: ErrorCounter,
}

impl Subscriber {
    pub(super) fn install(level: LevelFilter) -> Result<Self> {
        let (writer, guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
            .buffered_lines_limit(8192)
            .thread_name("wispdisk-stdout")
            .finish(std::io::stdout());
        let dropped = writer.error_counter();
        // Explicit CLI selection controls both Rust filtering and ETW enablement.
        // Disable dependency noise while including ferrisetw's own warnings.
        let filter = EnvFilter::new(format!("off,wispdisk={level},ferrisetw=warn"));
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .event_format(LogFormatter)
            .with_writer(writer)
            .try_init()
            .map_err(anyhow::Error::from_boxed)?;
        Ok(Self {
            _writer: guard,
            dropped,
        })
    }

    pub(super) fn report_dropped_lines(&self) {
        let count = self.dropped.dropped_lines();
        if count != 0 {
            // The log queue may be full. Report directly, once, on stderr.
            eprintln!("warning: stdout logging dropped {count} lines");
        }
    }
}

struct LogFormatter;

impl<S, N> FormatEvent<S, N> for LogFormatter
where
    S: TracingSubscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _context: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let mut fields = LogFields::default();
        event.record(&mut fields);
        let timestamp = fields.event_time.unwrap_or_else(|| {
            OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_else(|_| "unknown-time".into())
        });
        let metadata = event.metadata();
        writeln!(
            writer,
            "{timestamp} {:5} {} {}",
            metadata.level(),
            metadata.target(),
            fields.text
        )
    }
}

#[derive(Default)]
struct LogFields {
    event_time: Option<String>,
    text: String,
}

impl Visit for LogFields {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "event_time" {
            self.event_time = Some(value.to_owned());
        } else {
            self.record_debug(field, &value);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if !self.text.is_empty() {
            self.text.push(' ');
        }
        if field.name() == "message" {
            let _ = write!(self.text, "{value:?}");
        } else {
            let _ = write!(self.text, "{}={value:?}", field.name());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing::info;

    #[test]
    fn preserves_original_driver_time_and_formats_cli_wall_clock() {
        #[derive(Clone)]
        struct Buffer(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Buffer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let output = Arc::new(Mutex::new(Vec::new()));
        let buffer = Buffer(output.clone());
        let subscriber = tracing_subscriber::fmt()
            .event_format(LogFormatter)
            .with_writer(move || buffer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            info!(target: "wispdisk::driver", event_time = "2026-10-16T11:32:07.126Z", operation_id = 42, "Created");
            info!("Command finished");
        });
        let bytes = output.lock().unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        let mut lines = text.lines();
        let driver = lines.next().unwrap();
        assert!(driver.starts_with("2026-10-16T11:32:07.126Z INFO"));
        assert!(driver.contains("operation_id=42"));
        assert!(!driver.contains("event_time="));
        let cli = lines.next().unwrap();
        assert!(OffsetDateTime::parse(cli.split_whitespace().next().unwrap(), &Rfc3339).is_ok());
        assert!(cli.contains(concat!(module_path!(), " Command finished")));
    }
}
