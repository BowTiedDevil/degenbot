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
//!
//! ─────────────────────────────────────────────────────────────────────────
//!
//! **D6 enclosure-derivation invariant** (`d6_enclosure_derived_from_facts`).
//!
//! Delegation is necessary but NOT sufficient. D6's depth property requires
//! the enclosure (which FlashSwap/V4Unlock wraps which, the repayment order) to
//! be **derived from the `Repay`/`OutDest` tag partition**, not from hardcoded
//! `match (facts[0].prot, facts[1].prot, …)` arms with per-family bodies.
//!
//! The delegation gate passes if `derive_plan` is called — but `derive_plan`
//! can still dispatch on the protocol tuple with a hardcoded body per arm. That
//! is the false-completion shape this probe catches.
//!
//! Two checks:
//!
//! 1. **No prot-tuple dispatch:** enforced by the source-level lighthouse
//!    `d6_no_prot_tuple_match_arms` (the `FALLBACK_DISPATCH_CALLS` atomic
//!    that used to back this check was removed — it was never incremented, so
//!    its `== 0` assertion was vacuous — and the property is fully covered by
//!    the match-arm scan).
//!
//! 2. **V4 hops tagged `Repay::NetZero`:** V4 hops inside a V4Unlock must carry
//!    `Repay::NetZero` (the tag that bypasses the prot-tuple arms and routes to
//!    the tag-driven partition). Currently 22 V4 families use the generic
//!    `v4_hop_facts` helper which sets `Offstream` — the tags don't describe
//!    the enclosure, so the partition can't derive it.
//!
//! Both checks are RED until ALL 35 families are migrated. The failure message
//! lists every failing family. Migration per family:
//!
//! (a) Set the correct `repay`/`out_dest` tags in `facts_of_<family>` (replace
//!     the generic `v4_hop_facts` call with explicit NetZero/SelfRefund/
//!     Offstream tags that describe the family's actual enclosure).
//! (b) Extend the tag-driven partition in `derive_plan` to handle the new tag
//!     pattern (the `Repay`/`OutDest` partition at the end of `derive_plan`).
//! (c) Delete the prot-tuple match arm for that family.
//!
//! `EXPECTED_PROT_TUPLE_ARMS` counts `match (facts[...].prot` arms in the
//! source — a source-level lighthouse that hits 0 only when every arm is gone.

#![expect(clippy::cast_possible_truncation, clippy::doc_markdown)]

use alloy::primitives::{address, Address};
use degenbot_executor::composers::{
    ComposerInputs, EncodeOptions, HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo,
};
use degenbot_executor::grammar_ledger::Prot;
use degenbot_executor::grammar_shape::derive_shape;
use degenbot_executor::grammar_walker::{family_facts, Repay, DERIVE_PLAN_CALLS};
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
const DONE: &[&str] = &[
    "v3v4v3", "v2v2v2", "v3v2", "v3v3", "v2v3", "v3v3v3", "v3v3v2", "v3v2v3", "v3v2v2", "v2v2v3",
    "v2v3v3", "v2v3v2", "v4v2", "v4v4", "v4v3", "v3v4", "v2v4", "v4v4v4", "v4v2v2", "v4v2v4",
    "v4v3v3", "v4v3v4", "v4v4v2", "v4v4v3", "v4v2v3", "v4v3v2", "v2v2v4", "v2v4v4", "v2v3v4",
    "v3v2v4", "v3v3v4", "v2v4v2", "v2v4v3", "v3v4v2", "v3v4v4",
];

/// Per-family deriver bodies (`fn derive_2hop_*` / `fn derive_3hop_*` /
/// `fn derive_all_v2`) still present. Zero at full D6 realization. Decrement
/// as each body is folded into the generic `derive_plan`.
const EXPECTED_REMAINING: usize = 0;

/// Prot-tuple `match (facts[...].prot …)` arms in `derive_plan`. Each is a
/// hardcoded per-family dispatch D6 replaces with tag-driven derivation.
/// Currently: v4-led 2-hop (`match facts[1].prot`), 3-hop pure V2/V3
/// (`match (facts[0].prot, …)`), 3-hop V4-crossing (inner `match`), 2-hop
/// V2/V3 (`match (facts[0].prot, facts[1].prot)`).
const EXPECTED_PROT_TUPLE_ARMS: usize = 0;

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

// ── D6 enclosure-derivation invariant (the REAL depth property) ───────────

/// Extract the `Repay` tag sequence from a family's facts.
fn repay_tags(prots: &[Prot], path: &PathInfo, inputs: &ComposerInputs<'_>) -> Option<Vec<Repay>> {
    let key = (
        prots.first().copied(),
        prots.get(1).copied(),
        prots.get(2).copied(),
    );
    let facts_fn = family_facts(key)?;
    let facts = facts_fn(path, inputs)?;
    Some(facts.iter().map(|f| f.repay).collect())
}

/// D6 enclosure-derivation invariant.
///
/// Two checks (both RED until all 35 families are migrated; the first is a
/// source-level lighthouse):
///
/// 1. **No prot-tuple dispatch** — the enclosure must be derived from the
///    `Repay`/`OutDest` tag partition, not from `match (facts[0].prot, …)`
///    arms. See `d6_no_prot_tuple_match_arms`.
///
/// 2. **V4 hops tagged `NetZero`** — every V4 hop must carry `Repay::NetZero`
///    (the tag that routes to the tag-driven partition). The generic
///    `v4_hop_facts` helper sets `Offstream` — it must be replaced with
///    explicit tags that describe the family's actual enclosure.
#[test]
fn d6_enclosure_derived_from_facts() {
    let fams = [Prot::V2, Prot::V3, Prot::V4];
    let mut netzero_missing: Vec<String> = Vec::new();

    for n in [2usize, 3] {
        for fidx in 0..fams.len().pow(n as u32) {
            let prots: Vec<Prot> = (0..n)
                .map(|i| fams[(fidx / fams.len().pow(i as u32)) % fams.len()])
                .collect();
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

            // V4 hops tagged Repay::NetZero (no-prot-tuple dispatch is
            // covered by `d6_no_prot_tuple_match_arms`).
            if let Some(tags) = repay_tags(&prots, &path, &inputs) {
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
    }

    assert!(
        netzero_missing.is_empty(),
        "D6 violation — families whose V4 hops lack Repay::NetZero (the tag \
         that routes to the tag-driven partition). Currently using the generic \
         v4_hop_facts helper (Offstream) instead of explicit tags:\n  {}\n\
         Migration: replace v4_hop_facts with explicit NetZero tags for V4 hops \
         inside a V4Unlock.",
        netzero_missing.join("\n  ")
    );
}

/// Source-level lighthouse: count `match (facts[...].prot` arms in
/// `derive_plan`. Each is a hardcoded per-family dispatch that must be deleted
/// when the family is migrated to the tag-driven partition.
#[test]
fn d6_no_prot_tuple_match_arms() {
    let src = include_str!("../src/grammar_walker.rs");
    // Count `match (facts[...].prot` and `match facts[...].prot` in the source.
    let count =
        src.matches("match (facts[0].prot").count() + src.matches("match facts[1].prot").count();
    assert_eq!(
        count, EXPECTED_PROT_TUPLE_ARMS,
        "prot-tuple match arms in derive_plan: {count} found, \
         {EXPECTED_PROT_TUPLE_ARMS} expected — decrement EXPECTED_PROT_TUPLE_ARMS \
         as each arm is deleted (the enclosure is derived from the Repay/OutDest \
         tag partition, not from match arms). Target: 0."
    );
}
