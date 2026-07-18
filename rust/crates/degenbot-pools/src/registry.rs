//! Pool-registry sum type (`PoolEntry`) + V3/V4 concentrated-liquidity
//! family reader trait + token-entry metadata. **Relocated** from
//! `degenbot-bot/src/bot_core/mod.rs`.

use crate::aerodrome_v2_state::{AerodromeV2PoolIdentity, AerodromeV2PoolState};
use crate::balancer_stable_state::{BalancerStablePoolIdentity, BalancerStablePoolState};
use crate::balancer_weighted_state::{BalancerWeightedPoolIdentity, BalancerWeightedPoolState};
use crate::curve_state::{CurvePoolIdentity, CurvePoolState};
use crate::v2_state::{V2PoolIdentity, V2PoolState};
use crate::v3_state::{V3PoolIdentity, V3PoolState};
use crate::v4_state::{V4PoolIdentity, V4PoolState};
use crate::TickInfo;
use alloy::primitives::{Address, U256};
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
        self.seed_known_bitmap_words(tick_spacing);
        self.invalidate_tick_range_cache();
        true
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
