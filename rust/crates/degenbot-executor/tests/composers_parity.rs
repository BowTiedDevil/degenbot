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
    clippy::needless_pass_by_value
)]

use alloy::primitives::{address, U256};
use degenbot_executor::composers::{
    self, encode_cmd_stream, encode_execute_call, CmdExecutorComposer, EncodeOptions, HopInfo,
    PathInfo, V2HopInfo, V3HopInfo, V4HopInfo, V4PoolKeyConfig, V4SwapAmounts,
    V4V3ArbitragePayload, V4V4ArbitragePayload,
};

fn hx(s: &[u8]) -> Vec<u8> {
    s.to_vec()
}

#[test]
fn parity_v4v4_same_currency() {
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x50\x22\x40\xfe\x00\x0b\xb8\x00\x3c\xff\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x41\x00\xfe\x01\xf4\x00\x0a\xff\x01\x53\xfe\xfd\x57")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x50\x60\x40\xff\x00\x0b\xb8\x00\x3c\xff\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x52\xff\xfd\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x12\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x40\xfe\x00\x01\xf4\x00\x0a\xff\x00\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x56\xfe\x53\xfe\xfd\x57")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x50\x60\x40\x00\xfe\x0b\xb8\x00\x3c\xff\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x52\xfe\xfd\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x13\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x40\xff\x00\x01\xf4\x00\x0a\xff\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x56\xff\x53\x00\xfd\x57")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions {
            erc6909_profit: false,
            use_v4_batch: true,
        },
    );
    assert_eq!(rust, Some(hx(b"\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x50\x2b\x42\x02\xfe\x00\x0b\xb8\x00\x3c\xff\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x00\xfe\x01\xf4\x00\x0a\xff\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x57")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions {
            erc6909_profit: true,
            use_v4_batch: false,
        },
    );
    assert_eq!(rust, Some(hx(b"\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x50\x2e\x40\xfe\x00\x0b\xb8\x00\x3c\xff\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x41\x00\xfe\x01\xf4\x00\x0a\xff\x01\x58\xfe\xfd\x00\x00\x00\x00\x0d\xe4\x44\x32\x4c\x2a\x80\x00\x57")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions {
            erc6909_profit: false,
            use_v4_batch: true,
        },
    );
    assert_eq!(rust, Some(hx(b"\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x50\x60\x40\x00\xfe\x0b\xb8\x00\x3c\xff\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x52\xfe\xfd\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x13\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x40\xff\x00\x01\xf4\x00\x0a\xff\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x56\xff\x53\x00\xfd\x57")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\x00\x11\x11\x11\x11\x11\x11\x11\x11\x11\x11\x11\x11\x11\x11\x11\x11\x11\x11\x11\x11\xff\x50\x59\x40\x00\xff\x01\xf4\x00\x0a\xff\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x52\xff\xfd\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x12\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x30\x01\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\xfd\x00\x56\x00\x57")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\x00\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\xff\x50\x38\x40\xfe\x00\x0b\xb8\x00\x3c\xff\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x52\x00\xfd\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x30\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\xfd\x00\x56\xfe\x57")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\x00\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\xff\x50\x59\x40\xff\x00\x0b\xb8\x00\x3c\xff\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x52\x00\xfd\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x30\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\xfd\x00\x13\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x56\xff\x57")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x30\x00\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\xfd\x48\x50\x37\x54\x01\x10\x01\xfc\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x55\x40\x01\xfe\x01\xf4\x00\x0a\xff\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x52\xfe\xfd\x00\x00\x00\x00\x1b\xc4\xfa\xe5\xf3\x8e\x80\x00\x57\x10\xfe\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\x00\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\xff\x50\x6a\x40\x00\xff\x01\xf4\x00\x0a\xff\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x52\xff\xfd\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x12\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x20\x01\x01\x00\x00\x00\x00\x1b\xc4\xfa\xe5\xf3\x8e\x80\x00\xfd\x00\x1e\x0f\x10\xfe\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x56\x00\x57")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\x00\x77\x77\x77\x77\x77\x77\x77\x77\x77\x77\x77\x77\x77\x77\x77\x77\x77\x77\x77\x77\xff\x50\x3d\x40\xfe\x00\x0b\xb8\x00\x3c\xff\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x52\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x21\x01\x01\xfd\x00\x1e\x54\xfe\x10\xfe\xfc\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x55\x57")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\x00\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\x88\xff\x50\x4e\x40\xff\x00\x0b\xb8\x00\x3c\xff\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x52\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x21\x01\x01\xfd\x00\x1e\x13\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x56\xff\x57")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\x99\x99\x99\x99\x99\x99\x99\x99\x99\x99\x99\x99\x99\x99\x99\x99\x99\x99\x99\x99\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x20\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\xfd\x00\x1e\x69\x50\x37\x54\x01\x10\x01\xfc\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x55\x40\x01\xff\x01\xf4\x00\x0a\xff\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x52\xff\xfd\x00\x00\x00\x00\x1b\xc4\xfa\xe5\xf3\x8e\x80\x00\x57\x12\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x1b\xc4\xfa\xe5\xf3\x8e\x80\x00\x10\xfe\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\xaa\xaa\xaa\xaa\xaa\xaa\xaa\xaa\xaa\xaa\xaa\xaa\xaa\xaa\xaa\xaa\xaa\xaa\xaa\xaa\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x20\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\xfd\x00\x1e\x59\x13\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x50\x27\x40\xff\x01\x01\xf4\x00\x0a\xff\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x56\xff\x52\x01\xfd\x00\x00\x00\x00\x1b\xc4\xfa\xe5\xf3\x8e\x80\x00\x57\x10\x01\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\xbb\xbb\xbb\xbb\xbb\xbb\xbb\xbb\xbb\xbb\xbb\xbb\xbb\xbb\xbb\xbb\xbb\xbb\xbb\xbb\x00\xcc\xcc\xcc\xcc\xcc\xcc\xcc\xcc\xcc\xcc\xcc\xcc\xcc\xcc\xcc\xcc\xcc\xcc\xcc\xcc\xff\x30\x00\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\xfd\x20\x10\xfe\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x30\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\xfd\x00")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\xdd\xdd\xdd\xdd\xdd\xdd\xdd\xdd\xdd\xdd\xdd\xdd\xdd\xdd\xdd\xdd\xdd\xdd\xdd\xdd\x00\xee\xee\xee\xee\xee\xee\xee\xee\xee\xee\xee\xee\xee\xee\xee\xee\xee\xee\xee\xee\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x20\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\xfd\x00\x1e\x2f\x30\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\xfd\x0f\x10\x02\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x10\xfe\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\xf1\x11\x11\x11\x11\x11\x11\x11\x11\x11\x11\x11\x11\x11\x11\x11\x11\x11\x11\x11\x00\xf2\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x22\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x30\x00\x01\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\xfd\x31\x10\xfe\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00\x10\x02\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x20\x01\x01\x00\x00\x00\x00\x1b\xc4\xfa\xe5\xf3\x8e\x80\x00\xfd\x00\x1e\x00")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\xf3\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x33\x00\xf4\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x44\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x20\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\xfd\x00\x1e\x24\x10\x02\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x21\x01\x01\xfd\x00\x1e\x10\xfe\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00")));
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
        address!("DeAd0000000000000000000000000000000000Be"),
        address!("000000000004444c5dc75cB358380D2e3dE08A90"),
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        EncodeOptions::default(),
    );
    assert_eq!(rust, Some(hx(b"\x00\xf5\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x55\x00\xf6\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x66\x00\xf7\x77\x77\x77\x77\x77\x77\x77\x77\x77\x77\x77\x77\x77\x77\x77\x77\x77\x77\x77\x00\xa0\xb8\x69\x91\xc6\x21\x8b\x36\xc1\xd1\x9d\x4a\x2e\x9e\xb0\xce\x36\x06\xeb\x48\xff\x20\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\xfd\x00\x1e\x2a\x10\x03\x01\x00\x00\x00\x00\x00\x00\x00\x00\x77\x35\x94\x00\x21\x01\x01\x02\x00\x1e\x21\x02\x01\xfd\x00\x1e\x10\xfe\x00\x00\x00\x00\x00\x0d\xe0\xb6\xb3\xa7\x64\x00\x00")));
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
