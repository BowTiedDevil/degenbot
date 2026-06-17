//! Arbitrage optimizers using Möbius transformation composition.
//!
//! Every constant product swap y = (γ·s·x)/(r + γ·x) is a Möbius
//! transformation that fixes the origin. An n-hop path composes into
//! l(x) = K·x / (M + N·x), and the optimal input is
//! `x_opt` = (√(K·M) - M) / N (exact, zero iterations).
//!
//! # Modules
//!
//! - [`mobius_int`] — U512/U256 integer-exact Möbius recurrence + simulation
//! - [`mobius_int_exact`] — closed-form U512-native solver (`exact_mobius_solve`)
//! - [`mobius_v3_int`] — integer V3 tick-range types, crossing, CL solvers

pub mod affected_keys;
pub mod liquidity_event_buffer;
#[allow(clippy::doc_markdown)]
pub mod mobius_int;
#[allow(clippy::doc_markdown)]
pub mod mobius_int_exact;
#[allow(clippy::doc_markdown)]
pub mod mobius_v3_int;
pub mod uniswap_engine;
pub mod uniswap_engine_pump;
pub mod v2_sync_decoder;
