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
//! - [`degenbot_solvers::mobius_int`] / [`degenbot_solvers::mobius_int_exact`] /
//!   [`degenbot_solvers::mobius_v3_int`] — value-only Möbius solver math, relocated
//!   to the `degenbot-solvers` crate.
//! - [`degenbot_solvers::basket`] — the `QuantAMM` N-token Balancer weighted
//!   basket solver, relocated to `degenbot-solvers`.

pub mod arb_engine;
