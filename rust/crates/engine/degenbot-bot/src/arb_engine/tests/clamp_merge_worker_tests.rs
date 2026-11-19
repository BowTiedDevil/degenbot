#![expect(clippy::expect_used)] // tests assert clamp/merge invariants
use crate::arb_engine::executor::SolveClamp;
use crate::arb_engine::lane_walk::clamp_result_in_worker;
use crate::arb_engine::lifecycle::register_path;
use crate::arb_engine::solve_cycle::{ClampReport, ClampStatus, SolveCycleShared};
use crate::arb_engine::{ArbitrageEngine, BlockMetadata};
use ::degenbot_pools::solve::{MixedPath, SolvePathResult};
use alloy::primitives::{Address, U256};
use degenbot_pools::TickInfo;
use degenbot_pools::V4PoolKey;
use hashbrown::HashMap;
use std::sync::Arc;

/// The clamp's unsupported-family verdict: every family without a byte-exact
/// twin answers `ClampStatus::Unverified`, keeps its reported figures, and never
/// presents them as verified. The V2/concentrated-liquidity families own the
/// twin arm.
#[test]
fn every_unsupported_family_clamps_to_the_unverified_verdict() {
    use crate::arb_engine::hooked_leg_requote::HookedLegPlan;
    use crate::arb_engine::solve_cycle::clamp_result_with_state;
    use ::degenbot_pools::solve::{HopType, MixedPoolRef};
    use degenbot_substrate::state_lock::LockSite;

    let engine = ArbitrageEngine::new();
    let core = engine.core().read_at(LockSite::Solver);
    for family in [
        HopType::SolidlyStable,
        HopType::BalancerWeighted,
        HopType::BalancerStable,
        HopType::CurveStableswap,
    ] {
        let reported = SolvePathResult {
            optimal_input: U256::from(1_000u64),
            profit: U256::from(5u64),
            hop_outputs: vec![U256::from(1_010u64)],
            consumed_inputs: vec![U256::from(1_000u64)],
            ..Default::default()
        };
        let report = clamp_result_with_state(
            &core,
            0,
            &[MixedPoolRef {
                hop_type: family,
                pool_key: 9_999,
                zero_for_one: true,
            }],
            U256::from(1u8),
            &HookedLegPlan::empty(),
            reported.clone(),
        )
        .expect("an unsupported family still yields a report");
        assert_eq!(
            report.status,
            ClampStatus::Unverified,
            "{family:?} has no byte-exact twin at the clamp seam"
        );
        assert_eq!(
            report.result.hop_outputs, reported.hop_outputs,
            "{family:?} reported figures stand, unverified"
        );
    }
}

