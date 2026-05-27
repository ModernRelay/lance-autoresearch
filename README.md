# lance-autoresearch

A multi-target workspace for evolving [Lance](https://github.com/lance-format/lance)
hot-path kernels via LLM coding agents (Claude Code, Codex, Cursor),
in the style of Andrej Karpathy's
[`autoresearch`](https://github.com/karpathy/autoresearch) single-agent loop.

## What autoresearch is, and why it works

Karpathy's [`autoresearch`](https://github.com/karpathy/autoresearch)
(early 2026): give an LLM agent one mutable file, a fixed bench, and a
`program.md` of priors. The agent loops (edit, build, run,
keep-or-revert, commit) overnight, until you stop it. Karpathy's framing:
*"You wake up to a log of experiments and (hopefully) a better model."*

This repo adapts that shape for **Lance kernel optimization**: per-trial
~30s, one mutable file (`crates/<target>/src/kernels.rs`), correctness
oracle is **upstream Lance code itself** (vendored verbatim in
`crates/lance-snapshots/`). Any kept commit is bit-equivalent to what
Lance ships; wins port upstream as Apache-2.0 PRs.

Why the shape works: fixed-cost trials bound the per-iteration budget
(~100/hour); one mutable file keeps diffs reviewable and prevents scope
creep; a deterministic oracle kills failed trials without spiraling;
the loop self-orchestrates so the human can leave; findings compound
across sessions via gitignored `lessons.md` per target.

Each landed target is an independent Rust crate under `crates/`. The
candidates below are listed as a roadmap. They have no code yet, only a
`docs/targets/<name>.md` capsule when one exists. Spinning up a candidate
follows the [`docs/adding-a-target.md`](docs/adding-a-target.md) workflow.

| Target | Status | Lance source area | What's optimized | Best result |
|---|---|---|---|---|
| [`crates/pq-l2`](crates/pq-l2) | landed | `lance-linalg::distance::l2`, PQ probe | PQ L2 distance: distance_table + per-vector distances | **−43% geomean vs upstream** (M1 Max, aarch64; bit-equivalent output; x86 untested) |
| `crates/pq-cosine`     | candidate | `lance-linalg::distance::cosine` | PQ cosine distance | pending |
| `crates/pq-dot`        | candidate | `lance-linalg::distance::dot` | PQ dot-product distance | pending |
| `crates/ivf-partition` | candidate | `lance-index::vector::ivf` partition select | IVF partition selection (centroid scan) | pending |
| `crates/fts-bm25`      | candidate | `lance-index::scalar::inverted` BM25 | FTS BM25 scoring inner loop | pending |
| `crates/bitpack`       | candidate | `lance-encoding::encodings::bitpack` | Bitpack integer decode | pending |
| `crates/dictionary`    | candidate | `lance-encoding::encodings::dictionary` | Dictionary decode | pending |
| `crates/fsst`          | candidate | `lance-encoding::encodings::fsst` | FSST string decode | pending |
| `crates/take`          | candidate | `lance-core::utils::take` | Take / gather kernel | pending |
| `crates/predicate`     | candidate | `lance-datafusion` filter eval | Predicate evaluation kernels | pending |
| [`crates/posting-intersect`](crates/posting-intersect) | landed (off-path; see capsule) | `lance-index::scalar::inverted` (no direct call site) | Sorted u32 posting-list AND intersect | **−81% geomean vs scalar K-way merge** (M1 Max, aarch64; bit-equivalent output; x86 fallback intact). Kernel surface not in current Lance hot path; see [`posting-seek`](docs/targets/posting-seek.md) for the Lance-aligned shape. |
| [`crates/posting-seek`](crates/posting-seek) | landed | `lance-index::scalar::inverted::wand` (`next`, `shallow_next`) | Block-aware seek over compressed posting list | **−97% on worst-case Skip-deep × Large** (3011 → 74 ns), −58% geomean. Hybrid linear-budget + McIlroy gallop. M1 Max, aarch64; portable scalar code, no SIMD |
| `crates/topk-merge`    | candidate | scan-merge | Top-K k-way merge | pending |

The candidate targets are documented in [`docs/targets/`](docs/targets/) and
can be added by following [`docs/adding-a-target.md`](docs/adding-a-target.md).
`pq-l2`, `posting-intersect`, and `posting-seek` are landed; the rest
wait for an agent to spin them up. `pq-l2` carries a −43% geomean win
on M1 Max. `posting-intersect` lands at −81% geomean via three trials
(branchless merge → galloping at ratio>16× → NEON cross-product SIMD
merge), but a retroactive Step 0 trace (see `docs/adding-a-target.md`)
showed its kernel surface is not in Lance's current WAND hot path —
the trial wins are clean kernel engineering on a primitive Lance would
need a refactor to use. `posting-seek` is the Lance-aligned follow-up:
a hybrid linear-budget + McIlroy gallop change in `wand.rs::next` that
drops the worst-case seek (Large × Skip-deep) from 3011 ns → 74 ns,
~30 LOC, no `unsafe`, no SIMD. Step 0 of the workflow was added in
response to `posting-intersect`'s mis-scope; future targets won't ship
without their "Lance call site" capsule section filed first.

## The contract every target follows

Karpathy's three-file shape, applied per target:

| File (per target crate) | Mutability | Edited by |
|---|---|---|
| `src/kernels.rs` | **mutable** | the agent |
| `src/reference.rs`, `src/inputs.rs`, `src/lib.rs`, `src/bin/run_experiment.rs`, `benches/*.rs` | immutable | nobody |
| `program.md` | human-iterated | the human, between runs |
| `results.tsv` | append-only | the agent, per trial (gitignored) |
| `lessons.md` | append-only | the agent, on load-bearing findings (gitignored) |

The shared utilities (deterministic PRNG, geomean, bootstrap CI, PMC
counters, peak-RSS readback, tolerance constants, time-budget) live in
[`crates/harness-common`](crates/harness-common/src/lib.rs) and are
consumed by every target. There is intentionally **no `Target` trait**:
decode-kernel signatures and distance-kernel signatures are different
enough that a unifying trait would either bloat or require erased
boxing. Each target is its own natural shape; the shared crate is
plumbing only.

The shared loop conventions every target's `program.md` inherits live in
[`HARNESS.md`](HARNESS.md). Per-target priors and API specifics live in
each target's own `program.md`.

## Dataset-independent by design

Every other ANN benchmark you've seen is "compete on this fixed dataset"
(SIFT1M, GIST1M, DEEP1B). That conflates two things: *kernel correctness*
(the math) and *kernel speed under one specific data distribution*. An
LLM agent given recall@K as the oracle has incentive to overfit to the
dataset's quirks.

We split them, every target:

- **Correctness** = bit-equivalent (`max_abs_err ≤ 1e-4` for floats;
  bitwise for integer/byte kernels) match to a scalar reference, on
  diverse generated inputs. Mathematical equivalence; no dataset to
  overfit. Lossy techniques fail this gate.
- **Speed** = geomean ns/operation across multiple shape × distribution
  combinations, with worst-case guard. A kernel that wins on one
  distribution and regresses on another fails to keep.

Fixtures generate from a seeded PRNG in each target's `inputs.rs`.
Nothing to download. Reproducible across machines and across runs from
the same SHA.

## Quick start

```bash
# Run the landed PQ L2 target's baseline (3-pass for tight CI).
cargo run --release --bin run_experiment -p pq-l2 -- --mode baseline

# Or per-trial mode (1-pass, faster iteration):
cargo run --release --bin run_experiment -p pq-l2

# With Claude Code / Codex, working on one target:
cd crates/pq-l2
# Open in your agent of choice and prompt:
#   Hi, have a look at program.md and let's kick off a new experiment.

# Add a new target (see docs/adding-a-target.md):
./scripts/scaffold-target.sh pq-cosine
# Then rewrite kernels.rs / reference.rs / inputs.rs / program.md for the
# new kernel's math.

# Check whether our vendored upstream code has drifted:
./scripts/check-lance-drift.sh
```

## Repo layout

```
lance-autoresearch/
├── Cargo.toml                         # workspace root
├── README.md                          # you are here
├── HARNESS.md                         # shared loop contract every target inherits
├── LICENSE                            # Apache-2.0 (matches upstream Lance)
├── scripts/
│   ├── scaffold-target.sh             # cp -r pq-l2 + rename for a new target
│   └── check-lance-drift.sh           # report upstream-snapshot drift
├── crates/
│   ├── harness-common/                # SplitMix64, geomean, bootstrap CI, PMC counters, tolerance, time budget
│   │   └── src/{lib,prng,stats,sysinfo,tolerance,perf}.rs
│   ├── lance-snapshots/               # verbatim Apache-2.0 vendored Lance hot-path kernels (pinned SHA)
│   │   ├── RESYNC.md
│   │   └── src/{lib,assume,l2,pq}.rs
│   └── pq-l2/                         # landed target
│       ├── Cargo.toml
│       ├── program.md                 # this target's agent skill
│       ├── src/
│       │   ├── lib.rs                 # PqShape + module wiring (immutable)
│       │   ├── kernels.rs             # MUTABLE; agent's playground (starts as upstream clone)
│       │   ├── reference.rs           # IMMUTABLE; thin wrapper over lance-snapshots (oracle IS upstream code)
│       │   ├── inputs.rs              # IMMUTABLE; diverse test-data generators
│       │   └── bin/run_experiment.rs  # IMMUTABLE; per-trial entry point
│       └── benches/pq_l2.rs           # criterion benchmark (immutable)
└── docs/
    ├── design.md                      # rationale for the workspace shape
    ├── robustness.md                  # why each measurement feature exists
    ├── adding-a-target.md             # workflow for spinning up a new target
    └── targets/
        └── pq-l2.md                   # capsule: upstream Lance pointers, oracle, status
```

## Upstream contribution path

When a commit on any target clears the keep bar by a meaningful margin
(≥10% geomean speedup with worst-case guard intact), the human reviews
the diff, ports the technique against
[`lance-format/lance`](https://github.com/lance-format/lance) HEAD, runs
Lance's own test suite, and opens a PR. The harness is Apache-2.0
licensed to match Lance; the upstream PR inherits Apache-2.0 cleanly.
The correctness gate (`MAX_ABS_ERR ≤ 1e-4` against the vendored upstream
code in `crates/lance-snapshots`) means any kept commit is bit-equivalent
to what Lance ships today. Recall is preserved by construction, not just
empirically.

## License

Licensed under the Apache License, Version 2.0 ([LICENSE](LICENSE)).

Vendored upstream code in `crates/lance-snapshots/` carries the same
license and is attributed to The Lance Authors in each file's SPDX
header. See `crates/lance-snapshots/RESYNC.md` for the re-sync ritual.
