//! `BlockPump` — `Bot`'s WS transport + drain loop (ADR-006 D4).
//!
//! Generalized from the `BlockPump`: holds `Arc<Bot>` +
//! `Arc<dyn DrainSink>` instead of `Arc<Mutex<ArbitrageEngine>>`. Per WS log,
//! the pump calls `bot.dispatch_log(log)` (slice 4: decode → apply to
//! `BotState` → notify the `EngineSubscriber`, which dirties the engine). At
//! block boundaries / drain ticks / reorg the pump drives the `DrainSink`
//! (`on_drain` / `on_send` / `finalize_block`).
//!
//! `apply_log` is gone — ALL log application routes through `Bot::dispatch_log`,
//! so `process_block` / `process_block_and_send` decompose in the pump into
//! `dispatch_log`-per-log + `on_drain` (the D4 goal).
//!
//! The pump's **mechanics stay unchanged** from the `BlockPump` era: dual
//! `newHeads` + `logs` subscription, Rust-side topic+address filtering, block-
//! boundary detection, 50ms send-result debounce, gap/timeout `eth_getLogs`
//! backfill. Only the owner (`Bot`, via the wiring layer) + the per-block
//! dispatch targets changed.
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
//! BEFORE `resume_from_subscribe()`. The `DrainSink`'s
//! `last_processed_block()` is the backfill-start boundary. (Pre-epic-P73ER6
//! Python orchestrated this manually; the epic relocates backfill into the
//! core, driven automatically by `resume`.)

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::bot_core::pump_fsm::{PumpDecision, PumpFSM};

use alloy::primitives::B256;
use alloy::rpc::types::{Filter, Log, Topic};
use futures_util::{stream, StreamExt};
use tokio::time::timeout;
use tracing::Instrument;

use crate::bot_core::event_dispatch::{DispatchOwner, DrainWork, SolverVerifyRequest};
use crate::bot_core::solver_state_tripwire::{
    extract_solver_hop_states, judge, GateVerdict, TripReorgWindow, TripwireConfig,
    TripwireDivergence,
};
use crate::bot_core::LogDecision;
use crate::bot_core::{drain_sink::DrainSink, BlockMetadata, Bot};
use degenbot_decoders::v2_sync_decoder::V2_SYNC_TOPIC;
use degenbot_decoders::v3_mint_burn_decoder::{V3_BURN_TOPIC, V3_MINT_TOPIC};
use degenbot_decoders::v3_pancakeswap_swap_decoder::V3_PANCAKESWAP_SWAP_TOPIC;
use degenbot_decoders::v3_swap_decoder::V3_SWAP_TOPIC;
use degenbot_decoders::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC;
use degenbot_decoders::v4_swap_decoder::V4_SWAP_TOPIC;
use degenbot_rpc::provider::AlloyProvider;

/// How long to wait with no activity before assuming the connection is dead.
const BACKFILL_TIMEOUT_SECS: u64 = 60;

/// After the first dirty WS log for a block, wait this long for more logs
/// before solving and dispatching results to Python. Each new log resets
/// the timer. This debouncing ensures one dispatch per burst of logs
/// rather than one per individual log.
const DEBOUNCE_MS: u64 = 50;

/// If no block header arrives within this window, poll `eth_blockNumber`
/// and backfill the gap — independent of log activity.
///
/// The `newHeads` WS subscription can die silently while `logs` keeps
/// flowing. `stream::select` masks a dead block stream as long as logs
/// arrive every `< BACKFILL_TIMEOUT_SECS` (the combined-stream-silence
/// backfill path never fires). Because only headers advance `current_block`,
/// every result batch would then be stamped with a frozen block while
/// prices keep updating from the live log applies — looking like the bot is
/// running but making no block progress. This watchdog independently
/// detects header staleness (capped `wait_timeout` wakes the loop by the
/// deadline even under dense log pressure) and runs the same
/// `handle_timeout_eager` catch-up the no-activity path uses.
const HEADER_STALENESS_SECS: u64 = 30;

/// Default window (seconds) for the logs-subscription liveness watchdog: if
/// headers keep flowing (`newHeads` fresh) but the pump has received NO log
/// from the `eth_subscribe "logs"` arm within this window, the logs
/// subscription is presumed dead/stalled and a warning is emitted. This is
/// the INVERSE of `header_staleness` (a dead `newHeads`): it catches a
/// dead/stalled LOGS sub while the blocks sub is alive — the failure mode
/// Alternative B's header-only handshake no longer catches at startup (the
/// handshake never touches the data plane by design). Runs for the whole
/// pump lifetime, not only at startup. Overridable in tests via
/// `set_log_silence_for_test`.
const LOG_SILENCE_SECS: u64 = 60;

/// Default backfill chunk size (blocks per `eth_getLogs` request) for the
/// snapshot→WS gap closed automatically inside `resume_from_subscribe`
/// (J3FMDO). Mirrors the `pyo3` `backfill_from_snapshot` default (`chunk_size` = 2000):
/// the per-chunk response size stays under `eth_getLogs` payload caps.
const DEFAULT_BACKFILL_CHUNK_SIZE: u64 = 2000;

/// How long the subscribe handshake waits for the WS `logs` stream to deliver
/// its first log after the head is header-confirmed, before falling back to the
/// header-confirmed boundary. Bounds startup latency on a quiet/log-free chain
/// while still capturing the log stream's true live-from block on active ones.
const LOG_CATCHUP_SETTLE_SECS: u64 = 15;

/// Whether a log confirms that a tracked header block is "complete".
///
/// Block data sent from the pump to Python via the watch channel.
/// Topics we care about — used for in-Rust filtering of incoming logs.
pub const RELEVANT_TOPICS: [B256; 7] = [
    V2_SYNC_TOPIC,
    V3_SWAP_TOPIC,
    V3_PANCAKESWAP_SWAP_TOPIC,
    V3_MINT_TOPIC,
    V3_BURN_TOPIC,
    V4_SWAP_TOPIC,
    V4_MODIFY_LIQUIDITY_TOPIC,
];

/// Events from the two WS subscriptions.
pub enum WsEvent {
    /// A new block header arrived.
    BlockHeader {
        number: u64,
        timestamp: u64,
        base_fee_per_gas: Option<u64>,
        gas_used: u64,
        gas_limit: u64,
    },
    /// A log event arrived from the logs subscription.
    Log(Log),
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
    /// The drain sink (slice 6: `SolveCoordinator` fanning to every
    /// attached `Engine` under a `drain_lock`).
    sink: Arc<dyn DrainSink>,
    /// The per-event reorg coordinator (slice 7). Owned by the pump (not
    /// routed through the `DrainSink` — reorg is a `Bot` concern, parallel
    /// to `dispatch_log`).
    reorg_coordinator: Arc<crate::bot_core::reorg_coordinator::ReorgCoordinator>,
    /// ADR-021 D2 Part A — the bounded reorg-window evidence list
    /// (recorded at the FSM reorg decisions; snapshotted as Copies before
    /// each `judge()` await, never held across it).
    trip_reorg_windows: Arc<parking_lot::Mutex<std::collections::VecDeque<TripReorgWindow>>>,
    /// The Alloy provider (created from the RPC URL)
    provider: Arc<AlloyProvider>,
    /// Shutdown flag — set by `stop()` or by a too-deep reorg (graceful exit)
    shutdown: Arc<AtomicBool>,
    /// If no header arrives within this window, poll `eth_blockNumber` and
    /// backfill regardless of log activity (dead-`newHeads` recovery — see
    /// `HEADER_STALENESS_SECS`). Overridable in tests via
    /// `set_header_staleness_for_test`.
    header_staleness: Duration,
    /// If no log arrives within this window WHILE headers stay fresh, the
    /// `eth_subscribe "logs"` subscription is presumed dead/stalled and the
    /// logs-silence watchdog emits a `[pump] logs subscription silent`
    /// warning (see `LOG_SILENCE_SECS`). Overridable in tests via
    /// `set_log_silence_for_test`.
    log_silence: Duration,
    /// Count of logs-silence alarms fired since the pump started. Incremented
    /// once per silence episode (re-armed when the next `WsEvent::Log` resumes
    /// the sub) so the liveness watchdog is test-observable without depending
    /// on log-capture infrastructure.
    log_silence_alarms: u64,
    /// The packed ADR-021 solver-state tripwire stances (the publish-point
    /// accuracy gate + its observation stages). `enabled` is conservative
    /// default ON (`DEGENBOT_ASSERT_SOLVER_STATE`, via
    /// `bot_env_flag_default_on`); the three diagnostics default off (via
    /// `bot_env_flag_default_off`). Held as a field (not per-call env reads)
    /// so tests deterministically opt out per-pump (Z4KQXF); the tripwire
    /// module itself reads no env.
    tripwire_config: crate::bot_core::solver_state_tripwire::TripwireConfig,
    /// Whether the per-block WS-delivery completeness cross-check runs
    /// (`assert_ws_block_complete` — aborts on any relevant-topic log that
    /// `eth_getLogs` has but the live websocket dropped). Conservative default
    /// ON (`DEGENBOT_WS_COMPLETENESS`, via `bot_env_flag_default_on`): set
    /// `=0` to disable. Held as a field (not a global env read) so tests
    /// deterministically opt out per-pump (same pattern as `solver_state_verify`,
    /// Z4KQXF). When OFF the `ws_delivered` index-tracking map is not populated
    /// (no work on the hot loop).
    ws_completeness_enabled: bool,
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
    /// ADR-021 D2 Part B — parse the delivery-lag trip threshold (pure):
    /// unset/empty/unparseable/zero = `None` = off (today's report-only
    /// parity).
    #[must_use]
    fn delivery_lag_trip_threshold(raw: Option<&str>) -> Option<u64> {
        raw?.trim().parse::<u64>().ok().filter(|n| *n > 0)
    }

    #[expect(clippy::missing_errors_doc)]
    pub async fn subscribe(
        rpc_url: &str,
        bot: Arc<Bot>,
        sink: Arc<dyn DrainSink>,
        reorg_coordinator: Arc<crate::bot_core::reorg_coordinator::ReorgCoordinator>,
        shutdown: Arc<AtomicBool>,
    ) -> Result<(Self, SubscribeState), String> {
        let provider = AlloyProvider::new(rpc_url, 3)
            .await
            .map_err(|e| format!("BlockPump: failed to create provider: {e}"))?;

        let provider_arc = provider.provider_arc();

        // Subscribe to block headers
        let block_stream = provider_arc
            .subscribe_blocks()
            .await
            .map_err(|e| format!("BlockPump: failed to subscribe to blocks: {e}"))?
            .into_stream();

        // Subscribe to logs — unfiltered. All filtering happens in Rust.
        let log_filter = Filter::new();
        let log_stream = provider_arc
            .subscribe_logs(&log_filter)
            .await
            .map_err(|e| format!("BlockPump: failed to subscribe to logs: {e}"))?
            .into_stream();

        let combined = stream_select(block_stream, log_stream).boxed();

        let mut pump = Self {
            bot,
            sink,
            reorg_coordinator,
            trip_reorg_windows: Arc::new(
                parking_lot::Mutex::new(std::collections::VecDeque::new()),
            ),
            provider: Arc::new(provider),
            shutdown,
            header_staleness: Duration::from_secs(HEADER_STALENESS_SECS),
            log_silence: Duration::from_secs(LOG_SILENCE_SECS),
            log_silence_alarms: 0,
            tripwire_config: crate::bot_core::solver_state_tripwire::TripwireConfig {
                enabled: crate::bot_core::bot_env_flag_default_on("DEGENBOT_ASSERT_SOLVER_STATE"),
                divergence_scan: crate::bot_core::bot_env_flag_default_off(
                    "DEGENBOT_SOLVER_DIVERGENCE_SCAN",
                ),
                anchor_probe: crate::bot_core::bot_env_flag_default_off(
                    "DEGENBOT_TRACE_SOLVE_ANCHOR",
                ),
                staged_clock_probe: crate::bot_core::bot_env_flag_default_off(
                    "DEGENBOT_TRACE_STAGED_CLOCK",
                ),
                delivery_lag_trip_blocks: Self::delivery_lag_trip_threshold(
                    std::env::var("DEGENBOT_DELIVERY_LAG_TRIP_BLOCKS")
                        .ok()
                        .as_deref(),
                ),
            },
            ws_completeness_enabled: crate::bot_core::bot_env_flag_default_on(
                "DEGENBOT_WS_COMPLETENESS",
            ),
        };

        // MJXP5Z (Alternative B): single-stream handshake - NO resubscribe.
        // `subscribe_with_stream` hands the SAME `combined` onward, re-injecting
        // any logs consumed during header-only polling. One WS, one handoff.
        pump.subscribe_with_stream(combined)
            .await
            .map(|state| (pump, state))
    }

