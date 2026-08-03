#![allow(
    clippy::uninlined_format_args,
    clippy::doc_markdown,
    clippy::doc_lazy_continuation
)]
//! Decisive root-cause confirmation + fix guard for ergo `BZBOLL` — the V4
//! `CurrencyNotSettled` divergence is the V4 protocol fee. `RZKFKR` pinned
//! it (offline replay of the path=97 fixture); this test guards the FIX
//! (`calculate_swap_fee` threaded into `v4_simulate_swap` +
//! `build_int_v4_sequence` via `V4PoolState.protocol_fee`).
//!
//! ## The fixture (captured `logs/debug/v4_fixture_block_25635461.md`)
//!
//! V4 pool USDC/WETH 0.3% (poolManager `0x…444c…08A90`, pool_id `0xb9fd…857b`),
//! block 25635461, path=97: amount_in = 13_576_418_983_678 wei WETH (ofz),
//! on-chain actual_out = 25_885 USDC, solver predicted = 25_898 (off by +13).
//!
//! ## What this test proves (post-fix)
//!
//! With `protocol_fee = 0x001f_41f4` stored on `V4PoolState` (the packed uint24
//! read from `slot0.protocolFee` at registration), `v4_simulate_swap` AND the
//! solver's crossing path (`compute_crossing` + `int_simulate_v3_swap`) both
//! internally compute `calculateSwapFee(500, 3000) = 3499` pips and feed it as
//! the swap-step fee. The result must be the ON-CHAIN ACTUAL (25_885), not the
//! stale-feeling lpFee-only prediction (25_898). The two pre-fix sides —
//! solver vs `v4_simulate_swap` — still agree byte-for-byte (W2UWZO parity),
//! now against the REAL bytecode answer.
//!
//! A `protocol_fee = 0` variant reproduces the OLD prediction (25_898) — the
//! pre-fix behaviour — so the test pins BOTH sides of the protocol-fee coin
//! in one place.
//!
//! ## On-chain state at block 25635461 (read via `cast storage` against
//! `ETHEREUM_ARCHIVE_NODE_HTTP_URI`; slot derivation per
//! `docs/architecture/v4_poolmanager_storage_layout.md`)
//!
//! - slot0.sqrtPriceX96 = 1_809_847_926_502_557_434_949_706_007_283_582
//! - slot0.tick         = 200_738
//! - slot0.protocolFee  = 0x001f_41f4 → `getOneForZeroFee` = 500 pips (WETH side)
//! - liquidity           = 379_577_542_030
//! - tick 216_420: gross 347_841_283_144 / net -347_841_283_144
//! - tick 228_780: gross 31_736_257_617  / net -31_736_257_617
//! - PoolKey.fee (lpFee) = 3000 (0.3%), tick_spacing 60, hooks = address(0)
//!
//! Scanning blocks 25635459..25635463 confirms the scalars are byte-identical
//! across the failure window — NOT stale state.

use std::collections::HashMap;

use alloy::primitives::{I256, U128, U256};

use degenbot_pools::v3_state::PoolTickCoverage;
use degenbot_pools::v4_state::{v4_simulate_swap, RegisterV4PoolParams, V4PoolKey, V4PoolState};
use degenbot_pools::TickInfo;

use degenbot_solvers::mobius_v3_int::{int_simulate_v3_swap, IntV3TickRangeSequence};

/// The on-chain actual + the pre-fix lpFee-only prediction for path=97.
const ONCHAIN_ACTUAL_OUT: u128 = 25_885;
const PRE_FIX_LP_FEE_PREDICTION: u128 = 25_898;
const PATH97_AMOUNT_IN_WETH: u128 = 13_576_418_983_678;

/// PoolManager `slot0.protocolFee` at block 25635461 — packed uint24,
/// `0x001f_41f4`, both direction fees 500 pips.
const ONCHAIN_PROTOCOL_FEE_PACKED: u32 = 0x001f_41f4;

/// PoolManager PoolKey.fee for this pool (the static LP fee).
const LP_FEE: u32 = 3_000;
const TICK_SPACING: i32 = 60;

/// Build the V4 pool state from the on-chain scalars + tick_data read at
/// block 25635461, with the given `protocol_fee` (packed uint24).
fn onchain_v4_state_at_block_25635461(protocol_fee: u32) -> V4PoolState {
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
        protocol_fee,
        sqrt_price_x96,
        liquidity,
        tick,
        tick_data,
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
    };
    let (_identity, state) = V4PoolState::from_params(params, 8);
    state
}

