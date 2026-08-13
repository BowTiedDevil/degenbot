#![expect(clippy::unwrap_used)]
// 3-hop composer parity vectors.
//
// Every `parity_*` test here derives its expected bytestream from the Rust
// `enc_*` primitives via an `AddressTable` built with the same `at.add` order
// the composer uses, then `assert_eq!(rust, Some(expected))`. The Rust `enc_*`
// primitive sequence is the canonical opcode source (ADR-005), so a composer
// change that alters the opcode order or table-index assignment makes the
// test fail here at the `enc_*` builder, not at an opaque byte literal. The
// native-ETH / WETH-bridge shapes — where the opcode ORDER is the risk — are
// covered by `enc_*`-derived expectations in `native_eth_3hop_bridge.rs`,
// `native_v4_v2_v4_path_ends.rs`, `native_v4_v3_v4_path_ends.rs`, and
// `native_v4_v2_mixed_path_ends.rs`.
#![allow(
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::needless_pass_by_value,
    clippy::similar_names
)]

use alloy::primitives::{address, Address};
use degenbot_executor::composers::{
    encode_cmd_3_hop, EncodeOptions, HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo,
};
use degenbot_executor::encoders::{
    self, AddressTable, SENTINEL_NATIVE, SENTINEL_SELF, SENTINEL_WETH,
};

const PM: Address = address!("000000000004444c5dc75cB358380D2e3dE08A90");
const EXECUTOR: Address = address!("DeAd0000000000000000000000000000000000Be");
const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
const USDC: Address = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");

/// Build `enc_preamble(&at) + enc_v4_unlock(&inner)` — the V4 envelope every
/// V4-containing 3-hop composer wraps around its `inner` opcode sequence.
fn v4_envelope(at: &AddressTable, inner: &[u8]) -> Vec<u8> {
    let mut out = encoders::enc_preamble(at);
    out.extend_from_slice(&encoders::enc_v4_unlock(inner).unwrap());
    out
}