/// A mixed path keeps the twin-verified figures AND the one verdict: the
/// supported hop lands on exactly the figure it clamps to on its own, while the
/// unsupported hop's presence downgrades the whole path to `Unverified`.
#[test]
fn a_mixed_path_clamps_supported_figures_under_an_unverified_verdict() {
    use crate::arb_engine::hooked_leg_requote::HookedLegPlan;
    use crate::arb_engine::solve_cycle::clamp_result_with_state;
    use ::degenbot_pools::solve::{HopType, MixedPoolRef};
    use degenbot_substrate::state_lock::LockSite;

    let engine = ArbitrageEngine::new();
    let v2 = engine.register_v2_pool(
        Address::from([0x61u8; 20]),
        U256::from(1_000_000_000_000u64).to(),
        U256::from(2_000_000_000_000u64).to(),
        997,
        1000,
    );
    let solver_result = |outputs: Vec<U256>| SolvePathResult {
        optimal_input: U256::from(1_000u64),
        profit: U256::from(1_000u64),
        consumed_inputs: vec![U256::from(1_000u64); outputs.len()],
        hop_outputs: outputs,
        ..Default::default()
    };

    let core = engine.core().read_at(LockSite::Solver);
    // The supported hop ALONE: the twin's own byte-exact figure.
    let supported = clamp_result_with_state(
        &core,
        0,
        &[MixedPoolRef {
            hop_type: HopType::V2,
            pool_key: v2,
            zero_for_one: true,
        }],
        U256::from(1u8),
        &HookedLegPlan::empty(),
        solver_result(vec![U256::from(2_000u64)]),
    )
    .expect("the V2 hop commits");
    assert_eq!(
        supported.status,
        ClampStatus::Adjusted,
        "premise: the solver's 2_000 over-predicts the twin"
    );
    let twin_figure = supported.result.hop_outputs[0];

    // The same hop followed by an unsupported family.
    let mixed = clamp_result_with_state(
        &core,
        0,
        &[
            MixedPoolRef {
                hop_type: HopType::V2,
                pool_key: v2,
                zero_for_one: true,
            },
            MixedPoolRef {
                hop_type: HopType::CurveStableswap,
                pool_key: 9_999,
                zero_for_one: true,
            },
        ],
        U256::from(1u8),
        &HookedLegPlan::empty(),
        solver_result(vec![U256::from(2_000u64), U256::from(3_000u64)]),
    )
    .expect("the mixed path commits");

    assert_eq!(
        mixed.status,
        ClampStatus::Unverified,
        "one unsupported hop downgrades the whole path"
    );
    assert_eq!(
        mixed.result.hop_outputs[0], twin_figure,
        "the supported hop still clamps to its twin's figure"
    );
    assert_eq!(
        mixed.result.hop_outputs[1],
        U256::from(3_000u64),
        "the unsupported hop's reported figure stands, unverified"
    );
}
/// Narrow single-position V4 pool (±60 ticks, 1e6 liquidity) + a one-hop
/// path: the over-fed committed input is the empty-march class. Returns
/// (engine, `path_id`, the to_solve-aligned pool-ref snapshot).
fn overfed_v4_engine() -> (ArbitrageEngine, u64, Vec<std::sync::Arc<MixedPath>>) {
    use degenbot_pools::PoolTickCoverage;
    use degenbot_pools::RegisterV4PoolParams;
    fn usdc_local(amount: u64) -> alloy::primitives::Uint<112, 2> {
        (U256::from(amount) * U256::from(10u64).pow(U256::from(6)))
            .to::<alloy::primitives::Uint<112, 2>>()
    }
    fn weth_local(amount: u64) -> alloy::primitives::Uint<112, 2> {
        (U256::from(amount) * U256::from(10u64).pow(U256::from(18)))
            .to::<alloy::primitives::Uint<112, 2>>()
    }
    const GAMMA_03: u64 = 997;
    const FEE_DENOM_03: u64 = 1000;
    let mut engine = ArbitrageEngine::new();
    // V2 pool: large reserves so its output dwarfs the V4 hop's capacity —
    // the V4 hop is the over-fed one (this isolates hop1's input clamp).
    let v2 = engine.register_v2_pool(
        alloy::primitives::Address::from([0x11u8; 20]),
        usdc_local(1_500_000),
        weth_local(20_000_000_000),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let mut tick_data = HashMap::new();
    tick_data.insert(
        60,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(300),
            liquidity_net: 150i128,
            block: 0,
        },
    );
    tick_data.insert(
        -60,
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(200),
            liquidity_net: -100i128,
            block: 0,
        },
    );
    let v4_id = engine
        .register_v4_pool(&RegisterV4PoolParams {
            pool_manager: alloy::primitives::Address::from([0x44u8; 20]),
            pool_id: [0xabu8; 32],
            pool_key: V4PoolKey {
                currency0: alloy::primitives::Address::from([0x30u8; 20]),
                currency1: alloy::primitives::Address::from([0x31u8; 20]),
                fee: 500,
                tick_spacing: 10,
                hooks: alloy::primitives::Address::ZERO,
            },
            state_view: Address::ZERO,
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
            update_block: 0,
            tick_data_block: None,
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
            known_bitmap_words: Vec::new(),
        })
        .expect("V4 registration failed");
    let path_id = register_path(
        &mut engine,
        vec![
            ::degenbot_pools::solve::PoolHop {
                pool_id: v2,
                zero_for_one: true,
            },
            ::degenbot_pools::solve::PoolHop {
                pool_id: v4_id,
                zero_for_one: false,
            },
        ],
    )
    .expect("two-hop path registers");
    let pool_refs = std::iter::once(engine.registry.get(path_id).expect("registered").clone())
        .collect::<Vec<_>>();
    (engine, path_id, pool_refs)
}
fn worker_probe_ctx(
    core: Arc<degenbot_substrate::state_lock::StateLock<degenbot_substrate::BotState>>,
    pool_refs: Vec<std::sync::Arc<MixedPath>>,
) -> Arc<SolveCycleShared> {
    Arc::new(SolveCycleShared::for_test(
        0,
        BlockMetadata::default(),
        core,
        pool_refs,
        true,
        None,
    ))
}
/// The over-fed two-hop fixture every clamp/merge test shares: a V2 hop
/// feeding a narrow V4 hop whose committed input is far past its capacity.
fn overfed_result() -> SolvePathResult {
    let committed = U256::from(1u128) << 120;
    SolvePathResult {
        optimal_input: U256::from(1_000_000_000u64),
        profit: U256::from(1_000u64),
        hop_outputs: vec![committed, committed],
        consumed_inputs: vec![committed, committed],
        state_nonces: vec![],
        solver_pool_states: Vec::new(),
        ..Default::default()
    }
}

