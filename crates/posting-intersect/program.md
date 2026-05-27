# Target: posting-intersect, agent instructions

**Status: closed.** Three trials landed (microbench −81% geomean). Off-path
for Lance: Step 0 trace done retroactively in `docs/targets/posting-intersect.md`
shows the kernel surface is not in `wand.rs`'s actual hot path. Useful
primitive for Tantivy/Lucene-style systems; not an upstream Lance PR.

No further trials are planned. This file is kept so the existing kernel +
bench still compile under the harness contract. If you want to revive the
target, the loop conventions are in [`../../HARNESS.md`](../../HARNESS.md)
and the upstream-shape question is in the capsule.

## Public API contract (still pinned by the bench)

```rust
pub struct PostingIntersect { /* private fields */ }

impl PostingIntersect {
    pub fn new() -> Self;
    /// K-way AND-intersect over sorted-unique `&[u32]` slices. Output sorted
    /// ascending, unique, bitwise-identical to `reference::intersect_reference`.
    pub fn intersect(&mut self, lists: &[&[u32]], out: &mut Vec<u32>);
}
```

Bench times per-call `intersect` wall-clock; 8 calls per timing window;
3-pass `--mode baseline` for tight CI.

## Results.tsv header

```
commit	timestamp	correctness	geomean_ns	ci_lo	ci_hi	worst_ns	worst_combo	best_ns	best_combo	peak_mem_mb	total_seconds	keep	description
```
