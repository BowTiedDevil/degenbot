#![expect(clippy::unwrap_used, clippy::expect_used)]
//! Decisive offline experiment for ergo task `E7ALWT` — resolves the
//! "V3 solver-math vs. stale engine state" fork for the post-block-25647669
//! IIA `+13` V3-hop over-prediction WITHOUT a live mainnet run.
//!
//! ## What this proves
//!
//! The live mainnet trap (`DEGENBOT_SIM_EXIT_ON_FAIL=1`) captured a V3-V3-V3
//! path where hop[1] (UNI/DAI fee=500) over-predicted its output by 13 wei:
//! `solver hop1 = 150836781515`, `sim actual = 150836781502`. The composer
//! (`three_hop_v3_v3_v3`) chains the predicted (higher) amount as hop[2]'s
//! exact-in → the UNI token's `transfer` reverts `"IIA"`.
//!
//! `docs/architecture/sim_v4_swap_step_rounding.md` concludes V4-hop
//! over-prediction is the V4 protocol fee (RZKFKR — DONE) and routinely
//! claims "V3 hops match exactly." This fixture appears to refute that claim.
//! Two residual suspects (mirroring the W2UWZO V4 experiment):
//!
//! 1. **Stale engine state** — the engine `V3PoolState` the solver read lags
//!    the solve-block state the in-process revm sim reads.
//! 2. **A residual in the solver's CL crossing accumulation** —
//!    `IntV3TickRangeSequence::compute_crossing` + `int_simulate_v3_swap`
//!    diverges from V3's stepwise `compute_swap_step_v3` walk on a multi-tick
//!    crossing.
//!
//! This test feeds the **IDENTICAL** synthetic `V3PoolState` to BOTH:
//! - `degenbot_pools::v3_simulate_swap` — the byte-exact full-tick-walk the
//!   sim's `actual` amount mirrors.
//! - the solver's crossing path: `IntV3TickRangeSequence::compute_crossing`
//!   + `int_simulate_v3_swap` for the ending partial step — exactly what the
//!   solver assembles for a CL hop's `hop_outputs[i]`.
//!
//! Because the input state is byte-identical, ANY divergence is pure solver
//! math — hypothesis (2) — and stale state (1) is exonerated by construction.
//! Conversely, byte-exact parity across a liquidity×amount sweep means the
//! on-chain `+13` CANNOT originate in the solver math and must be stale state
//! — pointing the fix at pump ordering (apply block-N V3 events before solving).
//!
//! ## Why this is NOT already proven by `v4_crossing_solver_vs_sim_parity`
//!
//! The V4 parity test proved the solver's crossing accumulator against
//! `v4_simulate_swap` (which uses `compute_swap_step_v4`). V3 hops route
//! through `build_int_v3_sequence` + `compute_swap_step_v3`. The two builders
//! are byte-identical modulo `gamma_numer`
//! (`fee` vs. `calculate_swap_fee(protocol_fee, fee)` — equal only when
//! `protocol_fee == 0`), and the two swap-step functions produce identical
//! `amount_out` in both the target-reachable and partial-fill branches. So
//! parity *should* transfer — but this test PROVES it against the V3-specific
//! path (`build_int_v3_sequence` + `v3_simulate_swap`) rather than by
//! cross-version reasoning. The failing UNI/DAI pool uses fee=500 /
//! tick_spacing=10, a configuration the V4 sweep (fee=3000 / spacing=60)
//! does not exercise, so this adds a configuration-matching corner.
//!
//! ## RESOLUTION (fix landed — ergo E7ALWT)
//!
//! Suspect (2) was CONFIRMED and FIXED. Root cause: `compute_tick_ranges`
//! collapsed interior word-boundary ticks in constant-liquidity runs (to
//! keep `max_ranges` bounded on sparse-tick pools), so the solver modelled
//! a multi-word span as a SINGLE `compute_swap_step_v3` while `v3_simulate_swap`
//! floors `computeSwapStep` at EVERY word boundary → accumulated per-step
//! fee-rounding divergence = the on-chain `+13`. The fix records the collapsed
//! interior boundaries on `V3TickRangeForSolver::interior_boundaries` /
//! `IntV3TickRangeHop::word_boundary_prices` and makes `compute_crossing` +
//! `int_simulate_v3_swap` re-walk them per boundary (per-step flooring parity).
//! `v3_sparse_tick_topology_reproduces_onchain_plus_thirteen_class` is now a
//! GREEN regression guard (was `#[ignore]`d RED).

