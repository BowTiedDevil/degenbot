//! Pool-registry sum type (`PoolEntry`) + V3/V4 concentrated-liquidity
//! family reader trait + token-entry metadata. **Relocated** from
//! `degenbot-bot/src/bot_core/mod.rs`.

use crate::aerodrome_v2_state::{AerodromeV2PoolIdentity, AerodromeV2PoolState};
use crate::balancer_stable_state::{BalancerStablePoolIdentity, BalancerStablePoolState};
use crate::balancer_weighted_state::{BalancerWeightedPoolIdentity, BalancerWeightedPoolState};
use crate::curve_state::{CurvePoolIdentity, CurvePoolState};
use crate::state_history::{
    BalanceVectorPoolState, ReservePairPoolState, ScalarPriors, TickBefore, V3BlockDelta,
};
use crate::v2_state::{V2PoolIdentity, V2PoolState};
use crate::v3_state::{V3PoolIdentity, V3PoolState};
use crate::v4_state::{V4PoolIdentity, V4PoolState};
use crate::TickInfo;
use alloy::primitives::{Address, I256, U256};
use std::collections::HashMap;

/// A single pool's state. Pool-type-specific fields are in the enum variants.
#[derive(Clone, Debug)]
pub enum PoolEntry {
    V2(V2PoolIdentity, V2PoolState),
    V3(V3PoolIdentity, V3PoolState),
    V4(V4PoolIdentity, V4PoolState),
    Curve(CurvePoolIdentity, CurvePoolState),
    BalancerWeighted(BalancerWeightedPoolIdentity, BalancerWeightedPoolState),
    BalancerStable(BalancerStablePoolIdentity, BalancerStablePoolState),
    AerodromeV2(AerodromeV2PoolIdentity, AerodromeV2PoolState),
}

/// Per-variant projection methods for [`PoolEntry`] (ADR-014 D5).
///
/// Each `vN()` / `vN_mut()` returns the `(identity, state)` pair when the
/// entry matches the family, `None` otherwise — replacing the per-reader
/// `match` arms that listed all sibling variants only to return `None`
/// (`V3 | V4 | Curve | … => None`). Readers on `BotState` now collapse to
/// `self.pools.get(&pool_id).and_then(PoolEntry::vN).map(|(i,_)| i)`.
///
/// The identity/state sibling split in each variant is preserved — callers
/// asking for state get `&VxPoolState`, callers asking for identity get
/// `&VxPoolIdentity`, both via the same projection destructured differently.
impl PoolEntry {
    /// V2 `(identity, state)` borrow, or `None` for a different family.
    #[must_use]
    pub fn v2(&self) -> Option<(&V2PoolIdentity, &V2PoolState)> {
        if let Self::V2(i, s) = self {
            Some((i, s))
        } else {
            None
        }
    }
    /// V2 `(identity, state)` mutable borrow, or `None` for a different family.
    #[must_use]
    pub fn v2_mut(&mut self) -> Option<(&mut V2PoolIdentity, &mut V2PoolState)> {
        if let Self::V2(i, s) = self {
            Some((i, s))
        } else {
            None
        }
    }

    /// V3 `(identity, state)` borrow, or `None` for a different family.
    #[must_use]
    pub fn v3(&self) -> Option<(&V3PoolIdentity, &V3PoolState)> {
        if let Self::V3(i, s) = self {
            Some((i, s))
        } else {
            None
        }
    }
    /// V3 `(identity, state)` mutable borrow, or `None` for a different family.
    #[must_use]
    pub fn v3_mut(&mut self) -> Option<(&mut V3PoolIdentity, &mut V3PoolState)> {
        if let Self::V3(i, s) = self {
            Some((i, s))
        } else {
            None
        }
    }

    /// V4 `(identity, state)` borrow, or `None` for a different family.
    #[must_use]
    pub fn v4(&self) -> Option<(&V4PoolIdentity, &V4PoolState)> {
        if let Self::V4(i, s) = self {
            Some((i, s))
        } else {
            None
        }
    }
    /// V4 `(identity, state)` mutable borrow, or `None` for a different family.
    #[must_use]
    pub fn v4_mut(&mut self) -> Option<(&mut V4PoolIdentity, &mut V4PoolState)> {
        if let Self::V4(i, s) = self {
            Some((i, s))
        } else {
            None
        }
    }

