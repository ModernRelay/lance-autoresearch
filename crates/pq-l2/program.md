# Target: PQ L2 — agent instructions

This is the per-target overlay on top of [`../../HARNESS.md`](../../HARNESS.md).
Read **HARNESS.md first** for the universal loop contract (what's editable,
the metric, the loop, hygiene, never stop). This file adds the PQ-L2-specific
API spec and priors.

## Setup (once per session)

1. Read in this order:
   - `../../HARNESS.md`
   - `../../README.md`
   - `program.md` (this file)
   - `lessons.md` *(if present, gitignored — past trial findings for this machine)*
   - `src/lib.rs`
   - `src/kernels.rs` *(the only file you may edit)*
   - `src/reference.rs`
   - `src/inputs.rs`
   - `src/bin/run_experiment.rs`
2. Ensure `results.tsv` exists. If not, create it with this header:
   ```
   commit	timestamp	correctness	geomean_ns	ci_lo	ci_hi	worst_ns	worst_combo	best_ns	best_combo	peak_mem_mb	total_seconds	keep	description
   ```
3. Baseline trial (3 passes for a tight CI):
   ```
   cargo run --release --bin run_experiment -p pq-l2 -- --mode baseline > run.log 2>&1
   ```
   Append a row tagged `keep=baseline`, commit it. Note the `arch:` line in
   the header — that determines which `[arch=...]` priors sub-section applies.

4. Per-trial: default 1-pass mode (faster iteration):
   ```
   cargo run --release --bin run_experiment -p pq-l2 > run.log 2>&1
   ```
   Apply the keep-gate from `HARNESS.md` using `geomean_ns_ci_90pct` vs the
   baseline's CI: a trial keeps when its CI upper-bound is strictly below the
   current best's CI lower-bound.

## Public API contract (must remain stable)

The bench imports these from `crate::kernels`. You may NOT change their
signatures. You MAY add private helpers, internal data layouts, `unsafe`
blocks, `std::arch` intrinsics under `#[cfg(target_arch = ...)]` gates,
pre-computed state inside `PqKernel`, etc.

```rust
pub struct PqKernel { /* agent's private fields */ }

impl PqKernel {
    /// Constructor — pre-process the codebook AND codes here (transposes,
    /// L2Prepared SoA layout, cached c·c, etc.). Build cost is amortized
    /// across all subsequent queries.
    pub fn new(shape: PqShape, codebook: &[f32], codes_aos: &[u8], num_vectors: usize) -> Self;

    /// Build the asymmetric L2 distance table for one query.
    /// Mirrors upstream's `build_distance_table_l2`.
    pub fn distance_table(&self, query: &[f32], out: &mut [f32]);

    /// Compute per-vector L2 distances from the distance table.
    /// Mirrors upstream's `compute_pq_distance`. Writes `num_vectors`
    /// distances into `out`.
    pub fn compute_distances(&self, table: &[f32], out: &mut [f32]);

    pub fn num_vectors(&self) -> usize;
}
```

Top-K selection happens **outside** the kernel (in `run_experiment.rs`).
That matches upstream's split: kernel writes per-vector distances; the
caller selects top-K.

Pre-processing in `new` is free — the bench measures
`distance_table + compute_distances + top-K select` per query, not per
(build + query). Codebook transposes, codes transposes, cached `c·c`,
packed LUTs, etc., should live in `new`.

## What you can / cannot do

(See HARNESS.md for the universal table; this is the PQ-L2 specific
addition.)

- **Cannot** change `PqShape` or the constants in `lib.rs`. They define
  the optimization target.
- **Cannot** introduce lossy techniques (LUT u8/u16 quantization, asymmetric
  approximation, anything that drops bits relative to the scalar reference).
  The correctness phase asserts `max_abs_err ≤ 1e-4` against the scalar
  reference; lossy techniques fail this gate. If you want to explore a lossy
  track, propose it to the human as a separate kernel surface.
