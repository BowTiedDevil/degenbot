//! `BlockPump` — `Bot`'s WS transport + drain loop (ADR-006 D4), now the
//! thin driver of the unified stage machine (epic MROOY7, 7NFYQW + SZJUKL).
//!
//! Holds `Arc<Bot>` + the two ADR-046 engine seams (`Arc<dyn StageHandlers>`
//! stage hooks, `Arc<dyn PumpControl>` driver pokes). Per WS
//! log, the pump calls `bot.dispatch_log(log)` (decode → apply to `BotState`
//! → `EpochDelta` byproduct; the retired `EngineSubscriber` classification is
//! GONE — touched-pool tracking is the ledger's job since LXDY4C). At the
//! machine's decision points the pump drives the engine's stage hooks
//! directly (`on_resolve` → `on_solve`, `on_publish` at the Published edge,
//! `on_finalize` at the tombstone) — the drain FIFO/dispatch-owner
//! indirection (`DispatchOwner`/`DrainWork`) is deleted: work executes
//! INLINE in this driver, so the drainer liveness machinery
//! (`DrainerHealth`/`StallWatch`) has no separate task left to police; a
//! wedged driver IS a header-staleness stall the machine's watchdogs abort
//! on (`StageMachine::watchdog_phase`).
//!
//! The stale-epoch drop the FIFO needed (7NFYQW I3: pre-rewind items must
//! not consume `epoch.block()`) survives as the driver-side
//! `reorg_flying_stale` check at each work site — same WARN + metric,
//! no queue to check.
//!
//! `apply_log` routes ALL log application through `Bot::dispatch_log`.
//!
//! (Epic MROOY7, 5WTYYQ) The WS transport moved OUT of this module into the
//! pyo3-free `degenbot-ingestion` crate: the dual `newHeads` + `logs`
//! subscriptions, the MJXP5Z one-stream handshake, Rust-side topic filtering
//! ([`degenbot_ingestion::RELEVANT_TOPICS`]), gap-backfill `eth_getLogs`
//! fetching, and the header/logs watchdog windows all live there now. This
//! driver consumes the crate's `IngestEvent` stream (header + `PoolEvent`
//! emission) and owns ONLY the runtime half: the `StageMachine` + stage
//! driving + log application + debounce. The `PyO3` layer is just another sink
//! at the Published edge.
//!
//! # Two-Phase Lifecycle
//!
//! 1. **Subscribe phase** (`subscribe()`): Opens WS subscriptions (newHeads +
//!    unfiltered logs) and observes until the first *complete* block — both the
//!    header and a log for block N. N is returned as the backfill boundary W.
//!    No events are buffered during subscribe — backfill is the sole authority
//!    for blocks S+1..W (inclusive); the pump (resume) is sole authority for
//!    W+1 onward (it drops any WS log for block ≤ W — the boundary backfill
//!    already applied W's logs).
//!
//! 2. **Resume phase** (`resume_from_subscribe()`): Begins normal processing —
//!    logs applied eagerly, solved + sent on block boundaries / debounce.
//!
//! **Critical ordering**: backfill must run AFTER `subscribe()` returns but
//! BEFORE `resume_from_subscribe()`. The engine's
//! `last_processed_block()` is the backfill-start boundary. (Pre-epic-P73ER6
//! Python orchestrated this manually; the epic relocates backfill into the
//! core, driven automatically by `resume`.)

use degenbot_core::{op_error, op_info, op_warn};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

// LW-T5 (Seam E): the pump feeds the cgroup throttle sample straight
// through the Executor seam (block_pump no longer reaches into the
// engine's internal solve-executor module).
use crate::bot_core::stage_machine::QuiesceParams;
use crate::bot_core::stance;
use crate::bot_core::{CompletenessDecision, StageDecision, StageMachine};

use alloy::primitives::B256;
use alloy::rpc::types::Log;
// (5WTYYQ) The event/fetch surface of the ingestion crate. `PoolEvent` +
// `build_backfill_filter` are imported by the test module below.
use degenbot_ingestion::{
    IngestEvent as WsEvent, Watchdog, WsIngestor, BACKFILL_TIMEOUT_SECS,
    DEFAULT_BACKFILL_CHUNK_SIZE, RELEVANT_TOPICS,
};
use degenbot_workers::posture::ThrottleSample;
use futures_util::{stream, StreamExt};
use tokio::time::timeout;
use tracing::Instrument;

use crate::bot_core::LogDecision;
use crate::bot_core::{
    stage_handlers::{Finalize, GateOutcome, Publish, Resolve, Solve},
    BlockMetadata, Bot, Epoch, PumpControl, StageHandlers,
};
// (the topic-import list, the backfill/idle + handshake constants, and the
// header/log watchdog windows all live in degenbot-ingestion now — 5WTYYQ.)

// KAHU5W: the debounce / early-slice defaults moved into the typed schema
// (pump.pump_debounce_ms = 50, pump.early_slice_ms = 25; the loader validates
// debounce > 0). The fail-open parse helpers disappeared with them.

/// Microseconds -> seconds with a 32-bit guard (the cast lint is the point —
/// overflow callers get a saturated bucket, never a precision-lost value).
fn us_to_secs(us: u64) -> f64 {
    f64::from(u32::try_from(us).unwrap_or(u32::MAX)) / 1_000_000.0
}

/// LW-T5 (Seam E): wall-clock anchor for the header throttle-sample
/// cadence — the pump owns the sample interval. The FSM needs each
/// sample's poll interval (elapsed), and the pump poller samples on
/// header cadence, so the delta accounting lives here with the sampler.
static LAST_HEADER_SAMPLE_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Feed ONE per-header cgroup throttle sample to the ONE process-level
/// fleet posture owner (JCI2FW Part A — the Executor seam's
/// `observe_throttle` channel is dissolved; the pump feeds
/// `degenbot_workers::posture::process()` directly and every fleet host
/// consults that same owner). The pump owns the header sample cadence:
/// `elapsed_usec` is the time since the previous sample (0 for the first
/// sample, the `last_ms == 0` sentinel). Always fed.
fn feed_executor_throttle_sample(now_ms: u64, events: u64, throttled_usec: u64) {
    let last_ms = LAST_HEADER_SAMPLE_MS.swap(now_ms, Ordering::Relaxed);
    let elapsed_usec = if last_ms == 0 {
        0
    } else {
        now_ms.saturating_sub(last_ms).saturating_mul(1_000)
    };
    // Feeder-site contract (T3/T9): the wrapper feeds the owner AND wakes
    // the fleet hosts on a real (non-`Held`) transition — never a raw
    // `observe_throttle` call.
    crate::arb_engine::fleet_wake::feed_throttle(
        now_ms,
        ThrottleSample {
            events,
            throttled_usec,
            elapsed_usec,
        },
    );
}

