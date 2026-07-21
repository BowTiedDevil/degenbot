//! V2-style constant-product (`x·y=k`) swap math — pure-Rust, EVM-exact.
//!
//! This crate owns the constant-product AMM swap primitive that the V2 pool
//! family (Uniswap V2, Sushiswap, `PancakeSwap` V2, Swapbased, Camelot-v2
//! volatile, Aerodrome-v2 volatile) and the Solidly/Camelot/Aerodrome
//! volatile "V2-equivalent" hop representation all share: the EVM-exact
//! single-hop `getAmountOut`
//!
//! ```text
//! y = gamma_numer * reserve_out * x / (fee_denom * reserve_in + gamma_numer * x)
//! ```
//!
//! with floor division (EVM `DIV` semantics), computed in `U512` and narrowed
//! to `U256`.
//!
//! It is the V2-family sibling of the other pure-math leaf crates:
//! `degenbot-cl-math` (concentrated liquidity), `degenbot-curve-math`
//! (stableswap), `degenbot-balancer-math` (weighted/stable),
//! `degenbot-solidly-math` (Solidly stable), and `degenbot-evm-math`
//! (EIP-1559 / gas).
//!
//! ## Why this is a standalone crate (ADR-005 "standalone constraint")
//!
//! A single-hop swap amount calc is a pool-specific, value-only concern — it
//! is *not* an arbitrage/path-optimization (solver) concern. Previously the
//! primitive lived in `degenbot-bot/src/solvers/mobius_int.rs`, which is
//! documented as the "Arbitrage solvers using Möbius transformation
//! composition" module. That placement inverted the dependency direction:
//! pool-state code in `bot_core` reached *up* into `solvers` for a primitive
//! the solvers themselves sit on top of, and it stranded standalone-usable
//! V2 swap math inside the `degenbot-bot` engine crate — so a `cargo add
//! degenbot` consumer wanting just V2 swap math pulled the engine/pump/
//! registry surface.
//!
//! This crate is `pyo3`-free under its default features (enforced by
//! `just check-no-pyo3-in-cores`); it depends only on `alloy`. It is consumed
//! by `degenbot-bot` (the engine + the Möbius solvers, which compose
//! `IntHopState` hops into multi-hop arbitrage paths) and re-exported by the
//! `degenbot` umbrella for standalone Rust consumers.
//!
//! ## Contents
//!
//! The primitive surface lives in the [`hop_state`] submodule: [`IntHopState`]
//! (+ `new` / `swap`), [`SimulationResult`], and [`int_simulate_path`].

pub mod hop_state;
pub use hop_state::{int_simulate_path, HopSwapError, IntHopState, SimulationResult};