    /// Curve `(identity, state)` borrow, or `None` for a different family.
    #[must_use]
    pub fn curve(&self) -> Option<(&CurvePoolIdentity, &CurvePoolState)> {
        if let Self::Curve(i, s) = self {
            Some((i, s))
        } else {
            None
        }
    }
    /// Curve `(identity, state)` mutable borrow, or `None` for a different family.
    #[must_use]
    pub fn curve_mut(&mut self) -> Option<(&mut CurvePoolIdentity, &mut CurvePoolState)> {
        if let Self::Curve(i, s) = self {
            Some((i, s))
        } else {
            None
        }
    }

    /// Balancer weighted `(identity, state)` borrow, or `None` for a different family.
    #[must_use]
    pub fn balancer_weighted(
        &self,
    ) -> Option<(&BalancerWeightedPoolIdentity, &BalancerWeightedPoolState)> {
        if let Self::BalancerWeighted(i, s) = self {
            Some((i, s))
        } else {
            None
        }
    }
    /// Balancer weighted `(identity, state)` mutable borrow, or `None` for a different family.
    #[must_use]
    pub fn balancer_weighted_mut(
        &mut self,
    ) -> Option<(
        &mut BalancerWeightedPoolIdentity,
        &mut BalancerWeightedPoolState,
    )> {
        if let Self::BalancerWeighted(i, s) = self {
            Some((i, s))
        } else {
            None
        }
    }

    /// Balancer stable `(identity, state)` borrow, or `None` for a different family.
    #[must_use]
    pub fn balancer_stable(
        &self,
    ) -> Option<(&BalancerStablePoolIdentity, &BalancerStablePoolState)> {
        if let Self::BalancerStable(i, s) = self {
            Some((i, s))
        } else {
            None
        }
    }
    /// Balancer stable `(identity, state)` mutable borrow, or `None` for a different family.
    #[must_use]
    pub fn balancer_stable_mut(
        &mut self,
    ) -> Option<(
        &mut BalancerStablePoolIdentity,
        &mut BalancerStablePoolState,
    )> {
        if let Self::BalancerStable(i, s) = self {
            Some((i, s))
        } else {
            None
        }
    }

    /// Aerodrome V2 `(identity, state)` borrow, or `None` for a different family.
    #[must_use]
    pub fn aerodrome_v2(&self) -> Option<(&AerodromeV2PoolIdentity, &AerodromeV2PoolState)> {
        if let Self::AerodromeV2(i, s) = self {
            Some((i, s))
        } else {
            None
        }
    }
    /// Aerodrome V2 `(identity, state)` mutable borrow, or `None` for a different family.
    #[must_use]
    pub fn aerodrome_v2_mut(
        &mut self,
    ) -> Option<(&mut AerodromeV2PoolIdentity, &mut AerodromeV2PoolState)> {
        if let Self::AerodromeV2(i, s) = self {
            Some((i, s))
        } else {
            None
        }
    }

    /// Project to `&mut dyn ConcentratedLiquidityPoolMut` (ADR-017) — the
    /// mutable CL-family apply-twin of [`Self::as_reorg_state_mut`] (the
    /// reorg projection from ADR-016). Used by `BotState`'s CL apply
    /// dispatchers (`merge_tick_word`, and the future `apply_swap` /
    /// `apply_liquidity_update` slices) to reach the trait method without a
    /// per-family variant match at each call site.
    #[must_use]
    pub fn as_cl_mut(&mut self) -> Option<&mut dyn ConcentratedLiquidityPoolMut> {
        some_cl_mut(self)
    }

