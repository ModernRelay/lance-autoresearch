//! IMMUTABLE entry point, the single command the agent invokes per trial.
//!
//! Run with:  `cargo run --release --bin run_experiment -p posting-intersect > run.log 2>&1`
//! (or `-- --mode baseline` for the 3-pass baseline run).
//!
//! Two phases:
//!
//!   PHASE 1, CORRECTNESS.  For every (shape × distribution) in the correctness
//!   battery plus the four edge cases, run agent kernel and reference, assert
//!   the agent's `Vec<u32>` output is bitwise-identical to the reference's.
//!   Any single failure → exit 2.
//!
//!   PHASE 2, SPEED.  For every (shape × distribution) speed workload, build
//!   one agent kernel, then for each timing measurement run a batch of
//!   `SPEED_BATCH` intersects (across distinct instances) and divide the wall
//!   clock by the batch size to recover per-call ns. Bootstrap-CI the geomean
//!   across all measurements.
//!
//! Output (fixed format the agent can grep, mirrors `pq-l2`'s schema):
//!
//!     ---
//!     correctness:           pass | fail
//!     arch:                  aarch64 | x86_64
//!     passes:                1 | 3
//!     shapes_tested:         K=2 K=3 K=5
//!     distributions_tested:  balanced skewed dense
//!     geomean_ns_per_intersect:       18234
//!     geomean_ns_ci_90pct:            [17501, 19012]
//!     median_ns_per_intersect:        12345
//!     geomean_cycles_per_intersect:       54321 | n/a (no PMU access on this platform)
//!     geomean_instructions_per_intersect: ...   | n/a
//!     worst_ns_per_intersect:         24515 (K=5, dense)
//!     best_ns_per_intersect:          12876 (K=2, skewed)
//!     per_combo_geomean_ns:
//!       ...
//!     peak_mem_mb:           28.4
//!     total_seconds:         12.3
//!
//! Exit codes:
//!   0: both phases passed within time budget.
//!   2: correctness failure (agent kernel disagrees with reference).
//!   3: total wall-clock exceeded budget.
//!   1: any other error.

use std::time::Instant;

use harness_common::{
    PerfCounters, TIME_BUDGET_SECS, bootstrap_ci_geomean, geomean, median, peak_rss_mb,
};
use posting_intersect::PostingShape;
use posting_intersect::inputs::{
    DISTRIBUTIONS, DataDistribution, SHAPES, SPEED_BATCH, SpeedWorkload, correctness_battery,
    speed_workloads,
};
use posting_intersect::kernels::PostingIntersect;
use posting_intersect::reference::{intersect_reference, intersections_diff};

const CORRECTNESS_SEED: u64 = 0xC0FF_EEC0_DEBE_EFFE;
const SPEED_SEED: u64 = 0x5EED_F1AC_BABE_FACE;
const BOOTSTRAP_SEED: u64 = 0xB007_57AA_C1C1_DEAD;
const BOOTSTRAP_RESAMPLES: usize = 1000;

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
        "distributions_tested:  {}",
        DISTRIBUTIONS
            .iter()
            .map(format_dist)
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!("geomean_ns_per_intersect:       {}", report.geomean_ns);
    println!(
        "geomean_ns_ci_90pct:            [{}, {}]",
        report.geomean_ns_ci.0, report.geomean_ns_ci.1
    );
    println!("median_ns_per_intersect:        {}", report.median_ns);
    print_pmc("geomean_cycles_per_intersect", report.geomean_cycles);
    print_pmc(
        "geomean_instructions_per_intersect",
        report.geomean_instructions,
    );
    println!(
        "worst_ns_per_intersect:         {} ({}, {})",
        report.worst_ns,
        format_shape(&report.worst_shape),
        format_dist(&report.worst_dist)
    );
    println!(
        "best_ns_per_intersect:          {} ({}, {})",
        report.best_ns,
        format_shape(&report.best_shape),
        format_dist(&report.best_dist)
    );
    println!("per_combo_geomean_ns:");
    for combo in &report.per_combo {
        println!(
            "  {} {:<10} -> {} ns",
            format_shape(&combo.shape),
            format_dist(&combo.dist),
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
        Some(n) => println!("{stem:<32} {n}"),
        None => println!("{stem:<32} n/a (no PMU access on this platform)"),
    }
}

