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
//! convention is canonical at this seam; [`engine_amount_specified`] maps to
//! each family in exactly one place (V3 negates both directions vs canonical;
//! V4 is identity).

use hashbrown::{HashMap, HashSet};
use std::sync::Arc;

use alloy::primitives::{I256, U256};

use ::degenbot_pools::registry::PoolEntry;
use ::degenbot_pools::simulate_swap::simulate_swap;
use ::degenbot_pools::tick_fetch::{FetchedTickWord, TickWordFetcher};
use ::degenbot_pools::v3_state::{
    v3_simulate_swap, PoolTickCoverage, RegisterV3PoolParams, SimulateSwapError, V3PoolIdentity,
    V3PoolState, V3SwapOutcome,
};
use ::degenbot_pools::v4_state::{
    v4_simulate_swap, RegisterV4PoolParams, V4PoolIdentity, V4PoolState,
};

use super::BotState;

/// A swap-simulation request over registered pool state.
///
/// `amount_specified` is **signed, user perspective** (ADR-037):
/// - **positive** → exact-OUTPUT swap of that magnitude (the pool delivers);
/// - **negative** → exact-INPUT swap of that magnitude (the user sends).
///
/// Outcome deltas (`ClSwapOutcome::consumed` / `delivered`) use the same
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
/// signature change. (A `HOOKED_POOL` flag is reserved by ADR-037 and wired
/// up by ergo task X4EU3J; admission currently rejects hooked pools.)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Caveats(u8);

