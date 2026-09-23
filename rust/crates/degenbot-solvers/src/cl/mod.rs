//! Integer-exact V3 tick range representation and Möbius coefficient computation.
//!
//! Converts V3 concentrated liquidity parameters (L, √P, tick bounds) into
//! integer effective reserves compatible with the exact Möbius solver
//! (`exact_mobius_solve`).
//!
//! # V3 Effective Reserves
//!
//! A V3 tick range is a bounded-product CFMM with virtual reserves:
//!
//! ```text
//! R₀ + α = L / √P    (token0 virtual reserves, in Q96 integer form)
//! R₁ + β = L · √P    (token1 virtual reserves, in Q96 integer form)
//! ```
//!
//! Since √P is stored as `sqrtPriceX96 = √P · 2^96`, the effective reserves are:
//!
//! ```text
//! R₀ + α = (L · 2^96) / sqrtPriceX96
//! R₁ + β = (L · sqrtPriceX96) / 2^96
//! ```
//!
//! These are integer divisions that match EVM truncation semantics.
//!
//! # Fee Representation
//!
//! V3 fee is stored as `fee / 1_000_000` (e.g., 3000 = 0.3%).
//! Gamma = 1 - fee = (1_000_000 - fee) / 1_000_000.
//! We store `gamma_numer = 1_000_000 - fee`, `fee_denom = 1_000_000`.
//!
//! # Module layout
//!
//! - `hop_sim` — the canonical per-step V3 integer swap.
//! - `word_profile` — dense-range forward word-boundary profiles.
//! - `crossings` — crossing tables, hop assembly, and window-edge helpers.
//! - `active_set` — the active-set piecewise Möbius walk.
//! - `entries` — the public solve entry points and [`ClSolveTables`].
//! - `memo` — the cross-block composition memo.
//! - `telemetry` — walk counters, census, and process-wide timing statics,
//!   written only when the crate's default-off `telemetry` feature is enabled.

use std::sync::Arc;

pub use ::degenbot_pools::int_v3_hop::{
    IntTickRangeCrossing, IntV3TickRangeHop, IntV3TickRangeSequence,
};

/// Cached per-ending-range crossing table, parallel to `IntV3TickRangeSequence`.
pub type ClCrossingTable = Vec<IntTickRangeCrossing>;
/// Cached dense-range word-boundary profile table, parallel to crossings.
pub type ClProfileTable = Vec<Option<Arc<ClWordProfile>>>;

mod active_set;
mod crossings;
mod entries;
mod hop_sim;
mod memo;
mod telemetry;
mod word_profile;

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    clippy::panic,
    clippy::similar_names
)]
mod tests;

pub use active_set::{WalkOutcome, WalkStats};
pub use crossings::{
    build_cl_crossing_table, build_cl_word_profiles, build_cl_word_profiles_from_crossings,
    DENSE_OBSERVE_THRESHOLD,
};
pub use entries::{
    derive_and_solve_cl_piecewise, solve_cl_piecewise, solve_mixed_piecewise, ClSolveTables,
};
pub use hop_sim::{simulate_v3_range_swap, V3RangeSwapResult};
pub use memo::{walk_path_fingerprint, WalkMemo, WalkMemoStats};
pub use telemetry::{
    WalkEventCensus, WALK_ANCHOR_ARGMAX_NS, WALK_ANCHOR_BUILD_NS, WALK_ANCHOR_COMPOSE_NS,
    WALK_ANCHOR_NS_TOTAL, WALK_CENSUS_DIR_NS, WALK_CENSUS_DIR_SIMNS, WALK_CENSUS_DIR_SIMS,
    WALK_CENSUS_EDGE_NS, WALK_CENSUS_EDGE_SIMNS, WALK_CENSUS_EDGE_SIMS, WALK_CENSUS_REDGE_NS,
    WALK_CENSUS_REDGE_SIMNS, WALK_CENSUS_REDGE_SIMS, WALK_CENSUS_REFINE_NS,
    WALK_CENSUS_REFINE_SIMNS, WALK_CENSUS_REFINE_SIMS, WALK_CENSUS_SIMNS, WALK_PRED_NS_TOTAL,
    WALK_SIM_NS_TOTAL, WALK_SOLVE_NS_TOTAL,
};
pub use word_profile::ClWordProfile;
