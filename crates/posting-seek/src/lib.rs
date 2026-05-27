//! Autoresearch target: block-aware seek over compressed posting list.
//!
//! ## What this optimizes
//!
//! The per-iterator `next(least_id)` primitive that drives Lance's WAND
//! AND traversal. Given a posting list represented as a sequence of
//! fixed-size blocks of doc ids (sorted ascending) plus a per-block
//! `first_doc_id` sidecar, advance the cursor to the smallest doc id
//! ≥ `least_id`. Mirrors upstream's `PostingIterator::next` in
//! `lance-index::scalar::inverted::wand.rs` (line 349 at SHA
//! `5cf70b27`).
//!
//! ## Why this target exists (and posting-intersect doesn't)
//!
//! `posting-intersect` measures an in-memory K-way sorted-u32 intersect
//! primitive that Lance's FTS code does **not** call. The actual hot
//! path is `PostingIterator::next` driven by WAND. This target measures
//! the right primitive; see `docs/targets/posting-seek.md` for the
//! Lance call site quote.
//!
//! ## Karpathy three-file contract
//!
//! - `kernels`, the AGENT'S PLAYGROUND. Starts as a verbatim port of
//!   Lance's linear block-scan in `wand.rs::next`. The agent's job
//!   is the gallop-the-block-scan optimization (~O(N) → O(log N) on
//!   deep-skip patterns).
//! - `reference`, IMMUTABLE. Same linear-scan code as the starting
//!   kernel. The oracle.
//! - `inputs`, IMMUTABLE. Deterministic per seed. Generates posting
//!   lists with realistic block structure (128 doc ids per block,
//!   matches Lance's `BLOCK_SIZE`) and seek-call sequences in three
//!   patterns (Sequential / Skip-shallow / Skip-deep) that separate
//!   the algorithm regimes.
//!
//! Output gate is bitwise `Option<u32>` equality across the full seek
//! sequence. No float tolerance.

pub mod inputs;
pub mod kernels;
pub mod reference;

/// Fixed block size matching upstream Lance's `BLOCK_SIZE` constant in
/// `lance-index::scalar::inverted::builder`. The whole posting list
/// representation is sized by this; do not change.
pub const BLOCK_SIZE: usize = 128;

/// Shape parameter: number of full blocks in the posting list. All
/// blocks have exactly `BLOCK_SIZE` doc ids — no partial last block,
/// to keep the kernel surface minimal. Total doc ids = `num_blocks *
/// BLOCK_SIZE`.
///
/// The seek pattern (Sequential / Skip-shallow / Skip-deep) is held
/// orthogonally in `DataDistribution` (see `inputs`), not the shape,
/// because the interesting crossovers (linear vs gallop) are pattern-
/// driven across all sizes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct PostingShape {
    pub num_blocks: usize,
}

impl PostingShape {
    pub const fn new(num_blocks: usize) -> Self {
        Self { num_blocks }
    }
    pub const fn num_docs(&self) -> usize {
        self.num_blocks * BLOCK_SIZE
    }
}

/// Block-compressed posting list, in the layout `wand.rs` traverses.
///
/// `block_first_doc_id[k]` is the smallest doc id in block `k` — the
/// sidecar the kernel reads to skip blocks without decompressing them.
/// `blocks[k * BLOCK_SIZE + i]` is the `i`-th doc id in block `k`,
/// sorted ascending. All blocks have exactly `BLOCK_SIZE` doc ids;
/// adjacent blocks are non-overlapping (block `k+1`'s first doc id is
/// strictly greater than block `k`'s last).
pub struct PostingList {
    pub(crate) block_first_doc_id: Vec<u32>,
    pub(crate) blocks: Vec<u32>,
}

impl PostingList {
    pub fn num_blocks(&self) -> usize {
        self.block_first_doc_id.len()
    }
    pub fn num_docs(&self) -> usize {
        self.blocks.len()
    }
    /// Slice of doc ids for block `k`. Panics if `k >= num_blocks()`.
    pub fn block(&self, k: usize) -> &[u32] {
        let start = k * BLOCK_SIZE;
        &self.blocks[start..start + BLOCK_SIZE]
    }
}
