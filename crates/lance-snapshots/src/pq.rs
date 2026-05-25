// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright The Lance Authors
//
// Vendored from lance-format/lance @ 5cf70b27b3ad38ecdcd1547b7af385e05f67598a
// Original paths:
//   - rust/lance-index/src/vector/pq/distance.rs (build_distance_table_l2*, compute_pq_distance)
//   - rust/lance-index/src/vector/pq/utils.rs (get_sub_vector_centroids)
//   - rust/lance-index/src/vector/pq/storage.rs (transpose)
//
// Concretized to the 8-bit (num_bits=8, NUM_CENTROIDS=256) f32 path —
// the dominant Lance code path and the only one our harness exercises.
// The 4-bit path uses u8x16 SIMD shuffle (LUT16) and is intentionally omitted
// (separate kernel surface; would need its own target crate).
//
// The transpose function is concretized from `PrimitiveArray<T>` to `&[u8]`
// (the only element type PQ codes use). Same algorithm, slice-based API.

use crate::assume_eq;
use crate::l2::l2_distance_batch;

/// Number of centroids when num_bits = 8.
pub const NUM_CENTROIDS_8BIT: usize = 256;

/// Build a Distance Table from the query to each PQ centroid using L2.
///
/// Result layout: `[num_sub_vectors][num_centroids]` flat, length
/// `num_sub_vectors * NUM_CENTROIDS_8BIT`.
#[inline]
pub fn build_distance_table_l2(codebook: &[f32], num_sub_vectors: usize, query: &[f32]) -> Vec<f32> {
    let dimension = query.len();
    let sub_vector_length = dimension / num_sub_vectors;
    let num_centroids = NUM_CENTROIDS_8BIT;
    let mut result = Vec::with_capacity(num_sub_vectors * num_centroids);
    for (i, sub_vec) in query.chunks_exact(sub_vector_length).enumerate() {
        let subvec_centroids =
            get_sub_vector_centroids_8bit(codebook, dimension, num_sub_vectors, i);
        result.extend(l2_distance_batch(
            sub_vec,
            subvec_centroids,
            sub_vector_length,
        ));
    }
    result
}

/// Build a Distance Table writing into a caller-provided buffer.
///
/// `out` must have length `num_sub_vectors * NUM_CENTROIDS_8BIT`. Equivalent
/// to `build_distance_table_l2` but reuses an output buffer across queries
/// (the autoresearch harness pre-allocates one per workload).
#[inline]
pub fn build_distance_table_l2_into(
    codebook: &[f32],
    num_sub_vectors: usize,
    query: &[f32],
    out: &mut [f32],
) {
    let dimension = query.len();
    let sub_vector_length = dimension / num_sub_vectors;
    let num_centroids = NUM_CENTROIDS_8BIT;
    assume_eq!(out.len(), num_sub_vectors * num_centroids);
    for (i, sub_vec) in query.chunks_exact(sub_vector_length).enumerate() {
        let subvec_centroids =
            get_sub_vector_centroids_8bit(codebook, dimension, num_sub_vectors, i);
        let row_off = i * num_centroids;
        for (j, dist) in l2_distance_batch(sub_vec, subvec_centroids, sub_vector_length).enumerate()
        {
            out[row_off + j] = dist;
        }
    }
}

/// Compute L2 distance from the query to all code (8-bit PQ).
///
/// Parameters
/// ----------
/// - `distance_table`: pre-computed L2 distance table, flat `[num_sub_vectors][NUM_CENTROIDS_8BIT]`.
/// - `num_sub_vectors`: number of sub-quantizers (M).
/// - `code`: **transposed** PQ code; flat `[num_sub_vectors][num_vectors]` (`code[i][j]` is sub-vec i of vec j).
///
/// Returns squared L2 distances per vector, length `num_vectors`.
#[inline]
pub fn compute_pq_distance(
    distance_table: &[f32],
    num_sub_vectors: usize,
    code: &[u8],
) -> Vec<f32> {
    if code.is_empty() {
        return Vec::new();
    }
    let num_vectors = code.len() / num_sub_vectors;
    let mut distances = vec![0.0; num_vectors];
    compute_pq_distance_into(distance_table, num_sub_vectors, code, &mut distances);
    distances
}

