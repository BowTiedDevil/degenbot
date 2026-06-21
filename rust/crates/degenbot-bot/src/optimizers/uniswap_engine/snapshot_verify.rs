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

use alloy::primitives::Address;
use parking_lot::Mutex;

use crate::bot_core::{TickInfo, V3PoolState, V4PoolState};
use crate::optimizers::uniswap_engine::{PoolTickCoverage, UniswapEngine};

/// A verify-pipeline error — the non-pyo3 analogue of `PyRuntimeError`.
///
/// `py_binding.rs` maps this to typed Python exceptions at the seam
/// (`map_verify_err`):
/// - `Snapshot` → `VerificationMismatchError` (genuine mismatch — fatal)
/// - `Provider` → `VerificationRpcError` (verify-provider construction failure)
/// - `Rpc` → `VerificationRpcError` (per-call RPC transport failure — VP42BP)
/// - `NoSnapshotStream` → `PyRuntimeError` (programmer error)
///
/// `Rpc` and `Provider` both surface as `VerificationRpcError` but are kept
/// distinct here so the *orchestrator* (`run_cl_verification`) preserves the
/// per-call-transport category through the pure layer (a future retry/backoff
/// policy can branch on it). The Python seam folds both into the same
/// retryable exception type (VP42BP).
#[derive(Debug)]
pub enum VerifyError {
    /// `insert()` was called with no snapshot stream in progress.
    NoSnapshotStream,
    /// A verify-phase mismatch (snapshot block or backfill block). The message
    /// carries the phase + pool label + mismatch detail (mirrors the on-chain
    /// `VerificationMismatch` text the `py_binding` impl formats at the seam).
    Snapshot(String),
    /// A verify-provider construction failure (RPC init / connection error).
    Provider(String),
    /// A per-call RPC transport failure (an `eth_call` / `getTickBitmap` /
    /// `getTickLiquidity` read failed to reach the node). Transient — a
    /// retry/backoff candidate, NOT a genuine on-chain mismatch. Distinct from
    /// `Snapshot` (fatal) so the seam routes it to `VerificationRpcError`
    /// instead of `VerificationMismatchError` (VP42BP).
    Rpc(String),
}

/// The minimal on-chain RPC surface the two-phase CL verification needs
/// (ADR-006 slice-5 candidate-2 completion).
///
/// The pure orchestrator [`run_cl_verification`] drives this trait without
/// knowing `AlloyProvider`, `tokio`, or `PyResult`. The single concrete impl
/// lives in `py_binding.rs`: it holds a cached `AlloyProvider`, lazily
/// constructs it from the configured `rpc_url`, `block_on`s the underlying
/// `crate::bot_core::liquidity_verifier` calls, and maps
/// `VerificationMismatch` → [`VerifyError::Snapshot`].
///
/// Methods are **sync** (the impl blocks internally) per the slice-5 decision:
/// keeping `async` out of the pure layer keeps `snapshot_verify.rs` tokio-free
/// at the type level.
pub trait VerifyRpc {
    /// Is on-chain verification configured (`rpc_url` set)? When `false`, the
    /// orchestrator skips both phases — the pure control-flow equivalent of
    /// the legacy `rpc_url: None` early return.
    fn enabled(&self) -> bool;

    /// Snapshot phase — verify a V3 pool's raw tick data (pre-buffer) against
    /// on-chain at `block`.
    fn verify_v3_snapshot(
        &self,
        pool_address: Address,
        tick_data: &HashMap<i32, TickInfo>,
        block: u64,
    ) -> Result<(), VerifyError>;

    /// Backfill phase — verify a V3 pool's post-buffer state against on-chain
    /// at `block`.
    fn verify_v3_backfill(
        &self,
        pools: &HashMap<u64, V3PoolState>,
        block: u64,
    ) -> Result<(), VerifyError>;

    /// Snapshot phase — verify a V4 pool's raw tick data (pre-buffer) against
    /// on-chain at `block`.
    fn verify_v4_snapshot(
        &self,
        state_view: Address,
        pool_id: [u8; 32],
        tick_data: &HashMap<i32, TickInfo>,
        block: u64,
    ) -> Result<(), VerifyError>;

