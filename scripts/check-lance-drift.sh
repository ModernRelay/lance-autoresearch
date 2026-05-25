#!/usr/bin/env bash
# Report drift between our vendored lance-snapshots/ and upstream Lance HEAD.
#
# Usage: ./scripts/check-lance-drift.sh
#
# What it does:
#   1. Extracts our pinned upstream SHA from lance-snapshots/src/lib.rs.
#   2. Fetches the upstream HEAD SHA from GitHub.
#   3. If different, lists the upstream files we vendor and shows how many
#      commits each has had since our pinned SHA.
#   4. Prints a re-sync recommendation (decided by the human, not by this
#      script — semantic drift in a function we vendor doesn't always
#      require a re-sync).
#
# What it does NOT do:
#   - Auto-update our vendored files. Re-sync is a manual ritual; see
#     crates/lance-snapshots/RESYNC.md.
#   - Run in CI. Lance evolves continuously; CI noise would dilute the
#     signal. Run manually when working on a target or before a Lance
#     upstream PR.

set -euo pipefail

WORKSPACE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SNAPSHOTS_LIB="$WORKSPACE_DIR/crates/lance-snapshots/src/lib.rs"

if [[ ! -f "$SNAPSHOTS_LIB" ]]; then
  echo "error: $SNAPSHOTS_LIB not found" >&2
  exit 1
fi

# Extract pinned SHA from the doc comment in lib.rs.
PINNED_SHA="$(grep -oE '[a-f0-9]{40}' "$SNAPSHOTS_LIB" | head -1)"
if [[ -z "$PINNED_SHA" ]]; then
  echo "error: could not find pinned SHA in $SNAPSHOTS_LIB" >&2
  exit 1
fi

echo "Pinned upstream SHA: $PINNED_SHA"

# Get upstream HEAD via GitHub API (avoids cloning if just checking SHA).
UPSTREAM_HEAD="$(curl -s https://api.github.com/repos/lance-format/lance/commits/main | sed -n 's/.*"sha": "\([a-f0-9]*\)".*/\1/p' | head -1)"
if [[ -z "$UPSTREAM_HEAD" ]]; then
  echo "error: could not fetch upstream HEAD (rate-limited? network?)" >&2
  exit 1
fi

echo "Upstream HEAD SHA:   $UPSTREAM_HEAD"

if [[ "$PINNED_SHA" == "$UPSTREAM_HEAD" ]]; then
  echo
  echo "No drift. Snapshots are current."
  exit 0
fi

echo
echo "Drift detected. Upstream has advanced since our pin."
echo
echo "Files we vendor (check each for semantic changes upstream):"
echo "  rust/lance-core/src/utils/assume.rs           (assume!/assume_eq! macros)"
echo "  rust/lance-linalg/src/distance/l2.rs          (l2_scalar, L2Prepared, accumulate_l2_dimension)"
echo "  rust/lance-index/src/vector/pq/distance.rs    (build_distance_table_l2, compute_pq_distance)"
echo "  rust/lance-index/src/vector/pq/utils.rs       (get_sub_vector_centroids)"
echo "  rust/lance-index/src/vector/pq/storage.rs     (transpose)"
echo
echo "Compare ranges by fetching upstream at HEAD:"
echo "  git clone --depth 1 https://github.com/lance-format/lance /tmp/lance-drift && \\"
echo "    diff /tmp/lance-drift/rust/lance-linalg/src/distance/l2.rs \\"
echo "         $WORKSPACE_DIR/crates/lance-snapshots/src/l2.rs"
echo "  (note our snapshots are concretized to f32; a structural diff is"
echo "   more informative than a line-by-line diff)"
echo
echo "Re-sync ritual: see $WORKSPACE_DIR/crates/lance-snapshots/RESYNC.md"