/// The worker handoff, unwrapped: the stance is on in every probe here, so a
/// non-finalized or refused handoff is a fixture error.
fn expect_finalized(handoff: Option<crate::arb_engine::executor::SolveClamp>) -> ClampReport {
    match handoff {
        Some(crate::arb_engine::executor::SolveClamp::Finalized(report)) => report,
        Some(crate::arb_engine::executor::SolveClamp::Unfinalized(_)) | None => {
            unreachable!("the worker clamp finalizes when the stance is on")
        }
    }
}

/// The merge clamp and the WORKER clamp are the same computation from two call
/// sites: byte-identical figures AND the same status on identical input.
#[test]
fn worker_clamp_matches_engine_clamp_bit_for_bit() {
    let (engine, path_id, pool_refs) = overfed_v4_engine();
    let engine_report = engine
        .cycle
        .clamp_concentrated_liquidity_hop_capacity(path_id, overfed_result(), &engine.registry)
        .expect("the merge door commits");
    assert_eq!(
        engine_report.status,
        ClampStatus::Adjusted,
        "premise: the over-fed input must clamp"
    );
    assert!(
        engine_report.result.consumed_inputs[1] < U256::from(1u128) << 120,
        "premise: the V4 hop input clamp fired"
    );
    let ctx = worker_probe_ctx(Arc::clone(engine.core()), pool_refs);
    let worker_report =
        expect_finalized(clamp_result_in_worker(&ctx, 0, path_id, overfed_result()));
    assert_eq!(
        worker_report.status, engine_report.status,
        "status must match"
    );
    assert_eq!(
        worker_report.result, engine_report.result,
        "clamped result must be byte-identical"
    );
}

