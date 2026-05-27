# HARNESS, shared loop contract for every lance-autoresearch target

This document is the universal part of every target's agent instructions. Each
target's `program.md` is a thin layer of *target-specific priors and API spec*
on top of the conventions below. The agent reads `HARNESS.md` and the target's
`program.md` at the start of every session.

## What this harness is

A single agent (you) edits one file in one target crate to optimize a Lance
kernel. Per trial, you build, run a binary that exercises the kernel against
diverse inputs, parse a fixed-format output block, and decide keep-or-revert.

This is a Karpathy-style autoresearch loop. It assumes:

- Per-trial eval is **seconds-scale**. Long enough to measure, short enough to
  iterate hundreds of times in a session.
- The kernel has a **deterministic correctness oracle**, a scalar reference
  that produces the same answer to compare against.
- The optimization target is **dataset-independent**: the harness generates
  diverse inputs each trial, so wins generalize across distributions and
  shapes by construction.

Targets that don't fit these constraints (index-build parameter tuning,
plan-patching, anything where eval is minutes-to-hours) need a different
harness shape, not this one. See `docs/design.md` for the boundary.

## What's editable, per target

| Path | Mutability | Why |
|---|---|---|
| `crates/<target>/src/kernels.rs` | **mutable** | Your playground. The whole point. |
| `crates/<target>/src/reference.rs` | immutable | The oracle. Touching it makes wins meaningless. |
| `crates/<target>/src/inputs.rs` | immutable | The fixture generator. Touching it makes timings incomparable across trials. |
| `crates/<target>/src/lib.rs` | immutable | Shared types pinned by the bench (`PqShape` etc.). |
| `crates/<target>/src/bin/run_experiment.rs` | immutable | The trial harness. |
| `crates/<target>/benches/*.rs` | immutable | Criterion bench, optional read-only reference. |
| `crates/<target>/Cargo.toml` | immutable | Adding deps changes the optimization target. |
| `crates/<target>/program.md` | human-iterated between runs | Not edited by you in-loop; the human refines it. |
| `crates/<target>/results.tsv` | append-only | Your audit log. Gitignored. |
| `crates/harness-common/**` | immutable | Workspace-shared infrastructure. |
| `HARNESS.md` (this file) | immutable | Workspace-shared loop contract. |

