// SPDX-License-Identifier: Apache-2.0
//
// AGENT'S PLAYGROUND. This is the file you (the agent) modify.
//
// **STARTING POINT.** This kernel begins as a clone of `reference.rs`'s
// scalar K-way two-finger merge. The agent's job is to make it faster
// while producing bitwise-identical `Vec<u32>` output on every input the
// correctness battery throws at it.
//
// PUBLIC API CONTRACT (must remain stable so the bench keeps building):
//   - `pub struct PostingIntersect`
//   - `PostingIntersect::new() -> Self`
//   - `PostingIntersect::intersect(&mut self, lists: &[&[u32]], out: &mut Vec<u32>)`
//
// What you CAN do:
//   - Hold reusable scratch buffers inside `PostingIntersect` (bitmap arena,
//     workspace `Vec<u32>`, prefetch ring, sorted-list-pointer table, ...)
//     that grow once and amortize across calls. The bench creates ONE
//     `PostingIntersect` per combo and reuses it across instances.
//   - Reorder lists internally (smallest-first for left-fold; longest-first
//     for galloping). The output must remain sorted ascending unique
//     regardless of internal traversal order.
//   - Add private helpers, dispatch by input shape (K, density, length
//     ratios), drop down to `std::arch` intrinsics under `#[cfg(target_arch
//     = ...)]` gates (always keep a portable scalar fallback).
//   - Use `unsafe` if needed; document the invariants you rely on (e.g.,
//     "lists are sorted-unique by caller contract").
//
// What you CANNOT do:
//   - Change the public API above.
//   - Modify lib.rs / reference.rs / inputs.rs / run_experiment.rs.
//   - Produce different output than `reference.rs`. The correctness phase
//     asserts bitwise equality of the `Vec<u32>` output on every input;
//     any divergence (extra element, missing element, wrong order, wrong
//     dedup) fails the gate.
//
// INPUT CONTRACT:
//   - Each `lists[i]` is sorted ascending with no duplicates.
//   - `lists` may have any K including 0 or 1 (K=0 → empty output; K=1 →
//     output is a copy of `lists[0]`).
//   - Any `lists[i].is_empty()` → empty output.
//   - `out` is caller-allocated; the kernel clears it on entry.

/// Reusable kernel context. Scratch buffers held here grow across calls
/// and amortize allocation cost.
pub struct PostingIntersect {
    /// Working buffer for the left-fold of K-way intersection. Avoids
    /// re-allocating per call when K > 2.
    scratch: Vec<u32>,
}

impl Default for PostingIntersect {
    fn default() -> Self {
        Self::new()
    }
}

impl PostingIntersect {
    pub fn new() -> Self {
        Self {
            scratch: Vec::new(),
        }
    }

    /// K-way intersection. Output written to `out` in sorted ascending order,
    /// no duplicates.
    pub fn intersect(&mut self, lists: &[&[u32]], out: &mut Vec<u32>) {
        out.clear();
        if lists.is_empty() {
            return;
        }
        if lists.len() == 1 {
            out.extend_from_slice(lists[0]);
            return;
        }
        if lists.iter().any(|l| l.is_empty()) {
            return;
        }

        pairwise_intersect(lists[0], lists[1], out);

        for &list in &lists[2..] {
            if out.is_empty() {
                return;
            }
            self.scratch.clear();
            pairwise_intersect(out, list, &mut self.scratch);
            std::mem::swap(out, &mut self.scratch);
        }
    }
}

/// Dispatch threshold: when one list is more than this multiple of the
/// other, gallop on the longer list instead of two-finger merging. 16x is
/// the empirical crossover point: below it, gallop's per-element setup
/// (exponential search + binary-search bracket) is dominated by the linear
/// scan it would have skipped; above it, gallop's O(N log(M/N)) wins
/// decisively over merge's O(N+M).
const GALLOP_RATIO: usize = 16;

/// Minimum side length to engage the NEON SIMD merge path. Below this, the
/// SIMD setup (LUT load, vshrn pack, vqtbl1q gather) costs more than the
/// scalar tail it would replace.
#[cfg(target_arch = "aarch64")]
const SIMD_MIN_LEN: usize = 16;

