# Target: `posting-intersect`

Sorted `u32` posting-list AND intersection. Microbench result; **off-path
for Lance**, not an upstream PR candidate.

## Status

**Closed.** Three kept trials (branchless merge → galloping at length
ratio >16× → NEON 4×4 cross-product SIMD merge); cumulative −81% geomean
microbench vs the scalar K-way merge baseline, bit-equivalent output,
no per-combo regressions.

The kernel surface (`PostingIntersect::intersect(&[&[u32]], &mut Vec<u32>)`)
is **not** directly called by upstream Lance. WAND walks K iterators
forward via `PostingIterator::next(least_id)` seeks
(`lance-index::scalar::inverted::wand.rs::next_and_candidate`), never
materializing K decompressed posting lists for pairwise intersect.

The trial wins are kernel engineering on a primitive that:
- Other search systems (Tantivy, Lucene, custom indexes) DO use directly.
- Could be introduced as a new primitive in a hypothetical Lance refactor.
- Does NOT directly translate to a current Lance FTS speedup.

## Lance call site

**No direct call site in current upstream.** Closest analog at
`lance-format/lance` SHA `5cf70b27`, `wand.rs:904`:

```rust
// next_and_candidate — picks a candidate doc from lead[0], then for
// each other lead iterator calls posting.next(doc) to seek to >= doc.
// No pairwise intersect of full posting lists happens.
for posting in self.lead.iter_mut().skip(1) {
    if posting.doc()?.doc_id() < doc {
        posting.next(doc);
    }
    ...
}
```

The Lance-aligned target shape (seek primitive, not intersect primitive)
is [`posting-seek`](posting-seek.md). The gallop *mechanism* from this
target's trial 2 ports there as a ~5-line change in `wand.rs::next` —
but the integration result was negative (see posting-seek capsule).

## What was measured

`PostingIntersect::intersect(lists: &[&[u32]], out: &mut Vec<u32>)`,
K-way AND-intersect. Oracle is bitwise `Vec<u32>` equality against a
clean two-finger merge reference. Workload: 3 shapes (K=2/3/5) × 3
distributions (balanced/skewed/dense), 128 instances per combo, batches
of 8 per timing window. Trial wall-clock ~2 s (1-pass) / ~6 s (3-pass).

Trial-by-trial mechanism notes lived in `crates/posting-intersect/lessons.md`
(gitignored). The headline numbers and call-site argument here are the
durable record.

## Lesson for the harness

This capsule was written before `docs/adding-a-target.md` Step 0 existed.
Step 0 (mandatory upstream hot-path trace before scaffolding) was added
in response to this mis-scope. AGENTS.md principle 4 ("Mirror upstream's
surface, don't invent") encodes the same lesson at the principle level.
Future targets that fail Step 0 won't reach a trial loop.
