use super::*;
use crate::bot_core::stage_machine::QuiesceParams;
use degenbot_config::QuiesceMode;
use degenbot_decoders::v2_sync_decoder::V2_SYNC_TOPIC;
use degenbot_decoders::v3_mint_burn_decoder::{V3_BURN_TOPIC, V3_MINT_TOPIC};
use degenbot_decoders::v3_swap_decoder::V3_SWAP_TOPIC;
use degenbot_ingestion::{build_backfill_filter, PoolEvent};
use degenbot_rpc::provider::AlloyProvider;
use std::sync::atomic::AtomicU64;
use std::sync::Mutex;

#[cfg(test)]
impl BlockPump {
    /// Test-only constructor with an injected `AlloyProvider` (typically a
    /// mock transport) + a `Bot`/`sink`/`reorg_coordinator`. Lets tests drive
    /// [`BlockPump::run_with_stream`] from a deterministic synthetic
    /// `WsEvent` stream without a live RPC connection. The provider is only
    /// touched on the 60s-timeout backfill path — tests that avoid timeouts
    /// and block gaps never invoke it.
    #[must_use]
    pub fn for_test(
        bot: Arc<Bot>,
        engine: Arc<dyn StageHandlers>,
        control: Arc<dyn PumpControl>,
        reorg_coordinator: Arc<crate::bot_core::reorg_coordinator::ReorgCoordinator>,
        provider: Arc<AlloyProvider>,
        shutdown: Arc<AtomicBool>,
    ) -> Self {
        // Per-pump completeness opt-out (see the field doc): the dispatcher's
        // strict decode-miss fault follows this OFF stance so the synthetic
        // tombstone logs never trip it.
        bot.dispatcher().set_strict_decode_fault(false);
        Self {
            bot,
            engine,
            control,
            reorg_coordinator,
            // (5WTYYQ) The injected mock provider rides inside the ingestion
            // transport handle; tests that avoid timeouts never touch it.
            ingestor: WsIngestor::with_provider(provider),
            shutdown,
            watchdog: Watchdog::new(),
            stage_max_age: Duration::from_secs(
                crate::bot_core::stage_telemetry::STAGE_MAX_AGE_SECS,
            ),
            header_ms: std::sync::atomic::AtomicU64::new(0),
            // Same per-pump opt-out for the WS-delivery completeness cross-check:
            // default-ON in production, deterministically OFF in tests so the
            // synthetic log streams (which use relevant-topic logs as pure block
            // tombstones) never trip a spurious eth_getLogs comparison/abort.
            ws_completeness_enabled: false,
            // Fixed-debounce posture exactly as the historical tests pin it
            // (no ambient-env reads); adaptive-mode tests override via
            // `set_quiesce_for_test`. The fixed 50 mirrors the retired
            // `debounce_ms` field this constructor used to set.
            quiesce_params: QuiesceParams::fixed(50),
            // Production default (PWPPAZ T2) — finite test streams end before
            // the slice deadline, so existing quiesce tests are unaffected;
            // the gap-stream tests below set the field explicitly.
            early_slice_ms: 25,
        }
    }

    /// Test-only override of the quiesce-estimator parameters (BM35LK) —
    /// per-pump field override (not env) so tests stay immune to the
    /// environment. Applied to the FSM when the run loop starts.
    pub fn set_quiesce_for_test(&mut self, params: QuiesceParams) {
        self.quiesce_params = params;
    }

    /// Test-only access to the shared `Bot` arc (FD7NFG tests inject
    /// `snapshot_seed_block` to drive the `S≥W` / `S=0` no-op branches).
    #[must_use]
    pub fn bot_arc_for_test(&self) -> Arc<Bot> {
        Arc::clone(&self.bot)
    }

    /// Drive the resume loop with a synthetic `WsEvent` stream. Test-only
    /// seam over [`run_with_stream`](Self::run_with_stream) so tests need not
    /// reach the private method name.
    pub async fn run_test_loop(
        &mut self,
        combined: stream::BoxStream<'static, WsEvent>,
        first_observed_block: u64,
    ) {
        self.run_with_stream(combined, first_observed_block).await;
    }

    /// Test-only override of the header-staleness watchdog window (JIABO3).
    /// Lets tests drive the watchdog `tokio::time::interval` to a sub-second
    /// period instead of the 30s production default, so the select-arm fire
    /// is observable without a 30s wait.
    pub fn set_header_staleness_for_test(&mut self, staleness: Duration) {
        self.watchdog.header_staleness = staleness;
    }

    /// Test-only override of the early-slice window (PWPPAZ T2) — per-pump
    /// field override (not env) so tests stay immune to the environment.
    pub fn set_early_slice_ms_for_test(&mut self, ms: u64) {
        self.early_slice_ms = ms;
    }

    /// Test-only override of the logs-subscription liveness window
    /// (the INVERSE watchdog: headers fresh but no log for N seconds).
    /// Lets tests drive the alarm threshold to a sub-second value instead of
    /// the 60s production default. Pair with `set_header_staleness_for_test`
    /// so the staleness tick elapses often AND the silence threshold is short.
    pub fn set_log_silence_for_test(&mut self, silence: Duration) {
        self.watchdog.log_silence = silence;
    }

    /// Count of logs-silence alarms fired since the pump started (test
    /// observable for the logs-subscription liveness watchdog — incremented
    /// once per silence episode, re-armed when the next `WsEvent::Log`
    /// resumes the sub).
    #[must_use]
    pub fn log_silence_alarm_count(&self) -> u64 {
        self.watchdog.silence_alarm_count()
    }
}

