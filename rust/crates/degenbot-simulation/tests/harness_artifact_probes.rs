#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::doc_markdown,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::type_complexity
)]
//! TGUZCT item 3 — permanent artifact-side probes for the 0x43
//! `V4_BATCH_OPEN_WETH` opcode (settle-skip batch × `V4_MINT_COMPACT`).
//!
//! These run HAND-BUILT raw command streams — preamble from the current
//! production builder, command bodies from `degenbot_executor::encoders`
//! primitives — against the committed tier3 artifact runtime bytecode ×
//! stub PoolManager. They pin the CONTRACT behavior the encoder flip
//! (0x42→0x43 plan step) and the driver-bytecode resync must preserve,
//! independent of the encoder itself:
//!
//! - (A) open batch + trailing WETH mint EXECUTES with the `check_mode=2`
//!   floor armed, and the profit lands in the ERC6909 vault (positive
//!   `PM.balanceOf(executor, weth)` delta — the oracle half of the
//!   on-chain floor).
//! - (B) the legacy full-settle batch (0x42) + trailing mint REVERTS with
//!   the NAMED `InsufficientMintDelta` error (the SMOZG3 opaque PM `D0`
//!   credit-before-debit is gone).
//! - (C) the hazard: an open batch (0x43) with NO follow-up mint reverts
//!   at unlock with the stub's Error(string) "DELTA" revert (the stub's
//!   model of v4-core's `CurrencyNotSettled`); the encoder's ledger pairing rule
//!   (TGUZCT item 4) makes this stream unrepresentable.

use alloy::primitives::{keccak256, Address, U256};
use degenbot_executor::composers::{ComposerInputs, EncodeOptions};
use degenbot_executor::encoders::{
    enc_v4_batch, enc_v4_mint_compact, enc_v4_settle_all, enc_v4_unlock, V4BatchEntry,
};
use degenbot_executor::grammar_plan::{plan_to_bytes, PlanStep};
use degenbot_executor::grammar_walker::build_walk;
use degenbot_simulation::harness::{ExecOutcome, Harness, Hop, HopPool};

const RESERVE: u128 = 1_000_000_000_000;
const INPUT: u128 = 100_000;
const GAS: u64 = 8_000_000;

fn liq() -> u128 {
    10u128.pow(22)
}
fn q96_one() -> U256 {
    U256::from(1u128) << 96
}
fn sqrt_x(x: u64) -> U256 {
    if x == 1 {
        q96_one()
    } else {
        let s = ((x as f64).sqrt() * 65536.0) as u64;
        q96_one() * U256::from(s) / U256::from(65536)
    }
}

/// A 2-hop WETH→tok→WETH pure-V4 fixture at ~1×/~3× (profitable), matching
/// the declarative capture matrix fixtures.
fn v4v4_fixture(h: &mut Harness) -> Vec<Hop> {
    let t = h.add_token().unwrap();
    vec![
        Hop {
            src: h.weth,
            dst: t,
            pool: HopPool::V4(
                h.add_v4_pool(h.weth, t, 3000, 60, sqrt_x(1), liq(), RESERVE, RESERVE)
                    .unwrap(),
            ),
        },
        Hop {
            src: t,
            dst: h.weth,
            pool: HopPool::V4(
                h.add_v4_pool(t, h.weth, 3000, 60, sqrt_x(3), liq(), RESERVE, RESERVE)
                    .unwrap(),
            ),
        },
    ]
}

