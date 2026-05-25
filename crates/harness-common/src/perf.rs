// SPDX-License-Identifier: Apache-2.0

//! Per-measurement performance counters.
//!
//! Wraps wall-clock plus (Linux-only) `perf_event_open` `INSTRUCTIONS_RETIRED`
//! and `CPU_CYCLES`. On macOS / other platforms the counters are `None` and
//! only wall-clock is reported. Per-call overhead is on the order of ~50ns,
//! negligible at our 200k-1.5M ns/query scale.
//!
//! The PMC layer matters because wall-clock noise is ~4% trial-to-trial on
//! M1 Max, well above the harness's 1% keep-gate assumption. `cycles` on
//! Linux gives ~0.01% noise and is the right gate for trial-to-trial
//! decisions. Wall-clock remains the user-visible truth and stays in the
//! report; cycles is the engineer's truth.
//!
//! On non-Linux the keep-gate falls back to bootstrap-CI on wall-clock
//! (see `stats::bootstrap_ci`).

use std::time::Instant;

/// One measurement: wall-clock always; PMC fields populated on Linux when
/// the kernel grants `perf_event_open` access (no CAP_PERFMON needed unless
/// `/proc/sys/kernel/perf_event_paranoid > 1`).
#[derive(Clone, Copy, Debug, Default)]
pub struct PerfMeasurement {
    pub wall_clock_ns: u64,
    pub instructions: Option<u64>,
    pub cycles: Option<u64>,
}

/// Scoped counters for a single measurement window.
pub struct PerfCounters {
    wall_start: Option<Instant>,
    #[cfg(target_os = "linux")]
    pmc: Option<linux::Pmc>,
}

impl Default for PerfCounters {
    fn default() -> Self {
        Self::new()
    }
}

impl PerfCounters {
    pub fn new() -> Self {
        Self {
            wall_start: None,
            #[cfg(target_os = "linux")]
            pmc: linux::Pmc::try_new(),
        }
    }

    /// Returns whether hardware perf counters are available in this process.
    /// `false` on macOS, `false` on Linux when `perf_event_open` is denied.
    pub fn has_pmc(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.pmc.is_some()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    /// Start measurement. Resets PMC delta baseline and starts the wall clock.
    pub fn start(&mut self) {
        self.wall_start = Some(Instant::now());
        #[cfg(target_os = "linux")]
        if let Some(pmc) = self.pmc.as_mut() {
            pmc.snapshot_start();
        }
    }

    /// Stop measurement and return the delta since `start`. Idempotent if
    /// called twice (second call returns zeros).
    pub fn stop(&mut self) -> PerfMeasurement {
        let wall_clock_ns = self
            .wall_start
            .take()
            .map(|t| t.elapsed().as_nanos() as u64)
            .unwrap_or(0);
        #[cfg(target_os = "linux")]
        let (instructions, cycles) = match self.pmc.as_mut() {
            Some(pmc) => pmc.delta_since_start(),
            None => (None, None),
        };
        #[cfg(not(target_os = "linux"))]
        let (instructions, cycles) = (None, None);
        PerfMeasurement {
            wall_clock_ns,
            instructions,
            cycles,
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use perf_event::events::Hardware;
    use perf_event::{Builder, Counter};

    pub struct Pmc {
        instructions: Counter,
        cycles: Counter,
        instr_start: u64,
        cycles_start: u64,
    }

    impl Pmc {
        pub fn try_new() -> Option<Self> {
            let mut instructions = Builder::new().kind(Hardware::INSTRUCTIONS).build().ok()?;
            let mut cycles = Builder::new().kind(Hardware::CPU_CYCLES).build().ok()?;
            instructions.enable().ok()?;
            cycles.enable().ok()?;
            Some(Self {
                instructions,
                cycles,
                instr_start: 0,
                cycles_start: 0,
            })
        }

        pub fn snapshot_start(&mut self) {
            self.instr_start = self.instructions.read().unwrap_or(0);
            self.cycles_start = self.cycles.read().unwrap_or(0);
        }

        pub fn delta_since_start(&mut self) -> (Option<u64>, Option<u64>) {
            let i = self.instructions.read().ok().map(|v| v.saturating_sub(self.instr_start));
            let c = self.cycles.read().ok().map(|v| v.saturating_sub(self.cycles_start));
            (i, c)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_clock_always_populated() {
        let mut p = PerfCounters::new();
        p.start();
        // sleep guarantees the wall clock advances even on extremely fast
        // pipelines under aggressive LLVM DCE (in release builds an
        // arithmetic loop with black_box can compile down to nothing).
        std::thread::sleep(std::time::Duration::from_micros(100));
        let m = p.stop();
        assert!(
            m.wall_clock_ns >= 100_000,
            "wall_clock_ns must be at least the sleep duration (100us), got {}",
            m.wall_clock_ns
        );
    }

    #[test]
    fn double_stop_returns_zero() {
        let mut p = PerfCounters::new();
        p.start();
        let _ = p.stop();
        let m2 = p.stop();
        assert_eq!(m2.wall_clock_ns, 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_attempts_pmc() {
        // Best-effort: may return false if perf_event_open is denied in CI sandbox.
        // The test passes either way; we just exercise the path.
        let p = PerfCounters::new();
        let _ = p.has_pmc();
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn non_linux_reports_no_pmc() {
        let p = PerfCounters::new();
        assert!(!p.has_pmc());
    }
}
