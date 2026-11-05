//! The crawl controller's own fixtures: the transition table, the window, the
//! two latches, the operator surface, and the loud abort.
//!
//! Everything here is offline, and the units are REAL (`run_crawl_unit` over a
//! scripted graph). The cheap units answer before any RPC: a V4 hop the edge
//! carried no decodable hash for answers the bounded `V4NoHash` skip at the
//! build stage. The abort fixture spends four reads, and that is the point — a
//! V2 hop whose address is not the CREATE2 address of its own (factory, token0,
//! token1) is a family-level `IdentityMismatch`, the ADR-055 D4 loud arm.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn
)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use alloy::dyn_abi::DynSolValue;
use alloy::primitives::{address, Address, Bytes, U256};
use async_trait::async_trait;
use degenbot_core::errors::ProviderError;
use degenbot_db::{DbError, GraphEdition, PathGraphData};
use degenbot_pathfinding::PoolKind;
use hashbrown::{HashMap, HashSet};
use parking_lot::Mutex;
use tokio::sync::{mpsc, Notify};

use super::{
    transition, CrawlAbort, CrawlConfig, CrawlController, CrawlEvent, CrawlHandle, CrawlHost,
    CrawlRequestError, CrawlState, CrawlStateError, CrawlUnitFuture, CrawlUnitSeat, RunPhase,
    SeatHold, SnapshotArm,
};
use crate::arb_engine::crawl::admission::{CrawlLedger, DiscoveredHop, RegistrationAdmission};
use crate::arb_engine::crawl::discovery::{
    CrawlDiscoveryError, DiscoveredPath, DiscoverySource, Edition,
};
use crate::arb_engine::crawl::flights::BuildFlights;
use crate::arb_engine::crawl::policy::CrawlPathPolicy;
use crate::arb_engine::crawl::receipt::{CrawlReceipt, StageSplit};
use crate::arb_engine::EngineStages;
use crate::bot_core::construction_io::{ConstructionIo, NoDb, RpcConstruction};
use crate::bot_core::registration_ledger::RegistrationUnitOutcome;
use crate::bot_core::verification_retry::RetryPolicy;
use crate::bot_core::{Bot, ConstructionRoute};
use degenbot_rpc::abi;

// ── the fixture chain ───────────────────────────────────────────────────────

const CHAIN_ID: u64 = 1;

/// The two graph tokens the fixtures cycle between.
const TOKEN_A: u64 = 1;
const TOKEN_B: u64 = 2;

const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
const USDC: Address = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
const NATIVE: Address = address!("0000000000000000000000000000000000000000");

/// The `PoolManager` every fixture V4 edge names.
const V4_PM: Address = address!("3333333333333333333333333333333333333333");

/// A pool address that is NOT the CREATE2 address of its own identity — the
/// abort fixture's whole content.
const BOGUS_V2: Address = address!("4444444444444444444444444444444444444444");

const BLOCK: u64 = 18_000_000;

/// The `V4` graph-id offset the database read applies (`degenbot_db`).
const V4_OFFSET: u64 = 1 << 32;

// ── the discovery source ────────────────────────────────────────────────────

/// A scripted [`DiscoverySource`]: one graph, one edition, and the counters that
/// prove whether a sweep ran at all.
struct FixtureSource {
    data: PathGraphData,
    candidates: HashSet<u64>,
    tokens: HashMap<Address, u64>,
    edition: Mutex<Option<Edition>>,
    fail_probe: AtomicBool,
    graph_reads: AtomicUsize,
}

impl FixtureSource {
    fn new(data: PathGraphData) -> Arc<Self> {
        Arc::new(Self {
            data,
            candidates: HashSet::from_iter([TOKEN_A, TOKEN_B]),
            tokens: HashMap::from_iter([(WETH, TOKEN_A), (NATIVE, TOKEN_B)]),
            edition: Mutex::new(Some(GraphEdition {
                v2v3_count: 0,
                v2v3_max_id: 0,
                v4_count: 3,
                v4_max_id: 3,
            })),
            fail_probe: AtomicBool::new(false),
            graph_reads: AtomicUsize::new(0),
        })
    }

    fn set_edition(&self, edition: Option<Edition>) {
        *self.edition.lock() = edition;
    }

    fn fail_probe(&self) {
        self.fail_probe.store(true, Ordering::SeqCst);
    }

    /// How many sweeps read the graph (one per `DiscoverySweep::prepare`).
    fn sweeps(&self) -> usize {
        self.graph_reads.load(Ordering::SeqCst)
    }
}

impl DiscoverySource for FixtureSource {
    fn edition(&self) -> Result<Edition, CrawlDiscoveryError> {
        if self.fail_probe.load(Ordering::SeqCst) {
            return Err(CrawlDiscoveryError::Db(DbError::Decode(
                "the fixture's edition probe fails".to_owned(),
            )));
        }
        Ok((*self.edition.lock()).unwrap_or_default())
    }

    fn resolve_token_ids(
        &self,
        addresses: &[Address],
    ) -> Result<HashMap<Address, u64>, CrawlDiscoveryError> {
        Ok(addresses
            .iter()
            .filter_map(|address| self.tokens.get(address).map(|id| (*address, *id)))
            .collect())
    }

    fn candidate_tokens(&self, _kinds: &[PoolKind]) -> Result<HashSet<u64>, CrawlDiscoveryError> {
        Ok(self.candidates.clone())
    }

    fn graph_data(&self, _kinds: &[PoolKind]) -> Result<PathGraphData, CrawlDiscoveryError> {
        self.graph_reads.fetch_add(1, Ordering::SeqCst);
        Ok(self.data.clone())
    }
}

// ── the graphs ──────────────────────────────────────────────────────────────

/// `parallel` V4 pools between the two fixture tokens whose hash columns do not
/// decode: every cycle is a candidate that answers the bounded `V4NoHash` skip.
///
/// `m` parallel pools admit `m·(m-1)` ordered two-hop cycles, so the candidate
/// count is a knob rather than a luck.
fn skip_graph(parallel: u64) -> PathGraphData {
    let mut data = PathGraphData::default();
    for slot in 1..=parallel {
        let graph_id = V4_OFFSET + slot;
        data.edges.push((TOKEN_A, TOKEN_B, graph_id, PoolKind::V4));
        data.v4_lookups
            .insert(graph_id, (V4_PM, "0xnot-a-pool-hash".to_owned()));
    }
    data
}

/// One V2 pool whose on-chain identity does not match its address — the loud
/// abort fixture.
fn abort_graph() -> PathGraphData {
    let mut data = PathGraphData::default();
    // Two parallel pools, so a two-hop cycle exists; both name the same bogus
    // address, so whichever the DFS consumes aborts on its identity.
    for pool_id in [7, 8] {
        data.edges.push((TOKEN_A, TOKEN_B, pool_id, PoolKind::V2));
        data.v2v3_addresses.insert(pool_id, BOGUS_V2);
    }
    data
}

// ── the construction double ─────────────────────────────────────────────────

/// A construction-RPC double keyed per `(contract, selector)`. A missing entry
/// REVERTS, which is what makes "the fixture has no answer" a real refusal.
#[derive(Default)]
struct FakeRpc {
    responses: HashMap<(Address, [u8; 4]), Vec<u8>>,
}

impl FakeRpc {
    fn new() -> Self {
        Self::default()
    }

    fn set(&mut self, to: Address, signature: &[u8], bytes: Vec<u8>) {
        self.responses.insert((to, abi::selector(signature)), bytes);
    }
}

fn addr_word(address: Address) -> Vec<u8> {
    let mut word = vec![0u8; 12];
    word.extend_from_slice(address.as_slice());
    word
}

fn reserves_word(reserve0: u64, reserve1: u64) -> Vec<u8> {
    DynSolValue::Tuple(vec![
        DynSolValue::Uint(U256::from(reserve0), 112),
        DynSolValue::Uint(U256::from(reserve1), 112),
        DynSolValue::Uint(U256::ZERO, 32),
    ])
    .abi_encode()
}

