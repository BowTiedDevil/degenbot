//! Reconstruct captured-path pools from [`PoolData`](crate::investigation::PoolData)
//! into either a live `BotState` (via `register_*`) or standalone
//! `V3PoolState`/`V4PoolState` (for the oracle twins). Centralizes the
//! `tick_map`/`v4_pool_id_bytes`/`build_*_state` boilerplate every path-fixture
//! example previously re-derived.

#![expect(clippy::unwrap_used, clippy::expect_used)] // run-once investigation tooling

use std::collections::HashMap;

use alloy::primitives::{aliases::U112, Address, B256, I256, U256};

use crate::bot_core::BotState;
use crate::decoders::v4_swap_decoder::V4PoolId;
use crate::pools::v3_state::{PoolTickCoverage, RegisterV3PoolParams, V3PoolState};
use crate::pools::v4_state::{RegisterV4PoolParams, V4PoolKey, V4PoolState};
use crate::pools::TickInfo;
use crate::{DexVariant, RegisterV2PoolParams};

use super::fixture::{PoolData, TickJson};

/// Convert a fixture's captured tick map (string keys, `liquidity_net/gross`)
/// into the `TickInfo` map the pool states need.
pub fn tick_map(data: &HashMap<String, TickJson>) -> HashMap<i32, TickInfo> {
    data.iter()
        .map(|(t, v)| {
            (
                t.parse::<i32>().unwrap(),
                TickInfo {
                    liquidity_gross: alloy::primitives::U128::from(
                        v.liquidity_gross.parse::<u128>().unwrap(),
                    ),
                    liquidity_net: I256::try_from(v.liquidity_net.parse::<i128>().unwrap())
                        .unwrap(),
                    block: 0,
                },
            )
        })
        .collect()
}

