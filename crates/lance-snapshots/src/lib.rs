// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright The Lance Authors

//! Verbatim snapshots of Lance hot-path kernels (Apache-2.0).
//!
//! This crate makes the lance-autoresearch harness's baseline + correctness
//! oracle the **actual upstream Lance code**, not a strawman scalar
//! reimplementation. The agent's `kernels.rs` is then trying to beat
//! upstream's SOTA, not a naive baseline that upstream already surpassed.
//!
//! ## Upstream pinning
//!
//! All snapshots in this crate are copied verbatim from
//! [`lance-format/lance`](https://github.com/lance-format/lance) at commit
//! **`5cf70b27b3ad38ecdcd1547b7af385e05f67598a`** (2026-05-25; "feat: query
//! with scalar index support fast search ability", #6784).
//!
//! Each snapshot file's header lists its upstream source path and the same
//! pinned SHA. To re-sync against newer Lance HEAD, see `RESYNC.md`.
//!
//! ## Why vendor rather than depend
//!
//! - `compute_pq_distance` is `pub(super)` upstream; not callable as a
//!   direct dep without an upstream API change.
//! - Lance's full crate graph (`lance-linalg` + `lance-index`) pulls in
//!   Arrow + DataFusion (~30 transitive deps); too heavy for a kernel
//!   microbenchmark harness.
//! - Vendoring with a drift-check script gives us the right tradeoff:
//!   self-contained build, license cleanly attributed, periodic re-sync.
//!
//! Drift detection: `scripts/check-lance-drift.sh`.

pub mod assume;
pub mod l2;
pub mod pq;
