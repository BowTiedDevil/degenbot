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
//! - [`builder`] — the `PoolBuilder` orchestration (task `3FVZF4`) that composes
//!   these primitives into core structural pool identity+state (`build_v2/v3/v4`,
//!   `build_curve_pool`, `build_balancer_*`).
//! - [`curve_choreography`] — the Curve-specific primitives.
//!
//! Note: `build_curve_pool` / `build_balancer_*` are not yet re-exported from
//! the `degenbot` umbrella nor exposed on `PyBot` (see ADR-023/D4 + epic
//! `VK3YDM`); `build_v2`/`build_v3`/`build_v4` are the standalone-reachable set.

pub mod builder;
pub mod choreography;
pub mod curve_choreography;

#[cfg(test)]
mod tests;
