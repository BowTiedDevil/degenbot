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
//! `resolve_hops` collects EVERY failing hop (R522XA) — it does not stop at
//! the first `Err`. Prior successful hops still project; each failure is
//! returned as a `HopDeficit` so the path state machine can track every
//! responsible pool independently and clear it as it becomes suitable.
//! `valid` stays false whenever ANY hop failed. Reasons are logged at
//! `debug` so "why was this path rejected" is answerable on demand but
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

use std::collections::HashMap;
use std::sync::Arc;

use degenbot_solvers::mixed::{HopType, MixedPoolRef, ResolvedHop, ResolvedMixedPath};

use super::BotState;

/// A cached hop projection: either a successfully built [`ResolvedHop`] or
/// the terminal invalidation reason, tagged with the pool `state_nonce` it
/// was built against.
#[derive(Clone, Debug)]
pub(crate) enum CachedProjection {
    /// A built hop snapshot. Shared via `Arc` so N paths referencing the
    /// same (pool, direction) clone one allocation instead of re-walking.
    Hop(Arc<ResolvedHop>),
    /// The projection failed for this nonce (e.g. `SequenceUnavailable`).
    /// Cached too: the failed CL tick-walk is the expensive case, and live
    /// data shows roughly half of affected paths die here every cycle.
    Invalid(MissingHopReason),
}

impl CachedProjection {
    /// Re-materialize the projection into a path's hop list: an owned hop
    /// clone for `Ok`, the reason for an invalid entry. (Nonces are re-read
    /// from `core` by the caller — never trusted from a cached entry.)
    fn materialize(&self) -> Result<ResolvedHop, MissingHopReason> {
        match self {
            Self::Hop(arc) => Ok((**arc).clone()),
            Self::Invalid(reason) => Err(*reason),
        }
    }
}

/// Per-(pool, direction) hop-projection memo shared across paths and solve
/// cycles. Keyed by `(HopType, pool_key, zero_for_one)`; every entry carries
/// the `state_nonce` it was built against, so a stale entry is detected by
/// comparing against `core.pool_state_nonce` before reuse — correctness does
/// not depend on invalidation hooks (every state mutation bumps the nonce,
/// including reorg rollback).
///
/// Growth is bounded by the number of distinct (pool, direction) pairs ever
/// touched by resolution — the registered pool universe, not per-path work.
///
/// EXPIRY GRANULARITY (crossing-table cache review): CL crossing tables ride
/// inside the cached `ResolvedHop` and therefore share this whole-pool nonce
/// invalidation. A crossing table is prefix-cumulative
/// (`crossing_gross_input[k]` sums the gross inputs of ranges `0..k-1`), so a
/// single liquidity event can stale a SUFFIX of the table — not one isolated
/// tick entry. Whole-pool invalidation is the correct-safe granularity and also
/// the one every Mint/Burn/ModifyLiquidity already implements by bumping
/// `state_nonce`. A tick-keyed expiry map is deliberately NOT used: it would
/// need a nontrivial tick→affected-suffix index for a cache rebuilt in
/// microseconds, adding risk without a measured win.
pub(crate) type HopProjectionCache = HashMap<(HopType, u64, bool), (CachedProjection, u64)>;

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
    /// The pool carries an amount-modifying V4 hook: sims are caveated and
    /// paths through it are excluded from solving (ADR-037/X4EU3J).
    HookedPool,
    /// The pool carries an amount-modifying V4 hook: its sims are caveated
    /// (ADR-037/X4EU3J) and paths through it are excluded from solving.
    SequenceUnavailable,
    /// The balancer-stable invariant calculation errored.
    InvariantError,
    /// The pool cannot host a swap in this direction at its current state
    /// (directional viability gate — ported from the archived Python
    /// `swap_is_viable`; O(1), checked BEFORE any tick-range walk).
    NotViable,
}

