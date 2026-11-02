//! `EngineDriver` — the public Rust driver seam over the crate-private
//! engine (ADR-050, Gap G1 /).
//!
//! # Why this type exists
//!
//! Before this module the only external driver of the pump/solve lifecycle was
//! the `PyO3` `PyArbEngine` + `PumpState` pair in `degenbot-python`:
//! [`crate::arb_engine::ArbitrageEngine`] is `pub(crate)` machinery, and the
//! public [`crate::arb_engine::EngineStages`] seam is an observer/stage surface
//! (no `subscribe`/`resume`/`stop`). A pure-Rust `cargo add degenbot` consumer
//! could therefore not run the startup ritual. `EngineDriver` is the ONE driver
//! seam above the ONE engine seam: it composes the shared `Arc<Bot>`, the
//! public `Arc<EngineStages>`, the reorg coordinator, the shutdown flag, the
//! pump handle, the subscribe state, the verify provider, and the result/block
//! channel ends — the session state that previously lived on `PumpState`.
//!
//! This is **not** the "engine facade" GLOSSARY.md forbids (ADR-049 D1): the
//! engine type stays `pub(crate)`, every engine touch crosses `EngineStages`,
//! and the one-impl-block census gate is untouched. It is a layer above the
//! existing door, not a second door.
//!
//! # The startup ritual (mirrors `EngineRegistry.start()`)
//!
//! 1. `start(http, ws, state_view)` (or `subscribe`) reads the snapshot seed
//!    `S` from the shared `BotState`, opens the WS subscriptions, observes the
//!    first complete block `W`, and pokes the verify config. It **stops before
//!    `resume()`** so the consumer can attach its result receiver while no
//!    batches can flow.
//! 2. `resume()` gates on `PumpPhase::Subscribed`, **owns the
//!    `S+1..W` auto-backfill** (awaiting `BlockPump::backfill_with_drain`
//!    synchronously), then spawns the live pump loop and advances
//!    `Subscribed -> Resuming -> Running`. Consumers never call
//!    `backfill_from_snapshot`.
//! 3. `stop()` publishes the table's teardown edge (`Stop`, then
//!    `StopCompleted` once the teardown returns), sets the shutdown flag, waits
//!    up to `PUMP_STOP_GRACE` for the pump to exit **cooperatively** (its own
//!    500 ms tick polls the flag), and only escalates to `abort()` + join if the
//!    grace expires; it then clears the subscribe state and closes the delivery
//!    channels. It is any-phase + idempotent (the `BotRunner.shutdown`
//!    contract).
//!
//! # Open questions resolved in this implementation (ADR-050)
//!
//! - **Blocking wrappers.** The async `start`/`subscribe`/`resume` are the
//!   canonical entry points; the adapters own `runtime.block_on`. `stop` is
//!   sync (it parks on the join), matching the ADR.
//! - **`PumpState`.** It survives as a thin `PyBot`-side adapter holding
//!   `Arc<EngineDriver>` and delegating; the session fields collapsed into this
//!   driver. Full dissolution was deferred to avoid churning every `PyBot`
//!   call site in the same cutover.
//! - **`DriverError`.** Closed enum below. Phase violations carry a typed
//!   `PhaseError`; registration propagates the typed
//!   `PathRegistrationError`; the verify lifecycle propagates the core
//!   `RefusalKind` so the adapter keeps its byte-identical Python exception
//!   mapping.
//! - **Verify provider.** Built by the driver (`set_verify_rpc_url` + an
//!   internal async builder used by `start`) and stored on the driver.
//! - **Lifecycle inputs are typed.** ADR-050 D4 sketches `&str` inputs; the
//!   implementation takes `Address`/`V4PoolId` so string parsing stays in the
//!   `PyO3` adapter, preserving its synchronous `ValueError` raise (the
//!   Python-visible behavior is byte-identical).
//! - **Main loop.** No `run_until_stopped` convenience: the consumer owns its
//!   consume/dispatch loop (ADR-050 D9).

use crate::arb_engine::crawl::controller::{
    CrawlConfig, CrawlConfigError, CrawlController, CrawlHandle, CrawlHost, SnapshotArm,
};
use crate::arb_engine::crawl::receipt::CrawlReceipt;
use crate::arb_engine::lifecycle::PathRegistrationError;
use crate::arb_engine::pump_session::{self, PhaseAdvance, PumpEvent};
use crate::arb_engine::{
    BlockNotification, EngineRetune, EngineStages, InlineSimulator, PumpPhase, ResultBatch,
};
use crate::bot_core::block_pump::{BlockPump, SubscribeState};
use crate::bot_core::registration_ledger::{
    PreBuildIdentity, RefusalKind, RegistrationPhase, VerifyOrigin,
};
use crate::bot_core::reorg_coordinator::ReorgCoordinator;
use crate::bot_core::verification_retry::{retry_verification_call, RetryPolicy};
use crate::bot_core::verify_claims::PoolVerifications;
use crate::bot_core::{Bot, PumpControl, StageHandlers};
use crate::strategy_host::HostHub;
use alloy::primitives::Address;
use degenbot_core::{diag, op_error, op_info, op_warn};
use degenbot_eventhub::Hub;
use degenbot_executor::composers::PathInfo;
use degenbot_ingestion::IngestEvent as WsEvent;
use degenbot_pools::solve::{PoolHop, SolvePathResult};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_substrate::path_info::PathInfoBuildError;
use degenbot_substrate::session_registry::PoolIdentity;
use degenbot_substrate::state_lock::{LockSite, StateLock};
use degenbot_substrate::BotState;
use degenbot_substrate::PoolLookup;
use hashbrown::HashMap;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tracing::Instrument as _;

/// Hub registration name for the engine's result-batch source channel.
///
/// A named source channel ([`degenbot_eventhub::NamedSender`]) rather than a
/// [`degenbot_eventhub::HubEvent`] variant: `ResultBatch` is an engine-owned
/// downstream type the hub vocabulary deliberately does not name.
pub const RESULT_CHANNEL_NAME: &str = "engine_result_batch";

/// Hub registration name for the engine's block-clock source channel. Named
/// for the same reason as [`RESULT_CHANNEL_NAME`].
pub const BLOCK_CHANNEL_NAME: &str = "engine_block_notification";

/// How long [`EngineDriver::stop`] waits for the pump task to exit
/// **cooperatively** before escalating to `abort()`.
///
/// The pump arms a 500 ms `timed_exit_tick` whose select arm polls the shared
/// shutdown flag, so a halted session unwinds the loop normally within one
/// tick. Four ticks with margin is the window: long enough that a healthy
/// pump always returns through its own machinery, short enough that teardown
/// stays prompt. The escalation only exists for a pump parked somewhere that
/// tick cannot reach (the `run_loop.rs` limitation note: a GIL re-entry park
/// through `PySubscriberAdapter`, or engine-lock contention inside
/// `on_drain`/`apply_buffer_v3`).
pub const PUMP_STOP_GRACE: Duration = Duration::from_secs(2);

/// [`PUMP_STOP_GRACE`] in milliseconds, for the `stop()` log fields — the
/// `Duration::as_millis` widening kept non-truncating.
fn pump_stop_grace_ms() -> u64 {
    u64::try_from(PUMP_STOP_GRACE.as_millis()).unwrap_or(u64::MAX)
}

/// The engine's two named source-channel producers, minted together.
///
/// A hub owns one channel per name; the engine registers its `ResultBatch` +
/// `BlockNotification` channels once on the hub it is attaching to and keeps
/// the producer ends here until the driver installs them on the stage seam.
/// The consumer ends stay hub-held for the once-only `take_*_receiver`
/// hand-off, exactly as on [`EngineDriver::from_stages`].
pub struct EngineChannelHandles {
    result: tokio::sync::mpsc::UnboundedSender<ResultBatch>,
    block: tokio::sync::mpsc::UnboundedSender<BlockNotification>,
}

impl EngineChannelHandles {
    /// Register the engine's two named source channels on `hub` and return
    /// their producer ends.
    ///
    /// Taking `&mut Hub` is the point: a named channel is minted into the
    /// hub's map, so registration must complete before the hub is shared as an
    /// `Arc` and handed to a driver.
    #[must_use]
    pub fn register_on(hub: &mut Hub) -> Self {
        let result = hub
            .add_named_unbounded_source::<ResultBatch>(RESULT_CHANNEL_NAME)
            .into_inner();
        let block = hub
            .add_named_unbounded_source::<BlockNotification>(BLOCK_CHANNEL_NAME)
            .into_inner();
        Self { result, block }
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
    /// The rejected driver method (`"subscribe"`, `"resume"`, …).
    pub method: &'static str,
    /// The pump-protocol phase at call time.
    pub current: PumpPhase,
    /// The phase boundary the method required, or `None` when the method
    /// imposes none (`stop` admits every pose).
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

/// The closed driver error surface (ADR-050 D5).
///
/// A consumer matches on the variant; no string matching.
#[derive(Debug)]
pub enum DriverError {
    /// A lifecycle phase precondition failed.
    Phase(PhaseError),
    /// A pump-session precondition failed (a pump is already started or a
    /// subscribe state is already pending, or the session is terminal).
    SessionState(String),
    /// The `resume()` consumer-before-resume gate: no result receiver was
    /// taken, so the unbounded result channel would have no consumer.
    NoResultReceiver,
    /// The WS subscribe/handshake failed.
    Subscribe(String),
    /// The `S+1..W` resume backfill failed. The resume aborted with the
    /// snapshot→WS gap **UNAPPLIED** — the session landed `Abandoned` and no
    /// live loop was started, so the caller stops the session and starts a new
    /// one (a re-subscribe is refused: `Abandoned` is terminal) instead of
    /// running over stale pool state. The detail names the offending block
    /// range.
    Backfill(String),
    /// Typed path-registration refusal (propagated verbatim, ADR-050 D4).
    Registration(PathRegistrationError),
    /// The registration verify lifecycle failed — the core [`RefusalKind`]
    /// (mismatch/RPC/missing config) propagated verbatim so the adapter can
    /// keep its typed Python mapping.
    Verify(RefusalKind),
}

impl std::fmt::Display for DriverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Phase(e) => write!(f, "{e}"),
            Self::SessionState(m) | Self::Subscribe(m) | Self::Backfill(m) => f.write_str(m),
            Self::NoResultReceiver => f.write_str(
                "Cannot resume: the result receiver has not been taken. Call take_result_receiver() before resume() so the unbounded result channel has a consumer attached.",
            ),
            Self::Registration(e) => write!(f, "{e}"),
            Self::Verify(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for DriverError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Verify(e) => Some(e),
            _ => None,
        }
    }
}

/// One session move in flight, guarded against the future being dropped.
///
/// `subscribe` and `resume` publish their in-flight pose (`Connecting`,
/// `Resuming`) BEFORE their first await, so a reader always sees that WS work is
/// outstanding. The price of an in-flight pose is that a future dropped in that
/// window would park the session where no other entry point is legal — so the
/// guard carries the table's abandonment edge for that leg (`SubscribeAbandoned`,
/// `ResumeAbandoned`) and publishes it from `Drop`. The leg's own landing
/// (`Subscribed`, `ResumeSpawned`, or a typed failure event) defuses the guard;
/// that pairing is the documented cancel policy: **an in-flight pose is always
/// either landed or abandoned, never left standing**.
struct PhaseLeg<'a> {
    /// The projection the guard publishes into.
    driver: &'a EngineDriver,
    /// The rejected-method name a decline is reported under.
    method: &'static str,
    /// The boundary the leg's events require (see [`PhaseError::required`]).
    required: Option<PumpPhase>,
    /// The table's abandonment edge for this leg.
    abandonment: PumpEvent,
    /// Cleared by the landing; a still-armed guard publishes on drop.
    armed: bool,
}

impl<'a> PhaseLeg<'a> {
    /// Enter the leg: publish the in-flight pose and arm the abandonment guard.
    ///
    /// A refused move arms nothing, so a decline cannot steal a concurrent
    /// leg's in-flight pose.
    fn enter(
        driver: &'a EngineDriver,
        method: &'static str,
        required: Option<PumpPhase>,
        requested: PumpEvent,
        abandonment: PumpEvent,
    ) -> Result<Self, DriverError> {
        driver.advance(method, required, requested)?;
        Ok(Self {
            driver,
            method,
            required,
            abandonment,
            armed: true,
        })
    }

    /// Publish the table's landing event and defuse the guard. Consuming `self`
    /// makes a landed leg unusable and runs the (now unarmed) `Drop`.
    fn land(mut self, event: PumpEvent) -> Result<PhaseAdvance, DriverError> {
        let landed = self.driver.advance(self.method, self.required, event);
        self.armed = false;
        landed
    }
}

impl Drop for PhaseLeg<'_> {
    fn drop(&mut self) {
        if self.armed {
            // Dropped mid-flight (a cancelled future, or an early return): the
            // abandonment edge is the table's honest landing for a leg that
            // never committed. A decline means the session moved on its own
            // (teardown began), and there is nothing left to restore.
            let _ = self
                .driver
                .advance(self.method, self.required, self.abandonment);
        }
    }
}

