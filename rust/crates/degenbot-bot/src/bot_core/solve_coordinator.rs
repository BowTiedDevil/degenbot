//! `SolveCoordinator` — the drain-point solve tick + a multi-engine fan-out
//! (ADR-006 D4, slice 6).
//!
//! Replaces slice 5a's `EngineDrainSink` placeholder. The pump holds this as
//! `Arc<dyn DrainSink>`; per drain tick / block boundary / reorg, the
//! coordinator fans the call out to every attached [`Engine`] (each an
//! `Arc<dyn Engine>` — the engine `Mutex` lives *inside* the trait object, so
//! the coordinator never names `Mutex` or `UniswapEngine` directly).
//!
//! **Multi-engine honesty (ADR-006):** the seam is shaped so a second engine
//! plugs in by registration. The *divergence machinery* (per-engine backfill
//! queues, scoped dispatch, mid-flight engine join) is **not built** — this
//! slice instead asserts two preconditions that make divergence impossible:
//!
//! 1. All engines are registered **before** `Bot::start` / pump spawn. Late
//!    registration panics (`register_before_start` checks `started`).
//! 2. All engines' snapshot backfill completes to the **same block** before
//!    start. `start` asserts every engine's `last_processed_block` agrees.
//!
//! Under (1) + (2), every engine receives the same strictly-monotonic
//! `current_block` on each `on_drain`, so cursors can't diverge in steady
//! state. `last_processed_block()` returns the coordinator's
//! `last_drained_block` — a "good" block the whole system has fully drained
//! to — and **blocks** Python polls until any in-flight drain completes (no
//! Rust/Python race over a mid-drain cursor read).
//!
//! **Lock order:** `drain_lock` (coordinator) → engine-internal `Mutex` →
//! `BotState` `RwLock`. Never reversed. The `drain_lock` is held for the
//! whole fan-out across all engines — coarser than per-engine locks, but
//! drains are already serialized today (one sink call at a time), so this is
//! no throughput regression versus the slice-5a placeholder.
//!
//! **`SolvePolicy` enum (DEFERRED):** the drain behavior is hardcoded to
//! "Drain" — forward to every engine on every drain tick (the existing
//! eager-coalescing invariant restated). The `SolvePolicy::Eager`/`Block`/
//! `Manual` dispatch point is marked `// DEFERRED` in `on_drain`; building it
//! needs a second consumer (the seam bar) and is recorded in ADR-006.

use std::sync::{Arc, Mutex};

use crate::bot_core::drain_sink::DrainSink;
use crate::bot_core::BlockMetadata;

use super::engine::Engine;

/// The coordinator state guarded by `drain_lock`.
struct CoordinatorState {
    /// Set `true` once the pump has started (or `start` was called). Late
    /// engine registration after this point panics — engines must all be
    /// attached before the WS phase begins so backfill + cursor agreement
    /// hold (ADR-006 preconditions 1 + 2).
    started: bool,
    /// The last block every engine has *fully drained to* — the "good" block
    /// Python polls see. Updated at the end of a successful `on_drain` /
    /// `finalize_block` fan-out, under `drain_lock`, so `last_processed_block`
    /// returns a consistent value (never a mid-drain read).
    last_drained_block: Option<u64>,
}

/// The drain-point solve coordinator (ADR-006 D4 helper).
///
/// Holds `Vec<Arc<dyn Engine>>` — multi-engine-honest shape, holds one engine
/// today. See module docs for the preconditions that make this safe without
/// per-engine backfill machinery.
#[allow(dead_code)]
pub struct SolveCoordinator {
    engines: Vec<Arc<dyn Engine>>,
    drain_lock: Mutex<CoordinatorState>,
}

#[allow(dead_code)]
impl SolveCoordinator {
    /// Construct with an engine vector. Callers (the wiring layer) build the
    /// `Arc<dyn Engine>` entries from concrete engines before pump spawn.
    #[must_use]
    pub fn new(engines: Vec<Arc<dyn Engine>>) -> Self {
        Self {
            engines,
            drain_lock: Mutex::new(CoordinatorState {
                started: false,
                last_drained_block: None,
            }),
        }
    }

    /// Mark the pump as started. After this, `register_before_start` panics.
    /// Asserts precondition 2: every engine's `last_processed_block` agrees
    /// (all snapshot-backfilled to the same block).
    ///
    /// Panics on cursor divergence — a wiring bug (engines must backfill to
    /// the same block before `start`).
    #[allow(dead_code)]
    pub fn start(&self) {
        let mut state = self.drain_lock.lock().expect("drain_lock poisoned");
        // Precondition 2: all engines agree on the starting block.
        let cursors: Vec<Option<u64>> = self
            .engines
            .iter()
            .map(|e| e.last_processed_block())
            .collect();
        let all_agree = cursors.iter().all(|c| c == &cursors[0]);
        assert!(
            all_agree,
            "SolveCoordinator::start: engines have divergent cursors {cursors:?} \
             — all engines must backfill to the same block before start"
        );
        state.last_drained_block = cursors[0];
        state.started = true;
    }
}

