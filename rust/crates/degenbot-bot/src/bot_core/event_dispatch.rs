//! `DispatchOwner` — the block pump's single dispatch seam (epic B).
//!
//! One module owns the pump's hand-offs to the sink, the solver-state verifier,
//! and Python's block clock — the **dispatch owner** — but delivers them over
//! application-specific pipes, each with the delivery semantics its task needs
//! (see CONTEXT.md "Block-pump dispatch seam"). "One seam" means one coordinated
//! home, NEVER one queue forced to fit every task.
//!
//! ## Pipes
//!
//! - **Drain pipe (B1):** an ordered FIFO (`mpsc`) taking `Drain`/`Finalize`/
//!   `Publish` to a background drainer task → sink. FIFO + the engine/sink
//!   locks are what make the deferred path equal to the old inline one.
//! - **Block-clock pipe (B2):** a DIRECT `notify_block` dispatch to the sink's
//!   engine notification channels — deliberately NOT a `DrainWork` item, so it
//!   never rides the drain FIFO and is never queued behind solver work. Every
//!   accepted header is delivered 1:1 (no coalescing). The sink's `notify_block`
//!   no longer takes the `drain_lock` (the `engines` vec is frozen after start),
//!   so the clock does not contend with the drain fan-out.
//! - **Verifier pipe (B1):** a latest-wins `watch` to the solver-state verifier
//!   task (ADR-021). Only the most recent published block is ever verified;
//!   non-blocking so a slow verify can never stall the pump. The `watch`
//!   transmitter lives here; the verifier task construction stays in the pump
//!   (it needs `solver_state_verify_loop`, which is `impl BlockPump`).
//!
//! ## Delivery (sole mode since B4)
//!
//! All drain work is deferred to the background drainer task (the inline path
//! is retired — the WS poller never parks behind GIL-bound Python or a heavy
//! Möbius solve). FIFO order + the engine/sink locks give the deferred work the
//! same semantics the pre-B4GX7C inline path had.
//!
//! The `Publish` change-set is consumed atomically by the caller (single-writer)
//! and the verifier anchor is carried in the message.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use crate::bot_core::drain_sink::DrainSink;
use crate::bot_core::BlockMetadata;

/// The payload handed from the pump to the solver-state verifier task at each
/// publish point: the solve block and the pool refs for ONLY the paths re-solved
/// that block (the ADR-021 change set). Captured atomically at the publish so
/// the verifier diffs exactly what this block re-solved against the chain — never
/// the whole registered set (the root of the confirmed pump freeze).
pub type SolverVerifyRequest = (u64, Vec<Vec<degenbot_solvers::mixed::MixedPoolRef>>);

/// A deferred drain-sink operation to the background drainer task: the sink's
/// solve/dispatch/finalize run via these messages so the WS poller is never
/// parked behind GIL-bound or heavy sink work (`Python::attach`, Möbius solve).
/// FIFO ordering + the existing engine/sink locks give the deferred path the
/// inline semantics it replaced (B4). The verifier anchor (`open`) and the
/// change-set for `Publish` are consumed atomically in the pump (single-writer).
pub enum DrainWork {
    /// Eager solve of every dirty path at `block`.
    Drain { block: u64, metadata: BlockMetadata },
    /// Solve + emit the block-boundary batch (tombstone path).
    Finalize { block: u64, metadata: BlockMetadata },
    /// The quiesce-gated publish: flush the sink's `on_send` to Python, then
    /// hand the log-driven quiesced `block` + change-set to the latest-wins
    /// verifier.
    Publish {
        open: u64,
        metadata: BlockMetadata,
        change_set: Vec<Vec<degenbot_solvers::mixed::MixedPoolRef>>,
    },
}

/// Shared progress counters giving the WS poller feedback on the background
/// drainer (B4GX7C): `enqueued` is bumped by the pump on every successful send,
/// `processed` by the drainer after each completed sink operation. Used to
/// detect a dead or stalled drainer and fail loudly instead of silently losing
/// every solve/dispatch/publish while the pump's own loop keeps advancing (the
/// exact half-alive failure the codebase otherwise aborts on). Replaced by the
/// no-progress strike detector in B3.
pub struct DrainerHealth {
    enqueued: AtomicU64,
    /// Work items the drainer has *picked up* (received from the FIFO, counting
    /// the one it is currently processing). Incremented by the drainer right
    /// after `recv()` — so `depth() = enqueued - picked_up` is the number of
    /// items still queued (the B3 no-progress detector's depth signal).
    picked_up: AtomicU64,
    processed: AtomicU64,
}

