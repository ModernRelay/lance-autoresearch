//! Autoresearch target: sorted u32 posting-list AND intersection.
//!
//! ## What this optimizes
//!
//! The inner kernel of a full-text-search AND query: given K sorted slices
//! of `u32` document IDs (no duplicates within a list), produce the sorted
//! intersection. This is the per-call hot path of `lance-index::scalar::inverted`
//! when serving multi-term boolean queries.
//!
//! ## Why no `lance-snapshots` vendor
//!
//! Unlike `pq-l2`, upstream Lance does **not** expose `intersect_sorted_u32`
//! as a standalone function. The set-intersect logic is fused inside
//! `wand.rs` / `iter.rs` with WAND scoring, block-compressed posting
//! decompression, and Arrow array iteration. Vendoring that wouldn't give
//! the agent a clean kernel to optimize, it would give a tangled iterator.
//!
//! So this target inverts the pattern: define the intersect primitive
//! cleanly, let the agent find the best algorithm, then the upstream PR
//! adds the primitive AND wires it into the WAND inner loop.
//!
//! Correctness oracle is therefore `reference.rs`: a clean K-way scalar
//! merge. Output is integer doc IDs, so the gate is bitwise `Vec<u32>`
//! equality (no float tolerance). That's a strictly stronger contract than
//! `pq-l2`'s `MAX_ABS_ERR ≤ 1e-4`.
//!
//! ## Karpathy three-file contract
//!
//! - `kernels`, the AGENT'S PLAYGROUND. Starts as the same K-way merge as
//!   `reference` so the floor is "do no worse than the scalar baseline."
//!   All algorithmic exploration (galloping, SIMD-merge, bitmap, hash-set
//!   probe) happens here.
//! - `reference`, IMMUTABLE. Plain scalar K-way merge. The oracle.
//! - `inputs`, IMMUTABLE. Deterministic per seed. Posting lists generated
//!   in regimes that separate the algorithm families (balanced / skewed /
//!   dense), so a kernel that wins via one trick on one regime but
//!   regresses on another fails the worst-case guard.
//!
//! Shared utilities (deterministic PRNG, geomean, peak RSS, time budget,
//! PMC counters, bootstrap CI) come from the `harness-common` workspace crate.

pub mod inputs;
pub mod kernels;
pub mod reference;

/// Shape parameter for posting-intersect: the K-way arity (number of lists
/// to AND-intersect in one call).
///
/// The universe size and list densities are picked per-distribution in
/// `inputs.rs`, not pinned by the shape, because the interesting algorithmic
/// crossovers (bitmap vs galloping vs SIMD-merge) are density-driven, not
/// arity-driven.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct PostingShape {
    /// Number of posting lists in one intersect call. K=2 is the WAND inner
    /// loop's common case; K=3 and K=5 test K-way merge / left-fold pivot
    /// choices.
    pub num_lists: usize,
}

impl PostingShape {
    pub const fn new(num_lists: usize) -> Self {
        Self { num_lists }
    }
}
