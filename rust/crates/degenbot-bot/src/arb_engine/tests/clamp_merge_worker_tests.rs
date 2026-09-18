#![expect(clippy::expect_used)] // tests assert clamp/merge invariants
use crate::arb_engine::lane_walk::clamp_result_in_worker;
use crate::arb_engine::lifecycle::register_path;
use crate::arb_engine::solve_cycle::{PathTimesHeap, SolveCycleShared};
use crate::arb_engine::{ArbitrageEngine, BlockMetadata};
use crate::bot_core::{TickInfo, V4PoolKey};
use ::degenbot_solvers::mixed::{MixedPath, SolvePathResult};
use alloy::primitives::U256;
use hashbrown::HashMap;
use std::sync::Arc;

/// The clamp-skip census must name every non-twin family exactly once, and
/// leave the CL/V2 families unmapped (they own the twin arm).
#[test]
fn clamp_skip_kind_names_every_non_twin_family() {
    use crate::arb_engine::solve_cycle::clamp_skip_kind;
    use crate::telemetry::error_kind;
    use ::degenbot_solvers::mixed::HopType;

    assert_eq!(clamp_skip_kind(HopType::V2), None);
    assert_eq!(clamp_skip_kind(HopType::V3), None);
    assert_eq!(clamp_skip_kind(HopType::V4), None);
    assert_eq!(
        clamp_skip_kind(HopType::SolidlyStable),
        Some(error_kind::CLAMP_SKIP_SOLIDLY_STABLE)
    );
    assert_eq!(
        clamp_skip_kind(HopType::BalancerWeighted),
        Some(error_kind::CLAMP_SKIP_BALANCER_WEIGHTED)
    );
    assert_eq!(
        clamp_skip_kind(HopType::BalancerStable),
        Some(error_kind::CLAMP_SKIP_BALANCER_STABLE)
    );
    assert_eq!(
        clamp_skip_kind(HopType::CurveStableswap),
        Some(error_kind::CLAMP_SKIP_CURVE_STABLESWAP)
    );
}
/// Narrow single-position V4 pool (±60 ticks, 1e6 liquidity) + a one-hop
/// path: the over-fed committed input is the empty-march class. Returns
/// (engine, `path_id`, the to_solve-aligned pool-ref snapshot).
fn overfed_v4_engine() -> (ArbitrageEngine, u64, Vec<std::sync::Arc<MixedPath>>) {
    use crate::arb_engine::PoolTickCoverage;
    use crate::bot_core::RegisterV4PoolParams;
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
        })
        .expect("V4 registration failed");
    let path_id = register_path(
        &mut engine,
        vec![
            ::degenbot_solvers::mixed::PoolHop {
                pool_id: v2,
                zero_for_one: true,
            },
            ::degenbot_solvers::mixed::PoolHop {
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
    core: Arc<crate::bot_core::state_lock::StateLock<crate::bot_core::BotState>>,
    pool_refs: Vec<std::sync::Arc<MixedPath>>,
) -> Arc<SolveCycleShared> {
    Arc::new(SolveCycleShared {
        core,
        pool_refs,
        worker_clamp: true,
        inline_sim: None,
        solve_block: 0,
        epoch: 0,
        metadata: BlockMetadata::default(),
        runtime: ::degenbot_solvers::runtime::SolveRuntimeConfig::default(),
        gate_capture: None,
        walk_memo: Arc::new(::degenbot_solvers::mobius_v3_int::WalkMemo::new(
            false, false,
        )),
        prefix_cache: Arc::new(::degenbot_solvers::profit_envelope::PrefixCache::new()),
        min_profit: ::alloy::primitives::U256::ZERO,
        capture: None,
        capture_mixed: None,
        path_times: parking_lot::Mutex::new(PathTimesHeap::new()),
        gate_total: parking_lot::Mutex::new(
            ::degenbot_solvers::profit_envelope::GateStats::default(),
        ),
        solve_cpu_us: std::sync::atomic::AtomicU64::new(0),
        walk_pieces_total: std::sync::atomic::AtomicU64::new(0),
        walk_sims_total: std::sync::atomic::AtomicU64::new(0),
        walk_word_steps_total: std::sync::atomic::AtomicU64::new(0),
        walk_refine_sims_total: std::sync::atomic::AtomicU64::new(0),
        walk_ternary_total: std::sync::atomic::AtomicU64::new(0),
        walk_grid_total: std::sync::atomic::AtomicU64::new(0),
        sims_recorder: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        gate_recorder: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        test_solve_delay: None,
        #[cfg(test)]
        test_solve_panic: None,
    })
}
/// The engine clamp and the WORKER clamp are the same computation from
/// two call sites: byte-identical result + twin count on identical input.
#[test]
fn worker_clamp_matches_engine_clamp_bit_for_bit() {
    use ::degenbot_solvers::mixed::MixedPoolRef;
    let (engine, path_id, pool_refs) = overfed_v4_engine();
    let mk = || {
        let committed = U256::from(1u128) << 120;
        SolvePathResult {
            optimal_input: U256::from(1_000_000_000u64),
            profit: U256::from(1_000u64),
            hop_outputs: vec![committed, committed],
            consumed_inputs: vec![committed, committed],
            state_nonces: vec![],
            solver_pool_states: Vec::new(),
        }
    };
    let (mut r_engine, mut r_worker) = (mk(), mk());
    let twins_engine = engine
        .cycle
        .clamp_cl_hop_capacity(path_id, &mut r_engine, &engine.registry);
    assert!(twins_engine > 0, "premise: the over-fed input must clamp");
    assert!(
        r_engine.consumed_inputs[1] < U256::from(1u128) << 120,
        "premise: the V4 hop input clamp fired"
    );
    let ctx = worker_probe_ctx(Arc::clone(engine.core()), pool_refs);
    let twins_worker = clamp_result_in_worker(&ctx, 0, path_id, &mut r_worker);
    assert_eq!(twins_worker, twins_engine, "twin count must match");
    assert_eq!(r_engine, r_worker, "clamped result must be byte-identical");
    // The pool-ref SNAPSHOT path (worker side) is exercised; the MixedPoolRef _ unused is intentional.
    let _: Vec<Vec<MixedPoolRef>> = Vec::new();
}
// The merge honors the worker's twin report: twins > 0 = the result is
// already clamp-committed (no second clip); twins = 0 = the merge clips
// the over-fed input itself (the legacy path — bit-identical).
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
    engine.cycle.merge_one_result(
        42,
        &metadata,
        path_id,
        mk(),
        0,
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
    engine.cycle.merge_one_result(
        43,
        &metadata,
        path_id,
        mk(),
        0,
        None,
        &engine.registry,
        &mut engine.delivery,
    );
    assert!(
        !engine.cycle.inline_payloads.contains_key(&path_id),
        "a payload-less re-merge must drop the stale payload"
    );
}
#[test]
fn merge_reports_worker_twins_and_never_reclips() {
    let (mut engine, path_id, pool_refs) = overfed_v4_engine();
    let metadata = BlockMetadata::default();
    let overfed = || {
        let committed = U256::from(1u128) << 120;
        SolvePathResult {
            optimal_input: U256::from(1_000_000_000u64),
            profit: U256::from(1_000u64),
            hop_outputs: vec![committed, committed],
            consumed_inputs: vec![committed, committed],
            state_nonces: vec![],
            solver_pool_states: Vec::new(),
        }
    };
    // Worker arm: clamp once (the worker report = committed truth), then
    // merge with twins > 0 — the stored result stays byte-identical.
    let mut worker_result = overfed();
    let ctx = worker_probe_ctx(Arc::clone(engine.core()), pool_refs);
    let twins = clamp_result_in_worker(&ctx, 0, path_id, &mut worker_result);
    assert!(twins > 0, "premise: worker clamp fired");
    let committed = worker_result.clone();
    engine.cycle.merge_one_result(
        42,
        &metadata,
        path_id,
        worker_result,
        twins,
        None,
        &engine.registry,
        &mut engine.delivery,
    );
    {
        let stored = engine.cycle.results.get(&path_id).expect("worker-merged");
        assert_eq!(
            stored.consumed_inputs, committed.consumed_inputs,
            "twins>0 must not re-clip the committed inputs"
        );
        assert_eq!(stored.profit, committed.profit, "profit untouched on skip");
    }
    // Legacy arm (twins=0): the merge clips the over-fed V4 hop input
    // itself (index 1 — the V2 hop has no input clamp by design).
    let legacy = overfed();
    let pre = legacy.consumed_inputs[1];
    engine.cycle.merge_one_result(
        42,
        &metadata,
        path_id,
        legacy,
        0,
        None,
        &engine.registry,
        &mut engine.delivery,
    );
    let stored = engine.cycle.results.get(&path_id).expect("legacy-merged");
    assert_ne!(
        stored.consumed_inputs[1], pre,
        "twins=0 must run the merge-site clamp"
    );
}
// ----------------- RKXN5Z / IJUBV3: bundle.simulate span hygiene -----------------
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
        engine.cycle.merge_one_result(
            42,
            &metadata,
            path_id,
            mk(),
            0,
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
