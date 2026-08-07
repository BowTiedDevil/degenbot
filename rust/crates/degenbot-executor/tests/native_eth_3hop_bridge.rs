// Structural + byte-exact tests for the native-ETH↔WETH wrap/unwrap bridge
// in 3-hop V4 composers (ergo epic GVK2RY).
//
// The existing `composers_3hop_parity.rs` uses WETH/USDC/WBTC only (never
// address(0) as a V4 currency), so the bridge was never exercised. These tests
// build the expected bytes from the individual `enc_*` primitives — if the
// composer emits the right opcodes in the right order, the bytes match.

#![allow(clippy::too_many_lines, clippy::unreadable_literal)]

use alloy::primitives::{address, Address};
use degenbot_executor::composers::{encode_cmd_3_hop, EncodeOptions, HopInfo, PathInfo, V4HopInfo};
use degenbot_executor::encoders::{
    self, AddressTable, SENTINEL_NATIVE, SENTINEL_SELF, SENTINEL_WETH,
};

const PM: Address = address!("000000000004444c5dc75cB358380D2e3De08A90");
const EXECUTOR: Address = address!("DeAd0000000000000000000000000000000000Be");
const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
const USDC: Address = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
const NATIVE: Address = Address::ZERO;

const OPTIMAL_INPUT: u128 = 1_000_000_000_000_000_000;
const OUT_A: u128 = 500_000_000_000_000_000;
const OUT_B: u128 = 1_900_000_000_000_000_000;
const OUT_C: u128 = 1_000_000_000_000_000_000;

/// V4-V4-V4 with a native→WETH gap at the A→B boundary (`bridge_ab` = Wrap).
///
/// Hop A: `c0=USDC, c1=NATIVE, zfo=true` → input=USDC, output=native.
/// Hop B: `c0=WETH, c1=USDC, zfo=true` → input=WETH, output=USDC.
///   → `bridge_ab = at_boundary(native, WETH) = Wrap` ✓
/// Hop C: `c0=USDC, c1=WETH, zfo=true` → input=USDC, output=WETH.
///   → `bridge_bc = at_boundary(USDC, USDC) = None` ✓
#[test]
fn v4_v4_v4_wrap_at_ab_boundary() {
    let hop_a = V4HopInfo {
        pool_manager_address: PM,
        pool_id_hex: "0x1111".to_string(),
        currency0_address: USDC,
        currency1_address: NATIVE,
        fee: 500,
        tick_spacing: 10,
        hook_address: Address::ZERO,
        zfo: true, // input=USDC, output=native
    };
    let hop_b = V4HopInfo {
        pool_manager_address: PM,
        pool_id_hex: "0x2222".to_string(),
        currency0_address: WETH,
        currency1_address: USDC,
        fee: 3000,
        tick_spacing: 60,
        hook_address: Address::ZERO,
        zfo: true, // input=WETH, output=USDC
    };
    let hop_c = V4HopInfo {
        pool_manager_address: PM,
        pool_id_hex: "0x3333".to_string(),
        currency0_address: USDC,
        currency1_address: WETH,
        fee: 500,
        tick_spacing: 10,
        hook_address: Address::ZERO,
        zfo: true, // input=USDC, output=WETH
    };

    let rust = encode_cmd_3_hop(
        &PathInfo::new(vec![
            HopInfo::V4(hop_a),
            HopInfo::V4(hop_b),
            HopInfo::V4(hop_c),
        ]),
        OPTIMAL_INPUT,
        &[OUT_A, OUT_B, OUT_C],
        &[OUT_A, OUT_B, OUT_C],
        EXECUTOR,
        PM,
        WETH,
        EncodeOptions::default(),
    );

    // Build the expected bytes from primitives.
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let usdc_idx = at.add(USDC).unwrap(); // index 0
    let native_idx = SENTINEL_NATIVE;
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let zero_idx = SENTINEL_NATIVE; // no-hooks sentinel

    let mut inner = Vec::new();
    // 1. V4_SWAP_COMPACT(A) — input=USDC(c0), output=native(c1), zfo=true
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            usdc_idx,
            native_idx,
            500,
            10,
            zero_idx,
            true,
            OPTIMAL_INPUT,
        )
        .unwrap(),
    );
    // 2. bridge_ab = Wrap: TAKE(native) + WETH_DEPOSIT
    inner.extend_from_slice(
        &encoders::enc_v4_take_compact(native_idx, executor_idx, OUT_A).unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_weth_deposit(alloy::primitives::U256::from(
        OUT_A,
    )));
    // 3. V4_SWAP_COMPACT(B) — input=WETH(c0), output=USDC(c1), zfo=true, amount=OUT_A
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(weth_idx, usdc_idx, 3000, 60, zero_idx, true, OUT_A)
            .unwrap(),
    );
    // 4. V4_SETTLE_DELTA(WETH) — B's input
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(weth_idx));
    // 5. V4_SWAP_DYNAMIC(C) — no gap at B→C
    inner.extend_from_slice(&encoders::enc_v4_swap_dynamic(
        usdc_idx, weth_idx, 500, 10, zero_idx, true,
    ));
    // 6. Profit capture: output=WETH → TAKE_DELTA(WETH, executor)
    inner.extend_from_slice(&encoders::enc_v4_take_delta(weth_idx, executor_idx));
    // 7. V4_SETTLE_ALL
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);

    assert_eq!(rust, Some(expected));
}

