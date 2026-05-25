# Why the harness is shaped this way (robustness rationale)

The harness has several measurement and reproducibility features that look
like ceremony if you don't know the failure modes they prevent. This doc
explains each one and the empirical observation that justifies it. Read it
before removing or relaxing any of them.

The features described here came from running the first autoresearch loop
(`pq-l2`, May 2026 on Apple M1 Max — 6 trials, 4 keeps, 2 rejects, -49.8%
geomean from baseline) and noticing the harness was almost-but-not-quite
solid enough to publish the result. The fixes landed before the second
target launched so they apply to every kernel from `pq-l2` onward.

## Why baseline runs 3 passes (`--mode baseline`)

**Observation.** Wall-clock noise on M1 Max is ~4% trial-to-trial on the
same binary. With a single baseline measurement, the recorded baseline can
land anywhere in that band, and every subsequent "win" is inflated or
deflated by up to 4% accordingly.

**Fix.** Baseline runs 3 passes and bundles all per-query samples (864 vs
288). The bootstrap CI tightens from ±7% to ±4%. The recorded baseline is
the median + CI of those samples, not a single point.

**Cost.** ~2× longer baseline run (~75s instead of ~25s on M1). Baseline
runs once per session, so this is a one-time cost.

**Don't relax.** The 1% noise band the original `HARNESS.md` assumed
historically is below the wall-clock noise floor on Apple Silicon. Single-
sample baselines silently bias the entire experiment.

## Why we report cycles + instructions alongside wall-clock

**Observation.** Wall-clock conflates kernel work with thermal state,
scheduler interrupts, allocator behavior, and CPU frequency scaling. None
of those are properties of the agent's code.

**Fix.** On Linux, `perf_event_open` gives `INSTRUCTIONS_RETIRED` and
`CPU_CYCLES` with ~0.01% noise. On macOS, Apple's `kpc` framework needs
root / entitlements that aren't practical, so we ship a no-op stub and
fall back to wall-clock + bootstrap CI.

**Cost.** ~50ns per measurement (the perf_event_open read syscall),
negligible at 200k-1.5M ns/query.

**Read priority.** When PMC is available (Linux), the keep-gate uses
`cycles` as the primary metric. Wall-clock stays in the report for
user-visible comparability but is the secondary gate. Without PMC,
wall-clock is all you have, hence the bootstrap CI machinery.

## Why the keep-gate uses CI overlap rather than point estimate

**Observation.** A trial whose geomean is 0.5% better than baseline might
be a real win OR might be noise. The pre-fix gate used "geomean strictly
better with 1% noise band" — too strict (rejects real 0.5% wins when noise
is 4%) and too generous (accepts noise-positive 1.5% "wins").

**Fix.** Bootstrap 90% CI on the geomean, then test "trial CI upper-bound <
current-best CI lower-bound" (non-overlapping CIs in the favorable
direction). This is the textbook two-sample comparison.

**Concrete:** In the May 2026 run, trial 5 (8x unroll) had a per-combo
geomean improvement on small shapes but a +10% global geomean regression
due to within-combo variance. The old point-estimate gate would have
needed manual judgment. The CI gate rejects it cleanly because the trial
CI overlaps baseline's.

**Don't relax.** The CI gate is what makes the experiment publishable. A
Lance upstream maintainer will ask "is the speedup statistically
significant?" — the answer should be a number, not a vibe.

## Why `lessons.md` is gitignored

**Observation.** Per-machine findings (e.g., "8x unroll regresses on this
shape mix on M1") don't generalize to other machines. Committing them would
pollute the harness's shared knowledge base with platform-specific noise.

**Fix.** `lessons.md` lives in `crates/<target>/` and is gitignored, like
`results.tsv`. The agent reads it at session start to avoid re-treading the
same ground but the file stays local.

**Don't change.** If you start committing `lessons.md`, every machine's
findings collide and the file becomes useless. If a finding is genuinely
portable (e.g., "the cache-c² rewrite always fails large_dynamic_range —
that's an algorithmic constraint, not a machine constraint"), promote it
to `crates/<target>/program.md` as a documented prior caveat. That's where
shared knowledge goes.

## Why `Cargo.lock` is committed

**Observation.** Crate updates change inlining decisions, codegen, and
sometimes algorithmic shape (e.g., a `criterion` patch release that
changes its statistical model). Without a committed lockfile, timings on
two checkouts of the same SHA can differ measurably.

**Fix.** `Cargo.lock` is checked in at the repo root (no longer in
`.gitignore`). All measurements use `cargo build --locked`. Crate updates
are explicit `cargo update` commits with the timing impact re-measured
immediately after.

**Don't relax.** This is the difference between "I got 196k ns/query" and
"someone else can reproduce 196k ns/query." Without it, the experiment is
a story, not a result.

## Lance upstream-PR checklist

When a winning kernel is ready to port to `lance-format/lance`, gather:

1. **Patch.** `git diff <baseline-sha> -- crates/<target>/src/kernels.rs`.
2. **Criterion comparison.** `cargo bench --bench <target> -- --save-baseline baseline-sha` at the baseline commit, then `--save-baseline winning-sha` at the winning commit, then `critcmp baseline-sha winning-sha`. Gives a 95% CI on the speedup.
3. **PMC cycle count delta.** Re-run `cargo run --release --bin run_experiment -p <target> -- --mode baseline` on Linux at both commits; the `geomean_cycles_per_query` delta is the most defensible single number (no wall-clock noise).
4. **Fresh-clone verification.** Clone the repo to `/tmp/verify`, check out the winning commit, run the experiment. Confirms no incremental-compilation Heisenbug.
5. **Bit-exactness statement.** The harness's `MAX_ABS_ERR = 1e-4` against the scalar reference means recall is preserved by construction. Cite the correctness battery distributions (e.g. Gaussian / uniform / sparse / large_dynamic_range / mostly_zero).
6. **Machine context.** CPU model, OS, rustc version. Lance maintainers reproduce on their own CI.

Apache-2.0 PR (the harness's Apache-2.0 license matches Lance's directly).