/// A `StageHandlers` test double (AGENTS.md: `Fake` prefix, no mocking).
///
/// Records every `on_finalize` / `on_publish` (the retired `on_send` —
/// the Published-row delivery flush) / `on_solve` invocation with the
/// `(block, metadata)` pair the pump passed, so tests can assert the
/// *block N's* result batch carries *block N's* metadata — the VTWCIG
/// contract. Behaves as an empty engine (no dirty paths, no state).
struct FakeStageEngine {
    finalized: Mutex<Vec<(u64, BlockMetadata)>>,
    sent: Mutex<Vec<BlockMetadata>>,
    drained: Mutex<Vec<(u64, BlockMetadata)>>,
    notified: Mutex<Vec<(u64, BlockMetadata)>>,
    /// Records every `set_last_solved_block` call (JIABO3: proves the
    /// header-staleness watchdog reached `handle_timeout_eager` because
    /// only the backfill path + the header anchor call this — the watchdog
    /// is the sole path that backfills past the stream's observed block).
    solved: Mutex<Vec<u64>>,
    last_processed: AtomicU64,
    /// Test knob for the active-block promotion RED test (BO5FBS):
    /// when `true`, `has_dirty_paths()` reports dirty so the top-of-loop
    /// `on_drain` path fires. Default `false` keeps every existing test's
    /// no-drain behavior unchanged.
    dirty: AtomicBool,
    /// `record_logs_this_block` call count (T4 pairing pin, epic
    /// O3HW7E): the LEZJAS bookkeeping write must fire exactly when the
    /// FSM's `on_log_applied` ran for an applied forward log.
    logs_recorded: std::sync::atomic::AtomicUsize,
    /// `pump_ended` recorded (incident 2026-08-20 stream-death test).
    pump_ended: std::sync::atomic::AtomicBool,
    /// Candidate-2 seam pin : the loud close must arrive
    /// exactly once through the `PumpControl` surface, never through the
    /// stage seam. These split counters let the pin tell the two apart.
    pump_control_ends: std::sync::atomic::AtomicUsize,
    stage_seam_ends: std::sync::atomic::AtomicUsize,
    /// PWPPAZ T2: virtual-time stamps for each `on_drain` (paired with
    /// `drained`), read via `drained_at`.
    drained_at: Mutex<Vec<tokio::time::Instant>>,
}

impl FakeStageEngine {
    fn new(last_processed: Option<u64>) -> Self {
        Self {
            finalized: Mutex::new(Vec::new()),
            sent: Mutex::new(Vec::new()),
            drained: Mutex::new(Vec::new()),
            notified: Mutex::new(Vec::new()),
            solved: Mutex::new(Vec::new()),
            last_processed: AtomicU64::new(last_processed.unwrap_or(0)),
            dirty: AtomicBool::new(false),
            logs_recorded: std::sync::atomic::AtomicUsize::new(0),
            pump_ended: std::sync::atomic::AtomicBool::new(false),
            pump_control_ends: std::sync::atomic::AtomicUsize::new(0),
            stage_seam_ends: std::sync::atomic::AtomicUsize::new(0),
            drained_at: Mutex::new(Vec::new()),
        }
    }

    /// PWPPAZ T2: virtual-time stamps paired with `drained_blocks()`.
    fn drained_at(&self) -> Vec<tokio::time::Instant> {
        self.drained_at.lock().unwrap().clone()
    }

    /// Set the test dirty flag (see `dirty` field doc).
    fn set_dirty(&self, dirty: bool) {
        self.dirty.store(dirty, Ordering::Relaxed);
    }

