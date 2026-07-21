//! Arbitrage solvers.
//!
//! Möbius transformation composition: every constant product swap
//! y = (γ·s·x)/(r + γ·x) is a Möbius transformation that fixes the origin.
//! An n-hop path composes into l(x) = K·x / (M + N·x), and the optimal input
//! is `x_opt` = (√(K·M) - M) / N (exact, zero iterations).
//!
//! Non-Möbius pool families (Solidly stable, Curve stableswap, Balancer
//! weighted/stable) fall back to a derivative-free search (Brent /
//! golden-section) plus integer verification — see `arb_engine::solver_dispatch`.
//!
//! # Modules
//!
//! - [`arb_engine`] — the multi-DEX engine (V2/V3/V4/Solidly; Curve + Balancer
//!   in progress), owning the per-block lifecycle, path registry, and solve
//!   dispatch
//! - `mobius_int` / `mobius_int_exact` / `mobius_v3_int` — relocated to the
//!   `degenbot-solvers` crate (value-only Möbius solver math).
//! - `balancer_weighted_basket` (`QuantAMM`) — relocated to `degenbot-solvers::basket`.

pub mod arb_engine;