impl Caveats {
    /// Sparse tick coverage: the walk may have traversed unfetched regions.
    pub const SPARSE_COVERAGE: Self = Self(1 << 0);
    /// The pool carries an amount-modifying V4 hook — standard CL math may
    /// mis-price the swap. (Reachable since X4EU3J admitted hooked pools.)
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

impl ClSwapOutcome {
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
pub struct ClSwapOutcome {
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
/// has no end-state detail to fake; CL outcomes always carry full state).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SwapOutcome {
    /// Constant-product family result.
    V2(V2SwapOutcome),
    /// V3 concentrated-liquidity result.
    V3(ClSwapOutcome),
    /// V4 concentrated-liquidity result.
    V4(ClSwapOutcome),
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
    /// Zero amount, unregistered pool, non-computable arithmetic/invariant,
    /// or an exact-output request against a constant-product family.
    NotComputable,
    /// Miss recovery ran but the fetch itself failed for `word`.
    FetchFailed { word: i32 },
    /// Miss recovery was impossible or exhausted for `word` (no fetcher
    /// registered, or the same word missed twice after a merge).
    FetchExhausted { word: i32 },
}

/// Which engine a mapped `amountSpecified` feeds (sign-convention table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EngineFamily {
    V3Engine,
    V4Engine,
}

/// Map a canonical user-perspective request amount onto an engine's
/// internal `amountSpecified` convention — THE single mapping site
/// (ADR-037): V3 negates both directions vs canonical; V4 is identity.
#[must_use]
pub(crate) fn engine_amount_specified(request_amount: I256, family: EngineFamily) -> I256 {
    match family {
        EngineFamily::V3Engine => -request_amount,
        EngineFamily::V4Engine => request_amount,
    }
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

/// Build the rich CL payload from a raw engine outcome.
#[must_use]
pub(crate) fn cl_payload(
    outcome: &V3SwapOutcome,
    zero_for_one: bool,
    caveats: Caveats,
    fetched_words: Vec<i32>,
) -> ClSwapOutcome {
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
    ClSwapOutcome {
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
fn v2_reserves(entry: &PoolEntry, zero_for_one: bool) -> Option<(U256, U256)> {
    match entry {
        PoolEntry::V2(p) => {
            let state = &p.1;
            let r0 = state.reserve0.to::<U256>();
            let r1 = state.reserve1.to::<U256>();
            Some(if zero_for_one { (r0, r1) } else { (r1, r0) })
        }
        _ => None,
    }
}

/// Closed-form `constant_product_calc_exact_out`: the input required so that
/// after fee, the pool's output-side reserve loses exactly `amount_out`.
///
/// Returns `None` on overdraw (`amount_out >= reserve_out`) or a vanishing
/// denominator — the two legacy ZERO-sentinel cases.
#[must_use]
fn v2_required_input(
    reserve_in: U256,
    reserve_out: U256,
    gamma_numer: u64,
    fee_denom: u64,
    amount_out: U256,
) -> Option<U256> {
    if amount_out.is_zero() || amount_out >= reserve_out {
        return None;
    }
    // amount_in = 1 + (reserve_in * amount_out * fee_denom)
    //                / ((reserve_out - amount_out) * gamma_numer)
    let numerator = U256::from(reserve_in)
        .saturating_mul(amount_out)
        .saturating_mul(U256::from(fee_denom));
    let denominator =
        (reserve_out.saturating_sub(amount_out)).saturating_mul(U256::from(gamma_numer));
    if denominator.is_zero() {
        return None;
    }
    Some(U256::from(1) + numerator / denominator)
}

/// Hypothetical (override) pool scalars: "what if the pool were at state X?"
/// The transient copy borrows the registered pool's immutable params
/// (`fee`, `tick_spacing`, `pool_key`) — only these four scalars differ.
#[derive(Clone, Debug)]
pub struct OverrideSwap {
    /// Registered pool to derive identity/params/fetcher from.
    pub pool_id: u64,
    /// Directional swap request (same user-perspective sign convention).
    pub request: SwapRequest,
    /// Override scalars.
    pub sqrt_price_x96: U256,
    pub liquidity: u128,
    pub tick: i32,
    /// Replacement tick data for the hypothetical walk.
    pub tick_data: HashMap<i32, ::degenbot_pools::TickInfo>,
}

/// Which CL family the transient target is.
#[derive(Clone, Copy, Debug)]
enum TransientFamily {
    V3(u32, i32), // fee, tick_spacing
    V4(u32, i32),
}

/// One transient (hypothetical) CL target owned by an override sim.
struct TransientCl {
    family: TransientFamily,
    inner: TransientInner,
}

enum TransientInner {
    V3(Box<::degenbot_pools::v3_state::V3PoolState>),
    V4(Box<::degenbot_pools::v4_state::V4PoolState>),
}

impl TransientCl {
    fn simulate(
        &self,
        zero_for_one: bool,
        spec: I256,
        limit: U256,
    ) -> Result<V3SwapOutcome, SimulateSwapError> {
        match (&self.family, &self.inner) {
            (TransientFamily::V3(fee, ts), TransientInner::V3(st)) => {
                v3_simulate_swap(st, *fee, *ts, zero_for_one, spec, limit)
            }
            (TransientFamily::V4(fee, ts), TransientInner::V4(st)) => {
                v4_simulate_swap(st, *fee, *ts, zero_for_one, spec, limit)
            }
            _ => unreachable!("family/inner mismatch is unconstructible"),
        }
    }

    fn merge_word(&mut self, fetched: &FetchedTickWord) {
        use ::degenbot_pools::registry::ConcentratedLiquidityPoolMut;
        match &mut self.inner {
            // Merges land on the TRANSIENT state only — the override is a
            // hypothetical that cannot pollute registered `BotState`.
            TransientInner::V3(st) => {
                st.merge_tick_word(fetched);
            }
            TransientInner::V4(st) => {
                st.merge_tick_word(fetched);
            }
        }
    }
}

/// `ComputeMerge` adapter over a transient target: identical policy, different
/// merge destination (this is WHY the policy is generic over its merge target).
struct OverrideSim<'a> {
    target: &'a mut TransientCl,
    zero_for_one: bool,
    spec: I256,
    limit: U256,
    fetcher: Option<Arc<dyn TickWordFetcher>>,
    pool_id: u64,
    /// RATR5A/CXRHW3: miss recovery DISARMED - a fresh missing word
    /// surfaces as `FetchExhausted` (typed contract) instead of an inline
    /// fetch under the caller read guard.
    disarm_fetch: bool,
}

impl ComputeMerge for OverrideSim<'_> {
    fn compute(&self) -> Result<V3SwapOutcome, SimulateSwapError> {
        self.target
            .simulate(self.zero_for_one, self.spec, self.limit)
    }
    fn merge_word(&mut self, fetched: &FetchedTickWord) {
        self.target.merge_word(fetched);
    }
    fn fetch_word(&self, word: i32, block: u64) -> Result<FetchedTickWord, FetchFailure> {
        if self.disarm_fetch {
            return Err(FetchFailure::NoFetcher);
        }
        let Some(fetcher) = &self.fetcher else {
            return Err(FetchFailure::NoFetcher);
        };
        fetcher
            .fetch_missing_tick_word(self.pool_id, word, block)
            .map_err(|_| FetchFailure::Errored)
    }
}

/// The policy driver's contract: recompute against a (possibly mutated)
/// target, merge fetched words into it, and surface its stored fetcher.
/// Splitting compute/merge/fetch across `&self` / `&mut self` methods keeps
/// borrows sequenced per call — the reentrancy discipline the old tripled
/// loops encoded by hand (fetcher cloned off the entry before looping).
pub(crate) trait ComputeMerge {
    /// One simulation attempt against the current target state.
    fn compute(&self) -> Result<V3SwapOutcome, SimulateSwapError>;
    /// Merge a fetched word into the target state (registered OR transient —
    /// the override path never merges into registered `BotState`).
    fn merge_word(&mut self, fetched: &FetchedTickWord);
    /// Attempt the RPC-side recovery for `word`. `Err(NoFetcher)` means no
    /// fetcher is registered (⇒ [`SwapRead::FetchExhausted`]); `Err(Errored)`
    /// means the recovery itself failed (⇒ [`SwapRead::FetchFailed`]).
    fn fetch_word(&self, word: i32, block: u64) -> Result<FetchedTickWord, FetchFailure>;
}

/// Why a missing-word recovery could not run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FetchFailure {
    /// No fetcher registered for this pool.
    NoFetcher,
    /// The fetcher ran and failed.
    Errored,
}

/// Internal drive result before payload assembly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PolicyAttempt {
    Computed(V3SwapOutcome, Vec<i32>),
    NotComputable,
    FetchFailed(i32),
    FetchExhausted(i32),
}

