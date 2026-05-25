# Target: posting-intersect, agent instructions

This is the per-target overlay on top of [`../../HARNESS.md`](../../HARNESS.md).
Read **HARNESS.md first** for the universal loop contract (what's editable,
the metric, the loop, hygiene, never stop, paper-fetching). This file adds
the posting-intersect-specific API spec and priors.

## Setup (once per session)

1. Read in this order:
   - `../../HARNESS.md`
   - `../../README.md`
   - `program.md` (this file)
   - `lessons.md` *(if present, gitignored, past trial findings for this machine)*
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
   cargo run --release --bin run_experiment -p posting-intersect -- --mode baseline > run.log 2>&1
   ```
   Append a row tagged `keep=baseline`, commit it. Note the `arch:` line in the
   header, that determines which `[arch=...]` priors sub-section applies.

4. Per-trial: default 1-pass mode (faster iteration):
   ```
   cargo run --release --bin run_experiment -p posting-intersect > run.log 2>&1
   ```
   Apply the keep-gate from `HARNESS.md` using `geomean_ns_ci_90pct` vs the
   baseline's CI: a trial keeps when its CI upper-bound is strictly below the
   current best's CI lower-bound.

5. (Per HARNESS.md "Background research") If this is the first session on
   this target, skim 1–3 of the references listed under "Canonical papers"
   below; append one bullet per paper to `lessons.md` under `## References`
   summarizing the mechanism and the regime where it wins. Time budget ≤10
   minutes.

## Public API contract (must remain stable)

The bench imports these from `crate::kernels`. You may NOT change their
signatures. You MAY add private helpers, internal scratch fields on
`PostingIntersect`, `unsafe` blocks, `std::arch` intrinsics under
`#[cfg(target_arch = ...)]` gates, dispatch tables, etc.

```rust
pub struct PostingIntersect { /* agent's private fields */ }

impl PostingIntersect {
    /// Reusable kernel context. The bench creates ONE per combo and reuses
    /// it across all instances, so scratch buffers grow once and amortize.
    pub fn new() -> Self;

    /// K-way AND-intersect. Output is sorted ascending, no duplicates,
    /// bitwise-identical to the reference's output on every input.
    ///
    /// Input contract:
    ///   - Each `lists[i]` is sorted ascending with no duplicates.
    ///   - `lists.len()` may be 0 (empty output) or 1 (output is a copy
    ///     of `lists[0]`).
    ///   - Any `lists[i].is_empty()` → empty output.
    ///   - `out` is caller-allocated; the kernel clears it on entry.
    pub fn intersect(&mut self, lists: &[&[u32]], out: &mut Vec<u32>);
}
```

The bench measures per-call `intersect` wall-clock (8 calls per timing
window, divided to recover per-call ns). Construction cost is excluded from
the timing.

## What you can / cannot do

(See HARNESS.md for the universal table; this is the posting-intersect
specific addition.)

- **Cannot** change the public API above.
- **Cannot** change the input contract: callers pass sorted-unique lists
  and expect sorted-unique output. Don't relax this even internally
  (the bench will fail the correctness gate).
- **Cannot** produce different output than the reference. Output type is
  integer `Vec<u32>`, so the gate is bitwise equality (no tolerance).
  Lossy techniques (probabilistic Bloom-filter pre-filter, sampling) fail
  the gate. If you want a lossy track (e.g., approximate intersect for
  WAND scoring), propose it to the human as a separate kernel surface.
- **Can** dispatch internally by input shape: number of lists K, length
  ratio between smallest and largest, density (length / max(values)).
  This is the central source of wins on this target; see priors below.
- **Can** hold scratch buffers (bitmap arena, sorted-list-by-length
  pointer table, etc.) inside `PostingIntersect`; the bench reuses the
  same instance across calls.
- **Can** add `#[cfg(test)] mod tests { ... }` inside `kernels.rs` for
  in-file property checks against the reference.

## Posting-intersect-specific priors

These are the directions that pay off on this kernel shape without
compromising the bitwise-equal output contract. Pick one hypothesis per
trial; don't combine multiple ideas at once.

`run_experiment`'s header prints `arch:`, only the matching sub-section's
intrinsic ideas apply on your hardware. Algorithmic ideas in `[arch=any]`
apply everywhere.

Before picking from this list, read `lessons.md` (gitignored, per-machine).
Past trials may have already ruled out or confirmed specific hypotheses
with mechanism notes. Don't re-tread settled ground.

### `[arch=any]`, algorithmic / portable

- **Sort lists by length, fold from smallest.** Two-finger merge's cost
  is O(|a|+|b|) regardless of result size; the result of `intersect(a,b)`
  is at most `min(|a|, |b|)`. Doing the smallest-first reduces the working
  set early. For K-way, sort the list pointers by length on entry, then
  fold `result = intersect(lists[0], lists[1])`, `result = intersect(result,
  lists[2])`, etc. The reference does NOT do this, so it's the cheapest
  available win on K=3 and K=5 combos.

- **Galloping search on extreme asymmetry.** When one list is much shorter
  than the other (skewed combos, ratio > ~50:1), two-finger merge wastes
  iterations on the long list. Galloping (exponential search): for each
  element of the short list, doubling-then-binary-search the long list to
  find the next candidate. Cost is O(N log(M/N)) instead of O(N+M). Watch
  for regressions on balanced inputs where galloping's branch predictor
  cost dominates; gate on observed length ratio at intersect-call time
  (e.g., `if long.len() > short.len() * 16 → galloping`).