impl DrainerHealth {
    fn new() -> Self {
        Self {
            enqueued: AtomicU64::new(0),
            picked_up: AtomicU64::new(0),
            processed: AtomicU64::new(0),
        }
    }

    /// Work items the pump enqueued (bumped on a successful send).
    pub fn enqueued(&self) -> u64 {
        self.enqueued.load(Ordering::Relaxed)
    }

    /// Work items the drainer has picked up but not (or not yet) completed.
    pub fn picked_up(&self) -> u64 {
        self.picked_up.load(Ordering::Relaxed)
    }

    /// Work items the drainer completed (bumped after each sink call returns).
    pub fn processed(&self) -> u64 {
        self.processed.load(Ordering::Relaxed)
    }

    /// Current drain-pipe depth: how many items are queued but not yet picked
    /// up by the drainer. The B3 no-progress detector's depth signal (approximate
    /// under a concurrent drainer, which the K-consecutive design tolerates).
    pub fn depth(&self) -> u64 {
        self.enqueued().saturating_sub(self.picked_up())
    }
}

/// Watchdog tick for the drainer-liveness backstop (U4UOIS): short enough to
/// bound abort latency to ~one window + one tick, rare enough to be free.
#[cfg(feature = "otel")]
const STALL_WATCHDOG_TICK_MS: u64 = 5_000;

/// How many **consecutive** no-progress pushes (the drainer picks nothing up)
/// How long a backlogged drainer may go without completing any work before the
/// pump aborts. The soak proved that pure event-counting (depth- or
/// completion-based strikes) cannot distinguish a *frozen* drainer from one
/// mid-way through a single exceptionally long solve — only wall-clock time
/// can. So a clock backstop is used: when the drain pipe holds a backlog AND
/// the drainer has completed nothing for this window, fail loud. Matches the
/// validated B4GX7C window (~30s). A health-policy knob, not a load tune.
const STALL_WINDOW_MS: u64 = 30_000;

/// The drain-pipe depth at/above which a backlog is treated as a stall (a depth
/// of 1 is a single item in flight — not a backlog). A small constant floor.
const BACKLOG_FLOOR: u64 = 2;

/// Milliseconds → seconds for the latency histograms. Clamped at ~49 days:
/// a larger gap means the clock jumped, and the histogram bucket is garbage
/// either way — the clamp keeps the precision lint honest without pretending
/// the number is meaningful.
#[must_use]
pub fn ms_to_secs(ms: u64) -> f64 {
    f64::from(u32::try_from(ms).unwrap_or(u32::MAX)) / 1_000.0
}

/// Pure predicate for the B3 stall backstop (unit-testable): the drainer is
/// stalled if the queue holds a backlog (`depth >= floor`) AND the last healthy
/// moment was at least `window_ms` ago.
#[must_use]
fn stalled(depth: u64, ms_since_healthy: u64, floor: u64, window_ms: u64) -> bool {
    depth >= floor && ms_since_healthy >= window_ms
}

/// Pure decision (U4UOIS, unit-testable without clocks or globals): should a
/// sample refresh the healthy baseline? Refresh when the drainer completed
/// something since the last sample OR the queue went idle. Splitting the
/// decision from the mutation is what lets the verdict be tested
/// deterministically — the old inline version mixed clock reads into the
/// branch and could only be tested end-to-end via process-spawning freeze
/// tests.
#[must_use]
fn stall_refresh(depth: u64, processed: u64, last_processed: u64) -> bool {
    processed != last_processed || depth == 0
}

/// Shared stall state sampled BOTH by the pump at dispatch time and by the
/// always-on watchdog task. Single writer per field: the pump writes the two
/// baselines; the watchdog only reads them plus the completion counter.
struct StallWatch {
    /// Drainer completion count when the pump last saw progress-or-idle.
    last_processed: AtomicU64,
    /// Wall-clock (ms) of the last progress-or-idle observation.
    last_healthy_ms: AtomicU64,
    /// Production window; swapped to a small value in freeze tests.
    stall_window_ms: AtomicU64,
}

impl StallWatch {
    fn new() -> Self {
        Self {
            last_processed: AtomicU64::new(0),
            last_healthy_ms: AtomicU64::new(now_millis()),
            stall_window_ms: AtomicU64::new(STALL_WINDOW_MS),
        }
    }

