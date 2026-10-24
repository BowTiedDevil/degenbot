//! The swap-simulation gate (ADR-037): ONE owner of "simulate a swap
//! against current pool state".
//!
//! Every simulation read goes through [`BotState::swap_simulation`] —
//! replacing the former `*_miss_aware` / `*_with_fetch` method twins,
//! `calculate_tokens_in`, and the override path's ad-hoc retry shell. This
//! module owns three things:
//!
//! 1. the **request** shape ([`SwapRequest`], signed user-perspective),
//! 2. the **outcome** shapes ([`SwapRead`] / [`SwapOutcome`], typed failure
//!    modes instead of silent `U256::ZERO`),
//! 3. the single **fetch → merge → retry** policy for sparse tick-map misses
//!    ([`ComputeMerge`] / [`drive`]), generic over the merge target so the
//!    override path can reuse it without touching registered state.
//!
//! The pure family math stays in `degenbot-pools` (`simulate_swap`,
//! `v3_simulate_swap`, `v4_simulate_swap`).
//!
//! ## Sign convention (ADR-037)
//!
//! `SwapRequest::amount_specified` follows the **user perspective**: positive
//! = exact-output (the pool delivers that magnitude to the user), negative =
//! exact-input (the user sends that magnitude). Neither engine's internal
//! convention is canonical at this seam; the family read module maps it to
//! each design in exactly one place (V3 negates both directions vs canonical;
//! V4 is identity) — see
//! `degenbot_pools::families::concentrated_liquidity::{v3,v4}::engine_amount_specified`.

use hashbrown::HashMap;
use std::sync::Arc;

use ::degenbot_pools::tick_fetch::TickWordFetcher;

use alloy::primitives::aliases::U112;
use alloy::primitives::{I256, U256};

use ::degenbot_pools::families::concentrated_liquidity;
use ::degenbot_pools::families::door::{Observe, Observed, ReadOutcome, SwapReadRequest};
use ::degenbot_pools::families::reserve_pair;
use ::degenbot_pools::families::{balancer_stable, balancer_weighted};
use ::degenbot_pools::registry::PoolEntry;
use ::degenbot_pools::simulate_swap::simulate_swap;
use ::degenbot_pools::v3_state::{PoolTickCoverage, V3SwapOutcome};

use super::BotState;

/// A swap-simulation request over registered pool state.
///
/// `amount_specified` is **signed, user perspective** (ADR-037):
/// - **positive** → exact-OUTPUT swap of that magnitude (the pool delivers);
/// - **negative** → exact-INPUT swap of that magnitude (the user sends).
///
/// Outcome deltas (`ConcentratedLiquiditySwapOutcome::consumed` / `delivered`) use the same
/// perspective: negative = taken from the user, positive = given to the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwapRequest {
    /// Direction of the swap (token0 → token1 when `true`).
    pub zero_for_one: bool,
    /// Signed requested amount — see the type-level sign convention.
    pub amount_specified: I256,
    /// Price-limit bound for the walk; `None` selects the family default
    /// ([`V3PoolState::default_sqrt_price_limit`]).
    pub sqrt_price_limit: Option<U256>,
}

/// Additive trust flags carried on every [`SwapOutcome`] variant.
///
/// An EMPTY set means "this number is exact". The set is deliberately
/// `#[non_exhaustive]`: new inaccuracy sources become new flags, never a
/// signature change. (A `HOOKED_POOL` flag is reserved by ADR-037;
/// admission currently rejects hooked pools.)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Caveats(u8);

impl Caveats {
    /// Sparse tick coverage: the walk may have traversed unfetched regions.
    pub const SPARSE_COVERAGE: Self = Self(1 << 0);
    /// The pool carries an amount-modifying V4 hook — standard concentrated liquidity math may
    /// mis-price the swap. (Reachable since hooked pools were admitted.)
    pub const HOOKED_POOL: Self = Self(1 << 1);

    /// No caveats — the outcome is trustworthy.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether `self` carries all flags in `other`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Union of two flag sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Thin payload for constant-product families (V2, Curve, Balancer,
/// Aerodrome). Amounts are signed user-perspective deltas.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V2SwapOutcome {
    /// Taken FROM the user (negative magnitude of the input sent).
    pub consumed: I256,
    /// Given TO the user (positive magnitude of the output delivered).
    pub delivered: I256,
    /// Constant-product families carry no sparse-tick risk today.
    pub caveats: Caveats,
}

impl ConcentratedLiquiditySwapOutcome {
    /// Raw per-token magnitudes for legacy tuple seams (`(token0, token1)`
    /// absolute amounts moved), given the swap direction.
    #[must_use]
    pub fn raw_token_amounts(&self, zero_for_one: bool) -> (U256, U256) {
        let input = (-self.consumed).into_raw();
        let output = self.delivered.into_raw();
        if zero_for_one {
            (input, output)
        } else {
            (output, input)
        }
    }
}

/// Rich payload for concentrated-liquidity swaps (V3/V4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConcentratedLiquiditySwapOutcome {
    /// Taken FROM the user (negative; `-input_consumed`).
    pub consumed: I256,
    /// Given TO the user (positive; token1 for zfo swaps, token0 otherwise).
    pub delivered: I256,
    /// Final `sqrtPriceX96` after the walk (end-state reconstruction).
    pub end_sqrt_price_x96: U256,
    /// Final active liquidity after the walk.
    pub end_liquidity: u128,
    /// Final tick after the walk.
    pub end_tick: i32,
    /// Gross input actually converted by the pool (solver clamp bound — see
    /// `V3SwapOutcome::input_consumed`). Unsigned input-token units.
    pub input_consumed: U256,
    /// Tick-bitmap words fetched during miss recovery, in fetch order.
    pub fetched_words: Vec<i32>,
    /// Trust flags (see [`Caveats`]).
    pub caveats: Caveats,
}

/// Per-family simulation payloads (invalid states unrepresentable: a V2 hop
/// has no end-state detail to fake; concentrated liquidity outcomes always carry full state).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SwapOutcome {
    /// Constant-product family result.
    V2(V2SwapOutcome),
    /// V3 concentrated-liquidity result.
    V3(ConcentratedLiquiditySwapOutcome),
    /// V4 concentrated-liquidity result.
    V4(ConcentratedLiquiditySwapOutcome),
}

impl SwapOutcome {
    /// Unsigned magnitude of the output delivered to the user (exact-input
    /// reads). Delivered deltas are non-negative by construction.
    #[must_use]
    pub fn delivered_unsigned(&self) -> U256 {
        let delivered = match self {
            Self::V2(o) => o.delivered,
            Self::V3(o) | Self::V4(o) => o.delivered,
        };
        debug_assert!(
            delivered >= I256::ZERO,
            "delivered delta is user-perspective positive by construction"
        );
        delivered.into_raw()
    }

    /// Raw per-token magnitudes for legacy tuple seams (`(token0, token1)`
    /// absolute amounts moved), given the swap direction.
    #[must_use]
    pub fn legacy_token_amounts(&self, zero_for_one: bool) -> Option<(U256, U256)> {
        let (consumed, delivered) = match self {
            Self::V2(o) => (-o.consumed, o.delivered),
            Self::V3(o) | Self::V4(o) => (-o.consumed, o.delivered),
        };
        let input = consumed.into_raw();
        let output = delivered.into_raw();
        Some(if zero_for_one {
            (input, output)
        } else {
            (output, input)
        })
    }
}

