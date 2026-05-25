# Lance upstream-PR checklist

When an autoresearch trial produces a winning kernel that's ready to
port to [`lance-format/lance`](https://github.com/lance-format/lance),
this is the workflow. It's specific because Lance's PR validation model
is unusual: **no PR-blocking perf gate**, opt-in bench runs, and the
historical trendline lives in a separate repo.

## How Lance validates perf changes (so you can game the system fairly)

Lance has three perf-validation paths, none of which auto-runs on a
normal PR:

| Trigger | What runs | Hardware | Gated? |
|---|---|---|---|
| **Push to `main` or nightly cron (2AM PST)** | `cargo bench --bench {l2, cosine, dot, kmeans, norm_l2, sq, hnsw, inverted, pq_dist_table, pq_assignment}` with `--output-format bencher`. Results pushed to `lancedb/lance-benchmark-results` via `benchmark-action/github-action-benchmark`, rendered as a trendline on a GitHub Pages site. | `warp-ubuntu-latest-arm64-8x` (warp.dev 8-core ARM64) | No |
| **PR comment `@bench-bot benchmark`** | Dispatches a job in `lancedb/lance-bench` against the PR SHA. Reviewer opt-in. | Same warp ARM64 runner. | No |
| **Normal PR (default)** | Nothing. The workflow's `pull_request:` trigger is gated by `paths: - .github/workflows/rust-benchmark.yml`, so kernel-code PRs don't auto-bench. | n/a | n/a |

Two important consequences:

1. **The CI bench machine is ARM64.** Our M1 Max paired-CI measurements
   are on the same ISA (aarch64 NEON, ARMv8). Different microarchitecture
   (Apple Firestorm vs Ampere/Neoverse), but cache geometry and SIMD
   width are similar. M1 numbers are a closer-than-expected proxy for
   what Lance's CI will see.
2. **Noise handling is loose by design.** Their criterion config is
   `sample_size(10).significance_level(0.1)`. Default criterion is
   `sample_size(100)`. They trade rigor for total wall-clock (full
   workspace bench is already ~1 hour). Per-bench CI widths are
   correspondingly wide (~±5-15% on most benches by inspection). No
   paired methodology. **A reviewer's eyeball is the actual gate.**

This means our PR description carries more weight than usual. The
maintainer reads our numbers + asks the bot for theirs + makes a
judgment. Lean on **two-source corroboration**: their loose criterion
on their hardware, plus our tight paired CI on a comparable ARM64 box.
Either alone is plausibly noise; both pointing the same direction is
strong.

## What to gather before opening the PR

In order; each step builds on the previous.

### 1. The patch

```bash
# In the lance-autoresearch repo, at the winning trial commit:
git diff <baseline-sha> -- crates/<target>/src/kernels.rs > /tmp/win.patch
```

The diff should be small and reviewable. If it's >100 lines, the
upstream PR probably needs to split into preparation commits (data
layout changes, helper extraction) plus the actual optimization commit.

### 2. Re-sync `lance-snapshots` to current upstream HEAD

```bash
./scripts/check-lance-drift.sh
# If drift detected, follow crates/lance-snapshots/RESYNC.md to update
# the vendored files. Re-run the experiment on the re-synced harness:
cargo run --release --bin run_experiment -p <target> -- --mode baseline > /tmp/post-resync.log
```

This protects against the "our snapshot is 6 months old; upstream
already did the equivalent change" failure mode. Don't skip.

### 3. Paired-CI measurement at the winning SHA

```bash
git checkout <winning-sha>
cargo run --release --bin run_experiment -p <target> -- --mode baseline > /tmp/paired.log
grep -E '^(paired_ratio|paired_speedup|reference_geomean|geomean_ns)' /tmp/paired.log
```

The relevant fields:
- `paired_speedup_pct: +X.X% (CI [+lo, +hi])` — the headline number
- `reference_geomean_ns_per_query: N` — upstream's measured speed
- `geomean_ns_per_query: N` — agent's measured speed
- `worst_ns_per_query: ...` — worst combo's geomean (for the worst-case
  guard claim)

Include all four in the PR.

### 4. Lance's own criterion bench, before and after

This is the *defensive* evidence — the same bench Lance maintainers will
run via `@bench-bot`. Run it ourselves first so we know what they'll see.

```bash
git clone https://github.com/lance-format/lance /tmp/lance-pr
cd /tmp/lance-pr

# Baseline at upstream HEAD
cd rust/lance-index
cargo bench --bench pq_dist_table -- --save-baseline upstream

# Apply our patch
cd ../..
git apply /tmp/win.patch

# Trial at upstream HEAD + our patch
cd rust/lance-index
cargo bench --bench pq_dist_table -- --save-baseline ours

# Compare
critcmp upstream ours
```

`critcmp` output looks like:

```
group                          upstream                       ours
-----                          --------                       ----
build_distance_table_l2/...    1.00      45.3±0.5µs     ?     1.02      46.2±0.8µs
compute_distances/...          1.18     298.4±2.1µs     ?     1.00     252.7±1.4µs
```

