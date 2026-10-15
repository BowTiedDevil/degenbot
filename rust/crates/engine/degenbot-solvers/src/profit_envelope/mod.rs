//! Profit-envelope gate: rigorous pre-solve upper bound on path profit.
//!
//! For each hop we derive affine lines that dominate the hop's true output
//! curve point-wise; chaining across hops composes line sets (affine ∘ affine
//! stays affine), so the path-level bound is an exact point-wise minimum of
//! affine functions evaluated by integer-only arithmetic. If the maximum of
//! `bound(x) − x` over inputs falls below `min_profit`, the active-set walk is
//! provably unprofitable and is skipped without a single simulation.
//!
//! **Soundness core** (see GLOSSARY.md "Profit envelope gate"):
//! - A CL hop's output curve is concave; on ending-range piece `j` its slope
//!   never exceeds the marginal price at that piece's entry × (1−fee).
//!   Extending that entry slope linearly from the piece's cumulative
//!   `(gross_input, output)` anchor therefore dominates the curve on the whole
//!   domain (tangent-line property of concave functions; for a piecewise-linear
//!   concave function each segment's own line dominates globally).
//! - A Solidly/Aerodrome/Camelot STABLE hop's output curve is concave in the
//!   input (its marginal rate is non-increasing as the input grows), so its
//!   entry tangent dominates globally; the reserve cap stays beside it.
//! - CL swaps are monotone non-decreasing, so bounds chain monotonically.
//! - Every derived coefficient is computed exactly (I512); evaluation applies
//!   CEIL division so each line can only round UP, never under-cut the curve.
//!
//! The derivations and the measured evaluations behind these bounds — the
//! Solidly/Balancer-stable concavity proofs, the Curve non-concavity
//! counterexamples, and the rejected simulation-free `t`-search — are recorded
//! in `docs/architecture/stableswap-gate-bounds-and-solve-method-evaluation.md`.
//!
//! **Not** a valid bound: extending the FIRST piece's Möbius map beyond its
//! validity window (deeper later ranges can beat it). Only the entry-slope
//! envelope form is sound.
//!
//! Unsupported hop families make the gate return `Envelope::Unsupported`;
//! the caller must NOT skip in that case (conservative) — the verdict type
//! carries the distinction, so it cannot be ignored by accident.
//!
//! # Module layout
//!
//! - `kernel` — the `Line`/I512 numeric kernel, the exact concave-envelope
//!   composition (`compose_envelopes_exact`), and the discrete concave max.
//! - `derive` — per-hop line derivation (Möbius rises, CL tangent fans,
//!   `hop_lines_and_cap[_cached]`) plus the engine-owned prefix / fan cache.
//! - `gate` — the gate entry (`path_profit_bound*`), the typed verdict, the
//!   per-call dependency value, and the `GateStats` telemetry.

mod capture;
mod derive;
mod gate;
mod kernel;

pub use capture::{CapturedHop, DegeneratePathEvent, DegeneratePathSink};
pub use derive::{HopMath, PrefixCache};
// The CL family's gate view, re-exported at its historical path (the family
// module is its home; ADR-071 D1).
pub use crate::mixed::families::concentrated_liquidity::ConcentratedLiquidityHop;

pub(crate) use derive::content_mix_u256;
pub(crate) use gate::record_gate;

// The shared / residual envelope line builders the `FAMILY_DISPATCH` rows
// register (ADR-071 D3): the V2 constant-product builder, the gate-only
// `ReserveCap` pseudo-family, and the refusal / walkable stub. The per-family
// builders (weighted / balancer-stable / curve) now live in their family modules
// (`mixed::families::*`).
pub(crate) use derive::{refuse_lines, reserve_cap_hop_lines, v2_hop_lines};
// The shared constant-product rise builder: V2 and Solidly-volatile both call
// it, so the family modules reach it through this re-export.
pub(crate) use derive::mobius_lines;

#[cfg(test)]
pub(crate) use gate::path_bound_lines;
pub(crate) use gate::path_bound_lines_with_stats;
pub use gate::{
    path_output_bound_at, path_profit_bound, path_profit_bound_with_floor,
    path_profit_bound_with_stats, Envelope, GateDeps, GateSkipCause, GateStats, PathBoundLines,
};

pub use kernel::Line;
