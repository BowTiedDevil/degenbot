#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::doc_markdown,
    clippy::cast_possible_truncation,
    clippy::type_complexity,
    clippy::print_stderr
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

/// A V2 pair with the default 0.3% fee (3-arg form, for homogeneous fn-pointer
/// dispatch in the V2/V3-only 3-hop fold tests).
fn v2_pair3(t0: Address, t1: Address, zfo: bool) -> HopInfo {
    v2_pair(t0, t1, zfo, 30)
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

    // Sweep every EncodeOptions mode (default, V4_BATCH, erc6909 mint, both).
    // The `cutover` debug_assert independently re-derives against the hand-written
    // adapter (its oracle), so a wrong batch/erc6909 layout panics here even
    // though the assert_eq below compares the derivation to production (both
    // derive — the debug_assert inside `encode_cmd_stream` is the real check).
    let modes = [
        EncodeOptions::default(),
        EncodeOptions {
            erc6909_profit: false,
            use_v4_batch: true,
            ..Default::default()
        },
        EncodeOptions {
            erc6909_profit: true,
            use_v4_batch: false,
            ..Default::default()
        },
        EncodeOptions {
            erc6909_profit: true,
            use_v4_batch: true,
            ..Default::default()
        },
    ];
    for opts in modes {
        let inputs = ComposerInputs {
            executor_address: executor(),
            pool_manager_address: pm(),
            weth_address: weth(),
            optimal_input: exact_in,
            hop_outputs: &hop_outputs,
            consumed_inputs: &consumed,
            opts,
        };

        // The derivation must be live (Some) for every folded family in every mode.
        let derived = derive_shape(&path, &inputs)
            .unwrap_or_else(|| panic!("derive_shape returned None for a folded family"));
        let prod = composers::encode_cmd_stream(
            &path,
            exact_in,
            &hop_outputs,
            &consumed,
            executor(),
            pm(),
            weth(),
            opts,
        )
        .unwrap_or_else(|| panic!("encode_cmd_stream returned None"));
        assert_eq!(
            derived, prod,
            "folded family: production must be byte-identical to the derivation (opts {opts:?})"
        );
    }
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

// ── V2/V3-only 3-hop folds (WAYDTL) ────────────────────────────────────────
// The 7 previously-unfolded V2/V3-only 3-hop chains. Each is now emitted by
// `derive_shape` (byte-faithful transcription); `run_family` asserts it is
// live in every EncodeOptions mode and equals production (`encode_cmd_stream`,
// whose `cutover` `debug_assert` re-derives against the hand-written adapter as
// an independent oracle in dev builds), plus the 36-family runtime matrix.
#[test]
fn v2v3_only_3hop_folds_are_live_across_families() {
    let t = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    let u = address!("7Fc66500c84A76Ad7e9c93437bFc5Ac33E2DDaE9");
    let families: Vec<(
        &str,
        fn(Address, Address, bool) -> HopInfo,
        fn(Address, Address, bool) -> HopInfo,
        fn(Address, Address, bool) -> HopInfo,
    )> = vec![
        ("v2_v2_v3", v2_pair3, v2_pair3, v3_pool),
        ("v2_v3_v2", v2_pair3, v3_pool, v2_pair3),
        ("v2_v3_v3", v2_pair3, v3_pool, v3_pool),
        ("v3_v2_v2", v3_pool, v2_pair3, v2_pair3),
        ("v3_v2_v3", v3_pool, v2_pair3, v3_pool),
        ("v3_v3_v2", v3_pool, v3_pool, v2_pair3),
        ("v3_v3_v3", v3_pool, v3_pool, v3_pool),
    ];
    for (name, fa, fb, fc) in families {
        // A fixed 3-hop token chain (entry WETH → t → u → terminal WETH).
        // The terminal hop outputs WETH (zfo=true) — a coherent chain each
        // family's Plan+validator accepts (the validator correctly refuses a
        // u→WETH terminal that inputs WETH, a non-chain the emitter lazily
        // covered).
        let hops = vec![fa(weth(), t, true), fb(t, u, true), fc(u, weth(), true)];
        for amount in [1_000u128, 100_000, 10_000_000] {
            run_family(hops.clone(), amount);
        }
        eprintln!("    folded {name} (parity, all modes)");
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
        (t, NATIVE, true, weth(), t, true),           // Wrap bridge (a out native, b in WETH)
        (t, weth(), true, NATIVE, t, true),           // Unwrap bridge (a out WETH, b in native)
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
    // A non-WETH forward token (t2); WETH entry capital (a's input = WETH)
    // so the boundary-take funds the terminal V3 (the coherent builder+emitter
    // subspace).
    run_family(
        vec![v4_pair(weth(), t2, true), v3_pool(t2, weth(), true)],
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
    // Non-WETH forward token (t2); V3 inputs WETH and the V4's ERC-20 input
    // equals the V3's forward, with a WETH terminal (the coherent
    // builder+emitter subspace).
    run_family(
        vec![v3_pool(weth(), t2, true), v4_pair(t2, weth(), true)],
        100_000,
    );
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
    // Native V4 output (a:(t, NATIVE), zfo=true → output=c1=NATIVE) -> wrap
    // into WETH for the V2 pool (which consumes WETH). The wrap bridges
    // a's native output to b's WETH input — the coherent wrap topology.
    run_family(
        vec![v4_pair(t, NATIVE, true), v2_pair(weth(), u, true, 30)],
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
    // v4_v3: native V4 output (a:(t, NATIVE), zfo=true → output=NATIVE)
    // wrapped into WETH for the V3 pool (which consumes WETH). The wrap
    // bridges a's native output to the V3's WETH input — coherent wrap.
    run_family(
        vec![v4_pair(t, NATIVE, true), v3_pool(weth(), u, true)],
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

/// V4-trailing: v2_v2_v4 and v2_v3_v4. Byte-parity vs hand-written.
#[test]
fn v4_trailing_v2_lead_parity() {
    let t = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    let u = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
    // v2_v2_v4: W -> t -> u -> W through V4's trailing pool.
    run_family(
        vec![
            v2_pair(weth(), t, true, 30),
            v2_pair(t, u, true, 30),
            v4_pair(u, weth(), true),
        ],
        100_000,
    );
    // v2_v3_v4: W -> t -> u -> W (V2, then V3, then trailing V4).
    run_family(
        vec![
            v2_pair(weth(), t, true, 30),
            v3_pool(t, u, true),
            v4_pair(u, weth(), true),
        ],
        100_000,
    );
}

/// V4-trailing (V3-lead): v3_v2_v4 and v3_v3_v4.
#[test]
fn v4_trailing_v3_lead_parity() {
    let t = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    let u = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
    run_family(
        vec![
            v3_pool(weth(), t, true),
            v2_pair(t, u, true, 30),
            v4_pair(u, weth(), true),
        ],
        100_000,
    );
    run_family(
        vec![
            v3_pool(weth(), t, true),
            v3_pool(t, u, true),
            v4_pair(u, weth(), true),
        ],
        100_000,
    );
}

/// V4-middle: v2_v4_v2. Byte-parity vs hand-written.
#[test]
fn v4_middle_v2_parity() {
    let t = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    let u = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
    run_family(
        vec![
            v2_pair(weth(), t, true, 30),
            v4_pair(t, u, true),
            v2_pair(u, weth(), true, 30),
        ],
        100_000,
    );
}

/// V4-middle with V3 tail: v2_v4_v3.
#[test]
fn v4_middle_v2_v3_parity() {
    let t = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    let u = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
    run_family(
        vec![
            v2_pair(weth(), t, true, 30),
            v4_pair(t, u, true),
            v3_pool(u, weth(), true),
        ],
        100_000,
    );
}

/// V4-middle with V3 lead: v3_v4_v2 and v3_v4_v3.
#[test]
fn v4_middle_v3_parity() {
    let t = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    let u = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
    run_family(
        vec![
            v3_pool(weth(), t, true),
            v4_pair(t, u, true),
            v2_pair(u, weth(), true, 30),
        ],
        100_000,
    );
    run_family(
        vec![
            v3_pool(weth(), t, true),
            v4_pair(t, u, true),
            v3_pool(u, weth(), true),
        ],
        100_000,
    );
}

/// V4-middle two-V4: v2_v4_v4 and v3_v4_v4.
#[test]
fn v4_middle_two_v4_parity() {
    let t = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    let u = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
    run_family(
        vec![
            v2_pair(weth(), t, true, 30),
            v4_pair(t, u, true),
            v4_pair(u, weth(), true),
        ],
        100_000,
    );
    run_family(
        vec![
            v3_pool(weth(), t, true),
            v4_pair(t, u, true),
            v4_pair(u, weth(), true),
        ],
        100_000,
    );
}

/// V4-V4 lead into V2 / V3 tail: v4_v4_v2 and v4_v4_v3.
#[test]
fn v4_v4_lead_parity() {
    let t = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    let u = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
    run_family(
        vec![
            v4_pair(weth(), t, true),
            v4_pair(t, u, true),
            v2_pair(u, weth(), true, 30),
        ],
        100_000,
    );
    run_family(
        vec![
            v4_pair(weth(), t, true),
            v4_pair(t, u, true),
            v3_pool(u, weth(), true),
        ],
        100_000,
    );
}

/// V4-leading: v4_v2_v3, v4_v2_v4, v4_v3_v2, v4_v3_v3, v4_v3_v4.
#[test]
fn v4_leading_parity() {
    let t = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    let u = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
    run_family(
        vec![
            v4_pair(weth(), t, true),
            v2_pair(t, u, true, 30),
            v3_pool(u, weth(), true),
        ],
        100_000,
    );
    run_family(
        vec![
            v4_pair(weth(), t, true),
            v2_pair(t, u, true, 30),
            v4_pair(u, weth(), true),
        ],
        100_000,
    );
    run_family(
        vec![
            v4_pair(weth(), t, true),
            v3_pool(t, u, true),
            v2_pair(u, weth(), true, 30),
        ],
        100_000,
    );
    run_family(
        vec![
            v4_pair(weth(), t, true),
            v3_pool(t, u, true),
            v3_pool(u, weth(), true),
        ],
        100_000,
    );
    run_family(
        vec![
            v4_pair(weth(), t, true),
            v3_pool(t, u, true),
            v4_pair(u, weth(), true),
        ],
        100_000,
    );
}

// ── WAYDTL: no-backstop regression lock ───────────────────────────────────
// After retiring the `cutover` backstop (WAYDTL), `encode_grammar` is a pure
// delegate to `derive_shape` for every non-all-V2 family (the all-V2 3-hop
// path is the deliberate `v2_v2_v2` routing split). This locks that invariant:
// for every family, `encode_grammar(path, inputs) == derive_shape(path, inputs)`
// byte-for-byte across every EncodeOptions mode. Before the retire, three
// families (v2_v2_v4, v4_v4_v2, v4_v2_v2) had an over-restrictive WETH-forward
// guard that made `derive_shape` return None while the hand-written adapter
// (the backstop) returned Some — so this equality failed for them. A future
// re-introduction of a backstop, or a new derive_shape None-gap, trips this.

fn assert_no_backstop(hops: Vec<HopInfo>, exact_in: u128) {
    let path = PathInfo::new(hops);
    let n = path.hops.len();
    let hop_outputs: Vec<u128> = (0..n)
        .map(|i| exact_in * (10u128.pow(i as u32) + 1))
        .collect();
    let consumed: Vec<u128> = std::iter::once(exact_in)
        .chain(hop_outputs.iter().copied())
        .take(n)
        .collect();
    let modes = [
        EncodeOptions::default(),
        EncodeOptions {
            erc6909_profit: false,
            use_v4_batch: true,
            ..Default::default()
        },
        EncodeOptions {
            erc6909_profit: true,
            use_v4_batch: false,
            ..Default::default()
        },
        EncodeOptions {
            erc6909_profit: true,
            use_v4_batch: true,
            ..Default::default()
        },
    ];
    for opts in modes {
        let inputs = ComposerInputs {
            executor_address: executor(),
            pool_manager_address: pm(),
            weth_address: weth(),
            optimal_input: exact_in,
            hop_outputs: &hop_outputs,
            consumed_inputs: &consumed,
            opts,
        };
        assert_eq!(
            degenbot_executor::grammar::encode_grammar(&path, &inputs),
            derive_shape(&path, &inputs),
            "encode_grammar must be a pure delegate to derive_shape (no backstop); opts {opts:?}"
        );
    }
}

#[test]
fn no_backstop_weth_forward_families_previously_masked() {
    let t = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    let u = address!("7Fc66500c84A76Ad7e9c93437bFc5Ac33E2DDaE9");
    // v2_v2_v4: V2 b-hop forward = WETH (the masked-gap config).
    assert_no_backstop(
        vec![
            v2_pair(weth(), t, true, 30),
            v2_pair(t, weth(), true, 30),
            v4_pair(weth(), t, true),
        ],
        100_000,
    );
    // v4_v4_v2: V4 b-hop forward = WETH (the masked-gap config).
    assert_no_backstop(
        vec![
            v4_pair(t, u, true),
            v4_pair(u, weth(), true),
            v2_pair(weth(), t, true, 30),
        ],
        100_000,
    );
    // v4_v2_v2: V4 a-hop forward = WETH (the masked-gap config).
    assert_no_backstop(
        vec![
            v4_pair(t, weth(), true),
            v2_pair(weth(), t, true, 30),
            v2_pair(t, weth(), true, 30),
        ],
        100_000,
    );
}

#[test]
fn no_backstop_broad_family_sweep_across_modes() {
    let t = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    let u = address!("7Fc66500c84A76Ad7e9c93437bFc5Ac33E2DDaE9");
    // 2-hop V4-involving families.
    for (a, b) in [
        (v4_pair(weth(), t, true), v3_pool(t, weth(), true)),
        (v3_pool(weth(), t, true), v4_pair(t, weth(), true)),
        (v4_pair(weth(), t, true), v2_pair(t, weth(), true, 30)),
        (v2_pair(weth(), t, true, 30), v4_pair(t, weth(), true)),
        (v4_pair(weth(), t, true), v4_pair(t, weth(), true)),
    ] {
        assert_no_backstop(vec![a, b], 100_000);
    }
    // 3-hop V4-involving families (a representative slice incl. WETH forwards).
    for hops in [
        vec![
            v4_pair(weth(), t, true),
            v2_pair(t, weth(), true, 30),
            v2_pair(weth(), t, true, 30),
        ],
        vec![
            v2_pair(weth(), t, true, 30),
            v2_pair(t, weth(), true, 30),
            v4_pair(weth(), t, true),
        ],
        vec![
            v4_pair(t, u, true),
            v4_pair(u, weth(), true),
            v2_pair(weth(), t, true, 30),
        ],
        vec![
            v3_pool(weth(), t, true),
            v4_pair(t, weth(), true),
            v2_pair(weth(), t, true, 30),
        ],
        vec![
            v4_pair(weth(), t, true),
            v4_pair(t, u, true),
            v4_pair(u, weth(), true),
        ],
    ] {
        assert_no_backstop(hops, 100_000);
    }
}

// ── WE45KC: FundingSource::SelfFund on all-V2 (ADR-029 D1) ───────────────
// The funding axis is load-bearing for all-V2: SelfFund pre-funds the leading
// V2 pair + uses V2_SWAP_CALC for every hop (no V2_SWAP_COMPACT flash
// callback). Locks the byte-structure invariant.

#[test]
fn all_v2_self_fund_has_no_flash_compact_and_pre_funds_v2a() {
    use degenbot_executor::composers::config_for_options;
    use degenbot_executor::grammar::encode_all_v2;
    use degenbot_executor::grammar_ledger::FundingSource;

    let t = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
    let path = PathInfo::new(vec![
        v2_pair(weth(), t, true, 30),
        v2_pair(t, weth(), true, 30),
    ]);
    let outs: Vec<u128> = vec![1_100_000, 1_200_000];
    let consumed: Vec<u128> = vec![1_000_000, 1_100_000];
    let inputs = ComposerInputs {
        executor_address: executor(),
        pool_manager_address: pm(),
        weth_address: weth(),
        optimal_input: 1_000_000,
        hop_outputs: &outs,
        consumed_inputs: &consumed,
        opts: EncodeOptions {
            funding: FundingSource::SelfFund,
            ..Default::default()
        },
    };
    let bytes = encode_all_v2(&path, &inputs).expect("self-fund all-V2 must encode");

    // No V2_SWAP_COMPACT (0x20) — the flash-callback opcode is absent.
    assert!(
        !bytes.contains(&0x20),
        "SelfFund must not emit V2_SWAP_COMPACT (flash callback); got {bytes:?}"
    );
    // The default (InPathFlash) DOES emit V2_SWAP_COMPACT.
    let flash_inputs = ComposerInputs {
        opts: EncodeOptions::default(),
        ..inputs
    };
    let flash = encode_all_v2(&path, &flash_inputs).expect("flash all-V2 must encode");
    assert!(
        flash.contains(&0x20),
        "InPathFlash must emit V2_SWAP_COMPACT"
    );
    assert_ne!(bytes, flash, "SelfFund and InPathFlash bytes must differ");
    let _ = config_for_options; // suppress unused import
}
