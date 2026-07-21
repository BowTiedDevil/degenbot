//! Tier-2 behavioral dual-driver parity — V4 concentrated-liquidity swap
//! (ADR-005 standalone claim, the behavioral tier).
//!
//! The V4 dual-driver seed. V4 uses the **same CL math** as V3 but the
//! `amountSpecified` sign convention flips (V4 exact-input is *negative*,
//! opposite to V3's positive — see `simulate_swap`'s `PoolEntry::V4` arm).
//! So this test exercises a distinct FFI-seam path from the V3 seed: the
//! Rust core's sign flip in `simulate_swap` + the V4 `register_v4_pool`
//! admission (pool_manager + pool_id + pool_key + hook_flags). Divergence
//! between the Rust and Python consumers = a lossy seam on the V4 path.
//!
//! Like the V3 seed, the CL swap has no simple closed form, so the shared
//! oracle is the recorded constant `EXPECTED_AMOUNT_OUT_ZFO` (deterministic
//! — both consumers hit one `BotState::calculate_tokens_out_miss_aware` core). Plus
//! monotonicity + direction-symmetry sanity checks.
//!
//! The matching Python side lives at
//! `tests/standalone_parity/test_v4_swap_dual_driver.py`.
//!
//! ## Fixture (the shared contract)
//!
//! A 1:1-price V4 pool (sqrt_price_x96 = 2^96, tick 0), liquidity 1e12, fee
//! 500 (0.05%), tick_spacing 10, no hooks (`hook_flags = 0`), with tick 0
//! seeded + `Tracked` coverage. `amount_in = 1_000_000_000` → deterministic
//! output `998_501_997` (symmetric for zfo/ofz at the 1:1 price).
//!
//! Note the fee differs from the V3 seed (500 vs 3000) — V4's lower default
//! fee tier produces a distinct output, ruling out an accidental
//! hardcoding of the V3 constant.

#![allow(clippy::panic_in_result_fn, clippy::doc_markdown)]

use alloy::primitives::{address, I256, U256};
use degenbot::bot_core::BotState;
use degenbot::{RegisterV4PoolParams, V4PoolKey};
use degenbot_pools::v3_state::PoolTickCoverage;
use degenbot_pools::TickInfo;
use std::collections::HashMap;

// ---- the shared canonical fixture (mirror in the Python parity test) ----
const POOL_MANAGER: alloy::primitives::Address =
    address!("0000000000000000000000000000000000000044");
const CURRENCY0: alloy::primitives::Address = address!("0000000000000000000000000000000000000000");
const CURRENCY1: alloy::primitives::Address = address!("0000000000000000000000000000000000000001");
const HOOKS: alloy::primitives::Address = address!("0000000000000000000000000000000000000000");

const FEE: u32 = 500; // 0.05% (V4 default-low tier — distinct from V3's 3000)
const TICK_SPACING: i32 = 10;
const POOL_ID: [u8; 32] = [0xee; 32];
/// A 1:1 price: sqrt_price_x96 = 2^96 (the Q96.32 repr of 1.0).
const SQRT_PRICE_1TO1: U256 = U256::from_limbs([0, 1 << 32, 0, 0]);
const LIQUIDITY: u128 = 1_000_000_000_000; // 1e12
const TICK: i32 = 0;

const AMOUNT_IN: u128 = 1_000_000_000; // 1e9 — small, stays in-tick
const ZERO_FOR_ONE: bool = true; // currency0 → currency1

/// Canonical V4 swap output, deterministic from the fixture above. Symmetric
/// for zfo/ofz at the 1:1 price. Both consumers MUST reproduce this.
const EXPECTED_AMOUNT_OUT_ZFO: u128 = 998_501_997;

#[test]
fn standalone_rust_consumer_v4_swap_matches_recorded_constant() {
    // Tier-2 dual-driver gate — Rust consumer side (V4 CL path).
    //
    // V4 uses the same CL math as V3 but the `amountSpecified` sign flips
    // (V4 exact-input is negative — see `simulate_swap`'s `PoolEntry::V4`
    // arm's `-spec`). So this exercises a distinct FFI-seam path from the
    // V3 seed: the Rust core's sign flip + the V4 `register_v4_pool`
    // admission (pool_manager + pool_id + pool_key + hook_flags).
    let mut bot = BotState::new();
    let mut tick_data = HashMap::new();
    tick_data.insert(
        0,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(LIQUIDITY),
            liquidity_net: I256::ZERO,
            block: 0,
        },
    );
    let params = RegisterV4PoolParams {
        pool_manager: POOL_MANAGER,
        pool_id: POOL_ID,
        pool_key: V4PoolKey {
            currency0: CURRENCY0,
            currency1: CURRENCY1,
            fee: FEE,
            tick_spacing: TICK_SPACING,
            hooks: HOOKS,
        },
        hook_flags: 0,
        sqrt_price_x96: SQRT_PRICE_1TO1,
        liquidity: LIQUIDITY,
        tick: TICK,
        tick_data,
        update_block: 0,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
    };
    let pid = bot
        .register_v4_pool(&params)
        .expect("register canonical V4 pool");
    assert_eq!(pid, 1, "first registered pool gets id 1 (parity contract)");

    let amount_out = bot
        .calculate_tokens_out_miss_aware(pid, ZERO_FOR_ONE, U256::from(AMOUNT_IN))
        .expect(
            "small in-tick amount with pre-populated tick data; V4 calc must not miss or overflow",
        );
    assert_eq!(
        amount_out,
        U256::from(EXPECTED_AMOUNT_OUT_ZFO),
        "standalone Rust consumer V4 swap must match the recorded constant"
    );

    // Monotonicity (in-tick).
    let bigger_in = U256::from(AMOUNT_IN) * U256::from(10_u64);
    let bigger_out = bot
        .calculate_tokens_out_miss_aware(pid, ZERO_FOR_ONE, bigger_in)
        .expect(
            "in-tick 10x amount with pre-populated tick data; V4 calc must not miss or overflow",
        );
    assert!(
        bigger_out > amount_out,
        "V4 in-tick swap must be monotonic: {bigger_out} !> {amount_out}"
    );

    // Direction symmetry at the 1:1 price (catches a V4 sign-flip regression).
    let ofz_out = bot
        .calculate_tokens_out_miss_aware(pid, false, U256::from(AMOUNT_IN))
        .expect(
            "small in-tick amount with pre-populated tick data; V4 calc must not miss or overflow",
        );
    assert_eq!(
        ofz_out, amount_out,
        "1:1-price V4 swap must be direction-symmetric (zfo == ofz)"
    );
}