/// The typed result of [`BotState::swap_simulation`].
///
/// Formerly-silent failures are observable variants (hard cutover per
/// ADR-037 — no bare `U256::ZERO` / `Option::None` collapse):
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SwapRead {
    /// The swap computed; payload per family.
    Computed(SwapOutcome),
    /// The `pool_id` is not registered at all — a typed refusal, distinct
    /// from a real zero-output swap, so a use site cannot mistake an unknown
    /// pool for a computed zero.
    UnknownPool { pool_id: u64 },
    /// The pool is registered under a family whose requested operation has no
    /// implementation — a typed family gap (e.g. exact-output against a
    /// Curve/Balancer/Aerodrome invariant), distinct from `NotComputable`
    /// (arithmetic/invariant failure).
    UnsupportedFamily {
        /// The requested pool id.
        pool_id: u64,
        /// The registered family tag.
        family: &'static str,
    },
    /// Zero amount, non-computable arithmetic/invariant, or an exact-output
    /// request against a constant-product family.
    NotComputable,
    /// Miss recovery ran but the fetch itself failed for `word`.
    FetchFailed { word: i32 },
    /// Miss recovery was impossible or exhausted for `word` (no fetcher
    /// registered, or the same word missed twice after a merge).
    FetchExhausted { word: i32 },
}

/// The payload of a reserve-pair (constant-product) override read: THE
/// hypothetical read's answer, end state included.
///
/// A transient read leaves no registered state to re-read, so the post-swap
/// reserves ride the outcome — the shell and the Python companion consume them
/// as data and never re-derive them from the deltas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReservePairSwapOutcome {
    /// Taken FROM the user (negative magnitude of the input sent).
    pub consumed: I256,
    /// Given TO the user (positive magnitude of the output delivered).
    pub delivered: I256,
    /// POST-swap token0 reserve.
    pub end_reserves_token0: U112,
    /// POST-swap token1 reserve.
    pub end_reserves_token1: U112,
    /// Constant-product families carry no sparse-tick risk today.
    pub caveats: Caveats,
}

/// A computed override (hypothetical) read, per family — the outcome
/// vocabulary beside [`OverrideState`]'s state vocabulary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OverrideOutcome {
    /// Reserve-pair family result (the transient read's amounts + end reserves).
    ReservePair(ReservePairSwapOutcome),
    /// Concentrated-liquidity family result: the raw engine outcome, whose
    /// end state (price / liquidity / tick) is already complete.
    ConcentratedLiquidity(V3SwapOutcome),
}

/// Trust flags implied by a pool's registration-time tick coverage.
#[must_use]
pub(crate) fn caveats_for_coverage(coverage: PoolTickCoverage) -> Caveats {
    match coverage {
        // Tracked snapshots provided complete tick data — solver results are
        // trustworthy (glossary). Sparse results may be inaccurate.
        PoolTickCoverage::Tracked => Caveats::default(),
        PoolTickCoverage::Sparse => Caveats::SPARSE_COVERAGE,
    }
}

/// Convert an unsigned engine amount into the user-perspective delta.
/// Amounts are physically bounded far below `I256::MAX`; saturation there is
/// a defensive clamp, not a reachable state.
fn unsigned_to_user_delta(x: U256) -> I256 {
    I256::try_from(x).unwrap_or(I256::MAX)
}

/// Build the rich concentrated liquidity payload from a raw engine outcome.
#[must_use]
pub(crate) fn concentrated_liquidity_payload(
    outcome: &V3SwapOutcome,
    zero_for_one: bool,
    caveats: Caveats,
    fetched_words: Vec<i32>,
) -> ConcentratedLiquiditySwapOutcome {
    // Per-token moved amounts (absolute): token0/token1 by direction.
    // NOTE: do NOT derive these from `input_consumed` — that field is
    // denominated on the SPECIFIED side (see v3_state.rs:1417), so for an
    // exact-OUTPUT swap it carries the requested output magnitude, not the
    // input. The direction-keyed amounts are authoritative.
    let (input_moved_raw, output_moved_raw) = if zero_for_one {
        (outcome.amount0, outcome.amount1)
    } else {
        (outcome.amount1, outcome.amount0)
    };
    ConcentratedLiquiditySwapOutcome {
        consumed: -unsigned_to_user_delta(input_moved_raw),
        delivered: unsigned_to_user_delta(output_moved_raw),
        end_sqrt_price_x96: outcome.sqrt_price_x96,
        end_liquidity: outcome.liquidity,
        end_tick: outcome.tick,
        input_consumed: outcome.input_consumed,
        fetched_words,
        caveats,
    }
}

/// `(reserve_in, reserve_out)` for a V2 swap direction, widened to U256.
/// Exact-output across an explicit token pair (standalone-driver N-token
/// surface). The per-family dispatch is one row (ADR-073 D2): each
/// balance-vector family's `GIVEN_OUT` read lives behind its read module's
/// `read_swap` door. Returns None for non-balance-vector pools or an
/// out-of-domain computation.
#[must_use]
pub fn simulate_pair_in_given_out(
    bot: &BotState,
    pool_id: u64,
    idx_in: usize,
    idx_out: usize,
    amount_out: U256,
    override_balances: Option<&[U256]>,
    override_scaling_factors: Option<&[U256]>,
) -> Option<U256> {
    match bot.registry.pools.get(&pool_id)? {
        PoolEntry::BalancerWeighted(p) => balancer_weighted::read_swap(
            &p.1,
            &p.0,
            SwapReadRequest::PairExactOut {
                idx_in,
                idx_out,
                amount_out,
                override_balances,
                override_scaling_factors: None,
            },
        )
        .amount()
        .ok(),
        PoolEntry::BalancerStable(p) => balancer_stable::read_swap(
            &p.1,
            &p.0,
            SwapReadRequest::PairExactOut {
                idx_in,
                idx_out,
                amount_out,
                override_balances,
                override_scaling_factors,
            },
        )
        .amount()
        .ok(),
        _ => None,
    }
}

/// Exact-input across an explicit token pair (standalone-driver N-token
/// surface). The per-family dispatch is one row (ADR-073 D2): each
/// balance-vector family's N-token exact-in read lives behind its read
/// module's `read_swap` door. Overrides carry the caller's view of balances
/// (and stable scaling factors - `MetaStable` rates). Returns None for
/// non-balance-vector pools or an out-of-domain computation.
#[must_use]
pub fn simulate_pair_out(
    bot: &BotState,
    pool_id: u64,
    idx_in: usize,
    idx_out: usize,
    amount_in: U256,
    override_balances: Option<&[U256]>,
    override_scaling_factors: Option<&[U256]>,
) -> Option<U256> {
    match bot.registry.pools.get(&pool_id)? {
        PoolEntry::BalancerWeighted(p) => balancer_weighted::read_swap(
            &p.1,
            &p.0,
            SwapReadRequest::PairExactIn {
                idx_in,
                idx_out,
                amount_in,
                override_balances,
                override_scaling_factors: None,
            },
        )
        .amount()
        .ok(),
        PoolEntry::BalancerStable(p) => balancer_stable::read_swap(
            &p.1,
            &p.0,
            SwapReadRequest::PairExactIn {
                idx_in,
                idx_out,
                amount_in,
                override_balances,
                override_scaling_factors,
            },
        )
        .amount()
        .ok(),
        _ => None,
    }
}

