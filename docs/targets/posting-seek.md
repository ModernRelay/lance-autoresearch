# Target: `posting-seek`

## Status

**Kernel-level result in our harness; REJECTED as upstream Lance PR after
integration validation at two scales (1M and 10M docs).**

Microbench (this target, SHA `abe4dd3`): geomean 90 → 37 ns/seek
(−58%), worst combo 3011 → 74 ns (−97%) on Large × Skip-deep. Three
earlier gallop variants were rejected (mechanism notes in gitignored
`lessons.md`).

**Upstream integration: REJECTED as upstream PR.**

The same kernel change ported to `wand.rs::next/shallow_next` was
tested at two scales:

| Bench | 1M baseline | 1M patched | p | 10M baseline | 10M patched | p |
|---|---|---|---|---|---|---|
| `invert_search` (OR) | 10.50 ms | 9.98 ms | 0.19 | 65.06 ms | **69.71 ms** | **0.03** |
| `invert_phrase_search` (AND) | 18.52 ms | 18.04 ms | 0.71 | 118.60 ms | 125.08 ms | 0.84 |

**At 10M, `invert_search` REGRESSES by 12.7% with p=0.03 (statistically
significant).** The patch is correct (output unchanged) but produces
worse end-to-end performance than the linear baseline in upstream's
own bench.

This is the empirical answer to the cost-fraction question that
AGENTS.md principle 5 was added to require. My initial analysis
predicted the gallop would win at 10M because common-term posting
lists grow to ~8000 blocks and Phase 1 would dominate. **The prediction
was wrong:** WAND's block-max-score skipping (`and_move_to_next_block`
at `wand.rs:960`) skips most of the posting list at the score level,
so the modal `next(least_id)` call advances only 1–3 blocks — exactly
the regime where gallop's per-call overhead loses to a tight linear
scan. The asymptotic argument was true in isolation but irrelevant in
context, because the score-skip pre-empts the deep-skip work the
gallop would have saved.

The kernel-level autoresearch microbench is **self-fulfilling**: we
designed `skip_deep` to test the deep-skip regime, the gallop won
there by construction. The microbench never modeled WAND's outer
loop, so it never measured the shallow-skip regime that dominates
production traffic.

**Concrete revision to the methodology:** AGENTS.md principle 5 (cost-
fraction estimate) and HARNESS.md "Integration validation" are
necessary but not sufficient. Even with a correct cost-fraction
estimate, the *access pattern* the kernel sees in production may
differ from what the microbench imposes. Future targets must trace
not just *where* the kernel is called but *how* — what input
distribution the production caller actually generates.

100M+ doc scale was considered but not pursued: the trend is clear,
not regime-dependent. WAND's score-skip keeps `next()` calls shallow
at any scale; the gallop's cheap-case overhead would continue to
regress OR queries.

## What's optimized

The per-iterator seek-to-doc-id primitive that drives Lance's block-skip
WAND AND traversal. Concretely:

```rust
PostingSeek::next(&mut self, least_id: u32) -> Option<u32>
```

Advances the iterator to the smallest doc id ≥ `least_id` (or returns
None if exhausted). Operates over a block-compressed posting list
(PForDelta-style, 128 doc ids per block) with a per-block `first_doc_id`
sidecar that allows skipping whole blocks without decompression.

## Lance call site

Upstream `lance-format/lance` at SHA `5cf70b27b3ad38ecdcd1547b7af385e05f67598a`,
`rust/lance-index/src/scalar/inverted/wand.rs` lines 349-388:

```rust
fn next(&mut self, least_id: u64) {
    match self.list {
        PostingList::Compressed(ref list) => {
            let mut block_idx = self.index / BLOCK_SIZE;
            while block_idx + 1 < list.blocks.len()
                && list.block_least_doc_id(block_idx + 1) <= least_id
            {
                block_idx += 1;  // ← linear block scan; gallop opportunity
            }
            self.index = self.index.max(block_idx * BLOCK_SIZE);
            // ... then decompresses just the target block and partition_points within ...
        }
        PostingList::Plain(ref list) => {
            self.index += list.row_ids[self.index..].partition_point(|&id| id < least_id);
        }
    }
}

fn shallow_next(&mut self, least_id: u64) {
    // Same linear block scan, doesn't decompress (used by WAND for cheap
    // "could this block possibly contribute" checks).
}
```

