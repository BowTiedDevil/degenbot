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
use tokio::sync::mpsc;

use super::{
    transition, CrawlAbort, CrawlConfig, CrawlController, CrawlEvent, CrawlHandle, CrawlHost,
    CrawlRequestError, CrawlState, CrawlStateError, CrawlUnitFuture, CrawlUnitSeat, RunPhase,
};
use crate::arb_engine::crawl::discovery::{
    CrawlDiscoveryError, DiscoveredPath, DiscoverySource, Edition,
};
use crate::arb_engine::crawl::policy::CrawlPathPolicy;
use crate::arb_engine::crawl::receipt::{CrawlReceipt, StageSplit};
use crate::arb_engine::crawl::unit::DiscoveredHop;
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

/// The seat's counters: how many units were handed out, how many are running, and
/// the high-water mark (the window's own witness).
#[derive(Default)]
struct Metrics {
    outstanding: AtomicUsize,
    max_outstanding: AtomicUsize,
    dispatched: AtomicUsize,
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
}

impl TokioSeat {
    fn new(delay: Duration, hook: Option<SeatHook>) -> Arc<Self> {
        Arc::new(Self {
            metrics: Arc::new(Metrics::default()),
            delay,
            hook,
        })
    }

    fn metrics(&self) -> &Metrics {
        &self.metrics
    }
}

impl CrawlUnitSeat for TokioSeat {
    fn dispatch(&self, future: CrawlUnitFuture) -> Result<(), String> {
        let metrics = Arc::clone(&self.metrics);
        let running = metrics.outstanding.fetch_add(1, Ordering::SeqCst) + 1;
        metrics.max_outstanding.fetch_max(running, Ordering::SeqCst);
        let ordinal = metrics.dispatched.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(hook) = &self.hook {
            hook(ordinal, &metrics);
        }
        let delay = self.delay;
        tokio::spawn(async move {
            if delay > Duration::ZERO {
                tokio::time::sleep(delay).await;
            }
            future.await;
            metrics.outstanding.fetch_sub(1, Ordering::SeqCst);
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
    let source = FixtureSource::new(data);
    let io = Arc::new(ConstructionIo::new(Arc::new(NoDb), Arc::new(rpc)));
    let bot = Arc::new(Bot::new(CHAIN_ID));
    let stages = Arc::new(EngineStages::with_core(bot.state_arc(), bot.active_delta()));
    let driver = Arc::new(crate::arb_engine::EngineDriver::from_stages(
        Arc::clone(&bot),
        stages,
    ));
    let host = CrawlHost {
        bot,
        driver,
        io,
        db: None,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_window_bounds_the_outstanding_units_and_every_receipt_folds() {
    // 5 parallel pools -> 20 ordered cycles; a 2-unit window over a slow seat.
    let fixture = fixture(
        FakeRpc::new(),
        skip_graph(5),
        config(2, 1),
        TokioSeat::new(Duration::from_millis(5), None),
    );
    fixture.controller.run().await.unwrap();

    let metrics = fixture.seat.metrics();
    assert_eq!(
        metrics.max_outstanding.load(Ordering::SeqCst),
        2,
        "the window is the submission bound, and it was actually filled"
    );
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