    /// The pool's per-mutation state nonce (AV42C7). Bumped on every state
    /// change (`apply_swap`, `apply_liquidity_update`, `replace_tick_data`,
    /// `merge_tick_word`, `apply_sync`, `apply_balance_update`,
    /// `restore_before_block`). The solver snapshots this per-hop at resolve
    /// time so the dispatch seam can detect staleness: if a pool's current
    /// nonce has advanced past the snapshot, the solver computed its result
    /// against state that has since been superseded → skip the stale
    /// candidate (the block-N solve used pool@N-1 while on-chain@N has pool@N
    /// after a user swap landed between the solve and the sim).
    #[must_use]
    pub fn state_nonce(&self) -> u64 {
        match self {
            PoolEntry::V2(_, s) => s.state_nonce,
            PoolEntry::V3(_, s) => s.state_nonce,
            PoolEntry::V4(_, s) => s.state_nonce,
            PoolEntry::Curve(_, s) => s.state_nonce,
            PoolEntry::BalancerWeighted(_, s) => s.state_nonce,
            PoolEntry::BalancerStable(_, s) => s.state_nonce,
            PoolEntry::AerodromeV2(_, s) => s.state_nonce,
        }
    }

    /// The block the pool's reserves / `sqrt_price` / `tick` / liquidity were
    /// last mutated by a forward event — the freshness signal for the per-path
    /// mid-block solve gate (ergo AV42C7). A path is solved at `solve_block`
    /// only when EVERY hop's `update_block >= solve_block`; a stale pool defers
    /// its path until its block-N log lands and re-dirties it (the backrun
    /// otherwise lands at a block where one pool still holds N-1 state).
    #[must_use]
    pub fn update_block(&self) -> u64 {
        match self {
            PoolEntry::V2(_, s) => s.update_block,
            PoolEntry::V3(_, s) => s.update_block,
            PoolEntry::V4(_, s) => s.update_block,
            PoolEntry::Curve(_, s) => s.update_block,
            PoolEntry::BalancerWeighted(_, s) => s.update_block,
            PoolEntry::BalancerStable(_, s) => s.update_block,
            PoolEntry::AerodromeV2(_, s) => s.update_block,
        }
    }

    /// Project to `&dyn ReorgPoolState` (ADR-016). One match over all 7
    /// variants — used by `BotState`'s unified reorg dispatchers
    /// (`restore_pool_before_block` / `discard_pool_before_block` /
    /// `pool_journal_len`) to dispatch through the trait without re-matching
    /// family inline.
    #[must_use]
    pub fn as_reorg_state(&self) -> Option<&dyn crate::state_history::ReorgPoolState> {
        Some(match self {
            PoolEntry::V2(_, s) => s,
            PoolEntry::V3(_, s) => s,
            PoolEntry::V4(_, s) => s,
            PoolEntry::Curve(_, s) => s,
            PoolEntry::BalancerWeighted(_, s) => s,
            PoolEntry::BalancerStable(_, s) => s,
            PoolEntry::AerodromeV2(_, s) => s,
        })
    }

    /// Mutable projection to `&mut dyn ReorgPoolState` (ADR-016) — the write
    /// twin of [`as_reorg_state`](Self::as_reorg_state).
    #[must_use]
    pub fn as_reorg_state_mut(&mut self) -> Option<&mut dyn crate::state_history::ReorgPoolState> {
        Some(match self {
            PoolEntry::V2(_, s) => s,
            PoolEntry::V3(_, s) => s,
            PoolEntry::V4(_, s) => s,
            PoolEntry::Curve(_, s) => s,
            PoolEntry::BalancerWeighted(_, s) => s,
            PoolEntry::BalancerStable(_, s) => s,
            PoolEntry::AerodromeV2(_, s) => s,
        })
    }

    /// Project to `&mut dyn BalanceVectorPoolState` (ADR-017 D1) — the
    /// forward-apply view over the three balance-vector families
    /// (Curve / `BalancerWeighted` / `BalancerStable`). Used by `BotState`'s
    /// unified `apply_balance_update_by_pool_id` dispatcher to reach the
    /// trait method without a per-family variant match. Returns `None` for
    /// the four non-balance-vector families.
    #[must_use]
    pub fn as_balance_vector_mut(&mut self) -> Option<&mut dyn BalanceVectorPoolState> {
        match self {
            PoolEntry::Curve(_, s) => Some(s),
            PoolEntry::BalancerWeighted(_, s) => Some(s),
            PoolEntry::BalancerStable(_, s) => Some(s),
            PoolEntry::V2(..)
            | PoolEntry::V3(..)
            | PoolEntry::V4(..)
            | PoolEntry::AerodromeV2(..) => None,
        }
    }