Hot caller: `WandSearcher::next_and_candidate` at line 904 — calls
`posting.next(doc)` for every other lead iterator per candidate doc, and
`shallow_next(target)` inside `and_move_to_next_block` (line 960) for the
block-max scoring skip path.

This kernel's `PostingSeek::next` corresponds to line 349; the gallop
opportunity is the linear `while` loop at line 354 (and the symmetric
loop at line 393 inside `shallow_next`).

## Oracle

Bitwise: same `Option<u32>` return on every (block-compressed list,
least_id) input. Deterministic; integer math; no float tolerance.
Strictly stronger than `pq-l2`'s `MAX_ABS_ERR ≤ 1e-4`.

## Speed workload shape (proposed)

Three shapes (posting list size, in blocks of 128 doc ids each):

- `Small` (~10 blocks ≈ 1,280 doc ids; rare-term regime)
- `Medium` (~1,000 blocks ≈ 128k doc ids)
- `Large` (~80,000 blocks ≈ 10M doc ids; common-term-at-scale regime)

Three distributions (seek-call pattern):

- `Sequential` — `least_id` grows by ~1 block per call. Tests cache-
  friendly forward iteration; both linear and gallop should be fast,
  gallop's overhead should not regress here.
- `Skip-shallow` — `least_id` grows by ~10 blocks per call. Linear scan
  walks ~10 sidecar entries; gallop probes log₂(10) ≈ 4 then bisects.
  Marginal regime.
- `Skip-deep` — `least_id` jumps half the list per call. Linear scan
  walks ~N/2 sidecar entries; gallop probes log₂(N) then bisects.
  **This is where the asymptotic win materializes.** For Large × Skip-deep:
  linear ~40,000 probes, gallop ~17 probes, ~2,300× theoretical.

## Cost fraction (per AGENTS.md principle 5)

This section was added retroactively after the integration validation
revealed the kernel was a small fraction of total query cost at
upstream's bench scale. Future targets should file this section
BEFORE scaffolding.

At **1M-doc bench scale** (upstream `rust/lance-index/benches/inverted.rs`,
`TOTAL = 1_000_000`):
- Common-term Zipf posting list: ~100k docs → ~800 blocks
- Per `next()` call: linear scan walks up to ~800 sidecar reads × ~4 ns
  = ~3 µs
- Calls per query: ~100 (block-skip WAND skips most docs)
- Per-query kernel cost: ~300 µs
- Total query cost (measured): 18 ms (phrase) / 10.5 ms (OR)
- **Kernel fraction: ~2% (phrase) / ~3% (OR)**

A 100× kernel speedup at this scale is bounded by ~3% production
impact. Matches the empirical result: criterion reports
"no change in performance detected" (p > 0.05) on both benches.

At **10M-doc scale (estimated):**
- Common-term posting list: ~1M docs → ~8000 blocks
- Per `next()` call: linear ~32 µs, gallop ~50 ns → savings ~32 µs/call
- Per-query savings: ~3 ms (assuming ~100 calls/query)
- Total query cost: estimated ~30 ms
- **Kernel fraction: ~15% (phrase) — measurable**

