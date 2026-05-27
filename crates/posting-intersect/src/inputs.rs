// SPDX-License-Identifier: Apache-2.0

//! IMMUTABLE. Diverse test-data + workload generators for posting-intersect.
//!
//! Two surfaces. `correctness_battery(seed)` yields `(shape × distribution)`
//! cases plus edge cases (one-empty, identical, disjoint, single-element
//! overlap) used to pin the agent's kernel against the reference. Same seed
//! produces the same battery, so any regression surfaces in one trial.
//!
//! `speed_workloads(seed)` yields larger `(shape × distribution)` workloads
//! used by the speed phase. Each workload contains many distinct list-set
//! instances; the bench times one intersect per instance, geomeans across all.
//!
//! ## Distributions: regimes that separate algorithm families
//!
//! - `Balanced`: equal-size lists at moderate density (~1% of universe).
//!   Tests the canonical two-finger / SIMD-merge regime.
//! - `Skewed`: one short list (~50 entries) vs others ~50k. Tests galloping
//!   / exponential-search: a naive linear merge is O(N+M); galloping is
//!   O(N log(M/N)) and dominates when N ≪ M.
//! - `Dense`: small universe (10k), each list ~50% density. Tests bitmap
//!   intersection vs sorted merge: at high density, bitmap-AND of `u64`
//!   words has fewer ops per output element than merge.
//!
//! Two additional distributions used only in the correctness battery to
//! catch arithmetic bugs that distribution-specific kernels might hide:
//!
//! - `Sparse`: very low density (~10 entries per list, universe 1M). Stresses
//!   trivial-case fast paths.
//! - `Clustered`: doc IDs come in dense runs separated by gaps. Stresses
//!   skip-aware code: a kernel that assumes uniform distribution will
//!   over-skip or under-skip.

use crate::PostingShape;
use harness_common::SplitMix64;

/// K-way arities the bench evaluates. The agent's kernel must produce
/// correct output and competitive speed on every one. K=2 is the WAND
/// inner loop's common case; K=3 covers triple-term AND; K=5 stresses
/// K-way merge / left-fold pivot choices.
pub const SHAPES: &[PostingShape] = &[
    PostingShape::new(2),
    PostingShape::new(3),
    PostingShape::new(5),
];

/// Number of distinct posting-list-set instances per speed combo. Each
/// instance is timed exactly once; geomean across instances and combos
/// is the headline metric.
pub const SPEED_INSTANCES_PER_COMBO: usize = 128;

/// How many times each instance is intersected back-to-back inside one
/// timing window. A single intersect on the smallest case (skewed K=2)
/// runs in ~hundreds of nanoseconds; the `PerfCounters::start/stop`
/// overhead (~50 ns) would dominate. Batching N intersects per
/// measurement amortizes the timer overhead while keeping per-call
/// granularity for geomean.
///
/// 8 keeps the timing-window noise floor at ~6 ns/op while still
/// resolving per-call variation across the batch's instances.
pub const SPEED_BATCH: usize = 8;

#[derive(Clone, Copy, Debug)]
pub enum DataDistribution {
    Balanced,
    Skewed,
    Dense,
}

pub const DISTRIBUTIONS: &[DataDistribution] = &[
    DataDistribution::Balanced,
    DataDistribution::Skewed,
    DataDistribution::Dense,
];

/// All distribution kinds used in the correctness battery. The speed phase
/// uses the subset in `DISTRIBUTIONS` (above); correctness adds two more
/// to catch distribution-specific bugs.
#[derive(Clone, Copy, Debug)]
enum InputKind {
    Balanced,
    Skewed,
    Dense,
    Sparse,
    Clustered,
}

pub struct PostingSet {
    /// Owned storage backing the K posting lists. Each list is a sorted-unique
    /// `Vec<u32>`; the bench borrows `&[u32]` references at call time.
    pub lists: Vec<Vec<u32>>,
}

impl PostingSet {
    pub fn as_slices(&self) -> Vec<&[u32]> {
        self.lists.iter().map(|v| v.as_slice()).collect()
    }
}

pub struct CorrectnessCase {
    pub label: &'static str,
    pub shape: PostingShape,
    pub set: PostingSet,
}

pub struct SpeedWorkload {
    pub shape: PostingShape,
    pub distribution: DataDistribution,
    /// One set per timed instance. The bench iterates these in order.
    pub instances: Vec<PostingSet>,
}