    /// Project to `&mut dyn ReservePairPoolState` (ADR-017 D3) — the
    /// forward-apply view over the two reserve-pair families (V2 /
    /// `AerodromeV2`). Used by `BotState`'s unified `apply_sync_by_pool_id`
    /// dispatcher to reach the trait method without a per-family variant
    /// match. Returns `None` for the five non-reserve-pair families.
    #[must_use]
    pub fn as_reserve_pair_mut(&mut self) -> Option<&mut dyn ReservePairPoolState> {
        match self {
            PoolEntry::V2(_, s) => Some(s),
            PoolEntry::AerodromeV2(_, s) => Some(s),
            PoolEntry::V3(..)
            | PoolEntry::V4(..)
            | PoolEntry::Curve(..)
            | PoolEntry::BalancerWeighted(..)
            | PoolEntry::BalancerStable(..) => None,
        }
    }
}

/// `PoolEntry::as_cl_mut` helper — one match over V3 | V4. Free-standing so
/// `PoolEntry::as_cl_mut` stays a one-liner (matches the `as_reorg_state`
/// shape). Returns `None` for the five non-CL families.
fn some_cl_mut(entry: &mut PoolEntry) -> Option<&mut dyn ConcentratedLiquidityPoolMut> {
    match entry {
        PoolEntry::V3(_, s) => Some(s),
        PoolEntry::V4(_, s) => Some(s),
        PoolEntry::V2(..)
        | PoolEntry::Curve(..)
        | PoolEntry::BalancerWeighted(..)
        | PoolEntry::BalancerStable(..)
        | PoolEntry::AerodromeV2(..) => None,
    }
}

/// Read-only surface shared by [`V3PoolState`] and [`V4PoolState`] — the
/// fields the per-handle `PyLiquidityPool` reader API presents uniformly
/// across the V3/V4 concentrated-liquidity families (J63J3N).
///
/// Both variants store the same mutable scalars (`sqrt_price_x96`/
/// `liquidity`/`tick`/`update_block`) and an identical `tick_data:
/// HashMap<i32, TickInfo>`; V4 additionally nests `fee`/`tick_spacing`
/// inside `pool_key`, which the impl projects out. The trait lets
/// [`BotState::get_v3_or_v4_pool`] return one borrowed view covering both
/// families without cloning — the reader twin of the RAJ3PP apply dispatchers.
///
/// V2 is intentionally excluded (different state shape — reserves, not
/// scalars); a V2 `pool_id` yields `None` from the accessor, matching the
/// prior V3-only contract.
/// Mutable-reader trait for V3/V4 concentrated-liquidity pools.
///
/// Projects only mutable runtime scalars (`sqrt_price_x96`/`liquidity`/`tick`/
/// `update_block`/`tick_data`) — the values a swap calc consumes. Immutable
/// config (`fee`/`tick_spacing`) lives on `V3PoolIdentity`/`V4PoolIdentity`;
/// read it via [`BotState::get_v3_identity`]/[`BotState::get_v4_identity`].
/// The dyn-dispatch surface is a `&VxPoolState` borrowed from the registry.
pub trait ConcentratedLiquidityPool {
    fn sqrt_price_x96(&self) -> U256;
    fn liquidity(&self) -> u128;
    fn tick(&self) -> i32;
    fn update_block(&self) -> u64;
    fn tick_data(&self) -> &HashMap<i32, TickInfo>;
}

impl ConcentratedLiquidityPool for V3PoolState {
    fn sqrt_price_x96(&self) -> U256 {
        self.sqrt_price_x96
    }
    fn liquidity(&self) -> u128 {
        self.liquidity
    }
    fn tick(&self) -> i32 {
        self.tick
    }
    fn update_block(&self) -> u64 {
        self.update_block
    }
    fn tick_data(&self) -> &HashMap<i32, TickInfo> {
        &self.tick_data
    }
}

