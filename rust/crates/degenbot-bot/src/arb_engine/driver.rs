//! `EngineDriver` — the public Rust driver seam over the crate-private
//! engine (ADR-050, Gap G1 / ergo `5XOGRK`).
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
//! This is **not** the "engine facade" CONTEXT.md forbids (ADR-049 D1): the
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
//! 2. `resume()` gates on `EnginePhase::SnapshotLoaded`, **owns the
//!    `S+1..W` auto-backfill** (awaiting `BlockPump::backfill_with_drain`
//!    synchronously), then spawns the live pump loop and advances to
//!    `EnginePhase::Resumed`. Consumers never call `backfill_from_snapshot`.
//! 3. `stop()` sets the shutdown flag, aborts and joins the pump task, clears
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
//!   consume/dispatch loop (ADR-050 D9 maps that to `L4E7RI`).

use crate::arb_engine::lifecycle::PathRegistrationError;
use crate::arb_engine::path_info::PathInfoBuildError;
use crate::arb_engine::{
    BlockNotification, EnginePhase, EngineRetune, EngineStages, InlineSimulator, ResultBatch,
};
use crate::bot_core::block_pump::{BlockPump, SubscribeState};
use crate::bot_core::registration_lifecycle::RegistrationLifecycleError;
use crate::bot_core::reorg_coordinator::ReorgCoordinator;
use crate::bot_core::state_lock::{LockSite, StateLock};
use crate::bot_core::{Bot, BotState, PumpControl, StageHandlers};
use alloy::primitives::Address;
use degenbot_core::{diag, op_error, op_info, op_warn};
use degenbot_decoders::v4_swap_decoder::V4PoolId;
use degenbot_executor::composers::PathInfo;
use degenbot_ingestion::IngestEvent as WsEvent;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_solvers::mixed::{PoolHop, SolvePathResult};
use hashbrown::HashMap;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, watch};
use tracing::Instrument as _;