    /// Sample-and-abort, shared by the pump's dispatch-time check and the
    /// watchdog task (identical verdict either way — first sampler wins).
    fn check_and_abort(&self) {
        // Uninitialized gauges (no owner built yet) read as "idle" — the
        // watchdog only fires once a pump is actually dispatching.
        let depth = DEPTH.get().map_or(0, |d| d.load(Ordering::Relaxed));
        let processed = PROCESSED.get().map_or(0, |p| p.load(Ordering::Relaxed));
        let last_processed = self.last_processed.load(Ordering::Relaxed);
        if stall_refresh(depth, processed, last_processed) {
            self.last_processed.store(processed, Ordering::Relaxed);
            self.last_healthy_ms.store(now_millis(), Ordering::Relaxed);
            return;
        }
        let since_healthy =
            now_millis().saturating_sub(self.last_healthy_ms.load(Ordering::Relaxed));
        let window = self.stall_window_ms.load(Ordering::Relaxed);
        if !stalled(depth, since_healthy, BACKLOG_FLOOR, window) {
            return;
        }
        tracing::error!(
            since_healthy_ms = since_healthy,
            depth,
            "[B3] drainer stalled: backlog with no completion for {window} ms — ABORT"
        );
        #[expect(clippy::print_stderr)] // fatal diagnostic before abort
        {
            eprintln!(
                "[B3] ABORT: background drainer made no progress for {window} ms \
                 with {depth} queued — solve/dispatch/publish is not advancing. \
                 Fail loud, never half-alive."
            );
        }
        std::process::abort();
    }
}

/// Global depth gauge (drain-pipe backlog), written by `dispatch` and read by
/// the watchdog task. `OnceLock` of one cell: the owner is unique per process.
static DEPTH: OnceLock<AtomicU64> = OnceLock::new();
/// S53STH: cooperative-shutdown token for the `StallWatch` task, installed by
/// [`DispatchOwner::with_shutdown_token`]. Unset = never cancelled. Gated on
/// `otel` (the only consumer is the hotpath timed exit) so default builds
/// carry no tokio-util code at all.
#[cfg(feature = "otel")]
static SHUTDOWN_TOKEN: OnceLock<tokio_util::sync::CancellationToken> = OnceLock::new();

/// Global drainer completion counter mirror for the watchdog.
static PROCESSED: OnceLock<AtomicU64> = OnceLock::new();

/// Current wall-clock time in milliseconds (the B3 stall backstop clock).
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
/// pipe, and the verifier pipe.
///
/// Small interface: `dispatch(work)` (the drain pipe; enqueue or run inline) +
/// `notify_block` (the block-clock pipe; a direct, non-FIFO dispatch) + `pending()`
/// (the drain lag metric). The implementation absorbs the channel, the background
/// drainer task, the inline-vs-deferred routing, and the B3 no-progress liveness
/// detector. A dead (closed-channel) drainer aborts immediately; a frozen drainer
/// aborts after `NO_PROGRESS_STRIKE_LIMIT` consecutive no-progress pushes; a
/// drainer that progresses but falls behind only WARNs (lag metric, never abort).
pub struct DispatchOwner {
    sink: Arc<dyn DrainSink>,
    // MQUKB6-T0: each item rides with the span current at `dispatch()` time so
    // the drainer task can enter it — solve/finalize/publish spans fired under
    // the drainer parent under the pump's per-block span instead of orphaning
    // into disconnected Jaeger root traces.
    drain_send: tokio::sync::mpsc::UnboundedSender<(DrainWork, tracing::Span, u64)>,
    drainer_health: Arc<DrainerHealth>,
    /// B3 stall backstop state, shared with the always-on watchdog task
    /// (U4UOIS): the watchdog samples the baselines here AND the pump
    /// refreshes them at every dispatch, so a wedge that stops the pump from
    /// dispatching (the incident-2026-08-21 `BotState` lock deadlock) can no
    /// longer blind the detector.
    stall_watch: Arc<StallWatch>,
    /// Wall-clock ms of the last accepted header (T2): the anchor for the
    /// `header_to_solved` latency histogram — the drainer stamps elapsed time
    /// when a Drain/Finalize item completes.
    header_ms: Arc<AtomicU64>,
}

