# Adding a new target

Walk through this when spinning up a new optimization target (cosine,
bitpack, etc.). The workflow is: run the scaffold script for the structural
boilerplate (crate skeleton, Cargo manifest, workspace registration,
gitignore), then rewrite the source files for the new target's math
(`lib.rs`, `kernels.rs`, `reference.rs`, `inputs.rs`, `bin/run_experiment.rs`,
`program.md`, and a capsule under `docs/targets/`).

The scaffold automates ~30 minutes of manual setup. The per-target rewrite
is the actual work and can't be automated.

No architectural decisions are required per target if the kernel fits the
autoresearch shape. If your target's per-trial eval is more than ~30 seconds,
or the correctness oracle can't be a deterministic comparison against a
scalar reference, this harness is the wrong fit. See [`design.md`](design.md)
"When to revisit" for the boundary.

## Steps

### 1. Scaffold the crate

```bash
./scripts/scaffold-target.sh <my-target>
```

This copies `crates/pq-l2/` to `crates/<my-target>/`, renames the package
in `Cargo.toml` + `lib.rs`, registers the crate in the workspace `members`
list, and prints a TODO checklist matching the steps below.

The scaffolded crate will NOT build until you rewrite steps 3–7. That's by
design — the copied source still references `pq_l2::...` types.

### 2. Pick the template that matches your kernel shape

The script copies `pq-l2` by default. The other landed-target archetypes
(once they exist) are:

- Distance / scoring kernels that take a query and return per-row scores →
  `pq-l2` is the right template.
- Decode kernels that take encoded bytes and return an Arrow array →
  template off `bitpack` once it lands (the file layout is the same; the
  oracle and fixtures differ).
- Scan / merge kernels → template off `topk-merge` once it lands.

If `pq-l2` isn't the right template, after running the script, copy the
desired template's `kernels.rs` / `reference.rs` / `inputs.rs` skeletons
into your new crate before editing.

### 3. Rewrite `src/lib.rs`

Define the target's `Shape` type (analogue of `PqShape`) and any other types
shared between `kernels.rs` and `reference.rs` and `inputs.rs`. Document
which fields are pinned by the harness vs. agent-tunable.

This file is **immutable** to the agent. The shape parameters define the
optimization target — changing them changes what's being optimized.

### 4. Rewrite `src/reference.rs`

Implement the scalar reference kernel — the math, in plain Rust, no SIMD,
no cleverness. This is what the agent's kernel is compared against. Mirror
the public API of `kernels.rs` exactly.

For float kernels, also export `max_abs_err(a, b)` (or analogues) — the
comparison helpers the bench uses to assert near-bit-exact equivalence
with `harness_common::MAX_ABS_ERR`. The reference can also be a thin
wrapper over `lance-snapshots` if the upstream function exists (as
`pq-l2`'s does); that's the preferred pattern.

For integer / byte kernels, the comparison is simpler — `assert_eq!` on the
returned Arrow array. No tolerance constants needed.

### 5. Rewrite `src/inputs.rs`

Two surfaces:

- `correctness_battery(seed) -> Vec<CorrectnessCase>` — diverse shape ×
  distribution combinations, sized small enough that the correctness phase
  finishes in seconds. The point is breadth, not realism.
- `speed_workloads(seed) -> Vec<SpeedWorkload>` — larger shape × distribution
  combinations sized for stable timings. Aim for total trial wall-clock
  ≤ 60s; the agent's iteration latency dominates correctness elsewhere.

Use `harness_common::SplitMix64` for determinism. Same seed → same battery
across trials.

### 6. Rewrite `src/kernels.rs` (the agent's playground)

Implement a clean scalar baseline matching the algorithm shape of the Lance
upstream code. The header comment must:

- Cite the upstream Lance source (`lance-format/lance` rev / file path) the
  algorithm is modeled on.
- Document the public API the bench calls — these are the surfaces the agent
  may NOT change.
- List "what you can do" / "what you cannot do" rules specific to this
  target.

The starting kernel must be correct (passes the correctness phase against
`reference.rs`) and lint-clean. The agent's job is to make it faster.

### 7. Rewrite `src/bin/run_experiment.rs`

Two phases:

- **Correctness phase:** for each `CorrectnessCase`, run agent kernel +
  reference, compare. Any mismatch → print `correctness: fail`, diagnostic
  line, exit 2.
- **Speed phase:** for each `SpeedWorkload`, run agent kernel and time per
  query / per row / per byte. Aggregate geomean / worst / best across all
  combos. Print fixed-format result block.

Universal output fields (every target) are listed in `HARNESS.md` "The
metric." Add per-target fields above them as needed (e.g., `bit_widths_tested`
for bitpack).