/// V4-V4-V4 with a WETH→native gap at the A→B boundary (`bridge_ab` = Unwrap).
///
/// Hop A: native→WETH (c0=native, c1=WETH, zfo=true → input=native, output=WETH)
/// Hop B: native→USDC (c0=native, c1=USDC, zfo=true → input=native, output=USDC)
///   → `bridge_ab` = `at_boundary(WETH`, native) = Unwrap ✓
/// Hop C: USDC→native (c0=USDC, c1=native, zfo=true → input=USDC, output=native)
///   → `bridge_bc` = `at_boundary(USDC`, USDC) = None ✓
#[test]
fn v4_v4_v4_unwrap_at_ab_boundary() {
    let hop_a = V4HopInfo {
        pool_manager_address: PM,
        pool_id_hex: "0x1111".to_string(),
        currency0_address: NATIVE,
        currency1_address: WETH,
        fee: 500,
        tick_spacing: 10,
        hook_address: Address::ZERO,
        zfo: true,
    };
    let hop_b = V4HopInfo {
        pool_manager_address: PM,
        pool_id_hex: "0x2222".to_string(),
        currency0_address: NATIVE,
        currency1_address: USDC,
        fee: 3000,
        tick_spacing: 60,
        hook_address: Address::ZERO,
        zfo: true,
    };
    let hop_c = V4HopInfo {
        pool_manager_address: PM,
        pool_id_hex: "0x3333".to_string(),
        currency0_address: USDC,
        currency1_address: NATIVE,
        fee: 500,
        tick_spacing: 10,
        hook_address: Address::ZERO,
        zfo: true,
    };

    let rust = encode_cmd_3_hop(
        &PathInfo::new(vec![
            HopInfo::V4(hop_a),
            HopInfo::V4(hop_b),
            HopInfo::V4(hop_c),
        ]),
        OPTIMAL_INPUT,
        &[OUT_A, OUT_B, OUT_C],
        &[OUT_A, OUT_B, OUT_C],
        EXECUTOR,
        PM,
        WETH,
        EncodeOptions::default(),
    );

    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let usdc_idx = at.add(USDC).unwrap();
    let native_idx = SENTINEL_NATIVE;
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let zero_idx = SENTINEL_NATIVE;

    let mut inner = Vec::new();
    // V4_SWAP_COMPACT(A) — native→WETH
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            native_idx,
            weth_idx,
            500,
            10,
            zero_idx,
            true,
            OPTIMAL_INPUT,
        )
        .unwrap(),
    );
    // bridge_ab = Unwrap: TAKE(WETH) + WETH_WITHDRAW
    inner.extend_from_slice(&encoders::enc_v4_take_compact(weth_idx, executor_idx, OUT_A).unwrap());
    inner.extend_from_slice(&encoders::enc_weth_withdraw(alloy::primitives::U256::from(
        OUT_A,
    )));
    // V4_SWAP_COMPACT(B) — native→USDC, amount=OUT_A
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(native_idx, usdc_idx, 3000, 60, zero_idx, true, OUT_A)
            .unwrap(),
    );
    // V4_SETTLE_DELTA(native) — B's input
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(native_idx));
    // V4_SWAP_DYNAMIC(C) — USDC→native, no gap
    inner.extend_from_slice(&encoders::enc_v4_swap_dynamic(
        usdc_idx, native_idx, 500, 10, zero_idx, true,
    ));
    // Profit capture: output=native
    inner.extend_from_slice(&encoders::enc_v4_take_delta(native_idx, executor_idx));
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);

    assert_eq!(rust, Some(expected));
}