/// Write-into variant: same as `compute_pq_distance` but reuses a caller-provided buffer.
#[inline]
pub fn compute_pq_distance_into(
    distance_table: &[f32],
    num_sub_vectors: usize,
    code: &[u8],
    distances: &mut [f32],
) {
    if code.is_empty() {
        return;
    }
    let num_vectors = distances.len();
    assume_eq!(code.len(), num_vectors * num_sub_vectors);
    const NUM_CENTROIDS: usize = NUM_CENTROIDS_8BIT;
    distances.fill(0.0);
    for (sub_vec_idx, vec_indices) in code.chunks_exact(num_vectors).enumerate() {
        let dist_table =
            &distance_table[sub_vec_idx * NUM_CENTROIDS..(sub_vec_idx + 1) * NUM_CENTROIDS];
        assume_eq!(dist_table.len(), NUM_CENTROIDS);
        assume_eq!(vec_indices.len(), distances.len());
        vec_indices
            .iter()
            .zip(distances.iter_mut())
            .for_each(|(&centroid_idx, sum)| {
                *sum += dist_table[centroid_idx as usize];
            });
    }
}

/// Extract the sub-vector codebook slice for one sub-quantizer index.
#[inline]
pub fn get_sub_vector_centroids_8bit<T>(
    codebook: &[T],
    dimension: usize,
    num_sub_vectors: usize,
    sub_vector_idx: usize,
) -> &[T] {
    debug_assert!(sub_vector_idx < num_sub_vectors);
    let num_centroids = NUM_CENTROIDS_8BIT;
    let sub_vector_width = dimension / num_sub_vectors;
    &codebook[sub_vector_idx * num_centroids * sub_vector_width
        ..(sub_vector_idx + 1) * num_centroids * sub_vector_width]
}

/// Transpose PQ codes from AoS `[num_vectors][num_sub_vectors]` to SoA
/// `[num_sub_vectors][num_vectors]`.
///
/// Concretized from upstream's generic `PrimitiveArray<T>` version to
/// `&[u8]` (the only element type PQ codes use). The transpose is what
/// makes `compute_pq_distance`'s inner loop iterate over N codes
/// contiguously per sub-quantizer row, enabling auto-vectorization.
pub fn transpose(original: &[u8], num_vectors: usize, num_sub_vectors: usize) -> Vec<u8> {
    debug_assert_eq!(original.len(), num_vectors * num_sub_vectors);
    if original.is_empty() {
        return Vec::new();
    }
    let mut transposed = vec![0u8; original.len()];
    for (vec_idx, codes) in original.chunks_exact(num_sub_vectors).enumerate() {
        for (sub_vec_idx, &code) in codes.iter().enumerate() {
            transposed[sub_vec_idx * num_vectors + vec_idx] = code;
        }
    }
    transposed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transpose_round_trip_is_inverse() {
        let n = 7;
        let m = 4;
        let original: Vec<u8> = (0..n * m).map(|x| x as u8).collect();
        let trans = transpose(&original, n, m);
        // Transpose twice should restore the original (dimensions swap each time).
        let back = transpose(&trans, m, n);
        assert_eq!(back, original);
    }

    #[test]
    fn compute_pq_distance_matches_naive() {
        // Tiny fixture: 4 vectors, 2 sub-vectors, 256 centroids.
        let num_vectors = 4;
        let num_sub_vectors = 2;
        let num_centroids = NUM_CENTROIDS_8BIT;
        let distance_table: Vec<f32> = (0..num_sub_vectors * num_centroids)
            .map(|x| x as f32 * 0.1)
            .collect();
        let codes_aos: Vec<u8> = vec![10, 20, 30, 40, 50, 60, 70, 80];
        let codes_soa = transpose(&codes_aos, num_vectors, num_sub_vectors);

        let result = compute_pq_distance(&distance_table, num_sub_vectors, &codes_soa);
        assert_eq!(result.len(), num_vectors);

        // Naive scan for comparison.
        for vi in 0..num_vectors {
            let mut expected = 0.0f32;
            for si in 0..num_sub_vectors {
                let c = codes_aos[vi * num_sub_vectors + si] as usize;
                expected += distance_table[si * num_centroids + c];
            }
            assert!((result[vi] - expected).abs() < 1e-5);
        }
    }
}
