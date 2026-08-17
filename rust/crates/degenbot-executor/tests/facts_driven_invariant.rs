//! D6 tag-sanity invariant (epic `PZBGP7` T2 — after the ADR-031 record
//! correction).
//!
//! The migration-era tripwires (the `DERIVE_PLAN_CALLS` counter + `DONE`
//! allowlist probe, the `include_str!` source-scan lighthouses, the
//! `EXPECTED_*` counters) are DELETED: they measured spelling, not structure
//! — the "0 prot-tuple match arms" scan was green while identical dispatch
//! existed as if/else chains (see the ADR-031 record correction, 2026-08).
//! Behavioural coverage lives in `grammar_parity.rs` (routing through
//! `derive_shape`), the golden corpora, and the revm contract matrix
//! (ADR-029 D5's designated source of truth).
//!
//! What remains is the one honest, load-bearing property: **every supported
//! family's hop facts tag V4 hops `Repay::NetZero`** — the tag the residual
//! tag-driven partition (`grammar_walker/shapes/tag_residual.rs`) genuinely
//! routes on. This is a property of the facts the production dispatcher
//! (`family_facts`) constructs, not of source text.

#![expect(clippy::cast_possible_truncation)]

use alloy::primitives::{address, Address};
use degenbot_executor::composers::{
    ComposerInputs, EncodeOptions, HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo,
};
use degenbot_executor::grammar_ledger::Prot;
use degenbot_executor::grammar_walker::{family_facts, Repay};

const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
const USDC: Address = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
const WBTC: Address = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
const PM: Address = address!("000000000004444c5dc75cB358380D2e3De08A90");
const EXEC: Address = address!("DeAd0000000000000000000000000000000000Be");
const OPTIMAL: u128 = 1_000_000_000_000_000_000;
static OUTS: [u128; 3] = [1_000_000_000_000_000_000; 3];
static CONSUMED: [u128; 3] = [999_999_999_999_999_999; 3];

fn combo_hops(prots: &[Prot]) -> Vec<HopInfo> {
    (0..prots.len())
        .map(|i| {
            let in_t = match i % 3 {
                0 => WETH,
                1 => USDC,
                _ => WBTC,
            };
            let out_t = match (i + 1) % 3 {
                0 => WETH,
                1 => USDC,
                _ => WBTC,
            };
            match prots[i] {
                Prot::V2 => HopInfo::V2(V2HopInfo {
                    pool_address: Address::from([0xA0 + i as u8; 20]),
                    token0_address: in_t,
                    token1_address: out_t,
                    fee: 30,
                    zfo: true,
                }),
                Prot::V3 => HopInfo::V3(V3HopInfo {
                    pool_address: Address::from([0xB0 + i as u8; 20]),
                    token0_address: in_t,
                    token1_address: out_t,
                    fee: 3000,
                    zfo: true,
                }),
                Prot::V4 => HopInfo::V4(V4HopInfo {
                    pool_manager_address: PM,
                    pool_id_hex: format!("0x{i:02x}"),
                    currency0_address: in_t,
                    currency1_address: out_t,
                    fee: 500,
                    tick_spacing: 10,
                    hook_address: Address::ZERO,
                    zfo: true,
                }),
            }
        })
        .collect()
}

fn family_name(prots: &[Prot]) -> String {
    prots
        .iter()
        .map(|p| match p {
            Prot::V2 => "v2",
            Prot::V3 => "v3",
            Prot::V4 => "v4",
        })
        .collect()
}

/// Every supported family's hop facts tag its V4 hops `Repay::NetZero` — the
/// tag the residual tag-driven partition genuinely routes on. Facts are
/// fetched through `family_facts`, the same dispatcher the production path
/// uses (post-T3: the single facts route).
#[test]
fn d6_enclosure_derived_from_facts() {
    let fams = [Prot::V2, Prot::V3, Prot::V4];
    let mut netzero_missing: Vec<String> = Vec::new();

    for n in [2usize, 3] {
        for fidx in 0..fams.len().pow(n as u32) {
            let prots: Vec<Prot> = (0..n)
                .map(|i| fams[(fidx / fams.len().pow(i as u32)) % fams.len()])
                .collect();
            // (v2,v2) 2-hop and (v2,v2,v2) both resolve to all_v2.
            if prots == vec![Prot::V2, Prot::V2] {
                continue;
            }
            let name = family_name(&prots);
            let path = PathInfo::new(combo_hops(&prots));
            let inputs = ComposerInputs {
                executor_address: EXEC,
                pool_manager_address: PM,
                weth_address: WETH,
                optimal_input: OPTIMAL,
                hop_outputs: &OUTS[..n],
                consumed_inputs: &CONSUMED[..n],
                opts: EncodeOptions::default(),
            };
            let key = (
                prots.first().copied(),
                prots.get(1).copied(),
                prots.get(2).copied(),
            );
            let Some(facts_fn) = family_facts(key) else {
                continue;
            };
            let Some(tags) =
                facts_fn(&path, &inputs).map(|fs| fs.iter().map(|f| f.repay).collect::<Vec<_>>())
            else {
                continue;
            };

            let has_v4 = prots.contains(&Prot::V4);
            let v4_has_netzero = prots
                .iter()
                .zip(tags.iter())
                .any(|(p, r)| *p == Prot::V4 && *r == Repay::NetZero);
            if has_v4 && !v4_has_netzero {
                let tag_strs: Vec<String> = tags.iter().map(|r| format!("{r:?}")).collect();
                netzero_missing.push(format!("{name} (tags=[{}])", tag_strs.join(", ")));
            }
        }
    }

    assert!(
        netzero_missing.is_empty(),
        "D6 violation — families whose V4 hops lack Repay::NetZero (the tag \
         the residual tag partition routes on):\n  {}",
        netzero_missing.join("\n  ")
    );
}