/// The non-batch erc6909-capture variant — the production stream that is
/// ALREADY EXECUTABLE today. Fixture + preamble + the reference plan's exact
/// swap/mint codec steps + the reference payload (the baseline the batch
/// probes must not move).
fn probe_setup() -> (
    Harness,
    Vec<Hop>,
    Vec<u128>,
    Vec<u8>,
    PlanStep,
    PlanStep,
    PlanStep,
    Vec<u8>,
) {
    let mut h = Harness::new().unwrap();
    let hops = v4v4_fixture(&mut h);
    let (path, hop_outputs, consumed) = h.path_and_amounts(&hops, INPUT);
    let inputs = ComposerInputs {
        executor_address: h.executor,
        pool_manager_address: h.pool_manager,
        weth_address: h.weth,
        optimal_input: INPUT,
        hop_outputs: &hop_outputs,
        consumed_inputs: &consumed,
        opts: EncodeOptions {
            erc6909_profit: true,
            ..Default::default()
        },
    };
    let (preamble, ref_plan, at) = build_walk(&path, &inputs)
        .expect("v4_v4 non-batch erc6909 capture must not decline (pre-flip baseline)");
    // Pin the expected reference shape: [swap, swap, mint, settle_all] inside
    // the unlock — the stream the batch probes recombine.
    let ref_inner = match ref_plan.first() {
        Some(PlanStep::V4Unlock { inner, .. }) if inner.len() == 4 => inner.clone(),
        _ => panic!("reference plan should be a 4-step V4Unlock: {ref_plan:?}"),
    };
    let (s1, s2, mint, _settle) = match (
        ref_inner[0].clone(),
        ref_inner[1].clone(),
        ref_inner[2].clone(),
        ref_inner[3].clone(),
    ) {
        (
            PlanStep::V4Swap { .. },
            PlanStep::V4Swap { .. },
            PlanStep::V4Mint { .. },
            PlanStep::V4SettleAll,
        ) => (
            ref_inner[0].clone(),
            ref_inner[1].clone(),
            ref_inner[2].clone(),
            ref_inner[3].clone(),
        ),
        other => panic!("reference inner shape changed: {other:?}"),
    };
    let ref_payload = {
        let mut p = preamble.clone();
        p.extend(plan_to_bytes(&ref_plan, &at));
        p
    };
    (h, hops, hop_outputs, preamble, s1, s2, mint, ref_payload)
}

fn swap_codec(step: &PlanStep) -> (u8, u8, u16, i16, u8, bool, u128) {
    match step {
        PlanStep::V4Swap {
            c0_idx,
            c1_idx,
            fee,
            tick_spacing,
            hooks_idx,
            zfo,
            amount,
            ..
        } => (
            *c0_idx,
            *c1_idx,
            *fee,
            *tick_spacing,
            *hooks_idx,
            *zfo,
            *amount,
        ),
        _ => panic!("expected a V4Swap step: {step:?}"),
    }
}

#[track_caller]
fn batch_entry_from_swap(step: &PlanStep) -> V4BatchEntry {
    let (c0_idx, c1_idx, fee, tick_spacing, hooks_idx, zfo, amount) = swap_codec(step);
    V4BatchEntry {
        c0_idx,
        c1_idx,
        fee,
        tick_spacing,
        hooks_idx,
        zfo,
        amount_u96: amount,
    }
}

fn mint_codec(step: &PlanStep) -> (u8, u8, u128) {
    match step {
        PlanStep::V4Mint {
            currency_idx,
            recipient_idx,
            amount,
            ..
        } => (*currency_idx, *recipient_idx, *amount),
        _ => panic!("expected a V4Mint step: {step:?}"),
    }
}

struct ProbeResult {
    outcome: ExecOutcome,
    weth_delta: i128,
    erc6909_delta: i128,
    predicted: i128,
}

/// Like `Harness::run_raw_payload` but with an explicit `execute()` `config`
/// (the `check_mode` axis) — `run_raw_payload` hardwires `config=0`.
fn run_raw_config(
    h: &mut Harness,
    hops: &[Hop],
    hop_outputs: &[u128],
    payload: &[u8],
    config: U256,
) -> Result<ProbeResult, String> {
    let predicted = i128::try_from(*hop_outputs.last().unwrap_or(&INPUT)).unwrap()
        - i128::try_from(INPUT).unwrap();

    h.fund(h.weth, h.executor, INPUT * 2)?;
    for (i, hop) in hops.iter().enumerate().skip(1) {
        if hop.src != h.weth && hop.src != Address::ZERO {
            h.fund(hop.src, h.executor, hop_outputs[i - 1] * 2)?;
        }
    }
    for hop in hops {
        if let HopPool::V2(p) = &hop.pool {
            h.executor_approve_pair(*p)?;
        }
    }

    let before_weth = h.balance_of(h.weth, h.executor)?.to::<u128>();
    let before_erc = h.pm_balance_of(h.executor, h.weth)?.to::<u128>();
    let outcome = h.execute_payload_config(payload, GAS, config)?;
    let after_weth = h.balance_of(h.weth, h.executor)?.to::<u128>();
    let after_erc = h.pm_balance_of(h.executor, h.weth)?.to::<u128>();
    Ok(ProbeResult {
        outcome,
        weth_delta: i128::try_from(after_weth).unwrap() - i128::try_from(before_weth).unwrap(),
        erc6909_delta: i128::try_from(after_erc).unwrap() - i128::try_from(before_erc).unwrap(),
        predicted,
    })
}

