//! `EngineDriver` — the public Rust driver seam over the crate-private
//! engine (ADR-050, ADR-093 D1/D2/D5/D7).
//!
//! # Why this type exists
//!
//! The only external driver of the pump/solve lifecycle used to be the `PyO3`
//! `PyArbEngine` + `PumpState` pair in `degenbot-python`:
//! [`crate::arb_engine::ArbitrageEngine`] is `pub(crate)` machinery, and the
//! public [`crate::arb_engine::EngineStages`] seam is an observer/stage surface
//! (no connect/run/close). A pure-Rust `cargo add degenbot` consumer could not
//! run the startup ritual. `EngineDriver` is the ONE driver seam above the ONE
//! engine seam.
//!
//! # Lifecycle-only (ADR-093 D1)
//!
//! The seam is the session and nothing else:
//!
//! ```text
//! claim_consumer() -> Result<EventHandoff, ClaimRefusal>   // the stream + its claim
//! connect(ws)      -> Result<Connected, ConnectRefusal>    // Detached -> Subscribed
//! run(claim)       -> Result<(), DriverError>              // Subscribed -> Running
//! close()          -> TeardownReport                       // total from every pose
//! current_phase() / is_stopped() / wait_session_end()
//! ```
//!
//! Every engine operation is reached at [`EngineStages`] (path registry, retune,
//! inline simulator, results) or at its pose-independent core door; this type
//! does not forward them (ADR-093 D3). The Verify lifecycle is a capability of
//! its own ([`Self::verify`]), not a session move.
//!
//! # The startup ritual
//!
//! 1. [`Self::claim_consumer`] hands out the session's ordered event stream AND
//!    the [`ConsumerClaim`] that proves a consumer holds it. One hand-off per
//!    session; a second declines by name.
//! 2. [`Self::connect`] opens the WS subscriptions and observes the first
//!    complete block `W`, reporting it as [`Connected::link_block`]. It stops
//!    **before** production: no batch can flow down a stream nobody reads.
//! 3. [`Self::run`] consumes the claim, owns the `S+1..W` auto-backfill
//!    (awaiting `BlockPump::backfill_with_drain` synchronously), and spawns the
//!    live loop. Consumers never call `backfill_from_snapshot`.
//! 4. [`Self::close`] is asynchronous, total, idempotent and shared: it requests
//!    termination, waits for any in-flight move to unwind, latches the crawl's
//!    stop before joining the pump, closes the event stream exactly once, drains
//!    the walk memo after the pump is down, and publishes `StopCompleted` last.
//!
//! # The two ordering holes (ADR-093 §1.3), closed structurally
//!
//! The session's pose is private and **data-carrying** ([`crate::arb_engine::pump_session`]):
//! `Subscribed` owns the pump, `Running` owns the live loop's join fact. A move
//! the table no longer admits cannot install a payload, so a `run` landing after
//! a `close` aborts its own loop instead of arming it on a dead session, and a
//! crawl attach after teardown begins is refused by the same table.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use degenbot_core::{diag, op_error, op_info, op_warn};
use degenbot_eventhub::Hub;
use degenbot_substrate::session_registry::PoolIdentity;
use degenbot_substrate::state_lock::{LockSite, StateLock};
use degenbot_substrate::BotState;
#[cfg(test)]
use parking_lot::Mutex;
use tokio::sync::watch;

use crate::arb_engine::crawl::admission::{CrawlLedger, RegistrationAdmission};
use crate::arb_engine::crawl::controller::{
    CrawlConfig, CrawlConfigError, CrawlController, CrawlHandle, CrawlHost, SnapshotArm,
};
use crate::arb_engine::crawl::flights::BuildFlights;
use crate::arb_engine::crawl::receipt::CrawlReceipt;
use crate::arb_engine::pump_session::{
    ConnectedSession, CrawlExit, CrawlTeardown, LiveLoop, MovePayload, PumpDecline, PumpEvent,
    PumpExit, PumpPhase, PumpSession,
};
use crate::arb_engine::session_events::{
    ClaimRefusal, ConsumerClaim, EventHandoff, SessionEvent, SessionStream,
};
use crate::arb_engine::{BlockNotification, EngineStages};
use crate::bot_core::block_pump::{BlockPump, SubscribeState};
use crate::bot_core::reorg_coordinator::ReorgCoordinator;
use crate::bot_core::verify_lifecycle::VerifyLifecycleConfig;
use crate::bot_core::{Bot, PumpControl, StageHandlers, VerifyLifecycle};
use crate::strategy_host::HostHub;
#[cfg(test)]
use degenbot_ingestion::IngestEvent as WsEvent;

/// Hub registration name for the session's ONE ordered event source.
///
/// A named source channel ([`degenbot_eventhub::NamedSender`]) rather than a
/// [`degenbot_eventhub::HubEvent`] variant: `SessionEvent` is an engine-owned
/// downstream type the hub vocabulary deliberately does not name.
pub const SESSION_EVENT_CHANNEL_NAME: &str = "engine_session_event";

/// Hub registration name for the block-clock observer tap.
///
/// The forwarded-header edge (ADR-027) the observer surfaces read; the session's
/// own clock is the stream's `Block` arm, written by the same producer call.
pub const BLOCK_CHANNEL_NAME: &str = "engine_block_notification";

/// How long [`EngineDriver::close`] waits for each loop to exit
/// **cooperatively** before escalating to `abort()`.
///
/// The pump arms a 500 ms `timed_exit_tick` whose select arm polls the shared
/// shutdown flag, so a halted session unwinds the loop normally within one
/// tick. Four ticks with margin is the window: long enough that a healthy pump
/// always returns through its own machinery, short enough that teardown stays
/// prompt. The escalation only exists for a loop parked somewhere that tick
/// cannot reach (the `run_loop.rs` limitation note: a GIL re-entry park through
/// `PySubscriberAdapter`, or engine-lock contention inside `on_drain`).
pub const PUMP_STOP_GRACE: Duration = Duration::from_secs(2);

/// [`PUMP_STOP_GRACE`] in milliseconds, for the teardown log fields — the
/// `Duration::as_millis` widening kept non-truncating.
fn pump_stop_grace_ms() -> u64 {
    u64::try_from(PUMP_STOP_GRACE.as_millis()).unwrap_or(u64::MAX)
}

/// The engine's named source-channel producers, minted together.
///
/// A hub owns one channel per name; the engine registers its ordered
/// `SessionEvent` source and its block-clock observer tap once on the hub it is
/// attaching to, and keeps the producer ends here until the driver installs
/// them. The consumer ends stay hub-held for the once-only hand-offs.
pub struct EngineChannelHandles {
    session: tokio::sync::mpsc::UnboundedSender<SessionEvent>,
    block: tokio::sync::mpsc::UnboundedSender<BlockNotification>,
}

impl EngineChannelHandles {
    /// Register the engine's named source channels on `hub` and return their
    /// producer ends.
    ///
    /// Taking `&mut Hub` is the point: a named channel is minted into the hub's
    /// map, so registration must complete before the hub is shared as an `Arc`
    /// and handed to a driver.
    #[must_use]
    pub fn register_on(hub: &mut Hub) -> Self {
        let session = hub
            .add_named_unbounded_source::<SessionEvent>(SESSION_EVENT_CHANNEL_NAME)
            .into_inner();
        let block = hub
            .add_named_unbounded_source::<BlockNotification>(BLOCK_CHANNEL_NAME)
            .into_inner();
        Self { session, block }
    }
}

/// A typed pump-protocol-phase violation raised at the driver boundary.
///
/// The pump-session table refuses a move with a typed
/// [`PumpDecline`](crate::arb_engine::pump_session::PumpDecline); the driver
/// wraps that in this value so a consumer matches a variant instead of parsing
/// a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhaseError {
    /// The rejected driver method (`"connect"`, `"run"`, `"crawl"`).
    pub method: &'static str,
    /// The pump-protocol phase at call time.
    pub current: PumpPhase,
    /// The phase boundary the method required, or `None` when the method
    /// imposes none (`close` admits every pose).
    pub required: Option<PumpPhase>,
    /// The decline's own message — the table's error text, which the Python
    /// shells surface verbatim in a `RuntimeError`.
    pub detail: String,
}

impl std::fmt::Display for PhaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for PhaseError {}

/// Why the WS transport refused a `connect` — the handshake failure with its
/// diagnostic source retained, or a session that handed back no stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubscribeRefusal {
    /// The WS handshake failed. `source` is the transport's own diagnostic, kept
    /// verbatim so the operator sees the endpoint's error and not a paraphrase.
    Handshake {
        /// The transport error text.
        source: String,
    },
    /// The session opened but handed back no WS stream.
    NoStream,
}

impl std::fmt::Display for SubscribeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Handshake { source } => f.write_str(source),
            Self::NoStream => f.write_str("BlockPump::subscribe returned no WS stream"),
        }
    }
}

impl std::error::Error for SubscribeRefusal {}

/// Why `connect` was refused: a pose the table does not admit, or the WS
/// transport.
#[derive(Debug)]
pub enum ConnectRefusal {
    /// The session's pose does not admit `connect` (including another lifecycle
    /// move already in flight).
    Pose(PhaseError),
    /// The WS handshake failed, or handed back no stream.
    Transport(SubscribeRefusal),
}

impl std::fmt::Display for ConnectRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pose(e) => write!(f, "{e}"),
            Self::Transport(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ConnectRefusal {}

/// The `S+1..W` backfill range a failed resume left UNAPPLIED, as data.
///
/// The resume aborted with the snapshot→WS gap unclosed, so there is no live
/// loop and no pool admitted over a stale gap. The caller starts a new session;
/// a re-subscribe is refused (`Abandoned` is terminal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackfillGap {
    /// The first block that was not applied (`S + 1`).
    pub from: u64,
    /// The last block that was not applied (the WS link block `W`).
    pub to: u64,
}

