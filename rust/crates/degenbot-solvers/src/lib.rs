#![allow(clippy::doc_markdown)]
//! Value-only multi-hop Möbius solver math — computes optimal arbitrage
//! inputs + simulates multi-hop swap paths purely from in-memory hop states
//! (V2 constant-product via `degenbot-v2-math::IntHopState`, V3
//! concentrated-liquidity via `degenbot-pools::IntV3TickRangeHop`), with no
//! chain / registry / async / tokio.
//!
//! Every constant-product swap `y = (γ·s·x)/(r + γ·x)` is a Möbius
//! transformation that fixes the origin. An n-hop path composes into
//! `l(x) = K·x / (M + N·x)`, and the optimal exact-input is
//! `x_opt = (√(K·M) − M) / N` (closed-form, zero iterations).
//!
//! **Relocated** from `degenbot-bot/src/solvers/{mobius_int,
//! mobius_int_exact, mobius_v3_int, affected_keys}.rs`. The
//! `arb_engine/` I/O orchestrator (event subscription, BotState-driven
//! pump, async/tokio, RPC) stays in `degenbot-bot` and imports the solver
//! math from this crate.
//!
//! `pyo3`-free (enforced by `just check-no-pyo3-in-cores`); consumable by
//! both the standalone Rust path (`cargo add degenbot-solvers`) and the
//! PyO3 driver shell.
pub mod affected_keys;
#[allow(clippy::doc_markdown)]
pub mod mixed;
#[allow(clippy::doc_markdown)]
pub mod mobius_int;
#[allow(clippy::doc_markdown)]
pub mod mobius_int_exact;
#[allow(clippy::doc_markdown)]
pub mod mobius_v3_int;