fn run_correctness() -> Result<(), String> {
    let cases = correctness_battery(CORRECTNESS_SEED);
    let mut agent = PostingIntersect::new();
    let mut agent_out: Vec<u32> = Vec::new();
    let mut ref_out: Vec<u32> = Vec::new();

    for case in &cases {
        let slices = case.set.as_slices();

        agent.intersect(&slices, &mut agent_out);
        intersect_reference(&slices, &mut ref_out);

        if let Some(diff) = intersections_diff(&agent_out, &ref_out) {
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
    dist: DataDistribution,
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
    worst_dist: DataDistribution,
    best_ns: u64,
    best_shape: PostingShape,
    best_dist: DataDistribution,
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
            let mut kernel = PostingIntersect::new();
            let mut out: Vec<u32> = Vec::new();

            // Pre-collect slice borrows once per instance to keep the timing
            // window dominated by the kernel itself.
            let instance_slices: Vec<Vec<&[u32]>> =
                wl.instances.iter().map(|s| s.as_slices()).collect();

            // Warmup: prime caches & let any scratch growth happen out of band.
            for slices in &instance_slices {
                kernel.intersect(slices, &mut out);
                std::hint::black_box(&out);
            }

            // Measurement: each timing window covers SPEED_BATCH back-to-back
            // intersects drawn from successive instances. Divide the wall
            // clock by SPEED_BATCH to recover per-call ns; this amortizes the
            // ~50 ns PerfCounters overhead across a window large enough that
            // the noise floor is well under the per-call cost.
            let n_inst = instance_slices.len();
            let n_windows = n_inst / SPEED_BATCH;
            for w in 0..n_windows {
                let base = w * SPEED_BATCH;
                perf.start();
                for k in 0..SPEED_BATCH {
                    kernel.intersect(&instance_slices[base + k], &mut out);
                    std::hint::black_box(&out);
                }
                let m = perf.stop();
                let per_call_ns = m.wall_clock_ns / SPEED_BATCH as u64;
                all_timings.push(per_call_ns);
                per_combo_timings[wi].push(per_call_ns);
                if let Some(c) = m.cycles {
                    all_cycles.push(c / SPEED_BATCH as u64);
                }
                if let Some(i) = m.instructions {
                    all_instr.push(i / SPEED_BATCH as u64);
                }
            }
        }
    }

    let mut per_combo: Vec<ComboReport> = Vec::with_capacity(workloads.len());
    let mut worst = (0u64, SHAPES[0], DISTRIBUTIONS[0]);
    let mut best = (u64::MAX, SHAPES[0], DISTRIBUTIONS[0]);
    for (wi, wl) in workloads.iter().enumerate() {
        let combo_geo = geomean(&per_combo_timings[wi]);
        per_combo.push(ComboReport {
            shape: wl.shape,
            dist: wl.distribution,
            geomean_ns: combo_geo,
        });
        if combo_geo > worst.0 {
            worst = (combo_geo, wl.shape, wl.distribution);
        }
        if combo_geo < best.0 {
            best = (combo_geo, wl.shape, wl.distribution);
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
        worst_dist: worst.2,
        best_ns: best.0,
        best_shape: best.1,
        best_dist: best.2,
        per_combo,
    }
}

fn format_shape(s: &PostingShape) -> String {
    format!("K={}", s.num_lists)
}

fn format_dist(d: &DataDistribution) -> String {
    match d {
        DataDistribution::Balanced => "balanced",
        DataDistribution::Skewed => "skewed",
        DataDistribution::Dense => "dense",
    }
    .to_string()
}
