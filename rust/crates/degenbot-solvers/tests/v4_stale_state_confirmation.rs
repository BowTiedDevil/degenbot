#![allow(
    clippy::uninlined_format_args,
    clippy::doc_markdown,
    clippy::doc_lazy_continuation
)]
//! Decisive root-cause confirmation for ergo `RZKFKR` — the V4 hop
//! `CurrencyNotSettled` divergence is **NOT stale state** (RZKFKR's premise)
//! and **NOT a solver-math bug** (W2UWZO exonerated that): it is the **V4
//! protocol fee** that `v4_simulate_swap` / `compute_swap_step_v4` and the
//! solver's `IntV3TickRangeSequence` all OMIT. PoolManager applies
//! `swapFee = protocolFee + lpFee - (protocolFee*lpFee/1e6)`, which is HIGHER
//! than the lpFee alone when `protocolFee > 0`; the Rust swap-step twin uses
//! only `lpFee` → over-predicts the output → settlement overdraft.
//!
//! ## The fixture (captured `logs/debug/v4_fixture_block_25635461.md`)
//!
//! V4 pool USDC/WETH 0.3% (poolManager `0x…444c…08A90`, pool_id `0xb9fd…857b`),
//! block 25635461, path=97: amount_in = 13_576_418_983_678 wei WETH (ofz),
//! on-chain actual_out = 25_885 USDC, solver predicted = 25_898 (off by +13).
//!
//! W2UWZO proved `solver_crossing_path == v4_simulate_swap` for identical
//! state — which is consistent with this finding (BOTH omit the protocol fee,
//! so they agree with each other while BOTH diverge from real bytecode). The
//! earlier doc's claim "the sim runs `v4_simulate_swap` and captures the
//! CORRECT on-chain amount" conflated the revm sim (real PoolManager bytecode,
//! applies the protocol fee → 25_885) with `v4_simulate_swap` (Rust twin, omits
//! it → 25_898). They are DIFFERENT values.
//!
//! ## On-chain state at block 25635461 (read via `cast storage` against
//! `ETHEREUM_ARCHIVE_NODE_HTTP_URI`; slot derivation per
//! `docs/architecture/v4_poolmanager_storage_layout.md`)
//!
//! - slot0.sqrtPriceX96 = 1_809_847_926_502_557_434_949_706_007_283_582
//! - slot0.tick         = 200_738
//! - slot0.protocolFee  = 0x1f41f4 → `getOneForZeroFee` = 500 pips (WETH side)
//! - liquidity           = 379_577_542_030
//! - tick 216_420: gross 347_841_283_144 / net -347_841_283_144
//! - tick 228_780: gross 31_736_257_617  / net -31_736_257_617
//! - PoolKey.fee (lpFee) = 3000 (0.3%), tick_spacing 60, hooks = address(0)
//!
//! Scanning blocks 25635459..25635463 confirms the scalars are byte-identical
//! across the failure window — NOT a block-timing / stale-state artifact.
//!
//! ## The quantitative proof
//!
//! `calculateSwapFee(protocolFee=500, lpFee=3000) = 500 + 3000 - (500*3000/1e6)
//! = 3499` pips (the fee PoolManager's `computeSwapStep` actually charges).
//! Asserting `v4_simulate_swap(state, fee=3000) == 25_898` (the prediction)
//! AND `v4_simulate_swap(state, fee=3499) == 25_885` (the on-chain actual)
//! pins it: the ONLY difference between the prediction and the actual is the
//! protocol fee the Rust twin omits.
//!
//! ## Why W2UWZO's parity test passed despite this
//!
//! W2UWZO compared the solver's crossing path against `v4_simulate_swap` (both
//! Rust, both omit the protocol fee) — so they agree byte-for-byte. Neither
//! was compared against real PoolManager bytecode in that test. The
//! `compute_swap_step_v3` oracle used there is the V3 step math (which has no
//! protocol-fee-in-swap concept; V3 applies protocol fees differently), so the
//! V4 protocol-fee omission was invisible to it.

use std::collections::HashMap;

use alloy::primitives::{I256, U128, U256};

use degenbot_pools::v3_state::PoolTickCoverage;
use degenbot_pools::v4_state::{v4_simulate_swap, RegisterV4PoolParams, V4PoolKey, V4PoolState};
use degenbot_pools::TickInfo;

/// The on-chain actual + the solver's stale-feeling prediction for path=97.
const ONCHAIN_ACTUAL_OUT: u128 = 25_885;
const SOLVER_PREDICTED_OUT: u128 = 25_898;
const PATH97_AMOUNT_IN_WETH: u128 = 13_576_418_983_678;