#![expect(clippy::doc_markdown, clippy::doc_lazy_continuation)]
#![expect(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "tick·spacing products fit i32 in this fixture"
)]

use hashbrown::HashMap;

use alloy::primitives::{B256, I256, U128, U256};

use degenbot_pools::v3_state::{
    v3_simulate_swap, PoolTickCoverage, RegisterV3PoolParams, V3PoolState, V3SwapOutcome,
};
use degenbot_pools::TickInfo;

use degenbot_solvers::mobius_v3_int::{int_simulate_v3_swap, IntV3TickRangeSequence};

/// Sqrt-price limit that lets the walk cross every tick the input can reach
/// (V3 Pool.swap's `sqrtPriceLimit` MIN/MAX bound for the direction).
fn unbounded_limit(zero_for_one: bool) -> U256 {
    V3PoolState::default_sqrt_price_limit(zero_for_one)
}

/// Build a multi-tick V3 pool state at tick 0 (1:1 price), with initialized
/// ticks every `±tick_spacing·i` out to `±(tick_count)` boundaries.
///
/// `liquidity_net` alternates `+L`/`-L` at successive initialized ticks so
/// the active liquidity toggles between `L` and `2L` across ranges — a
/// realistic multi-tick-crossing topology that exercises the
/// `compute_crossing` accumulator across range boundaries. The failing
/// UNI/DAI pool has `fee=500, tick_spacing=10`; the sweep covers that
/// configuration plus the V4-test-canon `fee=3000, tick_spacing=60`.
fn build_multi_tick_v3_state(
    base_liquidity: u128,
    tick_count: usize,
    tick_spacing: i32,
    fee: u32,
    zero_for_one: bool,
) -> V3PoolState {
    let sp_0 = U256::from(1u128) << 96;
    let liq_gross = U256::from(base_liquidity).to::<U128>();
    let net_pos = i128::try_from(base_liquidity).unwrap();
    let net_neg = -net_pos;

    let mut tick_data: HashMap<i32, TickInfo> = HashMap::new();
    // Place initialized ticks at ±tick_spacing·i for i in 1..=tick_count.
    // Alternate the net so active liquidity toggles L ↔ 2L across ranges
    // (zfo crossing applies `liquidity -= net`: net=-L raises L→2L, net=+L
    // lowers 2L→L). Odd i gets -L, even i gets +L — strictly non-zero
    // liquidity in every range (avoids the degenerate zero-liquidity walk
    // that falsely flagged a divergence and is unreachable on real pools).
    for i in 1..=tick_count {
        let tick = if zero_for_one {
            -tick_spacing * i as i32
        } else {
            tick_spacing * i as i32
        };
        let net = if i % 2 == 1 { net_neg } else { net_pos };
        tick_data.insert(
            tick,
            TickInfo {
                liquidity_gross: liq_gross,
                liquidity_net: net,
                block: 0,
            },
        );
    }

    let params = RegisterV3PoolParams {
        address: alloy::primitives::Address::ZERO,
        token0: alloy::primitives::Address::ZERO,
        token1: alloy::primitives::Address::ZERO,
        fee,
        tick_spacing,
        factory: alloy::primitives::Address::ZERO,
        sqrt_price_x96: sp_0,
        liquidity: base_liquidity,
        tick: 0,
        tick_data,
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
        deployer: alloy::primitives::Address::ZERO,
        init_hash: B256::ZERO,
    };
    let (_identity, state) = V3PoolState::from_params(params, 8);
    state
}

/// The solver's CL-hop output for `amount_in` against a pre-built sequence —
/// the exact assembly the solver uses for a CL hop's `hop_outputs[i]`: find
/// the range `k` the input lands in (largest k with
/// `crossing_gross_input(k) ≤ amount_in`), then sum the crossing output +
/// the ending-range partial-step output.
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

/// Extract the exact-in token-OUT amount from a V3 swap outcome.
/// V3 convention: zfo exact-in → pool receives token0, sends token1 (amount1);
/// ofz exact-in → pool receives token1, sends token0 (amount0).
fn v3_exact_in_output(outcome: &V3SwapOutcome, zero_for_one: bool) -> U256 {
    if zero_for_one {
        outcome.amount1
    } else {
        outcome.amount0
    }
}