/// The clamp's status lattice over ONE V2→V2 fixture: a result whose figures
/// already equal the twins' byte-exact values is `Exact`, one moved executable
/// amount makes it `Adjusted`, and a hop with no byte-exact twin downgrades the
/// WHOLE path to `Unverified` even when another hop changed.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one V2→V2 fixture drives all three statuses — splitting it would re-register the pools three times"
)]
fn clamp_status_is_exact_adjusted_or_unverified() {
    use crate::arb_engine::hooked_leg_requote::HookedLegPlan;
    use crate::arb_engine::solve_cycle::clamp_result_with_state;
    use ::degenbot_pools::solve::{HopType, MixedPoolRef};
    use degenbot_substrate::state_lock::LockSite;

    // Pool A prices token0 richly in token1; pool B is its inverse — the
    // round trip is grossly profitable, so the solve never filters on profit.
    let mut engine = ArbitrageEngine::new();
    let v2_a = engine.register_v2_pool(
        Address::from([0x51u8; 20]),
        U256::from(1_000_000_000_000u64).to(),
        U256::from(2_000_000_000_000u64).to(),
        997,
        1000,
    );
    let v2_b = engine.register_v2_pool(
        Address::from([0x52u8; 20]),
        U256::from(2_000_000_000_000u64).to(),
        U256::from(1_000_000_000_000u64).to(),
        997,
        1000,
    );
    let path_id = register_path(
        &mut engine,
        vec![
            ::degenbot_pools::solve::PoolHop {
                pool_id: v2_a,
                zero_for_one: true,
            },
            ::degenbot_pools::solve::PoolHop {
                pool_id: v2_b,
                zero_for_one: false,
            },
        ],
    )
    .expect("V2→V2 path registers");

    // ── Exact: the solver's own figures already ARE the twins' figures ────
    let mut solved_all = engine.cycle.solve_all(&engine.registry);
    let solved = solved_all.remove(&path_id).expect("the V2 arb solves");
    assert_eq!(
        solved.status,
        ClampStatus::Exact,
        "the cold-start sweep stores the same fused report the hot path does"
    );
    let exact = engine
        .cycle
        .clamp_concentrated_liquidity_hop_capacity(path_id, solved.result, &engine.registry)
        .expect("the clamp commits");
    assert_eq!(
        exact.status,
        ClampStatus::Exact,
        "a V2/V2 path whose figures match both twins is exactly verified"
    );
    assert_eq!(
        exact.result.profit,
        exact
            .result
            .hop_outputs
            .last()
            .copied()
            .expect("hop outputs")
            .saturating_sub(exact.result.consumed_inputs[0]),
        "the selection profit IS the final output minus the first consumed input"
    );

    // ── Adjusted: one over-reported hop output is realigned ──────────────
    let mut drifted = exact.result.clone();
    *drifted.hop_outputs.last_mut().expect("hop outputs") += U256::ONE;
    let adjusted = engine
        .cycle
        .clamp_concentrated_liquidity_hop_capacity(path_id, drifted, &engine.registry)
        .expect("the clamp commits");
    assert_eq!(
        adjusted.status,
        ClampStatus::Adjusted,
        "a realigned hop output is an adjustment"
    );
    assert_eq!(
        adjusted.result, exact.result,
        "the realignment lands on the twin's own figures"
    );

    // ── Unverified: no byte-exact twin for the hop's family ──────────────
    let unsupported = SolvePathResult {
        optimal_input: U256::from(1_000u64),
        profit: U256::from(5u64),
        hop_outputs: vec![U256::from(1_010u64)],
        consumed_inputs: vec![U256::from(1_000u64)],
        ..Default::default()
    };
    let core = engine.core().read_at(LockSite::Solver);
    let unverified = clamp_result_with_state(
        &core,
        0,
        &[MixedPoolRef {
            hop_type: HopType::SolidlyStable,
            pool_key: 9_999,
            zero_for_one: true,
        }],
        U256::from(1u128),
        &HookedLegPlan::empty(),
        unsupported,
    )
    .expect("an unsupported family still yields a report");
    assert_eq!(
        unverified.status,
        ClampStatus::Unverified,
        "Solidly has no byte-exact twin at this seam — and Unverified dominates the profit recompute's adjustment"
    );
    assert_eq!(
        unverified.result.hop_outputs,
        vec![U256::from(1_010u64)],
        "an unsupported hop's reported figures stand"
    );
}
/// SIMPIPE2 T3: a payload riding `merge_one_result` is stored at the
/// engine (`inline_payloads`) and a re-merge WITHOUT the payload drops the
/// stale entry — per-entry presence decides Python-side. (The delivery
/// drain into `ResultBatch.payloads` is covered by the `delivery_policy`
/// tests + the FFI conversion; this pins the merge-site store/drop.)
#[test]
fn merge_stores_payload_and_drops_it_without_one() {
    use crate::arb_engine::inline_sim::{InlineSwapFamily, SimulatedPathResult};
    use alloy::primitives::{Address, I256, U256};
    let (mut engine, path_id, _pool_refs) = overfed_v4_engine();
    let metadata = BlockMetadata::default();
    let mk = || SolvePathResult {
        optimal_input: U256::from(1_000_000_000u64),
        profit: U256::from(1_000u64),
        hop_outputs: vec![U256::from(1u64)],
        consumed_inputs: vec![U256::from(1u64)],
        state_nonces: vec![0],
        solver_pool_states: Vec::new(),
        ..Default::default()
    };
    let payload = SimulatedPathResult {
        path_id,
        gross_profit: U256::from(1_000u64),
        net_profit: U256::from(900u64),
        gas_used: 300_000,
        priority_fee: 2,
        base_fee_next: 30,
        execute_calldata: vec![1, 2, 3],
        access_list: None,
        captured_swaps: vec![crate::arb_engine::inline_sim::CapturedSwapRow {
            emitter: Address::from([0x11u8; 20]),
            family: InlineSwapFamily::V4,
            amount0: I256::MINUS_ONE,
            amount1: I256::ONE,
            sqrt_price_x96: U256::ZERO,
            liquidity: U256::ZERO,
            tick: 0,
        }],
        hop_count: 1,
        failure: None,
    };
    let _ = engine.cycle.merge_one_result(
        42,
        &metadata,
        path_id,
        SolveClamp::finalized_for_test(mk()),
        Some(payload),
        &engine.registry,
        &mut engine.delivery,
    );
    assert!(
        engine.cycle.inline_payloads.contains_key(&path_id),
        "the payload must be stored at merge"
    );
    // The path re-solves WITHOUT a payload (stance off or hook silence):
    // the stale entry must drop — presence decides per entry.
    let _ = engine.cycle.merge_one_result(
        43,
        &metadata,
        path_id,
        SolveClamp::finalized_for_test(mk()),
        None,
        &engine.registry,
        &mut engine.delivery,
    );
    assert!(
        !engine.cycle.inline_payloads.contains_key(&path_id),
        "a payload-less re-merge must drop the stale payload"
    );
}
/// The single-finalization protocol at the merge: a FINALIZED handoff stores
/// the report's committed figures verbatim (no second margin application), and
/// an UNFINALIZED handoff runs the merge's one finalizer — landing on the same
/// figures, byte for byte.
#[test]
fn merge_stores_a_finalized_report_verbatim_and_finalizes_an_unfinalized_one_once() {
    let (mut engine, path_id, pool_refs) = overfed_v4_engine();
    let metadata = BlockMetadata::default();

    // Worker arm: the clamp commits once; the merge stores the report as-is.
    let ctx = worker_probe_ctx(Arc::clone(engine.core()), pool_refs);
    let report = expect_finalized(clamp_result_in_worker(&ctx, 0, path_id, overfed_result()));
    assert_eq!(
        report.status,
        ClampStatus::Adjusted,
        "premise: the clamp fired"
    );
    let committed = report.result.clone();
    let status = engine.cycle.merge_one_result(
        42,
        &metadata,
        path_id,
        SolveClamp::Finalized(report),
        None,
        &engine.registry,
        &mut engine.delivery,
    );
    assert_eq!(status, Some(ClampStatus::Adjusted));
    {
        let stored = engine.cycle.results.get(&path_id).expect("worker-merged");
        assert_eq!(
            stored.consumed_inputs, committed.consumed_inputs,
            "a finalized handoff must not re-clip the committed inputs"
        );
        assert_eq!(
            stored.profit, committed.profit,
            "a finalized handoff must not recompute the profit"
        );
        assert_eq!(
            stored.result, committed,
            "the report's figures are stored verbatim"
        );
        assert_eq!(
            stored.status,
            ClampStatus::Adjusted,
            "the stored row keeps the status that produced its figures"
        );
    }

    // No worker clamp (stance off): the MERGE runs that one finalization.
    let pre = overfed_result().consumed_inputs[1];
    let status = engine.cycle.merge_one_result(
        42,
        &metadata,
        path_id,
        SolveClamp::Unfinalized(overfed_result()),
        None,
        &engine.registry,
        &mut engine.delivery,
    );
    assert_eq!(
        status,
        Some(ClampStatus::Adjusted),
        "the merge finalizer clamps the over-fed hop itself"
    );
    let stored = engine.cycle.results.get(&path_id).expect("merge-finalized");
    assert!(
        stored.consumed_inputs[1] < pre,
        "the merge clamp must fire when the worker reported none"
    );
    assert_eq!(
        stored.result, committed,
        "both finalization sites agree byte for byte"
    );
}