Include this in the PR description verbatim. Lance reviewers know how
to read it.

**Caveat:** Lance's `pq_dist_table.rs` benches one shape (DIM=128,
PQ=16). If our win is shape-dependent (e.g. only shows up at DIM=768),
we should also vendor a temporary bench config that tests our target
shape, and include that result separately. The maintainer can decide
whether to add it permanently to Lance.

### 5. Fresh-clone verification

```bash
git clone https://github.com/lance-format/lance /tmp/lance-verify
cd /tmp/lance-verify
git apply /tmp/win.patch
cargo build --release --locked
cargo test --release --locked -p lance-index pq
```

Confirms the patch applies cleanly, builds with Lance's pinned Cargo.lock,
and passes their existing tests. No incremental-compilation Heisenbugs.

### 6. Bit-exactness statement

Phrase for the PR description (adjust for your target):

> Bit-equivalent to current upstream within `MAX_ABS_ERR = 1e-4` on the
> 5-distribution correctness battery in lance-autoresearch's harness
> (Gaussian, uniform, sparse, large_dynamic_range, mostly_zero). Recall
> preserved by construction; no oracle change needed.

If the change is `#[cfg(target_arch = "aarch64")]`-gated, add:

> x86 path unchanged. Only aarch64 callers see the new code.

### 7. Machine context

Include in the PR description:

> Measured on: Apple M1 Max, macOS 14.x, rustc 1.93.0 stable, paired
> bootstrap CI (n=1000 resamples over N=Q×P paired samples). Paired
> methodology described in lance-autoresearch's HARNESS.md.

If you have Linux numbers, include those too:

> Cross-verified on: AWS c7g.2xlarge (Graviton3, ARM64 Neoverse-V1),
> Ubuntu 22.04. Paired speedup: +X.X% (CI [+lo, +hi]).

## What the PR description should look like

A skeleton:

```markdown
## Summary

Adds an aarch64-specific fast path for `compute_pq_distance` that
trades upstream's loop-swap pattern for per-vector AoS+4x register
accumulators. ~12% paired-CI speedup on M1 Max; ~X% on Graviton3.
Bit-equivalent to current upstream.

## Mechanism

Upstream's `for m: for i` loop writes ~N×M f32 distances per query
(worst combo: 7.5 MB for our shape mix). The new path keeps M
accumulators register-resident; writes only N final values per query
(80 KB). On aarch64 the L1 write port is the bottleneck on the old
pattern; the new pattern frees it.

This is an aarch64-only win because AVX-512's `vmovups` (16 f32 per
store) reduces the upstream write count by 16× on x86. The x86 path
is unchanged.

## Numbers

Lance criterion `pq_dist_table` (`cargo bench --bench pq_dist_table`),
ARM64 (warp.dev 8-core Ampere):

[paste critcmp output]

lance-autoresearch paired CI (M1 Max):
- agent geomean: 183,513 ns
- upstream geomean: 208,311 ns
- paired ratio: 0.8810 (CI [0.8757, 0.8864])
- paired speedup: +11.9% (CI [+11.4%, +12.4%])

## Correctness

Bit-equivalent within `MAX_ABS_ERR = 1e-4` on 5 input distributions
(Gaussian / uniform / sparse / large_dynamic_range / mostly_zero) ×
3 PQ shapes ((128,16,256), (256,16,256), (768,96,256)). Tested via
lance-autoresearch's correctness battery + this repo's existing tests.

## Out of scope

- x86 path (unchanged; gated by `#[cfg(target_arch = "aarch64")]`)
- 4-bit PQ path (already SIMD-optimized via u8x16 shuffle)
- f16/f64 paths (this PR is f32 only)
```

## After the PR is open

1. Ask in the PR comment: `@bench-bot benchmark`. Wait for the bench
   bot to post results.
2. If their numbers diverge significantly from ours (>3% absolute), dig
   into why before defending. Common causes:
   - Lance's bench shape is only `(128, 16, 256)`; our biggest win is on
     `(768, 96, 256)`. The advertised number should be on Lance's shape.
   - Their CI was thermally hot from a parallel job. Ask for a re-run.
   - Their bench config differences (sample_size, significance level)
     mean their CI is wider. Read criterion's output carefully.
3. If a maintainer asks for x86 numbers, run on `c7i.2xlarge` (Intel
   Sapphire Rapids AVX-512) and post a follow-up comment. Don't claim
   the win on x86 without measurement.

## Apache-2.0 license alignment

The lance-autoresearch repo is Apache-2.0 to match Lance directly. No
re-licensing needed; the patch carries the same license as the upstream
file. Don't add new SPDX headers to existing Lance files; they already
have them.

## Last alignment audit

This doc is current as of 2026-05-25. Re-validate if:

- Lance changes their CI bench machine (currently `warp-ubuntu-latest-arm64-8x`)
- Lance adds an automated PR-blocking perf gate
- Lance changes their criterion config (currently `sample_size(10).significance_level(0.1)`)
- Lance's `pq_dist_table.rs` adds more shapes
