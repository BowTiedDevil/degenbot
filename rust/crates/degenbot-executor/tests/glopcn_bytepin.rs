//! GLOPCN byte-identity regression golden (the scaffold extraction must not
//! change any family's produced bytes or its guard↔decline partition).
//!
//! Captures a deterministic FNV-1a hash of the FULL produced byte stream
//! (preamble + plan) for every reachable 2-hop, 3-hop, and any-N all-V2
//! grammar family, under a sweep of amount sets + `EncodeOptions`. A
//! `None`-encoding (decline) is pinned too, so the guard ladder's decline
//! partition is likewise frozen. Regenerate (`--nocapture` → sort -u) only
//! when a change is *supposed* to alter bytes.
#![expect(
    clippy::cast_possible_truncation,
    clippy::print_stdout,
    clippy::unreadable_literal,
    clippy::doc_markdown
)]

use alloy::primitives::{address, Address};
use degenbot_executor::composers::{
    encode_cmd_3_hop, encode_cmd_stream, EncodeContext, EncodeOptions, EncodeRequest, HopInfo,
    PathInfo, V2HopInfo, V3HopInfo, V4HopInfo,
};

const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
const USDC: Address = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
const WBTC: Address = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
const PM: Address = address!("000000000004444c5dc75cB358380D2e3De08A90");
const EXECUTOR: Address = address!("DeAd0000000000000000000000000000000000Be");

