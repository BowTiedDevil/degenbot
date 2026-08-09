//! Facet A (T2TCJM) grammar coverage + routing invariant.
//!
//! Since `encode_cmd_stream` / `encode_cmd_3_hop` now *delegate* to the grammar
//! (`grammar::encode_grammar` / `grammar::encode_all_v2`), the byte-identity of
//! every combo is pinned end-to-end by the golden corpus
//! (`composers_parity.rs`, `composers_3hop_parity.rs`, `native_*`). The
//! byte-for-byte parity harness is therefore redundant post-swap.
//!
//! This test instead guards the **routing/coverage** invariant: every 2-hop
//! and 3-hop family combo must still encode (`Some`) through both public entry
//! points for valid amounts — i.e. no combo is accidentally dropped by the
//! grammar walk. It also asserts the deliberate all-V2 routing split: the
//! N-hop speedrail (`encode_cmd_stream`) and the 3-hop `v2_v2_v2` layout
//! (`encode_cmd_3_hop`) are both reachable and non-empty.

#![expect(
    clippy::cast_possible_truncation,
    clippy::print_stderr,
    clippy::too_many_lines
)]

use alloy::primitives::{address, Address};
use degenbot_executor::composers::{
    encode_cmd_3_hop, encode_cmd_stream, EncodeOptions, HopInfo, PathInfo, V2HopInfo, V3HopInfo,
    V4HopInfo,
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

#[test]
fn every_combo_encodes_through_both_entry_points() {
    let fams = ["V2", "V3", "V4"];
    let cycle = [WETH, USDC, WBTC];
    let configs = [
        ("base", EncodeOptions::default()),
        (
            "erc6909",
            EncodeOptions {
                erc6909_profit: true,
                use_v4_batch: false,
            },
        ),
        (
            "batch",
            EncodeOptions {
                erc6909_profit: false,
                use_v4_batch: true,
            },
        ),
    ];

    // Every 2-hop combo must encode via `encode_cmd_stream`.
    for fidx in 0..fams.len().pow(2) {
        let combo = [fams[fidx % 3], fams[(fidx / 3) % 3]];
        for (flabel, opts) in &configs {
            let mut hops = Vec::with_capacity(2);
            for i in 0..2 {
                let (in_t, out_t) = (cycle[i % 3], cycle[(i + 1) % 3]);
                hops.push(match combo[i] {
                    "V2" => v2(in_t, out_t, i as u8),
                    "V3" => v3(in_t, out_t, i as u8),
                    _ => v4(in_t, out_t, i as u8),
                });
            }
            let path = PathInfo::new(hops);
            let out2: Vec<u128> = vec![1_000_000_000_000_000_000u128; 2];
            let consumed: Vec<u128> = vec![999_999_999_999_999_999u128; 2];
            let r = encode_cmd_stream(
                &path,
                1_000_000_000_000_000_000,
                &out2,
                &consumed,
                EXECUTOR,
                PM,
                WETH,
                *opts,
            );
            assert!(
                r.is_some(),
                "2-hop {} / {} produced None",
                combo.join("_"),
                flabel
            );
        }
    }

    // Every 3-hop combo must encode via BOTH entry points.
    for fidx in 0..fams.len().pow(3) {
        let combo = [fams[fidx % 3], fams[(fidx / 3) % 3], fams[(fidx / 9) % 3]];
        for (flabel, opts) in &configs {
            let mut hops = Vec::with_capacity(3);
            for i in 0..3 {
                let (in_t, out_t) = (cycle[i % 3], cycle[(i + 1) % 3]);
                hops.push(match combo[i] {
                    "V2" => v2(in_t, out_t, i as u8),
                    "V3" => v3(in_t, out_t, i as u8),
                    _ => v4(in_t, out_t, i as u8),
                });
            }
            let path = PathInfo::new(hops);
            let out2: Vec<u128> = vec![1_000_000_000_000_000_000u128; 3];
            let consumed: Vec<u128> = vec![999_999_999_999_999_999u128; 3];
            let a = encode_cmd_stream(
                &path,
                1_000_000_000_000_000_000,
                &out2,
                &consumed,
                EXECUTOR,
                PM,
                WETH,
                *opts,
            );
            let b = encode_cmd_3_hop(
                &path,
                1_000_000_000_000_000_000,
                &out2,
                &consumed,
                EXECUTOR,
                PM,
                WETH,
                *opts,
            );
            assert!(
                a.is_some(),
                "3-hop {} / {} encode_cmd_stream -> None",
                combo.join("_"),
                flabel
            );
            assert!(
                b.is_some(),
                "3-hop {} / {} encode_cmd_3_hop -> None",
                combo.join("_"),
                flabel
            );
        }
    }
}

#[test]
fn all_v2_routing_split_holds() {
    // For an all-V2 3-hop path, `encode_cmd_stream` (N-hop speedrail, top swap
    // on pool A) and `encode_cmd_3_hop` (v2_v2_v2 layout, top swap on pool C)
    // produce structurally distinct non-empty streams — the deliberate routing
    // split preserved from the bespoke encoders.
    let tessera: Vec<HopInfo> = vec![v2(WETH, USDC, 0), v2(USDC, WBTC, 1), v2(WBTC, WETH, 2)];
    let path = PathInfo::new(tessera);
    let out2: Vec<u128> = vec![1_000_000_000_000_000_000u128; 3];
    let consumed: Vec<u128> = vec![999_999_999_999_999_999u128; 3];
    let a = encode_cmd_stream(
        &path,
        1_000_000_000_000_000_000,
        &out2,
        &consumed,
        EXECUTOR,
        PM,
        WETH,
        EncodeOptions::default(),
    );
    let b = encode_cmd_3_hop(
        &path,
        1_000_000_000_000_000_000,
        &out2,
        &consumed,
        EXECUTOR,
        PM,
        WETH,
        EncodeOptions::default(),
    );
    assert!(
        a.is_some() && b.is_some(),
        "all-V2 3-hop must encode through both entries"
    );
    assert_ne!(a, b, "speedrail and v2_v2_v2 layouts must remain distinct");
    if let (Some(a), Some(b)) = (a, b) {
        eprintln!(
            "  all-V2 3-hop: encode_cmd_stream len={} encode_cmd_3_hop len={}",
            a.len(),
            b.len()
        );
    }
}
