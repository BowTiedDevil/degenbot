//! # `degenbot-batch-executor` — the core batch executor
//!
//! ONE engine module owning the per-batch dispatch choreography both
//! first-class consumers drive (glossary: the core dispatch pipeline and the
//! Batch outcome record):
//!
//! 1. **Assembly** ([`assembly`]) — raw solver rows + inline-sim payload rows
//!    are shaped into pre-sim [`DispatchCandidate`]s / [`SubmitCandidate`]s.
//!    Every pre-sim policy (payload-served, resolve miss, suppression, pool
//!    divergence, fee-on-transfer, thin margin) applies HERE, exactly once,
//!    and emits a typed [`AssemblyVerdict`] per row.
//! 2. **Simulate** — the survivors ride the core fan-out
//!    (`degenbot_arbitrage::dispatch_profitable_results`) and are categorized
//!    into typed [`SimulateVerdict`]s.
//! 3. **Submit** — the joined candidates ride the production submit
//!    orchestration (`degenbot_submission::dispatch_and_submit`) and produce
//!    typed [`SubmitVerdict`]s.
//!
//! The ordered lane (bounded sim fan-out, FIFO submit order = nonce order,
//! loud-abort re-raise in the caller's frame) is
//! [`degenbot_submission::SimSubmitPipeline`] — CONSUMED, not re-implemented.
//! The module is value-configured ([`executor::ExecutorConfig`]): a driver
//! injects the cap as a plain count, the policy values, and the relay
//! posture — never choreography code. Its product is the stream of
//! [`record::BatchOutcome`] records; a driver's remaining responsibility is
//! display.
//!
//! Standalone-core constraint (ADR-005): pyo3-free. The pure-Rust consumer
//! (`cargo add degenbot`) and the `PyO3` shell (a thin construction +
//! `next_outcome` + typed-enums seam, ADR-013/ADR-032) drive the same module.

pub mod assembly;
pub mod executor;
pub mod record;
pub mod row;

pub use assembly::{
    build_raw_candidate, classify_raw_row, derive_path_pools, join_sim_result,
    merge_payload_results, AssemblyError, MergedPayloadOutcome, PathResolver, PayloadArm,
    RawRowClass,
};
pub use executor::{BatchExecutor, BatchWork, ExecutorConfig};
pub use record::{
    fold_counters, AssemblyVerdict, BatchCounters, BatchOutcome, FailureDetail, FailureKind,
    PathInfoView, SimReceipt, SimulateVerdict, SubmitVerdict,
};
pub use row::{PayloadFailure, PayloadRow, RawResult};