#[async_trait]
impl RpcConstruction for FakeRpc {
    async fn get_block_number(&self) -> Result<u64, ProviderError> {
        Ok(BLOCK)
    }

    async fn get_block(
        &self,
        _block: u64,
    ) -> Result<Option<degenbot_rpc::provider::EthBlock>, ProviderError> {
        Ok(None)
    }

    async fn get_block_timestamp(&self, _block: u64) -> Result<Option<u64>, ProviderError> {
        Ok(None)
    }

    async fn get_code(
        &self,
        _address: Address,
        _block: Option<u64>,
    ) -> Result<Bytes, ProviderError> {
        Ok(Bytes::from_static(&[0x60, 0x0a]))
    }

    async fn get_balance(
        &self,
        _address: Address,
        _block: Option<u64>,
    ) -> Result<U256, ProviderError> {
        Ok(U256::ZERO)
    }

    async fn call(
        &self,
        to: Address,
        data: Bytes,
        _block: Option<u64>,
    ) -> Result<Bytes, ProviderError> {
        let selector: [u8; 4] = data[..4].try_into().unwrap();
        match self.responses.get(&(to, selector)) {
            Some(answer) => Ok(answer.clone().into()),
            None => Err(ProviderError::ExecutionReverted {
                code: -32000,
                message: "the fixture RPC has no answer for this call".to_owned(),
            }),
        }
    }
}

/// The four reads a V2 identity check makes, on the bogus address: the builder
/// resolves the family, reads the sorted tokens and the factory, and finds the
/// CREATE2 address disagrees with the hop's.
fn abort_rpc() -> FakeRpc {
    let mut rpc = FakeRpc::new();
    let dex = &degenbot_uniswap::dex_identity::UNISWAP_V2;
    rpc.set(BOGUS_V2, b"factory()", addr_word(dex.factory));
    rpc.set(BOGUS_V2, b"token0()", addr_word(USDC));
    rpc.set(BOGUS_V2, b"token1()", addr_word(WETH));
    rpc.set(
        BOGUS_V2,
        b"getReserves()",
        reserves_word(1_500_000, 800_000),
    );
    rpc
}

// ── the seat ────────────────────────────────────────────────────────────────

/// The budget on a wait whose completion edge the fixture itself raises.
///
/// A guard, never an expectation: the fixtures' own work is microseconds, so this
/// sits far above any plausible scheduling delay and exists only so a regression
/// that never reaches the edge names itself instead of parking the test worker.
const FIXTURE_GUARD: Duration = Duration::from_secs(30);

/// The seat's two counters: how many units were handed out, and how many seat
/// tasks are still alive.
///
/// `outstanding` counts a TASK's life, not a submission: a unit's future
/// publishes its receipt from inside itself and the seat's task counts out only
/// after that future returns, so the loop may admit the next unit inside that
/// gap. It is the gauge for "no seat task is still running", never the window's
/// bound — nothing in a seat task is ordered against the admission edge, so a
/// high-water mark over it reads one past the window with nothing wrong. The
/// bound is read where nothing can resolve, off the quantity the production loop
/// owns: `a_parked_window_takes_on_exactly_the_window_and_names_every_submission`.
#[derive(Default)]
struct Metrics {
    outstanding: AtomicUsize,
    dispatched: AtomicUsize,
    /// Fired when the last outstanding unit's seat task finished counting out.
    quiesced: Notify,
}

impl Metrics {
    /// Count one unit out of the seat.
    ///
    /// A unit publishes its outcome from INSIDE its future and the run resolves on
    /// that outcome, so the run's return proves no unit is still to deliver — but
    /// it orders nothing about this counter, which is decremented by the seat's own
    /// task AFTER the future returns. A reader that must see "no unit is still in
    /// flight" therefore waits on [`Metrics::wait_quiesced`] instead of reading the
    /// count.
    fn count_out(&self) {
        if self.outstanding.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.quiesced.notify_waiters();
        }
    }

    /// Wait for the seat's own tasks to finish counting out.
    ///
    /// The edge is the seat's last task returning, which is the seat-side meaning
    /// of "no unit is still in flight". [`FIXTURE_GUARD`] only names a seat that
    /// never reaches it.
    async fn wait_quiesced(&self) {
        let quiesced = self.quiesced.notified();
        tokio::pin!(quiesced);
        // Enable before the check: the count can reach zero between the load and
        // the first poll of the wait.
        quiesced.as_mut().enable();
        if self.outstanding.load(Ordering::SeqCst) == 0 {
            return;
        }
        tokio::time::timeout(FIXTURE_GUARD, quiesced)
            .await
            .expect("the seat's unit task must finish counting out");
    }
}

/// The dispatch hook: fires on the Nth dispatch (the cap and stop latches'
/// trigger).
type SeatHook = Arc<dyn Fn(usize, &Metrics) + Send + Sync>;

/// A test seat: runs each unit's future on the ambient runtime, with an optional
/// delay so concurrent seats really do resolve out of order, and an optional
/// hook that fires on the Nth dispatch.
struct TokioSeat {
    metrics: Arc<Metrics>,
    delay: Duration,
    hook: Option<SeatHook>,
    /// When set, the FIRST unit may not run until the window's second dispatch
    /// lands — see [`TokioSeat::filling_the_window`].
    fill_gate: Option<Arc<Notify>>,
}

impl TokioSeat {
    fn new(delay: Duration, hook: Option<SeatHook>) -> Arc<Self> {
        Arc::new(Self {
            metrics: Arc::new(Metrics::default()),
            delay,
            hook,
            fill_gate: None,
        })
    }

    /// A seat whose FIRST unit may not run until the window's second submission
    /// has been dispatched.
    ///
    /// The window is filled by DISPATCH, not by a unit's speed, so "the window was
    /// actually filled" is a fact about two submissions. Without the gate the claim
    /// races the seat's own delay: a loop descheduled for longer than that delay
    /// lets the first unit resolve before the second submission is ordered, and no
    /// fill happened with nothing wrong — the window was never asked to fill. The
    /// gate makes the ordering the fill claim reads one the fixture owns.
    fn filling_the_window(delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            metrics: Arc::new(Metrics::default()),
            delay,
            hook: None,
            fill_gate: Some(Arc::new(Notify::new())),
        })
    }

    fn metrics(&self) -> &Metrics {
        &self.metrics
    }
}

impl CrawlUnitSeat for TokioSeat {
    fn dispatch(&self, future: CrawlUnitFuture) -> Result<(), String> {
        let metrics = Arc::clone(&self.metrics);
        metrics.outstanding.fetch_add(1, Ordering::SeqCst);
        let ordinal = metrics.dispatched.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(hook) = &self.hook {
            hook(ordinal, &metrics);
        }
        // The gate's release edge is the window's second dispatch, and only the
        // first unit waits on it.
        let fill_gate = self.fill_gate.as_ref().map(Arc::clone);
        if let Some(gate) = fill_gate.as_ref() {
            if ordinal == 2 {
                gate.notify_one();
            }
        }
        let held = fill_gate.filter(|_| ordinal == 1);
        let delay = self.delay;
        tokio::spawn(async move {
            if let Some(gate) = held {
                gate.notified().await;
            }
            if delay > Duration::ZERO {
                tokio::time::sleep(delay).await;
            }
            future.await;
            metrics.count_out();
        });
        Ok(())
    }
}

// ── the harness ─────────────────────────────────────────────────────────────

struct Fixture {
    controller: Arc<CrawlController>,
    source: Arc<FixtureSource>,
    seat: Arc<TokioSeat>,
    rx: mpsc::UnboundedReceiver<CrawlReceipt>,
}