/// V4-V4-V4 with gaps at BOTH boundaries (Wrap at A→B + Wrap at B→C).
///
/// Requires hop B to be a native/WETH pool so both its input (WETH) and
/// output (native) sit on the native↔WETH axis. Economically degenerate
/// (a native/WETH pool is a trivial wrap), so the path graph rarely
/// produces this — but the encoder must still handle it: the two bridges
/// are independent and the code paths compose.
///
/// Hop A: `c0=USDC, c1=NATIVE, zfo=true` → input=USDC, output=native.
/// Hop B: `c0=WETH, c1=NATIVE, zfo=true` → input=WETH, output=native.
///   → `bridge_ab = at_boundary(native, WETH) = Wrap` ✓
/// Hop C: `c0=WETH, c1=USDC, zfo=true` → input=WETH, output=USDC.
///   → `bridge_bc = at_boundary(native, WETH) = Wrap` ✓
#[test]
fn v4_v4_v4_double_gap_both_boundaries_bridge() {
    let hop_a = V4HopInfo {
        pool_manager_address: PM,
        pool_id_hex: "0x1111".to_string(),
        currency0_address: USDC,
        currency1_address: NATIVE,
        fee: 500,
        tick_spacing: 10,
        hook_address: Address::ZERO,
        zfo: true, // input=USDC, output=native
    };
    let hop_b = V4HopInfo {
        pool_manager_address: PM,
        pool_id_hex: "0x2222".to_string(),
        currency0_address: WETH,
        currency1_address: NATIVE,
        fee: 3000,
        tick_spacing: 60,
        hook_address: Address::ZERO,
        zfo: true, // input=WETH, output=native
    };
    let hop_c = V4HopInfo {
        pool_manager_address: PM,
        pool_id_hex: "0x3333".to_string(),
        currency0_address: WETH,
        currency1_address: USDC,
        fee: 500,
        tick_spacing: 10,
        hook_address: Address::ZERO,
        zfo: true, // input=WETH, output=USDC
    };

    let rust = encode_cmd_3_hop(
        &PathInfo::new(vec![
            HopInfo::V4(hop_a),
            HopInfo::V4(hop_b),
            HopInfo::V4(hop_c),
        ]),
        OPTIMAL_INPUT,
        &[OUT_A, OUT_B, OUT_C],
        &[OUT_A, OUT_B, OUT_C],
        EXECUTOR,
        PM,
        WETH,
        EncodeOptions::default(),
    );

    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let usdc_idx = at.add(USDC).unwrap();
    let native_idx = SENTINEL_NATIVE;
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let zero_idx = SENTINEL_NATIVE;

    let mut inner = Vec::new();
    // 1. V4_SWAP_COMPACT(A) — USDC→native
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            usdc_idx,
            native_idx,
            500,
            10,
            zero_idx,
            true,
            OPTIMAL_INPUT,
        )
        .unwrap(),
    );
    // 2. bridge_ab = Wrap: TAKE(native) + WETH_DEPOSIT(OUT_A)
    inner.extend_from_slice(
        &encoders::enc_v4_take_compact(native_idx, executor_idx, OUT_A).unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_weth_deposit(alloy::primitives::U256::from(
        OUT_A,
    )));
    // 3. V4_SWAP_COMPACT(B) — WETH→native, amount=OUT_A
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(weth_idx, native_idx, 3000, 60, zero_idx, true, OUT_A)
            .unwrap(),
    );
    // 4. V4_SETTLE_DELTA(WETH) — B's input (post-wrap)
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(weth_idx));
    // 5. bridge_bc = Wrap: TAKE(native) + WETH_DEPOSIT(OUT_B)
    inner.extend_from_slice(
        &encoders::enc_v4_take_compact(native_idx, executor_idx, OUT_B).unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_weth_deposit(alloy::primitives::U256::from(
        OUT_B,
    )));
    // 6. V4_SWAP_COMPACT(C) — WETH→USDC, amount=OUT_B
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(weth_idx, usdc_idx, 500, 10, zero_idx, true, OUT_B).unwrap(),
    );
    // 7. V4_SETTLE_DELTA(WETH) — C's input (post-wrap)
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(weth_idx));
    // 8. Profit capture: output=USDC → TAKE_DELTA(USDC, executor)
    inner.extend_from_slice(&encoders::enc_v4_take_delta(usdc_idx, executor_idx));
    // 9. V4_SETTLE_ALL
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);

    assert_eq!(rust, Some(expected));
}

