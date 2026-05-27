# Target: posting-seek, agent instructions

This is the per-target overlay on top of [`../../HARNESS.md`](../../HARNESS.md).
Read **HARNESS.md first** for the universal loop contract (what's editable,
the metric, the loop, hygiene, never stop, paper-fetching). This file adds
the posting-seek-specific API spec and priors.

## What this target is for

Block-aware seek over a compressed posting list — the per-iterator
primitive that drives Lance's WAND AND traversal. Step 0 of
[`../../docs/adding-a-target.md`](../../docs/adding-a-target.md) is
already filed in [`../../docs/targets/posting-seek.md`](../../docs/targets/posting-seek.md);
the Lance call site is `wand.rs::next` at SHA `5cf70b27`, line 349.

A win here is a clean upstream-PR shape: a 5-line change inside
`PostingIterator::next` and `shallow_next` that gallops the existing
linear `block_least_doc_id` sidecar scan.

## Setup (once per session)

1. Read in this order:
   - `../../HARNESS.md`
   - `../../README.md`
   - `../../docs/targets/posting-seek.md` *(Step 0 capsule)*
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
3. Baseline trial (3 passes for tight CI):
   ```
   cargo run --release --bin run_experiment -p posting-seek -- --mode baseline > run.log 2>&1
   ```
   Append a row tagged `keep=baseline`, commit it. Note the `arch:` line.

4. Per-trial: default 1-pass mode for iteration speed:
   ```
   cargo run --release --bin run_experiment -p posting-seek > run.log 2>&1
   ```
   Apply the keep-gate from `HARNESS.md`: trial CI upper-bound strictly below
   current-best CI lower-bound. Per-combo regression > 5% fails the worst-
   case guard.

## Public API contract (must remain stable)

```rust
pub struct PostingSeek<'a> { /* agent's private fields */ }

impl<'a> PostingSeek<'a> {
    /// Build a seeker for the given posting list. Per-list setup goes
    /// here (e.g., a cached `&'a [u32]` slice of the sidecar to skip
    /// the Vec indirection).
    pub fn new(list: &'a PostingList) -> Self;

    /// Reset cursor to block 0. The bench calls this between deep-skip
    /// seek phases; do not assume it's cold (caches are warm).
    pub fn reset(&mut self);

    /// Seek to the smallest doc id ≥ `least_id` at or after the current
    /// cursor. Returns `Some(doc_id)` or `None` if exhausted. Bitwise-
    /// identical to `reference::PostingSeekReference::next`.
    pub fn next(&mut self, least_id: u32) -> Option<u32>;
}
```

The bench creates ONE `PostingSeek` per workload + per pass, then
replays a fixed ops sequence (mix of Seek + Reset). Each Seek is timed
in batches of `BATCH = 32` (Reset ops untimed, dispatched between
batches) to amortize the `PerfCounters` start/stop overhead (~50 ns)
across many sub-µs Seek calls.

## What you can / cannot do

(See HARNESS.md for the universal table; this is the posting-seek-
specific addition.)

- **Cannot** change the public API above.
- **Cannot** modify `lib.rs`, `reference.rs`, `inputs.rs`, or
  `run_experiment.rs`. The `PostingList` layout (sidecar + flat block
  buffer) is pinned by `lib.rs`.
- **Cannot** return different `Option<u32>` than reference on any seek
  call. Output is integer; bitwise gate, no float tolerance.
- **Can** cache references / slices of the sidecar inside `PostingSeek`
  on construction (e.g., `sidecar_slice: &'a [u32]`) to skip the
  `self.list.block_first_doc_id` indirection on the hot path.
- **Can** dispatch internally by current position or remaining-blocks
  count (e.g., switch between scalar and SIMD sidecar scan when blocks-
  remaining exceeds a threshold).

## Posting-seek-specific priors

### `[arch=any]`, algorithmic / portable

