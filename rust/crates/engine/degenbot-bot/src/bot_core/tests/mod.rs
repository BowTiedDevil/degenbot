use super::*;
use crate::bot_core::swap_simulation::{Caveats, SwapOutcome, SwapRead, SwapRequest};
use alloy::primitives::aliases::U112;
use alloy::primitives::uint;
use alloy::primitives::I256;

mod apply_routes;
mod quarantine;
mod registration;
mod snapshots;
mod word_fetch;

/// Exact-input read through the swap-simulation gate (ADR-037) — the
/// replacement for the deleted `calculate_tokens_out_miss_aware` seam.
fn tokens_out(core: &mut BotState, pool_id: u64, zero_for_one: bool, amount_in: U256) -> U256 {
    match core.swap_simulation(
        0,
        pool_id,
        SwapRequest {
            zero_for_one,
            amount_specified: -I256::try_from(amount_in).unwrap(),
            sqrt_price_limit: None,
        },
    ) {
        SwapRead::Computed(outcome) => outcome.delivered_unsigned(),
        f => panic!("small non-overflowing V2 amount; calc must not miss or overflow: {f:?}"),
    }
}

const FEE_03: (u64, u64) = (997, 1000);

fn make_pool_addr() -> Address {
    Address::from([0xaa; 20])
}

fn make_token0() -> Address {
    Address::from([0xbb; 20])
}

fn make_token1() -> Address {
    Address::from([0xcc; 20])
}

fn make_factory() -> Address {
    Address::from([0xdd; 20])
}

fn make_params(r0: U112, r1: U112) -> RegisterV2PoolParams {
    RegisterV2PoolParams {
        address: make_pool_addr(),
        token0: make_token0(),
        token1: make_token1(),
        reserve0: r0,
        reserve1: r1,
        fee_token0: FEE_03,
        fee_token1: FEE_03,
        factory: make_factory(),
        update_block: 0,
        variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
        stable_swap: false,
        fee_denominator: None,
        ..Default::default()
    }
}

// --- V3 restore no-op (reorg on an unrelated pool) ---
fn register_v3(core: &mut BotState, update_block: u64) -> u64 {
    core.register_v3_pool(&RegisterV3PoolParams {
        address: make_pool_addr(),
        token0: make_token0(),
        token1: make_token1(),
        fee: 3_000,
        tick_spacing: 60,
        factory: make_factory(),
        sqrt_price_x96: U256::from(1u64) << 96,
        liquidity: 0,
        tick: 0,
        tick_data: HashMap::new(),
        update_block,
        tick_data_block: None,
        coverage: PoolTickCoverage::Sparse,
        fetcher: None,
        ..Default::default()
    })
    .expect("test setup: V3 registration")
}

// --- buffer appliers push journal deltas + advance update_block ---
/// Register a V3 pool with tick 60 pre-initialized (gross/net 100) and
/// tick 120 absent, so a buffered Mint at [60,120] bumps 60 → 600 and
/// newly initializes 120. Helper does NOT create the `BotState` — the
/// caller must buffer events on the SAME core before calling this.
fn register_v3_on_core(core: &mut BotState, pool_addr: Address, update_block: u64) -> u64 {
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::{RegisterV3PoolParams, TickInfo};
    use alloy::primitives::U128;
    let mut tick_data = HashMap::new();
    tick_data.insert(
        60,
        TickInfo {
            liquidity_gross: U128::from(100),
            liquidity_net: 100i128,
            block: 0,
        },
    );
    core.register_v3_pool(&RegisterV3PoolParams {
        address: pool_addr,
        token0: Address::ZERO,
        token1: Address::from([1u8; 20]),
        fee: 3000,
        tick_spacing: 60,
        factory: Address::ZERO,
        sqrt_price_x96: U256::from(1u128) << 96,
        liquidity: 1_000_000,
        tick: 0,
        tick_data,
        update_block,
        tick_data_block: None,
        coverage: PoolTickCoverage::Sparse,
        fetcher: None,
        ..Default::default()
    })
    .expect("test setup: V3 registration")
}

// --- typed refusals at the swap-read / encode seams ---

#[test]
fn swap_simulation_unknown_pool_is_a_typed_refusal() {
    let mut core = BotState::default();
    let read = core.swap_simulation(
        0,
        999_999,
        SwapRequest {
            zero_for_one: true,
            amount_specified: -I256::try_from(1u64).unwrap(),
            sqrt_price_limit: None,
        },
    );
    assert_eq!(read, SwapRead::UnknownPool { pool_id: 999_999 });
}

#[test]
fn encode_swap_refuses_an_unregistered_pool() {
    let core = BotState::default();
    let err = core
        .encode_swap(999_999, true, U256::from(1u64), Address::ZERO)
        .unwrap_err();
    assert!(matches!(
        err,
        EncodeSwapError::NotRegistered { pool_id: 999_999 }
    ));
}

#[test]
fn seed_genesis_refuses_a_non_cl_family() {
    let mut core = BotState::new();
    let v2_id = core
        .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
        .expect("test setup: V2 registration");
    let err = core.seed_genesis_by_pool_id(v2_id, 5).unwrap_err();
    assert_eq!(
        err,
        ClApplyError::UnsupportedFamily {
            pool_id: v2_id,
            family: "v2",
            op: "genesis seed"
        }
    );
}

#[test]
fn apply_liquidity_update_refuses_a_non_cl_family() {
    let mut core = BotState::new();
    let v2_id = core
        .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
        .expect("test setup: V2 registration");
    let err = core
        .apply_liquidity_update_by_pool_id(v2_id, 60, 120, 500, 5)
        .unwrap_err();
    assert_eq!(
        err,
        ClApplyError::UnsupportedFamily {
            pool_id: v2_id,
            family: "v2",
            op: "liquidity update"
        }
    );
}

#[test]
fn encode_swap_refuses_a_family_without_an_encoder() {
    let mut core = BotState::new();
    let v3_id = register_v3(&mut core, 0);
    let err = core
        .encode_swap(v3_id, true, U256::from(1u64), Address::ZERO)
        .unwrap_err();
    assert!(matches!(
        err,
        EncodeSwapError::UnsupportedFamily { pool_id, family }
            if pool_id == v3_id && family == "v3"
    ));
}