/// The store keeps the clamp's status: an `Unverified` report merged for a path
/// is `Unverified` in `results` AND in the `latest_results` accessor — the row
/// is the report, never a status-stripped result.
#[test]
fn an_unverified_clamp_status_survives_the_store() {
    use crate::arb_engine::hooked_leg_requote::HookedLegPlan;
    use crate::arb_engine::lifecycle::latest_results;
    use crate::arb_engine::solve_cycle::clamp_result_with_state;
    use ::degenbot_pools::solve::{HopType, MixedPoolRef};
    use degenbot_substrate::state_lock::LockSite;

    let (mut engine, path_id, _pool_refs) = overfed_v4_engine();
    let metadata = BlockMetadata::default();
    let unsupported = SolvePathResult {
        optimal_input: U256::from(1_000u64),
        profit: U256::from(5u64),
        hop_outputs: vec![U256::from(1_010u64)],
        consumed_inputs: vec![U256::from(1_000u64)],
        ..Default::default()
    };
    let report = {
        let core = engine.core().read_at(LockSite::Solver);
        clamp_result_with_state(
            &core,
            path_id,
            &[MixedPoolRef {
                hop_type: HopType::SolidlyStable,
                pool_key: 9_999,
                zero_for_one: true,
            }],
            U256::from(1u128),
            &HookedLegPlan::empty(),
            unsupported,
        )
        .expect("an unsupported family still yields a report")
    };
    assert_eq!(report.status, ClampStatus::Unverified);
    let merged = engine.cycle.merge_one_result(
        42,
        &metadata,
        path_id,
        SolveClamp::Finalized(report),
        None,
        &engine.registry,
        &mut engine.delivery,
    );
    assert_eq!(merged, Some(ClampStatus::Unverified));
    assert_eq!(
        engine
            .cycle
            .results
            .get(&path_id)
            .expect("merged row")
            .status,
        ClampStatus::Unverified,
        "the stored row keeps the status that produced its figures"
    );
    let (rows, _block) = latest_results(&engine);
    let row = rows.get(&path_id).expect("accessor row");
    assert_eq!(
        row.status,
        ClampStatus::Unverified,
        "the accessor hands the status out with the figures"
    );
    assert_eq!(
        row.profit,
        U256::from(10u64),
        "the figures still read off the report (profit recomputed from its outputs)"
    );
}

