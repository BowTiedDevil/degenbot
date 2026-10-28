//! Pool-builder construction orchestration.
//!
//! The builder port: the probe→dispatch→assemble turn the Python `builders/`
//! drove through `PyBotIo` moves **core-side** here as free async functions over
//! the atomic [`ConstructionIo`] handle. The `PyO3` wrapper (`degenbot-python`)
//! re-points its public methods to `block_on` these functions, so a standalone
//! `cargo add degenbot` consumer and a Python-driven bot share one construction
//! path (no `pyo3` in this module — the no-pyo3-in-cores invariant).
//!
//! Every contract read in this tier crosses ONE call step: the
//! [`ContractRead`](degenbot_rpc::abi::ContractRead) primitive the handle
//! implements (paced for tick reads through
//! [`PacedRead`](crate::bot_core::construction_io::PacedRead)), and the ABI
//! shapes come from `degenbot_rpc::abi`. What stays here is construction
//! *policy* — which read to fall back to, what a revert means, how a batch
//! degrades — never a second way to call the chain.
//!
//! This module:
//! - [`builder`] — the `PoolBuilder` orchestration: the pool-identity and
//!   fee/metadata reads with their construction policy (`build_v2/v3/v4`,
//!   `build_curve_pool`, `build_balancer_*`).
//! - [`curve_choreography`] — the Curve-specific detection primitives.
//!
//! - [`route`] — the construction route: the core's ONE entry from a
//!   requested pool to a constructed, registered pool (route order + DB
//!   two-step identity + get-or-register, classified on the registration
//!   ledger's build-refusal taxonomy).
//! - [`construction_door`] — the construction door: the ONE
//!   [`PoolConstruction`](construction_door::PoolConstruction) request and
//!   [`ConstructionVerdict`](construction_door::ConstructionVerdict) outcome,
//!   dispatching INTO [`route`]'s interior (ADR-078 D2).
//!
//! Note: `build_curve_pool` / `build_balancer_*` are not yet re-exported from
//! the `degenbot` umbrella nor exposed on `PyBot` (see ADR-023/D4);
//! `build_v2`/`build_v3`/`build_v4` are the standalone-reachable set.

pub mod builder;
pub mod construction_door;
pub mod curve_choreography;
pub mod parity;
pub mod route;
pub mod tick_source;

#[cfg(test)]
mod tests;