- **Branchless two-finger inner loop.** The merge inner loop has an
  unpredictable branch (`av < bv`); on random uniform inputs the branch
  predictor sees ~50% taken and pays the misprediction cost. A branchless
  variant uses `i += (av <= bv) as usize; j += (av >= bv) as usize;` and
  `out.push` only on equality (which is rare and predictable). LLVM may
  already do this for the simple loop, check the assembly with
  `cargo asm` before claiming the change is the win.

- **Bitmap intersect on dense small-universe combos.** When density is
  high (the `dense` combos: ~50% of a 10k universe), the merge's
  per-element cost dominates. A bitmap intersect of K dense lists:
  build a u64-word bitmap per list once (O(|l|) per list), then AND the
  bitmaps word-wise (O(universe/64) total), then extract set bits via
  `trailing_zeros`. Crossover threshold: when sum-of-list-lengths exceeds
  ~universe/8, bitmap wins. Watch for memory cost: a 1M-universe bitmap
  is 128KB; allocating per-call defeats the savings. Cache the bitmap in
  `PostingIntersect`.

- **Output-buffer growth amortization.** `out.push(v)` calls inside the
  inner loop can trigger reallocation. Pre-reserve `out` to `min(|a|, |b|)`
  before the merge loop; the actual intersection is always ≤ this bound.
  Same for K-way: reserve `min` across all lists before the first merge.

### `[arch=aarch64]`, NEON (Apple Silicon, ARM servers)

- **NEON SIMD-merge with `vqtbl1q_u8` / `vminq_u32`.** Lemire's "SIMD-
  based decoding of posting lists" (2015) describes a SIMD-merge that
  compares 4-element blocks of one list against 4-element blocks of the
  other in parallel. NEON's equivalent: load `uint32x4_t` lanes, compute
  pairwise equality with `vceqq_u32`, materialize a 4-bit move mask,
  index into a shuffle-table to compact matches. The shuffle-table is a
  256-entry `[uint8x16_t; 256]` indexed by the mask. Wins on balanced
  combos with both lists > ~256 elements (below that, the SIMD setup
  overhead exceeds the merge cost).

- **Prefetch with `__prefetch`.** When the long list's stride exceeds
  one cache line, explicit prefetch of `&long[j + 16]` while processing
  `long[j]` masks DRAM latency. M1's hardware prefetcher is aggressive on
  sequential strides (so this won't help balanced/dense), but galloping's
  jumps and skewed combos' long-list scans both have less-predictable
  strides where prefetch helps.

- **Population count for bitmap extract.** `vcntq_u8` gives byte-wise
  popcount; combined with horizontal sum, gives the number of set bits
  in a 128-bit chunk. Useful for the bitmap-intersect path's extract
  phase: `popcnt` the word, then `trailing_zeros` to peel bits.

### `[arch=x86_64]`, AVX2 (Intel, AMD)

- **AVX2 SIMD-merge via `_mm256_cmpeq_epi32` + `_mm256_movemask_ps`.**
  Same shape as the NEON version above but with 8 lanes per vector
  instead of 4. The shuffle-table is `[__m256i; 256]` (8KB) indexed by
  the move mask. Crossover threshold drops to ~128 elements per list.

- **BMI2 `pext` for bitmap-intersect extract.** Parallel bit extract
  selects only the set bits of a word into the low bits, removing the
  per-bit `trailing_zeros` loop on the extract path. Modern x86 only;
  gate on `target_feature = "bmi2"`.

- **Native gather `_mm256_i32gather_epi32`** for permuted-access patterns
  during the bitmap intersect's word-walk. NEON has no equivalent.

## Canonical papers (cite in commit messages when used)

When a trial pulls a technique from one of these, cite the paper in the
commit message body and append a one-line summary to `lessons.md` under
`## References`. Per HARNESS.md "Background research", the first session
on this target should skim 1–3 of these before forming a hypothesis.

- **Lemire, Boytsov, Kurz (2015).** "SIMD-based decoding of posting lists."
  Describes SSE merge and galloping primitives; v_simdgalloping is the
  canonical hybrid. <https://arxiv.org/abs/1401.6399>
- **Inoue, Ohara, Taura (2014).** "Faster set intersection with SIMD
  instructions by reducing branch mispredictions." Branchless-merge
  variants and benchmark across density regimes.
- **Schlegel, Willhalm, Lehner (2011).** "Fast sorted-set intersection
  using SIMD instructions." Original SIMD intersect via comparison
  pyramids. <https://www.vldb.org/pvldb/vol4/p851-schlegel.pdf>
- **Daniel Lemire's `simdcomp` repo** for reference C implementations
  of SIMD intersect / merge. Read pattern, don't add as a dep.
  <https://github.com/lemire/SIMDCompressionAndIntersection>
- **Roaring Bitmap paper (Chambi et al. 2016).** Hybrid container
  approach (run, bitset, array) — relevant if you're considering a
  per-list container choice as input prep. NB: input here is plain
  `&[u32]`, not roaring; this is reference reading, not a dep.

## `results.tsv` header

```
commit	timestamp	correctness	geomean_ns	ci_lo	ci_hi	worst_ns	worst_combo	best_ns	best_combo	peak_mem_mb	total_seconds	keep	description
```

`worst_combo` and `best_combo` are formatted as `K=<arity>,<dist>` (e.g.,
`K=5,dense`). `keep` is one of `baseline`, `kept`, `rejected`, `timeout`,
`fail`.
