# Target: posting-seek, agent instructions

**Status: closed; rejected as upstream Lance PR.** Hybrid linear-budget +
McIlroy gallop kept in our harness (microbench −58% geomean / −97% worst-
case). Upstream integration at 10M-doc scale **regressed** `invert_search`
by +12.7% (p=0.03). Full empirical breakdown in
[`../../docs/targets/posting-seek.md`](../../docs/targets/posting-seek.md)
"Upstream integration" section.

No further trials are planned. The mechanism failure is structural, not
scale-driven: WAND's outer-loop score-skip generates shallow `next()`
calls; the gallop's per-call setup loses on shallow skips at any corpus
size. AGENTS.md principle 5 was written from this lesson.

This file is kept so the existing kernel + bench compile. If you revive
the target, the loop conventions are in
[`../../HARNESS.md`](../../HARNESS.md). Note: the microbench's
`skip_deep` distribution was a self-fulfilling construction — designing
inputs that don't match production access patterns is exactly the failure
mode this capsule warns about.

## Public API contract (still pinned by the bench)

```rust
pub struct PostingSeek<'a> { /* private fields */ }

impl<'a> PostingSeek<'a> {
    pub fn new(list: &'a PostingList) -> Self;
    pub fn reset(&mut self);
    /// Seek to the smallest doc id ≥ `least_id` from the current cursor.
    /// Returns `Some(doc_id)` or `None` if exhausted. Bitwise-identical
    /// to `reference::PostingSeekReference::next`.
    pub fn next(&mut self, least_id: u32) -> Option<u32>;
}
```

Bench times per-`next` call in batches of 32; 3-pass `--mode baseline`
for tight CI.

## Results.tsv header

```
commit	timestamp	correctness	geomean_ns	ci_lo	ci_hi	worst_ns	worst_combo	best_ns	best_combo	peak_mem_mb	total_seconds	keep	description
```