fn config(window: usize, batch: usize) -> CrawlConfig {
    CrawlConfig {
        chain_id: CHAIN_ID,
        database_path: std::path::PathBuf::from("/nonexistent/fixture.sqlite"),
        route: ConstructionRoute::generic_only(),
        pool_kinds: vec![PoolKind::V4],
        pool_type_per_depth: None,
        start_tokens: vec![WETH],
        end_tokens: vec![WETH],
        min_depth: 2,
        max_depth: Some(2),
        discovery_batch_size: batch,
        intake_window: window,
        path_cap: None,
        retry: RetryPolicy {
            max_attempts: 1,
            base_delay: 0.0,
            max_delay: 0.0,
            jitter: 0.0,
        },
        policy: CrawlPathPolicy::default(),
        oracle: None,
        policy_batch_size: 1,
        profit_token: USDC,
        snapshot_block: None,
        state_block: Some(BLOCK),
    }
}

/// The controller over a scripted source and an arbitrary seat — the seam every
/// fixture needs, including the ones whose seat is not a [`TokioSeat`].
fn harness(
    rpc: FakeRpc,
    data: PathGraphData,
    config: CrawlConfig,
    seat: Arc<dyn CrawlUnitSeat>,
) -> (
    Arc<CrawlController>,
    Arc<FixtureSource>,
    mpsc::UnboundedReceiver<CrawlReceipt>,
) {
    harness_with_arm(rpc, data, config, seat, None)
}

/// The same seam with a tick-map arm attached — the head-construction read a
/// snapshot handle feeds.
fn harness_with_arm(
    rpc: FakeRpc,
    data: PathGraphData,
    config: CrawlConfig,
    seat: Arc<dyn CrawlUnitSeat>,
    snapshot: Option<Arc<dyn SnapshotArm>>,
) -> (
    Arc<CrawlController>,
    Arc<FixtureSource>,
    mpsc::UnboundedReceiver<CrawlReceipt>,
) {
    let source = FixtureSource::new(data);
    let io = Arc::new(ConstructionIo::new(Arc::new(NoDb), Arc::new(rpc)));
    let bot = Arc::new(Bot::new(CHAIN_ID));
    let stages = Arc::new(EngineStages::with_core(bot.state_arc(), bot.active_delta()));
    let driver = Arc::new(crate::arb_engine::EngineDriver::from_stages(
        Arc::clone(&bot),
        stages,
    ));
    let admission = Arc::new(RegistrationAdmission::new(
        Arc::clone(&bot),
        io,
        Arc::clone(driver.stages()),
        Arc::clone(driver.verify()),
        Arc::new(CrawlLedger::new()),
        Arc::new(BuildFlights::new()),
        Arc::new(config.admission_settings()),
    ));
    let host = CrawlHost {
        admission,
        snapshot,
    };
    let controller = CrawlController::attach_with(host, config, source.clone(), seat).unwrap();
    let (tx, rx) = mpsc::unbounded_channel();
    controller.attach_receipts(tx).unwrap();
    (controller, source, rx)
}

fn fixture(
    rpc: FakeRpc,
    data: PathGraphData,
    config: CrawlConfig,
    seat: Arc<TokioSeat>,
) -> Fixture {
    let (controller, source, rx) = harness(rpc, data, config, seat.clone());
    Fixture {
        controller,
        source,
        seat,
        rx,
    }
}

/// Drain the receipt stream's closed tail.
fn receipts(rx: &mut mpsc::UnboundedReceiver<CrawlReceipt>) -> Vec<CrawlReceipt> {
    let mut out = Vec::new();
    while let Ok(receipt) = rx.try_recv() {
        out.push(receipt);
    }
    out
}

// ── the transition table ────────────────────────────────────────────────────

const ALL_STATES: [CrawlState; 5] = [
    CrawlState::Idle,
    CrawlState::Running {
        last_complete: None,
    },
    CrawlState::Capped,
    CrawlState::Stopping,
    CrawlState::Stopped,
];

fn all_events() -> [CrawlEvent; 8] {
    [
        CrawlEvent::RunRequested,
        CrawlEvent::SweepRequested,
        CrawlEvent::PathEnqueued,
        CrawlEvent::SweepLatched {
            edition: GraphEdition::default(),
        },
        CrawlEvent::EditionChanged,
        CrawlEvent::CapReached,
        CrawlEvent::Stop,
        CrawlEvent::RunFinished,
    ]
}

#[test]
fn the_transition_table_is_total_and_pins_its_edges() {
    // Totality over the full 5x8 grid: no pair is undefined, and the allowed
    // count pins the table's SHAPE — an edge added or removed in one place has
    // to be acknowledged here.
    let mut allowed = 0;
    for state in ALL_STATES {
        for event in all_events() {
            if transition(state, event).is_ok() {
                allowed += 1;
            }
        }
    }
    // 5 poses admit `Stop`, plus the nineteen other named edges of the graph.
    assert_eq!(allowed, 25, "the graph's allowed edge count changed");
}

#[test]
fn a_stop_latches_stopping_and_only_the_resolution_finishes_it() {
    // Stop is total and declines nothing, and it never terminates a crawl whose
    // run future could still be draining the window.
    for state in [
        CrawlState::Idle,
        CrawlState::Running {
            last_complete: None,
        },
        CrawlState::Capped,
        CrawlState::Stopping,
    ] {
        assert_eq!(
            transition(state, CrawlEvent::Stop),
            Ok(CrawlState::Stopping),
            "stop from {state}"
        );
    }
    // Idempotent at the terminal pose: a second stop cannot regress it.
    assert_eq!(
        transition(CrawlState::Stopped, CrawlEvent::Stop),
        Ok(CrawlState::Stopped)
    );

    // The resolution edge is the ONLY way into `Stopped`.
    assert_eq!(
        transition(CrawlState::Stopping, CrawlEvent::RunFinished),
        Ok(CrawlState::Stopped),
        "the stop's join completes into the terminal pose"
    );
    assert_eq!(
        transition(CrawlState::Stopped, CrawlEvent::RunFinished),
        Ok(CrawlState::Stopped),
        "a second resolution is idempotent, never a re-opening"
    );
    assert!(
        transition(CrawlState::Idle, CrawlEvent::RunFinished).is_err(),
        "a resolution with no run is a caller error"
    );
    // A natural end keeps the pose the run had: the cap never un-latches, and a
    // running crawl stays running (its operator surface outlives the run).
    for state in [
        CrawlState::Running {
            last_complete: None,
        },
        CrawlState::Capped,
    ] {
        assert_eq!(
            transition(state, CrawlEvent::RunFinished),
            Ok(state),
            "the resolution edge moves nothing but `Stopping`"
        );
    }

    // `Stopped` is absorbing: it refuses everything else, loudly and by name.
    for event in all_events() {
        if matches!(event, CrawlEvent::Stop | CrawlEvent::RunFinished) {
            continue;
        }
        assert_eq!(
            transition(CrawlState::Stopped, event),
            Err(CrawlStateError::Stopped {
                current: CrawlState::Stopped,
                event
            }),
            "{event} against a stopped crawl"
        );
    }
}

#[test]
fn stopping_admits_no_work_and_moves_no_pose() {
    // The one allowance: a draining unit's cap skip is a fold, not an admission,
    // and the stop outranks the pose it would have latched.
    assert_eq!(
        transition(CrawlState::Stopping, CrawlEvent::CapReached),
        Ok(CrawlState::Stopping)
    );
    for event in all_events() {
        if matches!(
            event,
            CrawlEvent::Stop | CrawlEvent::RunFinished | CrawlEvent::CapReached
        ) {
            continue;
        }
        assert_eq!(
            transition(CrawlState::Stopping, event),
            Err(CrawlStateError::Stopping {
                current: CrawlState::Stopping,
                event
            }),
            "{event} offered to a stopping crawl"
        );
    }
}