/// A typed engine-phase violation raised at the driver boundary.
///
/// `EnginePhase::require` / `allow_subscribe` return a `String` for the
/// engine-internal callers; the driver wraps that in this typed value so a
/// consumer matches a variant instead of a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhaseError {
    /// The rejected driver method (`"subscribe"`, `"resume"`, …).
    pub method: &'static str,
    /// The engine phase at call time.
    pub current: EnginePhase,
    /// The phase boundary the method required (or `None` for the
    /// subscribe-window gate, which admits either `Created` or
    /// `SnapshotLoaded` — not a single boundary).
    pub required: Option<EnginePhase>,
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
    /// The result-batch receiver (handed out once via `take_result_receiver`).
    result_rx: Mutex<Option<mpsc::UnboundedReceiver<ResultBatch>>>,
    /// The block-clock receiver (handed out once via `take_block_receiver`).
    block_rx: Mutex<Option<mpsc::UnboundedReceiver<BlockNotification>>>,
    /// The terminal stopped latch (ADR-050 D5) — `EnginePhase` cannot express
    /// teardown, so the driver owns it.
    stopped: AtomicBool,
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
    /// wrapper path). The driver creates + installs the result/block channels
    /// and the reorg coordinator, then composes.
    #[must_use]
    pub fn from_stages(bot: Arc<Bot>, stages: Arc<EngineStages>) -> Self {
        let (result_tx, result_rx) = mpsc::unbounded_channel();
        stages.set_result_channel(result_tx);
        let (block_tx, block_rx) = mpsc::unbounded_channel();
        stages.set_block_channel(block_tx);
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
            result_rx: Mutex::new(Some(result_rx)),
            block_rx: Mutex::new(Some(block_rx)),
            stopped: AtomicBool::new(false),
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
    pub fn current_phase(&self) -> EnginePhase {
        self.stages.current_phase()
    }

    /// Advance the engine phase with no ordering check (callers validate).
    pub fn set_phase(&self, phase: EnginePhase) {
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
    pub async fn wait_pump_finished(&self) {
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

    /// Take the result-batch receiver — once only, **before** `resume()`.
    #[must_use]
    pub fn take_result_receiver(&self) -> Option<mpsc::UnboundedReceiver<ResultBatch>> {
        self.result_rx.lock().take()
    }

    /// Take the block-clock receiver — once only.
    #[must_use]
    pub fn take_block_receiver(&self) -> Option<mpsc::UnboundedReceiver<BlockNotification>> {
        self.block_rx.lock().take()
    }

    /// The snapshot seed block `S` (JUCFCB) — read from the shared core.
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
        // J3FMDO).
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
            .set_phase(EnginePhase::after_subscribe(phase, core_has_snapshot));
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
    /// - [`DriverError::Resume`] when the pending state carries no WS stream.
    pub async fn resume(&self) -> Result<(), DriverError> {
        if self.is_stopped() {
            return Err(DriverError::SessionState(
                "Cannot resume: driver has been stopped.".to_string(),
            ));
        }
        let phase = self.stages.current_phase();
        if let Err(detail) = phase.require(EnginePhase::SnapshotLoaded, "resume") {
            return Err(DriverError::Phase(PhaseError {
                method: "resume",
                current: phase,
                required: Some(EnginePhase::SnapshotLoaded),
                detail,
            }));
        }
        if phase == EnginePhase::Resumed {
            return Err(DriverError::SessionState(
                "Cannot resume: engine is already in Resumed phase.".to_string(),
            ));
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
        // J3FMDO: the backfill is SYNCHRONOUS with respect to `resume` so the
        // consumer's registration draining the per-pool backfill buffer cannot
        // race it. DFQYM5: `backfill_with_drain` also re-injects live events
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
        self.stages.set_phase(EnginePhase::Resumed);
        Ok(())
    }

    /// Stop the pump and latch the driver terminally stopped (ADR-050 D6).
    ///
    /// Any-phase + idempotent. Sets the shutdown flag, aborts and joins the
    /// pump task (so the WS subscription futures drop before return), clears
    /// the subscribe state, and closes the delivery channels so a pending
    /// receiver observes end-of-stream exactly once.
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
        if let Some(handle) = handle {
            handle.abort();
            // Drive the cancelled task to completion so its held resources
            // drop before return. `block_on` on the shared runtime matches
            // `subscribe`/`resume`'s sync discipline; the aborted task
            // completes promptly.
            let _ = degenbot_core::runtime::get_runtime().block_on(handle);
            op_info!(domain = pump, "EngineDriver: BlockPump task aborted");
        } else {
            op_info!(
                domain = pump,
                "EngineDriver: BlockPump not running (no pump handle to abort)"
            );
        }
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

    /// Run a V3 pool's core-owned registration verify lifecycle.
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
        let provider = self.verify_provider.lock().clone();
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
        result.map_err(DriverError::Verify)
    }

    /// Run a V4 pool's core-owned registration verify lifecycle.
    ///
    /// # Errors
    ///
    /// Propagates [`DriverError::Verify`] (V4 twin of the V3 lifecycle).
    pub async fn run_v4_registration_lifecycle(
        &self,
        pool_manager: Address,
        pool_id: V4PoolId,
        snapshot_block: Option<u64>,
    ) -> Result<(), DriverError> {
        let state_view = *self.verify_state_view.lock();
        let core = self.stages.core();
        let provider = self.verify_provider.lock().clone();
        let lifecycle_span = tracing::info_span!(
            "degenbot.pool.verify_lifecycle",
            pool.version = "v4",
            pool.manager = %pool_manager,
            pool.id = %degenbot_core::hex_utils::encode_hex(&pool_id),
        );
        let result = crate::bot_core::run_v4_registration_lifecycle(
            &core,
            provider.as_ref(),
            pool_manager,
            pool_id,
            state_view,
            snapshot_block,
        )
        .instrument(lifecycle_span)
        .await;
        if result.is_ok() {
            diag!(domain = pump, version = "v4", pool_id = %degenbot_core::hex_utils::encode_hex(&pool_id), "registration verify-lifecycle complete");
        } else {
            op_warn!(domain = pump, version = "v4", pool_id = %degenbot_core::hex_utils::encode_hex(&pool_id), "registration verify-lifecycle FAILED");
        }
        result.map_err(DriverError::Verify)
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

    /// The last solved results + block (DELEGATION, RAYPAR snapshot).
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

/// Soak-2026-08-22 forensics (relocated with the C5 `PumpState` dissolution):
/// name teardown paths that bypass [`EngineDriver::stop`]. If the pump task
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

#[expect(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(driver.current_phase(), EnginePhase::Created);
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
                assert_eq!(p.current, EnginePhase::Created);
                assert_eq!(p.required, Some(EnginePhase::SnapshotLoaded));
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
        driver.set_phase(EnginePhase::Resumed);
        let err = degenbot_core::runtime::get_runtime()
            .block_on(driver.subscribe("ws://127.0.0.1:1"))
            .unwrap_err();
        assert!(matches!(err, DriverError::Phase(_)), "got {err:?}");
    }

    #[test]
    fn resume_without_pending_subscribe_state_is_rejected() {
        let driver = driver_for_test();
        driver.set_phase(EnginePhase::SnapshotLoaded);
        let err = degenbot_core::runtime::get_runtime()
            .block_on(driver.resume())
            .unwrap_err();
        assert!(matches!(err, DriverError::SessionState(_)), "got {err:?}");
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

    #[test]
    fn resume_owns_the_backfill_and_spawns_the_live_loop() {
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
        // S stays None (cold start) so the driver's owned backfill is a no-op;
        // the point is that `resume` drives the pump, not the consumer.
        driver.install_subscribe_state_for_test(DriverSubscribeState {
            pump,
            first_block: 100,
            combined_stream: futures_util::stream::empty().boxed(),
        });
        driver.set_phase(EnginePhase::SnapshotLoaded);
        degenbot_core::runtime::get_runtime()
            .block_on(driver.resume())
            .expect("resume with a pending subscribe state");
        assert_eq!(driver.current_phase(), EnginePhase::Resumed);
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
}
