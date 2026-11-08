#![expect(clippy::doc_markdown)]
//! The `ExecutionAdapter` seam (ADR-025, ADR-098) — a deep, user-owned
//! execution layer over the thin engine.
//!
//! This pyo3-free crate owns the **`ExecutionAdapter` trait + its value
//! types**: the solve-result view ([`SolveResult`]), the neutral transaction
//! ([`TransactionRequest`]), the engine's typed
//! [observation](ExecutionObservation) of a simulated transaction, the gate
//! protocol ([`ProbeSpec`] / [`AssessOptions`] /
//! [`ExecutionResult`] / [`AssessError`] / [`FeePolicy`]), and the Encode seam
//! ([`PayloadComposer`] / [`ComposerInputs`] / [`ComposeError`], with
//! [`PayloadAdapter`] binding an encoding-only composer to its target). It
//! holds **no default strategy** and no simulation implementation.
//!
//! A strategy decomposes into four parts (ADR-025 D2):
//!
//! - **Encode** — one complete [`TransactionRequest`] (`to` + `data` +
//!   native `value`) for ONE execution contract. Rust users implement
//!   `ExecutionAdapter::encode`; Python users supply a callable lifted into it
//!   by `degenbot-python`. An encoding-only consumer implements the smaller
//!   [`PayloadComposer`] and binds it with [`PayloadAdapter`].
//! - **Probe** — declared data: which pre/post read-calls to snapshot
//!   ([`ProbeSpec`]). The engine runs the reads / warms the cache / access
//!   list.
//! - **Assess** — the adapter's interpretation of a typed
//!   [`ExecutionObservation`]: economic success, gross profit, diagnostics.
//!   The built-in shapes ([`AssessRule`]) are defaults, never requirements; a
//!   custom assessment reuses [`ExecutionResult::price`] for the pricing half,
//!   which also pins the terminal-status invariant: a reverted observation is
//!   always a zero-gross decline with `passed = false`.
//! - **Fee** — the **defaulted pricing half of Assess** ([`FeePolicy`]), not a
//!   fifth seam. `net = gross − gas×(base_fee_next + priority_fee)` is defined
//!   in terms of the pricing policy, so pricing cannot be independently
//!   ordered; a built-in market-percentile default (TARGET_PROFIT_RATIO /
//!   age-decay) is provided, overridable by a foreign searcher.
//!
//! The engine owns simulation mechanics and the production scope is **one
//! opportunity → one transaction** (ADR-098 D2): this crate carries no
//! simulation loop, no multi-transaction choreography, and no capability or
//! version metadata. An adapter may keep any private protocol version,
//! release, or deployment state it needs (ADR-098 D4/D5).
//!
//! The generic seam has no built-in `cmd_executor` policy. The concrete
//! production `CmdExecutorAdapter` lives in `degenbot-strategy`, where it owns
//! the canonical command-executor path; a foreign user's crate implements
//! [`ExecutionAdapter`] directly, or supplies a Python callable via the PyO3
//! lift — both foreign paths meet this same generic seam.
//!
//! `pyo3`-free (enforced by `just check-no-pyo3-in-cores`); consumable by both
//! the standalone Rust path and the PyO3 driver shell. Dep graph is a DAG:
//! `execution → {pools}`; `executor → execution` (ADR-098 D1) — this seam owns
//! the neutral opportunity descriptors ([`PathInfo`] / [`HopInfo`]), so a
//! foreign adapter never depends on the built-in command executor.

pub mod adapter;
pub mod gate;
pub mod observation;
pub mod path;
pub mod payload;
pub mod solve_result;
pub mod transaction;

pub use adapter::ExecutionAdapter;
pub use gate::{
    AssessError, AssessOptions, AssessRule, ExecutionResult, FeePolicy, ProbeSpec, ProbeSpecs,
};
pub use observation::{CapturedEvent, ExecutionObservation, ExecutionStatus, ProbeReading};
pub use path::{HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo};
pub use payload::{ComposeError, ComposeOptions, ComposerInputs, PayloadAdapter, PayloadComposer};
pub use solve_result::SolveResult;
pub use transaction::TransactionRequest;