/// A test-only injected failure of `subscribe`'s fallible step.
///
/// `BlockPump::subscribe` dials a live WS endpoint, so its failure returns have
/// no in-process double. Both shapes leave the same thing to prove: the session
/// lands back on `Detached`, from which a retry is legal.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SubscribeFault {
    /// The WS handshake failed (`BlockPump::subscribe` returned `Err`).
    Handshake,
    /// The session opened but handed back no WS stream.
    NoStream,
}

#[cfg(test)]
impl SubscribeFault {
    const fn message(self) -> &'static str {
        match self {
            Self::Handshake => "injected: the WS handshake failed",
            Self::NoStream => "BlockPump::subscribe returned no WS stream",
        }
    }
}

/// The crawl as the driver's teardown holds it.
///
/// The controller never spawns its own run (see `crawl::controller`, "Who
/// spawns the run"), so the `JoinHandle` belongs to whoever spawned it — the
/// boot wiring — and is handed over together with the narrowed
/// [`CrawlHandle`] a `StopPlan` carries. The pair stays beside the pump's own
/// handle so ONE [`EngineDriver::stop`] tears down both loops under one grace
/// window.
pub(crate) struct CrawlTeardown {
    /// The narrowed teardown edge: latch the stop, read the two facts.
    pub(crate) handle: CrawlHandle,
    /// The run future's handle — the join fact's only reporter.
    pub(crate) run: tokio::task::JoinHandle<()>,
}

impl CrawlTeardown {
    /// The narrowed edge a
    /// [`StopPlan`](crate::arb_engine::pump_session::StopPlan) carries — never
    /// the controller.
    fn edge(&self) -> CrawlHandle {
        self.handle.clone()
    }