/// Wall-clock milliseconds since the Unix epoch. The epoch-race anchor
/// clock: header accept (`header_ms`) and Published dispatch
/// (`drive_publish`) are both stamped here so the race can be diffed in a
/// method that does not own the run loop's `tick_epoch` local.
fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Milliseconds -> seconds with a 32-bit guard (the `event_dispatch`
/// `ms_to_secs` helper, kept for the header→publish epoch-race histogram
/// the dissolved `DispatchOwner` header→solved stamp became).
fn ms_to_secs(ms: u64) -> f64 {
    f64::from(u32::try_from(ms).unwrap_or(u32::MAX)) / 1_000.0
}

/// Per-header pre-solve gap marks, tracked by the pump loop (the
/// `pregap` local): marks the header accept, the first relevant log,
/// and the last relevant log; the settle point compares the settle
/// decision time against `last_log` to decompose the block-to-solve gap.
struct PreSolveGapTrack {
    header_at: std::time::Instant,
    first_log: Option<std::time::Instant>,
    last_log: Option<std::time::Instant>,
    logs: u64,
}

/// The unified pump that drives `Bot`'s drain sink.
///
/// Supports a two-phase lifecycle:
/// 1. `subscribe()` — opens WS connections, observes until first complete
///    block (header + log for same block), returns that block number
/// 2. `resume()` — begins normal processing on block boundaries
pub struct BlockPump {
    /// The per-chain orchestrator — owns `BotState` + the `LogDispatcher`. Per
    /// WS log, the pump calls `bot.dispatch_log(log)` (forward) or
    /// `reorg_coordinator.dispatch_reorg_log(log)` (`removed: true`).
    /// ADR-006 D4 + slice 7.
    bot: Arc<Bot>,
    /// The engine's stage surface (SZJUKL: the ONE seam — the dissolved
    /// `SolveCoordinator`/`DrainSink` fan-out collapsed onto the arb
    /// engine's `StageHandlers` implementation; no `drain_lock`, no FIFO).
    engine: Arc<dyn StageHandlers>,
    /// ADR-046: the driver-facing control seam — the seven pokes split off
    /// `StageHandlers` so that trait carries only the eight pure stage
    /// hooks. Injected beside `engine` at construction.
    control: Arc<dyn PumpControl>,
    /// The per-event reorg coordinator (slice 7). Owned by the pump (not
    /// routed through the engine seam — reorg is a `Bot` concern, parallel
    /// to `dispatch_log`).
    reorg_coordinator: Arc<crate::bot_core::reorg_coordinator::ReorgCoordinator>,
    /// (5WTYYQ) The WS transport handle — subscriptions + handshake +
    /// gap-backfill fetching live in `degenbot-ingestion`; this driver only
    /// consumes the emitted `IngestEvent` stream and calls the fetch API.
    ingestor: WsIngestor,
    /// Shutdown flag — set by `stop()` or by a too-deep reorg (graceful exit)
    shutdown: Arc<AtomicBool>,
    /// SONJQA: max age of a held stage-span interval before the pump
    /// force-closes it (with a stall warning) — the G3 stall lesson, see
    /// `STAGE_MAX_AGE_SECS` / `stage_telemetry`. Default 5s; the tick
    /// granularity is the 500ms timed-exit interval. Tests may set the field
    /// directly (`pump_for_test` construction + assignment) - no env race.
    stage_max_age: Duration,
    /// (5WTYYQ) The watchdog windows + silence-alarm accounting (owned by
    /// `degenbot-ingestion::Watchdog`). The tokio intervals stay in the
    /// driver's select (the FSM decides, the driver executes — ADR-008).
    watchdog: Watchdog,
    /// Wall-clock ms of the last accepted header — the anchor the driver
    /// measures `header_to_publish` (the ADR-041 epoch race) against
    /// (succeeds the dissolved `DispatchOwner`'s T2 header→solved anchor;
    /// single-writer: the pump task).
    header_ms: std::sync::atomic::AtomicU64,
    /// Whether the per-block WS-delivery completeness cross-check runs
    /// (`assert_ws_block_complete` — aborts on any relevant-topic log that
    /// `eth_getLogs` has but the live websocket dropped). Conservative default
    /// ON (`DEGENBOT_WS_COMPLETENESS`, via `bot_env_flag_default_on`): set
    /// `=0` to disable. Held as a field (not a global env read) so tests
    /// deterministically opt out per-pump (Z4KQXF pattern). When OFF the
    /// `ws_delivered` index-tracking map is not populated
    /// (no work on the hot loop).
    ws_completeness_enabled: bool,
    /// Early-slice window (ms) for the drained-settle gate (PWPPAZ T2): when
    /// nonzero and unsolved dirt has been observed this long in the current
    /// block window, the gate dispatches ONE bounded early Drain mid-burst
    /// instead of waiting for burst quiesce — the designed replacement for
    /// the retired finalize steal (J2X3LZ). `0` disables the slice (exact
    /// pre-T2 gate behavior). One slice per block window (reset at each
    /// accepted header and at each settle dispatch) keeps MBNASQ's unbounded
    /// per-gap serial solves from returning.
    early_slice_ms: u64,
    /// BM35LK: the quiesce-estimator parameters the FSM's adaptive
    /// trailing window arms from (a snapshot of the `pump.quiesce_*`
    /// schema keys; `fixed` + `pump_debounce_ms` is today's behavior).
    /// Held on the pump (not read from the ambient stance inside the loop)
    /// so tests stay immune to the global environment — the same per-pump
    /// pattern `early_slice_ms` established. The FSM's fixed-mode window
    /// now carries the live debounce value (`fixed_ms`; the retired
    /// `debounce_ms` field was its last read side).
    quiesce_params: QuiesceParams,
}

/// State held between `subscribe()` and `resume()` calls.
///
/// Contains the live WS subscriptions and the first observed block number.
/// Created by `subscribe()`, consumed by `resume()`.
pub struct SubscribeState {
    /// The first block number observed during subscribe.
    /// Python uses this as the backfill target.
    pub first_block: u64,
    /// Block timestamp from first observed block.
    pub first_timestamp: u64,
    /// The merged stream of WS events (block headers + logs).
    /// `None` after `resume()` consumes it.
    pub combined_stream: Option<stream::BoxStream<'static, WsEvent>>,
}

impl BlockPump {
    /// Subscribe phase: open WS connections and observe until first complete block.
    ///
    /// Returns a `SubscribeState` containing the first observed block number
    /// and the live WS stream. Python should:
    /// 1. Run backfill up to `subscribe_state.first_block`
    /// 2. Call `resume(subscribe_state)` to begin normal processing
    ///
    /// During this phase, no events are buffered. The backfill is the sole
    /// authority for blocks S+1..W (inclusive). The subscribe phase only
    /// observes until
    /// both a newHeads notification and a log for the same block arrive,
    /// confirming the logs subscription is live and caught up.
    #[expect(clippy::missing_errors_doc)]
    pub async fn subscribe(
        rpc_url: &str,
        bot: Arc<Bot>,
        engine: Arc<dyn StageHandlers>,
        control: Arc<dyn PumpControl>,
        reorg_coordinator: Arc<crate::bot_core::reorg_coordinator::ReorgCoordinator>,
        shutdown: Arc<AtomicBool>,
    ) -> Result<(Self, SubscribeState), String> {
        // (5WTYYQ) Transport connect + subscribe + MJXP5Z handshake all live in
        // degenbot-ingestion; the driver receives the fused, re-injected
        // `IngestEvent` stream + keeps the handle for gap-backfill fetching.
        let ingestor = WsIngestor::connect(rpc_url).await?;

        let pump = Self {
            bot,
            engine,
            control,
            reorg_coordinator,
            ingestor,
            shutdown: Arc::clone(&shutdown),
            watchdog: Watchdog::new(),
            stage_max_age: Duration::from_secs(super::stage_telemetry::STAGE_MAX_AGE_SECS),
            ws_completeness_enabled: stance::config().pump.ws_completeness,
            header_ms: std::sync::atomic::AtomicU64::new(0),
            early_slice_ms: stance::config().pump.early_slice_ms,
            quiesce_params: QuiesceParams::from_schema(
                stance::config(),
                stance::config().pump.pump_debounce_ms,
            ),
        };
        // KAHU5W: the dispatcher-side strict decode-miss fault follows the
        // pump's completeness stance (respecting any per-pump opt-out).
        pump.bot
            .dispatcher()
            .set_strict_decode_fault(pump.ws_completeness_enabled);

        // MJXP5Z (Alternative B): single-stream handshake - NO resubscribe.
        // The ingestion handshake hands the SAME merged stream onward,
        // re-injecting any logs consumed during header-only polling. One WS,
        // one handoff.
        let boundary = pump
            .ingestor
            .subscribe_with_handshake(Arc::clone(&shutdown))
            .await?;
        Ok((
            pump,
            SubscribeState {
                first_block: boundary.first_block,
                first_timestamp: boundary.first_timestamp,
                combined_stream: Some(boundary.stream),
            },
        ))
    }

    /// Resume the pump from a subscribe state — auto-backfilling the
    /// snapshot→WS gap (J3FMDO) before the live loop begins.
    ///
    /// When the core `BotState` carries a snapshot seed `S` (set by
    /// `Bot::load_snapshot_from_db` or `load_*_from_py`) strictly less than
    /// the first observed WS block `W`, this method first awaits
    /// [`backfill_from_snapshot`](Self::backfill_from_snapshot) with the
    /// pump's own provider — applying `S+1..W` (inclusive) log state under
    /// `BotState::process_backfill_logs` with zero result batches. The Python
    /// `engine_registry.start()` no longer calls the pyo3
    /// `backfill_from_snapshot`; one Python `resume()` invocation drives both.
    ///
    /// When `S` is `None` (cold start) or `S >= W` (snapshot already at/after
    /// the live head), the backfill step is skipped — the live loop anchors
    /// on `W` directly.
    ///
    /// # Panics
    ///
    /// Panics if `subscribe_state.combined_stream` is `None` (i.e., `subscribe()`
    /// was not called first).
    pub async fn resume_from_subscribe(&mut self, subscribe_state: SubscribeState) {
        #[expect(clippy::expect_used)] // invariant-guarded (documented)
        let combined = subscribe_state
            .combined_stream
            .expect("resume() called without WS stream — did you call subscribe() first?");
        let first_block = subscribe_state.first_block;
        let (backfill_res, combined) = self.backfill_with_drain(first_block, combined).await;
        if let Err(e) = backfill_res {
            op_error!(domain = pump, first_block,
                %e,
                "BlockPump: auto-backfill failed — starting live loop from gap (not closed)"
            );
        }
        self.run_with_stream(combined, first_block).await;
    }

    /// DFQYM5/WS-DROP: run the snapshot→WS gap backfill while concurrently
    /// draining `combined`, returning `(backfill_result, combined')` where
    /// `combined'` re-injects every event drained during the backfill ahead
    /// of the still-owned live tail, preserving arrival order (MJXP5Z).
    ///
    /// Why the drain is not optional: the alloy `logs` subscription buffers
    /// into a small broadcast channel (default capacity 16) that DROPS the
    /// OLDEST messages for a lagging receiver. A backfill that awaits without
    /// polling `combined` therefore loses the freshly-mined live blocks' logs
    /// permanently — the first live block then shows most of its logs missing
    /// and immediately trips the WS-completeness abort (observed live:
    /// `eth_getLogs=44 logs, WS delivered=0` at block 25800995). Both
    /// consumers of the synchronous backfill — the core
    /// [`resume_from_subscribe`](Self::resume_from_subscribe) AND the pyo3
    /// `PumpState::resume` (which must `block_on` the backfill before
    /// returning so Python's `build_paths` cannot race the per-pool buffer,
    /// J3FMDO) — MUST go through this helper so the drain discipline has a
    /// single owner.
    pub async fn backfill_with_drain(
        &self,
        first_block: u64,
        combined: stream::BoxStream<'static, WsEvent>,
    ) -> (Result<u64, String>, stream::BoxStream<'static, WsEvent>) {
        let mut combined = combined;
        let (backfill_res, drained) = self
            .drain_stream_during_backfill(first_block, &mut combined)
            .await;
        let combined = if drained.is_empty() {
            combined
        } else {
            stream::iter(drained).chain(combined).boxed()
        };
        (backfill_res, combined)
    }

    /// Concurrently drain the live WS stream while the blocking snapshot→WS
    /// gap backfill runs, returning `(backfill_result, drained_events)`.
    ///
    /// Rationale/member-fn boundary: isolating the `&self`-borrowing backfill
    /// future inside this method lets its borrow end on return, so the caller
    /// can then re-borrow `&mut self` for the live loop (see caller). See
    /// [`resume_from_subscribe`](Self::resume_from_subscribe) for the
    /// broadcast-overflow root cause this drains around.
    async fn drain_stream_during_backfill(
        &self,
        first_block: u64,
        combined: &mut stream::BoxStream<'static, WsEvent>,
    ) -> (Result<u64, String>, Vec<WsEvent>) {
        let mut drained: Vec<WsEvent> = Vec::new();
        let backfill = self.backfill_to_ws_block(first_block);
        tokio::pin!(backfill);
        loop {
            tokio::select! {
                biased;
                res = &mut backfill => return (res, drained),
                ev = combined.next() => {
                    if let Some(ev) = ev {
                        drained.push(ev);
                    } else {
                        op_warn!(domain = pump, "BlockPump: WS stream ended during backfill (no re-inject gap)"
                        );
                        return (Ok(0), drained);
                    }
                },
            }
        }
    }

    /// Close the snapshot→WS gap by buffering `eth_getLogs(S+1..W)` (inclusive)
    /// into the
    /// core `BotState`'s per-pool backfill buffer (no solve, no `on_send`).
    ///
    /// This is the SYNCHRONOUSLY-awaitable half of `resume_from_subscribe` —
    /// `PumpState::resume` `block_on`s it BEFORE spawning the live loop so
    /// Python's `build_paths` (which drains the per-pool backfill buffer via
    /// `apply_backfill_buffer_v3`) cannot race the backfill. Pre-fix the
    /// backfill ran inside the spawned `resume_from_subscribe` task and
    /// `resume` returned immediately, so an active pool's burn was not yet
    /// buffered when `build_paths` drained → `VerificationMismatchError` at
    /// post-drain verify (2026-07-12 settlement-arbitrage crash).
    ///
    /// No-op when `S` is unset (cold start), `S >= W` (catch-up snapshot), or
    /// `S == 0`. Errors log + return (the live loop still starts from `W`).
    ///
    /// # Errors
    ///
    /// Returns `Err(String)` if a chunk's `eth_getLogs` call fails (message
    /// includes the offending block range + provider error).
    pub async fn backfill_to_ws_block(&self, ws_block: u64) -> Result<u64, String> {
        let s = self
            .bot
            .state_arc()
            .read_at(crate::bot_core::state_lock::LockSite::Pump)
            .snapshot_seed_block();
        let Some(seed) = s else { return Ok(0) };
        if seed == 0 || ws_block == 0 || seed >= ws_block {
            return Ok(0);
        }
        op_info!(
            domain = pump,
            seed,
            ws_block,
            "BlockPump: auto-backfill from snapshot block to WS block before resume"
        );
        self.backfill_from_snapshot(ws_block, DEFAULT_BACKFILL_CHUNK_SIZE)
            .await
    }

    // L3B6AE (epic VHCRD2, DECIDED 2026-09-10) — phasing-trigger POLICY: phase
    // this method only when a single change must touch MORE THAN TWO of its five
    // interleaved concerns in one PR (below the threshold the interleaving in a
    // single-writer loop is measured-and-accepted, not a hazard). Line anchors are
    // HEAD-of-this-commit (0bd2b8909 + this comment block):
    //   (1) hotpath guards + timed-exit pruning (hotpath_guard / timed_exit_tick);
    //   (2) allocator purge control (allocator_ctrl::on_header_observed, header
    //       branch @1136; init_from_env_at_pump_start @527);
    //   (3) WS-completeness cross-check (CompletenessDecision Verify @1591 /
    //       BackfillOwned @1603, behind ws_completeness_enabled);
    //   (4) posture telemetry on the executor seam (feed_executor_throttle_sample
    //       header-cadence feed @1159);
    //   (5) backfill/rewind re-anchoring (fsm.record_backfill @619 + the header
    //       epoch anchors).
    // Re-baseline: production body ~1400 lines (492..1891, up to
    // boundary_drain_dispatch's docs); the original review's "1391+" came from a
    // drifted revision — churn since is additive test mass, so the
    // trailing-average shrink claim stays qualitative.
    // PRESERVE under phasing: Arc<dyn StageHandlers> (the test surface) and the
    // for_test knobs (set_quiesce_for_test / bot_arc_for_test / header-staleness /
    // early-slice / log-silence, ~2400-2485), plus the FSM instance and its
    // mutation points (fsm.set_quiesce_params @611, fsm.record_backfill @619).
    // The future phaser must document the phase-state carrier decision (shared
    // struct vs heavy parameter passing) with its change. Decision record:
    // CONTEXT.md "Block-pump dispatch seam" -> pump-driver phasing (DECIDED).
    /// Processes logs eagerly: each WS log is applied to engine state
    /// immediately and affected paths are solved right away, without
    /// waiting for a block header. Block headers provide metadata
    /// (timestamp, fees) and handle empty-block detection.
    ///
    /// # Panics
    ///
    /// Hard-aborts the process (never unwinds a half-alive pump) on the fatal
    /// failure buckets, on a live-websocket log drop
    /// (`DEGENBOT_WS_COMPLETENESS`), or a dead or
    /// stalled background drainer (a send into a closed channel, or
    /// `NO_PROGRESS_STRIKE_LIMIT` consecutive no-progress pushes). Also shuts
    /// down on a
    /// late-forward log on a tombstoned block (unreliable WS, ADR-008 D3).
    // MQUKB6-T0: `clippy::used_underscore_binding` expectation retired — it
    // was only fired by the removed `#[tracing::instrument]` expansion.
    #[expect(clippy::too_many_lines, clippy::cast_possible_truncation)]
    // MQUKB6-T0: the former `#[tracing::instrument]` here was a root span that
    // stayed open for the whole bot run. OTel only exports CLOSED spans, so the
    // root never reached Jaeger while every pump-task span referenced it as a
    // missing parent — one giant orphaned trace. Per-epoch `degenbot.epoch`
    // spans (below) are the trace roots now.
    pub async fn run_with_stream(
        &mut self,
        combined: stream::BoxStream<'static, WsEvent>,
        first_observed_block: u64,
    ) {
        // Drained-settle solve gate (TQ7PD6 follow-up): peekable so the loop
        // can probe "is another event already buffered?" WITHOUT consuming it.
        let mut combined = combined.peekable();
        // [DIAG] newHeads-stall investigation: track header arrivals so the
        // log shows, in production, whether `BlockHeader` events actually stop
        // arriving (subscription silent) vs. arrive but the arm doesn't fire
        // (pump not polling / bug). Remove once the freeze root cause is
        // confirmed and fixed — the counters/interval now live behind the
        // `PumpTelemetry` seam (`bot_core::pump_telemetry`).

        // hotpath drain-path tracer bullet (`src/profiling.rs`): hold a
        // profiling guard for the whole pump loop iff `DEGENBOT_HOTPATH=1`.
        // No-op (not even constructed) otherwise, and a no-op stub when the
        // `hotpath` Cargo feature is off. Dropping at loop exit writes the
        // report. With HOTPATH_SHUTDOWN_MS set, the cooperative timer below
        // raises the shutdown flag at the window; the guard drops HERE — after
        // the post-loop OTel flush — so the report captures the final state
        // without racing live workers (S53STH: replaces hotpath's own
        // build_with_shutdown thread, whose process::exit aborted tokio
        // workers mid-TLS-teardown).
        let _hotpath_guard = crate::profiling::hotpath_guard("block_pump");
        // hotpath tokio-runtime monitor (hotpath_tokio_* families): must run
        // inside the ambient I/O runtime, which this is. The interesting
        // getters need --cfg tokio_unstable (root .cargo/config.toml); without
        // it the monitor emits only the unstable-free subset. No-op when the
        // hotpath feature is off.
        #[cfg(feature = "hotpath")]
        hotpath::tokio_runtime!(&tokio::runtime::Handle::current());
        // AZZDBI/XXJR3A: apply any fixed DEGENBOT_MIMALLOC_PURGE_DELAY_MS and
        // arm the block-cadence discovery for the purge-delay control.
        crate::allocator_ctrl::init_from_env_at_pump_start();
        // S53STH cooperative timed exit: a 500ms tick that polls the shutdown
        // flag inside the parked select, so the loop unwinds through its span
        // guards promptly when the hotpath timer raises the flag. The flag is
        // the single source of truth (also checked at the loop head).
        let mut timed_exit_tick = tokio::time::interval(Duration::from_millis(500));
        timed_exit_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        timed_exit_tick.tick().await; // discard the immediate first tick
        #[cfg(feature = "hotpath")]
        if let Some(window) = crate::profiling::timed_exit_window() {
            let flag = Arc::clone(&self.shutdown);
            op_info!(
                domain = pump,
                window_ms = window.as_millis() as u64,
                "timed exit: cooperative pump shutdown armed"
            );
            tokio::spawn(async move {
                tokio::time::sleep(window).await;
                op_info!(
                    domain = pump,
                    "timed exit: HOTPATH_SHUTDOWN_MS window elapsed — raising pump shutdown"
                );
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
            });
        }

        let relevant_topic_set: HashSet<B256> = RELEVANT_TOPICS.into_iter().collect();

        // Read the last block processed by the engine (the post-backfill
        // cursor when the snapshot→WS gap was closed inside resume; cold-start
        // otherwise). J3FMDO: the core `BlockPump::backfill_from_snapshot`
        // applies state via `BotState::process_backfill_logs`, which advances
        // neither the solve/finalize hooks' cursor nor the engine's
        // `last_processed_block`. Hence on the post-backfill resume path the
        // engine's `last_processed_block` is still `None` and the branch below
        // re-anchors on `first_observed_block`. (SZJUKL: the dissolved
        // coordinator cursor — `last_drained_block` under `drain_lock` — is
        // gone; work runs inline in this single-writer driver, so the engine
        // cursor IS the drained cursor.)
        let mut current_block: u64 = self.control.last_processed_block().map_or(0, Epoch::block);

        let snapshot_seed = self
            .bot
            .state_arc()
            .read_at(crate::bot_core::state_lock::LockSite::Pump)
            .snapshot_seed_block();
        if current_block == 0 && first_observed_block > 0 {
            current_block = first_observed_block;
            // One resume/cold-start line either way (audit: the two identical
            // cold-start lines across branches were collapsed).
            if matches!(snapshot_seed, Some(seed) if seed > 0 && seed < first_observed_block) {
                let seed = snapshot_seed.unwrap_or_default();
                op_info!(
                    domain = pump,
                    first_observed_block,
                    backfill_start = seed + 1,
                    backfill_end = first_observed_block,
                    "BlockPump: resuming from block (backfilled snapshot gap)"
                );
            } else {
                op_info!(
                    domain = pump,
                    first_observed_block,
                    "BlockPump: cold start from block"
                );
            }
        } else {
            op_info!(
                domain = pump,
                current_block,
                "BlockPump: starting from block"
            );
        }

        // Track the last block we've solved for: owned by the engine since
        // ergo task LEZJAS (the pump's `last_solved_block` local retired).
        // Seed it to the pump's starting block so the first `finalize_block`
        // guard fires only on a genuine advance (matching the prior local
        // init). A mid-flight-joining engine inherits via `set_last_solved_block`
        // (ADR-006 D4).
        self.control.set_last_solved_block(Epoch::at(current_block));
        // Seed the cold-start solve-results anchor to the settled resume
        // boundary (`current_block` = `first_observed_block` = backfill end):
        // `results_block` is 0 until the first real `on_drain` solve, but
        // registration eagerly solves paths over this backfilled (tip-persisting)
        // state and would otherwise deliver at block 0 or be deferred until the
        // first dirty event. Anchoring it to the settled resume block (a
        // completed, fully-applied block within the backfill window) lets those
        // candidates deliver immediately at a valid, verification-safe solve
        // block — NOT the chain head, which a partially-applied live event could
        // race past the backfill window.
        self.control.set_solve_anchor(Epoch::at(current_block));
        // Whether we're past the first header after resume. The first
        // Epic A1: the pump's decision state now lives in the StageMachine; the
        // driver routes the decision arms through it. `current_block` seeds the FSM.
        let mut fsm = StageMachine::new(current_block, 0);
        // BM35LK: the FSM owns the adaptive quiesce estimator (pure) — the
        // pump hands it the operator-tuned parameter snapshot once and then
        // only feeds settle-point observations and reads the armed window.
        fsm.set_quiesce_params(self.quiesce_params);
        // DFQYM5 single-writer, now FSM-owned (epic O3HW7E/T3): on a resume
        // where the snapshot→WS gap was backfilled (S < W), the backfill owns
        // [S+1, W] inclusive and the live WS owns [W+1, ∞). Seed the FSM's
        // recovery anchor with W so `should_drop_recovered_forward` is the
        // single owner of the boundary drop rule — reorgs stay exempt (they
        // must reach the reorg classifier), and no inline duplicate remains.
        if snapshot_seed.is_some_and(|s| s > 0 && s < first_observed_block) {
            fsm.record_backfill(first_observed_block);
        }
        // header establishes our anchor but shouldn't trigger a solve
        // (backfill already solved up to this point).

        // Current block metadata — updated from headers, used for
        // solve batches when logs close out a block.

        // WS-delivery completeness tracker (see `assert_ws_block_complete`):
        // the set of relevant-topic log indices delivered per block, cross-
        // checked against `eth_getLogs` at the block's tombstone to panic on a
        // live websocket log drop. Default-ON (`DEGENBOT_WS_COMPLETENESS`, via
        // `bot_env_flag_default_on`; disable with `=0`); the map is only
        // populated when the gate is on (so the hot loop adds no work when
        // disabled).
        let ws_completeness_enabled = self.ws_completeness_enabled;

        // `has_logs_this_block` is engine-owned since LEZJAS — driven through
        // `self.sink.record_logs_this_block()` (cleared by `finalize_block`).
        // Debounce timer: started when the first dirty log arrives, reset on
        // each new log. When it fires, we send the accumulated result batch
        // to Python. This ensures one dispatch per burst of logs rather than
        // one per individual log.
        // ADR-008 D2: solver-release gate (see the flush in the Err + Ok(None) arms
        // below). `publish_pending` is armed when a forward log applies; the
        // flush fires `on_send` (gated on `consume_quiesced`) at a settle
        // point — a `DEBOUNCE_MS` window with no new event (coalescing a
        // same-block burst into one publish at the tail) OR stream exhaustion.
        // Replaces the wall-clock `DEBOUNCE_MS` send timer: publication is
        // gated on the truth condition (all dispatched logs applied).

        // BQ7ZBC — FSM recovery state: `recovery_anchor` is the highest block an
        // authoritative (eth_getLogs) catch-up has OWNEed — either a live-loop
        // gap/`handle_timeout_eager` backfill, or (at resume) the backfilled
        // snapshot→WS first block. Per the single-writer rule (DFQYM5
        // precedent), the live WS NO LONGER owns any block ≤ `recovery_anchor`:
        // when a stalled WS recovers and flushes buffered forward logs for
        // those blocks, they are duplicates of state we already applied and are
        // dropped (they never reach the `LateForward` benign late-admit
        // drop). Reorg logs (`removed: true`) are NEVER dropped — they always
        // reach the reorg classifier. A forward ABOVE `recovery_anchor` that
        // is stale takes the same benign `LateForward` drop as any other
        // post-tombstone survivor (HJ5HWF: lateness is counted noise, never
        // a fatal signal — only blocks the pump itself backfilled are silent
        // duplicates by construction).

        // ADR-008 per-block state machine. The clock is the authority for
        // block completeness (the tombstone) and the cursor; the pump loop is
        // a thin async driver translating its decisions into stage-hook
        // calls + backfill + shutdown. A header alone NEVER advances the cursor —
        // only `advance_to_drained` (after the tombstone) does.

        // Per-block metadata, snapshotted from each block's header. A block's
        // tombstone (first log for N+1) may arrive AFTER header N+1 overwrote
        // `current_metadata`, so the result batch that finalizes N must carry
        // N's OWN metadata, retrieved here (VTWCIG).

        // [DIAG] newHeads-stall counters — owned by the `PumpTelemetry` seam
        // (`diag_header_count`/`diag_log_count`/`last_header_at`/stats all live
        // inside it; the driver just calls `on_header`/`on_log`/`maybe_stats`).
        let mut telemetry = crate::bot_core::pump_telemetry::PumpTelemetry::new();
        // Logs-subscription liveness watchdog (the INVERSE of
        // `header_staleness`): anchored at pump start and refreshed on EVERY
        // `WsEvent::Log` (before the topic pre-filter, so an irrelevant log
        // still proves the `eth_subscribe "logs"` arm is alive). When the
        // staleness tick wins and headers are FRESH but this has elapsed past
        // `self.log_silence`, the logs sub is presumed stalled → one warning
        // per silence episode (re-armed when the next log resumes).
        // (the logs-silence clock + re-arm alarm now live in the FSM, fed via
        // `record_log`; the telemetry seam owns the DIAG gap anchor).

        // SZJUKL seam retirement: NO dispatch owner, NO drain FIFO, NO
        // background drainer task. The stage hooks run INLINE at the
        // machine's decision points (below), so the B4GX7C drainer-liveness
        // machinery (`DrainerHealth`/`StallWatch`/closed-channel abort) has
        // no separate task to police and is DELETED. The dissolved
        // `DrainerHealth`'s no-progress obligation maps onto the machine's
        // `WatchdogPhase` — a driver that stops making stall-window progress
        // stops accepting headers, and the header-staleness watchdog
        // (`StageDecision::Recover`) fires exactly as before; the logs-silence
        // watchdog covers the inverse. There is no queue left to go silently
        // dead while the loop advances.

        // JIABO3 Option A — header-staleness watchdog. A `tokio::time::interval`
        // selected against `combined.next()` (below) whose internal `Sleep`
        // elapses independently of stream activity. This catches a silent
        // `newHeads` (dead/stalled WS subscription) even under dense-log
        // pressure, where the in-loop `timeout(.. combined.next())` `Err(_)`
        // no-activity path never elapses because `combined.next()` keeps
        // yielding logs. When the tick wins the select AND headers are
        // genuinely stale (>= `header_staleness`), it runs the SAME
        // `handle_timeout_eager` catch-up the no-activity path uses.
        //
        // Limitation (documented in JIABO3 Option A): this fires only when the
        // pump is parked AT the select. If the pump parks BEFORE the select
        // (GIL re-entry park via `PySubscriberAdapter`, or engine-lock
        // contention inside `on_drain`/`apply_buffer_v3`), the interval can't
        // advance — that residual unbounded risk is Option B's
        // notify-delocalization work, out of scope here.
        let mut staleness_tick = tokio::time::interval(self.watchdog.header_staleness);
        staleness_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        staleness_tick.tick().await; // discard the immediate first tick
                                     // A4: a monotonic ms epoch feeding the FSM's pure watchdog `Tick` input
                                     // (time enters as data; the FSM owns no timer or `Instant`).
        let tick_epoch = tokio::time::Instant::now();
        let now_ms = || tick_epoch.elapsed().as_millis() as u64;

        // MQUKB6-T0: the current block's span, replaced by each accepted header.
        let mut block_span: Option<tracing::Span> = None;

        // Pre-solve gap decomposition (GC? tracking epilogue to the pump/`
        // solve-gap investigation): per-block marks so the gap between the
        // `degenbot.epoch` span start (header accepted) and the solve
        // dispatch decomposes into WS delivery (header → first relevant
        // log), burst jitter (first → last log), and the settle wait (last
        // log → settle decision). Reset on every accepted header; recorded
        // onto the block span + the three instruments at the settle point.
        let mut pregap = PreSolveGapTrack {
            header_at: std::time::Instant::now(),
            first_log: None,
            last_log: None,
            logs: 0,
        };

        // BM35LK — per-block intra-block silence-gap tracker: the max gap
        // between consecutive relevant logs feeds the FSM's adaptive
        // trailing-quiesce EWMA at each settle point (timings arrive as
        // data; the FSM owns no clock). Reset at each accepted header.
        let mut last_relevant_log_at: Option<std::time::Instant> = None;
        let mut block_max_gap_us: u64 = 0;

        // PWPPAZ T2 early-slice state: `Some` from the first gate iteration
        // that observed unsolved dirt in the current block window; the slice
        // fires once when the age crosses `early_slice_ms`. `slice_done`
        // makes it one-per-window. Reset at each accepted header (new window)
        // and at each settle dispatch. `tokio::time::Instant` (not std) so
        // paused-runtime tests advance the age with virtual time.
        let mut slice_first_dirty: Option<tokio::time::Instant> = None;
        let mut slice_done = false;

        // WAJEQP T-R1 reorg-window telemetry state: the episode span + its
        // per-window counters live ACROSS loop iterations (EnterReorg →
        // CloseReorg). The window span is its own trace root (episodes cross
        // block windows); the `restore` children are emitted by the
        // coordinator with this span as their explicit parent.
        let mut reorg_span: Option<tracing::Span> = None;
        let mut reorg_pools_restored: u64 = 0;
        let mut reorg_idempotent_noops: u64 = 0;
        // BF43PM (epic MROOY7): the per-stage waterfall seam. The legacy
        // `pump.log_wait` / `pump.apply_stream` children are replaced by the
        // machine's stage cycle rendered as `degenbot.stage.*` spans under
        // the per-epoch root, and the SONJQA force-close law carries over
        // (`force_close_aged` from the timed-exit tick below).
        let mut stage_tel = super::stage_telemetry::StageTelemetry::new();
        // REMED1 T3: per-block phase attribution - the apply-stream start
        // (first relevant log) vs the settle point, recorded on the throttled
        // diag line so slow-block serialization between the WS log wait and
        // the solve is visible from the console. (The apply-stream span
        // itself was folded into the stage waterfall's streaming interval.)
        let mut apply_started_at: Option<std::time::Instant> = None;
        loop {
            // Span lifecycle (TQ7PD6 fix): an enter guard must never outlive a
            // single poll. This task runs on a multi-threaded tokio runtime and
            // may migrate between worker threads at any `.await`; a guard
            // entered on one thread and dropped on another leaks the span's
            // entered state in that worker's TLS forever (observed as 23
            // nested pump.block spans; the leaked spans never close, so OTel
            // never exports them and every child span orphanes in Jaeger).
            // No loop-wide enter here: each dispatch site enters the cursor
            // span in a strictly-synchronous scope and the few futures that
            // must carry block context across an await are wrapped with
            // `.instrument(…)` instead.
            //
            // Solve execution moved OUT of the loop head to the drained-settle
            // gate at the bottom of the loop (TQ7PD6 follow-up): the solver
            // must not fire while buffered WS events are still unprocessed —
            // the 2026-08-22 stall crash was exactly the loop-head solve
            // racing a still-queued swap log.

            // ADR-008 D2: solver-release gate. `fsm.publish_pending` is set when a forward
            // log applies (block becomes quiesced). The flush below fires
            // the Published-edge `on_publish` (gated on `consume_quiesced`) only at a
            // settle point — a timeout with no new event (coalescing a
            // same-block burst into one publish at the tail) OR stream
            // exhaustion. This replaces the wall-clock `DEBOUNCE_MS` send
            // timer: publication is gated on the truth condition (all
            // dispatched logs applied), not schedule.

            // Check shutdown
            if self.shutdown.load(Ordering::Relaxed) {
                op_info!(domain = pump, "BlockPump: shutting down");
                return;
            }

            // Wait for the next event. Use a shorter settle window when a publish is
            // pending so the quiesce-gated flush fires promptly if no new log
            // arrives (coalescing a same-block burst); otherwise the long
            // inactivity backfill window. A new event arriving before the
            // window elapses cancels the flush (the burst is still in flight).
            let wait_timeout = if fsm.publish_pending() {
                // BM35LK: the settle timers arm the FSM's window (fixed mode
                // = the debounce history; adaptive = the estimator's current
                // W) instead of the raw debounce field.
                Duration::from_millis(fsm.settle_window_ms())
            } else {
                Duration::from_secs(BACKFILL_TIMEOUT_SECS)
            };
            let event = tokio::select! {
                biased;
                // S53STH cooperative timed exit: the hotpath timer raises the
                // shutdown flag; this arm polls it every 500ms so the parked
                // select wakes promptly (worst case otherwise: one full
                // BACKFILL_TIMEOUT_SECS park). The loop-head shutdown check
                // then exits and unwinds all span guards on this task. A tick
                // is free relative to the window (minutes) it serves.
                _ = timed_exit_tick.tick() => {
                    if self.shutdown.load(Ordering::Relaxed) {
                        op_info!(domain = pump, "timed exit: shutdown signaled — unwinding pump loop");
                        break;
                    }
                    // SONJQA (G3, preserved — BF43PM): force-close a stale
                    // held stage interval. The epoch root exports when the
                    // header arm's ENTERED scope exits (TQ7PD6 entry-refcount
                    // law) - microseconds after header acceptance on an
                    // all-quiet block - so an open stage span dangling until
                    // the next transition would extend a waterfall child far
                    // past a closed parent (trace a1ad51bd, block 25913381:
                    // 12.7s child on a 319us parent). Bound the child with an
                    // explicit stall event.
                    stage_tel.force_close_aged(self.stage_max_age);
                    // Flag not yet raised: re-park. `continue` keeps both arm
                    // paths diverging so the arm types coerce to the event
                    // arm's `Option<WsEvent>`.
                    continue;
                }
                // JIABO3 header-staleness watchdog — see the interval setup
                // above. Firing here does NOT consume the stream event; it runs
                // `handle_timeout_eager` then re-loops (the top-of-loop drain
                // picks up any dirty paths the backfill created). The
                // `timeout(wait_timeout, combined.next())` future is dropped on
                // this arm winning, so the inactivity/debounce countdown
                // restarts — acceptable since `DEBOUNCE_MS << header_staleness`
                // and the no-activity path is now superseded by this watchdog.
                _ = staleness_tick.tick() => {
                    // A4: the watchdog window decision lives in the FSM
                    // (`on_tick`), fed a synthetic `now_ms`; the interval only
                    // drives it. The driver executes the emitted decisions.
                    for decision in fsm.on_tick(
                        now_ms(),
                        self.watchdog.header_staleness.as_millis() as u64,
                        self.watchdog.log_silence.as_millis() as u64,
                    ) {
                        match decision {
                            StageDecision::Recover => {
                                self.handle_timeout_eager(&mut fsm)
                                    .instrument(block_span.clone().unwrap_or_else(tracing::Span::none))
                                    .await;
                            }
                            StageDecision::LogSilence => {
                                // Logs-subscription liveness watchdog (inverse
                                // of header staleness): headers are FRESH (the
                                // Recover branch did not fire) but no
                                // `WsEvent::Log` arrived in `self.log_silence`
                                // — the `eth_subscribe "logs"` arm is presumed
                                // stalled/dead while `newHeads` is alive. One
                                // warning per silence episode (re-armed when
                                // the next log resumes the sub).
                                op_warn!(domain = pump, silence_secs = self.watchdog.log_silence.as_secs(),
                                    "logs subscription silent: headers flowing but no log"
                                );
                                self.watchdog.record_silence_alarm();
                            }
                            other => unreachable!(
                                "on_tick only emits Recover|LogSilence, got {other:?}"
                            ),
                        }
                    }
                    continue;
                }
                event = timeout(wait_timeout, combined.next()) => event,
            };

            match event {
                // Settle point — no new event in the window. Flush the
                // quiesce-gated publish, OR (if nothing pending) the 60s
                // inactivity backfill path.
                Err(_) => {
                    // A2: settle-point rules live in the FSM (`on_settle`)
                    // — the quiesce-before-publish gate + solver-release gate
                    // (ADR-008 D2) vs the inactivity backfill. The driver only
                    // executes the emitted decisions.
                    //
                    // BM35LK: feed the settled block's observed max silence
                    // gap (when any relevant log arrived) so the estimator
                    // re-arms W for the NEXT settle, publish the current W on
                    // the quiesce-window gauge, and measure the
                    // on_settle-entry-to-decision latency in the hotpath
                    // profiler (VD62GX open item #1 — the 8.4%-of-blocks
                    // settle-overshoot suspects become visible as a bucket).
                    if pregap.logs > 0 {
                        fsm.observe_settle_gap(block_max_gap_us / 1000, now_ms());
                    }
                    if let Some(p) = crate::instruments::pipeline() {
                        p.observe_quiesce_window(fsm.settle_window_ms());
                    }
                    let settle_decisions =
                        hotpath::measure_block!("pump.settle_decision", fsm.on_settle());
                    for decision in settle_decisions {
                        match decision {
                            StageDecision::Publish { open, metadata } => {
                                // Option-A solver-state accuracy gate (AV42C7):
                                // publish the debounced batch to Python (the
                                // Published edge — delivery/submission/Python
                                // subscribe HERE, SZJUKL), then hand the
                                // quiesced `open` block + its change set to the
                                // latest-wins verifier task. The anchor is
                                // `open`, the LOG-DRIVEN quiesced block, NOT the
                                // racing header.
                                // Pre-solve gap decomposition (Jaeger span
                                // fields + pregap histograms): delivery, burst
                                // jitter, and the settle wait each own a
                                // measured slice of the block-to-solve gap so
                                // the opaque pump.span stretch stops hiding
                                // the WS-delivery and debounce components.
                                {
                                    let now = std::time::Instant::now();
                                    let (header_us, burst_us, settle_us, logs) =
                                        match (pregap.first_log, pregap.last_log) {
                                            (Some(f), Some(last)) => (
                                                Some(
                                                    f.saturating_duration_since(pregap.header_at)
                                                        .as_micros()
                                                        as u64,
                                                ),
                                                Some(last.saturating_duration_since(f).as_micros()
                                                    as u64),
                                                Some(
                                                    now.saturating_duration_since(last).as_micros()
                                                        as u64,
                                                ),
                                                pregap.logs,
                                            ),
                                            _ => (None, None, None, pregap.logs),
                                        };
                                    if let Some(span_ref) = block_span.as_ref() {
                                        span_ref.record("pregap.logs", logs);
                                        if let Some(us) = header_us {
                                            span_ref.record("header_to_first_log_us", us);
                                        }
                                        if let Some(us) = burst_us {
                                            span_ref.record("log_burst_us", us);
                                        }
                                        if let Some(us) = settle_us {
                                            span_ref.record("settle_wait_us", us);
                                        }
                                    }
                                    if let Some(p) = crate::instruments::pipeline() {
                                        if let Some(us) = header_us {
                                            p.observe_header_to_first_log(us_to_secs(us));
                                            // ok: us is small
                                        }
                                        if let Some(us) = burst_us {
                                            p.observe_log_burst(us_to_secs(us));
                                        }
                                        if let Some(us) = settle_us {
                                            p.observe_settle_wait(us_to_secs(us));
                                        }
                                    }
                                }
                                // BF43PM: the publish stage span (parented to
                                // this epoch's root) carries the from/to/
                                // queue-age attrs; the publish-cycle histogram
                                // (first relevant log → publish, per quiesce
                                // cycle) is recorded with it. The solve spans
                                // that follow sit beside it under the same
                                // epoch root.
                                stage_tel.on_publish(
                                    block_span.as_ref().unwrap_or(&tracing::Span::none()),
                                    Epoch::with_generation(open, fsm.rewind_seq()),
                                );
                                // REMED1 T3: throttled per-block phase
                                // attribution on the console (every 20th block
                                // - the Jaeger span carries all blocks).
                                #[expect(clippy::items_after_statements)]
                                static DIAG_ATTEMPT: std::sync::atomic::AtomicU32 =
                                    std::sync::atomic::AtomicU32::new(0);
                                let nth =
                                    DIAG_ATTEMPT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                if nth.is_multiple_of(20) {
                                    let apply_us = apply_started_at
                                        .map_or(0, |t| t.elapsed().as_micros() as u64);
                                    let (hw, lw) = (pregap.header_at, pregap.first_log);
                                    op_info!(
                                        domain = pump,
                                        block_number = open,
                                        sequence = nth,
                                        logs = pregap.logs,
                                        header_to_first_log_us = lw.map(|t| {
                                            t.saturating_duration_since(hw).as_micros() as u64
                                        }),
                                        apply_stream_us = apply_us,
                                        "per-block phase attribution (throttled)"
                                    );
                                }
                                let _ctx = block_span.as_ref().map(tracing::Span::enter);
                                self.drive_publish(
                                    &fsm,
                                    fsm.context_for(open, metadata),
                                    &GateOutcome::default(),
                                );
                            }
                            StageDecision::Backfill { from, to } => {
                                // No activity for 60s — backfill `[from, to)`.
                                debug_assert!(from == fsm.current_block() + 1 && to.is_none());
                                self.handle_timeout_eager(&mut fsm)
                                    .instrument(
                                        block_span.clone().unwrap_or_else(tracing::Span::none),
                                    )
                                    .await;
                            }
                            other => {
                                unreachable!("on_settle only emits Publish|Backfill, got {other:?}")
                            }
                        }
                    }
                }

                // Got a block header from the combined stream
                Ok(Some(WsEvent::BlockHeader {
                    number,
                    timestamp,
                    base_fee_per_gas,
                    gas_used,
                    gas_limit,
                })) => {
                    // MQUKB6 (epic KDUED5) + BF43PM (epic MROOY7): the
                    // per-epoch beat — one entered root span per observed
                    // header, carrying the EPOCH context (block + rewind
                    // generation) every span in the epoch waterfall answers
                    // to. Future solver/submission/stage spans fired within
                    // this arm nest under it for free.
                    let epoch_seq = fsm.rewind_seq();
                    let new_block_span = tracing::info_span!(
                        "degenbot.epoch.run",
                        epoch.block = number,
                        epoch.seq = epoch_seq,
                        block.number = number,
                        // pre-solve gap decomposition, recorded at the settle
                        // point (declared Empty so `record` at settle actually
                        // lands — undeclared fields are silently dropped by
                        // the OTel pass-through)
                        pregap.logs = tracing::field::Empty,
                        header_to_first_log_us = tracing::field::Empty,
                        log_burst_us = tracing::field::Empty,
                        settle_wait_us = tracing::field::Empty,
                        // WAJEQP T-R1 reorg breadcrumbs: when a reorg episode
                        // opens or closes while THIS block window is current,
                        // the block trace is silently interrupted — surface it
                        // here so an operator reading the block trace sees the
                        // interruption without opening the window root trace.
                        reorg.entry_block = tracing::field::Empty,
                        reorg.closed = tracing::field::Empty,
                    );
                    // MQUKB6-T0 / JYCTXI: detached-at-creation so each header
                    // span is its own trace ROOT (the detach + its reasoning
                    // live on the telemetry seam). Children (logs, solves,
                    // dispatch) nest under it via the loop-context below; only
                    // the parent linkage at creation changes.
                    crate::telemetry::make_trace_root(&new_block_span);
                    // MQUKB6-T0: this span becomes the loop's per-block context —
                    // subsequent iterations (logs, settle decisions) nest under it
                    // until the next header replaces it.
                    block_span = Some(new_block_span.clone());
                    // Pre-solve gap marks: this header is the clock anchor
                    // for the new block's gap decomposition.
                    pregap = PreSolveGapTrack {
                        header_at: std::time::Instant::now(),
                        first_log: None,
                        last_log: None,
                        logs: 0,
                    };
                    // BM35LK: restart the silence-gap tracker for the new
                    // block window.
                    last_relevant_log_at = None;
                    block_max_gap_us = 0;
                    // PWPPAZ T2: new block window — re-arm the early slice.
                    slice_first_dirty = None;
                    slice_done = false;
                    // BF43PM: a new epoch root — close any held stage interval
                    // from the prior epoch (an all-quiet prior block never
                    // reached a settle) so nothing dangles into the new one.
                    stage_tel.new_epoch();
                    // NO4DIW: the epoch that just closed is fully accounted —
                    // sample its log ledger into the per-block funnel gauges.
                    let ledger = self.bot.dispatcher().snapshot_epoch_logs_and_reset();
                    if let Some(p) = crate::instruments::pipeline() {
                        p.observe_epoch_logs(
                            ledger.seen,
                            ledger.received,
                            ledger.applied,
                            ledger.undecoded + ledger.apply_missed,
                            number,
                        );
                    }
                    // Sync-only header-processing scope (TQ7PD6): this enter
                    // guard dies before the first await below, so it can never
                    // leak across a task migration. The backfill future below
                    // carries the same span across ITS await via Instrument.
                    {
                        let _ctx = new_block_span.enter();
                        // [DIAG] newHeads-liveness: HEADER count, gap, and 20s stall
                        // warning → one call on the telemetry seam.
                        telemetry.on_header(number);
                        // AZZDBI/XXJR3A: block-cadence sample for the runtime
                        // mimalloc purge-delay control (hysteresis-limited
                        // option write; no-op unless allocator-ctrl + auto).
                        crate::allocator_ctrl::on_header_observed();
                        // T2: blocks-observed counter + the header→solved anchor.
                        if let Some(p) = crate::instruments::pipeline() {
                            p.count_block();
                            // VPD5ZH follow-up: the kernel throttle counters
                            // that identified the >10s solve p95 belong on the
                            // dashboard, one sample per block cadence.
                            if let Some(stats) = degenbot_core::cpu_budget::cgroup_throttle_delta()
                            {
                                p.observe_cgroup_throttled(
                                    stats.nr_throttled,
                                    stats.throttled_usec,
                                );
                                // LW-T5 (Seam E), re-routed by JCI2FW
                                // Part A: the SAME per-block sample feeds
                                // the ONE process fleet posture owner
                                // (`degenbot_workers::posture::process()`
                                // — the Executor seam channel is
                                // dissolved). The pump owns the
                                // header-cadence delta
                                // (LAST_HEADER_SAMPLE_MS). Always fed.
                                feed_executor_throttle_sample(
                                    wall_ms(),
                                    stats.nr_throttled,
                                    stats.throttled_usec,
                                );
                            }
                        }
                        // ADR-041: the header→publish epoch-race anchor
                        // (the dissolved DispatchOwner drainer's header→solved
                        // anchor, re-stamped at the Published edge; single-writer
                        // pump task). Wall clock — see `wall_ms`.
                        self.header_ms
                            .store(wall_ms(), std::sync::atomic::Ordering::Relaxed);
                    }
                    // ADR-028: THE header decision lives in the FSM. Feeding
                    // the header (metadata + a wall-clock `now_ms` for the
                    // watchdog anchors) emits, in order, the effects the driver
                    // must execute (Backfill → SetLastSolved → Notify); the FSM
                    // owns every cursor/metadata/anchor transition. The inline
                    // `is_first_header` copy that used to live here is gone —
                    // `on_header` is the single authoritative header handler.
                    let metadata = BlockMetadata {
                        timestamp,
                        base_fee_per_gas,
                        gas_used,
                        gas_limit,
                    };
                    for decision in fsm.on_header(number, metadata, now_ms()) {
                        match decision {
                            StageDecision::Backfill { from, to } => {
                                // Header-gap catch-up over `[from, to]`
                                // (ephemeral, header-driven). The decision's
                                // explicit range is authoritative — the FSM has
                                // already advanced its own cursor past it, so a
                                // `current_block + 1`-derived range would be
                                // wrong here (BQ7ZBC single-writer anchor is set
                                // inside `on_header`).
                                let to = to.unwrap_or_else(|| {
                                    unreachable!("on_header backfill always carries an upper bound")
                                });
                                op_info!(
                                    domain = pump,
                                    from_block = from,
                                    to_block = to,
                                    "BlockPump: gap from block to block — backfilling"
                                );
                                self.backfill_range(from, to, &mut fsm)
                                    .instrument(new_block_span.clone())
                                    .await;
                            }
                            StageDecision::SetLastSolved { block } => {
                                // LEZJAS: the backfill/first header solved up
                                // to `block` already — mark it solved so the
                                // first `finalize_block` guard no-ops.
                                let _ctx = new_block_span.enter();
                                self.control.set_last_solved_block(Epoch::at(block));
                            }
                            StageDecision::Notify { block, metadata } => {
                                // Python's block fsm tracks `newHeads` — the
                                // block-clock pipe (delivery-to-Python at the
                                // async boundary; never queued behind solve work).
                                let _ctx = new_block_span.enter();
                                self.control.notify_block(block, &metadata);
                            }
                            other => {
                                unreachable!("on_header only emits Backfill|SetLastSolved|Notify, got {other:?}")
                            }
                        }
                    }
                    // The `PendingSuccessor` / `OpenNew` decisions carry no
                    // pump action beyond the above — the liveness-probe signal
                    // (dead-logs-sub detection) is handled by the timeout path.
                }

                // Got a log event from the combined stream — apply eagerly.
                // Solve happens at the top of the next iteration. Batch send
                // is debounced — the timer starts/resets on each log.
                Ok(Some(WsEvent::Pool(pe))) => {
                    // (5WTYYQ) The ingestion crate emits the structured
                    // PoolEvent { epoch, log_index, payload }; the apply path
                    // consumes the raw payload.
                    let log = pe.payload;
                    // WS-delivery volume signal (pre topic-filter): pairs with
                    // `degenbot.logs.received` (relevant subset) so a WS feed
                    // that stops delivering relevant logs stays distinguishable
                    // from one that stopped delivering logs at all.
                    if let Some(p) = crate::instruments::pipeline() {
                        p.count_ws_log_seen();
                    }
                    // NO4DIW: the funnel's `seen` leg — tallied at the event
                    // source (pre topic-filter) exactly like the instrument.
                    self.bot.dispatcher().inc_seen();
                    // Logs-subscription liveness: ANY log (even one the topic
                    // pre-filter drops below) proves the `eth_subscribe
                    // "logs"` arm is delivering. Refresh before the pre-filter
                    // and re-arm the silence alarm so a single warning fires
                    // per silence episode (not per tick) — fed to the FSM.
                    fsm.record_log(now_ms());
                    // Fast-path topic pre-filter: the `logs` WS subscription
                    // is unfiltered (no topic/address filter on the server —
                    // see `stream_select`), so the overwhelming majority of
                    // logs here are irrelevant to any pool we track. Checking
                    // topic0 against `RELEVANT_TOPICS` *before* acquiring
                    // `engine.lock()` (a parking_lot mutex) and running the
                    // decoders skips the lock + decode work for those logs,
                    // keeping the hot path off the contention path. This is
                    // NOT redundant with the topic re-match inside
                    // `apply_log` — that re-check is defensive, so `apply_log`
                    // stays safe to call with unfiltered inputs (e.g. from
                    // backfill or tests). Do not remove this pre-filter: it
                    // is the lock-avoidance fast path.
                    if !relevant_topic_set.contains(log.topics().first().unwrap_or(&B256::ZERO)) {
                        continue;
                    }

                    let log_block = log.block_number.unwrap_or(fsm.current_block());
                    // Pre-solve gap marks: a relevant delivered log. First-log
                    // marks the WS delivery latency phase; last-log feeds the
                    // settle wait at the settle point.
                    {
                        let now = std::time::Instant::now();
                        if pregap.first_log.is_none() {
                            pregap.first_log = Some(now);
                        }
                        pregap.last_log = Some(now);
                        pregap.logs += 1;
                        // BM35LK: consecutive-relevant-log silence deltas —
                        // the exact r.v. the adaptive trailing quiesce must
                        // cover (design §2.1 intra-block gaps).
                        if let Some(prev) = last_relevant_log_at {
                            let gap_us = now.saturating_duration_since(prev).as_micros() as u64;
                            if gap_us > block_max_gap_us {
                                block_max_gap_us = gap_us;
                            }
                        }
                        last_relevant_log_at = Some(now);
                    }
                    // BF43PM: the Streaming stage interval opens at the first
                    // relevant log of the epoch (idempotent within the epoch —
                    // the burst's remaining logs only bump its age); it runs
                    // until the quiesce/tombstone/rewind transition. REMED1 T3
                    // keeps the apply-start anchor for the throttled diag line.
                    if apply_started_at.is_none() {
                        apply_started_at = Some(std::time::Instant::now());
                    }
                    stage_tel.on_first_log(
                        block_span.as_ref().unwrap_or(&tracing::Span::none()),
                        Epoch::with_generation(log_block, fsm.rewind_seq()),
                    );
                    // BQ7ZBC — FSM single-writer recovery discard. After an
                    // authoritative eth_getLogs catch-up (`fsm.recovery_anchor`), a
                    // stalled WS that recovers flushes buffered forward logs for
                    // blocks ≤ the anchor — those are duplicates of state the
                    // backfill already applied and are DROPPED (they never reach
                    // `observe_log`'s `LateForward` class). This mirrors the DFQYM5
                    // resume-boundary rule, generalized to mid-run recovery.
                    // Reorg logs (`removed: true`) are NEVER dropped — they must
                    // reach the reorg classifier to unwind the backfilled range.
                    // A forward ABOVE `recovery_anchor` that is still stale
                    // remains a hard ADR-008 D3 fault (only the pump's own
                    // single-writer range is benign).
                    if fsm.should_drop_recovered_forward(log_block, log.removed) {
                        // WAJEQP T-R1: a recovery-dropped log during an OPEN
                        // reorg window is episode evidence — emit it as a
                        // child span so the window trace shows which replay
                        // events were discarded (outside a window it is
                        // routine resume noise; only the log line remains).
                        if let Some(window) = reorg_span.as_ref() {
                            drop(tracing::info_span!(
                                parent: window.clone(),
                                "degenbot.reorg.dropped_recovery",
                                reorg.block = log_block,
                            ));
                        }
                        if let Some(p) = crate::instruments::pipeline() {
                            p.count_reorg_recovery_dropped();
                        }
                        crate::bot_core::trace_ws_log_dispatch(
                            log.address(),
                            log.topics(),
                            log_block,
                            log.log_index,
                            log.transaction_index,
                            log.removed,
                            "DroppedRecovery",
                        );
                        continue;
                    }
                    // WS-completeness tracker: record the delivered relevant
                    // log index for this block so the tombstone can cross-check
                    // it against authoritative on-chain logs (a missing index =
                    // a websocket drop → panic). Only tracked when the gate is
                    // on to keep the default hot loop at zero-cost.
                    if ws_completeness_enabled {
                        if let Some(li) = log.log_index {
                            fsm.record_ws_delivered(log_block, li);
                        }
                    }

                    // ADR-008: route the log via the per-block state machine.
                    // The FSM owns the clock transition + cursor + publish
                    // disarm (ADR-028): `on_log` decides whether this is a
                    // forward dispatch, a tombstone (first removed:false log
                    // for N+1), a reorg signal, or an unreliable-WS late
                    // forward (→ shutdown), and returns the verdict for the
                    // driver to execute the I/O.
                    // BF43PM: the stage row BEFORE this log's transition —
                    // the `stage.from` side of the transition attrs below.
                    let prev_stage = fsm.stage();
                    let log_decision = fsm.on_log(log_block, log.removed);
                    // The reorg classification may have just bumped the
                    // rewind generation (I2). SZJUKL: the dissolved FIFO's
                    // `observe_rewind_seq` mirror is gone — the driver checks
                    // each work item's epoch INLINE at its execution site
                    // (`reorg_flying_stale`), so a stale item cannot slip
                    // through a queue because its check happened pre-bump.
                    // WS delivery trace: log EVERY relevant-topic WS log —
                    // block, log-index, tx-index, topic0, removed, and the fsm
                    // decision — so the delivery order of same-block Mint/Burn
                    // logs is visible against the registration drain+pin that
                    // follows. Always-on DEBUG on `ingest`.
                    crate::bot_core::trace_ws_log_dispatch(
                        log.address(),
                        log.topics(),
                        log_block,
                        log.log_index,
                        log.transaction_index,
                        log.removed,
                        match log_decision {
                            LogDecision::EnterReorg(_) => "EnterReorg",
                            LogDecision::ContinueReorg => "ContinueReorg",
                            LogDecision::CloseReorg { .. } => "CloseReorg",
                            LogDecision::TombstonePrevious(_) => "TombstonePrevious",
                            LogDecision::DispatchForward => "DispatchForward",
                            LogDecision::LateForward(_) => "LateForward",
                        },
                    );
                    match log_decision {
                        LogDecision::EnterReorg(reorg_block) => {
                            // Reorg: per-event per-pool restore via the
                            // coordinator (ADR-006 slice 7). A too-deep reorg
                            // → graceful shutdown. The previous block was
                            // tombstoned; this `removed: true` log reopens it.
                            // Visible operator signal so an unwind is no longer
                            // silent — the prior success path logged nothing,
                            // making a duplicate block log ambiguous (reorg
                            // vs. WS duplication).
                            // BF43PM: the Rewind stage opens (from ANY row —
                            // I6), counted for the A/B Rewind-frequency series.
                            stage_tel.on_enter_reorg(
                                block_span.as_ref().unwrap_or(&tracing::Span::none()),
                                Epoch::with_generation(log_block, fsm.rewind_seq()),
                                prev_stage,
                            );
                            op_warn!(domain = pump, reorg_block,
                                "BlockPump: chain reorg detected (removed log) — entering unwind path"
                            );
                            // WAJEQP T-R1: open the episode span — its OWN
                            // trace root (the episode crosses block windows;
                            // parenting it under the current block span would
                            // misattribute the unwind to the delivering block,
                            // the same disease the solve-span reparenting
                            // cured). Depth is the rollback distance at entry.
                            let depth_blocks = fsm.current_block().saturating_sub(reorg_block);
                            let window = tracing::info_span!(
                                "degenbot.reorg.window",
                                reorg.block = reorg_block,
                                reorg.log_block = log_block,
                                reorg.depth_blocks = depth_blocks,
                                reorg.pools_restored = tracing::field::Empty,
                                reorg.idempotent_noops = tracing::field::Empty,
                                reorg.new_head = tracing::field::Empty,
                                reorg.outcome = tracing::field::Empty,
                            );
                            crate::telemetry::make_trace_root(&window);
                            // WAJEQP T-R1 metrics: episode count + entry depth.
                            if let Some(p) = crate::instruments::pipeline() {
                                p.count_reorg_window();
                                p.observe_reorg_depth(depth_blocks);
                            }
                            if let Some(bs) = block_span.as_ref() {
                                bs.record("reorg.entry_block", reorg_block);
                            }
                            reorg_span = Some(window);
                            reorg_pools_restored = 0;
                            reorg_idempotent_noops = 0;
                            let outcome = {
                                let _entered = reorg_span.as_ref().map(tracing::Span::enter);
                                self.reorg_coordinator
                                    .dispatch_reorg_log(&log, reorg_span.as_ref())
                            };
                            match outcome {
                                Ok(crate::bot_core::reorg_coordinator::ReorgOutcome::Restored) => {
                                    reorg_pools_restored += 1;
                                    if let Some(p) = crate::instruments::pipeline() {
                                        p.count_reorg_unwound_pool();
                                    }
                                }
                                Ok(
                                    crate::bot_core::reorg_coordinator::ReorgOutcome::IdempotentNoop,
                                ) => {
                                    reorg_idempotent_noops += 1;
                                }
                                Err(err) => {
                                    // Too-deep: record the fault on the window
                                    // span FIRST so the last trace before the
                                    // graceful shutdown names the cause, then
                                    // unwind (span guards drop on return).
                                    if let Some(window) = reorg_span.as_ref() {
                                        window.record("reorg.outcome", "too_deep_shutdown");
                                    }
                                    op_error!(domain = pump, ?err, "BlockPump: too-deep reorg — shutting down");
                                    self.shutdown.store(true, Ordering::Relaxed);
                                    return;
                                }
                            }
                            // Cancel any pending publish: results accumulated
                            // from pre-reorg state are invalid (the FSM disarmed
                            // the publish in `on_log`).
                            continue;
                        }
                        LogDecision::ContinueReorg => {
                            // Subsequent removed: true log in the same window —
                            // restore another pool at `log_block`. Trailing the
                            // first event lets the operator correlate successive
                            // unwinds in the same reorg.
                            op_warn!(
                                domain = pump,
                                log_block,
                                "BlockPump: reorg continues — restoring pool for removed log"
                            );
                            let outcome = {
                                let _entered = reorg_span.as_ref().map(tracing::Span::enter);
                                self.reorg_coordinator
                                    .dispatch_reorg_log(&log, reorg_span.as_ref())
                            };
                            match outcome {
                                Ok(crate::bot_core::reorg_coordinator::ReorgOutcome::Restored) => {
                                    reorg_pools_restored += 1;
                                    if let Some(p) = crate::instruments::pipeline() {
                                        p.count_reorg_unwound_pool();
                                    }
                                }
                                Ok(
                                    crate::bot_core::reorg_coordinator::ReorgOutcome::IdempotentNoop,
                                ) => {
                                    reorg_idempotent_noops += 1;
                                }
                                Err(err) => {
                                    if let Some(window) = reorg_span.as_ref() {
                                        window.record("reorg.outcome", "too_deep_shutdown");
                                    }
                                    op_error!(domain = pump, ?err, "BlockPump: too-deep reorg — shutting down");
                                    self.shutdown.store(true, Ordering::Relaxed);
                                    return;
                                }
                            }
                            continue;
                        }
                        LogDecision::CloseReorg { new_head } => {
                            // Reorg window closed — the coordinator restored
                            // unwound pools per-event; this forward log's block
                            // is the new head. Resume forward tracking from it.
                            op_info!(
                                domain = pump,
                                new_head,
                                "BlockPump: reorg window closed — resuming forward tracking"
                            );
                            // WAJEQP T-R1: close the episode span with its
                            // counters + outcome, and leave a breadcrumb field
                            // on the current block window's span.
                            if let Some(window) = reorg_span.take() {
                                window.record("reorg.pools_restored", reorg_pools_restored);
                                window.record("reorg.idempotent_noops", reorg_idempotent_noops);
                                window.record("reorg.new_head", new_head);
                                window.record("reorg.outcome", "closed");
                                // Explicit drop: the window ends HERE, not at
                                // the next reassignment of the local.
                                drop(window);
                            }
                            if let Some(bs) = block_span.as_ref() {
                                bs.record("reorg.closed", new_head);
                            }
                            // BF43PM: the Rewind interval closes (its duration
                            // histogram records) and the fresh epoch's cycle
                            // restarts at Streaming.
                            stage_tel.on_close_reorg(
                                block_span.as_ref().unwrap_or(&tracing::Span::none()),
                                Epoch::with_generation(new_head, fsm.rewind_seq()),
                            );
                            reorg_pools_restored = 0;
                            reorg_idempotent_noops = 0;
                            // Fall through to dispatch this forward log (the FSM
                            // moved the cursor to `new_head` in `on_log`).
                        }
                        LogDecision::TombstonePrevious(prev) => {
                            // 3M5PO5 correction (BGEDB6): this tombstone verdict is the
                            // pump's single writer of the delivery cutoff — `BotState`
                            // owns the value and the driver mirrors the verdict on
                            // execution (the same decision-execution pattern as the
                            // `set_last_solved_block` steps).
                            self.bot
                                .state_arc()
                                .write_at(crate::bot_core::state_lock::LockSite::Pump)
                                .advance_pump_complete_cutoff(prev);
                            // First removed:false log for N+1 → tombstone N.
                            // Finalize N with N's OWN metadata (snapshotted
                            // when N's header arrived), not fsm.current_metadata
                            // which may now hold N+1's — VTWCIG. The terminal
                            // publish (finalize_block) supersedes any pending
                            // quiesce publish for the open block.
                            //
                            // YLYJM2: the tombstone is the ADR-008 D1 signal
                            // that block `prev` is FULLY delivered — every log
                            // for `prev` has been buffered. Mark the V3/V4
                            // pump-buffer completeness marker so the
                            // registration drain+pin cannot capture a
                            // half-delivered `prev` (the rolling-start race
                            // where a later same-block log lands after the pin).
                            // 3M5PO5: no explicit `mark_pump_blocks_complete`
                            // here — the fsm's own `tombstone(prev)` (inside
                            // `on_log`) already advanced the shared cutoff
                            // the registration drain reads.
                            // LOUD WS-completeness check: block `prev` is now
                            // confirmed complete (tombstoned by the first log of
                            // N+1). The FSM owns the whole accountability policy
                            // (single-writer ownership included): `Verify` means
                            // the live WS is answerable for this block — fetch
                            // `eth_getLogs` and abort on a real drop;
                            // `BackfillOwned` means an authoritative catch-up
                            // delivered it, so the cross-check is vacuous.
                            if ws_completeness_enabled {
                                match fsm.completeness_decision(prev) {
                                    CompletenessDecision::Verify {
                                        block,
                                        delivered_log_indices,
                                    } => {
                                        self.assert_ws_block_complete(block, delivered_log_indices)
                                            .instrument(
                                                block_span
                                                    .clone()
                                                    .unwrap_or_else(tracing::Span::none),
                                            )
                                            .await;
                                    }
                                    CompletenessDecision::BackfillOwned => {}
                                }
                            }
                            let prev_meta = fsm
                                .block_metadata_for(prev)
                                .unwrap_or(fsm.current_metadata());
                            let _ctx = block_span.as_ref().map(tracing::Span::enter);
                            // BF43PM: the tombstone is the Finalize row of the
                            // EPOCH `prev` (the machine's coordinate, stamped
                            // with the current rewind generation).
                            stage_tel.on_tombstone(
                                block_span.as_ref().unwrap_or(&tracing::Span::none()),
                                Epoch::with_generation(prev, fsm.rewind_seq()),
                            );
                            self.drive_finalize(&fsm, fsm.context_for(prev, prev_meta));
                        }
                        LogDecision::DispatchForward => {}
                        LogDecision::LateForward(b) => {
                            // HJ5HWF no-landmine ruling: a removed:false log on
                            // a tombstoned block (NOT a reorg) is delivery-
                            // jitter LATENESS, not a structural fault. Blocks ≤
                            // the authoritative `fsm.recovery_anchor` are
                            // already dropped by the single-writer recovery
                            // discard (BQ7ZBC) before they reach this
                            // classifier — so what lands here is a
                            // post-tombstone survivor ABOVE the anchor. It is
                            // dropped UN-applied: I4 forbids pool-state writes
                            // outside the block's Streaming window, and the
                            // tombstone already fixed the delivery cutoff (I7 —
                            // no move, either way). Count it in the benign
                            // late-admit metric family and emit ONE deduped
                            // `late_log` policy event whose message names the
                            // benign path explicitly, so a spate of tight-
                            // settle-window drops can never masquerade as a
                            // structural bug. The completeness verify at the
                            // tombstone/Published edge stays the loud safety
                            // net for genuinely dropped WS logs.
                            if let Some(p) = crate::instruments::pipeline() {
                                p.count_late_log_admitted();
                            }
                            // BM35LK: feed the estimator's sliding-hour
                            // ledger — sustained budget overruns hold the
                            // adaptive window at the ceiling (HJ5HWF
                            // backstop contract).
                            fsm.record_late_admit(now_ms());
                            // The bool (first sighting vs cooldown-suppressed)
                            // is informational; the counted home is the
                            // late_log.admitted counter above.
                            let _ = crate::telemetry::record_exception_keyed(
                                crate::telemetry::error_kind::LATE_LOG,
                                "ws",
                                b,
                                format_args!(
                                    "late forward log for tombstoned block {b}; dropped un-applied via the benign late-admit path (delivery jitter past the D1 tombstone edge); not a structural fault"
                                ),
                            );
                            crate::bot_core::trace_ws_log_dispatch(
                                log.address(),
                                log.topics(),
                                log_block,
                                log.log_index,
                                log.transaction_index,
                                log.removed,
                                "LateAdmitDropped",
                            );
                            // Skip the apply below: the late log never writes
                            // state (I4) — back to the top of the loop.
                            continue;
                        }
                    }

                    // Apply the log immediately to engine state (no solve yet).
                    // ADR-006 D4: routes through `Bot::dispatch_log` (decode →
                    // apply to BotState → record the EpochDelta byproduct) —
                    // NOT `engine.apply_log`. The FSM's `on_log_applied`
                    // records the clock's received/applied edges and arms the
                    // quiesce-gated publish (ADR-008 D2).
                    // One fact — a forward log applied to engine state — feeds
                    // two consumers (T4 pairing pin, epic O3HW7E): the FSM
                    // quiesce arm (`on_log_applied` -> publish_pending) and
                    // the engine's `has_logs_this_block` (finalize
                    // bookkeeping, LEZJAS). Coordinated here, once; do not
                    // split or drop either write.
                    // 7LV6VN T1c: the DispatchForward arm never entered the
                    // block span (unlike Finalize et al.), so the
                    // `LogDispatcher::dispatch` instrument span ran with an
                    // empty thread-local and forked a single-span Jaeger ROOT
                    // per applied log (~25 roots/s — 1488 of 1500 newest
                    // traces in the live probe). Enter the block span so the
                    // apply chain nests under its block's trace.
                    let _block_ctx = block_span.as_ref().map(tracing::Span::enter);
                    self.bot.dispatch_log(&log);
                    fsm.on_log_applied(log_block);
                    // BF43PM: the apply completed — the epoch's Streaming
                    // interval closes and the Quiesced (StreamingComplete)
                    // point span fires with the burst's log count.
                    stage_tel.on_quiesced(
                        block_span.as_ref().unwrap_or(&tracing::Span::none()),
                        Epoch::with_generation(log_block, fsm.rewind_seq()),
                        pregap.logs,
                    );
                    telemetry.note_apply();

                    // LEZJAS: engine owns `has_logs_this_block` now — routed
                    // through the sink so the next `finalize_block` sees it.
                    self.control.record_logs_this_block();

                    // [DIAG] count logs + emit periodic stats so we can see,
                    // during a freeze, that the pump IS polling logs while
                    // headers are gone. This is the liveness signal the loop
                    // otherwise lacks — owned by the `PumpTelemetry` seam.
                    telemetry.on_log();
                    let pool_state_head = self
                        .bot
                        .state_arc()
                        .read_at(crate::bot_core::state_lock::LockSite::Pump)
                        .pool_state_head();
                    telemetry.maybe_stats(fsm.current_block(), pool_state_head);
                }

                Ok(None) => {
                    // ADR-008 D2: stream exhausted — final settle point. Flush
                    // any pending quiesce-gated publish before returning. The
                    // settle rule is the FSM's `on_stream_end`; the driver only
                    // executes the emitted Publish (I/O) and stops.
                    for decision in fsm.on_stream_end() {
                        match decision {
                            StageDecision::Publish { open, metadata } => {
                                let _ctx = block_span.as_ref().map(tracing::Span::enter);
                                // BF43PM: the final settle's publish carries
                                // the same publish stage span as the timed
                                // settle path.
                                stage_tel.on_publish(
                                    block_span.as_ref().unwrap_or(&tracing::Span::none()),
                                    Epoch::with_generation(open, fsm.rewind_seq()),
                                );
                                self.drive_publish(
                                    &fsm,
                                    fsm.context_for(open, metadata),
                                    &GateOutcome::default(),
                                );
                            }
                            StageDecision::Stop => {}
                            other => {
                                unreachable!("on_stream_end only emits Publish|Stop, got {other:?}")
                            }
                        }
                    }
                    // Incident 2026-08-20 (WS-silent class): the pump is DEAD -
                    // the WS subscription dropped and no reconnect exists.
                    // Loud error + sink notification (drops the engine delivery
                    // channels) so the Python consumer's block stream ENDS and
                    // the settlement bot aborts loudly instead of idling
                    // forever (the "deadlock" operators observed).
                    op_error!(domain = pump, "BlockPump: WS subscription streams ended - pump is STOPPED. The bot will no longer process blocks (no reconnect). Check the WS endpoint / restart."
                    );
                    self.control.on_pump_ended();
                    return;
                }
            }

            // DRAINED-SETTLE SOLVE GATE (TQ7PD6 follow-up): the solve fires
            // only once the combined stream is drained — no event is
            // immediately buffered. "Freshest available state" therefore
            // means "everything the WS has delivered so far has been applied",
            // not "whatever happened to fit before the top of the loop". The
            // peek below does NOT consume the next event, so a buffered event
            // simply re-arms the drain loop and the solve happens exactly once
            // at the end of the burst.
            //
            // MBNASQ: the original `poll_fn` was a single non-yielding poll —
            // it checked the stream's internal channel once without giving the
            // tokio runtime a chance to schedule the WS socket reader task. If
            // the WS delivered logs in multiple frames with brief gaps (5-70ms
            // between frames), the poll found the channel empty and the solve
            // fired prematurely. The next frame then triggered ANOTHER solve,
            // producing 2-3 serial solves per block whose total wall-time was
            // the sum. Replaced with a 50ms timed `peek()` await: if the WS
            // has another event ready within 50ms, this resolves `Ok` and the
            // solve is skipped (the loop processes the new event + re-checks).
            // If no event arrives in 50ms, the stream is genuinely quiet and
            // the solve fires — coalescing all logs in the burst into one
            // solve. 50ms is well within the 12s block interval (same
            // `DEBOUNCE_MS` as the publish gate).
            let dirty_now = self.control.has_dirty_paths();
            // PWPPAZ T2 — designed first-slice trigger: remember when the
            // window's unsolved dirt was first observed. While the burst
            // outlives `early_slice_ms`, ONE bounded early Drain fires
            // mid-burst (the timed peek below is shortened to the slice
            // deadline, so the dispatch happens at first-dirty + ~25ms
            // without waiting for quiesce — the latency the retired finalize
            // steal was accidentally providing). The slice consumes the dirty
            // sets (`take_all` semantics) and re-derives its anchor per
            // cycle, so a following tail solve only re-solves NEWLY dirtied
            // pools; one slice per block window keeps MBNASQ's unbounded
            // serial solves from returning. `0` = disabled → the wait below
            // is always the bare debounce window (exact pre-T2 behavior).
            if dirty_now && slice_first_dirty.is_none() && !slice_done {
                slice_first_dirty = Some(tokio::time::Instant::now());
            }
            let slice_pending =
                self.early_slice_ms > 0 && slice_first_dirty.is_some() && !slice_done;
            // The timed peek waits only as long as the EARLIEST of the settle
            // debounce and the slice deadline — the gate self-wakes at the
            // deadline instead of waiting for the next event.
            let peek_wait = match (slice_pending, slice_first_dirty) {
                (true, Some(first)) => {
                    let age = first.elapsed();
                    let target = Duration::from_millis(self.early_slice_ms);
                    if age >= target {
                        // Deadline passed: the slice dispatch decision is
                        // purely a function of the age below; the timed peek
                        // resolves immediately (zero wait).
                        Duration::ZERO.min(Duration::from_millis(fsm.settle_window_ms()))
                    } else {
                        target
                            .saturating_sub(age)
                            .min(Duration::from_millis(fsm.settle_window_ms()))
                    }
                }
                _ => Duration::from_millis(fsm.settle_window_ms()),
            };
            let has_buffered = if dirty_now {
                // Only await when there's work to solve — otherwise skip
                // straight to the select (no dirty paths = nothing to do).
                // `peek()` resolves immediately when an event is buffered or
                // the stream has ended (Ready(None)); it returns Pending (and
                // yields to the runtime so the WS task can deliver) only when
                // the stream is alive but momentarily empty. The timeout
                // fires only in that latter case — coalescing burst gaps without
                // adding latency to streams with ready events.
                use std::pin::Pin;
                match tokio::time::timeout(peek_wait, Pin::new(&mut combined).peek()).await {
                    // event buffered — skip solve
                    Ok(Some(_)) => true,
                    // stream ended OR the wait elapsed — dispatch solve
                    _ => false,
                }
            } else {
                false
            };
            if !has_buffered && dirty_now {
                // Strictly-synchronous solve dispatch: enter the cursor
                // block span just long enough for dispatch() to capture it
                // as the drainer parent (no await inside — TQ7PD6).
                let slice_due = slice_pending
                    && slice_first_dirty.is_some_and(|first| {
                        first.elapsed() >= Duration::from_millis(self.early_slice_ms)
                    });
                self.boundary_drain_dispatch(&fsm, block_span.as_ref(), now_ms());
                if slice_due {
                    // The bounded slice took its one shot this window.
                    slice_done = true;
                    slice_first_dirty = None;
                } else {
                    // Settled-quiet solve: the window's tail is done, and the
                    // slice budget resets with the next header.
                    slice_done = false;
                    slice_first_dirty = None;
                }
            }
        }
        // S53STH: the loop has unwound — every span guard (pump iteration,
        // drainer parent, solve) has popped through its scope on THIS task
        // before this point. Flush + shut down telemetry BEFORE the hotpath
        // guard drops at scope end (its Drop writes the report), so the report
        // and the exporter see the complete, final state. This is the exit
        // ordering that replaces hotpath's old process::exit() race.
        #[cfg(feature = "otel")]
        {
            if let Some(handle) = crate::otel::global_handle() {
                let _ = handle.flush();
                let _ = handle.shutdown();
            }
            crate::metrics::shutdown_global_metrics();
        }
    }

    /// Shared strictly-synchronous solve execution for the drained-settle
    /// gate's quiesce solve and the PWPPAZ T2 early slice (TQ7PD6: no await
    /// inside — the stage hooks run INLINE on this driver task, so their
    /// spans nest under the entered cursor block span naturally).
    ///
    /// Pump-owned ACTIVE BLOCK promotion (QMSTSV/BO5FBS): the solve anchor is
    /// the LOG-DRIVEN settled block (`fsm.latest_observed()`, never a
    /// racing header), floored by the pool-state head so it is never below
    /// the state it solves against (MQIZ5M +1-wei / IIA class; the
    /// backfill-ahead semantics). `drain_decision` owns the exact rule.
    fn boundary_drain_dispatch(
        &self,
        fsm: &StageMachine,
        block_span: Option<&tracing::Span>,
        _now_ms: u64, // the retired header→solved stamp consumed it; the epoch race is stamped in drive_publish
    ) {
        let _solve_ctx = block_span.map(tracing::Span::enter);
        let state_head = self
            .bot
            .state_arc()
            .read_at(crate::bot_core::state_lock::LockSite::Pump)
            .pool_state_head();
        let StageDecision::Drain { block, metadata } = fsm.drain_decision(state_head) else {
            unreachable!("drain_decision always drains when called");
        };
        self.drive_solve(fsm, fsm.context_for(block, metadata));
    }

    /// The I3 stale-epoch drop (7NFYQW, T6IYKY review Q2 edge) — the
    /// dissolved `DispatchOwner` FIFO drop, moved onto the driver: a work
    /// item whose rewind generation sits BELOW the machine's current one is
    /// reorg-flying. It is dropped LOUDLY here instead of silently consuming
    /// `epoch.block()` into solve/finalize bookkeeping; the fresh
    /// generation's stream re-delivers the block's work. Returns true when
    /// the item was dropped.
    #[expect(
        clippy::unused_self,
        reason = "driver-side hook kept on the pump for seam discoverability; the stage machine owns all state the check reads"
    )]
    fn reorg_flying_stale(&self, fsm: &StageMachine, ctx: &crate::bot_core::BlockContext) -> bool {
        let observed_seq = fsm.rewind_seq();
        let epoch = ctx.epoch();
        if epoch.seq() < observed_seq {
            op_warn!(domain = pump, item_block = epoch.block(),
                item_seq = epoch.seq(),
                observed_seq,
                "reorg-flying stage work: stale epoch dropped instead of consuming epoch.block() (I3)"
            );
            if let Some(p) = crate::instruments::pipeline() {
                p.count_stale_drop();
            }
            return true;
        }
        false
    }

    /// Drive the Solved cycle (Quiesced → Resolved → Solved) for `ctx`:
    /// the stage-table row order the drained-settle gate and the backfill
    /// solve both express. The affected keys are the epoch delta's take
    /// (`on_resolve`), consumed by `on_solve`. Marks the block solved
    /// (LEZJAS) on success. Ignores `StageError`s the engine cannot produce
    /// (its hooks are infallible; a hard failure logs loud, never silently
    /// skips — ADR-021 posture).
    fn drive_solve(&self, fsm: &StageMachine, ctx: crate::bot_core::BlockContext) {
        if self.reorg_flying_stale(fsm, &ctx) {
            return;
        }
        let delta = self.bot.active_delta();
        let quiesced = match self.engine.on_streaming_complete(
            &crate::bot_core::stage_handlers::StreamingComplete {
                ctx,
                delta: &delta,
                backfill: None,
            },
        ) {
            Ok(q) => q,
            Err(error) => {
                op_error!(domain = pump, %error, "stage StreamingComplete failed — solve skipped");
                return;
            }
        };
        let paths = match self.engine.on_resolve(&Resolve {
            ctx,
            quiesced: &quiesced,
            delta: &delta,
        }) {
            Ok(p) => p,
            Err(error) => {
                op_error!(domain = pump, %error, "stage Resolve failed — solve skipped");
                return;
            }
        };
        match self.engine.on_solve(&Solve { ctx, paths }) {
            Ok(outcome) => {
                // LEZJAS: the engine owns `last_solved_block`. The cursor
                // fact now crosses the seam ON the outcome (the Solved row
                // knows its anchor epoch), so the driver derives the cursor
                // from the product instead of re-poking the seam with
                // `ctx.epoch()`.
                self.control.set_last_solved_block(outcome.solved);
            }
            Err(error) => {
                op_error!(domain = pump, %error, "stage Solve failed");
            }
        }
    }

    /// Drive the Published row: the delivery-to-Python edge for the
    /// quiesce-gated publish (ADR-008 D2).
    fn drive_publish(
        &self,
        fsm: &StageMachine,
        ctx: crate::bot_core::BlockContext,
        gated: &crate::bot_core::GateOutcome,
    ) {
        if self.reorg_flying_stale(fsm, &ctx) {
            return;
        }
        // ADR-041 epoch race: header accept → Published dispatch (succeeds
        // the dissolved `DispatchOwner` drainer's header→solved stamp; the
        // money race ends at this edge — submission/delivery subscribe here).
        // Single-writer: the pump task alone stores `header_ms`.
        let header_ms = self.header_ms.load(std::sync::atomic::Ordering::Relaxed);
        if header_ms != 0 {
            if let Some(p) = crate::instruments::pipeline() {
                p.observe_header_to_publish(ms_to_secs(wall_ms().saturating_sub(header_ms)));
            }
        }
        if let Err(error) = self.engine.on_publish(&Publish {
            ctx,
            gated: gated.clone(),
        }) {
            op_error!(domain = pump, %error, "stage Publish failed — batch not delivered");
        }
    }

    /// Drive the Finalized row: the tombstone boundary catch (VTWCIG
    /// metadata; terminal publish supersedes the pending quiesce publish).
    fn drive_finalize(&self, fsm: &StageMachine, ctx: crate::bot_core::BlockContext) {
        if self.reorg_flying_stale(fsm, &ctx) {
            return;
        }
        if let Err(error) = self.engine.on_finalize(&Finalize { ctx }) {
            op_error!(domain = pump, %error, "stage Finalize failed — boundary not stamped");
        }
    }

    /// Handle a 60s timeout by backfilling any missed blocks (eager variant).
    async fn handle_timeout_eager(&self, fsm: &mut StageMachine) {
        op_warn!(
            domain = pump,
            backfill_timeout_secs = BACKFILL_TIMEOUT_SECS,
            "BlockPump: no activity — attempting backfill"
        );
        let latest_block = match self.ingestor.latest_block().await {
            Ok(n) => n,
            Err(e) => {
                op_error!(domain = pump, %e, "BlockPump: backfill failed — can't get block number");
                return;
            }
        };

        if latest_block > fsm.current_block() {
            self.backfill_range(fsm.current_block() + 1, latest_block, fsm)
                .await;
            // ADR-028: the cursor + single-writer recovery-anchor advance happen
            // inside the FSM (`on_backfill_range_done`). The driver only reports
            // the engine-side solved boundary.
            fsm.on_backfill_range_done(latest_block);
            // LEZJAS: engine owns `last_solved_block` now — mark the backfilled
            // range solved through the engine seam.
            self.control.set_last_solved_block(Epoch::at(latest_block));
        }
    }

    /// Backfill a range of blocks via `eth_getLogs`, applying each backfilled
    /// log through the SAME per-block state machine as a live WS log (ADR-008
    /// D4, single branch). The provider I/O (`get_logs`) and engine I/O
    /// (`dispatch_log`, `drive_finalize`, `drive_solve`) stay here on the driver;
    /// every FSM-state transition is routed through `StageMachine` methods
    /// (`on_log`, `on_log_applied`) — no fields are threaded out of the capsule.
    async fn backfill_range(&self, from_block: u64, to_block: u64, fsm: &mut StageMachine) {
        if from_block > to_block {
            return;
        }

        op_info!(
            domain = pump,
            from_block,
            to_block,
            "BlockPump: backfilling blocks"
        );
        // T2: one counter per executed backfill range.
        if let Some(p) = crate::instruments::pipeline() {
            p.count_backfill();
        }

        // (5WTYYQ) The transport fetch is degenbot-ingestion's; the
        // apply/solve loop below stays on the driver (its FSM + dispatch).
        let logs = match self.ingestor.fetch_logs(from_block, to_block).await {
            Ok(logs) => logs,
            Err(e) => {
                op_error!(domain = pump, %e, "BlockPump: backfill eth_getLogs failed");
                return;
            }
        };

        // Group logs by block number for sequential processing
        let mut logs_by_block: HashMap<u64, Vec<Log>> = HashMap::new();
        for log in &logs {
            if let Some(block_num) = log.block_number {
                logs_by_block
                    .entry(block_num)
                    .or_default()
                    .push(log.clone());
            }
        }

        let mut any_processed = false;
        for block in from_block..=to_block {
            if self.shutdown.load(Ordering::Relaxed) {
                op_info!(domain = pump, "BlockPump: shutting down during backfill");
                return;
            }

            let block_logs = logs_by_block.remove(&block).unwrap_or_default();
            for log in &block_logs {
                match fsm.on_log(block, log.removed) {
                    LogDecision::TombstonePrevious(prev) => {
                        let prev_meta = fsm.block_metadata_for(prev).unwrap_or_default();
                        self.drive_finalize(fsm, fsm.context_for(prev, prev_meta));
                        self.bot.dispatch_log(log);
                        fsm.on_log_applied(block);
                    }
                    LogDecision::DispatchForward => {
                        self.bot.dispatch_log(log);
                        fsm.on_log_applied(block);
                    }
                    // Backfilled logs come from an authoritative eth_getLogs
                    // against the canonical chain. Reorg/late-forward signals
                    // are not expected here; if one surfaces, skip applying
                    // this log (the canonical chain doesn't contain it) and let
                    // the live stream reconcile.
                    LogDecision::EnterReorg(_)
                    | LogDecision::ContinueReorg
                    | LogDecision::CloseReorg { .. }
                    | LogDecision::LateForward(_) => {
                        op_warn!(
                            domain = pump,
                            block,
                            "BlockPump: backfill saw unexpected decision; skipping log"
                        );
                    }
                }
            }
            if !block_logs.is_empty() {
                // The backfill solve: the Solved cycle at the block's default
                // metadata — NO Published row (no `on_publish`): the
                // backfill applies state without dispatching result batches
                // (the `Backfilled` phase invariant, FD7NFG).
                self.drive_solve(fsm, fsm.context_for(block, BlockMetadata::default()));
                any_processed = true;
            }
        }

        if any_processed {
            op_info!(
                domain = pump,
                from_block,
                to_block,
                "BlockPump: backfill complete for blocks"
            );
        } else {
            op_info!(
                domain = pump,
                from_block,
                to_block,
                "BlockPump: backfill found no relevant events"
            );
        }
    }

    /// LOUD assertion of the core WS-delivery invariant (ADR-008 D1): when
    /// block `block` is tombstoned, EVERY relevant-topic log that exists
    /// on-chain@block must have been delivered by the live WS subscription.
    ///
    /// The pump's correctness model assumes the websocket delivers every log;
    /// a silently dropped log (observed while driving the bot — a single `Mint`
    /// missing from an otherwise-delivered block) produces a pin/verify
    /// mismatch later and, worse, silently stale solve state. This check
    /// cross-references the delivered relevant-topic log-index set against the
    /// authoritative `eth_getLogs` for the block and PANICS if any on-chain
    /// relevant log is missing — a catastrophic websocket delivery failure that
    /// must NOT be masked or silently corrected.
    ///
    /// Gated on `DEGENBOT_WS_COMPLETENESS` (default ON via
    /// `bot_env_flag_default_on`; disable with `=0`; deterministically OFF in
    /// the test constructor). When disabled it is a no-op. On an
    /// `eth_getLogs` transport error (not a mismatch) it logs loudly and
    /// returns — the check cannot run, but the bot is not taken down by a
    /// transient RPC failure.
    ///
    /// # Panics
    ///
    /// Panics if `eth_getLogs` reveals a relevant-topic log for `block` that
    /// the live websocket did not deliver — a catastrophic WS delivery drop
    /// that must fail loudly rather than silently stale the engine state.
    // future=true: poll-level attribution for the per-block getLogs
    // completeness call (WS-gap verification) — poll time here is network
    // wait, useful against the header->published latency race.
    #[hotpath::measure(future = true)]
    pub async fn assert_ws_block_complete(
        &self,
        block: u64,
        delivered_log_indices: std::collections::HashSet<u64>,
    ) {
        // (5WTYYQ) The eth_getLogs transport call is the ingestion crate's.
        let logs = match self.ingestor.fetch_logs(block, block).await {
            Ok(logs) => logs,
            Err(e) => {
                op_error!(domain = pump, block,
                    %e,
                    "BlockPump: WS-completeness eth_getLogs failed (not a mismatch; "
                );
                return;
            }
        };
        // Filter the fetched logs CLIENT-SIDE by exact topic0 ∈ RELEVANT_TOPICS
        // before collecting log_index. `build_backfill_filter`'s server-side
        // topic[0] OR-list over-matches on some nodes (returns a superset —
        // observed: a block with 35 exact-topic relevant logs came back as 43),
        // inflating the "missing" set and creating FALSE drop positives. The
        // WS-delivered side is exact, so the on-chain side must be exact too
        // for an apples-to-apples comparison.
        let onchain: std::collections::HashSet<u64> = logs
            .iter()
            .filter(|l| matches!(l.topic0(), Some(t0) if RELEVANT_TOPICS.contains(t0)))
            .filter_map(|l| l.log_index)
            .collect();
        let missing: Vec<u64> = onchain
            .iter()
            .filter(|li| !delivered_log_indices.contains(li))
            .copied()
            .collect();
        if !missing.is_empty() {
            // LOUD immediate failure: a websocket legitimately failed to
            // deliver events — the very failure mode surfaced loudly rather
            // than masked or silently corrected. Log the full message to
            // stderr/a tracing sink, then ABORT the process so the bot dies
            // HARD and immediately. A contained worker-thread panic would
            // leave the bot half-alive (silent-ish), which is itself a failure
            // mode; `std::process::abort` guarantees termination.
            op_error!(domain = pump, "LIVE WEBSOCKET LOG DROP at block {block}: {} relevant on-chain log(s) missing from WS delivery: log_index {:?}. eth_getLogs={} logs, WS delivered={} logs. The websocket/pump delivery path dropped a relevant event — ABORT (DFQYM5/WS-DROP). Investigate the subscription/reconnect path; do NOT silence this.",
                missing.len(),
                missing,
                onchain.len(),
                delivered_log_indices.len(),
            );
            crate::telemetry::record_exception(
                crate::telemetry::error_kind::WS_COMPLETENESS,
                format_args!(
                    "live WS log drop at block {block}: {} of {} relevant logs missing (log_index {missing:?})",
                    missing.len(),
                    onchain.len()
                ),
            );
            crate::telemetry::flush_before_exit();
            #[expect(clippy::print_stderr)] // invariant-failure diagnostic before abort
            {
                eprintln!(
                    "ABORT: live websocket log drop at block {block} ({} of {} relevant logs missing); eth_getLogs vs WS divergence — see the untraced log for the log_index list.",
                    missing.len(),
                    onchain.len(),
                );
            }
            std::process::abort();
        }
        let extra: Vec<u64> = delivered_log_indices
            .iter()
            .filter(|li| !onchain.contains(li))
            .copied()
            .collect();
        if !extra.is_empty() {
            op_warn!(domain = pump, block,
                extras = ?extra,
                "BlockPump: WS delivered relevant logs not present in eth_getLogs"
            );
        }
    }

    /// Backfill the snapshot→WS gap `S+1..W` (inclusive) using the NO-SOLVE path
    /// (FD7NFG, epic P73ER6). Reads `S` from `BotState::snapshot_seed_block`
    /// (set by `Bot::load_snapshot_from_db`) and `W` from the `ws_block` param
    /// (the block the WS subscription landed on — `SubscribeState::first_block`,
    /// passed by the pyo3 caller or J3FMDO's auto-backfill before `resume`).
    /// Fetches logs via the pump's own `AlloyProvider` (no `rpc_url` from
    /// Python) in `chunk_size` chunks via `build_backfill_filter`, applying
    /// each chunk via `BotState::process_backfill_logs` (the relocated engine
    /// loop). No `solve_dirty` / no batches — the `Backfilled` phase invariant
    /// is "state advanced, no dispatch".
    ///
    /// Returns the count of blocks backfilled (`W - (S+1) + 1 = W-S`), or
    /// `Ok(0)` for a no-op (cold start / S≥W). The post-backfill boundary is
    /// `W`; the pump's resume anchors on `first_observed_block = W` regardless
    /// (the WS anchor, NOT `last_processed_block`), so this method does NOT stamp
    /// the sink's cursor.
    ///
    /// # Errors
    ///
    /// Returns `Err(String)` on a `get_logs` RPC failure.
    pub async fn backfill_from_snapshot(
        &self,
        ws_block: u64,
        chunk_size: u64,
    ) -> Result<u64, String> {
        let w = ws_block;
        let s = {
            let arc = self.bot.state_arc();
            let state = arc.read_at(crate::bot_core::state_lock::LockSite::Pump);
            state.snapshot_seed_block()
        };
        let Some(s) = s else {
            op_info!(
                domain = pump,
                "BlockPump::backfill_from_snapshot: no snapshot loaded, cold-start path"
            );
            return Ok(0);
        };
        if s == 0 {
            op_warn!(
                domain = pump,
                "BlockPump::backfill_from_snapshot: snapshot block S=0, skipping"
            );
            return Ok(0);
        }
        if s >= w {
            op_info!(
                domain = pump,
                s,
                ws_block = w,
                "BlockPump::backfill_from_snapshot: snapshot >= WS block, nothing to backfill"
            );
            return Ok(0);
        }
        let from_block = s + 1;
        // Include `w` (the resume boundary block) so the backfill covers
        // [S+1, W] INCLUSIVE (DFQYM5). Block W is a delivery hole if excluded:
        // the snapshot→WS gap backfill stops at W-1, and the fresh WS `logs`
        // subscription streams ONLY logs mined after it engages — block W's
        // pre-existing logs are never delivered by the WS (observed: 6 of 35
        // at the boundary block). Fetching W deterministically via eth_getLogs
        // closes the hole; the pump drops the sparse WS partial-W-dup logs in
        // `run_with_stream` (see the `log_block <= W` guard).
        let to_block = w;
        let total_blocks = to_block - from_block + 1;
        op_info!(
            domain = pump,
            from_block,
            to_block,
            total_blocks,
            chunk_size,
            "BlockPump::backfill_from_snapshot: fetching events"
        );
        let mut total_logs = 0usize;
        let mut chunk_start = from_block;
        while chunk_start <= to_block {
            let chunk_end = (chunk_start + chunk_size - 1).min(to_block);
            op_info!(
                domain = pump,
                chunk_start,
                chunk_end,
                "BlockPump::backfill_from_snapshot: fetching chunk"
            );
            let t0 = std::time::Instant::now();
            // (5WTYYQ) The eth_getLogs chunk fetch is the ingestion crate's;
            // the apply loop stays on the driver.
            let logs = self
                .ingestor
                .fetch_logs(chunk_start, chunk_end)
                .await
                .map_err(|e| {
                    format!("eth_getLogs failed for blocks {chunk_start}-{chunk_end}: {e}")
                })?;
            let n = logs.len();
            let fetch_ms = t0.elapsed().as_millis();
            op_info!(domain = pump, chunk_start,
                chunk_end,
                log_count = n,
                fetch_ms = %fetch_ms,
                "BlockPump::backfill_from_snapshot: chunk fetched logs"
            );
            total_logs += n;
            // Hold the write guard across the chunk so the apply + buffer-expire
            // (which advance `last_processed_block`) stay atomic per chunk.
            self.bot
                .state_arc()
                .write_at(crate::bot_core::state_lock::LockSite::Pump)
                .process_backfill_logs(&logs, chunk_end);
            op_info!(
                domain = pump,
                chunk_start,
                chunk_end,
                log_count = n,
                "BlockPump::backfill_from_snapshot: chunk logs applied"
            );
            chunk_start = chunk_end + 1;
        }
        op_info!(
            domain = pump,
            total_logs,
            total_blocks,
            "BlockPump::backfill_from_snapshot: complete"
        );
        Ok(total_blocks)
    }
}