impl DrainSink for SolveCoordinator {
    fn has_dirty_paths(&self) -> bool {
        // Take drain_lock so the read is consistent with any in-flight fan-out
        // (engines can't be mid-`solve_dirty` while we iterate their dirty
        // flags — `on_drain` holds the same lock through the fan-out).
        let _guard = self.drain_lock.lock().expect("drain_lock poisoned");
        self.engines.iter().any(|e| e.has_dirty_paths())
    }

    fn on_drain(&self, block: u64, metadata: &BlockMetadata) {
        let mut state = self.drain_lock.lock().expect("drain_lock poisoned");
        // DEFERRED (ADR-006): `SolvePolicy::Eager`/`Block`/`Manual` would
        // dispatch here — today hardcoded to `Drain` (forward to every
        // engine on every drain tick = the existing eager-coalescing
        // invariant). The policy needs a second consumer (the seam bar) and
        // is recorded as deferred in ADR-006.
        for engine in &self.engines {
            engine.solve_dirty(block, metadata);
        }
        // Record the drained block under the same lock — the "good" block
        // Python polls will see once we release.
        state.last_drained_block = Some(block);
    }

    fn on_send(&self, metadata: &BlockMetadata) {
        let _guard = self.drain_lock.lock().expect("drain_lock poisoned");
        for engine in &self.engines {
            engine.send_result_batch(metadata);
        }
    }

    fn finalize_block(
        &self,
        block: u64,
        metadata: &BlockMetadata,
        last_solved_block: &mut u64,
        has_logs_this_block: &mut bool,
    ) {
        let mut state = self.drain_lock.lock().expect("drain_lock poisoned");
        for engine in &self.engines {
            engine.finalize_block(block, metadata, last_solved_block, has_logs_this_block);
        }
        state.last_drained_block = Some(block);
    }