/// THE fetch → merge → retry loop (formerly copy-pasted three times).
///
/// Dedup rule: a second miss on the SAME word means the previous merge did
/// not satisfy the walker — give up rather than loop forever. No-fetcher
/// and exhausted-miss collapse to `FetchExhausted`; a fetcher error is
/// `FetchFailed`. All four legacy failure collapses are observable here.
pub(crate) fn drive(sim: &mut impl ComputeMerge, block: u64) -> PolicyAttempt {
    let mut attempted: HashSet<i32> = HashSet::new();
    let mut fetched_words: Vec<i32> = Vec::new();
    loop {
        match sim.compute() {
            Ok(outcome) => return PolicyAttempt::Computed(outcome, fetched_words),
            Err(SimulateSwapError::NotComputable) => return PolicyAttempt::NotComputable,
            Err(SimulateSwapError::MissingTickWord(word)) => {
                if !attempted.insert(word) {
                    return PolicyAttempt::FetchExhausted(word);
                }
                match sim.fetch_word(word, block) {
                    Ok(data) => {
                        sim.merge_word(&data);
                        fetched_words.push(word);
                    }
                    Err(FetchFailure::NoFetcher) => return PolicyAttempt::FetchExhausted(word),
                    Err(FetchFailure::Errored) => return PolicyAttempt::FetchFailed(word),
                }
            }
        }
    }
}

/// One simulation attempt against REGISTERED pool state (`pool_id`). The
/// compute re-resolves the entry each call so a mid-loop fetch-merge is
/// visible on the next attempt; borrows are sequenced through `&self` /
/// `&mut self` rather than competing closures.
pub(crate) struct RegisteredClSim<'a> {
    pub state: &'a mut BotState,
    pub pool_id: u64,
    pub zero_for_one: bool,
    /// Engine-mapped `amountSpecified` (see [`engine_amount_specified`]).
    pub spec: I256,
    pub limit: U256,
    /// RATR5A/CXRHW3: when set, miss recovery is DISARMED — a fresh missing
    /// word surfaces as `FetchExhausted` (additive, in-contract) instead of
    /// fetching under the caller's write guard. The python seams arm the
    /// staged pre-pass instead and enter this arm with words preinstalled.
    pub disarm_fetch: bool,
}

impl ComputeMerge for RegisteredClSim<'_> {
    fn compute(&self) -> Result<V3SwapOutcome, SimulateSwapError> {
        let Some(entry) = self.state.pools.get(&self.pool_id) else {
            return Err(SimulateSwapError::NotComputable);
        };
        match entry {
            PoolEntry::V3(p) => v3_simulate_swap(
                &p.1,
                p.0.fee,
                p.0.tick_spacing,
                self.zero_for_one,
                self.spec,
                self.limit,
            ),
            PoolEntry::V4(p) => v4_simulate_swap(
                &p.1,
                p.0.pool_key.fee,
                p.0.pool_key.tick_spacing,
                self.zero_for_one,
                self.spec,
                self.limit,
            ),
            _ => Err(SimulateSwapError::NotComputable),
        }
    }

    fn merge_word(&mut self, fetched: &FetchedTickWord) {
        self.state.merge_tick_word(self.pool_id, fetched);
    }

    fn fetch_word(&self, word: i32, block: u64) -> Result<FetchedTickWord, FetchFailure> {
        // RATR5A/CXRHW3: disarmed sims never fetch — their callers staged
        // the missing words beforehand (lock-free) and the typed contract
        // answers FetchExhausted for any residual miss.
        if self.disarm_fetch {
            return Err(FetchFailure::NoFetcher);
        }
        // Clone the stored fetcher off the registered V3/V4 state BEFORE any
        // mutation (avoids the self-referential borrow hazard the legacy
        // loops documented at bot_core/mod.rs:811).
        let fetcher: Option<Arc<dyn TickWordFetcher>> = match self.state.pools.get(&self.pool_id) {
            Some(PoolEntry::V3(p)) => p.1.fetcher.clone(),
            Some(PoolEntry::V4(p)) => p.1.fetcher.clone(),
            _ => None,
        };
        let Some(fetcher) = fetcher else {
            return Err(FetchFailure::NoFetcher);
        };
        fetcher
            .fetch_missing_tick_word(self.pool_id, word, block)
            .map_err(|_| FetchFailure::Errored)
    }
}

