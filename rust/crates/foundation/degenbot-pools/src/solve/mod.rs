//! Value-only multi-hop arbitrage solver math — computes optimal arbitrage
//! inputs + simulates multi-hop swap paths purely from in-memory hop states
//! (V2 constant-product via `degenbot-v2-math::IntHopState`, V3/V4
//! concentrated-liquidity via `crate::int_v3_hop::IntV3TickRangeHop`, Balancer
//! weighted/stable + Curve + Solidly via their math leaves), with no chain /
//! registry / async / tokio. Also holds the QuantAMM closed-form N-token
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
//! PyO3 driver shell.

// tokio's full `RuntimeMetrics` getters — which the `hotpath` crate reads for
// solver-phase profiling — are gated behind tokio's unstable cfg; there is no
// repository-wide rustflag, so supply it per-build (`just dev`,
// `just test-hotpath`). Without it the metrics are silently absent.
#[cfg(all(feature = "hotpath", not(tokio_unstable)))]
compile_error!(
    "the `hotpath` feature requires `RUSTFLAGS=--cfg tokio_unstable`; use `just dev` / `just test-hotpath`"
);

pub mod affected_keys;
pub mod basket;
pub mod bounded_brent;
pub mod capture_fixture;
pub mod mixed;
pub mod mobius_int;
#[expect(clippy::doc_markdown)]
pub mod mobius_int_exact;

#[expect(clippy::doc_markdown)]
pub mod concentrated_liquidity;
pub mod mobius_shifted_piece;
pub mod profit_envelope;
pub mod runtime;