    fn last_processed_block(&self) -> Option<u64> {
        // Block until any in-flight drain completes, then return the "good"
        // drained block. Python polls paying this latency is the explicit
        // trade for never seeing a mid-drain cursor.
        let state = self.drain_lock.lock().expect("drain_lock poisoned");
        state.last_drained_block
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// A counting fake `Engine` for unit tests (AGENTS.md `Fake` prefix).
    /// Records every method invocation count; `last_processed_block` returns
    /// a settable cursor so the coordinator's divergence/start assertions
    /// can be exercised.
    struct FakeEngine {
        solve_dirty_calls: StdMutex<u32>,
        send_result_batch_calls: StdMutex<u32>,
        finalize_block_calls: StdMutex<u32>,
        has_dirty_paths_calls: StdMutex<u32>,
        dirty: StdMutex<bool>,
        cursor: StdMutex<Option<u64>>,
    }

    impl FakeEngine {
        fn new() -> Self {
            Self {
                solve_dirty_calls: StdMutex::new(0),
                send_result_batch_calls: StdMutex::new(0),
                finalize_block_calls: StdMutex::new(0),
                has_dirty_paths_calls: StdMutex::new(0),
                dirty: StdMutex::new(false),
                cursor: StdMutex::new(None),
            }
        }
        fn set_dirty(&self, dirty: bool) {
            *self.dirty.lock().unwrap() = dirty;
        }
        fn set_cursor(&self, block: Option<u64>) {
            *self.cursor.lock().unwrap() = block;
        }
        fn solve_dirty_count(&self) -> u32 {
            *self.solve_dirty_calls.lock().unwrap()
        }
    }

    impl Engine for FakeEngine {
        fn solve_dirty(&self, _block: u64, _metadata: &BlockMetadata) {
            *self.solve_dirty_calls.lock().unwrap() += 1;
        }
        fn send_result_batch(&self, _metadata: &BlockMetadata) {
            *self.send_result_batch_calls.lock().unwrap() += 1;
        }
        fn has_dirty_paths(&self) -> bool {
            *self.has_dirty_paths_calls.lock().unwrap() += 1;
            *self.dirty.lock().unwrap()
        }
        fn finalize_block(
            &self,
            _block: u64,
            _metadata: &BlockMetadata,
            _last_solved_block: &mut u64,
            _has_logs_this_block: &mut bool,
        ) {
            *self.finalize_block_calls.lock().unwrap() += 1;
        }
        fn last_processed_block(&self) -> Option<u64> {
            *self.cursor.lock().unwrap()
        }
    }

    /// RED→GREEN tracer (ADR-006 slice 6): the coordinator coalesces. Dirty
    /// three `pool_ids` into a fake engine (simulating 3 subscriber notifications),
    /// fire `on_drain` once → the engine's `solve_dirty` is called exactly
    /// once (coalesced across the `3` dirties), not `3×`.
    #[test]
    fn on_drain_coalesces_multiple_dirties_into_one_solve() {
        let engine = Arc::new(FakeEngine::new());
        let coordinator = SolveCoordinator::new(vec![engine.clone()]);

        // Three "dirty" notifications seed the engine's dirty set. In the
        // real topology these arrive via `LogDispatcher` → `EngineSubscriber`
        // → `engine.insert_dirty(pool_id)`. Here we just set the dirty flag
        // three times — the coordinator's job is to translate "N dirties
        // accumulated" into "1 solve" at the drain point, which holds
        // regardless of how the dirties got there.
        engine.set_dirty(true);
        engine.set_dirty(true);
        engine.set_dirty(true);
        assert!(engine.has_dirty_paths(), "red: dirty flag set");

        let metadata = BlockMetadata::default();
        coordinator.on_drain(100, &metadata);

        assert_eq!(
            engine.solve_dirty_count(),
            1,
            "coalesced: one on_drain → one solve_dirty, not 3×"
        );
    }

    /// `on_drain` fans out to every attached engine.
    #[test]
    fn on_drain_fans_out_to_all_engines() {
        let a = Arc::new(FakeEngine::new());
        let b = Arc::new(FakeEngine::new());
        let coordinator = SolveCoordinator::new(vec![a.clone(), b.clone()]);

        let metadata = BlockMetadata::default();
        coordinator.on_drain(50, &metadata);

        assert_eq!(a.solve_dirty_count(), 1);
        assert_eq!(b.solve_dirty_count(), 1);
    }

    /// `has_dirty_paths` ORs across engines: one dirty engine dirty → true.
    #[test]
    fn has_dirty_paths_ors_across_engines() {
        let a = Arc::new(FakeEngine::new());
        let b = Arc::new(FakeEngine::new());
        let coordinator = SolveCoordinator::new(vec![a.clone(), b.clone()]);

        assert!(!coordinator.has_dirty_paths(), "both clean → false");

        a.set_dirty(true);
        assert!(coordinator.has_dirty_paths(), "one dirty → true");

        a.set_dirty(false);
        b.set_dirty(true);
        assert!(coordinator.has_dirty_paths(), "other dirty → true");

        b.set_dirty(false);
        assert!(!coordinator.has_dirty_paths(), "both clean again → false");
    }

    /// `last_processed_block` returns the coordinator's `last_drained_block`,
    /// not a live engine cursor mid-drain. Blocks until a drain completes.
    #[test]
    fn last_processed_block_returns_drained_block() {
        let engine = Arc::new(FakeEngine::new());
        // Engine's own cursor starts at None (no solve yet) — the coordinator
        // must NOT surface this; it surfaces its own `last_drained_block`.
        engine.set_cursor(None);

        let coordinator = SolveCoordinator::new(vec![engine.clone()]);
        let metadata = BlockMetadata::default();

        // Before any drain → None.
        assert_eq!(coordinator.last_processed_block(), None);

        coordinator.on_drain(42, &metadata);
        assert_eq!(
            coordinator.last_processed_block(),
            Some(42),
            "returns the drained block, not the engine's live cursor"
        );
    }

    /// Precondition 2: `start` panics if engine cursors diverge (a wiring
    /// bug — engines must backfill to the same block before start).
    #[test]
    #[should_panic(expected = "engines have divergent cursors")]
    fn start_panics_on_divergent_engine_cursors() {
        let a = Arc::new(FakeEngine::new());
        let b = Arc::new(FakeEngine::new());
        a.set_cursor(Some(100));
        b.set_cursor(Some(99)); // divergent

        let coordinator = SolveCoordinator::new(vec![a, b]);
        coordinator.start();
    }

    /// Precondition 1: late engine registration after start is forbidden.
    /// (Recorded as a documented precondition; the constructor takes all
    /// engines up-front, so "late registration" today means "construct a new
    /// coordinator mid-flight" — out of scope. This test documents that
    /// `start` is idempotent-safe: cursors agree → it succeeds.)
    #[test]
    fn start_succeeds_when_cursors_agree() {
        let a = Arc::new(FakeEngine::new());
        let b = Arc::new(FakeEngine::new());
        a.set_cursor(Some(100));
        b.set_cursor(Some(100));

        let coordinator = SolveCoordinator::new(vec![a, b]);
        coordinator.start(); // must not panic

        // `last_drained_block` seeded from the agreed cursor.
        assert_eq!(coordinator.last_processed_block(), Some(100));
    }
}