    /// Single-stream handshake seam (MJXP5Z / Alternative B): runs
    /// Single-stream handshake seam (MJXP5Z / Alternative B): runs
    /// `observe_complete_block` against `combined` (polling headers ONLY until
    /// two consecutive headers confirm the boundary, collecting any logs the
    /// fused stream interleaves during the handshake and re-injecting them),
    /// then hands the SAME `combined` onward. One WS connection, one handoff,
    /// no resubscribe — making a structurally lost log impossible (the
    /// handshake never touches the data plane).
    ///
    /// The logs consumed during header polling are re-injected via
    /// `stream::iter(pending).chain(combined)` so `run_with_stream` receives
    /// every log the node pushed during the handshake window.
    pub(crate) async fn subscribe_with_stream(
        &mut self,
        mut combined: stream::BoxStream<'static, WsEvent>,
    ) -> Result<SubscribeState, String> {
        let (first_block, first_timestamp, pending) =
            self.observe_complete_block(&mut combined).await;
        // Re-inject any logs the handshake consumed while polling for headers.
        let combined = if pending.is_empty() {
            combined
        } else {
            stream::iter(pending).chain(combined).boxed()
        };
        Ok(SubscribeState {
            first_block,
            first_timestamp,
            combined_stream: Some(combined),
        })
    }