impl BotState {
    /// RATR5A/CXRHW3 discovery pass: the missing bitmap words a CL swap
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
        let entry = self.pools.get(&pool_id)?;
        match entry {
            // Non-CL families never fetch: nothing to stage.
            PoolEntry::V2(..)
            | PoolEntry::Curve(..)
            | PoolEntry::BalancerWeighted(..)
            | PoolEntry::BalancerStable(..)
            | PoolEntry::AerodromeV2(..) => Some(Vec::new()),
            PoolEntry::V3(p) => {
                let (identity, st) = (&p.0, &p.1);
                let mut missing = Vec::new();
                let mut transient = self.v3_discovery_transient(identity, st);
                let spec =
                    engine_amount_specified(request.amount_specified, EngineFamily::V3Engine);
                let limit = request
                    .sqrt_price_limit
                    .unwrap_or_else(|| V3PoolState::default_sqrt_price_limit(request.zero_for_one));
                Self::drive_missing(
                    &mut transient,
                    request.zero_for_one,
                    spec,
                    limit,
                    &mut missing,
                );
                Some(missing)
            }
            PoolEntry::V4(p) => {
                let (identity, st) = (&p.0, &p.1);
                let mut missing = Vec::new();
                let mut transient = self.v4_discovery_transient(identity, st);
                let spec =
                    engine_amount_specified(request.amount_specified, EngineFamily::V4Engine);
                let limit = request
                    .sqrt_price_limit
                    .unwrap_or_else(|| V3PoolState::default_sqrt_price_limit(request.zero_for_one));
                let _ = block;
                Self::drive_missing(
                    &mut transient,
                    request.zero_for_one,
                    spec,
                    limit,
                    &mut missing,
                );
                Some(missing)
            }
        }
    }

    /// Build the V3 discovery transient for a registered pool: it mirrors
    /// the REGISTERED pool's coverage (a Tracked pool never raises
    /// `MissingTickWord`, so the staging walk must not invent fetch work
    /// for one) AND its checked-word set (T1 3WTDFK: `from_params` seeds
    /// known words from tick ROWS only — a caller-CHECKED empty word has no
    /// rows yet must never become a fetch target).
    fn v3_discovery_transient(&self, identity: &V3PoolIdentity, st: &V3PoolState) -> TransientCl {
        let mut v3_transient = V3PoolState::from_params(
            RegisterV3PoolParams {
                address: identity.address,
                token0: identity.token0,
                token1: identity.token1,
                fee: identity.fee,
                tick_spacing: identity.tick_spacing,
                factory: identity.factory,
                deployer: identity.deployer,
                init_hash: identity.init_hash,
                sqrt_price_x96: st.sqrt_price_x96,
                liquidity: st.liquidity,
                tick: st.tick,
                tick_data: st.tick_data.clone(),
                update_block: st.update_block,
                coverage: st.coverage,
                fetcher: None,
                ..Default::default()
            },
            self.journal_depth,
        )
        .1;
        v3_transient
            .known_bitmap_words
            .extend(st.known_bitmap_words.iter().copied());
        TransientCl {
            family: TransientFamily::V3(identity.fee, identity.tick_spacing),
            inner: TransientInner::V3(Box::new(v3_transient)),
        }
    }

    /// V4 twin of [`Self::v3_discovery_transient`] — the same coverage +
    /// checked-word mirroring discipline (see that doc).
    fn v4_discovery_transient(&self, identity: &V4PoolIdentity, st: &V4PoolState) -> TransientCl {
        let mut v4_transient = V4PoolState::from_params(
            RegisterV4PoolParams {
                pool_manager: identity.pool_manager,
                pool_id: identity.pool_id,
                pool_key: identity.pool_key.clone(),
                hook_flags: 0,
                protocol_fee: 0,
                sqrt_price_x96: st.sqrt_price_x96,
                liquidity: st.liquidity,
                tick: st.tick,
                tick_data: st.tick_data.clone(),
                update_block: st.update_block,
                tick_data_block: None,
                coverage: st.coverage,
                fetcher: None,
            },
            self.journal_depth,
        )
        .1;
        v4_transient
            .known_bitmap_words
            .extend(st.known_bitmap_words.iter().copied());
        TransientCl {
            family: TransientFamily::V4(identity.pool_key.fee, identity.pool_key.tick_spacing),
            inner: TransientInner::V4(Box::new(v4_transient)),
        }
    }

    /// The stored word fetcher for a registered pool, if any — the staged
    /// pre-pass fetches through it OUTSIDE any state lock.
    #[must_use]
    pub fn stored_fetcher_for_pool(&self, pool_id: u64) -> Option<Arc<dyn TickWordFetcher>> {
        match self.pools.get(&pool_id) {
            Some(PoolEntry::V3(p)) => p.1.fetcher.clone(),
            Some(PoolEntry::V4(p)) => p.1.fetcher.clone(),
            _ => None,
        }
    }

    /// RATR5A/CXRHW3 discovery drive: one compute-walk over a TRANSIENT pool
    /// state recording every missing word instead of fetching. Forged empty
    /// fills land on the TRANSIENT (never registered state), so the walker
    /// steps past each discovered word and surfaces ALL of a pass missing
    /// words (pair-review condition 5).
    fn drive_missing(
        target: &mut TransientCl,
        zero_for_one: bool,
        spec: I256,
        limit: U256,
        missing: &mut Vec<i32>,
    ) {
        let mut attempted: HashSet<i32> = HashSet::new();
        loop {
            match target.simulate(zero_for_one, spec, limit) {
                Ok(_) | Err(SimulateSwapError::NotComputable) => return,
                Err(SimulateSwapError::MissingTickWord(word)) => {
                    if !attempted.insert(word) {
                        return;
                    }
                    if !missing.contains(&word) {
                        missing.push(word);
                    }
                    // Forged empty fill on the TRANSIENT: the walker steps
                    // past the word; registered state is untouched.
                    target.merge_word(&FetchedTickWord {
                        word,
                        ticks: HashMap::new(),
                    });
                }
            }
        }
    }

    /// Simulate a swap over a HYPOTHETICAL (override) pool state with the
    /// shared fetch+retry policy (ADR-037). Builds a transient V3/V4 state
    /// from the override scalars + tick data, reusing the registered pool's
    /// immutable params and stored fetcher.
    ///
    /// INVARIANT: fetched words merge into the TRANSIENT state only — the
    /// override is a hypothetical that cannot pollute registered `BotState`.
    ///
    /// Legacy note: returns `Option<V3SwapOutcome>` (None on any failure)
    /// to keep the `PyO3` seam byte-stable; the typed outcome arrives when the
    /// driver layer adopts it.
    #[must_use]
    /// RATR5A/CXRHW3: the override sim with miss recovery DISARMED - a
    /// missing word surfaces as None (the legacy Option contract) instead of
    /// an inline fetch under the caller read guard. The pooled caller
    /// pre-stages through [`Self::override_missing_words`] + the lock-free
    /// fetch choreography before entering.
    pub fn simulate_override_disarmed(
        &self,
        over: &OverrideSwap,
        block: u64,
    ) -> Option<V3SwapOutcome> {
        self.simulate_override_ext(over, block, true)
    }

    /// RATR5A/CXRHW3: the missing bitmap words the OVERLAY sim would fetch,
    /// listed by a collect-only walk over a transient built from the
    /// override scalars + the caller tick data (no fetch, no registered
    /// mutation). Pair with the lock-free fetch choreography in pool.rs
    /// (`ensure_override_missing_staged`).
    #[must_use]
    pub fn override_missing_words(&self, over: &OverrideSwap) -> Option<Vec<i32>> {
        if over.request.amount_specified.is_zero() {
            return Some(Vec::new());
        }
        let entry = self.pools.get(&over.pool_id)?;
        match entry {
            PoolEntry::V3(p) => {
                let (identity, state) = (&p.0, &p.1);
                let mut missing = Vec::new();
                let mut transient = TransientCl {
                    family: TransientFamily::V3(identity.fee, identity.tick_spacing),
                    inner: TransientInner::V3(Box::new(
                        V3PoolState::from_params(
                            RegisterV3PoolParams {
                                address: identity.address,
                                token0: identity.token0,
                                token1: identity.token1,
                                fee: identity.fee,
                                tick_spacing: identity.tick_spacing,
                                factory: identity.factory,
                                deployer: identity.deployer,
                                init_hash: identity.init_hash,
                                sqrt_price_x96: over.sqrt_price_x96,
                                liquidity: over.liquidity,
                                tick: over.tick,
                                tick_data: over.tick_data.clone(),
                                update_block: state.update_block,
                                coverage: PoolTickCoverage::Sparse,
                                fetcher: None,
                                ..Default::default()
                            },
                            self.journal_depth,
                        )
                        .1,
                    )),
                };
                let spec =
                    engine_amount_specified(over.request.amount_specified, EngineFamily::V3Engine);
                let limit = over.request.sqrt_price_limit.unwrap_or_else(|| {
                    V3PoolState::default_sqrt_price_limit(over.request.zero_for_one)
                });
                Self::drive_missing(
                    &mut transient,
                    over.request.zero_for_one,
                    spec,
                    limit,
                    &mut missing,
                );
                Some(missing)
            }
            PoolEntry::V4(p) => {
                let (identity, state) = (&p.0, &p.1);
                let mut missing = Vec::new();
                let mut transient = TransientCl {
                    family: TransientFamily::V4(
                        identity.pool_key.fee,
                        identity.pool_key.tick_spacing,
                    ),
                    inner: TransientInner::V4(Box::new(
                        V4PoolState::from_params(
                            RegisterV4PoolParams {
                                pool_manager: identity.pool_manager,
                                pool_id: identity.pool_id,
                                pool_key: identity.pool_key.clone(),
                                hook_flags: 0,
                                protocol_fee: 0,
                                sqrt_price_x96: over.sqrt_price_x96,
                                liquidity: over.liquidity,
                                tick: over.tick,
                                tick_data: over.tick_data.clone(),
                                update_block: state.update_block,
                                tick_data_block: None,
                                coverage: PoolTickCoverage::Sparse,
                                fetcher: None,
                            },
                            self.journal_depth,
                        )
                        .1,
                    )),
                };
                let spec =
                    engine_amount_specified(over.request.amount_specified, EngineFamily::V4Engine);
                let limit = over.request.sqrt_price_limit.unwrap_or_else(|| {
                    V3PoolState::default_sqrt_price_limit(over.request.zero_for_one)
                });
                Self::drive_missing(
                    &mut transient,
                    over.request.zero_for_one,
                    spec,
                    limit,
                    &mut missing,
                );
                Some(missing)
            }
            _ => Some(Vec::new()),
        }
    }

    #[must_use]
    pub fn simulate_override(&self, over: &OverrideSwap, block: u64) -> Option<V3SwapOutcome> {
        self.simulate_override_ext(over, block, false)
    }

    fn simulate_override_ext(
        &self,
        over: &OverrideSwap,
        block: u64,
        disarm_fetch: bool,
    ) -> Option<V3SwapOutcome> {
        if over.request.amount_specified.is_zero() {
            return None;
        }
        let entry = self.pools.get(&over.pool_id)?;
        let spec = I256::try_from(over.request.amount_specified).ok()?;
        // Clone the stored fetcher off the registered state (the override
        // state is a transient copy — the fetcher itself is shared via Arc).
        let fetcher: Option<Arc<dyn TickWordFetcher>> = match entry {
            PoolEntry::V3(p) => p.1.fetcher.clone(),
            PoolEntry::V4(p) => p.1.fetcher.clone(),
            _ => None,
        };
        let limit = over
            .request
            .sqrt_price_limit
            .unwrap_or_else(|| V3PoolState::default_sqrt_price_limit(over.request.zero_for_one));
        let spec = engine_amount_specified(
            spec,
            match entry {
                PoolEntry::V3(..) => EngineFamily::V3Engine,
                PoolEntry::V4(..) => EngineFamily::V4Engine,
                _ => return None,
            },
        );

        // Build the transient target (registered params + override scalars).
        // Registered pools passed the hook/dynamic-fee admission gate, so
        // zeroing hook_flags on the transient copy is safe by construction.
        let mut target = match entry {
            PoolEntry::V3(p) => {
                let (identity, state) = (&p.0, &p.1);
                let params = RegisterV3PoolParams {
                    address: identity.address,
                    token0: identity.token0,
                    token1: identity.token1,
                    fee: identity.fee,
                    tick_spacing: identity.tick_spacing,
                    factory: identity.factory,
                    deployer: identity.deployer,
                    init_hash: identity.init_hash,
                    sqrt_price_x96: over.sqrt_price_x96,
                    liquidity: over.liquidity,
                    tick: over.tick,
                    tick_data: over.tick_data.clone(),
                    update_block: state.update_block,
                    coverage: PoolTickCoverage::Sparse,
                    fetcher: None,
                    ..Default::default()
                };
                let (_id, st) = V3PoolState::from_params(params, self.journal_depth);
                TransientCl {
                    family: TransientFamily::V3(identity.fee, identity.tick_spacing),
                    inner: TransientInner::V3(Box::new(st)),
                }
            }
            PoolEntry::V4(p) => {
                let (identity, state) = (&p.0, &p.1);
                let params = RegisterV4PoolParams {
                    pool_manager: identity.pool_manager,
                    pool_id: identity.pool_id,
                    pool_key: identity.pool_key.clone(),
                    hook_flags: 0,
                    protocol_fee: 0,
                    sqrt_price_x96: over.sqrt_price_x96,
                    liquidity: over.liquidity,
                    tick: over.tick,
                    tick_data: over.tick_data.clone(),
                    update_block: state.update_block,
                    tick_data_block: None,
                    coverage: PoolTickCoverage::Sparse,
                    fetcher: None,
                };
                let (_id, st) = V4PoolState::from_params(params, self.journal_depth);
                TransientCl {
                    family: TransientFamily::V4(
                        identity.pool_key.fee,
                        identity.pool_key.tick_spacing,
                    ),
                    inner: TransientInner::V4(Box::new(st)),
                }
            }
            _ => return None,
        };

        let mut sim = OverrideSim {
            target: &mut target,
            zero_for_one: over.request.zero_for_one,
            spec,
            limit,
            fetcher,
            pool_id: over.pool_id,
            disarm_fetch,
        };
        match drive(&mut sim, block) {
            PolicyAttempt::Computed(outcome, _) => Some(outcome),
            PolicyAttempt::NotComputable
            | PolicyAttempt::FetchFailed(..)
            | PolicyAttempt::FetchExhausted(..) => None,
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

    /// RATR5A/CXRHW3: the swap with miss recovery DISARMED — a residual
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

    // TODO(X4EU3J follow-up): extract the per-family arms once the hook
    // caveat plumbing settles; the body is 114 lines against a 100-line
    // clippy::too_many_lines budget.
    #[expect(clippy::too_many_lines)]
    fn swap_simulation_ext(
        &mut self,
        block: u64,
        pool_id: u64,
        request: &SwapRequest,
        disarm_fetch: bool,
    ) -> SwapRead {
        if !self.pools.contains_key(&pool_id) {
            // Unknown pool: return zero (legacy no-raise-on-miss contract).
            return SwapRead::Computed(SwapOutcome::V2(V2SwapOutcome {
                consumed: I256::ZERO,
                delivered: I256::ZERO,
                caveats: Caveats::default(),
            }));
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

        let Some(entry) = self.pools.get(&pool_id) else {
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
                // Exact-output for V2: the closed-form
                // `constant_product_calc_exact_out` (formerly buried in
                // `calculate_tokens_in`). Overdraw (amount_out >= reserve_out)
                // is NotComputable — the legacy ZERO sentinel becomes typed.
                (true, PoolEntry::V2(p)) => {
                    let identity = &p.0;
                    let Some((reserve_in, reserve_out)) = v2_reserves(entry, request.zero_for_one)
                    else {
                        return SwapRead::NotComputable;
                    };
                    let (gamma_numer, fee_denom) = if request.zero_for_one {
                        identity.fee_token0
                    } else {
                        identity.fee_token1
                    };
                    match v2_required_input(
                        reserve_in,
                        reserve_out,
                        gamma_numer,
                        fee_denom,
                        magnitude,
                    ) {
                        Some(required) => SwapRead::Computed(SwapOutcome::V2(V2SwapOutcome {
                            consumed: -unsigned_to_user_delta(required),
                            delivered: unsigned_to_user_delta(magnitude),
                            caveats: Caveats::default(),
                        })),
                        None => SwapRead::NotComputable,
                    }
                }
                // Curve/Balancer/Aerodrome: math not ported (see the former
                // calculate_tokens_in arm); NotComputable as before.
                (true, _) => SwapRead::NotComputable,
            },
            PoolEntry::V3(p) => {
                let (_, v3_state) = (&p.0, &p.1);
                let coverage = v3_state.coverage;
                let family = EngineFamily::V3Engine;
                let limit = request
                    .sqrt_price_limit
                    .unwrap_or_else(|| V3PoolState::default_sqrt_price_limit(request.zero_for_one));
                let mut sim = RegisteredClSim {
                    state: self,
                    pool_id,
                    zero_for_one: request.zero_for_one,
                    spec: engine_amount_specified(request.amount_specified, family),
                    limit,
                    disarm_fetch,
                };
                finish_cl(
                    &mut sim,
                    block,
                    SwapOutcomeFamily::V3,
                    coverage,
                    *request,
                    Caveats::default(),
                )
            }
            PoolEntry::V4(p) => {
                let (identity, v4_state) = (&p.0, &p.1);
                let coverage = v4_state.coverage;
                let family = EngineFamily::V4Engine;
                let hooked =
                    ::degenbot_pools::v4_state::has_amount_modifying_hook(identity.pool_key.hooks);
                let extra_caveats = if hooked {
                    Caveats::HOOKED_POOL
                } else {
                    Caveats::default()
                };
                let limit = request
                    .sqrt_price_limit
                    .unwrap_or_else(|| V3PoolState::default_sqrt_price_limit(request.zero_for_one));
                let mut sim = RegisteredClSim {
                    state: self,
                    pool_id,
                    zero_for_one: request.zero_for_one,
                    spec: engine_amount_specified(request.amount_specified, family),
                    limit,
                    disarm_fetch,
                };
                finish_cl(
                    &mut sim,
                    block,
                    SwapOutcomeFamily::V4,
                    coverage,
                    *request,
                    extra_caveats,
                )
            }
        }
    }
}

/// Which outcome variant a CL drive should assemble.
#[derive(Clone, Copy)]
enum SwapOutcomeFamily {
    V3,
    V4,
}

fn finish_cl(
    sim: &mut impl ComputeMerge,
    block: u64,
    family: SwapOutcomeFamily,
    coverage: PoolTickCoverage,
    request: SwapRequest,
    extra_caveats: Caveats,
) -> SwapRead {
    match drive(sim, block) {
        PolicyAttempt::Computed(outcome, fetched_words) => {
            let caveats = caveats_for_coverage(coverage).union(extra_caveats);
            let payload = cl_payload(&outcome, request.zero_for_one, caveats, fetched_words);
            SwapRead::Computed(match family {
                SwapOutcomeFamily::V3 => SwapOutcome::V3(payload),
                SwapOutcomeFamily::V4 => SwapOutcome::V4(payload),
            })
        }
        PolicyAttempt::NotComputable => SwapRead::NotComputable,
        PolicyAttempt::FetchFailed(word) => SwapRead::FetchFailed { word },
        PolicyAttempt::FetchExhausted(word) => SwapRead::FetchExhausted { word },
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::used_underscore_binding
    )]

    use super::*;
    use hashbrown::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ---------------------------------------------------------------------
    // RATR5A/CXRHW3: the staged pre-pass + DISARMED sim contract.
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
            .expect("CL pool yields discovery");
        assert_eq!(missing.len(), 1, "single crossing -> one word here");
        assert_eq!(calls.load(Ordering::Relaxed), 0, "discovery never fetches");
    }

    // ---------------------------------------------------------------------
    // Sign-convention table (ADR-037): THE pinning test. Canonical request
    // sign -> engine amountSpecified, for all four shapes x both engines.
    #[test]
    fn sign_mapping_table() {
        let in_pos = I256::try_from(1_000_u64).unwrap();
        let out_neg = -I256::try_from(2_000_u64).unwrap();
        // exact-input request (negative, user sends 2000):
        assert_eq!(
            engine_amount_specified(out_neg, EngineFamily::V3Engine),
            I256::try_from(2_000_u64).unwrap()
        );
        assert_eq!(
            engine_amount_specified(out_neg, EngineFamily::V4Engine),
            out_neg
        );
        // exact-output request (positive, pool delivers 1000):
        assert_eq!(
            engine_amount_specified(in_pos, EngineFamily::V3Engine),
            -in_pos
        );
        assert_eq!(
            engine_amount_specified(in_pos, EngineFamily::V4Engine),
            in_pos
        );
    }

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
    // CL payload: user-perspective signs from unsigned engine amounts.
    #[test]
    fn cl_payload_signs_follow_user_perspective() {
        let outcome = V3SwapOutcome {
            amount0: U256::from(500_u64),
            amount1: U256::from(700_u64),
            sqrt_price_x96: U256::from(42_u64),
            liquidity: 9,
            tick: -3,
            input_consumed: U256::from(1_234_u64),
        };
        let payload = cl_payload(&outcome, true, Caveats::SPARSE_COVERAGE, vec![7]);
        // zfo: input side is token0 → consumed tracks amount0 (500).
        assert_eq!(payload.consumed, -I256::try_from(500_u64).unwrap());
        assert_eq!(payload.delivered, I256::try_from(700_u64).unwrap());
        assert_eq!(payload.end_tick, -3);
        assert_eq!(payload.fetched_words, vec![7]);
        assert!(payload.caveats.contains(Caveats::SPARSE_COVERAGE));

        let ofz = cl_payload(&outcome, false, Caveats::default(), Vec::new());
        assert_eq!(ofz.delivered, I256::try_from(500_u64).unwrap());
    }

    // ---------------------------------------------------------------------
    // Policy drive: a scripted ComputeMerge exercises every branch without
    // constructing real pools.
    struct ScriptedSim {
        script: Vec<Result<V3SwapOutcome, SimulateSwapError>>,
        call: AtomicUsize,
        fetcher_ok: bool,
        has_fetcher: bool,
        fetched: Vec<FetchedTickWord>,
    }

    impl ScriptedSim {
        fn word(word: i32) -> FetchedTickWord {
            FetchedTickWord {
                word,
                ticks: HashMap::new(),
            }
        }
    }

    impl ComputeMerge for ScriptedSim {
        fn compute(&self) -> Result<V3SwapOutcome, SimulateSwapError> {
            let idx = self.call.load(Ordering::SeqCst);
            self.call.store(idx + 1, Ordering::SeqCst);
            self.script
                .get(idx)
                .cloned()
                .unwrap_or(Err(SimulateSwapError::NotComputable))
        }
        fn merge_word(&mut self, fetched: &FetchedTickWord) {
            self.fetched.push(fetched.clone());
        }
        fn fetch_word(&self, _word: i32, _block: u64) -> Result<FetchedTickWord, FetchFailure> {
            if !self.has_fetcher {
                Err(FetchFailure::NoFetcher)
            } else if self.fetcher_ok {
                Ok(Self::word(_word))
            } else {
                Err(FetchFailure::Errored)
            }
        }
    }

    fn outcome_stub() -> V3SwapOutcome {
        V3SwapOutcome::default()
    }

    #[test]
    fn drive_immediate_success() {
        let mut sim = ScriptedSim {
            script: vec![Ok(outcome_stub())],
            call: AtomicUsize::new(0),
            fetcher_ok: true,
            has_fetcher: true,
            fetched: Vec::new(),
        };
        match drive(&mut sim, 17) {
            PolicyAttempt::Computed(_, words) => assert!(words.is_empty()),
            other => panic!("expected Computed, got {other:?}"),
        }
        assert!(sim.fetched.is_empty());
    }

    #[test]
    fn drive_miss_then_fetch_then_success_records_words() {
        let mut sim = ScriptedSim {
            script: vec![
                Err(SimulateSwapError::MissingTickWord(-4)),
                Ok(outcome_stub()),
            ],
            call: AtomicUsize::new(0),
            fetcher_ok: true,
            has_fetcher: true,
            fetched: Vec::new(),
        };
        match drive(&mut sim, 17) {
            PolicyAttempt::Computed(_, words) => assert_eq!(words, vec![-4]),
            other => panic!("expected Computed, got {other:?}"),
        }
        assert_eq!(sim.fetched.len(), 1);
    }

    #[test]
    fn drive_repeated_miss_is_exhausted() {
        let miss = Err(SimulateSwapError::MissingTickWord(9));
        let mut sim = ScriptedSim {
            script: vec![miss.clone(), miss, Ok(outcome_stub())],
            call: AtomicUsize::new(0),
            fetcher_ok: true,
            has_fetcher: true,
            fetched: Vec::new(),
        };
        assert_eq!(drive(&mut sim, 17), PolicyAttempt::FetchExhausted(9));
    }

    #[test]
    fn drive_no_fetcher_is_exhausted() {
        // The legacy path collapsed "no fetcher registered" into the same
        // silent give-up as a failed fetch; the gate keeps them distinct:
        // NoFetcher => FetchExhausted (nothing was attempted), while a real
        // fetcher error => FetchFailed.
        let mut sim = ScriptedSim {
            script: vec![Err(SimulateSwapError::MissingTickWord(2))],
            call: AtomicUsize::new(0),
            fetcher_ok: false,
            has_fetcher: false,
            fetched: Vec::new(),
        };
        assert_eq!(drive(&mut sim, 17), PolicyAttempt::FetchExhausted(2));

        let mut errored = ScriptedSim {
            script: vec![Err(SimulateSwapError::MissingTickWord(2))],
            call: AtomicUsize::new(0),
            fetcher_ok: false,
            has_fetcher: true,
            fetched: Vec::new(),
        };
        assert_eq!(drive(&mut errored, 17), PolicyAttempt::FetchFailed(2));
    }

    #[test]
    fn drive_not_computable_short_circuits() {
        let mut sim = ScriptedSim {
            script: vec![Err(SimulateSwapError::NotComputable)],
            call: AtomicUsize::new(0),
            fetcher_ok: true,
            has_fetcher: true,
            fetched: Vec::new(),
        };
        assert_eq!(drive(&mut sim, 17), PolicyAttempt::NotComputable);
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
                    liquidity_net: I256::ZERO,
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
    fn unregistered_pool_and_zero_amount_return_zero() {
        let mut bot = BotState::new();
        // Unknown pool: legacy no-raise-on-miss → Computed(0).
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
            SwapRead::Computed(SwapOutcome::V2(V2SwapOutcome {
                consumed: I256::ZERO,
                delivered: I256::ZERO,
                caveats: Caveats::default(),
            }))
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
}
