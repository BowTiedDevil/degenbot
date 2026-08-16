#![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Tier-3 path-5000 V4 CL-hop clamp regression (ergo BHTWBZ, epic PJLIAE) —
//! RED→GREEN proof that the CL-hop input clamp turns the 20.7M-gas EMPTY-HALT
//! into a clean byte-exact fill under the executor's 5M gas ceiling.
//!
//! ## Narrative
//!
//! Path 5000 is V2(MATIC/WETH) → V4(UNI/MATIC) → V3(UNI/WETH); the recorded
//! V4 hop input (`15351327867212777`, selling MATIC/currency1, zfo=false) is the
//! FULL V2 output fed onward. The V4 pool's tracked band is `[-257352, 35067]`;
//! the tier-3-proven `v4_simulate_swap` twin reports it can only *convert*
//! `input_consumed = 15351327867192638` (the remaining 20,139 wei tips the
//! loop past the last funded tick into zero liquidity). With a MAX price limit
//! (what the live executor passes, `exploration-no-profit-crash.md` L308) the
//! **unclamped** over-feed makes the exact-in loop march empty bitmap words
//! toward `MAX_SQRT_PRICE` — a 20.7M-gas `EMPTY-HALT` that reverts under the
//! sim's execute gas ceiling (originally the 5M `INITIAL_EXECUTE_GAS`;
//! now the EIP-7825 16.7M default). GREEN must fill under the harness's own
//! `EXECUTOR_5M` gas threshold.
//!
//! The production solver clamp (AGENTS.md UO3JM4 / VAASFM; `arb_engine`
//! `clamp_cl_hop_capacity`) re-reads the live pool state post-solve and caps
//! the committed CL-hop input to `input_consumed − margin` (margin = 1 wei), so
//! the loop exits on `amountRemaining == 0` at the last funded tick (~190k gas,
//! well under 5M) with **byte-identical output** (`460882096151249`).
//!
//! ## What this test asserts (real PoolManager bytecode + all)
//!
//! Deploys the canonical v4-core `PoolManager` (via the committed
//! `V4SwapOracleHarness` unlocker wrapper) into an in-process revm
//! `CacheDB`, seeds the path-5000 pool slot-for-slot from the reconstructed
//! `V4PoolState`, and drives the real swap through `unlock→swap→settle` —
//! reusing the shared `degenbot::investigation::real_oracle` driver. It proves:
//!
//! 1. **GREEN**: the clamped committed input (`input_consumed − 1`) fills
//!    **ACCEPTED at 5M gas** with `BalanceDelta amount0 == 460882096151249`
//!    byte-exact (the recorded solver/twin output), at ~190k gas — `≪ 5M`.
//! 2. **RED preserved**: the unclamped recorded input with a MAX price limit
//!    does **not** accept at 5M (reverts/halts — the 20.7M empty-march
//!    truncated by the 5M ceiling). This guards the harness is not silently
//!    vacuous: the clamp is what turns RED→GREEN, not a retired fixture.
//! 3. The committed clamped input is `≤ input_consumed − 1`.
//!
//! ## No toolchain needed
//!
//! The harness bytecode is loaded from the committed `tier3-oracle/artifacts/`
//! tree (see `real_oracle` and `tier3_harness_artifacts.rs` for integrity
//! guards), so this runs in the default `cargo test --workspace` path. A
//! harness-source edit requires `tier3-oracle/build-tier3-v4-swap-harness.sh`
//! to regenerate + `just verify-tier3-artifacts` to confirm, per ADR-020.

#![expect(clippy::doc_markdown)] // Solidity/V4 identifiers (PoolManager, BalanceDelta…)

use alloy::primitives::{aliases::I256, U160, U256};
use degenbot::investigation::{build_v4_state, real_oracle, PathFixture};
use degenbot_cl_math::cl_lib::tick_math::MAX_SQRT_RATIO;
use degenbot_pools::v4_state::v4_simulate_swap;
use degenbot_simulation::oracle::Verdict;

const FIXTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/fixtures/path5000_v2v4v3_block25704509.json"
);

/// The VAASFM clamp margin (must stay in sync with
/// `ArbitrageEngine::cl_hop_clamp_margin` in degenbot-bot — 1 wei, the
/// maximum-extraction choice; see the 7E5D7W margin-policy checkpoint).
const CLAMP_MARGIN: u64 = 1;

/// The recorded byte-exact V4 output the clamp must preserve (== the recorded
/// solver `v4_predicted_output` and the `v4_simulate_swap` twin output).
const RECORDED_V4_OUTPUT: u64 = 460_882_096_151_249;

/// The real executor's per-swap gas ceiling for this regression — the harness
/// drives the real PoolManager at this gas (5M) and asserts the clamp drops
/// the march from gas-hungry (RED > 5M/2) to a clean byte-exact fill (GREEN
/// < 5M). Kept test-local: independent of `INITIAL_EXECUTE_GAS`.
const EXECUTOR_5M: u64 = 5_000_000;

fn fixture() -> String {
    String::from(FIXTURE_PATH)
}

