# Target: `posting-seek` (candidate, capsule only)

## Status

**Candidate, capsule only.** The target this session should have scoped
instead of `posting-intersect` for direct Lance impact. Scaffold deferred
to a future session; this capsule exists to lock in the design intent
while the upstream-validation context is fresh.

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
