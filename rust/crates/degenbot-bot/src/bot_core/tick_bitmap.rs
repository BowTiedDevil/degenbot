//! Concentrated-liquidity tick-bitmap traversal + tick-range composition for
//! V3/V4 swap simulation.
//!
//! **Relocated** to `degenbot-pools` (value-only: `gen_ticks`,
//! `compute_tick_ranges`, `V3TickRangeForSolver`, `update_tick_liquidity`,
//! `apply_liquidity_to_tick_range` operate on `HashMap<i32, TickInfo>` +
//! `degenbot-cl-math` primitives — no `PoolState`). The full module content now
//! lives in `degenbot_pools::tick_bitmap`; re-exported here at the historical
//! `bot_core::tick_bitmap` path so the bot's consumers (`v3_state` /
//! `v4_state` swap sims, `liquidity_verifier`) resolve unchanged.
//!
//! Transient re-export — repointed at `degenbot_pools::tick_bitmap` natively
//! by USPN7M/P2CKRL.

pub use ::degenbot_pools::tick_bitmap::*;