#[test]
fn a_standalone_sweep_is_the_edge_into_running() {
    // A one-shot discovery sweep with no run must make the pipeline live, or a
    // natural completion of it could never latch the edition: a sweep that
    // leaves the pose `Idle` can never latch.
    assert_eq!(
        transition(CrawlState::Idle, CrawlEvent::SweepRequested),
        Ok(CrawlState::Running {
            last_complete: None
        })
    );
    // A live or capped sweep moves nothing.
    for state in [
        CrawlState::Running {
            last_complete: None,
        },
        CrawlState::Capped,
    ] {
        assert_eq!(transition(state, CrawlEvent::SweepRequested), Ok(state));
    }
}

#[test]
fn the_cap_is_terminal_for_a_full_run_but_admits_operator_work() {
    // A full crawl post-cap would spend the whole sweep collecting cap skips.
    assert_eq!(
        transition(CrawlState::Capped, CrawlEvent::RunRequested),
        Err(CrawlStateError::EventOutOfOrder {
            current: CrawlState::Capped,
            event: CrawlEvent::RunRequested
        })
    );
    // The operator surfaces stay admitted (their units answer the cheap skip).
    assert_eq!(
        transition(CrawlState::Capped, CrawlEvent::SweepRequested),
        Ok(CrawlState::Capped)
    );
    assert_eq!(
        transition(CrawlState::Capped, CrawlEvent::PathEnqueued),
        Ok(CrawlState::Capped)
    );
    // And the cap never re-arms: a capped registry keeps refusing.
    assert_eq!(
        transition(CrawlState::Capped, CrawlEvent::EditionChanged),
        Ok(CrawlState::Capped)
    );
}

#[test]
fn the_latch_sets_only_under_a_live_pipeline_and_rearms_only_on_an_edition_change() {
    let edition = GraphEdition {
        v2v3_count: 5,
        v2v3_max_id: 9,
        v4_count: 1,
        v4_max_id: 1,
    };
    let running = CrawlState::Running {
        last_complete: None,
    };
    assert_eq!(
        transition(running, CrawlEvent::SweepLatched { edition }),
        Ok(CrawlState::Running {
            last_complete: Some(edition)
        })
    );
    // No live pipeline, no latch.
    for state in [CrawlState::Idle, CrawlState::Capped] {
        assert!(transition(state, CrawlEvent::SweepLatched { edition }).is_err());
    }
    // The one re-arm edge, and a disabled latch answers itself.
    assert_eq!(
        transition(
            CrawlState::Running {
                last_complete: Some(edition)
            },
            CrawlEvent::EditionChanged
        ),
        Ok(running)
    );
    assert_eq!(transition(running, CrawlEvent::EditionChanged), Ok(running));
    // The cap latch is idempotent.
    for state in [CrawlState::Idle, running, CrawlState::Capped] {
        assert_eq!(
            transition(state, CrawlEvent::CapReached),
            Ok(CrawlState::Capped)
        );
    }
}

// ── the loop ────────────────────────────────────────────────────────────────

/// The window fills by SUBMISSION, and every landed receipt folds once, in
/// submission order.
///
/// Two facts, and this test owns the pair: the loop takes on a second unit
/// BEFORE the first has resolved (the fill), and the FIFO reaper folds each
/// receipt exactly once in the order this call submitted, however the seats ran.
/// The window's CEILING is not here, because no seat-side meter can witness it:
/// a seat task counts out after the future that published its receipt returns,
/// so the admission edge is not ordered against the count, and a high-water mark
/// reads `window + 1` with nothing wrong. The bound is read instead where
/// nothing can resolve — `a_parked_window_takes_on_exactly_the_window_and_names_every_submission`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_window_fills_by_submission_and_every_receipt_folds_in_order() {
    // 5 parallel pools -> 20 ordered cycles; a 2-unit window over a slow seat whose
    // first unit cannot run until the window's second submission is dispatched, so
    // the fill is an edge this fixture owns.
    let fixture = fixture(
        FakeRpc::new(),
        skip_graph(5),
        config(2, 1),
        TokioSeat::filling_the_window(Duration::from_millis(5)),
    );
    // The guard names a loop that never fills the window (the first unit would
    // otherwise wait out the crawl's own idle deadline); it sequences nothing.
    tokio::time::timeout(FIXTURE_GUARD, fixture.controller.run())
        .await
        .expect("the window fills: the second unit is submitted before the first runs")
        .unwrap();

    assert_eq!(
        fixture.controller.summary().report.units_folded,
        20,
        "every candidate folded exactly once"
    );

    let mut rx = fixture.rx;
    let folded = receipts(&mut rx);
    assert_eq!(folded.len(), 20);
    let sequence: Vec<u64> = folded.iter().map(|receipt| receipt.seq).collect();
    let mut sorted = sequence.clone();
    sorted.sort_unstable();
    assert_eq!(
        sequence, sorted,
        "receipts reach the stream in FIFO submission order, not seat order"
    );
    assert!(
        fixture.controller.receipts_ended(),
        "the run ends the stream exactly once"
    );
}

