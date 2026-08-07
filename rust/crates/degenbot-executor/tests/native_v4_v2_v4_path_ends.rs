//! Diagnostic: does `encode_cmd_3_hop` accept a native-ETH-at-path-ends
//! V4-V2-V4 path? Mirrors the V4-V3-V4 diagnostic (ergo TPITPQ): a foundry
//! spike proved the canonical V4-V2-V4 encoding (and the degenbot Rust
//! reordering with `V2_SWAP_CALC`→executor + `settle_all`) executes correctly
//! on-chain for native ETH at path ends — no wrap/unwrap needed.
//!
//! Path modeled on the mainnet-native V4-V2-V4 shape:
//! - Hop A (V4): NATIVE→USDC, zfo=true  (input=native, output=USDC)
//! - Hop B (V2): USDC→WBTC, zfo=true    (input=USDC, output=WBTC)
//! - Hop C (V4): WBTC→NATIVE, zfo=true  (input=WBTC, output=native)

use alloy::primitives::{address, Address};
use degenbot_executor::composers::{
    encode_cmd_3_hop, EncodeOptions, HopInfo, PathInfo, V2HopInfo, V4HopInfo,
};

const NATIVE: Address = Address::ZERO;
const PM: Address = address!("000000000004444c5dc75cB358380D2e3dE08A90");
const EXECUTOR: Address = address!("DeAd0000000000000000000000000000000000Be");
const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");

#[test]
fn native_v4_v2_v4_path_ends_encodes() {
    let path = PathInfo::new(vec![
        HopInfo::V4(V4HopInfo {
            pool_manager_address: PM,
            pool_id_hex: "0x1111111111111111111111111111111111111111111111111111111111111111"
                .to_string(),
            currency0_address: NATIVE,
            currency1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
            fee: 3000,
            tick_spacing: 60,
            hook_address: NATIVE,
            zfo: true,
        }),
        HopInfo::V2(V2HopInfo {
            pool_address: address!("B4e8dBa08eFd07E50C8aCFb9681A6732e9C9F8a3"),
            token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
            token1_address: address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"),
            fee: 30,
            zfo: true,
        }),
        HopInfo::V4(V4HopInfo {
            pool_manager_address: PM,
            pool_id_hex: "0x2222222222222222222222222222222222222222222222222222222222222222"
                .to_string(),
            currency0_address: address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"),
            currency1_address: NATIVE,
            fee: 10000,
            tick_spacing: 200,
            hook_address: NATIVE,
            zfo: true,
        }),
    ]);
    let out_3hop = encode_cmd_3_hop(
        &path,
        1_000_000_000_000_000u128,
        &[2_000_000u128, 100_000_000u128, 2_001_000u128],
        &[2_000_000u128, 100_000_000u128, 2_001_000u128],
        EXECUTOR,
        PM,
        WETH,
        EncodeOptions::default(),
    );
    assert!(
        out_3hop.is_some(),
        "V4-V2-V4 native path-ends should encode (spike proved it executes on-chain), got None"
    );
    // Also exercise the exact entry point the bot uses.
    let out_stream = degenbot_executor::composers::encode_cmd_stream(
        &path,
        1_000_000_000_000_000u128,
        &[2_000_000u128, 100_000_000u128, 2_001_000u128],
        &[2_000_000u128, 100_000_000u128, 2_001_000u128],
        EXECUTOR,
        PM,
        WETH,
        EncodeOptions::default(),
    );
    assert!(
        out_stream.is_some(),
        "encode_cmd_stream: V4-V2-V4 native path-ends should encode, got None"
    );
}