/// Parse a 32-byte `pool_id` hex string (`0x`+64) into the decoder's `V4PoolId`.
pub fn v4_pool_id_bytes(hex: &str) -> V4PoolId {
    hex.trim_start_matches("0x")
        .as_bytes()
        .chunks(2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect::<Vec<u8>>()
        .try_into()
        .unwrap()
}

/// Default V2 fee (Sushi/Uni 0.3%) when the fixture carries no
/// `fee_gamma`/`fee_denom`.
pub const V2_DEFAULT_FEE: (u64, u64) = (997, 1000);

/// Register a captured V2 pool into `core`. Returns the bot's pool id.
///
/// Fee: `fee_gamma`/`fee_denom` from the fixture when present, else the Sushi
/// `997/1000` default (matches the legacy `_v2_get_amount_out` convention the
/// live bot uses).
pub fn register_v2(core: &mut BotState, p: &PoolData) -> Result<u64, String> {
    let fee = match (p.fee_gamma, p.fee_denom) {
        (Some(g), Some(d)) => (g, d),
        _ => V2_DEFAULT_FEE,
    };
    core.register_v2_pool(&RegisterV2PoolParams {
        address: p.address.expect("v2 pool address"),
        token0: p.token0.expect("v2 token0"),
        token1: p.token1.expect("v2 token1"),
        reserve0: U112::try_from(u128::try_from(p.reserve0.expect("v2 reserve0").0).unwrap())
            .unwrap(),
        reserve1: U112::try_from(u128::try_from(p.reserve1.expect("v2 reserve1").0).unwrap())
            .unwrap(),
        fee_token0: fee,
        fee_token1: fee,
        factory: Address::ZERO,
        deployer: Address::ZERO,
        init_hash: B256::ZERO,
        update_block: p.liquidity_update_block.unwrap_or(0),
        variant: DexVariant::UniswapV2,
        stable_swap: false,
        fee_denominator: None,
    })
    .map_err(|e| format!("register_v2: {e:?}"))
}

/// Register a captured V3 pool into `core`. Returns the bot's pool id.
pub fn register_v3(core: &mut BotState, p: &PoolData) -> Result<u64, String> {
    register_v3_with(core, p, None, None)
}

/// `register_v3` with optional scalar overrides (used to probe a DB-stale
/// snapshot against verified-current on-chain `sqrt_price`/`tick`).
pub fn register_v3_with(
    core: &mut BotState,
    p: &PoolData,
    sqrt_override: Option<U256>,
    tick_override: Option<i32>,
) -> Result<u64, String> {
    let sqrt_price_x96 = sqrt_override.unwrap_or(p.sqrt_price_x96.expect("v3 sqrt_price_x96").0);
    let tick = tick_override.unwrap_or(p.tick.expect("v3 tick"));
    core.register_v3_pool(&RegisterV3PoolParams {
        address: p.address.expect("v3 pool address"),
        token0: p.token0.expect("v3 token0"),
        token1: p.token1.expect("v3 token1"),
        fee: p.fee_token0.expect("v3 fee"),
        tick_spacing: p.tick_spacing.expect("v3 tick_spacing"),
        factory: Address::ZERO,
        sqrt_price_x96,
        liquidity: u128::try_from(p.liquidity.expect("v3 liquidity").0).unwrap(),
        tick,
        tick_data: tick_map(&p.tick_data),
        update_block: p.liquidity_update_block.expect("v3 update_block"),
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        deployer: Address::ZERO,
        init_hash: B256::ZERO,
        ..Default::default()
    })
    .map_err(|e| format!("register_v3: {e:?}"))
}

/// Register a captured V4 pool into `core`. Returns the bot's pool id.
pub fn register_v4(core: &mut BotState, p: &PoolData) -> Result<u64, String> {
    register_v4_with(core, p, None)
}

/// `register_v4` with an optional `protocol_fee` override (the DB does not hold
/// live V4 scalars; the fixture carries the captured value, and an investigation
/// may probe an alternate fee).
pub fn register_v4_with(
    core: &mut BotState,
    p: &PoolData,
    protocol_fee_override: Option<u32>,
) -> Result<u64, String> {
    core.register_v4_pool(&RegisterV4PoolParams {
        pool_manager: p.pool_manager.expect("v4 pool_manager"),
        pool_id: v4_pool_id_bytes(p.pool_id.as_ref().expect("v4 pool_id")),
        pool_key: V4PoolKey {
            currency0: p.currency0.expect("v4 currency0"),
            currency1: p.currency1.expect("v4 currency1"),
            fee: p.fee_currency0.expect("v4 fee"),
            tick_spacing: p.tick_spacing.expect("v4 tick_spacing"),
            hooks: Address::ZERO,
        },
        hook_flags: 0,
        protocol_fee: protocol_fee_override.unwrap_or(p.protocol_fee.unwrap_or(0)),
        sqrt_price_x96: p.sqrt_price_x96.expect("v4 sqrt_price_x96").0,
        liquidity: u128::try_from(p.liquidity.expect("v4 liquidity").0).unwrap(),
        tick: p.tick.expect("v4 tick"),
        tick_data: tick_map(&p.tick_data),
        update_block: p.liquidity_update_block.expect("v4 update_block"),
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
    })
    .map_err(|e| format!("register_v4: {e:?}"))
}

/// Build a standalone `V3PoolState` from captured `PoolData` (no `BotState`) —
/// used to drive `v3_simulate_swap` (the tier-3 validated oracle twin).
pub fn build_v3_state(p: &PoolData) -> V3PoolState {
    let (_identity, state) = V3PoolState::from_params(
        RegisterV3PoolParams {
            address: p.address.expect("v3 pool address"),
            token0: p.token0.expect("v3 token0"),
            token1: p.token1.expect("v3 token1"),
            fee: p.fee_token0.expect("v3 fee"),
            tick_spacing: p.tick_spacing.expect("v3 tick_spacing"),
            factory: Address::ZERO,
            sqrt_price_x96: p.sqrt_price_x96.expect("v3 sqrt_price_x96").0,
            liquidity: u128::try_from(p.liquidity.expect("v3 liquidity").0).unwrap(),
            tick: p.tick.expect("v3 tick"),
            tick_data: tick_map(&p.tick_data),
            update_block: p.liquidity_update_block.expect("v3 update_block"),
            tick_data_block: None,
            coverage: PoolTickCoverage::Tracked,
            deployer: Address::ZERO,
            init_hash: B256::ZERO,
            ..Default::default()
        },
        8,
    );
    state
}

/// Build a standalone `V4PoolState` from captured `PoolData` (no `BotState`) —
/// used to drive `v4_simulate_swap`.
pub fn build_v4_state(p: &PoolData) -> V4PoolState {
    let params = RegisterV4PoolParams {
        pool_manager: p.pool_manager.expect("v4 pool_manager"),
        pool_id: v4_pool_id_bytes(p.pool_id.as_ref().expect("v4 pool_id")),
        pool_key: V4PoolKey {
            currency0: p.currency0.expect("v4 currency0"),
            currency1: p.currency1.expect("v4 currency1"),
            fee: p.fee_currency0.expect("v4 fee"),
            tick_spacing: p.tick_spacing.expect("v4 tick_spacing"),
            hooks: Address::ZERO,
        },
        hook_flags: 0,
        protocol_fee: p.protocol_fee.unwrap_or(0),
        sqrt_price_x96: p.sqrt_price_x96.expect("v4 sqrt_price_x96").0,
        liquidity: u128::try_from(p.liquidity.expect("v4 liquidity").0).unwrap(),
        tick: p.tick.expect("v4 tick"),
        tick_data: tick_map(&p.tick_data),
        update_block: p.liquidity_update_block.expect("v4 update_block"),
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
    };
    let (_identity, state) = V4PoolState::from_params(params, 8);
    state
}