impl DispatchOwner {
    /// Build the drain pipe, always spawning the background drainer task (the
    /// sole mode since B4 — the inline path is retired). The drainer holds
    /// clones of `sink` and `verify_tx`. `verify_tx` is the latest-wins verifier
    /// transmitter the `Publish` path forwards to. The WS poller never parks
    /// behind GIL-bound `Python::attach` / heavy Möbius solve.
    pub fn new(
        sink: Arc<dyn DrainSink>,
        verify_tx: &Option<tokio::sync::watch::Sender<Option<SolverVerifyRequest>>>,
    ) -> Self {
        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<(DrainWork, tracing::Span, u64)>();
        let health = Arc::new(DrainerHealth::new());
        let header_ms = Arc::new(AtomicU64::new(0));
        let sink_clone = Arc::clone(&sink);
        let vt = verify_tx.clone();
        let health_clone = Arc::clone(&health);
        let header_ms_clone = Arc::clone(&header_ms);
        let _drainer = tokio::spawn(async move {
            while let Some((work, parent, enqueued_ms)) = rx.recv().await {
                // Picked up: this item left the FIFO (the B3 depth signal).
                health_clone.picked_up.fetch_add(1, Ordering::Relaxed);
                // T2: queue time for this item (histogram; no-op when the
                // metrics gate is off).
                if let Some(p) = crate::instruments::pipeline() {
                    p.observe_drain_queue_wait(ms_to_secs(
                        now_millis().saturating_sub(enqueued_ms),
                    ));
                }
                // MQUKB6-T0: enter the dispatch-time span so sink spans
                // (`degenbot.arb.solve` & co) inherit the pump block context
                // across the task boundary. Inert when no subscriber is
                // installed (`Span::current()` is then the disabled root).
                let _parent_guard = parent.enter();
                // Solve-carrying items anchor the header→solved measurement;
                // computed before the match (Publish partially moves `work`).
                let carries_solve = !matches!(work, DrainWork::Publish { .. });
                match work {
                    DrainWork::Drain { block, metadata } => {
                        sink_clone.on_drain(block, &metadata);
                    }
                    DrainWork::Finalize { block, metadata } => {
                        sink_clone.finalize_block(block, &metadata);
                    }
                    DrainWork::Publish {
                        open,
                        metadata,
                        change_set,
                    } => {
                        // on_send first, then the latest-wins verifier.
                        sink_clone.on_send(&metadata);
                        if let Some(ref tx) = vt {
                            let _ = tx.send(Some((open, change_set)));
                        }
                    }
                }
                // T2: header→solved latency for solve-carrying work items.
                if carries_solve {
                    if let Some(p) = crate::instruments::pipeline() {
                        let header_ms = header_ms_clone.load(Ordering::Relaxed);
                        if header_ms != 0 {
                            p.observe_header_to_solved(ms_to_secs(
                                now_millis().saturating_sub(header_ms),
                            ));
                        }
                    }
                }
                health_clone.processed.fetch_add(1, Ordering::Relaxed);
                if let Some(p) = PROCESSED.get() {
                    p.store(
                        health_clone.processed.load(Ordering::Relaxed),
                        Ordering::Relaxed,
                    );
                }
            }
        });

        // U4UOIS: the stall check previously ran ONLY inside `dispatch` — i.e.
        // on the pump task. When the pump itself wedged (`BotState` lock held
        // across an await, incident 2026-08-21) it stopped dispatching and the
        // B3 abort never fired despite 15+ min of zero drain progress. This
        // watchdog task samples the same shared state independently of pump
        // liveness; verdict logic is identical (`StallWatch::check_and_abort`),
        // first sampler wins.
        let stall_watch = Arc::new(StallWatch::new());
        let watch_clone = Arc::clone(&stall_watch);
        // S53STH: the watchdog samples a process-global cancellation token
        // (installed by [`DispatchOwner::with_shutdown_token`] before or after
        // construction - the owner is unique per process). Unset = never
        // cancelled, identical to pre-S53STH behavior. On cooperative
        // shutdown the watchdog stops ticking so no abort sample lands between
        // the OTel flush and teardown.
        let _watchdog = tokio::spawn(async move {
            loop {
                #[cfg(feature = "otel")]
                tokio::select! {
                    // Token unset = never cancelled; the sleep arm always wins.
                    () = async {
                        match SHUTDOWN_TOKEN.get() {
                            Some(t) => t.cancelled().await,
                            None => std::future::pending().await,
                        }
                    } => return,
                    () = tokio::time::sleep(std::time::Duration::from_millis(
                        STALL_WATCHDOG_TICK_MS,
                    )) => watch_clone.check_and_abort(),
                }
                #[cfg(not(feature = "otel"))]
                {
                    watch_clone.check_and_abort();
                }
            }
        });

        DEPTH.get_or_init(|| AtomicU64::new(0));
        Self {
            sink,
            drain_send: tx,
            drainer_health: health,
            stall_watch,
            header_ms,
        }
    }

    /// S53STH: hand the owner a cooperative-shutdown token. When cancelled,
    /// the `StallWatch` watchdog stops sampling so no tick lands between the
    /// `OTel` flush and process teardown during the hotpath timed exit.
    /// Without this setter the watchdog uses a never-cancelled token —
    /// identical to its pre-S53STH behavior.
    #[cfg(feature = "otel")]
    #[must_use]
    pub fn with_shutdown_token(self, token: tokio_util::sync::CancellationToken) -> Self {
        // Process-global cell (the owner is unique per process): the watchdog
        // task reads it directly, so installation works before or after
        // construction.
        let _ = SHUTDOWN_TOKEN.set(token);
        self
    }

