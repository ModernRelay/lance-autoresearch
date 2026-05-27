# Target: `posting-intersect`

Sorted `u32` posting-list AND intersection: the inner kernel of FTS
boolean queries in Lance.

## Status

**Landed.** Three kept trials (branchless merge step → galloping at length
ratio >16× → NEON 4×4 cross-product SIMD merge); cumulative −81% geomean
vs the scalar K-way merge baseline, bit-equivalent output, no per-combo
regressions. See `crates/posting-intersect/lessons.md` (gitignored) for
the trial-by-trial mechanism notes.

## Scope note (READ FIRST)

This kernel surface (`PostingIntersect::intersect(&[&[u32]], &mut Vec<u32>)`)
is **not** directly called by upstream Lance's current FTS code. Lance
uses block-skip WAND with streaming `PostingIterator::next(least_id)`
seeks (`lance-index::scalar::inverted::wand.rs::next_and_candidate`),
never materializing K decompressed posting lists for pairwise intersect.

The trial wins on this target are kernel engineering on a primitive that:

- Other search systems (Tantivy, Lucene, custom indexes) DO use directly.
- Could be introduced as a new primitive in a hypothetical Lance refactor.
- Does NOT directly translate to a Lance FTS speedup in current upstream.

For the actual Lance hot path, see [`posting-seek`](posting-seek.md): a
sibling target capsule for block-aware `next(least_id)` over compressed
posting lists with the existing `block_least_doc_id` sidecar. The gallop
*mechanism* from this target's trial 2 ports directly to `posting-seek`'s
block-scan as a ~5-line change in `wand.rs::next`. That's the
upstream-PR-shaped opportunity this session actually surfaced.

This capsule was written before `docs/adding-a-target.md` Step 0 existed.
Step 0 was added in response to the mis-scope here; the failure mode is
now structurally guarded against for future targets.

## What's optimized

One function in `crates/posting-intersect/src/kernels.rs`:

- `PostingIntersect::intersect(lists: &[&[u32]], out: &mut Vec<u32>)`,
  K-way AND-intersect of K sorted-unique `u32` slices, producing the
  sorted-unique intersection in `out`. Cost is bounded by O(Σ|list_i|)
  in the naive merge; better algorithms (galloping, bitmap, SIMD-merge)
  achieve O(N log(M/N)), O(U/64), or O((N+M)/W) respectively, where
  W is SIMD width.

`PostingIntersect::new()` returns an empty kernel context; the agent may
hold any scratch buffers (bitmap arena, sorted-list-pointer table, etc.)
on it that grow across calls and amortize allocation cost.

## Lance call site

**No direct call site in current upstream.** Per the Scope note above:
Lance's WAND AND traversal walks K iterators forward via repeated
`PostingIterator::next(least_id)` seeks, never materializing K
decompressed posting lists for pairwise intersection. The relevant code
in upstream `lance-format/lance` at SHA `5cf70b27b3ad38ecdcd1547b7af385e05f67598a`,
`rust/lance-index/src/scalar/inverted/wand.rs` line 904, looks like:

```rust
// next_and_candidate — picks a candidate doc from lead[0],
// then for each other lead iterator calls posting.next(doc) to
// seek that iterator's position to >= doc. No pairwise intersect
// of full posting lists happens anywhere in the hot path.
for posting in self.lead.iter_mut().skip(1) {
    if posting.doc()?.doc_id() < doc {
        posting.next(doc);
    }
    ...
}
```

For the Lance-aligned target shape (seek primitive, not intersect
primitive), see [`posting-seek.md`](posting-seek.md).

## Upstream Lance source (refactor target)

Lance does **not** expose `intersect_sorted_u32` as a standalone function.
The set-intersect logic is fused inside `lance-index::scalar::inverted`'s
WAND traversal (`wand.rs`, `iter.rs`) with scoring, block-compressed
posting decompression, and Arrow array iteration.

**No `lance-snapshots` vendor.** Unlike `pq-l2`, there's no clean upstream
function to clone as the starting kernel. The agent's `kernels.rs` and
`reference.rs` are both clean scalar K-way merge implementations.

A winning kernel ports upstream only as part of a larger refactor:
- New file: `lance-index/src/scalar/inverted/intersect.rs` exposing
  `pub(super) fn intersect_sorted(...)`.
- Wire it into a new WAND code path that decompresses K posting lists
  and intersects them (different from the current `next`-driven
  traversal). This is a hypothetical refactor, not the kind of PR that
  drops cleanly into current upstream.
- License: Apache-2.0 (matches our Apache-2.0 directly).

For a PR that DOES drop cleanly into current upstream (gallop the block
scan in `wand.rs::next`), see [`posting-seek.md`](posting-seek.md).

## Oracle

**Bitwise `Vec<u32>` equality** against the reference K-way merge. No
float tolerance because the output type is integer; any divergence
(extra element, missing element, wrong order, wrong dedup) fails the
gate. Strictly stronger than `pq-l2`'s `MAX_ABS_ERR ≤ 1e-4`.

Asserted on every input the bench generates: 15 distribution × shape
combinations + 4 edge cases (one-empty, disjoint, identical, single-
element overlap) per trial.

## Speed workload

Three shapes (K-way arity):
- `K=2` — two-way intersect (WAND inner-loop common case)
- `K=3` — three-way intersect (3-term AND query)
- `K=5` — five-way intersect (longer query, stresses K-way merge / fold)

Three distributions (algorithm-family-discriminating):
- `Balanced` — equal-size lists, 1% density in 1M universe. Canonical
  two-finger / SIMD-merge regime.
- `Skewed` — one short list (~50 entries) vs others ~50k entries in a
  500k universe. Tests galloping / exponential-search wins.
- `Dense` — all lists ~50% density in a 10k universe. Tests bitmap
  intersection vs sorted merge.

Per (shape × distribution): 128 distinct posting-list-set instances,
intersect timed in batches of 8 (window divided by 8 to recover per-call
ns; amortizes the ~50 ns `PerfCounters` start/stop overhead). Total
trial wall-clock: ~2s (1-pass) or ~6s (`--mode baseline`, 3-pass).

## Output fields

```
correctness:                    pass | fail
arch:                           aarch64 | x86_64 | ...
passes:                         1 | 3
shapes_tested:                  K=2 K=3 K=5
distributions_tested:           balanced skewed dense
geomean_ns_per_intersect:       <u64>
geomean_ns_ci_90pct:            [<u64>, <u64>]
median_ns_per_intersect:        <u64>
geomean_cycles_per_intersect:        <u64> | n/a (no PMU access on this platform)
geomean_instructions_per_intersect:  <u64> | n/a (no PMU access on this platform)
worst_ns_per_intersect:         <u64> (<shape>, <dist>)
best_ns_per_intersect:          <u64> (<shape>, <dist>)
per_combo_geomean_ns:
  (...)
peak_mem_mb:                    <f64>
total_seconds:                  <f64>
```

## Known headroom (priors for the agent)

See `crates/posting-intersect/program.md` § "Posting-intersect-specific
priors" for the canonical arch-split list and the canonical-papers
references. The full set of caveats lives in
`crates/posting-intersect/lessons.md` (gitignored per-machine; populated
as trials surface findings).

The first session should skim 1–3 of the cited papers (Lemire 2015 SIMD-
decoding; Inoue/Ohara/Taura branchless-merge; Schlegel SIMD-intersect)
per HARNESS.md "Background research" before forming a hypothesis.
