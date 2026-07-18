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

/// ERC20 token metadata.
#[derive(Clone, Debug)]
pub struct TokenEntry {
    pub address: Address,
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
    pub chain_id: u64,
}
