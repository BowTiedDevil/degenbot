//! Per-family resolve projections — the deepened internals of
//! [`ArbitrageEngine::resolve_path`](crate::solvers::arb_engine::ArbitrageEngine).
//!
//! One file per pool family exposing a free
//! `project_<family>(&BotState, &MixedPoolRef) -> Result<(ResolvedHop, u64),
//! MissingHopReason>` (u64 = the hop's state nonce). The projection is a pure
//! function of the locked [`BotState`] snapshot (ADR-003); it is internal to
//! `degenbot-bot` and never reached by `PyO3`. ADR-015 placement is unchanged —
//! the projection stays in degenbot-bot; only its internals moved here.
//!
//! Invalidation preserves the engine's mid-loop semantics exactly: the first
//! per-family `Err` stops the loop, prior successful hops remain pushed, and
//! `valid` stays false (the caller discards the path). The reason is logged
//! at `debug` so "why was this path rejected" is answerable on demand but
//! invisible in normal runs.
//!
//! CL guardrail: [`cl`] holds two SELF-CONTAINED entries. There is deliberately
//! no shared V3/V4 constructor — fee convention, current-tick drain framing,
//! and net-sign direction differ (CONTEXT.md "CL-projection guardrail"); the
//! shared surface is only the file + the thin `ResolvedHop` wrap + nonce
//! return.

pub(crate) mod balancer_stable;
pub(crate) mod balancer_weighted;
pub(crate) mod cl;
pub(crate) mod curve;
pub(crate) mod solidly;
pub(crate) mod v2;

use degenbot_solvers::mixed::{HopType, MixedPoolRef, ResolvedMixedPath};

use super::BotState;

/// Why a `project_<family>` hop could not be projected. Granular-but-grouped:
/// each variant maps 1:1 to a failure mode the flat match today encodes as a
/// bare `return`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MissingHopReason {
    /// The pool's state entry was missing from `BotState`.
    MissingState,
    /// The pool's identity entry was missing — including the Solidly
    /// Aerodrome/V2 fall-through (neither identity present).
    MissingIdentity,
    /// A token-registry (decimals) entry for one side of the pair was missing.
    MissingTokenPair,
    /// Fewer than two elements available: a pool with <2 tokens (no pairwise
    /// hop can be formed) or a path with <2 hops.
    TooFewTokens,
    /// A variant byte (pow version, Curve y/d variant, ...) decoded to nothing.
    UnknownVariant,
    /// A pairwise index fell outside the token list.
    OutOfRange,
    /// `build_int_v*_sequence` returned `None` (no integer tick-range sequence
    /// for the direction, e.g. tick-range cache miss).
    SequenceUnavailable,
    /// The balancer-stable invariant calculation errored.
    InvariantError,
}

impl std::fmt::Display for MissingHopReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::MissingState => "missing pool state",
            Self::MissingIdentity => "missing pool identity",
            Self::MissingTokenPair => "missing token entry",
            Self::TooFewTokens => "pool has fewer than 2 tokens, or path has fewer than 2 hops",
            Self::UnknownVariant => "unknown variant byte",
            Self::OutOfRange => "pairwise index out of range",
            Self::SequenceUnavailable => "integer tick-range sequence unavailable",
            Self::InvariantError => "stable invariant calculation failed",
        };
        f.write_str(s)
    }
}

/// Log a hop invalidation at `debug` (path context + hop index + reason).
fn log_invalidation(pool_ref: &MixedPoolRef, hop_index: usize, reason: MissingHopReason) {
    tracing::debug!(
        ?pool_ref,
        hop = hop_index,
        %reason,
        "[resolve-path] hop invalidates the path"
    );
}

/// The cross-family projection dispatcher — the body of the former flat
/// `#[expect(too_many_lines)]` `resolve_path` loop. Callers log the returned
/// reason at `debug` with the path context this signature deliberately does
/// not receive. Owns the accumulators (`max_update_block`,
/// `state_nonces`), the `valid` flag, and the stop-at-first-`Err`
/// invalidation; returns the `MissingHopReason` of the first unprojectable
/// hop (its hop-level detail also goes to `debug` via `log_invalidation`),
/// or `None` when the whole path projected.
pub(crate) fn resolve_hops(
    core: &BotState,
    pool_refs: &[MixedPoolRef],
    resolved: &mut ResolvedMixedPath,
) -> Option<MissingHopReason> {
    resolved.hops.clear();
    resolved.valid = false;
    resolved.state_nonces.clear();

    if pool_refs.len() < 2 {
        return Some(MissingHopReason::TooFewTokens);
    }

    resolved.hops.reserve(pool_refs.len());
    resolved.state_nonces.reserve(pool_refs.len());

    for (hop_index, pool_ref) in pool_refs.iter().enumerate() {
        // Capture the max price-clock `update_block` across all hops.
        resolved.max_update_block = resolved
            .max_update_block
            .max(core.pool_update_block(pool_ref.pool_key));
        let projection = match pool_ref.hop_type {
            HopType::V2 => v2::project_v2(core, pool_ref),
            HopType::V3 => cl::project_v3(core, pool_ref),
            HopType::V4 => cl::project_v4(core, pool_ref),
            HopType::SolidlyStable => solidly::project_solidly(core, pool_ref),
            HopType::BalancerWeighted => {
                balancer_weighted::project_balancer_weighted(core, pool_ref)
            }
            HopType::BalancerStable => balancer_stable::project_balancer_stable(core, pool_ref),
            HopType::CurveStableswap => curve::project_curve(core, pool_ref),
        };
        let (hop, nonce) = match projection {
            Ok(hop) => hop,
            Err(reason) => {
                log_invalidation(pool_ref, hop_index, reason);
                return Some(reason);
            }
        };
        resolved.hops.push(hop);
        resolved.state_nonces.push(nonce);
    }

    resolved.valid = true;
    None
}