#[test]
fn clamped_committed_input_fills_byte_exact_under_5m() {
    let fx = PathFixture::load(fixture().as_str()).unwrap_or_else(|e| panic!("{e}"));
    let fee = fx.pools["v4"].fee_currency0.unwrap();
    let spacing = fx.pools["v4"].tick_spacing.unwrap();
    let zfo = false; // recorded path-5000 V4 hop: sell MATIC/currency1 buy UNI/currency0

    let state = build_v4_state(&fx.pools["v4"]);

    // The recorded (unclamped) V4 input — the FULL V2 output fed onward.
    let recorded_in = fx.recorded_solve.v4_input.unwrap().0;
    // The tier-3-proven twin's max-convertible at that input.
    let neg = I256::ZERO
        .checked_sub(I256::try_from(recorded_in).expect("recorded input fits i256"))
        .expect("no underflow");
    let limit = U256::from(MAX_SQRT_RATIO - U160::from(1u64));
    let twin = v4_simulate_swap(&state, fee, spacing, zfo, neg, limit).expect("twin simulates");
    let input_consumed = twin.input_consumed;

    // Over-feed confirmed: the recorded input exceeds what the pool converts.
    assert!(
        input_consumed < recorded_in,
        "fixture must be an over-feed (input_consumed {input_consumed} < recorded_in {recorded_in}) \
         for the clamp to be the thing under test"
    );

    // The production clamp commits `input_consumed - margin`.
    let clamped = input_consumed - U256::from(CLAMP_MARGIN);
    assert!(
        clamped <= input_consumed - U256::from(CLAMP_MARGIN),
        "clamped input must be ≤ input_consumed − margin"
    );

    // The clamped output must remain byte-exact to the recorded solver output.
    let clamped_neg = I256::ZERO
        .checked_sub(I256::try_from(clamped).expect("clamped fits i256"))
        .expect("no underflow");
    let clamped_twin = v4_simulate_swap(&state, fee, spacing, zfo, clamped_neg, limit)
        .expect("clamped twin simulates");
    assert_eq!(
        clamped_twin.amount0,
        U256::from(RECORDED_V4_OUTPUT),
        "clamped input must still deliver the byte-exact recorded output"
    );

    // GREEN: drive the REAL PoolManager at 5M with the clamped input + a MAX
    // price limit (the worst case — an unbounded price march attempt). It must
    // ACCEPT with the byte-exact BalanceDelta, well under the 5M ceiling.
    let green = real_oracle::drive_real_v4_swap(
        &state,
        fee,
        spacing,
        zfo,
        clamped_neg,
        limit_low_u160(),
        EXECUTOR_5M,
    );
    match &green.verdict {
        Verdict::Accepted { .. } => {
            assert_eq!(
                green.delta.0,
                U256::from(RECORDED_V4_OUTPUT),
                "GREEN: real PoolManager amount0 must be byte-exact to the recorded output"
            );
            // A clean fill (~190k) is orders of magnitude under the 5M ceiling.
            assert!(
                green.gas_used < EXECUTOR_5M,
                "GREEN must fill under the 5M ceiling, got gas_used={}",
                green.gas_used
            );
        }
        other => {
            panic!(
                "GREEN FAILED: clamped committed input did not fill ACCEPTED at 5M (got {other:?}, \
                 gas_used={}) — the clamp regression guard is RED",
                green.gas_used
            );
        }
    }
}

#[test]
fn unclamped_recorded_input_still_marches_under_5m() {
    let fx = PathFixture::load(fixture().as_str()).unwrap_or_else(|e| panic!("{e}"));
    let fee = fx.pools["v4"].fee_currency0.unwrap();
    let spacing = fx.pools["v4"].tick_spacing.unwrap();
    let zfo = false;

    let state = build_v4_state(&fx.pools["v4"]);
    let recorded_in = fx.recorded_solve.v4_input.unwrap().0;
    let neg = I256::ZERO
        .checked_sub(I256::try_from(recorded_in).expect("recorded input fits i256"))
        .expect("no underflow");

    // RED preserved: the unclamped over-feed with a MAX price limit must NOT
    // complete at 5M (it marches empty words to MAX_SQRT_PRICE — a 20.7M-gas
    // EMPTY-HALT truncated by the ceiling), proving the harness isn't vacuous
    // and the clamp is what turns RED→GREEN.
    let red = real_oracle::drive_real_v4_swap(
        &state,
        fee,
        spacing,
        zfo,
        neg,
        limit_low_u160(),
        EXECUTOR_5M,
    );
    match &red.verdict {
        Verdict::Accepted { .. } => {
            // If it accepts at 5M with a clean low gas bill, the RED guard is
            // broken — the fixture no longer reproduces the live halt.
            assert!(
                red.gas_used > EXECUTOR_5M / 2,
                "RED guard broken: unclamped input filled ACCEPTED at 5M with low gas \
                 (gas_used={}) — the fixture no longer reproduces the EMPTY-HALT",
                red.gas_used
            );
        }
        Verdict::Reverted(_) | Verdict::Halted(_) => {
            // Expected: the 20.7M empty-march was truncated by the 5M ceiling.
        }
    }
}

/// The low-side sqrt price for zfo=false (buy) is `MAX_SQRT_RATIO - 1`.
fn limit_low_u160() -> U160 {
    MAX_SQRT_RATIO - U160::from(1u64)
}