#[cfg(test)]
use degenbot_rpc::provider::AlloyProvider;

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
            stage_max_age: Duration::from_secs(super::stage_telemetry::STAGE_MAX_AGE_SECS),
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

#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]
#[cfg(test)]
mod tests {
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

    #[test]
    fn build_backfill_filter_constructs_valid_filter() {
        let filter = build_backfill_filter(100, 200);
        let debug_str = format!("{filter:?}");
        assert!(!debug_str.is_empty());
    }

    #[test]
    fn shutdown_flag_stops_uniswap_pump() {
        let shutdown = Arc::new(AtomicBool::new(true));
        assert!(shutdown.load(Ordering::Relaxed));
    }

    #[test]
    fn relevant_topics_contains_all_seven() {
        assert_eq!(RELEVANT_TOPICS.len(), 7);
        // Verify each is non-zero
        for topic in &RELEVANT_TOPICS {
            assert_ne!(topic, &B256::ZERO);
        }
    }

    #[test]
    fn backfill_timeout_constant_is_reasonable() {
        // 60s is the chosen timeout — verify it's set
        assert_eq!(BACKFILL_TIMEOUT_SECS, 60);
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
        /// Candidate-2 seam pin (ergo 2KQZSC): the loud close must arrive
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

        /// Candidate-2 pin (ergo 2KQZSC): closes driven through the target
        /// `PumpControl` surface. Must be exactly 1 after the WS-streams-ended
        /// branch fires — this is the behavior the pin exists to prove.
        fn pump_control_ends(&self) -> usize {
            self.pump_control_ends
                .load(std::sync::atomic::Ordering::Relaxed)
        }

        /// Candidate-2 pin (ergo 2KQZSC): closes driven through the stage
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
        ) -> Result<crate::bot_core::stage_handlers::QuiesceOutcome, crate::bot_core::StageError>
        {
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

    /// Candidate-2 seam pin (ergo 2KQZSC): the TARGET `PumpControl` surface.
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

    #[test]
    fn test_pump_disables_ws_completeness_by_default() {
        // Same per-pump opt-out as the solver-state tripwire: the per-block
        // WS-delivery completeness cross-check is conservative-ON in production
        // (`DEGENBOT_WS_COMPLETENESS`, via `bot_env_flag_default_on`) but
        // deterministically OFF in the test constructor so synthetic log
        // streams (relevant-topic logs used as pure block tombstones) never
        // trip a spurious eth_getLogs comparison/abort.
        let (pump, _sink) = pump_for_test(None);
        assert!(
            !pump.ws_completeness_enabled,
            "test pumps must disable the WS-delivery completeness cross-check"
        );
        // And the production default must be ON so drops surface loudly out
        // of the box (KAHU5W: typed schema default, loader owns env).
        assert!(
            crate::bot_core::stance::config().pump.ws_completeness,
            "production default for pump.ws_completeness must be ON"
        );
    }

    /// B4GX7C/sole-mode: the GIL-bound `on_send` (Python dispatch) runs on the
    /// background drainer task so the WS poller is never parked behind
    /// `Python::attach`. This exercises the (now sole) mode end-to-end: a
    /// header opens block 101, a V2 Sync log for 101 opens + quiesces it, and
    /// the stream-exhaust settle point flushes the quiesce-gated publish —
    /// which MUST still fire `on_send` (with the block metadata) from the
    /// drainer.
    #[tokio::test]
    async fn decoupled_drain_still_publishes_with_block_metadata() {
        use alloy::primitives::{aliases::U112, Address as A};
        use stream::StreamExt;
        let bot = Arc::new(Bot::new(1));
        {
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
                update_block: 500,
                variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
                stable_swap: false,
                fee_denominator: None,
                ..Default::default()
            })
            .expect("test setup: V2 registration");
        }
        let (mut pump, sink, _shutdown) = pump_for_test_with_bot(bot, Some(100));
        sink.set_dirty(true); // fake sink: mark dirty so the eager drain fires

