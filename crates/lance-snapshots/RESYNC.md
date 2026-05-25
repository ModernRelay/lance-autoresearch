# Re-syncing `lance-snapshots` against upstream Lance

This crate vendors Lance's hot-path kernels verbatim (Apache-2.0). The
files are PINNED to a specific upstream SHA (recorded in `src/lib.rs`).
They drift from upstream over time. This doc describes when and how to
re-sync.

## When to re-sync

Re-sync when one of these is true:

1. **Lance ships a major release** (e.g. 6.x → 7.x). Upstream may have
   rewritten the hot path; our baseline becomes a strawman if we don't
   update.
2. **`scripts/check-lance-drift.sh` reports advancement**, AND a quick
   inspection of the upstream files shows semantic changes to functions
   we vendor (not just unrelated code in the same files). Most upstream
   commits don't touch these specific functions; drift reports are
   informational, not action items.
3. **An autoresearch trial fails to find any wins** and you suspect upstream
   has caught up. Re-sync, re-baseline, see if there's still headroom.
4. **Before opening a Lance upstream PR** based on an autoresearch finding.
   Re-sync first so the PR's "this is faster than X" claim is comparable
   to current Lance HEAD, not a months-old snapshot.

Do NOT re-sync just because upstream HEAD moved. The vendored files are
the SOTA at a known good point. As long as no kernel-relevant changes
landed upstream, our baseline stays meaningful.

## How to re-sync

1. **Clone upstream at HEAD** (or the target SHA):
   ```
   git clone --depth 1 https://github.com/lance-format/lance /tmp/lance-resync
   cd /tmp/lance-resync && git rev-parse HEAD
   # → record this SHA; it's the new pin
   ```

2. **Re-vendor each file.** For each vendored function:
   - `crates/lance-snapshots/src/assume.rs` ← `rust/lance-core/src/utils/assume.rs`
   - `crates/lance-snapshots/src/l2.rs` ← `rust/lance-linalg/src/distance/l2.rs` (concretized to f32; preserve `l2_scalar`, `l2_distance_batch`, `L2Prepared`, `accumulate_l2_dimension`)
   - `crates/lance-snapshots/src/pq.rs` ← merge of:
     - `rust/lance-index/src/vector/pq/distance.rs` (`build_distance_table_l2`, `compute_pq_distance`; 8-bit only)
     - `rust/lance-index/src/vector/pq/utils.rs` (`get_sub_vector_centroids`)
     - `rust/lance-index/src/vector/pq/storage.rs` (`transpose`, concretized to `&[u8]`)

   Copy verbatim. Keep our concretization (f32 only, 8-bit only) but
   preserve everything else exactly: function names, control flow,
   comments, the `assume_eq!` calls, `#[inline(never)]` annotations.

3. **Update the SHA pin** in every file header AND in
   `crates/lance-snapshots/src/lib.rs` (the doc comment names the SHA in
   one place; check git grep `5cf70b27b3ad38ecdcd1547b7af385e05f67598a`
   for all references).

4. **Update attribution.** If upstream files now have new SPDX headers or
   copyright lines, propagate them.

5. **Verify the re-sync compiles and tests pass:**
   ```
   cargo test --release --locked -p lance-snapshots
   cargo build --release --locked
   ```

6. **Re-run the baseline** for every landed target:
   ```
   cargo run --release --bin run_experiment -p <target> -- --mode baseline
   ```
   Compare the new baseline geomean against the previous baseline (from
   the target's `results.tsv` or last commit message). If the new baseline
   is meaningfully different, document the drift in `crates/<target>/lessons.md`:
   what changed upstream and what it means for prior trial findings.

7. **Commit the re-sync as a single commit** with the message format:
   ```
   lance-snapshots: re-sync to <new-sha-short> (was <old-sha-short>)

   Upstream changes since last pin:
     - <file>: <one-line description>
     ...

   Baseline impact on landed targets:
     - pq-l2: geomean <old> -> <new> (delta: <±X%>)
     ...
   ```

## What stays manual

- Selecting which upstream functions to vendor for a new target. The
  scaffold script doesn't auto-find them; the target author reads
  upstream and decides.
- Concretization choices (f32-only, 8-bit-only). Upstream supports more
  types; we deliberately drop the trait surface to keep the harness
  self-contained.
- Drift judgment. `check-lance-drift.sh` reports SHA mismatch but doesn't
  know which upstream commits touched our vendored functions.

## License attribution

Every vendored file carries:
```
// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright The Lance Authors
//
// Vendored from lance-format/lance @ <SHA>
// Original path: <upstream/path/to/file.rs>
```

`lance-snapshots/Cargo.toml` is licensed `Apache-2.0`, the same as the
harness root and upstream Lance.
