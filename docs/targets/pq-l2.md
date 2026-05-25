# Target: `pq-l2`

PQ L2 distance kernel for f32 dense vectors, the asymmetric-distance compute
that runs on every `IvfPq` / `IvfHnswPq` ANN query in Lance.

## Status

**Landed.** `kernels.rs` starts as a clone of upstream's
`compute_pq_distance` + `build_distance_table_l2` via `lance-snapshots`.
The agent's job is to find generalizable speedups on top of upstream SOTA.

## What's optimized

Two functions in `crates/pq-l2/src/kernels.rs`, mirroring upstream's API split:

- `PqKernel::distance_table(query, &mut out)`, writes the asymmetric
  distance table (`[num_sub_vectors][num_centroids]`) for one query against
  the codebook into a caller-provided `&mut [f32]` buffer. The bench
  pre-allocates and reuses one buffer per workload so allocator cost stays
  out of the per-query timing. Cost:
  `num_sub_vectors × num_centroids × sub_vector_dim` MAC ops per query.
  Mirrors upstream's `build_distance_table_l2`.
- `PqKernel::compute_distances(table, &mut out)`, for each PQ-encoded
  vector, accumulates the L2 distance via `num_sub_vectors` table lookups
  and writes `num_vectors` distances into the caller-provided `&mut [f32]`.
  Cost: `num_vectors × num_sub_vectors` lookups per query. Dominant cost at
  typical scales. Mirrors upstream's `compute_pq_distance`.

Top-K selection is **external to the kernel** (in `run_experiment.rs`),
matching upstream's split.

`PqKernel::new(shape, codebook, codes, num_vectors)` is also editable ,
the agent may pre-process anything (codebook transpose, codes transpose,
`L2Prepared` SoA layout, cached `c·c`, packed LUTs) and amortize over
queries; build cost is excluded from per-query timing.

## Upstream Lance source

Vendored verbatim in `crates/lance-snapshots/src/pq.rs` (at the SHA pinned
in `lance-snapshots/src/lib.rs`):

- `build_distance_table_l2` ← `rust/lance-index/src/vector/pq/distance.rs`
- `compute_pq_distance` ← same file
- `transpose` ← `rust/lance-index/src/vector/pq/storage.rs`

The reference kernel (`reference.rs`) calls these directly; the agent's
`kernels.rs` starts as the same call set and optimizes from there.

When porting a winning kernel upstream:
- File: `lance-linalg/src/distance/l2.rs` and the L2-specific path in
  `lance-index/src/vector/pq/distance.rs`.
- License: Apache-2.0 (matches our Apache-2.0 directly).

## Oracle

**Float-accumulator-tolerance match against the upstream-vendored reference.**
Per `harness_common::MAX_ABS_ERR = 1e-4`:

- Distance table values must match within `1e-4` per element.
- Per-vector distances vec must match within `1e-4` per element.

Both gates assert on every input combination, five input distributions ×
three PQ shapes = 15 cases per trial. Loose enough for legal SIMD-accumulator
reordering, tight enough to catch real arithmetic bugs (e.g., the
`q²+c²-2q·c` cancellation trap on the `large_dynamic_range` fixture).

## Speed workload

Three shapes:
- `(128, 16, 256)`, SIFT-like; sub_vector_dim = 8
- `(256, 16, 256)`, sub_vector_dim = 16
- `(768, 96, 256)`, BERT-base-like; large codebook

Three data distributions:
- `Clustered`, 32 cluster centers, low intra-cluster noise
- `Uniform`, uniform on [-1, 1]
- `Sparse`, 90% zeros + 10% Gaussian

Per (shape × distribution): 20,000 base vectors PQ-encoded, 32 queries
timed. Total trial wall-clock: ~30s on a developer laptop (1-pass) or
~90s (`--mode baseline`, 3-pass).

## Output fields

```
correctness:           pass | fail
arch:                  aarch64 | x86_64 | ...
passes:                1 | 3
shapes_tested:         (128,16,256) (256,16,256) (768,96,256)
distributions_tested:  clustered uniform sparse
geomean_ns_per_query:  <u64>
geomean_ns_ci_90pct:   [<u64>, <u64>]
median_ns_per_query:   <u64>
geomean_cycles_per_query:       <u64> | n/a (no PMU access on this platform)
geomean_instructions_per_query: <u64> | n/a (no PMU access on this platform)
worst_ns_per_query:    <u64> (<shape>, <dist>)
best_ns_per_query:     <u64> (<shape>, <dist>)
per_combo_geomean_ns:
  (...)
peak_mem_mb:           <f64>
total_seconds:         <f64>
```

## Known headroom (priors for the agent)

See `crates/pq-l2/program.md` "Lance-PQ-specific priors" for the canonical
arch-split list. The full set of caveats (which tricks fail correctness,
which combinations regress small shapes) lives in `crates/pq-l2/lessons.md`
(gitignored per-machine; populated as trials surface findings).
