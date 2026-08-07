// 2-hop composer regression vectors.
//
// Golden-master expected bytes for `encode_cmd_stream`'s 2-hop paths and the
// `V4V4`/`V4V3`/`CmdExecutorComposer` payload builders. The Rust `enc_*`
// primitive sequence is the canonical opcode source (ADR-005); these
// constants record the composed bytestream so a composer change that alters
// output is a visible, reviewable diff. The native-ETH / WETH-bridge shapes —
// where the opcode ORDER is the risk — are covered by `enc_*`-derived
// expectations in `native_eth_3hop_bridge.rs` and the `native_v4_*` files.

#![allow(
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::needless_pass_by_value,
    clippy::similar_names
)]

use alloy::primitives::{address, Address, U256};
use degenbot_executor::composers::{
    self, encode_cmd_stream, encode_execute_call, CmdExecutorComposer, EncodeOptions, HopInfo,
    PathInfo, V2HopInfo, V3HopInfo, V4HopInfo, V4PoolKeyConfig, V4SwapAmounts,
    V4V3ArbitragePayload, V4V4ArbitragePayload,
};
use degenbot_executor::encoders::{
    self, AddressTable, V4BatchEntry, SENTINEL_NATIVE, SENTINEL_SELF, SENTINEL_WETH,
};

const PM: Address = address!("000000000004444c5dc75cB358380D2e3dE08A90");
const EXECUTOR: Address = address!("DeAd0000000000000000000000000000000000Be");
const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
const USDC: Address = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");

/// Build `enc_preamble(&at) + enc_v4_unlock(&inner)` — the V4 envelope every
/// V4-containing composer wraps around its `inner` opcode sequence. This is
/// NOT the opcode order under test (the `inner` appends below are); factoring
/// the envelope cuts boilerplate without re-introducing copy-paste drift.
fn v4_envelope(at: &AddressTable, inner: &[u8]) -> Vec<u8> {
    let mut out = encoders::enc_preamble(at);
    out.extend_from_slice(&encoders::enc_v4_unlock(inner).unwrap());
    out
}

fn hx(s: &[u8]) -> Vec<u8> {
    s.to_vec()
}