impl ConcentratedLiquidityPool for V4PoolState {
    fn sqrt_price_x96(&self) -> U256 {
        self.sqrt_price_x96
    }
    fn liquidity(&self) -> u128 {
        self.liquidity
    }
    fn tick(&self) -> i32 {
        self.tick
    }
    fn update_block(&self) -> u64 {
        self.update_block
    }
    fn tick_data(&self) -> &HashMap<i32, TickInfo> {
        &self.tick_data
    }
}

/// Mutable CL-family trait — the write twin of [`ConcentratedLiquidityPool`]
/// (ADR-014 D2b). Carries the CL mutators previously inlined twice in
/// `BotState` (one arm per family); the body lives once in the trait impl.
///
/// `tick_spacing` is passed in (it lives on the identity slice, not the state
/// struct) — the caller reads it off `V3PoolIdentity.tick_spacing` /
/// `V4PoolIdentity.pool_key.tick_spacing` before dispatching, so the trait
/// stays identity-agnostic and the two state-struct impls are byte-identical.
pub trait ConcentratedLiquidityPoolMut: ConcentratedLiquidityPool {
    /// Wholesale-replace the `tick_data` map, advance `update_block` if newer
    /// (monotonic — no rewind), re-seed `known_bitmap_words` from the new
    /// keys' word positions, and invalidate the cached tick ranges. Scalars
    /// (`sqrt_price_x96`/`liquidity`/`tick`) are untouched.
    ///
    /// No journal delta — a wholesale replace has undefined rollback
    /// semantics; the pump is the authority for event-derived ticks (mirrors
    /// `sync_v3_pool_state`).
    ///
    /// Returns `true` (the CL mutator always succeeds; the dispatch site's
    /// `false` return is reserved for V2 / non-CL / unregistered pools).
    fn replace_tick_data(
        &mut self,
        tick_data: HashMap<i32, TickInfo>,
        update_block: u64,
        tick_spacing: i32,
    ) -> bool;

    /// Merge a fetched tick-bitmap word's ticks into this state's `tick_data`,
    /// mark the word as known in `known_bitmap_words`, and invalidate the
    /// cached tick ranges. Scalars (`sqrt_price_x96`/`liquidity`/`tick`)
    /// are untouched. Used by the fetch+retry miss path
    /// ([`crate::tick_fetch::miss::TickWordFetcher`]) and the
    /// `simulate_swap_with_override` transient-state merge loop.
    ///
    /// No journal delta — a fetched-word merge mutates `tick_data` only;
    /// reorg rollback covers it via the surrounding event's delta, not a
    /// merge delta (mirrors the no-journal contract of [`Self::replace_tick_data`]).
    ///
    /// Returns `true` (the CL mutator always succeeds; the dispatch site's
    /// `false` return is reserved for non-CL / unregistered pools — same
    /// convention as [`Self::replace_tick_data`]).
    fn merge_tick_word(&mut self, fetched: &crate::tick_fetch::FetchedTickWord) -> bool;

    /// Apply a Swap event to this pool's mutable `slot0` scalars + `tick_data`,
    /// capturing reverse-apply priors into the reorg journal (ADR-014 D1,
    /// ADR-017 slice 2). The delta carries `scalar_priors: Some(..)` (a Swap
    /// changes the slot0 head on every event) plus per-tick priors for any
    /// ticks this event mutates. A tick with no prior entry is journaled with
    /// `liquidity_gross_before: None` (on rollback, delete it).
    ///
    /// `tick_priors`: ticks the event reports as changed, with their new
    /// `TickInfo`. The method *writes* the passed tick info into `tick_data`
    /// (it carries new tick values from the event), whereas [`Self::apply_liquidity_update`]
    /// *reads* the current tick priors before mutating — same loop shape,
    /// different work.
    fn apply_swap(
        &mut self,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)],
    );

    /// Apply a liquidity update (V3 Mint/Burn, V4 `ModifyLiquidity`) to this
    /// pool's `tick_data`, capturing reverse-apply priors into the reorg
    /// journal (ADR-014 D1, ADR-017 slice 2). Applies the delta via
    /// [`crate::tick_bitmap::apply_liquidity_to_tick_range`] (matching Solidity
    /// `Tick.update`), advances `update_block`, invalidates the tick-range
    /// cache.
    ///
    /// Journal-capture policy: when the `[tick_lower, tick_upper)` region
    /// straddles the pool's current `tick`, the event ALSO changes the on-chain
    /// active `liquidity` scalar by `delta` (a Mint adds range liquidity to the
    /// active pool; a Burn removes it). In that case the delta carries
    /// `scalar_priors: Some(..)` so restore rolls the scalar back. Out-of-range
    /// events mutate `tick_data` only and carry `scalar_priors: None` (ADR-004
    /// — restore skips the scalar write-back). The two boundary-tick priors
    /// (captured BEFORE mutation) are reverse-applied in both cases.
    fn apply_liquidity_update(
        &mut self,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    );
}