fn v2(in_t: Address, out_t: Address, idx: u8) -> HopInfo {
    HopInfo::V2(V2HopInfo {
        pool_address: Address::from([0xA0 + idx; 20]),
        token0_address: in_t,
        token1_address: out_t,
        fee: 30,
        zfo: true,
    })
}
fn v3(in_t: Address, out_t: Address, idx: u8) -> HopInfo {
    HopInfo::V3(V3HopInfo {
        pool_address: Address::from([0xB0 + idx; 20]),
        token0_address: in_t,
        token1_address: out_t,
        fee: 3000,
        zfo: true,
    })
}
fn v4(in_t: Address, out_t: Address, idx: u8) -> HopInfo {
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
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn configs() -> Vec<(u128, Vec<u128>, Vec<u128>)> {
    // (optimal_input, hop_outputs, consumed_inputs)
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

fn opts() -> Vec<(&'static str, EncodeOptions)> {
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

fn build_hops(combo: &[&str]) -> Vec<HopInfo> {
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

/// The frozen byte-identity golden, captured at GLOPCN (the pure V2/V3
/// scaffold extraction). Sorted; every family/config must reproduce exactly.
const GOLDEN: &[&str] = &[
    "2hop_V2_V2_base_0 23c218ba470db61a",
    "2hop_V2_V2_base_1 23c218ba470db61a",
    "2hop_V2_V2_base_2 a5659e737e0d6a65",
    "2hop_V2_V2_base_3 a5659e737e0d6a65",
    "2hop_V2_V2_batch_0 23c218ba470db61a",
    "2hop_V2_V2_batch_1 23c218ba470db61a",
    "2hop_V2_V2_batch_2 a5659e737e0d6a65",
    "2hop_V2_V2_batch_3 a5659e737e0d6a65",
    "2hop_V2_V2_erc6909_0 23c218ba470db61a",
    "2hop_V2_V2_erc6909_1 23c218ba470db61a",
    "2hop_V2_V2_erc6909_2 a5659e737e0d6a65",
    "2hop_V2_V2_erc6909_3 a5659e737e0d6a65",
    "2hop_V2_V3_base_0 a6eed6115ae5b8a0",
    "2hop_V2_V3_base_1 9ccf9c10379eff2a",
    "2hop_V2_V3_base_2 27d899abc1b53026",
    "2hop_V2_V3_base_3 35cdbd80837cddd6",
    "2hop_V2_V3_batch_0 a6eed6115ae5b8a0",
    "2hop_V2_V3_batch_1 9ccf9c10379eff2a",
    "2hop_V2_V3_batch_2 27d899abc1b53026",
    "2hop_V2_V3_batch_3 35cdbd80837cddd6",
    "2hop_V2_V3_erc6909_0 a6eed6115ae5b8a0",
    "2hop_V2_V3_erc6909_1 9ccf9c10379eff2a",
    "2hop_V2_V3_erc6909_2 27d899abc1b53026",
    "2hop_V2_V3_erc6909_3 35cdbd80837cddd6",
    "2hop_V2_V4_base_0 923364d759eb212d",
    "2hop_V2_V4_base_1 3a1b12cf9f0ec888",
    "2hop_V2_V4_base_2 34faecb7315436c5",
    "2hop_V2_V4_base_3 3de42bc24146d541",
    "2hop_V2_V4_batch_0 923364d759eb212d",
    "2hop_V2_V4_batch_1 3a1b12cf9f0ec888",
    "2hop_V2_V4_batch_2 34faecb7315436c5",
    "2hop_V2_V4_batch_3 3de42bc24146d541",
    "2hop_V2_V4_erc6909_0 923364d759eb212d",
    "2hop_V2_V4_erc6909_1 3a1b12cf9f0ec888",
    "2hop_V2_V4_erc6909_2 34faecb7315436c5",
    "2hop_V2_V4_erc6909_3 3de42bc24146d541",
    "2hop_V3_V2_base_0 e98d8ad1caa33092",
    "2hop_V3_V2_base_1 e98d8ad1caa33092",
    "2hop_V3_V2_base_2 a7c27a500afbfbd1",
    "2hop_V3_V2_base_3 a7c27a500afbfbd1",
    "2hop_V3_V2_batch_0 e98d8ad1caa33092",
    "2hop_V3_V2_batch_1 e98d8ad1caa33092",
    "2hop_V3_V2_batch_2 a7c27a500afbfbd1",
    "2hop_V3_V2_batch_3 a7c27a500afbfbd1",
    "2hop_V3_V2_erc6909_0 e98d8ad1caa33092",
    "2hop_V3_V2_erc6909_1 e98d8ad1caa33092",
    "2hop_V3_V2_erc6909_2 a7c27a500afbfbd1",
    "2hop_V3_V2_erc6909_3 a7c27a500afbfbd1",
    "2hop_V3_V3_base_0 6d259f9b9b0934f9",
    "2hop_V3_V3_base_1 406bc25af55b505a",
    "2hop_V3_V3_base_2 1e6514e37620d02d",
    "2hop_V3_V3_base_3 f82c4896e4c30ea1",
    "2hop_V3_V3_batch_0 6d259f9b9b0934f9",
    "2hop_V3_V3_batch_1 406bc25af55b505a",
    "2hop_V3_V3_batch_2 1e6514e37620d02d",
    "2hop_V3_V3_batch_3 f82c4896e4c30ea1",
    "2hop_V3_V3_erc6909_0 6d259f9b9b0934f9",
    "2hop_V3_V3_erc6909_1 406bc25af55b505a",
    "2hop_V3_V3_erc6909_2 1e6514e37620d02d",
    "2hop_V3_V3_erc6909_3 f82c4896e4c30ea1",
    "2hop_V3_V4_base_0 91cede1cd9a55e3f",
    "2hop_V3_V4_base_1 e5f550676312ba0e",
    "2hop_V3_V4_base_2 14e64c847a39e6db",
    "2hop_V3_V4_base_3 8522cd96d6641187",
    "2hop_V3_V4_batch_0 91cede1cd9a55e3f",
    "2hop_V3_V4_batch_1 e5f550676312ba0e",
    "2hop_V3_V4_batch_2 14e64c847a39e6db",
    "2hop_V3_V4_batch_3 8522cd96d6641187",
    "2hop_V3_V4_erc6909_0 91cede1cd9a55e3f",
    "2hop_V3_V4_erc6909_1 e5f550676312ba0e",
    "2hop_V3_V4_erc6909_2 14e64c847a39e6db",
    "2hop_V3_V4_erc6909_3 8522cd96d6641187",
    "2hop_V4_V2_base_0 3adae8a43d6f79ce",
    "2hop_V4_V2_base_1 3adae8a43d6f79ce",
    "2hop_V4_V2_base_2 ff9113c64ec696a9",
    "2hop_V4_V2_base_3 ff9113c64ec696a9",
    "2hop_V4_V2_batch_0 3adae8a43d6f79ce",
    "2hop_V4_V2_batch_1 3adae8a43d6f79ce",
    "2hop_V4_V2_batch_2 ff9113c64ec696a9",
    "2hop_V4_V2_batch_3 ff9113c64ec696a9",
    "2hop_V4_V2_erc6909_0 3adae8a43d6f79ce",
    "2hop_V4_V2_erc6909_1 3adae8a43d6f79ce",
    "2hop_V4_V2_erc6909_2 ff9113c64ec696a9",
    "2hop_V4_V2_erc6909_3 ff9113c64ec696a9",
    "2hop_V4_V3_base_0 518e53d8d63b481e",
    "2hop_V4_V3_base_1 e1abca545ee1b7d7",
    "2hop_V4_V3_base_2 684ebfe7c96d6ab6",
    "2hop_V4_V3_base_3 2a3e56a813e6561a",
    "2hop_V4_V3_batch_0 518e53d8d63b481e",
    "2hop_V4_V3_batch_1 e1abca545ee1b7d7",
    "2hop_V4_V3_batch_2 684ebfe7c96d6ab6",
    "2hop_V4_V3_batch_3 2a3e56a813e6561a",
    "2hop_V4_V3_erc6909_0 518e53d8d63b481e",
    "2hop_V4_V3_erc6909_1 e1abca545ee1b7d7",
    "2hop_V4_V3_erc6909_2 684ebfe7c96d6ab6",
    "2hop_V4_V3_erc6909_3 2a3e56a813e6561a",
    "2hop_V4_V4_base_0 51aa71ec610de3b5",
    "2hop_V4_V4_base_1 5a137aa3db157ffa",
    "2hop_V4_V4_base_2 ac79cd7b8e92e5cc",
    "2hop_V4_V4_base_3 124e035d9389cec8",
    "2hop_V4_V4_batch_0 677fc97e39900f55",
    "2hop_V4_V4_batch_1 70705488b1a36c9a",
    "2hop_V4_V4_batch_2 f07dd803d7b0ca7a",
    "2hop_V4_V4_batch_3 6a28c372bc84cf16",
    "2hop_V4_V4_erc6909_0 51aa71ec610de3b5",
    "2hop_V4_V4_erc6909_1 5a137aa3db157ffa",
    "2hop_V4_V4_erc6909_2 ac79cd7b8e92e5cc",
    "2hop_V4_V4_erc6909_3 124e035d9389cec8",
    "3hop_V2_V2_V2_base_0 9e920e9ba07e743f",
    "3hop_V2_V2_V2_base_1 9e920e9ba07e743f",
    "3hop_V2_V2_V2_base_2 99906d3c235cbd4c",
    "3hop_V2_V2_V2_base_3 99906d3c235cbd4c",
    "3hop_V2_V2_V2_batch_0 9e920e9ba07e743f",
    "3hop_V2_V2_V2_batch_1 9e920e9ba07e743f",
    "3hop_V2_V2_V2_batch_2 99906d3c235cbd4c",
    "3hop_V2_V2_V2_batch_3 99906d3c235cbd4c",
    "3hop_V2_V2_V2_erc6909_0 9e920e9ba07e743f",
    "3hop_V2_V2_V2_erc6909_1 9e920e9ba07e743f",
    "3hop_V2_V2_V2_erc6909_2 99906d3c235cbd4c",
    "3hop_V2_V2_V2_erc6909_3 99906d3c235cbd4c",
    "3hop_V2_V2_V3_base_0 0eb26689395a7af0",
    "3hop_V2_V2_V3_base_1 870e4cfff4fac9f9",
    "3hop_V2_V2_V3_base_2 6cbfe2bc358a13ab",
    "3hop_V2_V2_V3_base_3 ca1e51b5e922f6df",
    "3hop_V2_V2_V3_batch_0 0eb26689395a7af0",
    "3hop_V2_V2_V3_batch_1 870e4cfff4fac9f9",
    "3hop_V2_V2_V3_batch_2 6cbfe2bc358a13ab",
    "3hop_V2_V2_V3_batch_3 ca1e51b5e922f6df",
    "3hop_V2_V2_V3_erc6909_0 0eb26689395a7af0",
    "3hop_V2_V2_V3_erc6909_1 870e4cfff4fac9f9",
    "3hop_V2_V2_V3_erc6909_2 6cbfe2bc358a13ab",
    "3hop_V2_V2_V3_erc6909_3 ca1e51b5e922f6df",
    "3hop_V2_V2_V4_base_0 a66fbd67eb49e613",
    "3hop_V2_V2_V4_base_1 6422509d6288d5ce",
    "3hop_V2_V2_V4_base_2 40754d3312445c52",
    "3hop_V2_V2_V4_base_3 b35e48ecc866ef7e",
    "3hop_V2_V2_V4_batch_0 a66fbd67eb49e613",
    "3hop_V2_V2_V4_batch_1 6422509d6288d5ce",
    "3hop_V2_V2_V4_batch_2 40754d3312445c52",
    "3hop_V2_V2_V4_batch_3 b35e48ecc866ef7e",
    "3hop_V2_V2_V4_erc6909_0 a66fbd67eb49e613",
    "3hop_V2_V2_V4_erc6909_1 6422509d6288d5ce",
    "3hop_V2_V2_V4_erc6909_2 40754d3312445c52",
    "3hop_V2_V2_V4_erc6909_3 b35e48ecc866ef7e",
    "3hop_V2_V3_V2_base_0 ff5ffd6aa3431dce",
    "3hop_V2_V3_V2_base_1 620a674882aa878b",
    "3hop_V2_V3_V2_base_2 88137b5a7684ac93",
    "3hop_V2_V3_V2_base_3 4c352c50817c6777",
    "3hop_V2_V3_V2_batch_0 ff5ffd6aa3431dce",
    "3hop_V2_V3_V2_batch_1 620a674882aa878b",
    "3hop_V2_V3_V2_batch_2 88137b5a7684ac93",
    "3hop_V2_V3_V2_batch_3 4c352c50817c6777",
    "3hop_V2_V3_V2_erc6909_0 ff5ffd6aa3431dce",
    "3hop_V2_V3_V2_erc6909_1 620a674882aa878b",
    "3hop_V2_V3_V2_erc6909_2 88137b5a7684ac93",
    "3hop_V2_V3_V2_erc6909_3 4c352c50817c6777",
    "3hop_V2_V3_V3_base_0 1515ed44cfe33a53",
    "3hop_V2_V3_V3_base_1 c16efcada3ea5fb5",
    "3hop_V2_V3_V3_base_2 e2be4a977a4a043d",
    "3hop_V2_V3_V3_base_3 efcbb712b2f1d4d5",
    "3hop_V2_V3_V3_batch_0 1515ed44cfe33a53",
    "3hop_V2_V3_V3_batch_1 c16efcada3ea5fb5",
    "3hop_V2_V3_V3_batch_2 e2be4a977a4a043d",
    "3hop_V2_V3_V3_batch_3 efcbb712b2f1d4d5",
    "3hop_V2_V3_V3_erc6909_0 1515ed44cfe33a53",
    "3hop_V2_V3_V3_erc6909_1 c16efcada3ea5fb5",
    "3hop_V2_V3_V3_erc6909_2 e2be4a977a4a043d",
    "3hop_V2_V3_V3_erc6909_3 efcbb712b2f1d4d5",
    "3hop_V2_V3_V4_base_0 0e6fa958f2d87d9d",
    "3hop_V2_V3_V4_base_1 90963e0f21a73f75",
    "3hop_V2_V3_V4_base_2 3d03e955420c1767",
    "3hop_V2_V3_V4_base_3 eec7f1a251c8c65f",
    "3hop_V2_V3_V4_batch_0 0e6fa958f2d87d9d",
    "3hop_V2_V3_V4_batch_1 90963e0f21a73f75",
    "3hop_V2_V3_V4_batch_2 3d03e955420c1767",
    "3hop_V2_V3_V4_batch_3 eec7f1a251c8c65f",
    "3hop_V2_V3_V4_erc6909_0 0e6fa958f2d87d9d",
    "3hop_V2_V3_V4_erc6909_1 90963e0f21a73f75",
    "3hop_V2_V3_V4_erc6909_2 3d03e955420c1767",
    "3hop_V2_V3_V4_erc6909_3 eec7f1a251c8c65f",
    "3hop_V2_V4_V2_base_0 987aee917fe12699",
    "3hop_V2_V4_V2_base_1 c1dc5c884b9890e4",
    "3hop_V2_V4_V2_base_2 01a0d4d6d9a5047c",
    "3hop_V2_V4_V2_base_3 3d05e541d9255058",
    "3hop_V2_V4_V2_batch_0 987aee917fe12699",
    "3hop_V2_V4_V2_batch_1 c1dc5c884b9890e4",
    "3hop_V2_V4_V2_batch_2 01a0d4d6d9a5047c",
    "3hop_V2_V4_V2_batch_3 3d05e541d9255058",
    "3hop_V2_V4_V2_erc6909_0 987aee917fe12699",
    "3hop_V2_V4_V2_erc6909_1 c1dc5c884b9890e4",
    "3hop_V2_V4_V2_erc6909_2 01a0d4d6d9a5047c",
    "3hop_V2_V4_V2_erc6909_3 3d05e541d9255058",
    "3hop_V2_V4_V3_base_0 cbaaa162a62e55df",
    "3hop_V2_V4_V3_base_1 39de3d74fa956186",
    "3hop_V2_V4_V3_base_2 526bbdf7ae04c752",
    "3hop_V2_V4_V3_base_3 404a5c3abaa4c26e",
    "3hop_V2_V4_V3_batch_0 cbaaa162a62e55df",
    "3hop_V2_V4_V3_batch_1 39de3d74fa956186",
    "3hop_V2_V4_V3_batch_2 526bbdf7ae04c752",
    "3hop_V2_V4_V3_batch_3 404a5c3abaa4c26e",
    "3hop_V2_V4_V3_erc6909_0 cbaaa162a62e55df",
    "3hop_V2_V4_V3_erc6909_1 39de3d74fa956186",
    "3hop_V2_V4_V3_erc6909_2 526bbdf7ae04c752",
    "3hop_V2_V4_V3_erc6909_3 404a5c3abaa4c26e",
    "3hop_V2_V4_V4_base_0 bd5fc2b207f6bfc5",
    "3hop_V2_V4_V4_base_1 a536fe14c576f697",
    "3hop_V2_V4_V4_base_2 d10d9804d29f0820",
    "3hop_V2_V4_V4_base_3 ac54e2740bdacd18",
    "3hop_V2_V4_V4_batch_0 bd5fc2b207f6bfc5",
    "3hop_V2_V4_V4_batch_1 a536fe14c576f697",
    "3hop_V2_V4_V4_batch_2 d10d9804d29f0820",
    "3hop_V2_V4_V4_batch_3 ac54e2740bdacd18",
    "3hop_V2_V4_V4_erc6909_0 bd5fc2b207f6bfc5",
    "3hop_V2_V4_V4_erc6909_1 a536fe14c576f697",
    "3hop_V2_V4_V4_erc6909_2 d10d9804d29f0820",
    "3hop_V2_V4_V4_erc6909_3 ac54e2740bdacd18",
    "3hop_V3_V2_V2_base_0 dfb9540dcc094cd7",
    "3hop_V3_V2_V2_base_1 dfb9540dcc094cd7",
    "3hop_V3_V2_V2_base_2 a90ced9fb9c8f1b5",
    "3hop_V3_V2_V2_base_3 a90ced9fb9c8f1b5",
    "3hop_V3_V2_V2_batch_0 dfb9540dcc094cd7",
    "3hop_V3_V2_V2_batch_1 dfb9540dcc094cd7",
    "3hop_V3_V2_V2_batch_2 a90ced9fb9c8f1b5",
    "3hop_V3_V2_V2_batch_3 a90ced9fb9c8f1b5",
    "3hop_V3_V2_V2_erc6909_0 dfb9540dcc094cd7",
    "3hop_V3_V2_V2_erc6909_1 dfb9540dcc094cd7",
    "3hop_V3_V2_V2_erc6909_2 a90ced9fb9c8f1b5",
    "3hop_V3_V2_V2_erc6909_3 a90ced9fb9c8f1b5",
    "3hop_V3_V2_V3_base_0 dbf24a33c2f6faf0",
    "3hop_V3_V2_V3_base_1 591ead5e459afa0b",
    "3hop_V3_V2_V3_base_2 12a18434ee5d4760",
    "3hop_V3_V2_V3_base_3 4b1b435144d77e64",
    "3hop_V3_V2_V3_batch_0 dbf24a33c2f6faf0",
    "3hop_V3_V2_V3_batch_1 591ead5e459afa0b",
    "3hop_V3_V2_V3_batch_2 12a18434ee5d4760",
    "3hop_V3_V2_V3_batch_3 4b1b435144d77e64",
    "3hop_V3_V2_V3_erc6909_0 dbf24a33c2f6faf0",
    "3hop_V3_V2_V3_erc6909_1 591ead5e459afa0b",
    "3hop_V3_V2_V3_erc6909_2 12a18434ee5d4760",
    "3hop_V3_V2_V3_erc6909_3 4b1b435144d77e64",
    "3hop_V3_V2_V4_base_0 558f393f46abda30",
    "3hop_V3_V2_V4_base_1 550e153664cb053f",
    "3hop_V3_V2_V4_base_2 840ce2d3bc6eff82",
    "3hop_V3_V2_V4_base_3 3e9c05aee17e731e",
    "3hop_V3_V2_V4_batch_0 558f393f46abda30",
    "3hop_V3_V2_V4_batch_1 550e153664cb053f",
    "3hop_V3_V2_V4_batch_2 840ce2d3bc6eff82",
    "3hop_V3_V2_V4_batch_3 3e9c05aee17e731e",
    "3hop_V3_V2_V4_erc6909_0 558f393f46abda30",
    "3hop_V3_V2_V4_erc6909_1 550e153664cb053f",
    "3hop_V3_V2_V4_erc6909_2 840ce2d3bc6eff82",
    "3hop_V3_V2_V4_erc6909_3 3e9c05aee17e731e",
    "3hop_V3_V3_V2_base_0 9ada8664e05f23a4",
    "3hop_V3_V3_V2_base_1 767f24d47a2bfc63",
    "3hop_V3_V3_V2_base_2 9fea48b3f2276bf0",
    "3hop_V3_V3_V2_base_3 f6c5487f60eb8f94",
    "3hop_V3_V3_V2_batch_0 9ada8664e05f23a4",
    "3hop_V3_V3_V2_batch_1 767f24d47a2bfc63",
    "3hop_V3_V3_V2_batch_2 9fea48b3f2276bf0",
    "3hop_V3_V3_V2_batch_3 f6c5487f60eb8f94",
    "3hop_V3_V3_V2_erc6909_0 9ada8664e05f23a4",
    "3hop_V3_V3_V2_erc6909_1 767f24d47a2bfc63",
    "3hop_V3_V3_V2_erc6909_2 9fea48b3f2276bf0",
    "3hop_V3_V3_V2_erc6909_3 f6c5487f60eb8f94",
    "3hop_V3_V3_V3_base_0 dbf24c3cb557ea60",
    "3hop_V3_V3_V3_base_1 f502c12436253efe",
    "3hop_V3_V3_V3_base_2 7e2c09bae86fe779",
    "3hop_V3_V3_V3_base_3 6a30b407e4472e31",
    "3hop_V3_V3_V3_batch_0 dbf24c3cb557ea60",
    "3hop_V3_V3_V3_batch_1 f502c12436253efe",
    "3hop_V3_V3_V3_batch_2 7e2c09bae86fe779",
    "3hop_V3_V3_V3_batch_3 6a30b407e4472e31",
    "3hop_V3_V3_V3_erc6909_0 dbf24c3cb557ea60",
    "3hop_V3_V3_V3_erc6909_1 f502c12436253efe",
    "3hop_V3_V3_V3_erc6909_2 7e2c09bae86fe779",
    "3hop_V3_V3_V3_erc6909_3 6a30b407e4472e31",
    "3hop_V3_V3_V4_base_0 bae21437545f465d",
    "3hop_V3_V3_V4_base_1 3e54d089114132c3",
    "3hop_V3_V3_V4_base_2 9e181baf1e9d99fb",
    "3hop_V3_V3_V4_base_3 d4e392af95fb5f5b",
    "3hop_V3_V3_V4_batch_0 bae21437545f465d",
    "3hop_V3_V3_V4_batch_1 3e54d089114132c3",
    "3hop_V3_V3_V4_batch_2 9e181baf1e9d99fb",
    "3hop_V3_V3_V4_batch_3 d4e392af95fb5f5b",
    "3hop_V3_V3_V4_erc6909_0 bae21437545f465d",
    "3hop_V3_V3_V4_erc6909_1 3e54d089114132c3",
    "3hop_V3_V3_V4_erc6909_2 9e181baf1e9d99fb",
    "3hop_V3_V3_V4_erc6909_3 d4e392af95fb5f5b",
    "3hop_V3_V4_V2_base_0 a34f9a93dc331dbc",
    "3hop_V3_V4_V2_base_1 950860eb2de00fb5",
    "3hop_V3_V4_V2_base_2 264ad27142fe94b4",
    "3hop_V3_V4_V2_base_3 8917e1f716531cf8",
    "3hop_V3_V4_V2_batch_0 a34f9a93dc331dbc",
    "3hop_V3_V4_V2_batch_1 950860eb2de00fb5",
    "3hop_V3_V4_V2_batch_2 264ad27142fe94b4",
    "3hop_V3_V4_V2_batch_3 8917e1f716531cf8",
    "3hop_V3_V4_V2_erc6909_0 a34f9a93dc331dbc",
    "3hop_V3_V4_V2_erc6909_1 950860eb2de00fb5",
    "3hop_V3_V4_V2_erc6909_2 264ad27142fe94b4",
    "3hop_V3_V4_V2_erc6909_3 8917e1f716531cf8",
    "3hop_V3_V4_V3_base_0 9914525553c328fa",
    "3hop_V3_V4_V3_base_1 56ddc3cbf292ddf3",
    "3hop_V3_V4_V3_base_2 5059524d9487e552",
    "3hop_V3_V4_V3_base_3 267075a971e60fe6",
    "3hop_V3_V4_V3_batch_0 9914525553c328fa",
    "3hop_V3_V4_V3_batch_1 56ddc3cbf292ddf3",
    "3hop_V3_V4_V3_batch_2 5059524d9487e552",
    "3hop_V3_V4_V3_batch_3 267075a971e60fe6",
    "3hop_V3_V4_V3_erc6909_0 9914525553c328fa",
    "3hop_V3_V4_V3_erc6909_1 56ddc3cbf292ddf3",
    "3hop_V3_V4_V3_erc6909_2 5059524d9487e552",
    "3hop_V3_V4_V3_erc6909_3 267075a971e60fe6",
    "3hop_V3_V4_V4_base_0 b771a5d6475d9331",
    "3hop_V3_V4_V4_base_1 704879a2dd56e70b",
    "3hop_V3_V4_V4_base_2 d3e658091d2cc5b3",
    "3hop_V3_V4_V4_base_3 faf5ea4ebd760e5b",
    "3hop_V3_V4_V4_batch_0 b771a5d6475d9331",
    "3hop_V3_V4_V4_batch_1 704879a2dd56e70b",
    "3hop_V3_V4_V4_batch_2 d3e658091d2cc5b3",
    "3hop_V3_V4_V4_batch_3 faf5ea4ebd760e5b",
    "3hop_V3_V4_V4_erc6909_0 b771a5d6475d9331",
    "3hop_V3_V4_V4_erc6909_1 704879a2dd56e70b",
    "3hop_V3_V4_V4_erc6909_2 d3e658091d2cc5b3",
    "3hop_V3_V4_V4_erc6909_3 faf5ea4ebd760e5b",
    "3hop_V4_V2_V2_base_0 6a24d10739f1b360",
    "3hop_V4_V2_V2_base_1 6a24d10739f1b360",
    "3hop_V4_V2_V2_base_2 8bcc4287ac1f338e",
    "3hop_V4_V2_V2_base_3 8bcc4287ac1f338e",
    "3hop_V4_V2_V2_batch_0 6a24d10739f1b360",
    "3hop_V4_V2_V2_batch_1 6a24d10739f1b360",
    "3hop_V4_V2_V2_batch_2 8bcc4287ac1f338e",
    "3hop_V4_V2_V2_batch_3 8bcc4287ac1f338e",
    "3hop_V4_V2_V2_erc6909_0 6a24d10739f1b360",
    "3hop_V4_V2_V2_erc6909_1 6a24d10739f1b360",
    "3hop_V4_V2_V2_erc6909_2 8bcc4287ac1f338e",
    "3hop_V4_V2_V2_erc6909_3 8bcc4287ac1f338e",
    "3hop_V4_V2_V3_base_0 a7e193f86a7dffa6",
    "3hop_V4_V2_V3_base_1 092b572ed5ba10b9",
    "3hop_V4_V2_V3_base_2 b2a930a88e265b0e",
    "3hop_V4_V2_V3_base_3 5a2dc595cede2082",
    "3hop_V4_V2_V3_batch_0 a7e193f86a7dffa6",
    "3hop_V4_V2_V3_batch_1 092b572ed5ba10b9",
    "3hop_V4_V2_V3_batch_2 b2a930a88e265b0e",
    "3hop_V4_V2_V3_batch_3 5a2dc595cede2082",
    "3hop_V4_V2_V3_erc6909_0 a7e193f86a7dffa6",
    "3hop_V4_V2_V3_erc6909_1 092b572ed5ba10b9",
    "3hop_V4_V2_V3_erc6909_2 b2a930a88e265b0e",
    "3hop_V4_V2_V3_erc6909_3 5a2dc595cede2082",
    "3hop_V4_V2_V4_base_0 ce81d252c14c04fc",
    "3hop_V4_V2_V4_base_1 268a7b13b5935735",
    "3hop_V4_V2_V4_base_2 3c00752a075e55e4",
    "3hop_V4_V2_V4_base_3 b51e97aec097d870",
    "3hop_V4_V2_V4_batch_0 ce81d252c14c04fc",
    "3hop_V4_V2_V4_batch_1 268a7b13b5935735",
    "3hop_V4_V2_V4_batch_2 3c00752a075e55e4",
    "3hop_V4_V2_V4_batch_3 b51e97aec097d870",
    "3hop_V4_V2_V4_erc6909_0 ce81d252c14c04fc",
    "3hop_V4_V2_V4_erc6909_1 268a7b13b5935735",
    "3hop_V4_V2_V4_erc6909_2 3c00752a075e55e4",
    "3hop_V4_V2_V4_erc6909_3 b51e97aec097d870",
    "3hop_V4_V3_V2_base_0 54b00fe4ed51a10e",
    "3hop_V4_V3_V2_base_1 ce483d826d7b5027",
    "3hop_V4_V3_V2_base_2 2fefeab2c08d02ec",
    "3hop_V4_V3_V2_base_3 0630022b09ec1ae8",
    "3hop_V4_V3_V2_batch_0 54b00fe4ed51a10e",
    "3hop_V4_V3_V2_batch_1 ce483d826d7b5027",
    "3hop_V4_V3_V2_batch_2 2fefeab2c08d02ec",
    "3hop_V4_V3_V2_batch_3 0630022b09ec1ae8",
    "3hop_V4_V3_V2_erc6909_0 54b00fe4ed51a10e",
    "3hop_V4_V3_V2_erc6909_1 ce483d826d7b5027",
    "3hop_V4_V3_V2_erc6909_2 2fefeab2c08d02ec",
    "3hop_V4_V3_V2_erc6909_3 0630022b09ec1ae8",
    "3hop_V4_V3_V3_base_0 af34e0fbb7cbd117",
    "3hop_V4_V3_V3_base_1 2723b9f2f0294f41",
    "3hop_V4_V3_V3_base_2 1681c467cbdb6815",
    "3hop_V4_V3_V3_base_3 df1a52da4e4e36ad",
    "3hop_V4_V3_V3_batch_0 af34e0fbb7cbd117",
    "3hop_V4_V3_V3_batch_1 2723b9f2f0294f41",
    "3hop_V4_V3_V3_batch_2 1681c467cbdb6815",
    "3hop_V4_V3_V3_batch_3 df1a52da4e4e36ad",
    "3hop_V4_V3_V3_erc6909_0 af34e0fbb7cbd117",
    "3hop_V4_V3_V3_erc6909_1 2723b9f2f0294f41",
    "3hop_V4_V3_V3_erc6909_2 1681c467cbdb6815",
    "3hop_V4_V3_V3_erc6909_3 df1a52da4e4e36ad",
    "3hop_V4_V3_V4_base_0 e4f33a198df80c01",
    "3hop_V4_V3_V4_base_1 580defa7bb7e7673",
    "3hop_V4_V3_V4_base_2 9a8dc21182469231",
    "3hop_V4_V3_V4_base_3 d69017899412f891",
    "3hop_V4_V3_V4_batch_0 e4f33a198df80c01",
    "3hop_V4_V3_V4_batch_1 580defa7bb7e7673",
    "3hop_V4_V3_V4_batch_2 9a8dc21182469231",
    "3hop_V4_V3_V4_batch_3 d69017899412f891",
    "3hop_V4_V3_V4_erc6909_0 e4f33a198df80c01",
    "3hop_V4_V3_V4_erc6909_1 580defa7bb7e7673",
    "3hop_V4_V3_V4_erc6909_2 9a8dc21182469231",
    "3hop_V4_V3_V4_erc6909_3 d69017899412f891",
    "3hop_V4_V4_V2_base_0 b4f571ceead1a80c",
    "3hop_V4_V4_V2_base_1 9285adb152f84e9a",
    "3hop_V4_V4_V2_base_2 93db3b5ffb34aa7b",
    "3hop_V4_V4_V2_base_3 098ad3811ef4710b",
    "3hop_V4_V4_V2_batch_0 b4f571ceead1a80c",
    "3hop_V4_V4_V2_batch_1 9285adb152f84e9a",
    "3hop_V4_V4_V2_batch_2 93db3b5ffb34aa7b",
    "3hop_V4_V4_V2_batch_3 098ad3811ef4710b",
    "3hop_V4_V4_V2_erc6909_0 b4f571ceead1a80c",
    "3hop_V4_V4_V2_erc6909_1 9285adb152f84e9a",
    "3hop_V4_V4_V2_erc6909_2 93db3b5ffb34aa7b",
    "3hop_V4_V4_V2_erc6909_3 098ad3811ef4710b",
    "3hop_V4_V4_V3_base_0 0162699f1bf77df2",
    "3hop_V4_V4_V3_base_1 564461becfd80022",
    "3hop_V4_V4_V3_base_2 cb468d7d95e0d51e",
    "3hop_V4_V4_V3_base_3 e8a4106fdd93e85e",
    "3hop_V4_V4_V3_batch_0 0162699f1bf77df2",
    "3hop_V4_V4_V3_batch_1 564461becfd80022",
    "3hop_V4_V4_V3_batch_2 cb468d7d95e0d51e",
    "3hop_V4_V4_V3_batch_3 e8a4106fdd93e85e",
    "3hop_V4_V4_V3_erc6909_0 0162699f1bf77df2",
    "3hop_V4_V4_V3_erc6909_1 564461becfd80022",
    "3hop_V4_V4_V3_erc6909_2 cb468d7d95e0d51e",
    "3hop_V4_V4_V3_erc6909_3 e8a4106fdd93e85e",
    "3hop_V4_V4_V4_base_0 39f195f6031f6dee",
    "3hop_V4_V4_V4_base_1 Reject",
    "3hop_V4_V4_V4_base_2 b94214b5afd781e8",
    "3hop_V4_V4_V4_base_3 Reject",
    "3hop_V4_V4_V4_batch_0 d5a0d382b8d2b575",
    "3hop_V4_V4_V4_batch_1 d76255dcc98bb884",
    "3hop_V4_V4_V4_batch_2 f8509c1e93981e5f",
    "3hop_V4_V4_V4_batch_3 644b660ad570a3a3",
    "3hop_V4_V4_V4_erc6909_0 d2f1daef2f632f31",
    "3hop_V4_V4_V4_erc6909_1 b6ef0108100546a6",
    "3hop_V4_V4_V4_erc6909_2 06d233f7b5edcf87",
    "3hop_V4_V4_V4_erc6909_3 5631c23d52d7b783",
    "4hop_allV2_base_0 c427ec50df8e08ea",
    "4hop_allV2_base_1 c427ec50df8e08ea",
    "4hop_allV2_base_2 3127a57c14ec0431",
    "4hop_allV2_base_3 3127a57c14ec0431",
    "4hop_allV2_batch_0 c427ec50df8e08ea",
    "4hop_allV2_batch_1 c427ec50df8e08ea",
    "4hop_allV2_batch_2 3127a57c14ec0431",
    "4hop_allV2_batch_3 3127a57c14ec0431",
    "4hop_allV2_erc6909_0 c427ec50df8e08ea",
    "4hop_allV2_erc6909_1 c427ec50df8e08ea",
    "4hop_allV2_erc6909_2 3127a57c14ec0431",
    "4hop_allV2_erc6909_3 3127a57c14ec0431",
];

#[test]
fn glopcn_byte_identity_is_pinned() {
    let fams = ["V2", "V3", "V4"];
    let mut lines = Vec::new();
    for n in [2usize, 3] {
        let entry = |path: &PathInfo,
                     optimal: u128,
                     out: &[u128],
                     consumed: &[u128],
                     opts: EncodeOptions|
         -> String {
            // ADR-030: a validator Reject is fatal (panics), distinct from a
            // routine decline. Catch it so the pin can record the true
            // decline/reject partition instead of conflating them under `None`.
            let ctx = EncodeContext::new(EXECUTOR, PM, WETH);
            let call = || {
                if n == 2 {
                    let req = EncodeRequest::new(
                        path.clone(),
                        optimal,
                        out.to_vec(),
                        consumed.to_vec(),
                        opts,
                    );
                    encode_cmd_stream(&ctx, &req)
                } else {
                    encode_cmd_3_hop(path, optimal, out, consumed, EXECUTOR, PM, WETH, opts)
                }
            };
            match std::panic::catch_unwind(call) {
                Ok(Some(b)) => format!("{:016x}", fnv1a(&b)),
                Ok(None) => "None".to_string(),
                Err(_) => "Reject".to_string(),
            }
        };
        for fidx in 0..fams.len().pow(n as u32) {
            let combo: Vec<&str> = (0..n)
                .map(|i| fams[(fidx / fams.len().pow(i as u32)) % fams.len()])
                .collect();
            let path = PathInfo::new(build_hops(&combo));
            for (label, opt) in opts() {
                for (ci, (optimal, out, consumed)) in configs().iter().enumerate() {
                    let out_vec: Vec<u128> = out[..n].to_vec();
                    let consumed_vec: Vec<u128> = consumed[..n].to_vec();
                    let hash = entry(&path, *optimal, &out_vec, &consumed_vec, opt);
                    lines.push(format!(
                        "{}hop_{}_{}_{} {}",
                        n,
                        combo.join("_"),
                        label,
                        ci,
                        hash
                    ));
                }
            }
        }
    }
    // Any-N all-V2 (N = 4): the only reachable >3-hop family.
    for (label, opt) in opts() {
        for (ci, (optimal, out, consumed)) in configs().iter().enumerate() {
            let mut hops = Vec::new();
            let cycle = [WETH, USDC, WBTC];
            for i in 0..4 {
                let (in_t, out_t) = (cycle[i % 3], cycle[(i + 1) % 3]);
                hops.push(v2(in_t, out_t, i as u8));
            }
            let path = PathInfo::new(hops);
            let out4: Vec<u128> = vec![out[0]; 4];
            let consumed4: Vec<u128> = vec![consumed[0]; 4];
            let ctx = EncodeContext::new(EXECUTOR, PM, WETH);
            let hash = match std::panic::catch_unwind(|| {
                encode_cmd_stream(
                    &ctx,
                    &EncodeRequest::new(
                        path.clone(),
                        *optimal,
                        out4.clone(),
                        consumed4.clone(),
                        opt,
                    ),
                )
            }) {
                Ok(Some(b)) => format!("{:016x}", fnv1a(&b)),
                Ok(None) => "None".to_string(),
                Err(_) => "Reject".to_string(),
            };
            lines.push(format!("4hop_allV2_{label}_{ci} {hash}"));
        }
    }
    lines.sort();
    for l in &lines {
        println!("{l}");
    }
    assert_eq!(
        lines.join("\n"),
        GOLDEN.join("\n"),
        "GRAMMAR BYTE-IDENTITY BROKEN — the scaffold extraction changed a family's bytes or decline partition"
    );
}
