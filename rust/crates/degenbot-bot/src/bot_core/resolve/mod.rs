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

pub(crate) mod cl;
pub(crate) mod solidly;

use degenbot_solvers::mixed::MixedPoolRef;

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
    /// The pool has fewer than two tokens; no pairwise hop can be formed.
    #[expect(
        dead_code,
        reason = "constructed in T2 (epic MKRKNB): Balancer/Curve arms"
    )]
    TooFewTokens,
    /// A variant byte (pow version, Curve y/d variant, ...) decoded to nothing.
    #[expect(
        dead_code,
        reason = "constructed in T2 (epic MKRKNB): Balancer/Curve arms"
    )]
    UnknownVariant,
    /// A pairwise index fell outside the token list.
    #[expect(
        dead_code,
        reason = "constructed in T2 (epic MKRKNB): Balancer/Curve arms"
    )]
    OutOfRange,
    /// `build_int_v*_sequence` returned `None` (no integer tick-range sequence
    /// for the direction, e.g. tick-range cache miss).
    SequenceUnavailable,
    /// The balancer-stable invariant calculation errored.
    #[expect(
        dead_code,
        reason = "constructed in T2 (epic MKRKNB): BalancerStable arm"
    )]
    InvariantError,
}

impl std::fmt::Display for MissingHopReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::MissingState => "missing pool state",
            Self::MissingIdentity => "missing pool identity",
            Self::MissingTokenPair => "missing token entry",
            Self::TooFewTokens => "pool has fewer than 2 tokens",
            Self::UnknownVariant => "unknown variant byte",
            Self::OutOfRange => "pairwise index out of range",
            Self::SequenceUnavailable => "integer tick-range sequence unavailable",
            Self::InvariantError => "stable invariant calculation failed",
        };
        f.write_str(s)
    }
}

/// Log a hop invalidation at `debug` (path context + hop index + reason).
pub(crate) fn log_invalidation(
    pool_ref: &MixedPoolRef,
    hop_index: usize,
    reason: MissingHopReason,
) {
    tracing::debug!(
        ?pool_ref,
        hop = hop_index,
        %reason,
        "[resolve-path] hop invalidates the path"
    );
}
