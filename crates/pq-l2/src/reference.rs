// SPDX-License-Identifier: Apache-2.0

//! IMMUTABLE. Reference kernel — defines the math the agent must match.
//!
//! Thin wrapper around the vendored upstream code in `lance-snapshots`.
//! The oracle IS upstream's current implementation (pinned SHA in
//! `lance-snapshots/src/lib.rs` doc). Agent kernels that match this within
//! `MAX_ABS_ERR = 1e-4` are bit-equivalent to what Lance ships today.

use crate::PqShape;
use lance_snapshots::pq::{build_distance_table_l2_into, compute_pq_distance_into, transpose};

pub struct ScalarReference {
    shape: PqShape,
    codebook: Vec<f32>,
    /// Pre-transposed codes (SoA `[num_sub_vectors][num_vectors]`), as
    /// upstream's `compute_pq_distance` expects.
    codes_soa: Vec<u8>,
    num_vectors: usize,
}

impl ScalarReference {
    pub fn new(shape: PqShape, codebook: &[f32], codes_aos: &[u8], num_vectors: usize) -> Self {
        assert_eq!(codebook.len(), shape.codebook_len());
        assert_eq!(codes_aos.len(), num_vectors * shape.num_sub_vectors);
        let codes_soa = transpose(codes_aos, num_vectors, shape.num_sub_vectors);
        Self {
            shape,
            codebook: codebook.to_vec(),
            codes_soa,
            num_vectors,
        }
    }

    pub fn distance_table(&self, query: &[f32], out: &mut [f32]) {
        assert_eq!(query.len(), self.shape.dim);
        assert_eq!(out.len(), self.shape.distance_table_len());
        build_distance_table_l2_into(&self.codebook, self.shape.num_sub_vectors, query, out);
    }

    pub fn compute_distances(&self, table: &[f32], out: &mut [f32]) {
        assert_eq!(table.len(), self.shape.distance_table_len());
        assert_eq!(out.len(), self.num_vectors);
        compute_pq_distance_into(table, self.shape.num_sub_vectors, &self.codes_soa, out);
    }

    pub fn num_vectors(&self) -> usize {
        self.num_vectors
    }
}

/// Compare two distance tables and report the worst absolute element error.
pub fn max_abs_err(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max)
}

/// Compare two per-vector distance vectors. Reports max absolute error
/// across all vectors. Same tolerance contract as `max_abs_err` on the
/// distance table: differences below `MAX_ABS_ERR` are considered
/// equivalent (legitimate SIMD-accumulator reordering).
pub fn distances_max_abs_err(agent: &[f32], reference: &[f32]) -> f32 {
    assert_eq!(agent.len(), reference.len());
    agent
        .iter()
        .zip(reference)
        .map(|(a, r)| (a - r).abs())
        .fold(0.0f32, f32::max)
}

/// Check top-K positional consistency by distance value.
///
/// At each rank `i`, asserts `|agent[i].dist - reference[i].dist| <= dist_tol`.
/// Ids at the same rank may differ silently (heap eviction order vs sort
/// stability for tied distances).
pub fn topk_consistent(
    agent: &[(u32, f32)],
    reference: &[(u32, f32)],
    dist_tol: f32,
) -> Result<(), String> {
    if agent.len() != reference.len() {
        return Err(format!(
            "topk length mismatch: agent={} reference={}",
            agent.len(),
            reference.len()
        ));
    }
    for (i, ((a_id, a_d), (r_id, r_d))) in agent.iter().zip(reference).enumerate() {
        if (a_d - r_d).abs() > dist_tol {
            return Err(format!(
                "topk[{i}] distance mismatch: agent=({a_id}, {a_d}) reference=({r_id}, {r_d}) | err={}",
                (a_d - r_d).abs()
            ));
        }
    }
    Ok(())
}