/// Run `v4_simulate_swap` (the byte-exact full-tick-walk) with the pool's
/// `protocol_fee` set on the state (so `swap_fee = calculate_swap_fee`) and
/// return the ofz exact-in token-OUT (amount0).
fn sim_out_with_protocol_fee(state: &V4PoolState, amount_in: U256) -> U256 {
    let amount_specified = I256::try_from(amount_in)
        .ok()
        .and_then(|v| I256::ZERO.checked_sub(v))
        .unwrap();
    let outcome = v4_simulate_swap(
        state,
        LP_FEE,
        TICK_SPACING,
        false, // ofz: WETH→USDC, price goes up
        amount_specified,
        U256::from(degenbot_cl_math::cl_lib::tick_math::MAX_SQRT_RATIO),
    )
    .expect("v4_simulate_swap succeeds on the on-chain state");
    outcome.amount0 // ofz exact-in: amount0 is the output (caller receives token0=USDC)
}

/// The solver's CL-hop output for `amount_in` against a pre-built sequence —
/// the exact assembly `int_simulate_mixed_path_n` uses for a CL hop's
/// `hop_outputs[i]`.
fn solver_crossing_output(amount_in: U256, seq: &IntV3TickRangeSequence) -> Option<U256> {
    let n = seq.ranges.len();
    let mut chosen_k = 0usize;
    for k in 0..n {
        let crossing = seq.compute_crossing(k)?;
        if crossing.crossing_gross_input <= amount_in {
            chosen_k = k;
        } else {
            break;
        }
    }
    let crossing = seq.compute_crossing(chosen_k)?;
    if amount_in < crossing.crossing_gross_input {
        return Some(U256::ZERO);
    }
    let remaining = amount_in - crossing.crossing_gross_input;
    let ending = int_simulate_v3_swap(remaining, &crossing.ending_range);
    Some(crossing.crossing_output.saturating_add(ending.output))
}

#[test]
fn v4_protocol_fee_threading_reproduces_on_chain_actual() {
    let amount_in = U256::from(PATH97_AMOUNT_IN_WETH);

    // (1) With the on-chain `slot0.protocolFee` (0x001f_41f4 → 500 pips ofz),
    // BOTH v4_simulate_swap AND the solver's crossing path reproduce the
    // ON-CHAIN ACTUAL — the fix's effect.
    let state = onchain_v4_state_at_block_25635461(ONCHAIN_PROTOCOL_FEE_PACKED);
    let sim_out = sim_out_with_protocol_fee(&state, amount_in);
    let seq = state
        .build_int_v4_sequence(TICK_SPACING, LP_FEE, false, 10)
        .expect("on-chain state builds a tick-range sequence");
    let solver_out =
        solver_crossing_output(amount_in, &seq).expect("solver produces a crossing output");

    assert_eq!(
        sim_out, solver_out,
        "v4_simulate_swap and the solver crossing path must agree (W2UWZO parity holds post-fix); \
         sim={sim_out} solver={solver_out}",
    );
    assert_eq!(
        sim_out,
        U256::from(ONCHAIN_ACTUAL_OUT),
        "with the on-chain protocol_fee, v4_simulate_swap must reproduce the on-chain actual \
         ({ONCHAIN_ACTUAL_OUT}); got {sim_out} (the protocol-fee threading regressed)",
    );

    // (2) `protocol_fee = 0` reproduces the PRE-FIX lpFee-only prediction —
    // the over-prediction that caused `CurrencyNotSettled`. Pins the
    // pre-fix→post-fix delta at the protocol fee and guards against
    // accidentally defaulting `protocol_fee` back to 0 on the production
    // registration path.
    let state_no_proto = onchain_v4_state_at_block_25635461(0);
    let sim_out_no_proto = sim_out_with_protocol_fee(&state_no_proto, amount_in);
    assert_eq!(
        sim_out_no_proto,
        U256::from(PRE_FIX_LP_FEE_PREDICTION),
        "protocol_fee=0 must reproduce the pre-fix over-prediction \
         ({PRE_FIX_LP_FEE_PREDICTION}); got {sim_out_no_proto}",
    );
    assert_ne!(
        sim_out, sim_out_no_proto,
        "with vs without protocol_fee must differ (else the threading is a no-op)",
    );

    eprintln!(
        "[protocol-fee-fix-confirmed] amount_in={amount_in} \
         swapFee_out={sim_out} (== on-chain actual {ONCHAIN_ACTUAL_OUT}) \
         lpFee_out={sim_out_no_proto} (== pre-fix prediction {PRE_FIX_LP_FEE_PREDICTION}) \
         => BZBOLL fix threads calculateSwapFee into both v4_simulate_swap + solver"
    );
}