    /// Number of `record_logs_this_block` calls the pump routed here
    /// (T4 pairing pin).
    /// True once the pump notified stream death (incident 2026-08-20).
    fn pump_ended(&self) -> bool {
        self.pump_ended.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Candidate-2 pin : closes driven through the target
    /// `PumpControl` surface. Must be exactly 1 after the WS-streams-ended
    /// branch fires — this is the behavior the pin exists to prove.
    fn pump_control_ends(&self) -> usize {
        self.pump_control_ends
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Candidate-2 pin : closes driven through the stage
    /// seam's pump-ended poke (removed at T2). Must stay 0.
    fn stage_seam_ends(&self) -> usize {
        self.stage_seam_ends
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn logs_recorded(&self) -> usize {
        self.logs_recorded
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Quiesce publishes the sink received (`on_send` call log).
    fn sends(&self) -> Vec<BlockMetadata> {
        self.sent.lock().unwrap().clone()
    }

    fn drained_blocks(&self) -> Vec<u64> {
        self.drained
            .lock()
            .unwrap()
            .iter()
            .map(|(b, _)| *b)
            .collect()
    }
}

impl StageHandlers for FakeStageEngine {
    fn on_streaming_complete(
        &self,
        _work: &crate::bot_core::stage_handlers::StreamingComplete<'_>,
    ) -> Result<crate::bot_core::stage_handlers::QuiesceOutcome, crate::bot_core::StageError> {
        Ok(crate::bot_core::stage_handlers::QuiesceOutcome {
            verdict: crate::bot_core::stage_handlers::QuiesceVerdict::Settled,
        })
    }
    fn on_resolve(
        &self,
        _work: &Resolve<'_>,
    ) -> Result<crate::bot_core::AffectedPaths, crate::bot_core::StageError> {
        Ok(crate::bot_core::AffectedPaths::default())
    }
    fn on_solve(
        &self,
        work: &Solve,
    ) -> Result<crate::bot_core::SolveOutcome, crate::bot_core::StageError> {
        // Faithful to the old `SolveCoordinator::on_drain` recording
        // behavior: record + advance the drained cursor so
        // `last_processed_block()` reflects the drained block (the
        // anchoring `resume` relies on — see
        // `resume_anchors_to_subscribe_block`).
        let block = work.ctx.block();
        self.drained
            .lock()
            .unwrap()
            .push((block, *work.ctx.metadata()));
        // PWPPAZ T2: virtual-time dispatch stamp (start_paused tests
        // assert the slice fired at its deadline, not at burst end).
        self.drained_at
            .lock()
            .unwrap()
            .push(tokio::time::Instant::now());
        self.last_processed.store(block, Ordering::Relaxed);
        Ok(crate::bot_core::SolveOutcome::default())
    }
    fn on_simulate(
        &self,
        _work: &crate::bot_core::Simulate,
    ) -> Result<crate::bot_core::SimulateOutcome, crate::bot_core::StageError> {
        Ok(crate::bot_core::SimulateOutcome::default())
    }
    fn on_gate(
        &self,
        _work: &crate::bot_core::Gate,
    ) -> Result<crate::bot_core::GateOutcome, crate::bot_core::StageError> {
        Ok(crate::bot_core::GateOutcome::default())
    }
    fn on_publish(
        &self,
        work: &Publish,
    ) -> Result<crate::bot_core::PublishOutcome, crate::bot_core::StageError> {
        self.sent.lock().unwrap().push(*work.ctx.metadata());
        Ok(crate::bot_core::PublishOutcome { published: None })
    }
    fn on_finalize(
        &self,
        work: &Finalize,
    ) -> Result<crate::bot_core::FinalizeOutcome, crate::bot_core::StageError> {
        self.finalized
            .lock()
            .unwrap()
            .push((work.ctx.block(), *work.ctx.metadata()));
        Ok(crate::bot_core::FinalizeOutcome {
            cutoff: work.ctx.epoch(),
        })
    }
    fn on_rewind(
        &self,
        _work: &crate::bot_core::Rewind,
    ) -> Result<crate::bot_core::RewindOutcome, crate::bot_core::StageError> {
        Ok(crate::bot_core::RewindOutcome {
            restored_to: crate::bot_core::Epoch::at(0),
        })
    }
}

/// Candidate-2 seam pin : the TARGET `PumpControl` surface.
/// At HEAD this cannot compile (`PumpControl` lands in T2); that is the
/// intended red. The fake records the close here so the pin proves the
/// pump actually drove the loud close through the control seam rather than
/// returning silently. The `StageHandlers` impl above is what HEAD uses; T2
/// deletes that poke and this impl becomes the only close path.
impl crate::bot_core::PumpControl for FakeStageEngine {
    fn has_dirty_paths(&self) -> bool {
        self.dirty.load(Ordering::Relaxed)
    }
    fn set_last_solved_block(&self, solved: Epoch) {
        self.solved.lock().unwrap().push(solved.block());
    }
    fn set_solve_anchor(&self, _anchor: Epoch) {}
    fn record_logs_this_block(&self) {
        self.logs_recorded
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    fn last_processed_block(&self) -> Option<Epoch> {
        let v = self.last_processed.load(Ordering::Relaxed);
        (v != 0).then(|| Epoch::at(v))
    }
    fn notify_block(&self, block: u64, metadata: &BlockMetadata) {
        self.notified.lock().unwrap().push((block, *metadata));
    }
    fn on_pump_ended(&self) {
        self.pump_ended
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.pump_control_ends
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Build a `BlockPump` whose provider is an `alloy` mock transport (never
/// hit on the no-timeout / no-gap test paths) and whose sink is a
/// `FakeStageEngine` that records metadata calls. Returns the pump + the
/// sink handle (for inspection). Offline + deterministic.
fn pump_for_test(last_processed: Option<u64>) -> (BlockPump, Arc<FakeStageEngine>) {
    use alloy::network::Ethereum as NetEth;
    use alloy::providers::{Provider, ProviderBuilder};
    // `alloy_transport::mock::{Asserter, MockTransport}` — unfeatured (no
    // `mock` feature flag needed under `alloy = { features = ["full"] }`).
    // The asserter's queue is never drained because the test paths avoid
    // provider calls (no 60s timeout, no block gaps).
    use alloy::rpc::client::ClientBuilder;
    use alloy::transports::mock::{Asserter, MockTransport};

    let asserter = Asserter::new();
    let client = ClientBuilder::default().transport(MockTransport::new(asserter), true);
    // `.erased()` yields a `DynProvider<Ethereum>` (implements
    // `Provider<Ethereum>`), matching `AlloyProvider::from_provider`'s
    // `Arc<dyn Provider<Ethereum>>` parameter — same shape as the live
    // `build_provider` path.
    let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
    let provider = Arc::new(AlloyProvider::from_provider(
        Arc::new(dyn_provider) as Arc<dyn alloy::providers::Provider<NetEth>>
    ));

    let bot = Arc::new(Bot::new(1));
    let reorg = Arc::new(crate::bot_core::reorg_coordinator::ReorgCoordinator::new(
        Arc::clone(&bot),
    ));
    let shutdown = Arc::new(AtomicBool::new(false));
    let sink = Arc::new(FakeStageEngine::new(last_processed));
    let pump = BlockPump::for_test(bot, sink.clone(), sink.clone(), reorg, provider, shutdown);
    (pump, sink)
}

/// Same shape as `pump_for_test` but also returns the mock transport's
/// `Asserter` (JIABO3) so tests can queue `eth_blockNumber` /
/// `eth_getLogs` responses reached by the header-staleness watchdog's
/// `handle_timeout_eager`. `pump_for_test` discards the asserter; this
/// variant exposes it.
fn pump_for_test_sink_and_asserter(
    last_processed: Option<u64>,
) -> (
    BlockPump,
    Arc<FakeStageEngine>,
    alloy::transports::mock::Asserter,
    Arc<AtomicBool>,
) {
    use alloy::network::Ethereum as NetEth;
    use alloy::providers::{Provider, ProviderBuilder};
    use alloy::rpc::client::ClientBuilder;
    use alloy::transports::mock::{Asserter, MockTransport};

    let asserter = Asserter::new();
    let client = ClientBuilder::default().transport(MockTransport::new(asserter.clone()), true);
    let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
    let provider = Arc::new(AlloyProvider::from_provider(
        Arc::new(dyn_provider) as Arc<dyn alloy::providers::Provider<NetEth>>
    ));
    let bot = Arc::new(Bot::new(1));
    let reorg = Arc::new(crate::bot_core::reorg_coordinator::ReorgCoordinator::new(
        Arc::clone(&bot),
    ));
    let shutdown = Arc::new(AtomicBool::new(false));
    let sink = Arc::new(FakeStageEngine::new(last_processed));
    let pump = BlockPump::for_test(
        bot,
        sink.clone(),
        sink.clone(),
        reorg,
        provider,
        Arc::clone(&shutdown),
    );
    (pump, sink, asserter, shutdown)
}

/// PWPPAZ T2 gap-stream builder: the block-101 header, then `logs` V2
/// Sync logs spaced `gap_ms` apart (a real burst shape: one header, a
/// multi-event log burst), then END. Under `start_paused` the sleeps
/// advance in virtual time, so the slice deadline (25ms < gap 40ms <
/// debounce 50ms) resolves deterministically mid-burst. Logs must NOT
/// reset the slice window — only headers do (one slice per BLOCK window).
fn gap_burst_stream(logs: u64, gap_ms: u64) -> stream::BoxStream<'static, WsEvent> {
    use alloy::primitives::{Address as A, U256};
    use stream::StreamExt;
    let pool = A::from([0xccu8; 20]);
    stream::unfold(0u64, move |i| {
        let pool = pool;
        async move {
            if i > 0 {
                tokio::time::sleep(Duration::from_millis(gap_ms)).await;
            }
            let ev = if i == 0 {
                WsEvent::BlockHeader {
                    number: 101,
                    timestamp: 101_000,
                    base_fee_per_gas: Some(1_000_000_001),
                    gas_used: 10_000_001,
                    gas_limit: 30_000_001,
                }
            } else {
                WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
                    pool,
                    U256::from(1_000),
                    U256::from(2_000),
                    101,
                    false,
                )))
            };
            (i <= logs).then_some((ev, i + 1))
        }
    })
    .boxed()
}

/// PWPPAZ T2 — designed first-slice: with a gapped multi-event burst
/// (headers every 40ms; gap > slice deadline 25ms, gap < settle debounce
/// 50ms), the gate dispatches ONE early Drain at ~first-dirty + 25ms
/// (mid-burst, NOT at burst end), then the tail still gets its quiesce
/// settle at the newest block. RED before T2 (the gate only dispatched
/// at stream end).
/// Register the V2 pool `gap_burst_stream` logs target (mirrors
/// `solve_gate_waits_for_buffered_log_before_solving`'s fixture).
fn register_burst_pool(bot: &Arc<Bot>) {
    use alloy::primitives::{aliases::U112, Address as A};
    let arc = bot.state_arc();
    let mut core = arc.write_at(crate::bot_core::state_lock::LockSite::Pump);
    core.register_v2_pool(&RegisterV2PoolParams {
        address: A::from([0xccu8; 20]),
        token0: A::from([0xa0u8; 20]),
        token1: A::from([0xa1u8; 20]),
        reserve0: U112::from(1_000),
        reserve1: U112::from(2_000),
        fee_token0: (997, 1000),
        fee_token1: (997, 1000),
        factory: A::from([0xf0u8; 20]),
        update_block: 100,
        variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
        stable_swap: false,
        fee_denominator: None,
        ..Default::default()
    })
    .expect("test setup: V2 registration");
}

/// WAJEQP T-R1 capture layer: one record per created span (name, id,
/// parent id) plus every `record`ed field, threaded through a
/// thread-local span stack so contextually-created children resolve the
/// way tracing's dispatcher does (same pattern as the `arb_span` tests'
/// `SpanParentCapture`; thread-local rather than global so it starves no
/// once-per-process `set_global_default` slot).
type SpanList = Vec<(String, u64, Option<u64>)>;
type FieldList = Vec<(u64, String, String)>;

#[derive(Clone)]
struct ReorgSpanCapture {
    spans: std::sync::Arc<std::sync::Mutex<SpanList>>,
    fields: std::sync::Arc<std::sync::Mutex<FieldList>>,
}

impl Default for ReorgSpanCapture {
    fn default() -> Self {
        Self {
            spans: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            fields: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
}

thread_local! {
    static REORG_SPAN_STACK: std::cell::RefCell<Vec<u64>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for ReorgSpanCapture {
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let parent = REORG_SPAN_STACK.with(|st| st.borrow().last().copied());
        self.spans
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((attrs.metadata().name().to_string(), id.into_u64(), parent));
    }

    fn on_record(
        &self,
        id: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        struct Saver(Vec<(String, String)>);
        impl tracing::field::Visit for Saver {
            fn record_str(&mut self, f: &tracing::field::Field, v: &str) {
                self.0.push((f.name().to_string(), v.to_string()));
            }
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                self.0.push((f.name().to_string(), format!("{v:?}")));
            }
            fn record_u64(&mut self, f: &tracing::field::Field, v: u64) {
                self.0.push((f.name().to_string(), v.to_string()));
            }
            fn record_i64(&mut self, f: &tracing::field::Field, v: i64) {
                self.0.push((f.name().to_string(), v.to_string()));
            }
        }
        let mut saver = Saver(Vec::new());
        values.record(&mut saver);
        let sid = id.into_u64();
        self.fields
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(saver.0.into_iter().map(|(k, v)| (sid, k, v)));
    }

    fn on_enter(&self, id: &tracing::span::Id, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        REORG_SPAN_STACK.with(|st| st.borrow_mut().push(id.into_u64()));
    }

    fn on_exit(&self, _id: &tracing::span::Id, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        REORG_SPAN_STACK.with(|st| {
            st.borrow_mut().pop();
        });
    }
}

/// Drive a reorg scenario under the [`ReorgSpanCapture`] layer on a local
/// current-thread runtime (spans are created on the pump task; the test
/// thread holds the subscriber for the whole `block_on`).
fn run_reorg_stream(capture: ReorgSpanCapture, pump: &mut BlockPump, events: Vec<WsEvent>) {
    use stream::StreamExt;
    use tracing_subscriber::layer::SubscriberExt;
    // WAJEQP flake fix (BGGTEG): the tracing callsite interest cache is
    // PROCESS-GLOBAL, and a parallel subscriber-less test (same pump
    // code, no thread-local default) that executes the shared
    // `degenbot.epoch.run` macro first registers the callsite as
    // `never` for every thread — our thread-local capture then never
    // constructs the span even though the header select-arm ran. Paint
    // the cache `always` with a best-effort all-enabled global
    // registry: spans on subscriber-less threads are created and
    // dropped by the bare registry (no layers: no behavior change for
    // any other test), while this test's capture stays thread-local.
    // `let _ =` — if some earlier test already took the once-per-process
    // slot (an all-enabled registry itself), the paint is already done.
    // NOT under `--features otel`: there the once-per-process global
    // slot belongs to the `header_arms_per_block_span...` test's
    // registry+otel layer (its own `set_global_default` repaints the
    // interest cache via tracing's rebuild), and a second registry
    // would mix span-Id spaces across threads.
    #[cfg(not(feature = "otel"))]
    let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry());
    let subscriber = tracing_subscriber::registry().with(capture);
    tracing::subscriber::with_default(subscriber, || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        rt.block_on(async {
            pump.run_test_loop(stream::iter(events).boxed(), 100).await;
        });
    });
}

// -----------------------------------------------------------------
// Pump-level reorg integration (ADR-006 slice 7).
//
// `ReorgCoordinator` is covered directly in `reorg_coordinator.rs`
// (dispatch → restore_before_block → notify). What is NOT covered
// anywhere is the pump's own reorg branch in `run_with_stream`: the
// `log.removed` arm routes the log to the coordinator, cancels the
// pending debounce, continues on an in-journal-depth reorg, and shuts
// down gracefully on a too-deep reorg. These tests pin those
// pump-specific behaviors (the coordinator's restore+notify is
// asserted as the downstream observable, not re-tested for its own
// sake).
// -----------------------------------------------------------------

use crate::bot_core::{BlockContext, RegisterV2PoolParams};
use alloy::primitives::{aliases::U112, Address, Bytes, U256};
use degenbot_solvers::affected_keys::AffectedKey;
use degenbot_solvers::mixed::HopType;

/// Build a V2 `Sync` log for `pool_address` carrying
/// `(reserve0, reserve1)`, at `block_number`, with `removed` set.
/// Mirrors `reorg_coordinator.rs`'s `make_sync_log` test helper.
fn make_v2_sync_log(
    pool_address: Address,
    reserve0: U256,
    reserve1: U256,
    block_number: u64,
    removed: bool,
) -> Log {
    // Test helper: emits a raw V2 `Sync(uint112,uint112)` log as 64
    // bytes of ABI data (two 32-byte left-padded words). The decoder
    // narrows to `U112` on decode — this helper keeps the `U256` ABI
    // word shape so the bytes match on-chain log data.
    let data = {
        let mut data = Vec::with_capacity(64);
        data.extend_from_slice(&reserve0.to_be_bytes::<32>());
        data.extend_from_slice(&reserve1.to_be_bytes::<32>());
        data
    };
    let inner =
        alloy::primitives::Log::new_unchecked(pool_address, vec![V2_SYNC_TOPIC], Bytes::from(data));
    Log {
        inner,
        block_hash: None,
        block_number: Some(block_number),
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: None,
        removed,
    }
}

/// Build a V3 `Mint` log with `block_number` set. Twin of
/// `make_v3_burn_log_with_block`. Topics = [`V3_MINT_TOPIC`, owner,
/// tickLower, tickUpper]; data = abi.encode(address sender, uint128
/// amount, uint256 amount0, uint256 amount1) = 4×32 = 128 bytes
/// (matches `decode_v3_mint_log`).
fn make_v3_mint_log_with_block(
    pool_address: Address,
    tick_lower: i32,
    tick_upper: i32,
    amount: u128,
    block_number: u64,
) -> Log {
    use alloy::primitives::{I256, U128};
    let tick_to_topic = |tick: i32| {
        let i = I256::try_from(i128::from(tick)).unwrap_or(I256::ZERO);
        alloy::primitives::B256::from(i.to_be_bytes::<32>())
    };
    let owner = alloy::primitives::Address::from([0xccu8; 20]);
    let sender = alloy::primitives::Address::from([0xddu8; 20]);
    let mut amount_word = [0u8; 32];
    amount_word[16..32].copy_from_slice(&U128::from(amount).to_be_bytes::<16>());
    let mut data = Vec::with_capacity(128);
    // word 0: sender (address, right-aligned)
    data.extend_from_slice(&[0u8; 12]);
    data.extend_from_slice(sender.as_slice());
    // word 1: amount (uint128, right-aligned)
    data.extend_from_slice(&amount_word);
    // word 2: amount0 (uint256)
    data.extend_from_slice(&alloy::primitives::U256::ZERO.to_be_bytes::<32>());
    // word 3: amount1 (uint256)
    data.extend_from_slice(&alloy::primitives::U256::ZERO.to_be_bytes::<32>());
    let inner = alloy::primitives::Log::new_unchecked(
        pool_address,
        vec![
            V3_MINT_TOPIC,
            owner.into_word(),
            tick_to_topic(tick_lower),
            tick_to_topic(tick_upper),
        ],
        Bytes::from(data),
    );
    Log {
        inner,
        block_hash: None,
        block_number: Some(block_number),
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: None,
        removed: false,
    }
}

/// Build a V3 `Burn` log with `block_number` set (for backfill tests).
/// data = abi.encode(uint128 amount, uint256 amount0, uint256 amount1).
fn make_v3_burn_log_with_block(
    pool_address: Address,
    tick_lower: i32,
    tick_upper: i32,
    amount: u128,
    block_number: u64,
) -> Log {
    use alloy::primitives::{I256, U128};
    let tick_to_topic = |tick: i32| {
        let i = I256::try_from(i128::from(tick)).unwrap_or(I256::ZERO);
        alloy::primitives::B256::from(i.to_be_bytes::<32>())
    };
    let mut amount_word = [0u8; 32];
    amount_word[16..32].copy_from_slice(&U128::from(amount).to_be_bytes::<16>());
    let mut data = Vec::with_capacity(96);
    data.extend_from_slice(&amount_word);
    data.extend_from_slice(&alloy::primitives::U256::ZERO.to_be_bytes::<32>());
    data.extend_from_slice(&alloy::primitives::U256::ZERO.to_be_bytes::<32>());
    let owner = alloy::primitives::Address::from([0xccu8; 20]);
    let inner = alloy::primitives::Log::new_unchecked(
        pool_address,
        vec![
            V3_BURN_TOPIC,
            owner.into_word(),
            tick_to_topic(tick_lower),
            tick_to_topic(tick_upper),
        ],
        Bytes::from(data),
    );
    Log {
        inner,
        block_hash: None,
        block_number: Some(block_number),
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: None,
        removed: false,
    }
}

/// Register a V2 pool on a fresh `Bot`, returning `(bot, pool_id)`. Genesis
/// reserves are anchored at `update_block`, seeding the reorg journal so an
/// in-journal reorg can roll back to them.
fn bot_with_registered_v2(pool_addr: Address, update_block: u64) -> (Arc<Bot>, u64) {
    let bot = Arc::new(Bot::new(1));
    let pool_id = bot
        .state_arc()
        .write_at(crate::bot_core::state_lock::LockSite::Pump)
        .register_v2_pool(&RegisterV2PoolParams {
            address: pool_addr,
            token0: Address::from([0xa0u8; 20]),
            token1: Address::from([0xa1u8; 20]),
            reserve0: U112::from(1_000),
            reserve1: U112::from(2_000),
            fee_token0: (997, 1000),
            fee_token1: (997, 1000),
            factory: Address::from([0xf0u8; 20]),
            update_block,
            variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
            stable_swap: false,
            fee_denominator: None,
            ..Default::default()
        })
        .expect("test setup: V2 registration");
    (bot, pool_id)
}

/// Build a `BlockPump` over a caller-provided `Arc<Bot>` (rather than a
/// fresh empty `Bot::new(1)`), returning the pump + sink + the shared
/// shutdown flag so a test can assert shutdown behavior. Same mock-transport
/// provider as `pump_for_test`; test paths avoid provider calls.
fn pump_for_test_with_bot(
    bot: Arc<Bot>,
    last_processed: Option<u64>,
) -> (BlockPump, Arc<FakeStageEngine>, Arc<AtomicBool>) {
    use alloy::network::Ethereum as NetEth;
    use alloy::providers::{Provider, ProviderBuilder};
    use alloy::rpc::client::ClientBuilder;
    use alloy::transports::mock::{Asserter, MockTransport};

    let asserter = Asserter::new();
    let client = ClientBuilder::default().transport(MockTransport::new(asserter), true);
    let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
    let provider = Arc::new(AlloyProvider::from_provider(
        Arc::new(dyn_provider) as Arc<dyn alloy::providers::Provider<NetEth>>
    ));
    let reorg = Arc::new(crate::bot_core::reorg_coordinator::ReorgCoordinator::new(
        Arc::clone(&bot),
    ));
    let shutdown = Arc::new(AtomicBool::new(false));
    let sink = Arc::new(FakeStageEngine::new(last_processed));
    let pump = BlockPump::for_test(
        bot,
        sink.clone(),
        sink.clone(),
        reorg,
        provider,
        Arc::clone(&shutdown),
    );
    (pump, sink, shutdown)
}

/// Wait (with a deadline, in `rt` runtime ticks) until `cond` returns true.
/// After `run_test_loop` returns, the background drainer task may still be
/// processing the final queued work asynchronously (the sole mode since
/// B4); tests that assert on the sink must settle the drainer first.
async fn drainer_settle(cond: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !cond() {
        assert!(
            std::time::Instant::now() < deadline,
            "drainer did not settle within timeout"
        );
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
}

// -----------------------------------------------------------------
// DFQYM5: verify-mismatch drain/buffer race characterization.
//
// The bot dies at registration `verify_v3_post_drain_snapshot` with a tick
// gross mismatch: the pin reports `update_block = N` but is missing one
// Mint whose on-chain `ticks()` value changed at block N. Two candidate
// causes: (A) a Mint buffered then missed by the drain (a race the verify-
// seam FSM would close), or (B) a Mint never delivered to the buffer at
// all (a WS/decode hole no FSM can fix). These tests drive the REAL pump
// (`run_test_loop`) with a controlled V3 log feed to distinguish them.
// -----------------------------------------------------------------

/// Register a `Tracked` V3 pool on a fresh `Bot`, seed tick 7 with
/// `seed_gross`, set `Quarantined` (so live Mints buffer to the pump
/// buffer — the `build_paths` contract). Returns `(bot, pool_addr)`.
/// Tick spacing 1 so tick 7 is a valid tick.
fn bot_with_quarantined_v3_tracked(seed_gross: u128, update_block: u64) -> (Arc<Bot>, Address) {
    use crate::bot_core::{PoolTickCoverage, RegisterV3PoolParams, TickInfo};
    use alloy::primitives::U128;
    let pool_addr = Address::from([0x34u8; 20]);
    let bot = Arc::new(Bot::new(1));
    let mut tick_data = hashbrown::HashMap::new();
    tick_data.insert(
        7,
        TickInfo {
            liquidity_gross: U128::from(seed_gross),
            liquidity_net: seed_gross.cast_signed(),
            block: 0,
        },
    );
    {
        let state = bot.state_arc();
        let mut core = state.write_at(crate::bot_core::state_lock::LockSite::Pump);
        core.register_v3_pool(&RegisterV3PoolParams {
            address: pool_addr,
            token0: Address::from([0xa0u8; 20]),
            token1: Address::from([0xa1u8; 20]),
            fee: 10000,
            tick_spacing: 1,
            factory: Address::from([0xf0u8; 20]),
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
            update_block,
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
            ..Default::default()
        })
        .expect("test setup: V3 registration");
        core.set_v3_pool_quarantined(pool_addr);
    }
    (bot, pool_addr)
}

/// Build a V3 `Swap` log (tombstone trigger — its block number N+1
/// tombstones N via `observe_log`). Minimal data: the decoder reads
/// `sqrtPriceX96`, `tick`, `liquidity`, `amount0`, `amount1` from 5 words.
fn make_v3_swap_log_with_block(pool_address: Address, block_number: u64) -> Log {
    let mut data = Vec::with_capacity(160);
    // amount0 (int256), amount1 (int256), sqrtPriceX96 (uint160),
    // liquidity (uint128), tick (int24)
    data.extend_from_slice(&alloy::primitives::U256::ZERO.to_be_bytes::<32>());
    data.extend_from_slice(&alloy::primitives::U256::ZERO.to_be_bytes::<32>());
    data.extend_from_slice(&alloy::primitives::U256::from(1u128).to_be_bytes::<32>());
    data.extend_from_slice(&alloy::primitives::U256::ZERO.to_be_bytes::<32>());
    data.extend_from_slice(&alloy::primitives::U256::ZERO.to_be_bytes::<32>());
    let inner =
        alloy::primitives::Log::new_unchecked(pool_address, vec![V3_SWAP_TOPIC], Bytes::from(data));
    Log {
        inner,
        block_hash: None,
        block_number: Some(block_number),
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: None,
        removed: false,
    }
}

// -----------------------------------------------------------------
// BAMKKI: interleaving fuzz harness. Randomized (seed-deterministic)
// composition of event feeds x pool lifecycle roles, driven through the
// REAL pump, with a replay oracle. Any member of the FUWYUR family
// (lost, duplicated, or mis-staged application across the
// unregistered/quarantined/live boundaries) shows up as an oracle
// divergence instead of waiting for chain data to find it.
// -----------------------------------------------------------------

/// Tiny deterministic xorshift64* so failures print the exact seed and
/// are reproducible without external crates.
struct FuzzRng(u64);
impl FuzzRng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

const FUZZ_POOL_COUNT: usize = 3;
const FUZZ_TICKS: [i32; 3] = [-10, 7, 20];
const FUZZ_SEED_GROSS: u128 = 10_000_000_000_000_000;

/// JUCFCB/J3FMDO helper: build a `pump_for_test_with_bot` variant that
/// also returns the `Asserter` so a test can push `eth_getLogs`
/// responses and observe whether the auto-backfill path drains them.
fn pump_for_test_with_asserter(
    bot: Arc<Bot>,
    last_processed: Option<u64>,
) -> (
    BlockPump,
    Arc<FakeStageEngine>,
    Arc<AtomicBool>,
    alloy::transports::mock::Asserter,
) {
    use alloy::network::Ethereum as NetEth;
    use alloy::providers::{Provider, ProviderBuilder};
    use alloy::rpc::client::ClientBuilder;
    use alloy::transports::mock::{Asserter, MockTransport};

    let asserter = Asserter::new();
    let client = ClientBuilder::default().transport(MockTransport::new(asserter.clone()), true);
    let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
    let provider = Arc::new(AlloyProvider::from_provider(
        Arc::new(dyn_provider) as Arc<dyn alloy::providers::Provider<NetEth>>
    ));
    let reorg = Arc::new(crate::bot_core::reorg_coordinator::ReorgCoordinator::new(
        Arc::clone(&bot),
    ));
    let shutdown = Arc::new(AtomicBool::new(false));
    let sink = Arc::new(FakeStageEngine::new(last_processed));
    let pump = BlockPump::for_test(
        bot,
        sink.clone(),
        sink.clone(),
        reorg,
        provider,
        Arc::clone(&shutdown),
    );
    (pump, sink, shutdown, asserter)
}

// ==================================================================
// Pins for the candidate-2 stage-seam contract.
// These tests target the post-cutover contract; production code is
// NOT changed here. They are intentionally red until the cutover.
// ==================================================================
mod candidate2_seam_pins {
    use super::*;

    /// The pump module's source files. The pump was split from one
    /// `block_pump.rs` into a module directory, so the source-latch pins
    /// below read every part.
    fn pump_module_sources() -> [&'static str; 5] {
        [
            include_str!("../mod.rs"),
            include_str!("../lifecycle.rs"),
            include_str!("../run_loop.rs"),
            include_str!("../stages.rs"),
            include_str!("../backfill.rs"),
        ]
    }

    /// Pin 3 (RED: compile-fails until T2). The BEHAVIORAL half: drive
    /// the WS-streams-ended branch (`block_pump.rs:1772`) and prove the
    /// loud close actually fires. At the target the pump drives the close
    /// through `PumpControl::on_pump_ended`; at HEAD it still called the
    /// old direct stage-seam close, so the
    /// `pump_control_ends` count is 0 and the `stage_seam_ends` count is
    /// 1 — the assertion below fails rather than passing vacuously. The
    /// `PumpControl` reference is the compile-red (trait lands in T2).
    #[tokio::test]
    async fn candidate2_pump_ended_is_loud_through_the_one_interface() {
        let (mut pump, sink) = pump_for_test(Some(100));
        // Immediately-exhausted stream -> the Ok(None) arm: loud op_error
        // plus the close. No provider calls, no timing.
        pump.run_test_loop(stream::iter(Vec::<WsEvent>::new()).boxed(), 100)
            .await;
        assert_eq!(
                sink.pump_control_ends(),
                1,
                "the WS-streams-ended branch must drive the loud close exactly once through PumpControl"
            );
        assert_eq!(
            sink.stage_seam_ends(),
            0,
            "the close must not travel through the StageHandlers stage seam"
        );
    }

    /// Pin 3 documentation latch (NOT the substance). The behavioral
    /// assertions above are the pin; this only records the intended target
    /// shape so a regression that reintroduces the old direct stage call
    /// is named explicitly. It must never be cited as the proof.
    #[test]
    fn candidate2_pump_ended_old_stage_call_absent_documentation() {
        let sources = pump_module_sources();
        assert!(
            !sources
                .iter()
                .any(|src| src.contains(concat!("self.engine.", "on_pump_ended()"))),
            "documentation latch: the pump is intended to reach the loud close through PumpControl"
        );
    }

    /// Pin 4 (RED: runs and fails until T2). The Solved outcome carries
    /// the epoch it solved (`solved: Epoch`); `drive_solve` derives the
    /// engine cursor from that outcome instead of poking the seam at the
    /// solve edge (`block_pump.rs:1987` today).
    #[test]
    fn candidate2_drive_solve_derives_cursor_from_solve_outcome() {
        let dbg = format!("{:?}", crate::bot_core::SolveOutcome::default());
        assert!(
            dbg.contains("solved"),
            "SolveOutcome must carry `solved: Epoch`; got {dbg}"
        );
    }

    /// Pin 5 (RED: runs and fails until T2). Finalize takes no
    /// `PublishOutcome` and the pump never fabricates a default
    /// `PublishOutcome` as a carrier at the tombstones (`block_pump.rs:1633`
    /// / 2122 today).
    #[test]
    fn candidate2_finalize_and_pump_carry_no_publish_outcome() {
        let pump_sources = pump_module_sources();
        assert!(
            !pump_sources
                .iter()
                .any(|src| src.contains(concat!("PublishOutcome::", "default()"))),
            "the pump must not fabricate a default PublishOutcome carrier"
        );
        let seam_src = include_str!("../../stage_handlers.rs");
        assert!(
            !seam_src.contains("pub published: PublishOutcome"),
            "Finalize must not carry a PublishOutcome field"
        );
    }

    /// Pin 6 (RED: compile-fails until T2; the `FakeStageEngine` half
    /// of the ADR-041 completeness proof). At the target the fake
    /// implements BOTH `StageHandlers` (eight hooks) AND `PumpControl`
    /// (seven pokes). The `NoopStubEngine` sibling pin lives in
    /// `stage_handlers.rs`.
    #[test]
    fn candidate2_fakestageengine_implements_both_traits() {
        fn assert_both<T: crate::bot_core::StageHandlers + crate::bot_core::PumpControl>() {}
        assert_both::<super::FakeStageEngine>();
    }
}

mod backfill;
mod quiesce;
mod reorg;
mod resume;
mod scenarios;
mod smoke;
mod spans_exit;
mod watchdogs;