You may add `#[cfg(test)] mod tests { ... }` inside `kernels.rs` for in-file
property checks. You may NOT add new crate dependencies. You may NOT use
unsafe-only-on-broken-assumptions tricks (e.g., assuming a fixture invariant
that holds today but isn't documented).

## The metric

Every target's `run_experiment` binary prints a fixed-format output block ending
with these universal fields:

- `correctness:`, `pass` or `fail`. Set by comparing your kernel against the
  scalar reference on every input the bench generates.
- `arch:`, the detected `target_arch` (e.g., `aarch64`, `x86_64`). Tells you
  which `program.md` priors section applies.
- `geomean_ns_per_*:`, geometric mean of per-operation wall-clock across all
  timed operations.
- `geomean_cycles_per_*:`, geomean of CPU cycles per operation. Populated on
  Linux when `perf_event_open` is available; `n/a` on macOS and on Linux when
  `/proc/sys/kernel/perf_event_paranoid > 1` (needs `CAP_PERFMON`).
- `geomean_instructions_per_*:`, same conditions as cycles.
- `worst_ns_per_*:`, slowest combo's geomean.
- `peak_mem_mb:`, process RSS high-water-mark.
- `total_seconds:`, trial wall-clock.

A kernel is **kept** iff:

1. `correctness: pass` (any failure → `std::process::exit(2)`).
2. **Primary speed gate (non-overlapping 90% CI).**
   - On Linux when PMC is available: `geomean_cycles_per_*` upper-bound of the
     trial CI strictly below the current-best baseline's CI lower-bound. Cycles
     noise is ~0.01% so this is effectively a strict-better test.
   - On macOS / no-PMC: same logic on `geomean_ns_per_*` CI. Wall-clock noise
     on Apple Silicon is ~4%, so the CI-overlap test prevents marginal "wins"
     from passing on noise.
   - Both CIs come from the bootstrap_ci_geomean field printed as
     `geomean_*_ci_90pct: [lo, hi]`.
3. **Worst-combo sanity cap.** `worst_ns_per_*` ≤ 1.05 × the previous best-kept
   kernel's worst. Single-number cap that catches catastrophic regressions on
   any one combo regardless of compensation elsewhere.
4. **Aggregate trade-off ratio.** Across all per-combo geomeans, the sum of
   absolute improvements (in nanoseconds) must be at least 10× the sum of
   absolute regressions. Captures "wins must clearly dominate losses" without
   forcing per-combo uniformity, which would reject legitimate asymmetric
   trades like −2931 ns on a deep-skip combo at the cost of +39 ns on a
   shallow-skip combo. Robust to the ~20% run-to-run variance in per-combo
   measurements at sub-30 ns scales on M1.
5. `total_seconds` ≤ 600 (the per-trial cap; exceed it → `std::process::exit(3)`).
6. Build clean: `cargo build --release` and
   `cargo clippy --release --all-targets -- -D warnings` both succeed.

**Apply the gate via `scripts/check-keep-gate.py`**, not by hand. The script
takes two `run.log` files (previous best, trial) and emits PASS/FAIL with the
exact violations cited. Exit codes: 0 pass, 1 fail (revert), 2 correctness
fail (revert), 3 parse error. Using the script keeps the decision consistent
across sessions; doing it by hand has historically produced inconsistent
verdicts on asymmetric trade-offs (see git history of `posting-seek` trials).

Thresholds (`WORST_COMBO_PCT_MAX = 0.05`, `TRADE_OFF_RATIO = 10.0`) live in
the script's header constants and are tuneable there. Document any change to
these values with a commit that explains the empirical motivation.

### Baseline vs trial measurements

Baseline runs use `cargo run --release --bin run_experiment -p <target> -- --mode baseline` which auto-runs the speed phase 3× and bundles all samples for a tighter CI (typically ±4% on wall-clock vs ±7% for a single pass). Trial runs use the default 1-pass mode for faster iteration.

Result: the baseline CI is tight (it's the reference); trial CIs are wider but still expected to clear by margin on real wins. The keep-gate "trial-upper < baseline-lower" works because a real win produces a trial geomean far enough below baseline that even the wide trial CI doesn't overlap.

Exit codes summary for `run_experiment`: `0` on success, `1` on internal
error (panic, fixture load failure), `2` on correctness failure, `3` on
time-budget breach. The agent's loop should treat anything non-zero as
"revert; do not log as a measurement."

Ties break toward simpler code: same speed within ~3% noise → fewer lines /
less `unsafe` wins.

## The loop

After reading `HARNESS.md` and the target's `program.md`:

1. **Setup (once per session).** Confirm `results.tsv` exists; if not, create
   it with a per-target header (the target's `program.md` defines the columns).
   Run the baseline trial:
   ```
   cargo run --release --bin run_experiment -p <target> > run.log 2>&1
   ```
   Append a row tagged `keep=baseline` and commit it.

2. **Observe state.** Read the last ~5 rows of `results.tsv`. Note which ideas
   have been tried, what won, what regressed. Form one hypothesis with one
   sentence stating the change and the predicted effect on speed and
   correctness.

3. **Edit `kernels.rs`.** Keep the diff focused on the one hypothesis.

4. **Build and lint.**
   ```
   cargo build --release
   cargo clippy --release --all-targets -- -D warnings
   ```
   If either fails, fix and retry. Do not commit broken state.

5. **Run the trial.**
   ```
   cargo run --release --bin run_experiment -p <target> > run.log 2>&1
   ```

6. **Parse and decide.** Invoke `scripts/check-keep-gate.py <previous-best.log>
   <run.log>`. The script applies the formal keep-gate (correctness, CI
   non-overlap on geomean, worst-combo cap at 5%, aggregate trade-off
   ratio ≥10×) and emits PASS/FAIL with violations cited. Exit 0 → keep;
   exit 1 → revert; exit 2 → correctness fail; exit 3 → parse error.

7. **Log.** Append one row to `results.tsv` matching the target's header.

8. **Commit.** One-line message describing the change and the headline number,
   e.g. `transpose codebook in new(); 18.2k → 14.1k geomean ns (worst -8%)`.

9. **Hygiene.**
   - Always commit `kernels.rs` changes; never commit `results.tsv`,
     `run.log`, or `lessons.md` (all gitignored).
   - If a change fails to build, do not commit. Iterate or revert cleanly.
   - If two consecutive ideas regress, take a beat: re-read the last ~10 rows
     and update your mental model before proposing the next.
   - Per-trial cap: 10 minutes. If `cargo run` is still going after 10 min,
     kill it and mark the trial as `timeout`.

10. **Capture lessons.** Append to `crates/<target>/lessons.md` (create if
    missing, gitignored, lives only on this machine) whenever a trial
    produces a finding worth remembering:
    - **Rejected trial with informative failure mode.** Concrete example: a
      cache-c² rewrite that fails the bit-exact oracle on a specific fixture.
      Future agents reading `lessons.md` at session start skip the dead end.
    - **Kept trial with surprising mechanism.** A 20% win that reveals the
      inner loop was FP-ADD latency bound (not throughput bound) is
      load-bearing context for the next hypothesis.
    - **Stop reasons that bound the search.** "8x+ unroll regresses small
      shapes; FP pipe saturated at 4x for this shape mix" tells the next
      agent where the cliff is.
    Format: free-form Markdown; one entry per lesson with date, trial
    commit SHA, the mechanism, and the implication. Keep entries short.

## Background research (papers, public benchmarks, blog posts)

The inner trial loop is bounded by deterministic per-trial cost. Web fetches
inside the loop break that bound and contaminate iteration latency. Background
research happens at three specific points OUTSIDE the loop:

1. **At scaffold time, before the first session on a new target.** The human
   or agent adding a target MUST do the upstream hot-path trace
   (`docs/adding-a-target.md` Step 0) before running `scripts/scaffold-target.sh`.
   This is itself a background-research action: read upstream Lance caller
   code, identify the primitive operation that dominates the hot path, quote
   the call site (SHA + path + line numbers) in the per-target capsule's
   "Lance call site" section. Treat this as part of session setup, not
   optional. Cost ≤30 min; the only structural insurance against shipping a
   target whose kernel surface doesn't match what Lance actually calls.
   `posting-intersect` skipped this step and ended up measuring a kernel
   surface that Lance's WAND traversal does not call; the lesson is now
   load-bearing.

2. **Session start, once.** After reading `program.md` + `lessons.md`, if the
   target's priors list cites named papers or algorithms (e.g. "Lemire 2015
   SIMD-galloping", "PForDelta", "BP128", "FSST paper"), or if `lessons.md`
   contains an open `RESEARCH:` marker, do a focused fetch:
   - Skim 1–3 papers/posts relevant to the next planned hypothesis.
   - Extract: the algorithm's mechanism (one sentence), the conditions under
     which it wins (input shape / distribution), and the published speedup
     vs. a stated baseline.
   - Append each to `lessons.md` under a `## References` section as one
     bullet: `[citation] — mechanism — wins when X — published Yx vs Z`.
   - Total time budget: ≤10 minutes. If a paper requires deep reading to be
     useful, summarize what you got and move on.

3. **On-stuck, between trials.** After **3 consecutive rejected trials on the
   same target**, pause the inner loop and do one focused research pass:
   re-read the last 10 `results.tsv` rows, identify the regime where the
   kernel is stuck (e.g. "FP-ADD latency-bound on aarch64, write-port limited
   when output width grows"), and fetch 1–2 sources targeting that regime.
   Record findings in `lessons.md` under `## On-stuck research, <date>`.
   Then propose a new hypothesis informed by what you read.

**Never** fetch inside the inner loop (edit → build → bench → commit). A
single web fetch can add 30s of variable latency; that destroys the
per-trial cost bound the harness depends on. If a hypothesis requires an
unfamiliar technique, do the fetch ONCE between trials, record what you
learned, then enter the loop.

**Provenance.** Cite every external source in the commit message of the
trial it informs: `"galloping-based skip (Lemire 2017, §3.2): -22% on
skewed-pair, no regression on balanced"`. Reviewers (you, future agents,
the human porting upstream) need to retrace the reasoning.

**Tools.** Use `WebFetch` for known URLs and `WebSearch` for discovery. If
the target's `program.md` lists canonical references, prefer those over
ad-hoc search; the human has already vetted them.

## Integration validation (once per target, before claiming a Lance win)

A microbench win is a kernel-level result. To claim a production
speedup that an upstream maintainer would care about, validate against
upstream Lance's own benchmark at upstream's scale BEFORE marking the
target as having produced "a Lance win" in the README target table:

1. Clone upstream at the pinned SHA (`crates/lance-snapshots/src/lib.rs`):
   ```
   git clone --depth 1 https://github.com/lance-format/lance /tmp/lance-bench
   cd /tmp/lance-bench
   git fetch --depth 1 origin <pinned-sha> && git checkout <pinned-sha>
   ```
2. Build the upstream bench that exercises this kernel (identified by
   `docs/adding-a-target.md` Step 0.5): typically in `rust/lance/benches/`
   or `rust/lance-<crate>/benches/`. Filter to the relevant bench
   function with `--bench '<regex>'` to skip irrelevant setup.
3. Capture baseline numbers by running the bench binary directly:
   `<target>/release/deps/<bench>-<hash> --bench '<filter>'`. Going
   through `cargo bench` triggers another rebuild; direct invocation
   reuses the existing release binary.
4. Apply the kernel change to upstream's source (port `kernels.rs` to
   the appropriate upstream file). Run `cargo test --release -p <crate>`
   to verify correctness against upstream's own tests.
5. Re-bench. Compare baseline vs patched with criterion's p-value.
6. Record both numbers in the target's capsule under a new "Upstream
   integration" section: the microbench delta AND the integration delta.

**If the integration delta is below criterion's significance threshold
(p > 0.05), the kernel win is real but its production impact is
unverified at upstream's current bench scale.** Document this honestly
in the capsule and README target table: do NOT claim a Lance speedup
based on the microbench alone. Either:

- Argue scale: if the asymptotic win materializes only at 100×
  upstream's bench scale, say so explicitly with the cost-model
  arithmetic. The PR's value becomes "low-risk infra change for
  billion-scale users," not "X% faster Lance."
- Defer: drop the target's status to `kernel-result-only` and move on
  to a target with higher cost-fraction.
- Refocus: the kernel may be too narrow; the production hot path may
  be something larger that this kernel sits inside. Consider whether
  the target's surface should be widened.

The autoresearch loop produces kernel results. The integration phase
proves whether those kernel results matter at upstream's scale.

## Never stop

Keep going until interrupted. Each loop iteration is one hypothesis, one edit,
one measurement, one commit. No multi-step plans across iterations.

## Working across multiple targets

If a session spans multiple targets, work on **one target per session**. Don't
edit `kernels.rs` in two crates between commits, the agent's mental model is
shared but the keep-decision is per-target. Pick a target, do a session there,
commit, switch.

The human is responsible for selecting which target to work on next. Don't
proactively switch targets unless the user asks.