    /// Join the crawl's run future under `grace`, escalating to `abort()`.
    ///
    /// The caller has already latched the crawl's stop (the plan's edge), so a
    /// healthy run stops submitting, drains its window, and resolves on its
    /// own; its resolution guard then lands the join fact and the terminal
    /// pose. The abort is the escalation for a run parked where the latch
    /// cannot reach it: it drops the run future, which still passes through the
    /// same guard.
    fn join(self, grace: Duration) {
        let mut run = self.run;
        // The timeout future is built INSIDE the async block: `Sleep` captures
        // the timer handle at construction, so it must not be constructed
        // outside the runtime context.
        let cooperative = degenbot_core::runtime::get_runtime()
            .block_on(async { tokio::time::timeout(grace, &mut run).await });
        match cooperative {
            Ok(Ok(())) => {
                op_info!(
                    domain = pump,
                    grace_ms = pump_stop_grace_ms(),
                    "EngineDriver: crawl run resolved within the grace window"
                );
            }
            Ok(Err(join_err)) => {
                op_error!(
                    domain = pump,
                    grace_ms = pump_stop_grace_ms(),
                    error = %join_err,
                    "EngineDriver: crawl run ended with a join error before the grace expired"
                );
            }
            Err(_) => {
                run.abort();
                let _ = degenbot_core::runtime::get_runtime().block_on(run);
                op_info!(
                    domain = pump,
                    grace_ms = pump_stop_grace_ms(),
                    "EngineDriver: crawl run did not resolve within grace — aborted (fallback)"
                );
            }
        }
    }
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
    /// (`None` = construction reads no tick maps). A boot with a fixed handle
    /// says [`Self::with_db`]; a boot whose handle can be withdrawn mid-run
    /// (the process closes its snapshot read transaction) supplies the owner's
    /// own arm and gets the no-DB arm after the close.
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

/// The pending pump session between `subscribe()` and `resume()`.
pub(crate) struct DriverSubscribeState {
    /// The subscribed pump (owns the WS transport + the live stream).
    pump: BlockPump,
    /// The first complete WS block `W`.
    first_block: u64,
    /// The live WS event stream, handed to the pump at resume.
    combined_stream: futures_util::stream::BoxStream<'static, WsEvent>,
}

/// The public Rust driver seam above the engine's one stage seam (ADR-050).
///
/// Composes the shared `Bot`, the public `EngineStages`, and the pump session
/// state. See the module docs for the ritual and the resolved open questions.
pub struct EngineDriver {
    /// The shared per-chain orchestrator (ADR-006 D4).
    bot: Arc<Bot>,
    /// The engine's ONE public seam (ADR-049). Every engine touch crosses it.
    stages: Arc<EngineStages>,
    /// The per-event reorg coordinator (ADR-006 D4).
    reorg_coordinator: Arc<ReorgCoordinator>,
    /// The cooperative shutdown flag shared with the pump.
    shutdown: Arc<AtomicBool>,
    /// The spawned live-loop handle (`None` until `resume`).
    pump_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The crawl's teardown pair (`None` until the boot wiring installs a
    /// crawl): the narrowed edge a `StopPlan` carries and the run future's
    /// handle, which only the spawning caller can hold.
    crawl: Mutex<Option<CrawlTeardown>>,
    /// The pump-completion broadcast sender. Created in `from_stages`; the
    /// sender is MOVED into the spawned pump task by `resume` (or dropped by
    /// `stop` when the pump never armed), so the channel closes — resolving
    /// every waiter — on a normal return, stream end, abort, or panic.
    pump_finished_tx: Mutex<Option<watch::Sender<bool>>>,
    /// The pump-completion receiver. Cloned per `wait_pump_finished` waiter;
    /// a clone created before OR after completion still resolves.
    pump_finished_rx: watch::Receiver<bool>,
    /// The subscribe state held between `subscribe()` and `resume()`.
    subscribe_state: Mutex<Option<DriverSubscribeState>>,
    /// The HTTP RPC URL used for verification.
    verify_rpc_url: Mutex<Option<String>>,
    /// The cached verification provider (one per driver/chain, ADR-022 D3).
    verify_provider: Mutex<Option<AlloyProvider>>,
    /// The optional V4 `StateView` contract address for verification.
    verify_state_view: Mutex<Option<Address>>,
    /// The session's per-pool verification owner (ADR-022): the at-most-once
    /// claim policy for live windows plus the durable verified-pool fact the
    /// core registration ledger holds, shared by the async entry points, the
    /// blocking seat-thread twins, and every `PyO3` caller of the same
    /// driver. A driver is the session, so a driver holds one owner.
    verifications: PoolVerifications<RefusalKind>,
    /// The engine host's process-lifetime event hub. The engine's
    /// `ResultBatch` + `BlockNotification` source channels are registered on
    /// it as named, typed `UnboundedFlagged` channels; the hub holds each
    /// channel's consumer end until the once-only `take_*_receiver` handoff.
    hub: Arc<Hub>,
    /// Test-only injected failure for the next `subscribe` (see
    /// [`SubscribeFault`]).
    #[cfg(test)]
    subscribe_fault: Mutex<Option<SubscribeFault>>,
    /// Test-only hold inside `resume`'s in-flight window, so a test can cancel
    /// a resume where a dropping consumer would cancel one.
    #[cfg(test)]
    resume_gate: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    /// The once-only result-receiver hand-off latch. `resume()` refuses until
    /// a consumer owns the unbounded result channel's receiver; otherwise the
    /// engine would pump batches into a channel with no reader.
    result_receiver_taken: AtomicBool,
}

/// Bind the session's path-identity owner to the engine this driver drives.
///
/// A driver's `Bot` IS the session, and this driver's `EngineStages` is the
/// session's only path registry, so composition is the one place both halves
/// are in hand. Binding them here is what makes the session's canonical path
/// identity reachable at all: a registry with no owner answers every path ask
/// with [`ObjectRefusal::NoPathOwner`](degenbot_substrate::session_registry::ObjectRefusal::NoPathOwner),
/// so the identity the session exposes would be one no consumer can name.
///
/// # A second install is reported, not merged
///
/// Two engines in one session are two path-id spaces, so a consumer asking
/// the session which id a route has could be answered by the first engine for
/// the second engine's route. The session therefore keeps the FIRST owner —
/// a complete, working identity space — and this site reports the fork at
/// ERROR rather than absorbing it silently or aborting the boot: a wiring
/// mistake should be impossible to miss, and killing the process over a second
/// seam would trade a diagnosable fork for an unbootable bot.
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

/// Classify a `DriverError` from a registration lifecycle into the core
/// verify taxonomy the retry dance reads.
///
/// Only the verify arm is representable; every other driver failure is
/// unreachable at a lifecycle call site and maps to the fatal
/// [`VerifyOrigin::Other`] refusal so the retry never re-attempts it.
fn verify_refusal_from_driver(err: DriverError) -> RefusalKind {
    match err {
        DriverError::Verify(kind) => kind,
        other => RefusalKind::verify_failed(
            RegistrationPhase::Verify,
            true,
            VerifyOrigin::Other,
            other.to_string(),
        ),
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
    /// wrapper path). The driver mints a private hub for the engine's two
    /// named source channels and composes; a host-minted hub uses
    /// [`Self::from_stages_with_hub`].
    #[must_use]
    pub fn from_stages(bot: Arc<Bot>, stages: Arc<EngineStages>) -> Self {
        let mut hub = Hub::new();
        let channels = EngineChannelHandles::register_on(&mut hub);
        Self::assemble(bot, stages, Arc::new(hub), channels)
    }

    /// Adapter adoption against a host-minted hub.
    ///
    /// `attached` is the pair `StrategyHost::mint` produces: the shared hub
    /// and the engine's two named source channels registered on it by the mint
    /// closure (for the engine, [`EngineChannelHandles::register_on`]). The
    /// driver installs the producers on the stages and keeps the shared hub
    /// for the once-only receiver handoff. A named channel is keyed by name
    /// per hub, so a hub backs at most one engine driver — a second
    /// `take_*_receiver` returns `None`. The delivery channels stay
    /// [`degenbot_eventhub::OverflowPolicy::UnboundedFlagged`] (lossless by
    /// request, flagged for audit), and the raw `tokio::sync::mpsc` pair is
    /// unchanged, so recv/order/close semantics stay byte-identical to
    /// [`Self::from_stages`].
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
        stages.set_result_channel(channels.result);
        // The stage surface's block-clock pipe is fresh here and this is its one
        // wiring site, so a decline is unreachable.
        let _ = stages.set_block_channel(channels.block);
        install_session_path_owner(&bot, &stages);
        let (pump_finished_tx, pump_finished_rx) = watch::channel(false);
        let reorg_coordinator = Arc::new(ReorgCoordinator::new(Arc::clone(&bot)));
        Self {
            bot,
            stages,
            reorg_coordinator,
            shutdown: Arc::new(AtomicBool::new(false)),
            pump_handle: Mutex::new(None),
            crawl: Mutex::new(None),
            pump_finished_tx: Mutex::new(Some(pump_finished_tx)),
            pump_finished_rx,
            subscribe_state: Mutex::new(None),
            verify_rpc_url: Mutex::new(None),
            verify_provider: Mutex::new(None),
            verify_state_view: Mutex::new(None),
            verifications: PoolVerifications::new(),
            hub,
            #[cfg(test)]
            subscribe_fault: Mutex::new(None),
            #[cfg(test)]
            resume_gate: Mutex::new(None),
            result_receiver_taken: AtomicBool::new(false),
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

    /// The current engine lifecycle phase (core-owned truth).
    #[must_use]
    pub fn current_phase(&self) -> PumpPhase {
        self.stages.current_phase()
    }

    /// Force the session's pose. Test-only: production reaches a pose by
    /// driving a transition, never by writing one.
    #[cfg(test)]
    pub(crate) fn set_pose_for_test(&self, phase: PumpPhase) {
        self.stages.publish_phase(phase);
    }

    /// The table's verdict on `event` in the current pose, without publishing.
    ///
    /// An entry point whose refusals must leave the session untouched (the
    /// consumer-before-resume gate, the session-state guards) asks here FIRST,
    /// so a request from the wrong pose is the phase error it is rather than a
    /// secondary complaint. The publishing path re-runs the same table under the
    /// projection's lock ([`Self::advance`]), which is the authoritative check.
    ///
    /// # Errors
    ///
    /// [`DriverError::Phase`] carrying the decline.
    fn check(
        &self,
        method: &'static str,
        required: Option<PumpPhase>,
        event: PumpEvent,
    ) -> Result<PumpPhase, DriverError> {
        let phase = self.stages.current_phase();
        pump_session::transition(phase, event).map_err(|decline| {
            DriverError::Phase(PhaseError {
                method,
                current: phase,
                required,
                detail: decline.to_string(),
            })
        })
    }

    /// Publish one table transition, returning what the table did.
    ///
    /// The read, the legality decision, and the write happen inside the
    /// projection's lock ([`EngineStages::advance_phase`]), so a caller can
    /// never act on a predecessor the cell has already left.
    ///
    /// # Errors
    ///
    /// The table's
    /// [`PumpDecline`](crate::arb_engine::pump_session::PumpDecline), wrapped as
    /// a [`DriverError::Phase`] whose `detail` is the decline's own message.
    fn advance(
        &self,
        method: &'static str,
        required: Option<PumpPhase>,
        event: PumpEvent,
    ) -> Result<PhaseAdvance, DriverError> {
        self.stages.advance_phase(event).map_err(|decline| {
            DriverError::Phase(PhaseError {
                method,
                current: self.stages.current_phase(),
                required,
                detail: decline.to_string(),
            })
        })
    }

    /// `true` once the session machine is teardown-bound: `Stopping` from
    /// `stop()`'s first statement, `Stopped` when the teardown completes.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.current_phase().is_terminal()
    }

    /// `true` when a live pump task handle is armed.
    #[must_use]
    pub fn pump_handle_armed(&self) -> bool {
        self.pump_handle.lock().is_some()
    }

    /// Await the spawned pump task's completion — cooperative exit, stream
    /// end, abort, or panic.
    ///
    /// The completion is a `watch` broadcast whose terminal state is
    /// retained, so a waiter created before OR after the pump ends resolves
    /// promptly instead of hanging. The pump task owns the channel's only
    /// sender; dropping it (a normal return, or the unwind of a panic) closes
    /// the channel, which `changed()` reports as end-of-stream.
    ///
    /// Multiple waiters are supported; each clones the shared receiver.
    pub(crate) async fn wait_pump_finished(&self) {
        let mut rx = self.pump_finished_rx.clone();
        loop {
            if *rx.borrow_and_update() {
                return;
            }
            // `Err` = every sender dropped: the pump task ended (normal
            // return or panic). That IS completion.
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    /// The engine host's process-lifetime hub.
    ///
    /// The engine's `ResultBatch` and `BlockNotification` source channels are
    /// registered here as named, typed `UnboundedFlagged` channels.
    #[must_use]
    pub fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }

    /// Take the result-batch receiver — once only, **before** `resume()`.
    ///
    /// The receiver is the hub-held consumer end of the engine's result
    /// source channel — the counting hand-off so hub-side growth
    /// (`Hub::named_pending`) reflects live depth instead of reading zero.
    ///
    /// A successful take latches the hand-off that `resume()` requires; a
    /// resume with no consumer attached is refused so the unbounded channel
    /// never backs up behind a pump no one drains.
    #[must_use]
    pub fn take_result_receiver(&self) -> Option<degenbot_eventhub::NamedReceiver<ResultBatch>> {
        match self
            .hub
            .take_named_counting_receiver::<ResultBatch>(RESULT_CHANNEL_NAME)
        {
            Ok(Some(rx)) => {
                self.result_receiver_taken.store(true, Ordering::SeqCst);
                Some(rx)
            }
            Ok(None) => None,
            Err(e) => {
                op_error!(
                    domain = pump,
                    %e,
                    "EngineDriver: result source channel missing from the hub"
                );
                None
            }
        }
    }

    /// Take the block-clock receiver — once only.
    ///
    /// The receiver is the hub-held consumer end of the engine's block source
    /// channel — counting hand-off (see `take_result_receiver`).
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

    /// Run the pre-pump startup ritual: `subscribe` → verify-config.
    ///
    /// Stops **before** `resume()` so the consumer attaches its result
    /// receiver first (the `BotRunner.run` ordering invariant).
    ///
    /// # Errors
    ///
    /// Propagates the typed `DriverError` from `subscribe` (phase gate,
    /// session state, or WS transport).
    pub async fn start(
        &self,
        node_http: &str,
        node_ws: &str,
        verify_state_view: Option<&str>,
    ) -> Result<u64, DriverError> {
        let w = self.subscribe(node_ws).await?;
        self.build_verify_provider(node_http).await;
        if let Some(view) = verify_state_view {
            self.set_verify_state_view(view);
        }
        Ok(w)
    }

    /// Open the WS subscriptions and observe the first complete block `W`.
    ///
    /// # Errors
    ///
    /// - [`DriverError::Phase`] when the phase is not `Detached` — including the
    ///   terminal `Abandoned`, whose one exit is `stop`.
    /// - [`DriverError::SessionState`] when the pump is already started or a
    ///   subscribe state is already pending, or the driver is stopped.
    /// - [`DriverError::Subscribe`] when the WS handshake fails.
    pub async fn subscribe(&self, node_ws: &str) -> Result<u64, DriverError> {
        if self.is_stopped() {
            return Err(DriverError::SessionState(
                "Cannot subscribe: driver has been stopped.".to_string(),
            ));
        }
        // Legality first, then the session-state guards, THEN the in-flight
        // publish: a refused request must leave the pose where it found it, and
        // `Connecting` is legal for neither `subscribe` nor `resume` —
        // publishing it for a request that never dials would wedge the session.
        self.check(
            "subscribe",
            Some(PumpPhase::Detached),
            PumpEvent::SubscribeRequested,
        )?;
        if self.pump_handle.lock().is_some() {
            return Err(DriverError::SessionState(
                "Cannot subscribe: pump is already started. Call stop() first.".to_string(),
            ));
        }
        if self.subscribe_state.lock().is_some() {
            return Err(DriverError::SessionState(
                "Cannot subscribe: already subscribed. Call resume() first.".to_string(),
            ));
        }
        let leg = PhaseLeg::enter(
            self,
            "subscribe",
            Some(PumpPhase::Detached),
            PumpEvent::SubscribeRequested,
            PumpEvent::SubscribeAbandoned,
        )?;
        let session = match self.open_ws_session(node_ws).await {
            Ok(session) => session,
            Err(e) => {
                // The table's failure landing: `Connecting --SubscribeFailed-->
                // Detached`, so the next `subscribe()` is legal again — the
                // recovery a WS failure had before the pose was published ahead
                // of the handshake.
                if let Err(landing) = leg.land(PumpEvent::SubscribeFailed) {
                    op_warn!(
                        domain = pump,
                        error = %landing,
                        "EngineDriver: a failed subscribe could not restore the session to detached"
                    );
                }
                return Err(e);
            }
        };
        let first_block = session.first_block;
        *self.subscribe_state.lock() = Some(session);
        leg.land(PumpEvent::Subscribed)?;
        Ok(first_block)
    }

    /// Open the WS session: the ONE fallible step of [`Self::subscribe`].
    ///
    /// Both failure shapes (a failed handshake, and a session that handed back
    /// no WS stream) are `DriverError::Subscribe`; the caller's job is the
    /// table landing, not the distinction.
    async fn open_ws_session(&self, node_ws: &str) -> Result<DriverSubscribeState, DriverError> {
        #[cfg(test)]
        if let Some(fault) = self.subscribe_fault.lock().take() {
            return Err(DriverError::Subscribe(fault.message().to_string()));
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
        .map_err(DriverError::Subscribe)?;
        let SubscribeState {
            first_block,
            combined_stream,
            ..
        } = state;
        let Some(combined_stream) = combined_stream else {
            return Err(DriverError::Subscribe(
                "BlockPump::subscribe returned no WS stream".to_string(),
            ));
        };
        Ok(DriverSubscribeState {
            pump,
            first_block,
            combined_stream,
        })
    }

    /// Resume the pump.
    ///
    /// The driver **owns** the `S+1..W` auto-backfill: it awaits
    /// `BlockPump::backfill_with_drain(W, stream)` synchronously, then spawns
    /// the live loop on the shared runtime. Consumers never call
    /// `backfill_from_snapshot`.
    ///
    /// # Errors
    ///
    /// - [`DriverError::Phase`] unless the table admits `resume` (it requires
    ///   `Subscribed`).
    /// - [`DriverError::SessionState`] when no subscribe state is pending, or
    ///   when the session is terminal.
    /// - [`DriverError::NoResultReceiver`] when the result receiver has not
    ///   been taken.
    /// - [`DriverError::Backfill`] when the `S+1..W` backfill fails: the gap is
    ///   UNAPPLIED, so the resume aborts (landing `Abandoned`) rather than
    ///   starting a live loop over stale state.
    ///
    /// # Cancel safety
    ///
    /// **Committed from the spawn on**: `resume` does not own the pump it
    /// starts. Once the live loop is handed to `tokio::spawn`, the spawned task
    /// OWNS the pump (`pump` is moved into the async block); only its
    /// `JoinHandle` is stored back into `self.pump_handle`. A dropped `resume`
    /// future therefore drops the handle's future, not the pump task — the pump
    /// keeps running to completion, and its completion sender (moved into the
    /// same task) still drops on whatever terminal path, so every
    /// `wait_pump_finished` waiter resolves exactly as if `resume` had returned
    /// normally.
    ///
    /// **Cancel-safe before it, by landing `Abandoned`**: the in-flight window is
    /// the subscribe-state handover and the awaited `backfill_with_drain`.
    /// `resume` publishes the table's `Resuming` at the top of that window and
    /// arms a [`PhaseLeg`], whose `Drop` publishes the table's `ResumeAbandoned`
    /// edge — so a future dropped mid-window lands `Abandoned` (the pump and its
    /// WS session died with it) instead of standing in `Resuming` with nothing
    /// left to resume. `Abandoned` is terminal: `stop()` is its one exit, and a
    /// re-subscribe is refused by name
    /// ([`pump_session::PumpDecline::SubscribeAfterAbandonment`]) — recovery
    /// from a cancelled resume is a new session, because the backfill buffer
    /// stages events without dedup and a retry would double-apply them at
    /// registration. A cancel after the `ResumeSpawned` landing is NOT "not
    /// resumed": the live loop is armed and its handle stored.
    ///
    /// **A backfill failure is an abort, not a degraded start**: the `S+1..W`
    /// gap would be unapplied, so the `Err` is returned typed
    /// ([`DriverError::Backfill`]) after the same `ResumeAbandoned` landing —
    /// the live loop is never spawned.
    pub async fn resume(&self) -> Result<(), DriverError> {
        if self.is_stopped() {
            return Err(DriverError::SessionState(
                "Cannot resume: driver has been stopped.".to_string(),
            ));
        }
        self.check(
            "resume",
            Some(PumpPhase::Subscribed),
            PumpEvent::ResumeRequested,
        )?;
        // Consumer-before-resume gate. The result channel is unbounded, so a
        // pump with no receiver attached accumulates batches no one drains.
        // This check runs before the in-flight pose AND before the subscribe
        // state is consumed, so a refused resume leaves the session exactly as
        // it found it for a corrected retry.
        if !self.result_receiver_taken.load(Ordering::SeqCst) {
            return Err(DriverError::NoResultReceiver);
        }
        // The in-flight pose, published BEFORE the state handover and before the
        // backfill await: from here to the live-loop landing, a dropped future is
        // what `ResumeAbandoned` exists for.
        let leg = PhaseLeg::enter(
            self,
            "resume",
            Some(PumpPhase::Subscribed),
            PumpEvent::ResumeRequested,
            PumpEvent::ResumeAbandoned,
        )?;
        let state = self.subscribe_state.lock().take().ok_or_else(|| {
            DriverError::SessionState(
                "Cannot resume: subscribe() has not been called. Call subscribe() first."
                    .to_string(),
            )
        })?;
        let DriverSubscribeState {
            mut pump,
            first_block,
            combined_stream,
        } = state;
        #[cfg(test)]
        let resume_gate = self.resume_gate.lock().take();
        #[cfg(test)]
        if let Some(gate) = resume_gate {
            // Hold the future inside the in-flight window so a test can cancel
            // it exactly where a dropping consumer cancels one.
            let _ = gate.await;
        }
        // the backfill is SYNCHRONOUS with respect to `resume` so the
        // consumer's registration draining the per-pool backfill buffer cannot
        // race it. `backfill_with_drain` also re-injects live events
        // drained during the backfill ahead of the live tail.
        let (backfill_res, combined) = pump.backfill_with_drain(first_block, combined_stream).await;
        let combined = match backfill_res {
            Ok(_) => combined,
            // The gap `S+1..W` is UNAPPLIED, so a live loop from `W` would run
            // over stale pool state: abort the resume instead. The pump and its
            // WS session drop with this early return, exactly as they do on a
            // cancelled resume, so the table's honest landing is the same
            // `Resuming -> Abandoned` edge. The failure itself is the caller's,
            // typed — the pose carries no payload.
            Err(backfill) => {
                op_error!(
                    domain = pump,
                    first_block,
                    error = %backfill,
                    "EngineDriver: auto-backfill failed — resume aborted, live loop NOT started (gap not closed)"
                );
                leg.land(PumpEvent::ResumeAbandoned)?;
                return Err(DriverError::Backfill(backfill));
            }
        };
        // The landing precedes the spawn by one synchronous statement (no await
        // between the two, so cancellation cannot observe the gap). It must:
        // a live loop that ends immediately — an already-ended stream, a panic
        // on its first poll — reports `PumpExited`, and the table has no
        // `Resuming -> Ended` edge, so landing `Running` first is what lets that
        // exit unwind the projection instead of being refused.
        leg.land(PumpEvent::ResumeSpawned)?;
        // Arm the completion broadcast: the sender is owned by the spawned
        // task, so wherever the pump ends — cooperative return, stream end,
        // abort, or panic — the sender drops and every `wait_pump_finished`
        // waiter resolves. No explicit send is needed; channel close IS the
        // terminal event (which is also what makes the panic path work).
        let completion_tx = self.pump_finished_tx.lock().take();
        let handle = tokio::spawn(async move {
            let _completion_tx = completion_tx;
            pump.run_with_stream(combined, first_block).await;
        });
        *self.pump_handle.lock() = Some(handle);
        Ok(())
    }

    /// Stop the pump and land the session machine terminal (ADR-050 D6).
    ///
    /// Any-phase + idempotent: the table's `Stop` declines from no pose and
    /// `Stopped` is absorbing, so a repeated or concurrent stop is a no-op. A
    /// session holding something to tear down lands `Stopping` and owes one
    /// `StopCompleted` (published when the teardown returns); one with nothing
    /// to tear down lands `Stopped` directly. Sets the shutdown flag, waits up to
    /// [`PUMP_STOP_GRACE`] for the pump to exit **cooperatively** (it polls
    /// the flag on its own 500 ms tick arm), and only then escalates to
    /// `abort()` + join if the grace expires. Either way the WS subscription
    /// futures have dropped before return. Clears the subscribe state and
    /// closes the delivery channels so a pending receiver observes
    /// end-of-stream exactly once.
    ///
    /// Configure a crawl over this driver's `Bot` and boot it.
    ///
    /// The DRIVER boots the crawl: it composes the host from the driver's own
    /// `Bot` (whose `ConstructionIo` the shell attached at boot time), attaches
    /// the receipt stream's sink, spawns [`CrawlController::run`], and keeps the
    /// run future's `JoinHandle` beside the pump's so ONE [`EngineDriver::stop`]
    /// tears both loops down under one grace window. The caller keeps the
    /// controller — the operator surface and the summary render — and nothing
    /// else, which is what makes the caller-holds-the-controller /
    /// driver-holds-the-join contract real.
    ///
    /// The receipt stream is attached BEFORE the run arms, so not one receipt
    /// can race past an unattached channel: a consumer that wants the stream
    /// passes its sink here rather than attaching after [`Self::begin_crawl`]
    /// returns.
    ///
    /// # Errors
    ///
    /// [`CrawlConfigError`] — an invalid configuration, a `Bot` with no
    /// construction I/O attached, a crawl already installed on this driver, or a
    /// receipt sink the channel refused.
    pub fn begin_crawl(
        self: &Arc<Self>,
        config: CrawlConfig,
    ) -> Result<Arc<CrawlController>, CrawlConfigError> {
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
    ) -> Result<Arc<CrawlController>, CrawlConfigError> {
        // One crawl per driver. The slot is taken for the whole boot so a
        // concurrent second boot cannot race the first into the same slot, and
        // `stop` waits on the same lock rather than tearing a half-booted crawl.
        let mut slot = self.crawl.lock();
        if slot.is_some() {
            return Err(CrawlConfigError::CrawlAlreadyBegun);
        }
        let CrawlBoot {
            config,
            receipts,
            snapshot,
        } = boot;
        let io = self
            .bot
            .construction_io_arc()
            .ok_or(CrawlConfigError::MissingConstructionIo)?;
        let host = CrawlHost::from_driver(self, io, snapshot);
        let controller = CrawlController::attach(host, config)?;
        if let Some(tx) = receipts {
            controller.attach_receipts(tx)?;
        }
        let runner = Arc::clone(&controller);
        let run = degenbot_core::runtime::get_runtime().spawn(async move {
            // The run's own answer is recorded on the controller (readable after
            // end-of-stream); the join handle reports only a panic.
            let _ = runner.run().await;
        });
        *slot = Some(CrawlTeardown {
            handle: CrawlHandle::new(Arc::clone(&controller)),
            run,
        });
        Ok(controller)
    }

    /// The crawl's teardown pair, for a caller that must read the run's join
    /// fact without stopping it.
    #[must_use]
    pub fn crawl_handle(&self) -> Option<CrawlHandle> {
        self.crawl.lock().as_ref().map(CrawlTeardown::edge)
    }

    /// The crawl is torn down with the live loop. The teardown plan is produced
    /// SYNCHRONOUSLY ([`pump_session::on_stop`]) from the pose the table landed
    /// the session from and the crawl edge this driver holds — nothing
    /// awaitable, and no lock held across an await — and its edge is latched
    /// before the pump join, so both loops unwind at once. The crawl's run
    /// future is then joined under the SAME [`PUMP_STOP_GRACE`] window (the
    /// controller never spawns its own run, so the driver holds that
    /// `JoinHandle`), escalating to `abort()` only if the latch cannot reach
    /// it.
    ///
    /// # Errors
    ///
    /// Currently never returns `Err`; the typed result keeps the surface
    /// symmetric with `subscribe`/`resume`.
    pub fn stop(&self) -> Result<(), DriverError> {
        // The table's teardown entry first: it is the machine's own answer to
        // "has teardown begun?", which every guard (and `is_stopped`) reads.
        let teardown = self.advance("stop", None, PumpEvent::Stop)?;
        if teardown.from.is_terminal() {
            // A repeated or concurrent stop: the caller that began the teardown
            // owns it, and this one has nothing left to join.
            return Ok(());
        }
        // The crawl's teardown pair, taken once: the plan carries the narrowed
        // edge (never the controller) and the run future's `JoinHandle` stays
        // here beside the pump's own.
        let crawl = self.crawl.lock().take();
        // The plan, produced SYNCHRONOUSLY from the pose the table landed the
        // session FROM — the same answer the projection just published, with no
        // await and no lock held across one.
        let (next, plan) =
            pump_session::on_stop(teardown.from, crawl.as_ref().map(CrawlTeardown::edge));
        debug_assert_eq!(
            next, teardown.to,
            "the plan and the projection are one table answer"
        );
        self.shutdown.store(true, Ordering::Relaxed);
        // Latch the crawl's stop BEFORE the pump join, so both loops unwind at
        // once: the crawl stops submitting and drains its window while the pump
        // walks its own tick.
        plan.stop_crawl();
        // Drop any un-armed sender (the pump never resumed): without this a
        // pre-finish waiter would hang after a stop-before-resume.
        drop(self.pump_finished_tx.lock().take());
        let handle = self.pump_handle.lock().take();
        if let Some(mut handle) = handle {
            // Cooperative first: the pump's own 500 ms `timed_exit_tick`
            // select arm polls the shutdown flag, so an ordinary stop lets
            // the loop unwind through its span guards and return normally.
            // Abort is the ESCALATION, not the primary mechanism.
            // The timeout future is built INSIDE the async block: `Sleep`
            // captures the timer handle at construction, so it must not be
            // constructed outside the runtime context.
            let cooperative = degenbot_core::runtime::get_runtime()
                .block_on(async { tokio::time::timeout(PUMP_STOP_GRACE, &mut handle).await });
            match cooperative {
                Ok(Ok(())) => {
                    op_info!(
                        domain = pump,
                        grace_ms = pump_stop_grace_ms(),
                        "EngineDriver: BlockPump task exited cooperatively on the shutdown flag"
                    );
                }
                Ok(Err(join_err)) => {
                    // The task ended on its own but with a join error (a
                    // panic inside the pump). It is already gone, so there is
                    // nothing to abort; name it so the abnormal end is not
                    // mistaken for the cooperative path.
                    op_error!(
                        domain = pump,
                        grace_ms = pump_stop_grace_ms(),
                        error = %join_err,
                        "EngineDriver: BlockPump task ended with a join error before the grace expired"
                    );
                }
                Err(_) => {
                    // The pump is parked somewhere its tick cannot reach (a
                    // GIL re-entry park via `PySubscriberAdapter`, or
                    // engine-lock contention inside `on_drain`), so the
                    // cooperative path cannot fire. Cancel it and drive the
                    // cancelled task to completion so its held resources
                    // drop before return. `block_on` on the shared runtime
                    // matches `subscribe`/`resume`'s sync discipline; the
                    // aborted task completes promptly. This should be rare.
                    handle.abort();
                    let _ = degenbot_core::runtime::get_runtime().block_on(handle);
                    op_info!(
                        domain = pump,
                        grace_ms = pump_stop_grace_ms(),
                        "EngineDriver: BlockPump did not exit cooperatively within grace — aborted (fallback)"
                    );
                }
            }
        } else {
            op_info!(
                domain = pump,
                "EngineDriver: BlockPump not running (no pump handle to stop)"
            );
        }
        // The crawl's run future, under the same grace window and in the same
        // shape: it was asked to stop above, so its window drains and the run
        // resolves on its own — abort is the escalation, not the mechanism.
        if let Some(crawl) = crawl {
            crawl.join(PUMP_STOP_GRACE);
        }
        // Final WalkMemo stats drain: the pump is down
        // and no further cycle can advance an epoch, so drain the LAST
        // epoch's counters here — no boundary will. Inert unless the memo
        // recorded activity (a stop-before-resume run emits nothing); the
        // observation never fails the stop.
        self.stages.drain_walk_memo_final();
        *self.subscribe_state.lock() = None;
        self.stages.close_delivery_channels();
        if next == PumpPhase::Stopping {
            self.advance("stop", None, PumpEvent::StopCompleted)?;
        }
        Ok(())
    }

    /// Set the HTTP RPC URL used for verification (builds the provider).
    ///
    /// # Panics
    ///
    /// Panics if called from within a Tokio runtime (it parks on the shared
    /// runtime via `block_on`). Use `start` from async contexts.
    pub fn set_verify_rpc_url(&self, rpc_url: &str) {
        degenbot_core::runtime::get_runtime().block_on(self.build_verify_provider(rpc_url));
    }

    /// Build + store the verification provider (the async half of
    /// [`Self::set_verify_rpc_url`], shared with `start` so an async caller
    /// never nests `block_on`).
    async fn build_verify_provider(&self, rpc_url: &str) {
        match AlloyProvider::new(rpc_url, degenbot_rpc::provider::DEFAULT_MAX_RETRIES).await {
            Ok(provider) => {
                *self.verify_provider.lock() = Some(provider);
            }
            Err(e) => {
                #[expect(clippy::print_stderr)] // startup diagnostic
                {
                    eprintln!("Failed to create verification provider: {e}");
                }
            }
        }
        *self.verify_rpc_url.lock() = Some(rpc_url.to_string());
    }

    /// Set the `StateView` contract address for V4 verification.
    pub fn set_verify_state_view(&self, state_view_address: &str) {
        let addr: Address = state_view_address.parse().unwrap_or(Address::ZERO);
        *self.verify_state_view.lock() = Some(addr);
    }

    /// The registered pool id for this pool identity, derived from the shared
    /// `BotState`.
    ///
    /// The read direction a driver needs when it holds a pool's IDENTITY — a
    /// family tag plus an address, or a V4 `PoolManager` plus pool id —
    /// rather than an id. The answer comes from the registration tables the
    /// state owner already keeps, so a driver never holds a second pool-id map
    /// that can disagree with it.
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

    /// How many verify-claim windows are live on this driver — the
    /// test/diagnostic witness for the at-most-once claim.
    #[must_use]
    pub fn live_verify_claim_count(&self) -> usize {
        self.verifications.live_claim_count()
    }

    /// How many pools have a completed verify-lifecycle fact on this driver —
    /// the durable counterpart to [`Self::live_verify_claim_count`], and the
    /// witness that a repeated registration is a no-op rather than a re-run.
    #[must_use]
    pub fn verified_pool_count(&self) -> usize {
        self.verifications.verified_pool_count()
    }

    /// Whether `key` names a pool whose verify lifecycle already COMPLETED on
    /// this driver.
    ///
    /// The driver exposes this for tests and diagnostics; registration paths
    /// do not consult it directly — they enter the lifecycle, which applies
    /// the fact itself.
    #[must_use]
    pub fn is_pool_verified(&self, key: &PreBuildIdentity) -> bool {
        self.verifications.is_verified(key)
    }

    /// Run a pool's core-owned registration verify lifecycle, AT MOST ONCE per
    /// live claim window — and never again once it has COMPLETED.
    ///
    /// The key is a value ([`degenbot_substrate::PoolLookup`]): a V3 pool by
    /// contract address, a V4 pool by `(PoolManager, pool_id)`. One body serves
    /// both designs, so the claim key, the unregistered no-op guard, the span
    /// fields and the diagnostic wording cannot drift apart per family.
    ///
    /// Concurrent callers — the async driver, a blocking seat-thread twin, or
    /// any `PyO3` caller of this same driver — share one run and each receive
    /// its outcome; a failed window is released so a later caller retries. A
    /// completed lifecycle records a durable verified-pool fact, so a
    /// subsequent registration of the same identity is a no-op rather than a
    /// re-run (the behavior the retired Python key cache provided).
    ///
    /// # Errors
    ///
    /// Propagates [`DriverError::Verify`] from the core lifecycle
    /// (`Mismatch` = fatal; `Rpc` = retryable; missing provider = fail-fast).
    pub async fn run_registration_lifecycle(
        &self,
        key: PoolLookup,
        snapshot_block: Option<u64>,
    ) -> Result<(), DriverError> {
        let core = self.stages.core();
        // The verify fact is keyed by the SAME identity the registration ledger
        // memos on, so the driver's claim path and the pipeline's ledger share
        // one fact instead of three private string formats.
        let claim_key = match key {
            PoolLookup::Address(address) => PreBuildIdentity::Address(address),
            PoolLookup::V4 { pool_id, .. } => PreBuildIdentity::V4(pool_id),
        };
        if self.verifications.is_verified(&claim_key) {
            return Ok(());
        }
        // An unregistered pool is a no-op `Ok` in the lifecycle; that must not
        // become a durable fact, or a later registration of this pool would
        // skip the verify it still needs.
        let registered = {
            let state = core.read_at(LockSite::Registration);
            key.coverage(&state).is_some()
        };
        if !registered {
            return Ok(());
        }
        let provider = self.verify_provider.lock().clone();
        self.verifications
            .run_exclusive(&claim_key, || async move {
                let span = tracing::info_span!(
                    "degenbot.pool.verify_lifecycle",
                    pool.version = key.family().tag(),
                    pool.address = %key,
                );
                let result = crate::bot_core::run_registration_lifecycle(
                    &core,
                    provider.as_ref(),
                    key,
                    snapshot_block,
                )
                .instrument(span)
                .await;

                if result.is_ok() {
                    diag!(domain = pump, version = %key.family().tag(), pool = %key, "registration verify-lifecycle complete");
                } else {
                    op_warn!(domain = pump, version = %key.family().tag(), pool = %key, "registration verify-lifecycle FAILED");
                }
                result
            })
            .await
            .map_err(DriverError::Verify)
    }

    /// Blocking registration lifecycle (the seat-thread twin).
    ///
    /// # Errors
    ///
    /// Propagates [`DriverError::Verify`].
    ///
    /// # Panics
    ///
    /// Panics if called from within a Tokio runtime.
    pub fn run_registration_lifecycle_sync(
        &self,
        key: degenbot_substrate::PoolLookup,
        snapshot_block: Option<u64>,
    ) -> Result<(), DriverError> {
        degenbot_core::runtime::get_runtime()
            .block_on(self.run_registration_lifecycle(key, snapshot_block))
    }

    /// Run a pool's registration verify lifecycle under the bounded retry
    /// dance.
    ///
    /// A transient verify failure (`fatal == false`) releases the claim (the
    /// plain lifecycle owns that), so the next attempt re-runs the whole
    /// lifecycle; a fatal mismatch is never retried. Returns the classified
    /// failure once the policy is exhausted.
    ///
    /// # Errors
    ///
    /// Returns the last transient [`RefusalKind`] after exhausting `policy`,
    /// or the first fatal one.
    pub async fn run_registration_lifecycle_with_retry(
        &self,
        key: degenbot_substrate::PoolLookup,
        snapshot_block: Option<u64>,
        policy: &RetryPolicy,
    ) -> Result<(), RefusalKind> {
        retry_verification_call(policy, |_attempt| async move {
            self.run_registration_lifecycle(key, snapshot_block)
                .await
                .map_err(verify_refusal_from_driver)
        })
        .await
    }

    /// Blocking with-retry twin (the seat-thread shape).
    ///
    /// # Errors
    ///
    /// As the async twin.
    ///
    /// # Panics
    ///
    /// Panics if called from within a Tokio runtime.
    pub fn run_registration_lifecycle_with_retry_sync(
        &self,
        key: degenbot_substrate::PoolLookup,
        snapshot_block: Option<u64>,
        policy: &RetryPolicy,
    ) -> Result<(), RefusalKind> {
        degenbot_core::runtime::get_runtime().block_on(self.run_registration_lifecycle_with_retry(
            key,
            snapshot_block,
            policy,
        ))
    }

    /// Register a mixed path (DELEGATION, ADR-050 D4).
    ///
    /// # Errors
    ///
    /// Propagates the typed [`PathRegistrationError`].
    pub fn register_path(&self, hops: Vec<PoolHop>) -> Result<(u64, bool), PathRegistrationError> {
        self.stages.register_path(hops)
    }

    /// Register a path and eagerly solve it (DELEGATION, ADR-050 D4).
    ///
    /// # Errors
    ///
    /// Propagates the typed [`PathRegistrationError`].
    pub fn register_and_solve_path(
        &self,
        hops: Vec<PoolHop>,
    ) -> Result<(u64, bool), PathRegistrationError> {
        self.stages.register_and_solve_path(hops)
    }

    /// De-register a path. Returns `true` if it existed (DELEGATION).
    pub fn deregister_path(&self, path_id: u64) -> bool {
        self.stages.deregister_path(path_id)
    }

    /// Set the registered-path cap (DELEGATION).
    pub fn set_path_cap(&self, cap: Option<usize>) {
        self.stages.set_path_cap(cap);
    }

    /// The registered path count (DELEGATION).
    #[must_use]
    pub fn path_count(&self) -> usize {
        self.stages.path_count()
    }

    /// The dedup-hit counter (DELEGATION).
    #[must_use]
    pub fn path_dedups(&self) -> u64 {
        self.stages.path_dedups()
    }

    /// Resolve a path id to its encoder projection (DELEGATION).
    #[must_use]
    pub fn path_info_for(&self, path_id: u64) -> Option<Result<PathInfo, PathInfoBuildError>> {
        self.stages.path_info_for(path_id)
    }

    /// Apply an operator retune (DELEGATION).
    pub fn apply_retune(&self, retune: &EngineRetune) {
        self.stages.apply_retune(retune);
    }

    /// Install the inline-sim hook (DELEGATION).
    pub fn set_inline_simulator(&self, sim: Arc<dyn InlineSimulator>) {
        self.stages.set_inline_simulator(sim);
    }

    /// The last solved results + block.
    #[must_use]
    pub fn latest_results(&self) -> (HashMap<u64, SolvePathResult>, u64) {
        self.stages.latest_results()
    }

    /// Install a pending subscribe state (test-only seam over the private
    /// session field).
    #[cfg(test)]
    pub(crate) fn install_subscribe_state_for_test(&self, state: DriverSubscribeState) {
        *self.subscribe_state.lock() = Some(state);
    }

    /// Inject the next `subscribe`'s failure shape (test-only seam over the
    /// dial, which needs a live WS endpoint).
    #[cfg(test)]
    pub(crate) fn set_subscribe_fault_for_test(&self, fault: SubscribeFault) {
        *self.subscribe_fault.lock() = Some(fault);
    }

    /// Hold `resume` inside its in-flight window until the sender fires or
    /// drops (test-only seam: the window is otherwise a live backfill await).
    #[cfg(test)]
    pub(crate) fn set_resume_gate_for_test(&self, gate: tokio::sync::oneshot::Receiver<()>) {
        *self.resume_gate.lock() = Some(gate);
    }
}

/// Soak 2026-08-22 forensics: name teardown paths that bypass
/// [`EngineDriver::stop`]. If the pump task
/// handle is still armed at drop time, unwinding tore down the driver without
/// calling `stop()` — the silent-exit shape this exists to catch. Leveling:
/// only the bypassed-`stop()` shape is WARN; a post-`stop()` drop
/// (`pump_handle_armed() == false`) is the healthy path every session takes
/// at exit — warning there trains operators to dismiss the line and buries
/// the real signal.
impl Drop for EngineDriver {
    fn drop(&mut self) {
        if self.pump_handle_armed() {
            op_warn!(
                domain = pump,
                pump_task_still_armed = true,
                "EngineDriver dropped WITHOUT stop() — unwind bypassed graceful shutdown"
            );
        } else {
            diag!(
                domain = pump,
                pump_task_still_armed = false,
                "EngineDriver dropped after stop()"
            );
        }
    }
}

/// The claim key for one pool: the family plus the identity that names it, so
/// two families at one address — or two pools under one `PoolManager` — never
/// share a claim window.
#[expect(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::arb_engine::crawl::controller::{
        CrawlConfig, CrawlController, CrawlHandle, CrawlHost, CrawlState, CrawlUnitFuture,
        CrawlUnitSeat, RunPhase,
    };
    use crate::arb_engine::crawl::discovery::{CrawlDiscoveryError, DiscoverySource, Edition};
    use crate::arb_engine::crawl::policy::CrawlPathPolicy;
    use crate::arb_engine::pump_session::PumpDecline;
    use crate::bot_core::construction_io::{ConstructionIo, NoDb, RpcConstruction};
    use crate::bot_core::ConstructionRoute;
    use alloy::primitives::{address, Bytes, U256};
    use degenbot_db::PathGraphData;
    use degenbot_decoders::v4_swap_decoder::V4PoolId;
    use degenbot_eventhub::OverflowPolicy;
    use degenbot_pathfinding::PoolKind;
    use degenbot_pools::PoolTickCoverage;
    use futures_util::StreamExt;
    use hashbrown::HashSet;

    /// Build an offline driver over a fresh `Bot` + `EngineStages`.
    fn driver_for_test() -> EngineDriver {
        let bot = Arc::new(Bot::new(1));
        let stages = Arc::new(EngineStages::with_core(bot.state_arc(), bot.active_delta()));
        EngineDriver::from_stages(bot, stages)
    }

    /// An alloy mock-transport provider (never hit on the no-RPC test paths).
    fn mock_provider() -> Arc<AlloyProvider> {
        mock_provider_and_asserter().0
    }

    /// [`mock_provider`] plus the `Asserter`, so a test can queue the exact RPC
    /// response the driver's own backfill will consume.
    fn mock_provider_and_asserter() -> (Arc<AlloyProvider>, alloy::transports::mock::Asserter) {
        use alloy::network::Ethereum as NetEth;
        use alloy::providers::{Provider, ProviderBuilder};
        use alloy::rpc::client::ClientBuilder;
        use alloy::transports::mock::{Asserter, MockTransport};
        let asserter = Asserter::new();
        let client = ClientBuilder::default().transport(MockTransport::new(asserter.clone()), true);
        let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
        (
            Arc::new(AlloyProvider::from_provider(
                Arc::new(dyn_provider) as Arc<dyn Provider<NetEth>>
            )),
            asserter,
        )
    }

    #[test]
    fn new_driver_starts_detached() {
        let driver = driver_for_test();
        assert_eq!(driver.current_phase(), PumpPhase::Detached);
        assert!(!driver.is_stopped());
        assert!(!driver.pump_handle_armed());
    }

    #[test]
    fn take_result_receiver_is_once_only() {
        let driver = driver_for_test();
        assert!(driver.take_result_receiver().is_some());
        assert!(driver.take_result_receiver().is_none());
    }

    #[test]
    fn take_block_receiver_is_once_only() {
        let driver = driver_for_test();
        assert!(driver.take_block_receiver().is_some());
        assert!(driver.take_block_receiver().is_none());
    }

    #[test]
    fn engine_channels_are_audited_unbounded_on_the_driver_hub() {
        let driver = driver_for_test();
        assert_eq!(
            driver.hub().named_policy(RESULT_CHANNEL_NAME),
            Some(OverflowPolicy::UnboundedFlagged {
                name: RESULT_CHANNEL_NAME
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
        for name in [RESULT_CHANNEL_NAME, BLOCK_CHANNEL_NAME] {
            assert_eq!(
                host.hub().named_policy(name),
                Some(OverflowPolicy::UnboundedFlagged { name }),
                "the host-minted hub carries {name}"
            );
        }

        assert!(driver.take_block_receiver().is_some());
        // The receiver the driver hands out must have the stages' live
        // producer behind it: an orphaned receiver (its sender dropped)
        // resolves `recv` immediately, while a wired one stays pending until
        // `stop` drops the sender.
        let mut rx = driver.take_result_receiver().unwrap();
        assert!(
            futures_util::FutureExt::now_or_never(rx.recv()).is_none(),
            "the host-minted receiver has a live producer wired to it"
        );
        driver.stop().expect("stop");
        let end = degenbot_core::runtime::get_runtime().block_on(async { rx.recv().await });
        assert!(
            end.is_none(),
            "the host-minted result stream closes with its driver"
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
    fn resume_before_subscribe_is_rejected_with_a_phase_error() {
        let driver = driver_for_test();
        let err = degenbot_core::runtime::get_runtime()
            .block_on(driver.resume())
            .unwrap_err();
        match err {
            DriverError::Phase(p) => {
                assert_eq!(p.method, "resume");
                assert_eq!(p.current, PumpPhase::Detached);
                assert_eq!(p.required, Some(PumpPhase::Subscribed));
            }
            other => assert!(
                !matches!(other, DriverError::Phase(_)),
                "unexpected: {other:?}"
            ),
        }
    }

    #[test]
    fn subscribe_after_resume_phase_is_rejected_before_any_ws_connect() {
        let driver = driver_for_test();
        driver.set_pose_for_test(PumpPhase::Running);
        let err = degenbot_core::runtime::get_runtime()
            .block_on(driver.subscribe("ws://127.0.0.1:1"))
            .unwrap_err();
        assert!(matches!(err, DriverError::Phase(_)), "got {err:?}");
    }

    #[test]
    fn resume_without_pending_subscribe_state_is_rejected() {
        let driver = driver_for_test();
        let _result_rx = driver.take_result_receiver();
        driver.set_pose_for_test(PumpPhase::Subscribed);
        let err = degenbot_core::runtime::get_runtime()
            .block_on(driver.resume())
            .unwrap_err();
        assert!(matches!(err, DriverError::SessionState(_)), "got {err:?}");
    }

    #[test]
    fn resume_without_result_receiver_is_rejected() {
        let driver = driver_for_test();
        let reorg = Arc::new(ReorgCoordinator::new(Arc::clone(driver.bot())));
        let stages_handlers: Arc<dyn StageHandlers> = driver.stages().clone();
        let control: Arc<dyn PumpControl> = driver.stages().clone();
        let pump = BlockPump::for_test(
            Arc::clone(driver.bot()),
            stages_handlers,
            control,
            reorg,
            mock_provider(),
            Arc::clone(&driver.shutdown),
        );
        driver.install_subscribe_state_for_test(DriverSubscribeState {
            pump,
            first_block: 100,
            combined_stream: futures_util::stream::empty().boxed(),
        });
        driver.set_pose_for_test(PumpPhase::Subscribed);

        let err = degenbot_core::runtime::get_runtime()
            .block_on(driver.resume())
            .unwrap_err();
        assert!(matches!(err, DriverError::NoResultReceiver), "got {err:?}");
        assert!(
            format!("{err}").contains("result receiver"),
            "the refusal names the missing consumer: {err}"
        );
        assert!(
            !driver.pump_handle_armed(),
            "a refused resume must not spawn the live loop"
        );
        assert!(
            driver.take_result_receiver().is_some(),
            "the receiver is still takeable after the refusal"
        );
    }

    // ── the crawl teardown edge ─────────────────────────────────────────

    /// The block the crawl fixture's construction stub names.
    const CRAWL_BLOCK: u64 = 18_000_000;

    /// The fixture's boundary token (the path start and end).
    const CRAWL_WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");

    /// The fixture's profit token.
    const CRAWL_USDC: Address = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");

    /// The `PoolManager` the fixture's V4 edges name.
    const CRAWL_V4_PM: Address = address!("3333333333333333333333333333333333333333");

    /// The `V4` graph-id offset the database read applies.
    const CRAWL_V4_OFFSET: u64 = 1 << 32;

    /// The fixture graph's two token ids.
    const CRAWL_TOKEN_A: u64 = 1;
    const CRAWL_TOKEN_B: u64 = 2;

    /// A construction stub the fixture's units never reach: every candidate hop
    /// is a V4 hop with no decodable pool hash, so the unit answers the bounded
    /// `V4NoHash` skip at the build stage and spends no read.
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
            _address: Address,
            _block_number: Option<u64>,
        ) -> Result<Bytes, degenbot_core::errors::ProviderError> {
            Ok(Bytes::new())
        }

        async fn get_balance(
            &self,
            _address: Address,
            _block_number: Option<u64>,
        ) -> Result<U256, degenbot_core::errors::ProviderError> {
            Ok(U256::ZERO)
        }

        async fn call(
            &self,
            _to: Address,
            _data: Bytes,
            _block_number: Option<u64>,
        ) -> Result<Bytes, degenbot_core::errors::ProviderError> {
            panic!("the crawl fixture's candidates are cheap skips: no construction read is made")
        }
    }

    /// A scripted discovery source: the fixture graph and a stable edition.
    struct SkipSource {
        data: PathGraphData,
    }

    impl DiscoverySource for SkipSource {
        fn edition(&self) -> Result<Edition, CrawlDiscoveryError> {
            Ok(Edition::default())
        }

        fn resolve_token_ids(
            &self,
            _addresses: &[Address],
        ) -> Result<HashMap<Address, u64>, CrawlDiscoveryError> {
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

    /// Two parallel V4 pools between the fixture's two tokens, neither with a
    /// decodable hash: every cycle is a real candidate that answers the cheap
    /// `V4NoHash` skip, so a unit resolves without a read.
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

    /// The fixture crawl's configuration: one-unit window, two-hop cycles.
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

    /// A seat that resolves each unit after `delay`, recording the crawl's pose
    /// and join fact at that moment — the window a latched stop has to leave
    /// draining, observed from inside the crawl.
    struct DriftingSeat {
        controller: Arc<std::sync::OnceLock<Arc<CrawlController>>>,
        observed: Arc<Mutex<Option<(CrawlState, bool)>>>,
        delay: Duration,
    }

    impl CrawlUnitSeat for DriftingSeat {
        fn dispatch(&self, future: CrawlUnitFuture) -> Result<(), String> {
            let holder = Arc::clone(&self.controller);
            let observed = Arc::clone(&self.observed);
            let delay = self.delay;
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                if let Some(controller) = holder.get() {
                    *observed.lock() = Some((controller.state(), controller.is_finished()));
                }
                future.await;
            });
            Ok(())
        }
    }

    struct CrawlFixture {
        controller: Arc<CrawlController>,
        run: tokio::task::JoinHandle<()>,
        observed: Arc<Mutex<Option<(CrawlState, bool)>>>,
    }

    /// Boot a crawl whose run future is LIVE and takes `delay` to drain its
    /// window, handing back the `JoinHandle` the controller deliberately never
    /// spawns for itself.
    fn live_crawl_for_test(driver: &Arc<EngineDriver>, delay: Duration) -> CrawlFixture {
        let observed = Arc::new(Mutex::new(None));
        let holder = Arc::new(std::sync::OnceLock::new());
        let seat = Arc::new(DriftingSeat {
            controller: Arc::clone(&holder),
            observed: Arc::clone(&observed),
            delay,
        });
        let io = Arc::new(ConstructionIo::new(Arc::new(NoDb), Arc::new(UnreachedRpc)));
        let host = CrawlHost::from_driver(driver, io, None);
        let controller = CrawlController::attach_with(
            host,
            crawl_config_for_test(),
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
        let runtime = degenbot_core::runtime::get_runtime();
        let run = runtime.spawn(async move {
            let _ = runner.run().await;
        });
        // `Live` is the run's own arming edge, taken before the sweep prepares,
        // so waiting on it means the stop below always meets a run to latch.
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async {
                while controller.run_phase() != RunPhase::Live {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .expect("the run arms before it prepares its sweep");
        });
        CrawlFixture {
            controller,
            run,
            observed,
        }
    }

    #[test]
    fn a_stop_with_no_crawl_plans_an_empty_edge_and_tears_down_as_before() {
        let driver = driver_for_test();
        assert!(
            driver.crawl.lock().is_none(),
            "no crawl is installed until the boot wiring installs one"
        );
        let started = std::time::Instant::now();
        driver.stop().expect("stop with no crawl");
        assert!(
            started.elapsed() < PUMP_STOP_GRACE,
            "an empty crawl edge owes no wait; took {:?}",
            started.elapsed()
        );
        assert_eq!(driver.current_phase(), PumpPhase::Stopped);
        assert!(!driver.pump_handle_armed());
    }

    #[test]
    fn a_stop_latches_the_crawl_and_joins_its_run_within_the_grace() {
        let driver = Arc::new(driver_for_test());
        let fixture = live_crawl_for_test(&driver, Duration::from_millis(500));
        assert_eq!(
            fixture.controller.state(),
            CrawlState::Running {
                last_complete: None
            },
            "the fixture's run is live and its window is not yet drained"
        );
        // Install the pair exactly as the boot wiring will: the narrowed edge
        // and the run future's handle, together.
        *driver.crawl.lock() = Some(CrawlTeardown {
            handle: CrawlHandle::new(Arc::clone(&fixture.controller)),
            run: fixture.run,
        });

        let started = std::time::Instant::now();
        driver.stop().expect("stop with a live crawl");
        let elapsed = started.elapsed();

        assert!(
            elapsed < PUMP_STOP_GRACE,
            "the crawl's run is joined inside the grace window, not aborted; took {elapsed:?}"
        );
        assert!(
            elapsed >= Duration::from_millis(300),
            "the stop really waited for the unit in flight; took {elapsed:?}"
        );
        assert_eq!(driver.current_phase(), PumpPhase::Stopped);
        assert!(driver.crawl.lock().is_none(), "the teardown took the pair");
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
    fn stop_is_idempotent_and_lands_stopped() {
        let driver = driver_for_test();
        driver.stop().expect("first stop");
        // Nothing was armed, so teardown had nothing to join: the table lands
        // `Stopped` at once (a live session lands `Stopping` first and owes one
        // `StopCompleted`).
        assert_eq!(driver.current_phase(), PumpPhase::Stopped);
        assert!(driver.is_stopped());
        driver.stop().expect("second stop is a no-op");
        assert_eq!(driver.current_phase(), PumpPhase::Stopped);
        let err = degenbot_core::runtime::get_runtime()
            .block_on(driver.resume())
            .unwrap_err();
        assert!(matches!(err, DriverError::SessionState(_)), "got {err:?}");
        let err = degenbot_core::runtime::get_runtime()
            .block_on(driver.subscribe("ws://127.0.0.1:1"))
            .unwrap_err();
        assert!(matches!(err, DriverError::SessionState(_)), "got {err:?}");
    }

    #[test]
    fn begin_crawl_refuses_a_bot_without_construction_io() {
        let driver = Arc::new(driver_for_test());
        let error = driver
            .begin_crawl(crawl_config_for_test())
            .err()
            .expect("a driver with no ConstructionIo cannot boot a crawl");
        assert!(
            matches!(error, CrawlConfigError::MissingConstructionIo),
            "got {error:?}"
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
        // boundary rows, so the run finishes on its own — the boot wiring is
        // what this test pins, not discovery.
        let path = std::env::temp_dir().join(format!(
            "degenbot-crawl-boot-{}-{:?}.sqlite",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut config = crawl_config_for_test();
        config.database_path = path.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let controller = driver
            .begin_crawl_with(CrawlBoot::new(config).with_receipts(tx))
            .expect("the driver boots the crawl");

        // The join fact is the DRIVER's: the controller never spawned its own
        // future, so a consumer joins through the handle the driver kept.
        let handle = driver.crawl_handle().expect("the boot installed the edge");
        let runtime = degenbot_core::runtime::get_runtime();
        runtime.block_on(async {
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
        // The run left its OWN answer, which is what a stream-owning consumer
        // reads instead of the join: this crawl is over an empty database, so
        // discovery refuses the absent boundary token loudly.
        let outcome = controller
            .run_outcome()
            .expect("the run records how it resolved");
        assert!(
            outcome
                .detail()
                .is_some_and(|detail| detail.contains("absent from the chain 1 token table")),
            "got {outcome:?}"
        );
        assert!(
            !outcome.is_clean(),
            "a refused discovery is not a clean run"
        );
        assert!(
            runtime.block_on(async { rx.recv().await }).is_none(),
            "the resolution edge ends the receipt stream"
        );

        // A second boot is refused: one crawl per driver.
        let error = driver
            .begin_crawl(crawl_config_for_test())
            .err()
            .expect("a driver holds one crawl");
        assert!(
            matches!(error, CrawlConfigError::CrawlAlreadyBegun),
            "got {error:?}"
        );

        driver.stop().expect("the shared stop tears the crawl down");
        assert!(
            driver.crawl_handle().is_none(),
            "the teardown took the pair"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn stop_closes_the_result_stream_exactly_once() {
        let driver = driver_for_test();
        let mut rx = driver.take_result_receiver().unwrap();
        driver.stop().expect("stop");
        let end = degenbot_core::runtime::get_runtime().block_on(async { rx.recv().await });
        assert!(end.is_none(), "stop must close the result stream");
        assert!(
            degenbot_core::runtime::get_runtime()
                .block_on(async { rx.recv().await })
                .is_none(),
            "a second recv stays ended"
        );
    }

    #[test]
    fn stop_aborts_a_running_pump_handle() {
        let driver = driver_for_test();
        let runtime = degenbot_core::runtime::get_runtime();
        let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
        let handle = runtime.spawn(async move {
            let _ = rx.await;
        });
        *driver.pump_handle.lock() = Some(handle);
        assert!(driver.pump_handle_armed());
        let started = std::time::Instant::now();
        driver.stop().expect("stop with a running handle");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "stop must abort the pending pump task promptly"
        );
        assert!(!driver.pump_handle_armed());
    }

    /// Minimal `tracing_subscriber::Layer` that records every event's level +
    /// message body. Same pattern as `LoudCloseCapture` in
    /// `engine_stages.rs` / `ReorgSpanCapture` in the block-pump tests: a real
    /// subscriber through `tracing::subscriber::with_default`, so the test
    /// observes the actual `op_info!`/`op_error!` dispatch rather than a mocked
    /// logger. Used to tell the pump's COOPERATIVE exit apart from the abort
    /// ESCALATION.
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

    /// Build a driver whose pump is resumed on a stream that NEVER yields, so
    /// the live loop parks in its select and only the 500 ms `timed_exit_tick`
    /// arm can end it — the exact shape `stop()`'s cooperative path serves.
    fn driver_with_parked_pump() -> EngineDriver {
        let driver = driver_for_test();
        let _result_rx = driver.take_result_receiver();
        let reorg = Arc::new(ReorgCoordinator::new(Arc::clone(driver.bot())));
        let stages_handlers: Arc<dyn StageHandlers> = driver.stages().clone();
        let control: Arc<dyn PumpControl> = driver.stages().clone();
        let pump = BlockPump::for_test(
            Arc::clone(driver.bot()),
            stages_handlers,
            control,
            reorg,
            mock_provider(),
            Arc::clone(&driver.shutdown),
        );
        driver.install_subscribe_state_for_test(DriverSubscribeState {
            pump,
            first_block: 100,
            combined_stream: futures_util::stream::pending::<WsEvent>().boxed(),
        });
        driver.set_pose_for_test(PumpPhase::Subscribed);
        degenbot_core::runtime::get_runtime()
            .block_on(driver.resume())
            .expect("resume with a pending subscribe state");
        assert!(driver.pump_handle_armed());
        driver
    }

    /// **Cooperative test.** A pump parked in its select (a stream that never
    /// yields) observes the shutdown flag on its OWN 500 ms tick arm and
    /// returns normally. Asserted three ways: the driver's cooperative-exit
    /// line is emitted, the abort escalation line is NOT, and the stop
    /// completes inside roughly one tick — far inside the 2 s grace.
    ///
    /// `stop()` drives its join on the test thread, so the thread-local capture
    /// sees the stop-path lines the driver emits.
    #[test]
    fn stop_exits_cooperatively_without_aborting_a_parked_pump() {
        use tracing_subscriber::layer::SubscriberExt;
        let capture = StopLogCapture::default();
        let subscriber = tracing_subscriber::registry().with(capture.clone());
        tracing::subscriber::with_default(subscriber, || {
            let driver = driver_with_parked_pump();
            // Let the resume task actually reach its parked select before the
            // stop, so this exercises the tick arm and not a pre-select race.
            std::thread::sleep(std::time::Duration::from_millis(50));

            let started = std::time::Instant::now();
            driver.stop().expect("cooperative stop");
            let elapsed = started.elapsed();

            assert!(
                elapsed >= std::time::Duration::from_millis(400),
                "the cooperative exit is reached via the 500ms tick, not instantly; took {elapsed:?}"
            );
            assert!(
                elapsed < std::time::Duration::from_millis(1500),
                "stop must return inside one tick window, well inside the grace; took {elapsed:?}"
            );
            assert!(!driver.pump_handle_armed());
            // The teardown landed the table: `Running -> Stopping -> Stopped`, so
            // the projection never claims a live loop after `stop()`.
            assert_eq!(driver.current_phase(), PumpPhase::Stopped);
            assert!(
                capture.describes("exited cooperatively"),
                "the cooperative exit must be logged; INFO lines were {:?}",
                capture.messages(tracing::Level::INFO)
            );
            assert!(
                !capture.describes("did not exit cooperatively"),
                "the abort escalation must NOT fire for a pump parked at its select; INFO lines were {:?}",
                capture.messages(tracing::Level::INFO)
            );
        });
    }

    /// **Escalation test.** A task that never polls the shutdown flag — an
    /// unconditional park — is the pump handle `stop()` must join. The grace
    /// expires and the `abort()` fallback fires, and the stop still resolves.
    ///
    /// This is the decomposition the chunk brief sanctions: the REAL
    /// non-cooperative parks named in `run_loop.rs` (a GIL re-entry park via
    /// `PySubscriberAdapter`, or engine-lock contention inside `on_drain`) are
    /// Python-embedding / lock-ordering shapes that cannot be constructed from
    /// the offline test driver without disproportionate scaffolding (a hostile
    /// `StageHandlers`/`PumpControl` impl holding the engine lock through the
    /// whole park). An unconditional park is their minimal honest equivalent:
    /// both are "parked somewhere the 500 ms tick cannot reach".
    #[test]
    fn stop_escalates_to_abort_when_the_grace_expires() {
        use tracing_subscriber::layer::SubscriberExt;
        let capture = StopLogCapture::default();
        let subscriber = tracing_subscriber::registry().with(capture.clone());
        tracing::subscriber::with_default(subscriber, || {
            let driver = driver_for_test();
            let runtime = degenbot_core::runtime::get_runtime();
            let handle = runtime.spawn(std::future::pending::<()>());
            *driver.pump_handle.lock() = Some(handle);
            assert!(driver.pump_handle_armed());

            let started = std::time::Instant::now();
            driver.stop().expect("stop must complete after escalation");
            let elapsed = started.elapsed();

            assert!(
                elapsed >= PUMP_STOP_GRACE,
                "the abort fallback must wait out the full grace; took {elapsed:?}"
            );
            assert!(
                elapsed < std::time::Duration::from_secs(10),
                "the escalation must still resolve promptly; took {elapsed:?}"
            );
            assert!(!driver.pump_handle_armed());
            assert!(
                capture.describes("did not exit cooperatively"),
                "the abort fallback must be logged; INFO lines were {:?}",
                capture.messages(tracing::Level::INFO)
            );
            assert!(
                !capture.describes("exited cooperatively"),
                "the cooperative arm must not be logged for a non-cooperative park; INFO lines were {:?}",
                capture.messages(tracing::Level::INFO)
            );
        });
    }

    #[test]
    fn resume_owns_the_backfill_and_spawns_the_live_loop() {
        let driver = driver_for_test();
        // The engine gate requires a consumer to own the result receiver.
        let _result_rx = driver.take_result_receiver();
        let reorg = Arc::new(ReorgCoordinator::new(Arc::clone(driver.bot())));
        let stages_handlers: Arc<dyn StageHandlers> = driver.stages().clone();
        let control: Arc<dyn PumpControl> = driver.stages().clone();
        let pump = BlockPump::for_test(
            Arc::clone(driver.bot()),
            stages_handlers,
            control,
            reorg,
            mock_provider(),
            Arc::clone(&driver.shutdown),
        );
        // S stays None (cold start) so the driver's owned backfill is a no-op;
        // the point is that `resume` drives the pump, not the consumer. The live
        // stream NEVER yields, so the pump parks: `Running` is then the pose a
        // reader observes, not a race against a loop that already exited.
        driver.install_subscribe_state_for_test(DriverSubscribeState {
            pump,
            first_block: 100,
            combined_stream: futures_util::stream::pending::<WsEvent>().boxed(),
        });
        driver.set_pose_for_test(PumpPhase::Subscribed);
        degenbot_core::runtime::get_runtime()
            .block_on(driver.resume())
            .expect("resume with a pending subscribe state");
        assert_eq!(
            driver.current_phase(),
            PumpPhase::Running,
            "resume lands the table's `Subscribed -> Resuming -> Running` chain"
        );
        assert!(driver.pump_handle_armed());
        // A second resume is an ordering decline: the session is already
        // running, so there is nothing left to resume.
        let err = degenbot_core::runtime::get_runtime()
            .block_on(driver.resume())
            .unwrap_err();
        assert!(matches!(err, DriverError::Phase(_)), "got {err:?}");
        driver.stop().expect("stop");
        assert!(!driver.pump_handle_armed());
    }

    /// **The failed-subscribe wedge.** `subscribe` publishes `Connecting`
    /// before its fallible step, so every failure return must publish the
    /// table's `SubscribeFailed` landing (`Detached`) — otherwise the pose sits
    /// in `Connecting`, where both `subscribe` and `resume` are declined and the
    /// session is unrecoverable. Both failure shapes land the same way, and the
    /// retry must reach the dial again (a phase decline here would mean the
    /// wedge is back).
    #[test]
    fn a_failed_subscribe_lands_detached_and_is_retryable() {
        let driver = driver_for_test();
        let runtime = degenbot_core::runtime::get_runtime();

        for fault in [SubscribeFault::Handshake, SubscribeFault::NoStream] {
            driver.set_subscribe_fault_for_test(fault);
            let err = runtime
                .block_on(driver.subscribe("ws://127.0.0.1:1"))
                .unwrap_err();
            assert!(matches!(err, DriverError::Subscribe(_)), "got {err:?}");
            assert_eq!(
                driver.current_phase(),
                PumpPhase::Detached,
                "{fault:?} must land the session back on detached"
            );
            assert!(!driver.pump_handle_armed(), "no live loop was armed");
            assert!(
                driver.subscribe_state.lock().is_none(),
                "no pending subscribe state survives the failure"
            );
        }

        // The retry is legal: it reaches the dial again. (`Connecting` would
        // decline with `DriverError::Phase` before the seam was consulted.)
        driver.set_subscribe_fault_for_test(SubscribeFault::Handshake);
        let retry = runtime
            .block_on(driver.subscribe("ws://127.0.0.1:1"))
            .unwrap_err();
        assert!(
            matches!(retry, DriverError::Subscribe(_)),
            "the retry must reach the WS step, not decline from a stuck pose: {retry:?}"
        );

        // And `resume` answers honestly: nothing is subscribed, so it is a table
        // decline, not a session wedged above the resume door.
        let resumed = runtime.block_on(driver.resume()).unwrap_err();
        assert!(matches!(resumed, DriverError::Phase(_)), "got {resumed:?}");
    }

    /// **The cancelled-resume wedge, and the terminal landing.** `resume` takes
    /// the pending subscribe state (dropping the pump and its WS session with
    /// the future) while the backfill is in flight, so a cancel that left the
    /// pose on `Subscribed` would refuse both doors. The in-flight pose
    /// (`Resuming`) plus the `PhaseLeg` abandonment guard land it on
    /// `Abandoned` — which is TERMINAL: a follow-up `subscribe` is the table's
    /// typed decline (the backfill buffer stages events without dedup, so a
    /// re-subscribe could double-apply liquidity events at registration), and
    /// `stop` is the one exit that remains.
    #[test]
    fn a_cancelled_resume_lands_abandoned_and_is_terminal() {
        let driver = Arc::new(driver_for_test());
        let runtime = degenbot_core::runtime::get_runtime();
        let _result_rx = driver.take_result_receiver();
        let reorg = Arc::new(ReorgCoordinator::new(Arc::clone(driver.bot())));
        let stages_handlers: Arc<dyn StageHandlers> = driver.stages().clone();
        let control: Arc<dyn PumpControl> = driver.stages().clone();
        let pump = BlockPump::for_test(
            Arc::clone(driver.bot()),
            stages_handlers,
            control,
            reorg,
            mock_provider(),
            Arc::clone(&driver.shutdown),
        );
        driver.install_subscribe_state_for_test(DriverSubscribeState {
            pump,
            first_block: 100,
            combined_stream: futures_util::stream::pending::<WsEvent>().boxed(),
        });
        driver.set_pose_for_test(PumpPhase::Subscribed);

        let (release, gate) = tokio::sync::oneshot::channel::<()>();
        driver.set_resume_gate_for_test(gate);
        let task = {
            let driver = Arc::clone(&driver);
            runtime.spawn(async move { driver.resume().await })
        };
        runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while driver.current_phase() != PumpPhase::Resuming {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
            })
            .await
            .expect("resume must publish its in-flight pose before its first await");
        });

        // Cancelling the future: the pump and the taken subscribe state die
        // with it.
        task.abort();
        assert!(
            runtime.block_on(task).is_err(),
            "the aborted resume task is cancelled"
        );
        drop(release);
        assert_eq!(
            driver.current_phase(),
            PumpPhase::Abandoned,
            "a cancelled resume lands the table's Resuming -> Abandoned edge"
        );
        assert!(!driver.pump_handle_armed());

        // The re-subscribe edge is gone: the table refuses it by name, before
        // the WS step is reached.
        let err = runtime
            .block_on(driver.subscribe("ws://127.0.0.1:1"))
            .unwrap_err();
        let decline = PumpDecline::SubscribeAfterAbandonment;
        match &err {
            DriverError::Phase(phase) => {
                assert_eq!(phase.method, "subscribe");
                assert_eq!(phase.current, PumpPhase::Abandoned);
                assert_eq!(phase.required, Some(PumpPhase::Detached));
                assert_eq!(
                    phase.detail,
                    decline.to_string(),
                    "the decline's own message reaches the caller"
                );
            }
            other => panic!(
                "a re-subscribe after abandonment must be the table's typed decline, got {other:?}"
            ),
        }
        assert_eq!(
            driver.current_phase(),
            PumpPhase::Abandoned,
            "a declined subscribe leaves the pose it found"
        );

        // `stop` — the one remaining exit — is still legal from `Abandoned`.
        driver.stop().expect("stop is total and the only exit");
        assert_eq!(driver.current_phase(), PumpPhase::Stopped);
        assert!(driver.is_stopped());
    }

    /// **The backfill-failure abort.** `resume` owns the `S+1..W` backfill; a
    /// failed `eth_getLogs` leaves that gap UNAPPLIED, so the resume aborts
    /// instead of starting a live loop over stale state. The failure lands
    /// `Abandoned` — the same edge a cancelled resume publishes, because the
    /// pump and its WS session drop with the early return — and the error
    /// itself reaches the caller typed.
    #[test]
    fn a_failed_backfill_aborts_the_resume_typed_and_armed_nowhere() {
        let driver = driver_for_test();
        let runtime = degenbot_core::runtime::get_runtime();
        let _result_rx = driver.take_result_receiver();
        // S = 85 < W = 100: a real gap, so resume asks for
        // eth_getLogs(86..=100) and the mock answers with an RPC error.
        driver.set_snapshot_seed_block(Some(85));
        let (provider, asserter) = mock_provider_and_asserter();
        asserter.push_failure_msg("injected: eth_getLogs unavailable");
        let reorg = Arc::new(ReorgCoordinator::new(Arc::clone(driver.bot())));
        let stages_handlers: Arc<dyn StageHandlers> = driver.stages().clone();
        let control: Arc<dyn PumpControl> = driver.stages().clone();
        let pump = BlockPump::for_test(
            Arc::clone(driver.bot()),
            stages_handlers,
            control,
            reorg,
            provider,
            Arc::clone(&driver.shutdown),
        );
        driver.install_subscribe_state_for_test(DriverSubscribeState {
            pump,
            first_block: 100,
            // A started live loop would park here forever: the prompt `Err` is
            // itself evidence that none was started.
            combined_stream: futures_util::stream::pending::<WsEvent>().boxed(),
        });
        driver.set_pose_for_test(PumpPhase::Subscribed);

        let err = runtime.block_on(driver.resume()).unwrap_err();
        match &err {
            DriverError::Backfill(detail) => assert!(
                detail.contains("eth_getLogs failed for blocks 86-100"),
                "the typed error names the offending chunk: {detail}"
            ),
            other => assert!(
                matches!(other, DriverError::Backfill(_)),
                "the resume must return the typed backfill error, got {other:?}"
            ),
        }
        assert_eq!(
            driver.current_phase(),
            PumpPhase::Abandoned,
            "a failed backfill lands the table's Resuming -> Abandoned edge"
        );
        assert!(
            !driver.pump_handle_armed(),
            "a failed backfill must NOT start the live loop: the S+1..W gap is unapplied"
        );
        assert!(
            driver.subscribe_state.lock().is_none(),
            "the consumed subscribe state does not survive the aborted resume"
        );
    }

    /// The guard's own contract: an armed leg that is dropped without landing
    /// publishes its abandonment edge, and a landed one does not.
    #[test]
    fn a_dropped_leg_publishes_its_abandonment_edge() {
        let driver = driver_for_test();
        {
            let _leg = PhaseLeg::enter(
                &driver,
                "subscribe",
                Some(PumpPhase::Detached),
                PumpEvent::SubscribeRequested,
                PumpEvent::SubscribeAbandoned,
            )
            .expect("detached admits subscribe");
            assert_eq!(driver.current_phase(), PumpPhase::Connecting);
        }
        assert_eq!(
            driver.current_phase(),
            PumpPhase::Detached,
            "a cancelled subscribe restores detached"
        );

        let leg = PhaseLeg::enter(
            &driver,
            "subscribe",
            Some(PumpPhase::Detached),
            PumpEvent::SubscribeRequested,
            PumpEvent::SubscribeAbandoned,
        )
        .expect("detached admits subscribe again");
        leg.land(PumpEvent::Subscribed).expect("connecting lands");
        assert_eq!(
            driver.current_phase(),
            PumpPhase::Subscribed,
            "a landed leg leaves the landing in place"
        );
    }

    #[test]
    fn wait_pump_finished_resolves_after_the_armed_pump_ends() {
        let driver = driver_for_test();
        let runtime = degenbot_core::runtime::get_runtime();
        // Arm a stand-in pump task exactly as `resume` does: the completion
        // sender moves into the task and drops when it returns.
        let completion_tx = driver.pump_finished_tx.lock().take();
        let handle = runtime.spawn(async move {
            let _completion_tx = completion_tx;
        });
        *driver.pump_handle.lock() = Some(handle);
        runtime.block_on(driver.wait_pump_finished());
    }

    #[test]
    fn wait_pump_finished_resolves_once_when_the_armed_pump_panics() {
        let driver = driver_for_test();
        let runtime = degenbot_core::runtime::get_runtime();
        let completion_tx = driver.pump_finished_tx.lock().take();
        let handle = runtime.spawn(async move {
            let _completion_tx = completion_tx;
            panic!("simulated pump panic");
        });
        *driver.pump_handle.lock() = Some(handle);
        // Two awaits both resolve: a panic is one terminal completion that
        // every waiter observes — it must never leave a waiter hanging.
        runtime.block_on(async {
            driver.wait_pump_finished().await;
            driver.wait_pump_finished().await;
        });
    }

    #[test]
    fn wait_pump_finished_resolves_for_a_late_waiter() {
        let driver = driver_for_test();
        let runtime = degenbot_core::runtime::get_runtime();
        // Simulate a pump that ended before any consumer existed: drop the
        // sender outright (closed channel = terminal completion).
        drop(driver.pump_finished_tx.lock().take());
        runtime.block_on(driver.wait_pump_finished());
    }

    #[test]
    fn a_non_verify_driver_error_classifies_as_fatal_other() {
        let classified = verify_refusal_from_driver(DriverError::NoResultReceiver);
        assert!(
            matches!(
                &classified,
                RefusalKind::VerifyFailed {
                    origin: VerifyOrigin::Other,
                    fatal: true,
                    ..
                }
            ),
            "a non-verify driver failure must classify as Other, got {classified:?}"
        );
        assert!(
            !crate::bot_core::verification_retry::is_retryable(&classified),
            "a non-verify driver failure is fatal, never retried"
        );
    }

    #[test]
    fn wait_session_end_delivers_the_pump_finished_fact() {
        let driver = driver_for_test();
        let runtime = degenbot_core::runtime::get_runtime();
        let completion_tx = driver.pump_finished_tx.lock().take();
        let handle = runtime.spawn(async move {
            let _completion_tx = completion_tx;
        });
        *driver.pump_handle.lock() = Some(handle);
        assert_eq!(
            runtime.block_on(driver.wait_session_end()),
            crate::arb_engine::session_end::SessionEndCause::PumpFinished
        );
    }

    #[test]
    fn wait_pump_finished_stays_pending_until_the_armed_pump_ends() {
        let driver = driver_for_test();
        let runtime = degenbot_core::runtime::get_runtime();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let completion_tx = driver.pump_finished_tx.lock().take();
        let handle = runtime.spawn(async move {
            let _completion_tx = completion_tx;
            let _ = release_rx.await;
        });
        *driver.pump_handle.lock() = Some(handle);
        runtime.block_on(async {
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

    // ── durable verified-pool fact (the verify-once registration invariant) ──

    /// Register a V3 pool directly in the driver's shared core.
    fn register_v3(driver: &EngineDriver, address: Address, coverage: PoolTickCoverage) -> u64 {
        use alloy::primitives::{U128, U256};
        use degenbot_pools::TickInfo;
        use degenbot_pools::{PoolTickCoverage, RegisterV3PoolParams};
        use hashbrown::HashMap;
        let mut tick_data = HashMap::new();
        if coverage == PoolTickCoverage::Tracked {
            tick_data.insert(
                60,
                TickInfo {
                    liquidity_gross: U128::from(100),
                    liquidity_net: 100i128,
                    block: 0,
                },
            );
        }
        driver
            .stages()
            .core()
            .write_at(LockSite::Registration)
            .register_v3_pool(&RegisterV3PoolParams {
                address,
                token0: Address::ZERO,
                token1: Address::from([1u8; 20]),
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data,
                update_block: 0,
                tick_data_block: None,
                coverage,
                fetcher: None,
                ..Default::default()
            })
            .expect("test setup: V3 registration")
    }

    fn register_sparse_v3(driver: &EngineDriver, address: Address) -> u64 {
        register_v3(driver, address, PoolTickCoverage::Sparse)
    }

    /// Register a Sparse V4 pool directly in the driver's shared core.
    fn register_sparse_v4(driver: &EngineDriver, pool_manager: Address, pool_id: V4PoolId) -> u64 {
        use alloy::primitives::U256;
        use degenbot_pools::PoolTickCoverage;
        use degenbot_pools::RegisterV4PoolParams;
        use degenbot_pools::V4PoolKey;
        use hashbrown::HashMap;
        driver
            .stages()
            .core()
            .write_at(LockSite::Registration)
            .register_v4_pool(&RegisterV4PoolParams {
                pool_manager,
                pool_id,
                pool_key: V4PoolKey {
                    currency0: Address::ZERO,
                    currency1: Address::from([1u8; 20]),
                    fee: 500,
                    tick_spacing: 10,
                    hooks: Address::ZERO,
                },
                state_view: Address::ZERO,
                hook_flags: 0,
                protocol_fee: 0,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data: HashMap::new(),
                update_block: 0,
                tick_data_block: None,
                coverage: PoolTickCoverage::Sparse,
                fetcher: None,
                known_bitmap_words: Vec::new(),
            })
            .expect("test setup: V4 registration")
    }

    /// The registration invariant the retired Python key cache provided: a
    /// later registration of the same identity does not re-run a COMPLETED
    /// lifecycle. Re-quarantine the verified pool and buffer a backfill event;
    /// a re-run would drain (apply) it, while a skipped run leaves it
    /// untouched — the observable witness that the lifecycle did not run.
    #[tokio::test]
    async fn repeat_registration_does_not_rerun_a_completed_v3_lifecycle() {
        let driver = driver_for_test();
        let address = Address::from([0x11u8; 20]);
        register_sparse_v3(&driver, address);
        let key = PreBuildIdentity::Address(address);

        driver
            .run_registration_lifecycle(PoolLookup::from_address(address), None)
            .await
            .expect("first lifecycle");
        assert!(driver.is_pool_verified(&key), "success records the fact");
        assert_eq!(driver.verified_pool_count(), 1);

        // Swap the now-verified Sparse pool for a Tracked one at the same
        // address: a re-run would reach the verify step and fail for lack of a
        // provider, so reaching `Ok` proves the durable fact short-circuited
        // the expensive lifecycle rather than running it again.
        {
            let shared = driver.stages().core();
            shared
                .write_at(LockSite::Registration)
                .unregister_pool(PoolLookup::from_address(address));
        }
        register_v3(&driver, address, PoolTickCoverage::Tracked);

        driver
            .run_registration_lifecycle(PoolLookup::from_address(address), None)
            .await
            .expect("a verified pool is not re-verified");
        assert_eq!(driver.verified_pool_count(), 1);
    }

    /// A failed lifecycle records no fact, so a later driver call retries it
    /// (here: a Tracked pool with no provider fails both times).
    #[tokio::test]
    async fn failed_driver_lifecycle_is_retriable_and_not_verified() {
        use alloy::primitives::U256;
        use degenbot_pools::{PoolTickCoverage, RegisterV3PoolParams};
        use hashbrown::HashMap;

        let driver = driver_for_test();
        let address = Address::from([0x12u8; 20]);
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            degenbot_pools::TickInfo {
                liquidity_gross: alloy::primitives::U128::from(100),
                liquidity_net: 100i128,
                block: 0,
            },
        );
        driver
            .stages()
            .core()
            .write_at(LockSite::Registration)
            .register_v3_pool(&RegisterV3PoolParams {
                address,
                token0: Address::ZERO,
                token1: Address::from([1u8; 20]),
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data,
                update_block: 0,
                tick_data_block: None,
                coverage: PoolTickCoverage::Tracked,
                fetcher: None,
                ..Default::default()
            })
            .expect("test setup: tracked V3 registration");
        let key = PreBuildIdentity::Address(address);

        for _ in 0..2 {
            assert!(
                driver
                    .run_registration_lifecycle(PoolLookup::from_address(address), None)
                    .await
                    .is_err(),
                "a tracked pool with no provider fails"
            );
            assert!(
                !driver.is_pool_verified(&key),
                "a failure must not record a verified fact"
            );
        }
        assert_eq!(driver.verified_pool_count(), 0);
    }

    /// The durable fact is scoped by the driver's family-scoped claim key: a
    /// V3 pool at an address does not verify a V4 pool under the same address
    /// as its manager, nor does one V4 `(manager, pool_id)` verify a sibling
    /// pool under the same manager.
    #[tokio::test]
    async fn driver_verify_facts_are_family_and_pair_scoped() {
        let driver = driver_for_test();
        let shared_address = Address::from([0x21u8; 20]);
        let sibling_id = [0x88u8; 32];

        register_sparse_v3(&driver, shared_address);
        register_sparse_v4(&driver, shared_address, sibling_id);

        let v3_key = PreBuildIdentity::Address(shared_address);
        let v4_key = PreBuildIdentity::V4(sibling_id);

        driver
            .run_registration_lifecycle(PoolLookup::from_address(shared_address), None)
            .await
            .expect("V3 lifecycle");
        assert!(driver.is_pool_verified(&v3_key));
        assert!(
            !driver.is_pool_verified(&v4_key),
            "a V3 fact is not a V4 fact at the same address"
        );

        driver
            .run_registration_lifecycle(
                PoolLookup::V4 {
                    pool_manager: shared_address,
                    pool_id: sibling_id,
                },
                None,
            )
            .await
            .expect("V4 lifecycle");
        assert!(driver.is_pool_verified(&v4_key));
        assert_eq!(driver.verified_pool_count(), 2);
    }
}