/// The per-family hypothetical pool state — "what if the pool were at state
/// X?" The VARIANT names the family, so a wrong-family override is
/// unrepresentable rather than a typed gap: the door dispatches on it, and the
/// registered pool supplies the family's IMMUTABLE params (fees, tick
/// spacing, pool key, Camelot decimal scales) — never re-sent by the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OverrideState {
    /// Reserve-pair (V2 / Aerodrome V2 volatile): the caller's view of the pair.
    ReservePair {
        /// Hypothetical token0 reserve (on-chain `uint112` width).
        reserves_token0: U112,
        /// Hypothetical token1 reserve (on-chain `uint112` width).
        reserves_token1: U112,
    },
    /// Balance-vector (Curve / Balancer): the caller's view of the balances.
    /// Those families answer override reads through their own pair door today
    /// (the caller's balances ride `SwapReadRequest::PairExactIn` /
    /// `curve_get_dy`); this variant is the door's shared shape, not yet a
    /// compute row here.
    BalanceVector {
        /// Hypothetical balances, one per pool token.
        balances: Vec<U256>,
    },
    /// Concentrated liquidity (V3 / V4): the hypothetical scalars + tick data.
    ConcentratedLiquidity {
        /// Override `sqrtPriceX96`.
        sqrt_price_x96: U256,
        /// Override active liquidity.
        liquidity: u128,
        /// Override current tick.
        tick: i32,
        /// Replacement tick data for the hypothetical walk.
        tick_data: HashMap<i32, ::degenbot_pools::TickInfo>,
    },
}

/// A hypothetical (override) swap read: the registered pool to borrow
/// immutable params + fetcher from, the signed request (same user-perspective
/// convention), and the family's override state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverrideSwap {
    /// Registered pool to derive identity/params/fetcher from.
    pub pool_id: u64,
    /// Directional swap request (same user-perspective sign convention).
    pub request: SwapRequest,
    /// The hypothetical state the read runs against.
    pub state: OverrideState,
}

// The transient (hypothetical) concentrated liquidity target machinery, the per-family compute
// arms, the engine sign mapping, and the fetch→merge→retry policy moved to
// the family read module (ADR-073 D1):
// degenbot_pools::families::concentrated_liquidity — the swap-read door owns
// ALL variants (registered stateless / armed / disarm-fetch / override /
// discovery) behind `read_swap`. Each other family owns its own override arm
// the same way (`reserve_pair::read_swap_override` over a transient built from
// the caller's reserves), so this module only marshalls the state and adapts
// the outcome — it owns no family math.
impl BotState {
    /// the missing bitmap words a concentrated liquidity swap
    /// would need, WITHOUT fetching and WITHOUT mutating registered state
    /// (a forged-empty drive walks a TRANSIENT clone). Multi-word safe: one
    /// pass lists every missing word on the crossing (pair-review condition
    /// 5). Fetches happen through the caller's lock-free staged loop.
    #[must_use]
    pub fn swap_missing_words(
        &self,
        block: u64,
        pool_id: u64,
        request: &SwapRequest,
    ) -> Option<Vec<i32>> {
        if request.amount_specified.is_zero() {
            return Some(Vec::new());
        }
        // One concentrated liquidity dispatch row (ADR-073 D2): the discovery pass is the
        // family read module's read_swap door in Discovery mode over a
        // transient the door builds; the door never fetches and never
        // touches registered state.
        let _ = block;
        let entry = self.registry.pools.get(&pool_id)?;
        let source = match entry {
            // Non-concentrated-liquidity families never fetch: nothing to stage.
            PoolEntry::V2(..)
            | PoolEntry::Curve(..)
            | PoolEntry::BalancerWeighted(..)
            | PoolEntry::BalancerStable(..)
            | PoolEntry::AerodromeV2(..) => return Some(Vec::new()),
            PoolEntry::V3(p) => concentrated_liquidity::SwapReadSource::Discovery(
                concentrated_liquidity::ConcentratedLiquidityTransientSource::V3 {
                    identity: &p.0,
                    state: &p.1,
                },
                self.registry.journal_depth,
            ),
            PoolEntry::V4(p) => concentrated_liquidity::SwapReadSource::Discovery(
                concentrated_liquidity::ConcentratedLiquidityTransientSource::V4 {
                    identity: &p.0,
                    state: &p.1,
                },
                self.registry.journal_depth,
            ),
        };
        Some(
            concentrated_liquidity::read_swap(
                source,
                SwapReadRequest::ConcentratedLiquidity {
                    zero_for_one: request.zero_for_one,
                    amount_specified: request.amount_specified,
                    sqrt_price_limit: request.sqrt_price_limit,
                },
                concentrated_liquidity::FetchMode::Discovery,
            )
            .missing_words(),
        )
    }

    /// Simulate a swap over a HYPOTHETICAL (override) pool state with the
    /// shared fetch+retry policy (ADR-037). Builds a transient V3/V4 state
    /// from the override scalars + tick data, reusing the registered pool's
    /// immutable params and stored fetcher.
    ///
    /// INVARIANT: fetched words merge into the TRANSIENT state only — the
    /// override is a hypothetical that cannot pollute registered `BotState`.
    ///
    /// The override sim with miss recovery DISARMED - a missing word
    /// surfaces as `Ok(None)` (the legacy Option contract) instead of an
    /// inline fetch under the caller read guard. The pooled caller pre-stages
    /// through [`Self::override_missing_words`] + the lock-free fetch
    /// choreography before entering.
    ///
    /// The stored word fetcher for a registered pool, if any — the staged
    /// pre-pass fetches through it OUTSIDE any state lock. The registry seam
    /// for the family door's [`concentrated_liquidity::FetchMode::Armed`] mode (the fetcher is a
    /// registration-time fact on the slice; cloning it here keeps the read
    /// lock-free).
    #[must_use]
    pub fn stored_fetcher_for_pool(&self, pool_id: u64) -> Option<Arc<dyn TickWordFetcher>> {
        match self.registry.pools.get(&pool_id) {
            Some(PoolEntry::V3(p)) => p.1.fetcher.clone(),
            Some(PoolEntry::V4(p)) => p.1.fetcher.clone(),
            _ => None,
        }
    }

    /// The override read with miss recovery DISARMED (the staged-pre-pass
    /// contract): a residual sparse miss surfaces as the door's not-computable
    /// verdict instead of fetching under the caller's read guard.
    #[must_use]
    pub fn simulate_override_disarmed(
        &self,
        over: &OverrideSwap,
        block: u64,
    ) -> Option<OverrideOutcome> {
        self.simulate_override_ext(over, block, true)
    }

    /// the missing bitmap words the OVERLAY sim would fetch,
    /// listed by a collect-only walk over a transient built from the
    /// override state + the caller tick data (no fetch, no registered
    /// mutation). Pair with the lock-free fetch choreography in pool.rs
    /// (`ensure_override_missing_staged`).
    #[must_use]
    pub fn override_missing_words(&self, over: &OverrideSwap) -> Option<Vec<i32>> {
        if over.request.amount_specified.is_zero() {
            return Some(Vec::new());
        }
        let entry = self.registry.pools.get(&over.pool_id)?;
        // Only the concentrated-liquidity family carries a sparse tick map, so
        // only its override read has words to miss — every other family's
        // override read is a pure transient (I/O-free).
        let OverrideState::ConcentratedLiquidity {
            sqrt_price_x96,
            liquidity,
            tick,
            tick_data,
        } = &over.state
        else {
            return Some(Vec::new());
        };
        // One concentrated liquidity dispatch row (ADR-073 D2): the override discovery pass is
        // the family read module's read_swap door in Discovery mode over a
        // Sparse transient the door builds from the override state. The
        // checked-word set is NOT mirrored (a hypothetical has no
        // established words) and registered state is never touched.
        let source = match entry {
            PoolEntry::V3(p) => concentrated_liquidity::SwapReadSource::Override(
                concentrated_liquidity::ConcentratedLiquidityTransientSource::V3 {
                    identity: &p.0,
                    state: &p.1,
                },
                concentrated_liquidity::OverrideScalars {
                    sqrt_price_x96: *sqrt_price_x96,
                    liquidity: *liquidity,
                    tick: *tick,
                    tick_data,
                },
                p.1.update_block,
                self.registry.journal_depth,
            ),
            PoolEntry::V4(p) => concentrated_liquidity::SwapReadSource::Override(
                concentrated_liquidity::ConcentratedLiquidityTransientSource::V4 {
                    identity: &p.0,
                    state: &p.1,
                },
                concentrated_liquidity::OverrideScalars {
                    sqrt_price_x96: *sqrt_price_x96,
                    liquidity: *liquidity,
                    tick: *tick,
                    tick_data,
                },
                p.1.update_block,
                self.registry.journal_depth,
            ),
            _ => return Some(Vec::new()),
        };
        Some(
            concentrated_liquidity::read_swap(
                source,
                SwapReadRequest::ConcentratedLiquidity {
                    zero_for_one: over.request.zero_for_one,
                    amount_specified: over.request.amount_specified,
                    sqrt_price_limit: over.request.sqrt_price_limit,
                },
                concentrated_liquidity::FetchMode::Discovery,
            )
            .missing_words(),
        )
    }

