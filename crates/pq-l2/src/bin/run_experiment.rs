//! IMMUTABLE entry point, the single command the agent invokes per trial.
//!
//! Run with:  `cargo run --release --bin run_experiment -p pq-l2 > run.log 2>&1`
//! (or `-- --mode baseline` for the 3-pass baseline run).
//!
//! Two phases:
//!
//!   PHASE 1, CORRECTNESS.  For every (shape × input distribution) in the
//!   correctness battery, build the agent kernel and the upstream-vendored
//!   reference, compare distance tables and per-vector distances. Both
//!   max-abs-err must be ≤ MAX_ABS_ERR. Any single failure → exit 2.
//!
//!   PHASE 2, SPEED.  For every (shape × data distribution) speed workload,
//!   build the agent kernel once, then time `distance_table +
//!   compute_distances + top-K select` for each query. Report per-(shape ×
//!   distribution) geomean ns, plus global geomean / worst / best across all
//!   timed queries, plus a bootstrap 90% CI on the global geomean.
//!
//! Output (fixed format the agent can grep):
//!
//!     ---
//!     correctness:           pass | fail
//!     arch:                  aarch64 | x86_64
//!     passes:                1 | 3
//!     shapes_tested:         (128,16,256) (256,16,256) (768,96,256)
//!     distributions_tested:  clustered uniform sparse
//!     geomean_ns_per_query:  18234
//!     geomean_ns_ci_90pct:   [17501, 19012]
//!     median_ns_per_query:   12345
//!     geomean_cycles_per_query: 54321 | n/a (no PMU access on this platform)
//!     worst_ns_per_query:    24515 (768x96, sparse)
//!     best_ns_per_query:     12876 (128x16, clustered)
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
    MAX_ABS_ERR, PerfCounters, TIME_BUDGET_SECS, bootstrap_ci_geomean, bootstrap_ci_paired_ratio,
    geomean, median, peak_rss_mb,
};
use pq_l2::inputs::{
    DISTRIBUTIONS, DataDistribution, SHAPES, SpeedWorkload, correctness_battery, speed_workloads,
};
use pq_l2::kernels::PqKernel;
use pq_l2::reference::{ScalarReference, distances_max_abs_err, max_abs_err};
use pq_l2::PqShape;

// Any constants; the only requirement is that they're pinned across trials so
// the inputs and the timings are reproducible.
const CORRECTNESS_SEED: u64 = 0xC0FF_EEC0_DEBE_EFFE;
const SPEED_SEED: u64 = 0x5EED_F1AC_BABE_FACE;
const BOOTSTRAP_SEED: u64 = 0xB007_57AA_C1C1_DEAD;
const BOOTSTRAP_RESAMPLES: usize = 1000;

