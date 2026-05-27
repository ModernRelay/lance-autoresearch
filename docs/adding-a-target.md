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

### 0. Trace the upstream hot path (BEFORE scaffolding)

Find the primitive operation that dominates the target's caller code path
in upstream Lance. This is the single highest-leverage step in the workflow;
skipping it produces microbenchmarks that don't transfer to production. The
`posting-intersect` target shipped without Step 0 and ended up measuring a
kernel surface that Lance's WAND traversal does not call.

Concretely:

1. **Identify the upstream function(s) the target nominally optimizes.**
   For `pq-l2` this was `compute_pq_distance` + `build_distance_table_l2`.
   For a hypothetical decoder target it's the `decompress_*` function.
2. **Grep upstream Lance for the file(s) that *call* those functions.**
   Read the caller's tightest loop. The `gh` CLI works against
   `lance-format/lance` at the pinned SHA from `crates/lance-snapshots/src/lib.rs`:
   ```
   gh api 'search/code?q=<function_name>+repo:lance-format/lance' \
     --jq '.items[] | "\(.path)"'
   gh api 'repos/lance-format/lance/contents/<path>?ref=<sha>' \
     --jq '.content' | base64 -d
   ```
3. **Confirm the proposed kernel API mirrors what the caller calls.** If
   your proposed signature (e.g. `fn intersect(lists: &[&[u32]], ...)`)
   doesn't match any call site you found, **stop and redesign before
   scaffolding.** The absence of a clean upstream function for your
   surface usually means the operation is fused with something else
   (scoring, decompression, iteration); inventing a clean surface measures
   a primitive Lance doesn't use.
4. **Record the call-site excerpt in the per-target capsule's "Lance call
   site" section** (see Step 10 below for the format). If no direct call
   site exists, the capsule must say so honestly; that admission triggers
   a scoping conversation before code lands.

Time budget: ≤30 minutes. Cheapest insurance the harness offers. See
[`../HARNESS.md`](../HARNESS.md) § "Background research" item 1 for why
this fires before scaffolding.

### 0.5. Estimate cost-fraction (also BEFORE scaffolding)

Step 0 found the kernel's call site. Step 0.5 estimates **what fraction
of an end-to-end Lance query that call site actually consumes at
upstream's bench scale**. This is the most-skipped step in the workflow
and the single biggest source of "the microbench wins but production
doesn't move" failures. `posting-seek` shipped a kept hybrid with -97%
on its microbench and 0% (within noise) on upstream's actual FTS
bench — because the kernel was only ~2% of total query cost at
upstream's 1M-doc bench scale, so even a 100× kernel speedup is
bounded by 2% production impact.

Procedure:

1. **Find the upstream bench that exercises this kernel.** Look in
   `rust/lance/benches/` and `rust/lance-<crate>/benches/` for a bench
   whose hot path goes through your kernel's call site. Read the
   bench's setup to understand its scale (corpus size, query mix,
   etc.).
2. **Estimate per-call kernel cost.** From the upstream code shape
   plus any inline comments or profiling notes, rough out the per-call
   cost in nanoseconds. For an unoptimized linear loop, that's roughly
   `loop_iterations × 3-5 ns/iter` on M1.
3. **Estimate calls per query.** From the upstream caller's loop
   structure: how many times does it invoke your kernel per typical
   query?
4. **Total query cost from upstream bench numbers.** Run the upstream
   bench once at its default scale, or read past benchmark results.
5. **Compute the fraction:** `(per-call cost × calls/query) / total
   query cost`. If <5%, the headline production win is bounded by 5%
   regardless of how good your kernel optimization is.

If the fraction is <5%:

- **Defer the target.** The work is methodology, not production
  impact.
- **Refocus.** The kernel may be too narrow. Consider whether the
  target's surface should expand to include the more dominant kernel
  it sits inside.
- **Argue scale.** If the kernel cost grows superlinearly with corpus
  size and the bench-vs-production scale gap is large (e.g., bench at
  1M docs, production at 1B), the cost-fraction at production scale
  may be much higher than at bench scale. Document this with cost-
  model arithmetic in the capsule's "Cost fraction" section. The PR
  becomes "low-risk infra change for billion-scale users," not "X%
  faster Lance."

Document the result in the target's capsule under a new "Cost
fraction" section with explicit numbers. Future agents reading the
capsule see the ceiling on the headline number before scaffolding any
trials.

### 1. Scaffold the crate

```bash
./scripts/scaffold-target.sh <my-target>
```

This copies `crates/pq-l2/` to `crates/<my-target>/`, renames the package
in `Cargo.toml` + `lib.rs`, registers the crate in the workspace `members`
list, and prints a TODO checklist matching the steps below.