/// Compute the new active (in-range) `liquidity` scalar when a Mint/Burn/
/// `ModifyLiquidity` event's `[tick_lower, tick_upper)` region straddles the
/// pool's current `tick`.
///
/// Parity with the pure reference
/// `degenbot-cl-math::cl_lib::liquidity_mapping::apply_liquidity_mapping_update`
/// and the Python companions (`UniswapV3Pool`/`UniswapV4Pool.update_liquidity_map`):
/// an in-range liquidity event changes the pool's on-chain `liquidity` field by
/// `delta` in addition to the boundary-tick map mutation. Without this the
/// solver's active-liquidity scalar drifts from on-chain even though
/// `sqrt_price_x96` / `tick` stay byte-identical — an in-range Burn leaves it
/// too high (capable of overdrawing / wrong-crossing estimates), an in-range
/// Mint leaves it too low.
///
/// Computed in wide `I256` so the intermediate never overflows for valid
/// inputs. Returns `Some(new_liquidity)` when the current tick is in range,
/// else `None` (out-of-range events leave the scalar untouched).
///
/// # Panics
///
/// Panics if the in-range result would be negative (an invariant violation —
/// on-chain liquidity can never be negative) or exceeds `u128::MAX` (real
/// surprise overflow). Out-of-range events never reach the arithmetic.
fn in_range_active_liquidity(
    current_liquidity: u128,
    current_tick: i32,
    tick_lower: i32,
    tick_upper: i32,
    liquidity_delta: i128,
) -> Option<u128> {
    if !(tick_lower <= current_tick && current_tick < tick_upper) {
        return None;
    }
    let base = I256::try_from(U256::from(current_liquidity)).expect("u128 fits I256");
    let delta = I256::try_from(liquidity_delta).expect("i128 fits I256");
    let new_active = base + delta;
    assert!(
        new_active >= I256::ZERO,
        "in-range liquidity adjustment violated invariant: {base} + {delta} < 0"
    );
    let narrowed =
        u128::try_from(new_active).expect("non-negative in-range reachable liquidity fits u128");
    Some(narrowed)
}

impl ConcentratedLiquidityPoolMut for V3PoolState {
    fn replace_tick_data(
        &mut self,
        tick_data: HashMap<i32, TickInfo>,
        update_block: u64,
        tick_spacing: i32,
    ) -> bool {
        self.tick_data = tick_data;
        if update_block > self.update_block {
            self.update_block = update_block;
        }
        self.seed_known_bitmap_words(tick_spacing);
        self.invalidate_tick_range_cache();
        true
    }

    fn merge_tick_word(&mut self, fetched: &crate::tick_fetch::FetchedTickWord) -> bool {
        for (tick, info) in &fetched.ticks {
            self.tick_data.insert(*tick, info.clone());
        }
        self.known_bitmap_words.insert(fetched.word);
        self.invalidate_tick_range_cache();
        true
    }