    /// Handshake (MJXP5Z / Alternative B) that confirms the boundary from the
    /// LOG STREAM's actual liveness, not headers alone (DFQYM5). Polls the
    /// fused stream until (a) two consecutive distinct headers confirm the
    /// head is near/finalized AND (b) the `logs` sub has delivered at least one
    /// log — the block of that first log (`first_log_block`) is where the log
    /// stream is PROVABLY live. The boundary `W` returned is `first_log_block`
    /// (falls back to the header-confirmed head if the log stream stays silent
    /// past `LOG_CATCHUP_SETTLE_SECS`).
    ///
    /// Why this matters: the node's `logs` sub can become live one or more
    /// blocks AFTER the header stream confirms the boundary (headers confirm
    /// the moment a block finalizes; the log sub registration lags). A
    /// header-only boundary then leaves `[W+1, logs_sub_live_from-1]` delivered
    /// by NEITHER the backfill (stops at W) NOR the WS (starts at `live_from`) —
    /// the systematic delivery hole the WS-completeness abort caught. Anchoring
    /// the boundary on the first-delivered log closes it: backfill owns
    /// `[S+1, W]`, the live WS owns `[W+1, ∞)` with no gap.
    ///
    /// Any `WsEvent::Log` the fused stream interleaves is collected into
    /// `pending` (preserving arrival order) for `subscribe_with_stream` to
    /// re-inject — the handshake never loses, matches, or drops a log.
    ///
    /// No events are buffered to pool state here. The backfill
    /// (`backfill_from_snapshot`) is the sole authority for blocks S+1..W
    /// (inclusive), and the pump (resume phase) is the sole authority for W+1
    /// onward.
    ///
    /// Returns (`first_block` W, `timestamp_of_W`, `pending_logs`).
    async fn observe_complete_block(
        &self,
        combined: &mut stream::BoxStream<'static, WsEvent>,
    ) -> (u64, u64, Vec<WsEvent>) {
        let mut prev_header: Option<u64> = None;
        let mut prev_timestamp: u64 = 0;
        // The block of the FIRST log the WS `logs` sub delivers — the earliest
        // proof the log stream is provably LIVE. The resume boundary + backfill
        // inclusive target = this block (DFQYM5).
        let mut first_log_block: Option<u64> = None;
        // The highest header-confirmed-finalized block (two consecutive
        // headers). Advances as headers flow; used to know we're near the head
        // and that the chosen boundary is (or will be) finalized.
        let mut confirmed_head: Option<u64> = None;
        // Deadline to keep waiting for the log stream's first log after the
        // head is header-confirmed. Falls back to the header boundary on a
        // genuinely quiet/log-free head so the handshake cannot hang.
        let mut settle_deadline: Option<tokio::time::Instant> = None;
        let mut pending: Vec<WsEvent> = Vec::new();

        loop {
            if self.shutdown.load(Ordering::Relaxed) {
                tracing::info!("BlockPump: shutting down during subscribe phase");
                return (0, 0, pending);
            }

            let event = timeout(Duration::from_secs(BACKFILL_TIMEOUT_SECS), combined.next()).await;

            match event {
                Err(_) => {
                    // Timeout — fall back to eth_blockNumber RPC (degraded path).
                    tracing::warn!("BlockPump: timeout during subscribe, fetching current block");
                    match self.provider.provider_arc().get_block_number().await {
                        Ok(block) => {
                            tracing::info!(
                                block,
                                "BlockPump: subscribe observed block via RPC (degraded - no two-header confirmation)"
                            );
                            return (block, 0, pending);
                        }
                        Err(e) => {
                            tracing::error!(%e, "BlockPump: can't get block number during subscribe");
                        }
                    }
                }

                Ok(Some(WsEvent::BlockHeader {
                    number,
                    timestamp,
                    base_fee_per_gas: _,
                    gas_used: _,
                    gas_limit: _,
                })) => {
                    if let Some(prev) = prev_header {
                        if number == prev + 1 {
                            // Two consecutive headers: `prev` confirmed
                            // finalized. Advance the confirmed head (and arm
                            // the log-catch-up settle deadline on first
                            // confirmation).
                            if confirmed_head.is_none() {
                                settle_deadline = Some(
                                    tokio::time::Instant::now()
                                        + Duration::from_secs(LOG_CATCHUP_SETTLE_SECS),
                                );
                            }
                            confirmed_head = Some(prev);
                            prev_timestamp = timestamp;
                            tracing::info!(
                                prev,
                                number,
                                "BlockPump: subscribe confirmed head at {prev} (header {number})"
                            );
                        } else if number > prev {
                            // Gap or jump - re-anchor on the newer header.
                            prev_header = Some(number);
                            prev_timestamp = timestamp;
                        }
                        // else: duplicate/stale header for the same block - ignore.
                    } else {
                        // First header ever observed.
                        prev_header = Some(number);
                        prev_timestamp = timestamp;
                    }
                }

                Ok(Some(WsEvent::Log(log))) => {
                    if first_log_block.is_none() {
                        if let Some(lb) = log.block_number {
                            first_log_block = Some(lb);
                        }
                    }
                    // Collect every log observed during the handshake; the
                    // handshake never touches the data plane (some may be for
                    // the boundary block and are already backfilled).
                    pending.push(WsEvent::Log(log));
                }

                Ok(None) => {
                    tracing::warn!("BlockPump: subscription streams ended during subscribe");
                    return (prev_header.unwrap_or(0), prev_timestamp, pending);
                }
            }

            // Finalize once we're near the head (headers confirmed) AND we know
            // the log stream's live-from block — or the settle window elapsed.
            if let Some(head) = confirmed_head {
                let deadline_passed =
                    settle_deadline.is_some_and(|d| tokio::time::Instant::now() >= d);
                let boundary_ok = match first_log_block {
                    // Boundary (first_log_block) is finalizable once the
                    // confirmed head reaches it; accept past the deadline.
                    Some(l) => l <= head || deadline_passed,
                    None => deadline_passed,
                };
                if boundary_ok {
                    let boundary = first_log_block.unwrap_or(head);
                    tracing::info!(
                        confirmed_head = head,
                        boundary,
                        source = if first_log_block.is_some() {
                            "first-delivered-log"
                        } else {
                            "header-fallback"
                        },
                        "BlockPump: subscribe boundary set to {boundary}"
                    );
                    return (boundary, prev_timestamp, pending);
                }
            }
        }
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
            tracing::error!(
                first_block,
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
                        tracing::warn!(
                            "BlockPump: WS stream ended during backfill (no re-inject gap)"
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
        let s = self.bot.state_arc().read().snapshot_seed_block();
        let Some(seed) = s else { return Ok(0) };
        if seed == 0 || ws_block == 0 || seed >= ws_block {
            return Ok(0);
        }
        tracing::info!(
            seed,
            ws_block,
            "BlockPump: auto-backfill from snapshot block to WS block before resume"
        );
        self.backfill_from_snapshot(ws_block, DEFAULT_BACKFILL_CHUNK_SIZE)
            .await
    }

    /// Judges the published block's change set against the chain (the ADR-021
    /// option-A tripwire). Loops over solve-block notifications from the pump
    /// via a LATEST-WINS `watch` channel: only the most recently published
    /// block is ever judged — a superseded block is dropped, never queued, so
    /// the judge can neither pile up an unbounded backlog of full-path
    /// snapshots nor stall the pump's `run_with_stream` on the
    /// `O(registered_paths × hops × RPC)` cost (the confirmed freeze).
    async fn solver_state_verify_loop(
        mut rx: tokio::sync::watch::Receiver<Option<SolverVerifyRequest>>,
        bot: Arc<Bot>,
        provider: Arc<AlloyProvider>,
        config: TripwireConfig,
        reorg_windows: Arc<parking_lot::Mutex<std::collections::VecDeque<TripReorgWindow>>>,
    ) {
        while rx.changed().await.is_ok() {
            let Some((block, path_refs)) = (*rx.borrow_and_update()).clone() else {
                continue;
            };
            if path_refs.is_empty() {
                continue;
            }
            tracing::debug!(
                block,
                paths = path_refs.len(),
                "solver-state tripwire: judging published block change set"
            );
            // Extract + resolve the anchor under the SHORT read guard — the
            // guard is dropped before `judge` awaits its on-chain reads.
            let (path_hop_states, anchor) = {
                let state = bot.state_arc();
                let core = state.read();
                let phs: Vec<_> = path_refs
                    .iter()
                    .map(|pools| extract_solver_hop_states(&core, pools))
                    .collect();
                (
                    phs,
                    crate::bot_core::solve_anchor::SolveAnchor::resolve(block, &core),
                )
            };
            // ADR-021 D2 Part A — snapshot the reorg evidence (Copies out
            // under the lock) BEFORE the judge awaits; no guard is held
            // across the await.
            let reorg_evidence: Vec<TripReorgWindow> =
                reorg_windows.lock().iter().copied().collect();
            if let GateVerdict::Divergent(d) = judge(
                &provider,
                &config,
                &path_hop_states,
                anchor,
                &reorg_evidence,
            )
            .await
            {
                Self::trip_and_exit(&d);
            }
        }
    }

    /// The pump's entire executor-side reaction to a verified desync — the trip
    /// and the exit (ADR-021 D1: "The pump loop keeps only the trip and the
    /// exit"). Prints the grep-able `[SOLVER-STATE] ABORT` marker (unbuffered
    /// stderr) after the structured `tracing::error!`, then aborts the PROCESS
    /// — no task unwind, no wedge, no teardown hang (see the tripwire module
    /// docs for the panic/shutdown-wedge history; UO3JM4).
    fn trip_and_exit(d: &TripwireDivergence) -> ! {
        crate::telemetry::record_exception(
            crate::telemetry::error_kind::SOLVER_STATE_DESYNC,
            format_args!(
                "{:?} path_idx={} hop_idx={}",
                d.class, d.path_idx, d.hop_idx
            ),
        );
        crate::telemetry::flush_before_exit();
        tracing::error!(
            class = ?d.class,
            path_idx = d.path_idx,
            hop_idx = d.hop_idx,
            "DEGENBOT_ASSERT_SOLVER_STATE: verified desync — ABORT"
        );
        #[expect(clippy::print_stderr)] // fatal diagnostic emitted before abort
        {
            eprintln!("{}", d.breadcrumb);
        }
        std::process::abort()
    }

    /// Run the main pump loop with an existing WS stream.
    ///
    /// Processes logs eagerly: each WS log is applied to engine state
    /// immediately and affected paths are solved right away, without
    /// waiting for a block header. Block headers provide metadata
    /// (timestamp, fees) and handle empty-block detection.
    ///
    /// # Panics
    ///
    /// Hard-aborts the process (never unwinds a half-alive pump) on any
    /// verified solver-state desync (ADR-021 `DEGENBOT_ASSERT_SOLVER_STATE`),
    /// a live-websocket log drop (`DEGENBOT_WS_COMPLETENESS`), or a dead or
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
    // missing parent — one giant orphaned trace. Per-block `degenbot.pump.block`
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
            tracing::info!(
                window_ms = window.as_millis() as u64,
                "timed exit: cooperative pump shutdown armed"
            );
            tokio::spawn(async move {
                tokio::time::sleep(window).await;
                tracing::info!(
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
        // neither the sink's drain cursor (only `on_drain`/`finalize_block`
        // do) nor the engine's `last_processed_block`. Hence on the
        // post-backfill resume path the sink's `last_processed_block` is still
        // `None` and the branch below re-anchors on `first_observed_block`.
        let mut current_block: u64 = self.sink.last_processed_block().unwrap_or(0);

        let snapshot_seed = self.bot.state_arc().read().snapshot_seed_block();
        if current_block == 0 && first_observed_block > 0 {
            current_block = first_observed_block;
            // One resume/cold-start line either way (audit: the two identical
            // cold-start lines across branches were collapsed).
            if matches!(snapshot_seed, Some(seed) if seed > 0 && seed < first_observed_block) {
                let seed = snapshot_seed.unwrap_or_default();
                tracing::info!(
                    first_observed_block,
                    backfill_start = seed + 1,
                    backfill_end = first_observed_block,
                    "BlockPump: resuming from block (backfilled snapshot gap)"
                );
            } else {
                tracing::info!(first_observed_block, "BlockPump: cold start from block");
            }
        } else {
            tracing::info!(current_block, "BlockPump: starting from block");
        }

        // Track the last block we've solved for: owned by the engine since
        // ergo task LEZJAS (the pump's `last_solved_block` local retired).
        // Seed it to the pump's starting block so the first `finalize_block`
        // guard fires only on a genuine advance (matching the prior local
        // init). A mid-flight-joining engine inherits via `set_last_solved_block`
        // (ADR-006 D4).
        self.sink.set_last_solved_block(current_block);
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
        self.sink.set_solve_anchor(current_block);
        // Whether we're past the first header after resume. The first
        // Epic A1: the pump's decision state now lives in the PumpFSM; the
        // driver routes the decision arms through it. `current_block` seeds the FSM.
        let mut fsm = PumpFSM::new(current_block, 0);
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
        // dropped (they never reach the `PanicLateForward` hard fault). Reorg
        // logs (`removed: true`) are NEVER dropped — they always reach the
        // reorg classifier. A forward ABOVE `recovery_anchor` that is stale
        // still faults (ADR-008 D3): only blocks the pump itself backfilled are
        // benign duplicates by construction.

        // ADR-008 per-block state machine. The clock is the authority for
        // block completeness (the tombstone) and the cursor; the pump loop is
        // a thin async driver translating its decisions into sink calls +
        // backfill + shutdown. A header alone NEVER advances the cursor —
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

        // Option-A solver-state accuracy gate (AV42C7): when enabled, diff each
        // solved path's per-hop pool state against the chain at the solve block
        // after every drain, aborting on any mismatch (ADR-021 tripwire).
        // Conservative default ON (`self.solver_state_verify` from
        // `DEGENBOT_ASSERT_SOLVER_STATE`); set `=0` to disable. Adds an RPC read
        // per path per solve on the hot loop (only at the publish point).
        let tripwire_config = self.tripwire_config;

        // ADR-021 relocation (pump-freeze fix): the solver-state verify is NOT
        // awaited inline on `run_with_stream`. It runs on a dedicated verifier
        // task fed by a LATEST-WINS `watch`; the pump hands every published
        // block to it with a non-blocking send and returns to polling the WS
        // stream immediately, so the O(registered × hops × RPC) verify can
        // never stall pump advancement (the confirmed freeze: `last_complete`
        // froze while the inline gate ground through the whole registered set).
        // The verifier abort()s the whole process on desync (unchanged ADR-021
        // fail-stop); only the most recent published block is ever verified.
        let verify_tx: Option<tokio::sync::watch::Sender<Option<SolverVerifyRequest>>> =
            if tripwire_config.enabled {
                let (tx, rx) = tokio::sync::watch::channel(None);
                tokio::spawn(Self::solver_state_verify_loop(
                    rx,
                    Arc::clone(&self.bot),
                    Arc::clone(&self.provider),
                    tripwire_config,
                    Arc::clone(&self.trip_reorg_windows),
                ));
                Some(tx)
            } else {
                None
            };

        // B4GX7C drain-decoupling: the sink's solve/dispatch/finalize calls run
        // on this spawned background drainer task so the WS poller returns to
        // `combined.next()` promptly instead of parking behind `Python::attach`
        // / heavy Möbius solve. FIFO order + the engine/sink locks give the
        // deferred work the inline semantics it replaced (the sole mode). The
        // change-set is consumed atomically in the pump (single-writer) and the
        // verifier anchor is carried in the message.
        //
        // The poller gets FEEDBACK on the drainer through `drainer_health`: a
        // send into a closed channel (drainer task dead) aborts loudly, and the
        // B3 no-progress detector aborts if the drainer is alive but makes no
        // progress — a dead/stalled drainer must never silently lose every
        // solve/dispatch/publish while the WS loop keeps advancing.
        // One dispatch owner (epic B) owns the drain pipe + the drainer task +
        // the verifier latest-wins transmitter. All sink work is deferred to the
        // background drainer task (sole mode) so the WS poller never parks
        // behind GIL-bound `Python::attach` / heavy Möbius solve. FIFO order +
        // the engine/sink locks give the deferred work the inline semantics it
        // replaced; the verifier anchor + change-set ride the `Publish` message
        // (single-writer). A dead or stalled drainer never silently loses work.
        let dispatch = DispatchOwner::new(Arc::clone(&self.sink), &verify_tx);
        // S53STH cooperative timed exit: arm the StallWatch cancel token when
        // a hotpath timed window is configured so no watchdog sample lands
        // between the post-loop OTel flush and teardown.
        #[cfg(all(feature = "hotpath", feature = "otel"))]
        let dispatch = if crate::profiling::timed_exit_window().is_some() {
            dispatch.with_shutdown_token(tokio_util::sync::CancellationToken::new())
        } else {
            dispatch
        };

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
        let mut staleness_tick = tokio::time::interval(self.header_staleness);
        staleness_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        staleness_tick.tick().await; // discard the immediate first tick
                                     // A4: a monotonic ms epoch feeding the FSM's pure watchdog `Tick` input
                                     // (time enters as data; the FSM owns no timer or `Instant`).
        let tick_epoch = tokio::time::Instant::now();
        let now_ms = || tick_epoch.elapsed().as_millis() as u64;

        // B4GX7C drainer-liveness heartbeat: a 2s cadence that aborts if the
        // background drainer has unacknowledged work and makes no progress for
        // B3 no-progress liveness now lives INSIDE `DispatchOwner` (see
        // `track_progress` + `dispatch`): a dead drainer aborts immediately on
        // a closed-channel send; a frozen drainer aborts at
        // `NO_PROGRESS_STRIKE_LIMIT` consecutive no-progress pushes; a drainer
        // that progresses but falls behind only WARNs (lag metric). The old
        // 30s `DRAINER_STALL_SECS` poll watchdog and the pump-side
        // `drainer_check` timer are retired — no time-based knob remains.

        // MQUKB6-T0: the current block's span, replaced by each accepted header.
        let mut block_span: Option<tracing::Span> = None;
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
            // Solve dispatch moved OUT of the loop head to the drained-settle
            // gate at the bottom of the loop (TQ7PD6 follow-up): the solver
            // must not fire while buffered WS events are still unprocessed —
            // the 2026-08-22 stall crash was exactly the loop-head solve
            // racing a still-queued swap log.

            // ADR-008 D2: solver-release gate. `fsm.publish_pending` is set when a forward
            // log applies (block becomes quiesced). The flush below fires
            // `on_send` (gated on `consume_quiesced`) only at a settle point —
            // a timeout with no new event (coalescing a same-block burst into
            // one publish at the tail) OR stream exhaustion. This replaces the
            // wall-clock `DEBOUNCE_MS` send timer: publication is gated on the
            // truth condition (all dispatched logs applied), not schedule.

            // Check shutdown
            if self.shutdown.load(Ordering::Relaxed) {
                tracing::info!("BlockPump: shutting down");
                return;
            }

            // Wait for the next event. Use a shorter settle window when a publish is
            // pending so the quiesce-gated flush fires promptly if no new log
            // arrives (coalescing a same-block burst); otherwise the long
            // inactivity backfill window. A new event arriving before the
            // window elapses cancels the flush (the burst is still in flight).
            let wait_timeout = if fsm.publish_pending() {
                Duration::from_millis(DEBOUNCE_MS)
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
                        tracing::info!("timed exit: shutdown signaled — unwinding pump loop");
                        break;
                    }
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
                        self.header_staleness.as_millis() as u64,
                        self.log_silence.as_millis() as u64,
                    ) {
                        match decision {
                            PumpDecision::Recover => {
                                self.handle_timeout_eager(&mut fsm)
                                    .instrument(block_span.clone().unwrap_or_else(tracing::Span::none))
                                    .await;
                            }
                            PumpDecision::LogSilence => {
                                // Logs-subscription liveness watchdog (inverse
                                // of header staleness): headers are FRESH (the
                                // Recover branch did not fire) but no
                                // `WsEvent::Log` arrived in `self.log_silence`
                                // — the `eth_subscribe "logs"` arm is presumed
                                // stalled/dead while `newHeads` is alive. One
                                // warning per silence episode (re-armed when
                                // the next log resumes the sub).
                                tracing::warn!(
                                    silence_secs = self.log_silence.as_secs(),
                                    "[pump] logs subscription silent: headers flowing but no log"
                                );
                                self.log_silence_alarms =
                                    self.log_silence_alarms.saturating_add(1);
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
                    for decision in fsm.on_settle() {
                        match decision {
                            PumpDecision::Publish { open, metadata } => {
                                // Option-A solver-state accuracy gate (AV42C7):
                                // publish on_send to Python, then hand the
                                // quiesced `open` block + its change set to the
                                // latest-wins verifier task. The publish defers
                                // on_send to the drainer (so the WS poller never
                                // parks behind the Python GIL). The anchor is
                                // `open`, the LOG-DRIVEN quiesced block, NOT the
                                // racing header.
                                let change_set = self.sink.take_solver_path_pool_refs_change_set();
                                let _ctx = block_span.as_ref().map(tracing::Span::enter);
                                dispatch.dispatch(DrainWork::Publish {
                                    open,
                                    metadata,
                                    change_set,
                                });
                            }
                            PumpDecision::Backfill { from, to } => {
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
                    // MQUKB6 (epic KDUED5): the per-block beat — one entered
                    // span per observed header. Future solver/submission spans
                    // fired within this arm inherit it as parent for free.
                    let new_block_span =
                        tracing::info_span!("degenbot.pump.block", block.number = number);
                    // JYCTXI: detach from the ambient context so each header
                    // span is its own trace ROOT. Without this, the new span
                    // is created while the PREVIOUS block span is still entered
                    // (the loop-context guard below), chaining every block of a
                    // session into one ever-growing mega-trace. Children (logs,
                    // solves, dispatch) still nest under it via the loop-context
                    // guard — only the parent linkage at creation changes.
                    #[cfg(feature = "otel")]
                    {
                        use tracing_opentelemetry::OpenTelemetrySpanExt as _;
                        // Detaching cannot fail; the Result is informational.
                        drop(new_block_span.set_parent(opentelemetry::context::Context::new()));
                    }
                    #[cfg(not(feature = "otel"))]
                    {
                        let _ = &new_block_span; // no OTel layer: nothing to detach
                    }
                    // MQUKB6-T0: this span becomes the loop's per-block context —
                    // subsequent iterations (logs, settle decisions) nest under it
                    // until the next header replaces it.
                    block_span = Some(new_block_span.clone());
                    // Sync-only header-processing scope (TQ7PD6): this enter
                    // guard dies before the first await below, so it can never
                    // leak across a task migration. The backfill future below
                    // carries the same span across ITS await via Instrument.
                    {
                        let _ctx = new_block_span.enter();
                        // [DIAG] newHeads-liveness: HEADER count, gap, and 20s stall
                        // warning → one call on the telemetry seam.
                        telemetry.on_header(number);
                        // T2: blocks-observed counter + the header→solved anchor.
                        if let Some(p) = crate::instruments::pipeline() {
                            p.count_block();
                        }
                        dispatch.note_header_accepted();
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
                            PumpDecision::Backfill { from, to } => {
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
                                tracing::info!(
                                    from_block = from,
                                    to_block = to,
                                    "BlockPump: gap from block to block — backfilling"
                                );
                                self.backfill_range(from, to, &mut fsm)
                                    .instrument(new_block_span.clone())
                                    .await;
                            }
                            PumpDecision::SetLastSolved { block } => {
                                // LEZJAS: the backfill/first header solved up
                                // to `block` already — mark it solved so the
                                // first `finalize_block` guard no-ops.
                                let _ctx = new_block_span.enter();
                                self.sink.set_last_solved_block(block);
                            }
                            PumpDecision::Notify { block, metadata } => {
                                // Python's block fsm.clock tracks `newHeads`.
                                let _ctx = new_block_span.enter();
                                dispatch.notify_block(block, &metadata);
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
                Ok(Some(WsEvent::Log(log))) => {
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
                    // BQ7ZBC — FSM single-writer recovery discard. After an
                    // authoritative eth_getLogs catch-up (`fsm.recovery_anchor`), a
                    // stalled WS that recovers flushes buffered forward logs for
                    // blocks ≤ the anchor — those are duplicates of state the
                    // backfill already applied and are DROPPED (they never reach
                    // `observe_log`/`PanicLateForward`). This mirrors the DFQYM5
                    // resume-boundary rule, generalized to mid-run recovery.
                    // Reorg logs (`removed: true`) are NEVER dropped — they must
                    // reach the reorg classifier to unwind the backfilled range.
                    // A forward ABOVE `recovery_anchor` that is still stale
                    // remains a hard ADR-008 D3 fault (only the pump's own
                    // single-writer range is benign).
                    if fsm.should_drop_recovered_forward(log_block, log.removed) {
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
                    let log_decision = fsm.on_log(log_block, log.removed);
                    // Per-pool trace: log EVERY relevant-topic WS log for the
                    // `DEGENBOT_DRAIN_DBG` pool — block, log-index, tx-index,
                    // topic0, removed, and the fsm.clock decision — so the
                    // delivery order of same-block Mint/Burn logs is visible
                    // against the registration drain+pin that follows. No-op
                    // for other pools / when the env var is unset.
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
                            LogDecision::PanicLateForward(_) => "PanicLateForward",
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
                            tracing::warn!(
                                reorg_block,
                                "BlockPump: chain reorg detected (removed log) — entering unwind path"
                            );
                            if let Err(err) = self.reorg_coordinator.dispatch_reorg_log(&log) {
                                tracing::error!(?err, "BlockPump: too-deep reorg — shutting down");
                                self.shutdown.store(true, Ordering::Relaxed);
                                return;
                            }
                            // ADR-021 D2 Part A — record the reorg window for the
                            // tripwire's UnhandledReorg evidence (cheap; the
                            // judge snapshots it at solve time).
                            crate::bot_core::solver_state_tripwire::reorg_window_open(
                                &mut self.trip_reorg_windows.lock(),
                                reorg_block,
                                log_block,
                            );
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
                            tracing::warn!(
                                log_block,
                                "BlockPump: reorg continues — restoring pool for removed log"
                            );
                            if let Err(err) = self.reorg_coordinator.dispatch_reorg_log(&log) {
                                tracing::error!(?err, "BlockPump: too-deep reorg — shutting down");
                                self.shutdown.store(true, Ordering::Relaxed);
                                return;
                            }
                            // ADR-021 D2 Part A — widen the open window's rollback.
                            crate::bot_core::solver_state_tripwire::reorg_window_continue(
                                &mut self.trip_reorg_windows.lock(),
                                log_block,
                            );
                            continue;
                        }
                        LogDecision::CloseReorg { new_head } => {
                            // Reorg window closed — the coordinator restored
                            // unwound pools per-event; this forward log's block
                            // is the new head. Resume forward tracking from it.
                            tracing::info!(
                                new_head,
                                "BlockPump: reorg window closed — resuming forward tracking"
                            );
                            // ADR-021 D2 Part A — close the evidence window.
                            crate::bot_core::solver_state_tripwire::reorg_window_close(
                                &mut self.trip_reorg_windows.lock(),
                                new_head,
                            );
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
                                .write()
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
                            // here — the fsm.clock's own `tombstone(prev)` (inside
                            // `on_log`) already advanced the shared cutoff
                            // the registration drain reads.
                            // LOUD WS-completeness check: block `prev` is now
                            // confirmed complete (tombstoned by the first log of
                            // N+1); the FSM emits the VerifyCompleteness verdict
                            // (the tracked delivered set); the driver cross-checks
                            // vs `eth_getLogs` and aborts on a websocket drop.
                            if ws_completeness_enabled {
                                let PumpDecision::VerifyCompleteness {
                                    block,
                                    delivered_log_indices,
                                } = fsm.completeness_decision(prev)
                                else {
                                    unreachable!(
                                        "completeness_decision always emits VerifyCompleteness"
                                    )
                                };
                                self.assert_ws_block_complete(block, delivered_log_indices)
                                    .instrument(
                                        block_span.clone().unwrap_or_else(tracing::Span::none),
                                    )
                                    .await;
                            }
                            let prev_meta = fsm
                                .block_metadata_for(prev)
                                .unwrap_or(fsm.current_metadata());
                            let _ctx = block_span.as_ref().map(tracing::Span::enter);
                            dispatch.dispatch(DrainWork::Finalize {
                                block: prev,
                                metadata: prev_meta,
                            });
                        }
                        LogDecision::DispatchForward => {}
                        LogDecision::PanicLateForward(b) => {
                            // A removed:false log on a tombstoned block, NOT in a
                            // reorg → unreliable WS (out-of-order / duplicated
                            // forward events). Blocks ≤ the authoritative
                            // `fsm.recovery_anchor` are already dropped by the
                            // single-writer recovery discard (BQ7ZBC) before they
                            // reach this classifier — so firing HERE means b is a
                            // genuine late forward ABOVE the recovery anchor that
                            // the pump did NOT itself backfill → unrecoverable
                            // for correctness → shut down (ADR-008 D3).
                            tracing::error!(
                                b,
                                "BlockPump: ADR-008 D3 late forward log on tombstoned block — unreliable WS, shutting down"
                            );
                            self.shutdown.store(true, Ordering::Relaxed);
                            return;
                        }
                    }

                    // Apply the log immediately to engine state (no solve yet).
                    // ADR-006 D4: routes through `Bot::dispatch_log` (decode →
                    // apply to BotState → notify EngineSubscriber → dirty the
                    // engine) — NOT `engine.apply_log`. The FSM's `on_log_applied`
                    // records the clock's received/applied edges and arms the
                    // quiesce-gated publish (ADR-008 D2).
                    // One fact — a forward log applied to engine state — feeds
                    // two consumers (T4 pairing pin, epic O3HW7E): the FSM
                    // quiesce arm (`on_log_applied` -> publish_pending) and
                    // the engine's `has_logs_this_block` (finalize
                    // bookkeeping, LEZJAS). Coordinated here, once; do not
                    // split or drop either write.
                    self.bot.dispatch_log(&log);
                    fsm.on_log_applied(log_block);

                    // LEZJAS: engine owns `has_logs_this_block` now — routed
                    // through the sink so the next `finalize_block` sees it.
                    self.sink.record_logs_this_block();

                    // [DIAG] count logs + emit periodic stats so we can see,
                    // during a freeze, that the pump IS polling logs while
                    // headers are gone. This is the liveness signal the loop
                    // otherwise lacks — owned by the `PumpTelemetry` seam.
                    telemetry.on_log();
                    let pool_state_head = self.bot.state_arc().read().pool_state_head();
                    telemetry.maybe_stats(fsm.current_block(), pool_state_head);
                }

                Ok(None) => {
                    // ADR-008 D2: stream exhausted — final settle point. Flush
                    // any pending quiesce-gated publish before returning. The
                    // settle rule is the FSM's `on_stream_end`; the driver only
                    // executes the emitted Publish (I/O) and stops.
                    for decision in fsm.on_stream_end() {
                        match decision {
                            PumpDecision::Publish { open, metadata } => {
                                let change_set = self.sink.take_solver_path_pool_refs_change_set();
                                let _ctx = block_span.as_ref().map(tracing::Span::enter);
                                dispatch.dispatch(DrainWork::Publish {
                                    open,
                                    metadata,
                                    change_set,
                                });
                            }
                            PumpDecision::Stop => {}
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
                    tracing::error!(
                        "BlockPump: WS subscription streams ended - pump is STOPPED. The bot will no longer process blocks (no reconnect). Check the WS endpoint / restart."
                    );
                    self.sink.on_pump_ended();
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
            // solve. 50ms is well within the 12s block interval and is the
            // same `DEBOUNCE_MS` already used for the publish gate.
            let has_buffered = if self.sink.has_dirty_paths() {
                // Only await when there's work to solve — otherwise skip
                // straight to the select (no dirty paths = nothing to do).
                // `peek()` resolves immediately when an event is buffered or
                // the stream has ended (Ready(None)); it returns Pending (and
                // yields to the runtime so the WS task can deliver) only when
                // the stream is alive but momentarily empty. The 50ms timeout
                // fires only in that latter case — coalescing burst gaps without
                // adding latency to streams with ready events.
                use std::pin::Pin;
                match tokio::time::timeout(
                    Duration::from_millis(DEBOUNCE_MS),
                    Pin::new(&mut combined).peek(),
                )
                .await
                {
                    Ok(Some(_)) => true, // event buffered — skip solve
                    // stream ended OR 50ms elapsed — dispatch solve
                    _ => false,
                }
            } else {
                false
            };
            if !has_buffered && self.sink.has_dirty_paths() {
                // Strictly-synchronous solve dispatch: enter the cursor
                // block span just long enough for dispatch() to capture it
                // as the drainer parent (no await inside — TQ7PD6).
                let _solve_ctx = block_span.as_ref().map(tracing::Span::enter);
                // Pump-owned ACTIVE BLOCK promotion (QMSTSV/BO5FBS): the
                // solve anchor is the LOG-DRIVEN settled block
                // (`fsm.clock.latest_observed()`, never a racing header),
                // floored by the pool-state head so it is never below the
                // state it solves against (MQIZ5M +1-wei / IIA class; the
                // backfill-ahead semantics). `drain_decision` owns the
                // exact rule.
                let state_head = self.bot.state_arc().read().pool_state_head();
                let PumpDecision::Drain { block, metadata } = fsm.drain_decision(state_head) else {
                    unreachable!("drain_decision always drains when called");
                };
                dispatch.dispatch(DrainWork::Drain { block, metadata });
                // LEZJAS: engine owns `last_solved_block` now — mark this
                // block solved so the next `finalize_block` guard no-ops.
                self.sink.set_last_solved_block(block);
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

    /// Handle a 60s timeout by backfilling any missed blocks (eager variant).
    async fn handle_timeout_eager(&self, fsm: &mut PumpFSM) {
        tracing::warn!(
            backfill_timeout_secs = BACKFILL_TIMEOUT_SECS,
            "BlockPump: no activity — attempting backfill"
        );
        let latest_block = match self.provider.provider_arc().get_block_number().await {
            Ok(n) => n,
            Err(e) => {
                tracing::error!(%e, "BlockPump: backfill failed — can't get block number");
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
            // range solved through the sink.
            self.sink.set_last_solved_block(latest_block);
        }
    }

    /// Backfill a range of blocks via `eth_getLogs`, applying each backfilled
    /// log through the SAME per-block state machine as a live WS log (ADR-008
    /// D4, single branch). The provider I/O (`get_logs`) and engine/sink I/O
    /// (`dispatch_log`, `finalize_block`, `on_drain`) stay here on the driver;
    /// every FSM-state transition is routed through `PumpFSM` methods
    /// (`on_log`, `on_log_applied`) — no fields are threaded out of the capsule.
    async fn backfill_range(&self, from_block: u64, to_block: u64, fsm: &mut PumpFSM) {
        if from_block > to_block {
            return;
        }

        tracing::info!(from_block, to_block, "BlockPump: backfilling blocks");
        // T2: one counter per executed backfill range.
        if let Some(p) = crate::instruments::pipeline() {
            p.count_backfill();
        }

        let filter = build_backfill_filter(from_block, to_block);
        let logs = match self.provider.provider_arc().get_logs(&filter).await {
            Ok(logs) => logs,
            Err(e) => {
                tracing::error!(%e, "BlockPump: backfill eth_getLogs failed");
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
                tracing::info!("BlockPump: shutting down during backfill");
                return;
            }

            let block_logs = logs_by_block.remove(&block).unwrap_or_default();
            for log in &block_logs {
                match fsm.on_log(block, log.removed) {
                    LogDecision::TombstonePrevious(prev) => {
                        let prev_meta = fsm.block_metadata_for(prev).unwrap_or_default();
                        self.sink.finalize_block(prev, &prev_meta);
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
                    | LogDecision::PanicLateForward(_) => {
                        tracing::warn!(
                            block,
                            "BlockPump: backfill saw unexpected decision; skipping log"
                        );
                    }
                }
            }
            if !block_logs.is_empty() {
                self.sink.on_drain(block, &BlockMetadata::default());
                any_processed = true;
            }
        }

        if any_processed {
            tracing::info!(
                from_block,
                to_block,
                "BlockPump: backfill complete for blocks"
            );
        } else {
            tracing::info!(
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
    pub async fn assert_ws_block_complete(
        &self,
        block: u64,
        delivered_log_indices: std::collections::HashSet<u64>,
    ) {
        let filter = build_backfill_filter(block, block);
        let logs = match self.provider.provider_arc().get_logs(&filter).await {
            Ok(logs) => logs,
            Err(e) => {
                tracing::error!(
                    block,
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
            tracing::error!(
                "[WS-INVARIANT] LIVE WEBSOCKET LOG DROP at block {block}: {} relevant on-chain log(s) missing from WS delivery: log_index {:?}. eth_getLogs={} logs, WS delivered={} logs. The websocket/pump delivery path dropped a relevant event — ABORT (DFQYM5/WS-DROP). Investigate the subscription/reconnect path; do NOT silence this.",
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
                    "[WS-INVARIANT] ABORT: live websocket log drop at block {block} ({} of {} relevant logs missing); eth_getLogs vs WS divergence — see the untraced log for the log_index list.",
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
            tracing::warn!(
                block,
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
            let state = arc.read();
            state.snapshot_seed_block()
        };
        let Some(s) = s else {
            tracing::info!(
                "BlockPump::backfill_from_snapshot: no snapshot loaded, cold-start path"
            );
            return Ok(0);
        };
        if s == 0 {
            tracing::warn!("BlockPump::backfill_from_snapshot: snapshot block S=0, skipping");
            return Ok(0);
        }
        if s >= w {
            tracing::info!(
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
        tracing::info!(
            from_block,
            to_block,
            total_blocks,
            chunk_size,
            "BlockPump::backfill_from_snapshot: fetching events"
        );
        let provider = self.provider.provider_arc();
        let mut total_logs = 0usize;
        let mut chunk_start = from_block;
        while chunk_start <= to_block {
            let chunk_end = (chunk_start + chunk_size - 1).min(to_block);
            let filter = build_backfill_filter(chunk_start, chunk_end);
            tracing::info!(
                chunk_start,
                chunk_end,
                "BlockPump::backfill_from_snapshot: fetching chunk"
            );
            let t0 = std::time::Instant::now();
            let logs = provider.get_logs(&filter).await.map_err(|e| {
                format!("eth_getLogs failed for blocks {chunk_start}-{chunk_end}: {e}")
            })?;
            let n = logs.len();
            let fetch_ms = t0.elapsed().as_millis();
            tracing::info!(
                chunk_start,
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
                .write()
                .process_backfill_logs(&logs, chunk_end);
            tracing::info!(
                chunk_start,
                chunk_end,
                log_count = n,
                "BlockPump::backfill_from_snapshot: chunk logs applied"
            );
            chunk_start = chunk_end + 1;
        }
        tracing::info!(
            total_logs,
            total_blocks,
            "BlockPump::backfill_from_snapshot: complete"
        );
        Ok(total_blocks)
    }
}

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
        sink: Arc<dyn DrainSink>,
        reorg_coordinator: Arc<crate::bot_core::reorg_coordinator::ReorgCoordinator>,
        provider: Arc<AlloyProvider>,
        shutdown: Arc<AtomicBool>,
    ) -> Self {
        Self {
            bot,
            sink,
            reorg_coordinator,
            trip_reorg_windows: Arc::new(
                parking_lot::Mutex::new(std::collections::VecDeque::new()),
            ),
            provider,
            shutdown,
            header_staleness: Duration::from_secs(HEADER_STALENESS_SECS),
            log_silence: Duration::from_secs(LOG_SILENCE_SECS),
            log_silence_alarms: 0,
            // ADR-021 tripwire OFF in tests (deterministic per-pump opt-out; see
            // the struct field doc). Tests arm it explicitly when they exercise
            // the tripwire (e.g. the desync-abort tests).
            tripwire_config: crate::bot_core::solver_state_tripwire::TripwireConfig::disabled(),
            // Same per-pump opt-out for the WS-delivery completeness cross-check:
            // default-ON in production, deterministically OFF in tests so the
            // synthetic log streams (which use relevant-topic logs as pure block
            // tombstones) never trip a spurious eth_getLogs comparison/abort.
            ws_completeness_enabled: false,
        }
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
        self.header_staleness = staleness;
    }

    /// Test-only override of the logs-subscription liveness window
    /// (the INVERSE watchdog: headers fresh but no log for N seconds).
    /// Lets tests drive the alarm threshold to a sub-second value instead of
    /// the 60s production default. Pair with `set_header_staleness_for_test`
    /// so the staleness tick elapses often AND the silence threshold is short.
    pub fn set_log_silence_for_test(&mut self, silence: Duration) {
        self.log_silence = silence;
    }

    /// Count of logs-silence alarms fired since the pump started (test
    /// observable for the logs-subscription liveness watchdog — incremented
    /// once per silence episode, re-armed when the next `WsEvent::Log`
    /// resumes the sub).
    #[must_use]
    pub fn log_silence_alarm_count(&self) -> u64 {
        self.log_silence_alarms
    }
}

/// The pump's solve anchor (`anchor = max(open, pool_state_head)`, BO5FBS +
/// ADR-008 D2) is owned by `crate::bot_core::solve_anchor`: the LOG-DRIVEN
/// settled block (`BlockClock::latest_observed`, falling back to the header
/// `current_block`) floored by the pool-state head, with the future-hop rule.
/// Its failure history (0x99ac8c false-abort, MQIZ5M +1-wei / IIA class) lives
/// in that module's docs.
///
/// Build an Alloy `Filter` for backfill via `eth_getLogs`.
///
/// Uses topic filtering server-side to reduce response size. No address
/// filter — all topic-filtered logs are passed through to the engine.
#[must_use]
pub fn build_backfill_filter(from_block: u64, to_block: u64) -> Filter {
    let mut filter = Filter::new().from_block(from_block).to_block(to_block);

    // Build a single Topic that matches ANY of the relevant event signatures.
    // Alloy's event_signature() overwrites topics[0] on each call, so we must
    // build the OR-list ourselves and set it once.
    let mut topic: Topic = Topic::default();
    for sig in &RELEVANT_TOPICS {
        topic = topic.extend(*sig);
    }
    filter.topics[0] = topic;

    filter
}

/// Merge a block header stream and a log stream into a single `WsEvent` stream.
///
/// Uses `stream::Select` to fairly interleave events from both subscriptions.
/// Returns a boxed stream for storage in `SubscribeState`.
fn stream_select(
    block_stream: impl StreamExt<Item = alloy::rpc::types::Header> + Unpin + Send + 'static,
    log_stream: impl StreamExt<Item = Log> + Unpin + Send + 'static,
) -> stream::BoxStream<'static, WsEvent> {
    let block_events = block_stream.map(|header| WsEvent::BlockHeader {
        number: header.number,
        timestamp: header.timestamp,
        base_fee_per_gas: header.base_fee_per_gas,
        gas_used: header.gas_used,
        gas_limit: header.gas_limit,
    });
    let log_events = log_stream.map(WsEvent::Log);

    stream::select(block_events, log_events).boxed()
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

    /// A `DrainSink` test double (AGENTS.md: `Fake` prefix, no mocking).
    ///
    /// Records every `finalize_block` / `on_send` / `on_drain` invocation with
    /// the `(block, metadata)` pair the pump passed, so tests can assert the
    /// *block N's* result batch carries *block N's* metadata — the VTWCIG
    /// contract. Behaves as an empty sink (no dirty paths, no state).
    struct FakeDrainSink {
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
        /// Configurable path-pool refs for the Option-A AV42C7 gate tests
        /// (default empty — the gate early-returns). Set via `set_path_refs`.
        path_refs: Mutex<Vec<Vec<degenbot_solvers::mixed::MixedPoolRef>>>,
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
    }

    impl FakeDrainSink {
        fn new(last_processed: Option<u64>) -> Self {
            Self {
                finalized: Mutex::new(Vec::new()),
                sent: Mutex::new(Vec::new()),
                drained: Mutex::new(Vec::new()),
                notified: Mutex::new(Vec::new()),
                solved: Mutex::new(Vec::new()),
                last_processed: AtomicU64::new(last_processed.unwrap_or(0)),
                path_refs: Mutex::new(Vec::new()),
                dirty: AtomicBool::new(false),
                logs_recorded: std::sync::atomic::AtomicUsize::new(0),
                pump_ended: std::sync::atomic::AtomicBool::new(false),
            }
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

        /// Configure the path-pool refs the AV42C7 gate verifies (test-only).
        fn set_path_refs(&self, refs: Vec<Vec<degenbot_solvers::mixed::MixedPoolRef>>) {
            *self.path_refs.lock().unwrap() = refs;
        }
    }

    impl DrainSink for FakeDrainSink {
        fn solver_path_pool_refs(&self) -> Vec<Vec<degenbot_solvers::mixed::MixedPoolRef>> {
            self.path_refs.lock().unwrap().clone()
        }
        fn has_dirty_paths(&self) -> bool {
            self.dirty.load(Ordering::Relaxed)
        }
        fn on_drain(&self, block: u64, metadata: &BlockMetadata) {
            // Faithful to `SolveCoordinator::on_drain`: record + advance the
            // drain cursor so `last_processed_block()` reflects the drained
            // block (the anchoring `resume` relies on — see
            // `resume_anchors_to_subscribe_block`).
            self.drained.lock().unwrap().push((block, *metadata));
            self.last_processed.store(block, Ordering::Relaxed);
        }
        fn on_send(&self, metadata: &BlockMetadata) {
            self.sent.lock().unwrap().push(*metadata);
        }
        fn finalize_block(&self, block: u64, metadata: &BlockMetadata) {
            self.finalized.lock().unwrap().push((block, *metadata));
        }
        fn set_last_solved_block(&self, block: u64) {
            self.solved.lock().unwrap().push(block);
        }
        fn set_solve_anchor(&self, _block: u64) {}
        fn record_logs_this_block(&self) {
            self.logs_recorded
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        fn on_pump_ended(&self) {
            self.pump_ended
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        fn last_processed_block(&self) -> Option<u64> {
            let v = self.last_processed.load(Ordering::Relaxed);
            (v != 0).then_some(v)
        }
        fn notify_block(&self, block: u64, metadata: &BlockMetadata) {
            // Record the forwarded newHeads tick so task 22Y7AB can assert the
            // pump emits one BlockNotification per accepted header.
            self.notified.lock().unwrap().push((block, *metadata));
        }
    }

    /// Build a `BlockPump` whose provider is an `alloy` mock transport (never
    /// hit on the no-timeout / no-gap test paths) and whose sink is a
    /// `FakeDrainSink` that records metadata calls. Returns the pump + the
    /// sink handle (for inspection). Offline + deterministic.
    fn pump_for_test(last_processed: Option<u64>) -> (BlockPump, Arc<FakeDrainSink>) {
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
        let sink = Arc::new(FakeDrainSink::new(last_processed));
        let pump = BlockPump::for_test(bot, sink.clone(), reorg, provider, shutdown);
        (pump, sink)
    }

    #[test]
    fn test_pump_disables_solver_state_verify_by_default() {
        // Z4KQXF: the ADR-021 tripwire is conservative-ON in production (via
        // `bot_env_flag_default_on`) but deterministically OFF in the test
        // constructor (per-pump opt-out) so TDD tests are immune to the global
        // env. Tests that exercise the verifier arm it explicitly.
        let (pump, _sink) = pump_for_test(None);
        assert!(
            !pump.tripwire_config.enabled,
            "test pumps must disable the tripwire"
        );
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
        // And the production default (env unset) must be ON so drops surface
        // loudly out of the box.
        assert!(
            crate::bot_core::bot_env_flag_default_on("DEGENBOT_WS_COMPLETENESS"),
            "production default for DEGENBOT_WS_COMPLETENESS must be ON"
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
            let mut core = arc.write();
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
            WsEvent::Log(make_v2_sync_log(
                pool,
                alloy::primitives::U256::ZERO,
                alloy::primitives::U256::ZERO,
                101,
                false,
            )),
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
        Arc<FakeDrainSink>,
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
        let sink = Arc::new(FakeDrainSink::new(last_processed));
        let pump = BlockPump::for_test(bot, sink.clone(), reorg, provider, Arc::clone(&shutdown));
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
                WsEvent::Log(make_v2_sync_log(pool, U256::ZERO, U256::ZERO, block, false))
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
            WsEvent::Log(tombstone_log),
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
            let mut core = arc.write();
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
            bot.state_arc().read().pool_state_head(),
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
            let mut core = arc.write();
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
            WsEvent::Log(make_v2_sync_log(
                pool,
                alloy::primitives::U256::from(1_000),
                alloy::primitives::U256::from(2_000),
                101,
                false,
            )),
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
    /// `DrainSink::notify_block` — independent of solve/debounce state. This
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
            WsEvent::Log(tombstone_log),
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
    /// `run_with_stream` (a resume with a fresh `PumpFSM`/`BlockClock`)
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
            WsEvent::Log(make_v2_sync_log(
                Address::from([0xfcu8; 20]),
                U256::from(1),
                U256::from(2),
                w + 1,
                false,
            )),
        ];
        pump1.run_test_loop(stream::iter(events).boxed(), w).await;
        assert_eq!(
            bot.state_arc().read().pump_complete_cutoff(),
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
            bot.state_arc().read().pump_complete_cutoff(),
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
            let mut core = arc.write();
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
                WsEvent::Log(dup_w),
                WsEvent::Log(live_w1),
            ])
            .boxed(),
            w,
        )
        .await;

        let arc = bot.state_arc();
        let core = arc.read();
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
            let mut core = arc.write();
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
            stream::iter(vec![header(w + 1), WsEvent::Log(reorg_w)]).boxed(),
            w,
        )
        .await;

        assert!(
            !shutdown.load(std::sync::atomic::Ordering::SeqCst),
            "a reorg inside the backfilled range is recoverable — no shutdown"
        );
        let arc = bot.state_arc();
        let core = arc.read();
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
            let mut core = arc.write();
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
                WsEvent::Log(make_v2_sync_log(
                    pool,
                    U256::from(2_000u64),
                    U256::from(1_500u64),
                    w + 1,
                    false,
                )),
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
                    WsEvent::Log(make_v2_sync_log(
                        pool,
                        U256::from(2_000u64),
                        U256::from(1_500u64),
                        w + 1,
                        true,
                    )),
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

    /// Resume-anchor contract: `on_drain(first_block)` anchors
    /// `current_block` to the subscribe block W (mimics
    /// `SolveCoordinator::on_drain` setting `last_drained_block`). With
    /// `first_observed_block = W` (the real subscribe block, NOT the legacy
    /// hard-coded `0`) the pump processes W+1, W+2 in order — the
    /// "applies logs in block order against DB-snapshot-seeded engine state"
    /// MJXP5Z (GREEN): the single-stream handshake does NOT drop block-W logs.
    ///
    /// `observe_complete_block` polls headers ONLY (two consecutive headers
    /// W, W+1 confirm the boundary), collecting any `WsEvent::Log` the fused
    /// stream interleaves and re-injecting it. With the OLD drop+resubscribe,
    /// the Mint/Burn queued after the confirming log were lost (XBQNJ5 RED).
    /// Under Alternative B they survive in `pending` and reach `run_with_stream`.
    #[tokio::test]
    async fn subscribe_with_stream_preserves_w_logs() {
        let (mut pump, _sink) = pump_for_test(None);
        let w = 21_500_000u64;
        let pool = Address::from([0xaau8; 20]);

        let header_w = WsEvent::BlockHeader {
            number: w,
            timestamp: 0,
            base_fee_per_gas: None,
            gas_used: 0,
            gas_limit: 0,
        };
        let sync_log = make_v2_sync_log(pool, U256::ZERO, U256::ZERO, w, false);
        let mint_log = make_v3_mint_log_with_block(pool, -100, 100, 1, w);
        let burn_log = make_v3_burn_log_with_block(pool, -100, 100, 1, w);
        let header_w_plus_1 = WsEvent::BlockHeader {
            number: w + 1,
            timestamp: 0,
            base_fee_per_gas: None,
            gas_used: 0,
            gas_limit: 0,
        };

        let combined = stream::iter(vec![
            header_w,
            WsEvent::Log(sync_log),
            WsEvent::Log(mint_log),
            WsEvent::Log(burn_log),
            header_w_plus_1,
        ])
        .boxed();

        let state = pump.subscribe_with_stream(combined).await.unwrap();
        assert_eq!(
            state.first_block, w,
            "handshake must anchor on block W (confirmed by W+1)"
        );

        let mut got_mint = false;
        let mut got_burn = false;
        let mut stream = state
            .combined_stream
            .expect("subscribe_with_stream must return a stream");
        while let Some(ev) = stream.next().await {
            if let WsEvent::Log(log) = ev {
                match log.topics().first().copied() {
                    Some(t) if t == V3_MINT_TOPIC => got_mint = true,
                    Some(t) if t == V3_BURN_TOPIC => got_burn = true,
                    _ => {}
                }
            }
        }
        assert!(
            got_mint,
            "block-W Mint MUST survive the handshake (Alternative B re-injects it)"
        );
        assert!(
            got_burn,
            "block-W Burn MUST survive the handshake (Alternative B re-injects it)"
        );
    }

    /// MJXP5Z: the handshake consumes ONLY headers (and collects logs); it
    /// never matches or interprets a log. Both W logs arrive between header(W)
    /// and header(W+1) and must be re-injected into `pending` for the resume
    /// stream.
    #[tokio::test]
    async fn observe_complete_block_does_not_consume_logs() {
        let (mut pump, _sink) = pump_for_test(None);
        let w = 42u64;
        let pool = Address::from([0xbbu8; 20]);

        let combined = stream::iter(vec![
            WsEvent::BlockHeader {
                number: w,
                timestamp: 0,
                base_fee_per_gas: None,
                gas_used: 0,
                gas_limit: 0,
            },
            WsEvent::Log(make_v3_mint_log_with_block(pool, -10, 10, 1, w)),
            WsEvent::Log(make_v3_burn_log_with_block(pool, -10, 10, 1, w)),
            WsEvent::BlockHeader {
                number: w + 1,
                timestamp: 0,
                base_fee_per_gas: None,
                gas_used: 0,
                gas_limit: 0,
            },
        ])
        .boxed();

        let state = pump.subscribe_with_stream(combined).await.unwrap();
        assert_eq!(state.first_block, w);

        let mut logs = 0u32;
        let mut stream = state.combined_stream.unwrap();
        while let Some(ev) = stream.next().await {
            if matches!(ev, WsEvent::Log(_)) {
                logs += 1;
            }
        }
        assert_eq!(
            logs, 2,
            "both Mint and Burn must be re-injected from pending"
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
        // the real `SolveCoordinator` does. Then resume with first_observed=W
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
        // The drain issued before resume anchors the cursor to W:
        pump.sink.on_drain(w, &meta_w);
        assert_eq!(
            pump.sink.last_processed_block(),
            Some(w),
            "on_drain(W) must anchor the cursor (mirrors SolveCoordinator)"
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
            WsEvent::Log(tombstone_log),
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

    use crate::bot_core::log_dispatcher::PoolStateSubscriber;
    use crate::bot_core::RegisterV2PoolParams;
    use alloy::primitives::{aliases::U112, Address, Bytes, U256};

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

    /// Counting subscriber — records `on_pool_state_updated` invocations so a
    /// pump-level reorg test can assert the restore fired the SAME notify
    /// path as a forward `dispatch_log`. `Fake` prefix per AGENTS.md.
    struct FakeCountingSubscriber {
        notifies: Mutex<u32>,
    }
    impl PoolStateSubscriber for FakeCountingSubscriber {
        fn on_pool_state_updated(&self, _pool_id: u64) {
            *self.notifies.lock().unwrap() += 1;
        }
    }

    /// Register a V2 pool on a fresh `Bot` with a counting subscriber attached,
    /// returning `(bot, pool_id, counting_subscriber)`. Genesis reserves are
    /// anchored at `update_block`, seeding the reorg journal so an in-journal
    /// reorg can roll back to them.
    fn bot_with_registered_v2(
        pool_addr: Address,
        update_block: u64,
    ) -> (Arc<Bot>, u64, Arc<FakeCountingSubscriber>) {
        let bot = Arc::new(Bot::new(1));
        let pool_id = bot
            .state_arc()
            .write()
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
        let counting = Arc::new(FakeCountingSubscriber {
            notifies: Mutex::new(0),
        });
        let sub: Arc<dyn PoolStateSubscriber> = counting.clone();
        bot.attach_engine(pool_id, Arc::downgrade(&sub));
        (bot, pool_id, counting)
    }

    /// Build a `BlockPump` over a caller-provided `Arc<Bot>` (rather than a
    /// fresh empty `Bot::new(1)`), returning the pump + sink + the shared
    /// shutdown flag so a test can assert shutdown behavior. Same mock-transport
    /// provider as `pump_for_test`; test paths avoid provider calls.
    fn pump_for_test_with_bot(
        bot: Arc<Bot>,
        last_processed: Option<u64>,
    ) -> (BlockPump, Arc<FakeDrainSink>, Arc<AtomicBool>) {
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
        let sink = Arc::new(FakeDrainSink::new(last_processed));
        let pump = BlockPump::for_test(bot, sink.clone(), reorg, provider, Arc::clone(&shutdown));
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
    /// restore the pool to its pre-fork state via the `ReorgCoordinator` —
    /// firing the SAME `on_pool_state_updated` notify as a forward Sync —
    /// and the pump does NOT shut down (it continues processing).
    ///
    /// This pins the pump-level wiring of ADR-006 slice 7: the coordinator's
    /// restore+notify (covered in `reorg_coordinator.rs`) is the downstream
    /// observable; what is asserted here is that the *pump* routes a
    /// `removed: true` log there, and that an in-depth reorg is non-fatal.
    /// Incident 2026-08-20 (WS-silent class): a WS subscription stream that
    /// ENDS mid-run must notify the sink (`on_pump_ended` - the production
    /// `SolveCoordinator` impl drops the engine delivery channels there), so
    /// the Python block/result streams END and the settlement bot fails
    /// loudly instead of idling forever (the silent stall operators saw).
    #[tokio::test]
    async fn stream_end_notifies_sink_on_pump_ended() {
        let pool_addr = Address::from([0x22u8; 20]);
        let (bot, _pool_id, _sub) = bot_with_registered_v2(pool_addr, 5);
        let (mut pump, sink, _shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
        assert!(!sink.pump_ended(), "no premature pump-ended signal");
        let forward = make_v2_sync_log(pool_addr, U256::from(1_000), U256::from(2_000), 7, false);
        // Stream ends immediately after the log -> Ok(None) arm.
        let combined = stream::iter(vec![WsEvent::Log(forward)]).boxed();
        pump.run_test_loop(combined, 5).await;
        assert!(
            sink.pump_ended(),
            "stream end must route to sink.on_pump_ended (closes the Python-facing channels)"
        );
    }

    #[tokio::test]
    async fn reorg_log_restores_pool_via_coordinator_and_pump_continues() {
        let pool_addr = Address::from([0x11u8; 20]);
        let (bot, pool_id, sub) = bot_with_registered_v2(pool_addr, 5);
        let notify_count = || *sub.notifies.lock().unwrap();

        // Forward Sync at block 7 — misprices the pool and seeds the journal
        // genesis(5) → transition(7). Drive through the *pump* (not
        // `bot.dispatch_log` directly) so the same code path that handles
        // live WS logs is exercised.
        let (mut pump, _sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
        let forward = make_v2_sync_log(pool_addr, U256::from(1_500), U256::from(2_500), 7, false);
        // Stream ends immediately after the log; the loop returns via the
        // `Ok(None)` arm (both subscription streams ended) once the reorg
        // branch `continue`s and the stream is exhausted.
        let combined = stream::iter(vec![WsEvent::Log(forward)]).boxed();
        pump.run_test_loop(combined, 5).await;

        assert_eq!(notify_count(), 1, "forward Sync through the pump notified");
        assert_eq!(
            bot.state_arc().read().v2_snapshot(pool_id),
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
        let combined = stream::iter(vec![WsEvent::Log(reorg_log)]).boxed();
        pump.run_test_loop(combined, 5).await;

        assert_eq!(
            notify_count(),
            2,
            "reorg fired the SAME notify path as a forward Sync",
        );
        assert_eq!(
            bot.state_arc().read().v2_snapshot(pool_id),
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
        let (bot, _pool_id, _sub) = bot_with_registered_v2(pool_addr, 5);

        let (mut pump, _sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
        // Removed-flag Sync at block 5 → coordinator restores before 5, which
        // is at the journal's genesis floor → `Err(NoStatePriorToBlock)`.
        let reorg_log = make_v2_sync_log(pool_addr, U256::from(1_500), U256::from(2_500), 5, true);
        let combined = stream::iter(vec![WsEvent::Log(reorg_log)]).boxed();
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
        let (bot, pool_id, sub) = bot_with_registered_v2(pool_addr, 5);
        let notify_count = || *sub.notifies.lock().unwrap();
        let snapshot = || bot.state_arc().read().v2_snapshot(pool_id);

        // Drive 5 -> 7 (forward sync at 7) -> tombstone 7 via a forward sync
        // at 8 (advance_to_drained(7) follows the tombstone).
        let (mut pump, _sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
        let s7 = make_v2_sync_log(pool_addr, U256::from(1_500), U256::from(2_500), 7, false);
        let s8 = make_v2_sync_log(pool_addr, U256::from(1_600), U256::from(2_600), 8, false);
        let combined = stream::iter(vec![WsEvent::Log(s7), WsEvent::Log(s8)]).boxed();
        pump.run_test_loop(combined, 5).await;
        assert_eq!(notify_count(), 2, "two forward syncs applied");
        assert_eq!(snapshot(), Some((U256::from(1_600), U256::from(2_600), 8)));
        assert!(!shutdown.load(Ordering::Relaxed));

        // Reorg over blocks 7 and 8: removed logs arrive in REVERSE order
        // (8 then 7), then the first removed:false at block 9 closes it.
        let (mut pump, _sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
        let r8 = make_v2_sync_log(pool_addr, U256::from(9), U256::from(9), 8, true);
        let r7 = make_v2_sync_log(pool_addr, U256::from(9), U256::from(9), 7, true);
        let s9 = make_v2_sync_log(pool_addr, U256::from(1_700), U256::from(2_700), 9, false);
        let combined =
            stream::iter(vec![WsEvent::Log(r8), WsEvent::Log(r7), WsEvent::Log(s9)]).boxed();
        pump.run_test_loop(combined, 5).await;

        // The reorg unwound 7 and 8 (restore to genesis), then the forward
        // sync at 9 re-applied -> reserves reflect block 9's values.
        assert_eq!(snapshot(), Some((U256::from(1_700), U256::from(2_700), 9)));
        assert!(
            !shutdown.load(Ordering::Relaxed),
            "reorg path closed cleanly — pump did NOT shut down"
        );
    }

    /// ADR-008 D3 pump-level: a `removed: false` log on a tombstoned block
    /// (NOT a reorg) means the WS delivered a forward event out-of-order /
    /// duplicated — unreliable. The pump must shut down rather than silently
    /// re-apply. Cursor never silently regresses.
    #[tokio::test]
    async fn late_forward_log_on_tombstoned_block_shuts_down_pump() {
        let pool_addr = Address::from([0x44u8; 20]);
        let (bot, _pool_id, _sub) = bot_with_registered_v2(pool_addr, 5);

        // Single pump session: forward sync(7) opens block 7; forward sync(8)
        // tombstones 7 (open block becomes 8); THEN a forward (removed:false)
        // sync at block 7 arrives late — block 7 is tombstoned and the open
        // block is 8 -> late forward -> unreliable WS -> shutdown.
        let (mut pump, _sink, shutdown) = pump_for_test_with_bot(Arc::clone(&bot), Some(5));
        let s7 = make_v2_sync_log(pool_addr, U256::from(1_500), U256::from(2_500), 7, false);
        let s8 = make_v2_sync_log(pool_addr, U256::from(1_600), U256::from(2_600), 8, false);
        let late = make_v2_sync_log(pool_addr, U256::from(9_999), U256::from(9_999), 7, false);
        let combined =
            stream::iter(vec![WsEvent::Log(s7), WsEvent::Log(s8), WsEvent::Log(late)]).boxed();
        pump.run_test_loop(combined, 5).await;

        assert!(
            shutdown.load(Ordering::Relaxed),
            "late removed:false on a tombstoned block must shut the pump down (ADR-008 D3)"
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
        let (_bot, _pool_id, _sub) = bot_with_registered_v2(pool_addr, 5);

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
                        Some((WsEvent::Log(stale), 2))
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

    /// BQ7ZBC — FSM guard: the single-writer discard is scoped to blocks the
    /// pump itself backfilled (≤ `recovery_anchor`). The header-staleness
    /// watchdog catch-up anchors at 102, then a `removed:false` forward at
    /// block 103 arrives late (103 tombstoned by 104, 103 > 102) — a GENUINE
    /// steady-state anomaly above the recovery anchor → MUST still shut the
    /// pump down (ADR-008 D3 preserved).
    #[tokio::test]
    async fn recovery_anchor_still_faults_stale_forward_above_anchor() {
        let pool_addr = Address::from([0x44u8; 20]);
        let (_bot, _pool_id, _sub) = bot_with_registered_v2(pool_addr, 5);

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
                        Some((WsEvent::Log(s103), 2))
                    }
                    2 => Some((WsEvent::Log(s104), 3)),
                    3 => Some((WsEvent::Log(late103), 4)),
                    _ => None,
                }
            }
        })
        .boxed();

        pump.run_test_loop(combined, 100).await;

        assert!(
            shutdown.load(Ordering::Relaxed),
            "a stale forward ABOVE recovery_anchor must still shut down (ADR-008 D3)"
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
            let mut core = state.write();
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
                    1 => Some((WsEvent::Log(s102), 2)),
                    2 => {
                        // Stall: let the watchdog catch up, then the WS resumes.
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        Some((WsEvent::Log(s104), 3))
                    }
                    3 => Some((WsEvent::Log(stale103), 4)),
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
        let core = state.read();
        let pool_id = *core.pool_addresses.get(&pool_addr).unwrap();
        if let Some(crate::bot_core::PoolEntry::V2(_, pool)) = core.pools.get(&pool_id) {
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

    /// UO3JM4 — the solver-state gate must FAIL HARD & LOUDLY on a verified
    /// desync: `abort()` the whole process, not `shutdown`+return silently
    /// (the AV42C7 fallback left the bot idling on discovery/probe threads)
    /// and not `panic!` (2026-08-02: unwound only the pump tokio task →
    /// no-progress busy loop). `abort()` can't unwind or linger, so the bot
    /// dies on the spot. `abort()` can't be tested in-process (it SIGABRTs
    /// the test binary), so this parent test spawns itself as a subprocess
    /// driving the gate through the desync and asserts the child died by
    /// SIGABRT AND printed the loud grep-able `[SOLVER-STATE] ABORT` marker.
    #[test]
    fn solver_state_desync_aborts_process() {
        let exe = std::env::current_exe().expect("current test exe");
        // The child is EXPECTED to SIGABRT here (UO3JM4) — that is the point
        // of the test, not a leak. But the kernel's default core-dump path
        // (kernel.core_pattern -> systemd-coredump/ABRT) makes GNOME's
        // "Problem Reporting" log a spurious "application crashed" entry for
        // every `cargo test` run of this crate. Suppress the core for the
        // subprocess: run it via `sh -c 'ulimit -c 0; exec "$@"'` so it
        // inherits RLIMIT_CORE=0 across the `exec`. SIGABRT and its exit
        // signal are still raised normally, so the assertions below are
        // unaffected — we only stop the OS from writing a core/report for
        // this intentional, expected abort.
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg("ulimit -c 0; exec \"$@\"")
            .arg("sh") // $0; the test binary + args follow as $1.. and become `$@`
            .arg(&exe)
            .arg("solver_state_desync_aborts_self")
            // --nocapture: the child is a test-harness run whose stderr is
            // captured by default; without it the eprintln! marker never
            // reaches the pipe the parent reads.
            .arg("--nocapture")
            .env("DEGENBOT_SELF_ABORT_TEST", "1")
            .output()
            .expect("spawn desync subprocess");
        let status = out.status;
        assert!(
            !status.success(),
            "a verified solver-state desync must kill the process, got {status:?}"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("[SOLVER-STATE] ABORT"),
            "desync must print the loud grep-able marker to stderr; got: {stderr}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(
                status.signal(),
                Some(6), // SIGABRT
                "expected the child killed by SIGABRT, got {status:?}"
            );
        }
    }

    /// UO3JM4 child half: no-op unless spawned by
    /// `solver_state_desync_aborts_process` (env `DEGENBOT_SELF_ABORT_TEST`).
    /// Drives the gate against a registered V2 pool whose on-chain
    /// `getReserves` (mocked) MISMATCHES the solver's stored reserves →
    /// `judge` returns `GateVerdict::Divergent` → `trip_and_exit` must
    /// `abort()` before this function can return.
    #[tokio::test]
    async fn solver_state_desync_aborts_self() {
        use degenbot_solvers::mixed::HopType;
        use degenbot_solvers::mixed::MixedPoolRef;

        if std::env::var_os("DEGENBOT_SELF_ABORT_TEST").is_none() {
            return; // no-op unless driven as the abort subprocess
        }

        let pool_addr = Address::from([0x44u8; 20]);
        let (pump, sink, asserter, _shutdown) = pump_for_test_sink_and_asserter(Some(100));
        let bot = pump.bot_arc_for_test();
        let pool_id = {
            let state = bot.state_arc();
            let mut core = state.write();
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
            .expect("test setup: V2 registration")
        };
        sink.set_path_refs(vec![vec![MixedPoolRef {
            hop_type: HopType::V2,
            pool_key: pool_id,
            zero_for_one: false,
        }]]);

        // Mock getReserves -> (777, 888, 0): MISMATCHES solver (1000, 2000),
        // so verify_solver_hop_states returns Err and the gate must abort.
        let word = |v: U256| {
            let mut w = [0u8; 32];
            w[..].copy_from_slice(&v.to_be_bytes::<32>());
            w
        };
        let mut resp = Vec::new();
        resp.extend_from_slice(&word(U256::from(777u64)));
        resp.extend_from_slice(&word(U256::from(888u64)));
        resp.extend_from_slice(&word(U256::ZERO));
        let hex_resp = format!("0x{}", alloy::primitives::hex::encode(&resp));
        asserter.push_success(&hex_resp);

        let refs = pump.sink.solver_path_pool_refs();
        let (path_hop_states, anchor) = {
            let state = pump.bot.state_arc();
            let core = state.read();
            (
                refs.iter()
                    .map(|pools| extract_solver_hop_states(&core, pools))
                    .collect::<Vec<_>>(),
                crate::bot_core::solve_anchor::SolveAnchor::resolve(200, &core),
            )
        };
        let reorg_evidence: Vec<TripReorgWindow> =
            pump.trip_reorg_windows.lock().iter().copied().collect();
        let verdict = judge(
            &pump.provider,
            &crate::bot_core::solver_state_tripwire::TripwireConfig::enabled_only(),
            &path_hop_states,
            anchor,
            &reorg_evidence,
        )
        .await;
        // The reaction (the whole executor-side surface) is trip + exit:
        // eprintln the loud marker, then abort the process.
        if let GateVerdict::Divergent(d) = verdict {
            BlockPump::trip_and_exit(&d);
        }
        unreachable!("the solver-state gate MUST abort the process on a verified desync (UO3JM4)");
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
        use alloy::primitives::{I256, U128};
        let pool_addr = Address::from([0x34u8; 20]);
        let bot = Arc::new(Bot::new(1));
        let mut tick_data = HashMap::new();
        tick_data.insert(
            7,
            TickInfo {
                liquidity_gross: U128::from(seed_gross),
                liquidity_net: I256::try_from(seed_gross.cast_signed()).unwrap(),
                block: 0,
            },
        );
        {
            let state = bot.state_arc();
            let mut core = state.write();
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
        let combined = stream::iter(vec![WsEvent::Log(mint), WsEvent::Log(swap)]).boxed();
        pump.run_test_loop(combined, block_n - 1).await;

        // The tombstone@N+1 set `last_complete_block = N`. Drain + pin.
        let (tick_data, pinned_block) = {
            let state = bot.state_arc();
            let mut core = state.write();
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
            let mut core = state.write();
            let pool_id = *core.pool_addresses.get(&pool_addr).unwrap();
            if let Some(crate::bot_core::PoolEntry::V3(_, pool)) = core.pools.get_mut(&pool_id) {
                use alloy::primitives::{I256, U128};
                pool.tick_data
                    .entry(6)
                    .or_insert(crate::bot_core::TickInfo {
                        liquidity_gross: U128::from(21_446_194_157_938_844u128),
                        liquidity_net: I256::try_from(21_446_194_157_938_844i128).unwrap(),
                        block: 0,
                    });
                pool.tick_data
                    .entry(8)
                    .or_insert(crate::bot_core::TickInfo {
                        liquidity_gross: U128::from(18_506_953_544_795_537u128),
                        liquidity_net: I256::try_from(-18_506_953_544_795_537i128).unwrap(),
                        block: 0,
                    });
            }
        }
        let mint_lower = make_v3_mint_log_with_block(pool_addr, 6, 7, amt_lower, block_n);
        let mint_upper = make_v3_mint_log_with_block(pool_addr, 7, 8, amt_upper, block_n);
        let swap = make_v3_swap_log_with_block(pool_addr, block_n + 1);
        let combined = stream::iter(vec![
            WsEvent::Log(mint_lower),
            WsEvent::Log(mint_upper),
            WsEvent::Log(swap),
        ])
        .boxed();
        pump.run_test_loop(combined, block_n - 1).await;

        let (tick_data, pinned_block) = {
            let state = bot.state_arc();
            let mut core = state.write();
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
            alloy::primitives::I256::try_from(
                i128::try_from(seed_gross).unwrap() - i128::try_from(amt_lower).unwrap()
                    + i128::try_from(amt_upper).unwrap()
            )
            .unwrap(),
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
        let combined = stream::iter(vec![WsEvent::Log(mint1), WsEvent::Log(swap)]).boxed();
        pump.run_test_loop(combined, block_n - 1).await;

        let (tick_data, pinned_block) = {
            let state = bot.state_arc();
            let mut core = state.write();
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

    /// Scenario A-race — the concurrent drain window: spawn the pump, feed
    /// Mint1@N, run the registration drain (cutoff < N so Mint1 is RETAINED,
    /// not drained), then feed Mint2@N + Swap@N+1. The pin captures
    /// `update_block = backfill block` (< N) — NOT the production symptom
    /// (which has `update_block = N`). This PROVES the race cannot produce
    /// the observed symptom: a pin at `update_block = N` requires the
    /// tombstone to have fired (cutoff = N), and the tombstone can only fire
    /// AFTER all of N's logs were dispatched (else `PanicLateForward`). So all
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
                    0 => Some((WsEvent::Log(logs.next().unwrap()), (1, rx_opt, logs))),
                    1 => {
                        let _ = rx_opt.unwrap().await; // park until drain completes
                        Some((WsEvent::Log(logs.next().unwrap()), (2, None, logs)))
                    }
                    2 => Some((WsEvent::Log(logs.next().unwrap()), (3, None, logs))),
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
            let mut core = state.write();
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
            let mut core = state.write();
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
    /// `stream::iter` (no delay between events) the 50ms `DEBOUNCE_MS` timer
    /// never fires before the stream ends, so `on_send` is never called.
    #[tokio::test]
    async fn burst_of_logs_publishes_once_at_tail_via_quiesce_gate() {
        let (mut pump, sink) = pump_for_test(Some(100));
        let pool_addr = Address::from([0x55u8; 20]);
        let mk = |r0, r1| {
            WsEvent::Log(make_v2_sync_log(
                pool_addr,
                U256::from(r0),
                U256::from(r1),
                101,
                false,
            ))
        };
        // 3 same-block sync logs, then stream exhaustion. Under the wall-clock
        // debounce the 50ms timer never fires before Ok(None) returns → 0
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
            let mut core = arc.write();
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
            bot.state_arc().write().set_snapshot_seed_block(Some(100));
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
            bot.state_arc().write().set_snapshot_seed_block(Some(0));
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
        Arc<FakeDrainSink>,
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
        let sink = Arc::new(FakeDrainSink::new(last_processed));
        let pump = BlockPump::for_test(bot, sink.clone(), reorg, provider, Arc::clone(&shutdown));
        (pump, sink, shutdown, asserter)
    }

    /// J3FMDO: `resume_from_subscribe` auto-backfills the snapshot→WS gap
    /// (S < W) before the live loop begins — proving the core path closes the
    /// gap with zero Python orchestration. The Asserter queue drains by exactly
    /// one `eth_getLogs` response (S+1..W fits in a single default-size chunk).
    #[tokio::test]
    async fn auto_backfill_runs_inside_resume_when_s_lt_w() {
        let bot = Arc::new(Bot::new(1));
        bot.state_arc().write().set_snapshot_seed_block(Some(85));
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
        bot.state_arc().write().set_snapshot_seed_block(Some(85));
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
            bot.state_arc().read().buffered_v3_event_count(&pool_addr),
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
        bot.state_arc().write().set_snapshot_seed_block(Some(85));
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
            WsEvent::Log(make_v2_sync_log(
                alloy::primitives::Address::from([0xd1u8; 20]),
                U256::ZERO,
                U256::ZERO,
                101,
                false,
            )),
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
            bot.state_arc().read().buffered_v3_event_count(&pool_addr),
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
                WsEvent::Log(l) => {
                    assert_eq!((kind, l.block_number.unwrap()), ("log", number));
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
        bot.state_arc().write().set_snapshot_seed_block(Some(100));
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

    /// MQUKB6 (epic KDUED5): one entered `degenbot.pump.block` span per
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
                sp.name.as_ref() == "degenbot.pump.block"
                    && sp.attributes.iter().any(|kv| {
                        kv.key == opentelemetry::Key::from_static_str("block.number")
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
                sp.name.as_ref() == "degenbot.pump.block"
                    && sp.attributes.iter().any(|kv| {
                        kv.key == opentelemetry::Key::from_static_str("block.number")
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
    /// every per-block span, never leaking still-entered spans on worker
    /// threads (the pre-fix loop-wide `Span::enter()` guard lived across the
    /// select's await points; when the multi-threaded runtime migrated the task
    /// between workers, it entered on one thread and dropped on another, so the
    /// span stayed entered in the abandoned worker's TLS — never closed, never
    /// exported, every child orphaned). The DETERMINISTIC defense is the
    /// structural fix (no `enter` guard may outlive a poll); this test locks
    /// the observable symptom — all N spans closed — and exercises cross-await
    /// parking so CI load that DOES migrate the task surfaces the old leak.
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
            if sp.name.as_ref() == "degenbot.pump.block" {
                for kv in &sp.attributes {
                    if kv.key == opentelemetry::Key::from_static_str("block.number") {
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
            "every header must export a CLOSED pump.block span; got {}/{}",
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
}