/// A refused candidate merges NOTHING: the merge drops whatever figure the
/// epoch still held for the path (and its stale sim payload), so the delivery
/// window expires the path instead of re-publishing a superseded figure.
#[test]
fn a_refused_candidate_merges_nothing() {
    let (mut engine, path_id, _pool_refs) = overfed_v4_engine();
    let metadata = BlockMetadata::default();
    let mut stale = overfed_result();
    stale.profit = U256::from(7u64);
    engine
        .cycle
        .results
        .insert(path_id, ClampReport::for_test(stale, ClampStatus::Exact));

    // A caveated (hooked) result the plan cannot price exactly is the clamp's
    // ONLY refusal.
    let mut caveated = overfed_result();
    caveated.caveats = degenbot_pools::Caveats::HOOKED_POOL;
    let status = engine.cycle.merge_one_result(
        42,
        &metadata,
        path_id,
        SolveClamp::Unfinalized(caveated),
        None,
        &engine.registry,
        &mut engine.delivery,
    );
    assert_eq!(status, None, "the clamp refused the candidate");
    assert!(
        !engine.cycle.results.contains_key(&path_id),
        "a refused candidate leaves no result to deliver"
    );
    assert!(
        !engine.cycle.inline_payloads.contains_key(&path_id),
        "a refused candidate leaves no sim payload behind"
    );
}

