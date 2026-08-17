//! Path **investigation toolkit** — reusable building blocks for reproducing a
//! captured failing settlement-arbitrage path (the `path<N>_…_block<B>` fixtures written by
//! `scripts/capture_*_fixture.py`).
//!
//! **Layering.** This is degenbot's OWN pool-level scaffold (its capture format,
//! its V2/V3/V4 pool reconstruction, its per-hop pool oracle). It contains NO
//! executor logic and is NOT the deep reusable primitive. The genuinely reusable,
//! contract-agnostic EVM spine is [`degenbot_simulation::oracle`] — deploy a
//! pinned contract into revm, seed storage slots, drive a call, classify
//! Revert-vs-Halt — which any individual/a user's OWN contract harness builds on
//! via `scripts/scaffold_revm_harness.py` (emits a standalone per-contract
//! project at the user layer). This module's per-hop oracle checks currently use
//! the fast Rust twins; a deep bytecode-level probe would replace them with
//! `degenbot_simulation::oracle` verds.
//!
//! The historical `path*_solver_fixture.rs` examples each copy-pasted the same
//! ~200-line preamble (fixture structs, `tick_map`, `register_v2/v3`,
//! `build_v3/v4_state`, `v4_pool_id_bytes`) with only the family-specific fee
//! and report logic left to the example. That preamble now lives here as three
//! deep modules:
//!
//! - [`fixture`] — the superset capture schema + `PathFixture::load` (accepts
//!   every historical file, amounts as number or string).
//! - [`reconstruct`] — register captured pools into a `BotState` or build
//!   standalone `V3PoolState`/`V4PoolState`.
//! - [`hop_oracle`] — drive each hop's input through the tier-3-validated
//!   oracle twin (V2 `getAmountOut` / `v3_simulate_swap` / `v4_simulate_swap`)
//!   and compare the output to the solver's, per hop.
//!
//! A new investigation then needs only: load the fixture, register the pools,
//! run the Möbius solver, and call the per-hop oracle check — no re-derivation.

//! Investigation tooling: allowed to sidestep the pedantic doc/must_use
//! discipline applied to production core crates (analogous to the tier-3 test
//! files' own lint gates) — these are run-once diagnostic helpers, not library
//! surface that future API consumers depend on.

/// Ignore pedantic doc/must_use/hasher nits for this run-once tooling (see
/// module comment for rationale).
#[expect(clippy::missing_errors_doc)]
pub mod fixture;
#[expect(clippy::missing_panics_doc, clippy::must_use_candidate)]
pub mod hop_oracle;
#[expect(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::implicit_hasher
)]
pub mod reconstruct;

// `real_oracle` carries its own module-level `#![allow]` (it's self-contained
// tooling extracted from the path5000_v4_gas_probe example), so it needs no
// wrapper here.
pub mod real_oracle;

pub use fixture::{PathFixture, PathHop, PoolData, RecordedSolve, TickJson};
pub use hop_oracle::{
    display_check, v2_get_amount_out, v3_hop_output, v4_hop_output, v4_hop_output_consumed,
    OracleOutcome, OracleWithConsumed,
};
pub use reconstruct::{
    build_v3_state, build_v4_state, register_v2, register_v3, register_v3_with, register_v4,
    register_v4_with, V2_DEFAULT_FEE,
};
