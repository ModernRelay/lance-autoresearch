// SPDX-License-Identifier: Apache-2.0

//! IMMUTABLE. Diverse test-data + workload generators for posting-seek.
//!
//! Two surfaces:
//!
//! - `correctness_battery(seed)`: 15 shape × edge-case pairs. Each yields
//!   a posting list and a fixed seek-call sequence; the bench compares
//!   `Option<u32>` returns element-by-element against the reference.
//!
//! - `speed_workloads(seed)`: 9 shape × seek-pattern pairs. Each yields
//!   a larger posting list and a 1000-call seek sequence in one of three
//!   patterns: Sequential (every call advances by ~1 block), Skip-shallow
//!   (every call advances by ~16 blocks), Skip-deep (every call advances
//!   by ~1/10 of the list, exhausting and resetting roughly every 10
//!   seeks). The Skip-deep pattern is where the gallop-the-block-scan
//!   win materializes; Sequential and Skip-shallow stress that gallop's
//!   dispatch overhead doesn't regress the cheap-case.
//!
//! ## Posting list layout
//!
//! Each block contains exactly `BLOCK_SIZE` doc ids, sorted ascending.
//! Generation pattern: block `k` holds doc ids `k * STRIDE + [0, 2, 4,
//! ..., 2 * (BLOCK_SIZE - 1)]` where `STRIDE = 2 * BLOCK_SIZE`. So:
//!   - Block first doc ids: `0, 256, 512, 768, ...`
//!   - All doc ids even; adjacent blocks non-overlapping.
//!
//! Deterministic, easy to reason about, easy to construct synthetic
//! seek targets that land between blocks (an odd value forces the seek
//! to overshoot inside its target block, exercising the partition_point
//! path in `next`'s Phase 2).

use crate::{BLOCK_SIZE, PostingList, PostingShape};
use harness_common::SplitMix64;

/// Shapes the bench evaluates. Three orders of magnitude in posting list
/// length so the asymptotic O(N) → O(log N) win on Skip-deep is visible.
pub const SHAPES: &[PostingShape] = &[
    PostingShape::new(100),    // ~12.8k docs, rare-term-ish
    PostingShape::new(10_000), // ~1.28M docs
    PostingShape::new(80_000), // ~10.24M docs, common-term-at-scale
];

#[derive(Clone, Copy, Debug)]
pub enum SeekPattern {
    /// Persistent cursor, ~1 block step per call. Linear scan walks ~1
    /// sidecar entry per call regardless of list size. Gallop should
    /// match (its exponential phase starts at step=1).
    Sequential,
    /// Persistent cursor, ~16 block step per call. Linear walks ~16
    /// sidecar entries per call. Gallop probes log₂(16) + bisects ≈ 8.
    /// Modest regime.
    SkipShallow,
    /// Cursor reset every ~10 seeks (driven by exhaustion), each seek
    /// jumps ~1/10 of the list from cursor. Linear walks ~num_blocks/10
    /// sidecar entries per call. Gallop walks ~log₂(num_blocks/10).
    /// Asymptotic win regime.
    SkipDeep,
}

pub const PATTERNS: &[SeekPattern] = &[
    SeekPattern::Sequential,
    SeekPattern::SkipShallow,
    SeekPattern::SkipDeep,
];

/// Number of timed seek calls per (shape, pattern) speed workload.
pub const NUM_SEEKS_PER_COMBO: usize = 1024;

/// Block-stride: spacing between adjacent blocks in the synthetic layout.
/// `STRIDE = 2 * BLOCK_SIZE` so doc ids within a block fill [k*STRIDE,
/// k*STRIDE + 2*BLOCK_SIZE) at every-other-value density.
const STRIDE: u32 = (2 * BLOCK_SIZE) as u32;

/// An operation in the seek sequence the bench executes. `Reset` is
/// untimed (resets the cursor to block 0); `Seek` is timed.
#[derive(Clone, Copy, Debug)]
pub enum SeekOp {
    Reset,
    Seek(u32),
}

pub struct CorrectnessCase {
    pub label: &'static str,
    pub shape: PostingShape,
    pub list: PostingList,
    pub ops: Vec<SeekOp>,
}

