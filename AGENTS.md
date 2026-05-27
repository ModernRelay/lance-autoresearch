# AGENTS.md

Always-on map for AI coding agents working in this repo. Map plus the
rules and principles that need to be in scope at all times.

`CLAUDE.md` is a symlink to this file. There is one source of truth.

## What this repo is

Single-agent autoresearch harness (Karpathy
[`autoresearch`](https://github.com/karpathy/autoresearch) shape) for
Lance hot-path kernels: edit one file (`crates/<target>/src/kernels.rs`),
build, run a fixed bench, decide keep-or-revert, commit. Loop until
interrupted. Works because per-trial cost is bounded (~30s), the oracle
is deterministic, and the loop self-orchestrates.

The correctness oracle is **upstream Lance code itself**, vendored
verbatim at a pinned SHA in `crates/lance-snapshots/`. Any kept commit is
bit-equivalent to what Lance ships today. Wins port upstream as
Apache-2.0 PRs by construction.

## Read what matches your intent

| Intent | Files, in order |
|---|---|
| Optimize an existing target | `HARNESS.md` → `crates/<target>/program.md` → `crates/<target>/lessons.md` (if present; gitignored per-machine) |
| Add a new target | `docs/adding-a-target.md` → `./scripts/scaffold-target.sh <name>` |
| Maintain the harness itself | `docs/design.md` + `docs/robustness.md` |
| Re-sync vendored Lance snapshots | `crates/lance-snapshots/RESYNC.md` |
| See the file layout | `README.md` § Repo layout |

Don't read everything. Pick the row.

## Principles (apply when the bright-line rules are silent)

The meta-principle: **every kept commit should be a Lance upstream PR
you'd defend in review.** Wins that wouldn't survive that bar are
noise, not signal. The rules below flow from this.

1. **Correctness > simplicity > performance, lexicographic.** Never
   trade correctness for simpler code. Never trade simplicity for
   marginal performance. A 1% speedup that requires 200 lines of
   unsafe is worse than the original.

2. **Wins must transfer across shapes AND distributions.** A change
   that improves one combo by 30% but regresses another by 5% fails
   the keep gate even if global geomean improves. Lance users run many
   shapes; an aarch64-only or high-dim-only win must be gated by
   `#[cfg(target_arch = ...)]` or shape-specific dispatch, never
   silently merged into a portable path.

3. **Mechanism > vibes.** When a trial wins, explain WHY in the commit
   message. `"4x unroll: -19% from FP-ADD latency bound on M1"` is a
   real explanation. `"cleanup: -19%"` is not. Upstream reviewers ask
   for mechanism; if you can't articulate it, you don't understand the
   win, and it's likely to regress in a future codegen change.

4. **Mirror upstream's surface, don't invent.** If upstream Lance
   doesn't expose your proposed kernel as a callable function, that's
   a load-bearing signal, not an inconvenience. It usually means the
   operation is fused into a larger loop (scoring, iteration,
   decompression). Inventing a clean surface around it produces a
   microbenchmark that doesn't match production cost paths. Trace
   the actual hot caller in upstream BEFORE scaffolding; design the
   kernel API to mirror what gets called. `docs/adding-a-target.md`
   Step 0 enforces this; every target's capsule has a "Lance call
   site" section that quotes the caller code with SHA + line numbers.

5. **Substrate first.** Don't reinvent what upstream Lance, LLVM
   autovec, or hardware prefetchers already do. Read `lance-snapshots/`
   for upstream's current pattern before proposing the same idea via
   different syntax. The same thing in different code carries no value.

6. **One hypothesis per trial.** Don't combine "transpose codebook" +
   "add NEON FMA" in one diff. You won't know which contributed. Land
   them as two trials; the composition becomes its own third trial.

7. **No new dependencies.** Adding `criterion-extras` or `simdeez`
   etc. moves the optimization into the dependency. The harness
   measures kernels in isolation; importing a SIMD library defeats
   the purpose.

8. **The bit-exact gate IS the contract.** Failing it means your
   change produces different output than upstream's, which is silent
   recall regression if shipped. Don't override "just this once." If
   you want a lossy track, surface it to the human as a separate
   kernel surface.

One prior finding on M1 Max: per-vector AoS codes + 4x register-
accumulator unroll beats upstream's loop-swap + SoA-distances pattern
by 43% geomean, bit-equivalent output, CIs strictly non-overlapping.
That's the shape of a real win. Future trials look for further wins
via explicit NEON SIMD, codebook transposes, or algorithmic changes
that preserve the bit-exact gate.

## Bright-line rules (verifiable, load-bearing)

1. **Edit ONLY `crates/<target>/src/kernels.rs`.** Per-target,
   `lib.rs`/`reference.rs`/`inputs.rs`/`bin/run_experiment.rs`/`benches/*`
   are immutable; the harness contract depends on them.
2. **Do not edit `crates/lance-snapshots/`.** Vendored upstream code.
   Re-syncing is the `RESYNC.md` ritual, human-driven.
3. **Do not edit `crates/harness-common/`** unless the user explicitly
   asks. It's the workspace measurement layer.
4. **Bit-exact correctness gate is load-bearing.** `MAX_ABS_ERR = 1e-4`
   against the upstream-via-`lance-snapshots` reference. Lossy tricks
   (u8 LUT quantization, `(q-c)² → q²+c²-2q·c` rewrites with
   catastrophic-cancellation risk) fail the gate; don't try them.
5. **The keep-gate uses CI overlap rather than point-estimate geomean.**
   A 1-2% geomean "improvement" with overlapping 90% CI is noise. See
   `HARNESS.md` § keep-gate.
6. **Never commit `results.tsv`, `run.log`, or `lessons.md`.** All
   gitignored, per-machine state. The git log of `kernels.rs` is the
   durable record.
7. **Use `cargo build --locked`.** `Cargo.lock` is committed; the
   toolchain is pinned in `rust-toolchain.toml`. Floating either
   invalidates timings.
8. **Commit each trial BEFORE running it.** Cycle: edit → commit →
   run → keep (advance) OR `git reset --hard HEAD~1` (revert). Every
   trial gets a SHA, including rejected ones (`git reflog`).

## First-session verify

```bash
cargo build --release --locked
cargo test --release --locked
cargo clippy --release --all-targets -- -D warnings
./scripts/check-lance-drift.sh
```

All four should pass. If `check-lance-drift.sh` reports drift, that's
informational; the human decides whether re-syncing is warranted (see
`RESYNC.md`).

## Baseline + trial commands

```bash
# Baseline (3-pass, tight 90% CI):
cargo run --release --bin run_experiment -p pq-l2 -- --mode baseline > run.log 2>&1

# Per-trial (1-pass, iteration speed):
cargo run --release --bin run_experiment -p pq-l2 > run.log 2>&1
```

Parse from output: `geomean_ns_per_query`, `geomean_ns_ci_90pct`,
`worst_ns_per_query`, and on Linux `geomean_cycles_per_query`. Apply
the keep-gate. Commit `kernels.rs`; append a row to `results.tsv`
(untracked).

## Maintenance contract

When you ship user-visible changes:

- Update the relevant `docs/*.md` in the same change
  (`adding-a-target.md` if the workflow changes; `robustness.md` if
  the measurement layer changes; per-target `program.md` if the
  priors change).
- Append to the appropriate `lessons.md` (gitignored per-machine) if
  a trial produced a finding worth remembering.
- Don't grow this file. New deep content goes in `docs/`. AGENTS.md
  stays a map plus principles plus bright-line rules.
