//! Autoresearch target: Lance PQ L2 distance kernel optimization.
//!
//! ## API mirrors upstream Lance
//!
//! The kernel API matches `lance-index::vector::pq::distance`'s actual split:
//!
//! 1. `PqKernel::new(shape, codebook, codes, num_vectors)`, one-time setup;
//!    pre-transposes codes from AoS `[num_vectors][num_sub_vectors]` to SoA
//!    `[num_sub_vectors][num_vectors]` (upstream's `pq::storage::transpose`).
//!    Any other agent-side pre-processing (codebook layout, c·c cache, ...)
//!    also happens here.
//! 2. `PqKernel::distance_table(query, &mut out)`, per query, build the
//!    M×K asymmetric distance table. Mirrors `build_distance_table_l2`.
//! 3. `PqKernel::compute_distances(table, &mut out)`, per query, compute N
//!    per-vector distances by indexing the table with each transposed code
//!    column. Mirrors `compute_pq_distance`.
//! 4. Top-K selection happens **outside** the kernel, in `run_experiment`.
//!    Upstream does the same; ANN top-K runs as a separate post-pass over
//!    `compute_pq_distance`'s output vec.
//!
//! ## Karpathy three-file contract (with upstream-vendored oracle)
//!
//! - `kernels`, the AGENT'S PLAYGROUND. **Starts as a clone of upstream's
//!   current SOTA** (vendored in `lance-snapshots`). The agent's job is to
//!   beat upstream, not to beat a strawman.
//! - `reference`, IMMUTABLE. Thin wrapper calling `lance-snapshots`
//!   directly. The oracle IS upstream's current code.
//! - `inputs`, IMMUTABLE. Deterministic per fixed seed; codes in natural
//!   AoS layout (the kernel transposes internally).
//!
//! Shared utilities (deterministic PRNG, geomean, peak RSS, tolerance
//! constants, time budget, PMC counters, bootstrap CI) come from the
//! `harness-common` workspace crate.

pub mod inputs;
pub mod kernels;
pub mod reference;

/// Geometry of a PQ index: vector dimension, number of sub-quantizers, centroids
/// per sub-quantizer. We pin nbits=8 (256 centroids), the dominant Lance code
/// path. `dim` must be divisible by `num_sub_vectors`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct PqShape {
    pub dim: usize,
    pub num_sub_vectors: usize,
    pub num_centroids: usize,
}

impl PqShape {
    pub const fn new(dim: usize, num_sub_vectors: usize, num_centroids: usize) -> Self {
        Self {
            dim,
            num_sub_vectors,
            num_centroids,
        }
    }
    pub const fn sub_vector_dim(&self) -> usize {
        self.dim / self.num_sub_vectors
    }
    pub const fn distance_table_len(&self) -> usize {
        self.num_sub_vectors * self.num_centroids
    }
    pub const fn codebook_len(&self) -> usize {
        self.num_sub_vectors * self.num_centroids * self.sub_vector_dim()
    }
}