#[test]
fn parity_v2_v2_v2() {
    let rust = encode_cmd_3_hop(
        &PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: address!("1111111111111111111111111111111111111111"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 30u16,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: address!("2222222222222222222222222222222222222222"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 30u16,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: address!("3333333333333333333333333333333333333333"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 30u16,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V2c flash borrow; callback repays WETH to V2a, then V2a→V2b, V2b→V2c.
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), None);
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let v2a_idx = at
        .add(address!("1111111111111111111111111111111111111111"))
        .unwrap(); // 0
    let v2b_idx = at
        .add(address!("2222222222222222222222222222222222222222"))
        .unwrap(); // 1
    let v2c_idx = at
        .add(address!("3333333333333333333333333333333333333333"))
        .unwrap(); // 2
    let mut c_fwd = Vec::new();
    c_fwd.extend_from_slice(
        &encoders::enc_erc20_transfer(weth_idx, v2a_idx, 1_000_000_000_000_000_000u128).unwrap(),
    );
    c_fwd.extend_from_slice(&encoders::enc_v2_swap_calc(v2a_idx, true, v2b_idx, 30));
    c_fwd.extend_from_slice(&encoders::enc_v2_swap_calc(v2b_idx, true, v2c_idx, 30));
    let commands =
        encoders::enc_v2_swap_compact(v2c_idx, true, 2_001_000_000u128, executor_idx, 30, &c_fwd)
            .unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v2_v2_v3() {
    let rust = encode_cmd_3_hop(
        &PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: address!("1111111111111111111111111111111111111111"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 30u16,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: address!("2222222222222222222222222222222222222222"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 30u16,
                zfo: true,
            }),
            HopInfo::V3(V3HopInfo {
                pool_address: address!("6666666666666666666666666666666666666666"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 3000u32,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        // consumed_inputs = [optimal_input, V2 forward, V3 clamped swap-in]. The
        // V3 (idx2, CL) swap-in feeds the clamp vector, 1 wei below the forward
        // into it.
        &[
            1000000000000000000u128,
            2000000000u128,
            2_000_999_999_999_999_999u128,
        ],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V2c replaced by V3c (auto-pay). Callback: WETH→V2a, V2a→V2b, V2b→V3c.
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), None);
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let v2a_idx = at
        .add(address!("1111111111111111111111111111111111111111"))
        .unwrap(); // 0
    let v2b_idx = at
        .add(address!("2222222222222222222222222222222222222222"))
        .unwrap(); // 1
    let v3c_idx = at
        .add(address!("6666666666666666666666666666666666666666"))
        .unwrap(); // 2
    let mut c_fwd = Vec::new();
    c_fwd.extend_from_slice(
        &encoders::enc_erc20_transfer(weth_idx, v2a_idx, 1_000_000_000_000_000_000u128).unwrap(),
    );
    c_fwd.extend_from_slice(&encoders::enc_v2_swap_calc(v2a_idx, true, v2b_idx, 30));
    c_fwd.extend_from_slice(&encoders::enc_v2_swap_calc(v2b_idx, true, v3c_idx, 30));
    let commands = encoders::enc_v3_swap_compact(
        v3c_idx,
        true,
        2_000_999_999_999_999_999u128, // = consumed_inputs[2] (CL clamp)
        executor_idx,
        &c_fwd,
    )
    .unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v2_v2_v4() {
    let rust = encode_cmd_3_hop(
        &PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: address!("1111111111111111111111111111111111111111"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 30u16,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: address!("2222222222222222222222222222222222222222"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 30u16,
                zfo: true,
            }),
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
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V2a→V2b→V4c all inside V4_UNLOCK. Sync forward_b (WETH), take WETH to
    // V2a, swap_calc chain V2a→V2b→PM, settle, V4 swap_dynamic, settle_all.
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let pm_idx = at.add(PM).unwrap(); // SENTINEL_PM
    let zero_idx = SENTINEL_NATIVE;
    let v2a_idx = at
        .add(address!("1111111111111111111111111111111111111111"))
        .unwrap(); // 0
    let v2b_idx = at
        .add(address!("2222222222222222222222222222222222222222"))
        .unwrap(); // 1
    let c0_idx = at.add(WETH).unwrap(); // SENTINEL_WETH (hc.currency0 = WETH)
    let c1_idx = at.add(USDC).unwrap(); // 2 (hc.currency1 = USDC)
    let forward_b_idx = at.add(WETH).unwrap(); // SENTINEL_WETH (hb forward = token1 = WETH)
    let mut inner = Vec::new();
    inner.extend_from_slice(&encoders::enc_v4_sync(forward_b_idx));
    inner.extend_from_slice(
        &encoders::enc_erc20_transfer(SENTINEL_WETH, v2a_idx, 1_000_000_000_000_000_000u128)
            .unwrap(), // placeholder
    );
    inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2a_idx, true, v2b_idx, 30));
    inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2b_idx, true, pm_idx, 30));
    inner.extend_from_slice(&encoders::enc_v4_settle());
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(c0_idx, c1_idx, 3000, 60, zero_idx, true, 2_001_000_000u128)
            .unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let expected = v4_envelope(&at, &inner);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v2_v3_v2() {
    let rust = encode_cmd_3_hop(
        &PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: address!("1111111111111111111111111111111111111111"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 30u16,
                zfo: true,
            }),
            HopInfo::V3(V3HopInfo {
                pool_address: address!("5555555555555555555555555555555555555555"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 500u32,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: address!("3333333333333333333333333333333333333333"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 30u16,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        &[
            1000000000000000000u128,
            1_999_999_999u128,
            2_001_000_000u128,
        ],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V2a flash → V3b (with forward_data) → V2c. The V2a forward token (USDC)
    // is registered first so V3b's callback can reference it.
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), None);
    let weth_idx = SENTINEL_WETH;
    at.add(USDC).unwrap(); // 0 — V2a forward token (ha.zfo → token1 = USDC)
    let executor_idx = SENTINEL_SELF;
    let v2a_idx = at
        .add(address!("1111111111111111111111111111111111111111"))
        .unwrap(); // 1
    let v2c_idx = at
        .add(address!("3333333333333333333333333333333333333333"))
        .unwrap(); // 2
    let v3b_idx = at
        .add(address!("5555555555555555555555555555555555555555"))
        .unwrap(); // 3
    let mut b_fwd = Vec::new();
    b_fwd.extend_from_slice(
        &encoders::enc_erc20_transfer(weth_idx, v2a_idx, 1_000_000_000_000_000_000u128).unwrap(),
    );
    b_fwd.extend_from_slice(
        &encoders::enc_v2_swap_direct(v2a_idx, true, 2_000_000_000u128, v3b_idx).unwrap(),
    );
    let c_fwd =
        encoders::enc_v3_swap_compact(v3b_idx, true, 1_999_999_999u128, v2c_idx, &b_fwd).unwrap();
    let commands =
        encoders::enc_v2_swap_compact(v2c_idx, true, 2_001_000_000u128, executor_idx, 30, &c_fwd)
            .unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v2_v3_v3() {
    let rust = encode_cmd_3_hop(
        &PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: address!("1111111111111111111111111111111111111111"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 30u16,
                zfo: true,
            }),
            HopInfo::V3(V3HopInfo {
                pool_address: address!("5555555555555555555555555555555555555555"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 500u32,
                zfo: true,
            }),
            HopInfo::V3(V3HopInfo {
                pool_address: address!("6666666666666666666666666666666666666666"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 3000u32,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        &[
            1000000000000000000u128,
            1_999_999_999u128,
            2_000_999_999_999_999_999u128,
        ],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V2a flash → V3b → V3c. V2a sends USDC direct to V3b; nested V3 callbacks.
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), None);
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let v2a_idx = at
        .add(address!("1111111111111111111111111111111111111111"))
        .unwrap(); // 0
    let v3b_idx = at
        .add(address!("5555555555555555555555555555555555555555"))
        .unwrap(); // 1
    let v3c_idx = at
        .add(address!("6666666666666666666666666666666666666666"))
        .unwrap(); // 2
    let mut v3b_fwd = Vec::new();
    v3b_fwd.extend_from_slice(
        &encoders::enc_erc20_transfer(weth_idx, v2a_idx, 1_000_000_000_000_000_000u128).unwrap(),
    );
    v3b_fwd.extend_from_slice(
        &encoders::enc_v2_swap_direct(v2a_idx, true, 2_000_000_000u128, v3b_idx).unwrap(),
    );
    let v3c_fwd =
        encoders::enc_v3_swap_compact(v3b_idx, true, 1_999_999_999u128, v3c_idx, &v3b_fwd).unwrap();
    let commands = encoders::enc_v3_swap_compact(
        v3c_idx,
        true,
        2_000_999_999_999_999_999u128, // = consumed_inputs[2] (CL clamp)
        executor_idx,
        &v3c_fwd,
    )
    .unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v2_v3_v4() {
    let rust = encode_cmd_3_hop(
        &PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: address!("1111111111111111111111111111111111111111"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 30u16,
                zfo: true,
            }),
            HopInfo::V3(V3HopInfo {
                pool_address: address!("5555555555555555555555555555555555555555"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"), // b fwd (t1) = WBTC
                fee: 500u32,
                zfo: true,
            }),
            HopInfo::V4(V4HopInfo {
                pool_manager_address: address!("000000000004444c5dc75cB358380D2e3dE08A90"),
                pool_id_hex: "0x1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
                currency0_address: address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"), // V4 tail input = WBTC
                currency1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"), // tail output = WETH
                fee: 3000u32,
                tick_spacing: 60i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[
            2000000000u128,
            2001000000000000000u128,
            2001000000000000000u128,
        ],
        &[
            1000000000000000000u128,
            1_999_999_999u128,
            2_000_999_999_999_999_999u128,
        ],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V2a flash → V3b (forward_data nests V4_UNLOCK) → V4c. Top-level is
    // V4_SYNC(forward_b) + V3_SWAP paying the PM. out_c - optimal_input is
    // the executor's WETH profit (taken inside the V4_UNLOCK).
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let pm_idx = at.add(PM).unwrap(); // SENTINEL_PM
    let zero_idx = SENTINEL_NATIVE;
    let v2a_idx = at
        .add(address!("1111111111111111111111111111111111111111"))
        .unwrap(); // 0
    let v3b_idx = at
        .add(address!("5555555555555555555555555555555555555555"))
        .unwrap(); // 1
    let wbtc = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
    let c0_idx = at.add(wbtc).unwrap(); // V4 tail input = WBTC
    let c1_idx = at.add(WETH).unwrap(); // SENTINEL_WETH (tail output = WETH)
    let forward_b_idx = at.add(wbtc).unwrap(); // hb fwd (t1) = WBTC
    let executor_idx = SENTINEL_SELF;
    let mut v4_inner = Vec::new();
    v4_inner.extend_from_slice(&encoders::enc_v4_settle());
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_idx,
            c1_idx,
            3000,
            60,
            zero_idx,
            true,
            2_000_999_999_999_999_999u128,
        )
        .unwrap(),
    );
    v4_inner.extend_from_slice(
        &encoders::enc_v4_take_compact(SENTINEL_WETH, v2a_idx, 1_000_000_000_000_000_000u128)
            .unwrap(),
    );
    v4_inner.extend_from_slice(
        &encoders::enc_v4_take_compact(SENTINEL_WETH, executor_idx, 1_001_000_000_000_000_000u128)
            .unwrap(),
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_sync(SENTINEL_WETH));
    // Mirrors `three_hop_v3_v3_v4`'s closing pattern: `V4_SETTLE_ALL` (not
    // bare `V4_SETTLE`) is bidirectional — it pays in positive deltas and
    // also `PM.take(WETH, self)`s any surplus credit the executor under-claimed.
    // Without it, a V4 swap whose actual output exceeds the predicted
    // `hop_outputs[2]` leaves a positive WETH delta on the PoolManager at
    // unlock exit → `CurrencyNotSettled(WETH, surplus)` (the residual class
    // observed for V2-V3-V4 post protocol-fee fix).
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let mut b_fwd = encoders::enc_v4_unlock(&v4_inner).unwrap();
    b_fwd.extend_from_slice(
        &encoders::enc_v2_swap_direct(v2a_idx, true, 2_000_000_000u128, v3b_idx).unwrap(),
    );
    let mut commands = encoders::enc_v4_sync(forward_b_idx);
    commands.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3b_idx, true, 1_999_999_999u128, pm_idx, &b_fwd).unwrap(),
    );
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v2_v4_v2() {
    let rust = encode_cmd_3_hop(
        &PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: address!("1111111111111111111111111111111111111111"),
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
                currency1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 500u32,
                tick_spacing: 10i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: address!("3333333333333333333333333333333333333333"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 30u16,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        &[
            1000000000000000000u128,
            1_999_999_999u128,
            2_001_000_000u128,
        ],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V2a→V4b→V2c. V4_UNLOCK nests sync+V2a swap_calc+V4 swap+take to V2c.
    // Top-level V2c flash; callback repays WETH to V2a then V4_UNLOCK.
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let pm_idx = at.add(PM).unwrap(); // SENTINEL_PM
    let forward_a_idx = at.add(USDC).unwrap(); // 0 (ha forward = token1 = USDC)
    let forward_b_idx = at.add(WETH).unwrap(); // SENTINEL_WETH (hb output = currency1 = WETH)
    let executor_idx = SENTINEL_SELF;
    let zero_idx = SENTINEL_NATIVE;
    let v2a_idx = at
        .add(address!("1111111111111111111111111111111111111111"))
        .unwrap(); // 1
    let v2c_idx = at
        .add(address!("3333333333333333333333333333333333333333"))
        .unwrap(); // 2
    let c0_b_idx = at.add(USDC).unwrap(); // 0 (hb.currency0 = USDC, already present)
    let c1_b_idx = at.add(WETH).unwrap(); // SENTINEL_WETH
    let mut v4_inner = Vec::new();
    v4_inner.extend_from_slice(&encoders::enc_v4_sync(forward_a_idx));
    v4_inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2a_idx, true, pm_idx, 30));
    v4_inner.extend_from_slice(&encoders::enc_v4_settle());
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_b_idx,
            c1_b_idx,
            500,
            10,
            zero_idx,
            true,
            1_999_999_999u128,
        )
        .unwrap(),
    );
    v4_inner.extend_from_slice(
        &encoders::enc_v4_take_compact(forward_b_idx, v2c_idx, 2_001_000_000_000_000_000u128)
            .unwrap(),
    );
    // The CL clamp caps the V4 swap-in below the settled V2 forward, leaving a
    // residual on the settled currency (forward_a). Sweep it back to the
    // executor so the unlock nets to zero (else CurrencyNotSettled at exit).
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_delta(forward_a_idx));
    let mut c_fwd = Vec::new();
    c_fwd.extend_from_slice(
        &encoders::enc_erc20_transfer(SENTINEL_WETH, v2a_idx, 1_000_000_000_000_000_000u128)
            .unwrap(),
    );
    c_fwd.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).unwrap());
    let commands =
        encoders::enc_v2_swap_compact(v2c_idx, true, 2_001_000_000u128, executor_idx, 30, &c_fwd)
            .unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v2_v4_v3() {
    let rust = encode_cmd_3_hop(
        &PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: address!("1111111111111111111111111111111111111111"),
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
                currency1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 500u32,
                tick_spacing: 10i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: true,
            }),
            HopInfo::V3(V3HopInfo {
                pool_address: address!("6666666666666666666666666666666666666666"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 3000u32,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        // consumed_inputs[1] is the V4 hop's clamped swap-in (1 wei below the
        // recorded forward 2_000_000_000) — proves the composer feeds the CL
        // clamp vector, not hop_outputs[0], as the V4 swap-in amount.
        &[2000000000u128, 1_999_999_999u128, 2001000000u128],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V2a→V4b→V3c. Same V4_UNLOCK shape as v2_v4_v2; top-level V3c auto-pay.
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let pm_idx = at.add(PM).unwrap(); // SENTINEL_PM
    let forward_a_idx = at.add(USDC).unwrap(); // 0
    let forward_b_idx = at.add(WETH).unwrap(); // SENTINEL_WETH
    let executor_idx = SENTINEL_SELF;
    let zero_idx = SENTINEL_NATIVE;
    let v2a_idx = at
        .add(address!("1111111111111111111111111111111111111111"))
        .unwrap(); // 1
    let v3c_idx = at
        .add(address!("6666666666666666666666666666666666666666"))
        .unwrap(); // 2
    let c0_b_idx = at.add(USDC).unwrap(); // 0
    let c1_b_idx = at.add(WETH).unwrap(); // SENTINEL_WETH
    let mut v4_inner = Vec::new();
    v4_inner.extend_from_slice(&encoders::enc_v4_sync(forward_a_idx));
    v4_inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2a_idx, true, pm_idx, 30));
    v4_inner.extend_from_slice(&encoders::enc_v4_settle());
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_b_idx,
            c1_b_idx,
            500,
            10,
            zero_idx,
            true,
            // V4 swap-in = consumed_inputs[1] (the CL clamp), 1 wei below the
            // recorded forward 2_000_000_000 — mirrors the parity fixture's
            // consumed_inputs so the composer emits the clamped amount.
            1_999_999_999u128,
        )
        .unwrap(),
    );
    // Exact-match-on-amount (path-73385 class): the V4 take + v3c exit swap-in
    // both use consumed_inputs[2] (=2_001_000_000), NOT the solver's
    // over-predictable out_b (2_001_000_000_000_000_000), so a take can never
    // over-take the pool's actual yield.
    v4_inner.extend_from_slice(
        &encoders::enc_v4_take_compact(forward_b_idx, v3c_idx, 2_001_000_000u128).unwrap(),
    );
    // The CL clamp caps the V4 swap-in below the settled V2 forward, leaving a
    // residual on the settled currency (forward_a). Sweep it back to the
    // executor so the unlock nets to zero (else CurrencyNotSettled at exit).
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_delta(forward_a_idx));
    let mut c_fwd = Vec::new();
    c_fwd.extend_from_slice(
        &encoders::enc_erc20_transfer(SENTINEL_WETH, v2a_idx, 1_000_000_000_000_000_000u128)
            .unwrap(),
    );
    c_fwd.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).unwrap());
    let commands =
        encoders::enc_v3_swap_compact(v3c_idx, true, 2_001_000_000u128, executor_idx, &c_fwd)
            .unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v2_v4_v4() {
    let rust = encode_cmd_3_hop(
        &PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: address!("1111111111111111111111111111111111111111"),
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
                currency1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 500u32,
                tick_spacing: 10i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: true,
            }),
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
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        &[
            1000000000000000000u128,
            1_999_999_999u128,
            2_001_000_000u128,
        ],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V2a→V4b→V4c all inside one V4_UNLOCK. take WETH to V2a, swap_calc V2a→PM,
    // settle, V4b swap_compact, V4c swap_dynamic, settle_all.
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let pm_idx = at.add(PM).unwrap(); // SENTINEL_PM
    let forward_a_idx = at.add(USDC).unwrap(); // 0
    let zero_idx = SENTINEL_NATIVE;
    let c0_b_idx = at.add(USDC).unwrap(); // 0
    let c1_b_idx = at.add(WETH).unwrap(); // SENTINEL_WETH
    let c0_c_idx = at.add(WETH).unwrap(); // SENTINEL_WETH
    let c1_c_idx = at.add(USDC).unwrap(); // 0
    let v2a_idx = at
        .add(address!("1111111111111111111111111111111111111111"))
        .unwrap(); // 1
    let mut v4_inner = Vec::new();
    v4_inner.extend_from_slice(&encoders::enc_v4_sync(forward_a_idx));
    v4_inner.extend_from_slice(
        &encoders::enc_erc20_transfer(SENTINEL_WETH, v2a_idx, 1_000_000_000_000_000_000u128)
            .unwrap(),
    );
    v4_inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2a_idx, true, pm_idx, 30));
    v4_inner.extend_from_slice(&encoders::enc_v4_settle());
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_b_idx,
            c1_b_idx,
            500,
            10,
            zero_idx,
            true,
            1_999_999_999u128, // = consumed_inputs[1] (CL clamp)
        )
        .unwrap(),
    );
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_c_idx,
            c1_c_idx,
            3000,
            60,
            zero_idx,
            true,
            2_001_000_000u128, // = consumed_inputs[2] (CL clamp)
        )
        .unwrap(),
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let expected = v4_envelope(&at, &v4_inner);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v3_v2_v2() {
    let rust = encode_cmd_3_hop(
        &PathInfo::new(vec![
            HopInfo::V3(V3HopInfo {
                pool_address: address!("4444444444444444444444444444444444444444"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 3000u32,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: address!("2222222222222222222222222222222222222222"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 30u16,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: address!("3333333333333333333333333333333333333333"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 30u16,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V3a flash → V2b → V2c. V3a callback: V2b calc, V2c calc, then WETH
    // repays V3a. (Terminal + CL-fed V2 hops are V2_SWAP_CALC, not
    // V2_SWAP_DIRECT — 1-wei exact-out K over-draw class.)
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), None);
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let v2b_idx = at
        .add(address!("2222222222222222222222222222222222222222"))
        .unwrap(); // 0
    let v2c_idx = at
        .add(address!("3333333333333333333333333333333333333333"))
        .unwrap(); // 1
    let v3a_idx = at
        .add(address!("4444444444444444444444444444444444444444"))
        .unwrap(); // 2
    let mut a_fwd = Vec::new();
    a_fwd.extend_from_slice(&encoders::enc_v2_swap_calc(v2b_idx, true, v2c_idx, 30));
    a_fwd.extend_from_slice(&encoders::enc_v2_swap_calc(v2c_idx, true, executor_idx, 30));
    a_fwd.extend_from_slice(
        &encoders::enc_erc20_transfer(weth_idx, v3a_idx, 1_000_000_000_000_000_000u128).unwrap(),
    );
    let commands = encoders::enc_v3_swap_compact(
        v3a_idx,
        true,
        1_000_000_000_000_000_000u128,
        v2b_idx,
        &a_fwd,
    )
    .unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v3_v2_v3() {
    let rust = encode_cmd_3_hop(
        &PathInfo::new(vec![
            HopInfo::V3(V3HopInfo {
                pool_address: address!("4444444444444444444444444444444444444444"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 3000u32,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: address!("2222222222222222222222222222222222222222"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 30u16,
                zfo: true,
            }),
            HopInfo::V3(V3HopInfo {
                pool_address: address!("6666666666666666666666666666666666666666"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 500u32,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        &[
            1000000000000000000u128,
            2000000000u128,
            2_000_999_999_999_999_999u128,
        ],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V3a flash → V2b → V3c. V3a callback: V2b direct to V3c, then WETH repays
    // V3a. V3c is the top-level swap.
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), None);
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let v2b_idx = at
        .add(address!("2222222222222222222222222222222222222222"))
        .unwrap(); // 0
    let v3a_idx = at
        .add(address!("4444444444444444444444444444444444444444"))
        .unwrap(); // 1
    let v3c_idx = at
        .add(address!("6666666666666666666666666666666666666666"))
        .unwrap(); // 2
    let mut v3a_fwd = Vec::new();
    v3a_fwd.extend_from_slice(&encoders::enc_v2_swap_calc(v2b_idx, true, v3c_idx, 30));
    v3a_fwd.extend_from_slice(
        &encoders::enc_erc20_transfer(weth_idx, v3a_idx, 1_000_000_000_000_000_000u128).unwrap(),
    );
    let v3c_fwd = encoders::enc_v3_swap_compact(
        v3a_idx,
        true,
        1_000_000_000_000_000_000u128,
        v2b_idx,
        &v3a_fwd,
    )
    .unwrap();
    let commands = encoders::enc_v3_swap_compact(
        v3c_idx,
        true,
        2_000_999_999_999_999_999u128, // = consumed_inputs[2] (CL clamp)
        executor_idx,
        &v3c_fwd,
    )
    .unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v3_v2_v4() {
    let rust = encode_cmd_3_hop(
        &PathInfo::new(vec![
            HopInfo::V3(V3HopInfo {
                pool_address: address!("4444444444444444444444444444444444444444"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 3000u32,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: address!("2222222222222222222222222222222222222222"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 30u16,
                zfo: true,
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
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        &[
            1000000000000000000u128,
            2000000000u128,
            2_000_999_999_999_999_999u128,
        ],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V3a→V2b→V4c. V4_UNLOCK nests V4c swap+take WETH to executor+settle_delta.
    // V2b callback (a_fwd) wraps V4_UNLOCK + WETH repay to V3a.
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let _ = at.add(PM); // SENTINEL_PM (discarded by the composer)
    let executor_idx = SENTINEL_SELF;
    let zero_idx = SENTINEL_NATIVE;
    let v3a_idx = at
        .add(address!("4444444444444444444444444444444444444444"))
        .unwrap(); // 0
    let v2b_idx = at
        .add(address!("2222222222222222222222222222222222222222"))
        .unwrap(); // 1
    let forward_b_idx = at.add(WETH).unwrap(); // SENTINEL_WETH (hb forward = token1 = WETH)
    let c0_c_idx = at.add(WETH).unwrap(); // SENTINEL_WETH
    let c1_c_idx = at.add(USDC).unwrap(); // 2
    let mut v4_inner = Vec::new();
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_c_idx,
            c1_c_idx,
            500,
            10,
            zero_idx,
            true,
            2_000_999_999_999_999_999u128, // = consumed_inputs[2] (CL clamp)
        )
        .unwrap(),
    );
    v4_inner.extend_from_slice(
        &encoders::enc_v4_take_compact(SENTINEL_WETH, executor_idx, 2_001_000_000u128).unwrap(),
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_delta(forward_b_idx));
    let b_fwd = encoders::enc_v4_unlock(&v4_inner).unwrap();
    let mut a_fwd = Vec::new();
    a_fwd.extend_from_slice(
        &encoders::enc_v2_swap_compact(
            v2b_idx,
            true,
            2_001_000_000_000_000_000u128,
            executor_idx,
            30,
            &b_fwd,
        )
        .unwrap(),
    );
    a_fwd.extend_from_slice(
        &encoders::enc_erc20_transfer(SENTINEL_WETH, v3a_idx, 1_000_000_000_000_000_000u128)
            .unwrap(),
    );
    let commands = encoders::enc_v3_swap_compact(
        v3a_idx,
        true,
        1_000_000_000_000_000_000u128,
        v2b_idx,
        &a_fwd,
    )
    .unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v3_v3_v2() {
    let rust = encode_cmd_3_hop(
        &PathInfo::new(vec![
            HopInfo::V3(V3HopInfo {
                pool_address: address!("4444444444444444444444444444444444444444"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 3000u32,
                zfo: true,
            }),
            HopInfo::V3(V3HopInfo {
                pool_address: address!("5555555555555555555555555555555555555555"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 500u32,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: address!("3333333333333333333333333333333333333333"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 30u16,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        &[
            1000000000000000000u128,
            1_999_999_999u128,
            2_001_000_000u128,
        ],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V3a → V3b → V2c. V3a callback: V2c calc to executor, then WETH repays
    // V3a. V3b is nested inside V3a's forward_data. (Terminal V2 is
    // V2_SWAP_CALC, not V2_SWAP_DIRECT — 1-wei exact-out K over-draw class.)
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), None);
    let weth_idx = SENTINEL_WETH;
    let executor_idx = SENTINEL_SELF;
    let v2c_idx = at
        .add(address!("3333333333333333333333333333333333333333"))
        .unwrap(); // 0
    let v3a_idx = at
        .add(address!("4444444444444444444444444444444444444444"))
        .unwrap(); // 1
    let v3b_idx = at
        .add(address!("5555555555555555555555555555555555555555"))
        .unwrap(); // 2
    let mut v3a_fwd = Vec::new();
    v3a_fwd.extend_from_slice(&encoders::enc_v2_swap_calc(v2c_idx, true, executor_idx, 30));
    v3a_fwd.extend_from_slice(
        &encoders::enc_erc20_transfer(weth_idx, v3a_idx, 1_000_000_000_000_000_000u128).unwrap(),
    );
    let v3b_fwd = encoders::enc_v3_swap_compact(
        v3a_idx,
        true,
        1_000_000_000_000_000_000u128,
        v3b_idx,
        &v3a_fwd,
    )
    .unwrap();
    let commands =
        encoders::enc_v3_swap_compact(v3b_idx, true, 1_999_999_999u128, v2c_idx, &v3b_fwd).unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v3_v3_v3() {
    let rust = encode_cmd_3_hop(
        &PathInfo::new(vec![
            HopInfo::V3(V3HopInfo {
                pool_address: address!("4444444444444444444444444444444444444444"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 3000u32,
                zfo: true,
            }),
            HopInfo::V3(V3HopInfo {
                pool_address: address!("5555555555555555555555555555555555555555"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 500u32,
                zfo: true,
            }),
            HopInfo::V3(V3HopInfo {
                pool_address: address!("6666666666666666666666666666666666666666"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 3000u32,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        &[
            1000000000000000000u128,
            1_999_999_999u128,
            2_000_999_999_999_999_999u128,
        ],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V3a → V3b → V3c, fully nested V3 callbacks (no WETH repayment — each V3
    // auto-pays from the prior swap's output).
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), None);
    let executor_idx = SENTINEL_SELF;
    let v3a_idx = at
        .add(address!("4444444444444444444444444444444444444444"))
        .unwrap(); // 0
    let v3b_idx = at
        .add(address!("5555555555555555555555555555555555555555"))
        .unwrap(); // 1
    let v3c_idx = at
        .add(address!("6666666666666666666666666666666666666666"))
        .unwrap(); // 2
    let v3a_callback: Vec<u8> = Vec::new();
    let v3b_callback = encoders::enc_v3_swap_compact(
        v3a_idx,
        true,
        1_000_000_000_000_000_000u128,
        v3b_idx,
        &v3a_callback,
    )
    .unwrap();
    let v3c_callback =
        encoders::enc_v3_swap_compact(v3b_idx, true, 1_999_999_999u128, v3c_idx, &v3b_callback)
            .unwrap();
    let commands = encoders::enc_v3_swap_compact(
        v3c_idx,
        true,
        2_000_999_999_999_999_999u128, // = consumed_inputs[2] (CL clamp)
        executor_idx,
        &v3c_callback,
    )
    .unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v3_v3_v4() {
    let rust = encode_cmd_3_hop(
        &PathInfo::new(vec![
            HopInfo::V3(V3HopInfo {
                pool_address: address!("4444444444444444444444444444444444444444"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 3000u32,
                zfo: true,
            }),
            HopInfo::V3(V3HopInfo {
                pool_address: address!("5555555555555555555555555555555555555555"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 500u32,
                zfo: true,
            }),
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
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        &[
            1000000000000000000u128,
            1_999_999_999u128,
            2_000_999_999_999_999_999u128,
        ],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V3a→V3b→V4c. V4_UNLOCK nests settle+V4c swap_dynamic+take_compact
    // WETH→V3a (EXACT optimal_input, not full delta — the V4 output exceeds
    // optimal_input by the profit; paying the full delta would donate the
    // profit to the V3 pool, which has no skim) +settle_all (sweeps the
    // residual WETH delta, == the profit, to the executor). V3a callback =
    // V4_UNLOCK; V3b top-level pays PM.
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let pm_idx = at.add(PM).unwrap(); // SENTINEL_PM
    let zero_idx = SENTINEL_NATIVE;
    let v3a_idx = at
        .add(address!("4444444444444444444444444444444444444444"))
        .unwrap(); // 0
    let v3b_idx = at
        .add(address!("5555555555555555555555555555555555555555"))
        .unwrap(); // 1
    let forward_b_idx = at.add(WETH).unwrap(); // SENTINEL_WETH (hb forward = token1 = WETH)
    let c0_c_idx = at.add(WETH).unwrap(); // SENTINEL_WETH
    let c1_c_idx = at.add(USDC).unwrap(); // 2
    let mut v4_inner = Vec::new();
    v4_inner.extend_from_slice(&encoders::enc_v4_settle());
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_c_idx,
            c1_c_idx,
            3000,
            60,
            zero_idx,
            true,
            2_000_999_999_999_999_999u128, // = consumed_inputs[2] (CL clamp)
        )
        .unwrap(),
    );
    v4_inner.extend_from_slice(
        &encoders::enc_v4_take_compact(SENTINEL_WETH, v3a_idx, 1_000_000_000_000_000_000u128)
            .unwrap(),
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let a_fwd = encoders::enc_v4_unlock(&v4_inner).unwrap();
    let b_fwd = encoders::enc_v3_swap_compact(
        v3a_idx,
        true,
        1_000_000_000_000_000_000u128,
        v3b_idx,
        &a_fwd,
    )
    .unwrap();
    let mut commands = encoders::enc_v4_sync(forward_b_idx);
    commands.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3b_idx, true, 1_999_999_999u128, pm_idx, &b_fwd).unwrap(),
    );
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v3_v4_v2() {
    let rust = encode_cmd_3_hop(
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
            HopInfo::V2(V2HopInfo {
                pool_address: address!("3333333333333333333333333333333333333333"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 30u16,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        &[2000000000u128, 1_999_999_999u128, 2001000000u128],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V3a→V4b→V2c. V4_UNLOCK nests V4b swap_compact(consumed_inputs[1]) +
    // take_delta(WETH→V2c) + settle_all.
    // V3a callback (a_fwd) wraps V4_UNLOCK + V2c direct + WETH repay to V3a.
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let pm_idx = at.add(PM).unwrap();
    let executor_idx = SENTINEL_SELF;
    let v3a_idx = at
        .add(address!("4444444444444444444444444444444444444444"))
        .unwrap();
    let v2c_idx = at
        .add(address!("3333333333333333333333333333333333333333"))
        .unwrap();
    let forward_a_idx = at.add(USDC).unwrap(); // V3a output (zfo→token1)
    let forward_b_idx = at.add(WETH).unwrap(); // V4b output (zfo→currency1) → SENTINEL_WETH
    let c0_b_idx = at.add(USDC).unwrap();
    let c1_b_idx = at.add(WETH).unwrap(); // SENTINEL_WETH
    let mut v4_inner = encoders::enc_v4_settle();
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_b_idx,
            c1_b_idx,
            500,
            10,
            SENTINEL_NATIVE,
            true,
            // V4 swap-in = consumed_inputs[1] (the CL clamp), not the full
            // dynamic forward — stops over-fed V4 pools at capacity.
            1_999_999_999u128,
        )
        .unwrap(),
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_take_delta(forward_b_idx, v2c_idx));
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let mut a_fwd = encoders::enc_v4_unlock(&v4_inner).unwrap();
    // Terminal V2 hop is V2_SWAP_CALC (exact-output via hopper), matching the
    // production `v3_v4_v2` composer — not V2_SWAP_DIRECT (460f23bf closes the
    // 1-wei exact-out K over-draw by letting the pair compute its own amount).
    a_fwd.extend_from_slice(&encoders::enc_v2_swap_calc(v2c_idx, true, executor_idx, 30));
    a_fwd.extend_from_slice(
        &encoders::enc_erc20_transfer(SENTINEL_WETH, v3a_idx, 1_000_000_000_000_000_000u128)
            .unwrap(),
    );
    let mut commands = encoders::enc_v4_sync(forward_a_idx);
    commands.extend_from_slice(
        &encoders::enc_v3_swap_compact(
            v3a_idx,
            true,
            1_000_000_000_000_000_000u128,
            pm_idx,
            &a_fwd,
        )
        .unwrap(),
    );
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v3_v4_v3() {
    let rust = encode_cmd_3_hop(
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
            HopInfo::V3(V3HopInfo {
                pool_address: address!("6666666666666666666666666666666666666666"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 3000u32,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        &[
            1000000000000000000u128,
            1_999_999_999u128,
            2_000_999_999_999_999_999u128,
        ],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V3a→V4b→V3c. V4_UNLOCK nests V4b swap_compact(consumed_inputs[1]) +
    // take_compact(WETH→V3c)
    // + settle_all. V3a callback (a_fwd) wraps WETH repay + V4_UNLOCK; V3c
    // callback (c_fwd) wraps V3a. take_compact carries the EXACT out_b (the
    // V3c swap's fixed input) so V3c's IIA is satisfied by construction — the
    // prediction→actual settlement fix for the V3-V4-V3 no-profit.
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let pm_idx = at.add(PM).unwrap();
    let executor_idx = SENTINEL_SELF;
    let v3a_idx = at
        .add(address!("4444444444444444444444444444444444444444"))
        .unwrap();
    let v3c_idx = at
        .add(address!("6666666666666666666666666666666666666666"))
        .unwrap();
    let forward_a_idx = at.add(USDC).unwrap(); // V3a output (zfo→token1)
    let forward_b_idx = at.add(WETH).unwrap(); // V4b output (zfo→currency1) → SENTINEL_WETH
    let c0_b_idx = at.add(USDC).unwrap();
    let c1_b_idx = at.add(WETH).unwrap(); // SENTINEL_WETH
    let mut v4_inner = encoders::enc_v4_settle();
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_b_idx,
            c1_b_idx,
            500,
            10,
            SENTINEL_NATIVE,
            true,
            // V4 swap-in = consumed_inputs[1] (the CL clamp), not the full
            // dynamic forward — stops over-fed V4 pools at capacity.
            1_999_999_999u128,
        )
        .unwrap(),
    );
    v4_inner.extend_from_slice(
        &encoders::enc_v4_take_compact(forward_b_idx, v3c_idx, 2_000_999_999_999_999_999u128)
            .unwrap(),
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let mut a_fwd =
        encoders::enc_erc20_transfer(SENTINEL_WETH, v3a_idx, 1_000_000_000_000_000_000u128)
            .unwrap();
    a_fwd.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).unwrap());
    let c_fwd =
        encoders::enc_v3_swap_compact(v3a_idx, true, 1_000_000_000_000_000_000u128, pm_idx, &a_fwd)
            .unwrap();
    let mut commands = encoders::enc_v4_sync(forward_a_idx);
    commands.extend_from_slice(
        &encoders::enc_v3_swap_compact(
            v3c_idx,
            true,
            2_000_999_999_999_999_999u128,
            executor_idx,
            &c_fwd,
        )
        .unwrap(),
    );
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v3_v4_v4() {
    let rust = encode_cmd_3_hop(
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
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        &[
            2000000000u128,
            1_999_999_999u128,
            2_000_999_999_999_999_999u128,
        ],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V3a→V4b→V4c, all inside one V4_UNLOCK, driven by CL-clamp settlement
    // (W2UWZO/CurrencyNotSettled fix) + the V3a-forward-through-PM routing
    // (0xbe8b8507 SwapAmountCannotBeZero fix): V4_SYNC(forward_a) precedes the
    // V3a swap so the generous V3 optimistic output transfer that delivers
    // forward_a to the PoolManager becomes a settlable PM delta; the V3a
    // callback wraps WETH repay + the V4_UNLOCK and routes forward_a to pm_idx
    // (not the executor) so the unlock's V4_SETTLE sees the positive delta.
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let pm_idx = at.add(PM).unwrap(); // SENTINEL_PM
    let executor_idx = SENTINEL_SELF;
    let v3a_idx = at
        .add(address!("4444444444444444444444444444444444444444"))
        .unwrap();
    let forward_a_idx = at.add(USDC).unwrap(); // V3a output currency (zfo→token1)
    let c0_b_idx = at.add(USDC).unwrap();
    let c1_b_idx = at.add(WETH).unwrap();
    let c0_c_idx = at.add(WETH).unwrap();
    let c1_c_idx = at.add(USDC).unwrap();
    let mut v4_inner = encoders::enc_v4_settle();
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_b_idx,
            c1_b_idx,
            500,
            10,
            SENTINEL_NATIVE,
            true,
            1_999_999_999u128, // = consumed_inputs[1] (CL clamp)
        )
        .unwrap(),
    );
    v4_inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_c_idx,
            c1_c_idx,
            3000,
            60,
            SENTINEL_NATIVE,
            true,
            2_000_999_999_999_999_999u128, // = consumed_inputs[2] (CL clamp)
        )
        .unwrap(),
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_take_delta(SENTINEL_WETH, executor_idx));
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let mut a_fwd =
        encoders::enc_erc20_transfer(SENTINEL_WETH, v3a_idx, 1_000_000_000_000_000_000u128)
            .unwrap();
    a_fwd.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).unwrap());
    let mut commands = encoders::enc_v4_sync(forward_a_idx);
    commands.extend_from_slice(
        &encoders::enc_v3_swap_compact(
            v3a_idx,
            true,
            1_000_000_000_000_000_000u128,
            pm_idx,
            &a_fwd,
        )
        .unwrap(),
    );
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v4_v2_v2() {
    let rust = encode_cmd_3_hop(
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
                pool_address: address!("2222222222222222222222222222222222222222"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 30u16,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: address!("3333333333333333333333333333333333333333"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 30u16,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V4a→V2b→V2c, all inside one V4_UNLOCK. Inner = swap_compact(A) + take_compact(USDC→V2b) +
    // V2_CALC(b→c) + V2_CALC(c→executor) + settle_delta(WETH).
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let executor_idx = SENTINEL_SELF;
    let forward_a_idx = at.add(USDC).unwrap(); // V4a output (zfo→currency1)
    let c0_a_idx = at.add(WETH).unwrap();
    let c1_a_idx = at.add(USDC).unwrap();
    let v2b_idx = at
        .add(address!("2222222222222222222222222222222222222222"))
        .unwrap();
    let v2c_idx = at
        .add(address!("3333333333333333333333333333333333333333"))
        .unwrap();
    let b_cmd = encoders::enc_v2_swap_calc(v2b_idx, true, v2c_idx, 30);
    let c_cmd = encoders::enc_v2_swap_calc(v2c_idx, true, executor_idx, 30);
    let mut inner = encoders::enc_v4_swap_compact(
        c0_a_idx,
        c1_a_idx,
        3000,
        60,
        SENTINEL_NATIVE,
        true,
        1_000_000_000_000_000_000u128,
    )
    .unwrap();
    inner.extend_from_slice(
        &encoders::enc_v4_take_compact(forward_a_idx, v2b_idx, 2_000_000_000u128).unwrap(),
    );
    inner.extend_from_slice(&b_cmd);
    inner.extend_from_slice(&c_cmd);
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(SENTINEL_WETH));
    let commands = encoders::enc_v4_unlock(&inner).unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v4_v2_v3() {
    let rust = encode_cmd_3_hop(
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
                pool_address: address!("2222222222222222222222222222222222222222"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 30u16,
                zfo: true,
            }),
            HopInfo::V3(V3HopInfo {
                pool_address: address!("6666666666666666666666666666666666666666"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 500u32,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        &[
            1000000000000000000u128,
            2000000000u128,
            2_000_999_999_999_999_999u128,
        ],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V4a→V2b→V3c, all inside one V4_UNLOCK (V3c reads the PM delta). Inner =
    // swap_compact(A) + take_compact(USDC→V2b) + V2_CALC(b→V3c) + settle_delta(WETH).
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let executor_idx = SENTINEL_SELF;
    let v3c_idx = at
        .add(address!("6666666666666666666666666666666666666666"))
        .unwrap();
    let forward_a_idx = at.add(USDC).unwrap(); // V4a output (zfo→currency1)
    let _ = at.add(WETH); // V2b output (zfo→token1) — idx discarded (SENTINEL_WETH)
    let c0_a_idx = at.add(WETH).unwrap();
    let c1_a_idx = at.add(USDC).unwrap();
    let v2b_idx = at
        .add(address!("2222222222222222222222222222222222222222"))
        .unwrap();
    let mut v4_inner = encoders::enc_v4_swap_compact(
        c0_a_idx,
        c1_a_idx,
        3000,
        60,
        SENTINEL_NATIVE,
        true,
        1_000_000_000_000_000_000u128,
    )
    .unwrap();
    v4_inner.extend_from_slice(
        &encoders::enc_v4_take_compact(forward_a_idx, v2b_idx, 2_000_000_000u128).unwrap(),
    );
    v4_inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2b_idx, true, v3c_idx, 30));
    v4_inner.extend_from_slice(&encoders::enc_v4_settle_delta(SENTINEL_WETH));
    let commands = encoders::enc_v3_swap_compact(
        v3c_idx,
        true,
        2_000_999_999_999_999_999u128,
        executor_idx,
        &encoders::enc_v4_unlock(&v4_inner).unwrap(),
    )
    .unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v4_v2_v4() {
    let rust = encode_cmd_3_hop(
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
                pool_address: address!("2222222222222222222222222222222222222222"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 30u16,
                zfo: true,
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
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        &[
            1000000000000000000u128,
            2000000000u128,
            2_000_999_999_999_999_999u128,
        ],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V4a→V2b→V4c, all inside one V4_UNLOCK. Inner = swap_compact(A) +
    // take_compact(USDC→V2b) + V2_CALC(b→executor) + swap_compact(C) + settle_all.
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let executor_idx = SENTINEL_SELF;
    let forward_a_idx = at.add(USDC).unwrap(); // V4a output (zfo→currency1)
    let _ = at.add(WETH); // V2b output (zfo→token1) — idx discarded (SENTINEL_WETH)
    let c0_a_idx = at.add(WETH).unwrap();
    let c1_a_idx = at.add(USDC).unwrap();
    let c0_c_idx = at.add(WETH).unwrap();
    let c1_c_idx = at.add(USDC).unwrap();
    let v2b_idx = at
        .add(address!("2222222222222222222222222222222222222222"))
        .unwrap();
    let mut inner = encoders::enc_v4_swap_compact(
        c0_a_idx,
        c1_a_idx,
        3000,
        60,
        SENTINEL_NATIVE,
        true,
        1_000_000_000_000_000_000u128,
    )
    .unwrap();
    inner.extend_from_slice(
        &encoders::enc_v4_take_compact(forward_a_idx, v2b_idx, 2_000_000_000u128).unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2b_idx, true, executor_idx, 30));
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_c_idx,
            c1_c_idx,
            500,
            10,
            SENTINEL_NATIVE,
            true,
            2_000_999_999_999_999_999u128,
        )
        .unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let commands = encoders::enc_v4_unlock(&inner).unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v4_v3_v2() {
    let rust = encode_cmd_3_hop(
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
                pool_address: address!("5555555555555555555555555555555555555555"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 500u32,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: address!("3333333333333333333333333333333333333333"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 30u16,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        &[
            1000000000000000000u128,
            1_999_999_999u128,
            2001000000000000000u128,
        ],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V4a→V3b→V2c, all inside one V4_UNLOCK. Inner = swap_compact(A) +
    // V3_SWAP_COMPACT(b: V4_TAKE_COMPACT(USDC→V3b) + V2_CALC(c→executor)) + settle_delta(WETH).
    // Terminal V2 is V2_SWAP_CALC, not V2_SWAP_DIRECT (1-wei exact-out K over-draw).
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let executor_idx = SENTINEL_SELF;
    let v3b_idx = at
        .add(address!("5555555555555555555555555555555555555555"))
        .unwrap();
    let forward_a_idx = at.add(USDC).unwrap(); // V4a output (zfo→currency1)
    let _ = at.add(WETH); // V3b output (zfo→token1) — idx discarded (SENTINEL_WETH)
    let c0_a_idx = at.add(WETH).unwrap();
    let c1_a_idx = at.add(USDC).unwrap();
    let v2c_idx = at
        .add(address!("3333333333333333333333333333333333333333"))
        .unwrap();
    let mut b_fwd =
        encoders::enc_v4_take_compact(forward_a_idx, v3b_idx, 2_000_000_000u128).unwrap();
    b_fwd.extend_from_slice(&encoders::enc_v2_swap_calc(v2c_idx, true, executor_idx, 30));
    let mut inner = encoders::enc_v4_swap_compact(
        c0_a_idx,
        c1_a_idx,
        3000,
        60,
        SENTINEL_NATIVE,
        true,
        1_000_000_000_000_000_000u128,
    )
    .unwrap();
    inner.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3b_idx, true, 1_999_999_999u128, v2c_idx, &b_fwd).unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(SENTINEL_WETH));
    let commands = encoders::enc_v4_unlock(&inner).unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v4_v3_v3() {
    let rust = encode_cmd_3_hop(
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
                pool_address: address!("5555555555555555555555555555555555555555"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 500u32,
                zfo: true,
            }),
            HopInfo::V3(V3HopInfo {
                pool_address: address!("6666666666666666666666666666666666666666"),
                token0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                token1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 3000u32,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        &[
            1000000000000000000u128,
            1_999_999_999u128,
            2_000_999_999_999_999_999u128,
        ],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V4a→V3b→V3c, all inside one V4_UNLOCK. Inner = swap_compact(A) +
    // V3_SWAP_COMPACT(c: V3_SWAP_COMPACT(b: V4_TAKE_COMPACT(USDC→V3b), →executor)) + settle_delta(WETH).
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let executor_idx = SENTINEL_SELF;
    let _ = at.add(PM); // SENTINEL_PM (discarded by the composer)
    let v3b_idx = at
        .add(address!("5555555555555555555555555555555555555555"))
        .unwrap();
    let v3c_idx = at
        .add(address!("6666666666666666666666666666666666666666"))
        .unwrap();
    let forward_a_idx = at.add(USDC).unwrap(); // V4a output (zfo→currency1)
    let b_fwd = encoders::enc_v4_take_compact(forward_a_idx, v3b_idx, 2_000_000_000u128).unwrap();
    let c0_a_idx = at.add(WETH).unwrap();
    let c1_a_idx = at.add(USDC).unwrap();
    let inner_v3b =
        encoders::enc_v3_swap_compact(v3b_idx, true, 1_999_999_999u128, v3c_idx, &b_fwd).unwrap();
    let inner_v3c = encoders::enc_v3_swap_compact(
        v3c_idx,
        true,
        2_000_999_999_999_999_999u128,
        executor_idx,
        &inner_v3b,
    )
    .unwrap();
    let mut inner = encoders::enc_v4_swap_compact(
        c0_a_idx,
        c1_a_idx,
        3000,
        60,
        SENTINEL_NATIVE,
        true,
        1_000_000_000_000_000_000u128,
    )
    .unwrap();
    inner.extend_from_slice(&inner_v3c);
    inner.extend_from_slice(&encoders::enc_v4_settle_delta(SENTINEL_WETH));
    let commands = encoders::enc_v4_unlock(&inner).unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v4_v3_v4() {
    let rust = encode_cmd_3_hop(
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
                pool_address: address!("5555555555555555555555555555555555555555"),
                token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 500u32,
                zfo: true,
            }),
            HopInfo::V4(V4HopInfo {
                pool_manager_address: address!("000000000004444c5dc75cB358380D2e3dE08A90"),
                pool_id_hex: "0x1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
                currency0_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                currency1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
                fee: 3000u32,
                tick_spacing: 10i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        &[
            1000000000000000000u128,
            1_999_999_999u128,
            2_000_999_999_999_999_999u128,
        ],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V4a→V3b→V4c, all inside one V4_UNLOCK. Inner = swap_compact(A) + sync(forward_b) +
    // V3_SWAP_COMPACT(b: V4_TAKE_COMPACT(USDC→V3b), →pm) + settle + swap_compact(C) + settle_all.
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let pm_idx = at.add(PM).unwrap();
    let v3b_idx = at
        .add(address!("5555555555555555555555555555555555555555"))
        .unwrap();
    let forward_a_idx = at.add(USDC).unwrap(); // V4a output (zfo→currency1)
    let forward_b_idx = at.add(WETH).unwrap(); // V3b output (zfo→token1) → SENTINEL_WETH
    let b_fwd = encoders::enc_v4_take_compact(forward_a_idx, v3b_idx, 2_000_000_000u128).unwrap();
    let c0_a_idx = at.add(WETH).unwrap();
    let c1_a_idx = at.add(USDC).unwrap();
    let c0_c_idx = at.add(WETH).unwrap();
    let c1_c_idx = at.add(USDC).unwrap();
    let mut inner = encoders::enc_v4_swap_compact(
        c0_a_idx,
        c1_a_idx,
        3000,
        60,
        SENTINEL_NATIVE,
        true,
        1_000_000_000_000_000_000u128,
    )
    .unwrap();
    inner.extend_from_slice(&encoders::enc_v4_sync(forward_b_idx));
    inner.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3b_idx, true, 1_999_999_999u128, pm_idx, &b_fwd).unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_v4_settle());
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_c_idx,
            c1_c_idx,
            3000,
            10,
            SENTINEL_NATIVE,
            true,
            2_000_999_999_999_999_999u128,
        )
        .unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let commands = encoders::enc_v4_unlock(&inner).unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v4_v4_v2() {
    let rust = encode_cmd_3_hop(
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
                // b's forward (zfo→currency1) = WBTC — NOT a's input (WETH), so
                // the take of b's full output doesn't overshoot PM[WETH] (the
                // coherent v4_v4_v2 subspace the Plan+validator accepts).
                currency1_address: address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"),
                fee: 500u32,
                tick_spacing: 10i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: address!("3333333333333333333333333333333333333333"),
                // consumes WBTC (b's forward), outputs WETH (terminal).
                token0_address: address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 30u16,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        &[1000000000000000000u128, 1_999_999_999u128, 2001000000u128],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V4a→V4b→V2c, all inside one V4_UNLOCK. Inner = swap_compact(A) +
    // swap_compact(B) + take_compact(WBTC→V2c) + V2_SWAP_CALC(c→executor) +
    // settle_all.
    // Terminal V2 hop is V2_SWAP_CALC (exact-output via hopper), matching the
    // production `v3_v4_v2`/`v4_v4_v2` composers — not V2_SWAP_DIRECT (460f23bf
    // / path-182449 closes the 1-wei exact-out K over-draw).
    let wbtc = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let executor_idx = SENTINEL_SELF;
    let forward_b_idx = at.add(wbtc).unwrap(); // V4b output (zfo→currency1) = WBTC
    let v2c_idx = at
        .add(address!("3333333333333333333333333333333333333333"))
        .unwrap();
    let c_cmd = encoders::enc_v2_swap_calc(v2c_idx, true, executor_idx, 30);
    let c0_a_idx = at.add(WETH).unwrap();
    let c1_a_idx = at.add(USDC).unwrap();
    let c0_b_idx = at.add(USDC).unwrap();
    let c1_b_idx = at.add(wbtc).unwrap();
    let mut inner = encoders::enc_v4_swap_compact(
        c0_a_idx,
        c1_a_idx,
        3000,
        60,
        SENTINEL_NATIVE,
        true,
        1_000_000_000_000_000_000u128,
    )
    .unwrap();
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_b_idx,
            c1_b_idx,
            500,
            10,
            SENTINEL_NATIVE,
            true,
            1_999_999_999u128, // = consumed_inputs[1] (CL clamp)
        )
        .unwrap(),
    );
    inner.extend_from_slice(
        &encoders::enc_v4_take_compact(forward_b_idx, v2c_idx, 2_001_000_000_000_000_000u128)
            .unwrap(),
    );
    inner.extend_from_slice(&c_cmd);
    inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let commands = encoders::enc_v4_unlock(&inner).unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v4_v4_v3() {
    let rust = encode_cmd_3_hop(
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
                currency1_address: address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"),
                fee: 500u32,
                tick_spacing: 10i32,
                hook_address: address!("0000000000000000000000000000000000000000"),
                zfo: true,
            }),
            HopInfo::V3(V3HopInfo {
                pool_address: address!("6666666666666666666666666666666666666666"),
                token0_address: address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"),
                token1_address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 3000u32,
                zfo: true,
            }),
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        &[
            1000000000000000000u128,
            2000000000u128,
            2_000_999_999_999_999_999u128,
        ],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V4a→V4b→V3c, all inside one V4_UNLOCK. Inner = swap_compact(A) +
    // swap_dynamic(B) + V3_SWAP_COMPACT(c: V4_TAKE_COMPACT(WETH→V3c), →executor) + settle_all.
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let executor_idx = SENTINEL_SELF;
    let wbtc = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
    let forward_b_idx = at.add(wbtc).unwrap(); // V4b output (zfo→currency1) = WBTC
    let v3c_idx = at
        .add(address!("6666666666666666666666666666666666666666"))
        .unwrap();
    // Exact-match-on-amount (path-73385 class): the V4 take uses
    // consumed_inputs[2] (=2_000_999_999_999_999_999, matching the v3c exit
    // swap-in below), NOT the over-predictable out_b (2_001_000_000_000_000_000).
    let c_take =
        encoders::enc_v4_take_compact(forward_b_idx, v3c_idx, 2_000_999_999_999_999_999u128)
            .unwrap();
    let c0_a_idx = at.add(WETH).unwrap();
    let c1_a_idx = at.add(USDC).unwrap();
    let c0_b_idx = at.add(USDC).unwrap();
    let c1_b_idx = at.add(wbtc).unwrap();
    let mut inner = encoders::enc_v4_swap_compact(
        c0_a_idx,
        c1_a_idx,
        3000,
        60,
        SENTINEL_NATIVE,
        true,
        1_000_000_000_000_000_000u128,
    )
    .unwrap();
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_b_idx,
            c1_b_idx,
            500,
            10,
            SENTINEL_NATIVE,
            true,
            2_000_000_000u128, // = consumed_inputs[1] (CL clamp)
        )
        .unwrap(),
    );
    inner.extend_from_slice(
        &encoders::enc_v3_swap_compact(
            v3c_idx,
            true,
            2_000_999_999_999_999_999u128,
            executor_idx,
            &c_take,
        )
        .unwrap(),
    );
    inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let commands = encoders::enc_v4_unlock(&inner).unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v4_v4_v4() {
    let rust = encode_cmd_3_hop(
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
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        &[
            1000000000000000000u128,
            1_999_999_999u128,
            2_000_999_999_999_999_999u128,
        ],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V4a→V4b→V4c (default opts). No native↔WETH gap at either boundary → compact path:
    // swap_compact(A) + swap_dynamic(B) + swap_dynamic(C) + take_delta(USDC→executor) + settle_all.
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let executor_idx = SENTINEL_SELF;
    let c0_a_idx = at.add(WETH).unwrap();
    let c1_a_idx = at.add(USDC).unwrap();
    let c0_b_idx = at.add(USDC).unwrap();
    let c1_b_idx = at.add(WETH).unwrap();
    let c0_c_idx = at.add(WETH).unwrap();
    let c1_c_idx = at.add(USDC).unwrap();
    let mut inner = encoders::enc_v4_swap_compact(
        c0_a_idx,
        c1_a_idx,
        3000,
        60,
        SENTINEL_NATIVE,
        true,
        1_000_000_000_000_000_000u128,
    )
    .unwrap();
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_b_idx,
            c1_b_idx,
            500,
            10,
            SENTINEL_NATIVE,
            true,
            1_999_999_999u128, // = consumed_inputs[1] (CL clamp)
        )
        .unwrap(),
    );
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_c_idx,
            c1_c_idx,
            3000,
            60,
            SENTINEL_NATIVE,
            true,
            2_000_999_999_999_999_999u128, // = consumed_inputs[2] (CL clamp)
        )
        .unwrap(),
    );
    // Profit: hop C output currency (zfo→currency1) = USDC (ERC-20) → take_delta(USDC, executor).
    let profit_idx = at.add(USDC).unwrap();
    inner.extend_from_slice(&encoders::enc_v4_take_delta(profit_idx, executor_idx));
    inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let commands = encoders::enc_v4_unlock(&inner).unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v4_v4_v4_batch() {
    let rust = encode_cmd_3_hop(
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
        ]),
        1000000000000000000u128,
        &[2000000000u128, 2001000000000000000u128, 2001000000u128],
        &[
            1000000000000000000u128,
            1_999_999_999u128,
            2_000_999_999_999_999_999u128,
        ],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions {
            erc6909_profit: false,
            use_v4_batch: true,
            ..Default::default()
        },
    );
    // V4a→V4b→V4c (use_v4_batch). No gap → single V4_BATCH of 3 swaps
    // (B/C = clamped amounts) + take_delta(USDC→executor) + settle_all.
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let executor_idx = SENTINEL_SELF;
    let c0_a_idx = at.add(WETH).unwrap();
    let c1_a_idx = at.add(USDC).unwrap();
    let c0_b_idx = at.add(USDC).unwrap();
    let c1_b_idx = at.add(WETH).unwrap();
    let c0_c_idx = at.add(WETH).unwrap();
    let c1_c_idx = at.add(USDC).unwrap();
    let batch = [
        encoders::V4BatchEntry {
            c0_idx: c0_a_idx,
            c1_idx: c1_a_idx,
            fee: 3000,
            tick_spacing: 60,
            hooks_idx: SENTINEL_NATIVE,
            zfo: true,
            amount_u96: 1_000_000_000_000_000_000u128,
        },
        encoders::V4BatchEntry {
            c0_idx: c0_b_idx,
            c1_idx: c1_b_idx,
            fee: 500,
            tick_spacing: 10,
            hooks_idx: SENTINEL_NATIVE,
            zfo: true,
            amount_u96: 1_999_999_999u128, // = consumed_inputs[1] (CL clamp)
        },
        encoders::V4BatchEntry {
            c0_idx: c0_c_idx,
            c1_idx: c1_c_idx,
            fee: 3000,
            tick_spacing: 60,
            hooks_idx: SENTINEL_NATIVE,
            zfo: true,
            amount_u96: 2_000_999_999_999_999_999u128, // = consumed_inputs[2] (CL clamp)
        },
    ];
    let mut inner = encoders::enc_v4_batch(&batch).unwrap();
    // profit currency (hc zfo→currency1) = USDC ≠ native/WETH → explicit take_delta.
    let profit_idx = at.add(USDC).unwrap();
    inner.extend_from_slice(&encoders::enc_v4_take_delta(profit_idx, executor_idx));
    inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let commands = encoders::enc_v4_unlock(&inner).unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}

#[test]
fn parity_v4_v4_v4_erc6909() {
    let rust = encode_cmd_3_hop(
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
        ]),
        1000000000000000000u128,
        &[
            2000000000u128,
            2001000000000000000u128,
            2001000000000000000u128,
        ],
        &[
            1000000000000000000u128,
            1_999_999_999u128,
            2_000_999_999_999_999_999u128,
        ],
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions {
            erc6909_profit: true,
            use_v4_batch: false,
            ..Default::default()
        },
    );
    // V4a→V4b→V4c (erc6909_profit, but profit currency = USDC ≠ WETH so the ERC6909
    // mint branch is NOT taken). Compact path: swap_compact(A) + swap_dynamic(B) +
    // swap_dynamic(C) + take_delta(USDC→executor) + settle_all. (erc6909 only fires when the
    // final hop outputs WETH; here it outputs USDC, so this is byte-identical to the default.)
    let mut at = AddressTable::with_sentinels(Some(WETH), Some(EXECUTOR), Some(PM));
    let executor_idx = SENTINEL_SELF;
    let c0_a_idx = at.add(WETH).unwrap();
    let c1_a_idx = at.add(USDC).unwrap();
    let c0_b_idx = at.add(USDC).unwrap();
    let c1_b_idx = at.add(WETH).unwrap();
    let c0_c_idx = at.add(WETH).unwrap();
    let c1_c_idx = at.add(USDC).unwrap();
    let mut inner = encoders::enc_v4_swap_compact(
        c0_a_idx,
        c1_a_idx,
        3000,
        60,
        SENTINEL_NATIVE,
        true,
        1_000_000_000_000_000_000u128,
    )
    .unwrap();
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_b_idx,
            c1_b_idx,
            500,
            10,
            SENTINEL_NATIVE,
            true,
            1_999_999_999u128, // = consumed_inputs[1] (CL clamp)
        )
        .unwrap(),
    );
    inner.extend_from_slice(
        &encoders::enc_v4_swap_compact(
            c0_c_idx,
            c1_c_idx,
            3000,
            60,
            SENTINEL_NATIVE,
            true,
            2_000_999_999_999_999_999u128, // = consumed_inputs[2] (CL clamp)
        )
        .unwrap(),
    );
    let profit_idx = at.add(USDC).unwrap();
    inner.extend_from_slice(&encoders::enc_v4_take_delta(profit_idx, executor_idx));
    inner.extend_from_slice(&encoders::enc_v4_settle_all());
    let commands = encoders::enc_v4_unlock(&inner).unwrap();
    let mut expected = encoders::enc_preamble(&at);
    expected.extend_from_slice(&commands);
    assert_eq!(rust, Some(expected));
}