impl std::fmt::Display for BackfillGap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the resume backfill failed for blocks {}-{}; the gap is UNAPPLIED and no live loop was started",
            self.from, self.to
        )
    }
}

impl std::error::Error for BackfillGap {}

/// The closed `run` error surface.
///
/// A consumer matches on the variant; no string matching, and no arm that
/// exists only to carry a sentence. The three prose arms the old surface had
/// (`SessionState(String)`, `Subscribe(String)`, `Backfill(String)`) became a
/// typed pose refusal, a typed transport refusal at `connect`, and this gap.
/// `NoResultReceiver` is deleted as unrepresentable: the hand-off carries the
/// claim and `run` takes it by value, so "no consumer attached" has no state to
/// name. `Registration` left with the operation (ADR-093 D4/D7).
#[derive(Debug)]
pub enum DriverError {
    /// The session's pose does not admit `run` (a concurrent move in flight, a
    /// session that was never connected, or a terminal one).
    Phase(PhaseError),
    /// The `S+1..W` resume backfill failed; the gap is unapplied.
    Backfill(BackfillGap),
}

impl std::fmt::Display for DriverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Phase(e) => write!(f, "{e}"),
            Self::Backfill(gap) => write!(f, "{gap}"),
        }
    }
}

impl std::error::Error for DriverError {}

/// Why a crawl attach was refused.
#[derive(Debug)]
pub enum CrawlRefusal {
    /// The session's pose does not admit a crawl (a terminal or tearing-down
    /// session, or a move in flight).
    Pose(PhaseError),
    /// The crawl configuration is invalid, a `Bot` has no construction I/O, or
    /// this driver already holds a crawl.
    Config(CrawlConfigError),
    /// The receipt stream's sink was refused (already attached, or attached
    /// after the stream closed).
    Receipts(degenbot_core::channel_lifecycle::ChannelDecline),
}

impl std::fmt::Display for CrawlRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pose(e) => write!(f, "{e}"),
            Self::Config(e) => write!(f, "{e}"),
            Self::Receipts(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for CrawlRefusal {}

/// The session's link block: the first complete block `W` the WS handshake
/// observed, which is the `run` backfill's upper bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Connected {
    /// The first complete WS block (`W`).
    pub link_block: u64,
}

/// What a teardown actually tore down.
///
/// A report, not a `Result`: `close` is total by construction, so there is
/// nothing to refuse. Concurrent callers share ONE completion and all observe
/// this same value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeardownReport {
    /// The pose the teardown landed the session from.
    pub from: PumpPhase,
    /// How the live loop resolved.
    pub pump: PumpExit,
    /// How the crawl's run resolved.
    pub crawl: CrawlExit,
    /// Where the session's ordered event stream is after the teardown (the
    /// natural end's closure state).
    pub delivery: degenbot_core::channel_lifecycle::ChannelPhase,
}

/// Everything a crawl boot needs beside the configuration.
///
/// The configuration is a value; the two remaining inputs are HANDLES the boot
/// wiring owns: the receipt stream's sink (attached before the run arms, so no
/// receipt races past an unattached channel) and the tick-map arm a
/// head-stamped V3/V4 construction reads. A standalone consumer supplies
/// neither and reads the summary off the controller instead.
pub struct CrawlBoot {
    /// The resolved crawl configuration.
    pub config: CrawlConfig,
    /// The receipt consumer's sink, or `None` for a summary-only consumer.
    pub receipts: Option<tokio::sync::mpsc::UnboundedSender<CrawlReceipt>>,
    /// The snapshot tick-map arm — a handle SOURCE the crawl asks once per unit
    /// (`None` = construction reads no tick maps).
    pub snapshot: Option<Arc<dyn SnapshotArm>>,
}

impl CrawlBoot {
    /// A boot over one configuration, with no receipt consumer and no tick-map
    /// arm.
    #[must_use]
    pub fn new(config: CrawlConfig) -> Self {
        Self {
            config,
            receipts: None,
            snapshot: None,
        }
    }

    /// Attach the receipt consumer's sink.
    #[must_use]
    pub fn with_receipts(
        mut self,
        receipts: tokio::sync::mpsc::UnboundedSender<CrawlReceipt>,
    ) -> Self {
        self.receipts = Some(receipts);
        self
    }

    /// Attach a FIXED snapshot handle: every unit's ask clones it.
    #[must_use]
    pub fn with_db(mut self, db: Option<Arc<dyn degenbot_db::snapshot::TickMapDb>>) -> Self {
        self.snapshot = db.map(|db| Arc::new(Some(db)) as Arc<dyn SnapshotArm>);
        self
    }

    /// Attach a live arm, whose handle the owner may withdraw while the crawl
    /// runs — the process's own snapshot slot rather than a clone held here.
    #[must_use]
    pub fn with_snapshot_arm(mut self, arm: Arc<dyn SnapshotArm>) -> Self {
        self.snapshot = Some(arm);
        self
    }
}

/// A test-only hold inside a lifecycle move's in-flight window.
///
/// The window is otherwise a live WS handshake or a live backfill await; the
/// hold lets a test cancel a move exactly where a dropping consumer would.
#[cfg(test)]
pub struct InFlightHold {
    /// Dropping the hold releases the held move (a dropped sender resolves the
    /// receiver's await), which is exactly how a test lets a move unwind.
    _release: tokio::sync::oneshot::Sender<()>,
}

/// A test-only injected failure of `connect`'s fallible step.
///
/// `BlockPump::subscribe` dials a live WS endpoint, so its failure returns have
/// no in-process double. Both shapes leave the same thing to prove: the session
/// lands back on `Detached`, from which a retry is legal.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectFault {
    /// The WS handshake failed (`BlockPump::subscribe` returned `Err`).
    Handshake,
    /// The session opened but handed back no WS stream.
    NoStream,
}

#[cfg(test)]
impl ConnectFault {
    pub(crate) fn refusal(self) -> SubscribeRefusal {
        match self {
            Self::Handshake => SubscribeRefusal::Handshake {
                source: "injected: the WS handshake failed".to_string(),
            },
            Self::NoStream => SubscribeRefusal::NoStream,
        }
    }
}

/// The live loop's one exit door, for the crate's stand-in loops: every way a
/// seeded body can end — a return or a panic unwind — publishes the pump-exit
/// fact exactly as the production loop's own guard does.
#[cfg(test)]
struct SeededExitGuard(Arc<dyn PumpControl>);

#[cfg(test)]
impl Drop for SeededExitGuard {
    fn drop(&mut self) {
        self.0.on_pump_ended();
    }
}

/// A construction-time session seed: a session value built in a pose a test
/// cannot reach without a live WS endpoint.
///
/// This is a CONSTRUCTOR, not a poke: it builds a whole session value (pose plus
/// the payload that pose owns) and installs it, so a test exercises the real
/// move protocol from there on.
#[cfg(test)]
#[expect(
    clippy::large_enum_variant,
    reason = "a seed carries the pump it seeds the pose with; one value at a time"
)]
pub enum SessionSeed {
    /// A connected session: `Subscribed` owning a local pump and a scripted
    /// stream.
    Connected {
        /// The local pump the resume move will drive.
        pump: BlockPump,
        /// The first complete block `W`.
        first_block: u64,
        /// The scripted WS stream.
        stream: futures_util::stream::BoxStream<'static, WsEvent>,
    },
    /// A live-loop pose over a stand-in loop body. The driver wraps the body in
    /// the same exit guard production uses, so the exit edge is exercised.
    Running(futures_util::future::BoxFuture<'static, ()>),
}

/// The public Rust driver seam above the engine's one stage seam (ADR-093 D1).
///
/// Composes the shared `Bot`, the public `EngineStages`, the reorg coordinator,
/// the shutdown flag, the verify home, the hub, and the ONE private session.
pub struct EngineDriver {
    /// The shared per-chain orchestrator (ADR-006 D4).
    bot: Arc<Bot>,
    /// The engine's ONE public seam (ADR-049). Every engine touch crosses it.
    stages: Arc<EngineStages>,
    /// The per-event reorg coordinator (ADR-006 D4).
    reorg_coordinator: Arc<ReorgCoordinator>,
    /// The cooperative shutdown flag shared with the pump.
    shutdown: Arc<AtomicBool>,
    /// The session: the pose, the payloads it owns, the crawl's teardown pair,
    /// the completion broadcast, and the consumer claim. ONE owner.
    session: Arc<PumpSession>,
    /// The pump-completion receiver. Cloned per `wait_pump_finished` waiter; a
    /// clone created before OR after completion still resolves.
    pump_finished_rx: watch::Receiver<bool>,
    /// The pose-independent Verify lifecycle (ADR-093 D4).
    verify: Arc<VerifyLifecycle>,
    /// The engine host's process-lifetime event hub.
    hub: Arc<Hub>,
    /// The one teardown's completion, shared by every `close` caller.
    close: tokio::sync::Mutex<Option<TeardownReport>>,
    /// Test-only injected failure for the next `connect`.
    #[cfg(test)]
    connect_fault: Mutex<Option<ConnectFault>>,
    /// Test-only hold inside a connect's in-flight window.
    #[cfg(test)]
    connect_gate: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    /// Test-only hold inside a run's in-flight window.
    #[cfg(test)]
    run_gate: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    /// Test-only injected failure of the resume backfill.
    #[cfg(test)]
    backfill_fault: AtomicBool,
}

/// Bind the session's path-identity owner to the engine this driver drives.
///
/// A driver's `Bot` IS the session, and this driver's `EngineStages` is the
/// session's only path registry, so composition is the one place both halves
/// are in hand. A second install is reported, not merged: two engines in one
/// session are two path-id spaces, and the session keeps the FIRST owner so a
/// consumer's ask is never answered by the wrong engine.
fn install_session_path_owner(bot: &Bot, stages: &EngineStages) {
    if let Err(refused) = bot
        .session_registry()
        .install_path_objects(stages.session_path_objects())
    {
        op_error!(
            domain = path,
            rejected_owner_paths = refused.path_count(),
            "a session already has a path-identity owner; this engine's paths are unreachable from the session and the session answers with the first engine's path ids"
        );
    }
}

/// The driver's `PhaseError` for one refused move.
fn phase_error(
    method: &'static str,
    required: Option<PumpPhase>,
    decline: PumpDecline,
) -> PhaseError {
    PhaseError {
        method,
        current: decline.current(),
        required,
        detail: decline.to_string(),
    }
}

impl EngineDriver {
    /// Standalone-Rust construction: adopts the shared `Bot` and builds the
    /// stage seam from the caller's typed config (ADR-050 D2).
    #[must_use]
    pub fn new(bot: Arc<Bot>, cfg: &Arc<degenbot_config::BotConfig>) -> Self {
        let stages = Arc::new(EngineStages::with_core_cfg(
            bot.state_arc(),
            cfg,
            bot.active_delta(),
        ));
        Self::from_stages(bot, stages)
    }