- **Gallop the sidecar scan (Phase 1 of `next`).** Replace the linear
  `while block_idx + 1 < num_blocks && sidecar[block_idx + 1] <=
  least_id { block_idx += 1; }` with exponential search + bisect over
  the same sidecar. Cost drops from O(skip-distance-in-blocks) to
  O(log skip-distance). On Skip-deep × Large (baseline ~3 µs from a
  ~8000-block linear walk), gallop walks log₂(8000) ≈ 13 sidecar
  entries plus a bisect — predicted ~50× reduction. **This is the
  headline trial.** Same mechanism as `posting-intersect`'s trial 2;
  port the structure (exponential phase doubles step; bisect the
  bracket via `slice::binary_search` or `partition_point`). The
  current sidecar element at `block_idx` is already known to be
  valid (cursor was placed there); start the gallop at offset +1.

- **Slice-deref `block_first_doc_id` once per seek.** The current
  reference accesses `self.list.block_first_doc_id[block_idx + 1]`
  inside the hot loop, which is `(Vec) → (slice) → bounds-check`. Take
  `let sidecar = self.list.block_first_doc_id.as_slice();` outside the
  loop. May be elided by LLVM already; check with `cargo asm` before
  attributing wins to this.

- **Branchless within-block bisect (Phase 2).** `partition_point` is
  already a binary search but has data-dependent branches; a
  conditional-move-based bisect can reduce mispredictions on dense
  blocks. Marginal; explore only after the gallop win lands.

- **Cache `last_block_first` for the cursor's current block.** The
  exit condition of the linear loop is `sidecar[block_idx + 1] >
  least_id`. If we cache `sidecar[block_idx + 1]` after the previous
  seek finished, we can short-circuit: if the new `least_id` is still
  below it, skip the Phase 1 scan entirely. Helps Sequential pattern
  where the cursor stays in the same block for many seeks.

### `[arch=aarch64]`, NEON (Apple Silicon, ARM servers)

- **NEON SIMD compare-and-find over sidecar chunks.** Load 4 sidecar
  entries via `vld1q_u32`, broadcast `least_id` via `vdupq_n_u32`,
  compare with `vcgtq_u32`; find the first lane where the comparison
  trips. Useful for Skip-shallow where the skip distance is in the
  4–16 range. Avoids the exponential-phase setup cost; competes with
  gallop on this regime.

- **Prefetch the sidecar.** `__prefetch(&sidecar[block_idx + 16])`
  during the gallop's exponential phase. Sidecar is a dense array of
  `u32`; 16 ahead is 64 bytes = one cache line. Strided pattern is
  hardware-prefetcher-friendly already, but explicit prefetch helps on
  cold sidecar accesses.

### `[arch=x86_64]`, AVX2 (Intel, AMD)

- **AVX2 sidecar compare.** Same as NEON SIMD compare but 8-wide via
  `_mm256_cmpgt_epi32` + `_mm256_movemask_ps`. Larger lane count makes
  this competitive against gallop for moderate skip distances.

## Canonical references (cite in commit messages when used)

- **`posting-intersect/kernels.rs::gallop_intersect`** (this workspace,
  SHA `76da21d`). Pattern: exponential search + `slice::binary_search`
  bisect. Port directly; the sidecar IS a sorted slice of u32 first-doc-ids,
  same as a posting list's doc-ids — gallop applies identically.
- **Lance upstream `wand.rs::next`** (SHA `5cf70b27`, line 349). The
  exact code being optimized; agent kernel must match its output
  bitwise.
- **McIlroy (1993) "Optimistic sorting and information theoretic
  complexity".** Where galloping for ordered-list search was introduced
  (Timsort merge), name "galloping mode" from there.

## `results.tsv` header

```
commit	timestamp	correctness	geomean_ns	ci_lo	ci_hi	worst_ns	worst_combo	best_ns	best_combo	peak_mem_mb	total_seconds	keep	description
```

`worst_combo` and `best_combo` are formatted as `<size>,<pattern>` (e.g.,
`Large,skip_deep`). `keep` is one of `baseline`, `kept`, `rejected`,
`timeout`, `fail`.