    fn apply_swap(
        &mut self,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)],
    ) {
        let mut journaled_priors: Vec<(i32, TickBefore)> = Vec::with_capacity(tick_priors.len());
        for &(tick_index, ref new_info) in tick_priors {
            let prior = self.tick_data.get(&tick_index).cloned();
            journaled_priors.push((
                tick_index,
                TickBefore {
                    liquidity_gross_before: prior.as_ref().map(|p| p.liquidity_gross),
                    liquidity_net_before: prior.as_ref().map_or(I256::ZERO, |p| p.liquidity_net),
                },
            ));
            self.tick_data.insert(tick_index, new_info.clone());
        }

        self.journal.push_delta(V3BlockDelta {
            block: block_number,
            scalar_priors: Some(ScalarPriors {
                sqrt_price_x96_before: self.sqrt_price_x96,
                liquidity_before: self.liquidity,
                tick_before: self.tick,
            }),
            tick_priors: journaled_priors,
        });

        self.sqrt_price_x96 = sqrt_price_x96;
        self.liquidity = liquidity;
        self.tick = tick;
        if block_number > self.update_block {
            self.update_block = block_number;
        }
        self.state_nonce = self.state_nonce.wrapping_add(1);
        self.invalidate_tick_range_cache();
    }

    fn apply_liquidity_update(
        &mut self,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) {
        // Historical-replay guard (UO3JM4 / Python `_initial_state_block`):
        // only a genuinely in-range event STRICTLY AFTER the seed block adjusts
        // the active-liquidity scalar. The seed's `liquidity` already reflects
        // every on-chain event <= initial_state_block, so replaying one (e.g. a
        // backfilled Burn applied after registration against head) would
        // double-count it. Such a replay still mutates the tick map, but the
        // scalar write-back + journal stay tick-only (scalar_priors: None).
        let in_range = tick_lower <= self.tick
            && self.tick < tick_upper
            && block_number > self.initial_state_block;

        let mut journaled_priors: Vec<(i32, TickBefore)> = Vec::with_capacity(2);
        for &tick_idx in &[tick_lower, tick_upper] {
            let prior = self.tick_data.get(&tick_idx).cloned();
            journaled_priors.push((
                tick_idx,
                TickBefore {
                    liquidity_gross_before: prior.as_ref().map(|p| p.liquidity_gross),
                    liquidity_net_before: prior.as_ref().map_or(I256::ZERO, |p| p.liquidity_net),
                },
            ));
        }

        crate::tick_bitmap::apply_liquidity_to_tick_range(
            &mut self.tick_data,
            tick_lower,
            tick_upper,
            liquidity_delta,
            block_number,
        );

        self.journal.push_delta(V3BlockDelta {
            block: block_number,
            scalar_priors: if in_range {
                Some(ScalarPriors {
                    sqrt_price_x96_before: self.sqrt_price_x96,
                    liquidity_before: self.liquidity,
                    tick_before: self.tick,
                })
            } else {
                None
            },
            tick_priors: journaled_priors,
        });

        if in_range {
            self.liquidity = in_range_active_liquidity(
                self.liquidity,
                self.tick,
                tick_lower,
                tick_upper,
                liquidity_delta,
            )
            .expect("in-range branch implies the range straddles the current tick");
        }

        if block_number > self.update_block {
            self.update_block = block_number;
        }
        self.state_nonce = self.state_nonce.wrapping_add(1);
        self.invalidate_tick_range_cache();
    }
}

impl ConcentratedLiquidityPoolMut for V4PoolState {
    fn replace_tick_data(
        &mut self,
        tick_data: HashMap<i32, TickInfo>,
        update_block: u64,
        tick_spacing: i32,
    ) -> bool {
        self.tick_data = tick_data;
        if update_block > self.update_block {
            self.update_block = update_block;
        }
        self.state_nonce = self.state_nonce.wrapping_add(1);
        self.seed_known_bitmap_words(tick_spacing);
        self.invalidate_tick_range_cache();
        true
    }

    fn merge_tick_word(&mut self, fetched: &crate::tick_fetch::FetchedTickWord) -> bool {
        for (tick, info) in &fetched.ticks {
            self.tick_data.insert(*tick, info.clone());
        }
        self.known_bitmap_words.insert(fetched.word);
        self.state_nonce = self.state_nonce.wrapping_add(1);
        self.invalidate_tick_range_cache();
        true
    }

