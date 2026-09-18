//! Process-wide, opt-in timing for the CLI's sequential command phases.

use std::{
    io::{self, Write},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

static TIMING: PhaseTiming = PhaseTiming::new();

pub(crate) fn init(enabled: bool) {
    TIMING.init(enabled);
}

pub(crate) fn mark(phase: &str) {
    TIMING.mark(phase);
}

// Owning the slot lets tests use independent instances rather than resetting
// or enabling the process-wide timer shared by the test harness.
struct PhaseTiming {
    timer: OnceLock<Mutex<PhaseTimer>>,
}

impl PhaseTiming {
    const fn new() -> Self {
        Self {
            timer: OnceLock::new(),
        }
    }

    fn init(&self, enabled: bool) {
        if enabled {
            self.timer.get_or_init(|| Mutex::new(PhaseTimer::new()));
        }
    }

    fn mark(&self, phase: &str) {
        let Some(timer) = self.timer.get() else {
            return;
        };
        // Diagnostic failures must not interrupt disk creation or rollback.
        let Ok(mut timer) = timer.lock() else {
            return;
        };
        let (elapsed, total) = timer.measure(Instant::now());
        let _ = write_measurement(&mut io::stderr().lock(), phase, elapsed, total);
    }
}

struct PhaseTimer {
    started: Instant,
    phase_started: Instant,
}

impl PhaseTimer {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            phase_started: now,
        }
    }

    fn measure(&mut self, now: Instant) -> (Duration, Duration) {
        let elapsed = now.duration_since(self.phase_started);
        let total = now.duration_since(self.started);
        self.phase_started = now;
        (elapsed, total)
    }
}

fn write_measurement(
    output: &mut impl Write,
    phase: &str,
    elapsed: Duration,
    total: Duration,
) -> io::Result<()> {
    writeln!(
        output,
        "timing: {phase}: {:.3} ms (total {:.3} ms)",
        elapsed.as_secs_f64() * 1_000.0,
        total.as_secs_f64() * 1_000.0,
    )
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use super::*;

    #[test]
    fn disabled_timing_does_not_initialize_on_marks() {
        let timing = PhaseTiming::new();
        timing.init(false);
        timing.mark("disabled phase");
        assert!(timing.timer.get().is_none());
    }

    #[test]
    fn enabling_timing_initializes_the_timer() {
        let timing = PhaseTiming::new();
        timing.init(true);
        let timer = timing.timer.get().unwrap().lock().unwrap();
        assert_eq!(timer.started, timer.phase_started);
    }

    #[test]
    fn repeated_initialization_does_not_restart_timing() {
        let timing = PhaseTiming::new();
        timing.init(true);
        let (started, last_phase) = {
            let mut timer = timing.timer.get().unwrap().lock().unwrap();
            let started = timer.started;
            let last_phase = started + Duration::from_millis(2);
            timer.measure(last_phase);
            (started, last_phase)
        };

        timing.init(true);
        let timer = timing.timer.get().unwrap().lock().unwrap();
        assert_eq!(timer.started, started);
        assert_eq!(timer.phase_started, last_phase);
    }

    #[test]
    fn measures_each_phase_and_total_from_the_initial_start() {
        let started = Instant::now();
        let mut timer = PhaseTimer {
            started,
            phase_started: started,
        };
        let first = Duration::from_micros(1_500);
        let total = Duration::from_micros(4_250);
        assert_eq!(timer.measure(started + first), (first, first));
        assert_eq!(timer.measure(started + total), (total - first, total));
    }

    #[test]
    fn retains_the_existing_timing_output_format() {
        let mut output = Vec::new();
        write_measurement(
            &mut output,
            "quick-format NTFS",
            Duration::from_micros(1_500),
            Duration::from_micros(4_250),
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "timing: quick-format NTFS: 1.500 ms (total 4.250 ms)\n"
        );
    }

    #[test]
    fn a_poisoned_timer_does_not_fail_an_operation() {
        let timing = PhaseTiming::new();
        timing.init(true);
        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _guard = timing.timer.get().unwrap().lock().unwrap();
            panic!("poison the diagnostic lock");
        }));
        assert!(panic.is_err());
        timing.mark("phase after diagnostic failure");
        assert!(timing.timer.get().unwrap().is_poisoned());
    }
}