    /// Backfill phase — verify a V4 pool's post-buffer state against on-chain
    /// at `block`.
    fn verify_v4_backfill(
        &self,
        state_view: Address,
        pools: &HashMap<u64, V4PoolState>,
        block: u64,
    ) -> Result<(), VerifyError>;
}

/// Run the two-phase CL verification (snapshot block + backfill block) if
/// configured, in order.
///
/// Lifted verbatim-in-shape from `py_binding.rs` (ADR-006 slice-5 candidate-2):
/// the CORE control flow is pure — decide whether to run (via
/// [`VerifyRpc::enabled`]) → run the snapshot-verify phase → run the
/// backfill-verify phase, in order. Only the `AlloyProvider`/`PyResult` I/O
/// stays behind in `py_binding.rs` (inside the `VerifyRpc` impl).
///
/// The per-call-site captured data (pool address, snapshot `tick_data`,
/// backfill pool map) is threaded through `verify_snapshot`/`verify_backfill`
/// closures that shrink to delegate to the trait — so the orchestrator stays
/// family-agnostic (one fn serves both the V3 and V4 register paths).
///
/// **Order is load-bearing**: the snapshot phase must run before the backfill
/// phase (a snapshot mismatch short-circuits before touching the post-buffer
/// state). Preserves the legacy `RuntimeError`-on-mismatch contract: a
/// `VerifyError::Snapshot` from either phase propagates immediately.
pub fn run_cl_verification<R, F1, F2>(
    rpc: &R,
    snapshot_block: Option<u64>,
    backfill_block: Option<u64>,
    verify_snapshot: F1,
    verify_backfill: F2,
) -> Result<(), VerifyError>
where
    R: VerifyRpc,
    F1: FnOnce(&R, u64) -> Result<(), VerifyError>,
    F2: FnOnce(&R, u64) -> Result<(), VerifyError>,
{
    if !rpc.enabled() {
        return Ok(());
    }
    if let Some(block) = snapshot_block {
        verify_snapshot(rpc, block)?;
    }
    if let Some(block) = backfill_block {
        verify_backfill(rpc, block)?;
    }
    Ok(())
}

/// Snapshot storage keyed by pool identifier (V3 address or V4 pool manager + pool ID).
///
/// Holds a one-way transfer of tick data: `load()` replaces the store, and
/// `take()` removes a single pool's data at registration time. Streaming loads
/// begin with `begin_load()` and are populated via `insert()`.
///
/// Lifted verbatim from `py_binding.rs` (ADR-006 slice 5b); the only change is
/// `insert` returns [`VerifyError`] instead of `PyResult`.
pub struct SnapshotStore<K: Eq + std::hash::Hash> {
    data: Mutex<Option<HashMap<K, HashMap<i32, TickInfo>>>>,
}

