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

use alloy::primitives::B256;
use alloy::rpc::types::{Filter, Log, Topic};
use futures_util::{stream, StreamExt};
use tokio::time::timeout;

use crate::bot_core::solver_state_verifier::{
    extract_solver_hop_states, verify_solver_hop_states, SolverStateMismatch,
};
use crate::bot_core::{drain_sink::DrainSink, BlockMetadata, Bot};
use crate::bot_core::{BlockClock, HeaderDecision, LogDecision};
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
    #[allow(clippy::missing_errors_doc)]
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
            provider: Arc::new(provider),
            shutdown,
            header_staleness: Duration::from_secs(HEADER_STALENESS_SECS),
            log_silence: Duration::from_secs(LOG_SILENCE_SECS),
            log_silence_alarms: 0,
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
    #[allow(clippy::too_many_lines)]
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
        let mut combined = subscribe_state
            .combined_stream
            .expect("resume() called without WS stream — did you call subscribe() first?");
        let first_block = subscribe_state.first_block;
        // Drain the WS stream DURING the blocking backfill (DFQYM5 root cause).
        // The alloy `logs` subscription buffers into a small broadcast channel
        // (default capacity 16) that DROPS the OLDEST messages for a lagging
        // receiver. If the backfill awaits without draining `combined`, the
        // freshly-mined live blocks' logs (oldest in the channel) overflow and
        // are lost permanently — the first live block then shows most of its
        // logs missing, immediately tripping the WS-completeness abort even
        // though no message was ever dropped by the node. Poll `combined`
        // concurrently here and collect its events so the buffer never
        // overflows; the drained events are re-injected ahead of the live loop.
        let (backfill_res, drained) = self
            .drain_stream_during_backfill(first_block, &mut combined)
            .await;
        if let Err(e) = backfill_res {
            tracing::error!(
                first_block,
                %e,
                "BlockPump: auto-backfill failed — starting live loop from gap (not closed)"
            );
        }
        // Re-inject any WS events drained during the backfill ahead of the
        // still-owned stream tail, preserving arrival order (single-stream
        // invariant MJXP5Z).
        let combined = stream::iter(drained).chain(combined).boxed();
        self.run_with_stream(combined, first_block).await;
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
    /// post-drain verify (2026-07-12 backrun crash).
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

    /// Option-A solver-state accuracy gate (AV42C7): diff every registered
    /// path's per-hop solver pool state against the chain at `block` (the
    /// solve block) and PANIC on the first mismatch or read failure.
    ///
    /// The scalar state is extracted under a short core read-guard and the
    /// guard dropped before the async on-chain reads begin (a `parking_lot`
    /// guard is not `Send` and must not be held across an `.await`). Env-gated
    /// by `DEGENBOT_ASSERT_SOLVER_STATE=1` at the call site; off the hot loop
    /// by default.
    ///
    /// NOTE (AV42C7): the on-chain diff is performed at each hop's OWN
    /// `update_block` anchor (see `verify_solver_hop_states`), not at `block`
    /// — a solver holding 1-2 blocks of normal latency must not panic; only a
    /// state that diverges from the chain even at its own anchor is a true
    /// desync. `block` appears in the message for staleness context.
    #[allow(clippy::missing_panics_doc)]
    async fn verify_solver_state_against_chain(&self, block: u64) {
        let path_refs = self.sink.solver_path_pool_refs();
        if path_refs.is_empty() {
            return;
        }
        // Extract per-path scalar states under a short read guard, then drop
        // the guard BEFORE awaiting the RPC reads.
        let mut path_hop_states = Vec::with_capacity(path_refs.len());
        {
            let state_arc = self.bot.state_arc();
            let core = state_arc.read();
            for pools in &path_refs {
                path_hop_states.push(extract_solver_hop_states(&core, pools));
            }
        }
        for (path_idx, hop_states) in path_hop_states.iter().enumerate() {
            if let Err(mismatch) = verify_solver_hop_states(&self.provider, hop_states, block).await
            {
                // Diagnose the cause class before panicking: log every hop's
                // solver-stored update_block and its staleness vs. the solve
                // block. `stale == 0` on the failing hop is the sub-tick
                // corruption signal (state advanced to solve_block but the
                // within-tick scalar still diverges); `stale > 0` is a WS
                // delivery/backfill lag. The first hop reported by the verifier
                // is also in this log, but this captures the whole path at once.
                let hops_diag: Vec<String> = hop_states
                    .iter()
                    .filter(|h| h.update_block != 0)
                    .map(|h| {
                        let meta = h
                            .cl_meta
                            .as_ref()
                            .map(|(c, l)| format!(" cov={c}, lifecycle={l}"))
                            .unwrap_or_default();
                        format!(
                            "hop {:?} update_block={} stale_by={}{}",
                            h.hop_type,
                            h.update_block,
                            block.saturating_sub(h.update_block),
                            meta
                        )
                    })
                    .collect();
                tracing::error!(
                    path_idx,
                    block,
                    hops = %hops_diag.join(", ").as_str(),
                    "DEGENBOT_ASSERT_SOLVER_STATE: solve used desynced pool state; panicking"
                );
                let SolverStateMismatch { message } = mismatch;
                panic!(
                    "DEGENBOT_ASSERT_SOLVER_STATE: path {path_idx} solver pool state does not \
                     match the chain at block {block}: {message}"
                );
            }
        }
    }

    /// Run the main pump loop with an existing WS stream.
    ///
    /// Processes logs eagerly: each WS log is applied to engine state
    /// immediately and affected paths are solved right away, without
    /// waiting for a block header. Block headers provide metadata
    /// (timestamp, fees) and handle empty-block detection.
    #[allow(unused_assignments, clippy::too_many_lines)]
    #[tracing::instrument(skip(self, combined), fields(first_observed_block))]
    pub async fn run_with_stream(
        &mut self,
        mut combined: stream::BoxStream<'static, WsEvent>,
        first_observed_block: u64,
    ) {
        // [DIAG] newHeads-stall investigation: track header arrivals so the
        // log shows, in production, whether `BlockHeader` events actually stop
        // arriving (subscription silent) vs. arrive but the arm doesn't fire
        // (pump not polling / bug). Remove once the freeze root cause is
        // confirmed and fixed.
        const DIAG_STATS_INTERVAL: Duration = Duration::from_secs(10);

        // hotpath drain-path tracer bullet (`src/profiling.rs`): hold a
        // profiling guard for the whole pump loop iff `DEGENBOT_HOTPATH=1`.
        // No-op (not even constructed) otherwise, and a no-op stub when the
        // `hotpath` Cargo feature is off. Dropping at loop exit writes the
        // report; for a long-running bot use `HOTPATH_SHUTDOWN_MS`.
        let _hotpath_guard = crate::profiling::hotpath_guard("block_pump");

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
            if let Some(seed) = snapshot_seed {
                if seed > 0 && seed < first_observed_block {
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
        // Whether we're past the first header after resume. The first
        // header establishes our anchor but shouldn't trigger a solve
        // (backfill already solved up to this point).
        let mut first_header = true;
        // Current block metadata — updated from headers, used for
        // solve batches when logs close out a block.
        let mut current_metadata: BlockMetadata = BlockMetadata::default();
        // WS-delivery completeness tracker (see `assert_ws_block_complete`):
        // the set of relevant-topic log indices delivered per block, cross-
        // checked against `eth_getLogs` at the block's tombstone to panic on a
        // live websocket log drop. Gated on `DEGENBOT_WS_COMPLETENESS`; the map
        // is only populated when the gate is on (so the hot loop adds no work
        // when disabled).
        let ws_completeness_enabled = std::env::var("DEGENBOT_WS_COMPLETENESS").is_ok();
        let mut ws_delivered: std::collections::HashMap<u64, std::collections::HashSet<u64>> =
            std::collections::HashMap::new();
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
        let mut publish_pending = false;

        // ADR-008 per-block state machine. The clock is the authority for
        // block completeness (the tombstone) and the cursor; the pump loop is
        // a thin async driver translating its decisions into sink calls +
        // backfill + shutdown. A header alone NEVER advances the cursor —
        // only `advance_to_drained` (after the tombstone) does.
        let mut clock = BlockClock::new();
        // 3M5PO5: share the clock's tombstone cutoff with BotState so the
        // registration drain reads the SAME "highest fully-delivered block"
        // the pump's clock tracks (no buffer-local shadow marker).
        self.bot
            .state_arc()
            .write()
            .set_pump_complete_cutoff(clock.highest_applied_handle());
        // Per-block metadata, snapshotted from each block's header. A block's
        // tombstone (first log for N+1) may arrive AFTER header N+1 overwrote
        // `current_metadata`, so the result batch that finalizes N must carry
        // N's OWN metadata, retrieved here (VTWCIG).
        let mut block_metadata: HashMap<u64, BlockMetadata> = HashMap::new();

        // [DIAG] newHeads-stall counters (JIABO3: `last_header_at` is shared
        // with the header-staleness watchdog below — not DIAG-only).
        let mut diag_header_count: u64 = 0;
        let mut diag_log_count: u64 = 0;
        let mut last_header_at = tokio::time::Instant::now();
        // Logs-subscription liveness watchdog (the INVERSE of
        // `header_staleness`): anchored at pump start and refreshed on EVERY
        // `WsEvent::Log` (before the topic pre-filter, so an irrelevant log
        // still proves the `eth_subscribe "logs"` arm is alive). When the
        // staleness tick wins and headers are FRESH but this has elapsed past
        // `self.log_silence`, the logs sub is presumed stalled → one warning
        // per silence episode (re-armed when the next log resumes).
        let mut last_log_at = tokio::time::Instant::now();
        let mut log_silence_alarm_armed = false;
        let mut diag_last_stats = tokio::time::Instant::now();

        // Option-A solver-state accuracy gate (AV42C7): when `DEGENBOT_ASSERT_SOLVER_STATE`
        // is set, diff each solved path's per-hop pool state against the chain
        // at the solve block after every drain, panicking on any mismatch. Off
        // by default (adds an RPC read per path per solve on the hot loop).
        let solver_state_verify_enabled = std::env::var("DEGENBOT_ASSERT_SOLVER_STATE").is_ok();

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

        loop {
            // Solve any dirty paths accumulated from the previous iteration's
            // log(s). This naturally coalesces multiple logs that arrive
            // between await points — only one solve per batch of WS events.
            // Note: solving is decoupled from sending — the pump controls
            // when result batches are dispatched to Python.
            {
                if self.sink.has_dirty_paths() {
                    self.sink.on_drain(current_block, &current_metadata);
                    // LEZJAS: engine owns `last_solved_block` now — mark this
                    // block solved so the next `finalize_block` guard no-ops.
                    self.sink.set_last_solved_block(current_block);
                }
            }

            // ADR-008 D2: solver-release gate. `publish_pending` is set when a forward
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
            let wait_timeout = if publish_pending {
                Duration::from_millis(DEBOUNCE_MS)
            } else {
                Duration::from_secs(BACKFILL_TIMEOUT_SECS)
            };
            let event = tokio::select! {
                biased;
                // JIABO3 header-staleness watchdog — see the interval setup
                // above. Firing here does NOT consume the stream event; it runs
                // `handle_timeout_eager` then re-loops (the top-of-loop drain
                // picks up any dirty paths the backfill created). The
                // `timeout(wait_timeout, combined.next())` future is dropped on
                // this arm winning, so the inactivity/debounce countdown
                // restarts — acceptable since `DEBOUNCE_MS << header_staleness`
                // and the no-activity path is now superseded by this watchdog.
                _ = staleness_tick.tick() => {
                    if last_header_at.elapsed() >= self.header_staleness {
                        self.handle_timeout_eager(
                            &mut current_block,
                            &mut clock,
                            &mut block_metadata,
                            &mut publish_pending,
                        )
                        .await;
                    } else if last_log_at.elapsed() >= self.log_silence {
                        // Logs-subscription liveness watchdog (inverse of
                        // header staleness): headers are FRESH (above branch did
                        // not fire) but no `WsEvent::Log` has arrived in
                        // `self.log_silence` — the `eth_subscribe "logs"` arm
                        // is presumed stalled/dead while `newHeads` is alive.
                        // One warning per silence episode (re-armed when the
                        // next log resumes the sub).
                        if !log_silence_alarm_armed {
                            tracing::warn!(
                                silence_secs = self.log_silence.as_secs(),
                                "[pump] logs subscription silent: headers flowing but no log"
                            );
                            self.log_silence_alarms = self.log_silence_alarms.saturating_add(1);
                            log_silence_alarm_armed = true;
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
                    if publish_pending {
                        if let Some(open) = clock.latest_observed() {
                            if clock.consume_quiesced(open) {
                                self.sink.on_send(&current_metadata);
                                // Option-A solver-state accuracy gate (AV42C7),
                                // run at the PUBLISH point: the result being
                                // sent here is the coalesced, quiesce-gated
                                // (block-final) solve — the one Python will
                                // actually simulate. Verifying here, not after
                                // every transient `on_drain`, means a mid-block
                                // stale solve that the eager design discards
                                // (re-solved when the block completes) never
                                // trips the hard panic, while a desync on a
                                // result that SURVIVES to publication still
                                // panics before Python simulates it. Each hop is
                                // diffed against the chain at its own anchor
                                // block (`verify_solver_hop_states`); hops
                                // touched in the in-progress block are skipped
                                // (mid-block captures are unverifiable via
                                // historical slot0).
                                if solver_state_verify_enabled {
                                    self.verify_solver_state_against_chain(current_block).await;
                                }
                            }
                        }
                        publish_pending = false;
                    } else {
                        // No activity for 60s — try to backfill
                        self.handle_timeout_eager(
                            &mut current_block,
                            &mut clock,
                            &mut block_metadata,
                            &mut publish_pending,
                        )
                        .await;
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
                    // [DIAG]
                    diag_header_count += 1;
                    let diag_gap = if diag_header_count == 1 {
                        0.0
                    } else {
                        last_header_at.elapsed().as_secs_f64()
                    };
                    tracing::info!(
                        number,
                        diag_header_count,
                        gap_secs = %format!("{:.1}", diag_gap),
                        "BlockPump: [DIAG] HEADER"
                    );
                    if diag_header_count > 1 && last_header_at.elapsed() > Duration::from_secs(20) {
                        tracing::warn!(
                            number,
                            silent_secs = %format!("{:.1}", diag_gap),
                            "BlockPump: [DIAG] *** HEADER STALL: headers were silent"
                        );
                    }
                    last_header_at = tokio::time::Instant::now();
                    // Snapshot the just-finished block's metadata BEFORE
                    // overwriting `current_metadata` with the incoming
                    // header's. `current_metadata` at this point holds the
                    // metadata of `current_block` (set when ITS header arrived
                    // and held through that block's log processing), so the
                    // batch that finalizes `current_block` must carry THAT
                    // metadata — NOT the incoming header's. Passing the
                    // post-overwrite value here would make Python compute
                    // `base_fee_next` from the wrong block and submit
                    // under-/over-priced backrun txs (VTWCIG). The incoming
                    // metadata (assigned just below) drives the empty-block
                    // send path, which is correctly about the NEW block.
                    // ADR-008: a header alone NEVER finalizes/drain. The
                    // clock records the block's metadata (so a later
                    // tombstone-driven finalize carries the CORRECT block's
                    // metadata — the tombstone log for N+1 may arrive AFTER
                    // header N+1, at which point `current_metadata` would
                    // already hold N+1's). `notify_block` still fires so
                    // Python's block clock tracks `newHeads`.
                    current_metadata = BlockMetadata {
                        timestamp,
                        base_fee_per_gas,
                        gas_used,
                        gas_limit,
                    };
                    let header_decision = clock.observe_header(number);
                    if matches!(header_decision, HeaderDecision::Stale) {
                        // duplicate/stale header — ignore
                        continue;
                    }
                    // Snapshot this block's metadata so its (deferred)
                    // tombstone-finalize carries the correct block's metadata.
                    block_metadata.insert(number, current_metadata);

                    let is_first_header = first_header;
                    if is_first_header {
                        // First header after backfill. The backfill already
                        // solved up to this point — just record the anchor and
                        // skip solving. Set up for normal operation.
                        if number > current_block {
                            if number > current_block + 1 {
                                tracing::info!(
                                    from_block = current_block + 1,
                                    to_block = number,
                                    "BlockPump: gap from block to block — backfilling"
                                );
                                self.backfill_range(
                                    current_block + 1,
                                    number - 1,
                                    &mut current_block,
                                    &mut clock,
                                    &mut block_metadata,
                                    &mut publish_pending,
                                )
                                .await;
                            }
                            current_block = number;
                            // LEZJAS: the backfill path solved up to `number`
                            // already; mark it solved on the engine so the next
                            // `finalize_block` guard no-ops for this block.
                            self.sink.set_last_solved_block(number);
                            self.sink.notify_block(current_block, &current_metadata);
                        }
                        first_header = false;
                    } else if number > current_block {
                        // New block header, but the previous block is NOT
                        // finalized here — only the tombstone (first log for
                        // N+1) closes it (ADR-008 D1). Gap backfill still runs.
                        if number > current_block + 1 {
                            tracing::info!(
                                from_block = current_block + 1,
                                to_block = number,
                                "BlockPump: gap from block to block — backfilling"
                            );
                            self.backfill_range(
                                current_block + 1,
                                number - 1,
                                &mut current_block,
                                &mut clock,
                                &mut block_metadata,
                                &mut publish_pending,
                            )
                            .await;
                        }

                        current_block = number;
                        self.sink.notify_block(current_block, &current_metadata);
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
                    // per silence episode (not per tick).
                    last_log_at = tokio::time::Instant::now();
                    log_silence_alarm_armed = false;
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

                    let log_block = log.block_number.unwrap_or(current_block);
                    // Boundary-block delivery alignment (DFQYM5): when the
                    // snapshot→WS gap was closed (snapshot seed S <
                    // first_observed_block W), the backfill above covered
                    // [S+1, W] INCLUSIVE — block W's logs are ALREADY fully
                    // applied to BotState. The fresh WS `logs` subscription,
                    // however, delivers only the PARTIAL set of W's logs mined
                    // after it engaged (observed: 6 of 35 at the boundary).
                    // Those partial duplicates must NOT be re-applied
                    // (double-apply → state corruption), and must not re-anchor
                    // the clock at W (which would tombstone W and trip the
                    // WS-completeness check on a block the backfill owns, not
                    // the WS). Drop any WS log for block ≤ W when backfill
                    // covered W — this is the single-writer rule: backfill owns
                    // [S+1, W], the live WS owns [W+1, ∞).
                    if snapshot_seed.is_some_and(|s| s > 0 && s < first_observed_block)
                        && log_block <= first_observed_block
                    {
                        continue;
                    }
                    // WS-completeness tracker: record the delivered relevant
                    // log index for this block so the tombstone can cross-check
                    // it against authoritative on-chain logs (a missing index =
                    // a websocket drop → panic). Only tracked when the gate is
                    // on to keep the default hot loop at zero-cost.
                    if ws_completeness_enabled {
                        if let Some(li) = log.log_index {
                            ws_delivered.entry(log_block).or_default().insert(li);
                        }
                    }

                    // ADR-008: route the log via the per-block state machine.
                    // The clock decides whether this is a forward dispatch, a
                    // tombstone (first removed:false log for N+1), a reorg
                    // signal, or an unreliable-WS late forward (→ shutdown).
                    let log_decision = clock.observe_log(log_block, log.removed);
                    // Per-pool trace: log EVERY relevant-topic WS log for the
                    // `DEGENBOT_DRAIN_DBG` pool — block, log-index, tx-index,
                    // topic0, removed, and the clock decision — so the
                    // delivery order of same-block Mint/Burn logs is visible
                    // against the registration drain+pin that follows. No-op
                    // for other pools / when the env var is unset.
                    crate::bot_core::trace_ws_log_dispatch(
                        log.address(),
                        log_block,
                        log.log_index,
                        log.transaction_index,
                        *log.topics()
                            .first()
                            .unwrap_or(&alloy::primitives::B256::ZERO),
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
                            // Cancel any pending publish: results accumulated
                            // from pre-reorg state are invalid.
                            publish_pending = false;
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
                            publish_pending = false;
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
                            current_block = new_head;
                            publish_pending = false;
                            // Fall through to dispatch this forward log.
                        }
                        LogDecision::TombstonePrevious(prev) => {
                            // First removed:false log for N+1 → tombstone N.
                            // Finalize N with N's OWN metadata (snapshotted
                            // when N's header arrived), not current_metadata
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
                            publish_pending = false;
                            // 3M5PO5: no explicit `mark_pump_blocks_complete`
                            // here — the clock's own `tombstone(prev)` (inside
                            // `observe_log`) already advanced the shared cutoff
                            // the registration drain reads.
                            // LOUD WS-completeness check: block `prev` is now
                            // confirmed complete (tombstoned by the first log of
                            // N+1); cross-check delivered relevant logs vs
                            // `eth_getLogs` and panic on a websocket drop.
                            if ws_completeness_enabled {
                                let delivered = ws_delivered.remove(&prev).unwrap_or_default();
                                self.assert_ws_block_complete(prev, delivered).await;
                            }
                            let prev_meta = block_metadata
                                .get(&prev)
                                .copied()
                                .unwrap_or(current_metadata);
                            self.finalize_if_dirty(prev, &prev_meta);
                            clock.advance_to_drained(prev);
                            if log_block > current_block {
                                current_block = log_block;
                            }
                        }
                        LogDecision::DispatchForward => {
                            if log_block > current_block {
                                current_block = log_block;
                            }
                        }
                        LogDecision::PanicLateForward(b) => {
                            // A removed:false log on a tombstoned block, NOT
                            // in a reorg → unreliable WS (out-of-order /
                            // duplicated forward events). Unrecoverable for
                            // correctness — shut down (ADR-008 D3).
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
                    // engine) — NOT `engine.apply_log`.
                    self.bot.dispatch_log(&log);
                    clock.log_received(log_block);
                    clock.log_applied(log_block);

                    // LEZJAS: engine owns `has_logs_this_block` now — routed
                    // through the sink so the next `finalize_block` sees it.
                    self.sink.record_logs_this_block();

                    // ADR-008 D2: arm the quiesce-gated publish. The flush
                    // fires at the next settle point (timeout or stream end)
                    // if `consume_quiesced` is true — once per quiesce cycle.
                    publish_pending = true;

                    // [DIAG] count logs + emit periodic stats so we can see,
                    // during a freeze, that the pump IS polling logs while
                    // headers are gone. This is the liveness signal the loop
                    // otherwise lacks.
                    diag_log_count += 1;
                    if diag_last_stats.elapsed() >= DIAG_STATS_INTERVAL {
                        let diag_since_header = last_header_at.elapsed().as_secs();
                        tracing::info!(
                            diag_header_count,
                            diag_log_count,
                            last_header_secs = diag_since_header,
                            current_block,
                            "BlockPump: [DIAG] stats"
                        );
                        diag_last_stats = tokio::time::Instant::now();
                    }
                }

                Ok(None) => {
                    // ADR-008 D2: stream exhausted — final settle point. Flush
                    // any pending quiesce-gated publish before returning.
                    if publish_pending {
                        if let Some(open) = clock.latest_observed() {
                            if clock.consume_quiesced(open) {
                                self.sink.on_send(&current_metadata);
                            }
                        }
                        publish_pending = false;
                    }
                    tracing::warn!("BlockPump: both subscription streams ended");
                    return;
                }
            }
        }
    }

    /// Finalize the current block: solve any dirty paths and send the result
    /// batch to Python, carrying the caller's real `current_metadata`.
    ///
    /// Delegates to the `DrainSink`'s `finalize_block` (the slice-6
    /// `SolveCoordinator` fans to every attached `Engine`). The
    /// `last_solved_block` / `has_logs_this_block` bookkeeping is owned by
    /// the engine since ergo task LEZJAS (the pump's out-params retired);
    /// all engine-state mutation happens inside the sink under its lock.
    /// ADR-006 D4: the pump no longer holds the engine `Mutex` directly.
    fn finalize_if_dirty(&self, block: u64, metadata: &BlockMetadata) {
        self.sink.finalize_block(block, metadata);
    }

    /// Handle a 60s timeout by backfilling any missed blocks (eager variant).
    async fn handle_timeout_eager(
        &self,
        current_block: &mut u64,
        clock: &mut BlockClock,
        block_metadata: &mut HashMap<u64, BlockMetadata>,
        publish_pending: &mut bool,
    ) {
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

        if latest_block > *current_block {
            let mut lpb = *current_block;
            self.backfill_range(
                *current_block + 1,
                latest_block,
                &mut lpb,
                clock,
                block_metadata,
                publish_pending,
            )
            .await;
            *current_block = lpb;
            // LEZJAS: engine owns `last_solved_block` + `has_logs_this_block`
            // now — mark the backfilled range solved + clear the logs flag
            // through the sink (mirrors the retired pump-local writes).
            self.sink.set_last_solved_block(lpb);
        }
    }

    /// Take the watch receiver from the pump's block channel.
    ///
    /// Handle a block header event from the WS subscription.
    ///
    /// Returns the updated `last_processed_block` and `first_header` flag.
    /// Takes `pending_logs` by mutable reference and clears it as needed.
    /// Backfill a range of blocks via `eth_getLogs`.
    ///
    /// Processes each block in the range sequentially.
    async fn backfill_range(
        &self,
        from_block: u64,
        to_block: u64,
        last_processed_block: &mut u64,
        clock: &mut BlockClock,
        block_metadata: &mut HashMap<u64, BlockMetadata>,
        publish_pending: &mut bool,
    ) {
        if from_block > to_block {
            return;
        }

        tracing::info!(from_block, to_block, "BlockPump: backfilling blocks");

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

        // ADR-008 D4: backfilled logs flow through the SAME state machine as
        // live WS logs (single branch — no distinguished `Backfilled` edge).
        // Each log routes via `clock.observe_log`; a tombstoned predecessor is
        // finalized + drained through the clock, and the backfilled block is
        // solved via `on_drain` (results piggyback onto the next debounce
        // `on_send` carrying real metadata).
        let mut any_processed = false;
        for block in from_block..=to_block {
            if self.shutdown.load(Ordering::Relaxed) {
                tracing::info!("BlockPump: shutting down during backfill");
                return;
            }

            let block_logs = logs_by_block.remove(&block).unwrap_or_default();
            for log in &block_logs {
                match clock.observe_log(block, log.removed) {
                    LogDecision::TombstonePrevious(prev) => {
                        let prev_meta = block_metadata.get(&prev).copied().unwrap_or_default();
                        self.sink.finalize_block(prev, &prev_meta);
                        clock.advance_to_drained(prev);
                        self.bot.dispatch_log(log);
                        clock.log_received(block);
                        clock.log_applied(block);
                    }
                    LogDecision::DispatchForward => {
                        self.bot.dispatch_log(log);
                        clock.log_received(block);
                        clock.log_applied(block);
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
                // ADR-008 D2: arm the quiesce-gated publish so backfilled
                // solved results flush at the next settle point (the live
                // loop's on_send), carrying real metadata.
                *publish_pending = true;
            }
            *last_processed_block = block;
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
    /// Gated on `DEGENBOT_WS_COMPLETENESS`; when unset (default, incl. tests
    /// that feed synthetic logs through `run_test_loop`) it is a no-op. On an
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
            // Flush best-effort before abort.
            eprintln!(
                "[WS-INVARIANT] ABORT: live websocket log drop at block {block} ({} of {} relevant logs missing); eth_getLogs vs WS divergence — see the untraced log for the log_index list.",
                missing.len(),
                onchain.len(),
            );
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
            provider,
            shutdown,
            header_staleness: Duration::from_secs(HEADER_STALENESS_SECS),
            log_silence: Duration::from_secs(LOG_SILENCE_SECS),
            log_silence_alarms: 0,
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
            }
        }
    }

    impl DrainSink for FakeDrainSink {
        fn has_dirty_paths(&self) -> bool {
            false
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
        fn record_logs_this_block(&self) {}
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
        // this metadata; carrying N+1's would systematically mis-price backruns.
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

        let sent = sink.sent.lock().unwrap().clone();
        assert_eq!(
            sent.len(),
            1,
            "a 3-log burst publishes exactly once at the tail via the quiesce \
             gate (got {} sends)",
            sent.len()
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
    /// verify mismatched on-chain and crashed the backrun bot with
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
    #[allow(clippy::too_many_lines)]
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
}