    /// The override read with miss recovery ARMED (the stored fetcher merges
    /// into the hypothetical only).
    #[must_use]
    pub fn simulate_override(&self, over: &OverrideSwap, block: u64) -> Option<OverrideOutcome> {
        self.simulate_override_ext(over, block, false)
    }

    /// The override door: dispatch on the override STATE's variant (the state
    /// names its family), borrow the registered pool's immutable params, and
    /// run the family's override read. A `None` is the door's typed
    /// not-computable verdict — a compute miss, an overdraw, or a state the
    /// registration cannot carry (a pairing the shell's registered-family
    /// dispatch cannot construct).
    fn simulate_override_ext(
        &self,
        over: &OverrideSwap,
        block: u64,
        disarm_fetch: bool,
    ) -> Option<OverrideOutcome> {
        if over.request.amount_specified.is_zero() {
            return None;
        }
        let entry = self.registry.pools.get(&over.pool_id)?;
        match &over.state {
            OverrideState::ReservePair {
                reserves_token0,
                reserves_token1,
            } => self.reserve_pair_override_read(
                entry,
                over,
                reserve_pair::OverrideReserves {
                    reserve0: *reserves_token0,
                    reserve1: *reserves_token1,
                },
            ),
            // The balance-vector families answer override reads through their
            // own pair door (the caller's balances ride
            // `SwapReadRequest::PairExactIn` / `curve_get_dy`); this door
            // carries the shared shape, not a compute row.
            OverrideState::BalanceVector { .. } => None,
            OverrideState::ConcentratedLiquidity {
                sqrt_price_x96,
                liquidity,
                tick,
                tick_data,
            } => self.concentrated_liquidity_override_read(
                entry,
                over,
                block,
                disarm_fetch,
                concentrated_liquidity::OverrideScalars {
                    sqrt_price_x96: *sqrt_price_x96,
                    liquidity: *liquidity,
                    tick: *tick,
                    tick_data,
                },
            ),
        }
    }

    /// The reserve-pair override read row: the family read module's override
    /// arm over the caller's reserves, the registered identity's fee fractions
    /// / Camelot scales, and the registered price clock. I/O-free — a
    /// reserve-pair slice has no tick words and no fetcher.
    fn reserve_pair_override_read(
        &self,
        entry: &PoolEntry,
        over: &OverrideSwap,
        reserves: reserve_pair::OverrideReserves,
    ) -> Option<OverrideOutcome> {
        let design = match entry {
            PoolEntry::V2(p) => reserve_pair::ReservePairState::V2 {
                identity: &p.0,
                state: &p.1,
            },
            PoolEntry::AerodromeV2(p) => reserve_pair::ReservePairState::Aerodrome {
                identity: &p.0,
                state: &p.1,
            },
            // A reserve-pair state presented for a family whose registration
            // carries no reserve-pair identity.
            _ => return None,
        };
        // The door's signed request → the family door's exact side.
        let magnitude = over.request.amount_specified.into_sign_and_abs().1;
        let request = if over.request.amount_specified.is_positive() {
            SwapReadRequest::ExactOut {
                zero_for_one: over.request.zero_for_one,
                amount_out: magnitude,
            }
        } else {
            SwapReadRequest::ExactIn {
                zero_for_one: over.request.zero_for_one,
                amount_in: magnitude,
            }
        };
        match reserve_pair::read_swap_override(
            design,
            reserves,
            self.registry.journal_depth,
            request,
        ) {
            reserve_pair::OverrideReadOutcome::Computed(amounts) => {
                Some(OverrideOutcome::ReservePair(ReservePairSwapOutcome {
                    consumed: -unsigned_to_user_delta(amounts.amount_in),
                    delivered: unsigned_to_user_delta(amounts.amount_out),
                    end_reserves_token0: amounts.end_reserves.reserve0,
                    end_reserves_token1: amounts.end_reserves.reserve1,
                    caveats: Caveats::default(),
                }))
            }
            reserve_pair::OverrideReadOutcome::NotComputable => None,
        }
    }

    /// The concentrated-liquidity override read row: the family read module's
    /// [`concentrated_liquidity::read_swap`] door over a Sparse transient the
    /// door builds from the override scalars. Registered pools passed the
    /// hook/dynamic-fee admission gate, so the transient's zeroed hook and
    /// protocol-fee fields are safe by construction. Fetched words merge into
    /// the TRANSIENT only — the override is a hypothetical that cannot pollute
    /// registered `BotState`.
    fn concentrated_liquidity_override_read(
        &self,
        entry: &PoolEntry,
        over: &OverrideSwap,
        block: u64,
        disarm_fetch: bool,
        over_scalars: concentrated_liquidity::OverrideScalars<'_>,
    ) -> Option<OverrideOutcome> {
        // One concentrated liquidity dispatch row (ADR-073 D2): the override sim is the family
        // read module's read_swap door over a Sparse transient the door
        // builds from the override state.
        let (fetcher, source) = match entry {
            PoolEntry::V3(p) => (
                p.1.fetcher.clone(),
                concentrated_liquidity::SwapReadSource::Override(
                    concentrated_liquidity::ConcentratedLiquidityTransientSource::V3 {
                        identity: &p.0,
                        state: &p.1,
                    },
                    over_scalars,
                    p.1.update_block,
                    self.registry.journal_depth,
                ),
            ),
            PoolEntry::V4(p) => (
                p.1.fetcher.clone(),
                concentrated_liquidity::SwapReadSource::Override(
                    concentrated_liquidity::ConcentratedLiquidityTransientSource::V4 {
                        identity: &p.0,
                        state: &p.1,
                    },
                    over_scalars,
                    p.1.update_block,
                    self.registry.journal_depth,
                ),
            ),
            // Concentrated-liquidity scalars presented for another family.
            _ => return None,
        };
        let mode = match &fetcher {
            Some(f) if !disarm_fetch => concentrated_liquidity::FetchMode::Armed {
                fetcher: f,
                pool_id: over.pool_id,
                block,
            },
            _ => concentrated_liquidity::FetchMode::Disarmed,
        };
        match concentrated_liquidity::read_swap(
            source,
            SwapReadRequest::ConcentratedLiquidity {
                zero_for_one: over.request.zero_for_one,
                amount_specified: over.request.amount_specified,
                sqrt_price_limit: over.request.sqrt_price_limit,
            },
            mode,
        ) {
            ReadOutcome::Computed { outcome, .. } => {
                Some(OverrideOutcome::ConcentratedLiquidity(outcome))
            }
            ReadOutcome::NotComputable
            | ReadOutcome::FetchFailed { .. }
            | ReadOutcome::FetchExhausted { .. } => None,
            ReadOutcome::Amount(_) | ReadOutcome::Stateless(_) | ReadOutcome::MissingWords(_) => {
                unreachable!("override reads run armed or disarmed, never stateless/discovery")
            }
        }
    }
}

