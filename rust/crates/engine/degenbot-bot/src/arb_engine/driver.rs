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
//! 2. `resume()` gates on `PumpPhase::SnapshotLoaded`, **owns the
//!    `S+1..W` auto-backfill** (awaiting `BlockPump::backfill_with_drain`
//!    synchronously), then spawns the live pump loop and advances to
//!    `PumpPhase::Resumed`. Consumers never call `backfill_from_snapshot`.
//! 3. `stop()` sets the shutdown flag, waits up to `PUMP_STOP_GRACE` for the
//!    pump to exit **cooperatively** (its own 500 ms tick polls the flag), and
//!    only escalates to `abort()` + join if the grace expires; it then clears
//!    the subscribe state, closes the delivery channels, and latches the driver
//!    terminally stopped. It is any-phase + idempotent (the `BotRunner.shutdown`
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
//!   `RegistrationLifecycleError` so the adapter keeps its byte-identical
//!   Python exception mapping.
//! - **Verify provider.** Built by the driver (`set_verify_rpc_url` + an
//!   internal async builder used by `start`) and stored on the driver.
//! - **Lifecycle inputs are typed.** ADR-050 D4 sketches `&str` inputs; the
//!   implementation takes `Address`/`V4PoolId` so string parsing stays in the
//!   `PyO3` adapter, preserving its synchronous `ValueError` raise (the
//!   Python-visible behavior is byte-identical).
//! - **Main loop.** No `run_until_stopped` convenience: the consumer owns its
//!   consume/dispatch loop (ADR-050 D9).

use crate::arb_engine::lifecycle::PathRegistrationError;
use crate::arb_engine::{
    BlockNotification, EngineRetune, EngineStages, InlineSimulator, PumpPhase, ResultBatch,
};
use crate::bot_core::block_pump::{BlockPump, SubscribeState};
use crate::bot_core::liquidity_verifier::LiquidityVerifyError;
use crate::bot_core::registration_lifecycle::RegistrationLifecycleError;
use crate::bot_core::reorg_coordinator::ReorgCoordinator;
use crate::bot_core::snapshot_verify::VerifyError;
use crate::bot_core::verification_retry::{retry_verification_call, RetryPolicy};
use crate::bot_core::verify_claims::PoolVerifications;
use crate::bot_core::{Bot, PumpControl, StageHandlers};
use crate::strategy_host::HostHub;
use alloy::primitives::Address;
use degenbot_core::{diag, op_error, op_info, op_warn};
use degenbot_decoders::v4_swap_decoder::V4PoolId;
use degenbot_eventhub::Hub;
use degenbot_executor::composers::PathInfo;
use degenbot_ingestion::IngestEvent as WsEvent;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_solvers::mixed::{PoolHop, SolvePathResult};
use degenbot_substrate::path_info::PathInfoBuildError;
use degenbot_substrate::session_registry::PoolIdentity;
use degenbot_substrate::state_lock::{LockSite, StateLock};
use degenbot_substrate::BotState;
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
/// `PumpPhase::require` / `allow_subscribe` return a `String` for the
/// engine-internal callers; the driver wraps that in this typed value so a
/// consumer matches a variant instead of a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhaseError {
    /// The rejected driver method (`"subscribe"`, `"resume"`, …).
    pub method: &'static str,
    /// The pump-protocol phase at call time.
    pub current: PumpPhase,
    /// The phase boundary the method required (or `None` for the
    /// subscribe-window gate, which admits either `Created` or
    /// `SnapshotLoaded` — not a single boundary).
    pub required: Option<PumpPhase>,
    /// The exact engine-gate message (kept so Python-facing `RuntimeError`
    /// strings stay byte-identical).
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
    /// A pump-session precondition failed (already started/subscribed, not
    /// subscribed, already resumed, or the terminal stopped latch).
    SessionState(String),
    /// The `resume()` consumer-before-resume gate: no result receiver was
    /// taken, so the unbounded result channel would have no consumer.
    NoResultReceiver,
    /// The WS subscribe/handshake failed.
    Subscribe(String),
    /// A resume-time failure (currently only a missing WS stream).
    Resume(String),
    /// Typed path-registration refusal (propagated verbatim, ADR-050 D4).
    Registration(PathRegistrationError),
    /// The registration verify lifecycle failed — the core
    /// `RegistrationLifecycleError` (mismatch/RPC/missing config) propagated
    /// verbatim so the adapter can keep its typed Python mapping.
    Verify(RegistrationLifecycleError),
}