/// The window's ceiling, read where nothing can resolve it.
///
/// A parked window is the one place the bound is exact: every future is held
/// unpolled, so no receipt can land, and the loop's own unresolved set — the
/// fold's submission window, reported verbatim as `stalled` — is its submission
/// count. Exactly `window` submissions is the bound, and the report names every
/// one of them; a loop without the window check would take on all twenty
/// candidates and name them here instead.
///
/// This is the ceiling no mid-flight reading can show. A seat task outlives the
/// receipt its unit published, so a task-lifetime high-water mark reads one past
/// the window while the loop never left it; and the loop's own gauge
/// (`Fold::inflight`) counts a landed receipt only when the FIFO front reaches
/// it, so one out-of-order landing leaves that reading at `window + 1` with the
/// admission discipline intact. Parked, nothing can land: both readings are the
/// submission count, and the bound is exact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_parked_window_takes_on_exactly_the_window_and_names_every_submission() {
    let seat = Arc::new(ParkingSeat::default());
    let (controller, _source, _rx) = harness(
        FakeRpc::new(),
        // Twenty ordered cycles behind a two-unit window: a bound of two says
        // where submission stopped, not how little there was to submit.
        skip_graph(5),
        config(2, 1),
        Arc::clone(&seat) as Arc<dyn CrawlUnitSeat>,
    );
    controller.set_unit_idle_deadline_for_test(Duration::from_millis(50));

    let run = tokio::time::timeout(Duration::from_secs(5), controller.run())
        .await
        .expect("a parked window resolves at the deadline, never parks its consumer");
    match run {
        Err(CrawlRequestError::UnitStalled { stalled, .. }) => assert_eq!(
            stalled,
            vec![1, 2],
            "the loop took on exactly the window, and the report names every unresolved submission"
        ),
        other => panic!("expected the loud stalled-window error, got {other:?}"),
    }
    assert_eq!(
        seat.parked.lock().len(),
        2,
        "the second submission filled the window; a third would be one past it"
    );
    assert_eq!(controller.run_phase(), RunPhase::Finished);
    assert!(
        controller.receipts_ended(),
        "the consumer's stream ends with the run, however the run ended"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_natural_run_latches_the_edition_and_the_same_edition_costs_nothing() {
    let fixture = fixture(
        FakeRpc::new(),
        skip_graph(3),
        config(4, 8),
        TokioSeat::new(Duration::ZERO, None),
    );
    fixture.controller.run().await.unwrap();

    let latched = fixture
        .controller
        .last_complete()
        .expect("a natural, uncapped completion latches the edition");
    assert_eq!(latched, (*fixture.source.edition.lock()).unwrap());
    let sweeps_after_run = fixture.source.sweeps();
    assert_eq!(sweeps_after_run, 1);

    // The unchanged structure can only re-yield what this pipeline processed.
    assert_eq!(fixture.controller.trigger_discovery(None).await.unwrap(), 0);
    assert_eq!(
        fixture.source.sweeps(),
        sweeps_after_run,
        "the latched edition admits no second sweep"
    );
    assert_eq!(fixture.controller.last_complete(), Some(latched));

    // A changed edition re-arms: one transition, then the sweep runs and latches
    // the new fingerprint.
    let changed = GraphEdition {
        v4_count: 9,
        v4_max_id: 9,
        ..latched
    };
    fixture.source.set_edition(Some(changed));
    assert!(fixture.controller.trigger_discovery(None).await.unwrap() > 0);
    assert_eq!(fixture.source.sweeps(), sweeps_after_run + 1);
    assert_eq!(fixture.controller.last_complete(), Some(changed));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_standalone_sweep_latches_its_edition_without_a_run() {
    // No `run` ever executes here: the operator surface alone must make the
    // pipeline live, or its natural completion can never latch.
    let fixture = fixture(
        FakeRpc::new(),
        skip_graph(3),
        config(4, 4),
        TokioSeat::new(Duration::ZERO, None),
    );
    assert_eq!(
        fixture.controller.trigger_discovery(None).await.unwrap(),
        6,
        "three parallel pools admit six ordered cycles"
    );
    let edition = (*fixture.source.edition.lock()).unwrap();
    assert_eq!(
        fixture.controller.state(),
        CrawlState::Running {
            last_complete: Some(edition)
        },
        "the standalone sweep latched the edition it exhausted"
    );
    assert!(!fixture.controller.is_finished(), "no run future resolved");

    // The unchanged structure admits no second sweep.
    let sweeps = fixture.source.sweeps();
    assert_eq!(fixture.controller.trigger_discovery(None).await.unwrap(), 0);
    assert_eq!(fixture.source.sweeps(), sweeps);
    assert_eq!(fixture.controller.last_complete(), Some(edition));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bound_truncated_sweep_does_not_latch() {
    let fixture = fixture(
        FakeRpc::new(),
        skip_graph(4),
        config(4, 4),
        TokioSeat::new(Duration::ZERO, None),
    );
    assert_eq!(
        fixture.controller.trigger_discovery(Some(2)).await.unwrap(),
        2
    );
    assert_eq!(
        fixture.controller.last_complete(),
        None,
        "a truncated sweep leaves the latch armed"
    );
    assert_eq!(
        fixture.controller.state(),
        CrawlState::Running {
            last_complete: None
        },
        "the sweep left the pipeline live"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_edition_probe_never_latches() {
    let fixture = fixture(
        FakeRpc::new(),
        skip_graph(3),
        config(4, 4),
        TokioSeat::new(Duration::ZERO, None),
    );
    fixture.source.fail_probe();
    assert!(fixture.controller.trigger_discovery(None).await.unwrap() > 0);
    assert_eq!(
        fixture.controller.last_complete(),
        None,
        "an unreadable probe never latches — the next trigger sweeps"
    );
}

// ── the latches that stop work ──────────────────────────────────────────────

/// The receipt a cap stop answers with, folded from the seat's hook to model
/// "a unit answered the cap mid-sweep".
fn cap_receipt() -> CrawlReceipt {
    CrawlReceipt {
        seq: 0,
        hop_sig: None,
        outcome: RegistrationUnitOutcome::Cap,
        split: StageSplit::new(),
        path_id: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_cap_latch_halts_submission_and_the_window_still_drains() {
    let holder: Arc<OnceLock<Arc<CrawlController>>> = Arc::new(OnceLock::new());
    // What the crawl looked like from inside the seat, at the moment the cap
    // latched: the pose and the join, side by side.
    let observed: Arc<Mutex<Option<(CrawlState, bool)>>> = Arc::new(Mutex::new(None));
    let trigger = Arc::clone(&holder);
    let seen = Arc::clone(&observed);
    let seat = TokioSeat::new(
        Duration::from_millis(2),
        Some(Arc::new(move |ordinal: usize, _metrics: &Metrics| {
            if ordinal == 2 {
                if let Some(controller) = trigger.get() {
                    controller.fold(&cap_receipt());
                    *seen.lock() = Some((controller.state(), controller.is_finished()));
                }
            }
        })),
    );
    let fixture = fixture(FakeRpc::new(), skip_graph(6), config(2, 1), seat);
    assert!(holder.set(Arc::clone(&fixture.controller)).is_ok());

    fixture.controller.run().await.unwrap();

    assert_eq!(
        observed.lock().take(),
        Some((CrawlState::Capped, false)),
        "a capped crawl is NOT a finished one: its run was still draining"
    );
    assert_eq!(
        fixture.controller.state(),
        CrawlState::Capped,
        "the cap receipt latches the crawl, and the run's end never un-latches it"
    );
    assert_eq!(fixture.controller.run_phase(), RunPhase::Finished);
    assert!(fixture.controller.receipts_ended());
    let dispatched = fixture.seat.metrics().dispatched.load(Ordering::SeqCst);
    assert_eq!(
        dispatched, 2,
        "no candidate is submitted after the cap fired"
    );
    fixture.seat.metrics().wait_quiesced().await;
    assert_eq!(
        fixture.seat.metrics().outstanding.load(Ordering::SeqCst),
        0,
        "the window drained: every submitted unit resolved"
    );
    // A later full crawl refuses: the cap is terminal for discovery.
    assert!(matches!(
        fixture.controller.run().await,
        Err(CrawlRequestError::State(
            CrawlStateError::EventOutOfOrder { .. }
        ))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_during_the_crawl_latches_stopping_and_drains() {
    let holder: Arc<OnceLock<Arc<CrawlController>>> = Arc::new(OnceLock::new());
    // The pose, the join, and the stream, as they stood the instant the stop
    // latched from a live run.
    let observed: Arc<Mutex<Option<(CrawlState, RunPhase, bool)>>> = Arc::new(Mutex::new(None));
    let trigger = Arc::clone(&holder);
    let seen = Arc::clone(&observed);
    let seat = TokioSeat::new(
        Duration::from_millis(2),
        Some(Arc::new(move |ordinal: usize, _metrics: &Metrics| {
            if ordinal == 2 {
                if let Some(controller) = trigger.get() {
                    controller.stop();
                    *seen.lock() = Some((
                        controller.state(),
                        controller.run_phase(),
                        controller.receipts_ended(),
                    ));
                }
            }
        })),
    );
    let fixture = fixture(FakeRpc::new(), skip_graph(6), config(2, 1), seat);
    assert!(holder.set(Arc::clone(&fixture.controller)).is_ok());

    fixture.controller.run().await.unwrap();

    assert_eq!(
        observed.lock().take(),
        Some((CrawlState::Stopping, RunPhase::Live, false)),
        "a stop against a live run latches `Stopping` and leaves the stream open, so no receipt is truncated"
    );
    assert_eq!(
        fixture.controller.state(),
        CrawlState::Stopped,
        "the run future's resolution finished the stop — nothing else can"
    );
    assert_eq!(fixture.controller.run_phase(), RunPhase::Finished);
    assert_eq!(
        fixture.seat.metrics().dispatched.load(Ordering::SeqCst),
        2,
        "a stop is honoured before the next submission"
    );
    fixture.seat.metrics().wait_quiesced().await;
    assert_eq!(
        fixture.seat.metrics().outstanding.load(Ordering::SeqCst),
        0,
        "outstanding units are never cancelled — they resolve"
    );
    assert!(fixture.controller.receipts_ended());
    let mut rx = fixture.rx;
    assert_eq!(
        receipts(&mut rx).len(),
        2,
        "the draining units' receipts all reached the stream before it ended"
    );
}

// ── the loud contract ───────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fatal_unit_aborts_the_crawl_loudly_after_the_drain() {
    let fixture = fixture(
        abort_rpc(),
        abort_graph(),
        config(1, 1),
        TokioSeat::new(Duration::ZERO, None),
    );
    let outcome = fixture.controller.run().await;
    match outcome {
        Err(CrawlRequestError::Abort(CrawlAbort::Construction(kind))) => {
            assert_eq!(kind.tag(), "identity-mismatch", "the D4 family refusal");
        }
        other => panic!("expected the loud construction abort, got {other:?}"),
    }
    fixture.seat.metrics().wait_quiesced().await;
    assert_eq!(
        fixture.seat.metrics().outstanding.load(Ordering::SeqCst),
        0,
        "the abort stops submission but never cancels an outstanding unit"
    );
    assert!(
        fixture.seat.metrics().dispatched.load(Ordering::SeqCst) <= 2,
        "the loop submits nothing after the fatal receipt"
    );
    // A fatal abort is a resolution, not a stop: the pose keeps what it had,
    // the join latches, and the stream still ends exactly once.
    assert!(!fixture.controller.state().is_stopped());
    assert_eq!(fixture.controller.run_phase(), RunPhase::Finished);
    assert!(fixture.controller.receipts_ended());
}

// ── the window's completion clause ──────────────────────────────────

/// A seat that accepts a unit and drops its future unpolled.
///
/// The S2 intake fault's exact shape: a lane death drains the held work, which
/// runs ZERO times, so the unit's future is dropped without ever sending its
/// outcome. Any seat thread that dies between accept and run looks the same.
struct DroppedSeat;

impl CrawlUnitSeat for DroppedSeat {
    fn dispatch(&self, _future: CrawlUnitFuture) -> Result<(), String> {
        // Taking the future by value and never polling it IS the loss.
        Ok(())
    }
}

/// A lost unit must END the run, not park it.
///
/// The consumer of a run is parked on its receipt stream: a window that waits
/// forever for a receipt nobody can send hangs the consumer with it, and the
/// boot behind it. The loud path exists ([`CrawlRequestError::UnitLost`]) — it
/// has to be reachable from the loop, which means the drive's own sender must be
/// gone before the window waits.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_unit_dropped_by_its_seat_ends_the_run_loudly() {
    // Two candidate paths, a window wider than both: the sweep exhausts and the
    // LOST units are the drain's, which is where the channel's end answers.
    let (controller, _source, mut rx) = harness(
        FakeRpc::new(),
        skip_graph(2),
        config(4, 1),
        Arc::new(DroppedSeat),
    );

    let run = tokio::time::timeout(Duration::from_secs(5), controller.run())
        .await
        .expect("a dropped unit must resolve the run — never park it");
    match run {
        Err(CrawlRequestError::UnitLost { outstanding }) => {
            assert!(outstanding >= 1, "the loss names what never arrived");
        }
        other => panic!("expected the loud lost-unit error, got {other:?}"),
    }
    assert_eq!(controller.run_phase(), RunPhase::Finished);
    assert!(
        controller.receipts_ended(),
        "the consumer's stream ends with the run, however the run ended"
    );
    assert!(
        rx.recv().await.is_none(),
        "the consumer observes end-of-stream instead of waiting forever"
    );
}

/// A seat that keeps every unit's future alive and never polls it.
///
/// A parked unit: a fleet seat thread stuck inside the unit, a build flight's
/// peer whose leader vanished, a chain read that never answers. The unit's
/// sender stays alive, so the channel never ends and only the idle deadline can
/// free the run.
#[derive(Default)]
struct ParkingSeat {
    parked: Mutex<Vec<CrawlUnitFuture>>,
}

impl CrawlUnitSeat for ParkingSeat {
    fn dispatch(&self, future: CrawlUnitFuture) -> Result<(), String> {
        self.parked.lock().push(future);
        Ok(())
    }
}

/// A parked unit is named at the deadline, and the run resolves.
///
/// This is the shape the live boot showed: receipts stop, nothing errors, and
/// the run never resolves — so the consumer's `async for` parks forever and the
/// boot never reaches the trim. A silent window is a parked window.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_parked_unit_is_named_at_the_idle_deadline() {
    let seat = Arc::new(ParkingSeat::default());
    let (controller, _source, mut rx) =
        harness(FakeRpc::new(), skip_graph(2), config(1, 1), seat.clone());
    controller.set_unit_idle_deadline_for_test(Duration::from_millis(50));

    let run = tokio::time::timeout(Duration::from_secs(5), controller.run())
        .await
        .expect("a parked unit must resolve the run at the deadline, never park its consumer");
    match run {
        Err(CrawlRequestError::UnitStalled { stalled, waited_ms }) => {
            assert_eq!(stalled, vec![1], "the report names the parked submission");
            assert_eq!(waited_ms, 50);
        }
        other => panic!("expected the loud stalled-window error, got {other:?}"),
    }
    assert_eq!(
        seat.parked.lock().len(),
        1,
        "the unit really was held alive, not dropped"
    );
    assert_eq!(controller.run_phase(), RunPhase::Finished);
    assert!(
        controller.receipts_ended(),
        "the consumer's stream ends with the run"
    );
    assert!(
        rx.recv().await.is_none(),
        "the consumer observes end-of-stream, so its async-for cannot hang"
    );
}

/// A seat whose intake is HELD: it accepts every unit's future and parks it
/// unpolled while the hold flag is up, then runs them all on the lift.
///
/// This is the fleet intake under a posture cordon, at the seam the controller
/// sees: `dispatch` accepts (never refuses, never drops — the never-drop
/// ledger), nothing runs, and [`CrawlUnitSeat::hold`] reports why. The cordon's
/// lift is the throttle feed's clean-window exit, which runs the whole held
/// backlog.
struct HoldingSeat {
    metrics: Arc<Metrics>,
    holding: AtomicBool,
    held: Mutex<Vec<CrawlUnitFuture>>,
}

impl HoldingSeat {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            metrics: Arc::new(Metrics::default()),
            holding: AtomicBool::new(true),
            held: Mutex::new(Vec::new()),
        })
    }

    fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    /// The lift: everything the hold parked runs now.
    fn lift(&self) {
        self.holding.store(false, Ordering::SeqCst);
        let lifted: Vec<CrawlUnitFuture> = std::mem::take(&mut *self.held.lock());
        for future in lifted {
            run_async(future, Arc::clone(&self.metrics));
        }
    }

    /// How many units the hold still parks (the "held, never dropped" count).
    fn parked(&self) -> usize {
        self.held.lock().len()
    }
}

impl CrawlUnitSeat for HoldingSeat {
    fn dispatch(&self, future: CrawlUnitFuture) -> Result<(), String> {
        self.metrics.dispatched.fetch_add(1, Ordering::SeqCst);
        self.metrics.outstanding.fetch_add(1, Ordering::SeqCst);
        if self.holding.load(Ordering::SeqCst) {
            // Held: accepted, queued, and run ZERO times until the lift.
            self.held.lock().push(future);
        } else {
            run_async(future, Arc::clone(&self.metrics));
        }
        Ok(())
    }

    fn hold(&self) -> Option<SeatHold> {
        self.holding
            .load(Ordering::SeqCst)
            .then_some(SeatHold::Posture)
    }
}

/// Run one unit's future on the ambient runtime, counting it out when it lands.
fn run_async(future: CrawlUnitFuture, metrics: Arc<Metrics>) {
    tokio::spawn(async move {
        future.await;
        metrics.count_out();
    });
}

/// The live boot's stall, at the seam that broke: a HELD window is not a parked
/// one.
///
/// The boot's shape (verified): the crawl filled its first window, the fleet
/// posture cordoned the registration intake, every submitted unit waited in the
/// unbounded backlog, and NOT ONE receipt arrived for a minute — because not one
/// unit had run. Nothing was stuck and nothing was lost; the cordon's
/// clean-window exit released the whole backlog at once. A window that charges
/// that hold to the park deadline aborts a crawl that is working, so the hold
/// must be waited through — and deliver on the lift.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_held_window_outlives_the_park_deadline_and_delivers_on_the_lift() {
    let seat = HoldingSeat::new();
    let (controller, _source, mut rx) = harness(
        FakeRpc::new(),
        skip_graph(2),
        config(2, 1),
        Arc::clone(&seat) as Arc<dyn CrawlUnitSeat>,
    );
    // The park deadline is a fifth of the hold, so several deadlines pass while
    // the units are held: a window that mistook the hold for silence fires here.
    controller.set_unit_idle_deadline_for_test(Duration::from_millis(40));
    controller.set_held_window_budget_for_test(Duration::from_secs(10));
    let lifter = Arc::clone(&seat);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        lifter.lift();
    });

    let run = tokio::time::timeout(Duration::from_secs(10), controller.run())
        .await
        .expect("a held window must resolve: the hold lifts and the units deliver");
    run.expect("the held window delivered, so the run completes");
    // Two parallel pools, so the sweep's ordered cycles are the two (i, j) pairs
    // with i != j — both of them land in the first window, which is the whole
    // point: the window is full, held, and delivers on the lift.
    assert_eq!(
        controller.summary().report.units_folded,
        2,
        "every held unit ran on the lift and folded exactly once"
    );
    let folded = receipts(&mut rx);
    assert_eq!(folded.len(), 2, "the consumer saw every receipt");
    assert_eq!(
        seat.parked(),
        0,
        "the lift released everything the hold parked"
    );
    seat.metrics().wait_quiesced().await;
    assert_eq!(
        seat.metrics().outstanding.load(Ordering::SeqCst),
        0,
        "every unit resolved"
    );
    assert_eq!(controller.run_phase(), RunPhase::Finished);
    assert!(controller.receipts_ended());
}

/// A hold that never lifts is NAMED at the hold budget, not waited on forever.
///
/// The cordon's exit needs a clean throttle window, and a host under sustained
/// cgroup throttling never produces one — so the hold can outlast any sane
/// bound. The window then reports the posture (the operator's lever) instead of
/// a park it cannot diagnose, and the run still resolves: no hang, and no
/// mistyped cause.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hold_that_never_lifts_is_named_at_the_hold_budget() {
    let seat = HoldingSeat::new();
    let (controller, _source, mut rx) = harness(
        FakeRpc::new(),
        skip_graph(2),
        config(2, 2),
        Arc::clone(&seat) as Arc<dyn CrawlUnitSeat>,
    );
    controller.set_unit_idle_deadline_for_test(Duration::from_millis(10));
    controller.set_held_window_budget_for_test(Duration::from_millis(120));

    let run = tokio::time::timeout(Duration::from_secs(10), controller.run())
        .await
        .expect("a never-lifting hold must resolve the run at its budget, never park it");
    match run {
        Err(CrawlRequestError::IntakeHeld {
            stalled,
            held_ms,
            hold,
        }) => {
            assert_eq!(hold, SeatHold::Posture, "the report names the posture");
            assert_eq!(stalled, vec![1, 2], "every held submission is named");
            assert!(
                held_ms >= 120,
                "the budget was the bound that ended the wait, got {held_ms}ms"
            );
        }
        other => panic!("expected the held-window error, got {other:?}"),
    }
    assert_eq!(
        seat.parked(),
        2,
        "the units were HELD — queued at the seat, never dropped"
    );
    assert_eq!(controller.run_phase(), RunPhase::Finished);
    assert!(
        controller.receipts_ended(),
        "the consumer's stream ends with the run, however the run ended"
    );
    assert!(
        rx.recv().await.is_none(),
        "the consumer observes end-of-stream instead of waiting forever"
    );
}

