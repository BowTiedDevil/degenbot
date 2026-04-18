//! Arbitrage optimizers using Möbius transformation composition.
//!
//! Every constant product swap y = (γ·s·x)/(r + γ·x) is a Möbius
//! transformation that fixes the origin. An n-hop path composes into
//! l(x) = K·x / (M + N·x), and the optimal input is
//! `x_opt` = (√(K·M) - M) / N (exact, zero iterations).
//!
//! # Modules
//!
//! - [`mobius`] — Core f64 Möbius recurrence, solve, and path simulation
//! - [`mobius_int`] — Integer U512/U256 Möbius with EVM-exact simulation
//! - [`mobius_v3`] — V3 tick range types, crossing computation, piecewise solve
//! - [`mobius_v3_v3`] — V3-V3 arbitrage solver (two V3 hops)
//! - [`mobius_batch`] — Batch solver (serial, vectorized, Rayon parallel)
//! - [`mobius_py`] — `PyO3` Python bindings

#[allow(clippy::doc_markdown)]
pub mod mobius;
#[allow(clippy::doc_markdown)]
pub mod mobius_batch;
#[allow(clippy::doc_markdown)]
pub mod mobius_int;
#[allow(clippy::doc_markdown)]
pub mod mobius_py;
#[allow(clippy::doc_markdown)]
pub mod mobius_v3;
#[allow(clippy::doc_markdown)]
pub mod mobius_v3_v3;