At **100M-doc scale (won't fit in RAM on dev machine):**
- Common-term posting list: ~10M docs → ~80,000 blocks
- Per call: linear ~320 µs, gallop ~80 ns → savings ~320 µs/call
- **Kernel fraction: ~30%+**

The gallop's asymptotic O(log N) vs O(N) advantage materializes only
at scales where common-term posting lists are large. Upstream's
default 1M-doc bench is below the threshold.

## Upstream integration

Validation against upstream `lance-format/lance` @ SHA `5cf70b27`,
bench `rust/lance-index/benches/inverted.rs`.

**At 1M-doc scale (upstream's default):**

| Bench | Baseline | Patched | Δ | p-value | Verdict |
|---|---|---|---|---|---|
| `invert_search` (OR, 15 tokens) | 10.504 ms | 9.980 ms | −4.9% | 0.19 | No change |
| `invert_phrase_search` (AND, 2 tokens) | 18.519 ms | 18.041 ms | −1.0% | 0.71 | No change |

Both deltas below criterion's `p < 0.05` significance threshold. Patch
verified correct (output unchanged); just produces no measurable
end-to-end speedup at this scale.

**At 10M-doc scale (`TOTAL = 10_000_000`):**

| Bench | Baseline | Patched | Δ | p-value | Verdict |
|---|---|---|---|---|---|
| `invert_search` (OR, 15 tokens) | 65.06 ms | 69.71 ms | **+12.7%** | **0.03** | **REGRESSION** |
| `invert_phrase_search` (AND, 2 tokens) | 118.60 ms | 125.08 ms | +3.8% | 0.84 | No change |

**The patch regressed OR queries at 10M scale by 12.7% with p=0.03,
contradicting the cost-fraction prediction of ~15% improvement.**

Diagnosis: WAND's outer-loop block-max-score skipping
(`and_move_to_next_block` at `wand.rs:960`) prunes most of the posting
list at the score level before `next(least_id)` is called. As a result,
the modal `next` call advances 1-3 blocks (shallow skip), not 8000
blocks (deep skip). The gallop's per-call setup cost (5 reads vs 2-3
for the bare linear scan) loses to the simpler baseline in the regime
that actually dominates.

The cost-fraction prediction assumed `next()` calls would be drawn
from a "deep skip" distribution as corpus grows. The empirical access
pattern is the opposite: deeper score-skip in the outer loop means
*shallower* doc-id skips in the inner `next()` loop. Phase 1 cost
grew (the kernel got bigger), but the gallop's overhead grew with it.

**100M+ scales not pursued.** The trend is mechanism-driven, not
scale-driven: WAND's score-skip preserves shallow access patterns at
any corpus size.

## Known headroom (priors for the agent)

1. **Gallop the block scan.** Replace the linear `while ... { block_idx
   += 1; }` in both `next` and `shallow_next` with exponential search +
   bisect, same as `posting-intersect/kernels.rs::gallop_intersect` but
   over blocks (the `block_least_doc_id` sidecar) instead of elements.
   ~5 lines of code change in `wand.rs`. **This is the headline
   opportunity** and a paper-thin upstream PR.

2. **`partition_point` within block** — already a binary search,
   likely already optimal. Don't touch unless trial data shows otherwise.

3. **Branchless block-max-doc-id check** — possibly marginal; explore
   only after #1 lands.

4. **Prefetch the next sidecar entry** during the gallop's exponential
   phase — M1 hardware prefetcher handles sequential strides well; explicit
   prefetch helps mainly on the bisect phase's random access pattern.

## Scaffold checklist (for the future session that lands this target)

Per `docs/adding-a-target.md`:

- [x] **Step 0 done** (this capsule).
- [ ] `./scripts/scaffold-target.sh posting-seek`
- [ ] Rewrite `lib.rs` — `PostingShape { block_size: usize, num_blocks: usize }`
- [ ] Rewrite `reference.rs` — scalar linear-scan, matches current Lance's
      `PostingIterator::next` semantics exactly.
- [ ] Rewrite `inputs.rs` — 3 shapes × 3 seek-pattern distributions;
      synthesize block-compressed posting lists + the sidecar.
- [ ] Rewrite `kernels.rs` — starts as scalar linear-scan (clone of reference);
      first trial is the gallop block scan.
- [ ] Rewrite `bin/run_experiment.rs` — per-seek timing, batched if per-call
      is sub-µs (likely needed; seek is cheap).
- [ ] Write `program.md` — priors above; reference `posting-intersect`'s
      `lessons.md` for the gallop mechanism, branchless-step caveat,
      worst-case-guard discipline.
- [ ] Verify build + baseline runs.
- [ ] Mark `landed` in README target table.

## Why this capsule exists without a crate

`posting-intersect` was scaffolded and landed without doing the Step 0
hot-path trace. The trace, done retroactively as a validation pass,
showed the kernel surface doesn't match Lance's WAND-driven AND
traversal. `posting-seek` is what Step 0 would have produced first.
Stubbing the capsule (rather than scaffolding the crate immediately)
locks in the design intent while it's fresh, lets reviewers see how
Step 0 changes target selection, and gives a future agent a
ready-to-scaffold starting point.

The autoresearch loop's value depends on starting from the right kernel
surface; this capsule documents what "right" looks like for this corner
of Lance.
