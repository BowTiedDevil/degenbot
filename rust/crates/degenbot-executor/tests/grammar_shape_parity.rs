#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::doc_markdown
)]
//! wayDTL cutover parity (epic 463V2C) — the V2/V3 2-hop families (`v2_v3`,
//! `v3_v2`, `v3_v3`) emit via the `grammar_shape` derivation inside
//! `encode_grammar` (`cutover_2hop`, with the proven adapter as backstop). This
//! test documents that the fold is **live and stable**: for every folded family
//! across **protocol-order × zfo × amount** variations, the derivation produces
//! bytes AND production (`encode_cmd_stream`) is byte-identical to it. Together
//! with `grammar_parity.rs` (derive-vs-bespoke through both entry points) and
//! the runtime matrix (`harness_declarative.rs`, exact-delta) this pins the fold.

use alloy::primitives::{address, Address};
use degenbot_executor::composers::{
    self, ComposerInputs, EncodeOptions, HopInfo, PathInfo, V2HopInfo, V3HopInfo,
};
use degenbot_executor::grammar_shape::derive_shape;

fn weth() -> Address {
    address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
}
fn executor() -> Address {
    address!("DeAd0000000000000000000000000000000000Be")
}
fn pm() -> Address {
    address!("000000000004444c5dc75cB358380D2e3dE08A90")
}
fn v2_pair(t0: Address, t1: Address, zfo: bool, fee: u16) -> HopInfo {
    HopInfo::V2(V2HopInfo {
        pool_address: address!("00000000000000000000000000000000000000aa"),
        token0_address: t0,
        token1_address: t1,
        fee,
        zfo,
    })
}
fn v3_pool(t0: Address, t1: Address, zfo: bool) -> HopInfo {
    HopInfo::V3(V3HopInfo {
        pool_address: address!("00000000000000000000000000000000000000bb"),
        token0_address: t0,
        token1_address: t1,
        fee: 3000,
        zfo,
    })
}

fn run_family(hops: Vec<HopInfo>, exact_in: u128) {
    let path = PathInfo::new(hops);
    let n = path.hops.len();
    // A generic, non-degenerate forward amount chain (arbitrary, fixed).
    let hop_outputs: Vec<u128> = (0..n)
        .map(|i| exact_in * (10u128.pow(i as u32) + 1))
        .collect();
    let consumed: Vec<u128> = std::iter::once(exact_in)
        .chain(hop_outputs.iter().copied())
        .take(n)
        .collect();
    let inputs = ComposerInputs {
        executor_address: executor(),
        pool_manager_address: pm(),
        weth_address: weth(),
        optimal_input: exact_in,
        hop_outputs: &hop_outputs,
        consumed_inputs: &consumed,
        opts: EncodeOptions {
            erc6909_profit: false,
            use_v4_batch: false,
        },
    };

    // The derivation must be live (Some) for every folded family.
    let derived = derive_shape(&path, &inputs)
        .unwrap_or_else(|| panic!("derive_shape returned None for a folded family"));
    // Production (which routes this family through the cutover) must equal it.
    let prod = composers::encode_cmd_stream(
        &path,
        exact_in,
        &hop_outputs,
        &consumed,
        executor(),
        pm(),
        weth(),
        EncodeOptions::default(),
    )
    .unwrap_or_else(|| panic!("encode_cmd_stream returned None"));
    assert_eq!(
        derived, prod,
        "folded family: production must be byte-identical to the derivation"
    );
}

#[test]
fn v2_v3_fold_is_live_and_stable_across_zfo_and_amounts() {
    let t = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    for zfo_a in [true, false] {
        for zfo_b in [true, false] {
            for amount in [1_000u128, 100_000, 10_000_000] {
                run_family(
                    vec![v2_pair(weth(), t, zfo_a, 30), v3_pool(t, weth(), zfo_b)],
                    amount,
                );
            }
        }
    }
}

#[test]
fn v3_v2_fold_is_live_and_stable_across_zfo_and_amounts() {
    let t = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    for zfo_a in [true, false] {
        for zfo_b in [true, false] {
            for amount in [1_000u128, 100_000, 10_000_000] {
                run_family(
                    vec![v3_pool(weth(), t, zfo_a), v2_pair(t, weth(), zfo_b, 30)],
                    amount,
                );
            }
        }
    }
}

#[test]
fn v3_v3_fold_is_live_and_stable_across_zfo_and_amounts() {
    let t = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    for zfo_a in [true, false] {
        for zfo_b in [true, false] {
            for amount in [1_000u128, 100_000, 10_000_000] {
                run_family(
                    vec![v3_pool(weth(), t, zfo_a), v3_pool(t, weth(), zfo_b)],
                    amount,
                );
            }
        }
    }
}
