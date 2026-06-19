//! Snapshot store + CL-registration orchestration (ADR-006 slice 5b).
//!
//! Extracted from `py_binding.rs` so the pure-logic core is testable without
//! `PyO3`. `py_binding.rs` keeps the `PyO3` arg-parsing + I/O (the
//! `AlloyProvider`-based verify pipeline, whose closures capture on-chain RPC
//! reads + `PyResult`) and delegates to this module, mapping [`VerifyError`]
//! to `PyRuntimeError`.
//!
//! ## What's pure here
//! - [`SnapshotStore<K>`] — a one-way tick-data transfer store
//!   (`load`/`take`/`begin_load`/`insert`). `insert` returns [`VerifyError`]
//!   (no `PyResult`).
//! - [`register_with_cl_buffers`] — the CL pool-registration orchestration:
//!   register → apply backfill buffer → capture backfill-boundary snapshot →
//!   apply pump buffer, all under a single engine-lock acquisition (so the pump
//!   cannot race between registration and verification). Pure: takes closures
//!   for the engine-specific bits + an `Arc<Mutex<UniswapEngine>>`.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::bot_core::TickInfo;
use crate::optimizers::uniswap_engine::{PoolTickCoverage, UniswapEngine};

/// A verify-pipeline error — the non-pyo3 analogue of `PyRuntimeError`.
///
/// `py_binding.rs` maps this to `pyo3::exceptions::PyRuntimeError` at the seam.
#[derive(Debug)]
pub(crate) enum VerifyError {
    /// `insert()` was called with no snapshot stream in progress.
    NoSnapshotStream,
}

/// Snapshot storage keyed by pool identifier (V3 address or V4 pool manager + pool ID).
///
/// Holds a one-way transfer of tick data: `load()` replaces the store, and
/// `take()` removes a single pool's data at registration time. Streaming loads
/// begin with `begin_load()` and are populated via `insert()`.
///
/// Lifted verbatim from `py_binding.rs` (ADR-006 slice 5b); the only change is
/// `insert` returns [`VerifyError`] instead of `PyResult`.
pub(crate) struct SnapshotStore<K: Eq + std::hash::Hash> {
    data: Mutex<Option<HashMap<K, HashMap<i32, TickInfo>>>>,
}

impl<K: Eq + std::hash::Hash + Clone> SnapshotStore<K> {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            data: Mutex::new(None),
        }
    }

    #[must_use]
    pub(crate) fn is_loaded(&self) -> bool {
        self.data.lock().is_some()
    }

    pub(crate) fn load(&self, data: HashMap<K, HashMap<i32, TickInfo>>) {
        *self.data.lock() = Some(data);
    }

    pub(crate) fn begin_load(&self) {
        *self.data.lock() = Some(HashMap::new());
    }

    /// Insert a pool's `tick_data` into the in-progress stream.
    ///
    /// # Errors
    /// [`VerifyError::NoSnapshotStream`] if `begin_load`/`load` hasn't been called.
    pub(crate) fn insert(
        &self,
        key: K,
        tick_data: HashMap<i32, TickInfo>,
    ) -> Result<(), VerifyError> {
        let mut guard = self.data.lock();
        let Some(ref mut map) = *guard else {
            return Err(VerifyError::NoSnapshotStream);
        };
        map.insert(key, tick_data);
        Ok(())
    }

    /// Remove a single pool's tick data from the store.
    ///
    /// Returns `Tracked` coverage if the key existed, otherwise `Sparse`.
    pub(crate) fn take(&self, key: &K) -> (HashMap<i32, TickInfo>, PoolTickCoverage) {
        let mut guard = self.data.lock();
        if let Some(ref mut map) = *guard {
            if let Some(tick_data) = map.remove(key) {
                return (tick_data, PoolTickCoverage::Tracked);
            }
        }
        (HashMap::new(), PoolTickCoverage::Sparse)
    }

    pub(crate) fn clear(&self) {
        *self.data.lock() = None;
    }
}