fn main() {
    let start = Instant::now();

    // Argv parsing: --mode baseline runs the speed phase 3× and bundles all
    // per-query samples for a tighter bootstrap CI. Default mode runs once
    // (the trial-iteration cadence). The keep-gate in HARNESS.md compares
    // trial CI against baseline CI; baseline gets the tight reference, trial
    // CIs are wider but still expected to clear by margin on real wins.
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
    println!("geomean_ns_per_query:  {}", report.geomean_ns);
    println!(
        "geomean_ns_ci_90pct:   [{}, {}]",
        report.geomean_ns_ci.0, report.geomean_ns_ci.1
    );
    println!("median_ns_per_query:   {}", report.median_ns);
    print_pmc("geomean_cycles_per_query", report.geomean_cycles);
    print_pmc("geomean_instructions_per_query", report.geomean_instructions);
    // Paired (interleaved with reference per query) measurement: this is
    // the keep-gate-grade number because it eliminates cross-session
    // calibration drift. Ratio < 1.0 means agent is faster than upstream.
    println!(
        "reference_geomean_ns_per_query: {}",
        report.reference_geomean_ns
    );
    println!(
        "paired_ratio_agent_over_ref:    {:.4}",
        report.paired_ratio
    );
    println!(
        "paired_ratio_ci_90pct:          [{:.4}, {:.4}]",
        report.paired_ratio_ci.0, report.paired_ratio_ci.1
    );
    let paired_speedup_pct = (1.0 - report.paired_ratio) * 100.0;
    let paired_speedup_lo = (1.0 - report.paired_ratio_ci.1) * 100.0;
    let paired_speedup_hi = (1.0 - report.paired_ratio_ci.0) * 100.0;
    println!(
        "paired_speedup_pct:             {:+.2}% (CI [{:+.2}%, {:+.2}%])",
        paired_speedup_pct, paired_speedup_lo, paired_speedup_hi
    );
    println!(
        "worst_ns_per_query:    {} ({}, {})",
        report.worst_ns,
        format_shape(&report.worst_shape),
        format_dist(&report.worst_dist)
    );
    println!(
        "best_ns_per_query:     {} ({}, {})",
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
    // Pad the "label:" portion to 22 chars so values align with other fields.
    let stem = format!("{label}:");
    match v {
        Some(n) => println!("{stem:<22} {n}"),
        None => println!("{stem:<22} n/a (no PMU access on this platform)"),
    }
}

/// Select top-K smallest distances. Scans `distances` once, maintains a
/// max-heap of capacity K (smaller-than-root is the admission test).
/// Lives in the harness (not the kernel) to match upstream's split:
/// kernel computes per-vector distances, top-K selection is external.
fn select_top_k(distances: &[f32], k: usize, out: &mut Vec<(u32, f32)>) {
    out.clear();
    if k == 0 {
        return;
    }
    for (i, &d) in distances.iter().enumerate() {
        if out.len() < k {
            out.push((i as u32, d));
            if out.len() == k {
                heapify(out);
            }
        } else if d < out[0].1 {
            out[0] = (i as u32, d);
            sift_down(out, 0);
        }
    }
    out.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
}

fn heapify(entries: &mut [(u32, f32)]) {
    for i in (0..entries.len() / 2).rev() {
        sift_down(entries, i);
    }
}

fn sift_down(entries: &mut [(u32, f32)], mut i: usize) {
    let len = entries.len();
    loop {
        let l = 2 * i + 1;
        let r = 2 * i + 2;
        let mut largest = i;
        if l < len && entries[l].1 > entries[largest].1 {
            largest = l;
        }
        if r < len && entries[r].1 > entries[largest].1 {
            largest = r;
        }
        if largest == i {
            return;
        }
        entries.swap(i, largest);
        i = largest;
    }
}

fn run_correctness() -> Result<(), String> {
    let cases = correctness_battery(CORRECTNESS_SEED);
    for case in &cases {
        let agent = PqKernel::new(case.shape, &case.codebook, &case.codes, case.num_vectors);
        let reference =
            ScalarReference::new(case.shape, &case.codebook, &case.codes, case.num_vectors);

        let mut agent_table = vec![0.0f32; case.shape.distance_table_len()];
        let mut ref_table = vec![0.0f32; case.shape.distance_table_len()];
        agent.distance_table(&case.query, &mut agent_table);
        reference.distance_table(&case.query, &mut ref_table);
        let table_err = max_abs_err(&agent_table, &ref_table);
        if table_err > MAX_ABS_ERR {
            return Err(format!(
                "case={}/{} distance_table max_abs_err={table_err} > {MAX_ABS_ERR}",
                format_shape(&case.shape),
                case.label
            ));
        }

        let mut agent_dists = vec![0.0f32; case.num_vectors];
        let mut ref_dists = vec![0.0f32; case.num_vectors];
        agent.compute_distances(&agent_table, &mut agent_dists);
        reference.compute_distances(&ref_table, &mut ref_dists);
        let dist_err = distances_max_abs_err(&agent_dists, &ref_dists);
        if dist_err > MAX_ABS_ERR {
            return Err(format!(
                "case={}/{} compute_distances max_abs_err={dist_err} > {MAX_ABS_ERR}",
                format_shape(&case.shape),
                case.label
            ));
        }
    }
    Ok(())
}

struct ComboReport {
    shape: PqShape,
    dist: DataDistribution,
    geomean_ns: u64,
}

struct SpeedReport {
    // Agent measurements
    geomean_ns: u64,
    geomean_ns_ci: (u64, u64),
    median_ns: u64,
    geomean_cycles: Option<u64>,
    geomean_instructions: Option<u64>,
    worst_ns: u64,
    worst_shape: PqShape,
    worst_dist: DataDistribution,
    best_ns: u64,
    best_shape: PqShape,
    best_dist: DataDistribution,
    per_combo: Vec<ComboReport>,
    // Reference (upstream-via-lance-snapshots) measurements, interleaved
    // with the agent in the same thermal/cache state.
    reference_geomean_ns: u64,
    // Paired ratio agent/reference. Ratio < 1.0 means agent is faster.
    paired_ratio: f64,
    paired_ratio_ci: (f64, f64),
}

fn run_speed(workloads: &[SpeedWorkload], passes: usize) -> SpeedReport {
    let mut all_timings: Vec<u64> = Vec::new();
    let mut all_cycles: Vec<u64> = Vec::new();
    let mut all_instr: Vec<u64> = Vec::new();
    // Paired (agent_ns, reference_ns) per query, captured back-to-back in
    // the same thermal/cache state. Alternating order across queries
    // (even qi: agent first; odd qi: reference first) cancels the
    // warm-cache bias of the second call.
    let mut paired_agent: Vec<u64> = Vec::new();
    let mut paired_ref: Vec<u64> = Vec::new();
    let mut per_combo_timings: Vec<Vec<u64>> = vec![Vec::new(); workloads.len()];

    let mut perf = PerfCounters::new();
    let pmc_available = perf.has_pmc();

    for _pass in 0..passes {
        for (wi, wl) in workloads.iter().enumerate() {
            let kernel = PqKernel::new(wl.shape, &wl.codebook, &wl.codes, wl.num_vectors);
            let reference =
                ScalarReference::new(wl.shape, &wl.codebook, &wl.codes, wl.num_vectors);
            // Per-query scratch reused across queries, allocs stay out of the
            // per-query timing so allocator improvements don't masquerade as
            // kernel improvements.
            let mut table = vec![0.0f32; wl.shape.distance_table_len()];
            let mut distances = vec![0.0f32; wl.num_vectors];
            let mut topk: Vec<(u32, f32)> = Vec::with_capacity(wl.k);

            // Warmup both agent and reference; primes caches for this combo.
            {
                let q = &wl.queries[..wl.shape.dim];
                kernel.distance_table(q, &mut table);
                kernel.compute_distances(&table, &mut distances);
                select_top_k(&distances, wl.k, &mut topk);
                std::hint::black_box(&topk);
                reference.distance_table(q, &mut table);
                reference.compute_distances(&table, &mut distances);
                select_top_k(&distances, wl.k, &mut topk);
                std::hint::black_box(&topk);
            }

            for qi in 0..wl.num_queries {
                let q = &wl.queries[qi * wl.shape.dim..(qi + 1) * wl.shape.dim];

                let (agent_ns, agent_m, ref_ns) = if qi.is_multiple_of(2) {
                    // Even qi: agent first, reference second.
                    perf.start();
                    kernel.distance_table(q, &mut table);
                    kernel.compute_distances(&table, &mut distances);
                    select_top_k(&distances, wl.k, &mut topk);
                    let m_a = perf.stop();
                    std::hint::black_box(&topk);
                    perf.start();
                    reference.distance_table(q, &mut table);
                    reference.compute_distances(&table, &mut distances);
                    select_top_k(&distances, wl.k, &mut topk);
                    let m_r = perf.stop();
                    std::hint::black_box(&topk);
                    (m_a.wall_clock_ns, m_a, m_r.wall_clock_ns)
                } else {
                    // Odd qi: reference first, agent second.
                    perf.start();
                    reference.distance_table(q, &mut table);
                    reference.compute_distances(&table, &mut distances);
                    select_top_k(&distances, wl.k, &mut topk);
                    let m_r = perf.stop();
                    std::hint::black_box(&topk);
                    perf.start();
                    kernel.distance_table(q, &mut table);
                    kernel.compute_distances(&table, &mut distances);
                    select_top_k(&distances, wl.k, &mut topk);
                    let m_a = perf.stop();
                    std::hint::black_box(&topk);
                    (m_a.wall_clock_ns, m_a, m_r.wall_clock_ns)
                };

                all_timings.push(agent_ns);
                per_combo_timings[wi].push(agent_ns);
                paired_agent.push(agent_ns);
                paired_ref.push(ref_ns);
                if let Some(c) = agent_m.cycles {
                    all_cycles.push(c);
                }
                if let Some(i) = agent_m.instructions {
                    all_instr.push(i);
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

    let agent_geo = geomean(&all_timings);
    let ref_geo = geomean(&paired_ref);
    let paired_ratio = if ref_geo > 0 {
        agent_geo as f64 / ref_geo as f64
    } else {
        1.0
    };
    let paired_ratio_ci = bootstrap_ci_paired_ratio(
        &paired_agent,
        &paired_ref,
        BOOTSTRAP_RESAMPLES,
        BOOTSTRAP_SEED,
    );

    SpeedReport {
        geomean_ns: agent_geo,
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
        reference_geomean_ns: ref_geo,
        paired_ratio,
        paired_ratio_ci,
    }
}

fn format_shape(s: &PqShape) -> String {
    format!("({},{},{})", s.dim, s.num_sub_vectors, s.num_centroids)
}

fn format_dist(d: &DataDistribution) -> String {
    match d {
        DataDistribution::Clustered => "clustered",
        DataDistribution::Uniform => "uniform",
        DataDistribution::Sparse => "sparse",
    }
    .to_string()
}