Use:
- `harness_common::geomean` for the aggregator
- `harness_common::peak_rss_mb` for memory readback
- `harness_common::TIME_BUDGET_SECS` for the time-budget check

### 8. (Optional) Rewrite `benches/<my-target>.rs`

Criterion benchmark with the same kernel calls as `run_experiment` but
under criterion's statistical-sampling harness. Optional — the per-trial
binary is the agent's primary measurement; criterion is for the human's
deeper investigation.

### 9. Write `program.md`

Per-target agent skill, layered on top of `HARNESS.md`. Sections:

- **Setup** — which files to read at session start. Always include
  `../../HARNESS.md` and `lessons.md` (if present).
- **Public API contract** — the exact functions / structs the agent must
  keep stable.
- **Target-specific priors split into sub-sections** —
  `[arch=any]` for algorithmic ideas, `[arch=aarch64]` for NEON specifics,
  `[arch=x86_64]` for AVX2 specifics. The `run_experiment` header prints
  the detected `arch:` so the agent knows which sub-section applies.
  This is the highest-leverage content; spend time on it.
- **`results.tsv` header** — the per-target column set. Include `ci_lo`,
  `ci_hi` columns since the keep-gate uses CI overlap.

### 10. Write the per-target capsule in `docs/targets/<my-target>.md`

A short doc covering:

- What's optimized (one sentence)
- Upstream Lance source pointers (rev, file paths, function names)
- Oracle definition (bit-exact / `max_abs_err`)
- Speed workload shape (what shapes × distributions span)
- Status (candidate / landed / has-results)

### 11. Verify end-to-end

```bash
cargo build --release --locked -p <my-target>
cargo clippy --release -p <my-target> --all-targets -- -D warnings
cargo run --release --bin run_experiment -p <my-target> -- --mode baseline
```

The baseline trial (3-pass) must:
- Print `correctness: pass`
- Print `arch:` and `geomean_ns_ci_90pct: [lo, hi]`
- Exit 0
- Finish within ~180s (3 passes × ~60s budget per pass)
- Reference a sensible `geomean_ns_per_*` baseline number

Smoke-test the gate: deliberately break `kernels.rs` (e.g., return constant
zero), confirm the trial exits 2 with `correctness: fail`. Restore.

### 12. Add the target row to the top-level `README.md`

In the targets table at the top of the README, change the new target's row
from `candidate` to `landed`.

### 13. Commit

One commit for the target's scaffolding (the script's output) and a separate
commit for the per-target rewrite. Don't bundle multiple targets in
one commit — each target's history should be independently revertible.
`lessons.md` and `results.tsv` are gitignored; only source + docs commit.

## Common gotchas

- **Forgetting the empty `[workspace]` block** at the root means cargo walks
  up to the omnigraph parent workspace. Already handled; just don't remove it.
- **Per-target `Cargo.toml` referencing the wrong `harness-common` path.**
  Use `harness-common = { path = "../harness-common" }`.
- **Picking a `SHAPES` set that's too small.** Three shapes is the floor;
  with one shape an agent could specialize and pass, with two there's not
  enough variety. Ensure the shapes span at least one "outlier" (e.g., for
  PQ, one shape with `sub_vector_dim != 8`).
- **Correctness battery too narrow.** Five distributions is the floor: at
  minimum Gaussian / uniform / sparse / large-dynamic-range / mostly-zero (or
  the integer analogue: uniform / clustered / skewed / few-distinct /
  monotonic).
- **Trial time too long.** If the speed phase exceeds ~60s, agent iteration
  rate drops below useful. Reduce workload sizes; the speed metric is
  per-operation, not per-workload, so absolute size doesn't change the
  comparison.