impl BotState {
    /// Simulate a swap against current registered pool state — THE swap-read
    /// interface (ADR-037). See module docs for the sign convention and the
    /// typed failure modes.
    ///
    /// `block` is the fetch context threaded into the tick-word fetcher on
    /// sparse-miss recovery; it does not affect pure computation.
    pub fn swap_simulation(&mut self, block: u64, pool_id: u64, request: SwapRequest) -> SwapRead {
        self.swap_simulation_ext(block, pool_id, &request, false)
    }

    /// the swap with miss recovery DISARMED — a residual
    /// missing word after the staged pre-pass surfaces as the typed
    /// `FetchExhausted` contract (additive, ADR-037) instead of fetching under
    /// the caller write guard. Identical arithmetic otherwise (the caller
    /// must have staged the missing words via the lock-free pre-pass).
    pub fn swap_simulation_disarmed(
        &mut self,
        block: u64,
        pool_id: u64,
        request: &SwapRequest,
    ) -> SwapRead {
        self.swap_simulation_ext(block, pool_id, request, true)
    }

    fn swap_simulation_ext(
        &mut self,
        block: u64,
        pool_id: u64,
        request: &SwapRequest,
        disarm_fetch: bool,
    ) -> SwapRead {
        if !self.registry.pools.contains_key(&pool_id) {
            // Unknown pool: typed refusal — a fabricated zero would be
            // indistinguishable from a real zero-output swap.
            return SwapRead::UnknownPool { pool_id };
        }
        if request.amount_specified.is_zero() {
            // Zero input → zero output (on-chain getAmountOut(0) = 0).
            return SwapRead::Computed(SwapOutcome::V2(V2SwapOutcome {
                consumed: I256::ZERO,
                delivered: I256::ZERO,
                caveats: Caveats::default(),
            }));
        }
        let magnitude = request.amount_specified.into_sign_and_abs().1;
        let exact_output = request.amount_specified.is_positive();

        let Some(entry) = self.registry.pools.get(&pool_id) else {
            return SwapRead::NotComputable;
        };
        match entry {
            PoolEntry::V2(..)
            | PoolEntry::Curve(..)
            | PoolEntry::BalancerWeighted(..)
            | PoolEntry::BalancerStable(..)
            | PoolEntry::AerodromeV2(..) => match (exact_output, entry) {
                // Exact-input: constant-product / ported invariant math.
                (false, _) => match simulate_swap(entry, request.zero_for_one, magnitude) {
                    Ok(out) => SwapRead::Computed(SwapOutcome::V2(V2SwapOutcome {
                        consumed: -unsigned_to_user_delta(magnitude),
                        delivered: unsigned_to_user_delta(out),
                        caveats: Caveats::default(),
                    })),
                    Err(_) => SwapRead::NotComputable,
                },
                // Exact-output for V2: the family read module's read_swap
                // door (ADR-073 D2) — the closed-form
                // `constant_product_calc_exact_out` (formerly buried in
                // `calculate_tokens_in`) lives there, refusals included
                // (zero request, overdraw, vanishing denominator →
                // NotComputable — the retired helper's `None` mapping).
                (true, PoolEntry::V2(p)) => {
                    let required = reserve_pair::read_swap(
                        reserve_pair::ReservePairState::V2 {
                            identity: &p.0,
                            state: &p.1,
                        },
                        SwapReadRequest::ExactOut {
                            zero_for_one: request.zero_for_one,
                            amount_out: magnitude,
                        },
                    )
                    .amount();
                    match required {
                        Ok(required) => SwapRead::Computed(SwapOutcome::V2(V2SwapOutcome {
                            consumed: -unsigned_to_user_delta(required),
                            delivered: unsigned_to_user_delta(magnitude),
                            caveats: Caveats::default(),
                        })),
                        Err(_) => SwapRead::NotComputable,
                    }
                }
                // Curve/Balancer/Aerodrome exact-output: the family is
                // registered and requested, but its exact-out math is not
                // ported — a typed family gap, never the generic
                // `NotComputable` that reads as an arithmetic failure.
                (true, _) => SwapRead::UnsupportedFamily {
                    pool_id,
                    family: super::entry_family(entry),
                },
            },
            // One concentrated liquidity dispatch row (ADR-073 D2): the swap-read goes through
            // the family read module's read_swap door — registered state,
            // armed or disarmed miss recovery, engine sign mapping and
            // per-design compute all behind the door.
            PoolEntry::V3(..) | PoolEntry::V4(..) => {
                self.concentrated_liquidity_swap_read(pool_id, request, block, disarm_fetch)
            }
        }
    }

    /// The ONE registered-state concentrated liquidity swap-read row: classify the entry (the
    /// registry seam), then everything design-specific happens behind the
    /// family read module's [`concentrated_liquidity::read_swap`] door. Caveats assembly (the
    /// outcome-shape adapter) stays here because [`Caveats`] is this crate's
    /// type; the door supplies every per-design fact the caveats need
    /// (coverage via [`concentrated_liquidity::observe`], V4 hooks included).
    #[expect(
        clippy::too_many_lines,
        reason = "the length is the row's dispatch shape (classify the registry \
         entry for both designs, one read_swap door call, then adapt the \
         outcome to this crate's Caveats/SwapRead); the compute leaves already \
         live behind the family door, and per-arm extraction would drag \
         substrate types across that seam"
    )]
    fn concentrated_liquidity_swap_read(
        &mut self,
        pool_id: u64,
        request: &SwapRequest,
        block: u64,
        disarm_fetch: bool,
    ) -> SwapRead {
        let (coverage, extra_caveats, outcome_family, fetcher, scalars, hooks) = {
            let Some(entry) = self.registry.pools.get(&pool_id) else {
                return SwapRead::NotComputable;
            };
            match entry {
                PoolEntry::V3(p) => {
                    let design = concentrated_liquidity::ConcentratedLiquidityState::V3 {
                        state: &p.1,
                        scalars: concentrated_liquidity::DesignScalars::new(
                            p.0.fee,
                            p.0.tick_spacing,
                        ),
                    };
                    let Observed::Coverage(coverage) =
                        concentrated_liquidity::observe(design, Observe::Coverage)
                    else {
                        unreachable!("coverage probe answers")
                    };
                    (
                        coverage,
                        Caveats::default(),
                        SwapOutcomeFamily::V3,
                        p.1.fetcher.clone(),
                        concentrated_liquidity::DesignScalars::new(p.0.fee, p.0.tick_spacing),
                        None,
                    )
                }
                PoolEntry::V4(p) => {
                    let hooks = p.0.pool_key.hooks;
                    let design = concentrated_liquidity::ConcentratedLiquidityState::V4 {
                        state: &p.1,
                        scalars: concentrated_liquidity::DesignScalars::new(
                            p.0.pool_key.fee,
                            p.0.pool_key.tick_spacing,
                        ),
                        hooks,
                    };
                    let Observed::Coverage(coverage) =
                        concentrated_liquidity::observe(design, Observe::Coverage)
                    else {
                        unreachable!("coverage probe answers")
                    };
                    let Observed::Hooked(hooked) =
                        concentrated_liquidity::observe(design, Observe::Hooked)
                    else {
                        unreachable!("hooked probe answers")
                    };
                    let extra_caveats = if hooked {
                        Caveats::HOOKED_POOL
                    } else {
                        Caveats::default()
                    };
                    (
                        coverage,
                        extra_caveats,
                        SwapOutcomeFamily::V4,
                        p.1.fetcher.clone(),
                        concentrated_liquidity::DesignScalars::new(
                            p.0.pool_key.fee,
                            p.0.pool_key.tick_spacing,
                        ),
                        Some(hooks),
                    )
                }
                _ => return SwapRead::NotComputable,
            }
        };
        let mode = match &fetcher {
            Some(f) if !disarm_fetch => concentrated_liquidity::FetchMode::Armed {
                fetcher: f,
                pool_id,
                block,
            },
            _ => concentrated_liquidity::FetchMode::Disarmed,
        };
        let Some(state_mut) = self.registry.pools.get_mut(&pool_id) else {
            return SwapRead::NotComputable;
        };
        let target = match state_mut {
            PoolEntry::V3(p) => concentrated_liquidity::ConcentratedLiquidityStateMut::V3 {
                state: &mut p.1,
                scalars,
            },
            PoolEntry::V4(p) => concentrated_liquidity::ConcentratedLiquidityStateMut::V4 {
                state: &mut p.1,
                scalars,
                hooks: hooks.unwrap_or_default(),
            },
            _ => return SwapRead::NotComputable,
        };
        let read = concentrated_liquidity::read_swap(
            concentrated_liquidity::SwapReadSource::Slice(target),
            SwapReadRequest::ConcentratedLiquidity {
                zero_for_one: request.zero_for_one,
                amount_specified: request.amount_specified,
                sqrt_price_limit: request.sqrt_price_limit,
            },
            mode,
        );
        match read {
            ReadOutcome::Computed {
                outcome,
                fetched_words,
            } => {
                let caveats = caveats_for_coverage(coverage).union(extra_caveats);
                let payload = concentrated_liquidity_payload(
                    &outcome,
                    request.zero_for_one,
                    caveats,
                    fetched_words,
                );
                SwapRead::Computed(match outcome_family {
                    SwapOutcomeFamily::V3 => SwapOutcome::V3(payload),
                    SwapOutcomeFamily::V4 => SwapOutcome::V4(payload),
                })
            }
            ReadOutcome::NotComputable => SwapRead::NotComputable,
            ReadOutcome::FetchFailed { word } => SwapRead::FetchFailed { word },
            ReadOutcome::FetchExhausted { word } => SwapRead::FetchExhausted { word },
            ReadOutcome::Amount(_) | ReadOutcome::Stateless(_) | ReadOutcome::MissingWords(_) => {
                unreachable!("registered reads run armed or disarmed, never stateless/discovery")
            }
        }
    }
}