// ── the operator surface ────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enqueue_after_stopped_refuses_loudly() {
    let fixture = fixture(
        FakeRpc::new(),
        skip_graph(2),
        config(2, 2),
        TokioSeat::new(Duration::ZERO, None),
    );
    fixture.controller.stop();
    let path = DiscoveredPath {
        hops: vec![DiscoveredHop::V4 {
            pool_manager: V4_PM,
            pool_hash: None,
        }],
    };
    let refused = fixture.controller.enqueue_path(path, None).await;
    match refused {
        Err(CrawlRequestError::State(CrawlStateError::Stopped { current, event })) => {
            assert_eq!(current, CrawlState::Stopped);
            assert_eq!(event, CrawlEvent::PathEnqueued);
        }
        other => panic!("expected the typed stopped refusal, got {other:?}"),
    }
    assert!(
        fixture.controller.summary().report.units_folded == 0,
        "nothing ran"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enqueue_is_admitted_post_cap_and_folds_its_receipt() {
    let fixture = fixture(
        FakeRpc::new(),
        skip_graph(2),
        config(2, 2),
        TokioSeat::new(Duration::ZERO, None),
    );
    fixture.controller.fold(&cap_receipt());
    assert_eq!(fixture.controller.state(), CrawlState::Capped);
    let seq = fixture
        .controller
        .enqueue_path(
            DiscoveredPath {
                hops: vec![DiscoveredHop::V4 {
                    pool_manager: V4_PM,
                    pool_hash: None,
                }],
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(seq, 1, "the operator path is the first submission");
    assert_eq!(
        fixture.controller.summary().report.units_folded,
        2,
        "the folded cap receipt plus the operator path — one fold point"
    );
}

/// The operator surface is bounded too: an enqueued unit that parks answers the
/// caller at the idle deadline instead of parking the Python await that made it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_enqueue_whose_unit_parks_is_named_at_the_deadline() {
    let seat = Arc::new(ParkingSeat::default());
    let (controller, _source, _rx) =
        harness(FakeRpc::new(), skip_graph(2), config(2, 2), seat.clone());
    controller.set_unit_idle_deadline_for_test(Duration::from_millis(50));

    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        controller.enqueue_path(
            DiscoveredPath {
                hops: vec![DiscoveredHop::V4 {
                    pool_manager: V4_PM,
                    pool_hash: None,
                }],
            },
            None,
        ),
    )
    .await
    .expect("the operator call must resolve: a parked unit is named, never awaited forever");
    match outcome {
        Err(CrawlRequestError::UnitStalled { stalled, waited_ms }) => {
            assert_eq!(stalled, vec![1], "the report names the parked submission");
            assert_eq!(waited_ms, 50);
        }
        other => panic!("expected the loud stalled-window error, got {other:?}"),
    }
    assert_eq!(
        seat.parked.lock().len(),
        1,
        "the unit really was held alive, not dropped"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_teardown_handle_latches_and_reads_the_two_facts() {
    let fixture = fixture(
        FakeRpc::new(),
        skip_graph(2),
        config(2, 2),
        TokioSeat::new(Duration::ZERO, None),
    );
    let handle = CrawlHandle::new(Arc::clone(&fixture.controller));
    assert_eq!(handle.state(), CrawlState::Idle);
    assert!(
        !handle.is_finished(),
        "nothing has run, nothing can be joined"
    );

    fixture.controller.run().await.unwrap();
    assert!(handle.is_finished(), "the run future resolved");
    assert!(
        !handle.state().is_stopped(),
        "a natural end is a resolution, not a stop"
    );

    // A stop after the run resolved has nothing to join, so it terminates at
    // once, and it is idempotent: the handle cannot regress a terminal pose.
    handle.stop();
    assert_eq!(handle.state(), CrawlState::Stopped);
    assert!(handle.is_finished());
    handle.stop();
    assert_eq!(handle.state(), CrawlState::Stopped, "stop is idempotent");
    assert_eq!(
        handle.clone().state(),
        CrawlState::Stopped,
        "the handle is Clone"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_with_no_live_run_ends_the_stream_immediately() {
    // Nothing spawned a run, so nothing will ever resolve one: the stop must end
    // the stream itself, or a consumer would park on it forever.
    let fixture = fixture(
        FakeRpc::new(),
        skip_graph(2),
        config(2, 2),
        TokioSeat::new(Duration::ZERO, None),
    );
    fixture.controller.stop();
    assert_eq!(
        fixture.controller.state(),
        CrawlState::Stopped,
        "the stop applied the resolution edge: no run can ever reach it"
    );
    assert!(fixture.controller.is_finished());
    assert!(fixture.controller.receipts_ended());

    // A run offered to it refuses with the typed terminal error, not a panic.
    assert!(matches!(
        fixture.controller.run().await,
        Err(CrawlRequestError::State(CrawlStateError::Stopped {
            current: CrawlState::Stopped,
            event: CrawlEvent::RunRequested
        }))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_run_while_one_is_live_is_refused() {
    let fixture = fixture(
        FakeRpc::new(),
        skip_graph(5),
        config(1, 1),
        TokioSeat::new(Duration::from_millis(5), None),
    );
    let (first, second) = tokio::join!(fixture.controller.run(), fixture.controller.run());
    first.unwrap();
    assert!(
        matches!(second, Err(CrawlRequestError::RunInFlight)),
        "one run at a time: the window is per-run and the join fact has one writer"
    );
    assert_eq!(fixture.controller.run_phase(), RunPhase::Finished);
}

/// A seat that panics instead of running its unit — the run future's panic path.
struct PanicSeat;

impl CrawlUnitSeat for PanicSeat {
    fn dispatch(&self, _future: CrawlUnitFuture) -> Result<(), String> {
        panic!("the fixture seat panics before it can run the unit")
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panicking_run_still_ends_the_receipt_stream() {
    // The end-of-stream contract cannot depend on the run returning: a consumer
    // parked on a stream nobody closes is exactly the failure a guard prevents.
    let (controller, _source, mut rx) = harness(
        FakeRpc::new(),
        skip_graph(2),
        config(2, 1),
        Arc::new(PanicSeat),
    );
    let runner = Arc::clone(&controller);
    let joined = tokio::spawn(async move { runner.run().await })
        .await
        .expect_err("the seat's panic unwinds the run future");
    assert!(joined.is_panic(), "the run future really panicked");

    assert!(
        controller.receipts_ended(),
        "a panicking run ends the stream exactly once"
    );
    assert_eq!(
        controller.run_phase(),
        RunPhase::Finished,
        "the join fact is the resolution, not the return value"
    );
    assert_eq!(
        controller.state(),
        CrawlState::Running {
            last_complete: None
        },
        "no stop was latched, so the resolution moves no pose"
    );
    assert!(
        rx.recv().await.is_none(),
        "the consumer observes the natural end-of-stream"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_vanished_receipt_consumer_does_not_fail_the_crawl() {
    // `SendOutcome::ReceiverGone` latches the channel `Closed`: observation ends,
    // the fold does not. The summary is the reporting surface, the stream is not.
    let Fixture { controller, rx, .. } = fixture(
        FakeRpc::new(),
        skip_graph(3),
        config(4, 4),
        TokioSeat::new(Duration::ZERO, None),
    );
    drop(rx);
    controller.run().await.unwrap();
    assert_eq!(
        controller.summary().report.units_folded,
        6,
        "every unit still folded into the summary"
    );
    assert!(controller.receipts_ended());
}

// ── the snapshot arm ────────────────────────────────────────────────────────

/// The arm a process with a closable snapshot read transaction owns: ONE slot,
/// emptied when the transaction commits, asked once per unit.
struct SlotArm {
    slot: Mutex<Option<Arc<NeverReadDb>>>,
    asks: AtomicUsize,
}

impl SnapshotArm for SlotArm {
    fn acquire(&self) -> Option<Arc<dyn degenbot_db::snapshot::TickMapDb>> {
        self.asks.fetch_add(1, Ordering::SeqCst);
        self.slot
            .lock()
            .clone()
            .map(|db| db as Arc<dyn degenbot_db::snapshot::TickMapDb>)
    }
}

/// A tick-map handle the fixture never reads: what is measured here is the ASK,
/// not the data. Every read answers "no rows", so a unit that does hold it takes
/// the same no-data path a cold DB does.
struct NeverReadDb;

impl degenbot_db::snapshot::TickMapDb for NeverReadDb {
    fn fetch_liquidity_map(
        &self,
        _pool_address: Address,
    ) -> Result<Option<degenbot_db::snapshot::LiquidityMap>, DbError> {
        Ok(None)
    }
    fn fetch_liquidity_map_v4(
        &self,
        _pool_manager: Address,
        _pool_id_hash: alloy::primitives::B256,
    ) -> Result<Option<degenbot_db::snapshot::LiquidityMap>, DbError> {
        Ok(None)
    }
    fn fetch_newest_update_block(
        &self,
        _chain: i64,
        _family: degenbot_db::read::ExchangeFamily,
    ) -> Result<Option<i64>, DbError> {
        Ok(None)
    }
    fn fetch_liquidity_update_block(&self, _pool_address: Address) -> Result<Option<i64>, DbError> {
        Ok(None)
    }
    fn fetch_liquidity_update_block_v4(
        &self,
        _pool_manager: Address,
        _pool_id_hash: alloy::primitives::B256,
    ) -> Result<Option<i64>, DbError> {
        Ok(None)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_snapshot_handle_is_acquired_per_unit_and_dropped_when_the_run_ends() {
    // The process holds the ONE long-lived handle; the crawl asks the arm per
    // unit and keeps nothing between units. That is the contract the close's
    // sole-holder canary reads: a crawl-scoped clone would outlive the run.
    let handle = Arc::new(NeverReadDb);
    let arm = Arc::new(SlotArm {
        slot: Mutex::new(Some(Arc::clone(&handle))),
        asks: AtomicUsize::new(0),
    });
    // The slot's handle plus this test's own: every other reference is a unit's.
    let holders = Arc::strong_count(&handle);
    let (controller, source, mut rx) = harness_with_arm(
        FakeRpc::new(),
        skip_graph(5),
        config(2, 1),
        TokioSeat::new(Duration::from_millis(5), None),
        Some(Arc::clone(&arm) as Arc<dyn SnapshotArm>),
    );
    controller.run().await.unwrap();

    let units = controller.summary().report.units_folded;
    assert_eq!(units, 20, "the fixture's cycle count");
    assert_eq!(
        arm.asks.load(Ordering::SeqCst),
        units,
        "the arm is asked once per unit — per USE, never once at boot"
    );
    assert_eq!(
        Arc::strong_count(&handle),
        holders,
        "no unit retained a handle: the slot is the handle's only other holder"
    );

    // The close: the owner empties the slot, and every later ask is the no-DB
    // arm. The operator surface keeps working — it registers without the DB seed.
    assert_eq!(
        receipts(&mut rx).len(),
        20,
        "the run's own receipts, drained before the close"
    );
    *arm.slot.lock() = None;
    assert_eq!(
        Arc::strong_count(&handle),
        1,
        "the close leaves only this test's own reference"
    );
    // A changed graph edition re-arms the sweep latch the natural run set: the
    // post-close operator trigger must be admitted, not answered "already done".
    let latched = controller
        .last_complete()
        .expect("the run latched its edition");
    source.set_edition(Some(GraphEdition {
        v4_count: 9,
        v4_max_id: 9,
        ..latched
    }));
    let sweeps = source.sweeps();
    let asked_before = arm.asks.load(Ordering::SeqCst);
    let bound = controller.trigger_discovery(Some(4)).await.unwrap();
    assert!(bound > 0, "the post-close sweep still submits units");
    assert_eq!(source.sweeps(), sweeps + 1);
    assert!(
        arm.asks.load(Ordering::SeqCst) > asked_before,
        "the post-close units asked the arm again and got the no-DB arm"
    );
    assert_eq!(
        Arc::strong_count(&handle),
        1,
        "a post-close unit acquired no handle at all"
    );
    assert_eq!(
        controller.summary().report.units_folded,
        units + bound,
        "every post-close unit resolved and folded — without the DB seed"
    );
}