impl<K: Eq + std::hash::Hash + Clone> SnapshotStore<K> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            data: Mutex::new(None),
        }
    }

    #[must_use]
    pub fn is_loaded(&self) -> bool {
        self.data.lock().is_some()
    }

    pub fn load(&self, data: HashMap<K, HashMap<i32, TickInfo>>) {
        *self.data.lock() = Some(data);
    }

    pub fn begin_load(&self) {
        *self.data.lock() = Some(HashMap::new());
    }

    /// Insert a pool's `tick_data` into the in-progress stream.
    ///
    /// # Errors
    /// [`VerifyError::NoSnapshotStream`] if `begin_load`/`load` hasn't been called.
    pub fn insert(&self, key: K, tick_data: HashMap<i32, TickInfo>) -> Result<(), VerifyError> {
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
    pub fn take(&self, key: &K) -> (HashMap<i32, TickInfo>, PoolTickCoverage) {
        let mut guard = self.data.lock();
        if let Some(ref mut map) = *guard {
            if let Some(tick_data) = map.remove(key) {
                return (tick_data, PoolTickCoverage::Tracked);
            }
        }
        (HashMap::new(), PoolTickCoverage::Sparse)
    }

    pub fn clear(&self) {
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
pub fn register_with_cl_buffers<Key, BackfillSnapshot>(
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
                block: 0,
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

    // -----------------------------------------------------------------
    // run_cl_verification orchestrator (ADR-006 slice-5 candidate-2)
    // -----------------------------------------------------------------

    /// A fake `VerifyRpc` that records every call + can be made to fail at a
    /// chosen phase. No live RPC — drives the pure orchestrator's ordering,
    // early-return, and mismatch-propagation assertions.
    struct FakeVerifyRpc {
        enabled: bool,
        snapshot_calls: Mutex<Vec<u64>>,
        backfill_calls: Mutex<Vec<u64>>,
        /// `Some(err)` → the snapshot phase returns this error.
        fail_snapshot: Option<VerifyError>,
        /// `Some(err)` → the backfill phase returns this error.
        fail_backfill: Option<VerifyError>,
    }

    impl FakeVerifyRpc {
        fn new(enabled: bool) -> Self {
            Self {
                enabled,
                snapshot_calls: Mutex::new(Vec::new()),
                backfill_calls: Mutex::new(Vec::new()),
                fail_snapshot: None,
                fail_backfill: None,
            }
        }
    }

    impl VerifyRpc for FakeVerifyRpc {
        fn enabled(&self) -> bool {
            self.enabled
        }

        fn verify_v3_snapshot(
            &self,
            _pool_address: Address,
            _tick_data: &HashMap<i32, TickInfo>,
            block: u64,
        ) -> Result<(), VerifyError> {
            self.snapshot_calls.lock().push(block);
            match &self.fail_snapshot {
                Some(e) => Err(clone_err(e)),
                None => Ok(()),
            }
        }

        fn verify_v3_backfill(
            &self,
            _pools: &HashMap<u64, V3PoolState>,
            block: u64,
        ) -> Result<(), VerifyError> {
            self.backfill_calls.lock().push(block);
            match &self.fail_backfill {
                Some(e) => Err(clone_err(e)),
                None => Ok(()),
            }
        }

        fn verify_v4_snapshot(
            &self,
            _state_view: Address,
            _pool_id: [u8; 32],
            _tick_data: &HashMap<i32, TickInfo>,
            block: u64,
        ) -> Result<(), VerifyError> {
            self.snapshot_calls.lock().push(block);
            match &self.fail_snapshot {
                Some(e) => Err(clone_err(e)),
                None => Ok(()),
            }
        }

        fn verify_v4_backfill(
            &self,
            _state_view: Address,
            _pools: &HashMap<u64, V4PoolState>,
            block: u64,
        ) -> Result<(), VerifyError> {
            self.backfill_calls.lock().push(block);
            match &self.fail_backfill {
                Some(e) => Err(clone_err(e)),
                None => Ok(()),
            }
        }
    }

    /// `VerifyError` isn't `Clone`; the fakes need to replay a canned error.
    fn clone_err(e: &VerifyError) -> VerifyError {
        match e {
            VerifyError::NoSnapshotStream => VerifyError::NoSnapshotStream,
            VerifyError::Snapshot(m) => VerifyError::Snapshot(m.clone()),
            VerifyError::Provider(m) => VerifyError::Provider(m.clone()),
            VerifyError::Rpc(m) => VerifyError::Rpc(m.clone()),
        }
    }

    /// `enabled() == false` short-circuits both phases (the legacy
    /// `rpc_url: None` early return).
    #[test]
    fn run_cl_verification_skips_when_disabled() {
        let rpc = FakeVerifyRpc::new(false);
        let snap = |_: &FakeVerifyRpc, b: u64| {
            panic!("snapshot phase must not run when disabled (block {b})")
        };
        let back = |_: &FakeVerifyRpc, b: u64| {
            panic!("backfill phase must not run when disabled (block {b})")
        };
        let res = run_cl_verification(&rpc, Some(10), Some(20), snap, back);
        assert!(res.is_ok());
        assert!(rpc.snapshot_calls.lock().is_empty());
        assert!(rpc.backfill_calls.lock().is_empty());
    }

    /// Both phases run in order: snapshot at `snapshot_block`, then backfill at
    /// `backfill_block`.
    #[test]
    fn run_cl_verification_runs_snapshot_then_backfill_in_order() {
        let rpc = FakeVerifyRpc::new(true);
        let order = Arc::new(AtomicUsize::new(0));
        let snap_order = Arc::clone(&order);
        let back_order = Arc::clone(&order);

        let snap = move |_: &FakeVerifyRpc, b: u64| {
            assert_eq!(
                snap_order.fetch_add(1, Ordering::SeqCst),
                0,
                "snapshot first"
            );
            assert_eq!(b, 10, "snapshot block");
            Ok(())
        };
        let back = move |_: &FakeVerifyRpc, b: u64| {
            assert_eq!(
                back_order.fetch_add(1, Ordering::SeqCst),
                1,
                "backfill second"
            );
            assert_eq!(b, 20, "backfill block");
            Ok(())
        };

        let res = run_cl_verification(&rpc, Some(10), Some(20), snap, back);
        assert!(res.is_ok());
        assert_eq!(
            order.load(Ordering::SeqCst),
            2,
            "both phases ran exactly once"
        );
    }

    /// A snapshot-phase mismatch short-circuits before the backfill phase
    /// (order is load-bearing — a snapshot mismatch must NOT touch post-buffer
    /// state).
    #[test]
    fn run_cl_verification_snapshot_mismatch_short_circuits() {
        let mut rpc = FakeVerifyRpc::new(true);
        rpc.fail_snapshot = Some(VerifyError::Snapshot(
            "V3 pool 0x.. at snapshot block 10: tick data mismatch: lg=1 vs 2".to_string(),
        ));

        let snap =
            |r: &FakeVerifyRpc, b: u64| r.verify_v3_snapshot(Address::ZERO, &HashMap::new(), b);
        let back = |_: &FakeVerifyRpc, b: u64| {
            panic!("backfill must not run after a snapshot mismatch (block {b})")
        };

        let res = run_cl_verification(&rpc, Some(10), Some(20), snap, back);
        assert!(matches!(
            res,
            Err(VerifyError::Snapshot(m)) if m.contains("snapshot block 10")
        ));
        assert_eq!(*rpc.snapshot_calls.lock(), vec![10]);
        assert!(rpc.backfill_calls.lock().is_empty(), "backfill skipped");
    }

    /// A backfill-phase mismatch propagates (snapshot phase ran first + ok).
    #[test]
    fn run_cl_verification_backfill_mismatch_propagates() {
        let mut rpc = FakeVerifyRpc::new(true);
        rpc.fail_backfill = Some(VerifyError::Snapshot(
            "V3 pool 0x.. at backfill block 20: tick data mismatch".to_string(),
        ));

        let snap =
            |r: &FakeVerifyRpc, b: u64| r.verify_v3_snapshot(Address::ZERO, &HashMap::new(), b);
        let back = |r: &FakeVerifyRpc, b: u64| r.verify_v3_backfill(&HashMap::new(), b);

        let res = run_cl_verification(&rpc, Some(10), Some(20), snap, back);
        assert!(matches!(
            res,
            Err(VerifyError::Snapshot(m)) if m.contains("backfill block 20")
        ));
        assert_eq!(*rpc.snapshot_calls.lock(), vec![10]);
        assert_eq!(*rpc.backfill_calls.lock(), vec![20]);
    }

    /// A provider construction failure from the snapshot phase propagates as
    /// `VerifyError::Provider` and still short-circuits the backfill phase.
    #[test]
    fn run_cl_verification_provider_error_short_circuits() {
        let mut rpc = FakeVerifyRpc::new(true);
        rpc.fail_snapshot = Some(VerifyError::Provider(
            "verify: 0x..: failed to create provider: connection refused".to_string(),
        ));

        let snap =
            |r: &FakeVerifyRpc, b: u64| r.verify_v3_snapshot(Address::ZERO, &HashMap::new(), b);
        let back = |_: &FakeVerifyRpc, b: u64| {
            panic!("backfill must not run after provider failure (block {b})")
        };

        let res = run_cl_verification(&rpc, Some(10), Some(20), snap, back);
        assert!(matches!(res, Err(VerifyError::Provider(_))));
        assert!(rpc.backfill_calls.lock().is_empty());
    }

    /// VP42BP: a per-call RPC transport failure from the snapshot phase
    /// propagates as `VerifyError::Rpc` — NOT flattened to `Snapshot` (which
    /// would conflate a transient transport error with a genuine mismatch and
    /// surface as `VerificationMismatchError` at the Python seam). The
    /// orchestrator must preserve the variant so `map_verify_err` can route it
    /// to `VerificationRpcError` (retryable) distinct from
    /// `VerificationMismatchError` (fatal).
    #[test]
    fn run_cl_verification_rpc_error_propagates_unflattened() {
        let mut rpc = FakeVerifyRpc::new(true);
        rpc.fail_snapshot = Some(VerifyError::Rpc(
            "V3 pool 0x.. at snapshot block 10: tickBitmap(0) RPC call failed: timeout".to_string(),
        ));

        let snap =
            |r: &FakeVerifyRpc, b: u64| r.verify_v3_snapshot(Address::ZERO, &HashMap::new(), b);
        let back = |_: &FakeVerifyRpc, b: u64| {
            panic!("backfill must not run after a snapshot RPC failure (block {b})")
        };

        let res = run_cl_verification(&rpc, Some(10), Some(20), snap, back);
        assert!(
            matches!(res, Err(VerifyError::Rpc(m)) if m.contains("RPC call failed")),
            "per-call RPC transport failure must surface as VerifyError::Rpc, not Snapshot"
        );
        assert_eq!(*rpc.snapshot_calls.lock(), vec![10]);
        assert!(
            rpc.backfill_calls.lock().is_empty(),
            "backfill skipped after snapshot RPC failure"
        );
    }

    /// VP42BP: a per-call RPC transport failure from the backfill phase
    /// propagates as `VerifyError::Rpc` (snapshot phase ran first + ok).
    #[test]
    fn run_cl_verification_backfill_rpc_error_propagates_unflattened() {
        let mut rpc = FakeVerifyRpc::new(true);
        rpc.fail_backfill = Some(VerifyError::Rpc(
            "V3 pool 0x.. at backfill block 20: ticks(100) RPC call failed: connection reset"
                .to_string(),
        ));

        let snap =
            |r: &FakeVerifyRpc, b: u64| r.verify_v3_snapshot(Address::ZERO, &HashMap::new(), b);
        let back = |r: &FakeVerifyRpc, b: u64| r.verify_v3_backfill(&HashMap::new(), b);

        let res = run_cl_verification(&rpc, Some(10), Some(20), snap, back);
        assert!(
            matches!(res, Err(VerifyError::Rpc(m)) if m.contains("backfill block 20") && m.contains("RPC call failed")),
            "backfill-phase RPC failure must surface as VerifyError::Rpc, not Snapshot"
        );
        assert_eq!(*rpc.snapshot_calls.lock(), vec![10]);
        assert_eq!(*rpc.backfill_calls.lock(), vec![20]);
    }

    /// Missing snapshot block is a no-op for the snapshot phase but the
    /// backfill phase still runs (mirrors the legacy per-`Option<u64>` guards).
    #[test]
    fn run_cl_verification_missing_snapshot_block_still_runs_backfill() {
        let rpc = FakeVerifyRpc::new(true);
        let snap = |_: &FakeVerifyRpc, b: u64| {
            panic!("snapshot phase must not run when its block is None (block {b})")
        };
        let back = |r: &FakeVerifyRpc, b: u64| r.verify_v3_backfill(&HashMap::new(), b);

        let res = run_cl_verification(&rpc, None, Some(20), snap, back);
        assert!(res.is_ok());
        assert!(rpc.snapshot_calls.lock().is_empty());
        assert_eq!(*rpc.backfill_calls.lock(), vec![20]);
    }
}