#[test]
fn parity_v4v4_same_currency() {
    let rust = encode_cmd_stream(
        &PathInfo::new(vec![
            HopInfo::V4(V4HopInfo {
                pool_manager_address: PM,
                pool_id_hex: "0x1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
                currency0_address: WETH,
                currency1_address: USDC,
                fee: 3000u32,
                tick_spacing: 60i32,
                hook_address: Address::ZERO,
                zfo: true,
            }),
            HopInfo::V4(V4HopInfo {
                pool_manager_address: PM,
                pool_id_hex: "0x1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
                currency0_address: USDC,
                currency1_address: WETH,
                fee: 500u32,
                tick_spacing: 10i32,
                hook_address: Address::ZERO,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128],
        &[2000000000u128, 2001000000000000000u128],
        EXECUTOR,
        PM,
        WETH,
        EncodeOptions::default(),
    );
    // Expected: same intermediate currency (USDC) — delta netting, no bridge.
    // A: V4_SWAP_COMPACT(WETH→USDC, exact-in 1e18).  B: V4_SWAP_DYNAMIC reads
    // A's PM delta.  Profit=WETH → TAKE_DELTA(WETH→executor).  SETTLE_ALL.
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let usdc_idx = at.add(USDC).unwrap(); // 0
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let zero_idx = SENTINEL_NATIVE; // no-hooks sentinel
    let mut inner = Vec::new();
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            weth_idx,
            usdc_idx,
            3000,
            60,
            zero_idx,
            true,
            1000000000000000000u128,
        )
        .unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_v4_swap_dynamic(
        usdc_idx, weth_idx, 500, 10, zero_idx, true,
    ));
    inner.extend_from_slice(&encoders::enc_v4_take_delta(weth_idx, executor_idx));
    inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let expected = v4_envelope(&at, &inner);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v4v4_native_to_weth_wrap() {
    let rust = encode_cmd_stream(
        &PathInfo::new(vec![
            HopInfo::V4(V4HopInfo {
                pool_manager_address: address!("000000000004444c5dc75cB358380D2e3dE08A90"),
                pool_id_hex: "0x1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
                currency0_address: address!("0000000000000000000000000000000000000000"),
                currency1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 3000u32,
                tick_spacing: 60i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: false,
            }),
            HopInfo::V4(V4HopInfo {
                pool_manager_address: address!("000000000004444c5dc75cB358380D2e3dE08A90"),
                pool_id_hex: "0x1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
                currency0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                currency1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 500u32,
                tick_spacing: 10i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: false,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128],
        &[2000000000u128, 2001000000000000000u128],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let idx0 = at.add(USDC).unwrap(); // 0
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let native_idx = SENTINEL_NATIVE;
    let mut inner = Vec::new();
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            native_idx,
            idx0,
            3000,
            60,
            native_idx,
            false,
            1000000000000000000u128,
        )
        .unwrap(),
    );
    inner.extend_from_slice(
        &encoders::enc_v4_take_compact(native_idx, executor_idx, 2000000000u128).unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_weth_deposit(U256::from(2000000000u128)));
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(weth_idx, idx0, 500, 10, native_idx, false, 2000000000u128)
            .unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(weth_idx));
    inner.extend_from_slice(&encoders::enc_v4_take_delta(weth_idx, executor_idx));
    inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let expected = v4_envelope(&at, &inner);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v4v4_weth_to_native_unwrap() {
    let rust = encode_cmd_stream(
        &PathInfo::new(vec![
            HopInfo::V4(V4HopInfo {
                pool_manager_address: address!("000000000004444c5dc75cB358380D2e3dE08A90"),
                pool_id_hex: "0x1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
                currency0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                currency1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 3000u32,
                tick_spacing: 60i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: true,
            }),
            HopInfo::V4(V4HopInfo {
                pool_manager_address: address!("000000000004444c5dc75cB358380D2e3dE08A90"),
                pool_id_hex: "0x1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
                currency0_address: address!("0000000000000000000000000000000000000000"),
                currency1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 500u32,
                tick_spacing: 10i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: true,
            }),
        ]),
        2000000000u128,
        &[1000000000000000000u128, 2001000000000000000u128],
        &[1000000000000000000u128, 2001000000000000000u128],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let idx0 = at.add(USDC).unwrap(); // 0
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let native_idx = SENTINEL_NATIVE;
    let mut inner = Vec::new();
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(idx0, weth_idx, 3000, 60, native_idx, true, 2000000000u128)
            .unwrap(),
    );
    inner.extend_from_slice(
        &encoders::enc_v4_take_compact(weth_idx, executor_idx, 1000000000000000000u128).unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_weth_withdraw(U256::from(
        1000000000000000000u128,
    )));
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            native_idx,
            idx0,
            500,
            10,
            native_idx,
            true,
            1000000000000000000u128,
        )
        .unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(native_idx));
    inner.extend_from_slice(&encoders::enc_v4_take_delta(idx0, executor_idx));
    inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let expected = v4_envelope(&at, &inner);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v4v4_same_currency_batch() {
    let rust = encode_cmd_stream(
        &PathInfo::new(vec![
            HopInfo::V4(V4HopInfo {
                pool_manager_address: address!("000000000004444c5dc75cB358380D2e3dE08A90"),
                pool_id_hex: "0x1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
                currency0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                currency1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 3000u32,
                tick_spacing: 60i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: true,
            }),
            HopInfo::V4(V4HopInfo {
                pool_manager_address: address!("000000000004444c5dc75cB358380D2e3dE08A90"),
                pool_id_hex: "0x1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
                currency0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                currency1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 500u32,
                tick_spacing: 10i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128],
        &[2000000000u128, 2001000000000000000u128],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions {
            erc6909_profit: false,
            use_v4_batch: true,
        },
    );
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let idx0 = at.add(USDC).unwrap(); // 0
    let weth_idx = SENTINEL_WETH;
    let native_idx = SENTINEL_NATIVE;
    let mut inner = Vec::new();
    let batch = [
        V4BatchEntry {
            c0_idx: weth_idx,
            c1_idx: idx0,
            fee: 3000,
            tick_spacing: 60,
            hooks_idx: native_idx,
            zfo: true,
            amount_u96: 1000000000000000000u128,
        },
        V4BatchEntry {
            c0_idx: idx0,
            c1_idx: weth_idx,
            fee: 500,
            tick_spacing: 10,
            hooks_idx: native_idx,
            zfo: true,
            amount_u96: 0u128,
        },
    ];
    inner.extend_from_slice(&encoders::enc_v4_batch(&batch).unwrap());
    inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let expected = v4_envelope(&at, &inner);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v4v4_same_currency_erc6909() {
    let rust = encode_cmd_stream(
        &PathInfo::new(vec![
            HopInfo::V4(V4HopInfo {
                pool_manager_address: address!("000000000004444c5dc75cB358380D2e3dE08A90"),
                pool_id_hex: "0x1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
                currency0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                currency1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 3000u32,
                tick_spacing: 60i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: true,
            }),
            HopInfo::V4(V4HopInfo {
                pool_manager_address: address!("000000000004444c5dc75cB358380D2e3dE08A90"),
                pool_id_hex: "0x1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
                currency0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                currency1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 500u32,
                tick_spacing: 10i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128],
        &[2000000000u128, 2001000000000000000u128],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions {
            erc6909_profit: true,
            use_v4_batch: false,
        },
    );
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let idx0 = at.add(USDC).unwrap(); // 0
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let native_idx = SENTINEL_NATIVE;
    let mut inner = Vec::new();
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            weth_idx,
            idx0,
            3000,
            60,
            native_idx,
            true,
            1000000000000000000u128,
        )
        .unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_v4_swap_dynamic(
        idx0, weth_idx, 500, 10, native_idx, true,
    ));
    inner.extend_from_slice(
        &encoders::enc_v4_mint_compact(weth_idx, executor_idx, 1001000000000000000u128).unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let expected = v4_envelope(&at, &inner);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v4v4_weth_to_native_unwrap_batch() {
    let rust = encode_cmd_stream(
        &PathInfo::new(vec![
            HopInfo::V4(V4HopInfo {
                pool_manager_address: address!("000000000004444c5dc75cB358380D2e3dE08A90"),
                pool_id_hex: "0x1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
                currency0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                currency1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 3000u32,
                tick_spacing: 60i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: true,
            }),
            HopInfo::V4(V4HopInfo {
                pool_manager_address: address!("000000000004444c5dc75cB358380D2e3dE08A90"),
                pool_id_hex: "0x1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
                currency0_address: address!("0000000000000000000000000000000000000000"),
                currency1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 500u32,
                tick_spacing: 10i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: true,
            }),
        ]),
        2000000000u128,
        &[1000000000000000000u128, 2001000000000000000u128],
        &[1000000000000000000u128, 2001000000000000000u128],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions {
            erc6909_profit: false,
            use_v4_batch: true,
        },
    );
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let idx0 = at.add(USDC).unwrap(); // 0
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let native_idx = SENTINEL_NATIVE;
    let mut inner = Vec::new();
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(idx0, weth_idx, 3000, 60, native_idx, true, 2000000000u128)
            .unwrap(),
    );
    inner.extend_from_slice(
        &encoders::enc_v4_take_compact(weth_idx, executor_idx, 1000000000000000000u128).unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_weth_withdraw(U256::from(
        1000000000000000000u128,
    )));
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            native_idx,
            idx0,
            500,
            10,
            native_idx,
            true,
            1000000000000000000u128,
        )
        .unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(native_idx));
    inner.extend_from_slice(&encoders::enc_v4_take_delta(idx0, executor_idx));
    inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let expected = v4_envelope(&at, &inner);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v4v3_native_out_deposit() {
    let rust = encode_cmd_stream(
        &PathInfo::new(vec![
            HopInfo::V4(V4HopInfo {
                pool_manager_address: address!("000000000004444c5dc75cB358380D2e3dE08A90"),
                pool_id_hex: "0x1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
                currency0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                currency1_address: address!("0000000000000000000000000000000000000000"),
                fee: 500u32,
                tick_spacing: 10i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: true,
            }),
            HopInfo::V3(V3HopInfo {
                pool_address: address!("1111111111111111111111111111111111111111"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 3000u32,
                zfo: true,
            }),
        ]),
        2000000000u128,
        &[1000000000000000000u128, 2001000000000000000u128],
        // consumed_inputs = [optimal_input, V3 clamped swap-in]. The V3 swap-in
        // (1 wei below the V4 forward 1_000_000_000_000_000_000) proves the
        // encoder feeds the CL clamp vector, not hop_outputs[1].
        &[2000000000u128, 999_999_999_999_999_999u128],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V4 outputs native ETH → wrap to WETH for V3 (auto-pay); settle V4's
    // USDC input debt last. All inside V4_UNLOCK.
    let pool_v3 = address!("1111111111111111111111111111111111111111");
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let zero_idx = SENTINEL_NATIVE;
    let c0_v4_idx = at.add(USDC).unwrap(); // 0
    let c1_v4_idx = at.add(Address::ZERO).unwrap(); // SENTINEL_NATIVE
    let v3_idx = at.add(pool_v3).unwrap(); // 1
    let native_idx = at.add(Address::ZERO).unwrap(); // SENTINEL_NATIVE
    let mut inner = Vec::new();
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_v4_idx,
            c1_v4_idx,
            500,
            10,
            zero_idx,
            true,
            2_000_000_000u128,
        )
        .unwrap(),
    );
    inner.extend_from_slice(
        &encoders::enc_v4_take_compact(native_idx, SENTINEL_SELF, 1_000_000_000_000_000_000u128)
            .unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_weth_deposit(U256::from(
        1_000_000_000_000_000_000u128,
    )));
    inner.extend_from_slice(
        &encoders::enc_v3_swap_compact(
            v3_idx,
            true,
            999_999_999_999_999_999u128, // = consumed_inputs[1] (CL clamp)
            SENTINEL_SELF,
            &[],
        )
        .unwrap(),
    );
    let input_idx = c0_v4_idx; // zfo → input is currency0 = USDC
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(input_idx));
    inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let expected = v4_envelope(&at, &inner);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v4v3_erc20_out_autopay() {
    let rust = encode_cmd_stream(
        &PathInfo::new(vec![
            HopInfo::V4(V4HopInfo {
                pool_manager_address: address!("000000000004444c5dc75cB358380D2e3dE08A90"),
                pool_id_hex: "0x1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
                currency0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                currency1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 3000u32,
                tick_spacing: 60i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: true,
            }),
            HopInfo::V3(V3HopInfo {
                pool_address: address!("2222222222222222222222222222222222222222"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 500u32,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128],
        // consumed_inputs = [optimal_input, V3 clamped swap-in] — V3 is the CL
        // hop (index 1); its swap-in feeds the clamp vector, not hop_outputs[1]
        // nor hop_outputs[0].
        &[1000000000000000000u128, 1_999_999_999u128],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V4 outputs USDC (ERC-20, currency1) → take to executor; V3 auto-pays
    // from that balance; settle V4's WETH input debt.
    let pool_v3 = address!("2222222222222222222222222222222222222222");
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let zero_idx = SENTINEL_NATIVE;
    let c0_v4_idx = at.add(WETH).unwrap(); // SENTINEL_WETH (currency0 = WETH)
    let c1_v4_idx = at.add(USDC).unwrap(); // 0 (currency1 = USDC)
    let v3_idx = at.add(pool_v3).unwrap(); // 1
    let weth_idx = SENTINEL_WETH;
    let mut inner = Vec::new();
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_v4_idx,
            c1_v4_idx,
            3000,
            60,
            zero_idx,
            true,
            1_000_000_000_000_000_000u128,
        )
        .unwrap(),
    );
    let forward_idx = c1_v4_idx; // zfo → output is currency1 = USDC
    inner.extend_from_slice(
        &encoders::enc_v4_take_compact(forward_idx, SENTINEL_SELF, 2_000_000_000u128).unwrap(),
    );
    inner.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3_idx, true, 1_999_999_999u128, SENTINEL_SELF, &[])
            .unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(weth_idx));
    inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let expected = v4_envelope(&at, &inner);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v4v3_erc20_out_v4_in_native_unwrap() {
    let rust = encode_cmd_stream(
        &PathInfo::new(vec![
            HopInfo::V4(V4HopInfo {
                pool_manager_address: address!("000000000004444c5dc75cB358380D2e3dE08A90"),
                pool_id_hex: "0x1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
                currency0_address: address!("0000000000000000000000000000000000000000"),
                currency1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 3000u32,
                tick_spacing: 60i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: true,
            }),
            HopInfo::V3(V3HopInfo {
                pool_address: address!("3333333333333333333333333333333333333333"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 500u32,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128],
        // consumed_inputs = [optimal_input, V3 clamped swap-in] — V3 is the CL
        // hop (index 1); its swap-in feeds the clamp vector, not hop_outputs[1]
        // nor hop_outputs[0].
        &[1000000000000000000u128, 1_999_999_999u128],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V4 input is native ETH (currency0=NATIVE, zfo); output is USDC → take
    // to executor for V3 auto-pay. V4's native debt settled by unwrapping
    // WETH first.
    let pool_v3 = address!("3333333333333333333333333333333333333333");
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let zero_idx = SENTINEL_NATIVE;
    let c0_v4_idx = at.add(Address::ZERO).unwrap(); // SENTINEL_NATIVE
    let c1_v4_idx = at.add(USDC).unwrap(); // 0
    let v3_idx = at.add(pool_v3).unwrap(); // 1
    let mut inner = Vec::new();
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_v4_idx,
            c1_v4_idx,
            3000,
            60,
            zero_idx,
            true,
            1_000_000_000_000_000_000u128,
        )
        .unwrap(),
    );
    let forward_idx = c1_v4_idx; // zfo → output is currency1 = USDC
    inner.extend_from_slice(
        &encoders::enc_v4_take_compact(forward_idx, SENTINEL_SELF, 2_000_000_000u128).unwrap(),
    );
    inner.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3_idx, true, 1_999_999_999u128, SENTINEL_SELF, &[])
            .unwrap(),
    );
    // V4 input is native → unwrap WETH then settle the native delta.
    let input_idx = c0_v4_idx; // zfo → input is currency0 = NATIVE
    inner.extend_from_slice(&encoders::enc_weth_withdraw(U256::from(
        1_000_000_000_000_000_000u128,
    )));
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(input_idx));
    inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let expected = v4_envelope(&at, &inner);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v3v4_v4_in_weth() {
    let rust = encode_cmd_stream(
        &PathInfo::new(vec![
            HopInfo::V3(V3HopInfo {
                pool_address: address!("4444444444444444444444444444444444444444"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 3000u32,
                zfo: true,
            }),
            HopInfo::V4(V4HopInfo {
                pool_manager_address: address!("000000000004444c5dc75cB358380D2e3dE08A90"),
                pool_id_hex: "0x1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
                currency0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                currency1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 500u32,
                tick_spacing: 10i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128],
        // consumed_inputs = [optimal_input, V4 clamped swap-in] — the V4 swap-in
        // (1 wei below the recorded forward 2_000_000_000) proves the encoder
        // feeds the CL clamp vector, not hop_outputs[1].
        &[1000000000000000000u128, 1_999_999_999u128],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V4 needs WETH (ERC-20 input) — standard sync+transfer+settle+swap+take
    // inside V4_UNLOCK, nested as V3's forward_data callback. V3 amount is
    // optimal_input (the WETH into V3).
    let pool_v3 = address!("4444444444444444444444444444444444444444");
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let pm_idx = at.add(PM).unwrap(); // SENTINEL_PM
    let zero_idx = SENTINEL_NATIVE;
    let v3_idx = at.add(pool_v3).unwrap(); // 0
    let c0_v4_idx = at.add(USDC).unwrap(); // 1
    let c1_v4_idx = at.add(WETH).unwrap(); // SENTINEL_WETH
    let weth_idx = SENTINEL_WETH;
    let forward_idx = c0_v4_idx; // V3 forward token = USDC, already in table
    let mut v4_inner = Vec::new();
    v4_inner.extend_from_slice(&encoders::enc_v4_sync(forward_idx));
    v4_inner.extend_from_slice(
        &encoders::enc_erc20_transfer(forward_idx, pm_idx, 2_000_000_000u128).unwrap(),
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_settle());
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_v4_idx,
            c1_v4_idx,
            500,
            10,
            zero_idx,
            true,
            1_999_999_999u128, // = consumed_inputs[1] (CL clamp)
        )
        .unwrap(),
    );
    let output_idx = c1_v4_idx; // zfo → output is currency1 = WETH
    v4_inner.extend_from_slice(
        &encoders::enc_v4_take_compact(output_idx, SENTINEL_SELF, 2_001_000_000_000_000_000u128)
            .unwrap(),
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let mut callback = encoders::enc_v4_unlock(&v4_inner).unwrap();
    callback.extend_from_slice(
        &encoders::enc_erc20_transfer(weth_idx, v3_idx, 1_000_000_000_000_000_000u128).unwrap(),
    );
    let commands = encoders::enc_v3_swap_compact(
        v3_idx,
        true,
        1_000_000_000_000_000_000u128,
        SENTINEL_SELF,
        &callback,
    )
    .unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v4v2_native_out_deposit() {
    let rust = encode_cmd_stream(
        &PathInfo::new(vec![
            HopInfo::V4(V4HopInfo {
                pool_manager_address: address!("000000000004444c5dc75cB358380D2e3dE08A90"),
                pool_id_hex: "0x1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
                currency0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                currency1_address: address!("0000000000000000000000000000000000000000"),
                fee: 500u32,
                tick_spacing: 10i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: address!("6666666666666666666666666666666666666666"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 30u16,
                zfo: true,
            }),
        ]),
        2000000000u128,
        &[1000000000000000000u128, 2001000000000000000u128],
        &[1000000000000000000u128, 2001000000000000000u128],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V4 outputs native ETH → wrap to WETH for V2 (callback pays WETH to V2);
    // settle V4's USDC input debt last.
    let pool_v2 = address!("6666666666666666666666666666666666666666");
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let zero_idx = SENTINEL_NATIVE;
    let c0_v4_idx = at.add(USDC).unwrap(); // 0
    let c1_v4_idx = at.add(Address::ZERO).unwrap(); // SENTINEL_NATIVE
    let v2_idx = at.add(pool_v2).unwrap(); // 1
    let weth_idx = SENTINEL_WETH;
    let native_idx = at.add(Address::ZERO).unwrap(); // SENTINEL_NATIVE
    let mut inner = Vec::new();
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_v4_idx,
            c1_v4_idx,
            500,
            10,
            zero_idx,
            true,
            2_000_000_000u128,
        )
        .unwrap(),
    );
    inner.extend_from_slice(
        &encoders::enc_v4_take_compact(native_idx, SENTINEL_SELF, 1_000_000_000_000_000_000u128)
            .unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_weth_deposit(U256::from(
        1_000_000_000_000_000_000u128,
    )));
    let v2_cb_cmds =
        encoders::enc_erc20_transfer(weth_idx, v2_idx, 1_000_000_000_000_000_000u128).unwrap();
    inner.extend_from_slice(
        &encoders::enc_v2_swap_compact(
            v2_idx,
            true,
            2_001_000_000_000_000_000u128,
            SENTINEL_SELF,
            30,
            &v2_cb_cmds,
        )
        .unwrap(),
    );
    let input_idx = c0_v4_idx; // zfo → input is currency0 = USDC
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(input_idx));
    inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let expected = v4_envelope(&at, &inner);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v4v2_erc20_out_direct() {
    let rust = encode_cmd_stream(
        &PathInfo::new(vec![
            HopInfo::V4(V4HopInfo {
                pool_manager_address: address!("000000000004444c5dc75cB358380D2e3dE08A90"),
                pool_id_hex: "0x1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
                currency0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                currency1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 3000u32,
                tick_spacing: 60i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: address!("7777777777777777777777777777777777777777"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 30u16,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128],
        &[2000000000u128, 2001000000000000000u128],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V4 outputs USDC (ERC-20) → take directly to V2 pool; V2_SWAP_CALC reads
    // the excess. V4 input is WETH → sync+transfer+settle pays the debt.
    let pool_v2 = address!("7777777777777777777777777777777777777777");
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let pm_idx = at.add(PM).unwrap(); // SENTINEL_PM
    let zero_idx = SENTINEL_NATIVE;
    let c0_v4_idx = at.add(WETH).unwrap(); // SENTINEL_WETH
    let c1_v4_idx = at.add(USDC).unwrap(); // 0
    let v2_idx = at.add(pool_v2).unwrap(); // 1
    let weth_idx = SENTINEL_WETH;
    let mut inner = Vec::new();
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_v4_idx,
            c1_v4_idx,
            3000,
            60,
            zero_idx,
            true,
            1_000_000_000_000_000_000u128,
        )
        .unwrap(),
    );
    let forward_idx = c1_v4_idx; // zfo → output is currency1 = USDC
    inner.extend_from_slice(
        &encoders::enc_v4_take_compact(forward_idx, v2_idx, 2_000_000_000u128).unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2_idx, true, SENTINEL_SELF, 30));
    inner.extend_from_slice(&encoders::enc_v4_sync(weth_idx));
    inner.extend_from_slice(
        &encoders::enc_erc20_transfer(weth_idx, pm_idx, 1_000_000_000_000_000_000u128).unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_v4_settle());
    inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let expected = v4_envelope(&at, &inner);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v4v2_erc20_out_v4_in_native() {
    let rust = encode_cmd_stream(
        &PathInfo::new(vec![
            HopInfo::V4(V4HopInfo {
                pool_manager_address: address!("000000000004444c5dc75cB358380D2e3dE08A90"),
                pool_id_hex: "0x1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
                currency0_address: address!("0000000000000000000000000000000000000000"),
                currency1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 3000u32,
                tick_spacing: 60i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: address!("8888888888888888888888888888888888888888"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 30u16,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128],
        &[2000000000u128, 2001000000000000000u128],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V4 outputs USDC → take to V2 (V2_SWAP_CALC). V4 input is native ETH →
    // unwrap WETH then settle the native delta.
    let pool_v2 = address!("8888888888888888888888888888888888888888");
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let zero_idx = SENTINEL_NATIVE;
    let c0_v4_idx = at.add(Address::ZERO).unwrap(); // SENTINEL_NATIVE
    let c1_v4_idx = at.add(USDC).unwrap(); // 0
    let v2_idx = at.add(pool_v2).unwrap(); // 1
    let mut inner = Vec::new();
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_v4_idx,
            c1_v4_idx,
            3000,
            60,
            zero_idx,
            true,
            1_000_000_000_000_000_000u128,
        )
        .unwrap(),
    );
    let forward_idx = c1_v4_idx; // zfo → output is currency1 = USDC
    inner.extend_from_slice(
        &encoders::enc_v4_take_compact(forward_idx, v2_idx, 2_000_000_000u128).unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2_idx, true, SENTINEL_SELF, 30));
    let input_idx = c0_v4_idx; // zfo → input is currency0 = NATIVE
    inner.extend_from_slice(&encoders::enc_weth_withdraw(U256::from(
        1_000_000_000_000_000_000u128,
    )));
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(input_idx));
    inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let expected = v4_envelope(&at, &inner);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v2v4_v4_out_native() {
    let rust = encode_cmd_stream(
        &PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: address!("9999999999999999999999999999999999999999"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 30u16,
                zfo: true,
            }),
            HopInfo::V4(V4HopInfo {
                pool_manager_address: address!("000000000004444c5dc75cB358380D2e3dE08A90"),
                pool_id_hex: "0x1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
                currency0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                currency1_address: address!("0000000000000000000000000000000000000000"),
                fee: 500u32,
                tick_spacing: 10i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128],
        // consumed_inputs = [optimal_input, V4 clamped swap-in] — the V4 swap-in
        // (1 wei below the recorded forward 2_000_000_000) proves the encoder
        // feeds the CL clamp vector, not hop_outputs[1].
        &[1000000000000000000u128, 1_999_999_999u128],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V2 flash → V4 (ERC-20 input, native output) nested in V2 callback.
    // V4 outputs native → wrap to WETH before repaying V2.
    let pool_v2 = address!("9999999999999999999999999999999999999999");
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let pm_idx = at.add(PM).unwrap(); // SENTINEL_PM
    let zero_idx = SENTINEL_NATIVE;
    let v2_idx = at.add(pool_v2).unwrap(); // 0
    let c0_v4_idx = at.add(USDC).unwrap(); // 1
    let c1_v4_idx = at.add(Address::ZERO).unwrap(); // SENTINEL_NATIVE
    let weth_idx = SENTINEL_WETH;
    let forward_idx = c0_v4_idx; // V2 forward token = USDC
    let native_idx_out = at.add(Address::ZERO).unwrap(); // SENTINEL_NATIVE
    let mut v4_inner = Vec::new();
    v4_inner.extend_from_slice(&encoders::enc_v4_sync(forward_idx));
    v4_inner.extend_from_slice(
        &encoders::enc_erc20_transfer(forward_idx, pm_idx, 2_000_000_000u128).unwrap(),
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_settle());
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_v4_idx,
            c1_v4_idx,
            500,
            10,
            zero_idx,
            true,
            1_999_999_999u128, // = consumed_inputs[1] (CL clamp)
        )
        .unwrap(),
    );
    v4_inner.extend_from_slice(
        &encoders::enc_v4_take_compact(
            native_idx_out,
            SENTINEL_SELF,
            2_001_000_000_000_000_000u128,
        )
        .unwrap(),
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let mut callback = encoders::enc_v4_unlock(&v4_inner).unwrap();
    callback.extend_from_slice(&encoders::enc_weth_deposit(U256::from(
        2_001_000_000_000_000_000u128,
    )));
    callback.extend_from_slice(
        &encoders::enc_erc20_transfer(weth_idx, v2_idx, 1_000_000_000_000_000_000u128).unwrap(),
    );
    let commands = encoders::enc_v2_swap_compact(
        v2_idx,
        true,
        2_000_000_000u128,
        SENTINEL_SELF,
        30,
        &callback,
    )
    .unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v2v4_v4_in_native() {
    let rust = encode_cmd_stream(
        &PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 30u16,
                zfo: true,
            }),
            HopInfo::V4(V4HopInfo {
                pool_manager_address: address!("000000000004444c5dc75cB358380D2e3dE08A90"),
                pool_id_hex: "0x1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
                currency0_address: address!("0000000000000000000000000000000000000000"),
                currency1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 500u32,
                tick_spacing: 10i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128],
        // consumed_inputs = [optimal_input, V4 clamped swap-in]. The V4 swap-in
        // (1 wei below the recorded forward 2_000_000_000) proves the encoder
        // feeds the CL clamp vector, not hop_outputs[1].
        &[1000000000000000000u128, 1_999_999_999u128],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V2 flash → V4 (native ETH input) nested in V2 callback. Unwrap WETH
    // first, then V4 swap+settle+take, then repay V2.
    let pool_v2 = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let zero_idx = SENTINEL_NATIVE;
    let v2_idx = at.add(pool_v2).unwrap(); // 0
    let c0_v4_idx = at.add(Address::ZERO).unwrap(); // SENTINEL_NATIVE
    let c1_v4_idx = at.add(USDC).unwrap(); // 1
    let native_idx_in = at.add(Address::ZERO).unwrap(); // SENTINEL_NATIVE
    let forward_idx = c1_v4_idx; // V2 forward token = USDC
    let mut v4_inner = Vec::new();
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_v4_idx,
            c1_v4_idx,
            500,
            10,
            zero_idx,
            true,
            1_999_999_999u128, // = consumed_inputs[1] (CL clamp)
        )
        .unwrap(),
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_delta(native_idx_in));
    let output_idx = c1_v4_idx; // zfo → output is currency1 = USDC
    v4_inner.extend_from_slice(
        &encoders::enc_v4_take_compact(output_idx, SENTINEL_SELF, 2_001_000_000_000_000_000u128)
            .unwrap(),
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let mut callback = encoders::enc_weth_withdraw(U256::from(2_000_000_000u128));
    callback.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).unwrap());
    callback.extend_from_slice(
        &encoders::enc_erc20_transfer(forward_idx, v2_idx, 1_000_000_000_000_000_000u128).unwrap(),
    );
    let commands = encoders::enc_v2_swap_compact(
        v2_idx,
        true,
        2_000_000_000u128,
        SENTINEL_SELF,
        30,
        &callback,
    )
    .unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v3v3_forward_order() {
    let rust = encode_cmd_stream(
        &PathInfo::new(vec![
            HopInfo::V3(V3HopInfo {
                pool_address: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 3000u32,
                zfo: true,
            }),
            HopInfo::V3(V3HopInfo {
                pool_address: address!("cccccccccccccccccccccccccccccccccccccccc"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 500u32,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128],
        &[2000000000u128, 2001000000000000000u128],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V3a sends USDC to executor before callback; callback pays WETH to V3a,
    // then V3b swaps (auto-pay) sending WETH to executor.
    let pool_a = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    let pool_b = address!("cccccccccccccccccccccccccccccccccccccccc");
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), None);
    let v3_a_idx = at.add(pool_a).unwrap(); // 0
    let v3_b_idx = at.add(pool_b).unwrap(); // 1
    let weth_idx = SENTINEL_WETH;
    let mut v3_a_callback = Vec::new();
    v3_a_callback.extend_from_slice(
        &encoders::enc_erc20_transfer(weth_idx, v3_a_idx, 1_000_000_000_000_000_000u128).unwrap(),
    );
    v3_a_callback.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3_b_idx, true, 2_000_000_000u128, SENTINEL_SELF, &[])
            .unwrap(),
    );
    let commands = encoders::enc_v3_swap_compact(
        v3_a_idx,
        true,
        1_000_000_000_000_000_000u128,
        SENTINEL_SELF,
        &v3_a_callback,
    )
    .unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v2v3_callback_forward_data() {
    let rust = encode_cmd_stream(
        &PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: address!("dddddddddddddddddddddddddddddddddddddddd"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 30u16,
                zfo: true,
            }),
            HopInfo::V3(V3HopInfo {
                pool_address: address!("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 500u32,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128],
        &[2000000000u128, 2001000000000000000u128],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V2 flash borrow sends USDC to executor; callback runs V3 swap with the
    // ERC20_TRANSFER inside V3's forward_data (IIA ordering), then WETH repays V2.
    let pool_v2 = address!("dddddddddddddddddddddddddddddddddddddddd");
    let pool_v3 = address!("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee");
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), None);
    let v2_idx = at.add(pool_v2).unwrap(); // 0
    let v3_idx = at.add(pool_v3).unwrap(); // 1
    let weth_idx = SENTINEL_WETH;
    let forward_idx = at.add(USDC).unwrap(); // 2 (V2 forward token = USDC)
    let v3_callback_cmds =
        encoders::enc_erc20_transfer(forward_idx, v3_idx, 2_000_000_000u128).unwrap();
    let mut callback = Vec::new();
    callback.extend_from_slice(
        &encoders::enc_v3_swap_compact(
            v3_idx,
            true,
            2_000_000_000u128,
            SENTINEL_SELF,
            &v3_callback_cmds,
        )
        .unwrap(),
    );
    callback.extend_from_slice(
        &encoders::enc_erc20_transfer(weth_idx, v2_idx, 1_000_000_000_000_000_000u128).unwrap(),
    );
    let commands = encoders::enc_v2_swap_compact(
        v2_idx,
        true,
        2_000_000_000u128,
        SENTINEL_SELF,
        30,
        &callback,
    )
    .unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v3v2_callback_nested() {
    let rust = encode_cmd_stream(
        &PathInfo::new(vec![
            HopInfo::V3(V3HopInfo {
                pool_address: address!("f111111111111111111111111111111111111111"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 3000u32,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: address!("f222222222222222222222222222222222222222"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 30u16,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128],
        &[2000000000u128, 2001000000000000000u128],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V3 sends USDC to executor before callback; callback pays WETH to V3,
    // pre-funds V2 with USDC, then runs V2 direct swap (on-chain WETH output).
    // V3 amount is optimal_input (WETH into V3), not forward_out.
    let pool_v3 = address!("f111111111111111111111111111111111111111");
    let pool_v2 = address!("f222222222222222222222222222222222222222");
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), None);
    let v3_idx = at.add(pool_v3).unwrap(); // 0
    let v2_idx = at.add(pool_v2).unwrap(); // 1
    let weth_idx = SENTINEL_WETH;
    let forward_idx = at.add(USDC).unwrap(); // 2 (V3 forward token = USDC)
    let mut v3_callback = Vec::new();
    v3_callback.extend_from_slice(
        &encoders::enc_erc20_transfer(weth_idx, v3_idx, 1_000_000_000_000_000_000u128).unwrap(),
    );
    v3_callback.extend_from_slice(
        &encoders::enc_erc20_transfer(forward_idx, v2_idx, 2_000_000_000u128).unwrap(),
    );
    v3_callback.extend_from_slice(
        &encoders::enc_v2_swap_compact(
            v2_idx,
            true,
            2_001_000_000_000_000_000u128,
            SENTINEL_SELF,
            30,
            &[],
        )
        .unwrap(),
    );
    let commands = encoders::enc_v3_swap_compact(
        v3_idx,
        true,
        1_000_000_000_000_000_000u128,
        SENTINEL_SELF,
        &v3_callback,
    )
    .unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v2_n_hop_2() {
    let rust = encode_cmd_stream(
        &PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: address!("f333333333333333333333333333333333333333"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 30u16,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: address!("f444444444444444444444444444444444444444"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 30u16,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128],
        &[2000000000u128, 2001000000000000000u128],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // N-hop V2 (N=2): flash borrow pool A, transfer forward token to pool B,
    // V2_SWAP_CALC pool B → executor, then WETH repays pool A's flash.
    let pool_a = address!("f333333333333333333333333333333333333333");
    let pool_b = address!("f444444444444444444444444444444444444444");
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), None);
    let pool_a_idx = at.add(pool_a).unwrap(); // 0
    let pool_b_idx = at.add(pool_b).unwrap(); // 1
    let weth_idx = SENTINEL_WETH;
    let forward_idx = at.add(USDC).unwrap(); // 2 (pool A forward token = USDC)
    let mut callback = Vec::new();
    callback.extend_from_slice(
        &encoders::enc_erc20_transfer(forward_idx, pool_b_idx, 2_000_000_000u128).unwrap(),
    );
    callback.extend_from_slice(&encoders::enc_v2_swap_calc(
        pool_b_idx,
        true,
        SENTINEL_SELF,
        30,
    ));
    callback.extend_from_slice(
        &encoders::enc_erc20_transfer(weth_idx, pool_a_idx, 1_000_000_000_000_000_000u128).unwrap(),
    );
    let commands = encoders::enc_v2_swap_compact(
        pool_a_idx,
        true,
        2_000_000_000u128,
        SENTINEL_SELF,
        30,
        &callback,
    )
    .unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v2_n_hop_3() {
    let rust = encode_cmd_stream(
        &PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: address!("f555555555555555555555555555555555555555"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 30u16,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: address!("f666666666666666666666666666666666666666"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"),
                fee: 30u16,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: address!("f777777777777777777777777777777777777777"),
                token0_address: address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 30u16,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[
            2000000000u128,
            1000000000000000u128,
            2001000000000000000u128,
        ],
        &[
            2000000000u128,
            1000000000000000u128,
            2001000000000000000u128,
        ],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // N-hop V2 (N=3): flash borrow pool A, transfer forward token to pool B,
    // V2_SWAP_CALC pool B → pool C, V2_SWAP_CALC pool C → executor, then WETH
    // repays pool A's flash. (Intermediate WBTC held by direct custody, never
    // registered in the table.)
    let pool_a = address!("f555555555555555555555555555555555555555");
    let pool_b = address!("f666666666666666666666666666666666666666");
    let pool_c = address!("f777777777777777777777777777777777777777");
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), None);
    let pool_a_idx = at.add(pool_a).unwrap(); // 0
    let pool_b_idx = at.add(pool_b).unwrap(); // 1
    let pool_c_idx = at.add(pool_c).unwrap(); // 2
    let weth_idx = SENTINEL_WETH;
    let forward_idx = at.add(USDC).unwrap(); // 3 (pool A forward token = USDC)
    let mut callback = Vec::new();
    callback.extend_from_slice(
        &encoders::enc_erc20_transfer(forward_idx, pool_b_idx, 2_000_000_000u128).unwrap(),
    );
    callback.extend_from_slice(&encoders::enc_v2_swap_calc(
        pool_b_idx, true, pool_c_idx, 30,
    ));
    callback.extend_from_slice(&encoders::enc_v2_swap_calc(
        pool_c_idx,
        true,
        SENTINEL_SELF,
        30,
    ));
    callback.extend_from_slice(
        &encoders::enc_erc20_transfer(weth_idx, pool_a_idx, 1_000_000_000_000_000_000u128).unwrap(),
    );
    let commands = encoders::enc_v2_swap_compact(
        pool_a_idx,
        true,
        2_000_000_000u128,
        SENTINEL_SELF,
        30,
        &callback,
    )
    .unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v4v4_payload_encode_same_currency() {
    let rust = {
        let mut p = V4V4ArbitragePayload::new(
            address!("000000000004444c5dc75cB358380D2e3dE08A90"),
            address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            address!("DeAd0000000000000000000000000000000000Be"),
        );
        p.set_pool_a(
            address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
            3000u32,
            60i32,
            address!("0000000000000000000000000000000000000000"),
            1000000000000000000u128,
            2000000000u128,
            None,
        );
        p.set_pool_b(
            address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
            address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            500u32,
            10i32,
            address!("0000000000000000000000000000000000000000"),
            2000000000u128,
            2001000000000000000u128,
            None,
        );
        p.encode()
    };
    assert_eq!(rust, Some(hx(b"\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x50\x3b\x40\x00\xfe\x0b\xb8\x00\x3c\xff\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x40\x00\xfe\x01\xf4\x00\x0a\xff\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x52\xfe\xfd\x00\x00\x00\x00\x0d\xe4\x44\x32\x4c\x2a\x80\x00\x56\xfe")));
}

#[test]
fn parity_v4v4_payload_encode_native_profit() {
    let rust = {
        let mut p = V4V4ArbitragePayload::new(
            address!("000000000004444c5dc75cB358380D2e3dE08A90"),
            address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            address!("DeAd0000000000000000000000000000000000Be"),
        );
        p.set_pool_a(
            address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
            3000u32,
            60i32,
            address!("0000000000000000000000000000000000000000"),
            1000000000000000000u128,
            2000000000u128,
            None,
        );
        p.set_pool_b(
            address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
            address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            500u32,
            10i32,
            address!("0000000000000000000000000000000000000000"),
            2000000000u128,
            2001000000000000000u128,
            None,
        );
        p.profit_currency = Some(address!("0000000000000000000000000000000000000000"));
        p.encode()
    };
    assert_eq!(rust, Some(hx(b"\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x50\x3b\x40\x00\xfe\x0b\xb8\x00\x3c\xff\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x40\x00\xfe\x01\xf4\x00\x0a\xff\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x52\xff\xfd\x00\x00\x00\x00\x1b\xc4\xfa\xe5\xf3\x8e\x80\x00\x56\xfe")));
}

#[test]
fn parity_v4v4_payload_encode_batch() {
    let rust = {
        let mut p = V4V4ArbitragePayload::new(
            address!("000000000004444c5dc75cB358380D2e3dE08A90"),
            address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            address!("DeAd0000000000000000000000000000000000Be"),
        );
        p.set_pool_a(
            address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
            3000u32,
            60i32,
            address!("0000000000000000000000000000000000000000"),
            1000000000000000000u128,
            2000000000u128,
            None,
        );
        p.set_pool_b(
            address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
            address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            500u32,
            10i32,
            address!("0000000000000000000000000000000000000000"),
            2000000000u128,
            2001000000000000000u128,
            None,
        );
        p.encode_batch()
    };
    assert_eq!(rust, Some(hx(b"\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x50\x2a\x42\x02\x00\xfe\x0b\xb8\x00\x3c\xff\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x00\xfe\x01\xf4\x00\x0a\xff\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00")));
}

#[test]
fn parity_v4v3_payload_encode_autopay() {
    let rust = {
        let mut p = V4V3ArbitragePayload::new(
            address!("000000000004444c5dc75cB358380D2e3dE08A90"),
            address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            address!("DeAd0000000000000000000000000000000000Be"),
            address!("f888888888888888888888888888888888888888"),
            address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
        );
        p.set_v4_pool(
            address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
            3000u32,
            60i32,
            address!("0000000000000000000000000000000000000000"),
            1000000000000000000u128,
            2000000000u128,
            None,
        );
        p.set_v3_pool(2000000000u128, 2001000000000000000u128, true);
        p.encode()
    };
    assert_eq!(rust, Some(hx(b"\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\x00\xf8\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\xff\x50\x37\x40\x00\xfe\x0b\xb8\x00\x3c\xff\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x52\x00\xfd\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x30\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\xfd\x00\x56\xfe")));
}

#[test]
fn parity_v4v3_payload_encode_forward_data() {
    let rust = {
        let mut p = V4V3ArbitragePayload::new(
            address!("000000000004444c5dc75cB358380D2e3dE08A90"),
            address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            address!("DeAd0000000000000000000000000000000000Be"),
            address!("f888888888888888888888888888888888888888"),
            address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
        );
        p.set_v4_pool(
            address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
            3000u32,
            60i32,
            address!("0000000000000000000000000000000000000000"),
            1000000000000000000u128,
            2000000000u128,
            None,
        );
        p.set_v3_pool(2000000000u128, 2001000000000000000u128, true);
        p.encode_with_forward_data()
    };
    assert_eq!(rust, Some(hx(b"\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\x00\xf8\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\xff\x50\x56\x40\x00\xfe\x0b\xb8\x00\x3c\xff\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x52\x00\xfd\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x30\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\xfd\x21\x10\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x54\xfe\x10\xfe\xfc\x00\x00\x00\x00\x1b\xc4\xfa\xe5\xf3\x8e\x80\x00\x55")));
}

#[test]
fn parity_cmd_executor_compose_v4v4() {
    let rust = {
        let c = CmdExecutorComposer::new(
            address!("000000000004444c5dc75cB358380D2e3dE08A90"),
            address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            address!("DeAd0000000000000000000000000000000000Be"),
        );
        let sa = V4SwapAmounts {
            pool_key: V4PoolKeyConfig {
                currency0: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                currency1: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 3000u32,
                tick_spacing: 60i32,
                hooks: address!("0000000000000000000000000000000000000000"),
            },
            zero_for_one: true,
            amount_in: 1000000000000000000u128,
            amount_out: 2000000000u128,
        };
        let sb = V4SwapAmounts {
            pool_key: V4PoolKeyConfig {
                currency0: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                currency1: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 500u32,
                tick_spacing: 10i32,
                hooks: address!("0000000000000000000000000000000000000000"),
            },
            zero_for_one: true,
            amount_in: 2000000000u128,
            amount_out: 2001000000000000000u128,
        };
        Some(c.compose(&[sa, sb], U256::ZERO).unwrap().unwrap().data)
    };
    assert_eq!(rust, Some(hx(b"\xab\x58\x98\xe8\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x40\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x53\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x50\x3b\x40\xfe\x00\x0b\xb8\x00\x3c\xff\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x40\x00\xfe\x01\xf4\x00\x0a\xff\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x52\xfe\xfd\x00\x00\x00\x00\x0d\xe4\x44\x32\x4c\x2a\x80\x00\x56\xfe\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00")));
}

#[test]
fn parity_encode_execute_call_wrap() {
    let rust = {
        Some({
            let cmds = hx(b"\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x50\x3b\x40\x00\xfe\x0b\xb8\x00\x3c\xff\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x40\x00\xfe\x01\xf4\x00\x0a\xff\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x52\xfe\xfd\x00\x00\x00\x00\x0d\xe4\x44\x32\x4c\x2a\x80\x00\x56\xfe");
            encode_execute_call(
                address!("DeAd0000000000000000000000000000000000Be"),
                &cmds,
                U256::ZERO,
            )
            .unwrap()
            .data
        })
    };
    assert_eq!(rust, Some(hx(b"\xab\x58\x98\xe8\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x40\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x53\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x50\x3b\x40\x00\xfe\x0b\xb8\x00\x3c\xff\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x40\x00\xfe\x01\xf4\x00\x0a\xff\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x52\xfe\xfd\x00\x00\x00\x00\x0d\xe4\x44\x32\x4c\x2a\x80\x00\x56\xfe\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00")));
}

#[test]
fn parity_execute_selector() {
    assert_eq!(&composers::EXECUTE_SELECTOR, &[0xab, 0x58, 0x98, 0xe8]);
    // Matches Web3.keccak(text="execute(bytes,uint256)")[:4].
}
