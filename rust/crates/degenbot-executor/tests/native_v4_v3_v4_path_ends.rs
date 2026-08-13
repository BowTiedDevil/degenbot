//! Diagnostic: does `encode_cmd_3_hop` accept a native-ETH-at-path-ends
//! V4-V3-V4 path? The mainnet bot run showed these paths failing with
//! `encode-failed` (encoder returns `None`), but a foundry spike proved the
//! resulting byte encoding executes correctly on-chain. This test bisects why
//! the encoder refuses a valid path.
//!
//! Path modeled on mainnet path 384:
//! - Hop A (V4): NATIVE→USDC, zfo=true  (input=native, output=USDC)
//! - Hop B (V3): USDC→0xd8A2, zfo=true  (input=USDC, output=0xd8A2)
//! - Hop C (V4): NATIVE→0xd8A2, zfo=false (input=0xd8A2, output=NATIVE)
//!
//! `A input native` + `C output native` = native at path ends.

use alloy::primitives::{address, Address};
use degenbot_executor::composers::{
    encode_cmd_3_hop, EncodeOptions, HopInfo, PathInfo, V3HopInfo, V4HopInfo,
};

const NATIVE: Address = Address::ZERO;
const PM: Address = address!("000000000004444c5dc75cB358380D2e3dE08A90");
const EXECUTOR: Address = address!("DeAd0000000000000000000000000000000000Be");
const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");

#[test]
// **Root Cause B**: this path's solver amounts are incoherent — the V3
// mid hop's swap-in (435_867_037_568_084_649) is ~1.6e7× its repayment
// source (A's output, 27_604), so the V3 flash debt is unconesurably
// under-repaid. The old emitter produced bytes anyway (reverting on-chain);
// the LedgerValidator now correctly rejects it (FlashDebtUnpaid). The path
// encode-failed in production for exactly this reason; the gate is the fix.
fn native_v4_v3_v4_path_ends_declines() {
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
        HopInfo::V3(V3HopInfo {
            pool_address: address!("5555555555555555555555555555555555555555"),
            token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
            token1_address: address!("d8A271974E8EdAE9D7b58e3370dc1669427503F4"),
            fee: 500,
            zfo: true,
        }),
        HopInfo::V4(V4HopInfo {
            pool_manager_address: PM,
            pool_id_hex: "0x2222222222222222222222222222222222222222222222222222222222222222"
                .to_string(),
            currency0_address: NATIVE,
            currency1_address: address!("d8A271974E8EdAE9D7b58e3370dc1669427503F4"),
            fee: 10000,
            tick_spacing: 200,
            hook_address: NATIVE,
            zfo: false,
        }),
    ]);
    // Exact amounts from mainnet path 384 (the encode-failed log line).
    let out_3hop = encode_cmd_3_hop(
        &path,
        14_430_326_917_328u128,
        &[
            27_604u128,
            435_867_037_568_084_649u128,
            18_602_645_426_649u128,
        ],
        &[
            27_604u128,
            435_867_037_568_084_649u128,
            18_602_645_426_649u128,
        ],
        EXECUTOR,
        PM,
        WETH,
        EncodeOptions::default(),
    );
    assert!(
        out_3hop.is_none(),
        "encode_cmd_3_hop: V4-V3-V4 path-384 (incoherent repayment) must decline, got {out_3hop:?}"
    );

    // Also exercise the exact entry point the bot uses (encode_cmd_stream).
    let out_stream = degenbot_executor::composers::encode_cmd_stream(
        &path,
        14_430_326_917_328u128,
        &[
            27_604u128,
            435_867_037_568_084_649u128,
            18_602_645_426_649u128,
        ],
        &[
            27_604u128,
            435_867_037_568_084_649u128,
            18_602_645_426_649u128,
        ],
        EXECUTOR,
        PM,
        WETH,
        EncodeOptions::default(),
    );
    assert!(
        out_stream.is_none(),
        "encode_cmd_stream: V4-V3-V4 path-384 (incoherent repayment) must decline, got {out_stream:?}"
    );
}

/// The REAL `encode-failed` root cause: V4 pools with `fee > u16::MAX` (65535)
/// hit the `u16::try_from(ha.fee).ok()?` guard in `three_hop_v4_v3_v4` (and
/// every 3-hop composer). Mainnet high-fee V4 pools (e.g. fee=320000 = 32%,
/// fee=965700 = 96.57%) are read into the u32 `fee` field, then rejected by
/// the compact encoder's 2-byte fee width. This is UNRELATED to native-ETH.
#[test]
fn high_fee_v4_pool_causes_encode_failed() {
    // Same path as `native_v4_v3_v4_path_ends_encodes` but hop A's fee = 320000.
    let mut hops = vec![
        HopInfo::V4(V4HopInfo {
            pool_manager_address: PM,
            pool_id_hex: "0x1111111111111111111111111111111111111111111111111111111111111111"
                .to_string(),
            currency0_address: NATIVE,
            currency1_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
            fee: 320_000, // > u16::MAX (65535) — a real 32%-fee V4 pool
            tick_spacing: 6400,
            hook_address: NATIVE,
            zfo: true,
        }),
        HopInfo::V3(V3HopInfo {
            pool_address: address!("5555555555555555555555555555555555555555"),
            token0_address: address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"),
            token1_address: address!("d8A271974E8EdAE9D7b58e3370dc1669427503F4"),
            fee: 500,
            zfo: true,
        }),
        HopInfo::V4(V4HopInfo {
            pool_manager_address: PM,
            pool_id_hex: "0x2222222222222222222222222222222222222222222222222222222222222222"
                .to_string(),
            currency0_address: NATIVE,
            currency1_address: address!("d8A271974E8EdAE9D7b58e3370dc1669427503F4"),
            fee: 10_000,
            tick_spacing: 200,
            hook_address: NATIVE,
            zfo: false,
        }),
    ];
    let path = PathInfo::new(std::mem::take(&mut hops));
    let out = encode_cmd_3_hop(
        &path,
        14_430_326_917_328u128,
        &[
            27_604u128,
            435_867_037_568_084_649u128,
            18_602_645_426_649u128,
        ],
        &[
            27_604u128,
            435_867_037_568_084_649u128,
            18_602_645_426_649u128,
        ],
        EXECUTOR,
        PM,
        WETH,
        EncodeOptions::default(),
    );
    // Documents the current (intended-or-not) behaviour: high-fee V4 pools
    // cannot be encoded by the compact 2-byte-fee encoder, so the composer
    // returns None → the bot buckets this as `encode-failed`.
    assert!(
        out.is_none(),
        "V4 fee > u16::MAX should fail encode (compact encoder fee is 2 bytes), got Some"
    );
    let _ = hops; // suppress unused warning
}
