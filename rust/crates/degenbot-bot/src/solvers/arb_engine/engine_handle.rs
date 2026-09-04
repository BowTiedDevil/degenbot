//! `EngineHandle` — the `Arc<dyn Engine>` wrapper for a shared
//! `ArbitrageEngine` (ADR-006 D4, slice 6).
//!
//! [`SolveCoordinator`](crate::bot_core::solve_coordinator::SolveCoordinator)
//! holds `Arc<dyn Engine>` and never names `Mutex` or `ArbitrageEngine`
//! directly. This wrapper holds a strong clone of the shared
//! `Arc<parking_lot::Mutex<ArbitrageEngine>>` (the same `Arc` that
//! `PyArbitrageEngine.engine` and the pump's strong handle both reference),
//! and implements [`Engine`](crate::bot_core::engine::Engine) by
//! locking-then-forwarding — so the engine's lock is encapsulated behind the
//! trait object.
//!
//! Replaces slice 5a's `EngineDrainSink` (a `Weak<Mutex<ArbitrageEngine>>`
//! pass-through that held the concrete type leakily and impl'd `DrainSink`).
//! The handle is the strong-owning, trait-erased counterpart: the coordinator
//! sees only `Arc<dyn Engine>`.
//!
//! **Lock order (D2):** each method locks the engine `Mutex` **alone** —
//! the `BotState` write guard from the preceding `Bot::dispatch_log` is
//! already released (slice 4's `EngineSubscriber` invariant), and the engine
//! takes its own core `read`/`write` internally. Coordinator `drain_lock` →
//! `engine-Mutex` → `BotState` `RwLock`, never reversed.

use std::sync::{Arc, Weak};

use crate::bot_core::engine::Engine;
use crate::bot_core::log_dispatcher::PoolStateSubscriber;
use crate::bot_core::BlockMetadata;
use degenbot_solvers::mixed::MixedPoolRef;

use super::engine_subscriber::EngineSubscriber;
use super::ArbitrageEngine;

/// A `Arc<dyn Engine>` view over a shared `Arc<parking_lot::Mutex<ArbitrageEngine>>`.
///
/// The strong handle the wiring layer (`py_binding.rs`) clones into the
/// coordinator's engine vector. `PyArbitrageEngine.engine` retains its own
/// strong clone for direct engine access (e.g. `register_path`,
/// `latest_results`) not on the `Engine` drain trait; both clones reference
/// the same underlying `ArbitrageEngine`.
pub struct EngineHandle {
    engine: Arc<parking_lot::Mutex<ArbitrageEngine>>,
    /// Strong owner of the `EngineSubscriber` (ADR-006 cycle-free topology:
    /// the strong lives on the engine side, co-owned with `engine`).
    /// `LogDispatcher::notify` holds only a `Weak<dyn PoolStateSubscriber>`;
    /// its `upgrade()` succeeds until every `EngineHandle` (and thus the
    /// engine) drops — the subscriber dies with the engine, no leak, no cycle.
    ///
    /// This field exists because `EngineSubscriber::weak_handle` returned a
    /// dangling `Weak` (its strong dropped on return). Holding the strong here
    /// is the lift: the subscriber is alive for the engine's lifetime.
    subscriber: Arc<dyn PoolStateSubscriber>,
}

impl EngineHandle {
    /// Construct from a strong clone of the shared engine handle.
    ///
    /// Builds and holds the strong `EngineSubscriber` (the cycle-free home
    /// for the dispatcher's `Weak`) — see [`subscriber_weak`](Self::subscriber_weak).
    #[must_use]
    pub fn new(engine: Arc<parking_lot::Mutex<ArbitrageEngine>>) -> Self {
        // RAYPAR engine-shard T3: pass the shared dirty sets + core to the
        // subscriber so on_pool_state_updated never takes the engine lock.
        let (dirty, core) = {
            let guard = engine.lock();
            (Arc::clone(&guard.dirty_sets), Arc::clone(guard.core()))
        };
        let subscriber: Arc<dyn PoolStateSubscriber> =
            Arc::new(EngineSubscriber::new(Arc::downgrade(&engine), dirty, core));
        Self { engine, subscriber }
    }