    /// Shrink the stall window for a subprocess freeze test (test-only).
    #[cfg(test)]
    fn set_stall_window_for_test(&mut self, window_ms: u64) {
        self.stall_watch
            .stall_window_ms
            .store(window_ms, Ordering::Relaxed);
        self.stall_watch
            .last_healthy_ms
            .store(now_millis(), Ordering::Relaxed);
    }

    /// The drain-pipe lag metric (B3): the number of work items currently queued
    /// behind the drainer (approximately, under a concurrent drainer).
    #[must_use]
    pub fn pending(&self) -> u64 {
        self.drainer_health.depth()
    }

    /// The drainer progress counters.
    #[must_use]
    pub fn health(&self) -> &DrainerHealth {
        &self.drainer_health
    }

    /// Route one drain-sink operation to the background drainer task (the sole
    /// mode since B4). FIFO ordering + the engine/sink locks give the deferred
    /// work the same semantics the pre-B4GX7C inline path had. For `Publish` the
    /// change-set is already consumed atomically by the caller (single-writer)
    /// and the verifier anchor is carried in the message.
    ///
    /// The B3 stall backstop (soak-hardened): the pump aborts when the drain
    /// pipe holds a backlog AND the drainer has completed nothing for
    /// [`Self::stall_window_ms`] — the wall-clock window (not event-counting) is what
    /// correctly distinguishes a *frozen* drainer from one mid-way through a
    /// single exceptionally long solve — the live dry-run proved event-counting
    /// false-positives on a busy-but-alive drainer.
    pub fn dispatch(&self, work: DrainWork) {
        let tx = &self.drain_send;
        let depth_before = self.drainer_health.depth();

        // Publish the depth gauge for the always-on watchdog (U4UOIS), then
        // run the same sample-and-abort verdict the watchdog runs — identical
        // logic (`StallWatch::check_and_abort`), so dispatch-time checks stay
        // latency-free and the watchdog covers the case where the pump itself
        // stops dispatching (the incident-2026-08-21 wedge).
        if let Some(d) = DEPTH.get() {
            d.store(depth_before, Ordering::Relaxed);
        }
        self.stall_watch.check_and_abort();

        let parent = tracing::Span::current();
        // T2: stamp enqueue time (queue-wait histogram) + sample depth gauge.
        if let Some(p) = crate::instruments::pipeline() {
            p.set_drain_queue_depth(depth_before);
        }
        if tx.send((work, parent, now_millis())).is_ok() {
            self.drainer_health.enqueued.fetch_add(1, Ordering::Relaxed);
        } else {
            tracing::error!(
                "[B4GX7C] drainer task dead: channel closed, cannot enqueue drain work"
            );
            #[expect(clippy::print_stderr)] // fatal diagnostic before abort
            {
                eprintln!(
                    "[B4GX7C] ABORT: background drainer task is dead (channel closed) \
                     — the WS poller can no longer enqueue solve/dispatch/publish. Fail \
                     loud, never half-alive."
                );
            }
            std::process::abort();
        }
    }

    /// T2: record that a block header was just accepted — the anchor the
    /// drainer measures `header_to_solved` latency against. Called from the
    /// pump's header arm; single-writer (pump task only).
    pub fn note_header_accepted(&self) {
        self.header_ms.store(now_millis(), Ordering::Relaxed);
    }

    /// The **block-clock pipe** (B2): forward a `newHeads` tick to the sink's
    /// engine notification channels. This is deliberately a DIRECT dispatch,
    /// NOT a `DrainWork` item — it NEVER rides the drain FIFO, so Python's head
    /// tracker is never queued behind solver work and every accepted header is
    /// delivered 1:1 (no coalescing). `notify_block` / `finalize` on the sink
    /// zero-contend on the drain FIFO; the fan-out is quick (`mpsc::send`).
    pub fn notify_block(&self, block: u64, metadata: &BlockMetadata) {
        self.sink.notify_block(block, metadata);
    }
}