    /// Adapter adoption: the caller already built the stage seam (the `PyO3`
    /// wrapper path). The driver mints a private hub for the engine's named
    /// source channels and composes; a host-minted hub uses
    /// [`Self::from_stages_with_hub`].
    #[must_use]
    pub fn from_stages(bot: Arc<Bot>, stages: Arc<EngineStages>) -> Self {
        let mut hub = Hub::new();
        let channels = EngineChannelHandles::register_on(&mut hub);
        Self::assemble(bot, stages, Arc::new(hub), channels)
    }

    /// Adapter adoption against a host-minted hub.
    ///
    /// `attached` is the pair `StrategyHost::mint` produces: the shared hub and
    /// the engine's named source channels registered on it by the mint closure
    /// (for the engine, [`EngineChannelHandles::register_on`]). A named channel
    /// is keyed by name per hub, so a hub backs at most one engine driver — a
    /// second `claim_consumer` returns `None`.
    #[must_use]
    pub fn from_stages_with_hub(
        bot: Arc<Bot>,
        stages: Arc<EngineStages>,
        attached: HostHub<EngineChannelHandles>,
    ) -> Self {
        let (hub, channels) = attached.into_parts();
        Self::assemble(bot, stages, hub, channels)
    }

    /// Compose a driver over an already-bound hub and channel producers.
    fn assemble(
        bot: Arc<Bot>,
        stages: Arc<EngineStages>,
        hub: Arc<Hub>,
        channels: EngineChannelHandles,
    ) -> Self {
        // The session's ordered source: ONE producer end for both arms, attached
        // before any caller can claim the consumer end.
        if let Err(e) = stages.attach_session_events(channels.session) {
            op_error!(
                domain = pump,
                %e,
                "EngineDriver: the session event source refused its producer; the stream cannot be claimed"
            );
        }
        // The stage surface's block-clock pipe is fresh here and this is its one
        // wiring site, so a decline is unreachable.
        let _ = stages.set_block_channel(channels.block);
        install_session_path_owner(&bot, &stages);

        let (completion_tx, pump_finished_rx) = watch::channel(false);
        let session = Arc::new(PumpSession::new(completion_tx));
        stages.install_session(Arc::clone(&session));

        let verify_core = stages.core();
        let reorg_coordinator = Arc::new(ReorgCoordinator::new(Arc::clone(&bot)));
        Self {
            bot,
            stages,
            reorg_coordinator,
            shutdown: Arc::new(AtomicBool::new(false)),
            session,
            pump_finished_rx,
            verify: Arc::new(VerifyLifecycle::new(
                verify_core,
                VerifyLifecycleConfig::default(),
            )),
            hub,
            close: tokio::sync::Mutex::new(None),
            #[cfg(test)]
            connect_fault: Mutex::new(None),
            #[cfg(test)]
            connect_gate: Mutex::new(None),
            #[cfg(test)]
            run_gate: Mutex::new(None),
            #[cfg(test)]
            backfill_fault: AtomicBool::new(false),
        }
    }

    /// The shared `BotState` arc (the stage surface's `core` handoff).
    #[must_use]
    pub fn core(&self) -> Arc<StateLock<BotState>> {
        self.stages.core()
    }

    /// The shared `Bot` orchestrator.
    #[must_use]
    pub fn bot(&self) -> &Arc<Bot> {
        &self.bot
    }

    /// The engine's ONE public seam — the observer/registration escape hatch.
    #[must_use]
    pub fn stages(&self) -> &Arc<EngineStages> {
        &self.stages
    }

    /// The session's verify home: the pose-independent Verify lifecycle, its
    /// construction values, its claim window and its verified-pool facts.
    #[must_use]
    pub fn verify(&self) -> &Arc<VerifyLifecycle> {
        &self.verify
    }

    /// The current engine lifecycle phase — derived from the session's pose.
    #[must_use]
    pub fn current_phase(&self) -> PumpPhase {
        self.session.phase()
    }

    /// `true` once the session machine is teardown-bound: `Stopping` from the
    /// close's first statement, `Stopped` when the teardown completes.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.current_phase().is_terminal()
    }

