//! Value-only multi-hop arbitrage solver math — computes optimal arbitrage
//! inputs + simulates multi-hop swap paths purely from in-memory hop states
//! (V2 constant-product via `degenbot-v2-math::IntHopState`, V3/V4
//! concentrated-liquidity via `crate::int_v3_hop::IntV3TickRangeHop`, Balancer
//! weighted/stable + Curve + Solidly via their math leaves), with no chain /
//! registry / async / tokio. Also holds the `QuantAMM` closed-form N-token
//! basket solver (`basket`) and the hop-state intake contract (`mixed` —
//! ADR-015).
//!
//! Every constant-product swap `y = (γ·s·x)/(r + γ·x)` is a Möbius
//! transformation that fixes the origin. An n-hop path composes into
//! `l(x) = K·x / (M + N·x)`, and the optimal exact-input is
//! `x_opt = (√(K·M) − M) / N` (closed-form, zero iterations).
//!
//! This tree is the ADR-073 endgame re-home of the dissolved
//! `degenbot-solvers` crate: the family-blind kernels live HERE, beside the
//! family read modules ([`crate::families`]), and each family's solve entry
//! lives inside its family module. The `arb_engine/` I/O orchestrator (event
//! subscription, BotState-driven pump, async/tokio, RPC) stays in
//! `degenbot-bot` and imports the solver math from this module tree.
//!
//! `pyo3`-free (enforced by `just check-no-pyo3-in-cores`); consumable by
//! both the standalone Rust path (`cargo add degenbot-pools`) and the
//! `PyO3` driver shell.
//!
//! # The door (ADR-084 D2/W2)
//!
//! `pub` at this module's root is the curated surface: the mixed-path intake
//! contract promoted from the former `solve::mixed` tree, the profit-envelope
//! gate boundary behind [`gate`], the walk modules, the runtime knobs, the
//! basket entry, and the feature-gated fixture reader. The modules beneath
//! the door are `pub(crate)` unless the door itself names them. The item set
//! is pinned by the committed census (`tests/solve_door_census.txt` in the
//! facade crate), closed under D2(a): a type in a public field of a door item
//! is a door item.

// tokio's full `RuntimeMetrics` getters — which the `hotpath` crate reads for
// solver-phase profiling — are gated behind tokio's unstable cfg; there is no
// repository-wide rustflag, so supply it per-build (`just dev`,
// `just test-hotpath`). Without it the metrics are silently absent.
#[cfg(all(feature = "hotpath", not(tokio_unstable)))]
compile_error!(
    "the `hotpath` feature requires `RUSTFLAGS=--cfg tokio_unstable`; use `just dev` / `just test-hotpath`"
);

// Door modules.
pub mod basket;

#[expect(clippy::doc_markdown)]
pub mod concentrated_liquidity;

pub mod runtime;

// Fixture I/O: transparent .zst corpus decode (ADR-084 D3). Feature-gated so
// the C-build dependency (zstd) stays out of every default dependency tree;
// test lanes enable `capture-fixtures` explicitly.
#[cfg(feature = "capture-fixtures")]
pub mod fixtures;

// The gate door: the profit-envelope boundary types, hoisted from the former
// `solve::profit_envelope` path (no `solve::gate::gate` collision — the inner
// gate submodule stays private).
pub mod gate;

// ---------------------------------------------------------------------------
// Door items — the mixed-path intake contract (ADR-015), promoted from the
// former `solve::mixed` tree to the root door.
// ---------------------------------------------------------------------------
pub use crate::families::solidly::solve::simulate_solidly_path;
pub use mixed::solve::{solve_path, solve_path_with_min_profit, SolveOutcome};
pub use mixed::{
    BalancerStableHopState, BalancerWeightedHopState, CurveStableswapHopState, HopType, MixedPath,
    MixedPoolRef, PoolHop, ResolvedHop, ResolvedMixedPath, SolidlyHopState, SolveArmOutcome,
    SolvePathResult, UnsupportedHopIdentity, V3SnapshotData, V4SnapshotData, INT128_MAX,
};
// Internal plumbing: the per-family solve entry and the dispatch rows the
// family arms and the line-derivation kernel share.
pub(crate) use mixed::solve::solve_golden_section_path_int;
pub(crate) use mixed::solve::{family_row_for, FamilyRow, RESERVE_CAP_ROW};

// Internal modules — `pub(crate)`; external callers name the door above.
// The two kernel characterization labs are in-crate `#[cfg(test)]` modules
// (ADR-084 D2(b): a lab needing internals moves into the crate).
#[cfg(test)]
mod bounded_brent_golden_tests;
#[cfg(test)]
mod mobius_discriminant_tests;
// Brent bounded minimization: consumed only by the in-crate characterization
// labs today, so it compiles with them (ADR-084 D2(b) — a lab's kernel rides
// the lab until a consumer earns it a door).
#[cfg(test)]
pub(crate) mod bounded_brent;
pub(crate) mod mixed;
pub(crate) mod mobius_int;
#[expect(clippy::doc_markdown)]
pub(crate) mod mobius_int_exact;
pub(crate) mod mobius_shifted_piece;
pub(crate) mod profit_envelope;
