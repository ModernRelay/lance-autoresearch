//! IMMUTABLE entry point, the single command the agent invokes per trial.
//!
//! Run with:  `cargo run --release --bin run_experiment -p posting-seek > run.log 2>&1`
//! (or `-- --mode baseline` for the 3-pass baseline run).
//!
//! Two phases:
//!
//!   PHASE 1, CORRECTNESS. For every (shape × edge-kind) in the
//!   correctness battery, run agent kernel and reference through the
//!   same ops sequence; assert agent's `Option<u32>` output matches
//!   reference's bitwise on every Seek op. Any divergence → exit 2.
//!
//!   PHASE 2, SPEED. For every (shape × seek-pattern) speed workload,
//!   build one agent kernel per pass; replay the ops sequence; time
//!   each `Seek` in batches of `BATCH` calls (Reset ops untimed).
//!   Bootstrap-CI the geomean across all timed-batch measurements.
//!
//! Output (fixed-format, mirrors pq-l2 / posting-intersect schemas).
//!
//! Exit codes: 0 success; 2 correctness fail; 3 time-budget breach;
//! 1 other.

use std::time::Instant;

use harness_common::{
    PerfCounters, TIME_BUDGET_SECS, bootstrap_ci_geomean, geomean, median, peak_rss_mb,
};
use posting_seek::PostingShape;
use posting_seek::inputs::{
    NUM_SEEKS_PER_COMBO, PATTERNS, SHAPES, SeekOp, SeekPattern, SpeedWorkload,
    correctness_battery, speed_workloads,
};
use posting_seek::kernels::PostingSeek;
use posting_seek::reference::{PostingSeekReference, seek_results_diff};

const CORRECTNESS_SEED: u64 = 0xC0FF_EEC0_DEBE_EFFE;
const SPEED_SEED: u64 = 0x5EED_F1AC_BABE_FACE;
const BOOTSTRAP_SEED: u64 = 0xB007_57AA_C1C1_DEAD;
const BOOTSTRAP_RESAMPLES: usize = 1000;

/// Per-call cost on Sequential at small list sizes is on the order of
/// tens of nanoseconds — comparable to `PerfCounters` start/stop
/// overhead (~50 ns). Batching N seeks per timing window amortizes that.
const BATCH: usize = 32;