The scaffolded crate will NOT build until you rewrite steps 3–7. That's by
design, the copied source still references `pq_l2::...` types.

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
optimization target, changing them changes what's being optimized.

### 4. Rewrite `src/reference.rs`

Implement the scalar reference kernel: the math, in plain Rust, no SIMD,
no cleverness. This is what the agent's kernel is compared against. Mirror
the public API of `kernels.rs` exactly.

For float kernels, also export `max_abs_err(a, b)` (or analogues): the
comparison helpers the bench uses to assert near-bit-exact equivalence
with `harness_common::MAX_ABS_ERR`. The reference can also be a thin
wrapper over `lance-snapshots` if the upstream function exists (as
`pq-l2`'s does); that's the preferred pattern.

For integer / byte kernels, the comparison is simpler: `assert_eq!` on the
returned Arrow array. No tolerance constants needed.

### 5. Rewrite `src/inputs.rs`

Two surfaces:

- `correctness_battery(seed) -> Vec<CorrectnessCase>`, diverse shape ×
  distribution combinations, sized small enough that the correctness phase
  finishes in seconds. The point is breadth, not realism.
- `speed_workloads(seed) -> Vec<SpeedWorkload>`, larger shape × distribution
  combinations sized for stable timings. Aim for total trial wall-clock
  ≤ 60s; the agent's iteration latency dominates correctness elsewhere.

Use `harness_common::SplitMix64` for determinism. Same seed → same battery
across trials.

### 6. Rewrite `src/kernels.rs` (the agent's playground)

Implement a clean scalar baseline matching the algorithm shape of the Lance
upstream code. The header comment must:

- Cite the upstream Lance source (`lance-format/lance` rev / file path) the
  algorithm is modeled on.
- Document the public API the bench calls, these are the surfaces the agent
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
under criterion's statistical-sampling harness. Optional, the per-trial
binary is the agent's primary measurement; criterion is for the human's
deeper investigation.

### 9. Write `program.md`

Per-target agent skill, layered on top of `HARNESS.md`. Sections:

- **Setup**, which files to read at session start. Always include
  `../../HARNESS.md` and `lessons.md` (if present).
- **Public API contract**: the exact functions / structs the agent must
  keep stable.
- **Target-specific priors split into sub-sections**:
  `[arch=any]` for algorithmic ideas, `[arch=aarch64]` for NEON specifics,
  `[arch=x86_64]` for AVX2 specifics. The `run_experiment` header prints
  the detected `arch:` so the agent knows which sub-section applies.
  The priors section is where most of the agent's productivity comes from;
  spend time on it.
- **`results.tsv` header**: the per-target column set. Include `ci_lo`,
  `ci_hi` columns since the keep-gate uses CI overlap.

### 10. Write the per-target capsule in `docs/targets/<my-target>.md`

A short doc covering:

- Status (candidate / landed / has-results)
- What's optimized (one sentence)
- **Lance call site** (REQUIRED, the output of Step 0)
- Upstream Lance source pointers (rev, file paths, function names)
- Oracle definition (bit-exact / `max_abs_err`)
- Speed workload shape (what shapes × distributions span)

The "Lance call site" section quotes the upstream caller code that drives
this kernel, anchoring the per-target work to a real production hot path.
Format:

```
## Lance call site

Upstream `lance-format/lance` at SHA `<sha>`,
`rust/lance-<crate>/src/<path>.rs`:

    // lines N..M
    <quoted excerpt of the tightest caller loop>

This kernel's `<method>` corresponds to <line N> in <caller>.
```

If no direct call site exists for the kernel surface as proposed, the
section MUST say so honestly:

> No direct call site in current upstream; this kernel is
> [a refactor target / a primitive for external systems / etc.].

That admission is load-bearing. It signals that the target is measuring
something Lance doesn't currently call, which should trigger a scoping
conversation (and probably a sibling `<target>-<correct-shape>` capsule)
before further code lands. See `docs/targets/posting-intersect.md` for
an example of this honest scope-note pattern.

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
one commit, each target's history should be independently revertible.
`lessons.md` and `results.tsv` are gitignored; only source + docs commit.

## Common gotchas

- **Proposed kernel surface doesn't match an upstream call site.** The
  single highest-impact failure mode. Catches itself only if you actually
  do Step 0. `posting-intersect` shipped without Step 0 and ended up
  measuring `intersect(&[&[u32]], ...)`, a surface Lance's WAND traversal
  does not call. The trials are clean kernel engineering but the −81%
  geomean is on a primitive that doesn't appear in production. Step 0
  exists to prevent this.
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
