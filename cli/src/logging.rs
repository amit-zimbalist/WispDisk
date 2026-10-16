//! Process-wide diagnostics. The command boundary owns background resources.

mod driver_events;
mod subscriber;

use anyhow::Result;
use tracing::level_filters::LevelFilter;

pub(crate) struct Logging {
    // Drop the consumer before flushing its output on the writer thread.
    driver_events: Option<driver_events::DriverEvents>,
    subscriber: subscriber::Subscriber,
}

impl Logging {
    pub(crate) fn start(level: LevelFilter) -> Result<Self> {
        let subscriber = subscriber::Subscriber::install(level)?;
        let driver_events = driver_events::DriverEvents::start(level)?;
        Ok(Self {
            driver_events: Some(driver_events),
            subscriber,
        })
    }
}

impl Drop for Logging {
    fn drop(&mut self) {
        if let Some(mut events) = self.driver_events.take() {
            if let Err(error) = events.finish() {
                eprintln!("warning: logging cleanup failed: {error:#}");
            }
        }
        self.subscriber.report_dropped_lines();
    }
}