impl std::fmt::Display for DriverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Phase(e) => write!(f, "{e}"),
            Self::SessionState(m) | Self::Subscribe(m) | Self::Resume(m) => f.write_str(m),
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
    /// The per-event reorg coordinator (ADR-006 slice 7).
    reorg_coordinator: Arc<ReorgCoordinator>,
    /// The cooperative shutdown flag shared with the pump.
    shutdown: Arc<AtomicBool>,
    /// The spawned live-loop handle (`None` until `resume`).
    pump_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
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
    verifications: PoolVerifications<RegistrationLifecycleError>,
    /// The engine host's process-lifetime event hub. The engine's
    /// `ResultBatch` + `BlockNotification` source channels are registered on
    /// it as named, typed `UnboundedFlagged` channels; the hub holds each
    /// channel's consumer end until the once-only `take_*_receiver` handoff.
    hub: Arc<Hub>,
    /// The terminal stopped latch (ADR-050 D5) — `PumpPhase` cannot express
    /// teardown, so the driver owns it.
    stopped: AtomicBool,
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
/// [`VerifyError::Other`] so the retry never re-attempts it.
fn verify_error_from_driver(err: DriverError) -> VerifyError {
    match err {
        DriverError::Verify(RegistrationLifecycleError::Verify(
            LiquidityVerifyError::Mismatch(mismatch),
        )) => VerifyError::Snapshot(mismatch.message),
        DriverError::Verify(RegistrationLifecycleError::Verify(LiquidityVerifyError::Rpc {
            message,
        })) => VerifyError::Rpc(message),
        DriverError::Verify(RegistrationLifecycleError::MissingProvider) => {
            VerifyError::Provider(RegistrationLifecycleError::MissingProvider.to_string())
        }
        DriverError::Verify(RegistrationLifecycleError::MissingTickSpacing) => {
            VerifyError::NotConfigured(RegistrationLifecycleError::MissingTickSpacing.to_string())
        }
        other => VerifyError::Other(other.to_string()),
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
        stages.set_block_channel(channels.block);
        install_session_path_owner(&bot, &stages);
        let (pump_finished_tx, pump_finished_rx) = watch::channel(false);
        let reorg_coordinator = Arc::new(ReorgCoordinator::new(Arc::clone(&bot)));
        Self {
            bot,
            stages,
            reorg_coordinator,
            shutdown: Arc::new(AtomicBool::new(false)),
            pump_handle: Mutex::new(None),
            pump_finished_tx: Mutex::new(Some(pump_finished_tx)),
            pump_finished_rx,
            subscribe_state: Mutex::new(None),
            verify_rpc_url: Mutex::new(None),
            verify_provider: Mutex::new(None),
            verify_state_view: Mutex::new(None),
            verifications: PoolVerifications::new(),
            hub,
            stopped: AtomicBool::new(false),
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

    /// Advance the engine phase with no ordering check (callers validate).
    pub fn set_phase(&self, phase: PumpPhase) {
        self.stages.set_phase(phase);
    }

    /// `true` once `stop()` has latched the driver terminally stopped.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
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
    /// - [`DriverError::Phase`] when the phase is past the subscribe window.
    /// - [`DriverError::SessionState`] when the pump is already started or a
    ///   subscribe state is already pending, or the driver is stopped.
    /// - [`DriverError::Subscribe`] when the WS handshake fails.
    pub async fn subscribe(&self, node_ws: &str) -> Result<u64, DriverError> {
        if self.is_stopped() {
            return Err(DriverError::SessionState(
                "Cannot subscribe: driver has been stopped.".to_string(),
            ));
        }
        let phase = self.stages.current_phase();
        if let Err(detail) = phase.allow_subscribe("subscribe") {
            return Err(DriverError::Phase(PhaseError {
                method: "subscribe",
                current: phase,
                required: None,
                detail,
            }));
        }
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
        // Read S BEFORE subscribe so `after_subscribe` reflects whether the
        // core already holds a snapshot (the construction-time-load path,
        // per the regression fix above).
        let core_has_snapshot = self
            .bot
            .state_arc()
            .read_at(LockSite::Pump)
            .snapshot_seed_block()
            .is_some();
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
        *self.subscribe_state.lock() = Some(DriverSubscribeState {
            pump,
            first_block,
            combined_stream,
        });
        self.stages
            .set_phase(PumpPhase::after_subscribe(phase, core_has_snapshot));
        Ok(first_block)
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
    /// - [`DriverError::Phase`] when the phase is below `SnapshotLoaded`.
    /// - [`DriverError::SessionState`] when already `Resumed`, when no
    ///   subscribe state is pending, or when the driver is stopped.
    /// - [`DriverError::NoResultReceiver`] when the result receiver has not
    ///   been taken.
    /// - [`DriverError::Resume`] when the pending state carries no WS stream.
    ///
    /// # Cancel safety
    ///
    /// **Cancel-safe**: `resume` does not own the pump it starts. Once the
    /// synchronous `backfill_with_drain` returns and the live loop is handed
    /// to `tokio::spawn`, the spawned task OWNS the pump (`pump` is moved into
    /// the async block); only its `JoinHandle` is stored back into
    /// `self.pump_handle`. A dropped `resume` future therefore drops the
    /// handle's future, not the pump task — the pump keeps running to
    /// completion, and its completion sender (moved into the same task) still
    /// drops on whatever terminal path, so every `wait_pump_finished` waiter
    /// resolves exactly as if `resume` had returned normally.
    ///
    /// **The unsafe remainder is temporal, not structural**: if the future is
    /// cancelled BEFORE the spawn (during the awaited `backfill_with_drain`),
    /// the pump is never started and the pending `subscribe_state` was already
    /// `take()`n — the driver is left in `SnapshotLoaded` with no subscribe
    /// state, so a retry fails with [`DriverError::SessionState`] and the
    /// caller must `subscribe()` again. Cancelling after the spawn leaves a
    /// running pump whose handle is stored; the caller must not assume a
    /// cancelled `resume` means "not resumed".
    pub async fn resume(&self) -> Result<(), DriverError> {
        if self.is_stopped() {
            return Err(DriverError::SessionState(
                "Cannot resume: driver has been stopped.".to_string(),
            ));
        }
        let phase = self.stages.current_phase();
        if let Err(detail) = phase.require(PumpPhase::SnapshotLoaded, "resume") {
            return Err(DriverError::Phase(PhaseError {
                method: "resume",
                current: phase,
                required: Some(PumpPhase::SnapshotLoaded),
                detail,
            }));
        }
        if phase == PumpPhase::Resumed {
            return Err(DriverError::SessionState(
                "Cannot resume: engine is already in Resumed phase.".to_string(),
            ));
        }
        // Consumer-before-resume gate. The result channel is unbounded, so a
        // pump with no receiver attached accumulates batches no one drains.
        // This check runs before the subscribe state is consumed, so a refused
        // resume leaves the pending state intact for a corrected retry.
        if !self.result_receiver_taken.load(Ordering::SeqCst) {
            return Err(DriverError::NoResultReceiver);
        }
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
        // the backfill is SYNCHRONOUS with respect to `resume` so the
        // consumer's registration draining the per-pool backfill buffer cannot
        // race it. `backfill_with_drain` also re-injects live events
        // drained during the backfill ahead of the live tail.
        let (backfill_res, combined) = pump.backfill_with_drain(first_block, combined_stream).await;
        if let Err(e) = backfill_res {
            op_error!(
                domain = pump,
                first_block,
                %e,
                "EngineDriver: auto-backfill failed — starting live loop from gap (not closed)"
            );
        }
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
        self.stages.set_phase(PumpPhase::Resumed);
        Ok(())
    }

    /// Stop the pump and latch the driver terminally stopped (ADR-050 D6).
    ///
    /// Any-phase + idempotent. Sets the shutdown flag, waits up to
    /// [`PUMP_STOP_GRACE`] for the pump to exit **cooperatively** (it polls
    /// the flag on its own 500 ms tick arm), and only then escalates to
    /// `abort()` + join if the grace expires. Either way the WS subscription
    /// futures have dropped before return. Clears the subscribe state and
    /// closes the delivery channels so a pending receiver observes
    /// end-of-stream exactly once.
    ///
    /// # Errors
    ///
    /// Currently never returns `Err`; the typed result keeps the surface
    /// symmetric with `subscribe`/`resume`.
    pub fn stop(&self) -> Result<(), DriverError> {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.shutdown.store(true, Ordering::Relaxed);
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
        // Final WalkMemo stats drain: the pump is down
        // and no further cycle can advance an epoch, so drain the LAST
        // epoch's counters here — no boundary will. Inert unless the memo
        // recorded activity (a stop-before-resume run emits nothing); the
        // observation never fails the stop.
        self.stages.drain_walk_memo_final();
        *self.subscribe_state.lock() = None;
        self.stages.close_delivery_channels();
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
    pub fn is_pool_verified(&self, key: &str) -> bool {
        self.verifications.is_verified(key)
    }

    /// Run a V3 pool's core-owned registration verify lifecycle, AT MOST ONCE
    /// per live claim window — and never again once it has COMPLETED.
    ///
    /// The choreography is entered through the session's per-pool
    /// [`PoolVerifications`] owner, keyed by the pool identity. Concurrent
    /// callers — the async driver, a blocking seat-thread twin, or any `PyO3`
    /// caller of this same driver — share one run and each receive its
    /// outcome; a failed window is released so a later caller retries. A
    /// completed lifecycle records a durable verified-pool fact, so a
    /// subsequent registration of the same identity is a no-op rather than a
    /// re-run (the behavior the retired Python key cache provided).
    ///
    /// # Errors
    ///
    /// Propagates [`DriverError::Verify`] from the core lifecycle
    /// (`Mismatch` = fatal; `Rpc` = retryable; missing provider = fail-fast).
    pub async fn run_v3_registration_lifecycle(
        &self,
        address: Address,
        snapshot_block: Option<u64>,
    ) -> Result<(), DriverError> {
        let core = self.stages.core();
        let key = verify_claim_key("v3", &alloy::hex::encode_prefixed(address));
        if self.verifications.is_verified(&key) {
            return Ok(());
        }
        // An unregistered pool is a no-op `Ok` in the lifecycle; that must not
        // become a durable fact, or a later registration of this address would
        // skip the verify it still needs.
        if core
            .read_at(LockSite::Registration)
            .v3_pool_coverage(address)
            .is_none()
        {
            return Ok(());
        }
        let provider = self.verify_provider.lock().clone();
        self.verifications
            .run_exclusive(&key, || async move {
                let lifecycle_span = tracing::info_span!(
            "degenbot.pool.verify_lifecycle",
            pool.version = "v3",
            pool.address = %address,
        );
                let result = crate::bot_core::run_v3_registration_lifecycle(
                    &core,
                    provider.as_ref(),
                    address,
                    snapshot_block,
                )
                .instrument(lifecycle_span)
                .await;
                if result.is_ok() {
                    diag!(domain = pump, version = "v3", address = %address, "registration verify-lifecycle complete");
                } else {
                    op_warn!(domain = pump, version = "v3", address = %address, "registration verify-lifecycle FAILED");
                }
                result
            })
            .await
            .map_err(DriverError::Verify)
    }

    /// Run a V4 pool's core-owned registration verify lifecycle, at most once
    /// per live claim window — and never again once it has COMPLETED — the V4
    /// twin of [`Self::run_v3_registration_lifecycle`], keyed by the
    /// `(PoolManager, pool_id)` pair rather than an address, because one
    /// manager hosts many pools.
    ///
    /// # Errors
    ///
    /// Propagates [`DriverError::Verify`].
    pub async fn run_v4_registration_lifecycle(
        &self,
        pool_manager: Address,
        pool_id: V4PoolId,
        snapshot_block: Option<u64>,
    ) -> Result<(), DriverError> {
        let core = self.stages.core();
        let key = verify_claim_key(
            "v4",
            &format!(
                "{}:{}",
                alloy::hex::encode_prefixed(pool_manager),
                alloy::hex::encode_prefixed(pool_id)
            ),
        );
        if self.verifications.is_verified(&key) {
            return Ok(());
        }
        // An unregistered pool is a no-op `Ok` in the lifecycle; that must not
        // become a durable fact, or a later registration of this pair would
        // skip the verify it still needs.
        if core
            .read_at(LockSite::Registration)
            .v4_pool_coverage(pool_manager, &pool_id)
            .is_none()
        {
            return Ok(());
        }
        let provider = self.verify_provider.lock().clone();
        self.verifications
            .run_exclusive(&key, || async move {
                let lifecycle_span = tracing::info_span!(
                    "degenbot.pool.verify_lifecycle",
                    pool.version = "v4",
                    pool.manager = %pool_manager,
                    pool.id = %alloy::hex::encode_prefixed(pool_id),
                );
                let result = crate::bot_core::run_v4_registration_lifecycle(
                    &core,
                    provider.as_ref(),
                    pool_manager,
                    pool_id,
                    snapshot_block,
                )
                .instrument(lifecycle_span)
                .await;
                if result.is_ok() {
                    diag!(domain = pump, version = "v4", pool_id = %alloy::hex::encode_prefixed(pool_id), "registration verify-lifecycle complete");
                } else {
                    op_warn!(domain = pump, version = "v4", pool_id = %alloy::hex::encode_prefixed(pool_id), "registration verify-lifecycle FAILED");
                }
                result
            })
            .await
            .map_err(DriverError::Verify)
    }

    /// Blocking V3 registration lifecycle (the seat-thread twin).
    ///
    /// # Errors
    ///
    /// Propagates [`DriverError::Verify`].
    ///
    /// # Panics
    ///
    /// Panics if called from within a Tokio runtime.
    pub fn run_v3_registration_lifecycle_sync(
        &self,
        address: Address,
        snapshot_block: Option<u64>,
    ) -> Result<(), DriverError> {
        degenbot_core::runtime::get_runtime()
            .block_on(self.run_v3_registration_lifecycle(address, snapshot_block))
    }

    /// Blocking V4 registration lifecycle (the seat-thread twin).
    ///
    /// # Errors
    ///
    /// Propagates [`DriverError::Verify`].
    ///
    /// # Panics
    ///
    /// Panics if called from within a Tokio runtime.
    pub fn run_v4_registration_lifecycle_sync(
        &self,
        pool_manager: Address,
        pool_id: V4PoolId,
        snapshot_block: Option<u64>,
    ) -> Result<(), DriverError> {
        degenbot_core::runtime::get_runtime().block_on(self.run_v4_registration_lifecycle(
            pool_manager,
            pool_id,
            snapshot_block,
        ))
    }

    /// Run a V3 pool's registration verify lifecycle under the bounded retry
    /// dance.
    ///
    /// A transient [`VerifyError::Rpc`] / [`VerifyError::Provider`] failure
    /// releases the claim (the plain lifecycle owns that), so the next attempt
    /// re-runs the whole lifecycle; a [`VerifyError::Snapshot`] mismatch is
    /// fatal and is never retried. Returns the classified failure once the
    /// policy is exhausted.
    ///
    /// # Errors
    ///
    /// Returns the last transient [`VerifyError`] after exhausting `policy`,
    /// or the first fatal one.
    pub async fn run_v3_registration_lifecycle_with_retry(
        &self,
        address: Address,
        snapshot_block: Option<u64>,
        policy: &RetryPolicy,
    ) -> Result<(), VerifyError> {
        retry_verification_call(policy, |_attempt| async move {
            self.run_v3_registration_lifecycle(address, snapshot_block)
                .await
                .map_err(verify_error_from_driver)
        })
        .await
    }

    /// V4 twin of [`Self::run_v3_registration_lifecycle_with_retry`].
    ///
    /// # Errors
    ///
    /// As the V3 twin.
    pub async fn run_v4_registration_lifecycle_with_retry(
        &self,
        pool_manager: Address,
        pool_id: V4PoolId,
        snapshot_block: Option<u64>,
        policy: &RetryPolicy,
    ) -> Result<(), VerifyError> {
        retry_verification_call(policy, |_attempt| async move {
            self.run_v4_registration_lifecycle(pool_manager, pool_id, snapshot_block)
                .await
                .map_err(verify_error_from_driver)
        })
        .await
    }

    /// Blocking V3 with-retry twin (the seat-thread shape).
    ///
    /// # Errors
    ///
    /// As the async twin.
    ///
    /// # Panics
    ///
    /// Panics if called from within a Tokio runtime.
    pub fn run_v3_registration_lifecycle_with_retry_sync(
        &self,
        address: Address,
        snapshot_block: Option<u64>,
        policy: &RetryPolicy,
    ) -> Result<(), VerifyError> {
        degenbot_core::runtime::get_runtime().block_on(
            self.run_v3_registration_lifecycle_with_retry(address, snapshot_block, policy),
        )
    }

    /// Blocking V4 with-retry twin (the seat-thread shape).
    ///
    /// # Errors
    ///
    /// As the async twin.
    ///
    /// # Panics
    ///
    /// Panics if called from within a Tokio runtime.
    pub fn run_v4_registration_lifecycle_with_retry_sync(
        &self,
        pool_manager: Address,
        pool_id: V4PoolId,
        snapshot_block: Option<u64>,
        policy: &RetryPolicy,
    ) -> Result<(), VerifyError> {
        degenbot_core::runtime::get_runtime().block_on(
            self.run_v4_registration_lifecycle_with_retry(
                pool_manager,
                pool_id,
                snapshot_block,
                policy,
            ),
        )
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
fn verify_claim_key(family: &str, identity: &str) -> String {
    format!("{family}:{identity}")
}

#[expect(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot_core::PoolTickCoverage;
    use degenbot_eventhub::OverflowPolicy;
    use futures_util::StreamExt;

    /// Build an offline driver over a fresh `Bot` + `EngineStages`.
    fn driver_for_test() -> EngineDriver {
        let bot = Arc::new(Bot::new(1));
        let stages = Arc::new(EngineStages::with_core(bot.state_arc(), bot.active_delta()));
        EngineDriver::from_stages(bot, stages)
    }

    /// An alloy mock-transport provider (never hit on the no-RPC test paths).
    fn mock_provider() -> Arc<AlloyProvider> {
        use alloy::network::Ethereum as NetEth;
        use alloy::providers::{Provider, ProviderBuilder};
        use alloy::rpc::client::ClientBuilder;
        use alloy::transports::mock::{Asserter, MockTransport};
        let asserter = Asserter::new();
        let client = ClientBuilder::default().transport(MockTransport::new(asserter), true);
        let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
        Arc::new(AlloyProvider::from_provider(
            Arc::new(dyn_provider) as Arc<dyn Provider<NetEth>>
        ))
    }

    #[test]
    fn new_driver_starts_in_created_phase() {
        let driver = driver_for_test();
        assert_eq!(driver.current_phase(), PumpPhase::Created);
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
                assert_eq!(p.current, PumpPhase::Created);
                assert_eq!(p.required, Some(PumpPhase::SnapshotLoaded));
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
        driver.set_phase(PumpPhase::Resumed);
        let err = degenbot_core::runtime::get_runtime()
            .block_on(driver.subscribe("ws://127.0.0.1:1"))
            .unwrap_err();
        assert!(matches!(err, DriverError::Phase(_)), "got {err:?}");
    }

    #[test]
    fn resume_without_pending_subscribe_state_is_rejected() {
        let driver = driver_for_test();
        let _result_rx = driver.take_result_receiver();
        driver.set_phase(PumpPhase::SnapshotLoaded);
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
        driver.set_phase(PumpPhase::SnapshotLoaded);

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

    #[test]
    fn stop_is_idempotent_and_latches_terminal() {
        let driver = driver_for_test();
        driver.stop().expect("first stop");
        assert!(driver.is_stopped());
        driver.stop().expect("second stop is a no-op");
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
            struct Message(String);
            impl tracing::field::Visit for Message {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    if field.name() == "message" {
                        self.0 = format!("{value:?}");
                    }
                }
            }
            let mut message = Message(String::new());
            event.record(&mut message);
            self.events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((*event.metadata().level(), message.0));
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
        driver.set_phase(PumpPhase::SnapshotLoaded);
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
        // the point is that `resume` drives the pump, not the consumer.
        driver.install_subscribe_state_for_test(DriverSubscribeState {
            pump,
            first_block: 100,
            combined_stream: futures_util::stream::empty().boxed(),
        });
        driver.set_phase(PumpPhase::SnapshotLoaded);
        degenbot_core::runtime::get_runtime()
            .block_on(driver.resume())
            .expect("resume with a pending subscribe state");
        assert_eq!(driver.current_phase(), PumpPhase::Resumed);
        assert!(driver.pump_handle_armed());
        // A second resume is an already-resumed session-state error.
        let err = degenbot_core::runtime::get_runtime()
            .block_on(driver.resume())
            .unwrap_err();
        assert!(matches!(err, DriverError::SessionState(_)), "got {err:?}");
        driver.stop().expect("stop");
        assert!(!driver.pump_handle_armed());
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
        let classified = verify_error_from_driver(DriverError::NoResultReceiver);
        assert!(
            matches!(&classified, VerifyError::Other(_)),
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
        use crate::bot_core::{PoolTickCoverage, RegisterV3PoolParams, TickInfo};
        use alloy::primitives::{U128, U256};
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
        use crate::bot_core::{PoolTickCoverage, RegisterV4PoolParams};
        use alloy::primitives::U256;
        use degenbot_pools::v4_state::V4PoolKey;
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
        let key = verify_claim_key("v3", &alloy::hex::encode_prefixed(address));

        driver
            .run_v3_registration_lifecycle(address, None)
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
                .unregister_pool(address, None);
        }
        register_v3(&driver, address, PoolTickCoverage::Tracked);

        driver
            .run_v3_registration_lifecycle(address, None)
            .await
            .expect("a verified pool is not re-verified");
        assert_eq!(driver.verified_pool_count(), 1);
    }

    /// A failed lifecycle records no fact, so a later driver call retries it
    /// (here: a Tracked pool with no provider fails both times).
    #[tokio::test]
    async fn failed_driver_lifecycle_is_retriable_and_not_verified() {
        use crate::bot_core::{PoolTickCoverage, RegisterV3PoolParams};
        use alloy::primitives::U256;
        use hashbrown::HashMap;

        let driver = driver_for_test();
        let address = Address::from([0x12u8; 20]);
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            crate::bot_core::TickInfo {
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
        let key = verify_claim_key("v3", &alloy::hex::encode_prefixed(address));

        for _ in 0..2 {
            assert!(
                driver
                    .run_v3_registration_lifecycle(address, None)
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

        let v3_key = verify_claim_key("v3", &alloy::hex::encode_prefixed(shared_address));
        let v4_key = verify_claim_key(
            "v4",
            &format!(
                "{}:{}",
                alloy::hex::encode_prefixed(shared_address),
                alloy::hex::encode_prefixed(sibling_id)
            ),
        );

        driver
            .run_v3_registration_lifecycle(shared_address, None)
            .await
            .expect("V3 lifecycle");
        assert!(driver.is_pool_verified(&v3_key));
        assert!(
            !driver.is_pool_verified(&v4_key),
            "a V3 fact is not a V4 fact at the same address"
        );

        driver
            .run_v4_registration_lifecycle(shared_address, sibling_id, None)
            .await
            .expect("V4 lifecycle");
        assert!(driver.is_pool_verified(&v4_key));
        assert_eq!(driver.verified_pool_count(), 2);
    }
}