- **Can** mark hot functions `#[inline]`, split them, add private helpers.
- **Can** add `#[cfg(test)] mod tests { ... }` inside `kernels.rs` for in-file
  property checks against the scalar path.

## Lance-PQ-specific priors

These are the directions that pay off on this kernel shape without
compromising arithmetic accuracy. Pick one hypothesis per trial; don't try
to combine multiple ideas at once.

`run_experiment`'s header prints `arch:` — only the matching sub-section's
intrinsic ideas apply on your hardware. Algorithmic ideas in `[arch=any]`
apply everywhere.

Before picking from this list, read `lessons.md` (gitignored, per-machine).
Past trials may have already ruled out or confirmed specific hypotheses with
mechanism notes. Don't re-tread settled ground.

### `[arch=any]` — algorithmic / portable

- **Codebook layout transpose.** Reference layout is `[m][k][d]`. Transposing
  to `[m][d][k]` lets a SIMD inner loop broadcast `q[d]` across `k` and
  compute `nc` distances per iteration instead of one. Do the transpose in
  `PqKernel::new` once; amortizes across queries.
- **Cache `c·c` per centroid + hoist `q·q`** (CAVEAT — see lessons.md).
  Rewrites `(q-c)² = q² + c² - 2·q·c`; the inner loop becomes an FMA-friendly
  dot product. **This breaks the bit-exact oracle on the `large_dynamic_range`
  fixture** due to catastrophic cancellation when `q,c` are O(10³) and the
  true distance is O(10¹). Only proceed if you have a strategy for the
  precision loss; otherwise this is off-limits for this kernel.
- **Top-K block-then-merge.** `push()` branches + heap-sifts on every code.
  At 20k probes per query × 9 (shape × dist) that's the second-largest cost
  after the gather. Block the probe (e.g. 512 codes), find local top-K with
  a branchless pass, merge into the global heap.
- **Bounds-check elision / strength-reduce indices.** `get_unchecked` plus
  incremental `tbl_row += nc` instead of `m * nc + c` removed multiple
  bounds-checks per inner iteration and gave -20% in a past trial; check
  `lessons.md` before re-attempting.
- **N-way unroll of probe.** Independent accumulator chains (2x, 4x) hide
  the FP-ADD latency on platforms where the inner loop is dependency-bound.
  Watch for diminishing returns past 4x and per-shape regressions when the
  batch dominates a small inner loop.

### `[arch=aarch64]` — NEON (Apple Silicon, ARM servers)

- **Inner loop with `vfmaq_f32`.** `core::arch::aarch64::vfmaq_f32(a, b, c)`
  computes `a + b*c` lane-wise. Maps directly to `fmla v0.4s, v1.4s, v2.4s`.
  For svd=8, two FMAs per centroid cover the dot product.
- **Byte-table gather via `vqtbl1q_u8`.** Loads 16 bytes from a 16-byte table
  using 16 indices. Useful if you transpose codes and process 16 vectors per
  probe iteration.
- **Prefetch with `prfm`.** `core::arch::aarch64::__prefetch(ptr, ...)` or
  inline `asm!("prfm pldl1keep, [{0}]", ...)`. M1's hardware prefetcher is
  aggressive on sequential access, so explicit prefetch helps mainly on
  strided / random patterns.
- **No `vpgatherdq` equivalent.** NEON has no native gather instruction
  (unlike AVX2). Either transpose-then-load or scalar-loop the gather.

### `[arch=x86_64]` — AVX2 (Intel, AMD)

- **FMA via `_mm256_fmadd_ps`.** 8 lanes of `a + b*c` per instruction.
  Same role as NEON's `vfmaq_f32`.
- **Gather via `_mm256_i32gather_ps`** or `vpgatherdq` for 32-bit indices.
  Native instruction; useful for non-transposed code layouts.
- **Prefetch via `_mm_prefetch`** with `_MM_HINT_T0` / `_MM_HINT_T1` for
  controlling cache level.
- **Use `core::arch::x86_64`** under `#[cfg(target_arch = "x86_64")]` gates
  with a portable scalar fallback so the code still compiles on other archs.