/// Run the parity sweep for one (direction, liquidity, tick_count, spacing,
/// fee) config. Asserts `v3_simulate_swap == solver_crossing_output` for every
/// amount_in landing strictly INSIDE a known range interior. Returns the
/// divergences (if any) for diagnostics.
fn run_parity_sweep(
    zero_for_one: bool,
    base_liquidity: u128,
    tick_count: usize,
    tick_spacing: i32,
    fee: u32,
) -> Vec<String> {
    let state =
        build_multi_tick_v3_state(base_liquidity, tick_count, tick_spacing, fee, zero_for_one);
    let seq = state
        .build_int_v3_sequence(tick_spacing, fee, zero_for_one)
        .expect("V3 state builds a tick-range sequence");
    let limit = unbounded_limit(zero_for_one);

    let n = seq.ranges.len();
    if n < 3 {
        return Vec::new();
    }
    let gins: Vec<U256> = (0..n)
        .map(|k| seq.compute_crossing(k).unwrap().crossing_gross_input)
        .collect();

    // Build amounts landing strictly inside each FULLY-BOUNDED range
    // interior. Range k is fully bounded by initialized ticks on BOTH sides
    // only for k in 0..n-1 — the solver's `compute_crossing` is undefined for
    // the unbounded tail past the last initialized tick, so the last range's
    // interior is deliberately NOT probed (a mirror-span heuristic there can
    // exceed coverage and flag a catastrophic under-estimation that is a
    // *separate* bounded-coverage artefact, not a +13-style rounding bug).
    let mut amounts: Vec<U256> = Vec::new();
    amounts.push(gins[1] / U256::from(2u64)); // range 0 interior
    for k in 1..n - 1 {
        let span = gins[k + 1].saturating_sub(gins[k]);
        amounts.push(gins[k] + span / U256::from(2u64));
        for delta in [1u64, 2, 3, 7, 13] {
            amounts.push(gins[k].saturating_add(U256::from(delta)));
            amounts.push(gins[k + 1].saturating_sub(U256::from(delta)));
        }
    }

    let mut failures = Vec::new();
    for amount_in_u256 in amounts {
        if amount_in_u256.is_zero() {
            continue;
        }
        // V3 exact-in: positive amount_specified.
        let Some(amount_specified) = I256::try_from(amount_in_u256).ok() else {
            continue;
        };
        let outcome = v3_simulate_swap(
            &state,
            fee,
            tick_spacing,
            zero_for_one,
            amount_specified,
            limit,
        )
        .expect("v3_simulate_swap succeeds on a well-formed multi-tick state");

        let sim_out = v3_exact_in_output(&outcome, zero_for_one);
        let solver_out = solver_crossing_output(amount_in_u256, &seq)
            .expect("solver produces a crossing output");

        if sim_out != solver_out {
            let n_ranges = seq.ranges.len();
            let delta = if sim_out > solver_out {
                sim_out - solver_out
            } else {
                solver_out - sim_out
            };
            failures.push(format!(
                "DIVERGENCE dir={} L={} spacing={} fee={} ticks={} amount_in={} sim={sim_out} \
                 solver={solver_out} delta={delta} n_ranges={n_ranges}",
                if zero_for_one { "zfo" } else { "ofz" },
                base_liquidity,
                tick_spacing,
                fee,
                tick_count,
                amount_in_u256,
            ));
        }
    }
    failures
}

