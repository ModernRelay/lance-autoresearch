// SPDX-License-Identifier: Apache-2.0

//! IMMUTABLE. Reference kernel, defines the set-intersection result the
//! agent must match exactly.
//!
//! K-way AND-intersect over sorted-unique slices, implemented as a simple
//! left-fold of pairwise two-finger merges. No SIMD, no cleverness, no
//! galloping. The agent's kernel must produce a bitwise-identical
//! `Vec<u32>` output (same length, same values, same order); there is no
//! float tolerance because the output type is integer.

/// Reference K-way intersection. Output is sorted ascending with no
/// duplicates (an invariant inherited from the input lists, which are
/// guaranteed by `inputs.rs` to be sorted-unique).
///
/// `out` is cleared on entry. Caller pre-allocates capacity to avoid
/// regrowth in the timed portion of the bench; the kernel reuses the
/// buffer across calls.
pub fn intersect_reference(lists: &[&[u32]], out: &mut Vec<u32>) {
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

    // Two-finger merge of the first two lists into `out`.
    pairwise_intersect(lists[0], lists[1], out);

    // Left-fold the rest using a scratch buffer to avoid the in-place hazard.
    let mut scratch: Vec<u32> = Vec::with_capacity(out.len());
    for &list in &lists[2..] {
        if out.is_empty() {
            return;
        }
        scratch.clear();
        pairwise_intersect(out, list, &mut scratch);
        std::mem::swap(out, &mut scratch);
    }
}

/// Two-finger merge of two sorted-unique slices. Output appended to `out`.
fn pairwise_intersect(a: &[u32], b: &[u32], out: &mut Vec<u32>) {
    let mut i = 0usize;
    let mut j = 0usize;
    while i < a.len() && j < b.len() {
        let av = a[i];
        let bv = b[j];
        if av == bv {
            out.push(av);
            i += 1;
            j += 1;
        } else if av < bv {
            i += 1;
        } else {
            j += 1;
        }
    }
}

/// Compare two intersect results. Returns `Some(diff)` describing the
/// first divergence on mismatch, `None` on bitwise equality.
///
/// Used by the correctness phase: any non-None return → `correctness: fail`
/// and `exit 2`.
pub fn intersections_diff(agent: &[u32], reference: &[u32]) -> Option<String> {
    if agent.len() != reference.len() {
        return Some(format!(
            "length mismatch: agent={} reference={}",
            agent.len(),
            reference.len()
        ));
    }
    for (i, (a, r)) in agent.iter().zip(reference).enumerate() {
        if a != r {
            return Some(format!("element {i}: agent={a} reference={r}"));
        }
    }
    None
}