fn tolerance(predicted: i128) -> i128 {
    (predicted.abs() / 1000).max(64)
}

/// `(A)` guard: the non-batch capture stream is the pre-flip baseline — the
/// batch probes must not move it.
#[test]
fn reference_nonbatch_capture_stream_still_executes_with_mode2_floor() {
    let (mut h, hops, hop_outputs, _preamble, _s1, _s2, _mint, ref_payload) = probe_setup();
    // The oracle probe's regime pin (SMOZG3 open question 1): zero pre-held
    // ERC6909 position — the capture is a fresh mint of the surplus.
    assert_eq!(
        h.pm_balance_of(h.executor, h.weth).unwrap(),
        U256::ZERO,
        "fixture starts from a zero ERC6909 position"
    );
    let res = run_raw_config(&mut h, &hops, &hop_outputs, &ref_payload, U256::from(2u32))
        .unwrap_or_else(|e| panic!("[reference] run: {e}"));
    let tol = tolerance(res.predicted);
    assert!(
        res.outcome.executed(2),
        "[reference] non-batch capture must execute: {:?}",
        res.outcome
    );
    assert!(
        res.erc6909_delta > 0 && (res.erc6909_delta - res.predicted).abs() <= tol,
        "[reference] ERC6909 capture delta {} vs predicted {} (tol {})",
        res.erc6909_delta,
        res.predicted,
        tol
    );
    assert!(
        res.weth_delta <= tol,
        "[reference] custody WETH delta {} must not carry the profit (tol {})",
        res.weth_delta,
        tol
    );
    println!(
        "── reference non-batch capture: erc6909_delta={} predicted={} ── ok",
        res.erc6909_delta, res.predicted
    );
}

/// `(A)` an open-weth batch (0x43) + trailing WETH mint executes with the
/// `check_mode=2` floor armed and lands the profit in the ERC6909 vault.
#[test]
fn probe_open_weth_batch_plus_mint_executes_with_mode2_floor_and_capture() {
    let (mut h, hops, hop_outputs, preamble, s1, s2, mint, _ref) = probe_setup();
    let entries = [batch_entry_from_swap(&s1), batch_entry_from_swap(&s2)];
    let (currency_idx, recipient_idx, amount) = mint_codec(&mint);

    // The 0x43 variant: identical layout to 0x42, one command byte — the
    // production encoder gains this variant with the SW42JA flip; these
    // probes pin the artifact first, so the byte is patched in test space.
    let mut batch = enc_v4_batch(&entries).unwrap();
    assert_eq!(batch[0], 0x42);
    batch[0] = 0x43;

    let mut inner = Vec::new();
    inner.extend_from_slice(&batch);
    inner.extend_from_slice(&enc_v4_mint_compact(currency_idx, recipient_idx, amount).unwrap());
    inner.extend_from_slice(&enc_v4_settle_all());
    let mut payload = preamble.clone();
    payload.extend(enc_v4_unlock(&inner).unwrap());

    let res = run_raw_config(&mut h, &hops, &hop_outputs, &payload, U256::from(2u32))
        .unwrap_or_else(|e| panic!("[A] run: {e}"));
    let tol = tolerance(res.predicted);
    assert!(
        res.outcome.executed(2),
        "[A] open-weth batch + mint must execute: {:?}",
        res.outcome
    );
    assert!(
        res.erc6909_delta > 0 && (res.erc6909_delta - res.predicted).abs() <= tol,
        "[A] expected the open batch to feed the mint: delta {} vs predicted {} (tol {})",
        res.erc6909_delta,
        res.predicted,
        tol
    );
    assert!(
        res.weth_delta <= tol,
        "[A] custody WETH delta {} must not carry the profit (tol {})",
        res.weth_delta,
        tol
    );
    println!(
        "── (A) 0x43 open batch + mint: erc6909_delta={} predicted={} ── ok",
        res.erc6909_delta, res.predicted
    );
}