pub struct SpeedWorkload {
    pub shape: PostingShape,
    pub pattern: SeekPattern,
    pub list: PostingList,
    pub ops: Vec<SeekOp>,
}

pub fn correctness_battery(seed: u64) -> Vec<CorrectnessCase> {
    let mut out = Vec::new();

    // Per-shape: 5 patterns/edges = 15 cases total.
    let cases: &[(EdgeKind, &'static str)] = &[
        (EdgeKind::Sequential, "sequential"),
        (EdgeKind::SkipShallow, "skip_shallow"),
        (EdgeKind::SkipDeep, "skip_deep"),
        (EdgeKind::PastEnd, "past_end"),
        (EdgeKind::BeforeStart, "before_start"),
    ];

    for &shape in SHAPES {
        for (edge, label) in cases {
            let mut rng = SplitMix64::new(mix_seeds(&[
                seed,
                shape_hash(shape),
                edge_hash(*edge),
            ]));
            let ops = build_correctness_ops(shape, *edge, &mut rng);
            // Each correctness case gets its own owned list (PostingList
            // isn't Copy); cheap-enough at correctness sizes.
            out.push(CorrectnessCase {
                label,
                shape,
                list: build_posting_list(shape),
                ops,
            });
        }
    }

    out
}

pub fn speed_workloads(seed: u64) -> Vec<SpeedWorkload> {
    let mut out = Vec::new();
    for &shape in SHAPES {
        for &pattern in PATTERNS {
            let mut rng = SplitMix64::new(mix_seeds(&[
                seed,
                shape_hash(shape),
                pattern_hash(pattern),
            ]));
            let list = build_posting_list(shape);
            let ops = build_speed_ops(shape, pattern, &mut rng);
            out.push(SpeedWorkload {
                shape,
                pattern,
                list,
                ops,
            });
        }
    }
    out
}

/// Build a posting list with the synthetic block layout described in the
/// module header.
fn build_posting_list(shape: PostingShape) -> PostingList {
    let num_blocks = shape.num_blocks;
    let mut block_first_doc_id = Vec::with_capacity(num_blocks);
    let mut blocks = Vec::with_capacity(num_blocks * BLOCK_SIZE);
    for k in 0..num_blocks {
        let base = (k as u32) * STRIDE;
        block_first_doc_id.push(base);
        for i in 0..BLOCK_SIZE {
            blocks.push(base + (2 * i) as u32);
        }
    }
    PostingList {
        block_first_doc_id,
        blocks,
    }
}

#[derive(Clone, Copy, Debug)]
enum EdgeKind {
    Sequential,
    SkipShallow,
    SkipDeep,
    /// Seek past the end of the list: should return None.
    PastEnd,
    /// First seek with least_id=0 followed by a normal advance.
    BeforeStart,
}

fn build_correctness_ops(shape: PostingShape, kind: EdgeKind, _rng: &mut SplitMix64) -> Vec<SeekOp> {
    let num_docs = shape.num_docs() as u32;
    let max_id = (shape.num_blocks as u32).saturating_mul(STRIDE);
    match kind {
        EdgeKind::Sequential => {
            // 8 advancing seeks at ~1-block intervals.
            (1..=8)
                .map(|i| SeekOp::Seek(i as u32 * STRIDE / 2))
                .collect()
        }
        EdgeKind::SkipShallow => (1..=8)
            .map(|i| SeekOp::Seek(i as u32 * 16 * STRIDE / 4))
            .collect(),
        EdgeKind::SkipDeep => {
            // Alternate Reset + jump to mid-list.
            let mid = max_id / 2;
            let mut ops = Vec::with_capacity(16);
            for i in 0..4 {
                ops.push(SeekOp::Reset);
                ops.push(SeekOp::Seek(mid + (i as u32) * STRIDE));
            }
            ops
        }
        EdgeKind::PastEnd => vec![
            SeekOp::Seek(max_id),                // exactly at end
            SeekOp::Seek(max_id.saturating_add(1_000_000)), // far past end
            SeekOp::Reset,
            SeekOp::Seek(u32::MAX),
        ],
        EdgeKind::BeforeStart => {
            let _ = num_docs;
            vec![
                SeekOp::Seek(0),
                SeekOp::Seek(1),
                SeekOp::Seek(STRIDE / 4),
                SeekOp::Reset,
                SeekOp::Seek(0),
            ]
        }
    }
}

fn build_speed_ops(shape: PostingShape, pattern: SeekPattern, _rng: &mut SplitMix64) -> Vec<SeekOp> {
    let max_id = (shape.num_blocks as u32).saturating_mul(STRIDE);

    let step = match pattern {
        SeekPattern::Sequential => STRIDE,             // ~1 block
        SeekPattern::SkipShallow => 16 * STRIDE,       // ~16 blocks
        SeekPattern::SkipDeep => (max_id / 10).max(STRIDE), // ~1/10 of list
    };

    let mut ops = Vec::with_capacity(NUM_SEEKS_PER_COMBO * 2);
    let mut cur_target: u32 = 0;
    let mut seeks_done = 0;
    while seeks_done < NUM_SEEKS_PER_COMBO {
        // Adding step may overflow on very large lists; saturate.
        cur_target = cur_target.saturating_add(step);
        if cur_target >= max_id {
            // Cursor will exhaust on the next seek. Reset and restart
            // the walk; the Reset op is untimed, the Seek that follows
            // measures the from-zero scan (where the gallop opportunity
            // is largest on SkipDeep).
            ops.push(SeekOp::Reset);
            cur_target = step;
        }
        ops.push(SeekOp::Seek(cur_target));
        seeks_done += 1;
    }
    ops
}

fn mix_seeds(parts: &[u64]) -> u64 {
    let mut mixed: u64 = 0;
    for &p in parts {
        mixed = mixed.wrapping_add(p).wrapping_add(0x9E37_79B9_7F4A_7C15);
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        mixed ^= mixed >> 31;
    }
    mixed
}

fn shape_hash(s: PostingShape) -> u64 {
    (s.num_blocks as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

fn pattern_hash(p: SeekPattern) -> u64 {
    let tag: u64 = match p {
        SeekPattern::Sequential => 0x11,
        SeekPattern::SkipShallow => 0x22,
        SeekPattern::SkipDeep => 0x33,
    };
    tag.wrapping_mul(0xFEED_FACE_BABE_CAFE)
}

fn edge_hash(k: EdgeKind) -> u64 {
    let tag: u64 = match k {
        EdgeKind::Sequential => 0x101,
        EdgeKind::SkipShallow => 0x202,
        EdgeKind::SkipDeep => 0x303,
        EdgeKind::PastEnd => 0x404,
        EdgeKind::BeforeStart => 0x505,
    };
    tag.wrapping_mul(0xDEAD_BEEF_CAFE_F00D)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posting_list_invariants() {
        for &shape in SHAPES {
            let list = build_posting_list(shape);
            assert_eq!(list.num_blocks(), shape.num_blocks);
            assert_eq!(list.num_docs(), shape.num_docs());
            // Block first doc ids strictly increasing.
            for k in 1..list.num_blocks() {
                assert!(list.block_first_doc_id[k] > list.block_first_doc_id[k - 1]);
            }
            // Doc ids within each block strictly increasing.
            for k in 0..list.num_blocks() {
                let block = list.block(k);
                for i in 1..block.len() {
                    assert!(block[i] > block[i - 1]);
                }
            }
        }
    }

    #[test]
    fn speed_workloads_match_shapes() {
        let w = speed_workloads(0x1234);
        assert_eq!(w.len(), SHAPES.len() * PATTERNS.len());
        for wl in w {
            // Count timed seeks (excludes Reset ops).
            let timed = wl.ops.iter().filter(|op| matches!(op, SeekOp::Seek(_))).count();
            assert_eq!(timed, NUM_SEEKS_PER_COMBO);
        }
    }

    #[test]
    fn correctness_battery_is_deterministic() {
        let a = correctness_battery(0xABCD);
        let b = correctness_battery(0xABCD);
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x.shape, y.shape);
            assert_eq!(x.ops.len(), y.ops.len());
        }
    }
}
