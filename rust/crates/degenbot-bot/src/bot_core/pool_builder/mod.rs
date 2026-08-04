//! Pool-builder construction orchestration (epic `Z5CNPB`, Part 1).
//!
//! The builder-choreography port: the orchestrated encode→call→decode
//! choreography that the Python `builders/` drove through `PyBotIo` moves
//! **core-side** here as free async functions over the atomic [`ConstructionIo`]
//! handle. The `PyO3` wrapper (`degenbot-python`) re-points its public methods
//! to `block_on` these functions, so a standalone `cargo add degenbot` consumer
//! and a Python-driven bot share one construction path (no `pyo3` in this
//! module — the no-pyo3-in-cores invariant).
//!
//! This module:
//! - [`choreography`] — the moved encode→call→decode primitives (V2/V3/V4 +
//!   ERC-20 + tick), per decision D-C.
//! - (future, task `3FVZF4`) — the `PoolBuilder` orchestration that composes
//!   these primitives into core structural pool identity+state.

pub mod builder;
pub mod choreography;

#[cfg(test)]
mod tests;
