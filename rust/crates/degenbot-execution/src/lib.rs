#![expect(clippy::doc_markdown)]
//! The `ExecutionStrategy` seam (ADR-025) — a deep, user-owned execution layer
//! over the thin engine.
//!
//! This pyo3-free crate owns the **`ExecutionStrategy` trait + its value
//! types**: the solve-result view ([`SolveResult`]), the gate protocol
//! ([`ProbeSpec`] / [`AssessOptions`] / [`ExecutionResult`] / [`FeePolicy`]),
//! and the Encode seam ([`PayloadComposer`] / [`ComposerInputs`] /
//! [`ComposeError`]). It holds **no default strategy**.
//!
//! A strategy decomposes into four parts (ADR-025 D2):
//!
//! - **Encode** — [`PayloadComposer`]: `solve result → payload bytes` for ONE
//!   execution contract. Rust users implement the trait; Python users supply a
//!   callable lifted into it by `degenbot-python` (`PyPayloadComposer`).
//! - **Probe** — declared data: which pre/post read-calls to snapshot
//!   ([`ProbeSpec`]). The engine runs the reads / warms the cache / access
//!   list.
//! - **Assess** — a gate rule: how deltas → gross and pass/fail (built-in
//!   shapes in [`AssessRule`] + a tiny optional user interpreter).
//! - **Fee** — the **defaulted pricing half of Assess** ([`FeePolicy`]), not a
//!   fifth seam. `net = gross − gas×(base_fee_next + priority_fee)` is defined
//!   in terms of the pricing policy, so pricing cannot be independently
//!   ordered; a built-in market-percentile default (TARGET_PROFIT_RATIO /
//!   age-decay) is provided, overridable by a foreign searcher.
//!
//! `degenbot-arbitrage` implements [`ExecutionStrategy`] as the
//! **default adapter** (stays Rust-canonical, ADR-019 R). A foreign user's
//! crate implements it directly, or supplies a Python callable via the PyO3
//! lift — both meet this same seam.
//!
//! `pyo3`-free (enforced by `just check-no-pyo3-in-cores`); consumable by both
//! the standalone Rust path and the PyO3 driver shell. Dep graph is a DAG:
//! `execution → {executor, simulation, solvers}`.

pub mod gate;
pub mod payload;
pub mod solve_result;
pub mod strategy;

// Re-export the Encode seam (ADR-025 D2) so `PayloadComposer` +
// `ComposerInputs` + `ComposeError` are reachable directly off the crate root,
// matching the docs contract (`use degenbot_execution::{PayloadComposer,
// ComposerInputs, ComposeError};`).
pub use gate::{AssessOptions, AssessRule, ExecutionResult, FeePolicy, ProbeSpec, ProbeSpecs};
pub use payload::{ComposeError, ComposeOptions, ComposerInputs, PayloadComposer};
pub use solve_result::SolveResult;
pub use strategy::ExecutionStrategy;