#[inline]
fn pairwise_intersect(a: &[u32], b: &[u32], out: &mut Vec<u32>) {
    if b.len() > a.len().saturating_mul(GALLOP_RATIO) {
        return gallop_intersect(a, b, out);
    }
    if a.len() > b.len().saturating_mul(GALLOP_RATIO) {
        return gallop_intersect(b, a, out);
    }

    #[cfg(target_arch = "aarch64")]
    if a.len() >= SIMD_MIN_LEN && b.len() >= SIMD_MIN_LEN {
        // SAFETY: NEON is part of the base aarch64 ISA; the function uses
        // only standard intrinsics with in-bounds loads/stores guarded by
        // the loop conditions and the explicit `out.reserve` below.
        unsafe { neon_intersect(a, b, out) };
        return;
    }

    scalar_merge_intersect(a, b, out);
}

/// Branchless scalar merge: the original `av < bv` branch mispredicts ~50%
/// on uniform data. Arithmetic comparisons let both counters advance
/// unconditionally; the remaining `av == bv` branch is rare and predictable.
#[inline]
fn scalar_merge_intersect(a: &[u32], b: &[u32], out: &mut Vec<u32>) {
    let mut i = 0usize;
    let mut j = 0usize;
    while i < a.len() && j < b.len() {
        let av = a[i];
        let bv = b[j];
        if av == bv {
            out.push(av);
        }
        i += (av <= bv) as usize;
        j += (av >= bv) as usize;
    }
}

/// Shuffle LUT for NEON match compaction. Index by the 4-bit match-mask
/// code (`0..16`); each entry is a 16-byte permutation that gathers the
/// matched u32 lanes into the low bytes via `vqtbl1q_u8`. Unused slots
/// hold `0xFF` (out-of-range -> tbl returns 0; we ignore those bytes by
/// advancing the output cursor by only `count_ones()` lanes).
#[cfg(target_arch = "aarch64")]
const SIMD_SHUFFLE_LUT: [[u8; 16]; 16] = build_shuffle_lut();

#[cfg(target_arch = "aarch64")]
const fn build_shuffle_lut() -> [[u8; 16]; 16] {
    let mut lut = [[0xFFu8; 16]; 16];
    let mut code: usize = 0;
    while code < 16 {
        let mut out_pos: usize = 0;
        let mut lane: usize = 0;
        while lane < 4 {
            if code & (1 << lane) != 0 {
                let base = (lane * 4) as u8;
                lut[code][out_pos] = base;
                lut[code][out_pos + 1] = base + 1;
                lut[code][out_pos + 2] = base + 2;
                lut[code][out_pos + 3] = base + 3;
                out_pos += 4;
            }
            lane += 1;
        }
        code += 1;
    }
    lut
}

