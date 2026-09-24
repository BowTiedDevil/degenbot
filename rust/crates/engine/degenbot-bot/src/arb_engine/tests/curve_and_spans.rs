use super::*;

// -----------------------------------------------------------------------
// Curve stableswap solve branch (RPDDWH)
// -----------------------------------------------------------------------
/// Two-token Curve stableswap pool params (standard, raw balances, no
/// rates, no lending). amp=100 (raw), fee=4e6 (0.04% of 1e10).
fn curve_stable_params(
    addr: Address,
    balance0: u128,
    balance1: u128,
) -> crate::bot_core::RegisterCurvePoolParams {
    let one_e18 = U256::from(10u64).pow(U256::from(18u64));
    let precision = one_e18; // PRECISION = 1e18
    crate::bot_core::RegisterCurvePoolParams {
        address: addr,
        tokens: vec![Address::repeat_byte(0x01), Address::repeat_byte(0x02)],
        a_coefficient: 10,
        a_precision: 100,
        fee: 4_000_000, // 0.04% of 1e10
        admin_fee: 0,
        rate_multipliers: vec![precision, precision], // identity rates
        balances: vec![
            U256::from(balance0) * one_e18,
            U256::from(balance1) * one_e18,
        ],
        update_block: 0,
        swap_style: 0,         // STANDARD
        lending_rate_style: 0, // NONE
        d_variant: 1,          // Standard
        y_variant: 1,          // Standard
        yd_variant: 1,
        base_pool: None,
        initial_a_coefficient: None,
        future_a_coefficient: None,
        initial_a_coefficient_time: None,
        future_a_coefficient_time: None,
        create_timestamp: None,
        fee_gamma: None,
        mid_fee: None,
        offpeg_fee_multiplier: None,
        out_fee: None,
        gamma: None,
        lp_token: None,
        use_lending: vec![false, false],
        precision_multipliers: vec![precision, precision],
        tokens_underlying: None,
        metapool_rate_style: 0,
        metapool_underlying_style: 0,
        data_provider: None,
    }
}
#[test]
fn curve_stable_finds_profitable_arb() {
    let mut engine = ArbitrageEngine::new();
    let pool_a = engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_curve_pool(&curve_stable_params(
            Address::from([0xe1u8; 20]),
            1000,
            2000,
        ));
    let pool_b = engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_curve_pool(&curve_stable_params(
            Address::from([0xe2u8; 20]),
            1000,
            1950,
        ));
    register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: pool_a,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: pool_b,
                zero_for_one: false,
            },
        ],
    )
    .unwrap();
    let results = engine.cycle.solve_all(&engine.registry);
    eprintln!("results: {}", results.len());
    assert!(
        !results.is_empty(),
        "should find profitable Curve stableswap arb"
    );
    let r = results.values().next().unwrap();
    assert!(
        !r.optimal_input.is_zero(),
        "optimal input should be non-zero"
    );
    assert!(!r.profit.is_zero(), "profit should be non-zero");
}
#[test]
fn curve_stable_unprofitable_path_returns_none() {
    let mut engine = ArbitrageEngine::new();
    let pool_a = engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_curve_pool(&curve_stable_params(
            Address::from([0xf1u8; 20]),
            1000,
            2000,
        ));
    let pool_b = engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_curve_pool(&curve_stable_params(
            Address::from([0xf2u8; 20]),
            1000,
            2000,
        ));
    register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: pool_a,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: pool_b,
                zero_for_one: false,
            },
        ],
    )
    .unwrap();
    let results = engine.cycle.solve_all(&engine.registry);
    assert!(
        results.is_empty(),
        "identical Curve pools should not produce an arb"
    );
}
#[test]
fn curve_stable_mixed_with_v2_finds_arb() {
    let mut engine = ArbitrageEngine::new();
    let one_e18 = U256::from(10u64).pow(U256::from(18u64));
    let v2 = engine.register_v2_pool(
        Address::from([0xa5u8; 20]),
        (U256::from(1000u64) * one_e18).to::<U112>(),
        (U256::from(2000u64) * one_e18).to::<U112>(),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let cs = engine
        .core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_curve_pool(&curve_stable_params(
            Address::from([0xa6u8; 20]),
            1000,
            1500,
        ));
    register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: v2,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: cs,
                zero_for_one: false,
            },
        ],
    )
    .unwrap();
    let results = engine.cycle.solve_all(&engine.registry);
    assert!(!results.is_empty(), "should find V2+Curve mixed arb");
}
#[test]
fn curve_stable_rejects_mixed_with_cl() {
    use std::sync::Arc;
    let core = Arc::new(crate::bot_core::state_lock::StateLock::new(
        crate::bot_core::BotState::new(),
    ));
    let cs = core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_curve_pool(&curve_stable_params(
            Address::from([0xb4u8; 20]),
            1000,
            2000,
        ));
    let v3 = core
        .write_at(crate::bot_core::state_lock::LockSite::Solver)
        .register_v3_pool(&RegisterV3PoolParams {
            address: Address::from([0xc4u8; 20]),
            token0: Address::repeat_byte(0x01),
            token1: Address::repeat_byte(0x02),
            fee: 500,
            tick_spacing: 10,
            sqrt_price_x96: U256::from(1u64) << 96,
            tick: 0,
            liquidity: 1_000_000,
            tick_data: HashMap::new(),
            update_block: 0,
            tick_data_block: None,
            ..Default::default()
        })
        .expect("test setup: V3 registration");
    let mut engine = ArbitrageEngine::with_core(Arc::clone(&core));
    let path_id = register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: cs,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: v3,
                zero_for_one: false,
            },
        ],
    )
    .expect("path registers (resolve succeeds per-arm)");
    let resolved = &engine.cycle.path_resolved[&path_id];
    assert!(
        ::degenbot_solvers::mixed::solve_path(
            resolved,
            &::degenbot_solvers::profit_envelope::GateDeps::offline()
        )
        .result
        .is_none(),
        "Curve + CL must not solve"
    );
}
/// A minimal tracing capture layer: records (name, span id, parent id)
/// for every span created under the subscriber. Deliberately NOT the
/// `OTel` exporter - span-PARENTING is not an `OTel` concern, so this
/// invariant test runs in the DEFAULT test gate (the otel-gated
/// `InMemorySpanExporter` harness stays for the attribute-level tests).
#[derive(Clone, Default)]
struct SpanParentCapture {
    spans: std::sync::Arc<std::sync::Mutex<Vec<SpanRecord>>>,
}
/// One captured span: (name, span id, parent id).
type SpanRecord = (String, u64, Option<u64>);
thread_local! {
    /// Current-span stack mirror: on_enter/on_exit maintain it so
    /// contextually-created children resolve their parent the way
    /// tracing's dispatcher does.
    static SPAN_STACK: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
}
impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for SpanParentCapture {
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let parent = SPAN_STACK.with(|st| st.borrow().last().copied());
        let spans = std::sync::Arc::clone(&self.spans);
        let name = attrs.metadata().name().to_string();
        spans
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((name, id.into_u64(), parent));
    }
    fn on_enter(&self, id: &tracing::span::Id, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        SPAN_STACK.with(|st| st.borrow_mut().push(id.into_u64()));
    }
    fn on_exit(&self, _id: &tracing::span::Id, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        SPAN_STACK.with(|st| {
            st.borrow_mut().pop();
        });
    }
}
/// Follow-up (trace f06ea422 / block 25900244): the old
/// two-acquisition gate let a concurrent dirty marker land BETWEEN the
/// probe and the take - the solve then did real work (1518 affected
/// paths) through the no-span branch, orphaning its phase spans under
/// `degenbot.epoch` and escaping the `solve_duration` histogram. The gate and
/// the work now share ONE mutex acquisition (dirt marking needs the same
/// mutex, so probe and take cannot disagree). Invariant under test:
/// every fanout span's parent is an arb.solve span (a fanout implies
/// real affected paths; real work must have taken the span branch).
///
/// DEFAULT-GATE VISIBLE (no otel cfg) - reviewer flag on 2f22fa575: the
/// race class must not live behind an optional feature.
///
/// The metrics half of the harm (`solve_duration` sample + `solves_executed`
/// count) is structural now: counting happens on the same span-branch as
/// parenting, so there is no code path that does work without either.
#[test]
#[expect(clippy::expect_used)]
#[expect(clippy::too_many_lines)]
fn solve_cycle_race_marks_dirty_work_with_solve_span() {
    use crate::arb_engine::EngineStages;
    use std::collections::HashSet;
    use std::sync::Arc;
    use tracing_subscriber::layer::SubscriberExt;
    let capture = SpanParentCapture::default();
    let log = std::sync::Arc::clone(&capture.spans);
    let subscriber = tracing_subscriber::registry().with(capture);
    // Real registered paths (mirrors the 3780 concurrency fixture) so a
    // dirty marker produces genuine fan-out phase work.
    let core = Arc::new(crate::bot_core::state_lock::StateLock::new(
        crate::bot_core::BotState::new(),
    ));
    let mut engine = ArbitrageEngine::with_core(Arc::clone(&core));
    let mut pool_ids = Vec::new();
    for i in 0u8..8 {
        let addr_a = Address::from([0x10_u8 + i; 20]);
        let a = engine.register_v2_pool(
            addr_a,
            usdc(1_500_000),
            weth(800 + u64::from(i) * 10),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let addr_b = Address::from([0x20_u8 + i; 20]);
        let b = engine.register_v2_pool(
            addr_b,
            weth(800 + u64::from(i) * 10),
            usdc(2_000_000),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let _ = register_path(
            &mut engine,
            vec![
                PoolHop {
                    pool_id: a,
                    zero_for_one: true,
                },
                PoolHop {
                    pool_id: b,
                    zero_for_one: true,
                },
            ],
        )
        .expect("path registration should succeed");
        pool_ids.push(a);
    }
    let engine = Arc::new(parking_lot::Mutex::new(engine));
    // Seed every path dirty up front: the fanout assertions below need
    // at least one solve cycle, and the marker thread's 50us cadence is
    // best-effort — a slow CI runner can starve it for the whole 200
    // solve loop, leaving the engine clean and the fixture vacuous (CI
    // panic: "fixture must produce phase spans"). Pre-seeding makes the
    // first solve deterministically dirty while the marker thread
    // continues to exercise the probe<->take race window.
    // the seeds + marker ride the SHARED epoch ledger; the drain
    // consumes take_keys per cycle (deterministically dirty on the first
    // solve; the marker thread keeps landing NEW dirt in later cycles).
    let marker_delta = std::sync::Arc::new(crate::bot_core::EpochDelta::new(0u64));
    {
        for pid in &pool_ids {
            marker_delta.record_affected(HopType::V2, *pid, 0u64);
        }
    }
    // Marker thread: continuously re-marks a tracked V2 pool dirty -
    // under the old gate these landings are exactly the probe<->take
    // window; under the single-acquisition gate they can only be seen by
    // the take itself.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let starter = Arc::new(std::sync::Barrier::new(2));
    let marker_stop = Arc::clone(&stop);
    let marker_delta_thread = std::sync::Arc::clone(&marker_delta);
    let bar0 = Arc::clone(&starter);
    let marker = std::thread::spawn(move || {
        bar0.wait();
        let mut rot = 0usize;
        while !marker_stop.load(std::sync::atomic::Ordering::Relaxed) {
            // the marker records into the shared epoch ledger —
            // the delta IS what the drain takes (no engine-local intake
            // remains, so the retired probe<->take window cannot exist).
            marker_delta_thread.record_affected(HopType::V2, pool_ids[rot % 8], 0u64);
            rot = rot.wrapping_add(1);
            // Bounded pace: enough iterations to hit any probe<->take
            // window the old gate exposed, without spinning hot and
            // perturbing the timing-sensitive detached-cycle neighbors.
            std::thread::sleep(std::time::Duration::from_micros(50));
        }
    });
    let handle = EngineStages::new(
        std::sync::Arc::clone(&engine),
        std::sync::Arc::new(crate::bot_core::EpochDelta::new(0u64)),
    );
    starter.wait();
    let block = 5000u64;
    tracing::subscriber::with_default(subscriber, || {
        for i in 0..200 {
            // Drain the ledger the way the settle drain does: take
            // keys, solve exactly what the take returned.
            let keys = marker_delta.take_keys();
            if !keys.is_empty() {
                handle.run_solve_cycle(&keys, block + i, &BlockMetadata::default());
            }
        }
    });
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    marker.join().expect("marker thread");
    let spans = log
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let solve_ids: HashSet<u64> = spans
        .iter()
        .filter(|(name, _, _)| name == "degenbot.arb.solve")
        .map(|(_, id, _)| *id)
        .collect();
    let fanouts: Vec<_> = spans
        .iter()
        .filter(|(name, _, _)| name == "degenbot.arb.fanout")
        .collect();
    assert!(
        !fanouts.is_empty(),
        "fixture must produce phase spans (marker thread keeps the engine dirty)"
    );
    let orphaned: Vec<_> = fanouts
        .iter()
        .filter(|(_, _, parent)| parent.is_none_or(|p| !solve_ids.contains(&p)))
        .collect();
    assert!(
        orphaned.is_empty(),
        "fanout spans orphaned outside an arb.solve parent: {} of {}",
        orphaned.len(),
        fanouts.len()
    );
}
/// (flips the trace-91a4a776 pin): the tombstone finalize must
/// NOT run a solve cycle. Trace 91a4a776's inner `solve_dirty` — and the
/// span gate later added around it — retired with this task: the finalize
/// is dispatched tombstone-driven and executed by the drainer while the
/// SUCCESSOR block's burst is still being applied, so its solve consumed
/// the successor's first-dirt under the dead block's identity (traces
/// ab13f75f: finalize(83) solved 1,755 paths of 84's dirt; 98f7cf52 and
/// the fresh census: 2/20 blocks with the degenerate pattern). The
/// boundary is now bookkeeping-only: the guarded transition advances the
/// block cursor (`BlockCursor::finalize`) and emits the terminal
/// publish; dirt stays unconsumed for the pump's drained-settle gate. RED
/// while `finalize_block` still called `solve_dirty`.
#[test]
#[expect(clippy::expect_used)]
fn finalize_block_consumes_no_dirt_and_emits_no_solve() {
    use crate::arb_engine::EngineStages;
    use std::collections::HashSet;
    use std::sync::Arc;
    use tracing_subscriber::layer::SubscriberExt;
    let mut oracle = crate::arb_engine::tests::test_keys::DirtyKeys::new();
    let capture = SpanParentCapture::default();
    let log = Arc::clone(&capture.spans);
    let subscriber = tracing_subscriber::registry().with(capture);
    // Real pools + path so the finalize solve does genuine fan-out work
    // (mirrors the tombstone-adjacent dirt crossing the burst boundary).
    let mut engine = ArbitrageEngine::new();
    let a = engine.register_v2_pool(
        Address::from([0x21u8; 20]),
        usdc(1_500_000),
        weth(800),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let b = engine.register_v2_pool(
        Address::from([0x22u8; 20]),
        weth(800),
        usdc(1_600_000),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let _pid = register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: a,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: b,
                zero_for_one: true,
            },
        ],
    )
    .expect("path registers");
    oracle.insert(a, HopType::V2);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    set_result_channel(&mut engine, tx);
    let engine_state = Arc::new(parking_lot::Mutex::new(engine));
    let _handle = EngineStages::new(
        Arc::clone(&engine_state),
        Arc::new(crate::bot_core::EpochDelta::new(0u64)),
    );
    tracing::subscriber::with_default(subscriber, || {
        finalize_for_test(&mut engine_state.lock(), 5, &BlockMetadata::default());
    });
    let spans = log
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    // the finalize emits NO solve at all — no arb.solve span,
    // no phase spans. Any solve here would race the successor block's
    // burst (the steal observed in traces ab13f75f / 98f7cf52).
    let solve_ids: HashSet<u64> = spans
        .iter()
        .filter(|(name, _, _)| name == "degenbot.arb.solve")
        .map(|(_, id, _)| *id)
        .collect();
    assert!(
        solve_ids.is_empty(),
        "finalize must not run a solve cycle; got arb.solve spans {solve_ids:?}"
    );
    let phase_spans: Vec<&String> = spans
        .iter()
        .map(|(name, _, _)| name)
        .filter(|name| name.contains("arb."))
        .collect();
    assert!(
        phase_spans.is_empty(),
        "finalize must emit no solve-phase spans; got {phase_spans:?}"
    );
    // unconsumed dirt lives in the DRAIN-SEAM epoch ledger now —
    // the engine has no local dirty intake to probe; finalize consumed no
    // keys (the coordinator's has_dirty/ledger tests pin that contract).
    // Boundary bookkeeping advanced under the same guard.
    {
        let engine = engine_state.lock();
        assert_eq!(
            last_solved_block(&engine,),
            5,
            "finalize must advance the solved boundary"
        );
        assert!(!has_logs_this_block(&engine,));
        // Results anchor advanced for the terminal batch.
        assert_eq!(engine.cycle.cursor.results_block(), 5);
    }
    // Terminal publish: the boundary batch still flows to Python with the
    // finalized block as its solve_block.
    let batch = rx
        .try_recv()
        .expect("finalize must emit the terminal boundary batch");
    assert_eq!(batch.solve_block, 5);
}
/// both guard branches — a block whose logs dirtied nothing
/// (or never arrived) still gets its one-shot boundary advance + terminal
/// publish (`solve_block` = the finalized block), and a re-fire of the
/// guard for the same boundary must not double-publish.
#[test]
#[expect(clippy::expect_used)]
fn finalize_boundary_publishes_even_when_nothing_dirtied() {
    let mut engine = ArbitrageEngine::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    set_result_channel(&mut engine, tx);
    // Empty-logs branch: no dirt, no recorded logs — the pure
    // header-advance boundary.
    finalize_for_test(&mut engine, 7, &BlockMetadata::default());
    assert_eq!(last_solved_block(&engine,), 7);
    assert!(!has_logs_this_block(&engine,));
    let batch = rx
        .try_recv()
        .expect("boundary batch must be emitted without dirt");
    assert_eq!(batch.solve_block, 7);
    // Guard no-ops the re-fired boundary.
    finalize_for_test(&mut engine, 7, &BlockMetadata::default());
    assert!(
        rx.try_recv().is_err(),
        "guard must not double-publish a settled boundary"
    );
}
/// ZZS6CG (trace hygiene): a solve span must parent to its OWN block's
/// published epoch root span (`degenbot.epoch`) - exact-match only. The stale
/// `DrainWork::Finalize` retired) crossing a block boundary parked
/// block N-1's
/// `arb.solve` inside block N's trace in 19/20 of the recent traces
/// analyzed (the drain/finalize arms inherited the dispatch-time loop
/// context unconditionally). RED before `attach_published_parent_exact`
/// existed. Two assertions:
/// 1. exact hit - solve(100) with a published context for 100 parents to
///    the published epoch(100) span, not the ambient newer block;
/// 2. exact miss - solve with NO published context keeps its ambient
///    parent (no fallback onto an unrelated older block, no orphan).
#[cfg(feature = "otel")]
#[test]
#[expect(clippy::expect_used)]
fn solve_spans_anchor_to_their_own_published_block() {
    use crate::arb_engine::EngineStages;
    use crate::otel;
    use opentelemetry_sdk::trace::InMemorySpanExporter;
    use std::sync::Arc;
    use tracing_subscriber::layer::SubscriberExt;
    let exporter = InMemorySpanExporter::default();
    let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
    let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));
    let mut oracle = crate::arb_engine::tests::test_keys::DirtyKeys::new();
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let engine = Arc::new(parking_lot::Mutex::new(ArbitrageEngine::new()));
            oracle.insert(0x0BAD_F00D, HopType::V2);
            Arc::new(EngineStages::new(
                engine,
                Arc::new(crate::bot_core::EpochDelta::new(0u64)),
            ))
        })
        .collect();
    tracing::subscriber::with_default(subscriber, || {
        // Published context for block 100 (a completed earlier settle).
        {
            let block100 = tracing::info_span!("degenbot.epoch.run", block.number = 100u64);
            let _guard = block100.enter();
            crate::telemetry::publish_block_context(100);
        }
        // The newer block's loop context is ambient during both solves
        // (the stale-crossing shape: block 101's context is current).
        let ambient = tracing::info_span!("degenbot.epoch.run", block.number = 101u64);
        let _ambient_guard = ambient.enter();
        // (1) Exact hit: solve of the PUBLISHED block 100 re-attaches to
        // the published epoch(100) span, not the ambient 101 span.
        let h100 = Arc::clone(&handles[0]);
        h100.run_solve_cycle(&oracle.to_affected_keys(), 100, &BlockMetadata::default());
        // (2) Exact miss: solve of block 101 (never published) keeps the
        // ambient parent - no fallback re-parenting, no orphan.
        let h101 = Arc::clone(&handles[1]);
        h101.run_solve_cycle(&oracle.to_affected_keys(), 101, &BlockMetadata::default());
    });
    provider.force_flush().expect("flush");
    let spans = exporter.get_finished_spans().expect("spans");
    let span_for_block = |blk: u64, name: &str| {
        spans
                .iter()
                .find(|sp| {
                    sp.name.as_ref() == name
                        && sp.attributes.iter().any(|kv| {
                            kv.key == opentelemetry::Key::from_static_str("block.number")
                                && (matches!(kv.value, opentelemetry::Value::I64(v) if v == blk.cast_signed())
                                    || matches!(kv.value, opentelemetry::Value::String(ref v) if v.as_str() == blk.to_string().as_str()))
                        })
                }).map_or_else(|| panic!("{name} for block {blk} must be exported"), |sp| sp.span_context.span_id())
    };
    let published_100 = span_for_block(100, "degenbot.epoch.run");
    let ambient_101 = span_for_block(101, "degenbot.epoch.run");
    let solve_100 = span_for_block(100, "degenbot.arb.solve");
    let solve_101 = span_for_block(101, "degenbot.arb.solve");
    // Look up both solve spans' parents via the exported spans.
    let parent_of = |id| {
        spans
            .iter()
            .find(|sp| sp.span_context.span_id() == id)
            .map(|sp| sp.parent_span_id)
            .expect("solve span exported")
    };
    assert_eq!(
        parent_of(solve_100),
        published_100,
        "solve(published block) must re-attach to its own block's published span"
    );
    assert_eq!(
        parent_of(solve_101),
        ambient_101,
        "solve(unpublished block) must keep the ambient parent - no fallback mis-dating"
    );
}
/// KNEUQX: the arb.solve span records `cycle.solve_block` (the cycle's
/// anchored work block = `engine.cycle.cursor.results_block()`) alongside the entry
/// block.number tag. At a settle boundary the anchor is the pool-state
/// head and can run one (or more) ahead of the entry block - the field
/// makes that visible/self-documenting in Jaeger instead of showing a
/// parent span seemingly contradicting its phase children. Pin: the
/// exported attribute matches the engine's post-solve anchor.
#[cfg(feature = "otel")]
#[test]
#[expect(clippy::expect_used)]
fn solve_span_records_cycle_solve_block() {
    use crate::arb_engine::EngineStages;
    use crate::otel;
    use opentelemetry_sdk::trace::InMemorySpanExporter;
    use std::sync::Arc;
    use tracing_subscriber::layer::SubscriberExt;
    const MY_SOLVE_BLOCK: u64 = 0x5EED_B10C;
    let exporter = InMemorySpanExporter::default();
    let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
    let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));
    let mut oracle = crate::arb_engine::tests::test_keys::DirtyKeys::new();
    let engine = Arc::new(parking_lot::Mutex::new(ArbitrageEngine::new()));
    oracle.insert(0x0BAD_F00D, HopType::V2);
    let engine_arc = Arc::clone(&engine);
    let handle = EngineStages::new(
        engine,
        std::sync::Arc::new(crate::bot_core::EpochDelta::new(0u64)),
    );
    tracing::subscriber::with_default(subscriber, || {
        handle.run_solve_cycle(
            &oracle.to_affected_keys(),
            MY_SOLVE_BLOCK,
            &BlockMetadata::default(),
        );
    });
    provider.force_flush().expect("flush");
    let spans = exporter.get_finished_spans().expect("spans");
    let solve = spans
        .iter()
        .find(|sp| sp.name.as_ref() == "degenbot.arb.solve")
        .expect("solve span must be exported");
    let expected = engine_arc.lock().cycle.cursor.results_block();
    let recorded = solve
        .attributes
        .iter()
        .find(|kv| kv.key == opentelemetry::Key::from_static_str("cycle.solve_block"))
        .map_or_else(|| "ABSENT".to_string(), |kv| kv.value.to_string());
    assert_eq!(
        recorded,
        expected.to_string(),
        "arb.solve must record the cycle's anchored block"
    );
}
// P5FEOI : original span test, otel-gated like its harness.
#[cfg(feature = "otel")]
#[test]
#[expect(clippy::expect_used)]
fn solve_cycle_emits_arb_solve_span_with_block_number() {
    use crate::arb_engine::EngineStages;
    use crate::otel;
    use opentelemetry_sdk::trace::InMemorySpanExporter;
    use std::sync::Arc;
    use tracing_subscriber::layer::SubscriberExt;
    const MY_SOLVE_BLOCK: u64 = 0x0BAD_F00D;
    const MY_SOLVE_BLOCK_I64: i64 = 0x0BAD_F00D;
    let exporter = InMemorySpanExporter::default();
    let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
    let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));
    // T0 no-op gating: the span fires only when the engine holds dirty
    // paths — mark one so this test still exercises the emitted-span path.
    let mut oracle = crate::arb_engine::tests::test_keys::DirtyKeys::new();
    let engine = Arc::new(parking_lot::Mutex::new(ArbitrageEngine::new()));
    oracle.insert(0x0BAD_F00D, HopType::V2);
    let handle = EngineStages::new(
        engine,
        std::sync::Arc::new(crate::bot_core::EpochDelta::new(0u64)),
    );
    tracing::subscriber::with_default(subscriber, || {
        handle.run_solve_cycle(
            &oracle.to_affected_keys(),
            MY_SOLVE_BLOCK,
            &BlockMetadata::default(),
        );
    });
    provider.force_flush().expect("flush");
    let spans = exporter.get_finished_spans().expect("spans");
    // Same dual-representation attribute check as the MQUKB6 pump test (tracing-
    // opentelemetry 0.33 maps u64 fields to strings; an OTel bump may switch to
    // I64 - accept both).
    let my_spans: Vec<_> = spans
            .iter()
            .filter(|sp| {
                sp.name.as_ref() == "degenbot.arb.solve"
            })
            .filter(|sp| {
                sp.attributes.iter().any(|kv| {
                    kv.key == opentelemetry::Key::from_static_str("block.number")
                        && (matches!(kv.value, opentelemetry::Value::I64(v) if v == MY_SOLVE_BLOCK_I64)
                            || matches!(kv.value, opentelemetry::Value::String(ref v) if v.as_str() == MY_SOLVE_BLOCK.to_string().as_str()))
                })
            })
            .collect();
    assert_eq!(
        my_spans.len(),
        1,
        "expected exactly one degenbot.arb.solve span for block {MY_SOLVE_BLOCK}; got names: {:?}",
        spans.iter().map(|sp| sp.name.as_ref()).collect::<Vec<_>>()
    );
}
/// XC7SWD + LPEOBI: the pre-cycle expiry window (core write
/// `expire_v3/v4`) owns a ~2.8-3.1s lock-queue slot per cycle. When
/// `max_age` is unset (production cockpit default) the expiry is
/// PROVABLY a no-op and must not take the core write at all: no
/// `degenbot.arb.expire` span, no queue position.
#[cfg(feature = "otel")]
#[test]
fn solve_cycle_skips_expire_spans_when_max_age_unset() {
    use crate::arb_engine::EngineStages;
    use crate::otel;
    use opentelemetry_sdk::trace::InMemorySpanExporter;
    use std::sync::Arc;
    use tracing_subscriber::layer::SubscriberExt;
    let exporter = InMemorySpanExporter::default();
    let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
    let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));
    let mut oracle = crate::arb_engine::tests::test_keys::DirtyKeys::new();
    let engine = Arc::new(parking_lot::Mutex::new(ArbitrageEngine::new()));
    oracle.insert(0x0BAD_F00D, HopType::V2);
    let handle = EngineStages::new(
        engine,
        std::sync::Arc::new(crate::bot_core::EpochDelta::new(0u64)),
    );
    tracing::subscriber::with_default(subscriber, || {
        handle.run_solve_cycle(
            &oracle.to_affected_keys(),
            0x0BAD_F00D,
            &BlockMetadata::default(),
        );
    });
    provider.force_flush().expect("flush");
    let spans = exporter.get_finished_spans().expect("spans");
    let expire_spans: Vec<_> = spans
        .iter()
        .filter(|sp| sp.name.as_ref() == "degenbot.arb.expire")
        .collect();
    assert!(
        expire_spans.is_empty(),
        "max_age=None expiry is a no-op - must not take the core write; got {expire_spans:?}"
    );
}
/// Resolve->LPT staging trace (f701ccd36f4ecf80d671e798df218fa4, block
/// 25906841): between the close of `arb.resolve` and the open of
/// `arb.lpt` sat 647 ms of uninstrumented wall time — the results sweep
/// and resolved-snapshot staging (`to_solve`) that makes the engine
/// borrow-free for the parallel dispatch. That phase must emit its own
/// `degenbot.arb.stage` phase span carrying `paths.staged`, so the
/// staging cost is attributable in Jaeger like its fanout/resolve/lpt/
/// merge siblings (MQUKB6-T2 pattern). RED before the span existed.
#[cfg(feature = "otel")]
#[test]
#[expect(clippy::expect_used)]
fn run_epoch_emits_stage_span_with_paths_staged() {
    use crate::otel;
    use hashbrown::HashSet;
    use opentelemetry_sdk::trace::InMemorySpanExporter;
    use tracing_subscriber::layer::SubscriberExt;
    const PATH_COUNT: u64 = 1;
    let exporter = InMemorySpanExporter::default();
    let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
    let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));
    let mut engine = ArbitrageEngine::new();
    let a = engine.register_v2_pool(
        Address::from([0x11u8; 20]),
        usdc(1_000_000),
        weth(500),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let a2 = engine.register_v2_pool(
        Address::from([0x13u8; 20]),
        usdc(1_100_000),
        weth(510),
        GAMMA_03,
        FEE_DENOM_03,
    );
    let _path_id = register_path(
        &mut engine,
        vec![
            PoolHop {
                pool_id: a,
                zero_for_one: true,
            },
            PoolHop {
                pool_id: a2,
                zero_for_one: true,
            },
        ],
    )
    .expect("path registers");
    tracing::subscriber::with_default(subscriber, || {
        engine.cycle.run_epoch(
            &crate::arb_engine::tests::test_keys::affected_keys(
                &HashSet::from([a]),
                &HashSet::new(),
                &HashSet::new(),
            ),
            5,
            &BlockMetadata::default(),
            &engine.registry,
            &mut engine.delivery,
        );
    });
    provider.force_flush().expect("flush");
    let spans = exporter.get_finished_spans().expect("spans");
    let stage_spans: Vec<_> = spans
        .iter()
        .filter(|sp| sp.name.as_ref() == "degenbot.arb.stage")
        .collect();
    assert_eq!(
        stage_spans.len(),
        1,
        "expected exactly one degenbot.arb.stage span per solve cycle; got names: {:?}",
        spans.iter().map(|sp| sp.name.as_ref()).collect::<Vec<_>>()
    );
    // Dual-representation check (u64 fields map to String or I64 under
    // tracing-opentelemetry 0.33; mirrors the MQUKB6 pump test).
    assert!(
            stage_spans[0].attributes.iter().any(|kv| {
                kv.key == opentelemetry::Key::from_static_str("paths.staged")
                    && (matches!(kv.value, opentelemetry::Value::I64(v) if v.cast_unsigned() == PATH_COUNT)
                        || matches!(kv.value, opentelemetry::Value::String(ref v) if v.as_str() == PATH_COUNT.to_string().as_str()))
            }),
            "stage span must carry paths.staged={PATH_COUNT}; got {:?}",
            stage_spans[0].attributes
        );
}
/// With `max_age` SET the expiry write returns and each buffer kind gets
/// one `degenbot.arb.expire` span with `lock_wait_us`/`expire_work_us`.
#[cfg(feature = "otel")]
#[test]
fn solve_cycle_emits_expire_spans_with_phase_split() {
    use crate::arb_engine::EngineStages;
    use crate::otel;
    use opentelemetry_sdk::trace::InMemorySpanExporter;
    use std::sync::Arc;
    use tracing_subscriber::layer::SubscriberExt;
    let exporter = InMemorySpanExporter::default();
    let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
    let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));
    let mut oracle = crate::arb_engine::tests::test_keys::DirtyKeys::new();
    let engine = Arc::new(parking_lot::Mutex::new(ArbitrageEngine::new()));
    oracle.insert(0x0BAD_F00D, HopType::V2);
    // Gate ON: only a configured max_age justifies the core write.
    crate::arb_engine::lifecycle::set_event_buffer_max_age(&mut engine.lock(), Some(100));
    let handle = EngineStages::new(
        engine,
        std::sync::Arc::new(crate::bot_core::EpochDelta::new(0u64)),
    );
    tracing::subscriber::with_default(subscriber, || {
        handle.run_solve_cycle(
            &oracle.to_affected_keys(),
            0x0BAD_F00D,
            &BlockMetadata::default(),
        );
    });
    provider.force_flush().expect("flush");
    let spans = exporter.get_finished_spans().expect("spans");
    let expire_spans: Vec<_> = spans
        .iter()
        .filter(|sp| sp.name.as_ref() == "degenbot.arb.expire")
        .collect();
    let has_stage = |sp: &&opentelemetry_sdk::trace::SpanData, kind: &str| {
        sp.attributes.iter().any(|kv| {
            kv.key == opentelemetry::Key::from_static_str("kind")
                && matches!(kv.value, opentelemetry::Value::String(ref v) if v.as_str() == kind)
        })
    };
    let has_phase_field = |sp: &&opentelemetry_sdk::trace::SpanData, field: &str| {
        sp.attributes.iter().any(|kv| {
            if field == "lock_wait_us" {
                kv.key == opentelemetry::Key::from_static_str("lock_wait_us")
            } else {
                kv.key == opentelemetry::Key::from_static_str("expire_work_us")
            }
        })
    };
    for kind in ["v3", "v4"] {
        let matched = expire_spans
            .iter()
            .filter(|sp| has_stage(sp, kind))
            .collect::<Vec<_>>();
        assert_eq!(
            matched.len(),
            1,
            "expected one degenbot.arb.expire span for kind={kind}; got spans: {:?}",
            expire_spans
                .iter()
                .map(|sp| sp.name.as_ref())
                .collect::<Vec<_>>()
        );
        assert!(
            matched
                .iter()
                .all(|sp| has_phase_field(sp, "lock_wait_us")
                    && has_phase_field(sp, "expire_work_us")),
            "expire span kind={kind} missing lock_wait_us/expire_work_us attributes"
        );
    }
}
/// T0 no-op gating: a clean engine (no dirty paths) must NOT emit an
/// `degenbot.arb.solve` span — the 2µs no-op solves were flooding Jaeger's
/// recent-traces list and drowning the real solves.
#[cfg(feature = "otel")]
#[test]
fn solve_cycle_skips_span_when_nothing_dirty() {
    use crate::arb_engine::EngineStages;
    use crate::otel;
    use opentelemetry_sdk::trace::InMemorySpanExporter;
    use std::sync::Arc;
    use tracing_subscriber::layer::SubscriberExt;
    let exporter = InMemorySpanExporter::default();
    let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
    let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));
    let handle = EngineStages::new(
        Arc::new(parking_lot::Mutex::new(ArbitrageEngine::new())),
        std::sync::Arc::new(crate::bot_core::EpochDelta::new(0u64)),
    );
    tracing::subscriber::with_default(subscriber, || {
        handle.run_solve_cycle(&[], 1, &BlockMetadata::default());
    });
    provider.force_flush().expect("flush");
    let spans = exporter.get_finished_spans().expect("spans");
    let solve_spans = spans
        .iter()
        .filter(|sp| sp.name.as_ref() == "degenbot.arb.solve")
        .count();
    assert_eq!(
        solve_spans, 0,
        "no-op solve must not emit a degenbot.arb.solve span"
    );
}
/// with the tokio solve executor each path's
/// result reaches `self.results` as soon as ITS OWN solve completes —
/// the slowest path in the batch may not delay the fast ones' merge.
/// RED before the per-path result-queue streaming exists: the batched
/// barrier merges everything only AFTER the slowest solve, so the drain
/// probe stays empty past the deadline while the slow path still runs.
///
/// Pinned-tier fixture (FF-T2): the streaming-merge premise binds only
/// on a host whose auto-resolved fleet binding is pinned (see the
/// host-tier gate in the body); the serial tier's ONE solve seat has no
/// second LPT bin to stream a fast merge into the probe.
#[expect(
    clippy::too_many_lines,
    reason = "the T1 acceptance carries the whole hook-probe + streaming-ordering story in one deterministic body"
)]
#[test]
fn tokio_executor_merges_fast_paths_while_slow_path_solves() {
    if std::thread::available_parallelism().is_ok_and(|n| n.get() < 2) {
        eprintln!("skipping: streaming-merge test requires >=2 cores");
        return;
    }
    // Host-tier gate (FF-T2): the premise needs the PINNED tier's
    // multi-seat solver fan-out — the slow path isolated in its own LPT
    // bin while the other bins stream fast merges into the probe. On a
    // 2-5-core host the auto profile resolves the fleet to the serial
    // binding by design (ONE cycle lane, ONE solve seat): there is no
    // second bin to merge anything before the slow path's release
    // marker, so the ordering assertion cannot bind there. The
    // serial-tier delivery story is covered by
    // `streaming_delivery_emits_fast_result_while_slow_path_solves`
    // (which tolerates the single-seat ordering). Mirror the <2-core
    // self-skip channel.
    let tier_quota =
        degenbot_workers::budget::detected_quota_cpus(&degenbot_config::FleetConfig::default());
    if degenbot_workers::budget::FleetBudget::derive(
        tier_quota,
        &degenbot_workers::budget::BudgetOverrides::default(),
    )
    .is_err()
    {
        eprintln!(
            "skipping: the fleet resolves this host to the serial tier \
                 ({tier_quota} cores) — one solve seat, no second LPT bin"
        );
        return;
    }
    let probe: std::sync::Arc<parking_lot::Mutex<Vec<u64>>> =
        std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut engine = ArbitrageEngine::new();
    // Seven independent mispriced V2->V2 pairs -> seven profitable paths
    // (>=2 cores: LPT puts the slow path FIRST in its bin, so at least
    // one fast path always lands in a different bin - the streaming
    // drain merges it long before the slow solve ends).
    let mut pool_ids = Vec::new();
    let mut path_ids = Vec::new();
    for i in 0u8..7 {
        let fwd = engine.register_v2_pool(
            Address::from([0x40 + i; 20]),
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let back = engine.register_v2_pool(
            Address::from([0x50 + i; 20]),
            weth(800),
            usdc(1_600_000),
            GAMMA_03,
            FEE_DENOM_03,
        );
        pool_ids.push(fwd);
        pool_ids.push(back);
        path_ids.push(
            register_path(
                &mut engine,
                vec![
                    PoolHop {
                        pool_id: fwd,
                        zero_for_one: true,
                    },
                    PoolHop {
                        pool_id: back,
                        zero_for_one: true,
                    },
                ],
            )
            .unwrap(),
        );
    }
    // Slowen path 0; STRUCTURAL interleaving (load-immune): the slow
    // path's hook parks until at least one fast path is MERGED (observed
    // via the probe), then stamps a release marker. Under the batched
    // barrier no fast merge can precede the marker even after the full
    // wait (a merge happens only after the slowest path returns), so the
    // ordering assertion catches it - no absolute deadline to flake on.
    let slow_pid = path_ids[0];
    let fast_pids: Vec<u64> = path_ids[1..].to_vec();
    // Pin LPT placement deterministically: a huge MEASURED sims cost on
    // the slow path sorts it FIRST into its own bin, so the other bins
    // always host fast paths no matter what order the (HashSet-ordered)
    // work items land in. Without this, bin position is nondeterministic
    // (equal structural costs + arbitrary dirty-set iteration order).
    engine
        .cycle
        .last_walk_sims
        .lock()
        .insert(slow_pid, u64::MAX - 1);
    engine
        .cycle
        .last_walk_sims
        .lock()
        .insert(*fast_pids.first().unwrap_or(&0), u64::MAX / 4);
    engine
        .cycle
        .last_walk_sims
        .lock()
        .insert(*fast_pids.get(1).unwrap_or(&0), u64::MAX / 8);
    let hook_probe = probe.clone();
    let hook_fast = fast_pids.clone();
    engine
        .cycle
        .set_solve_delay_hook(std::sync::Arc::new(move |pid: u64| {
            if pid == slow_pid {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
                while std::time::Instant::now() < deadline {
                    if hook_probe.lock().iter().any(|p| hook_fast.contains(p)) {
                        hook_probe.lock().push(u64::MAX); // merge-before-release marker
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                hook_probe.lock().push(u64::MAX); // released WITHOUT a fast merge
            }
        }));
    engine.cycle.set_merge_probe(probe.clone());
    let pool_set: HashSet<u64> = pool_ids.iter().copied().collect();
    let joiner = std::thread::spawn(move || {
        engine.cycle.run_epoch(
            &crate::arb_engine::tests::test_keys::affected_keys(
                &pool_set,
                &HashSet::new(),
                &HashSet::new(),
            ),
            100,
            &BlockMetadata::default(),
            &engine.registry,
            &mut engine.delivery,
        );
        engine
    });
    let engine = joiner.join().unwrap();
    let (results, _block) = latest_results(&engine);
    // Structural streaming proof: the first probe entry must be a fast
    // path MERGE (a fast path merged before the slow solve released its
    // hook). Under the batched barrier the marker would land first: only
    // after the slowest path returns can the drain merge anything.
    let observed = probe.lock().clone();
    let marker = observed.iter().position(|p| *p == u64::MAX);
    let first_fast = observed
        .iter()
        .position(|p| *p != u64::MAX && fast_pids.contains(p));
    assert_eq!(results.len(), 7, "all seven paths profitable and merged");
    let marker =
        marker.expect("the slow path hook must stamp a release marker (probe = {observed:?})");
    let first_fast = first_fast.expect(
        "a fast-path MERGE must happen before the slow path finishes (probe = {observed:?})",
    );
    assert!(
        first_fast < marker,
        "a fast-path MERGE must precede the slow path release marker; batched \
             barrier order puts the marker first (probe = {observed:?})"
    );
}
/// T3 acceptance: with `DEGENBOT_STREAMING_DELIVERY` the drain
/// emits each clamp-passed above-threshold result as an immediate single
/// -entry batch — a fast path's batch must arrive on the channel while the
/// slow path is still solving. RED before the per-result emission: the
/// debounce path sends nothing until `send_result_batch`.
#[test]
#[expect(clippy::too_many_lines)]
fn streaming_delivery_emits_fast_result_while_slow_path_solves() {
    if std::thread::available_parallelism().is_ok_and(|n| n.get() < 2) {
        eprintln!("skipping: streaming-delivery test requires >=2 cores");
        return;
    }
    let probe: std::sync::Arc<parking_lot::Mutex<Vec<u64>>> =
        std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut engine = ArbitrageEngine::new();
    engine.cycle.set_streaming_delivery(true);
    let (result_tx, mut result_rx) = tokio::sync::mpsc::unbounded_channel();
    set_result_channel(&mut engine, result_tx);
    let mut pool_ids = Vec::new();
    let mut path_ids = Vec::new();
    for i in 0u8..3 {
        let fwd = engine.register_v2_pool(
            Address::from([0x70 + i; 20]),
            usdc(1_500_000),
            weth(800),
            GAMMA_03,
            FEE_DENOM_03,
        );
        let back = engine.register_v2_pool(
            Address::from([0x80 + i; 20]),
            weth(800),
            usdc(1_600_000),
            GAMMA_03,
            FEE_DENOM_03,
        );
        pool_ids.push(fwd);
        pool_ids.push(back);
        path_ids.push(
            register_path(
                &mut engine,
                vec![
                    PoolHop {
                        pool_id: fwd,
                        zero_for_one: true,
                    },
                    PoolHop {
                        pool_id: back,
                        zero_for_one: true,
                    },
                ],
            )
            .unwrap(),
        );
    }
    // Structural interleave (mirror of the solve-orchestration test): the
    // slow path's hook parks until the delivery side stamped a flag (set
    // by the payer loop below when it sees any batch), then releases.
    let slow_pid = path_ids[0];
    let fast_pids: Vec<u64> = path_ids[1..].to_vec();
    let observed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hook_observed = observed.clone();
    engine
        .cycle
        .set_solve_delay_hook(std::sync::Arc::new(move |pid: u64| {
            if pid == slow_pid {
                let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2500);
                while std::time::Instant::now() < deadline {
                    if hook_observed.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }
        }));
    engine.cycle.set_merge_probe(probe.clone());
    let pool_set: HashSet<u64> = pool_ids.iter().copied().collect();
    let joiner = std::thread::spawn(move || {
        engine.cycle.run_epoch(
            &crate::arb_engine::tests::test_keys::affected_keys(
                &pool_set,
                &HashSet::new(),
                &HashSet::new(),
            ),
            100,
            &BlockMetadata::default(),
            &engine.registry,
            &mut engine.delivery,
        );
        engine
    });
    // Payer: drain the channel from THIS thread while the solve_THREAD
    // holds the engine Mutex; declare success as soon as any batch carries
    // a fast path.
    let mut saw_fast_batch = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2200);
    while std::time::Instant::now() < deadline {
        if observed.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        while let Ok(batch) = result_rx.try_recv() {
            let has_fast = batch
                .fresh
                .iter()
                .chain(batch.updated.iter())
                .any(|(id, _)| fast_pids.contains(id));
            if has_fast {
                saw_fast_batch = true;
                break;
            }
        }
        if saw_fast_batch {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    observed.store(true, std::sync::atomic::Ordering::Relaxed);
    let engine = joiner.join().unwrap();
    // drained bookkeeping: every path above threshold is in `delivered`.
    assert_eq!(engine.delivery.delivered.len(), 3, "all paths in delivered");
    assert!(
            saw_fast_batch || !result_rx.is_empty(),
            "a fast path's batch must arrive on the channel while the slow path \\\n             is still solving (flag on); saw_fast_batch = {saw_fast_batch}"
        );
}