fn main() {
    let start = Instant::now();

    let args: Vec<String> = std::env::args().collect();
    let passes: usize = if args.iter().any(|s| s == "--mode" || s == "-m")
        && args
            .windows(2)
            .any(|w| (w[0] == "--mode" || w[0] == "-m") && w[1] == "baseline")
    {
        3
    } else {
        1
    };

    if let Err(e) = run_correctness() {
        eprintln!("---");
        eprintln!("correctness:           fail");
        eprintln!("first_failure:         {e}");
        eprintln!("total_seconds:         {:.2}", start.elapsed().as_secs_f64());
        std::process::exit(2);
    }
    println!("correctness:           pass");

    let workloads = speed_workloads(SPEED_SEED);
    let report = run_speed(&workloads, passes);

    let elapsed = start.elapsed();
    let mem_mb = peak_rss_mb();

    println!("---");
    println!("correctness:           pass");
    println!("arch:                  {}", std::env::consts::ARCH);
    println!("passes:                {passes}");
    println!(
        "shapes_tested:         {}",
        SHAPES
            .iter()
            .map(format_shape)
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!(
        "patterns_tested:       {}",
        PATTERNS
            .iter()
            .map(format_pattern)
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!("seeks_per_combo:       {NUM_SEEKS_PER_COMBO}");
    println!("geomean_ns_per_seek:        {}", report.geomean_ns);
    println!(
        "geomean_ns_ci_90pct:        [{}, {}]",
        report.geomean_ns_ci.0, report.geomean_ns_ci.1
    );
    println!("median_ns_per_seek:         {}", report.median_ns);
    print_pmc("geomean_cycles_per_seek", report.geomean_cycles);
    print_pmc("geomean_instructions_per_seek", report.geomean_instructions);
    println!(
        "worst_ns_per_seek:          {} ({}, {})",
        report.worst_ns,
        format_shape(&report.worst_shape),
        format_pattern(&report.worst_pattern)
    );
    println!(
        "best_ns_per_seek:           {} ({}, {})",
        report.best_ns,
        format_shape(&report.best_shape),
        format_pattern(&report.best_pattern)
    );
    println!("per_combo_geomean_ns:");
    for combo in &report.per_combo {
        println!(
            "  {} {:<14} -> {} ns",
            format_shape(&combo.shape),
            format_pattern(&combo.pattern),
            combo.geomean_ns
        );
    }
    println!("peak_mem_mb:           {mem_mb:.1}");
    println!("total_seconds:         {:.2}", elapsed.as_secs_f64());

    if elapsed.as_secs() > TIME_BUDGET_SECS {
        eprintln!(
            "FAIL: total wall-clock {}s exceeds budget {}s",
            elapsed.as_secs(),
            TIME_BUDGET_SECS
        );
        std::process::exit(3);
    }
}

fn print_pmc(label: &str, v: Option<u64>) {
    let stem = format!("{label}:");
    match v {
        Some(n) => println!("{stem:<28} {n}"),
        None => println!("{stem:<28} n/a (no PMU access on this platform)"),
    }
}

fn run_correctness() -> Result<(), String> {
    let cases = correctness_battery(CORRECTNESS_SEED);

    for case in &cases {
        let mut agent = PostingSeek::new(&case.list);
        let mut reference = PostingSeekReference::new(&case.list);
        let mut agent_results: Vec<Option<u32>> = Vec::new();
        let mut ref_results: Vec<Option<u32>> = Vec::new();

        for op in &case.ops {
            match *op {
                SeekOp::Reset => {
                    agent.reset();
                    reference.reset();
                }
                SeekOp::Seek(least_id) => {
                    agent_results.push(agent.next(least_id));
                    ref_results.push(reference.next(least_id));
                }
            }
        }

        if let Some(diff) = seek_results_diff(&agent_results, &ref_results) {
            return Err(format!(
                "case={}/{} {}",
                format_shape(&case.shape),
                case.label,
                diff
            ));
        }
    }
    Ok(())
}

struct ComboReport {
    shape: PostingShape,
    pattern: SeekPattern,
    geomean_ns: u64,
}

struct SpeedReport {
    geomean_ns: u64,
    geomean_ns_ci: (u64, u64),
    median_ns: u64,
    geomean_cycles: Option<u64>,
    geomean_instructions: Option<u64>,
    worst_ns: u64,
    worst_shape: PostingShape,
    worst_pattern: SeekPattern,
    best_ns: u64,
    best_shape: PostingShape,
    best_pattern: SeekPattern,
    per_combo: Vec<ComboReport>,
}

fn run_speed(workloads: &[SpeedWorkload], passes: usize) -> SpeedReport {
    let mut all_timings: Vec<u64> = Vec::new();
    let mut all_cycles: Vec<u64> = Vec::new();
    let mut all_instr: Vec<u64> = Vec::new();
    let mut per_combo_timings: Vec<Vec<u64>> = vec![Vec::new(); workloads.len()];

    let mut perf = PerfCounters::new();
    let pmc_available = perf.has_pmc();

    for _pass in 0..passes {
        for (wi, wl) in workloads.iter().enumerate() {
            let mut kernel = PostingSeek::new(&wl.list);

            // Warmup: replay once untimed to prime caches.
            for op in &wl.ops {
                match *op {
                    SeekOp::Reset => kernel.reset(),
                    SeekOp::Seek(id) => {
                        std::hint::black_box(kernel.next(id));
                    }
                }
            }
            kernel.reset();

            // Measurement: time batches of BATCH consecutive Seeks. Reset
            // ops are executed between batches but untimed. A "batch
            // window" times exactly BATCH Seek calls; we recover per-call
            // ns by dividing wall-clock by BATCH.
            let mut batch_buf: Vec<u32> = Vec::with_capacity(BATCH);
            let mut i = 0;
            while i < wl.ops.len() {
                // Collect up to BATCH consecutive Seek targets, executing
                // any intervening Reset ops outside the timing window.
                batch_buf.clear();
                while i < wl.ops.len() && batch_buf.len() < BATCH {
                    match wl.ops[i] {
                        SeekOp::Reset => {
                            // If we already have queued seeks, time them
                            // first before resetting.
                            if !batch_buf.is_empty() {
                                break;
                            }
                            kernel.reset();
                            i += 1;
                        }
                        SeekOp::Seek(id) => {
                            batch_buf.push(id);
                            i += 1;
                        }
                    }
                }
                if batch_buf.is_empty() {
                    continue;
                }

                perf.start();
                for &id in &batch_buf {
                    std::hint::black_box(kernel.next(id));
                }
                let m = perf.stop();

                let per_call_ns = m.wall_clock_ns / batch_buf.len() as u64;
                all_timings.push(per_call_ns);
                per_combo_timings[wi].push(per_call_ns);
                if let Some(c) = m.cycles {
                    all_cycles.push(c / batch_buf.len() as u64);
                }
                if let Some(ins) = m.instructions {
                    all_instr.push(ins / batch_buf.len() as u64);
                }
            }
        }
    }

    let mut per_combo: Vec<ComboReport> = Vec::with_capacity(workloads.len());
    let mut worst = (0u64, SHAPES[0], PATTERNS[0]);
    let mut best = (u64::MAX, SHAPES[0], PATTERNS[0]);
    for (wi, wl) in workloads.iter().enumerate() {
        let combo_geo = geomean(&per_combo_timings[wi]);
        per_combo.push(ComboReport {
            shape: wl.shape,
            pattern: wl.pattern,
            geomean_ns: combo_geo,
        });
        if combo_geo > worst.0 {
            worst = (combo_geo, wl.shape, wl.pattern);
        }
        if combo_geo < best.0 {
            best = (combo_geo, wl.shape, wl.pattern);
        }
    }

    SpeedReport {
        geomean_ns: geomean(&all_timings),
        geomean_ns_ci: bootstrap_ci_geomean(&all_timings, BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED),
        median_ns: median(&all_timings),
        geomean_cycles: pmc_available.then(|| geomean(&all_cycles)),
        geomean_instructions: pmc_available.then(|| geomean(&all_instr)),
        worst_ns: worst.0,
        worst_shape: worst.1,
        worst_pattern: worst.2,
        best_ns: best.0,
        best_shape: best.1,
        best_pattern: best.2,
        per_combo,
    }
}

fn format_shape(s: &PostingShape) -> String {
    match s.num_blocks {
        100 => "Small".to_string(),
        10_000 => "Medium".to_string(),
        80_000 => "Large".to_string(),
        n => format!("blocks={n}"),
    }
}

fn format_pattern(p: &SeekPattern) -> String {
    match p {
        SeekPattern::Sequential => "sequential",
        SeekPattern::SkipShallow => "skip_shallow",
        SeekPattern::SkipDeep => "skip_deep",
    }
    .to_string()
}