/// PoolManager PoolKey.fee for this pool (the static LP fee).
const LP_FEE: u32 = 3_000;
/// `calculateSwapFee(protocolFee=500, lpFee=3000)` — the fee PoolManager's
/// `computeSwapStep` actually charges (protocol fee + LP fee combined).
const EFFECTIVE_SWAP_FEE: u32 = 3_499;
const TICK_SPACING: i32 = 60;

/// Build the V4 pool state from the on-chain scalars + tick_data read at
/// block 25635461.
fn onchain_v4_state_at_block_25635461() -> V4PoolState {
    let sqrt_price_x96 = U256::from(1_809_847_926_502_557_434_949_706_007_283_582u128);
    let liquidity = 379_577_542_030u128;
    let tick = 200_738;

    let mut tick_data: HashMap<i32, TickInfo> = HashMap::new();
    let mk = |gross: u128, net: i128| TickInfo {
        liquidity_gross: U256::from(gross).to::<U128>(),
        liquidity_net: I256::try_from(net).unwrap(),
        block: 0,
    };
    tick_data.insert(216_420, mk(347_841_283_144, -347_841_283_144));
    tick_data.insert(228_780, mk(31_736_257_617, -31_736_257_617));

    let params = RegisterV4PoolParams {
        pool_manager: alloy::primitives::Address::ZERO,
        pool_id: [0u8; 32],
        pool_key: V4PoolKey {
            currency0: alloy::primitives::Address::ZERO,
            currency1: alloy::primitives::Address::ZERO,
            fee: LP_FEE,
            tick_spacing: TICK_SPACING,
            hooks: alloy::primitives::Address::ZERO,
        },
        hook_flags: 0,
        sqrt_price_x96,
        liquidity,
        tick,
        tick_data,
        update_block: 0,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
    };
    let (_identity, state) = V4PoolState::from_params(params, 8);
    state
}

/// Run `v4_simulate_swap` (the byte-exact full-tick-walk) with a given fee
/// and return the ofz exact-in token-OUT (amount0).
fn sim_out_with_fee(state: &V4PoolState, fee: u32, amount_in: U256) -> U256 {
    let amount_specified = I256::try_from(amount_in)
        .ok()
        .and_then(|v| I256::ZERO.checked_sub(v))
        .unwrap();
    let outcome = v4_simulate_swap(
        state,
        fee,
        TICK_SPACING,
        false, // ofz: WETH→USDC, price goes up
        amount_specified,
        U256::from(degenbot_cl_math::cl_lib::tick_math::MAX_SQRT_RATIO),
    )
    .expect("v4_simulate_swap succeeds on the on-chain state");
    outcome.amount0 // ofz exact-in: amount0 is the output (caller receives token0=USDC)
}

#[test]
fn v4_protocol_fee_omission_reproduces_the_divergence() {
    let state = onchain_v4_state_at_block_25635461();
    let amount_in = U256::from(PATH97_AMOUNT_IN_WETH);

    // (1) The Rust twin with the LP fee ALONE (what the solver + sim twin use)
    //     reproduces the SOLVER'S PREDICTION — the over-prediction.
    let out_lp_fee_only = sim_out_with_fee(&state, LP_FEE, amount_in);
    assert_eq!(
        out_lp_fee_only,
        U256::from(SOLVER_PREDICTED_OUT),
        "v4_simulate_swap with lpFee={} must reproduce the solver's predicted {} (the \
         over-prediction); got {out_lp_fee_only}",
        LP_FEE,
        SOLVER_PREDICTED_OUT,
    );

    // (2) The Rust twin with the EFFECTIVE swap fee (lpFee + protocolFee, as
    //     PoolManager's computeSwapStep actually charges) reproduces the
    //     ON-CHAIN ACTUAL. The ONLY difference between (1) and (2) is the
    //     protocol fee → it IS the root cause.
    let out_effective_fee = sim_out_with_fee(&state, EFFECTIVE_SWAP_FEE, amount_in);
    assert_eq!(
        out_effective_fee,
        U256::from(ONCHAIN_ACTUAL_OUT),
        "v4_simulate_swap with effective swapFee={} (lpFee + protocolFee) must reproduce the \
         on-chain actual {}; got {out_effective_fee} — if not, the protocol-fee hypothesis is \
         refuted",
        EFFECTIVE_SWAP_FEE,
        ONCHAIN_ACTUAL_OUT,
    );

    assert_ne!(
        out_lp_fee_only, out_effective_fee,
        "lpFee-only and effective-swapFee outputs must differ (else the protocol fee has no effect)",
    );

    eprintln!(
        "[protocol-fee-confirmed] amount_in={amount_in} \
         lpFee_out={out_lp_fee_only} (== solver predicted {SOLVER_PREDICTED_OUT}) \
         swapFee_out={out_effective_fee} (== on-chain actual {ONCHAIN_ACTUAL_OUT}) \
         => root cause = V4 protocol fee omitted by v4_simulate_swap + solver crossing path"
    );
}