        // Header(101) + V2 Sync@101 opens + quiesces block 101; stream end
        // flushes the quiesce-gated publish.
        let pool = A::from([0xccu8; 20]);
        let events: Vec<WsEvent> = vec![
            WsEvent::BlockHeader {
                number: 101,
                timestamp: 101_000,
                base_fee_per_gas: Some(1_000_000_001),
                gas_used: 10_000_001,
                gas_limit: 30_000_001,
            },
            WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
                pool,
                alloy::primitives::U256::ZERO,
                alloy::primitives::U256::ZERO,
                101,
                false,
            ))),
        ];
        let combined = stream::iter(events).boxed();
        pump.run_test_loop(combined, 100).await;

        // All sink ops were deferred to the drainer; wait until the drain,
        // the header notify, and the publish have all landed.
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            let done = !sink.drained.lock().unwrap().is_empty()
                && !sink.notified.lock().unwrap().is_empty()
                && !sink.sent.lock().unwrap().is_empty();
            if done || std::time::Instant::now() >= deadline {
                break;
            }
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        // Notify(header 101) routed to the drainer.
        let notified = sink.notified.lock().unwrap().clone();
        assert!(
            notified.iter().any(|&(b, _)| b == 101),
            "decoupled header notify must fire (got {notified:?})"
        );

        // Drain(eager solve of the dirty pool) routed to the drainer.
        let drained = sink.drained.lock().unwrap().clone();
        assert!(
            !drained.is_empty(),
            "decoupled eager drain must solve dirty paths (got {drained:?})"
        );

        // Publish(on_send) routed to the drainer — with the block metadata.
        let sent = sink.sent.lock().unwrap().clone();
        assert!(
            !sent.is_empty(),
            "decoupled publish must still fire on_send (got {sent:?})"
        );
        assert_eq!(
            sent[0].timestamp, 101_000,
            "publish carries the quiesced block's metadata"
        );
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

    /// JIABO3 Option A — header-staleness watchdog independence.
    ///
    /// Contract: a `tokio::time::interval` selected against `combined.next()`
    /// wakes the pump even when the WS stream is silent (no new headers / no
    /// logs after an initial header), firing `handle_timeout_eager` and
    /// backfilling past the stream's observed block. This is the independence
    /// the in-loop `timeout(.. combined.next())` lacked: that timeout only
    /// arms once the loop body reaches its select await, and under dense-log
    /// pressure `combined.next()` keeps yielding so the 60s no-activity path
    /// never elapses — a silent `newHeads` goes undetected. The watchdog tick
    /// elapses on its OWN internal `Sleep`, racing `combined.next()`.
    ///
    /// Stream: header(101), then 250ms silence, then end. `header_staleness`
    /// overridden to 100ms. Mock RPC: `eth_blockNumber` → 102,
    /// `eth_getLogs`(102) → empty. The only way `set_last_solved_block(102)`
    /// lands is the watchdog's backfill — the stream delivered only 101.
    #[tokio::test]
    async fn header_staleness_watchdog_fires_under_silent_stream() {
        let (mut pump, sink, asserter, _shutdown) = pump_for_test_sink_and_asserter(Some(100));
        pump.set_header_staleness_for_test(Duration::from_millis(100));

        // FIFO mock queue: the watchdog's `get_block_number` (returns 102 so
        // `latest > current` triggers backfill), then `get_logs` for block 102
        // (empty — `backfill_range` still stamps `last_processed_block=102`
        // per iteration). Extra `0x66` results pad later ticks (current already
        // 102 → `latest > current` is false → no second backfill, no `get_logs`).
        asserter.push_success(&"0x66".to_string()); // eth_blockNumber → 102
        asserter.push_success(&Vec::<Log>::new()); // eth_getLogs(102) → []
        asserter.push_success(&"0x66".to_string());
        asserter.push_success(&"0x66".to_string());
        asserter.push_success(&"0x66".to_string());

        // Stream: one header (anchors current_block=101, sets last_header_at),
        // then 250ms of silence (combined.next() stays pending → only the
        // watchdog tick can win the select), then end.
        let combined = stream::unfold(0u8, |phase| async move {
            match phase {
                0 => Some((
                    WsEvent::BlockHeader {
                        number: 101,
                        timestamp: 1,
                        base_fee_per_gas: None,
                        gas_used: 0,
                        gas_limit: 0,
                    },
                    1,
                )),
                1 => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    None
                }
                _ => None,
            }
        })
        .boxed();

        pump.run_test_loop(combined, 100).await;

        let solved = sink.solved.lock().unwrap().clone();
        assert!(
            solved.contains(&102),
            "watchdog must backfill block 102 under a silent stream; \
             set_last_solved_block calls were {solved:?}"
        );
    }

    /// JIABO3 Option A — guard: the watchdog does NOT spuriously fire when
    /// headers keep arriving within the staleness window. The
    /// `last_header_at.elapsed() >= header_staleness` guard must prevent
    /// backfill under a live `newHeads` stream, even though the interval tick
    /// still elapses. Locks the guard so a future regression that drops it (and
    /// backfills on every tick) fails here.
    #[tokio::test]
    async fn header_staleness_watchdog_does_not_fire_when_headers_fresh() {
        let (mut pump, sink, asserter, _shutdown) = pump_for_test_sink_and_asserter(Some(100));
        // Generous margin (200ms staleness, headers every 50ms) so the test is
        // not timing-flaky: at any tick elapse, `last_header_at` is <100ms old.
        pump.set_header_staleness_for_test(Duration::from_millis(200));

        // If the watchdog fired spuriously, it would consume these and
        // backfill block 999 (way beyond the stream's observed blocks) →
        // `set_last_solved_block(999)` would land. The assertion is the
        // negative: 999 absent AND the queue unconsumed.
        asserter.push_success(&"0x3e7".to_string()); // eth_blockNumber → 999
        asserter.push_success(&Vec::<Log>::new());

        // Headers 101..105 arriving every 50ms (well within the 200ms
        // staleness window), then end at 300ms.
        let combined = stream::unfold((0u8, 101u64), |(phase, block)| async move {
            match phase {
                _ if block <= 105 => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Some((
                        WsEvent::BlockHeader {
                            number: block,
                            timestamp: block,
                            base_fee_per_gas: None,
                            gas_used: 0,
                            gas_limit: 0,
                        },
                        (phase, block + 1),
                    ))
                }
                _ => None,
            }
        })
        .boxed();

        pump.run_test_loop(combined, 100).await;

        let solved = sink.solved.lock().unwrap().clone();
        assert!(
            !solved.contains(&999),
            "watchdog must NOT backfill while headers are fresh; \
             set_last_solved_block calls were {solved:?}"
        );
        assert_eq!(
            asserter.read_q().len(),
            2,
            "watchdog must not have polled the provider while headers were fresh"
        );
    }

    /// Logs-subscription liveness watchdog (inverse of header staleness):
    /// headers keep flowing but the `eth_subscribe "logs"` arm delivers
    /// NOTHING for `log_silence` → one warning per silence episode. Proves the
    /// detector fires under the failure mode Alternative B's header-only
    /// handshake no longer catches at startup.
    #[tokio::test]
    async fn logs_silence_watchdog_fires_when_headers_flow_but_no_logs() {
        let (mut pump, _sink, _asserter, _shutdown) = pump_for_test_sink_and_asserter(Some(100));
        // Tick every 100ms so the silence check runs often; headers fresh
        // every 40ms (well within the 100ms window); silence threshold 150ms.
        pump.set_header_staleness_for_test(Duration::from_millis(100));
        pump.set_log_silence_for_test(Duration::from_millis(150));

        // Headers 101..110 every 40ms (kept fresh), NO logs at all, then end.
        // At ~150ms `last_log_at` (anchored at start) crosses the threshold;
        // the next staleness tick (headers fresh) fires the alarm.
        let combined = stream::unfold(101u64, |block| async move {
            if block > 110 {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
            Some((
                WsEvent::BlockHeader {
                    number: block,
                    timestamp: block,
                    base_fee_per_gas: None,
                    gas_used: 0,
                    gas_limit: 0,
                },
                block + 1,
            ))
        })
        .boxed();

        pump.run_test_loop(combined, 100).await;

        assert!(
            pump.log_silence_alarm_count() >= 1,
            "logs-silence alarm MUST fire when headers flow but no log arrives \
             within log_silence (got {})",
            pump.log_silence_alarm_count()
        );
    }

    /// Guard: the logs-silence alarm does NOT fire while logs are flowing
    /// (each `WsEvent::Log` refreshes `last_log_at` and re-arms the alarm).
    /// Locks the refresh path so a regression that drops it (and alarms on
    /// every tick despite live logs) fails here.
    #[tokio::test]
    async fn logs_silence_watchdog_does_not_fire_when_logs_flowing() {
        let (mut pump, _sink, _asserter, _shutdown) = pump_for_test_sink_and_asserter(Some(100));
        pump.set_header_staleness_for_test(Duration::from_millis(100));
        pump.set_log_silence_for_test(Duration::from_millis(150));

        let pool = Address::from([0x11u8; 20]);
        // Header + one V2 Sync log every 40ms (both subs alive):
        // `last_log_at` never reaches 150ms. Header+log pairs for blocks 101..110, then end.
        let combined = stream::unfold((101u64, 0u8, pool), |(block, toggle, pool)| async move {
            if block > 110 {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
            let event = if toggle == 0 {
                WsEvent::BlockHeader {
                    number: block,
                    timestamp: block,
                    base_fee_per_gas: None,
                    gas_used: 0,
                    gas_limit: 0,
                }
            } else {
                WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
                    pool,
                    U256::ZERO,
                    U256::ZERO,
                    block,
                    false,
                )))
            };
            Some((event, (block + u64::from(toggle), toggle ^ 1, pool)))
        })
        .boxed();

        pump.run_test_loop(combined, 100).await;

        assert_eq!(
            pump.log_silence_alarm_count(),
            0,
            "logs-silence alarm must NOT fire while logs are flowing"
        );
    }

    #[tokio::test]
    async fn finalize_carries_just_finished_blocks_metadata() {
        // Contract (VTWCIG, ADR-008): block N is finalized when the FIRST
        // `removed: false` LOG for N+1 arrives (the tombstone — NOT a header).
        // The result batch that finalizes N must carry N's OWN metadata, even
        // though header N+1 (with distinct metadata) arrived earlier and
        // overwrote `current_metadata`. Python computes `base_fee_next` from
        // this metadata; carrying N+1's would systematically mis-price settlement arbitrage.
        //
        // Stream: header 101, header 102 (overwrites current_metadata to
        // meta_102), then a forward log for block 102 (tombstones 101). The
        // finalize(101) must carry meta_101, NOT meta_102.
        let (mut pump, sink) = pump_for_test(Some(100));
        let meta_101 = BlockMetadata {
            timestamp: 1_700_000_100,
            base_fee_per_gas: Some(1_000_000_001),
            gas_used: 10_000_001,
            gas_limit: 30_000_001,
        };
        let meta_102 = BlockMetadata {
            timestamp: 1_700_000_200,
            base_fee_per_gas: Some(2_000_000_002),
            gas_used: 20_000_002,
            gas_limit: 30_000_002,
        };
        // header(101): first_header anchor → current_block 101.
        // header(102): new block, current_metadata overwritten to meta_102,
        //   but NO finalize on header (ADR-008).
        // log(102, removed=false): tombstones 101 → finalize(101, meta_101).
        let tombstone_log = make_v2_sync_log(
            Address::from([0xfcu8; 20]),
            U256::from(1),
            U256::from(2),
            102,
            false,
        );
        let events: Vec<WsEvent> = vec![
            WsEvent::BlockHeader {
                number: 101,
                timestamp: meta_101.timestamp,
                base_fee_per_gas: meta_101.base_fee_per_gas,
                gas_used: meta_101.gas_used,
                gas_limit: meta_101.gas_limit,
            },
            WsEvent::BlockHeader {
                number: 102,
                timestamp: meta_102.timestamp,
                base_fee_per_gas: meta_102.base_fee_per_gas,
                gas_used: meta_102.gas_used,
                gas_limit: meta_102.gas_limit,
            },
            WsEvent::Pool(PoolEvent::from_log(tombstone_log)),
        ];
        let combined = stream::iter(events).boxed();
        pump.run_test_loop(combined, 100).await;
        drainer_settle(|| !sink.finalized.lock().unwrap().is_empty()).await;

        let finalized = sink.finalized.lock().unwrap().clone();
        assert!(
            !finalized.is_empty(),
            "log 102 should tombstone+finalize 101"
        );
        let (block, metadata) = &finalized[0];
        assert_eq!(*block, 101, "first finalize is for block 101");
        assert_eq!(
            *metadata, meta_101,
            "block 101's batch must carry 101's metadata, not 102's"
        );
        assert_ne!(
            *metadata, meta_102,
            "block 101's batch must NOT carry 102's metadata"
        );
    }

    /// BO5FBS active-block promotion (QMSTSV, confirmed): the pump sets the
    /// solve anchor = max(newHead-driven `current_block`, `pool_state_head`).
    /// On a header stall, ordered backfill advances the state clock above
    /// `current_block`; the solve anchor must never be below the state it
    /// solves against (MQIZ5M +1-wei / IIA class). Here a V2 pool is
    /// registered at `update_block` 500 while the pump advances headers only to
    /// 103 — every `on_drain` must receive the promoted 500, not the lagging
    /// header. RED before the pump-owned promotion, GREEN after.
    #[tokio::test]
    async fn on_drain_receives_promoted_active_block_not_stalled_header() {
        use alloy::primitives::{aliases::U112, Address as A};
        use stream::StreamExt;
        let bot = Arc::new(Bot::new(1));
        {
            let arc = bot.state_arc();
            let mut core = arc.write_at(crate::bot_core::state_lock::LockSite::Pump);
            core.register_v2_pool(&RegisterV2PoolParams {
                address: A::from([0xabu8; 20]),
                token0: A::from([0xa0u8; 20]),
                token1: A::from([0xa1u8; 20]),
                reserve0: U112::from(1_000),
                reserve1: U112::from(2_000),
                fee_token0: (997, 1000),
                fee_token1: (997, 1000),
                factory: A::from([0xf0u8; 20]),
                update_block: 500,
                variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
                stable_swap: false,
                fee_denominator: None,
                ..Default::default()
            })
            .expect("test setup: V2 registration");
        }
        assert_eq!(
            bot.state_arc()
                .read_at(crate::bot_core::state_lock::LockSite::Pump)
                .pool_state_head(),
            500,
            "state clock is ahead of the header clock (the stall)"
        );
        let (mut pump, sink, _shutdown) = pump_for_test_with_bot(bot, Some(100));
        sink.set_dirty(true);

        let events: Vec<WsEvent> = (101..=103)
            .map(|number| WsEvent::BlockHeader {
                number,
                timestamp: number * 1_000,
                base_fee_per_gas: Some(1_000_000_001),
                gas_used: 10_000_001,
                gas_limit: 30_000_001,
            })
            .collect();
        let combined = stream::iter(events).boxed();
        pump.run_test_loop(combined, 100).await;
        drainer_settle(|| !sink.drained_blocks().is_empty()).await;

        let drained = sink.drained_blocks();
        assert!(
            !drained.is_empty(),
            "dirty sink must fire on_drain each top-of-loop iteration"
        );
        assert!(
            drained.iter().all(|&b| b == 500),
            "every on_drain must receive the promoted active_block (pool_state_head 500), got {drained:?}"
        );
        assert!(
            drained.iter().all(|&b| b >= 103),
            "no on_drain may lag below the state clock: {drained:?}"
        );
    }

    /// TQ7PD6 follow-up — drained-settle solve gate (header form): the solve
    /// fires EXACTLY ONCE, after the buffered header burst is drained, at the
    /// newest observed block — never eagerly at the top of every loop
    /// iteration (the old behavior dispatched one solve per buffered event and
    /// lagged each header by one block).
    #[tokio::test]
    async fn solve_gate_waits_for_drained_stream_headers() {
        use stream::StreamExt;
        let bot = Arc::new(Bot::new(1));
        let (mut pump, sink, _shutdown) = pump_for_test_with_bot(bot, Some(100));
        sink.set_dirty(true);

        let events: Vec<WsEvent> = (101..=103)
            .map(|number| WsEvent::BlockHeader {
                number,
                timestamp: number * 1_000,
                base_fee_per_gas: Some(1_000_000_001),
                gas_used: 10_000_001,
                gas_limit: 30_000_001,
            })
            .collect();
        let combined = stream::iter(events).boxed();
        pump.run_test_loop(combined, 100).await;
        drainer_settle(|| !sink.drained_blocks().is_empty()).await;

        assert_eq!(
            sink.drained_blocks(),
            vec![103],
            "solve must fire once, after the buffered header burst drains, at the newest block"
        );
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

        fn on_enter(
            &self,
            id: &tracing::span::Id,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            REORG_SPAN_STACK.with(|st| st.borrow_mut().push(id.into_u64()));
        }

        fn on_exit(
            &self,
            _id: &tracing::span::Id,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            REORG_SPAN_STACK.with(|st| {
                st.borrow_mut().pop();
            });
        }
    }

    #[tokio::test(start_paused = true)]
    async fn early_slice_fires_mid_burst_then_settles() {
        let bot = Arc::new(Bot::new(1));
        register_burst_pool(&bot);
        let bot = Arc::new(Bot::new(1));
        let (mut pump, sink, _shutdown) = pump_for_test_with_bot(bot, Some(100));
        pump.set_early_slice_ms_for_test(25);
        sink.set_dirty(true);

        let combined = gap_burst_stream(3, 40);
        let t0 = tokio::time::Instant::now();
        pump.run_test_loop(combined, 100).await;
        drainer_settle(|| sink.drained_blocks().len() >= 2).await;

        assert_eq!(
            sink.drained_blocks(),
            vec![101, 101],
            "slice fires at the deadline mid-burst, the tail settles at the newest block"
        );
        let stamps = sink.drained_at();
        assert_eq!(stamps.len(), 2, "exactly slice + tail dispatches");
        let first_rel = stamps[0] - t0;
        assert!(
            first_rel >= Duration::from_millis(20) && first_rel <= Duration::from_millis(45),
            "slice must dispatch at ~first-dirty + 25ms, got {first_rel:?}"
        );
        let second_rel = stamps[1] - t0;
        assert!(
            second_rel >= Duration::from_millis(75),
            "tail settle must fire at burst end, got {second_rel:?}"
        );
    }

    /// PWPPAZ T2 — bounded: ONE early slice per block window, however long
    /// the burst (MBNASQ's unbounded per-gap serial solves must not return).
    /// Six gapped headers → exactly slice + tail, never a third dispatch.
    #[tokio::test(start_paused = true)]
    async fn early_slice_fires_at_most_once_per_window() {
        let bot = Arc::new(Bot::new(1));
        register_burst_pool(&bot);
        let (mut pump, sink, _shutdown) = pump_for_test_with_bot(bot, Some(100));
        pump.set_early_slice_ms_for_test(25);
        sink.set_dirty(true);

        let combined = gap_burst_stream(5, 40);
        pump.run_test_loop(combined, 100).await;
        drainer_settle(|| sink.drained_blocks().len() >= 2).await;

        assert_eq!(
            sink.drained_blocks(),
            vec![101, 101],
            "exactly one early slice + one quiesce tail solve for the window"
        );
    }

    /// PWPPAZ T2 — parity: `DEGENBOT_EARLY_SLICE_MS=0` disables the slice
    /// entirely — the same gapped burst produces exactly the pre-T2 gate
    /// behavior (one quiesce solve at stream end, timed by the full debounce).
    #[tokio::test(start_paused = true)]
    async fn early_slice_disabled_restores_gate_parity() {
        let bot = Arc::new(Bot::new(1));
        register_burst_pool(&bot);
        let (mut pump, sink, _shutdown) = pump_for_test_with_bot(bot, Some(100));
        pump.set_early_slice_ms_for_test(0);
        sink.set_dirty(true);

        let combined = gap_burst_stream(3, 40);
        let t0 = tokio::time::Instant::now();
        pump.run_test_loop(combined, 100).await;
        drainer_settle(|| !sink.drained_blocks().is_empty()).await;

        assert_eq!(
            sink.drained_blocks(),
            vec![101],
            "slice disabled: exactly the pre-T2 quiesce solve, at the newest block"
        );
        let stamps = sink.drained_at();
        assert_eq!(stamps.len(), 1, "single late dispatch, no early slice");
        let rel = stamps[0] - t0;
        assert!(
            rel >= Duration::from_millis(120),
            "slice disabled: the solve must wait out the full debounce/quiesce, got {rel:?}"
        );
    }

    /// BM35LK — adaptive quiesce: with `quiesce_mode = adaptive` (pre-seed
    /// window = the 20 ms ceiling) the drained-settle gate arms the
    /// ESTIMATOR window, not the fixed 50 ms debounce: the same 40 ms-gapped
    /// burst that settles at ≥ 120 ms under the fixed debounce (see
    /// `early_slice_disabled_restores_gate_parity`) dispatches at its
    /// window deadline — because each 40 ms inter-log gap exceeds the
    /// 20 ms window, exactly one dispatch fires per gap, all at block 101.
    #[tokio::test(start_paused = true)]
    async fn adaptive_quiesce_arms_the_estimator_window() {
        let bot = Arc::new(Bot::new(1));
        register_burst_pool(&bot);
        let (mut pump, sink, _shutdown) = pump_for_test_with_bot(bot, Some(100));
        pump.set_early_slice_ms_for_test(0);
        pump.set_quiesce_for_test(QuiesceParams {
            mode: QuiesceMode::Adaptive,
            ..QuiesceParams::default()
        });
        // The fake engine's dirty flag is a static test toggle, so raise it
        // at 35 ms VIRTUAL time — after the header, just before the first
        // log lands at 40 ms — so every drain dispatch is attributable to
        // the settle window alone (a pre-header dirty would dispatch at the
        // window deadline with no logs at all).
        {
            let sink_flag = Arc::clone(&sink);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(35)).await;
                sink_flag.set_dirty(true);
            });
        }

        let combined = gap_burst_stream(3, 40);
        let t0 = tokio::time::Instant::now();
        pump.run_test_loop(combined, 100).await;
        drainer_settle(|| !sink.drained_blocks().is_empty()).await;

        let stamps = sink.drained_at();
        assert!(
            !stamps.is_empty(),
            "adaptive mode must still settle (the gate never starves)"
        );
        let rel_first = stamps[0] - t0;
        // The estimator window (pre-seed = ceil 20 ms) must beat the fixed
        // debounce's earliest possible dispatch under this stream shape
        // (3 gaps × 40 ms + anything ≥ the 20 ms window): the fixed-50 ms
        // posture dispatches at ≥ 120 ms.
        assert!(
            rel_first < Duration::from_millis(120),
            "adaptive settle dispatched at {rel_first:?}; expected inside the estimator window (< 120 ms fixed-debounce floor)"
        );
        assert!(
            rel_first >= Duration::from_millis(40),
            "adaptive settle must still wait its window; dispatched at {rel_first:?}"
        );
        for (i, b) in sink.drained_blocks().iter().enumerate() {
            assert_eq!(*b, 101, "all dispatches settle the open block (drain #{i})");
        }
    }

    /// Drive a reorg scenario under the [`ReorgSpanCapture`] layer on a local
    /// current-thread runtime (spans are created on the pump task; the test
    /// thread holds the subscriber for the whole `block_on`).
    fn run_reorg_stream(capture: ReorgSpanCapture, pump: &mut BlockPump, events: Vec<WsEvent>) {
        use stream::StreamExt;
        use tracing_subscriber::layer::SubscriberExt;
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

    /// WAJEQP T-R1: the reorg window span lifecycle. A `removed:true` log for
    /// the current block opens exactly ONE `degenbot.reorg.window` span (own
    /// root); each subsequent event adds a `degenbot.reorg.restore` child;
    /// the closing forward log records `reorg.new_head` + counters +
    /// `reorg.outcome=closed` and ends the span. The real restore is
    /// `restored`; the replay duplicate is labeled `idempotent_noop`.
    #[test]
    #[expect(clippy::too_many_lines)]
    fn reorg_window_span_lifecycle_enter_restore_close() {
        use alloy::primitives::{Address as A, U256};
        let capture = ReorgSpanCapture::default();
        let bot = Arc::new(Bot::new(1));
        register_burst_pool(&bot);
        let (mut pump, _sink, _shutdown) = pump_for_test_with_bot(bot, Some(100));
        let pool = A::from([0xccu8; 20]);
        let events = vec![
            WsEvent::BlockHeader {
                number: 101,
                timestamp: 101_000,
                base_fee_per_gas: Some(1_000_000_001),
                gas_used: 10_000_001,
                gas_limit: 30_000_001,
            },
            // Forward apply at 101: journal delta at 101.
            WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
                pool,
                U256::from(1_000),
                U256::from(2_000),
                101,
                false,
            ))),
            // Removed at 101 → EnterReorg, journal pops the 101 delta.
            WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
                pool,
                U256::from(1_000),
                U256::from(2_000),
                101,
                true,
            ))),
            // Duplicate removed replay → ContinueReorg, idempotent no-op.
            WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
                pool,
                U256::from(1_000),
                U256::from(2_000),
                101,
                true,
            ))),
            // First forward above the window → CloseReorg{102}.
            WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
                pool,
                U256::from(900),
                U256::from(1_800),
                102,
                false,
            ))),
        ];
        run_reorg_stream(capture.clone(), &mut pump, events);

        let spans = capture
            .spans
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let fields = capture
            .fields
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let windows: Vec<(u64, Option<u64>)> = spans
            .iter()
            .filter(|(n, _, _)| n == "degenbot.reorg.window")
            .map(|(_, id, p)| (*id, *p))
            .collect();
        assert_eq!(
            windows.len(),
            1,
            "exactly one window span per episode; got {:?}",
            spans.iter().map(|(n, _, _)| n).collect::<Vec<_>>()
        );
        let (window_id, window_parent) = windows[0];
        assert!(window_parent.is_none(), "window must be its own trace root");
        let restores: Vec<(u64, Option<u64>)> = spans
            .iter()
            .filter(|(n, _, _)| n == "degenbot.reorg.restore")
            .map(|(_, id, p)| (*id, *p))
            .collect();
        assert_eq!(restores.len(), 2, "one restore span per removed event");
        for (_id, parent) in &restores {
            assert_eq!(*parent, Some(window_id), "restore must parent the window");
        }
        // First restore: a real journal pop. Second: idempotent replay.
        let actions = || -> Vec<(u64, String)> {
            restores
                .iter()
                .filter_map(|(id, _)| {
                    fields
                        .iter()
                        .find(|(sid, k, _)| sid == id && k == "reorg.action")
                        .map(|(_, _, v)| (*id, v.clone()))
                })
                .collect()
        };
        let actions = actions();
        assert!(
            actions.iter().any(|(_, v)| v == "restored"),
            "first removed event must restore: {actions:?}"
        );
        assert!(
            actions.iter().any(|(_, v)| v == "idempotent_noop"),
            "the replay duplicate must be idempotent: {actions:?}"
        );
        let window_fields = |want: &str| -> Option<String> {
            fields
                .iter()
                .find(|(sid, k, _)| *sid == window_id && k == want)
                .map(|(_, _, v)| v.clone())
        };
        assert_eq!(window_fields("reorg.outcome").as_deref(), Some("closed"));
        assert_eq!(window_fields("reorg.new_head").as_deref(), Some("102"));
        assert_eq!(window_fields("reorg.pools_restored").as_deref(), Some("1"));
        assert_eq!(
            window_fields("reorg.idempotent_noops").as_deref(),
            Some("1")
        );
        // Breadcrumbs on the interrupted block's epoch root span.
        let block_spans: Vec<u64> = spans
            .iter()
            .filter(|(n, _, _)| n == "degenbot.epoch.run")
            .map(|(_, id, _)| *id)
            .collect();
        assert!(!block_spans.is_empty());
        let bs_id = block_spans[0];
        assert_eq!(
            fields
                .iter()
                .find(|(sid, k, _)| *sid == bs_id && k == "reorg.entry_block")
                .map(|(_, _, v)| v.as_str()),
            Some("101")
        );
        assert_eq!(
            fields
                .iter()
                .find(|(sid, k, _)| *sid == bs_id && k == "reorg.closed")
                .map(|(_, _, v)| v.as_str()),
            Some("102")
        );
    }

    /// WAJEQP T-R1: a removed log for a pool whose newest journal delta is
    /// already below the target restores nothing — the restore span is still
    /// emitted, labeled `idempotent_noop`, and the window still closes.
    #[test]
    fn reorg_restore_without_delta_is_idempotent_noop() {
        use alloy::primitives::{Address as A, U256};
        let capture = ReorgSpanCapture::default();
        let bot = Arc::new(Bot::new(1));
        register_burst_pool(&bot);
        let (mut pump, _sink, _shutdown) = pump_for_test_with_bot(bot, Some(100));
        let pool = A::from([0xccu8; 20]);
        let events = vec![
            WsEvent::BlockHeader {
                number: 101,
                timestamp: 101_000,
                base_fee_per_gas: Some(1_000_000_001),
                gas_used: 10_000_001,
                gas_limit: 30_000_001,
            },
            // No forward apply at 101: the newest delta is the registration
            // (block 100), so this removed event is a guaranteed no-op.
            WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
                pool,
                U256::from(1_000),
                U256::from(2_000),
                101,
                true,
            ))),
            WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
                pool,
                U256::from(900),
                U256::from(1_800),
                102,
                false,
            ))),
        ];
        run_reorg_stream(capture.clone(), &mut pump, events);

        let spans = capture
            .spans
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let fields = capture
            .fields
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let restores: Vec<u64> = spans
            .iter()
            .filter(|(n, _, _)| n == "degenbot.reorg.restore")
            .map(|(_, id, _)| *id)
            .collect();
        assert_eq!(restores.len(), 1);
        assert_eq!(
            fields
                .iter()
                .find(|(sid, k, _)| sid == &restores[0] && k == "reorg.action")
                .map(|(_, _, v)| v.as_str()),
            Some("idempotent_noop")
        );
        assert_eq!(
            fields
                .iter()
                .find(|(_, k, _)| k == "reorg.outcome")
                .map(|(_, _, v)| v.as_str()),
            Some("closed")
        );
    }

    /// TQ7PD6 follow-up — drained-settle solve gate (log form): the solve must
    /// NOT fire before a still-buffered log for the block is applied. Header
    /// 101 + V2 Sync@101 are delivered back-to-back; the old loop-head solve
    /// dispatched at block 100 (the pre-log anchor) before consuming the log.
    /// The gate defers until both events are drained, then solves at 101 — the
    /// freshest block, with the swap applied.
    #[tokio::test]
    async fn solve_gate_waits_for_buffered_log_before_solving() {
        use alloy::primitives::{aliases::U112, Address as A};
        use stream::StreamExt;
        let bot = Arc::new(Bot::new(1));
        {
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
        let (mut pump, sink, _shutdown) = pump_for_test_with_bot(bot, Some(100));
        sink.set_dirty(true);

        let pool = A::from([0xccu8; 20]);
        let events: Vec<WsEvent> = vec![
            WsEvent::BlockHeader {
                number: 101,
                timestamp: 101_000,
                base_fee_per_gas: Some(1_000_000_001),
                gas_used: 10_000_001,
                gas_limit: 30_000_001,
            },
            WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
                pool,
                alloy::primitives::U256::from(1_000),
                alloy::primitives::U256::from(2_000),
                101,
                false,
            ))),
        ];
        let combined = stream::iter(events).boxed();
        pump.run_test_loop(combined, 100).await;
        drainer_settle(|| !sink.drained_blocks().is_empty()).await;

        assert_eq!(
            sink.drained_blocks(),
            vec![101],
            "solve must fire only after the buffered Sync log is applied (fresh block)"
        );
    }

    /// Solve-anchor regression (ADR-008 D2 solver-release gate): the SOLVE anchor
    /// follows the LOG-DRIVEN settled block (`open`), not a header that raced a
    /// RED→GREEN tracer (epic 6W35AI, 22Y7AB): the pump forwards a
    /// `BlockNotification` for every `newHeads` header it accepts (one per
    /// header, carrying the header's number + metadata), via
    /// `StageHandlers::notify_block` — independent of solve/debounce state. This
    /// is the seam that lets Python derive its block clock from `newHeads`
    /// instead of the stale `ResultBatch::solve_block`.
    #[tokio::test]
    async fn notify_block_fires_once_per_accepted_header() {
        let (mut pump, sink) = pump_for_test(Some(100));
        let meta_101 = BlockMetadata {
            timestamp: 1_700_000_100,
            base_fee_per_gas: Some(1_000_000_001),
            gas_used: 10_000_001,
            gas_limit: 30_000_001,
        };
        let meta_102 = BlockMetadata {
            timestamp: 1_700_000_200,
            base_fee_per_gas: Some(2_000_000_002),
            gas_used: 20_000_002,
            gas_limit: 30_000_002,
        };
        let events: Vec<WsEvent> = vec![
            WsEvent::BlockHeader {
                number: 101,
                timestamp: meta_101.timestamp,
                base_fee_per_gas: meta_101.base_fee_per_gas,
                gas_used: meta_101.gas_used,
                gas_limit: meta_101.gas_limit,
            },
            WsEvent::BlockHeader {
                number: 102,
                timestamp: meta_102.timestamp,
                base_fee_per_gas: meta_102.base_fee_per_gas,
                gas_used: meta_102.gas_used,
                gas_limit: meta_102.gas_limit,
            },
        ];
        let combined = stream::iter(events).boxed();
        pump.run_test_loop(combined, 100).await;

        let notified = sink.notified.lock().unwrap().clone();
        assert_eq!(
            notified.len(),
            2,
            "exactly one notify_block per accepted header"
        );
        assert_eq!(notified[0].0, 101);
        assert_eq!(notified[0].1, meta_101);
        assert_eq!(notified[1].0, 102);
        assert_eq!(notified[1].1, meta_102);
    }

    /// 5DM6JJ contract: the cold-start branch in `run_with_stream` must honor
    /// `first_observed_block` when `last_processed_block()` is `None` (no
    /// prior anchor). This is the defensive safety net the legacy `spawn` fix
    /// leans on: passing the REAL subscribe block W (instead of the legacy
    /// hard-coded `0`) means that if the `on_drain(first_block)` anchor were
    /// ever absent, the pump still cold-starts to W — NOT stuck at 0 to be
    /// jumped out-of-order by the first WS log. Under ADR-008 the tombstone is
    /// a real log for W+1 (not a header).
    #[tokio::test]
    async fn cold_start_anchors_to_first_observed_block() {
        // No prior processed block → `current_block` starts at 0. Pass the
        // subscribe block W as `first_observed_block`. The cold-start branch
        // anchors `current_block` to W. header(W) is the first header
        // (anchor, no finalize); a forward log for W+1 tombstones W →
        // finalize(W) carrying meta_w. Proves we cold-started to W, not 0.
        let (mut pump, sink) = pump_for_test(None);
        let w = 21_500_000u64; // a "huge" chain-head block number
        let meta_w = BlockMetadata {
            timestamp: 1,
            base_fee_per_gas: Some(7),
            gas_used: 8,
            gas_limit: 9,
        };
        let meta_w1 = BlockMetadata {
            timestamp: 2,
            base_fee_per_gas: Some(10),
            gas_used: 11,
            gas_limit: 12,
        };
        let tombstone_log = make_v2_sync_log(
            Address::from([0xfcu8; 20]),
            U256::from(1),
            U256::from(2),
            w + 1,
            false,
        );
        let events: Vec<WsEvent> = vec![
            WsEvent::BlockHeader {
                number: w,
                timestamp: meta_w.timestamp,
                base_fee_per_gas: meta_w.base_fee_per_gas,
                gas_used: meta_w.gas_used,
                gas_limit: meta_w.gas_limit,
            },
            WsEvent::BlockHeader {
                number: w + 1,
                timestamp: meta_w1.timestamp,
                base_fee_per_gas: meta_w1.base_fee_per_gas,
                gas_used: meta_w1.gas_used,
                gas_limit: meta_w1.gas_limit,
            },
            WsEvent::Pool(PoolEvent::from_log(tombstone_log)),
        ];
        let combined = stream::iter(events).boxed();
        pump.run_test_loop(combined, w).await;
        drainer_settle(|| !sink.finalized.lock().unwrap().is_empty()).await;

        let finalized = sink.finalized.lock().unwrap().clone();
        assert!(!finalized.is_empty(), "log w+1 should tombstone+finalize w");
        assert_eq!(
            finalized[0].0, w,
            "first finalize is for the anchored block w"
        );
        assert_eq!(
            finalized[0].1, meta_w,
            "block w's batch carries w's metadata (in-order, anchored)"
        );
    }

    /// BGEDB6 (3M5PO5 correction): the delivery cutoff (last complete block)
    /// is owned by `BotState` and outlives a pump run. A second
    /// `run_with_stream` (a resume with a fresh `StageMachine`)
    /// must NOT reset it — the old design re-embedded a fresh
    /// `Arc<AtomicU64>` (starting at 0) into `BotState` at startup, and the
    /// registration drain stalled until every block re-tombstoned.
    #[tokio::test]
    async fn resume_never_resets_pump_complete_cutoff() {
        use stream::StreamExt;
        let header = |n: u64| WsEvent::BlockHeader {
            number: n,
            timestamp: n,
            base_fee_per_gas: None,
            gas_used: 0,
            gas_limit: 0,
        };
        let bot = Arc::new(Bot::new(1));
        let w = 21_500_000u64;

        // Run 1: header(w) + header(w+1) + a forward log for w+1 -> tombstone w.
        let (mut pump1, _sink1, _shutdown1) = pump_for_test_with_bot(Arc::clone(&bot), None);
        let events: Vec<WsEvent> = vec![
            header(w),
            header(w + 1),
            WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
                Address::from([0xfcu8; 20]),
                U256::from(1),
                U256::from(2),
                w + 1,
                false,
            ))),
        ];
        pump1.run_test_loop(stream::iter(events).boxed(), w).await;
        assert_eq!(
            bot.state_arc()
                .read_at(crate::bot_core::state_lock::LockSite::Pump)
                .pump_complete_cutoff(),
            w,
            "run 1's tombstone of w must reach the state-owned cutoff"
        );

        // Run 2 (resume): a fresh pump, fresh FSM + clock. One header, no new
        // logs -> no new tombstone. The cutoff must survive, not reset.
        let (mut pump2, _sink2, _shutdown2) = pump_for_test_with_bot(Arc::clone(&bot), None);
        pump2
            .run_test_loop(stream::iter(vec![header(w + 1)]).boxed(), w)
            .await;
        assert_eq!(
            bot.state_arc()
                .read_at(crate::bot_core::state_lock::LockSite::Pump)
                .pump_complete_cutoff(),
            w,
            "a resume must NOT reset the cutoff — the value outlives the run"
        );
    }

    // ==============================================================
    // T3 (epic O3HW7E): the single-writer boundary rule has one owner —
    // the FSM's recovery anchor + `should_drop_recovered_forward` (the
    // BQ7ZBC drop path). The driver seeds the anchor from the resume
    // boundary; no inline `snapshot_seed` check remains in the log loop.
    // ==============================================================

    /// DFQYM5 single-writer regression for the resume boundary: with the
    /// snapshot→WS gap backfilled (S < W), the WS's partial duplicate of W
    /// (the boundary block the backfill already fully applied) must not be
    /// re-applied, while the first LIVE log (W+1) flows through. Pins the
    /// behavior T3 preserves while the drop rule's owner moves from the
    /// inline driver check to the FSM's recovery anchor.
    #[tokio::test]
    async fn resume_boundary_duplicate_dropped_live_block_applied() {
        use stream::StreamExt;

        let bot = Arc::new(Bot::new(1));
        let w = 21_500_000u64;
        let pool = Address::from([0xc0u8; 20]);
        let pool_id = {
            let arc = bot.state_arc();
            let mut core = arc.write_at(crate::bot_core::state_lock::LockSite::Pump);
            let pool_id = core
                .register_v2_pool(&RegisterV2PoolParams {
                    address: pool,
                    token0: Address::from([0xa0u8; 20]),
                    token1: Address::from([0xa1u8; 20]),
                    reserve0: U112::from(1_000),
                    reserve1: U112::from(2_000),
                    fee_token0: (997, 1000),
                    fee_token1: (997, 1000),
                    factory: Address::from([0xf0u8; 20]),
                    variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
                    update_block: w - 10,
                    ..Default::default()
                })
                .expect("test setup: V2 registration");
            // Simulate the backfill having applied W (the boundary): state +
            // the drain cutoff both land at W.
            let _ = core.apply_sync_by_pool_id(pool_id, U112::from(5_000), U112::from(1_000), w);
            core.advance_pump_complete_cutoff(w);
            core.set_snapshot_seed_block(Some(w - 10)); // S < W -> backfill owned
            pool_id
        };

        let (mut pump, sink, _shutdown) = pump_for_test_with_bot(Arc::clone(&bot), None);
        sink.set_dirty(true);

        let header = |n: u64| WsEvent::BlockHeader {
            number: n,
            timestamp: n,
            base_fee_per_gas: None,
            gas_used: 0,
            gas_limit: 0,
        };
        let dup_w = make_v2_sync_log(pool, U256::from(5_500u64), U256::from(900u64), w, false);
        let live_w1 =
            make_v2_sync_log(pool, U256::from(6_000u64), U256::from(950u64), w + 1, false);
        pump.run_test_loop(
            stream::iter(vec![
                header(w + 1),
                WsEvent::Pool(PoolEvent::from_log(dup_w)),
                WsEvent::Pool(PoolEvent::from_log(live_w1)),
            ])
            .boxed(),
            w,
        )
        .await;

        let arc = bot.state_arc();
        let core = arc.read_at(crate::bot_core::state_lock::LockSite::Pump);
        let st = core.get_v2_pool_state(pool_id).expect("v2 state");
        assert_eq!(
            st.reserve0,
            U112::from(6_000),
            "W's partial duplicate must NOT re-apply (backfill owns [S+1, W])"
        );
        assert_eq!(st.update_block, w + 1, "the live W+1 log applies");
    }

    /// The T3 behavior delta: a `removed: true` (reorg) log at or below the
    /// resume boundary must REACH the reorg classifier — the single-writer
    /// drop rule only exempts forward logs. Before T3 the inline
    /// `snapshot_seed` check silently dropped reorg logs at the boundary
    /// (a deep-reorg re-delivery could never unwind the backfilled range);
    /// after T3 `should_drop_recovered_forward(removed: true)` is false and
    /// `ReorgCoordinator` restores the pool's pre-block state.
    ///
    /// (RED on pre-T3 code: the reorg log drops inline and the pool stays at
    /// the backfilled-at-W reserves.)
    #[tokio::test]
    async fn resume_boundary_reorg_reaches_classifier_not_inline_drop() {
        use stream::StreamExt;

        let bot = Arc::new(Bot::new(1));
        let w = 21_500_000u64;
        let pool = Address::from([0xc1u8; 20]);
        let pool_id = {
            let arc = bot.state_arc();
            let mut core = arc.write_at(crate::bot_core::state_lock::LockSite::Pump);
            let pool_id = core
                .register_v2_pool(&RegisterV2PoolParams {
                    address: pool,
                    token0: Address::from([0xa0u8; 20]),
                    token1: Address::from([0xa1u8; 20]),
                    reserve0: U112::from(1_000),
                    reserve1: U112::from(2_000),
                    fee_token0: (997, 1000),
                    fee_token1: (997, 1000),
                    factory: Address::from([0xf0u8; 20]),
                    variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
                    update_block: w - 10,
                    ..Default::default()
                })
                .expect("test setup: V2 registration");
            // Backfill-applied state: w-5 then W (both inside [S+1, W]).
            let _ =
                core.apply_sync_by_pool_id(pool_id, U112::from(3_000), U112::from(1_500), w - 5);
            let _ = core.apply_sync_by_pool_id(pool_id, U112::from(5_000), U112::from(1_000), w);
            core.advance_pump_complete_cutoff(w);
            core.set_snapshot_seed_block(Some(w - 10)); // S < W -> backfill owned
            pool_id
        };

        let (mut pump, _sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), None);

        let header = |n: u64| WsEvent::BlockHeader {
            number: n,
            timestamp: n,
            base_fee_per_gas: None,
            gas_used: 0,
            gas_limit: 0,
        };
        // The WS re-delivers the removed boundary Sync (deep-reorg replay).
        let reorg_w = make_v2_sync_log(pool, U256::from(5_000u64), U256::from(1_000u64), w, true);
        pump.run_test_loop(
            stream::iter(vec![
                header(w + 1),
                WsEvent::Pool(PoolEvent::from_log(reorg_w)),
            ])
            .boxed(),
            w,
        )
        .await;

        assert!(
            !shutdown.load(std::sync::atomic::Ordering::SeqCst),
            "a reorg inside the backfilled range is recoverable — no shutdown"
        );
        let arc = bot.state_arc();
        let core = arc.read_at(crate::bot_core::state_lock::LockSite::Pump);
        let st = core.get_v2_pool_state(pool_id).expect("v2 state");
        assert_eq!(
            st.reserve0,
            U112::from(3_000),
            "the reorg classifier restored the pre-W state (unwound W's delta)"
        );
        assert_eq!(st.reserve1, U112::from(1_500));
        assert_eq!(st.update_block, w - 5);
    }

    /// T4 (epic O3HW7E): one fact — a forward log applied to engine state —
    /// feeds two consumers: the FSM quiesce arm (`on_log_applied`, which
    /// arms the quiesce-gated publish) and the engine-side
    /// `has_logs_this_block` bookkeeping (LEZJAS), routed through the
    /// sink's `record_logs_this_block`. This pin asserts the pairing: a
    /// forward log fires exactly one `record_logs_this_block` AND arms the
    /// quiesce publish (`on_send`); a reorg (`removed: true`) log fires
    /// neither — the reorg arms early-return before the apply+record site.
    /// Green-on-first-run pin of the status quo (no production change).
    #[tokio::test]
    async fn log_applied_pairing_forward_records_reorg_does_not() {
        use stream::StreamExt;

        let bot = Arc::new(Bot::new(1));
        let w = 21_500_000u64;
        let pool = Address::from([0xc2u8; 20]);
        {
            let arc = bot.state_arc();
            let mut core = arc.write_at(crate::bot_core::state_lock::LockSite::Pump);
            let _ = core
                .register_v2_pool(&RegisterV2PoolParams {
                    address: pool,
                    token0: Address::from([0xa0u8; 20]),
                    token1: Address::from([0xa1u8; 20]),
                    reserve0: U112::from(1_000),
                    reserve1: U112::from(2_000),
                    fee_token0: (997, 1000),
                    fee_token1: (997, 1000),
                    factory: Address::from([0xf0u8; 20]),
                    variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
                    update_block: w,
                    ..Default::default()
                })
                .expect("test setup: V2 registration");
        }

        let header = |n: u64| WsEvent::BlockHeader {
            number: n,
            timestamp: n,
            base_fee_per_gas: None,
            gas_used: 0,
            gas_limit: 0,
        };

        // Forward: header(w+1) + a live Sync@w+1 -> applied -> both writes
        // fire (the pairing).
        let (mut pump, sink, _shutdown) = pump_for_test_with_bot(Arc::clone(&bot), None);
        sink.set_dirty(true);
        pump.run_test_loop(
            stream::iter(vec![
                header(w + 1),
                WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
                    pool,
                    U256::from(2_000u64),
                    U256::from(1_500u64),
                    w + 1,
                    false,
                ))),
            ])
            .boxed(),
            w,
        )
        .await;
        // Sink ops are deferred to the background drainer — wait for the
        // quiesce publish to land (same pattern as
        // `decoupled_drain_still_publishes_with_block_metadata`).
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            if !sink.sends().is_empty() || std::time::Instant::now() >= deadline {
                break;
            }
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            sink.logs_recorded(),
            1,
            "forward log: exactly one record_logs_this_block"
        );
        assert!(
            !sink.sends().is_empty(),
            "forward log: the quiesce publish (on_log_applied's arm) fired"
        );

        // Reorg: a removed:true Sync at w+1 -> the EnterReorg arm,
        // which early-returns before the apply + record site: no record,
        // no publish.
        let (mut pump2, sink2, shutdown2) = pump_for_test_with_bot(Arc::clone(&bot), None);
        sink2.set_dirty(true);
        pump2
            .run_test_loop(
                stream::iter(vec![
                    header(w + 1),
                    WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
                        pool,
                        U256::from(2_000u64),
                        U256::from(1_500u64),
                        w + 1,
                        true,
                    ))),
                ])
                .boxed(),
                w,
            )
            .await;
        // Let the drainer settle any (nonexistent) work before asserting the
        // negative.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!shutdown2.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(
            sink2.logs_recorded(),
            0,
            "reorg arm: no record_logs_this_block"
        );
        assert!(
            sink2.sends().is_empty(),
            "reorg arm: no quiesce publish armed"
        );
    }

    /// `resume_anchors_to_subscribe_block` invariant — with no out-of-order jump from 0.
    ///
    /// Previously named `legacy_spawn_processes_blocks_in_order_…` and framed
    /// around the deleted `BlockPump::spawn` one-shot; the invariant it
    /// actually pins is `resume`/`run_with_stream`'s anchoring, which survives
    /// the Slice 1 deletion of `spawn` (Plan 102).
    #[tokio::test]
    async fn resume_anchors_to_subscribe_block() {
        // Mimic resume's first step: start with no prior cursor, then
        // `on_drain(W)` (the drain Python issues after `subscribe` returns,
        // before `resume`) anchors `last_processed_block` to W — exactly as
        // the production settle drain does (the `StageHandlers` solve stage).
        // Then resume with first_observed=W
        // (the real subscribe block, post-fix).
        let (mut pump, sink) = pump_for_test(None);
        let w = 21_500_000u64;
        let meta_w = BlockMetadata {
            timestamp: 1,
            base_fee_per_gas: Some(7),
            gas_used: 8,
            gas_limit: 9,
        };
        let meta_w1 = BlockMetadata {
            timestamp: 2,
            base_fee_per_gas: Some(10),
            gas_used: 11,
            gas_limit: 12,
        };
        let meta_w2 = BlockMetadata {
            timestamp: 3,
            base_fee_per_gas: Some(13),
            gas_used: 14,
            gas_limit: 15,
        };
        // The solve issued before resume anchors the cursor to W (SZJUKL:
        // the engine's own cursor; the dissolved coordinator cursor is gone).
        let _ = sink.on_solve(&crate::bot_core::Solve {
            ctx: BlockContext::new(w, meta_w),
            paths: crate::bot_core::AffectedPaths::default(),
        });
        assert_eq!(
            sink.last_processed_block(),
            Some(Epoch::at(w)),
            "solve(W) must anchor the cursor (mirrors the old SolveCoordinator drain)"
        );

        // Resume stream (post-fix: first_observed = W, not 0). header(W+1)
        // is the first header → first_header anchor advances W→W+1; then a
        // forward log for W+2 tombstones W+1 → finalize(W+1, meta_w1).
        let tombstone_log = make_v2_sync_log(
            Address::from([0xfcu8; 20]),
            U256::from(1),
            U256::from(2),
            w + 2,
            false,
        );
        let events: Vec<WsEvent> = vec![
            WsEvent::BlockHeader {
                number: w + 1,
                timestamp: meta_w1.timestamp,
                base_fee_per_gas: meta_w1.base_fee_per_gas,
                gas_used: meta_w1.gas_used,
                gas_limit: meta_w1.gas_limit,
            },
            WsEvent::BlockHeader {
                number: w + 2,
                timestamp: meta_w2.timestamp,
                base_fee_per_gas: meta_w2.base_fee_per_gas,
                gas_used: meta_w2.gas_used,
                gas_limit: meta_w2.gas_limit,
            },
            WsEvent::Pool(PoolEvent::from_log(tombstone_log)),
        ];
        let combined = stream::iter(events).boxed();
        pump.run_test_loop(combined, w).await;
        drainer_settle(|| !sink.finalized.lock().unwrap().is_empty()).await;

        // log(W+2) tombstones W+1 — carrying meta_w1 (W+1's own metadata,
        // snapshotted when header W+1 arrived). Proves the anchor held: we
        // advanced W→W+1→W+2 in order, never jumping from 0.
        let finalized = sink.finalized.lock().unwrap().clone();
        assert!(
            !finalized.is_empty(),
            "log w+2 should tombstone+finalize w+1"
        );
        assert_eq!(finalized[0].0, w + 1, "first finalize is for block w+1");
        assert_eq!(
            finalized[0].1, meta_w1,
            "block w+1's batch carries w+1's metadata (in-order)"
        );
    }

    /// SZJUKL port of the dissolved `event_dispatch` test
    /// `drainer_warns_and_drops_reorg_flying_stale_epoch_work`: the stale-epoch
    /// drop is now the DRIVER-side `reorg_flying_stale` check at each work
    /// site — the `DispatchOwner` FIFO is gone. A work item minted in the
    /// pre-rewind generation is dropped LOUDLY (WARN + metric) instead of
    /// silently consuming `epoch.block()` into solve/finalize bookkeeping;
    /// the post-rewind item minted in the bumped generation is applied.
    #[test]
    fn driver_drops_reorg_flying_stale_epoch_work() {
        let (pump, sink) = pump_for_test(None);
        let mut fsm = StageMachine::new(100, 0);

        // The stage machine rewinds: the generation bumps to 1 (I2) — a
        // removed log opens the window, a forward closes it.
        assert!(matches!(fsm.on_log(90, true), LogDecision::EnterReorg(_)));
        assert!(matches!(
            fsm.on_log(91, false),
            LogDecision::CloseReorg { .. }
        ));
        assert_eq!(fsm.rewind_seq(), 1);

        // Pre-rewind (reorg-flying) work: an item minted BEFORE the bump in
        // generation 0 — dropped, never applied to the engine.
        let stale_ctx = BlockContext::new(
            crate::bot_core::Epoch::with_generation(100, 0),
            BlockMetadata::default(),
        );
        assert!(pump.reorg_flying_stale(&fsm, &stale_ctx));
        pump.drive_finalize(
            &fsm,
            BlockContext::new(
                crate::bot_core::Epoch::with_generation(100, 0),
                BlockMetadata::default(),
            ),
        );
        assert!(
            sink.finalized.lock().unwrap().is_empty(),
            "the reorg-flying finalize must be dropped, never applied"
        );

        // Fresh (post-rewind) work at the bumped generation: applied normally.
        assert!(
            !pump.reorg_flying_stale(&fsm, &fsm.context_for(100, BlockMetadata::default())),
            "context_for mints at the CURRENT generation"
        );
        pump.drive_finalize(
            &fsm,
            crate::bot_core::BlockContext::new(
                crate::bot_core::Epoch::with_generation(100, 1),
                BlockMetadata::default(),
            ),
        );
        assert_eq!(sink.finalized.lock().unwrap().len(), 1);
        assert_eq!(sink.finalized.lock().unwrap().first().unwrap().0, 100);
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
        let inner = alloy::primitives::Log::new_unchecked(
            pool_address,
            vec![V2_SYNC_TOPIC],
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

    /// A `removed: true` V2 Sync log for a registered pool, when its block is
    /// within the reorg journal's depth, drives the pump's reorg branch to
    /// restore the pool to its pre-fork state via the `ReorgCoordinator` and
    /// record it into the epoch `EpochDelta`, and the pump does NOT shut down
    /// (it continues processing).
    ///
    /// This pins the pump-level wiring of ADR-006 slice 7: the coordinator's
    /// restore+notify (covered in `reorg_coordinator.rs`) is the downstream
    /// observable; what is asserted here is that the *pump* routes a
    /// `removed: true` log there, and that an in-depth reorg is non-fatal.
    /// Incident 2026-08-20 (WS-silent class): a WS subscription stream that
    /// ENDS mid-run must notify the sink (`on_pump_ended` — the production
    /// `StageHandlers` impl closes the engine delivery channels there), so
    /// the Python block/result streams END and the settlement bot fails
    /// loudly instead of idling forever (the silent stall operators saw).
    #[tokio::test]
    async fn stream_end_notifies_sink_on_pump_ended() {
        let pool_addr = Address::from([0x22u8; 20]);
        let (bot, _pool_id) = bot_with_registered_v2(pool_addr, 5);
        let (mut pump, sink, _shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
        assert!(!sink.pump_ended(), "no premature pump-ended signal");
        let forward = make_v2_sync_log(pool_addr, U256::from(1_000), U256::from(2_000), 7, false);
        // Stream ends immediately after the log -> Ok(None) arm.
        let combined = stream::iter(vec![WsEvent::Pool(PoolEvent::from_log(forward))]).boxed();
        pump.run_test_loop(combined, 5).await;
        assert!(
            sink.pump_ended(),
            "stream end must route to sink.on_pump_ended (closes the Python-facing channels)"
        );
    }

    // -----------------------------------------------------------------
    // HJ5HWF — late-log admission safety (the no-landmine rule).
    //
    // A tightened settle/debounce window (50ms → 16ms, task VD62GX) may
    // admit logs whose delivery jitter carries them PAST their block's
    // quiesce/tombstone edge (the first successor log — ADR-008 D1). Every
    // such late log must land in a counted, benign, documented state-
    // machine path: dropped un-applied (I4: writers are Streaming-confined),
    // counted (degenbot.late_log.admitted + the deduped `late_log` bucket),
    // NEVER a shutdown/abort that would masquerade as a structural bug.
    //
    // The completeness verify at the tombstone/Published edge stays the loud
    // safety net for genuinely dropped WS logs — these paths cover only
    // POST-tombstone delivery jitter, which the verify cannot see.
    // -----------------------------------------------------------------

    /// A forward log for a block that is ALREADY tombstoned (delivery jitter
    /// past the tombstone edge) must NOT shut the pump down (the retired
    /// ADR-008 D3 hard-fault behavior). Target contract, task HJ5HWF:
    /// - the pump keeps running and later blocks still process normally;
    /// - the late log is dropped WITHOUT applying it (no pool-state mutation
    ///   outside the Streaming window — I4);
    /// - the delivery cutoff stays monotone at the last tombstone (I7);
    /// - every tombstone finalize still fires exactly once (I5).
    #[tokio::test]
    async fn late_forward_after_tombstone_is_benign_late_admit() {
        let pool_addr = Address::from([0x33u8; 20]);
        let (bot, pool_id) = bot_with_registered_v2(pool_addr, 5);
        let (mut pump, sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));

        // Stream: Sync@7 (opens block 7), Sync@8 (tombstones 7, cutoff → 7),
        // then a jittered TAIL for block 7 arriving AFTER the tombstone edge
        // (the late admission), then Sync@9 (tombstones 8, cutoff → 8). End.
        let events = vec![
            WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
                pool_addr,
                U256::from(3_000),
                U256::from(4_000),
                7,
                false,
            ))),
            WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
                pool_addr,
                U256::from(5_000),
                U256::from(6_000),
                8,
                false,
            ))),
            // LATE: block 7's tail log, delivered after 7 was tombstoned.
            WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
                pool_addr,
                U256::from(9_999),
                U256::from(8_888),
                7,
                false,
            ))),
            WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
                pool_addr,
                U256::from(7_000),
                U256::from(8_000),
                9,
                false,
            ))),
        ];
        pump.run_test_loop(stream::iter(events).boxed(), 5).await;
        drainer_settle(|| sink.finalized.lock().unwrap().len() >= 2).await;

        // THE no-landmine assertion: lateness must never look structural.
        assert!(
            !shutdown.load(Ordering::Relaxed),
            "a late forward after the tombstone edge must NOT shut the pump down"
        );
        // Both tombstone finalizes fired exactly once (I5 at the Finalize row).
        let finalized: Vec<u64> = sink
            .finalized
            .lock()
            .unwrap()
            .iter()
            .map(|(b, _)| *b)
            .collect();
        assert_eq!(
            finalized.len(),
            2,
            "both tombstones (7 and 8) must finalize exactly once (got {finalized:?})"
        );
        assert!(
            finalized.contains(&7) && finalized.contains(&8),
            "tombstone finalizes for 7 and 8 must both fire (got {finalized:?})"
        );
        // Delivery cutoff monotone: advanced to the LAST tombstoned block;
        // the late log for 7 must not regress it (I7).
        assert_eq!(
            bot.state_arc()
                .read_at(crate::bot_core::state_lock::LockSite::Pump)
                .pump_complete_cutoff(),
            8,
            "cutoff must rest at the last tombstone (8), untouched by the late log"
        );
        // Streaming-only writes: exactly the 3 in-window logs were applied
        // (blocks 7, 8, 9) — the dropped late log never dispatches (I4).
        assert_eq!(
            sink.logs_recorded(),
            3,
            "only the in-Streaming-window logs apply; the late log is dropped"
        );
        // Pool state holds the LAST legitimately applied sync (9's), never
        // the late log's reserves — either applying the late log after 9's
        // or counting it into the applies would trip this.
        assert_eq!(
            bot.state_arc()
                .read_at(crate::bot_core::state_lock::LockSite::Pump)
                .v2_snapshot(pool_id),
            Some((U256::from(7_000), U256::from(8_000), 9)),
            "pool state reflects only Streaming-window applies; late reserves dropped"
        );
    }

    // HJ5HWF property: synthetic lateness across a whole capture. The tail
    // of EVERY block's log set (a randomized subset) is jitted past its
    // quiesce/tombstone edge (delivered behind the successor's first log).
    // Invariants asserted per run:
    // - no tripwire/fatal path fires from lateness (pump never shuts down);
    // - the delivery cutoff stays monotone at the last tombstone (I7);
    // - no pool-state mutation outside Streaming: apply accounting counts
    //   exactly the in-window deliveries — late tails are benign drops (I4);
    // - one tombstone finalize per block, no superfluous publishes (I5);
    // - the reorg recovery-drop class is NOT a bucket for lateness: no
    //   recovery anchor is ever established by a jitter stream, so no
    //   late log may be mis-filed there (it takes the late-admit path).
    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(16))]
        #[test]
        fn synthetic_lateness_jitter_never_trips_fatal_paths(
            plan in proptest::collection::vec((1u8..=3, 0u8..=2), 2usize..=6),
        ) {
            use stream::StreamExt;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("test runtime");
            // The async case reports its first invariant breach as an Err
            // message; the `return Err` below targets the proptest run
            // closure (a property failure must surface as a TestCaseError,
            // and a `return` inside an async block cannot reach it).
            let check = async {
                let pool_addr = Address::from([0x34u8; 20]);
                let (bot, pool_id) = bot_with_registered_v2(pool_addr, 5);
                let (mut pump, sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));

                let base = 100u64;
                // Distinct reserves per (block, log-index) so any mis-applied
                // (late) log is observable in the final state.
                let reserves = |block: u64, j: usize| {
                    let k = 1_000u64 + block * 10 + j as u64;
                    (U256::from(k), U256::from(2_000 + k))
                };
                let log_event = |block: u64, j: usize| {
                    let (r0, r1) = reserves(block, j);
                    WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
                        pool_addr, r0, r1, block, false,
                    )))
                };

                // Weave the stream: per block, deliver pre-tail logs; every
                // NON-LAST block's tail is deferred until just after the next
                // block's FIRST log (the tombstone/quiesce edge) — the
                // synthetic-jitter shape for a tightened settle window. The
                // last block's tail is still inside its open Streaming window
                // and applies normally.
                let mut events: Vec<WsEvent> = Vec::new();
                let mut deferred_tail: Vec<WsEvent> = Vec::new();
                let mut expected_applied = 0usize;
                let mut expected_finalized: Vec<u64> = Vec::new();
                let (mut last_reserves, mut last_block) = ((U256::ZERO, U256::ZERO), 0u64);
                for (i, (pre, tail)) in plan.iter().enumerate() {
                    let block = base + i as u64;
                    // First log of the block = the tombstone edge; the
                    // previous block's jittered tail lands right behind it.
                    events.push(log_event(block, 0));
                    events.append(&mut deferred_tail);
                    for j in 1..*pre as usize {
                        events.push(log_event(block, j));
                    }
                    expected_applied += *pre as usize;
                    if i + 1 < plan.len() {
                        // This block's tail jitters past its tombstone edge.
                        deferred_tail = (0..*tail as usize)
                            .map(|j| log_event(block, *pre as usize + j))
                            .collect();
                        expected_finalized.push(block);
                    } else {
                        // Last block: tail inside the open window.
                        for j in 0..*tail as usize {
                            events.push(log_event(block, *pre as usize + j));
                        }
                        expected_applied += *tail as usize;
                        let jj = if *tail > 0 {
                            *pre as usize + *tail as usize - 1
                        } else {
                            *pre as usize - 1
                        };
                        last_reserves = reserves(block, jj);
                        last_block = block;
                    }
                }

                pump.run_test_loop(stream::iter(events).boxed(), 5).await;
                drainer_settle(|| sink.finalized.lock().unwrap().len() >= expected_finalized.len())
                    .await;

                // No-landmine: lateness never trips a fatal path.
                if shutdown.load(Ordering::Relaxed) {
                    return Err("synthetic lateness must never shut the pump down".to_owned());
                }
                // I5: exactly one finalize per tombstoned block, none else.
                let finalized: Vec<u64> = sink
                    .finalized
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(b, _)| *b)
                    .collect();
                if finalized != expected_finalized {
                    return Err(format!(
                        "one finalize per block, in order: got {finalized:?}, want {expected_finalized:?}"
                    ));
                }
                // I7: cutoff monotone at the last tombstone.
                let cutoff = bot.state_arc().read_at(crate::bot_core::state_lock::LockSite::Pump).pump_complete_cutoff();
                if cutoff != base + plan.len() as u64 - 2 {
                    return Err(format!(
                        "delivery cutoff must rest at the last tombstone: got {cutoff}"
                    ));
                }
                // I4: applies == in-window deliveries exactly.
                let recorded = sink.logs_recorded();
                if recorded != expected_applied {
                    return Err(format!(
                        "only in-window logs may apply (I4): got {recorded}, want {expected_applied}"
                    ));
                }
                // The last LEGITIMATELY applied log owns pool state.
                let snap = bot.state_arc().read_at(crate::bot_core::state_lock::LockSite::Pump).v2_snapshot(pool_id);
                if snap != Some((last_reserves.0, last_reserves.1, last_block)) {
                    return Err(format!(
                        "pool state holds the last in-window apply, not a late tail: got {snap:?}"
                    ));
                }
                Ok(())
            };
            if let Err(msg) = rt.block_on(check) {
                return Err(proptest::test_runner::TestCaseError::fail(msg));
            }
        }
    }

    #[tokio::test]
    async fn reorg_log_restores_pool_via_coordinator_and_pump_continues() {
        let pool_addr = Address::from([0x11u8; 20]);
        let (bot, pool_id) = bot_with_registered_v2(pool_addr, 5);

        // Forward Sync at block 7 — misprices the pool and seeds the journal
        // genesis(5) → transition(7). Drive through the *pump* (not
        // `bot.dispatch_log` directly) so the same code path that handles
        // live WS logs is exercised.
        let (mut pump, _sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
        let forward = make_v2_sync_log(pool_addr, U256::from(1_500), U256::from(2_500), 7, false);
        // Stream ends immediately after the log; the loop returns via the
        // `Ok(None)` arm (both subscription streams ended) once the reorg
        // branch `continue`s and the stream is exhausted.
        let combined = stream::iter(vec![WsEvent::Pool(PoolEvent::from_log(forward))]).boxed();
        pump.run_test_loop(combined, 5).await;

        assert!(
            bot.active_delta()
                .snapshot_keys()
                .contains(&AffectedKey::new(HopType::V2, pool_id)),
            "forward Sync through the pump recorded the pool into the EpochDelta"
        );
        assert_eq!(
            bot.state_arc()
                .read_at(crate::bot_core::state_lock::LockSite::Pump)
                .v2_snapshot(pool_id),
            Some((U256::from(1_500), U256::from(2_500), 7)),
            "forward Sync applied through the pump",
        );
        assert!(
            !shutdown.load(Ordering::Relaxed),
            "no reorg yet — pump running"
        );

        // Reorg: a removed-flag Sync at block 7 rolls back to genesis. Build a
        // fresh pump over the SAME bot (the journal + state persist on `Bot`)
        // and feed only the removed log.
        let (mut pump, _sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
        let reorg_log = make_v2_sync_log(
            pool_addr,
            U256::from(1_500), // content unused — block + pool identity matter
            U256::from(2_500),
            7,
            true,
        );
        let combined = stream::iter(vec![WsEvent::Pool(PoolEvent::from_log(reorg_log))]).boxed();
        pump.run_test_loop(combined, 5).await;

        assert!(
            bot.active_delta()
                .snapshot_keys()
                .contains(&AffectedKey::new(HopType::V2, pool_id)),
            "reorg re-recorded the restored pool into the EpochDelta"
        );
        assert_eq!(
            bot.state_arc()
                .read_at(crate::bot_core::state_lock::LockSite::Pump)
                .v2_snapshot(pool_id),
            Some((U256::from(1_000), U256::from(2_000), 5)),
            "reorg rolled back to genesis reserves",
        );
        assert!(
            !shutdown.load(Ordering::Relaxed),
            "in-journal-depth reorg is non-fatal — pump did NOT shut down"
        );
    }

    /// A too-deep reorg (the removed log's block is at/below the journal's
    /// earliest delta) returns `Err(NoStatePriorToBlock)` from the
    /// coordinator; the pump treats this as unrecoverable — it sets the
    /// shutdown flag and returns from `run_with_stream` so Python observes
    /// the pump task exiting, rather than continuing with stale state.
    #[tokio::test]
    async fn too_deep_reorg_shuts_down_pump_gracefully() {
        let pool_addr = Address::from([0x22u8; 20]);
        // Genesis anchored at block 5 — restore_before_block(5) is too deep
        // (nothing the journal can land on prior to the genesis delta).
        let (bot, _pool_id) = bot_with_registered_v2(pool_addr, 5);

        let (mut pump, _sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
        // Removed-flag Sync at block 5 → coordinator restores before 5, which
        // is at the journal's genesis floor → `Err(NoStatePriorToBlock)`.
        let reorg_log = make_v2_sync_log(pool_addr, U256::from(1_500), U256::from(2_500), 5, true);
        let combined = stream::iter(vec![WsEvent::Pool(PoolEvent::from_log(reorg_log))]).boxed();
        pump.run_test_loop(combined, 5).await;

        assert!(
            shutdown.load(Ordering::Relaxed),
            "too-deep reorg must set the shutdown flag",
        );
        // `run_test_loop` returned (this assert is reached), proving the pump
        // exited its loop instead of looping forever on a fatal reorg.
    }

    /// ADR-008 D3 pump-level: a contiguous `removed: true` chunk (delivered
    /// in REVERSE log-index order — nodes may emit reorg events unordered)
    /// enters + continues the reorg path, restoring the pool per-event via the
    /// coordinator; the first `removed: false` event after entry closes the
    /// window, its block becomes the new head, and the pump CONTINUES (no
    /// shutdown). The forward log at the new head re-applies against the
    /// restored state.
    #[tokio::test]
    async fn reorg_contiguous_chunk_closes_on_first_forward_and_continues() {
        let pool_addr = Address::from([0x33u8; 20]);
        // Genesis anchored at block 5: reserves (1000, 2000).
        let (bot, pool_id) = bot_with_registered_v2(pool_addr, 5);
        let snapshot = || {
            bot.state_arc()
                .read_at(crate::bot_core::state_lock::LockSite::Pump)
                .v2_snapshot(pool_id)
        };

        // Drive 5 -> 7 (forward sync at 7) -> tombstone 7 via a forward sync
        // at 8 (advance_to_drained(7) follows the tombstone).
        let (mut pump, _sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
        let s7 = make_v2_sync_log(pool_addr, U256::from(1_500), U256::from(2_500), 7, false);
        let s8 = make_v2_sync_log(pool_addr, U256::from(1_600), U256::from(2_600), 8, false);
        let combined = stream::iter(vec![
            WsEvent::Pool(PoolEvent::from_log(s7)),
            WsEvent::Pool(PoolEvent::from_log(s8)),
        ])
        .boxed();
        pump.run_test_loop(combined, 5).await;
        assert!(
            bot.active_delta()
                .snapshot_keys()
                .contains(&AffectedKey::new(HopType::V2, pool_id)),
            "forward syncs recorded the pool into the EpochDelta"
        );
        assert_eq!(snapshot(), Some((U256::from(1_600), U256::from(2_600), 8)));
        assert!(!shutdown.load(Ordering::Relaxed));

        // Reorg over blocks 7 and 8: removed logs arrive in REVERSE order
        // (8 then 7), then the first removed:false at block 9 closes it.
        let (mut pump, _sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
        let r8 = make_v2_sync_log(pool_addr, U256::from(9), U256::from(9), 8, true);
        let r7 = make_v2_sync_log(pool_addr, U256::from(9), U256::from(9), 7, true);
        let s9 = make_v2_sync_log(pool_addr, U256::from(1_700), U256::from(2_700), 9, false);
        let combined = stream::iter(vec![
            WsEvent::Pool(PoolEvent::from_log(r8)),
            WsEvent::Pool(PoolEvent::from_log(r7)),
            WsEvent::Pool(PoolEvent::from_log(s9)),
        ])
        .boxed();
        pump.run_test_loop(combined, 5).await;

        // The reorg unwound 7 and 8 (restore to genesis), then the forward
        // sync at 9 re-applied -> reserves reflect block 9's values.
        assert_eq!(snapshot(), Some((U256::from(1_700), U256::from(2_700), 9)));
        assert!(
            !shutdown.load(Ordering::Relaxed),
            "reorg path closed cleanly — pump did NOT shut down"
        );
    }

    /// HJ5HWF pump-level (supersedes the retired ADR-008 D3 hard-fault
    /// behavior, the no-landmine ruling): a `removed: false` log on a
    /// tombstoned block (NOT a reorg) is delivery-jitter LATENESS. The pump
    /// takes the benign late-admit path: the late log is dropped UN-applied,
    /// counted (`degenbot.late_log.admitted` + the deduped `late_log`
    /// bucket, mapped in the trace as `LateAdmitDropped`), and the pump
    /// KEEPS RUNNING. No silent re-apply: the cutoff never regresses and the
    /// pool's state pins are untouched by the late survivor.
    #[tokio::test]
    async fn late_forward_log_on_tombstoned_block_is_benign_late_admit() {
        let pool_addr = Address::from([0x44u8; 20]);
        let (bot, pool_id) = bot_with_registered_v2(pool_addr, 5);

        // Single pump session: forward sync(7) opens block 7; forward sync(8)
        // tombstones 7 (open block becomes 8); THEN a forward (removed:false)
        // sync at block 7 arrives late — block 7 is tombstoned and the open
        // block is 8 -> late forward -> benign late-admit drop.
        let (mut pump, sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
        let s7 = make_v2_sync_log(pool_addr, U256::from(1_500), U256::from(2_500), 7, false);
        let s8 = make_v2_sync_log(pool_addr, U256::from(1_600), U256::from(2_600), 8, false);
        let late = make_v2_sync_log(pool_addr, U256::from(9_999), U256::from(9_999), 7, false);
        let combined = stream::iter(vec![
            WsEvent::Pool(PoolEvent::from_log(s7)),
            WsEvent::Pool(PoolEvent::from_log(s8)),
            WsEvent::Pool(PoolEvent::from_log(late)),
        ])
        .boxed();
        pump.run_test_loop(combined, 5).await;
        drainer_settle(|| sink.logs_recorded() >= 2).await;

        assert!(
            !shutdown.load(Ordering::Relaxed),
            "late removed:false on a tombstoned block is counted lateness — the pump must keep running (HJ5HWF)"
        );
        // Exactly the two in-window logs applied; the late survivor dropped.
        assert_eq!(
            sink.logs_recorded(),
            2,
            "the late forward must never dispatch (I4: Streaming-only writes)"
        );
        // Cutoff rests at the last tombstone — the late log for 7 moved
        // neither the cutoff nor the pool state (I7).
        assert_eq!(
            bot.state_arc()
                .read_at(crate::bot_core::state_lock::LockSite::Pump)
                .pump_complete_cutoff(),
            7,
            "cutoff rests at the tombstoned block 7"
        );
        assert_eq!(
            bot.state_arc()
                .read_at(crate::bot_core::state_lock::LockSite::Pump)
                .v2_snapshot(pool_id),
            Some((U256::from(1_600), U256::from(2_600), 8)),
            "pool state holds the last in-window apply, never the late survivor"
        );
    }

    /// BQ7ZBC — FSM RECOVERY green path: after the header-staleness watchdog
    /// performs an authoritative catch-up to block 102 (`recovery_anchor = 102`),
    /// a recovering WS flushes a buffered forward Sync log at block 102 (≤ the
    /// anchor). It is a single-writer duplicate of the already-applied backfill
    /// and MUST be dropped — NOT a false ADR-008 D3 shutdown.
    ///
    /// This is the exact observed failure (block 25670138): catch-up OWNs the
    /// range, the delayed WS re-delivers it, and the pump must discard.
    #[tokio::test]
    async fn recovery_single_writer_discards_stale_forward_after_backfill() {
        let pool_addr = Address::from([0x44u8; 20]);
        let (_bot, _pool_id) = bot_with_registered_v2(pool_addr, 5);

        let (mut pump, _sink, asserter, shutdown) = pump_for_test_sink_and_asserter(Some(100));
        pump.set_header_staleness_for_test(Duration::from_millis(100));

        // Watchdog path: `get_block_number` → 102 (triggers backfill), then
        // `get_logs(102)` → [] (recovery_anchor = 102). Extra `0x66` pads later
        // ticks (current already 102 → latest>current false → no second backfill).
        asserter.push_success(&"0x66".to_string()); // eth_blockNumber → 102
        asserter.push_success(&Vec::<Log>::new()); // eth_getLogs(102) → []
        asserter.push_success(&"0x66".to_string());
        asserter.push_success(&"0x66".to_string());
        asserter.push_success(&"0x66".to_string());

        // Header 101 (anchor), then silence so the watchdog backfills to 102,
        // then the recovering WS flushes a STALE forward sync at block 102.
        let stale = make_v2_sync_log(pool_addr, U256::from(9_999), U256::from(9_999), 102, false);
        let combined = stream::unfold(0u8, move |phase| {
            let stale = stale.clone();
            async move {
                match phase {
                    0 => Some((
                        WsEvent::BlockHeader {
                            number: 101,
                            timestamp: 1,
                            base_fee_per_gas: None,
                            gas_used: 0,
                            gas_limit: 0,
                        },
                        1,
                    )),
                    1 => {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        Some((WsEvent::Pool(PoolEvent::from_log(stale)), 2))
                    }
                    _ => None,
                }
            }
        })
        .boxed();

        pump.run_test_loop(combined, 100).await;

        assert!(
            !shutdown.load(Ordering::Relaxed),
            "a stale forward ≤ recovery_anchor (single-writer duplicate) must be discarded, not fatal (BQ7ZBC)"
        );
    }

    /// BQ7ZBC × HJ5HWF — FSM guard: the single-writer discard is scoped to
    /// blocks the pump itself backfilled (≤ `recovery_anchor`). The
    /// header-staleness watchdog catch-up anchors at 102, then a
    /// `removed:false` forward at block 103 arrives late (103 tombstoned by
    /// 104, 103 > 102) — OUTSIDE the silent single-writer duplicate class, so
    /// it lands in the BENIGN late-admit drop: dropped un-applied, counted in
    /// `degenbot.late_log.admitted`, pump keeps running (the no-landmine
    /// ruling supersedes the retired ADR-008 D3 hard fault).
    #[tokio::test]
    async fn recovery_anchor_stale_forward_above_anchor_is_benign_late_admit() {
        let pool_addr = Address::from([0x44u8; 20]);
        let (_bot, _pool_id) = bot_with_registered_v2(pool_addr, 5);

        let (mut pump, _sink, asserter, shutdown) = pump_for_test_sink_and_asserter(Some(100));
        pump.set_header_staleness_for_test(Duration::from_millis(100));

        asserter.push_success(&"0x66".to_string()); // eth_blockNumber → 102
        asserter.push_success(&Vec::<Log>::new()); // eth_getLogs(102) → []
        asserter.push_success(&"0x66".to_string());
        asserter.push_success(&"0x66".to_string());
        asserter.push_success(&"0x66".to_string());

        let s103 = make_v2_sync_log(pool_addr, U256::from(1_500), U256::from(2_500), 103, false);
        let s104 = make_v2_sync_log(pool_addr, U256::from(1_600), U256::from(2_600), 104, false);
        let late103 = make_v2_sync_log(pool_addr, U256::from(9_999), U256::from(9_999), 103, false);
        let combined = stream::unfold(0u8, move |phase| {
            let s103 = s103.clone();
            let s104 = s104.clone();
            let late103 = late103.clone();
            async move {
                match phase {
                    0 => Some((
                        WsEvent::BlockHeader {
                            number: 101,
                            timestamp: 1,
                            base_fee_per_gas: None,
                            gas_used: 0,
                            gas_limit: 0,
                        },
                        1,
                    )),
                    1 => {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        Some((WsEvent::Pool(PoolEvent::from_log(s103)), 2))
                    }
                    2 => Some((WsEvent::Pool(PoolEvent::from_log(s104)), 3)),
                    3 => Some((WsEvent::Pool(PoolEvent::from_log(late103)), 4)),
                    _ => None,
                }
            }
        })
        .boxed();

        pump.run_test_loop(combined, 100).await;

        assert!(
            !shutdown.load(Ordering::Relaxed),
            "a stale forward ABOVE recovery_anchor is counted lateness — the pump keeps running (HJ5HWF no-landmine)"
        );
    }

    /// BQ7ZBC — FULL FSM lifecycle on a mocked websocket. One session drives
    /// `LIVE → RESET/CATCH_UP → back-to-LIVE`:
    ///   1. LIVE: a forward Sync@102 is applied (reserves 1500/2500).
    ///   2. Stall → the header-staleness watchdog does an authoritative catch-up
    ///      to block 103 (mocked `eth_blockNumber`/`eth_getLogs`), setting
    ///      `recovery_anchor = 103` (the RESET transition).
    ///   3. back-to-LIVE: a fresh Sync@104 (> anchor) is applied (reserves
    ///      2600/3600).
    ///   4. A recovering WS then flushes a STALE Sync@103 (9999/9999, ≤ anchor).
    ///      The single-writer discard must DROP it — if it were re-asserted it
    ///      would overwrite the pool reserves back to the older 9999/9999.
    /// Asserts: the pump did NOT shut down (it survived the recovery) AND the
    /// V2 pool reserves are 2600/3600 (the stale log was not re-applied).
    #[tokio::test]
    async fn fsm_lifecycle_recovers_and_does_not_reassert_stale() {
        let pool_addr = Address::from([0x44u8; 20]);

        let (mut pump, _sink, asserter, shutdown) = pump_for_test_sink_and_asserter(Some(100));
        pump.set_header_staleness_for_test(Duration::from_millis(100));
        // Register the pool on the pump's OWN bot (the one it applies logs to),
        // so the V2 reserves reflect the dispatched Sync events.
        let bot = pump.bot_arc_for_test();
        {
            let state = bot.state_arc();
            let mut core = state.write_at(crate::bot_core::state_lock::LockSite::Pump);
            core.register_v2_pool(&RegisterV2PoolParams {
                address: pool_addr,
                token0: Address::from([0xa0u8; 20]),
                token1: Address::from([0xa1u8; 20]),
                reserve0: U112::from(1_000),
                reserve1: U112::from(2_000),
                fee_token0: (997, 1000),
                fee_token1: (997, 1000),
                factory: Address::from([0xf0u8; 20]),
                update_block: 100,
                variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
                stable_swap: false,
                fee_denominator: None,
                ..Default::default()
            })
            .expect("test setup: V2 registration");
        }

        // Watchdog catch-up to 103: `get_block_number` → 103, then `get_logs`
        // for the range → []. Extra `0x67` pads later ticks (once caught up,
        // `latest > current` is false → no second backfill).
        asserter.push_success(&"0x67".to_string()); // eth_blockNumber → 103
        asserter.push_success(&Vec::<Log>::new()); // eth_getLogs(range) → []
        asserter.push_success(&"0x67".to_string());
        asserter.push_success(&"0x67".to_string());
        asserter.push_success(&"0x67".to_string());

        let s102 = make_v2_sync_log(pool_addr, U256::from(1_500), U256::from(2_500), 102, false);
        let s104 = make_v2_sync_log(pool_addr, U256::from(2_600), U256::from(3_600), 104, false);
        let stale103 =
            make_v2_sync_log(pool_addr, U256::from(9_999), U256::from(9_999), 103, false);
        let combined = stream::unfold(0u8, move |phase| {
            let s102 = s102.clone();
            let s104 = s104.clone();
            let stale103 = stale103.clone();
            async move {
                match phase {
                    0 => Some((
                        WsEvent::BlockHeader {
                            number: 101,
                            timestamp: 1,
                            base_fee_per_gas: None,
                            gas_used: 0,
                            gas_limit: 0,
                        },
                        1,
                    )),
                    1 => Some((WsEvent::Pool(PoolEvent::from_log(s102)), 2)),
                    2 => {
                        // Stall: let the watchdog catch up, then the WS resumes.
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        Some((WsEvent::Pool(PoolEvent::from_log(s104)), 3))
                    }
                    3 => Some((WsEvent::Pool(PoolEvent::from_log(stale103)), 4)),
                    _ => None,
                }
            }
        })
        .boxed();

        pump.run_test_loop(combined, 100).await;

        assert!(
            !shutdown.load(Ordering::Relaxed),
            "the FSM must survive a stall-recovery and stay alive (BQ7ZBC)"
        );
        // The stale Sync@103 must NOT have been re-asserted: final reserves are
        // those of the last applied forward (Sync@104), not the stale 9999/9999.
        let state = bot.state_arc();
        let core = state.read_at(crate::bot_core::state_lock::LockSite::Pump);
        let pool_id = *core.pool_addresses.get(&pool_addr).unwrap();
        if let Some(crate::bot_core::PoolEntry::V2(p)) = core.pools.get(&pool_id) {
            let pool = &p.1;
            assert_eq!(
                pool.reserve0.to::<u128>(),
                2_600,
                "stale forward ≤ recovery_anchor must be dropped, not re-asserted (BQ7ZBC)"
            );
            assert_eq!(
                pool.reserve1.to::<u128>(),
                3_600,
                "stale forward ≤ recovery_anchor must be dropped, not re-asserted (BQ7ZBC)"
            );
        } else {
            panic!("test setup: V2 pool not found for {pool_addr}");
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
        let inner = alloy::primitives::Log::new_unchecked(
            pool_address,
            vec![V3_SWAP_TOPIC],
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

    /// Scenario A — the normal path: a Mint@N in the WS feed IS buffered by
    /// the pump, the tombstone@N+1 sets `last_complete_block = N`, and the
    /// registration drain+pin captures it. PASSES → the drain/buffer path is
    /// correct for delivered logs. If this test ever FAILS the race (A) is
    /// real and an FSM on the verify seam is the fix.
    #[tokio::test]
    async fn scenario_a_buffered_mint_is_drained_into_pin() {
        let seed_gross: u128 = 10_000_000_000_000_000;
        let delta: u128 = 454_021;
        let block_n = 10u64;
        let (bot, pool_addr) = bot_with_quarantined_v3_tracked(seed_gross, block_n - 1);
        let (mut pump, _sink, _shutdown) =
            pump_for_test_with_bot(Arc::clone(&bot), Some(block_n - 1));

        // Feed: Mint@N (tick -100..7, +delta) then Swap@N+1 (tombstones N).
        let mint = make_v3_mint_log_with_block(pool_addr, -100, 7, delta, block_n);
        let swap = make_v3_swap_log_with_block(pool_addr, block_n + 1);
        let combined = stream::iter(vec![
            WsEvent::Pool(PoolEvent::from_log(mint)),
            WsEvent::Pool(PoolEvent::from_log(swap)),
        ])
        .boxed();
        pump.run_test_loop(combined, block_n - 1).await;

        // The tombstone@N+1 set `last_complete_block = N`. Drain + pin.
        let (tick_data, pinned_block) = {
            let state = bot.state_arc();
            let mut core = state.write_at(crate::bot_core::state_lock::LockSite::Pump);
            core.apply_backfill_buffer_v3(&pool_addr);
            core.apply_pump_buffer_v3(&pool_addr);
            core.pin_v3_post_drain_snapshot(pool_addr);
            core.take_v3_post_drain_snapshot(pool_addr)
                .expect("Tracked pool pins after drain")
        };
        assert_eq!(pinned_block, block_n, "pin's update_block advanced to N");
        assert_eq!(
            tick_data.get(&7).unwrap().liquidity_gross,
            alloy::primitives::U128::from(seed_gross + delta),
            "scenario A: the buffered Mint WAS drained into the pin"
        );
    }

    /// Scenario C — the EXACT on-chain topology at block 25648846: TWO
    /// same-block Mints where tick 7 is the UPPER tick of one (li=1213,
    /// tl=6,tu=7,amount=454021) and the LOWER tick of the other (li=1215,
    /// tl=7,tu=8,amount=400353245599). On chain, tick-7 gross grows by their
    /// sum (+400353699620). Production pin captured only li=1215's amount
    /// (+400353245599) — missing exactly li=1213's +454021. This test feeds
    /// BOTH Mints in log-index order (li=1213 first, li=1215 second) + the
    /// tombstone Swap@N+1, drains, pins, and asserts BOTH Mints landed in
    /// the pin. If this test FAILS, the pump→drain→pin path drops the first
    /// of two adjacent same-block Mints — the real bug. If it PASSES, the
    /// drop is not in this in-process path (it's a real-bot concurrency /
    /// bucket-boundary issue the test harness can't reach).
    #[tokio::test]
    async fn scenario_c_two_adjacent_same_block_mints_both_applied_to_pin() {
        let seed_gross: u128 = 10_953_626_740_480_101; // on-chain@845
        let amt_lower: u128 = 454_021; // li=1213: tl=6, tu=7 (tick 7 = upper)
        let amt_upper: u128 = 400_353_245_599; // li=1215: tl=7, tu=8 (tick 7 = lower)
        let block_n = 10u64;
        let (bot, pool_addr) = bot_with_quarantined_v3_tracked(seed_gross, block_n - 1);
        let (mut pump, _sink, _shutdown) =
            pump_for_test_with_bot(Arc::clone(&bot), Some(block_n - 1));

        // Feed the two Mints in log-index order (li=1213 then li=1215) then a
        // Swap@N+1 to tombstone N. Pre-seed tick 6 so the tl=6,tu=7 Mint has a
        // lower tick to mutate (mirrors on-chain where tick 6 is initialized).
        {
            let state = bot.state_arc();
            let mut core = state.write_at(crate::bot_core::state_lock::LockSite::Pump);
            let pool_id = *core.pool_addresses.get(&pool_addr).unwrap();
            if let Some(crate::bot_core::PoolEntry::V3(p)) = core.pools.get_mut(&pool_id) {
                use alloy::primitives::U128;
                let pool = &mut p.1;
                pool.tick_data
                    .entry(6)
                    .or_insert(crate::bot_core::TickInfo {
                        liquidity_gross: U128::from(21_446_194_157_938_844u128),
                        liquidity_net: 21_446_194_157_938_844i128,
                        block: 0,
                    });
                pool.tick_data
                    .entry(8)
                    .or_insert(crate::bot_core::TickInfo {
                        liquidity_gross: U128::from(18_506_953_544_795_537u128),
                        liquidity_net: -18_506_953_544_795_537i128,
                        block: 0,
                    });
            }
        }
        let mint_lower = make_v3_mint_log_with_block(pool_addr, 6, 7, amt_lower, block_n);
        let mint_upper = make_v3_mint_log_with_block(pool_addr, 7, 8, amt_upper, block_n);
        let swap = make_v3_swap_log_with_block(pool_addr, block_n + 1);
        let combined = stream::iter(vec![
            WsEvent::Pool(PoolEvent::from_log(mint_lower)),
            WsEvent::Pool(PoolEvent::from_log(mint_upper)),
            WsEvent::Pool(PoolEvent::from_log(swap)),
        ])
        .boxed();
        pump.run_test_loop(combined, block_n - 1).await;

        let (tick_data, pinned_block) = {
            let state = bot.state_arc();
            let mut core = state.write_at(crate::bot_core::state_lock::LockSite::Pump);
            core.apply_backfill_buffer_v3(&pool_addr);
            core.apply_pump_buffer_v3(&pool_addr);
            core.pin_v3_post_drain_snapshot(pool_addr);
            core.take_v3_post_drain_snapshot(pool_addr)
                .expect("Tracked pool pins after drain")
        };
        assert_eq!(pinned_block, block_n, "pin's update_block advanced to N");
        // On-chain@846 tick-7 gross = seed + amt_lower + amt_upper.
        assert_eq!(
            tick_data.get(&7).unwrap().liquidity_gross,
            alloy::primitives::U128::from(seed_gross + amt_lower + amt_upper),
            "scenario C: BOTH adjacent same-block Mints drained into the pin \
             (on-chain@846 value). If this fails with only +amt_upper present, \
             the first of two adjacent same-block Mints is dropped by the \
             pump→drain→pin path."
        );
        // And the per-tick net: tick 7 net = seed_net - amt_lower + amt_upper.
        // And the per-tick net: seed_net - amt_lower (upper tick) + amt_upper (lower tick).
        // The helper seeds tick-7 net = +seed_gross.
        assert_eq!(
            tick_data.get(&7).unwrap().liquidity_net,
            i128::try_from(seed_gross).unwrap() - i128::try_from(amt_lower).unwrap()
                + i128::try_from(amt_upper).unwrap(),
            "scenario C: tick-7 net reflects both Mints (upper: -amt_lower, lower: +amt_upper)"
        );
    }

    /// Scenario B — the WS-drop reproduction: feed Mint1@N but NOT Mint2@N
    /// (simulating a WS transport drop). The drain captures `update_block = N`
    /// (from Mint1) but the pin is missing Mint2 — exactly the production
    /// symptom. A verify vs on-chain@N (which has both) would mismatch. This
    /// confirms the production failure is cause (B), which a verify-seam FSM
    /// does NOT fix (re-draining an empty buffer still misses it).
    #[tokio::test]
    async fn scenario_b_dropped_mint_reproduces_verify_mismatch_symptom() {
        let seed_gross: u128 = 10_000_000_000_000_000;
        let delta1: u128 = 400_000_000_000u128; // the +400M burst
        let delta2: u128 = 454_021; // the ONE missed Mint
        let block_n = 10u64;
        let (bot, pool_addr) = bot_with_quarantined_v3_tracked(seed_gross, block_n - 1);
        let (mut pump, _sink, _shutdown) =
            pump_for_test_with_bot(Arc::clone(&bot), Some(block_n - 1));

        // Feed Mint1@N then Swap@N+1 (tombstones N). Mint2@N is NOT fed —
        // simulating the WS dropping exactly ONE of block N's Mints.
        let mint1 = make_v3_mint_log_with_block(pool_addr, -100, 7, delta1, block_n);
        let swap = make_v3_swap_log_with_block(pool_addr, block_n + 1);
        let combined = stream::iter(vec![
            WsEvent::Pool(PoolEvent::from_log(mint1)),
            WsEvent::Pool(PoolEvent::from_log(swap)),
        ])
        .boxed();
        pump.run_test_loop(combined, block_n - 1).await;

        let (tick_data, pinned_block) = {
            let state = bot.state_arc();
            let mut core = state.write_at(crate::bot_core::state_lock::LockSite::Pump);
            core.apply_backfill_buffer_v3(&pool_addr);
            core.apply_pump_buffer_v3(&pool_addr);
            core.pin_v3_post_drain_snapshot(pool_addr);
            core.take_v3_post_drain_snapshot(pool_addr)
                .expect("Tracked pool pins after drain")
        };
        // The pin advanced to N (from Mint1) but is missing Mint2.
        assert_eq!(pinned_block, block_n, "update_block = N (from Mint1)");
        assert_eq!(
            tick_data.get(&7).unwrap().liquidity_gross,
            alloy::primitives::U128::from(seed_gross + delta1),
            "scenario B: the dropped Mint2 is NOT in the pin — reproduces the symptom"
        );
        // On-chain@N would be seed + delta1 + delta2 (Mint2 was applied
        // on-chain at block N). The pin lacks delta2 → a verify would fatal.
        assert_ne!(
            tick_data.get(&7).unwrap().liquidity_gross,
            alloy::primitives::U128::from(seed_gross + delta1 + delta2),
            "pin diverges from on-chain@N (the production mismatch)"
        );
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

    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        reason = "fuzz-bounded: FUZZ_POOL_COUNT=3 and seed<=48 so these casts cannot truncate/wrap"
    )]
    async fn bamkki_routing_fuzz_oracle_holds_across_lifecycle_roles() {
        type ExpectedTicks = HashMap<i32, (u128, i128)>;
        for seed in 1u64..=48 {
            let mut rng = FuzzRng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let block_n = 100_u64;
            let bot = Arc::new(Bot::new(1));

            // Role assignment per pool rotates with the seed so every role
            // combination is exercised across the iteration space:
            // 0 => Live from start, 1 => unregistered during feed,
            // 2 => Quarantined from start (set_live after drain).
            let roles: Vec<u8> = (0..FUZZ_POOL_COUNT as u8)
                .map(|i| (seed as u8 + i) % 3)
                .collect();
            let addrs: Vec<Address> = (0..FUZZ_POOL_COUNT)
                .map(|i| Address::from([0x40 + i as u8; 20]))
                .collect();

            // Pre-register roles 0 (Live) and 2 (Quarantined).
            for (i, addr) in addrs.iter().enumerate() {
                if roles[i] == 1 {
                    continue;
                }
                let mut tick_data = hashbrown::HashMap::new();
                for &t in &FUZZ_TICKS {
                    tick_data.insert(
                        t,
                        crate::bot_core::TickInfo {
                            liquidity_gross: alloy::primitives::U128::from(FUZZ_SEED_GROSS),
                            liquidity_net: i128::try_from(FUZZ_SEED_GROSS).unwrap(),
                            block: 0,
                        },
                    );
                }
                let state = bot.state_arc();
                let mut core = state.write_at(crate::bot_core::state_lock::LockSite::Pump);
                core.register_v3_pool(&crate::bot_core::RegisterV3PoolParams {
                    address: *addr,
                    token0: Address::from([0xa0u8; 20]),
                    token1: Address::from([0xa1u8; 20]),
                    fee: 500,
                    tick_spacing: 10,
                    factory: Address::from([0xf0u8; 20]),
                    sqrt_price_x96: U256::from(1u128) << 96,
                    liquidity: 1_000_000,
                    tick: 0,
                    tick_data,
                    update_block: block_n - 1,
                    coverage: crate::bot_core::PoolTickCoverage::Tracked,
                    fetcher: None,
                    ..Default::default()
                })
                .expect("fuzz registration");
                if roles[i] == 2 {
                    core.set_v3_pool_quarantined(*addr);
                }
            }

            // Generate the event stream: mints over the pre-seeded tick set +
            // tombstone swaps, distributed across pools/blocks by the seed.
            let mut oracle: Vec<ExpectedTicks> = Vec::new();
            let mut expected_events: Vec<(usize, Log)> = Vec::new();
            let mut log_index = 0u64;
            for i in 0..FUZZ_POOL_COUNT {
                let mut ticks: ExpectedTicks = FUZZ_TICKS
                    .iter()
                    .map(|&t| {
                        (
                            t,
                            (FUZZ_SEED_GROSS, i128::try_from(FUZZ_SEED_GROSS).unwrap()),
                        )
                    })
                    .collect();
                oracle.push(ticks.clone());
                let _ = ticks;
                let _ = &mut ticks;
                oracle[i] = FUZZ_TICKS
                    .iter()
                    .map(|&t| {
                        (
                            t,
                            (FUZZ_SEED_GROSS, i128::try_from(FUZZ_SEED_GROSS).unwrap()),
                        )
                    })
                    .collect();
            }

            for _ in 0..12 {
                let pool_idx = rng.below(FUZZ_POOL_COUNT as u64) as usize;
                let block = block_n - rng.below(3); // blocks N-2..=N
                let is_mint = rng.below(2) == 0;
                if is_mint {
                    let tl = FUZZ_TICKS[rng.below(FUZZ_TICKS.len() as u64) as usize];
                    let tu = tl + 10;
                    let amount: u128 = u128::from(1000_u64 + rng.below(50_000));
                    expected_events.push((
                        pool_idx,
                        make_v3_mint_log_with_block(addrs[pool_idx], tl, tu, amount, block),
                    ));
                    // Oracle replay in arrival order (Solidity Tick.update):
                    let lo = oracle[pool_idx].entry(tl).or_insert((0, 0));
                    lo.0 += amount;
                    lo.1 += amount as i128;
                    let hi = oracle[pool_idx].entry(tu).or_insert((0, 0));
                    hi.0 += amount;
                    hi.1 -= amount as i128;
                } else {
                    expected_events.push((
                        pool_idx,
                        make_v3_swap_log_with_block(addrs[pool_idx], block),
                    ));
                }
                log_index += 1;
            }
            // Tombstone swap at N+1 closes block N for the cutoff.
            let tomb_pool = rng.below(FUZZ_POOL_COUNT as u64) as usize;
            expected_events.push((
                tomb_pool,
                make_v3_swap_log_with_block(addrs[tomb_pool], block_n + 1),
            ));
            // WS delivery is per-block ordered: stable-sort by block so the
            // feed never travels backward (a backward log is an ADR-008 D3
            // unreliable-WS signal, not a fuzz dimension).
            expected_events.sort_by_key(|(_, l)| l.block_number);

            let min_block = expected_events
                .iter()
                .map(|(_, l)| l.block_number.unwrap_or(block_n))
                .min()
                .unwrap_or(block_n);
            let (mut pump, _sink, _shutdown) =
                pump_for_test_with_bot(Arc::clone(&bot), Some(min_block - 1));
            let ws_events: Vec<WsEvent> = expected_events
                .iter()
                .map(|(_, l)| WsEvent::Pool(PoolEvent::from_log(l.clone())))
                .collect();
            let combined = stream::iter(ws_events).boxed();
            pump.run_test_loop(combined, min_block - 1).await;

            // Late registration for role-1 pools (the FUWYUR shape), then the
            // standard staged-application seam for every pool.
            for (i, addr) in addrs.iter().enumerate() {
                if roles[i] != 1 {
                    continue;
                }
                let state = bot.state_arc();
                let mut core = state.write_at(crate::bot_core::state_lock::LockSite::Pump);
                let mut tick_data = hashbrown::HashMap::new();
                for &t in &FUZZ_TICKS {
                    tick_data.insert(
                        t,
                        crate::bot_core::TickInfo {
                            liquidity_gross: alloy::primitives::U128::from(FUZZ_SEED_GROSS),
                            liquidity_net: i128::try_from(FUZZ_SEED_GROSS).unwrap(),
                            block: 0,
                        },
                    );
                }
                core.register_v3_pool(&crate::bot_core::RegisterV3PoolParams {
                    address: *addr,
                    token0: Address::from([0xa0u8; 20]),
                    token1: Address::from([0xa1u8; 20]),
                    fee: 500,
                    tick_spacing: 10,
                    factory: Address::from([0xf0u8; 20]),
                    sqrt_price_x96: U256::from(1u128) << 96,
                    liquidity: 1_000_000,
                    tick: 0,
                    tick_data,
                    update_block: block_n - 1,
                    coverage: crate::bot_core::PoolTickCoverage::Tracked,
                    fetcher: None,
                    ..Default::default()
                })
                .expect("late fuzz registration");
                core.set_v3_pool_quarantined(*addr);
            }
            for addr in &addrs {
                let state = bot.state_arc();
                let mut core = state.write_at(crate::bot_core::state_lock::LockSite::Pump);
                core.apply_backfill_buffer_v3(addr);
                core.apply_pump_buffer_v3(addr);
                core.set_v3_pool_live(*addr);
            }

            // ORACLE COMPARISON.
            for (i, addr) in addrs.iter().enumerate() {
                let state = bot.state_arc();
                let core = state.read_at(crate::bot_core::state_lock::LockSite::Pump);
                let pool_id = *core.pool_addresses.get(addr).unwrap();
                let pool = core.get_v3_pool(pool_id).unwrap();
                for &t in &FUZZ_TICKS {
                    let actual = pool
                        .tick_data
                        .get(&t)
                        .map(|x| (x.liquidity_gross.to::<u128>(), x.liquidity_net.abs()));
                    let want = oracle[i].get(&t).copied().unwrap_or((0, 0));
                    let actual_gross = actual.map_or(want.0, |a| a.0);
                    assert_eq!(
                        actual_gross, want.0,
                        "BAMKKI seed={seed} pool={i} role={} tick={t}: gross diverged \\
                         (lost/duplicated/mis-staged application)",
                        roles[i]
                    );
                }
            }
            let _ = log_index;
        }
    }

    /// FUWYUR RED tracer — live-window Mint for a NOT-YET-REGISTERED pool must
    /// survive late registration.
    ///
    /// Production shape (the 2026-08-25 20:51 UTC ADR-021 trip): crawl is
    /// mid-flight when a Mint lands in block N for a Tracked pool that
    /// `build_paths` has not registered yet; registration happens AFTER N
    /// completed and pins pre-Mint DB data (`tick_data_block = N` with stale
    /// gross). The dual buffer exists precisely for staged application at
    /// registration — but `LogDispatcher::dispatch`'s APPLY-MISS funnel
    /// early-returns BEFORE reaching `apply_v3_liquidity_update`'s
    /// unregistered-buffering arm, so the event never reaches the buffer and
    /// the pool goes Live permanently missing it (UO3JM4 desync class).
    /// Uses the exact on-chain numbers from the trip: pool 0x88e6A0c2 tick
    /// 193370 liquidityGross `244_132_769_082_101_7` -> `256_007_624_942_870_5`.
    #[tokio::test]
    async fn fuwyur_live_mint_for_unregistered_pool_survives_late_registration() {
        use crate::bot_core::{PoolTickCoverage, RegisterV3PoolParams, TickInfo};
        const SEED_GROSS: u128 = 2_441_327_690_821_017;
        const MINT_DELTA: u128 = 118_748_558_607_688;
        let block_n = 10u64;
        let pool_addr = Address::from([0x34u8; 20]);
        // Crawl mid-flight: NOTHING is registered yet.
        let bot = Arc::new(Bot::new(1));
        let (mut pump, _sink, _shutdown) =
            pump_for_test_with_bot(Arc::clone(&bot), Some(block_n - 1));

        // Live WS feed: Mint@N for the not-yet-registered pool, then Swap@N+1
        // to tombstone N (completes the block for the cutoff).
        let mint = make_v3_mint_log_with_block(pool_addr, -100, 7, MINT_DELTA, block_n);
        let swap = make_v3_swap_log_with_block(pool_addr, block_n + 1);
        let combined = stream::iter(vec![
            WsEvent::Pool(PoolEvent::from_log(mint)),
            WsEvent::Pool(PoolEvent::from_log(swap)),
        ])
        .boxed();
        pump.run_test_loop(combined, block_n - 1).await;

        // LATE registration (crawl reaches the pool after block N completed):
        // Tracked pool loads stale DB data and starts Quarantined.
        {
            let state = bot.state_arc();
            let mut core = state.write_at(crate::bot_core::state_lock::LockSite::Pump);
            let mut tick_data = hashbrown::HashMap::new();
            tick_data.insert(
                7,
                TickInfo {
                    liquidity_gross: alloy::primitives::U128::from(SEED_GROSS),
                    liquidity_net: i128::try_from(SEED_GROSS).unwrap(),
                    block: 0,
                },
            );
            core.register_v3_pool(&RegisterV3PoolParams {
                address: pool_addr,
                token0: Address::from([0xa0u8; 20]),
                token1: Address::from([0xa1u8; 20]),
                fee: 500,
                tick_spacing: 10,
                factory: Address::from([0xf0u8; 20]),
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data,
                update_block: block_n - 1,
                coverage: PoolTickCoverage::Tracked,
                fetcher: None,
                ..Default::default()
            })
            .expect("late V3 registration");
            core.set_v3_pool_quarantined(pool_addr);
        }

        // Registration drain+pin seam, then set_live flush of the retained
        // tail — the standard staged-application contract.
        let tick_data = {
            let state = bot.state_arc();
            let mut core = state.write_at(crate::bot_core::state_lock::LockSite::Pump);
            core.apply_backfill_buffer_v3(&pool_addr);
            core.apply_pump_buffer_v3(&pool_addr);
            core.set_v3_pool_live(pool_addr);
            let pool_id = *core.pool_addresses.get(&pool_addr).unwrap();
            core.get_v3_pool(pool_id)
                .expect("registered")
                .tick_data
                .clone()
        };
        assert_eq!(
            tick_data.get(&7).expect("tick 7 seeded").liquidity_gross,
            alloy::primitives::U128::from(SEED_GROSS + MINT_DELTA),
            "FUWYUR: the live-window Mint for a not-yet-registered pool must \
             reach the pump buffer and land via the registration drain/flush. \
             RED means dispatch's APPLY-MISS funnel silently dropped it."
        );
    }

    /// Scenario A-race — the concurrent drain window: spawn the pump, feed
    /// Mint1@N, run the registration drain (cutoff < N so Mint1 is RETAINED,
    /// not drained), then feed Mint2@N + Swap@N+1. The pin captures
    /// `update_block = backfill block` (< N) — NOT the production symptom
    /// (which has `update_block = N`). This PROVES the race cannot produce
    /// the observed symptom: a pin at `update_block = N` requires the
    /// tombstone to have fired (cutoff = N), and the tombstone can only fire
    /// AFTER all of N's logs were dispatched (else `LateForward` — the benign
    /// late-admit drop, HJ5HWF). So all
    /// delivered Mints@N are drained together. The missing Mint must have
    /// been never delivered (scenario B).
    #[tokio::test]
    async fn scenario_a_race_concurrent_drain_cannot_produce_symptom() {
        use tokio::sync::oneshot;
        let seed_gross: u128 = 10_000_000_000_000_000;
        let delta1: u128 = 400_000_000_000u128;
        let delta2: u128 = 454_021;
        let block_n = 10u64;
        let (bot, pool_addr) = bot_with_quarantined_v3_tracked(seed_gross, block_n - 1);
        let (mut pump, _sink, _shutdown) =
            pump_for_test_with_bot(Arc::clone(&bot), Some(block_n - 1));

        let mint1 = make_v3_mint_log_with_block(pool_addr, -100, 7, delta1, block_n);
        let mint2 = make_v3_mint_log_with_block(pool_addr, -200, 7, delta2, block_n);
        let swap = make_v3_swap_log_with_block(pool_addr, block_n + 1);

        // Stream: Mint1@N, then await the drain-done signal, then Mint2@N +
        // Swap@N+1 (tombstone), then end. The pump dispatches Mint1 into the
        // buffer (cutoff < N), parks on the oneshot receive; the test runs
        // the drain+pin (cutoff < N → Mint1 retained); then signals.
        let (drain_done_tx, drain_done_rx) = oneshot::channel::<()>();
        let logs: Vec<Log> = vec![mint1, mint2, swap];
        let combined = stream::unfold(
            (0u8, Some(drain_done_rx), logs.into_iter()),
            |(phase, rx_opt, mut logs)| async move {
                match phase {
                    0 => Some((
                        WsEvent::Pool(PoolEvent::from_log(logs.next().unwrap())),
                        (1, rx_opt, logs),
                    )),
                    1 => {
                        let _ = rx_opt.unwrap().await; // park until drain completes
                        Some((
                            WsEvent::Pool(PoolEvent::from_log(logs.next().unwrap())),
                            (2, None, logs),
                        ))
                    }
                    2 => Some((
                        WsEvent::Pool(PoolEvent::from_log(logs.next().unwrap())),
                        (3, None, logs),
                    )),
                    _ => None,
                }
            },
        )
        .boxed();
        let pump_handle = tokio::spawn(async move {
            pump.run_test_loop(combined, block_n - 1).await;
        });
        // Let the pump process Mint1 (cutoff still < N — no tombstone yet).
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Run the registration drain+pin NOW (cutoff < N → Mint1 retained,
        // NOT drained). The pin captures the backfill seed state.
        let pin_after_mint1 = {
            let state = bot.state_arc();
            let mut core = state.write_at(crate::bot_core::state_lock::LockSite::Pump);
            core.apply_backfill_buffer_v3(&pool_addr);
            core.apply_pump_buffer_v3(&pool_addr);
            core.pin_v3_post_drain_snapshot(pool_addr);
            core.take_v3_post_drain_snapshot(pool_addr)
        };
        // Release the pump to feed Mint2 + Swap (tombstone N, cutoff = N).
        let _ = drain_done_tx.send(());
        let _ = pump_handle.await;

        // The pin captured at the race window has update_block = backfill
        // block (< N), NOT N — because cutoff was < N at drain time, Mint1
        // was retained. This is NOT the production symptom (update_block = N).
        let (tick_data, pinned_block) = pin_after_mint1.expect("pin captured");
        assert_eq!(
            pinned_block,
            block_n - 1,
            "race drain (cutoff < N) pins the backfill block, NOT N — not the symptom"
        );
        assert_eq!(
            tick_data.get(&7).unwrap().liquidity_gross,
            alloy::primitives::U128::from(seed_gross),
            "race drain retained Mint1 (cutoff < N) — pin has only the seed"
        );

        // After the tombstone, a SECOND drain (cutoff = N) drains both
        // retained Mints onto the LIVE state — proving they were buffered,
        // just not drained into the pin.
        let live_gross = {
            let state = bot.state_arc();
            let mut core = state.write_at(crate::bot_core::state_lock::LockSite::Pump);
            core.apply_pump_buffer_v3(&pool_addr);
            let pool_id = *core.pool_addresses.get(&pool_addr).unwrap();
            core.get_v3_pool(pool_id)
                .unwrap()
                .tick_data
                .get(&7)
                .unwrap()
                .liquidity_gross
        };
        assert_eq!(
            live_gross,
            alloy::primitives::U128::from(seed_gross + delta1 + delta2),
            "both Mints WERE buffered — a post-tombstone drain recovers them onto live state"
        );
    }

    // -----------------------------------------------------------------
    // ADR-008 D2: `LogsQuiesced` solver-release gate.
    //
    // The pump must publish (`on_send`) only when the open block is
    // quiesced (all dispatched logs fully applied), and coalesce a burst of
    // same-block logs into ONE publish at the burst tail (not once per log).
    // Re-arm on straggler is covered at the clock level by
    // `consume_quiesced_publishes_once_per_cycle_and_re_arms_on_straggler`.
    // -----------------------------------------------------------------

    /// 3 same-block logs in a tight burst → exactly ONE `on_send`, fired at
    /// the burst tail (after the 3rd log applies + the stream settles), NOT
    /// 3× (one per log) and NOT zero. RED against the wall-clock timer: with
    /// `stream::iter` (no delay between events) the `DEBOUNCE_MS` timer
    /// never fires before the stream ends, so `on_send` is never called.
    #[tokio::test]
    async fn burst_of_logs_publishes_once_at_tail_via_quiesce_gate() {
        let (mut pump, sink) = pump_for_test(Some(100));
        let pool_addr = Address::from([0x55u8; 20]);
        let mk = |r0, r1| {
            WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
                pool_addr,
                U256::from(r0),
                U256::from(r1),
                101,
                false,
            )))
        };
        // 3 same-block sync logs, then stream exhaustion. Under the wall-clock
        // debounce the timer never fires before Ok(None) returns → 0
        // sends. Under the quiesce gate, after the 3rd log applies the settle
        // probe (timeout(ZERO) on the exhausted stream) flushes on_send once.
        let combined =
            stream::iter(vec![mk(1_500, 2_500), mk(1_600, 2_600), mk(1_700, 2_700)]).boxed();
        pump.run_test_loop(combined, 100).await;
        drainer_settle(|| !sink.sent.lock().unwrap().is_empty()).await;

        let sent = sink.sent.lock().unwrap().clone();
        assert_eq!(
            sent.len(),
            1,
            "a 3-log burst publishes exactly once at the tail via the quiesce \
             gate (got {} sends)",
            sent.len()
        );
    }
    /// BO5FBS publish-gate interaction: the newHead-driven eager solve is
    /// distinct from the publish gate. With the promotion live, `on_drain`
    /// fires eagerly at the promoted block (`pool_state_head` 500) on the
    /// `LogsArriving` path, but NO publish (`on_send`) occurs until a forward
    /// log quiesces the block (ADR-008 D2). A header with no log must never
    /// leak a publish — newHead is a promote/liveness signal, never a
    /// completeness signal.
    #[tokio::test]
    async fn newhead_promoted_solve_does_not_publish_until_quiesced() {
        use alloy::primitives::{aliases::U112, Address as A};
        use stream::StreamExt;
        let bot = Arc::new(Bot::new(1));
        {
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
                update_block: 500,
                variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
                stable_swap: false,
                fee_denominator: None,
                ..Default::default()
            })
            .expect("test setup: V2 registration");
        }
        let (mut pump, sink, _shutdown) = pump_for_test_with_bot(bot, Some(100));
        sink.set_dirty(true);

        // newHead(101) only — no log for 101, so the block is LogsArriving
        // (open), not quiesced.
        let events: Vec<WsEvent> = vec![WsEvent::BlockHeader {
            number: 101,
            timestamp: 101_000,
            base_fee_per_gas: Some(1_000_000_001),
            gas_used: 10_000_001,
            gas_limit: 30_000_001,
        }];
        let combined = stream::iter(events).boxed();
        pump.run_test_loop(combined, 100).await;
        drainer_settle(|| !sink.drained_blocks().is_empty()).await;

        let drained = sink.drained_blocks();
        assert!(
            !drained.is_empty(),
            "dirty sink fires the eager newHead-driven solve"
        );
        assert!(
            drained.iter().all(|&b| b == 500),
            "eager solve anchors to the promoted active_block (500)"
        );
        let sent = sink.sent.lock().unwrap().clone();
        assert!(
            sent.is_empty(),
            "no publish during LogsArriving: a header alone must never leak \
             an on_send (quiesce gate), got {sent:?}"
        );
    }

    /// FD7NFG: `backfill_from_snapshot` no-op when no snapshot loaded (cold
    /// start — `snapshot_seed_block = None`). Default fresh `Bot` has S=None.
    #[tokio::test]
    async fn backfill_from_snapshot_cold_start_is_noop() {
        let (pump, _sink) = pump_for_test(None);
        // Fresh Bot: snapshot_seed_block is None → no-op, no provider call.
        let n = pump.backfill_from_snapshot(100, 10).await.unwrap();
        assert_eq!(n, 0, "cold start (S=None) → no blocks backfilled");
    }

    /// FD7NFG: `backfill_from_snapshot` no-op when `S >= W` (snapshot at/after
    /// the WS block — nothing to backfill).
    #[tokio::test]
    async fn backfill_from_snapshot_s_ge_w_is_noop() {
        let (pump, _sink) = pump_for_test(None);
        // Inject S = W (snapshot caught up to the WS block).
        {
            let bot = pump.bot_arc_for_test();
            bot.state_arc()
                .write_at(crate::bot_core::state_lock::LockSite::Pump)
                .set_snapshot_seed_block(Some(100));
        }
        let n = pump.backfill_from_snapshot(100, 10).await.unwrap();
        assert_eq!(n, 0, "S >= W → nothing to backfill");
    }

    /// FD7NFG: `backfill_from_snapshot` no-op when `S = 0` (degenerate
    /// snapshot block — guarded to avoid a `from_block=1` unbounded fetch).
    #[tokio::test]
    async fn backfill_from_snapshot_s_zero_is_noop() {
        let (pump, _sink) = pump_for_test(None);
        {
            let bot = pump.bot_arc_for_test();
            bot.state_arc()
                .write_at(crate::bot_core::state_lock::LockSite::Pump)
                .set_snapshot_seed_block(Some(0));
        }
        let n = pump.backfill_from_snapshot(100, 10).await.unwrap();
        assert_eq!(n, 0, "S = 0 → skip (degenerate)");
    }

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

    /// J3FMDO: `resume_from_subscribe` auto-backfills the snapshot→WS gap
    /// (S < W) before the live loop begins — proving the core path closes the
    /// gap with zero Python orchestration. The Asserter queue drains by exactly
    /// one `eth_getLogs` response (S+1..W fits in a single default-size chunk).
    #[tokio::test]
    async fn auto_backfill_runs_inside_resume_when_s_lt_w() {
        let bot = Arc::new(Bot::new(1));
        bot.state_arc()
            .write_at(crate::bot_core::state_lock::LockSite::Pump)
            .set_snapshot_seed_block(Some(85));
        let (mut pump, _sink, _shutdown, asserter) = pump_for_test_with_asserter(bot, None);

        // The single eth_getLogs chunk (blocks 86..99, ≤ DEFAULT_BACKFILL_CHUNK_SIZE)
        // returns an empty log array — the pump's provider drains this response.
        asserter.push_success(&Vec::<Log>::new());

        let combined = stream::iter(Vec::<WsEvent>::new()).boxed();
        let state = SubscribeState {
            first_block: 100,
            first_timestamp: 0,
            combined_stream: Some(combined),
        };
        pump.resume_from_subscribe(state).await;

        assert_eq!(
            asserter.read_q().len(),
            0,
            "auto-backfill inside resume popped exactly one eth_getLogs response; queue must be empty"
        );
    }

    /// J3FMDO race regression: `backfill_to_ws_block` is the
    /// synchronously-awaitable backfill that `PumpState::resume` `block_on`s
    /// BEFORE spawning the live loop. Pre-fix the backfill ran INSIDE the
    /// spawned `resume_from_subscribe` task, so `PumpState::resume` returned
    /// immediately and Python's `build_paths` drained an EMPTY backfill buffer
    /// (the burn for an active pool was not yet buffered) → the post-drain
    /// verify mismatched on-chain and crashed the settlement-arbitrage bot with
    /// `VerificationMismatchError`. This pins the contract: after
    /// `backfill_to_ws_block` returns, the V3 backfill buffer is populated —
    /// the event did NOT require the live loop to run first.
    #[tokio::test]
    async fn backfill_to_ws_block_populates_buffer_before_return() {
        let pool_addr = alloy::primitives::Address::from([0xc2u8; 20]);
        let bot = Arc::new(Bot::new(1));
        bot.state_arc()
            .write_at(crate::bot_core::state_lock::LockSite::Pump)
            .set_snapshot_seed_block(Some(85));
        let (pump, _sink, _shutdown, asserter) =
            pump_for_test_with_asserter(Arc::clone(&bot), None);

        // A V3 Burn log at block 90 (in the backfill range 86..99).
        asserter.push_success(&vec![make_v3_burn_log_with_block(
            pool_addr, -100, 100, 500, 90,
        )]);

        // backfill_to_ws_block must fully buffer the burn BEFORE returning.
        pump.backfill_to_ws_block(100)
            .await
            .expect("backfill_to_ws_block completes against the mock");

        // The burn was buffered (not applied — pool unregistered → buffer
        // branch). Pre-fix: this method did not exist and `resume` returned
        // before the spawned task buffered → count 0 → race.
        assert_eq!(
            bot.state_arc()
                .read_at(crate::bot_core::state_lock::LockSite::Pump)
                .buffered_v3_event_count(&pool_addr),
            1,
            "backfill_to_ws_block must buffer the V3 burn before returning (race regression)"
        );
    }

    /// DFQYM5/WS-DROP regression: the resume-path backfill helper must drain
    /// the WS stream WHILE the snapshot backfill runs and re-inject the
    /// drained events ahead of the live tail. Pre-fix the pyo3
    /// `PumpState::resume` ran `backfill_to_ws_block` with the stream
    /// untouched, so alloy's capacity-16 subscription broadcast ring
    /// overflowed (unfiltered log sub → hundreds of messages per mainnet
    /// block) and silently dropped the OLDEST messages — the first live
    /// block's logs — tripping the WS-completeness abort (observed live:
    /// `eth_getLogs=44 logs, WS delivered=0` at block 25800995). The helper
    /// returns the stream to hand to `run_with_stream`: drained events
    /// first (arrival order, MJXP5Z), live tail after — and the J3FMDO
    /// synchronous-backfill contract still holds (buffer populated on
    /// return).
    #[tokio::test]
    async fn backfill_with_drain_reinjects_events_present_during_backfill() {
        let pool_addr = alloy::primitives::Address::from([0xc3u8; 20]);
        let bot = Arc::new(Bot::new(1));
        bot.state_arc()
            .write_at(crate::bot_core::state_lock::LockSite::Pump)
            .set_snapshot_seed_block(Some(85));
        let (pump, _sink, _shutdown, asserter) =
            pump_for_test_with_asserter(Arc::clone(&bot), None);

        // The snapshot→WS gap backfill (86..100): one V3 Burn log at block 90.
        asserter.push_success(&vec![make_v3_burn_log_with_block(
            pool_addr, -100, 100, 500, 90,
        )]);

        // Live events present on the combined stream while the backfill is in
        // flight — in production these are the freshly-mined first live
        // block's logs that the undrained alloy ring used to evict. The tail
        // pends forever to model a LIVE websocket (the drain must keep
        // running until the backfill completes, not bail on a closed stream).
        let live = vec![
            WsEvent::Pool(PoolEvent::from_log(make_v2_sync_log(
                alloy::primitives::Address::from([0xd1u8; 20]),
                U256::ZERO,
                U256::ZERO,
                101,
                false,
            ))),
            WsEvent::BlockHeader {
                number: 101,
                timestamp: 1_000_101,
                base_fee_per_gas: None,
                gas_used: 0,
                gas_limit: 0,
            },
        ];
        let combined = stream::iter(live)
            .chain(stream::pending::<WsEvent>())
            .boxed();

        let (backfill_res, mut combined) = pump.backfill_with_drain(100, combined).await;
        backfill_res.expect("backfill completes against the mock");

        // J3FMDO invariant preserved: the backfill buffer is populated on
        // return (the synchronous contract `PumpState::resume` relies on).
        assert_eq!(
            bot.state_arc()
                .read_at(crate::bot_core::state_lock::LockSite::Pump)
                .buffered_v3_event_count(&pool_addr),
            1,
            "backfill_with_drain must buffer the V3 burn before returning (J3FMDO)"
        );

        // The drained events were captured during the backfill and are
        // re-injected ahead of the live tail, arrival order preserved.
        let expected: [(&str, u64); 2] = [("log", 101), ("header", 101)];
        for (kind, number) in expected {
            let ev = tokio::time::timeout(std::time::Duration::from_secs(5), combined.next())
                .await
                .expect("re-injected event must arrive")
                .expect("stream yields the drained event");
            match ev {
                WsEvent::Pool(pe) => {
                    assert_eq!((kind, pe.payload.block_number.unwrap()), ("log", number));
                }
                WsEvent::BlockHeader { number: n, .. } => {
                    assert_eq!((kind, n), ("header", number));
                }
            }
        }
    }

    /// J3FMDO: `resume_from_subscribe` skips the auto-backfill entirely when no
    /// snapshot seed is present (`S = None`, cold start). The Asserter queue is
    /// left untouched (the pump never calls `eth_getLogs`) and the live loop
    /// anchors on `first_observed_block` directly. An empty queue under a live
    /// `eth_getLogs` request would error; we assert the queue stays empty AND
    /// the resume returns without a provider error.
    #[tokio::test]
    async fn auto_backfill_skipped_when_s_none_in_resume() {
        let bot = Arc::new(Bot::new(1));
        // Fresh Bot: snapshot_seed_block is None — no gap to backfill.
        let (mut pump, _sink, _shutdown, asserter) = pump_for_test_with_asserter(bot, None);

        let combined = stream::iter(Vec::<WsEvent>::new()).boxed();
        let state = SubscribeState {
            first_block: 100,
            first_timestamp: 0,
            combined_stream: Some(combined),
        };
        pump.resume_from_subscribe(state).await;

        assert_eq!(
            asserter.read_q().len(),
            0,
            "cold-start resume never calls eth_getLogs (auto-backfill gated on S<W)"
        );
    }

    /// J3FMDO: `resume_from_subscribe` skips the auto-backfill when the
    /// snapshot is already at/after the WS block (`S >= W` — catch-up snapshot
    /// with no gap to backfill).
    #[tokio::test]
    async fn auto_backfill_skipped_when_s_ge_w_in_resume() {
        let bot = Arc::new(Bot::new(1));
        bot.state_arc()
            .write_at(crate::bot_core::state_lock::LockSite::Pump)
            .set_snapshot_seed_block(Some(100));
        let (mut pump, _sink, _shutdown, asserter) = pump_for_test_with_asserter(bot, None);

        let combined = stream::iter(Vec::<WsEvent>::new()).boxed();
        let state = SubscribeState {
            first_block: 100,
            first_timestamp: 0,
            combined_stream: Some(combined),
        };
        pump.resume_from_subscribe(state).await;

        assert_eq!(
            asserter.read_q().len(),
            0,
            "S ≥ W → no auto-backfill, no eth_getLogs call"
        );
    }

    /// Diagnostic for the 2026-07-12 WS `eth_getLogs` hang.
    ///
    /// Root cause (confirmed here with tracing + a concurrent
    /// `get_block_number` probe): tungstenite correctly returns
    /// `Error::Capacity(MessageTooLong)` for a response larger than the
    /// default `max_frame_size` (16 MiB) / `max_message_size` (64 MiB), but
    /// `alloy-pubsub`'s `WsBackend` converts that to
    /// `TransportErrorKind::backend_gone()` (a *retryable* error) at the
    /// backend→service boundary — losing the Capacity specificity. The pubsub
    /// service then enters an INFINITE reconnect→redispatch loop: `reconnect()`
    /// succeeds on the first attempt (the WS handshake is fine; only the
    /// response is too big), `max_retries` is never consumed, and the pending
    /// in-flight `eth_getLogs` is re-dispatched each cycle. The caller's
    /// `get_logs` future never resolves; small concurrent calls keep working.
    ///
    /// Three variants:
    /// A — default tungstenite caps: demonstrates the infinite cycle (HUNG,
    ///     `get_block_number` probe still succeeding concurrently);
    /// B — raised caps via raw `WsConnect::with_config`: WS handles it;
    /// C — production `AlloyProvider::new` path (= the `build_provider` fix):
    ///     regression sentinel.
    ///
    /// Run with:
    /// `cargo test -p degenbot-bot --manifest-path rust/Cargo.toml \
    ///   -- --ignored --nocapture ws_getlogs_large_filter_diagnostic`
    ///
    /// Requires `DEGENBOT_RPC_WS_CHAINID_1` (a mainnet WS endpoint).
    #[tokio::test]
    #[ignore = "requires a live mainnet WS endpoint (DEGENBOT_RPC_WS_CHAINID_1)"]
    #[expect(clippy::too_many_lines)]
    async fn ws_getlogs_large_filter_diagnostic() {
        use alloy::network::Ethereum;
        use alloy::providers::{Provider, ProviderBuilder, WebSocketConfig, WsConnect};
        use std::time::Duration;
        use tokio::time::timeout;
        use tracing_subscriber::util::SubscriberInitExt;
        type Erased = std::sync::Arc<dyn Provider<Ethereum>>;

        let Ok(ws_url) = std::env::var("DEGENBOT_RPC_WS_CHAINID_1") else {
            eprintln!("skip: DEGENBOT_RPC_WS_CHAINID_1 not set");
            return;
        };

        // Fetch a recent block number (small call — works over default WS).
        let anchor_provider: Erased = {
            let mid = ProviderBuilder::default()
                .connect_ws(WsConnect::new(ws_url.clone()))
                .await
                .expect("ws connect (anchor)")
                .erased();
            Arc::new(mid)
        };
        let latest = anchor_provider
            .get_block_number()
            .await
            .expect("block number");
        // Leave a few blocks of margin so the range is settled.
        let to = latest - 5;
        let from = to - 1_999;
        let filter = build_backfill_filter(from, to);
        eprintln!("filter range {from}–{to} (latest={latest})");

        // --- Variant A: DEFAULT tungstenite config (max_message_size=64MiB) ---
        // Install a tracing subscriber so alloy's reconnect-cycle `error!`/
        // `warn!` logs surface (without one they're silently dropped — which is
        // why the earlier run showed "no error surfaced"). Also poll
        // `get_block_number` concurrently: if it keeps succeeding while
        // `get_logs` is pending, the WS service is alive and silently
        // reconnecting (proving the oversized-response cycle), NOT truly
        // stalled in tungstenite.
        let _guard = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(
                "alloy_pubsub=debug,alloy_transport_ws=debug,tungstenite=info",
            ))
            .with_test_writer()
            .set_default();
        let p: Erased = {
            let mid = ProviderBuilder::default()
                .connect_ws(WsConnect::new(ws_url.clone()))
                .await
                .expect("ws connect (A)")
                .erased();
            Arc::new(mid)
        };
        // Concurrent block-number probe on the SAME provider.
        let probe_p = Arc::clone(&p);
        let probe = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(2));
            tick.tick().await; // skip immediate
            for i in 1..=15_u32 {
                tick.tick().await;
                match probe_p.get_block_number().await {
                    Ok(n) => eprintln!("A probe #{i}: get_block_number OK = {n}"),
                    Err(e) => eprintln!("A probe #{i}: get_block_number ERR = {e}"),
                }
            }
        });
        let t0 = std::time::Instant::now();
        let res = timeout(Duration::from_secs(30), p.get_logs(&filter)).await;
        let elapsed = t0.elapsed();
        match res {
            Ok(Ok(logs)) => eprintln!(
                "A DEFAULT   : OK  {} logs in {:.2}s (under the 64MiB cap this run)",
                logs.len(),
                elapsed.as_secs_f64()
            ),
            Ok(Err(e)) => eprintln!(
                "A DEFAULT   : ERR after {:.2}s — `{e}`",
                elapsed.as_secs_f64()
            ),
            Err(_) => {
                eprintln!("A DEFAULT   : HUNG (30s timeout, no error surfaced to the caller)");
            }
        }
        // Let the probe finish printing so we see the concurrent-call verdict.
        let _ = timeout(Duration::from_secs(35), probe).await;

        // --- Variant B: RAISED config (no size cap) ---
        let cfg = WebSocketConfig::default()
            .max_message_size(None)
            .max_frame_size(None);
        let p: Erased = {
            let mid = ProviderBuilder::default()
                .connect_ws(WsConnect::new(ws_url.clone()).with_config(cfg))
                .await
                .expect("ws connect (B)")
                .erased();
            Arc::new(mid)
        };
        let t0 = std::time::Instant::now();
        let res = timeout(Duration::from_mins(1), p.get_logs(&filter)).await;
        let elapsed = t0.elapsed();
        match res {
            Ok(Ok(logs)) => eprintln!(
                "B RAISED    : OK  {} logs in {:.2}s",
                logs.len(),
                elapsed.as_secs_f64()
            ),
            Ok(Err(e)) => eprintln!(
                "B RAISED    : ERR after {:.2}s — `{e}`",
                elapsed.as_secs_f64()
            ),
            Err(_) => eprintln!("B RAISED    : HUNG (60s timeout)"),
        }

        // --- Variant C: production path (`AlloyProvider::new` → ---
        // `build_provider`), which now raises the tungstenite caps in
        // `degenbot_rpc::provider::build_provider`. This is the regression
        // sentinel: if a future change drops the raised-config in
        // `build_provider`, this variant hangs and the test suite surfaces it.
        let alloy_provider = degenbot_rpc::provider::AlloyProvider::new(&ws_url, 3)
            .await
            .expect("AlloyProvider::new");
        let p = alloy_provider.provider_arc();
        let t0 = std::time::Instant::now();
        let res = timeout(Duration::from_mins(1), p.get_logs(&filter)).await;
        let elapsed = t0.elapsed();
        match res {
            Ok(Ok(logs)) => eprintln!(
                "C PRODUCTION: OK  {} logs in {:.2}s",
                logs.len(),
                elapsed.as_secs_f64()
            ),
            Ok(Err(e)) => eprintln!(
                "C PRODUCTION: ERR after {:.2}s — `{e}`",
                elapsed.as_secs_f64()
            ),
            Err(_) => {
                eprintln!("C PRODUCTION: HUNG (60s timeout) — `build_provider` config regression");
            }
        }
    }

    /// MQUKB6 (epic KDUED5): one entered `degenbot.epoch` root span per
    /// observed header, carrying a `block.number` field, parented under the
    /// `run_with_stream` instrument span. In-memory exporter +
    /// `set_global_default` (the repo convention: the thread-local `set_default`
    /// is unsafe in a parallel test process - stale `DefaultGuard` restores
    /// corrupt it). No other lib test sets a global subscriber, so this test
    /// wins the once-per-process slot; the `OTel` layer itself is covered by the
    /// `otel_plumbing` integration tests.
    #[cfg(feature = "otel")]
    #[tokio::test]
    async fn header_arms_per_block_span_with_number_and_parent() {
        const NEXT_BLOCK: u64 = MY_BLOCK + 1;
        use crate::otel;
        use opentelemetry_sdk::trace::InMemorySpanExporter;
        use tracing_subscriber::layer::SubscriberExt;

        // Unique block number (0xDEADBEEF): with a global subscriber,
        // concurrent tests' pump spans land in this exporter too, so assert
        // on THIS test's header by number, not on total span counts.
        const MY_BLOCK: u64 = 0xDEAD_BEEF;
        const MY_BLOCK_I64: i64 = 0xDEAD_BEEF;

        let (mut pump, _sink) = pump_for_test(None);

        let exporter = InMemorySpanExporter::default();
        let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
        let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));
        // Global subscriber (repo convention, `set_global_default` - the
        // thread-local `set_default` is process-unsafe in parallel tests). No
        // other lib test takes the once-per-process global slot.
        tracing::subscriber::set_global_default(subscriber)
            .expect("global default already set by another test");

        // JYCTXI: a second header exercises the consecutive-header case —
        // the new span must detach from the still-entered previous block
        // span (loop-context guard) instead of chaining into one mega-trace.
        let events: Vec<WsEvent> = vec![
            WsEvent::BlockHeader {
                number: MY_BLOCK,
                timestamp: 1,
                base_fee_per_gas: Some(1),
                gas_used: 1,
                gas_limit: 1,
            },
            WsEvent::BlockHeader {
                number: NEXT_BLOCK,
                timestamp: 2,
                base_fee_per_gas: Some(2),
                gas_used: 2,
                gas_limit: 2,
            },
        ];
        let combined = stream::iter(events).boxed();
        pump.run_test_loop(combined, MY_BLOCK - 1).await;

        provider.force_flush().expect("flush");
        let spans = exporter.get_finished_spans().expect("spans");

        // Select THIS test's header span by its unique number (tracing-
        // opentelemetry 0.33 maps u64 fields to strings; an OTel bump may
        // switch to I64 - accept both representations).
        let my_spans: Vec<_> = spans
            .iter()
            .filter(|sp| {
                sp.name.as_ref() == "degenbot.epoch.run"
                    && sp.attributes.iter().any(|kv| {
                        kv.key == opentelemetry::Key::from_static_str("epoch.block")
                            && (matches!(kv.value, opentelemetry::Value::I64(v) if v == MY_BLOCK_I64)
                                || matches!(kv.value, opentelemetry::Value::String(ref v) if v.as_str() == MY_BLOCK.to_string().as_str()))
                    })
            })
            .collect();
        assert_eq!(
            my_spans.len(),
            1,
            "expected exactly one span for block {}; got names: {:?}",
            MY_BLOCK,
            spans.iter().map(|sp| sp.name.as_ref()).collect::<Vec<_>>()
        );
        let block_span = &my_spans[0];

        // BF43PM: the epoch root carries the rewind generation (no reorg yet
        // in this fixture — seq 0).
        assert!(
            block_span.attributes.iter().any(|kv| {
                kv.key == opentelemetry::Key::from_static_str("epoch.seq")
                    && matches!(kv.value, opentelemetry::Value::String(ref v) if v.as_str() == "0")
            }),
            "epoch root must carry epoch.seq; got {:?}",
            block_span.attributes
        );

        // MQUKB6-T0: the per-block span is now a trace ROOT — the former
        // `run_with_stream` instrument span was a never-closing root that OTel
        // never exported (orphaning every pump-task span under a missing
        // parent). Roots export cleanly; parent_span_id is the zero sentinel.
        assert_eq!(
            block_span.parent_span_id,
            opentelemetry::trace::SpanId::INVALID,
            "per-block span for block {} must be a trace root; parent_span_id: {:?}",
            MY_BLOCK,
            block_span.parent_span_id
        );

        // JYCTXI: the NEXT header's span must ALSO be a trace root in its own
        // trace — created while block {}'s span was still entered (the loop
        // context guard), it must detach rather than chain into a mega-trace.
        let next_spans: Vec<_> = spans
            .iter()
            .filter(|sp| {
                sp.name.as_ref() == "degenbot.epoch.run"
                    && sp.attributes.iter().any(|kv| {
                        kv.key == opentelemetry::Key::from_static_str("epoch.block")
                            && (matches!(kv.value, opentelemetry::Value::I64(v) if v == i64::try_from(NEXT_BLOCK).unwrap_or(i64::MAX))
                                || matches!(kv.value, opentelemetry::Value::String(ref v) if v.as_str() == NEXT_BLOCK.to_string().as_str()))
                    })
            })
            .collect();
        assert_eq!(next_spans.len(), 1, "expected one span for the next header");
        let next_span = &next_spans[0];
        assert_eq!(
            next_span.parent_span_id,
            opentelemetry::trace::SpanId::INVALID,
            "consecutive-header span must also be a trace root; parent_span_id: {:?}",
            next_span.parent_span_id
        );
        assert_ne!(
            next_span.span_context.trace_id(),
            block_span.span_context.trace_id(),
            "consecutive headers must be separate traces (mega-trace regression)"
        );
    }

    /// TQ7PD6 regression: a header burst through the pump must CLOSE (export)
    /// every per-epoch span, never leaking still-entered spans on worker
    /// threads (the pre-fix loop-wide `Span::enter()` guard lived across the
    /// select's await points; when the multi-threaded runtime migrated the task
    /// between workers, it entered on one thread and dropped on another, so the
    /// span stayed entered in the abandoned worker's TLS — never closed, never
    /// exported, every child orphaned). The DETERMINISTIC defense is the
    /// structural fix (no `enter` guard may outlive a poll); this test locks
    /// the observable symptom — all N spans closed — and exercises cross-await
    /// parking so CI load that DOES migrate the task surfaces the old leak.
    ///
    /// SONJQA/G3 note (BF43PM): the pump-level `log_wait` force-close test
    /// was retired with the `pump.log_wait` waterfall — quiet headers now open
    /// NO stage span at all. The force-close law it pinned lives on as the
    /// `stage_telemetry::otel_tests::stale_stage_span_exports_force_closed`
    /// pinned export test against `StageTelemetry::force_close_aged`, driven
    /// from the timed-exit tick with the same `stage_max_age` bound.
    #[cfg(feature = "otel")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn header_burst_closes_every_block_span() {
        use crate::otel;
        use opentelemetry_sdk::trace::InMemorySpanExporter;
        use tracing_subscriber::layer::SubscriberExt;

        const BASE: u64 = 0xBEEF_0000;
        const COUNT: u64 = 32;

        let (mut pump, _sink) = pump_for_test(None);
        let exporter = InMemorySpanExporter::default();
        let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
        let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));
        // NB: set_global_default can only be installed once per process. This
        // test and the sibling header-span test both take it; cargo runs each
        // lib test in its own process by default, but to be robust against a
        // shared process use set_default (thread-local) where possible. The
        // header_arms test above uses the global slot; this one uses a local
        // guard so they can coexist under `--test-threads`.
        let _guard = tracing::subscriber::set_default(subscriber);

        let events: Vec<WsEvent> = (0..COUNT)
            .map(|i| WsEvent::BlockHeader {
                number: BASE + i,
                timestamp: 1,
                base_fee_per_gas: Some(1),
                gas_used: 1,
                gas_limit: 1,
            })
            .collect();
        // Force a park between headers: a ready stream never suspends, so the
        // task would stay on one worker and the pre-fix leaked-enter bug (which
        // only manifests when the task MIGRATES across an enter guard) would not
        // be exercised. A 1ms sleep makes every inter-header await pend, giving
        // the multi-threaded runtime a migration opportunity each iteration.
        let combined = stream::iter(events)
            .then(|e| async move {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                e
            })
            .boxed();
        pump.run_test_loop(combined, BASE - 1).await;
        // The channels may still be flushing; give the idle settle one beat.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        provider.force_flush().expect("flush");
        let spans = exporter.get_finished_spans().expect("spans");

        let mut seen = std::collections::HashSet::new();
        for sp in &spans {
            if sp.name.as_ref() == "degenbot.epoch.run" {
                for kv in &sp.attributes {
                    if kv.key == opentelemetry::Key::from_static_str("epoch.block") {
                        if let opentelemetry::Value::String(ref v) = kv.value {
                            if let Ok(n) = v.as_str().parse::<u64>() {
                                seen.insert(n);
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(
            seen.len(),
            usize::try_from(COUNT).unwrap_or(usize::MAX),
            "every header must export a CLOSED epoch span; got {}/{}",
            seen.len(),
            COUNT
        );
    }

    /// S53STH: the cooperative timed-exit path must make a PARKED select wake
    /// and return promptly (unwinding all span guards on this task) when the
    /// hotpath timer raises the flag mid-park — not sit out the full settle
    /// window, and never `process::exit`.
    #[cfg(feature = "hotpath")]
    #[tokio::test(flavor = "current_thread")]
    async fn timed_exit_flag_exits_parked_select_promptly() {
        let (mut pump, _sink) = pump_for_test(None);
        // Raise the flag from outside after 100ms — mid-park on the select's
        // settle window. The 500ms timed-exit tick polls it and breaks the
        // loop; success is sub-second return (vs the 60s park regression).
        let flag = Arc::clone(&pump.shutdown);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        let events: Vec<WsEvent> = vec![WsEvent::BlockHeader {
            number: 0xB000_0001,
            timestamp: 1,
            base_fee_per_gas: Some(1),
            gas_used: 1,
            gas_limit: 1,
        }];
        let started = std::time::Instant::now();
        pump.run_test_loop(stream::iter(events).boxed(), 0xB000_0000)
            .await;
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "watch-raised shutdown must exit promptly, took {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn shutdown_flag_exits_loop_promptly() {
        let (mut pump, _sink) = pump_for_test(None);
        // Pre-raise: the very first select! arm sees the watch fire and breaks.
        pump.shutdown
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let events: Vec<WsEvent> = vec![WsEvent::BlockHeader {
            number: 0xB000_0001,
            timestamp: 1,
            base_fee_per_gas: Some(1),
            gas_used: 1,
            gas_limit: 1,
        }];
        let started = std::time::Instant::now();
        pump.run_test_loop(stream::iter(events).boxed(), 0xB000_0000)
            .await;
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "shutdown flag must exit the loop promptly, took {:?}",
            started.elapsed()
        );
    }

    // ==================================================================
    // ergo 2KQZSC — RED pins for the candidate-2 stage-seam contract.
    // These tests target the post-cutover contract; production code is
    // NOT changed here. They are intentionally red until the cutover.
    // ==================================================================
    mod candidate2_seam_pins {
        use super::*;

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
            let src = include_str!("block_pump.rs");
            assert!(
                !src.contains(concat!("self.engine.", "on_pump_ended()")),
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
            let pump_src = include_str!("block_pump.rs");
            assert!(
                !pump_src.contains(concat!("PublishOutcome::", "default()")),
                "the pump must not fabricate a default PublishOutcome carrier"
            );
            let seam_src = include_str!("stage_handlers.rs");
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
}