/// NEON SIMD intersect for balanced pairs. Processes 4 elements of each
/// side per iteration via cross-product compare (Lemire 2015 §4): 4
/// `vceqq_u32` against rotated Vb find all matches in the 4×4 block;
/// `vshrn` pack -> 4-bit code -> LUT shuffle compacts matched lanes;
/// `vst1q_u8` writes 16 bytes unconditionally and the output cursor
/// advances by `popcount(code)`. Pointers advance by 4 in whichever side
/// has the smaller window-max (both if equal).
///
/// # Safety
/// Requires `a.len() >= SIMD_MIN_LEN && b.len() >= SIMD_MIN_LEN`. The
/// caller-allocated `out` may grow during reserve; SIMD writes go through
/// `as_mut_ptr().add(out.len())` after a `reserve(min(la,lb) + 4)` that
/// guarantees ≥16 bytes of headroom across every iteration.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn neon_intersect(a: &[u32], b: &[u32], out: &mut Vec<u32>) {
    use core::arch::aarch64::*;

    let la = a.len();
    let lb = b.len();
    let max_match = la.min(lb);
    out.reserve(max_match + 4);

    // Weights for the movemask reduction: per-lane multiplier 1/2/4/8
    // ANDed with the narrowed match mask, then horizontal-summed via
    // `vaddv_u16` to yield a 4-bit code in 0..16.
    let weights: uint16x4_t = unsafe { vld1_u16([1u16, 2, 4, 8].as_ptr()) };

    let mut i = 0usize;
    let mut j = 0usize;

    while i + 4 <= la && j + 4 <= lb {
        let va = unsafe { vld1q_u32(a.as_ptr().add(i)) };
        let vb = unsafe { vld1q_u32(b.as_ptr().add(j)) };
        let a_max = unsafe { *a.get_unchecked(i + 3) };
        let b_max = unsafe { *b.get_unchecked(j + 3) };

        // 4×4 cross-product compare via rotation of Vb.
        let m0 = vceqq_u32(va, vb);
        let vb1 = vextq_u32::<1>(vb, vb);
        let m1 = vceqq_u32(va, vb1);
        let vb2 = vextq_u32::<2>(vb, vb);
        let m2 = vceqq_u32(va, vb2);
        let vb3 = vextq_u32::<3>(vb, vb);
        let m3 = vceqq_u32(va, vb3);
        let m = vorrq_u32(vorrq_u32(m0, m1), vorrq_u32(m2, m3));

        // Movemask: narrow each 32-bit FF/0 lane to 16-bit FFFF/0; AND with
        // weights to isolate one bit per matching lane; horizontal-sum to a
        // 4-bit code in 0..16.
        let m_u16 = vshrn_n_u32::<16>(m);
        let weighted = vand_u16(m_u16, weights);
        let code = vaddv_u16(weighted) as usize;

        // Compact matched lanes via 16-byte table lookup.
        let shuf = unsafe { vld1q_u8(SIMD_SHUFFLE_LUT[code].as_ptr()) };
        let compacted = vqtbl1q_u8(vreinterpretq_u8_u32(va), shuf);

        // Write 16 bytes unconditionally; advance output cursor by the
        // actual match count (popcount of the 4-bit code).
        let count = (code as u32).count_ones() as usize;
        let cur_len = out.len();
        let out_ptr = unsafe { out.as_mut_ptr().add(cur_len) } as *mut u8;
        unsafe { vst1q_u8(out_ptr, compacted) };
        unsafe { out.set_len(cur_len + count) };

        // Advance pointers: by 4 in whichever side has the smaller window
        // max. If equal, advance both.
        let adv_a = (a_max <= b_max) as usize * 4;
        let adv_b = (a_max >= b_max) as usize * 4;
        i += adv_a;
        j += adv_b;
    }

    // Scalar tail. Resumes from (i, j) without re-skipping the SIMD-
    // processed prefix; correct because SIMD only advances when the
    // entire window has been compared against the other side's window.
    while i < la && j < lb {
        let av = unsafe { *a.get_unchecked(i) };
        let bv = unsafe { *b.get_unchecked(j) };
        if av == bv {
            out.push(av);
        }
        i += (av <= bv) as usize;
        j += (av >= bv) as usize;
    }
}

/// Galloping intersect: `short` has many fewer elements than `long`. For
/// each element of `short`, exponentially-search `long` from the current
/// position, then bisect the bracket. Cost is O(|short| log(|long|/|short|)).
#[inline]
fn gallop_intersect(short: &[u32], long: &[u32], out: &mut Vec<u32>) {
    let mut j = 0usize;
    for &sv in short {
        if j >= long.len() {
            return;
        }
        // Exponential search: double the step until long[j+step] >= sv or
        // we run off the end.
        let mut step = 1usize;
        while j + step < long.len() && long[j + step] < sv {
            step *= 2;
        }
        // sv lies in long[j..min(j+step+1, long.len())]; bisect that bracket.
        let hi = (j + step + 1).min(long.len());
        match long[j..hi].binary_search(&sv) {
            Ok(idx) => {
                out.push(sv);
                j += idx + 1;
            }
            Err(idx) => {
                j += idx;
            }
        }
    }
}
