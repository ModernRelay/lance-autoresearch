// SPDX-License-Identifier: Apache-2.0
//
// AGENT'S PLAYGROUND. This is the file you (the agent) modify.
//
// **STARTING POINT.** This kernel begins as a clone of `reference.rs`'s
// scalar K-way two-finger merge. The agent's job is to make it faster
// while producing bitwise-identical `Vec<u32>` output on every input the
// correctness battery throws at it.
//
// PUBLIC API CONTRACT (must remain stable so the bench keeps building):
//   - `pub struct PostingIntersect`
//   - `PostingIntersect::new() -> Self`
//   - `PostingIntersect::intersect(&mut self, lists: &[&[u32]], out: &mut Vec<u32>)`
//
// What you CAN do:
//   - Hold reusable scratch buffers inside `PostingIntersect` (bitmap arena,
//     workspace `Vec<u32>`, prefetch ring, sorted-list-pointer table, ...)
//     that grow once and amortize across calls. The bench creates ONE
//     `PostingIntersect` per combo and reuses it across instances.
//   - Reorder lists internally (smallest-first for left-fold; longest-first
//     for galloping). The output must remain sorted ascending unique
//     regardless of internal traversal order.
//   - Add private helpers, dispatch by input shape (K, density, length
//     ratios), drop down to `std::arch` intrinsics under `#[cfg(target_arch
//     = ...)]` gates (always keep a portable scalar fallback).
//   - Use `unsafe` if needed; document the invariants you rely on (e.g.,
//     "lists are sorted-unique by caller contract").
//
// What you CANNOT do:
//   - Change the public API above.
//   - Modify lib.rs / reference.rs / inputs.rs / run_experiment.rs.
//   - Produce different output than `reference.rs`. The correctness phase
//     asserts bitwise equality of the `Vec<u32>` output on every input;
//     any divergence (extra element, missing element, wrong order, wrong
//     dedup) fails the gate.
//
// INPUT CONTRACT:
//   - Each `lists[i]` is sorted ascending with no duplicates.
//   - `lists` may have any K including 0 or 1 (K=0 → empty output; K=1 →
//     output is a copy of `lists[0]`).
//   - Any `lists[i].is_empty()` → empty output.
//   - `out` is caller-allocated; the kernel clears it on entry.

/// Reusable kernel context. Scratch buffers held here grow across calls
/// and amortize allocation cost.
pub struct PostingIntersect {
    /// Working buffer for the left-fold of K-way intersection. Avoids
    /// re-allocating per call when K > 2.
    scratch: Vec<u32>,
}

impl Default for PostingIntersect {
    fn default() -> Self {
        Self::new()
    }
}

impl PostingIntersect {
    pub fn new() -> Self {
        Self {
            scratch: Vec::new(),
        }
    }

    /// K-way intersection. Output written to `out` in sorted ascending order,
    /// no duplicates.
    pub fn intersect(&mut self, lists: &[&[u32]], out: &mut Vec<u32>) {
        out.clear();
        if lists.is_empty() {
            return;
        }
        if lists.len() == 1 {
            out.extend_from_slice(lists[0]);
            return;
        }
        if lists.iter().any(|l| l.is_empty()) {
            return;
        }

        pairwise_intersect(lists[0], lists[1], out);

        for &list in &lists[2..] {
            if out.is_empty() {
                return;
            }
            self.scratch.clear();
            pairwise_intersect(out, list, &mut self.scratch);
            std::mem::swap(out, &mut self.scratch);
        }
    }
}

/// Dispatch threshold: when one list is more than this multiple of the
/// other, gallop on the longer list instead of two-finger merging. 16x is
/// the empirical crossover point: below it, gallop's per-element setup
/// (exponential search + binary-search bracket) is dominated by the linear
/// scan it would have skipped; above it, gallop's O(N log(M/N)) wins
/// decisively over merge's O(N+M).
const GALLOP_RATIO: usize = 16;

#[inline]
fn pairwise_intersect(a: &[u32], b: &[u32], out: &mut Vec<u32>) {
    if b.len() > a.len().saturating_mul(GALLOP_RATIO) {
        return gallop_intersect(a, b, out);
    }
    if a.len() > b.len().saturating_mul(GALLOP_RATIO) {
        return gallop_intersect(b, a, out);
    }
    // Branchless step for balanced inputs: the original `av < bv` branch
    // mispredicts ~50% on uniform data. Arithmetic comparisons let both
    // counters advance unconditionally; the remaining `av == bv` branch
    // is rare and predictable.
    let mut i = 0usize;
    let mut j = 0usize;
    while i < a.len() && j < b.len() {
        let av = a[i];
        let bv = b[j];
        if av == bv {
            out.push(av);
        }
        i += (av <= bv) as usize;
        j += (av >= bv) as usize;
    }
}

/// Galloping intersect: `short` has many fewer elements than `long`. For
/// each element of `short`, exponentially-search `long` from the current
/// position, then bisect the bracket. Cost is O(|short| log(|long|/|short|)).
#[inline]
fn gallop_intersect(short: &[u32], long: &[u32], out: &mut Vec<u32>) {
    let mut j = 0usize;
    for &sv in short {
        if j >= long.len() {
            return;
        }
        // Exponential search: double the step until long[j+step] >= sv or
        // we run off the end.
        let mut step = 1usize;
        while j + step < long.len() && long[j + step] < sv {
            step *= 2;
        }
        // sv lies in long[j..min(j+step+1, long.len())]; bisect that bracket.
        let hi = (j + step + 1).min(long.len());
        match long[j..hi].binary_search(&sv) {
            Ok(idx) => {
                out.push(sv);
                j += idx + 1;
            }
            Err(idx) => {
                j += idx;
            }
        }
    }
}
