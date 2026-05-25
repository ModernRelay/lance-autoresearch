// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright The Lance Authors
//
// Vendored from lance-format/lance @ 5cf70b27b3ad38ecdcd1547b7af385e05f67598a
// Original path: rust/lance-linalg/src/distance/l2.rs
//
// Concretized to f32 (the only type our harness uses). The original generic
// `L2` trait, fp16 kernels, bf16, f64, u8 paths are omitted. The
// `l2_scalar<LANES=16>` strategy is preserved verbatim: it relies on LLVM
// to auto-vectorize chunks_exact(16) of (a-b)² + sum, which on aarch64
// emits vsubq_f32 + vfmaq_f32 + vaddvq_f32, and on x86-64 with AVX2/AVX-512
// emits vsubps + vfmadd231ps + horizontal sum. See upstream PR #2450 for
// why LANES=16.
//
// `L2Prepared` is the upstream "pre-transposed targets in SoA layout" struct
// used by the build_distance_table_l2_prepared path. The `accumulate_l2_dimension`
// helper is marked `#[inline(never)]` to force the &[f32] / &mut [f32]
// signature so LLVM proves non-aliasing and emits packed SIMD
// (vbroadcastss + vsubps + vfmadd231ps on x86; the NEON equivalent on aarch64).

use crate::assume_eq;

/// Calculate the L2 distance between two f32 vectors. Relies on LLVM
/// auto-vectorization over chunks_exact(LANES=16).
#[inline]
pub fn l2_f32(from: &[f32], to: &[f32]) -> f32 {
    l2_scalar_f32::<16>(from, to)
}

/// Concretized f32 version of upstream's generic `l2_scalar<T, Output, LANES>`.
/// Relies on LLVM auto-vectorization + loop unrolling over `chunks_exact(LANES)`.
#[inline]
pub fn l2_scalar_f32<const LANES: usize>(from: &[f32], to: &[f32]) -> f32 {
    let x_chunks = from.chunks_exact(LANES);
    let y_chunks = to.chunks_exact(LANES);

    let s = if !x_chunks.remainder().is_empty() {
        x_chunks
            .remainder()
            .iter()
            .zip(y_chunks.remainder())
            .map(|(&x, &y)| {
                let diff = x - y;
                diff * diff
            })
            .sum::<f32>()
    } else {
        0.0
    };

    let mut sums = [0.0f32; LANES];
    for (x, y) in x_chunks.zip(y_chunks) {
        for i in 0..LANES {
            let diff = x[i] - y[i];
            sums[i] += diff * diff;
        }
    }

    s + sums.iter().copied().sum::<f32>()
}

/// Compute L2 distance between a vector and a batch of vectors.
///
/// Returns an iterator of pair-wise distance between `from` to each vector in `to`.
pub fn l2_distance_batch<'a>(
    from: &'a [f32],
    to: &'a [f32],
    dimension: usize,
) -> impl Iterator<Item = f32> + 'a {
    assume_eq!(from.len(), dimension);
    assume_eq!(to.len() % dimension, 0);
    to.chunks_exact(dimension).map(|v| l2_f32(from, v))
}

/// Accumulate squared differences for one dimension into per-target results.
///
/// Separated into its own function so that LLVM sees `row` and `result`
/// as non-aliasing via the function signature (`&[f32]` vs `&mut [f32]`),
/// enabling packed SIMD vectorization (vbroadcastss + vsubps + vfmadd231ps).
#[inline(never)]
pub fn accumulate_l2_dimension(q: f32, row: &[f32], result: &mut [f32]) {
    for (dist, &target) in result.iter_mut().zip(row.iter()) {
        let diff = q - target;
        *dist += diff * diff;
    }
}

/// Pre-transposed target vectors for batched L2 distance computation.
///
/// Stores targets in SoA layout `[dimension][num_targets]` so the inner
/// distance loop iterates over targets contiguously. The AoS-to-SoA
/// transpose is done once at construction; callers should reuse the
/// struct across many queries to amortize that cost.
///
/// **Cache constraint**: this is designed for cases where
/// `num_targets × dimension × 4` fits in L1 cache (~32 KB), such as PQ
/// sub-vector codebooks (e.g. 256 centroids × 16 dims = 16 KB).
#[derive(Debug, Clone)]
pub struct L2Prepared {
    transposed: Vec<f32>,
    dimension: usize,
    num_targets: usize,
}

impl L2Prepared {
    /// Transpose `targets` from AoS `[num_targets][dimension]` to SoA layout.
    pub fn new(targets: &[f32], dimension: usize) -> Self {
        let num_targets = targets.len() / dimension;
        debug_assert_eq!(targets.len(), num_targets * dimension);

        let mut transposed = vec![0.0f32; targets.len()];
        for t in 0..num_targets {
            for d in 0..dimension {
                transposed[d * num_targets + t] = targets[t * dimension + d];
            }
        }

        Self {
            transposed,
            dimension,
            num_targets,
        }
    }

    /// Compute L2 distances from `query` to every target, writing into `out`.
    ///
    /// `out` must have length `num_targets`. It will be zeroed before accumulation.
    pub fn distances_into(&self, query: &[f32], out: &mut [f32]) {
        debug_assert_eq!(query.len(), self.dimension);
        debug_assert_eq!(out.len(), self.num_targets);

        out.fill(0.0);
        for (d, &q) in query.iter().enumerate() {
            let row = &self.transposed[d * self.num_targets..][..self.num_targets];
            accumulate_l2_dimension(q, row, out);
        }
    }

    pub fn num_targets(&self) -> usize {
        self.num_targets
    }

    pub fn dimension(&self) -> usize {
        self.dimension
    }
}