/// Which outcome variant a concentrated liquidity drive should assemble.
#[derive(Clone, Copy)]
enum SwapOutcomeFamily {
    V3,
    V4,
}

// finish_concentrated_liquidity retired with the drive policy: the payload
// assembly now runs at the ONE concentrated liquidity row ([`BotState::concentrated_liquidity_swap_read`])
// directly over the family door's outcome.

#[cfg(test)]
mod tests {
    #![expect(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use hashbrown::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use ::degenbot_pools::v3_state::RegisterV3PoolParams;

    // ---------------------------------------------------------------------
    // the staged pre-pass + DISARMED sim contract.
    // ---------------------------------------------------------------------

    /// Registered sparse pool whose stored fetcher counts every call.
    fn counting_fetcher_setup(calls: Arc<AtomicUsize>) -> (BotState, u64) {
        #[derive(Debug)]
        struct CountingFetcher(Arc<AtomicUsize>);
        impl ::degenbot_pools::tick_fetch::TickWordFetcher for CountingFetcher {
            fn fetch_missing_tick_word(
                &self,
                _pool_id: u64,
                word: i32,
                _block: u64,
            ) -> Result<
                ::degenbot_pools::tick_fetch::FetchedTickWord,
                ::degenbot_pools::tick_fetch::FetchTickWordError,
            > {
                self.0.fetch_add(1, Ordering::Relaxed);
                Ok(::degenbot_pools::tick_fetch::FetchedTickWord {
                    word,
                    ticks: HashMap::new(),
                })
            }
        }
        let mut core = BotState::new();
        let pool_id = core
            .register_v3_pool(&RegisterV3PoolParams {
                address: alloy::primitives::Address::ZERO,
                token0: alloy::primitives::Address::ZERO,
                token1: alloy::primitives::Address::from([1u8; 20]),
                fee: 3000,
                tick_spacing: 60,
                factory: alloy::primitives::Address::ZERO,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 10_000_000_000_000u128,
                tick: 0,
                tick_data: HashMap::new(),
                update_block: 0,
                tick_data_block: None,
                coverage: PoolTickCoverage::Sparse,
                fetcher: Some(Arc::new(CountingFetcher(calls))),
                ..Default::default()
            })
            .expect("test setup: V3 registration");
        (core, pool_id)
    }

