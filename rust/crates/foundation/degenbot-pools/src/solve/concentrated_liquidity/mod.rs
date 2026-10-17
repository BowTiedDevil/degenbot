//! The **Concentrated Liquidity Kernel** — the concentrated liquidity family's home and the
//! library the typed V3/V4 adapters call at the resolve seam.
//!
//! GLOSSARY.md "Concentrated Liquidity Kernel": the data-parameterized module
//! that is the concrete home of everything the concentrated-liquidity solve
//! shares across the Uniswap V3 and V4 designs — crossing derivation from a
//! tick-range sequence, the active-set walk over the piecewise-Möbius
//! segments, the concentrated liquidity hop and path simulation, and the word-profile build over
//! the prepared hop's tables.
//!
//! # Interface
//!
//! The kernel's intake is the **prepared hop** (ADR-069 D2): the tick-range
//! sequence plus the crossing table and word profiles derived from it, built
//! once per `(pool, direction)` by the projection and shared via `Arc`. The
//! solve entries take `&[PreparedConcentratedLiquidityHop]` (all-concentrated-liquidity) or one [`MixedHopView`] per
//! position (mixed), so the tables travel with the sequence that produced them
//! and nothing is re-derived per solve.
//!
//! The kernel is generic over *data*, never over a trait per pool (ADR-059 D6):
//! there is no trait object on the walk, so the hottest loop in the solver
//! stays monomorphic. The concrete V3 and V4 designs differ in storage slots,
//! event shapes, and state reads (slot0 vs state-view, hooks hashed into the
//! PoolId), not in solve structure, so their adapters stay typed code
//! (ADR-059 D3) and call the kernel at the resolve seam to produce the one
//! prepared hop.
//!
//! # Import direction
//!
//! `concentrated_liquidity/*` carries no envelope vocabulary — no `Line`, verdict, or cap. The
//! tangent fan is derived from the kernel's crossing tables by the concentrated liquidity family
//! module's bound side, and the engine-owned fan cache stays borrowed, never
//! owned.
//!
//! The walk's refine-window intake is kernel-side data ([`RefineBound`]), not
//! an envelope type: the concentrated liquidity family module translates the composed
//! `PathBoundLines` into windows at the seam (see `concentrated_liquidity::window`).
//!
//! # Module layout
//!
//! - `hop_sim` — the canonical per-step V3 integer swap.
//! - `word_profile` — dense-range forward word-boundary profiles.
//! - `crossings` — crossing tables, hop assembly, and window-edge helpers.
//! - `active_set` — the active-set piecewise Möbius walk.
//! - `entries` — the public solve entry points, [`PreparedConcentratedLiquidityHop`], and the
//!   mixed entry's [`MixedHopView`] intake.
//! - `memo` — the cross-block composition memo.
//! - `path_sim` — the public byte-exact forward path simulator.
//! - `window` — the kernel-side refine-window data ([`RefineBound`]) and its
//!   exact integer clamp, with no envelope vocabulary.
//! - `tally` — the owned walk-telemetry value: the per-solve [`WalkTally`]
//!   accumulator and the engine-owned [`WalkTelemetry`] collector it merges
//!   into.
//! - `telemetry` — the event census and the walk's timed-section helper,
//!   written only when the crate's default-off `telemetry` feature is enabled.
//! - `cache` — the cache-lab seam: instrumented crossing-table + word-profile
//!   cache strategies (crossing derivation is kernel territory), driven by the
//!   `concentrated_liquidity_cache_lab` example and the `concentrated_liquidity_cache_*` regression binaries.

use std::sync::Arc;

pub(crate) use crate::int_v3_hop::IntV3TickRangeHop;
pub use crate::int_v3_hop::{IntTickRangeCrossing, IntV3TickRangeSequence};

/// Cached per-ending-range crossing table, parallel to `IntV3TickRangeSequence`.
pub type ConcentratedLiquidityCrossingTable = Vec<IntTickRangeCrossing>;
/// Cached dense-range word-boundary profile table, parallel to crossings.
pub type ConcentratedLiquidityProfileTable = Vec<Option<Arc<ConcentratedLiquidityWordProfile>>>;

mod active_set;
mod crossings;
mod entries;
mod hop_sim;
mod memo;
mod path_sim;
mod tally;
mod telemetry;
mod window;
mod word_profile;

pub mod cache;

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    clippy::panic,
    clippy::similar_names
)]
mod tests;

pub use active_set::{WalkOutcome, WalkRefusal, WalkStats};
pub use crossings::{
    build_concentrated_liquidity_crossing_table, build_concentrated_liquidity_word_profiles,
    build_concentrated_liquidity_word_profiles_from_crossings, DENSE_OBSERVE_THRESHOLD,
};
pub use entries::{
    derive_and_solve_concentrated_liquidity_piecewise, solve_concentrated_liquidity_piecewise,
    solve_piecewise, MixedHopView, PreparedConcentratedLiquidityHop,
};
pub use hop_sim::{simulate_v3_range_swap, V3RangeSwapResult};
pub use memo::{WalkMemo, WalkMemoStats};
// The byte-exact forward path simulator is the concentrated liquidity kernel's public harness
// surface (external benchmarks/offline replays evaluate path profit through
// it), so it stays public API: the family module owns the solve/bound surface,
// not this simulator.
pub use path_sim::{ConcentratedLiquidityPathOutcome, ConcentratedLiquidityPathSim};
pub use tally::{WalkNsCensus, WalkSection, WalkTally, WalkTelemetry};
pub use telemetry::WalkEventCensus;
pub use window::RefineBound;
pub use word_profile::ConcentratedLiquidityWordProfile;