impl<K: Eq + std::hash::Hash + Clone> Default for SnapshotStore<K> {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper that registers a CL pool, applies backfill/pump buffers, and captures
/// a backfill-boundary snapshot while the engine lock is held.
///
/// The lock ordering is load-bearing: `register` + `apply_backfill` +
/// `take_backfill_snapshot` + `apply_pump` run under a single lock acquisition
/// so the WS pump cannot interleave a `dispatch_log`/`dispatch_reorg_log`
/// between registration and the post-backfill state capture (the
/// [`crate::bot_core::rust_owned_bot`] §16 register-verify race's structural
/// closure).
///
/// Returns the registration key + the captured backfill-boundary snapshot.
#[allow(clippy::type_complexity)] // matches the upstream signature's closure tuple
pub(crate) fn register_with_cl_buffers<Key, BackfillSnapshot>(
    engine: &Arc<Mutex<UniswapEngine>>,
    register: impl FnOnce(&mut UniswapEngine) -> Key,
    apply_backfill: impl FnOnce(&mut UniswapEngine),
    take_backfill_snapshot: impl FnOnce(&UniswapEngine, &Key) -> Option<BackfillSnapshot>,
    apply_pump: impl FnOnce(&mut UniswapEngine),
) -> (Key, Option<BackfillSnapshot>) {
    let mut engine = engine.lock();
    let key = register(&mut engine);
    apply_backfill(&mut engine);
    let backfill_snapshot = take_backfill_snapshot(&engine, &key);
    apply_pump(&mut engine);
    (key, backfill_snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// ADR-006 slice 5b RED→GREEN: `register_with_cl_buffers` runs register →
    /// `apply_backfill` → `take_backfill_snapshot` → `apply_pump` in ORDER under one
    /// lock acquisition, returning the key + the captured snapshot. (The
    /// ordering is load-bearing for the no-race guarantee.)
    #[test]
    fn register_with_cl_buffers_runs_in_order_under_one_lock() {
        let engine = Arc::new(Mutex::new(UniswapEngine::new()));
        let order = Arc::new(AtomicUsize::new(0));
        let snap_calls = Arc::new(AtomicUsize::new(0));

        let key = 42u8;
        let (returned_key, returned_snap) = register_with_cl_buffers(
            &engine,
            {
                let order = Arc::clone(&order);
                move |_| {
                    assert_eq!(
                        order.fetch_add(1, Ordering::SeqCst),
                        0,
                        "register runs first"
                    );
                    key
                }
            },
            {
                let order = Arc::clone(&order);
                move |_| {
                    assert_eq!(
                        order.fetch_add(1, Ordering::SeqCst),
                        1,
                        "apply_backfill runs second"
                    );
                }
            },
            {
                let snap_calls = Arc::clone(&snap_calls);
                let order = Arc::clone(&order);
                move |_engine, k: &u8| {
                    snap_calls.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(*k, key, "snapshot key matches the registered key");
                    assert_eq!(
                        order.fetch_add(1, Ordering::SeqCst),
                        2,
                        "snapshot capture runs third"
                    );
                    Some(*k)
                }
            },
            {
                let order = Arc::clone(&order);
                move |_| {
                    assert_eq!(
                        order.fetch_add(1, Ordering::SeqCst),
                        3,
                        "apply_pump runs fourth"
                    );
                }
            },
        );

        assert_eq!(returned_key, key);
        assert_eq!(returned_snap, Some(key));
        assert_eq!(
            snap_calls.load(Ordering::SeqCst),
            1,
            "snapshot taken exactly once"
        );
        assert_eq!(
            order.load(Ordering::SeqCst),
            4,
            "all four phases ran exactly once"
        );
    }

    /// ADR-006 slice 5b: `SnapshotStore` is a one-way transfer — `load` then
    /// `take` returns the data + `Tracked`; a second `take` is empty + `Sparse`.
    #[test]
    fn snapshot_store_one_way_transfer() {
        let store: SnapshotStore<String> = SnapshotStore::new();
        assert!(!store.is_loaded());
        let mut ticks = HashMap::new();
        ticks.insert(
            -60,
            TickInfo {
                liquidity_gross: alloy::primitives::U128::ZERO,
                liquidity_net: alloy::primitives::I256::ZERO,
            },
        );
        let mut data = HashMap::new();
        data.insert("0xpool".to_string(), ticks);
        store.load(data);
        assert!(store.is_loaded());

        let (tick_data, coverage) = store.take(&"0xpool".to_string());
        assert!(matches!(coverage, PoolTickCoverage::Tracked));
        assert!(tick_data.contains_key(&-60));

        // Second take — already consumed.
        let (tick_data, coverage) = store.take(&"0xpool".to_string());
        assert!(matches!(coverage, PoolTickCoverage::Sparse));
        assert!(tick_data.is_empty());
    }

    /// ADR-006 slice 5b: `insert` without `begin_load`/`load` →
    /// `NoSnapshotStream` (not a panic).
    #[test]
    fn snapshot_store_insert_without_stream_errors() {
        let store: SnapshotStore<u64> = SnapshotStore::new();
        let res = store.insert(1, HashMap::new());
        assert!(matches!(res, Err(VerifyError::NoSnapshotStream)));

        store.begin_load();
        assert!(store.insert(1, HashMap::new()).is_ok());

        // Reset + clear returns to the no-stream state.
        store.clear();
        assert!(!store.is_loaded());
        assert!(matches!(
            store.insert(2, HashMap::new()),
            Err(VerifyError::NoSnapshotStream)
        ));
    }
}