#[expect(clippy::expect_used)] // subprocess-abort tests use .expect() on env/process calls
#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU64};

    /// Minimal recording sink: captures drain/send/finalize/notify calls so a
    /// test can assert that `DispatchOwner` routed work to it (inline) or that
    /// the background drainer applied it (decoupled) in FIFO order. Send/finalize
    /// count via atomics; drain/notify record blocks.
    struct RecordingSink {
        drained: Mutex<Vec<u64>>,
        send_count: AtomicU64,
        finalized: Mutex<Vec<u64>>,
        notified: Mutex<Vec<u64>>,
        last_processed: AtomicU64,
        dirty: AtomicBool,
    }

    impl RecordingSink {
        fn new() -> Self {
            Self {
                drained: Mutex::new(Vec::new()),
                send_count: AtomicU64::new(0),
                finalized: Mutex::new(Vec::new()),
                notified: Mutex::new(Vec::new()),
                last_processed: AtomicU64::new(0),
                dirty: AtomicBool::new(false),
            }
        }
    }

    impl DrainSink for RecordingSink {
        fn has_dirty_paths(&self) -> bool {
            self.dirty.load(Ordering::Relaxed)
        }
        fn on_drain(&self, block: u64, _metadata: &BlockMetadata) {
            self.drained.lock().push(block);
        }
        fn on_send(&self, _metadata: &BlockMetadata) {
            self.send_count.fetch_add(1, Ordering::Relaxed);
        }
        fn finalize_block(&self, block: u64, _metadata: &BlockMetadata) {
            self.finalized.lock().push(block);
        }
        fn set_last_solved_block(&self, block: u64) {
            self.last_processed.store(block, Ordering::Relaxed);
        }
        fn set_solve_anchor(&self, _block: u64) {}
        fn record_logs_this_block(&self) {}
        fn last_processed_block(&self) -> Option<u64> {
            Some(self.last_processed.load(Ordering::Relaxed))
        }
        fn notify_block(&self, block: u64, _metadata: &BlockMetadata) {
            self.notified.lock().push(block);
        }
    }

    fn owner_for(sink: &Arc<RecordingSink>) -> DispatchOwner {
        DispatchOwner::new(Arc::clone(sink) as Arc<dyn DrainSink>, &None)
    }

    /// The block-clock pipe is delivered even without any drain work — the
    /// clock is not gated behind the drain pipe at all.
    #[tokio::test]
    async fn block_clock_notify_bypasses_drain_pipe() {
        let sink = Arc::new(RecordingSink::new());
        let owner = owner_for(&sink);

        owner.notify_block(9, &BlockMetadata::default());

        // Delivered directly to the sink (not queued on the drain FIFO).
        assert_eq!(*sink.notified.lock(), vec![9]);
        assert_eq!(owner.health().enqueued(), 0);
    }

    /// The background drainer (the sole mode since B4) applies dispatch work
    /// asynchronously, with the health counters reflecting completed work.
    #[tokio::test]
    async fn dispatch_applies_via_drainer_and_counts() {
        let sink = Arc::new(RecordingSink::new());
        let owner = owner_for(&sink);

        owner.dispatch(DrainWork::Drain {
            block: 42,
            metadata: BlockMetadata::default(),
        });
        // The block-clock pipe is a direct dispatch, independent of the drainer.
        owner.notify_block(43, &BlockMetadata::default());

        // Wait for the drainer to consume the drain item.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if owner.health().processed() >= 1 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "drainer did not process work within timeout"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        // The drain applied via the drainer; the notify applied directly (and
        // was NOT queued behind the drain's solve).
        assert_eq!(*sink.drained.lock(), vec![42]);
        assert_eq!(*sink.notified.lock(), vec![43]);
    }

    /// Inline `Publish` hands the quiesced block + change-set to the latest-wins
    /// The `Publish` drain-work hands the quiesced block + change-set to the
    /// latest-wins verifier watch (the ADR-021 hand-off), after `on_send`. Via
    /// the background drainer (sole mode).
    #[tokio::test]
    #[expect(clippy::panic)] // the test asserts a required side effect; abort on absence
    async fn publish_forwards_to_verifier() {
        let sink = Arc::new(RecordingSink::new());
        let (verify_tx, verify_rx) = tokio::sync::watch::channel(None);
        let owner = DispatchOwner::new(Arc::clone(&sink) as Arc<dyn DrainSink>, &Some(verify_tx));

        owner.dispatch(DrainWork::Publish {
            open: 7,
            metadata: BlockMetadata::default(),
            change_set: Vec::new(),
        });

        // Wait for the drainer to process the publish (on_send + verify).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while owner.health().processed() < 1 {
            assert!(
                std::time::Instant::now() < deadline,
                "drainer did not process the publish within timeout"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        // on_send ran, then the verifier got the most-recent request.
        assert_eq!(sink.send_count.load(Ordering::Relaxed), 1);
        let got = verify_rx.borrow().clone();
        let Some((open, change_set)) = got else {
            panic!("verifier did not receive a publish");
        };
        assert_eq!(open, 7);
        assert!(change_set.is_empty());
    }

    #[test]
    fn no_progress_stalled_predicate() {
        // Pure `stalled()` semantics: a backlog with no completion for >= the
        // window is a stall; a backlog with recent completion, or no backlog,
        // is not.
        let floor = 2;
        let window = 30_000u64;
        // No backlog (< floor) → never stalled even after a long idle.
        assert!(!stalled(1, 60_000, floor, window));
        // Backlog but recent healthy moment → not stalled.
        assert!(!stalled(4, 1_000, floor, window));
        // Backlog + no completion for >= window → stalled (the freeze).
        assert!(stalled(4, window, floor, window));
        assert!(stalled(4, window + 1, floor, window));
    }

    /// A sink whose every operation blocks forever. Used to simulate a frozen
    /// drainer: the drainer picks up one item and never returns, so the queue
    /// grows without completion and the B3 stall backstop aborts.
    /// A sink whose every operation blocks forever. Used to simulate a frozen
    /// drainer: the drainer picks up one item and never returns, so the queue
    /// grows without pickup and the B3 no-progress detector aborts.
    /// Block the calling thread indefinitely WITHOUT the kernel-edge cases of
    /// `thread::sleep(Duration::MAX)` (huge timespec; on some kernels
    /// `clock_nanosleep` rejects it and std retries — a spin). A parked thread
    /// is a clean, zero-CPU indefinite block; the process aborts anyway, so
    /// the poisoned-result branch is unreachable in practice.
    fn park_forever() -> ! {
        use std::sync::Mutex;
        static PARK: Mutex<()> = Mutex::new(());
        let _deadlock = PARK.lock(); // held forever; nothing else locks PARK
        loop {
            std::thread::park();
        }
    }

    struct BlockingSink;

    impl DrainSink for BlockingSink {
        fn has_dirty_paths(&self) -> bool {
            // `-> !` coerces to bool for the sink trait's signature.
            park_forever()
        }
        fn on_drain(&self, _block: u64, _metadata: &BlockMetadata) {
            park_forever();
        }
        fn on_send(&self, _metadata: &BlockMetadata) {
            park_forever();
        }
        fn finalize_block(&self, _block: u64, _metadata: &BlockMetadata) {
            park_forever();
        }
        fn set_last_solved_block(&self, _block: u64) {}
        fn set_solve_anchor(&self, _block: u64) {}
        fn record_logs_this_block(&self) {}
        fn last_processed_block(&self) -> Option<u64> {
            None
        }
        fn notify_block(&self, _block: u64, _metadata: &BlockMetadata) {
            park_forever();
        }
    }

    /// The subprocess child: builds a DECOUPLED owner over a blocking sink (a
    /// genuinely frozen drainer) and dispatches enough work to trip the B3
    /// no-progress abort. Only runs when `DEGENBOT_NO_PROGRESS_ABORT_TEST=1`
    /// (spawned by the parent test); otherwise it is a benign no-op unit test.
    ///
    /// Uses a MULTI-THREAD runtime: the blocking sink's `std::thread::sleep`
    /// occupies one worker while the main task keeps dispatching — on a
    /// single-threaded runtime that `sleep` would block the only worker and
    /// deadlock the child before the abort can fire.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_progress_frozen_drainer_aborts_self() {
        if std::env::var("DEGENBOT_NO_PROGRESS_ABORT_TEST").as_deref() != Ok("1") {
            return;
        }
        let sink = Arc::new(BlockingSink);
        let mut owner = DispatchOwner::new(sink as Arc<dyn DrainSink>, &None);
        // Small stall window so the freeze aborts in milliseconds, not 30s.
        owner.set_stall_window_for_test(50);
        // The blocking sink means the drainer picks up one item and never
        // completes another, so the queue grows without completion; after the
        // stall window elapses the B3 backstop aborts.
        let meta = BlockMetadata::default();
        for i in 0..8u64 {
            owner.dispatch(DrainWork::Drain {
                block: i,
                metadata: meta,
            });
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
        unreachable!("B3 stall abort should have killed this process");
    }

    /// U4UOIS regression: the incident-2026-08-21 shape — the PUMP wedges
    /// (`BotState` lock held across an await) so `dispatch` is never called
    /// again after the drainer freezes mid-item. The dispatch-time B3 check
    /// therefore never re-runs; only the always-on watchdog can abort.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn watchdog_aborts_when_pump_stops_dispatching() {
        if std::env::var("DEGENBOT_NO_PROGRESS_ABORT_TEST").as_deref() != Ok("1") {
            return;
        }
        let sink = Arc::new(BlockingSink);
        let mut owner = DispatchOwner::new(sink as Arc<dyn DrainSink>, &None);
        owner.set_stall_window_for_test(50);
        let meta = BlockMetadata::default();
        // The drainer picks up item 0 and freezes mid-on_drain; items 1..
        // stay queued. THREE dispatches so the frozen-state backlog
        // (depth = 2) clears BACKLOG_FLOOR — two dispatches leave depth 1,
        // under the floor, and the (correctly!) never-firing check made this
        // child hang until its sleep elapsed instead of aborting. After the
        // third dispatch the pump wedges: no further dispatch calls, so only
        // the always-on watchdog tick can observe depth>=floor with a stale
        // healthy baseline and abort.
        owner.dispatch(DrainWork::Drain {
            block: 0,
            metadata: meta,
        });
        owner.dispatch(DrainWork::Drain {
            block: 1,
            metadata: meta,
        });
        owner.dispatch(DrainWork::Drain {
            block: 2,
            metadata: meta,
        });
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        unreachable!("watchdog abort should have killed this process before 30s");
    }

    /// The parent: spawn the pump-wedge child and assert it was killed
    /// (SIGABRT) with the loud `[B3] ABORT` marker on stderr — a wedged pump
    /// must not blind the drainer-liveness detector (U4UOIS).
    #[test]
    fn watchdog_aborts_when_pump_stops_dispatching_proc() {
        let exe = std::env::current_exe().expect("current test exe");
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg("ulimit -c 0; exec \"$@\"")
            .arg("sh")
            .arg(&exe)
            .arg("watchdog_aborts_when_pump_stops_dispatching")
            .arg("--nocapture")
            .env("DEGENBOT_NO_PROGRESS_ABORT_TEST", "1")
            .output()
            .expect("spawn pump-wedge abort subprocess");
        let status = out.status;
        assert!(
            !status.success(),
            "the watchdog must kill a process whose pump wedged, got {status:?}"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("[B3] ABORT"),
            "watchdog must print the loud grep-able marker; got: {stderr}"
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

    /// U4UOIS pure-verdict tests: pin the refresh decision and the wedge-test
    /// dispatch count WITHOUT clocks, globals, or subprocesses — these must
    /// pass before any process-spawning freeze test runs.
    #[test]
    fn stall_refresh_verdicts() {
        // Drainer completed something since the last sample -> refresh.
        assert!(stall_refresh(3, 5, 4));
        // Queue went idle -> refresh (an idle pipe must never accumulate
        // staleness toward the abort threshold).
        assert!(stall_refresh(0, 5, 5));
        // Backlog unchanged, no completions -> do NOT refresh (the
        // frozen-drainer case where staleness must accrue).
        assert!(!stall_refresh(3, 5, 5));
    }

    /// The wedge-test child must dispatch enough items to exceed
    /// `BACKLOG_FLOOR`: the first version dispatched 2, leaving depth 1 (the
    /// drainer froze mid-item), under the floor — so no verdict could ever
    /// fire and the child hung until its sleep elapsed. Three dispatches
    /// leave depth 2 -> fires.
    #[test]
    fn wedge_child_dispatch_count_exceeds_floor() {
        let picked_up = 1u64; // the drainer froze mid-item
                              // Old (buggy) child: 2 dispatches -> depth 1 -> never stalls.
        assert!(!stalled(2 - picked_up, u64::MAX, BACKLOG_FLOOR, 50));
        // Fixed child: 3 dispatches -> depth 2 -> stalls at any window.
        assert!(stalled(3 - picked_up, u64::MAX, BACKLOG_FLOOR, 50));
    }

    /// The parent: spawn the child and assert it was killed (SIGABRT) with the
    /// loud `[B3] ABORT` marker on stderr — a frozen drainer must never silently
    /// lose solve/dispatch/publish.
    #[test]
    fn no_progress_frozen_drainer_aborts_proc() {
        let exe = std::env::current_exe().expect("current test exe");
        // Suppress the kernel core dump for this intentional SIGABRT (same as
        // the existing solver-state desync abort test).
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg("ulimit -c 0; exec \"$@\"")
            .arg("sh")
            .arg(&exe)
            .arg("no_progress_frozen_drainer_aborts_self")
            .arg("--nocapture")
            .env("DEGENBOT_NO_PROGRESS_ABORT_TEST", "1")
            .output()
            .expect("spawn no-progress abort subprocess");
        let status = out.status;
        assert!(
            !status.success(),
            "a frozen drainer must kill the process, got {status:?}"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("[B3] ABORT"),
            "frozen drainer must print the loud grep-able marker; got: {stderr}"
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
}
