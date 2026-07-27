// 3-hop composer regression vectors.
//
// Golden-master expected bytes for `encode_cmd_3_hop` across all 27
// V2×V3×V4 hop combinations (plus the `use_v4_batch` / `erc6909_profit`
// variants for V4-V4-V4). The Rust `enc_*` primitive sequence is the canonical
// opcode source (ADR-005); these constants record the composed bytestream so a
// composer change that alters output is a visible, reviewable diff. The
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

#[allow(dead_code)]
const PM: Address = address!("000000000004444c5dc75cB358380D2e3dE08A90");
const EXECUTOR: Address = address!("DeAd0000000000000000000000000000000000Be");
const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
const USDC: Address = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");

/// Build `enc_preamble(&at) + enc_v4_unlock(&inner)` — the V4 envelope every
/// V4-containing 3-hop composer wraps around its `inner` opcode sequence.
#[allow(dead_code)]
fn v4_envelope(at: &AddressTable, inner: &[u8]) -> Vec<u8> {
    let mut out = encoders::enc_preamble(at);
    out.extend_from_slice(&encoders::enc_v4_unlock(inner).unwrap());
    out
}

fn hx(s: &[u8]) -> Vec<u8> {
    s.to_vec()
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
        2_001_000_000_000_000_000u128,
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
        &encoders::enc_v4_take_compact(SENTINEL_WETH, v2a_idx, 1_000_000_000_000_000_000u128)
            .unwrap(), // placeholder
    );
    inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2a_idx, true, v2b_idx, 30));
    inner.extend_from_slice(&encoders::enc_v2_swap_calc(v2b_idx, true, pm_idx, 30));
    inner.extend_from_slice(&encoders::enc_v4_settle());
    inner.extend_from_slice(&encoders::enc_v4_swap_dynamic(
        c0_idx, c1_idx, 3000, 60, zero_idx, true,
    ));
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
        encoders::enc_v3_swap_compact(v3b_idx, true, 2_000_000_000u128, v2c_idx, &b_fwd).unwrap();
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
        encoders::enc_v3_swap_compact(v3b_idx, true, 2_000_000_000u128, v3c_idx, &v3b_fwd).unwrap();
    let commands = encoders::enc_v3_swap_compact(
        v3c_idx,
        true,
        2_001_000_000_000_000_000u128,
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
        &[
            2000000000u128,
            2001000000000000000u128,
            2001000000000000000u128,
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
    let c0_idx = at.add(WETH).unwrap(); // SENTINEL_WETH (hc.currency0 = WETH)
    let c1_idx = at.add(USDC).unwrap(); // 2 (hc.currency1 = USDC)
    let forward_b_idx = at.add(WETH).unwrap(); // SENTINEL_WETH (hb forward = token1 = WETH)
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
            2_001_000_000_000_000_000u128,
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
    v4_inner.extend_from_slice(&encoders::enc_v4_settle());
    let mut b_fwd = encoders::enc_v4_unlock(&v4_inner).unwrap();
    b_fwd.extend_from_slice(
        &encoders::enc_v2_swap_direct(v2a_idx, true, 2_000_000_000u128, v3b_idx).unwrap(),
    );
    let mut commands = encoders::enc_v4_sync(forward_b_idx);
    commands.extend_from_slice(
        &encoders::enc_v3_swap_compact(v3b_idx, true, 2_000_000_000u128, pm_idx, &b_fwd).unwrap(),
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
            2_000_000_000u128,
        )
        .unwrap(),
    );
    v4_inner.extend_from_slice(
        &encoders::enc_v4_take_compact(forward_b_idx, v2c_idx, 2_001_000_000_000_000_000u128)
            .unwrap(),
    );
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
            2_000_000_000u128,
        )
        .unwrap(),
    );
    v4_inner.extend_from_slice(
        &encoders::enc_v4_take_compact(forward_b_idx, v3c_idx, 2_001_000_000_000_000_000u128)
            .unwrap(),
    );
    let mut c_fwd = Vec::new();
    c_fwd.extend_from_slice(
        &encoders::enc_erc20_transfer(SENTINEL_WETH, v2a_idx, 1_000_000_000_000_000_000u128)
            .unwrap(),
    );
    c_fwd.extend_from_slice(&encoders::enc_v4_unlock(&v4_inner).unwrap());
    let commands = encoders::enc_v3_swap_compact(
        v3c_idx,
        true,
        2_001_000_000_000_000_000u128,
        executor_idx,
        &c_fwd,
    )
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
        &encoders::enc_v4_take_compact(SENTINEL_WETH, v2a_idx, 1_000_000_000_000_000_000u128)
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
            2_000_000_000u128,
        )
        .unwrap(),
    );
    v4_inner.extend_from_slice(&encoders::enc_v4_swap_dynamic(
        c0_c_idx, c1_c_idx, 3000, 60, zero_idx, true,
    ));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V3a flash → V2b → V2c. V3a callback: V2b direct, V2c direct, then WETH
    // repays V3a.
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
    a_fwd.extend_from_slice(
        &encoders::enc_v2_swap_direct(v2b_idx, true, 2_001_000_000_000_000_000u128, v2c_idx)
            .unwrap(),
    );
    a_fwd.extend_from_slice(
        &encoders::enc_v2_swap_direct(v2c_idx, true, 2_001_000_000u128, executor_idx).unwrap(),
    );
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
    v3a_fwd.extend_from_slice(
        &encoders::enc_v2_swap_direct(v2b_idx, true, 2_001_000_000_000_000_000u128, v3c_idx)
            .unwrap(),
    );
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
        2_001_000_000_000_000_000u128,
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
            2_001_000_000_000_000_000u128,
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V3a → V3b → V2c. V3a callback: V2c direct to executor, then WETH repays
    // V3a. V3b is nested inside V3a's forward_data.
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
    v3a_fwd.extend_from_slice(
        &encoders::enc_v2_swap_direct(v2c_idx, true, 2_001_000_000u128, executor_idx).unwrap(),
    );
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
        encoders::enc_v3_swap_compact(v3b_idx, true, 2_000_000_000u128, v2c_idx, &v3b_fwd).unwrap();
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
        encoders::enc_v3_swap_compact(v3b_idx, true, 2_000_000_000u128, v3c_idx, &v3b_callback)
            .unwrap();
    let commands = encoders::enc_v3_swap_compact(
        v3c_idx,
        true,
        2_001_000_000_000_000_000u128,
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    // V3a→V3b→V4c. V4_UNLOCK nests settle+V4c swap_dynamic+take_delta WETH→V3a
    // +settle_all. V3a callback = V4_UNLOCK; V3b top-level pays PM.
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
    v4_inner.extend_from_slice(&encoders::enc_v4_swap_dynamic(
        c0_c_idx, c1_c_idx, 3000, 60, zero_idx, true,
    ));
    v4_inner.extend_from_slice(&encoders::enc_v4_take_delta(SENTINEL_WETH, v3a_idx));
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
        &encoders::enc_v3_swap_compact(v3b_idx, true, 2_000_000_000u128, pm_idx, &b_fwd).unwrap(),
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x00\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x54\x02\x30\x00\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\xfc\x2f\x50\x0e\x55\x41\x02\xfe\x01\xf4\x00\x0a\xff\x01\x53\xfe\x01\x57\x22\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x44\xd6\x40\xfd\x10\xfe\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x00\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x54\x02\x30\x01\x01\x00\x00\x00\x00\x1b\xc4\xfa\xe5\xf3\x8e\x80\x00\xfd\x30\x30\x00\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\xfc\x1f\x10\xfe\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x50\x0e\x55\x41\x02\xfe\x01\xf4\x00\x0a\xff\x01\x53\xfe\x01\x57")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x30\x00\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\xfd\x4b\x50\x3a\x55\x40\x01\xfe\x01\xf4\x00\x0a\xff\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x40\xfe\x01\x0b\xb8\x00\x3c\xff\x01\x00\x00\x00\x00\x1b\xc4\xfa\xe5\xf3\x8e\x80\x00\x52\xfe\xfd\x00\x00\x00\x00\x00\x00\x00\x00\x77\x44\xd6\x40\x10\xfe\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\x00\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x00\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\xff\x50\x32\x40\xfe\x00\x0b\xb8\x00\x3c\xff\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x52\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x21\x01\x01\x02\x00\x1e\x21\x02\x01\xfd\x00\x1e\x56\xfe")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\x00\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\xff\x30\x00\x01\x00\x00\x00\x00\x1b\xc4\xfa\xe5\xf3\x8e\x80\x00\xfd\x2e\x50\x2c\x40\xfe\x01\x0b\xb8\x00\x3c\xff\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x52\x01\x02\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x21\x02\x01\x00\x00\x1e\x56\xfe")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\x00\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\xff\x50\x40\x40\xfe\x00\x0b\xb8\x00\x3c\xff\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x52\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x21\x01\x01\xfd\x00\x1e\x40\xfe\x00\x01\xf4\x00\x0a\xff\x01\x00\x00\x00\x00\x1b\xc4\xfa\xe5\xf3\x8e\x80\x00\x57")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\x00\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\xff\x50\x47\x40\xfe\x01\x0b\xb8\x00\x3c\xff\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x30\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x02\x1f\x52\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x22\x02\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x44\xd6\x40\xfd\x56\xfe")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x00\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x50\x48\x40\xfe\x02\x0b\xb8\x00\x3c\xff\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x30\x01\x01\x00\x00\x00\x00\x1b\xc4\xfa\xe5\xf3\x8e\x80\x00\xfd\x20\x30\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x01\x0f\x52\x02\x00\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x56\xfe")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x50\x4e\x40\xfe\x01\x0b\xb8\x00\x3c\xff\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x54\xfe\x30\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\xfc\x0f\x52\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x55\x40\xfe\x01\x0b\xb8\x00\x0a\xff\x01\x00\x00\x00\x00\x1b\xc4\xfa\xe5\xf3\x8e\x80\x00\x57")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x50\x3e\x40\xfe\x01\x0b\xb8\x00\x3c\xff\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x41\x01\xfe\x01\xf4\x00\x0a\xff\x01\x52\xfe\x00\x00\x00\x00\x00\x1b\xc4\xfa\xe5\xf3\x8e\x80\x00\x22\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x44\xd6\x40\xfd\x57")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x50\x3f\x40\xfe\x01\x0b\xb8\x00\x3c\xff\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x41\x01\xfe\x01\xf4\x00\x0a\xff\x01\x30\x00\x01\x00\x00\x00\x00\x1b\xc4\xfa\xe5\xf3\x8e\x80\x00\xfd\x0f\x52\xfe\x00\x00\x00\x00\x00\x1b\xc4\xfa\xe5\xf3\x8e\x80\x00\x57")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x50\x2b\x40\xfe\x00\x0b\xb8\x00\x3c\xff\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x41\x00\xfe\x01\xf4\x00\x0a\xff\x01\x41\xfe\x00\x0b\xb8\x00\x3c\xff\x01\x53\x00\xfd\x57")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions {
            erc6909_profit: false,
            use_v4_batch: true,
        },
    );
    assert_eq!(rust, Some(hx(b"\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x50\x42\x42\x03\xfe\x00\x0b\xb8\x00\x3c\xff\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x00\xfe\x01\xf4\x00\x0a\xff\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\xfe\x00\x0b\xb8\x00\x3c\xff\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x53\x00\xfd\x57")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions {
            erc6909_profit: true,
            use_v4_batch: false,
        },
    );
    assert_eq!(rust, Some(hx(b"\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x50\x2b\x40\xfe\x00\x0b\xb8\x00\x3c\xff\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x41\x00\xfe\x01\xf4\x00\x0a\xff\x01\x41\xfe\x00\x0b\xb8\x00\x3c\xff\x01\x53\x00\xfd\x57")));
}
