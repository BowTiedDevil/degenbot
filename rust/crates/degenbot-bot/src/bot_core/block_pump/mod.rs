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

mod backfill;
mod lifecycle;
mod run_loop;
mod stages;

#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]
#[cfg(test)]
mod tests;