/// The refusal at the WORKER seam: `clamp_result_in_worker` reports the refusal
/// as `None`, so the walk records one Suppressed outcome and the merge never
/// sees the candidate.
#[test]
fn the_worker_clamp_reports_a_refusal_as_absence() {
    let (engine, path_id, pool_refs) = overfed_v4_engine();
    let ctx = worker_probe_ctx(Arc::clone(engine.core()), pool_refs);
    let mut caveated = overfed_result();
    caveated.caveats = degenbot_pools::Caveats::HOOKED_POOL;
    assert!(
        clamp_result_in_worker(&ctx, 0, path_id, caveated).is_none(),
        "an unpriceable hooked leg is refused at the worker seam too"
    );
}
// ----------------- bundle.simulate span hygiene -----------------
/// RED-gate: the merge-site microsecond `degenbot.bundle.simulate`
/// "verdict bookmark" spans collided with the REAL per-path EVM sim spans
/// of the same name (traces 98f7cf52 / ab13f75fad50: 90-300 markers per
/// block drowned the ms-scale sims). The merge must create NO span with
/// that name - the verdict is an `info!` event on the enclosing merge
/// span, and the span name now belongs solely to simulation work.
///
/// DEFAULT-GATE VISIBLE (no otel cfg), on the established pattern: the marker
/// flood was what made Jaeger unreadable, so the regression gate must not
/// hide behind --features otel.
#[test]
fn merge_payload_store_emits_no_bundle_simulate_span() {
    use std::sync::Mutex;
    struct SpanNameCapture {
        names: std::sync::Arc<Mutex<Vec<String>>>,
    }
    impl<S> tracing_subscriber::Layer<S> for SpanNameCapture
    where
        S: tracing::Subscriber,
    {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            _id: &tracing::span::Id,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            self.names
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(attrs.metadata().name().to_string());
        }
    }
    use tracing_subscriber::layer::SubscriberExt as _;
    let names = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
    let capture = SpanNameCapture {
        names: std::sync::Arc::clone(&names),
    };
    let subscriber = tracing_subscriber::registry().with(capture);
    let (mut engine, path_id, _pool_refs) = overfed_v4_engine();
    let metadata = BlockMetadata::default();
    let mk = || SolvePathResult {
        optimal_input: U256::from(1_000_000_000u64),
        profit: U256::from(1_000u64),
        hop_outputs: vec![U256::from(1u64)],
        consumed_inputs: vec![U256::from(1u64)],
        state_nonces: vec![0],
        solver_pool_states: Vec::new(),
        ..Default::default()
    };
    let payload = crate::arb_engine::inline_sim::SimulatedPathResult {
        path_id,
        gross_profit: U256::from(1_000u64),
        net_profit: U256::from(900u64),
        gas_used: 300_000,
        priority_fee: 2,
        base_fee_next: 30,
        execute_calldata: vec![1, 2, 3],
        access_list: None,
        captured_swaps: Vec::new(),
        hop_count: 1,
        failure: None,
    };
    tracing::subscriber::with_default(subscriber, || {
        // Enclosing merge span, as in both production arms.
        let merge = tracing::info_span!("degenbot.arb.merge", merge.paths = 1u64);
        let _ctx = merge.enter();
        let _ = engine.cycle.merge_one_result(
            42,
            &metadata,
            path_id,
            SolveClamp::finalized_for_test(mk()),
            Some(payload),
            &engine.registry,
            &mut engine.delivery,
        );
    });
    let created = names
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let offenders: Vec<_> = created
        .iter()
        .filter(|n| *n == "degenbot.bundle.simulate")
        .collect();
    assert!(
        offenders.is_empty(),
        "merge must not create bundle.simulate markers (the name belongs to real sims); \
             spans created: {created:?}"
    );
}
/// GREEN-gate: the WORKER-side inline sim gets the honest
/// `degenbot.bundle.simulate` span - a real ms-class EVM sim on the solve
/// path, parented under the cycle span, with the terminal verdict.
#[cfg(feature = "otel")]
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "single end-to-end span-emission assertion: stub + emit + export + attribute checks read best as one sequence"
)]
fn inline_sim_payload_emits_worker_sim_span_with_verdict() {
    use crate::arb_engine::lane_walk::inline_sim_payload;
    use crate::otel;
    use opentelemetry_sdk::trace::InMemorySpanExporter;
    use tracing_subscriber::layer::SubscriberExt;
    struct StubSim {
        fail: bool,
        path_id: u64,
    }
    impl crate::arb_engine::inline_sim::InlineSimulator for StubSim {
        fn simulate_path(
            &self,
            request: crate::arb_engine::inline_sim::InlineSimRequest,
        ) -> Option<crate::arb_engine::inline_sim::SimulatedPathResult> {
            assert_eq!(
                request.path_id, self.path_id,
                "stub receives the merged path id"
            );
            Some(crate::arb_engine::inline_sim::SimulatedPathResult {
                path_id: request.path_id,
                gross_profit: U256::from(1_000u64),
                net_profit: U256::from(900u64),
                gas_used: 300_000,
                priority_fee: 2,
                base_fee_next: 30,
                execute_calldata: vec![7, 8, 9],
                access_list: None,
                captured_swaps: Vec::new(),
                hop_count: 1,
                failure: self
                    .fail
                    .then(|| crate::arb_engine::inline_sim::InlineSimFailure {
                        fail_index: None,
                        revert_data: Vec::new(),
                        bucket: "test".to_string(),
                    }),
            })
        }
    }
    let exporter = InMemorySpanExporter::default();
    let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
    let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));
    let (engine, path_id, pool_refs) = overfed_v4_engine();
    let mut ctx = worker_probe_ctx(Arc::clone(engine.core()), pool_refs);
    // Fresh Arc (refcount 1): install the stub via get_mut.
    Arc::get_mut(&mut ctx)
        .expect("probe ctx exclusively owned")
        .inline_sim = Some(Arc::new(StubSim {
        fail: false,
        path_id,
    }));
    let result = SolvePathResult {
        optimal_input: U256::from(1_000_000_000u64),
        profit: U256::from(1_000u64),
        hop_outputs: vec![U256::from(1u64)],
        consumed_inputs: vec![U256::from(1u64)],
        state_nonces: vec![0],
        solver_pool_states: Vec::new(),
        ..Default::default()
    };
    tracing::subscriber::with_default(subscriber, || {
        let solve = tracing::info_span!("degenbot.arb.solve", block.number = 7u64);
        let _guard = solve.enter();
        let payload = inline_sim_payload(&ctx, 0, path_id, &result, &tracing::Span::current());
        assert!(
            payload.is_some(),
            "stub hook returns a payload; None only when the seam is off"
        );
    });
    provider.force_flush().expect("flush");
    let spans = exporter.get_finished_spans().expect("spans");
    let solve_id = spans
        .iter()
        .find(|sp| sp.name.as_ref() == "degenbot.arb.solve")
        .map(|sp| sp.span_context.span_id())
        .expect("solve span must be exported");
    let sims: Vec<_> = spans
        .iter()
        .filter(|sp| sp.name.as_ref() == "degenbot.bundle.simulate")
        .collect();
    assert_eq!(
        sims.len(),
        1,
        "exactly one worker-side sim span; all: {:?}",
        spans.iter().map(|sp| sp.name.as_ref()).collect::<Vec<_>>()
    );
    assert_eq!(
        sims[0].parent_span_id, solve_id,
        "the worker sim span must parent under the cycle span"
    );
    let attr = |k: &'static str| {
        sims[0]
            .attributes
            .iter()
            .find(|kv| kv.key == opentelemetry::Key::from_static_str(k))
            .map(|kv| kv.value.to_string())
    };
    assert_eq!(
        attr("path_id").as_deref(),
        Some(path_id.to_string().as_str()),
        "path_id attribute"
    );
    assert_eq!(
        attr("simulate.verdict").as_deref(),
        Some("profitable"),
        "verdict recorded at span close; attrs: {:?}",
        sims[0].attributes
    );
    assert_eq!(
        attr("sim.path").as_deref(),
        Some("worker_inline"),
        "seam discriminator distinguishes worker sims from the FFI seam"
    );
}