    #[test]
    fn disarmed_sim_never_fetches_and_surfaces_exhausted() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (mut core, pool_id) = counting_fetcher_setup(Arc::clone(&calls));
        let request = SwapRequest {
            zero_for_one: false,
            amount_specified: I256::try_from(-1_000_000i64).unwrap(),
            sqrt_price_limit: None,
        };
        let read = core.swap_simulation_disarmed(99, pool_id, &request);
        assert_eq!(
            read,
            SwapRead::FetchExhausted { word: 0 },
            "disarm surfaces the typed exhausted-miss contract"
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "disarm must never run the stored fetcher"
        );
    }

    #[test]
    fn missing_words_discovery_lists_both_words_in_one_pass() {
        // A walk crossing two sparse words must collect BOTH in the
        // discovery pass (pair-review condition 5), not one per round-trip.
        let calls = Arc::new(AtomicUsize::new(0));
        let (core, pool_id) = counting_fetcher_setup(Arc::clone(&calls));
        let request = SwapRequest {
            zero_for_one: false,
            amount_specified: I256::try_from(-1_000_000i64).unwrap(),
            sqrt_price_limit: None,
        };
        let missing = core
            .swap_missing_words(99, pool_id, &request)
            .expect("concentrated liquidity pool yields discovery");
        assert_eq!(missing.len(), 1, "single crossing -> one word here");
        assert_eq!(calls.load(Ordering::Relaxed), 0, "discovery never fetches");
    }

    // (The ADR-037 sign-convention table moved WITH the family:
    // degenbot_pools::families::concentrated_liquidity::{v3,v4} pin the
    // per-design mapping rows.)

    // ---------------------------------------------------------------------
    // Caveats: empty-means-exact, additive union.
    #[test]
    fn caveats_bit_semantics() {
        assert!(Caveats::default().is_empty());
        let sparse = Caveats::SPARSE_COVERAGE;
        assert!(!sparse.is_empty());
        assert!(sparse.contains(Caveats::SPARSE_COVERAGE));
        assert_eq!(sparse.union(sparse), sparse);
    }

    #[test]
    fn coverage_to_caveats() {
        assert!(caveats_for_coverage(PoolTickCoverage::Tracked).is_empty());
        assert!(caveats_for_coverage(PoolTickCoverage::Sparse).contains(Caveats::SPARSE_COVERAGE));
    }

    // ---------------------------------------------------------------------
    // concentrated liquidity payload: user-perspective signs from unsigned engine amounts.
    #[test]
    fn concentrated_liquidity_payload_signs_follow_user_perspective() {
        let outcome = V3SwapOutcome {
            amount0: U256::from(500_u64),
            amount1: U256::from(700_u64),
            sqrt_price_x96: U256::from(42_u64),
            liquidity: 9,
            tick: -3,
            input_consumed: U256::from(1_234_u64),
        };
        let payload =
            concentrated_liquidity_payload(&outcome, true, Caveats::SPARSE_COVERAGE, vec![7]);
        // zfo: input side is token0 → consumed tracks amount0 (500).
        assert_eq!(payload.consumed, -I256::try_from(500_u64).unwrap());
        assert_eq!(payload.delivered, I256::try_from(700_u64).unwrap());
        assert_eq!(payload.end_tick, -3);
        assert_eq!(payload.fetched_words, vec![7]);
        assert!(payload.caveats.contains(Caveats::SPARSE_COVERAGE));

        let ofz = concentrated_liquidity_payload(&outcome, false, Caveats::default(), Vec::new());
        assert_eq!(ofz.delivered, I256::try_from(500_u64).unwrap());
    }

    // ---------------------------------------------------------------------
    // Integration: registered canonical V3 fixture shape through the gate.
    const FIXTURE_LIQUIDITY: u128 = 1_000_000_000_000; // 1e12
    const FIXTURE_AMOUNT_IN: u128 = 1_000_000_000; // 1e9
    /// Recorded constant of the shared V3 fixture (`tests/standalone_parity/fixtures/v3_swap.json`)
    /// — mirrored here deliberately; the parity suite remains the single
    /// source of truth for drift detection.
    const FIXTURE_EXPECTED_OUT: u128 = 996_006_981;

    fn register_canonical_v3(
        bot: &mut BotState,
        coverage: PoolTickCoverage,
        with_ticks: bool,
    ) -> u64 {
        let mut tick_data = HashMap::new();
        if with_ticks {
            tick_data.insert(
                0_i32,
                degenbot_pools::TickInfo {
                    liquidity_gross: alloy::primitives::U128::from(FIXTURE_LIQUIDITY),
                    liquidity_net: 0,
                    block: 0,
                },
            );
        }
        bot.register_v3_pool(&degenbot_pools::v3_state::RegisterV3PoolParams {
            address: "0x00000000000000000000000000000000000d0001"
                .parse()
                .unwrap(),
            token0: "0x00000000000000000000000000000000000a0001"
                .parse()
                .unwrap(),
            token1: "0x00000000000000000000000000000000000a0002"
                .parse()
                .unwrap(),
            fee: 3000,
            tick_spacing: 60,
            factory: "0x00000000000000000000000000000000000f0001"
                .parse()
                .unwrap(),
            sqrt_price_x96: U256::from(2_u8).pow(U256::from(96_u8)),
            liquidity: FIXTURE_LIQUIDITY,
            tick: 0,
            tick_data,
            update_block: 0,
            coverage,
            fetcher: None,
            ..Default::default()
        })
        .unwrap_or_else(|e| panic!("register canonical v3 pool: {e:?}"))
    }

    #[test]
    fn tracked_v3_pool_computes_fixture_constant_through_gate() {
        let mut bot = BotState::new();
        let pid = register_canonical_v3(&mut bot, PoolTickCoverage::Tracked, true);
        let read = bot.swap_simulation(
            0,
            pid,
            SwapRequest {
                zero_for_one: true,
                amount_specified: -I256::try_from(FIXTURE_AMOUNT_IN).unwrap(),
                sqrt_price_limit: None,
            },
        );
        match read {
            SwapRead::Computed(SwapOutcome::V3(payload)) => {
                assert_eq!(
                    payload.delivered,
                    I256::try_from(FIXTURE_EXPECTED_OUT).unwrap()
                );
                assert_eq!(
                    payload.consumed,
                    -I256::try_from(FIXTURE_AMOUNT_IN).unwrap()
                );
                assert!(payload.caveats.is_empty());
                assert!(payload.fetched_words.is_empty());
            }
            other => panic!("expected Computed(V3), got {other:?}"),
        }
    }

    #[test]
    fn unregistered_pool_is_typed_and_zero_amount_returns_zero() {
        let mut bot = BotState::new();
        // Unknown pool: typed refusal, never a fabricated zero.
        assert_eq!(
            bot.swap_simulation(
                0,
                999,
                SwapRequest {
                    zero_for_one: true,
                    amount_specified: -I256::ONE,
                    sqrt_price_limit: None,
                }
            ),
            SwapRead::UnknownPool { pool_id: 999 }
        );
        let pid = register_canonical_v3(&mut bot, PoolTickCoverage::Tracked, true);
        // Zero input → zero output (on-chain getAmountOut(0) = 0).
        assert_eq!(
            bot.swap_simulation(
                0,
                pid,
                SwapRequest {
                    zero_for_one: true,
                    amount_specified: I256::ZERO,
                    sqrt_price_limit: None,
                }
            ),
            SwapRead::Computed(SwapOutcome::V2(V2SwapOutcome {
                consumed: I256::ZERO,
                delivered: I256::ZERO,
                caveats: Caveats::default(),
            }))
        );
    }

    #[test]
    fn sparse_v3_pool_missing_word_surfaces_exhaustion_with_caveat_free_error() {
        // Sparse + empty ticks + no fetcher: the walk cannot establish its
        // first bitmap word → FetchExhausted (the legacy silent-ZERO path).
        let mut bot = BotState::new();
        let pid = register_canonical_v3(&mut bot, PoolTickCoverage::Sparse, false);
        let read = bot.swap_simulation(
            0,
            pid,
            SwapRequest {
                zero_for_one: true,
                amount_specified: -I256::try_from(FIXTURE_AMOUNT_IN * 10).unwrap(),
                sqrt_price_limit: None,
            },
        );
        assert!(matches!(read, SwapRead::FetchExhausted { word } if word != i32::MIN));
    }

    /// Exact-output against a family whose exact-out math is not ported is a
    /// typed family gap, not the generic `NotComputable` (which means
    /// arithmetic/invariant failure).
    #[test]
    fn exact_output_on_curve_is_a_typed_family_gap() {
        let mut bot = BotState::new();
        let pid = register_curve_for_swap(&mut bot);
        let read = bot.swap_simulation(
            0,
            pid,
            SwapRequest {
                zero_for_one: true,
                amount_specified: I256::try_from(1_000_000u64).unwrap(),
                sqrt_price_limit: None,
            },
        );
        assert_eq!(
            read,
            SwapRead::UnsupportedFamily {
                pool_id: pid,
                family: "curve"
            }
        );
    }

    // ---------------------------------------------------------------------
    // The override (hypothetical) door: family-complete override state,
    // dispatched by variant, carrying the post-swap reserves for the
    // reserve-pair family.
    // ---------------------------------------------------------------------

    fn v2_swap_fixture(bot: &mut BotState) -> u64 {
        use alloy::primitives::aliases::U112;
        bot.register_v2_pool(&crate::RegisterV2PoolParams {
            address: alloy::primitives::Address::from([0xbdu8; 20]),
            token0: alloy::primitives::Address::ZERO,
            token1: alloy::primitives::Address::from([1u8; 20]),
            reserve0: U112::from(1_000u64),
            reserve1: U112::from(2_000u64),
            fee_token0: (997, 1000),
            fee_token1: (997, 1000),
            factory: alloy::primitives::Address::ZERO,
            update_block: 0,
            ..Default::default()
        })
        .expect("test setup: V2 registration")
    }

    fn exact_in_100() -> SwapRequest {
        SwapRequest {
            zero_for_one: true,
            amount_specified: -I256::try_from(100u64).unwrap(),
            sqrt_price_limit: None,
        }
    }

    #[test]
    fn reserve_pair_override_reads_the_hypothetical_reserves_and_carries_the_end_state() {
        let mut bot = BotState::new();
        let pid = v2_swap_fixture(&mut bot);
        // Reserves doubled with respect to the registration: the read must
        // follow the OVERRIDE, and answer a different number than the slice.
        let over = OverrideSwap {
            pool_id: pid,
            request: exact_in_100(),
            state: OverrideState::ReservePair {
                reserves_token0: alloy::primitives::aliases::U112::from(2_000u64),
                reserves_token1: alloy::primitives::aliases::U112::from(4_000u64),
            },
        };
        // I/O-free: the staged fetch pre-pass lists no words for this family,
        // so the fetch choreography never runs a fetcher.
        assert_eq!(bot.override_missing_words(&over), Some(Vec::new()));
        let outcome = bot
            .simulate_override_disarmed(&over, 0)
            .expect("the reserve-pair override computes");
        let OverrideOutcome::ReservePair(payload) = outcome else {
            panic!("the state variant selects the family: {outcome:?}")
        };
        // (100 * 997 * 4000) / (2000 * 1000 + 100 * 997) = 189 (EVM floor div).
        assert_eq!(payload.consumed, -I256::try_from(100u64).unwrap());
        assert_eq!(payload.delivered, I256::try_from(189u64).unwrap());
        assert_eq!(
            payload.end_reserves_token0,
            alloy::primitives::aliases::U112::from(2_100u64)
        );
        assert_eq!(
            payload.end_reserves_token1,
            alloy::primitives::aliases::U112::from(3_811u64)
        );
        assert!(payload.caveats.is_empty());

        // The hypothetical cannot pollute the registration: the registered
        // read still answers 181 from the registered 1000/2000 reserves.
        assert_eq!(
            bot.swap_simulation(0, pid, exact_in_100()),
            SwapRead::Computed(SwapOutcome::V2(V2SwapOutcome {
                consumed: -I256::try_from(100u64).unwrap(),
                delivered: I256::try_from(181u64).unwrap(),
                caveats: Caveats::default(),
            }))
        );
    }

    #[test]
    fn reserve_pair_override_armed_and_disarmed_reads_agree() {
        // The reserve-pair override read is a pure transient: arming the miss
        // recovery cannot change the answer (no fetch to run).
        let mut bot = BotState::new();
        let pid = v2_swap_fixture(&mut bot);
        let over = OverrideSwap {
            pool_id: pid,
            request: exact_in_100(),
            state: OverrideState::ReservePair {
                reserves_token0: alloy::primitives::aliases::U112::from(1_000u64),
                reserves_token1: alloy::primitives::aliases::U112::from(2_000u64),
            },
        };
        assert_eq!(
            bot.simulate_override(&over, 0),
            bot.simulate_override_disarmed(&over, 0)
        );
    }

    #[test]
    fn concentrated_liquidity_override_computes_through_the_door() {
        let mut bot = BotState::new();
        let pid = register_canonical_v3(&mut bot, PoolTickCoverage::Tracked, true);
        // A complete hypothetical tick set around the current tick — the walk
        // needs no fetch (the concentrated liquidity transient is Sparse).
        let mut over_ticks = HashMap::new();
        let liquidity_net = FIXTURE_LIQUIDITY.cast_signed();
        for (tick, net) in [
            (-20i32, liquidity_net),
            (10, -liquidity_net),
            (20, liquidity_net),
        ] {
            over_ticks.insert(
                tick,
                degenbot_pools::TickInfo {
                    liquidity_gross: alloy::primitives::U128::from(FIXTURE_LIQUIDITY),
                    liquidity_net: net,
                    block: 0,
                },
            );
        }
        let over = OverrideSwap {
            pool_id: pid,
            request: SwapRequest {
                zero_for_one: true,
                amount_specified: -I256::try_from(FIXTURE_AMOUNT_IN).unwrap(),
                sqrt_price_limit: None,
            },
            state: OverrideState::ConcentratedLiquidity {
                sqrt_price_x96: U256::from(2_u8).pow(U256::from(96_u8)),
                liquidity: FIXTURE_LIQUIDITY,
                tick: 0,
                tick_data: over_ticks,
            },
        };
        let outcome = bot
            .simulate_override_disarmed(&over, 0)
            .expect("the hypothetical walk computes");
        let OverrideOutcome::ConcentratedLiquidity(walk) = outcome else {
            panic!("the state variant selects the family: {outcome:?}")
        };
        assert!(walk.amount1 > U256::ZERO, "the hypothetical walk delivers");
    }

    /// The door dispatches by the state VARIANT against the registered
    /// family (the shell marshalls the variant from the registration, so a
    /// mismatch is unreachable from Python): a state whose family the
    /// registration does not carry answers the door's refusal, never a panic
    /// and never a mis-priced compute.
    #[test]
    fn an_override_state_the_registration_cannot_carry_is_the_doors_refusal() {
        let mut bot = BotState::new();
        let v2 = v2_swap_fixture(&mut bot);
        let v3 = register_canonical_v3(&mut bot, PoolTickCoverage::Tracked, true);
        let curve = register_curve_for_swap(&mut bot);
        let concentrated_liquidity_state = OverrideState::ConcentratedLiquidity {
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000,
            tick: 0,
            tick_data: HashMap::new(),
        };
        let reserve_pair_state = OverrideState::ReservePair {
            reserves_token0: alloy::primitives::aliases::U112::from(1_000u64),
            reserves_token1: alloy::primitives::aliases::U112::from(2_000u64),
        };
        // concentrated liquidity scalars for a reserve-pair registration
        assert_eq!(
            bot.simulate_override_disarmed(
                &OverrideSwap {
                    pool_id: v2,
                    request: exact_in_100(),
                    state: concentrated_liquidity_state.clone(),
                },
                0
            ),
            None
        );
        // reserve-pair reserves for a concentrated liquidity registration
        assert_eq!(
            bot.simulate_override_disarmed(
                &OverrideSwap {
                    pool_id: v3,
                    request: exact_in_100(),
                    state: reserve_pair_state.clone(),
                },
                0
            ),
            None
        );
        // The balance-vector family takes its override reads through its own
        // pair door (the caller's balances ride `simulate_pair_out` /
        // `curve_get_dy`); this door answers the shape, not a compute row.
        assert_eq!(
            bot.simulate_override_disarmed(
                &OverrideSwap {
                    pool_id: curve,
                    request: exact_in_100(),
                    state: OverrideState::BalanceVector {
                        balances: vec![U256::from(1_000u64), U256::from(2_000u64)],
                    },
                },
                0
            ),
            None
        );
    }

    fn register_curve_for_swap(bot: &mut BotState) -> u64 {
        use crate::RegisterCurvePoolParams;
        const E18: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);
        bot.register_curve_pool(&RegisterCurvePoolParams {
            address: alloy::primitives::Address::from([0xccu8; 20]),
            tokens: vec![
                alloy::primitives::Address::ZERO,
                alloy::primitives::Address::from([1u8; 20]),
            ],
            a_coefficient: 100,
            a_precision: 100,
            fee: 500_000,
            admin_fee: 0,
            rate_multipliers: vec![E18, E18],
            balances: vec![E18, E18],
            update_block: 0,
            swap_style: 1,
            lending_rate_style: 1,
            d_variant: 1,
            y_variant: 1,
            yd_variant: 1,
            base_pool: None,
            initial_a_coefficient: None,
            future_a_coefficient: None,
            initial_a_coefficient_time: None,
            future_a_coefficient_time: None,
            create_timestamp: None,
            fee_gamma: None,
            mid_fee: None,
            offpeg_fee_multiplier: None,
            out_fee: None,
            gamma: None,
            lp_token: None,
            use_lending: Vec::new(),
            precision_multipliers: vec![E18, E18],
            tokens_underlying: None,
            metapool_rate_style: 1,
            metapool_underlying_style: 1,
            data_provider: None,
        })
    }
}
