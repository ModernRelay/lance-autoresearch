// SPDX-License-Identifier: Apache-2.0
//
// AGENT'S PLAYGROUND. This is the file you (the agent) modify.
//
// **STARTING POINT: upstream Lance code.** This file starts as a clone of
// `lance-snapshots::pq` (the SOTA shape Lance ships today, pinned SHA in
// `lance-snapshots/src/lib.rs`). The reference kernel calls the SAME
// upstream functions via `lance-snapshots` directly; the agent's `kernels.rs`
// is a clone, not a strawman.
//
// **The agent's job: beat upstream.** Find optimizations on top of
// upstream's current code. The 1e-4 bit-exact gate against the reference
// (which is the unmodified upstream code) ensures any "win" is real
// numerical equivalence with what Lance ships.
//
// PUBLIC API CONTRACT (must remain stable so the bench keeps building):
//   - `pub struct PqKernel`
//   - `PqKernel::new(shape: PqShape, codebook: &[f32], codes_aos: &[u8], num_vectors: usize) -> Self`
//   - `PqKernel::distance_table(&self, query: &[f32], out: &mut [f32])`
//   - `PqKernel::compute_distances(&self, table: &[f32], out: &mut [f32])`
//   - `PqKernel::num_vectors(&self) -> usize`
//
// What you CAN do:
//   - Pre-process EVERYTHING in `new` (codebook transpose, codes transpose,
//     L2Prepared SoA layout, c·c cache, etc.). Build cost is amortized
//     across all queries, the bench measures per-query, not per-(build + query).
//   - Reorder loops, switch internal data layouts, drop down to `std::arch`
//     intrinsics under `#[cfg(target_arch = ...)]` gates (always keep a
//     portable scalar fallback so this compiles everywhere).
//   - Use `unsafe` if needed; document the invariants.
//   - Add private helpers freely.
//
// What you CANNOT do:
//   - Change the public API above.
//   - Modify lib.rs / reference.rs / inputs.rs / run_experiment.rs / benches/.
//   - Lose accuracy, the correctness phase asserts max_abs_err ≤ 1e-4 against
//     the upstream-via-lance-snapshots reference on every input. Lossy
//     techniques (u8-LUT quantization, etc.) will fail the gate.

use crate::PqShape;
use lance_snapshots::pq::{build_distance_table_l2_into, compute_pq_distance_into, transpose};

/// Kernel context. Pre-computed state derived from the codebook + codes lives here.
pub struct PqKernel {
    shape: PqShape,
    codebook: Vec<f32>,
    /// Pre-transposed codes (SoA `[num_sub_vectors][num_vectors]`). Same
    /// layout upstream uses internally.
    codes_soa: Vec<u8>,
    num_vectors: usize,
}

impl PqKernel {
    /// Build a kernel context. Pre-processing time is amortized across all
    /// queries against this kernel.
    ///
    /// `codebook` layout: `[num_sub_vectors][num_centroids][sub_vector_dim]` flat.
    /// `codes_aos` layout: `[num_vectors][num_sub_vectors]` flat (natural AoS).
    pub fn new(shape: PqShape, codebook: &[f32], codes_aos: &[u8], num_vectors: usize) -> Self {
        debug_assert_eq!(codebook.len(), shape.codebook_len());
        debug_assert_eq!(codes_aos.len(), num_vectors * shape.num_sub_vectors);
        let codes_soa = transpose(codes_aos, num_vectors, shape.num_sub_vectors);
        Self {
            shape,
            codebook: codebook.to_vec(),
            codes_soa,
            num_vectors,
        }
    }

    /// Write the asymmetric L2 distance table for one query into `out`.
    ///
    /// `out` layout: `[num_sub_vectors][num_centroids]` flat
    /// (`out[m * num_centroids + k]`). Caller pre-allocates `out` with length
    /// `shape.distance_table_len()`.
    pub fn distance_table(&self, query: &[f32], out: &mut [f32]) {
        debug_assert_eq!(query.len(), self.shape.dim);
        debug_assert_eq!(out.len(), self.shape.distance_table_len());
        build_distance_table_l2_into(&self.codebook, self.shape.num_sub_vectors, query, out);
    }

    /// Compute L2 distance from the query (via the distance table) to every
    /// vector. Writes `num_vectors` distances into `out`.
    pub fn compute_distances(&self, table: &[f32], out: &mut [f32]) {
        debug_assert_eq!(table.len(), self.shape.distance_table_len());
        debug_assert_eq!(out.len(), self.num_vectors);
        compute_pq_distance_into(table, self.shape.num_sub_vectors, &self.codes_soa, out);
    }

    pub fn num_vectors(&self) -> usize {
        self.num_vectors
    }
}
