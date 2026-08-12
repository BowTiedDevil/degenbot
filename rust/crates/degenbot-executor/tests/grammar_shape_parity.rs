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

// ── V4: native + wrap/unwrap bridge parity (WAYDTL step 2, (A) widen) ─────

const NATIVE: Address = Address::ZERO;

fn v4_pair(c0: Address, c1: Address, zfo: bool) -> HopInfo {
    HopInfo::V4(degenbot_executor::composers::V4HopInfo {
        pool_manager_address: pm(),
        pool_id_hex: "0x0".into(),
        currency0_address: c0,
        currency1_address: c1,
        fee: 3000,
        tick_spacing: 60,
        hook_address: Address::ZERO,
        zfo,
    })
}

/// The V4/v4 derivation must match the hand-written v4_v4 across native +
/// wrap/unwrap-bridge currency configurations (byte parity).
#[test]
fn v4_v4_native_and_bridge_parity() {
    let t = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    // Explicit configs to keep readability (currency addresses set inline):
    for (a0, a1, az, b0, b1, bz) in [
        (weth(), t, true, t, weth(), true),           // WETH->t->WETH
        (NATIVE, t, true, t, NATIVE, true),           // NATIVE->t->NATIVE (native capture)
        (weth(), NATIVE, true, NATIVE, weth(), true), // WETH->native->WETH (non-gap, mid native)
        (t, NATIVE, false, weth(), t, true),          // Wrap bridge (a out native, b in WETH)
        (weth(), t, true, NATIVE, t, true),           // Unwrap bridge (a out t, b in native)
        (NATIVE, t, false, t, weth(), false),         // mixed zfo
        (weth(), t, false, t, NATIVE, false),         // mixed zfo + native end
    ] {
        run_family(vec![v4_pair(a0, a1, az), v4_pair(b0, b1, bz)], 100_000);
    }
}

// ── V4 boundary-crossing: v4_v3 and v3_v4 (WAYDTL step 2 / (A)) ───────────

/// V4→V3: V4's ERC-20 output leaves the PM (TAKE→SELF) to fund the V3 swap.
/// Must match the hand-written v4_v3 byte-for-byte.
#[test]
fn v4_v3_boundary_parity() {
    let t = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    let t2 = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
    run_family(
        vec![v4_pair(weth(), t, true), v3_pool(t, weth(), true)],
        100_000,
    );
    run_family(
        vec![v4_pair(t2, t, false), v3_pool(t2, weth(), true)],
        100_000,
    );
}

/// V3→V4: the V3 flash's ERC-20 output enters the PM (sync+transfer+settle) to
/// seed the V4 input; V3 repaid with WETH. Must match v3_v4 byte-for-byte.
#[test]
fn v3_v4_boundary_parity() {
    let t = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    let t2 = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
    run_family(
        vec![v3_pool(weth(), t, true), v4_pair(t, weth(), true)],
        100_000,
    );
    run_family(vec![v3_pool(t2, t, false), v4_pair(t, t2, false)], 100_000);
}

// ── V4↔V2 boundary + native/mixed (WAYDTL step 2 / (A) close-out) ──────────

/// V4→V2: V4's ERC-20 forward leaves the PM straight to the V2 pool; a native
/// V4 output is wrapped first; a native V4 input is unwrapped to settle.
#[test]
fn v4_v2_native_and_boundary_parity() {
    let t = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    let u = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
    run_family(
        vec![v4_pair(weth(), t, true), v2_pair(t, weth(), true, 30)],
        100_000,
    );
    // Native V4 output -> wrap into WETH for the V2 pool.
    run_family(
        vec![v4_pair(t, NATIVE, false), v2_pair(weth(), u, true, 30)],
        100_000,
    );
    // Native V4 input -> unwrap WETH to settle.
    run_family(
        vec![v4_pair(NATIVE, t, true), v2_pair(t, weth(), true, 30)],
        100_000,
    );
}

/// V2→V4: the V2 flash's forward enters the PM to seed the V4 input; native V4
/// input unwraps the V2 WETH output; native V4 output re-wraps for the V2
/// repayment.
#[test]
fn v2_v4_native_and_boundary_parity() {
    let t = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    let u = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
    run_family(
        vec![v2_pair(weth(), t, true, 30), v4_pair(t, weth(), true)],
        100_000,
    );
    // Native V4 input: V2 WETH output is unwrapped to seed the native input.
    run_family(
        vec![v2_pair(t, weth(), true, 30), v4_pair(NATIVE, u, true)],
        100_000,
    );
    // Native V4 output: WETH_DEPOSIT re-wraps for the V2 repayment.
    run_family(
        vec![v2_pair(weth(), t, true, 30), v4_pair(t, NATIVE, true)],
        100_000,
    );
}

/// Mixed native↔WETH mids across V4↔V3 boundary-crossing families.
#[test]
fn v4_v3_and_v3_v4_native_mixed_parity() {
    let t = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    let u = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
    // v4_v3: native V4 output wrapped into WETH for the V3 pool.
    run_family(
        vec![v4_pair(t, NATIVE, false), v3_pool(weth(), u, true)],
        100_000,
    );
    // v4_v3: native V4 input unwrapped to settle before the V3 swap.
    run_family(
        vec![v4_pair(NATIVE, t, true), v3_pool(t, weth(), true)],
        100_000,
    );
    // v3_v4: native V4 input, V3 WETH output unwrapped to seed it.
    run_family(
        vec![v3_pool(t, weth(), true), v4_pair(NATIVE, u, true)],
        100_000,
    );
    // v3_v4: native V4 output taken directly (non-native input branch).
    run_family(
        vec![v3_pool(weth(), t, true), v4_pair(t, NATIVE, true)],
        100_000,
    );
}

// ── 3-hop pure-V4 container (WAYDTL step 3) ────────────────────────────────

/// v4_v4_v4: one unlock over three internal V4 swaps; must be byte-identical
/// to the hand-written adapter for WETH-only and native/bridge configs.
#[test]
fn v4_v4_v4_native_and_bridge_parity() {
    let t = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    let u = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
    let t2 = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
    let _ = t2;
    // WETH -> t -> u -> WETH (no gaps).
    run_family(
        vec![
            v4_pair(weth(), t, true),
            v4_pair(t, u, true),
            v4_pair(u, weth(), true),
        ],
        100_000,
    );
    // NATIVE -> t -> u -> NATIVE (native at both ends, ERC20 mids).
    run_family(
        vec![
            v4_pair(NATIVE, t, true),
            v4_pair(t, u, true),
            v4_pair(u, NATIVE, true),
        ],
        100_000,
    );
    // WETH -> native -> WETH -> u (native mid, non-gap).
    run_family(
        vec![
            v4_pair(weth(), NATIVE, true),
            v4_pair(NATIVE, weth(), true),
            v4_pair(weth(), t, true),
        ],
        100_000,
    );
}

/// v4_v2_v2: V4 output leaves PM to a V2 pool, chaining two V2_SWAP_CALC legs.
#[test]
fn v4_v2_v2_parity() {
    let t = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    let u = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
    run_family(
        vec![
            v4_pair(weth(), t, true),
            v2_pair(t, u, true, 30),
            v2_pair(u, weth(), true, 30),
        ],
        100_000,
    );
}
