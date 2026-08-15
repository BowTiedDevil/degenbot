//! D6 honesty invariant — the HONEST gate (epic `6SU5LM` / T0 re-drive).
//!
//! The original A2/A3 "facts_driven" probe was vacuous: every `build_*_walk`
//! called a per-family `derive_<fam>` body directly, and `derive_plan` merely
//! dispatched back to that same body — so asserting `build == derive_plan` was
//! `f(x) == f(x)`. This probe is NOT that.
//!
//! **Structural delegation check.** `DERIVE_PLAN_CALLS` is incremented on every
//! `derive_plan` entry. For each family the probe resets the counter, encodes
//! the family through the public `derive_shape` (→ `build_for_walk` →
//! `build_<fam>_walk`), then reads the counter. A family is genuinely
//! facts-driven iff its `build_<fam>_walk` routes the Plan through `derive_plan`
//! (counter ≥ 1) AND the per-family `derive_<fam>` body has been deleted so the
//! routing cannot be confused with dispatch-deception.
//!
//! **Progress honest, never false-green.** `DONE` lists the families whose
//! `build_<fam>_walk` truly delegates. A done family MUST delegate (or RED). A
//! pending family MUST still bypass the generic deriver (counter == 0, or RED —
//! it would mean someone wired delegation without folding the body / deleting
//! the per-family deriver, the exact false-completion shape). Un-ignoring a row
//! is therefore a claim the body was genuinely folded into the generic deriver.
//!
//! `remaining_per_family_derivers` caps the lot: it counts `fn derive_2hop_*` /
//! `fn derive_3hop_*` / `fn derive_all_v2` definitions still in the source and
//! asserts the count matches `EXPECTED_REMAINING` — decrementing as each body is
//! folded into the generic `derive_plan`. Zero at completion.

#![allow(
    clippy::too_many_lines,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::doc_markdown
)]

use alloy::primitives::{address, Address};
use degenbot_executor::composers::{
    ComposerInputs, EncodeOptions, HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo,
};
use degenbot_executor::grammar_ledger::Prot;
use degenbot_executor::grammar_shape::derive_shape;
use degenbot_executor::grammar_walker::DERIVE_PLAN_CALLS;
use std::sync::atomic::Ordering;

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

/// Families whose `build_<fam>_walk` genuinely routes through the generic
/// `derive_plan` with the per-family body folded in (deleted). Migrate a
/// family by folding its deriver into `derive_plan` + wiring delegation, then
/// add its name here and un-ignore the row below.
const DONE: &[&str] = &["v3v4v3", "v2v2v2", "v3v2", "v3v3", "v2v3"];

/// Per-family deriver bodies (`fn derive_2hop_*` / `fn derive_3hop_*` /
/// `fn derive_all_v2`) still present. Zero at full D6 realization. Decrement
/// as each body is folded into the generic `derive_plan`.
const EXPECTED_REMAINING: usize = 37;

#[test]
fn facts_driven_invariant() {
    let fams = [Prot::V2, Prot::V3, Prot::V4];
    let mut done_set: std::collections::HashSet<&str> = DONE.iter().copied().collect();
    let mut pending_delegated: Vec<String> = Vec::new();
    let mut done_bypassed: Vec<String> = Vec::new();
    for n in [2usize, 3] {
        for fidx in 0..fams.len().pow(n as u32) {
            let prots: Vec<Prot> = (0..n)
                .map(|i| fams[(fidx / fams.len().pow(i as u32)) % fams.len()])
                .collect();
            // The (v2,v2) 2-hop and (v2,v2,v2) 3-hop both resolve to all_v2.
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
            DERIVE_PLAN_CALLS.store(0, Ordering::Relaxed);
            let encoded = derive_shape(&path, &inputs);
            let calls = DERIVE_PLAN_CALLS.load(Ordering::Relaxed);
            let is_done = done_set.remove(name.as_str());
            // Every family must encode under the fixed fixture (the regression
            // floor); a None here means the probe fixture drifted.
            assert!(
                encoded.is_some(),
                "[{name}] probe fixture must encode the family"
            );
            if is_done {
                if calls == 0 {
                    done_bypassed.push(name.clone());
                }
                assert!(calls >= 1, "[{name}] DONE family bypassed derive_plan (calls={calls}) — fold its per-family deriver into the generic derive_plan and wire build_<fam>_walk to delegate");
            } else if calls >= 1 {
                pending_delegated.push(name.clone());
            }
        }
    }
    assert!(
        done_set.is_empty(),
        "DONE allowlist names not exercised by the probe: {done_set:?}"
    );
    assert!(
        done_bypassed.is_empty(),
        "DONE families that did NOT delegate: {done_bypassed:?}"
    );
    // A pending family that delegates means someone wired `build_*_walk` →
    // `derive_plan` WITHOUT folding/deleting the per-family body — the exact
    // false-completion shape. Fold the body, then move the name into DONE.
    assert!(pending_delegated.is_empty(), "pending families that delegate derive_plan without the body folded (false-completion): {pending_delegated:?}");
}

#[test]
fn remaining_per_family_derivers() {
    let src = include_str!("../src/grammar_walker.rs");
    let count = src.matches("fn derive_2hop_").count()
        + src.matches("fn derive_3hop_").count()
        + src.matches("fn derive_all_v2").count();
    assert_eq!(count, EXPECTED_REMAINING, "per-family deriver body count drifted: {count} present, {EXPECTED_REMAINING} expected — update EXPECTED_REMAINING only when a body is folded into the generic derive_plan (decrementing toward 0)");
}