/// `(B)` the legacy full-settle batch (0x42) + trailing mint reverts with the
/// NAMED `InsufficientMintDelta` error — the batch tail-settle took the WETH
/// delta into custody, so the mint finds no live delta (SMOZG3's opaque PM
/// `D0` is gone).
#[test]
fn probe_full_settle_batch_plus_mint_reverts_with_named_error() {
    let (mut h, hops, hop_outputs, preamble, s1, s2, mint, _ref) = probe_setup();
    let entries = [batch_entry_from_swap(&s1), batch_entry_from_swap(&s2)];
    let (currency_idx, recipient_idx, amount) = mint_codec(&mint);

    let mut inner = Vec::new();
    inner.extend_from_slice(&enc_v4_batch(&entries).unwrap()); // 0x42, tail-settles WETH
    inner.extend_from_slice(&enc_v4_mint_compact(currency_idx, recipient_idx, amount).unwrap());
    inner.extend_from_slice(&enc_v4_settle_all());
    let mut payload = preamble.clone();
    payload.extend(enc_v4_unlock(&inner).unwrap());

    let res = run_raw_config(&mut h, &hops, &hop_outputs, &payload, U256::from(2u32))
        .unwrap_or_else(|e| panic!("[B] run: {e}"));
    let ExecOutcome::Reverted { raw, .. } = &res.outcome else {
        panic!("[B] expected a revert, got {:?}", res.outcome);
    };
    let sel: [u8; 4] = keccak256(b"InsufficientMintDelta(uint256,uint256)").0[..4]
        .try_into()
        .unwrap();
    assert!(
        raw.starts_with(&sel),
        "[B] expected the named InsufficientMintDelta selector {sel:02x?}, got raw {:02x?}",
        &raw[..raw.len().min(256)]
    );
    println!("── (B) 0x42 batch + mint reverts with named InsufficientMintDelta ── ok");
}

/// `(C)` the hazard: an open-weth batch (0x43) with NO follow-up mint leaves
/// the positive WETH delta unsettled, so the stub PoolManager reverts at
/// unlock with Error(string) "DELTA" — the stub's model of v4-core's
/// `CurrencyNotSettled` unlock check.
/// The encoder's ledger pairing rule (TGUZCT item 4) makes this stream
/// unrepresentable; this probe pins the on-chain consequence it protects.
#[test]
fn probe_open_batch_without_mint_reverts_unsettled_at_unlock() {
    let (mut h, hops, hop_outputs, preamble, s1, s2, _mint, _ref) = probe_setup();
    let entries = [batch_entry_from_swap(&s1), batch_entry_from_swap(&s2)];

    let mut inner = Vec::new();
    let mut batch = enc_v4_batch(&entries).unwrap();
    batch[0] = 0x43;
    inner.extend_from_slice(&batch); // open batch, nothing else inside the unlock
    let mut payload = preamble.clone();
    payload.extend(enc_v4_unlock(&inner).unwrap());

    let res = run_raw_config(&mut h, &hops, &hop_outputs, &payload, U256::ZERO)
        .unwrap_or_else(|e| panic!("[C] run: {e}"));
    let ExecOutcome::Reverted { raw, .. } = &res.outcome else {
        panic!("[C] expected a revert, got {:?}", res.outcome);
    };
    // The stub models v4-core's unlock `CurrencyNotSettled` check with an
    // Error(string) revert of "DELTA" whenever a nonzero PM delta survives
    // unlock exit — assert the exact ABI encoding.
    let mut expected = keccak256(b"Error(string)").0[..4].to_vec();
    expected.extend_from_slice(&U256::from(0x20u64).to_be_bytes::<32>());
    expected.extend_from_slice(&U256::from(5u64).to_be_bytes::<32>());
    let mut word = [0u8; 32];
    word[..5].copy_from_slice(b"DELTA");
    expected.extend_from_slice(&word);
    assert!(
        raw.starts_with(&expected),
        "[C] expected the stub _checkDelta DELTA revert, got raw {raw:?}"
    );
    println!("── (C) 0x43 open batch without mint reverts DELTA at unlock ── ok");
}