    fn apply_swap(
        &mut self,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)],
    ) {
        let mut journaled_priors: Vec<(i32, TickBefore)> = Vec::with_capacity(tick_priors.len());
        for &(tick_index, ref new_info) in tick_priors {
            let prior = self.tick_data.get(&tick_index).cloned();
            journaled_priors.push((
                tick_index,
                TickBefore {
                    liquidity_gross_before: prior.as_ref().map(|p| p.liquidity_gross),
                    liquidity_net_before: prior.as_ref().map_or(I256::ZERO, |p| p.liquidity_net),
                },
            ));
            self.tick_data.insert(tick_index, new_info.clone());
        }

        self.journal.push_delta(V3BlockDelta {
            block: block_number,
            scalar_priors: Some(ScalarPriors {
                sqrt_price_x96_before: self.sqrt_price_x96,
                liquidity_before: self.liquidity,
                tick_before: self.tick,
            }),
            tick_priors: journaled_priors,
        });

        self.sqrt_price_x96 = sqrt_price_x96;
        self.liquidity = liquidity;
        self.tick = tick;
        if block_number > self.update_block {
            self.update_block = block_number;
        }
        self.state_nonce = self.state_nonce.wrapping_add(1);
        self.invalidate_tick_range_cache();
    }

    fn apply_liquidity_update(
        &mut self,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) {
        let in_range = tick_lower <= self.tick
            && self.tick < tick_upper
            && block_number > self.initial_state_block;

        let mut journaled_priors: Vec<(i32, TickBefore)> = Vec::with_capacity(2);
        for &tick_idx in &[tick_lower, tick_upper] {
            let prior = self.tick_data.get(&tick_idx).cloned();
            journaled_priors.push((
                tick_idx,
                TickBefore {
                    liquidity_gross_before: prior.as_ref().map(|p| p.liquidity_gross),
                    liquidity_net_before: prior.as_ref().map_or(I256::ZERO, |p| p.liquidity_net),
                },
            ));
        }

        crate::tick_bitmap::apply_liquidity_to_tick_range(
            &mut self.tick_data,
            tick_lower,
            tick_upper,
            liquidity_delta,
            block_number,
        );

        self.journal.push_delta(V3BlockDelta {
            block: block_number,
            scalar_priors: if in_range {
                Some(ScalarPriors {
                    sqrt_price_x96_before: self.sqrt_price_x96,
                    liquidity_before: self.liquidity,
                    tick_before: self.tick,
                })
            } else {
                None
            },
            tick_priors: journaled_priors,
        });

        if in_range {
            self.liquidity = in_range_active_liquidity(
                self.liquidity,
                self.tick,
                tick_lower,
                tick_upper,
                liquidity_delta,
            )
            .expect("in-range branch implies the range straddles the current tick");
        }

        if block_number > self.update_block {
            self.update_block = block_number;
        }
        self.state_nonce = self.state_nonce.wrapping_add(1);
        self.invalidate_tick_range_cache();
    }
}

/// ERC20 token metadata.
#[derive(Clone, Debug)]
pub struct TokenEntry {
    pub address: Address,
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
    pub chain_id: u64,
}

#[cfg(test)]
mod projection_tests {
    #![allow(unused_imports)]
    use super::*;
    use crate::v3_state::{RegisterV3PoolParams, V3PoolState};

    fn v3_entry() -> PoolEntry {
        let (identity, state) = V3PoolState::from_params(RegisterV3PoolParams::default(), 8);
        PoolEntry::V3(identity, state)
    }

    #[test]
    fn v3_projection_returns_some_for_v3_and_none_for_sibling_families() {
        // What: v3() returns Some when the entry is V3, None for every other
        // family. The sibling-family None is the collapse that removes the
        // variant-exhaustion arms in the get_* readers (D5).
        let entry = v3_entry();
        assert!(entry.v3().is_some());
        assert!(entry.v2().is_none());
        assert!(entry.v4().is_none());
        assert!(entry.curve().is_none());
        assert!(entry.balancer_weighted().is_none());
        assert!(entry.balancer_stable().is_none());
        assert!(entry.aerodrome_v2().is_none());
    }

    #[test]
    fn v3_mut_projection_returns_mutable_borrow() {
        // What: v3_mut() returns a mutable (identity, state) borrow.
        let mut entry = v3_entry();
        if let Some((_, state)) = entry.v3_mut() {
            state.update_block = 7;
        }
        assert_eq!(entry.v3().unwrap().1.update_block, 7);
    }
}
