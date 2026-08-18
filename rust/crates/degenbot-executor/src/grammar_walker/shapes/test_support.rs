//! Shared fixtures for the per-shape walk probes (T1 RKNRJO, epic 6SWFBS).
//!
//! Mirrors `tests/glopcn_bytepin.rs` (addresses, hop builders, amount sets,
//! option matrix, FNV-1a hash) so each shape file pins the same stream space
//! at per-file granularity. Test-only; compiled under `cfg(test)` via
//! `shapes.rs`.
#![expect(
    clippy::cast_possible_truncation,
    reason = "fixture hop indices are structurally 0..2"
)]
#![expect(clippy::doc_markdown, reason = "plan-step / ADR identifiers in docs")]
#![expect(
    clippy::unreadable_literal,
    reason = "amount literals intentionally mirror the glopcn fixtures"
)]

use crate::composers::{
    encode_cmd_stream, EncodeContext, EncodeOptions, EncodeRequest, HopInfo, PathInfo, V2HopInfo,
    V3HopInfo, V4HopInfo,
};
use alloy::primitives::{address, Address};

pub const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
pub const USDC: Address = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
pub const WBTC: Address = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
pub const PM: Address = address!("000000000004444c5dc75cB358380D2e3dE08A90");
pub const EXECUTOR: Address = address!("DeAd0000000000000000000000000000000000Be");

pub fn v2(in_t: Address, out_t: Address, idx: u8) -> HopInfo {
    HopInfo::V2(V2HopInfo {
        pool_address: Address::from([0xA0 + idx; 20]),
        token0_address: in_t,
        token1_address: out_t,
        fee: 30,
        zfo: true,
    })
}

pub fn v3(in_t: Address, out_t: Address, idx: u8) -> HopInfo {
    HopInfo::V3(V3HopInfo {
        pool_address: Address::from([0xB0 + idx; 20]),
        token0_address: in_t,
        token1_address: out_t,
        fee: 3000,
        zfo: true,
    })
}

pub fn v4(in_t: Address, out_t: Address, idx: u8) -> HopInfo {
    HopInfo::V4(V4HopInfo {
        pool_manager_address: PM,
        pool_id_hex: format!("0x{idx:02x}"),
        currency0_address: in_t,
        currency1_address: out_t,
        fee: 500,
        tick_spacing: 10,
        hook_address: Address::ZERO,
        zfo: true,
    })
}

/// FNV-1a 64-bit (deterministic across runs/processes — no RandomState).
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// The frozen amount sets (optimal_input, hop_outputs, consumed_inputs) —
/// same four as the glopcn fixture.
pub fn configs() -> Vec<(u128, Vec<u128>, Vec<u128>)> {
    vec![
        (
            1_000_000_000_000_000_000,
            vec![1_000_000_000_000_000_000; 3],
            vec![999_999_999_999_999_999; 3],
        ),
        (
            1_000_000_000_000_000_000,
            vec![1_000_000_000_000_000_000; 3],
            vec![1_000_000_000_000_000_000; 3],
        ),
        (
            2u128.pow(95),
            vec![2u128.pow(95); 3],
            vec![2u128.pow(95) - 1; 3],
        ),
        (
            2u128.pow(95),
            vec![2u128.pow(95); 3],
            vec![2u128.pow(95); 3],
        ),
    ]
}

/// The frozen option matrix (same labels as the glopcn fixture).
pub fn opts() -> Vec<(&'static str, EncodeOptions)> {
    vec![
        ("base", EncodeOptions::default()),
        (
            "erc6909",
            EncodeOptions {
                erc6909_profit: true,
                use_v4_batch: false,
                ..Default::default()
            },
        ),
        (
            "batch",
            EncodeOptions {
                erc6909_profit: false,
                use_v4_batch: true,
                ..Default::default()
            },
        ),
    ]
}

/// Build the token-cycle hops for a protocol combo (WETH→USDC→WBTC).
pub fn build_hops(combo: &[&str]) -> Vec<HopInfo> {
    let cycle = [WETH, USDC, WBTC];
    (0..combo.len())
        .map(|i| {
            let (in_t, out_t) = (cycle[i % 3], cycle[(i + 1) % 3]);
            match combo[i] {
                "V2" => v2(in_t, out_t, i as u8),
                "V3" => v3(in_t, out_t, i as u8),
                _ => v4(in_t, out_t, i as u8),
            }
        })
        .collect()
}

/// Encode through the public seam and pin the true outcome per ADR-030:
/// stream → `{:016x}` FNV-1a hash, a routine Decline → `None`, a fatal
/// validator Reject (panics) → `Reject`.
#[expect(clippy::too_many_arguments)]
pub fn entry_line(
    family: &str,
    path: PathInfo,
    optimal: u128,
    out: &[u128],
    consumed: &[u128],
    label: &str,
    ci: usize,
    opt: EncodeOptions,
) -> String {
    let ctx = EncodeContext::new(EXECUTOR, PM, WETH);
    let req = EncodeRequest::new(path, optimal, out.to_vec(), consumed.to_vec(), opt);
    let call = || encode_cmd_stream(&ctx, &req);
    match std::panic::catch_unwind(call) {
        Ok(Some(b)) => format!("{family}_{label}_{ci} {:016x}", fnv1a(&b)),
        Ok(None) => format!("{family}_{label}_{ci} None"),
        Err(_) => format!("{family}_{label}_{ci} Reject"),
    }
}