    /// A `Weak<dyn PoolStateSubscriber>` that stays live while this `EngineHandle`
    /// (and thus the engine) lives. The wiring layer hands this to
    /// `Bot::attach_engine` at `register_path` time so `LogDispatcher::notify`
    /// routes `on_pool_state_updated` → `insert_dirty` on the live engine.
    ///
    /// Replaces the deleted `EngineSubscriber::weak_handle`, which returned a
    /// dangling `Weak` (its strong dropped on return — hotpath capture
    /// 2026-07-14 showed 71 notifies → 0 dirties because every `upgrade()`
    /// returned `None`).
    #[must_use]
    pub fn subscriber_weak(&self) -> Weak<dyn PoolStateSubscriber> {
        Arc::downgrade(&self.subscriber)
    }

    /// Construct an `Arc<dyn Engine>` from a strong clone — the convenience
    /// the wiring layer uses to populate `SolveCoordinator::new`.
    #[must_use]
    pub fn arc_dyn(engine: Arc<parking_lot::Mutex<ArbitrageEngine>>) -> Arc<dyn Engine> {
        Arc::new(Self::new(engine))
    }
}

/// Is the caller inside an ambient multi-thread tokio runtime (the
/// production pump uses `degenbot_core::runtime::get_runtime()`, which is
/// `new_multi_thread`)? `block_in_place` is only valid there; a
/// current-thread runtime or no runtime means run inline.
fn is_multi_thread_runtime() -> bool {
    tokio::runtime::Handle::try_current()
        .is_ok_and(|handle| handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
}

impl EngineHandle {
    /// SRQEK5 (WV62TX): if the just-returned `solve_dirty` ENQUEUED the
    /// FIRST detached cycle, the merge sidecar is
    /// not running yet — take the parked Receiver and spawn the sidecar now
    /// (a plain `std::thread`, per the epic DEADLOCK note: a scoped rayon
    /// install JOINS its tasks and can starve against a held `parking_lot`
    /// guard; `std::thread` cannot deadlock with rayon). Later detached
    /// enqueues reuse the running sidecar through the stored
    /// `detached_merge_tx` — this is a cheap `Option` check for them.
    /// Spawn failure aborts LOUDLY: a stranded merge pipe would silently
    /// orphan every detached result (the preferred loud-failure posture).
    fn spawn_detached_sidecar_if_pending(&self) {
        let Some(merge_rx) = self.engine.lock().take_detached_merge_rx() else {
            return;
        };
        let engine_arc = Arc::clone(&self.engine);
        if let Err(err) = std::thread::Builder::new()
            .name("arb-detached-merge".to_string())
            .spawn(move || {
                super::solver_dispatch::detached_merge_sidecar(&engine_arc, merge_rx);
            })
        {
            tracing::error!(
                error = %err,
                "detached merge sidecar spawn failed — aborting (stranded merge pipe)"
            );
            std::process::abort();
        }
    }
}

impl Engine for EngineHandle {
    /// Hold-time invariant (ergo 3HYYGQ, assessed 2026-06 — no refactor).
    ///
    /// This call holds the engine `Mutex` for the whole `solve_dirty`,
    /// including the rayon `par_iter` solve in `rebuild_and_solve_affected`
    /// (~5-20ms for 100-200 affected paths). This is **acceptable**: Python's
    /// result hot path consumes batches via the unbounded `mpsc` channel
    /// (`PyArbitrageEngine::__anext__` → `rx.recv().await`), which never
    /// acquires the engine lock, so the hold does NOT block result delivery.
    /// The only cross-thread contenders during a solve are:
    ///   - `EngineSubscriber::insert_dirty` (queues a dirty marker for the
    ///     NEXT solve — a delayed mark is absorbed by the following
    ///     `solve_dirty` iteration, so the delay is benign);
    ///   - non-hot-path introspection/admin methods (`inspect_path`,
    ///     `latest_results`, `deregister_path`, `set_profit_thresholds`).
    ///
    /// Do NOT speculatively split this lock (e.g. release around the rayon
    /// `par_iter`) without first re-deriving interleaving safety for
    /// `register_path`/`deregister_path`: those run GIL-released on the
    /// Python thread concurrently with the pump and currently rely on this
    /// single hold for serialization. `latest_results()` is test/admin-only —
    /// grep-verified absent from the example hot loop, which is
    /// `async for batch in engine_registry.engine:`.
    ///
    /// **Detached-cycles exception (epic SRQEK5, task 4QKZE3):** when the
    /// engine's `detached_solving` stance is ON, this hold collapses to
    /// ENQUEUE end (~µs — resolve/gate/bookkeeping only): the solves run on
    /// per-bin threads and each straggler merges on the sidecar thread under
    /// its own per-item engine-Mutex acquisition. The `register_path`/
    /// `deregister_path` serialization argument above still holds in detached
    /// mode — those methods take the SAME engine Mutex the per-item merges
    /// acquire, so no interleaving hazard is created by the split. The
    /// `degenbot.solve.mutex_hold` histogram therefore shifts from the
    /// 5-20ms range to the µs range ONLY while the stance is ON (default
    /// OFF until epic SRQEK5 T3's soak flip); the historical in-cycle hold
    /// text above stays true for the default engine.
    #[hotpath::measure(label = "EngineHandle::solve_dirty")]
    fn solve_dirty(&self, block: u64, metadata: &BlockMetadata) {
        // P5FEOI (epic 2LXPPV): the drain-path solve is one Jaeger node
        // (OTel tier-1). Entered for the whole lock-hold so sim/dispatch/
        // monitor spans fired inside inherit it, and it parents under
        // `degenbot.pump.block` when pump-driven (MQUKB6). Inert without
        // a subscriber; lock-hold invariant untouched.
        //
        // T0 no-op gating: most per-block solves find nothing dirty (a ~2µs
        // lock-and-return) and their spans flooded Jaeger's recent-traces list,
        // drowning the real solves. Emit the span only when there is dirty
        // work.
        //
        // K4ETHF follow-up (block 25900244 / trace f06ea422): the probe and
        // the work previously ran under TWO separate lock acquisitions, so a
        // log burst applied by the pump thread between them landed in
        // dirty_sets after the probe — the solve then did REAL work (1518
        // affected paths) through the no-span branch and its phase spans
        // orphaned into the drainer's pump.block context. That also dropped
        // the solve_duration histogram sample + solves_executed count. The
        // gate and the work now share ONE mutex acquisition — dirt marking
        // requires the same mutex, so probe and take cannot disagree. The
        // no-dirty path still emits no span (T0) and is idempotent under
        // the held guard.
        let mut engine = hotpath::measure_block!("EngineHandle::solve_dirty.probe_lock", {
            self.engine.lock()
        });
        if !engine.has_dirty_paths() {
            engine.solve_dirty(block, metadata);
            drop(engine);
            self.spawn_detached_sidecar_if_pending();
            return;
        }
        let span = tracing::info_span!("degenbot.arb.solve", block.number = block);
        let _guard = span.enter();
        // T3: solve duration + registered-path gauge (dirty solves only; the
        // no-op path above returns before this).
        let solve_start = std::time::Instant::now();
        {
            // The mutex is ALREADY held (probe acquisition above) — the
            // former second acquire (and its TOCTOU window) is gone. The
            // hold window measured below covers the in-engine solve only.
            if let Some(p) = crate::instruments::pipeline() {
                p.set_registered_paths(u64::try_from(engine.path_count()).unwrap_or(u64::MAX));
            }
            // T2 (epic BXZBWY): the solve cycle — including the T1 streaming
            // drain's std-channel recv on the dedicated solve executor — must
            // not pin a shared-pump-runtime worker while it runs. The single
            // Mutex hold stays (the register_path/deregister_path warning
            // above is honored); block_in_place marks THIS worker blocking so
            // the multi-thread scheduler spawns/migrates the other tasks
            // (block clock, WS) off it. In production the caller runs on
            // `degenbot_core::runtime::get_runtime()` (multi-thread); outside
            // a runtime or on a current-thread flavor the inline path is the
            // prior behavior (tests without a runtime).
            let hold_start = std::time::Instant::now();
            if is_multi_thread_runtime() {
                tokio::task::block_in_place(|| engine.solve_dirty(block, metadata));
            } else {
                engine.solve_dirty(block, metadata);
            }
            if let Some(p) = crate::instruments::pipeline() {
                p.observe_mutex_hold_duration(hold_start.elapsed().as_secs_f64());
            }
            // SRQEK5 (WV62TX): the detached cycle's enqueue half returned
            // inside \`engine.solve_dirty\` — if this was the FIRST detached
            // enqueue the merge sidecar is not running yet. Take the parked
            // Receiver and spawn it while the rx take + spawn stay atomic;
            // the fresh thread parks on the engine lock until this hold ends.
            if let Some(merge_rx) = engine.take_detached_merge_rx() {
                let engine_arc = Arc::clone(&self.engine);
                if let Err(err) = std::thread::Builder::new()
                    .name("arb-detached-merge".to_string())
                    .spawn(move || {
                        super::solver_dispatch::detached_merge_sidecar(&engine_arc, merge_rx);
                    })
                {
                    // LOUD abort (loud-failure discipline): a stranded merge
                    // pipe would orphan every future detached result.
                    tracing::error!(
                        error = %err,
                        "detached merge sidecar spawn failed — aborting (stranded merge pipe)"
                    );
                    std::process::abort();
                }
            }
        }
        if let Some(p) = crate::instruments::pipeline() {
            p.observe_solve_duration(solve_start.elapsed().as_secs_f64());
            p.count_solves_executed();
        }
    }

    #[hotpath::measure(label = "EngineHandle::on_pump_ended")]
    fn on_pump_ended(&self) {
        self.engine.lock().on_pump_ended();
    }

    #[hotpath::measure(label = "EngineHandle::send_result_batch")]
    fn send_result_batch(&self, metadata: &BlockMetadata) {
        self.engine.lock().send_result_batch(metadata);
    }

    #[hotpath::measure(label = "EngineHandle::has_dirty_paths")]
    fn has_dirty_paths(&self) -> bool {
        self.engine.lock().has_dirty_paths()
    }

    #[hotpath::measure(label = "EngineHandle::finalize_block")]
    fn finalize_block(&self, block: u64, metadata: &BlockMetadata) {
        self.engine.lock().finalize_block(block, metadata);
    }

    fn set_last_solved_block(&self, block: u64) {
        self.engine.lock().set_last_solved_block(block);
    }

    fn set_solve_anchor(&self, block: u64) {
        self.engine.lock().set_solve_anchor(block);
    }

    fn record_logs_this_block(&self) {
        self.engine.lock().record_logs_this_block();
    }

    fn last_processed_block(&self) -> Option<u64> {
        self.engine.lock().last_processed_block()
    }

    fn solver_path_pool_refs(&self) -> Vec<Vec<MixedPoolRef>> {
        self.engine.lock().solver_path_pool_refs()
    }

    fn take_solver_path_pool_refs_change_set(&self) -> Vec<Vec<MixedPoolRef>> {
        self.engine.lock().take_solver_path_pool_refs_change_set()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RED→GREEN tracer (slice 6): `EngineHandle` forwards each `Engine`
    /// method to the underlying `ArbitrageEngine` without panic. Empty engine
    /// → no dirty paths; `solve_dirty`/`send_result_batch` are no-ops but
    /// must not panic.
    #[test]
    fn engine_handle_forwards_calls_without_panic() {
        let engine = Arc::new(parking_lot::Mutex::new(ArbitrageEngine::new()));
        let handle = EngineHandle::new(engine);
        let metadata = BlockMetadata::default();

        assert!(!handle.has_dirty_paths(), "fresh engine has no dirty paths");
        assert_eq!(
            handle.last_processed_block(),
            None,
            "fresh engine processed no block"
        );

        handle.solve_dirty(1, &metadata);
        handle.send_result_batch(&metadata);
        handle.set_last_solved_block(0);
        handle.finalize_block(1, &metadata);

        assert!(!handle.has_dirty_paths());
    }

    /// GREEN (fix for the hotpath-captured bug 2026-07-14): `subscriber_weak`
    /// returns a `Weak<dyn PoolStateSubscriber>` whose `upgrade()` succeeds
    /// while the `EngineHandle` (and thus the engine) lives. This is the
    /// invariant `LogDispatcher::notify` relies on to route
    /// `on_pool_state_updated` → `insert_dirty`.
    ///
    /// The deleted `EngineSubscriber::weak_handle` returned a *dangling* Weak
    /// (its strong dropped on return) — the 5-minute mainnet capture showed
    /// 71 WS logs reaching `notify` but 0 `on_drain`/`solve_dirty`, because
    /// every `weak.upgrade()` returned `None`. Holding the strong on
    /// `EngineHandle` (the ADR-006 cycle-free engine-side owner) is the lift.
    #[test]
    fn subscriber_weak_stays_live_while_engine_handle_lives() {
        let weak = {
            let engine = Arc::new(parking_lot::Mutex::new(ArbitrageEngine::new()));
            let handle = EngineHandle::new(engine);
            handle.subscriber_weak() // handle drops at end of block
        };
        // EngineHandle dropped → the strong subscriber dropped → Weak is dead.
        assert!(
            weak.upgrade().is_none(),
            "subscriber Weak should dangle once EngineHandle drops"
        );

        // And the positive case: held alive → upgrade succeeds.
        let engine = Arc::new(parking_lot::Mutex::new(ArbitrageEngine::new()));
        let handle = EngineHandle::new(engine);
        let weak = handle.subscriber_weak();
        assert!(
            weak.upgrade().is_some(),
            "subscriber Weak must upgrade while EngineHandle is alive — \
             LogDispatcher::notify depends on this to fire on_pool_state_updated"
        );
    }
}
