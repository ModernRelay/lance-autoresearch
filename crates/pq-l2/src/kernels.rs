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
use lance_snapshots::pq::build_distance_table_l2_into;

/// Kernel context. Pre-computed state derived from the codebook + codes lives here.
pub struct PqKernel {
    shape: PqShape,
    codebook: Vec<f32>,
    /// Codes in natural AoS layout `[num_vectors][num_sub_vectors]`. The 4x
    /// per-vector unroll below iterates m inside i (vector-major), with M
    /// register-resident accumulators that only write to `out[i]` once per
    /// vector. Avoids upstream's loop-swap SoA-distances write pressure.
    codes_aos: Vec<u8>,
    num_vectors: usize,
}

impl PqKernel {
    pub fn new(shape: PqShape, codebook: &[f32], codes_aos: &[u8], num_vectors: usize) -> Self {
        debug_assert_eq!(codebook.len(), shape.codebook_len());
        debug_assert_eq!(codes_aos.len(), num_vectors * shape.num_sub_vectors);
        Self {
            shape,
            codebook: codebook.to_vec(),
            codes_aos: codes_aos.to_vec(),
            num_vectors,
        }
    }

    pub fn distance_table(&self, query: &[f32], out: &mut [f32]) {
        debug_assert_eq!(query.len(), self.shape.dim);
        debug_assert_eq!(out.len(), self.shape.distance_table_len());
        build_distance_table_l2_into(&self.codebook, self.shape.num_sub_vectors, query, out);
    }

    /// 4x unroll over per-vector AoS codes with register-resident accumulators.
    /// Trades upstream's loop-swap (writes N=20k distances per inner m
    /// iteration) for in-register accumulation (one write per vector).
    pub fn compute_distances(&self, table: &[f32], out: &mut [f32]) {
        debug_assert_eq!(table.len(), self.shape.distance_table_len());
        debug_assert_eq!(out.len(), self.num_vectors);
        let nsv = self.shape.num_sub_vectors;
        let nc = self.shape.num_centroids;
        let codes = self.codes_aos.as_slice();
        let num_vectors = self.num_vectors;

        // SAFETY: debug_asserts above pin codes.len() == num_vectors*nsv,
        // table.len() == nsv*nc, out.len() == num_vectors.
        let quads = num_vectors / 4;
        for q in 0..quads {
            let i = q * 4;
            let off0 = i * nsv;
            let off1 = off0 + nsv;
            let off2 = off1 + nsv;
            let off3 = off2 + nsv;
            let mut a0 = 0.0f32;
            let mut a1 = 0.0f32;
            let mut a2 = 0.0f32;
            let mut a3 = 0.0f32;
            let mut tbl_row = 0usize;
            for m in 0..nsv {
                let c0 = unsafe { *codes.get_unchecked(off0 + m) } as usize;
                let c1 = unsafe { *codes.get_unchecked(off1 + m) } as usize;
                let c2 = unsafe { *codes.get_unchecked(off2 + m) } as usize;
                let c3 = unsafe { *codes.get_unchecked(off3 + m) } as usize;
                a0 += unsafe { *table.get_unchecked(tbl_row + c0) };
                a1 += unsafe { *table.get_unchecked(tbl_row + c1) };
                a2 += unsafe { *table.get_unchecked(tbl_row + c2) };
                a3 += unsafe { *table.get_unchecked(tbl_row + c3) };
                tbl_row += nc;
            }
            unsafe {
                *out.get_unchecked_mut(i) = a0;
                *out.get_unchecked_mut(i + 1) = a1;
                *out.get_unchecked_mut(i + 2) = a2;
                *out.get_unchecked_mut(i + 3) = a3;
            }
        }
        // Tail for the residue (0..=3 leftover vectors).
        let tail_start = quads * 4;
        for i in tail_start..num_vectors {
            let off = i * nsv;
            let mut acc = 0.0f32;
            let mut tbl_row = 0usize;
            for m in 0..nsv {
                let c = unsafe { *codes.get_unchecked(off + m) } as usize;
                acc += unsafe { *table.get_unchecked(tbl_row + c) };
                tbl_row += nc;
            }
            unsafe { *out.get_unchecked_mut(i) = acc };
        }
    }

    pub fn num_vectors(&self) -> usize {
        self.num_vectors
    }
}