/// One unprojectable hop inside a resolved path (R522XA): the failing pool
/// plus the reason. `resolve_hops` returns the FULL set (not just the first
/// failure) so the path state machine can clear each responsible pool
/// independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HopDeficit {
    /// The hop's pool family + key (identifies the responsible pool).
    pub hop_type: HopType,
    /// The hop's pool key within its family.
    pub pool_key: u64,
    /// Why this hop could not be projected.
    pub reason: MissingHopReason,
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
            Self::NotViable => "pool not viable in the swap direction",
            Self::HookedPool => "pool carries an amount-modifying V4 hook",
        };
        f.write_str(s)
    }
}

impl MissingHopReason {
    /// R522XA: structural/topology failures that can never self-heal from a
    /// pool going dirty — a misconfigured pool or path shape, not a state
    /// that can clear. A path with one must be REJECTED LOUDLY at
    /// construction (`register_path`), not stored as a state that could
    /// silently evade detection.
    #[must_use]
    pub(crate) const fn is_structurally_unroutable(self) -> bool {
        matches!(
            self,
            Self::TooFewTokens | Self::UnknownVariant | Self::OutOfRange
        )
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
///
/// Projections are MEMOIZED in `cache` keyed by `(pool_type, pool_key,
/// zero_for_one)` and validated against the pool's live `state_nonce`: a
/// hit re-clones the shared snapshot (no tick walk); a miss (first touch,
/// or any state mutation since — every mutation including reorg rollback
/// bumps the nonce) projects fresh and stores. The nonce comparison IS the
/// invalidation; there are no invalidation hooks to miss.
///
/// `projection_count` (optional probe) counts actual family projections —
/// the cache-miss metric separating "N paths re-walked one dirty pool"
/// from "one walk served N paths" (tests + solve-phase telemetry).
pub(crate) fn resolve_hops(
    core: &BotState,
    pool_refs: &[MixedPoolRef],
    resolved: &mut ResolvedMixedPath,
    cache: &mut HopProjectionCache,
    mut projection_count: Option<&mut u64>,
) -> Vec<HopDeficit> {
    resolved.hops.clear();
    resolved.valid = false;
    resolved.state_nonces.clear();

    // R522XA: a path with <2 hops is rejected loudly at `register_path`,
    // so reaching here is a programming error. Return no deficits rather than
    // fabricating a responsible pool that could never clear.
    if pool_refs.len() < 2 {
        tracing::error!(hops = pool_refs.len(), "resolve_hops: path has <2 hops");
        return Vec::new();
    }

    resolved.hops.reserve(pool_refs.len());
    resolved.state_nonces.reserve(pool_refs.len());
    let mut deficits: Vec<HopDeficit> = Vec::new();

    for (hop_index, pool_ref) in pool_refs.iter().enumerate() {
        // Capture the max price-clock `update_block` across all hops.
        resolved.max_update_block = resolved
            .max_update_block
            .max(core.pool_update_block(pool_ref.pool_key));

        let cache_key = (pool_ref.hop_type, pool_ref.pool_key, pool_ref.zero_for_one);
        let current_nonce = core.pool_state_nonce(pool_ref.pool_key);

        let cached_hit = match cache.get(&cache_key) {
            // Nonce unchanged since the entry was built → reuse it.
            Some((cached, built_nonce)) if *built_nonce == current_nonce => {
                Some(cached.materialize())
            }
            // Stale or absent — project fresh below.
            _ => None,
        };

        let projection = if let Some(replay) = cached_hit {
            replay.map(|hop| (hop, current_nonce))
        } else {
            if let Some(count) = projection_count.as_deref_mut() {
                *count += 1;
            }
            let projected = match pool_ref.hop_type {
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
            let entry = match &projected {
                Ok((hop, _nonce)) => CachedProjection::Hop(Arc::new(hop.clone())),
                Err(reason) => CachedProjection::Invalid(*reason),
            };
            cache.insert(cache_key, (entry, current_nonce));
            projected
        };

        match projection {
            Ok((hop, nonce)) => {
                resolved.hops.push(hop);
                resolved.state_nonces.push(nonce);
            }
            Err(reason) => {
                log_invalidation(pool_ref, hop_index, reason);
                // R522XA: CONTINUE past this failure — collect every failing
                // pool so the path state machine can clear them independently.
                deficits.push(HopDeficit {
                    hop_type: pool_ref.hop_type,
                    pool_key: pool_ref.pool_key,
                    reason,
                });
            }
        }
    }

    if deficits.is_empty() {
        resolved.valid = true;
    }
    deficits
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::collections::HashMap;
    use std::sync::Arc;

    use super::*;
    use crate::bot_core::{BotState, PoolTickCoverage, RegisterV3PoolParams, TickInfo};
    use alloy::primitives::{Address, I256, U128, U256};
    use degenbot_solvers::mixed::{HopType, MixedPoolRef, ResolvedHop, ResolvedMixedPath};
    use degenbot_solvers::mobius_v3_int::{IntTickRangeCrossing, V3WordProfile};

    fn ref_v3(pool_key: u64) -> MixedPoolRef {
        MixedPoolRef {
            hop_type: HopType::V3,
            pool_key,
            zero_for_one: true,
        }
    }

    fn register_v3(core: &mut BotState, addr: [u8; 20]) -> u64 {
        let mut t = HashMap::new();
        t.insert(
            120,
            TickInfo {
                liquidity_gross: U128::from(10_000),
                liquidity_net: I256::try_from(5_000i128).unwrap(),
                block: 0,
            },
        );
        t.insert(
            -120,
            TickInfo {
                liquidity_gross: U128::from(8_000),
                liquidity_net: I256::try_from(-4_000i128).unwrap(),
                block: 0,
            },
        );
        core.register_v3_pool(&RegisterV3PoolParams {
            address: Address::from(addr),
            token0: Address::from([0x30u8; 20]),
            token1: Address::from([0x31u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(79_228_162_514_264_337_593_543_950_336u128),
            liquidity: 10_000_000_000_000,
            tick: 0,
            tick_data: t,
            update_block: 42,
            tick_data_block: None,
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
            ..Default::default()
        })
        .expect("v3 registration")
    }

    /// The allocation pointer of the cached hop's word-boundary profile `Arc` -
    /// stable while the cache entry is unchanged, new after re-projection.
    fn profile_ptr(
        cache: &HopProjectionCache,
        key: &(HopType, u64, bool),
    ) -> *const Vec<Option<Arc<V3WordProfile>>> {
        match &cache[key] {
            (CachedProjection::Hop(arc), _) => match arc.as_ref() {
                ResolvedHop::V3 { word_profiles, .. } => Arc::as_ptr(word_profiles),
                _ => panic!("expected a cached V3 hop"),
            },
            (CachedProjection::Invalid(_), _) => panic!("expected a cached hop, got invalid"),
        }
    }

    /// The allocation pointer of the cached hop's crossing-table `Arc` -
    /// stable while the cache entry is unchanged, new after re-projection.
    fn crossing_ptr(
        cache: &HopProjectionCache,
        key: &(HopType, u64, bool),
    ) -> *const Vec<IntTickRangeCrossing> {
        match &cache[key] {
            (CachedProjection::Hop(arc), _) => match arc.as_ref() {
                ResolvedHop::V3 { crossing_table, .. } => Arc::as_ptr(crossing_table),
                _ => panic!("expected a cached V3 hop"),
            },
            (CachedProjection::Invalid(_), _) => panic!("expected a cached hop, got invalid"),
        }
    }

    /// Hop-projection memoization invariant: a liquidity event on one pool
    /// re-projects (rebuilds the profile for) ONLY that pool; every sibling
    /// pool's cached profile `Arc` is reused untouched. The nonce comparison is
    /// the invalidation, so nothing outside the modified pool is invalidated.
    #[test]
    fn memoization_reprojects_only_the_modified_pool() {
        let mut core = BotState::new();
        let p = register_v3(&mut core, [0xa1u8; 20]);
        let q = register_v3(&mut core, [0xb2u8; 20]);
        let refs = [ref_v3(p), ref_v3(q)];
        let mut cache = HopProjectionCache::new();

        // 1) First resolve: both pools project (their profile Arcs are built + cached).
        let mut r1 = ResolvedMixedPath::default();
        let mut pc = 0u64;
        assert!(
            resolve_hops(&core, &refs, &mut r1, &mut cache, Some(&mut pc)).is_empty(),
            "both pools project"
        );
        assert_eq!(pc, 2, "first resolve projects both pools");
        let p0 = profile_ptr(&cache, &(HopType::V3, p, true));
        let q0 = profile_ptr(&cache, &(HopType::V3, q, true));
        let p_cross0 = crossing_ptr(&cache, &(HopType::V3, p, true));
        let q_cross0 = crossing_ptr(&cache, &(HopType::V3, q, true));

        // 2) Re-resolve with no state change: both are cache hits - no re-projection,
        // both profile Arcs are the same allocations (reused, not rebuilt).
        let mut r2 = ResolvedMixedPath::default();
        let mut pc2 = 0u64;
        assert!(resolve_hops(&core, &refs, &mut r2, &mut cache, Some(&mut pc2)).is_empty());
        assert_eq!(pc2, 0, "unchanged pools are cache hits (no re-projection)");
        assert_eq!(
            profile_ptr(&cache, &(HopType::V3, p, true)),
            p0,
            "P profile Arc reused"
        );
        assert_eq!(
            profile_ptr(&cache, &(HopType::V3, q, true)),
            q0,
            "Q profile Arc reused"
        );
        assert_eq!(
            crossing_ptr(&cache, &(HopType::V3, p, true)),
            p_cross0,
            "P crossing-table Arc reused"
        );
        assert_eq!(
            crossing_ptr(&cache, &(HopType::V3, q, true)),
            q_cross0,
            "Q crossing-table Arc reused"
        );

        // 3) A real Mint/Burn on P ([low, high] straddling the current tick) changes
        // P's tick_data + active liquidity, bumping ONLY P's state_nonce.
        let p_nonce_before = core.pool_state_nonce(p);
        let q_nonce_before = core.pool_state_nonce(q);
        core.apply_v3_liquidity_update_by_pool_id(p, -120, 120, 1_000_000, 43)
            .expect("mint applied to P (pool_id path, no buffering)");
        assert_ne!(
            core.pool_state_nonce(p),
            p_nonce_before,
            "mint bumps P's state_nonce"
        );
        assert_eq!(
            core.pool_state_nonce(q),
            q_nonce_before,
            "sibling Q's nonce is untouched"
        );

        // 4) Re-resolve: only P is stale (nonce mismatch) so only P re-projects.
        // P's profile Arc is a fresh allocation; Q's is the SAME allocation.
        let mut r3 = ResolvedMixedPath::default();
        let mut pc3 = 0u64;
        assert!(resolve_hops(&core, &refs, &mut r3, &mut cache, Some(&mut pc3)).is_empty());
        assert_eq!(pc3, 1, "only the minted pool re-projects; Q is a cache hit");
        assert_ne!(
            profile_ptr(&cache, &(HopType::V3, p, true)),
            p0,
            "P's profile was rebuilt (new Arc allocation)"
        );
        assert_ne!(
            crossing_ptr(&cache, &(HopType::V3, p, true)),
            p_cross0,
            "P's crossing table was rebuilt (new Arc allocation)"
        );
        assert_eq!(
            profile_ptr(&cache, &(HopType::V3, q, true)),
            q0,
            "Q's cached profile Arc is untouched (nothing outside the modified range invalidates)"
        );
        assert_eq!(
            crossing_ptr(&cache, &(HopType::V3, q, true)),
            q_cross0,
            "Q's cached crossing-table Arc is untouched"
        );
    }
}