/// V4-V4-V4 with a native→WETH gap at the B→C boundary (`bridge_bc` = Wrap).
///
/// Hop A: USDC→WETH (output WETH, no gap at A→B since B inputs WETH)
/// Hop B: WETH→native (input WETH, output native)
/// Hop C: WETH→USDC (input WETH) → `bridge_bc` = `at_boundary(native`, WETH) = Wrap
#[test]
fn v4_v4_v4_wrap_at_bc_boundary() {
    let hop_a = V4HopInfo {
        pool_manager_address: PM,
        pool_id_hex: "0x1111".to_string(),
        currency0_address: USDC,
        currency1_address: WETH,
        fee: 500,
        tick_spacing: 10,
        hook_address: Address::ZERO,
        zfo: true,
    };
    let hop_b = V4HopInfo {
        pool_manager_address: PM,
        pool_id_hex: "0x2222".to_string(),
        currency0_address: WETH,
        currency1_address: NATIVE,
        fee: 3000,
        tick_spacing: 60,
        hook_address: Address::ZERO,
        zfo: true,
    };
    let hop_c = V4HopInfo {
        pool_manager_address: PM,
        pool_id_hex: "0x3333".to_string(),
        currency0_address: WETH,
        currency1_address: USDC,
        fee: 500,
        tick_spacing: 10,
        hook_address: Address::ZERO,
        zfo: true,
    };

    let rust = encode_cmd_3_hop(
        &PathInfo::new(vec![
            HopInfo::V4(hop_a),
            HopInfo::V4(hop_b),
            HopInfo::V4(hop_c),
        ]),
        OPTIMAL_INPUT,
        &[OUT_A, OUT_B, OUT_C],
        &[OUT_A, OUT_B, OUT_C],
        EXECUTOR,
        PM,
        WETH,
        EncodeOptions::default(),
    );

    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let usdc_idx = at.add(USDC).unwrap();
    let weth_idx = SENTINEL_WETH;
    let native_idx = SENTINEL_NATIVE;
    let executor_idx = SENTINEL_SELF;
    let zero_idx = SENTINEL_NATIVE;

    let mut inner = Vec::new();
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(usdc_idx, weth_idx, 500, 10, zero_idx, true, OPTIMAL_INPUT)
            .unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_v4_swap_dynamic(
        weth_idx, native_idx, 3000, 60, zero_idx, true,
    ));
    // bridge_bc = Wrap: TAKE(native) + WETH_DEPOSIT
    inner.extend_from_slice(
        &encoders::enc_v4_take_compact(native_idx, executor_idx, OUT_B).unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_weth_deposit(alloy::primitives::U256::from(
        OUT_B,
    )));
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(weth_idx, usdc_idx, 500, 10, zero_idx, true, OUT_B).unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(weth_idx));
    inner.extend_from_slice(&encoders::enc_v4_take_delta(usdc_idx, executor_idx));
    inner.extend_from_slice(&encoders::enc_v4_settle_all());

    let commands = encoders::enc_v4_unlock(&inner).unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);

    assert_eq!(rust, Some(expected));
}

/// When `use_v4_batch` is set but a gap exists, the composer must fall
/// through to the compact path (batch can't bridge a gap).
#[test]
fn v4_v4_v4_batch_with_gap_falls_through_to_compact() {
    let hop_a = V4HopInfo {
        pool_manager_address: PM,
        pool_id_hex: "0x1111".to_string(),
        currency0_address: USDC,
        currency1_address: NATIVE,
        fee: 500,
        tick_spacing: 10,
        hook_address: Address::ZERO,
        zfo: true,
    };
    let hop_b = V4HopInfo {
        pool_manager_address: PM,
        pool_id_hex: "0x2222".to_string(),
        currency0_address: WETH,
        currency1_address: USDC,
        fee: 3000,
        tick_spacing: 60,
        hook_address: Address::ZERO,
        zfo: true,
    };
    let hop_c = V4HopInfo {
        pool_manager_address: PM,
        pool_id_hex: "0x3333".to_string(),
        currency0_address: USDC,
        currency1_address: WETH,
        fee: 500,
        tick_spacing: 10,
        hook_address: Address::ZERO,
        zfo: true,
    };

    let path = PathInfo::new(vec![
        HopInfo::V4(hop_a),
        HopInfo::V4(hop_b),
        HopInfo::V4(hop_c),
    ]);
    let batch_result = encode_cmd_3_hop(
        &path,
        OPTIMAL_INPUT,
        &[OUT_A, OUT_B, OUT_C],
        &[OUT_A, OUT_B, OUT_C],
        EXECUTOR,
        PM,
        WETH,
        EncodeOptions {
            use_v4_batch: true,
            ..Default::default()
        },
    );
    let compact_result = encode_cmd_3_hop(
        &path,
        OPTIMAL_INPUT,
        &[OUT_A, OUT_B, OUT_C],
        &[OUT_A, OUT_B, OUT_C],
        EXECUTOR,
        PM,
        WETH,
        EncodeOptions::default(),
    );
    assert_eq!(batch_result, compact_result);
    assert!(
        batch_result.is_some(),
        "batch-with-gap must still produce bytes"
    );
}