#[test]
fn v3_crossing_solver_matches_v3_simulate_swap_across_liquidity_and_amounts() {
    // Liquidities spanning the V4 sweep regime PLUS the low-liquidity regime
    // of the failing UNI/DAI fee=500 pool whose `liquidity ≈ 2.6e10` is two
    // orders of magnitude below the V4 sweep floor — the systematic +13
    // over-prediction on that pool forced reaching into this regime.
    let liquidities: [u128; 8] = [
        1_000_000_000,                 // 1e9
        10_000_000_000,                // 1e10
        100_000_000_000,               // 1e11  (failing pool magnitude ≈ 2.6e10)
        10_000_000_000_000,            // 1e13
        1_000_000_000_000_000,         // 1e15
        100_000_000_000_000_000,       // 1e17
        10_000_000_000_000_000_000,    // 1e19
        1_000_000_000_000_000_000_000, // 1e21
    ];

    let mut failures = Vec::new();

    // Canonical V4-test config (fee=3000 / spacing=60).
    for &liq in &liquidities {
        for &ticks in &[5usize] {
            for &zfo in &[true, false] {
                failures.extend(run_parity_sweep(zfo, liq, ticks, 60, 3_000));
            }
        }
    }
    // Failing-pool config (fee=500 / spacing=10 — the UNI/DAI bucket).
    for &liq in &liquidities {
        for &ticks in &[5usize] {
            for &zfo in &[true, false] {
                failures.extend(run_parity_sweep(zfo, liq, ticks, 10, 500));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "V3 crossing solver diverges from v3_simulate_swap (hypothesis 2 confirmed — residual is \
         in compute_crossing/int_simulate_v3_swap, NOT stale state). First {} divergence(s):\n{}",
        failures.len(),
        failures
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

#[test]
fn v3_crossing_solver_matches_v3_simulate_swap_failing_pool_config_corner() {
    // A targeted thin-liquidity corner in the failing pool's exact fee /
    // spacing bucket, with a denser tick topology to stress the crossing
    // accumulator across many boundaries.
    let liq = 100_000_000_000_000_000_000u128; // 1e20
    let mut failures = Vec::new();
    for &zfo in &[true, false] {
        failures.extend(run_parity_sweep(zfo, liq, 8, 10, 500));
    }
    assert!(
        failures.is_empty(),
        "UNI/DAI-config corner diverged:\n{}",
        failures
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

/// Build a SPARSE-tick V3 pool: initialized ticks every `words_apart` bitmap
/// words (not every `tick_spacing`), so the active range spans MANY word
/// boundaries with constant liquidity before the first initialized tick is
/// reached. This mirrors the real UNI/DAI fee=500 pool at block 25647669 — its
/// sparse-tick topology (initialized ticks far from the active tick) is what
/// the dense synthetic sweep (`build_multi_tick_v3_state`) cannot construct
/// and where the on-chain `+13` over-prediction reproduces.
fn build_sparse_tick_v3_state(
    base_liquidity: u128,
    words_apart: usize,
    tick_count: usize,
    tick_spacing: i32,
    fee: u32,
    zero_for_one: bool,
) -> V3PoolState {
    let sp_0 = U256::from(1u128) << 96;
    let liq_gross = U256::from(base_liquidity).to::<U128>();
    let net_pos = i128::try_from(base_liquidity).unwrap();
    let net_neg = -net_pos;

    let mut tick_data: HashMap<i32, TickInfo> = HashMap::new();
    // Place initialized ticks every `words_apart` bitmap words. One bitmap
    // word covers 256·tick_spacing ticks, so the tick index is
    // `±(words_apart·256·tick_spacing)·i`. Liquidity toggles L↔2L across the
    // sparse initialized ticks (odd -L, even +L).
    for i in 1..=tick_count {
        let tick = (words_apart * 256 * u32::try_from(tick_spacing).unwrap() as usize * i) as i32;
        let tick = if zero_for_one { -tick } else { tick };
        let net = if i % 2 == 1 { net_neg } else { net_pos };
        tick_data.insert(
            tick,
            TickInfo {
                liquidity_gross: liq_gross,
                liquidity_net: net,
                block: 0,
            },
        );
    }

    let params = RegisterV3PoolParams {
        address: alloy::primitives::Address::ZERO,
        token0: alloy::primitives::Address::ZERO,
        token1: alloy::primitives::Address::ZERO,
        fee,
        tick_spacing,
        factory: alloy::primitives::Address::ZERO,
        sqrt_price_x96: sp_0,
        liquidity: base_liquidity,
        tick: 0,
        tick_data,
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
        deployer: alloy::primitives::Address::ZERO,
        init_hash: alloy::primitives::B256::ZERO,
    };
    let (_identity, state) = V3PoolState::from_params(params, 8);
    state
}

/// Run a parity sweep on a sparse-tick pool: amounts landing STRICTLY inside
/// range 0 (before the first initialized tick) so the swap crosses only word
/// boundaries with constant liquidity. The solver collapses range 0 to a
/// SINGLE `compute_swap_step_v3` (one fee rounding) while `v3_simulate_swap`
/// floors at EVERY word boundary (cumulative per-step rounding) — the
/// accumulated divergence is the on-chain `+13` class.
fn run_sparse_range0_sweep(
    zero_for_one: bool,
    base_liquidity: u128,
    words_apart: usize,
    tick_spacing: i32,
    fee: u32,
) -> Vec<String> {
    let state = build_sparse_tick_v3_state(
        base_liquidity,
        words_apart,
        3, // 3 initialized ticks far apart
        tick_spacing,
        fee,
        zero_for_one,
    );
    let seq = state
        .build_int_v3_sequence(tick_spacing, fee, zero_for_one)
        .expect("sparse state builds a sequence");
    let limit = unbounded_limit(zero_for_one);

    let n = seq.ranges.len();
    if n < 2 {
        return vec![format!("sparse sweep: only {n} ranges (expected ≥2)")];
    }
    // range 0 spans to the first initialized tick — its full capacity (gross
    // input to reach the first boundary) is `compute_crossing(1).crossing_gross_input`.
    let gin1 = seq
        .compute_crossing(1)
        .expect("crossing(1)")
        .crossing_gross_input;

    // Amounts landing strictly inside range 0 at several fractions + dust
    // offsets near the boundary.
    let mut amounts: Vec<U256> = vec![
        gin1 / U256::from(4u64),
        gin1 / U256::from(2u64),
        gin1 * U256::from(3u64) / U256::from(4u64),
        gin1.saturating_sub(U256::from(1u64)),
        gin1.saturating_sub(U256::from(13u64)),
        gin1.saturating_sub(U256::from(100u64)),
    ];
    // also tiny amounts (dust) + mid-range.
    amounts.push(U256::from(1u64));
    amounts.push(gin1 / U256::from(8u64));

    let mut failures = Vec::new();
    for amount_in_u256 in amounts {
        if amount_in_u256.is_zero() || amount_in_u256 >= gin1 {
            continue;
        }
        let Some(amount_specified) = I256::try_from(amount_in_u256).ok() else {
            continue;
        };
        let outcome = v3_simulate_swap(
            &state,
            fee,
            tick_spacing,
            zero_for_one,
            amount_specified,
            limit,
        )
        .expect("v3_simulate_swap succeeds");
        let sim_out = v3_exact_in_output(&outcome, zero_for_one);
        let solver_out =
            solver_crossing_output(amount_in_u256, &seq).expect("solver crossing output");
        if sim_out != solver_out {
            let delta = if sim_out > solver_out {
                sim_out - solver_out
            } else {
                solver_out - sim_out
            };
            failures.push(format!(
                "SPARSE-DIVERGENCE dir={} L={} words={} spacing={} fee={} amount_in={} sim={sim_out} \
                 solver={solver_out} delta={delta} n_ranges={n}",
                if zero_for_one { "zfo" } else { "ofz" },
                base_liquidity,
                words_apart,
                tick_spacing,
                fee,
                amount_in_u256,
            ));
        }
    }
    failures
}

#[test]
// GREEN regression guard for ergo E7ALWT — the solver's `compute_tick_ranges`
// collapses interior word-boundary ticks in constant-liquidity runs; the solver
// now RE-WALKS the collapsed interior boundaries per word boundary in
// `compute_crossing` / `int_simulate_v3_swap` (via `word_boundary_prices`),
// restoring the per-step `computeSwapStep` flooring `v3_simulate_swap`
// performs at every word boundary. The on-chain `+13` IIA trap (block
// 25647669, pool 0x57D7…dF80) is reproduced here on a synthetic sparse
// topology and now matches byte-for-byte. Re-introducing the collapse-without-
// re-walk regression makes this RED.
fn v3_sparse_tick_topology_reproduces_onchain_plus_thirteen_class() {
    // The decisive reproduction. Sparse initialized ticks (every 3 words)
    // with low liquidity in the failing pool's fee/spacing bucket — the
    // topology `v3_simulate_swap` floors per word boundary but the solver's
    // `compute_tick_ranges` collapses to a single range-0 span.
    let mut failures = Vec::new();
    for &liq in &[
        100_000_000_000u128,            // 1e11 (failing pool magnitude ≈ 2.6e10)
        26_362_865_912u128,             // exact failing-pool liquidity
        1_000_000_000_000_000u128,      // 1e15
        10_000_000_000_000_000_000u128, // 1e19
    ] {
        for &zfo in &[true, false] {
            failures.extend(run_sparse_range0_sweep(zfo, liq, 3, 10, 500));
        }
    }
    assert!(
        failures.is_empty(),
        "V3 sparse-tick solver diverges from v3_simulate_swap on range-0 interior \
         (the on-chain +13 class — `compute_tick_ranges` collapses interior word \
         boundaries, losing the per-step `computeSwapStep` flooring). Divergences:\n{}",
        failures
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

/// Build a V3 pool at current_tick 0 (a word boundary) with the current price
/// STRICTLY between sqrt(0) and sqrt(spacing) — so the on-chain step-0
/// current-tick drain is a real non-zero floored step (the trigger the fee-1
/// V4 recurrence exposed, which a price exactly on the boundary cannot
/// exercise). Initialized ticks on both sides so both directions work.
fn build_real_position_v3_state(base_liquidity: u128, tick_spacing: i32, fee: u32) -> V3PoolState {
    use degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal;
    let sq_t = get_sqrt_ratio_at_tick_internal(0).unwrap();
    let sq_n = get_sqrt_ratio_at_tick_internal(tick_spacing).unwrap();
    let sp = (U256::from(sq_t) + U256::from(sq_n)) / U256::from(2u64);

    let liq_gross = U256::from(base_liquidity).to::<U128>();
    let net_pos = i128::try_from(base_liquidity).unwrap();
    let net_neg = -net_pos;
    let mut tick_data: HashMap<i32, TickInfo> = HashMap::new();
    for sig in [-1i32, 1] {
        tick_data.insert(
            sig * tick_spacing,
            TickInfo {
                liquidity_gross: liq_gross,
                liquidity_net: net_pos,
                block: 0,
            },
        );
        tick_data.insert(
            sig * 2 * tick_spacing,
            TickInfo {
                liquidity_gross: liq_gross,
                liquidity_net: net_neg,
                block: 0,
            },
        );
    }
    let params = RegisterV3PoolParams {
        address: alloy::primitives::Address::ZERO,
        token0: alloy::primitives::Address::ZERO,
        token1: alloy::primitives::Address::ZERO,
        fee,
        tick_spacing,
        factory: alloy::primitives::Address::ZERO,
        sqrt_price_x96: sp,
        liquidity: base_liquidity,
        tick: 0,
        tick_data,
        update_block: 0,
        tick_data_block: None,
        coverage: PoolTickCoverage::Tracked,
        fetcher: None,
        deployer: alloy::primitives::Address::ZERO,
        init_hash: B256::ZERO,
    };
    let (_identity, state) = V3PoolState::from_params(params, 8);
    state
}

/// GREEN guard for the shared `compute_tick_ranges` current-tick step-0 floor
/// fix on the V3 side: with the current price strictly inside a word-boundary
/// current tick (V4 fee-1 trigger, shared V3/V4 code path), the V3 solver
/// int-solve path must equal `v3_simulate_swap` (the on-chain oracle) in BOTH
/// directions. Pre-fix this over-predicted by a few wei at zfo=true.
#[test]
fn fee1_word_boundary_current_tick_v3_parity() {
    let liq = 1_000_000_000_000_000_000u128; // 1e18
    let mut failures = Vec::new();
    for ts in [60i32, 1i32] {
        for zfo in [true, false] {
            let state = build_real_position_v3_state(liq, ts, 3_000);
            let Some(seq) = state.build_int_v3_sequence(ts, 3_000, zfo) else {
                continue;
            };
            let limit = unbounded_limit(zfo);
            let n = seq.ranges.len();
            if n < 3 {
                continue;
            }
            let gins: Vec<U256> = (0..n)
                .map(|k| seq.compute_crossing(k).unwrap().crossing_gross_input)
                .collect();
            let mut amounts: Vec<U256> = vec![gins[1] / U256::from(2u64)];
            for k in 1..n - 1 {
                let span = gins[k + 1].saturating_sub(gins[k]);
                amounts.push(gins[k] + span / U256::from(2u64));
                for delta in [1u64, 2, 3, 5, 13] {
                    amounts.push(gins[k].saturating_add(U256::from(delta)));
                    amounts.push(gins[k + 1].saturating_sub(U256::from(delta)));
                }
            }
            for a in amounts {
                if a.is_zero() {
                    continue;
                }
                let Ok(amt) = I256::try_from(a) else { continue };
                let outcome = v3_simulate_swap(&state, 3_000, ts, zfo, amt, limit).expect("v3 sim");
                let sim = v3_exact_in_output(&outcome, zfo);
                let Some(sol) = solver_crossing_output(a, &seq) else {
                    continue;
                };
                if sim != sol {
                    failures.push(format!(
                        "tick=0 ts={ts} dir={} amount={a} sim={sim} solver={sol}",
                        if zfo { "zfo" } else { "ofz" },
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "V3 word-boundary real-position parity diverged (shared compute_tick_ranges fix). {}:\n{}",
        failures.len(),
        failures
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
    );
}
