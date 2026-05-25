#!/usr/bin/env bash
# Scaffold a new autoresearch target by copying crates/pq-l2 and renaming.
#
# Usage: ./scripts/scaffold-target.sh <target-name>
#
# What it does:
#   1. cp -r crates/pq-l2 crates/<target>
#   2. Rename package + bin in <target>/Cargo.toml
#   3. Update lib.rs doc comment to reference <target>
#   4. Reset <target>/lessons.md to an empty placeholder
#   5. Append <target> to the workspace `members` in the root Cargo.toml
#   6. Print a TODO list of what the human needs to rewrite next
#
# What it does NOT do:
#   - Rewrite kernels.rs / reference.rs / inputs.rs / run_experiment.rs for
#     the new target's math. That's the per-target work the human does next.
#   - Add a docs/targets/<name>.md capsule. The human writes that once the
#     target's API and oracle are decided.

set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <target-name>" >&2
  echo "  e.g.: $0 pq-cosine" >&2
  exit 1
fi

NAME="$1"

# Validate name: kebab-case, lowercase, no special chars.
if [[ ! "$NAME" =~ ^[a-z][a-z0-9-]*$ ]]; then
  echo "error: target name must be lowercase kebab-case, got '$NAME'" >&2
  exit 1
fi

# Resolve workspace dir from script location.
WORKSPACE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$WORKSPACE_DIR/crates/pq-l2"
DST="$WORKSPACE_DIR/crates/$NAME"

if [[ ! -d "$SRC" ]]; then
  echo "error: template $SRC not found" >&2
  exit 1
fi
if [[ -d "$DST" ]]; then
  echo "error: $DST already exists; remove it first or pick a different name" >&2
  exit 1
fi

# 1. Copy the template.
cp -R "$SRC" "$DST"

# Remove any local-only files (results.tsv, lessons.md, run logs).
rm -f "$DST/results.tsv" "$DST/run.log" "$DST/lessons.md"
rm -f "$DST/run.t"*.log "$DST/run."*.log 2>/dev/null || true

# 2. Rename package name. macOS and Linux sed both accept `-i.bak` form.
sed -i.bak "s/^name = \"pq-l2\"$/name = \"$NAME\"/" "$DST/Cargo.toml"
sed -i.bak "s|Autoresearch target: Lance PQ L2 distance kernel optimization\.|Autoresearch target: <FILL IN — describe the Lance kernel under autoresearch>.|" "$DST/Cargo.toml"
rm -f "$DST/Cargo.toml.bak"

# 3. Update lib.rs doc comment to reference the new target.
sed -i.bak "s|//! Autoresearch target: Lance PQ L2 distance kernel optimization\.|//! Autoresearch target: <FILL IN — describe the Lance kernel under autoresearch>.|" "$DST/src/lib.rs"
rm -f "$DST/src/lib.rs.bak"

# 4. Reset lessons.md (gitignored — won't be committed; just establishes the
#    convention so the agent's session-start read doesn't 404).
cat > "$DST/lessons.md" <<EOF
# $NAME lessons

(empty — append after the first trial that produces a load-bearing finding.
See HARNESS.md step 10 for what's worth recording.)
EOF

# 5. Add to workspace members. The root Cargo.toml has:
#      members = [
#          "crates/harness-common",
#          "crates/pq-l2",
#      ]
#    Append after the last existing member.
WORKSPACE_TOML="$WORKSPACE_DIR/Cargo.toml"
if ! grep -q "\"crates/$NAME\"" "$WORKSPACE_TOML"; then
  # Find the last `"crates/...",` line and insert after it.
  awk -v name="$NAME" '
    /^    "crates\// { last_crate_line = NR }
    { lines[NR] = $0 }
    END {
      for (i = 1; i <= NR; i++) {
        print lines[i]
        if (i == last_crate_line) {
          print "    \"crates/" name "\","
        }
      }
    }
  ' "$WORKSPACE_TOML" > "$WORKSPACE_TOML.new"
  mv "$WORKSPACE_TOML.new" "$WORKSPACE_TOML"
fi

# 6. Print the TODO list.
cat <<EOF

Scaffolded $DST

TODO before first trial (per docs/adding-a-target.md):
  1. Rewrite $DST/src/lib.rs        — define the target's Shape type
  2. Rewrite $DST/src/reference.rs  — scalar reference (the oracle)
  3. Rewrite $DST/src/inputs.rs     — diverse correctness + speed workloads
                                       (at least 5 distributions, 3 shapes)
  4. Rewrite $DST/src/kernels.rs    — agent's playground; start with a clean
                                       scalar baseline matching reference.rs
  5. Rewrite $DST/src/bin/run_experiment.rs — update fixture types + output fields
  6. Rewrite $DST/program.md        — target priors split by [arch=any]/aarch64/x86_64
  7. Add docs/targets/$NAME.md capsule (one page: what's optimized, upstream
     pointer, oracle, speed-workload shape, status)
  8. Verify: cargo build --release -p $NAME && cargo run --release --bin run_experiment -p $NAME -- --mode baseline
  9. Mark this target 'landed' in README.md candidate table

Then open a fresh \`claude\` session in this workspace and paste the
Karpathy-shaped launch prompt from docs/adding-a-target.md (with this
target's arch pin substituted in).
EOF