    /// The engine host's process-lifetime hub.
    #[must_use]
    pub fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }

    /// The registered pool id for this pool identity, derived from the shared
    /// `BotState`.
    ///
    /// `None` when no pool with that identity is registered in this session.
    #[must_use]
    pub fn pool_id_for_identity(&self, identity: &PoolIdentity) -> Option<u64> {
        let core = self.stages.core();
        let pool_id = core
            .read_at(LockSite::Registration)
            .pool_id_for_identity(identity);
        pool_id
    }

    /// The snapshot seed block `S` — read from the shared core.
    #[must_use]
    pub fn snapshot_seed_block(&self) -> Option<u64> {
        self.bot
            .state_arc()
            .read_at(LockSite::Pump)
            .snapshot_seed_block()
    }

    /// Store the snapshot seed block `S` on the shared core (the non-DB
    /// snapshot path; the DB path sets it in `Bot::load_snapshot_from_db`).
    pub fn set_snapshot_seed_block(&self, block: Option<u64>) {
        self.bot
            .state_arc()
            .write_at(LockSite::Pump)
            .set_snapshot_seed_block(block);
    }

    /// Take the block-clock observer tap's receiver — once only.
    ///
    /// The forwarded-header edge (ADR-027) the observer surfaces read. The
    /// session's own clock is the ordered stream's `Block` arm; both are written
    /// by the same producer call, so the two can never disagree about the clock.
    #[must_use]
    pub fn take_block_receiver(
        &self,
    ) -> Option<degenbot_eventhub::NamedReceiver<BlockNotification>> {
        match self
            .hub
            .take_named_counting_receiver::<BlockNotification>(BLOCK_CHANNEL_NAME)
        {
            Ok(rx) => rx,
            Err(e) => {
                op_error!(
                    domain = pump,
                    %e,
                    "EngineDriver: block source channel missing from the hub"
                );
                None
            }
        }
    }

    /// Await the spawned pump task's completion — cooperative exit, stream end,
    /// abort, or panic.
    ///
    /// The completion is a `watch` broadcast whose terminal state is retained,
    /// so a waiter created before OR after the pump ends resolves promptly
    /// instead of hanging. The pump task owns the channel's only sender;
    /// dropping it (a normal return, or the unwind of a panic) closes the
    /// channel, which `changed()` reports as end-of-stream.
    pub(crate) async fn wait_pump_finished(&self) {
        let mut rx = self.pump_finished_rx.clone();
        loop {
            if *rx.borrow_and_update() {
                return;
            }
            // `Err` = every sender dropped: the pump task ended (normal return
            // or panic). That IS completion.
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    /// Take the session's consumer hand-off: the ordered event stream AND the
    /// claim that proves a consumer holds it. Once per session.
    ///
    /// # Errors
    ///
    /// [`ClaimRefusal::AlreadyClaimed`] when the session's one hand-off already
    /// happened: the first consumer's stream must not be stranded with a second
    /// producer unaware of it.
    pub fn claim_consumer(&self) -> Result<EventHandoff, ClaimRefusal> {
        if !self.session.claim() {
            return Err(ClaimRefusal::AlreadyClaimed);
        }
        match self
            .hub
            .take_named_counting_receiver::<SessionEvent>(SESSION_EVENT_CHANNEL_NAME)
        {
            Ok(Some(rx)) => Ok(EventHandoff::new(
                SessionStream::new(rx.into_inner()),
                ConsumerClaim::mint(),
            )),
            Ok(None) => Err(ClaimRefusal::AlreadyClaimed),
            Err(e) => {
                op_error!(
                    domain = pump,
                    %e,
                    "EngineDriver: the session event channel is missing from the hub"
                );
                Err(ClaimRefusal::AlreadyClaimed)
            }
        }
    }

    /// Open the WS subscriptions and observe the first complete block `W`:
    /// `Detached -> Connecting -> Subscribed`.
    ///
    /// Stops **before** production: no batch flows down a stream no consumer
    /// holds. A handshake failure (or a cancelled connect) lands `Detached`, so
    /// the move stays retryable; an `Abandoned` session refuses it by name.
    ///
    /// # Errors
    ///
    /// [`ConnectRefusal::Pose`] when the pose does not admit the move (including
    /// another lifecycle move in flight), and [`ConnectRefusal::Transport`] when
    /// the WS step fails.
    pub async fn connect(&self, node_ws: &str) -> Result<Connected, ConnectRefusal> {
        let mv = self
            .session
            .take(PumpEvent::SubscribeRequested)
            .map_err(|decline| {
                ConnectRefusal::Pose(phase_error("connect", Some(PumpPhase::Detached), decline))
            })?;

        #[cfg(test)]
        {
            // The guard is dropped at the end of the `let`, never held across the
            // await below (workspace convention: no `parking_lot` guard crosses
            // one).
            let gate = self.connect_gate.lock().take();
            if let Some(gate) = gate {
                // Hold the future inside the in-flight window so a test can cancel
                // it exactly where a dropping consumer would.
                let _ = gate.await;
            }
        }

        let pending = match self.open_ws_session(node_ws).await {
            Ok(pending) => pending,
            Err(refusal) => {
                // The table's failure landing: `Connecting -> Detached`, so the
                // next `connect` is legal again.
                if let Err(landing) = mv.land_empty(PumpEvent::SubscribeFailed) {
                    op_warn!(
                        domain = pump,
                        error = %landing,
                        "EngineDriver: a failed connect could not restore the session to detached"
                    );
                }
                return Err(ConnectRefusal::Transport(refusal));
            }
        };

        let link_block = pending.first_block;
        mv.land_connected(Box::new(pending)).map_err(|decline| {
            ConnectRefusal::Pose(phase_error("connect", Some(PumpPhase::Detached), decline))
        })?;
        Ok(Connected { link_block })
    }

    /// Open the WS session: the ONE fallible step of [`Self::connect`].
    async fn open_ws_session(&self, node_ws: &str) -> Result<ConnectedSession, SubscribeRefusal> {
        #[cfg(test)]
        if let Some(fault) = self.connect_fault.lock().take() {
            return Err(fault.refusal());
        }

        let stage_handlers: Arc<dyn StageHandlers> = self.stages.clone();
        let control: Arc<dyn PumpControl> = self.stages.clone();
        let (pump, state) = BlockPump::subscribe(
            node_ws,
            Arc::clone(&self.bot),
            stage_handlers,
            control,
            Arc::clone(&self.reorg_coordinator),
            Arc::clone(&self.shutdown),
        )
        .await
        .map_err(|source| SubscribeRefusal::Handshake { source })?;

        let SubscribeState {
            first_block,
            combined_stream,
            ..
        } = state;
        let Some(stream) = combined_stream else {
            return Err(SubscribeRefusal::NoStream);
        };
        Ok(ConnectedSession {
            pump,
            first_block,
            stream,
        })
    }

    /// Arm the live loop: `Subscribed -> Resuming -> Running`.
    ///
    /// The driver **owns** the `S+1..W` auto-backfill: it awaits
    /// `BlockPump::backfill_with_drain(W, stream)` synchronously, then spawns the
    /// live loop. Consumers never call `backfill_from_snapshot`.
    ///
    /// The claim is taken BY VALUE: `run` cannot be called without one, so
    /// "a consumer is attached before production starts" is a type fact rather
    /// than an ordering convention.
    ///
    /// # Errors
    ///
    /// - [`DriverError::Phase`] unless the table admits `run` (it requires
    ///   `Subscribed`, and refuses a concurrent move as in flight).
    /// - [`DriverError::Backfill`] when the `S+1..W` backfill fails: the gap is
    ///   UNAPPLIED, so the resume aborts (landing `Abandoned`) rather than
    ///   starting a live loop over stale state.
    ///
    /// # Cancel safety
    ///
    /// **Committed from the spawn on**: `run` does not own the pump it starts.
    /// Once the live loop is handed to `tokio::spawn`, the spawned task OWNS the
    /// pump; only its `JoinHandle` is stored back into the session's `Running`
    /// pose.
    ///
    /// **Cancel-safe before it, by landing `Abandoned`**: the in-flight window is
    /// the subscribe-state handover and the awaited `backfill_with_drain`.
    /// `run` publishes the table's `Resuming` at the top of that window and arms
    /// a [`SessionMove`](crate::arb_engine::pump_session::SessionMove), whose
    /// `Drop` publishes the table's `ResumeAbandoned` edge — so a future dropped
    /// mid-window lands `Abandoned` (the pump and its WS session died with it)
    /// instead of standing in `Resuming` with nothing left to resume.
    /// `Abandoned` is terminal: `close` is its one exit, and a re-connect is
    /// refused by name.
    ///
    /// A cancel after the `ResumeSpawned` landing is NOT "not running": the live
    /// loop is armed and owned by the `Running` pose.
    pub async fn run(&self, claim: ConsumerClaim) -> Result<(), DriverError> {
        // The claim's work is the attachment proof; consuming it is enough.
        let _ = claim;

        let mut mv = self
            .session
            .take(PumpEvent::ResumeRequested)
            .map_err(|decline| {
                DriverError::Phase(phase_error("run", Some(PumpPhase::Subscribed), decline))
            })?;

        let MovePayload::Connected(pending) = mv.take_payload() else {
            // Unreachable: the table admits `ResumeRequested` only from
            // `Subscribed`, and that pose always owns a connected session.
            debug_assert!(false, "a resume take-out must carry the connected session");
            let _ = mv.land_empty(PumpEvent::ResumeAbandoned);
            return Err(DriverError::Phase(phase_error(
                "run",
                Some(PumpPhase::Subscribed),
                PumpDecline::ResumeRequiresSubscribed {
                    current: self.current_phase(),
                },
            )));
        };
        let ConnectedSession {
            mut pump,
            first_block,
            stream,
        } = *pending;

        #[cfg(test)]
        {
            let gate = self.run_gate.lock().take();
            if let Some(gate) = gate {
                // Hold the future inside the in-flight window so a test can cancel
                // it exactly where a dropping consumer cancels one.
                let _ = gate.await;
            }
        }

        let gap = self.backfill_gap(first_block);

        // the backfill is SYNCHRONOUS with respect to `run` so the consumer's
        // registration draining the per-pool backfill buffer cannot race it.
        // `backfill_with_drain` also re-injects live events drained during the
        // backfill ahead of the live tail.
        #[cfg(test)]
        let injected = self.backfill_fault.swap(false, Ordering::SeqCst);
        #[cfg(not(test))]
        let injected = false;

        let (backfill_res, combined) = if injected {
            (
                Err(gap.map_or_else(
                    || "injected: the resume backfill failed".to_string(),
                    |gap| {
                        format!(
                            "injected: eth_getLogs failed for blocks {}-{}",
                            gap.from, gap.to
                        )
                    },
                )),
                stream,
            )
        } else {
            pump.backfill_with_drain(first_block, stream).await
        };

        let combined = match backfill_res {
            Ok(_) => combined,
            // The gap `S+1..W` is UNAPPLIED, so a live loop from `W` would run
            // over stale pool state: abort the resume instead. The pump and its
            // WS session drop with this early return, exactly as they do on a
            // cancelled resume, so the table's honest landing is the same
            // `Resuming -> Abandoned` edge. The failure itself is the caller's,
            // typed — the pose carries no payload.
            Err(detail) => {
                op_error!(
                    domain = pump,
                    first_block,
                    error = %detail,
                    "EngineDriver: auto-backfill failed — resume aborted, live loop NOT started (gap not closed)"
                );
                if let Err(landing) = mv.land_empty(PumpEvent::ResumeAbandoned) {
                    op_warn!(
                        domain = pump,
                        error = %landing,
                        "EngineDriver: a failed backfill could not land the abandon edge"
                    );
                }
                let Some(gap) = gap else {
                    // No gap means the failure was not about `S+1..W`; report it
                    // as the empty range at the link block.
                    return Err(DriverError::Backfill(BackfillGap {
                        from: first_block,
                        to: first_block,
                    }));
                };
                return Err(DriverError::Backfill(gap));
            }
        };

        // Arm the completion broadcast: the sender is owned by the spawned task,
        // so wherever the pump ends — cooperative return, stream end, abort, or
        // panic — the sender drops and every `wait_pump_finished` waiter
        // resolves. No explicit send is needed; channel close IS the terminal
        // event (which is also what makes the panic path work).
        let completion = self.session.take_completion();
        let handle = degenbot_core::runtime::get_runtime().spawn(async move {
            let _completion = completion;
            pump.run_with_stream(combined, first_block).await;
        });

        // The landing is the ONLY way the live loop enters the session: a
        // landing the table no longer admits (a close that got there first)
        // aborts the loop instead of installing it.
        mv.land_live(Box::new(LiveLoop {
            handle,
            completion: None,
        }))
        .map_err(|decline| {
            DriverError::Phase(phase_error("run", Some(PumpPhase::Subscribed), decline))
        })?;
        Ok(())
    }

    /// The `S+1..W` range a resume must close, when the snapshot seed is behind
    /// the WS link block.
    fn backfill_gap(&self, link_block: u64) -> Option<BackfillGap> {
        let seed = self.snapshot_seed_block()?;
        if seed == 0 || link_block == 0 || seed >= link_block {
            return None;
        }
        Some(BackfillGap {
            from: seed + 1,
            to: link_block,
        })
    }

    /// Tear the session down: asynchronous, total from every pose, idempotent,
    /// and shared.
    ///
    /// Concurrent callers join the ONE teardown and each receives the same
    /// [`TeardownReport`]. The teardown requests termination, waits for any
    /// in-flight move to unwind (its guard lands the abandonment edge first),
    /// latches the crawl's stop BEFORE the pump join so both loops unwind at
    /// once, joins each loop under one grace window, drains the walk memo after
    /// the pump is down, closes the event stream exactly once, and publishes
    /// `StopCompleted` last.
    ///
    /// Nothing here parks a runtime: the joins are awaits, so the old
    /// `block_on`-inside-the-runtime hazard is gone. A caller that must block
    /// does it at its own adapter boundary.
    pub async fn close(&self) -> TeardownReport {
        let mut slot = self.close.lock().await;
        if let Some(report) = *slot {
            return report;
        }
        let report = self.teardown().await;
        *slot = Some(report);
        report
    }

    /// The one teardown body, run under the shared completion latch.
    async fn teardown(&self) -> TeardownReport {
        // The table's teardown entry first: it is the machine's own answer to
        // "has teardown begun?", which every guard (and `is_stopped`) reads.
        let mut mv = match self.session.take(PumpEvent::Stop) {
            Ok(mv) => mv,
            Err(decline) => {
                // `Stop` never declines; a decline means the machine is corrupt.
                op_error!(domain = pump, %decline, "EngineDriver: the teardown entry declined");
                return TeardownReport {
                    from: self.current_phase(),
                    pump: PumpExit::NotArmed,
                    crawl: CrawlExit::None,
                    delivery: self.stages.session_event_source().phase(),
                };
            }
        };
        let from = mv.from();
        let payload = mv.take_payload();
        mv.release();

        let delivery = self.stages.session_event_source().phase();
        let MovePayload::Teardown(mut teardown) = payload else {
            debug_assert!(false, "a stop take-out must carry the teardown payload");
            return TeardownReport {
                from,
                pump: PumpExit::NotArmed,
                crawl: CrawlExit::None,
                delivery,
            };
        };

        if from.is_terminal() {
            // Another close owns (or owned) the teardown: this caller reports
            // the same facts rather than tearing down twice.
            teardown.latch_crawl();
            let (pump, crawl) = teardown.join(PUMP_STOP_GRACE).await;
            return TeardownReport {
                from,
                pump,
                crawl,
                delivery,
            };
        }

        // Request termination, then wait for any in-flight move to unwind: its
        // landing declines against the teardown pose and its guard aborts
        // whatever it could not install.
        self.shutdown.store(true, Ordering::Relaxed);
        self.session.wait_for_moves().await;

        // Latch the crawl's stop BEFORE the pump join, so both loops unwind at
        // once: the crawl stops submitting and drains its window while the pump
        // walks its own tick.
        teardown.latch_crawl();

        if let Some(crawl) = teardown.crawl.as_ref() {
            op_info!(
                domain = pump,
                grace_ms = pump_stop_grace_ms(),
                crawl = ?crawl.handle.state(),
                "EngineDriver: crawl stop latched before the pump join"
            );
        }
        let (pump, crawl) = teardown.join(PUMP_STOP_GRACE).await;
        match pump {
            PumpExit::Cooperative => op_info!(
                domain = pump,
                grace_ms = pump_stop_grace_ms(),
                "EngineDriver: BlockPump task exited cooperatively on the shutdown flag"
            ),
            PumpExit::NotArmed => op_info!(
                domain = pump,
                "EngineDriver: BlockPump not running (no live loop to join)"
            ),
            PumpExit::Aborted | PumpExit::JoinError => {}
        }

        // Final WalkMemo stats drain: the pump is down and no further cycle can
        // advance an epoch, so drain the LAST epoch's counters here — no boundary
        // will. Inert unless the memo recorded activity.
        self.stages.drain_walk_memo_final();

        // Close the stream exactly once — the natural end's one owner.
        self.stages.close_delivery_channels();
        let delivery = self.stages.session_event_source().phase();

        // `StopCompleted` last: nothing above this line may still publish.
        if let Err(decline) = self.session.complete_close() {
            op_warn!(
                domain = pump,
                %decline,
                "EngineDriver: the teardown could not land StopCompleted"
            );
        }
        TeardownReport {
            from,
            pump,
            crawl,
            delivery,
        }
    }

    /// Configure a crawl over this driver's `Bot` and boot it.
    ///
    /// The DRIVER boots the crawl: it composes the host from the driver's own
    /// `Bot` (whose `ConstructionIo` the shell attached at boot time), attaches
    /// the receipt stream's sink, spawns [`CrawlController::run`], and hands the
    /// run future's `JoinHandle` to the session, so ONE [`Self::close`] tears
    /// both loops down under one grace window. The caller keeps the controller —
    /// the operator surface and the summary render — and nothing else.
    ///
    /// # Errors
    ///
    /// [`CrawlRefusal::Pose`] when the session does not admit an attach (it is
    /// gone or tearing down — the §1.3(b) regression), and
    /// [`CrawlRefusal::Config`] for an invalid configuration, a `Bot` with no
    /// construction I/O, or a driver that already holds a crawl.
    pub fn begin_crawl(
        self: &Arc<Self>,
        config: CrawlConfig,
    ) -> Result<Arc<CrawlController>, CrawlRefusal> {
        self.begin_crawl_with(CrawlBoot::new(config))
    }

    /// Boot a crawl with the boot's receipt sink and tick-map arm attached.
    ///
    /// # Errors
    ///
    /// As [`Self::begin_crawl`].
    pub fn begin_crawl_with(
        self: &Arc<Self>,
        boot: CrawlBoot,
    ) -> Result<Arc<CrawlController>, CrawlRefusal> {
        // The attach is a SESSION MOVE: the table answers legality, so a crawl
        // cannot be booted over a session that is gone or tearing down.
        let mv = self
            .session
            .take(PumpEvent::CrawlAttached)
            .map_err(|decline| CrawlRefusal::Pose(phase_error("crawl", None, decline)))?;

        let CrawlBoot {
            config,
            receipts,
            snapshot,
        } = boot;

        let io = self.bot.construction_io_arc().ok_or(CrawlRefusal::Config(
            CrawlConfigError::MissingConstructionIo,
        ))?;

        // The crawl is one ADAPTER over Registration admission: it contributes
        // its candidate source, its loop and its measurement, and the admission
        // is composed here from the capability handles this session owns.
        let admission = Arc::new(RegistrationAdmission::new(
            Arc::clone(&self.bot),
            io,
            Arc::clone(&self.stages),
            Arc::clone(&self.verify),
            Arc::new(CrawlLedger::new()),
            Arc::new(BuildFlights::new()),
            Arc::new(config.admission_settings()),
        ));
        let host = CrawlHost {
            admission,
            snapshot,
        };
        let controller = CrawlController::attach(host, config).map_err(CrawlRefusal::Config)?;
        if let Some(tx) = receipts {
            controller
                .attach_receipts(tx)
                .map_err(CrawlRefusal::Receipts)?;
        }

        let runner = Arc::clone(&controller);
        let run = degenbot_core::runtime::get_runtime().spawn(async move {
            // The run's own answer is recorded on the controller (readable after
            // end-of-stream); the join handle reports only a panic.
            let _ = runner.run().await;
        });

        mv.land_crawl(Box::new(CrawlTeardown {
            handle: CrawlHandle::new(Arc::clone(&controller)),
            run,
        }))
        .map_err(|decline| CrawlRefusal::Pose(phase_error("crawl", None, decline)))?;
        Ok(controller)
    }

    /// The crawl's narrowed teardown edge, for a caller that must read the run's
    /// join fact without stopping it.
    #[must_use]
    pub fn crawl_handle(&self) -> Option<CrawlHandle> {
        self.session.crawl_edge()
    }
}

/// Soak 2026-08-22 forensics: name teardown paths that bypass
/// [`EngineDriver::close`]. If the session still holds an armed live loop at
/// drop time, unwinding tore down the driver without calling `close()` — the
/// silent-exit shape this exists to catch. Leveling: only the bypassed-close
/// shape is WARN; a post-close drop is the healthy path every session takes at
/// exit.
impl Drop for EngineDriver {
    fn drop(&mut self) {
        if self.current_phase().live_loop_armed() {
            op_warn!(
                domain = pump,
                pump_task_still_armed = true,
                "EngineDriver dropped WITHOUT close() — unwind bypassed graceful shutdown"
            );
        } else {
            diag!(
                domain = pump,
                pump_task_still_armed = false,
                "EngineDriver dropped after close()"
            );
        }
    }
}

// ── the crate's own construction-time seams (test builds only) ─────────────

#[cfg(test)]
impl EngineDriver {
    /// A local pump over this driver's own seams: no WS endpoint, no dial.
    pub(crate) fn local_pump_for_test(&self) -> BlockPump {
        let stage_handlers: Arc<dyn StageHandlers> = self.stages.clone();
        let control: Arc<dyn PumpControl> = self.stages.clone();
        BlockPump::for_test(
            Arc::clone(&self.bot),
            stage_handlers,
            control,
            Arc::clone(&self.reorg_coordinator),
            mock_provider(),
            Arc::clone(&self.shutdown),
        )
    }

    /// Build the session in a pose a test cannot reach without a live WS
    /// endpoint — a whole session VALUE (pose plus the payload it owns), never a
    /// field poke.
    pub(crate) fn seed_session_for_test(&self, seed: SessionSeed) {
        match seed {
            SessionSeed::Connected {
                pump,
                first_block,
                stream,
            } => self
                .session
                .seed_connected_for_test(Box::new(ConnectedSession {
                    pump,
                    first_block,
                    stream,
                })),
            SessionSeed::Running(body) => {
                let control: Arc<dyn PumpControl> = self.stages.clone();
                // The completion broadcast is owned by the loop while it runs,
                // exactly as production hands it over: the channel's close is the
                // completion signal, so a panic completes too.
                let completion = self.session.take_completion();
                let handle = degenbot_core::runtime::get_runtime().spawn(async move {
                    let _completion = completion;
                    // The same one exit door production uses: every return from a
                    // live loop — normal, early, or a panic unwind — publishes
                    // the pump-exit fact.
                    let _guard = SeededExitGuard(control);
                    body.await;
                });
                self.session.seed_running_for_test(Box::new(LiveLoop {
                    handle,
                    completion: None,
                }));
            }
        }
    }

    /// Inject the next `connect`'s failure shape.
    pub(crate) fn inject_connect_fault_for_test(&self, fault: ConnectFault) {
        *self.connect_fault.lock() = Some(fault);
    }

    /// Hold `connect` inside its in-flight window until the hold releases.
    pub(crate) fn hold_connect_in_flight_for_test(&self) -> InFlightHold {
        let (tx, rx) = tokio::sync::oneshot::channel();
        *self.connect_gate.lock() = Some(rx);
        InFlightHold { _release: tx }
    }

    /// Hold `run` inside its in-flight window until the hold releases.
    pub(crate) fn hold_run_in_flight_for_test(&self) -> InFlightHold {
        let (tx, rx) = tokio::sync::oneshot::channel();
        *self.run_gate.lock() = Some(rx);
        InFlightHold { _release: tx }
    }

    /// Make the next resume backfill fail.
    pub(crate) fn inject_backfill_fault_for_test(&self) {
        self.backfill_fault.store(true, Ordering::SeqCst);
    }

    /// Attach a crawl's teardown pair through the session's `CrawlAttached` move
    /// — the crate-test construction seam for a controller a test builds itself
    /// (the boot wiring's `begin_crawl_with` composes the host; a fixture needs a
    /// scripted seat instead).
    ///
    /// # Errors
    ///
    /// [`CrawlRefusal`] when the table does not admit the attach, or the session
    /// already owns a crawl.
    #[cfg(test)]
    pub(crate) fn attach_crawl_teardown_for_test(
        &self,
        controller: &Arc<CrawlController>,
        run: tokio::task::JoinHandle<()>,
    ) -> Result<(), CrawlRefusal> {
        let mv = self
            .session
            .take(PumpEvent::CrawlAttached)
            .map_err(|decline| CrawlRefusal::Pose(phase_error("crawl", None, decline)))?;
        mv.land_crawl(Box::new(CrawlTeardown {
            handle: CrawlHandle::new(Arc::clone(controller)),
            run,
        }))
        .map_err(|decline| CrawlRefusal::Pose(phase_error("crawl", None, decline)))?;
        Ok(())
    }
}

/// An alloy mock-transport provider (never hit on the no-RPC test paths).
#[cfg(test)]
fn mock_provider() -> Arc<degenbot_rpc::provider::AlloyProvider> {
    mock_provider_and_asserter().0
}

/// [`mock_provider`] plus the `Asserter`, so a test can queue the exact RPC
/// response a backfill will consume.
#[cfg(test)]
fn mock_provider_and_asserter() -> (
    Arc<degenbot_rpc::provider::AlloyProvider>,
    alloy::transports::mock::Asserter,
) {
    use alloy::network::Ethereum as NetEth;
    use alloy::providers::{Provider, ProviderBuilder};
    use alloy::rpc::client::ClientBuilder;
    use alloy::transports::mock::{Asserter, MockTransport};
    let asserter = Asserter::new();
    let client = ClientBuilder::default().transport(MockTransport::new(asserter.clone()), true);
    let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
    (
        Arc::new(degenbot_rpc::provider::AlloyProvider::from_provider(
            Arc::new(dyn_provider) as Arc<dyn Provider<NetEth>>,
        )),
        asserter,
    )
}
#[cfg(test)]
#[expect(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::arb_engine::crawl::controller::{
        CrawlState, CrawlUnitFuture, CrawlUnitSeat, RunPhase,
    };
    use crate::arb_engine::crawl::discovery::{CrawlDiscoveryError, DiscoverySource, Edition};
    use crate::arb_engine::crawl::policy::CrawlPathPolicy;
    use crate::bot_core::construction_io::{ConstructionIo, NoDb, RpcConstruction};
    use crate::bot_core::verification_retry::RetryPolicy;
    use crate::bot_core::ConstructionRoute;
    use alloy::primitives::{address, Bytes, U256};
    use degenbot_db::PathGraphData;
    use degenbot_eventhub::OverflowPolicy;
    use degenbot_pathfinding::PoolKind;
    use futures_util::StreamExt;
    use hashbrown::{HashMap, HashSet};
    use tokio::sync::Notify;

    fn runtime() -> &'static tokio::runtime::Runtime {
        degenbot_core::runtime::get_runtime()
    }

    /// Build an offline driver over a fresh `Bot` + `EngineStages`.
    fn driver_for_test() -> EngineDriver {
        let bot = Arc::new(Bot::new(1));
        let stages = Arc::new(EngineStages::with_core(bot.state_arc(), bot.active_delta()));
        EngineDriver::from_stages(bot, stages)
    }

    #[test]
    fn new_driver_starts_detached() {
        let driver = driver_for_test();
        assert_eq!(driver.current_phase(), PumpPhase::Detached);
        assert!(!driver.is_stopped());
        assert!(!driver.current_phase().live_loop_armed());
    }

    #[test]
    fn the_driver_hub_carries_the_ordered_source_and_the_block_tap() {
        let driver = driver_for_test();
        assert_eq!(
            driver.hub().named_policy(SESSION_EVENT_CHANNEL_NAME),
            Some(OverflowPolicy::UnboundedFlagged {
                name: SESSION_EVENT_CHANNEL_NAME
            })
        );
        assert_eq!(
            driver.hub().named_policy(BLOCK_CHANNEL_NAME),
            Some(OverflowPolicy::UnboundedFlagged {
                name: BLOCK_CHANNEL_NAME
            })
        );
        assert_eq!(
            driver.hub().unbounded_flagged_count(),
            2,
            "both engine channels are the deliberate unbounded audit posture"
        );
    }

    #[test]
    fn take_block_receiver_is_once_only() {
        let driver = driver_for_test();
        assert!(driver.take_block_receiver().is_some());
        assert!(driver.take_block_receiver().is_none());
    }

    #[test]
    fn a_host_minted_hub_wires_one_driver() {
        use crate::connector_index::V2ConnectorIndex;
        use crate::strategy_host::StrategyHost;
        use degenbot_substrate::nonce::NonceAuthority;
        use degenbot_substrate::route_registry::RouteRegistry;

        let (host, attached) = StrategyHost::mint(
            Arc::new(RouteRegistry::new(V2ConnectorIndex::default())),
            Arc::new(NonceAuthority::new(7)),
            EngineChannelHandles::register_on,
        );

        let bot = Arc::new(Bot::new(1));
        let stages = Arc::new(EngineStages::with_core(bot.state_arc(), bot.active_delta()));
        let driver = EngineDriver::from_stages_with_hub(bot, stages, attached);

        assert!(
            Arc::ptr_eq(host.hub(), driver.hub()),
            "the driver attached to the host-minted hub, not a private one"
        );
        for name in [SESSION_EVENT_CHANNEL_NAME, BLOCK_CHANNEL_NAME] {
            assert_eq!(
                host.hub().named_policy(name),
                Some(OverflowPolicy::UnboundedFlagged { name }),
                "the host-minted hub carries {name}"
            );
        }

        assert!(driver.take_block_receiver().is_some());
        // The stream the driver hands out must have the stages' live producer
        // behind it: an orphaned stream (its sender dropped) resolves `recv`
        // immediately, while a wired one stays pending until `close` closes it.
        let (mut stream, _claim) = driver.claim_consumer().expect("the hand-off").split();
        assert!(
            futures_util::FutureExt::now_or_never(stream.recv()).is_none(),
            "the host-minted stream has a live producer wired to it"
        );
        runtime().block_on(driver.close());
        let end = runtime().block_on(async { stream.recv().await });
        assert!(
            end.is_none(),
            "the host-minted stream closes with its driver"
        );
    }

    #[test]
    fn snapshot_seed_block_roundtrips_through_the_shared_core() {
        let driver = driver_for_test();
        assert_eq!(driver.snapshot_seed_block(), None);
        driver.set_snapshot_seed_block(Some(123));
        assert_eq!(driver.snapshot_seed_block(), Some(123));
        assert_eq!(
            driver
                .bot()
                .state_arc()
                .read_at(LockSite::Pump)
                .snapshot_seed_block(),
            Some(123)
        );
        driver.set_snapshot_seed_block(None);
        assert_eq!(driver.snapshot_seed_block(), None);
    }

    #[test]
    fn close_is_idempotent_and_lands_stopped() {
        let driver = driver_for_test();
        let first = runtime().block_on(driver.close());
        assert_eq!(first.from, PumpPhase::Detached);
        assert_eq!(driver.current_phase(), PumpPhase::Stopped);
        assert!(driver.is_stopped());
        let second = runtime().block_on(driver.close());
        assert_eq!(second, first, "a repeated close is the same report");
        // And both moves are refused by the table afterwards.
        assert!(matches!(
            runtime().block_on(driver.run(super::tests::mint_claim(&driver))),
            Err(DriverError::Phase(_))
        ));
    }

    /// A claim for a `run` a test drives directly: the session mints one per
    /// hand-off, and the tests that do not read the stream still need one.
    fn mint_claim(driver: &EngineDriver) -> ConsumerClaim {
        let (_stream, claim) = driver
            .claim_consumer()
            .expect("one hand-off per session")
            .split();
        claim
    }

    #[test]
    fn close_closes_the_session_stream_exactly_once() {
        let driver = driver_for_test();
        let (mut stream, _claim) = driver.claim_consumer().expect("the hand-off").split();
        runtime().block_on(driver.close());
        assert!(runtime().block_on(async { stream.recv().await }).is_none());
        assert!(
            runtime().block_on(async { stream.recv().await }).is_none(),
            "a second read stays ended"
        );
    }

    #[test]
    fn close_aborts_an_armed_pump_handle() {
        let driver = driver_for_test();
        let handle = runtime().spawn(std::future::pending::<()>());
        // A stand-in live loop, seeded through the session's own running pose.
        driver.seed_session_for_test(SessionSeed::Running(Box::pin(async {})));
        drop(handle);
        let started = std::time::Instant::now();
        runtime().block_on(driver.close());
        assert!(
            started.elapsed() < PUMP_STOP_GRACE,
            "a close with a finished loop resolves promptly; took {:?}",
            started.elapsed()
        );
        assert!(!driver.current_phase().live_loop_armed());
    }

    #[test]
    fn run_owns_the_backfill_and_spawns_the_live_loop() {
        let driver = driver_for_test();
        let (_stream, claim) = driver.claim_consumer().expect("the hand-off").split();
        driver.seed_session_for_test(SessionSeed::Connected {
            pump: driver.local_pump_for_test(),
            first_block: 100,
            stream: futures_util::stream::pending::<WsEvent>().boxed(),
        });
        runtime()
            .block_on(driver.run(claim))
            .expect("run with a seeded connected session");
        assert_eq!(
            driver.current_phase(),
            PumpPhase::Running,
            "run lands the table's `Subscribed -> Resuming -> Running` chain"
        );
        assert!(driver.current_phase().live_loop_armed());
        // A second run cannot even be attempted: the claim is consumed by value,
        // and the session hands out exactly one (the type-level attachment proof).
        assert!(driver.claim_consumer().is_err());
        runtime().block_on(driver.close());
        assert!(!driver.current_phase().live_loop_armed());
    }

    // ── the crawl's teardown ordering ─────────────────────────────────────

    const CRAWL_BLOCK: u64 = 18_000_000;
    const CRAWL_WETH: alloy::primitives::Address =
        address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
    const CRAWL_USDC: alloy::primitives::Address =
        address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
    const CRAWL_V4_PM: alloy::primitives::Address =
        address!("3333333333333333333333333333333333333333");
    const CRAWL_V4_OFFSET: u64 = 1 << 32;
    const CRAWL_TOKEN_A: u64 = 1;
    const CRAWL_TOKEN_B: u64 = 2;

    struct UnreachedRpc;

    #[async_trait::async_trait]
    impl RpcConstruction for UnreachedRpc {
        async fn get_block_number(&self) -> Result<u64, degenbot_core::errors::ProviderError> {
            Ok(CRAWL_BLOCK)
        }
        async fn get_block(
            &self,
            _block_number: u64,
        ) -> Result<Option<degenbot_rpc::provider::EthBlock>, degenbot_core::errors::ProviderError>
        {
            Ok(None)
        }
        async fn get_block_timestamp(
            &self,
            _block_number: u64,
        ) -> Result<Option<u64>, degenbot_core::errors::ProviderError> {
            Ok(None)
        }
        async fn get_code(
            &self,
            _address: alloy::primitives::Address,
            _block_number: Option<u64>,
        ) -> Result<Bytes, degenbot_core::errors::ProviderError> {
            Ok(Bytes::new())
        }
        async fn get_balance(
            &self,
            _address: alloy::primitives::Address,
            _block_number: Option<u64>,
        ) -> Result<U256, degenbot_core::errors::ProviderError> {
            Ok(U256::ZERO)
        }
        async fn call(
            &self,
            _to: alloy::primitives::Address,
            _data: Bytes,
            _block_number: Option<u64>,
        ) -> Result<Bytes, degenbot_core::errors::ProviderError> {
            panic!("the crawl fixture's candidates are cheap skips: no construction read is made")
        }
    }

    struct SkipSource {
        data: PathGraphData,
    }

    impl DiscoverySource for SkipSource {
        fn edition(&self) -> Result<Edition, CrawlDiscoveryError> {
            Ok(Edition::default())
        }
        fn resolve_token_ids(
            &self,
            _addresses: &[alloy::primitives::Address],
        ) -> Result<HashMap<alloy::primitives::Address, u64>, CrawlDiscoveryError> {
            Ok(HashMap::from_iter([
                (CRAWL_WETH, CRAWL_TOKEN_A),
                (CRAWL_USDC, CRAWL_TOKEN_B),
            ]))
        }
        fn candidate_tokens(
            &self,
            _kinds: &[PoolKind],
        ) -> Result<HashSet<u64>, CrawlDiscoveryError> {
            Ok(HashSet::from_iter([CRAWL_TOKEN_A, CRAWL_TOKEN_B]))
        }
        fn graph_data(&self, _kinds: &[PoolKind]) -> Result<PathGraphData, CrawlDiscoveryError> {
            Ok(self.data.clone())
        }
    }

    fn crawl_graph() -> PathGraphData {
        let mut data = PathGraphData::default();
        for slot in 1..=2 {
            let graph_id = CRAWL_V4_OFFSET + slot;
            data.edges
                .push((CRAWL_TOKEN_A, CRAWL_TOKEN_B, graph_id, PoolKind::V4));
            data.v4_lookups
                .insert(graph_id, (CRAWL_V4_PM, "0xnot-a-pool-hash".to_owned()));
        }
        data
    }

    fn crawl_config_for_test() -> CrawlConfig {
        CrawlConfig {
            chain_id: 1,
            database_path: std::path::PathBuf::from("/nonexistent/stop-edge.sqlite"),
            route: ConstructionRoute::generic_only(),
            pool_kinds: vec![PoolKind::V4],
            pool_type_per_depth: None,
            start_tokens: vec![CRAWL_WETH],
            end_tokens: vec![CRAWL_WETH],
            min_depth: 2,
            max_depth: Some(2),
            discovery_batch_size: 1,
            intake_window: 1,
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
            profit_token: CRAWL_USDC,
            snapshot_block: None,
            state_block: Some(CRAWL_BLOCK),
        }
    }

    /// A seat whose unit stays in flight until the crawl's own teardown latches,
    /// recording the crawl's pose and join fact at that moment — the window a
    /// latched stop has to leave draining, observed from inside the crawl.
    struct DriftingSeat {
        controller: Arc<std::sync::OnceLock<Arc<CrawlController>>>,
        observed: Arc<Mutex<Option<(CrawlState, bool)>>>,
        inflight: Arc<Notify>,
    }

    impl CrawlUnitSeat for DriftingSeat {
        fn dispatch(&self, future: CrawlUnitFuture) -> Result<(), String> {
            let holder = Arc::clone(&self.controller);
            let observed = Arc::clone(&self.observed);
            tokio::spawn(async move {
                while holder
                    .get()
                    .is_some_and(|controller| !controller.state().is_tearing_down())
                {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                if let Some(controller) = holder.get() {
                    *observed.lock() = Some((controller.state(), controller.is_finished()));
                }
                future.await;
            });
            self.inflight.notify_one();
            Ok(())
        }
    }

    struct CrawlFixture {
        controller: Arc<CrawlController>,
        run: tokio::task::JoinHandle<()>,
        observed: Arc<Mutex<Option<(CrawlState, bool)>>>,
    }

    fn live_crawl_for_test(driver: &Arc<EngineDriver>) -> CrawlFixture {
        let observed = Arc::new(Mutex::new(None));
        let holder = Arc::new(std::sync::OnceLock::new());
        let inflight = Arc::new(Notify::new());
        let seat = Arc::new(DriftingSeat {
            controller: Arc::clone(&holder),
            observed: Arc::clone(&observed),
            inflight: Arc::clone(&inflight),
        });
        let io = Arc::new(ConstructionIo::new(Arc::new(NoDb), Arc::new(UnreachedRpc)));
        let config = crawl_config_for_test();
        let admission = Arc::new(RegistrationAdmission::new(
            Arc::clone(driver.bot()),
            io,
            Arc::clone(driver.stages()),
            Arc::clone(driver.verify()),
            Arc::new(CrawlLedger::new()),
            Arc::new(BuildFlights::new()),
            Arc::new(config.admission_settings()),
        ));
        let host = CrawlHost {
            admission,
            snapshot: None,
        };
        let controller = CrawlController::attach_with(
            host,
            config,
            Arc::new(SkipSource {
                data: crawl_graph(),
            }),
            seat,
        )
        .expect("the fixture configuration is valid");
        assert!(
            holder.set(Arc::clone(&controller)).is_ok(),
            "the seat's controller holder is set once"
        );
        let runner = Arc::clone(&controller);
        let run = runtime().spawn(async move {
            let _ = runner.run().await;
        });
        runtime().block_on(async {
            tokio::time::timeout(Duration::from_secs(5), inflight.notified())
                .await
                .expect("the run submits a unit, so a close below always has a window to drain");
        });
        CrawlFixture {
            controller,
            run,
            observed,
        }
    }

    #[test]
    fn a_close_with_no_crawl_plans_an_empty_edge_and_tears_down_as_before() {
        let driver = driver_for_test();
        assert!(
            driver.crawl_handle().is_none(),
            "no crawl is installed until the boot wiring installs one"
        );
        let started = std::time::Instant::now();
        let report = runtime().block_on(driver.close());
        assert!(
            started.elapsed() < PUMP_STOP_GRACE,
            "an empty crawl edge owes no wait; took {:?}",
            started.elapsed()
        );
        assert_eq!(report.crawl, CrawlExit::None);
        assert_eq!(driver.current_phase(), PumpPhase::Stopped);
    }

    /// **The teardown ordering.** The crawl's stop is latched BEFORE the pump
    /// join, so a unit already in flight sees the crawl `Stopping` (draining, not
    /// truncated) while the join is still unresolved, and only the run's own
    /// resolution reaches `Stopped`.
    #[test]
    fn a_close_latches_the_crawl_before_the_pump_join() {
        let driver = Arc::new(driver_for_test());
        let fixture = live_crawl_for_test(&driver);
        assert_eq!(
            fixture.controller.state(),
            CrawlState::Running {
                last_complete: None
            },
            "the fixture's run is live and its window is not yet drained"
        );
        driver
            .attach_crawl_teardown_for_test(&fixture.controller, fixture.run)
            .expect("a live session admits a crawl");

        let started = std::time::Instant::now();
        let report = runtime().block_on(driver.close());
        let elapsed = started.elapsed();

        assert!(
            elapsed < PUMP_STOP_GRACE,
            "the crawl's run is joined inside the grace window, not aborted; took {elapsed:?}"
        );
        assert_eq!(report.from, PumpPhase::Detached);
        assert_eq!(report.crawl, CrawlExit::Resolved);
        assert_eq!(driver.current_phase(), PumpPhase::Stopped);
        assert!(
            driver.crawl_handle().is_none(),
            "the teardown took the pair"
        );
        assert_eq!(
            fixture.controller.state(),
            CrawlState::Stopped,
            "the stop latched `Stopping`, and only the run's resolution reached `Stopped`"
        );
        assert_eq!(fixture.controller.run_phase(), RunPhase::Finished);
        assert!(fixture.controller.receipts_ended());
        assert_eq!(
            *fixture.observed.lock(),
            Some((CrawlState::Stopping, false)),
            "the unit in flight saw the latched stop and the still-unresolved join, so no receipt was truncated"
        );
    }

    #[test]
    fn begin_crawl_refuses_a_bot_without_construction_io() {
        let driver = Arc::new(driver_for_test());
        let error = driver
            .begin_crawl(crawl_config_for_test())
            .err()
            .expect("a driver with no ConstructionIo cannot boot a crawl");
        assert!(
            matches!(
                error,
                CrawlRefusal::Config(CrawlConfigError::MissingConstructionIo)
            ),
            "got {error}"
        );
        assert!(
            driver.crawl_handle().is_none(),
            "a refused boot installs nothing"
        );
    }

    #[test]
    fn begin_crawl_boots_the_run_and_the_driver_holds_its_join() {
        let bot = Arc::new(Bot::new(1));
        bot.set_construction_io(ConstructionIo::new(Arc::new(NoDb), Arc::new(UnreachedRpc)));
        let stages = Arc::new(EngineStages::with_core(bot.state_arc(), bot.active_delta()));
        let driver = Arc::new(EngineDriver::from_stages(Arc::clone(&bot), stages));

        // A crawl over an EMPTY database: the sweep resolves its graph with no
        // boundary rows, so the run finishes on its own — the boot wiring is what
        // this test pins, not discovery.
        // Under `rust/target` (gitignored), so a leaked sidecar can never reach a
        // commit: the SQLite open writes `-wal`/`-shm` siblings.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../target")
            .join(format!(
                "degenbot-crawl-boot-{}-{:?}.sqlite",
                std::process::id(),
                std::thread::current().id()
            ));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        // Materialize the (empty) database first: the crawl opens it READ-ONLY,
        // and a filesystem that answers EACCES for a read-only open of a missing
        // file would make this test about the filesystem instead of the boot.
        drop(degenbot_db::DegenbotDb::open(&path).expect("an empty crawl database"));
        let mut config = crawl_config_for_test();
        config.database_path = path.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let controller = driver
            .begin_crawl_with(CrawlBoot::new(config).with_receipts(tx))
            .expect("the driver boots the crawl");

        let handle = driver.crawl_handle().expect("the boot installed the edge");
        runtime().block_on(async {
            tokio::time::timeout(Duration::from_secs(10), async {
                while !controller.is_finished() {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
            .await
            .expect("the run resolves");
        });
        assert!(
            handle.is_finished(),
            "the driver's join reports the run's end"
        );
        // The run left its OWN answer (a stream-owning consumer reads it instead
        // of the join): this crawl is over an empty database, so it resolves with
        // a refusal — from discovery (an absent boundary token) or from the
        // database read, depending on the filesystem's read-only-open behaviour.
        // The BOOT wiring is what this test pins, so the verdict is only required
        // to be recorded and not clean.
        let outcome = controller
            .run_outcome()
            .expect("the run records how it resolved");
        assert!(
            outcome.detail().is_some(),
            "a refusing run names its reason: got {outcome:?}"
        );
        assert!(
            !outcome.is_clean(),
            "a refused discovery is not a clean run"
        );
        assert!(
            runtime().block_on(async { rx.recv().await }).is_none(),
            "the resolution edge ends the receipt stream"
        );

        // A second boot is refused by the SESSION's table (one crawl per
        // session), before any configuration work can run.
        let error = driver
            .begin_crawl(crawl_config_for_test())
            .err()
            .expect("a driver holds one crawl");
        assert!(
            matches!(error, CrawlRefusal::Pose(_)),
            "a second attach must be the table's refusal, got {error}"
        );

        runtime().block_on(driver.close());
        assert!(
            driver.crawl_handle().is_none(),
            "the teardown took the pair"
        );
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    // ── the pump exit doors ───────────────────────────────────────────────

    #[derive(Clone, Default)]
    struct StopLogCapture {
        events: Arc<std::sync::Mutex<Vec<(tracing::Level, String)>>>,
    }

    impl StopLogCapture {
        fn messages(&self, level: tracing::Level) -> Vec<String> {
            self.events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .filter(|(recorded, _)| *recorded == level)
                .map(|(_, message)| message.clone())
                .collect()
        }

        fn describes(&self, needle: &str) -> bool {
            self.events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .any(|(_, message)| message.contains(needle))
        }
    }

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for StopLogCapture {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut visitor = degenbot_core::log_capture::MessageVisitor::default();
            event.record(&mut visitor);
            let message = visitor.into_message().unwrap_or_default();
            self.events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((*event.metadata().level(), message));
        }
    }

    /// A driver whose pump is resumed on a stream that NEVER yields, so the live
    /// loop parks in its select and only the 500 ms `timed_exit_tick` arm can end
    /// it — the exact shape the cooperative path serves.
    fn driver_with_parked_pump() -> EngineDriver {
        let driver = driver_for_test();
        let (_stream, claim) = driver.claim_consumer().expect("the hand-off").split();
        driver.seed_session_for_test(SessionSeed::Connected {
            pump: driver.local_pump_for_test(),
            first_block: 100,
            stream: futures_util::stream::pending::<WsEvent>().boxed(),
        });
        runtime()
            .block_on(driver.run(claim))
            .expect("run with a pending subscribe state");
        assert!(driver.current_phase().live_loop_armed());
        driver
    }

    #[test]
    fn close_exits_cooperatively_without_aborting_a_parked_pump() {
        use tracing_subscriber::layer::SubscriberExt;
        let capture = StopLogCapture::default();
        let subscriber = tracing_subscriber::registry().with(capture.clone());
        tracing::subscriber::with_default(subscriber, || {
            let driver = driver_with_parked_pump();
            std::thread::sleep(std::time::Duration::from_millis(50));

            let started = std::time::Instant::now();
            let report = runtime().block_on(driver.close());
            let elapsed = started.elapsed();

            assert!(
                elapsed >= std::time::Duration::from_millis(400),
                "the cooperative exit is reached via the 500ms tick, not instantly; took {elapsed:?}"
            );
            assert!(
                elapsed < std::time::Duration::from_millis(2500),
                "close must return inside the grace window; took {elapsed:?}"
            );
            assert_eq!(report.pump, PumpExit::Cooperative);
            assert!(!driver.current_phase().live_loop_armed());
            assert_eq!(driver.current_phase(), PumpPhase::Stopped);
            assert!(
                capture.describes("exited cooperatively"),
                "the cooperative exit must be logged; INFO lines were {:?}",
                capture.messages(tracing::Level::INFO)
            );
            assert!(
                !capture.describes("did not exit cooperatively"),
                "the abort escalation must NOT fire for a pump parked at its select"
            );
        });
    }

    #[test]
    fn close_escalates_to_abort_when_the_grace_expires() {
        let driver = driver_for_test();
        let handle = runtime().spawn(std::future::pending::<()>());
        // A task that never polls the shutdown flag — an unconditional park — is
        // the loop the grace cannot reach. The escalation is what joins it.
        driver.seed_session_for_test(SessionSeed::Running(Box::pin(async {})));
        drop(handle);
        let started = std::time::Instant::now();
        let report = runtime().block_on(driver.close());
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "the teardown must resolve promptly; took {:?}",
            started.elapsed()
        );
        assert_ne!(report.pump, PumpExit::NotArmed);
        assert_eq!(driver.current_phase(), PumpPhase::Stopped);
    }

    // ── the completion broadcast ─────────────────────────────────────────

    #[test]
    fn wait_pump_finished_resolves_after_the_armed_pump_ends() {
        let driver = driver_for_test();
        // The loop owns the completion sender; its return closes the channel,
        // which IS the terminal completion.
        driver.seed_session_for_test(SessionSeed::Running(Box::pin(async {})));
        runtime().block_on(driver.wait_pump_finished());
    }

    #[test]
    fn wait_pump_finished_resolves_once_when_the_armed_pump_panics() {
        let driver = driver_for_test();
        driver.seed_session_for_test(SessionSeed::Running(Box::pin(async {
            panic!("simulated pump panic");
        })));
        // Two awaits both resolve: a panic is one terminal completion that every
        // waiter observes — it must never leave a waiter hanging.
        runtime().block_on(async {
            driver.wait_pump_finished().await;
            driver.wait_pump_finished().await;
        });
    }

    #[test]
    fn wait_pump_finished_stays_pending_until_the_armed_pump_ends() {
        let driver = driver_for_test();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        driver.seed_session_for_test(SessionSeed::Running(Box::pin(async move {
            let _ = release_rx.await;
        })));
        runtime().block_on(async {
            assert!(
                tokio::time::timeout(
                    std::time::Duration::from_millis(50),
                    driver.wait_pump_finished(),
                )
                .await
                .is_err(),
                "the completion future must not fire before the pump ends"
            );
            release_tx.send(()).expect("release the pump");
            driver.wait_pump_finished().await;
        });
    }

    #[test]
    fn wait_pump_finished_resolves_for_a_late_waiter() {
        let driver = driver_for_test();
        // A pump that ended before any consumer existed: the sender is gone, so
        // the channel is closed, which IS terminal completion.
        drop(driver.session.take_completion());
        runtime().block_on(driver.wait_pump_finished());
    }

    #[test]
    fn wait_session_end_delivers_the_pump_finished_fact() {
        let driver = driver_for_test();
        driver.seed_session_for_test(SessionSeed::Running(Box::pin(async {})));
        assert_eq!(
            runtime().block_on(driver.wait_session_end()),
            crate::arb_engine::session_end::SessionEndCause::PumpFinished
        );
    }
}