pub fn correctness_battery(seed: u64) -> Vec<CorrectnessCase> {
    let mut out = Vec::new();
    let kinds: &[(InputKind, &'static str)] = &[
        (InputKind::Balanced, "balanced"),
        (InputKind::Skewed, "skewed"),
        (InputKind::Dense, "dense"),
        (InputKind::Sparse, "sparse"),
        (InputKind::Clustered, "clustered"),
    ];

    for &shape in SHAPES {
        for (kind, label) in kinds {
            let mut rng = SplitMix64::new(mix_seeds(&[
                seed,
                shape_hash(shape),
                kind_hash(*kind),
            ]));
            let set = gen_posting_set(shape, *kind, &mut rng, /*correctness=*/ true);
            out.push(CorrectnessCase {
                label,
                shape,
                set,
            });
        }
    }

    // Edge cases at K=2 (the common WAND case). The reference handles these
    // via the bare two-finger merge; the agent's kernel must match.
    let mut rng = SplitMix64::new(mix_seeds(&[seed, 0xED6E_CA5E_u64]));
    out.push(CorrectnessCase {
        label: "edge_one_empty",
        shape: PostingShape::new(2),
        set: PostingSet {
            lists: vec![sorted_unique(&mut rng, 100, 10_000), Vec::new()],
        },
    });
    out.push(CorrectnessCase {
        label: "edge_disjoint",
        shape: PostingShape::new(2),
        set: PostingSet {
            lists: vec![
                (0..50u32).step_by(2).collect(),  // evens 0..100
                (1..50u32).step_by(2).collect(),  // odds 1..100
            ],
        },
    });
    out.push(CorrectnessCase {
        label: "edge_identical",
        shape: PostingShape::new(2),
        set: PostingSet {
            lists: vec![(0..100u32).collect(), (0..100u32).collect()],
        },
    });
    out.push(CorrectnessCase {
        label: "edge_single_overlap",
        shape: PostingShape::new(3),
        set: PostingSet {
            lists: vec![
                vec![1, 2, 3, 42, 100, 200],
                vec![5, 42, 99, 101],
                vec![42, 500, 600],
            ],
        },
    });

    out
}

pub fn speed_workloads(seed: u64) -> Vec<SpeedWorkload> {
    let mut out = Vec::new();
    for &shape in SHAPES {
        for &dist in DISTRIBUTIONS {
            let mut rng = SplitMix64::new(mix_seeds(&[
                seed,
                shape_hash(shape),
                dist_hash(dist),
            ]));
            let kind = match dist {
                DataDistribution::Balanced => InputKind::Balanced,
                DataDistribution::Skewed => InputKind::Skewed,
                DataDistribution::Dense => InputKind::Dense,
            };
            let mut instances = Vec::with_capacity(SPEED_INSTANCES_PER_COMBO);
            for _ in 0..SPEED_INSTANCES_PER_COMBO {
                instances.push(gen_posting_set(shape, kind, &mut rng, /*correctness=*/ false));
            }
            out.push(SpeedWorkload {
                shape,
                distribution: dist,
                instances,
            });
        }
    }
    out
}

/// Generate one set of K sorted-unique u32 posting lists for the given shape
/// and distribution.
///
/// `correctness` shrinks sizes by ~10x to keep the correctness phase fast;
/// the speed phase uses the full sizes that exercise the inner-loop regime.
fn gen_posting_set(
    shape: PostingShape,
    kind: InputKind,
    rng: &mut SplitMix64,
    correctness: bool,
) -> PostingSet {
    let mut lists = Vec::with_capacity(shape.num_lists);
    let scale: f64 = if correctness { 0.1 } else { 1.0 };

    match kind {
        InputKind::Balanced => {
            // Universe 1M (or 100k for correctness), each list ~1% density.
            let universe = scale_u32(1_000_000, scale);
            let len = (universe / 100).max(8);
            for _ in 0..shape.num_lists {
                lists.push(sorted_unique(rng, len as usize, universe));
            }
        }
        InputKind::Skewed => {
            // One short list (~50 / ~10 entries), others ~5% of universe.
            let universe = scale_u32(500_000, scale);
            let long_len = (universe / 20).max(16) as usize;
            let short_len = if correctness { 5 } else { 50 };
            lists.push(sorted_unique(rng, short_len, universe));
            for _ in 1..shape.num_lists {
                lists.push(sorted_unique(rng, long_len, universe));
            }
        }
        InputKind::Dense => {
            // Small universe, each list ~50% density. Bitmap-friendly regime.
            let universe = scale_u32(10_000, scale);
            let len = (universe / 2) as usize;
            for _ in 0..shape.num_lists {
                lists.push(sorted_unique(rng, len, universe));
            }
        }
        InputKind::Sparse => {
            // Very low density: 10 entries each in a 1M universe. Tests
            // trivial-case fast paths and short-list dispatch.
            let universe = scale_u32(1_000_000, scale);
            let len = if correctness { 4 } else { 10 };
            for _ in 0..shape.num_lists {
                lists.push(sorted_unique(rng, len, universe));
            }
        }
        InputKind::Clustered => {
            // Doc IDs in dense runs separated by gaps. Universe 100k, each
            // list ~5% density but distributed in 32 clusters of length ~150.
            let universe = scale_u32(100_000, scale);
            let num_clusters = 32usize;
            let cluster_len = ((universe as usize / 100).max(8)) / num_clusters;
            for _ in 0..shape.num_lists {
                let mut list = Vec::with_capacity(num_clusters * cluster_len);
                for _c in 0..num_clusters {
                    let center = rng.next_u64() as u32 % universe.saturating_sub(cluster_len as u32);
                    for d in 0..cluster_len {
                        list.push(center + d as u32);
                    }
                }
                list.sort_unstable();
                list.dedup();
                lists.push(list);
            }
        }
    }

    PostingSet { lists }
}

/// Generate `n` sorted-unique u32 values uniformly in `[0, universe)`.
///
/// Uses a Fisher-Yates-style partial shuffle when `n` is a meaningful
/// fraction of `universe` (density >= ~10%), otherwise rejection sampling.
/// Both produce uniform draws; the choice is purely a generator-speed
/// optimization to keep fixture build cheap.
fn sorted_unique(rng: &mut SplitMix64, n: usize, universe: u32) -> Vec<u32> {
    let n = n.min(universe as usize);
    if n == 0 {
        return Vec::new();
    }
    // Density threshold: above ~25% we shuffle the full domain; below, we
    // reject-sample. 25% is empirically where shuffle's O(U) becomes worse
    // than rejection's O(n) at ~10% expected retry rate.
    if (n as f64) / (universe as f64) > 0.25 {
        let mut domain: Vec<u32> = (0..universe).collect();
        // Partial Fisher-Yates: swap n elements to the front, then sort them.
        for i in 0..n {
            let j = i + (rng.next_u64() as usize) % (domain.len() - i);
            domain.swap(i, j);
        }
        let mut out: Vec<u32> = domain.into_iter().take(n).collect();
        out.sort_unstable();
        out
    } else {
        // Rejection sampling with a u64 dedup-mask. For the densities we see
        // (sparse / balanced), retries are rare.
        let mut out: Vec<u32> = Vec::with_capacity(n);
        // Use a bit-set when universe is small, hash-probe otherwise.
        if universe <= 1 << 20 {
            let words = (universe as usize).div_ceil(64);
            let mut seen = vec![0u64; words];
            while out.len() < n {
                let v = (rng.next_u64() % universe as u64) as u32;
                let w = (v >> 6) as usize;
                let b = 1u64 << (v & 63);
                if seen[w] & b == 0 {
                    seen[w] |= b;
                    out.push(v);
                }
            }
        } else {
            let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
            while out.len() < n {
                let v = (rng.next_u64() % universe as u64) as u32;
                if seen.insert(v) {
                    out.push(v);
                }
            }
        }
        out.sort_unstable();
        out
    }
}

fn scale_u32(base: u32, scale: f64) -> u32 {
    ((base as f64) * scale).max(64.0) as u32
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
    (s.num_lists as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

fn kind_hash(k: InputKind) -> u64 {
    let tag: u64 = match k {
        InputKind::Balanced => 0x11,
        InputKind::Skewed => 0x22,
        InputKind::Dense => 0x33,
        InputKind::Sparse => 0x44,
        InputKind::Clustered => 0x55,
    };
    tag.wrapping_mul(0xDEAD_BEEF_CAFE_F00D)
}

fn dist_hash(d: DataDistribution) -> u64 {
    let tag: u64 = match d {
        DataDistribution::Balanced => 0x101,
        DataDistribution::Skewed => 0x202,
        DataDistribution::Dense => 0x303,
    };
    tag.wrapping_mul(0xFEED_FACE_BABE_CAFE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correctness_battery_is_deterministic() {
        let a = correctness_battery(0xABCD);
        let b = correctness_battery(0xABCD);
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x.shape, y.shape);
            for (la, lb) in x.set.lists.iter().zip(&y.set.lists) {
                assert_eq!(la, lb);
            }
        }
    }

    #[test]
    fn lists_are_sorted_unique() {
        let cases = correctness_battery(0x1234);
        for case in &cases {
            for list in &case.set.lists {
                for w in list.windows(2) {
                    assert!(w[0] < w[1], "list not strictly increasing in {}", case.label);
                }
            }
        }
    }

    #[test]
    fn speed_workloads_match_shapes() {
        let w = speed_workloads(0x1234);
        assert_eq!(w.len(), SHAPES.len() * DISTRIBUTIONS.len());
        for wl in w {
            assert_eq!(wl.instances.len(), SPEED_INSTANCES_PER_COMBO);
            for inst in &wl.instances {
                assert_eq!(inst.lists.len(), wl.shape.num_lists);
            }
        }
    }
}
