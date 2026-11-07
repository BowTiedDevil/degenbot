//! Tests for the runtime module.

use super::*;
#[cfg(feature = "llvm")]
use alloy_primitives::B256;
use alloy_primitives::Bytes;
use evm2::SpecId;
use std::sync::Arc;
#[cfg(feature = "llvm")]
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// Test bytecodes.
// ---------------------------------------------------------------------------

/// PUSH1 0x42 PUSH0 MSTORE PUSH1 0x20 PUSH0 RETURN — returns 0x42.
const BYTECODE_RET42: &[u8] = &[0x60, 0x42, 0x5f, 0x52, 0x60, 0x20, 0x5f, 0xf3];

/// PUSH1 1 PUSH1 1 ADD PUSH0 MSTORE PUSH1 0x20 PUSH0 RETURN — returns 2.
const BYTECODE_ADD: &[u8] = &[0x60, 0x01, 0x60, 0x01, 0x01, 0x5f, 0x52, 0x60, 0x20, 0x5f, 0xf3];

/// Returns a simple bytecode that varies by index (for generating distinct code hashes).
#[cfg(feature = "llvm")]
fn indexed_bytecode(i: u8) -> Vec<u8> {
    vec![0x60, i, 0x5f, 0x52, 0x60, 0x20, 0x5f, 0xf3]
}

// ---------------------------------------------------------------------------
// Test harness.
// ---------------------------------------------------------------------------

/// Helper: polls `f` until it returns `Some(T)` or the deadline is reached.
fn poll_until<T>(timeout: std::time::Duration, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(std::time::Instant::now() < deadline, "timed out polling");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Test harness wrapping a [`JitBackend`] with convenience methods.
///
/// Automatically shuts down the backend on drop.
struct TestBackend {
    backend: JitBackend,
}

impl TestBackend {
    /// Creates a backend with the given config.
    fn new(config: RuntimeConfig) -> Self {
        Self { backend: JitBackend::new(config).unwrap() }
    }

    /// Creates a backend with `enabled: true` and the given tuning overrides.
    fn with_tuning(tuning: RuntimeTuning) -> Self {
        Self::new(RuntimeConfig { enabled: true, tuning, ..Default::default() })
    }

    /// Creates a backend with `enabled: false` and the given tuning overrides.
    fn disabled_with_tuning(tuning: RuntimeTuning) -> Self {
        Self::new(RuntimeConfig { enabled: false, tuning, ..Default::default() })
    }

    /// Creates a backend with `blocking: true` and the given tuning overrides.
    fn blocking_with_tuning(tuning: RuntimeTuning) -> Self {
        Self::new(RuntimeConfig { blocking: true, tuning, ..Default::default() })
    }

    /// Creates a backend with `enabled: true`, the given store, and the given tuning overrides.
    #[cfg(feature = "llvm")]
    fn with_store(store: Arc<dyn ArtifactStore>, tuning: RuntimeTuning) -> Self {
        Self::new(RuntimeConfig { enabled: true, store: Some(store), tuning, ..Default::default() })
    }

    /// Creates a backend with `enabled: true`, 1 worker, and the given tuning overrides.
    fn with_tuning_1w(tuning: RuntimeTuning) -> Self {
        Self::with_tuning(RuntimeTuning { jit_worker_count: 1, ..tuning })
    }

    /// Creates a backend with `enabled: true`, 1 worker, and default tuning.
    fn default_1w() -> Self {
        Self::with_tuning_1w(RuntimeTuning::default())
    }

    /// Builds a [`LookupRequest`] for the given bytecode and spec.
    fn req(bytecode: &[u8], spec_id: SpecId) -> LookupRequest {
        LookupRequest {
            key: RuntimeCacheKey { code_hash: alloy_primitives::keccak256(bytecode), spec_id },
            code: Bytes::copy_from_slice(bytecode),
        }
    }

    /// Builds a [`LookupRequest`] with [`SpecId::CANCUN`].
    fn req_cancun(bytecode: &[u8]) -> LookupRequest {
        Self::req(bytecode, SpecId::CANCUN)
    }

    /// Sends lookups until JIT promotion triggers and the program appears in the resident map.
    /// Returns the compiled program.
    #[cfg(feature = "llvm")]
    fn trigger_jit(&self, bytecode: &[u8], spec_id: SpecId) -> Arc<CompiledProgram> {
        for _ in 0..10 {
            let _ = self.backend.lookup(Self::req(bytecode, spec_id));
        }
        self.wait_compiled(bytecode, spec_id)
    }

    /// Like [`trigger_jit`](Self::trigger_jit) but with [`SpecId::CANCUN`].
    #[cfg(feature = "llvm")]
    fn trigger_jit_cancun(&self, bytecode: &[u8]) -> Arc<CompiledProgram> {
        self.trigger_jit(bytecode, SpecId::CANCUN)
    }

    /// Polls until the given bytecode is compiled and available in the resident map.
    #[cfg(feature = "llvm")]
    fn wait_compiled(&self, bytecode: &[u8], spec_id: SpecId) -> Arc<CompiledProgram> {
        poll_until(std::time::Duration::from_secs(30), || {
            match self.backend.lookup(Self::req(bytecode, spec_id)) {
                LookupDecision::Compiled(p) => Some(p),
                _ => None,
            }
        })
    }

    /// Polls until the resident map reaches the expected entry count.
    #[cfg(feature = "llvm")]
    fn wait_resident_count(&self, expected: u64) {
        poll_until(std::time::Duration::from_secs(5), || {
            let n = self.backend.stats().resident_entries;
            if n == expected { Some(()) } else { None }
        });
    }

    /// Polls until the stats snapshot satisfies the given predicate, then returns it.
    fn wait_stats(
        &self,
        mut pred: impl FnMut(&RuntimeStatsSnapshot) -> bool,
    ) -> RuntimeStatsSnapshot {
        poll_until(std::time::Duration::from_secs(5), || {
            let s = self.backend.stats();
            if pred(&s) { Some(s) } else { None }
        })
    }

    fn stats(&self) -> RuntimeStatsSnapshot {
        self.backend.stats()
    }
}

impl std::ops::Deref for TestBackend {
    type Target = JitBackend;
    fn deref(&self) -> &JitBackend {
        &self.backend
    }
}

// `JitBackend` shuts down automatically when the last `Arc<BackendInner>` drops.

// ---------------------------------------------------------------------------
// Artifact stores for testing.
// ---------------------------------------------------------------------------

/// A no-op artifact store that returns no artifacts.
struct EmptyStore;

impl ArtifactStore for EmptyStore {
    fn load_all(&self) -> eyre::Result<Vec<(ArtifactKey, StoredArtifact)>> {
        Ok(vec![])
    }

    fn load(&self, _key: &ArtifactKey) -> eyre::Result<Option<StoredArtifact>> {
        Ok(None)
    }

    fn store(
        &self,
        _key: &ArtifactKey,
        _manifest: &ArtifactManifest,
        _object_bytes: &[u8],
    ) -> eyre::Result<()> {
        Ok(())
    }

    fn delete(&self, _key: &ArtifactKey) -> eyre::Result<()> {
        Ok(())
    }

    fn clear(&self) -> eyre::Result<()> {
        Ok(())
    }
}

/// A store that fails on load_all.
struct FailingStore;

impl ArtifactStore for FailingStore {
    fn load_all(&self) -> eyre::Result<Vec<(ArtifactKey, StoredArtifact)>> {
        Err(std::io::Error::other("boom").into())
    }

    fn load(&self, _key: &ArtifactKey) -> eyre::Result<Option<StoredArtifact>> {
        Ok(None)
    }

    fn store(
        &self,
        _key: &ArtifactKey,
        _manifest: &ArtifactManifest,
        _object_bytes: &[u8],
    ) -> eyre::Result<()> {
        Ok(())
    }

    fn delete(&self, _key: &ArtifactKey) -> eyre::Result<()> {
        Ok(())
    }

    fn clear(&self) -> eyre::Result<()> {
        Ok(())
    }
}

/// A store that counts `clear` calls.
struct CountingStore {
    cleared: Arc<std::sync::atomic::AtomicUsize>,
}

impl ArtifactStore for CountingStore {
    fn load_all(&self) -> eyre::Result<Vec<(ArtifactKey, StoredArtifact)>> {
        Ok(vec![])
    }

    fn load(&self, _key: &ArtifactKey) -> eyre::Result<Option<StoredArtifact>> {
        Ok(None)
    }

    fn store(
        &self,
        _key: &ArtifactKey,
        _manifest: &ArtifactManifest,
        _object_bytes: &[u8],
    ) -> eyre::Result<()> {
        Ok(())
    }

    fn delete(&self, _key: &ArtifactKey) -> eyre::Result<()> {
        Ok(())
    }

    fn clear(&self) -> eyre::Result<()> {
        self.cleared.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

#[cfg(feature = "llvm")]
#[derive(Clone, Copy, Debug)]
enum AotFailure {
    Persist,
    ReloadMissing,
    ReloadError,
    Link,
    NoStore,
}

#[cfg(feature = "llvm")]
struct AotFailureStore {
    inner: RuntimeArtifactStore,
    failure: AotFailure,
}

#[cfg(feature = "llvm")]
impl ArtifactStore for AotFailureStore {
    fn load_all(&self) -> eyre::Result<Vec<(ArtifactKey, StoredArtifact)>> {
        self.inner.load_all()
    }

    fn load(&self, key: &ArtifactKey) -> eyre::Result<Option<StoredArtifact>> {
        let Some(mut stored) = self.inner.load(key)? else { return Ok(None) };
        match self.failure {
            AotFailure::ReloadMissing => return Ok(None),
            AotFailure::ReloadError => eyre::bail!("injected AOT reload failure"),
            // A malformed symbol makes linking fail without handing invalid object bytes to LLVM.
            AotFailure::Link => stored.manifest.symbol_name.push('\0'),
            _ => {}
        }
        Ok(Some(stored))
    }

    fn store(
        &self,
        key: &ArtifactKey,
        manifest: &ArtifactManifest,
        object_bytes: &[u8],
    ) -> eyre::Result<()> {
        if matches!(self.failure, AotFailure::Persist) {
            eyre::bail!("injected AOT persist failure");
        }
        self.inner.store(key, manifest, object_bytes)
    }

    fn delete(&self, key: &ArtifactKey) -> eyre::Result<()> {
        self.inner.delete(key)
    }

    fn clear(&self) -> eyre::Result<()> {
        self.inner.clear()
    }
}

#[cfg(feature = "llvm")]
static STALE_RESULT_GATE: Mutex<Option<(chan::Sender<()>, chan::Receiver<()>)>> = Mutex::new(None);

#[cfg(feature = "llvm")]
fn gated_builtin_address(builtin: evm2_jit_builtins::Builtin) -> usize {
    let gate = STALE_RESULT_GATE.lock().unwrap().take();
    if let Some((entered, release)) = gate {
        entered.send(()).unwrap();
        release.recv_timeout(std::time::Duration::from_secs(30)).unwrap();
    }
    builtin.addr::<evm2::BaseEvmTypes>()
}

// ===========================================================================
// Tests: startup / basic.
// ===========================================================================

#[test]
fn start_no_store() {
    let tb = TestBackend::new(RuntimeConfig::default());
    assert_eq!(tb.stats().resident_entries, 0);
}

#[test]
fn start_empty_store() {
    let config =
        RuntimeConfig { enabled: true, store: Some(Arc::new(EmptyStore)), ..Default::default() };
    let tb = TestBackend::new(config);
    assert_eq!(tb.stats().resident_entries, 0);
}

#[test]
fn startup_store_failure() {
    let config =
        RuntimeConfig { enabled: true, store: Some(Arc::new(FailingStore)), ..Default::default() };
    let result = JitBackend::new(config);
    assert!(result.is_err());
}

#[test]
fn aot_without_store_is_rejected() {
    let config = RuntimeConfig { enabled: true, aot: true, store: None, ..Default::default() };
    let error = JitBackend::new(config).unwrap_err();
    assert_eq!(error.to_string(), "AOT mode requires an artifact store");
}

#[test]
#[cfg(unix)]
fn aot_out_of_process_is_rejected() {
    let config = RuntimeConfig {
        enabled: true,
        aot: true,
        store: Some(Arc::new(EmptyStore)),
        jit_mode: JitMode::OutOfProcess,
        ..Default::default()
    };
    let error = JitBackend::new(config).unwrap_err();
    assert_eq!(error.to_string(), "AOT mode does not support out-of-process compilation");
}

// ===========================================================================
// Tests: lookup behavior.
// ===========================================================================

#[test]
fn zero_workers_fails_sync_compile_without_blocking() {
    let tb = TestBackend::blocking_with_tuning(RuntimeTuning {
        jit_worker_count: 0,
        ..Default::default()
    });

    let decision = tb.lookup(TestBackend::req_cancun(BYTECODE_RET42));
    assert!(matches!(decision, LookupDecision::Unavailable(LookupMissReason::JitFailed)));

    let stats = tb.stats();
    assert_eq!(stats.resident_entries, 0);
    assert_eq!(stats.compilations_dispatched, 0);
    assert_eq!(stats.interpreter_fallbacks, 1);
}

#[test]
fn blocking_pending_job_cap_counts_fallback() {
    let tb = TestBackend::blocking_with_tuning(RuntimeTuning {
        jit_max_pending_jobs: 0,
        ..Default::default()
    });

    let decision = tb.lookup(TestBackend::req_cancun(BYTECODE_RET42));
    assert!(matches!(decision, LookupDecision::Unavailable(LookupMissReason::JitFailed)));
    assert_eq!(tb.stats().interpreter_fallbacks, 1);
}

#[test]
#[cfg(feature = "llvm")]
fn blocking_sync_compile_timeout_counts_fallback() {
    // The callback runs before the waiting caller is notified, so the caller times out first.
    let tb = TestBackend::new(RuntimeConfig {
        blocking: true,
        tuning: RuntimeTuning {
            jit_worker_count: 1,
            sync_compile_timeout: std::time::Duration::from_millis(20),
            ..Default::default()
        },
        on_compilation: Some(Arc::new(|_| {
            std::thread::sleep(std::time::Duration::from_millis(500))
        })),
        ..Default::default()
    });

    let decision = tb.lookup(TestBackend::req_cancun(BYTECODE_RET42));
    assert!(matches!(decision, LookupDecision::Unavailable(LookupMissReason::JitFailed)));
    assert_eq!(tb.stats().interpreter_fallbacks, 1);
}

#[test]
#[cfg(feature = "llvm")]
fn blocking_mode_compiles_bytecode_over_max_len() {
    let tb = TestBackend::blocking_with_tuning(RuntimeTuning {
        jit_worker_count: 1,
        jit_max_bytecode_len: 4,
        ..Default::default()
    });

    let decision = tb.lookup(TestBackend::req_cancun(BYTECODE_RET42));
    assert!(matches!(decision, LookupDecision::Compiled(_)));
    assert_eq!(tb.stats().interpreter_fallbacks, 0);
}

#[test]
fn lookup_disabled() {
    let tb = TestBackend::disabled_with_tuning(Default::default());

    let req = TestBackend::req_cancun(&[0x00]);
    let decision = tb.lookup(req);
    assert!(matches!(decision, LookupDecision::Unavailable(LookupMissReason::Disabled)));

    let stats = tb.stats();
    assert_eq!(stats.lookup_hits, 0);
    assert_eq!(stats.lookup_misses, 0);
}

#[test]
fn lookup_miss_when_enabled() {
    let tb = TestBackend::with_tuning(Default::default());

    let decision = tb.lookup(TestBackend::req_cancun(&[0x00]));
    assert!(matches!(decision, LookupDecision::Unavailable(LookupMissReason::NotReady)));

    let stats = tb.wait_stats(|s| s.lookup_misses == 1);
    assert_eq!(stats.lookup_misses, 1);
    assert_eq!(stats.lookup_hits, 0);
}

#[test]
fn set_enabled_toggle() {
    let tb = TestBackend::disabled_with_tuning(Default::default());
    let req = TestBackend::req_cancun(&[0x00]);

    assert!(matches!(
        tb.lookup(req.clone()),
        LookupDecision::Unavailable(LookupMissReason::Disabled)
    ));

    tb.set_enabled(true).unwrap();
    assert!(matches!(
        tb.lookup(req.clone()),
        LookupDecision::Unavailable(LookupMissReason::NotReady)
    ));

    tb.set_enabled(false).unwrap();
    assert!(matches!(tb.lookup(req), LookupDecision::Unavailable(LookupMissReason::Disabled)));
}

#[test]
fn pause_processes_lookup_events() {
    let tb = TestBackend::with_tuning(RuntimeTuning { jit_worker_count: 0, ..Default::default() });
    let req = TestBackend::req_cancun(&[0x00]);

    assert!(!tb.is_paused());
    tb.pause();
    tb.pause();
    assert!(tb.is_paused());
    assert!(matches!(
        tb.lookup(req.clone()),
        LookupDecision::Unavailable(LookupMissReason::NotReady)
    ));

    let stats = tb.wait_stats(|s| s.lookup_misses == 1);
    assert_eq!(stats.lookup_misses, 1);
    assert_eq!(stats.lookup_hits, 0);

    tb.resume();
    assert!(tb.is_paused());
    tb.resume();
    assert!(!tb.is_paused());
    assert!(matches!(tb.lookup(req), LookupDecision::Unavailable(LookupMissReason::NotReady)));

    let stats = tb.wait_stats(|s| s.lookup_misses == 2);
    assert_eq!(stats.lookup_misses, 2);
    assert_eq!(stats.lookup_hits, 0);
}

#[test]
fn lookup_increments_miss_counter() {
    let tb = TestBackend::with_tuning(Default::default());

    for _ in 0..10 {
        let _ = tb.lookup(TestBackend::req_cancun(BYTECODE_RET42));
    }

    // All 10 lookups missed (no compiled program), so misses == 10.
    let stats = tb.wait_stats(|s| s.lookup_misses == 10);
    assert_eq!(stats.lookup_misses, 10);
    assert_eq!(stats.lookup_hits, 0);
}

#[test]
fn drop_shuts_down_backend() {
    let backend = JitBackend::new(RuntimeConfig::default()).unwrap();
    let backend2 = backend.clone();
    drop(backend);

    // Lookups still work (no panic) — backend is still running because backend2 holds a ref.
    let _ = backend2.lookup(TestBackend::req_cancun(&[]));
}

#[test]
fn backend_clone() {
    let tb = TestBackend::with_tuning(Default::default());
    let b2 = tb.backend.clone();

    let _ = tb.lookup(TestBackend::req_cancun(&[0x00]));
    let _ = b2.lookup(TestBackend::req_cancun(&[0x00]));

    let stats = tb.wait_stats(|s| s.lookup_misses == 2);
    assert_eq!(stats.lookup_misses, 2);
}

// ===========================================================================
// Tests: enqueue APIs (fire-and-forget, no LLVM needed).
// ===========================================================================

#[test]
fn compile_jit_enqueue() {
    let tb = TestBackend::with_tuning(RuntimeTuning { jit_worker_count: 0, ..Default::default() });
    tb.compile_jit(TestBackend::req_cancun(BYTECODE_RET42));
}

#[test]
fn compile_jit_never_blocks_when_startup_fails() {
    // `ensure_started` fails (the store errors) and restores the lazy-spawn state, so the
    // receiver stays alive while nothing drains the bounded command channel. Enqueueing would
    // fill the channel after `channel_capacity` requests and park the caller forever, the same
    // way `pause_resume_never_block_without_backend_thread` describes for pause/resume.
    let tb = TestBackend::new(RuntimeConfig {
        enabled: false,
        store: Some(Arc::new(FailingStore)),
        tuning: RuntimeTuning { channel_capacity: 1, ..Default::default() },
        ..Default::default()
    });

    let backend = tb.backend.clone();
    let (done_tx, done_rx) = chan::bounded(1);
    let worker = std::thread::spawn(move || {
        // More requests than the channel can hold: deadlocks without the started check.
        for _ in 0..8 {
            backend.compile_jit(TestBackend::req_cancun(BYTECODE_RET42));
        }
        let _ = done_tx.send(());
    });

    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("compile_jit blocked after the backend failed to start");
    worker.join().unwrap();

    // Nothing was queued: the requests were dropped instead of filling the channel.
    assert_eq!(tb.stats().command_queue_len, 0);
    assert!(!tb.inner.started.load(Ordering::Relaxed));
    assert!(tb.inner.lazy_spawn.lock().unwrap().is_some());
}

#[test]
fn prepare_aot_enqueue() {
    let tb = TestBackend::with_tuning(RuntimeTuning { jit_worker_count: 0, ..Default::default() });
    tb.prepare_aot(AotRequest {
        code_hash: alloy_primitives::keccak256(BYTECODE_RET42),
        code: Bytes::from_static(BYTECODE_RET42),
        spec_id: SpecId::CANCUN,
    });
}

#[test]
fn prepare_aot_batch_enqueue() {
    let tb = TestBackend::with_tuning(RuntimeTuning { jit_worker_count: 0, ..Default::default() });
    let reqs = vec![
        AotRequest {
            code_hash: alloy_primitives::keccak256(BYTECODE_RET42),
            code: Bytes::from_static(BYTECODE_RET42),
            spec_id: SpecId::CANCUN,
        },
        AotRequest {
            code_hash: alloy_primitives::keccak256(BYTECODE_ADD),
            code: Bytes::from_static(BYTECODE_ADD),
            spec_id: SpecId::CANCUN,
        },
    ];
    tb.prepare_aot_batch(reqs);
}

#[test]
fn prepare_aot_batch_never_blocks_when_startup_fails() {
    // Same hazard as `compile_jit_never_blocks_when_startup_fails`, on the AOT path.
    let tb = TestBackend::new(RuntimeConfig {
        enabled: false,
        store: Some(Arc::new(FailingStore)),
        tuning: RuntimeTuning { channel_capacity: 1, ..Default::default() },
        ..Default::default()
    });

    let backend = tb.backend.clone();
    let (done_tx, done_rx) = chan::bounded(1);
    let worker = std::thread::spawn(move || {
        let reqs = vec![AotRequest {
            code_hash: alloy_primitives::keccak256(BYTECODE_RET42),
            code: Bytes::from_static(BYTECODE_RET42),
            spec_id: SpecId::CANCUN,
        }];
        // More requests than the channel can hold: deadlocks without the started check.
        for _ in 0..8 {
            backend.prepare_aot(reqs[0].clone());
            backend.prepare_aot_batch(reqs.clone());
        }
        let _ = done_tx.send(());
    });

    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("prepare_aot_batch blocked after the backend failed to start");
    worker.join().unwrap();

    // Nothing was queued: the requests were dropped instead of filling the channel.
    assert_eq!(tb.stats().command_queue_len, 0);
    assert!(!tb.inner.started.load(Ordering::Relaxed));
    assert!(tb.inner.lazy_spawn.lock().unwrap().is_some());
}

#[test]
fn clear_resident() {
    let tb = TestBackend::with_tuning(Default::default());
    tb.clear_resident();

    let decision = tb.lookup(TestBackend::req_cancun(&[0x00]));
    assert!(matches!(decision, LookupDecision::Unavailable(LookupMissReason::NotReady)));
}

#[test]
fn sub_threshold_misses_do_not_dispatch() {
    // Inline counters live on the resident entry; threshold-crossing dispatches
    // exactly one CompileJit. With a tiny command channel and threshold above
    // the call count, no dispatch occurs and lookups never block.
    let tb = TestBackend::with_tuning_1w(RuntimeTuning {
        channel_capacity: 2,
        jit_hot_threshold: 1000,
        ..Default::default()
    });

    for _ in 0..200 {
        let _ = tb.lookup(TestBackend::req_cancun(BYTECODE_RET42));
    }

    // Misses are tracked asynchronously by the backend, with overflow dropped.
    // The two together must add up to the total lookup count.
    let stats = tb.wait_stats(|s| s.lookup_misses + s.events_dropped == 200);
    assert_eq!(stats.lookup_misses + stats.events_dropped, 200);
    assert_eq!(stats.compilations_dispatched, 0);
}

#[test]
fn pause_resume_never_block_without_backend_thread() {
    // When the runtime is constructed but compilation is never enabled, the backend thread is
    // never spawned and nothing drains the bounded command channel. Pause/resume are issued once
    // per validated block on the engine's critical path, so queueing them would fill the channel
    // after `channel_capacity` sends and block the caller forever (observed in production as a
    // consensus-engine deadlock after exactly capacity/2 blocks). They must be skipped, not
    // queued, while the backend is not started.
    let tb = TestBackend::disabled_with_tuning(RuntimeTuning {
        channel_capacity: 2,
        ..Default::default()
    });

    // More pause/resume cycles than the channel can hold: deadlocks without the started check.
    for _ in 0..16 {
        tb.pause();
        assert!(tb.is_paused());
        tb.resume();
        assert!(!tb.is_paused());
    }

    // Nothing was queued or dropped: the sends were skipped entirely.
    let stats = tb.stats();
    assert_eq!(stats.command_queue_len, 0);
    assert_eq!(stats.commands_dropped, 0);
}

#[test]
fn clear_commands_never_block_without_backend_thread() {
    // The clears share the hazard of `pause_resume_never_block_without_backend_thread`: they are
    // called on a runtime whose backend thread may never have been spawned (`JitBackend::disabled`)
    // or failed to start, and nothing drains the bounded command channel in that state. Unlike
    // pause/resume the clears still have work to do, so they are applied in place rather than
    // skipped.
    let cleared = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tb = TestBackend::new(RuntimeConfig {
        enabled: false,
        store: Some(Arc::new(CountingStore { cleared: Arc::clone(&cleared) })),
        tuning: RuntimeTuning { channel_capacity: 2, ..Default::default() },
        ..Default::default()
    });

    let backend = tb.backend.clone();
    let (done_tx, done_rx) = chan::bounded(1);
    std::thread::spawn(move || {
        // More clears than the channel can hold: deadlocks without the started check.
        for _ in 0..16 {
            backend.clear_resident();
            backend.clear_persisted();
            backend.clear_all();
        }
        let _ = done_tx.send(());
    });

    done_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("clear commands blocked without a backend thread");

    // Nothing was queued: every clear was applied in place.
    let stats = tb.stats();
    assert_eq!(stats.command_queue_len, 0);
    assert_eq!(stats.commands_dropped, 0);
    // `clear_persisted` and `clear_all` each reached the store in every round.
    assert_eq!(cleared.load(std::sync::atomic::Ordering::Relaxed), 32);
}

#[test]
fn clear_commands_are_queued_after_startup_takes_config() {
    for (clear, command) in [
        (JitBackend::clear_resident as fn(&JitBackend), Command::ClearResident),
        (JitBackend::clear_persisted, Command::ClearPersisted),
        (JitBackend::clear_all, Command::ClearAll),
    ] {
        let tb = TestBackend::disabled_with_tuning(RuntimeTuning {
            channel_capacity: 1,
            ..Default::default()
        });
        let mut guard = tb.inner.lazy_spawn.lock().unwrap();
        let lazy = guard.take().unwrap();
        let backend = tb.backend.clone();
        let worker = std::thread::spawn(move || clear(&backend));
        drop(guard);

        let queued = lazy.rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(std::mem::discriminant(&queued), std::mem::discriminant(&command));
        worker.join().unwrap();
    }
}

#[test]
fn clear_commands_racing_with_startup_reach_store() {
    let cleared = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tb = TestBackend::new(RuntimeConfig {
        enabled: false,
        store: Some(Arc::new(CountingStore { cleared: Arc::clone(&cleared) })),
        tuning: RuntimeTuning { channel_capacity: 1, ..Default::default() },
        ..Default::default()
    });
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let backend = tb.backend.clone();
    let startup_barrier = Arc::clone(&barrier);
    let startup = std::thread::spawn(move || {
        startup_barrier.wait();
        backend.set_enabled(true).unwrap();
    });
    let backend = tb.backend.clone();
    let clearing_barrier = Arc::clone(&barrier);
    let (done_tx, done_rx) = chan::bounded(1);
    let clearing = std::thread::spawn(move || {
        clearing_barrier.wait();
        for _ in 0..16 {
            backend.clear_resident();
            backend.clear_persisted();
            backend.clear_all();
        }
        done_tx.send(()).unwrap();
    });
    barrier.wait();
    done_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    startup.join().unwrap();
    clearing.join().unwrap();
    poll_until(std::time::Duration::from_secs(5), || {
        (cleared.load(std::sync::atomic::Ordering::Relaxed) == 32).then_some(())
    });
    assert!(tb.enabled());
}

#[test]
fn clear_commands_never_block_after_startup_receiver_disconnects() {
    let tb = TestBackend::disabled_with_tuning(RuntimeTuning {
        channel_capacity: 1,
        ..Default::default()
    });
    drop(tb.inner.lazy_spawn.lock().unwrap().take().unwrap());
    let backend = tb.backend.clone();
    let (done_tx, done_rx) = chan::bounded(1);
    let worker = std::thread::spawn(move || {
        for _ in 0..16 {
            backend.clear_resident();
            backend.clear_persisted();
            backend.clear_all();
        }
        done_tx.send(()).unwrap();
    });
    done_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    worker.join().unwrap();
    assert_eq!(tb.stats().command_queue_len, 0);
}

#[test]
fn clear_commands_after_failed_spawn_reach_store_and_allow_retry() {
    let cleared = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tb = TestBackend::new(RuntimeConfig {
        enabled: false,
        store: Some(Arc::new(CountingStore { cleared: Arc::clone(&cleared) })),
        tuning: RuntimeTuning { channel_capacity: 1, ..Default::default() },
        ..Default::default()
    });
    assert!(
        tb.ensure_started_with(std::thread::Builder::new().stack_size(usize::MAX / 2)).is_err()
    );
    assert!(!tb.inner.started.load(Ordering::Relaxed));
    assert!(tb.inner.lazy_spawn.lock().unwrap().is_some());

    let backend = tb.backend.clone();
    let (done_tx, done_rx) = chan::bounded(1);
    let worker = std::thread::spawn(move || {
        for _ in 0..16 {
            backend.clear_resident();
            backend.clear_persisted();
            backend.clear_all();
        }
        done_tx.send(()).unwrap();
    });
    done_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    worker.join().unwrap();
    assert_eq!(cleared.load(std::sync::atomic::Ordering::Relaxed), 32);
    assert_eq!(tb.stats().command_queue_len, 0);

    tb.set_enabled(true).unwrap();
    assert!(tb.inner.started.load(Ordering::Relaxed));
    assert!(tb.enabled());
}

#[test]
fn pause_resume_depth_tracking_with_backend_thread() {
    // With the backend running, pause/resume still track depth and deliver commands.
    let tb = TestBackend::default_1w();

    tb.pause();
    tb.pause();
    assert!(tb.is_paused());
    tb.resume();
    assert!(tb.is_paused());
    tb.resume();
    assert!(!tb.is_paused());

    // Commands are drained by the backend thread rather than accumulating.
    let stats = tb.wait_stats(|s| s.command_queue_len == 0);
    assert_eq!(stats.command_queue_len, 0);
    assert_eq!(stats.commands_dropped, 0);
}

#[test]
#[cfg(all(unix, feature = "llvm"))]
fn blocking_out_of_process_pause_resume_does_not_stop_helper() {
    // The caller waits on compilation in blocking mode, so stopping the helper would hang it.
    let tb = TestBackend::new(RuntimeConfig {
        blocking: true,
        jit_mode: JitMode::OutOfProcess,
        jit_helper_path: Some("/nonexistent/evm2-jit-helper".into()),
        tuning: RuntimeTuning { jit_worker_count: 1, ..Default::default() },
        ..Default::default()
    });

    tb.pause();
    assert!(tb.is_paused());
    tb.resume();
    assert!(!tb.is_paused());

    // Shutdown is queued behind pause/resume and joins the backend thread, so every earlier
    // command has been handled once the backend is dropped.
    let shared = tb.backend.inner.shared.clone();
    drop(tb);
    let stats = shared.stats();
    assert_eq!(stats.jit_helper_pause_requests, 0);
    assert_eq!(stats.jit_helper_resume_requests, 0);
}

// ===========================================================================
// Tests: JIT compilation (require LLVM).
// ===========================================================================

#[test]
#[cfg(feature = "llvm")]
fn blocking_mode() {
    let tb = TestBackend::blocking_with_tuning(RuntimeTuning {
        jit_worker_count: 1,
        ..Default::default()
    });

    let req = || TestBackend::req_cancun(BYTECODE_RET42);

    // First lookup should block until JIT finishes, then return Compiled.
    let decision = tb.lookup(req());
    assert!(
        matches!(&decision, LookupDecision::Compiled(p) if p.kind == ProgramKind::Jit),
        "blocking mode should return Compiled on first lookup, got: {decision:?}",
    );

    // Second lookup should hit the resident map immediately.
    assert!(matches!(tb.lookup(req()), LookupDecision::Compiled(_)));

    // Empty bytecodes return Ineligible (nothing to compile).
    let decision = tb.lookup(TestBackend::req_cancun(&[]));
    assert!(matches!(decision, LookupDecision::Unavailable(LookupMissReason::Ineligible)));
    assert_eq!(tb.stats().interpreter_fallbacks, 1);
}

#[test]
fn default_jit_max_bytecode_len_matches_eth_limit() {
    let tuning = RuntimeTuning::default();
    let max_len_bytecode = vec![0; 24 * 1024];
    let too_large_bytecode = vec![0; 24 * 1024 + 1];

    assert_eq!(tuning.jit_max_bytecode_len, 24 * 1024);
    assert!(tuning.should_compile(&max_len_bytecode));
    assert!(!tuning.should_compile(&too_large_bytecode));
}

#[test]
#[cfg(feature = "llvm")]
fn jit_hotness_promotion() {
    let tb =
        TestBackend::with_tuning_1w(RuntimeTuning { jit_hot_threshold: 3, ..Default::default() });

    let req = || TestBackend::req_cancun(BYTECODE_RET42);

    // Below threshold: should remain NotReady.
    for _ in 0..2 {
        assert!(matches!(
            tb.lookup(req()),
            LookupDecision::Unavailable(LookupMissReason::NotReady)
        ));
    }

    // Hit threshold and beyond: JIT should eventually compile.
    let p = tb.trigger_jit_cancun(BYTECODE_RET42);
    assert_eq!(p.kind, ProgramKind::Jit);

    let stats = tb.wait_stats(|s| s.resident_entries >= 1 && s.lookup_hits >= 1);
    assert!(stats.resident_entries >= 1);
    assert!(stats.lookup_hits >= 1);
}

#[test]
#[cfg(feature = "llvm")]
fn jit_max_bytecode_len_prevents_promotion() {
    let tb = TestBackend::with_tuning_1w(RuntimeTuning {
        jit_hot_threshold: 1,
        jit_max_bytecode_len: 4,
        ..Default::default()
    });

    let req = || TestBackend::req_cancun(BYTECODE_RET42);

    // Send many lookups to exceed any threshold.
    for _ in 0..20 {
        let _ = tb.lookup(req());
    }

    // Give the backend time to process events.
    std::thread::sleep(std::time::Duration::from_millis(200));

    // Should be Ineligible — bytecode length exceeds jit_max_bytecode_len.
    assert!(matches!(tb.lookup(req()), LookupDecision::Unavailable(LookupMissReason::Ineligible)));
}

#[test]
#[cfg(all(unix, feature = "llvm"))]
fn failed_compile_is_not_retried_until_cleared() {
    let tb = TestBackend::new(RuntimeConfig {
        enabled: true,
        jit_mode: JitMode::OutOfProcess,
        jit_helper_path: Some("/nonexistent/evm2-jit-helper".into()),
        tuning: RuntimeTuning {
            jit_worker_count: 1,
            jit_hot_threshold: 1,
            idle_evict_duration: None,
            ..Default::default()
        },
        ..Default::default()
    });

    let lookup_rounds = || {
        for _ in 0..10 {
            let _ = tb.lookup(TestBackend::req_cancun(BYTECODE_RET42));
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    };

    // Every compile fails at helper spawn, but the key is only dispatched once.
    lookup_rounds();
    tb.wait_stats(|s| s.compilations_failed == 1);
    lookup_rounds();
    tb.compile_jit(TestBackend::req_cancun(BYTECODE_RET42));
    tb.compile_jit_sync(TestBackend::req_cancun(BYTECODE_RET42)).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    let stats = tb.stats();
    assert_eq!(stats.compilations_dispatched, 1);
    assert_eq!(stats.compilations_failed, 1);

    // Clearing the resident map forgets the failure.
    tb.clear_resident();
    lookup_rounds();
    let stats = tb.wait_stats(|s| s.compilations_failed == 2);
    assert_eq!(stats.compilations_dispatched, 2);
}

#[test]
#[cfg(feature = "llvm")]
fn clear_resident_discards_inflight_jit() {
    for clear in [JitBackend::clear_resident, JitBackend::clear_all] {
        let (entered_tx, entered_rx) = chan::bounded(1);
        let (release_tx, release_rx) = chan::bounded(1);
        *STALE_RESULT_GATE.lock().unwrap() = Some((entered_tx, release_rx));
        let tb = TestBackend::new(RuntimeConfig {
            enabled: true,
            // SAFETY: the resolver returns the base host's unchanged builtin addresses.
            host: unsafe {
                HostConfig::for_types::<evm2::BaseEvmTypes>().with_resolver(gated_builtin_address)
            },
            tuning: RuntimeTuning { jit_worker_count: 1, ..Default::default() },
            ..Default::default()
        });

        std::thread::scope(|scope| {
            let compiling = scope.spawn(|| {
                tb.compile_jit_sync(TestBackend::req_cancun(BYTECODE_RET42)).unwrap();
            });
            entered_rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap();
            clear(&tb);
            // An ineligible request completes without a worker. Its notification proves the
            // earlier clear command was processed, while the real compilation is still held.
            tb.compile_jit_sync(TestBackend::req_cancun(&[])).unwrap();
            assert_eq!(tb.stats().pending_jobs, 1);
            release_tx.send(()).unwrap();
            compiling.join().unwrap();
        });

        let stats = tb.stats();
        assert_eq!(stats.compilations_dispatched, 1);
        assert_eq!(stats.compilations_succeeded, 0);
        assert_eq!(stats.compilations_failed, 0);
        assert_eq!(stats.pending_jobs, 0);
        assert_eq!(stats.compilations_discarded, 1);
        assert!(
            tb.get_compiled(alloy_primitives::keccak256(BYTECODE_RET42), SpecId::CANCUN).is_none()
        );

        // The new generation still compiles and notifies normally.
        tb.compile_jit_sync(TestBackend::req_cancun(BYTECODE_RET42)).unwrap();
        assert!(
            tb.get_compiled(alloy_primitives::keccak256(BYTECODE_RET42), SpecId::CANCUN).is_some()
        );
        assert_eq!(tb.stats().pending_jobs, 0);
        assert_eq!(tb.stats().compilations_succeeded, 1);
        assert_eq!(tb.stats().compilations_discarded, 1);
    }
}

#[test]
#[cfg(feature = "llvm")]
fn jit_memory_tracks_compilation() {
    let tb =
        TestBackend::with_tuning_1w(RuntimeTuning { jit_hot_threshold: 1, ..Default::default() });

    let baseline = tb.stats().jit_code_bytes;

    tb.trigger_jit_cancun(BYTECODE_RET42);

    let stats = tb.stats();
    assert!(stats.jit_code_bytes > baseline, "jit_code_bytes should increase after JIT");
    assert_eq!(stats.resident_entries, 1);
}

#[test]
#[cfg(feature = "llvm")]
fn compile_jit_sync_blocks() {
    let tb = TestBackend::default_1w();

    let code_hash = alloy_primitives::keccak256(BYTECODE_RET42);
    tb.compile_jit_sync(TestBackend::req_cancun(BYTECODE_RET42)).unwrap();

    let program = tb.get_compiled(code_hash, SpecId::CANCUN);
    assert!(program.is_some(), "program should be resident after compile_jit_sync");
    assert_eq!(program.unwrap().kind, ProgramKind::Jit);
}

#[test]
fn sync_compile_wait_times_out() {
    let tb = TestBackend::disabled_with_tuning(RuntimeTuning {
        sync_compile_timeout: std::time::Duration::from_millis(1),
        ..Default::default()
    });
    let (_tx, rx) = chan::bounded(1);

    let err = tb.recv_compile_completion(rx).unwrap_err();
    assert!(err.to_string().contains("timed out waiting for compilation"));
}

#[test]
#[cfg(feature = "llvm")]
fn compile_jit_sync_deduplicates() {
    let compiled_count = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let compiled_count2 = compiled_count.clone();

    let tb = TestBackend::new(RuntimeConfig {
        enabled: true,
        tuning: RuntimeTuning { jit_worker_count: 1, ..Default::default() },
        on_compilation: Some(Arc::new(move |event| {
            if event.success {
                compiled_count2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        })),
        ..Default::default()
    });

    let code_hash = alloy_primitives::keccak256(BYTECODE_RET42);

    // Multiple sync compiles from different threads.
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let b = tb.backend.clone();
            let r = TestBackend::req_cancun(BYTECODE_RET42);
            std::thread::spawn(move || {
                let _ = b.compile_jit_sync(r);
            })
        })
        .collect();

    for t in threads {
        t.join().unwrap();
    }

    assert!(tb.get_compiled(code_hash, SpecId::CANCUN).is_some());
    assert_eq!(
        compiled_count.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "should compile exactly once even with concurrent sync requests",
    );
}

// ===========================================================================
// Tests: multiple SpecIds.
// ===========================================================================

#[test]
#[cfg(feature = "llvm")]
fn multiple_spec_ids() {
    let tb = TestBackend::with_tuning(RuntimeTuning {
        jit_hot_threshold: 1,
        jit_worker_count: 2,
        ..Default::default()
    });

    let code_hash = alloy_primitives::keccak256(BYTECODE_RET42);

    for &spec_id in &[SpecId::CANCUN, SpecId::PRAGUE] {
        tb.trigger_jit(BYTECODE_RET42, spec_id);
    }

    assert!(tb.stats().resident_entries >= 2);

    let p1 = tb.get_compiled(code_hash, SpecId::CANCUN).unwrap();
    let p2 = tb.get_compiled(code_hash, SpecId::PRAGUE).unwrap();
    assert_eq!(p1.key.spec_id, SpecId::CANCUN);
    assert_eq!(p2.key.spec_id, SpecId::PRAGUE);
}

// ===========================================================================
// Tests: eviction.
// ===========================================================================

#[test]
#[cfg(feature = "llvm")]
fn idle_eviction() {
    let tb = TestBackend::with_tuning_1w(RuntimeTuning {
        jit_hot_threshold: 1,
        idle_evict_duration: Some(std::time::Duration::from_millis(100)),
        eviction_sweep_interval: std::time::Duration::from_millis(50),
        ..Default::default()
    });

    tb.trigger_jit_cancun(BYTECODE_RET42);
    assert_eq!(tb.stats().resident_entries, 1);

    // Stop hitting the entry and wait for idle eviction.
    tb.wait_resident_count(0);
    assert_eq!(tb.stats().resident_entries, 0);
}

#[test]
#[cfg(feature = "llvm")]
fn eviction_frees_jit_memory() {
    let tb = TestBackend::with_tuning_1w(RuntimeTuning {
        jit_hot_threshold: 1,
        idle_evict_duration: Some(std::time::Duration::from_millis(100)),
        eviction_sweep_interval: std::time::Duration::from_millis(50),
        ..Default::default()
    });

    let baseline = tb.stats().jit_code_bytes;

    let compiled = tb.trigger_jit_cancun(BYTECODE_RET42);
    let after_compile = tb.stats().jit_code_bytes;
    assert!(
        after_compile > baseline,
        "jit_code_bytes should increase: baseline={baseline}, after={after_compile}",
    );

    // Drop our reference so eviction can free it.
    drop(compiled);

    tb.wait_resident_count(0);
    std::thread::sleep(std::time::Duration::from_millis(100));

    poll_until(std::time::Duration::from_secs(5), || {
        let after_eviction = tb.stats().jit_code_bytes;
        (after_eviction < after_compile).then_some(())
    });
}

#[test]
#[cfg(feature = "llvm")]
fn memory_budget_eviction() {
    let tb = TestBackend::with_tuning_1w(RuntimeTuning {
        jit_hot_threshold: 1,
        // Tiny budget: 1 byte — any entry should be evicted.
        resident_code_cache_bytes: 1,
        eviction_sweep_interval: std::time::Duration::from_millis(50),
        ..Default::default()
    });

    // Hammer lookups so the entry promotes; we can't observe it as Compiled
    // (eviction races with insertion under budget pressure), so wait on the
    // evictions counter instead.
    let req = TestBackend::req_cancun(BYTECODE_RET42);
    poll_until(std::time::Duration::from_secs(10), || {
        let _ = tb.lookup(req.clone());
        if tb.stats().evictions >= 1 { Some(()) } else { None }
    });
}

#[test]
#[cfg(feature = "llvm")]
fn memory_budget_evicts_aot_entries() {
    let tb = TestBackend::new(RuntimeConfig {
        enabled: true,
        store: Some(Arc::new(RuntimeArtifactStore::new().unwrap())),
        tuning: RuntimeTuning {
            jit_worker_count: 1,
            resident_code_cache_bytes: 1,
            eviction_sweep_interval: std::time::Duration::from_millis(50),
            ..Default::default()
        },
        ..Default::default()
    });

    tb.prepare_aot_sync(AotRequest {
        code_hash: alloy_primitives::keccak256(BYTECODE_RET42),
        code: Bytes::from_static(BYTECODE_RET42),
        spec_id: SpecId::CANCUN,
    })
    .unwrap();

    poll_until(std::time::Duration::from_secs(10), || (tb.stats().evictions >= 1).then_some(()));
}

// ===========================================================================
// Tests: concurrency.
// ===========================================================================

#[test]
#[cfg(feature = "llvm")]
fn concurrent_lookup_same_key() {
    let tb = TestBackend::with_tuning(RuntimeTuning {
        jit_hot_threshold: 1,
        jit_worker_count: 2,
        ..Default::default()
    });

    tb.trigger_jit_cancun(BYTECODE_RET42);

    // Hammer from many threads.
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let b = tb.backend.clone();
            std::thread::spawn(move || {
                for _ in 0..100 {
                    let d = b.lookup(TestBackend::req_cancun(BYTECODE_RET42));
                    assert!(
                        matches!(d, LookupDecision::Compiled(_)),
                        "expected Compiled, got: {d:?}"
                    );
                }
            })
        })
        .collect();

    for t in threads {
        t.join().unwrap();
    }

    let stats = tb.wait_stats(|s| s.lookup_hits + s.events_dropped >= 800);
    assert!(
        stats.lookup_hits + stats.events_dropped >= 800,
        "expected ≥800 hits+dropped, got hits={} dropped={}",
        stats.lookup_hits,
        stats.events_dropped,
    );
}

#[test]
#[cfg(feature = "llvm")]
fn single_jit_admission_per_key() {
    let compiled_count = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let compiled_count2 = compiled_count.clone();

    let tb = TestBackend::new(RuntimeConfig {
        enabled: true,
        tuning: RuntimeTuning { jit_hot_threshold: 1, jit_worker_count: 2, ..Default::default() },
        on_compilation: Some(Arc::new(move |event| {
            if event.success {
                compiled_count2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        })),
        ..Default::default()
    });

    // Many lookups to trigger admission multiple times.
    for _ in 0..50 {
        let _ = tb.lookup(TestBackend::req_cancun(BYTECODE_RET42));
    }

    tb.wait_compiled(BYTECODE_RET42, SpecId::CANCUN);

    // Give extra time for any spurious duplicate compilations.
    std::thread::sleep(std::time::Duration::from_millis(200));

    assert_eq!(
        compiled_count.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "key should be compiled exactly once",
    );
}

#[test]
#[cfg(feature = "llvm")]
fn stats_accuracy_concurrent() {
    let tb =
        TestBackend::with_tuning_1w(RuntimeTuning { jit_hot_threshold: 1, ..Default::default() });

    tb.trigger_jit_cancun(BYTECODE_RET42);

    let before = tb.stats();

    let n_threads = 4u64;
    let n_lookups = 50u64;
    let threads: Vec<_> = (0..n_threads)
        .map(|_| {
            let b = tb.backend.clone();
            std::thread::spawn(move || {
                for _ in 0..n_lookups {
                    let _ = b.lookup(TestBackend::req_cancun(BYTECODE_RET42));
                }
            })
        })
        .collect();

    for t in threads {
        t.join().unwrap();
    }

    let total_lookups = n_threads * n_lookups;
    // Stats are updated asynchronously by the backend; on overflow events drop.
    // Pre-existing in-flight events from trigger_jit_cancun may also slip in,
    // so use `>=` rather than equality.
    let after = tb.wait_stats(|s| {
        let new_hits = s.lookup_hits - before.lookup_hits;
        let new_dropped = s.events_dropped - before.events_dropped;
        new_hits + new_dropped >= total_lookups
    });
    let new_hits = after.lookup_hits - before.lookup_hits;
    let new_dropped = after.events_dropped - before.events_dropped;
    let new_misses = after.lookup_misses - before.lookup_misses;

    assert!(
        new_hits + new_dropped >= total_lookups,
        "all lookups should be accounted for: hits={new_hits} dropped={new_dropped} total={total_lookups}",
    );
    assert_eq!(new_misses, 0, "all lookups should be hits");
}

// ===========================================================================
// Tests: lifecycle (clear, set_enabled, shutdown).
// ===========================================================================

#[test]
#[cfg(feature = "llvm")]
fn clear_during_active_work() {
    let tb =
        TestBackend::with_tuning_1w(RuntimeTuning { jit_hot_threshold: 1, ..Default::default() });

    // Fire many JIT requests to keep workers busy.
    for i in 0u8..20 {
        let bytecode = indexed_bytecode(i);
        let _ = tb.lookup(TestBackend::req_cancun(&bytecode));
    }

    // Clear while work is in-flight.
    tb.clear_resident();
    tb.clear_all();

    // Should not panic.
    std::thread::sleep(std::time::Duration::from_millis(500));
    let _stats = tb.stats();
}

#[test]
#[cfg(feature = "llvm")]
fn set_enabled_during_compilation() {
    let tb =
        TestBackend::with_tuning_1w(RuntimeTuning { jit_hot_threshold: 1, ..Default::default() });

    // Trigger JIT.
    for _ in 0..5 {
        let _ = tb.lookup(TestBackend::req_cancun(BYTECODE_RET42));
    }

    // Disable while compilation may be in progress.
    tb.set_enabled(false).unwrap();
    assert!(matches!(
        tb.lookup(TestBackend::req_cancun(BYTECODE_RET42)),
        LookupDecision::Unavailable(LookupMissReason::Disabled)
    ));

    // Re-enable and check compilation eventually completes.
    tb.set_enabled(true).unwrap();
    tb.wait_compiled(BYTECODE_RET42, SpecId::CANCUN);
}

// ===========================================================================
// Tests: on_compilation callback.
// ===========================================================================

#[test]
#[cfg(feature = "llvm")]
fn on_compilation_callback() {
    let events: Arc<Mutex<Vec<CompilationEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let events2 = events.clone();

    let tb = TestBackend::new(RuntimeConfig {
        enabled: true,
        tuning: RuntimeTuning { jit_hot_threshold: 1, jit_worker_count: 1, ..Default::default() },
        on_compilation: Some(Arc::new(move |event| {
            events2.lock().unwrap().push(event);
        })),
        ..Default::default()
    });

    let code_hash = alloy_primitives::keccak256(BYTECODE_RET42);
    tb.trigger_jit_cancun(BYTECODE_RET42);

    let captured = events.lock().unwrap();
    assert_eq!(captured.len(), 1, "expected exactly one compilation event");
    let event = &captured[0];
    assert_eq!(event.code_hash, code_hash);
    assert_eq!(event.spec_id, SpecId::CANCUN);
    assert!(event.success);
    assert_eq!(event.kind, CompilationKind::Jit);
    assert!(!event.duration.is_zero());
}

// ===========================================================================
// Tests: AOT prepare and persistence.
// ===========================================================================

#[test]
#[cfg(feature = "llvm")]
fn prepare_aot_persist_and_load() {
    let store = Arc::new(RuntimeArtifactStore::new().unwrap());
    let tb = TestBackend::new(RuntimeConfig {
        enabled: true,
        store: Some(store.clone()),
        tuning: RuntimeTuning { jit_worker_count: 1, ..Default::default() },
        ..Default::default()
    });

    let code_hash = alloy_primitives::keccak256(BYTECODE_RET42);
    tb.prepare_aot_sync(AotRequest {
        code_hash,
        code: Bytes::from_static(BYTECODE_RET42),
        spec_id: SpecId::CANCUN,
    })
    .unwrap();

    let p = tb.get_compiled(code_hash, SpecId::CANCUN).unwrap();
    assert_eq!(p.kind, ProgramKind::Aot);
    assert_eq!(store.len(), 1);
}

#[test]
#[cfg(feature = "llvm")]
fn failed_aot_persist_is_not_retried_until_cleared() {
    let mut dispatches = Vec::new();
    for failure in [
        AotFailure::Persist,
        AotFailure::ReloadMissing,
        AotFailure::ReloadError,
        AotFailure::Link,
        AotFailure::NoStore,
    ] {
        let store = (!matches!(failure, AotFailure::NoStore)).then(|| {
            Arc::new(AotFailureStore { inner: RuntimeArtifactStore::new().unwrap(), failure })
                as Arc<dyn ArtifactStore>
        });
        let tb = TestBackend::new(RuntimeConfig {
            enabled: true,
            aot: store.is_some(),
            store,
            tuning: RuntimeTuning {
                jit_worker_count: 1,
                jit_hot_threshold: 1,
                idle_evict_duration: None,
                ..Default::default()
            },
            ..Default::default()
        });
        let req = || AotRequest {
            code_hash: alloy_primitives::keccak256(BYTECODE_RET42),
            code: Bytes::copy_from_slice(BYTECODE_RET42),
            spec_id: SpecId::CANCUN,
        };

        // Duplicate requests must all be notified even when persistence/linking fails.
        tb.prepare_aot_batch_sync(vec![req(), req(), req()]).unwrap();
        assert_eq!(tb.stats().compilations_failed, 1, "failure case {failure:?}");
        for _ in 0..10 {
            if !matches!(failure, AotFailure::NoStore) {
                let misses = tb.stats().lookup_misses;
                let _ = tb.lookup(TestBackend::req_cancun(BYTECODE_RET42));
                tb.wait_stats(|s| s.lookup_misses > misses);
            }
            // Also acts as a completion barrier and checks suppressed requests notify callers.
            tb.prepare_aot_sync(req()).unwrap();
        }
        let attempts = tb.stats().compilations_dispatched;
        eprintln!("{failure:?}: dispatched {attempts} AOT attempts before clear");
        dispatches.push(attempts);

        tb.clear_resident();
        tb.prepare_aot_sync(req()).unwrap();
        assert_eq!(tb.stats().compilations_dispatched, attempts + 1);
        assert_eq!(tb.stats().compilations_failed, attempts + 1);

        // In JIT mode an explicit AOT failure must not suppress the other compilation kind.
        // In AOT mode even compile_jit_sync deliberately requests AOT output.
        if matches!(failure, AotFailure::NoStore) {
            tb.compile_jit_sync(TestBackend::req_cancun(BYTECODE_RET42)).unwrap();
            let program = tb.get_compiled(req().code_hash, SpecId::CANCUN).unwrap();
            assert_eq!(program.kind, ProgramKind::Jit);
            assert_eq!(tb.stats().compilations_succeeded, 1);
        }
        assert_eq!(tb.stats().pending_jobs, 0);
    }
    assert_eq!(dispatches, vec![1; 5]);
}

#[test]
#[cfg(feature = "llvm")]
fn failed_aot_persist_allows_jit_retry() {
    for failure in
        [AotFailure::Persist, AotFailure::ReloadMissing, AotFailure::ReloadError, AotFailure::Link]
    {
        let tb = TestBackend::new(RuntimeConfig {
            enabled: true,
            store: Some(Arc::new(AotFailureStore {
                inner: RuntimeArtifactStore::new().unwrap(),
                failure,
            })),
            tuning: RuntimeTuning { jit_worker_count: 1, ..Default::default() },
            ..Default::default()
        });
        let req = TestBackend::req_cancun(BYTECODE_RET42);
        tb.prepare_aot_sync(AotRequest {
            code_hash: req.key.code_hash,
            code: req.code.clone(),
            spec_id: req.key.spec_id,
        })
        .unwrap();
        assert_eq!(tb.stats().compilations_failed, 1);
        tb.compile_jit_sync(req.clone()).unwrap();
        assert_eq!(
            tb.get_compiled(req.key.code_hash, req.key.spec_id).unwrap().kind,
            ProgramKind::Jit
        );
        assert_eq!(tb.stats().compilations_dispatched, 2);
        assert_eq!(tb.stats().compilations_succeeded, 1);
        assert_eq!(tb.stats().pending_jobs, 0);
    }
}

#[test]
#[cfg(feature = "llvm")]
fn blocking_aot_mode_compiles_miss_to_aot() {
    let store = Arc::new(RuntimeArtifactStore::new().unwrap());
    let tb = TestBackend::new(RuntimeConfig {
        enabled: true,
        aot: true,
        blocking: true,
        store: Some(store.clone()),
        tuning: RuntimeTuning { jit_worker_count: 1, ..Default::default() },
        ..Default::default()
    });

    let decision = tb.lookup(TestBackend::req_cancun(BYTECODE_RET42));
    assert!(matches!(&decision, LookupDecision::Compiled(p) if p.kind == ProgramKind::Aot));
    assert_eq!(store.len(), 1);
}

#[test]
#[cfg(feature = "llvm")]
fn prepare_aot_batch_persist_and_load() {
    let store = Arc::new(RuntimeArtifactStore::new().unwrap());
    let tb = TestBackend::new(RuntimeConfig {
        enabled: true,
        store: Some(store.clone()),
        tuning: RuntimeTuning { jit_worker_count: 1, ..Default::default() },
        ..Default::default()
    });

    let bytecodes: &[&[u8]] = &[BYTECODE_RET42, BYTECODE_ADD];
    let hashes: Vec<B256> = bytecodes.iter().map(alloy_primitives::keccak256).collect();

    let reqs: Vec<AotRequest> = bytecodes
        .iter()
        .zip(&hashes)
        .map(|(code, hash)| AotRequest {
            code_hash: *hash,
            code: Bytes::copy_from_slice(code),
            spec_id: SpecId::CANCUN,
        })
        .collect();
    tb.prepare_aot_batch_sync(reqs).unwrap();

    for (bc, hash) in bytecodes.iter().zip(&hashes) {
        let p = tb.get_compiled(*hash, SpecId::CANCUN).unwrap_or_else(|| {
            panic!("missing AOT program for bytecode {bc:?}");
        });
        assert_eq!(p.kind, ProgramKind::Aot);
    }
    assert_eq!(store.len(), 2);
}

#[test]
#[cfg(feature = "llvm")]
fn aot_artifacts_survive_restart() {
    let store = Arc::new(RuntimeArtifactStore::new().unwrap());
    let code_hash = alloy_primitives::keccak256(BYTECODE_RET42);

    // First backend: compile and persist an AOT artifact.
    {
        let tb = TestBackend::new(RuntimeConfig {
            enabled: true,
            store: Some(store.clone()),
            tuning: RuntimeTuning { jit_worker_count: 1, ..Default::default() },
            ..Default::default()
        });

        tb.prepare_aot_sync(AotRequest {
            code_hash,
            code: Bytes::from_static(BYTECODE_RET42),
            spec_id: SpecId::CANCUN,
        })
        .unwrap();
    }

    assert_eq!(store.len(), 1);

    // Second backend: should preload the artifact at startup.
    {
        let tb = TestBackend::with_store(store, Default::default());

        let decision = tb.lookup(TestBackend::req_cancun(BYTECODE_RET42));
        assert!(
            matches!(&decision, LookupDecision::Compiled(p) if p.kind == ProgramKind::Aot),
            "expected AOT hit after restart, got: {decision:?}",
        );
        assert_eq!(tb.stats().resident_entries, 1);
    }
}

#[test]
#[cfg(feature = "llvm")]
fn preload_aot_seeds_resident() {
    let store = Arc::new(RuntimeArtifactStore::new().unwrap());
    let code_hash = alloy_primitives::keccak256(BYTECODE_RET42);

    // First backend: compile and persist.
    {
        let tb = TestBackend::new(RuntimeConfig {
            enabled: true,
            store: Some(store.clone()),
            tuning: RuntimeTuning { jit_worker_count: 1, ..Default::default() },
            ..Default::default()
        });

        tb.prepare_aot_sync(AotRequest {
            code_hash,
            code: Bytes::from_static(BYTECODE_RET42),
            spec_id: SpecId::CANCUN,
        })
        .unwrap();
    }

    // Second backend: preloaded AOT should be available immediately.
    {
        let tb = TestBackend::with_store(store, Default::default());
        assert_eq!(tb.stats().resident_entries, 1);
    }
}

#[test]
#[cfg(feature = "llvm")]
fn preload_aot_skips_stale_abi_version() {
    let store = Arc::new(RuntimeArtifactStore::new().unwrap());
    let code_hash = alloy_primitives::keccak256(BYTECODE_RET42);

    {
        let tb = TestBackend::new(RuntimeConfig {
            enabled: true,
            store: Some(store.clone()),
            tuning: RuntimeTuning { jit_worker_count: 1, ..Default::default() },
            ..Default::default()
        });
        tb.prepare_aot_sync(AotRequest {
            code_hash,
            code: Bytes::from_static(BYTECODE_RET42),
            spec_id: SpecId::CANCUN,
        })
        .unwrap();
    }

    let (key, stored) = store.load_all().unwrap().pop().unwrap();
    let stale_key = ArtifactKey { abi_version: crate::ABI_VERSION - 1, ..key };
    let stale_manifest = ArtifactManifest { artifact_key: stale_key, ..stored.manifest.clone() };
    let stale_store = Arc::new(RuntimeArtifactStore::new().unwrap());
    stale_store
        .store(&stale_key, &stale_manifest, &std::fs::read(&stored.object_path).unwrap())
        .unwrap();

    let tb = TestBackend::new(RuntimeConfig {
        enabled: true,
        store: Some(stale_store.clone()),
        ..Default::default()
    });
    assert!(tb.get_compiled(code_hash, SpecId::CANCUN).is_none());
    assert_eq!(tb.stats().resident_entries, 0);
    assert_eq!(stale_store.len(), 1);
}

#[test]
#[cfg(feature = "llvm")]
fn preload_aot_skips_manifest_key_mismatch() {
    let store = Arc::new(RuntimeArtifactStore::new().unwrap());
    let code_hash = alloy_primitives::keccak256(BYTECODE_RET42);

    {
        let tb = TestBackend::new(RuntimeConfig {
            enabled: true,
            store: Some(store.clone()),
            tuning: RuntimeTuning { jit_worker_count: 1, ..Default::default() },
            ..Default::default()
        });
        tb.prepare_aot_sync(AotRequest {
            code_hash,
            code: Bytes::from_static(BYTECODE_RET42),
            spec_id: SpecId::CANCUN,
        })
        .unwrap();
    }

    let (key, stored) = store.load_all().unwrap().pop().unwrap();
    let stale_key = ArtifactKey { abi_version: crate::ABI_VERSION - 1, ..key };
    let manifest = ArtifactManifest { artifact_key: stale_key, ..stored.manifest.clone() };
    let mismatched_store = Arc::new(RuntimeArtifactStore::new().unwrap());
    mismatched_store.store(&key, &manifest, &std::fs::read(&stored.object_path).unwrap()).unwrap();

    let tb = TestBackend::with_store(
        mismatched_store,
        RuntimeTuning { jit_worker_count: 1, ..Default::default() },
    );
    assert_eq!(tb.stats().resident_entries, 0);

    // The probe rejects it too, so the artifact is recompiled.
    tb.prepare_aot_sync(AotRequest {
        code_hash,
        code: Bytes::from_static(BYTECODE_RET42),
        spec_id: SpecId::CANCUN,
    })
    .unwrap();
    assert_eq!(tb.stats().compilations_dispatched, 1);
}

#[test]
fn compile_config_hash_covers_every_compile_input() {
    let hash = |config: RuntimeConfig| storage::compile_config_hash(&config);
    let base = RuntimeConfig::default();
    let mut gas_params = evm2::Version::base(SpecId::CANCUN).gas_params;
    let with_gas_params = RuntimeConfig { gas_params: Some(gas_params), ..Default::default() };
    *gas_params.get_mut(evm2::version::GasId::ExpByte) += 1;

    let hashes = [
        hash(base.clone()),
        hash(RuntimeConfig { debug_assertions: !base.debug_assertions, ..base.clone() }),
        hash(RuntimeConfig { single_error: !base.single_error, ..base.clone() }),
        hash(RuntimeConfig { no_dedup: !base.no_dedup, ..base.clone() }),
        hash(RuntimeConfig { no_dse: !base.no_dse, ..base.clone() }),
        hash(with_gas_params),
        hash(RuntimeConfig { gas_params: Some(gas_params), ..base }),
    ];
    for (i, a) in hashes.iter().enumerate() {
        assert!(hashes[i + 1..].iter().all(|b| a != b), "hash {i} collides");
    }
}

/// An artifact compiled under one gas schedule is neither preloaded nor probed by a backend that
/// runs another, and both artifacts coexist in the shared store.
#[test]
#[cfg(feature = "llvm")]
fn artifacts_are_scoped_to_compile_config() {
    let store = Arc::new(RuntimeArtifactStore::new().unwrap());
    let code_hash = alloy_primitives::keccak256(BYTECODE_RET42);
    let req = || AotRequest {
        code_hash,
        code: Bytes::from_static(BYTECODE_RET42),
        spec_id: SpecId::CANCUN,
    };
    let config = |gas_params| RuntimeConfig {
        enabled: true,
        store: Some(store.clone()),
        gas_params,
        tuning: RuntimeTuning { jit_worker_count: 1, ..Default::default() },
        ..Default::default()
    };

    {
        let tb = TestBackend::new(config(None));
        tb.prepare_aot_sync(req()).unwrap();
    }
    assert_eq!(store.len(), 1);

    let mut gas_params = evm2::Version::base(SpecId::CANCUN).gas_params;
    *gas_params.get_mut(evm2::version::GasId::ExpByte) += 1;

    let tb = TestBackend::new(config(Some(gas_params)));
    assert!(tb.get_compiled(code_hash, SpecId::CANCUN).is_none());
    assert_eq!(tb.stats().resident_entries, 0);

    tb.prepare_aot_sync(req()).unwrap();
    assert_eq!(tb.stats().compilations_dispatched, 1);
    assert_eq!(store.len(), 2);
    assert!(tb.get_compiled(code_hash, SpecId::CANCUN).is_some());
}

/// `prepare_aot()` on a key that is already persisted in the store but not resident
/// (e.g. after a `clear_resident`) loads from the store without recompiling.
#[test]
#[cfg(feature = "llvm")]
fn prepare_aot_already_persisted_not_resident() {
    let store = Arc::new(RuntimeArtifactStore::new().unwrap());
    let code_hash = alloy_primitives::keccak256(BYTECODE_RET42);

    let tb = TestBackend::new(RuntimeConfig {
        enabled: true,
        store: Some(store.clone()),
        tuning: RuntimeTuning { jit_worker_count: 1, ..Default::default() },
        ..Default::default()
    });

    // Compile and persist an AOT artifact.
    tb.prepare_aot_sync(AotRequest {
        code_hash,
        code: Bytes::from_static(BYTECODE_RET42),
        spec_id: SpecId::CANCUN,
    })
    .unwrap();
    assert_eq!(store.len(), 1);

    let dispatched_before = tb.stats().compilations_dispatched;

    // Clear resident — artifact is still persisted in the store but not in memory.
    tb.clear_resident();
    tb.wait_resident_count(0);

    // Requesting prepare_aot again should load from store without recompiling.
    tb.prepare_aot_sync(AotRequest {
        code_hash,
        code: Bytes::from_static(BYTECODE_RET42),
        spec_id: SpecId::CANCUN,
    })
    .unwrap();

    let p = tb.get_compiled(code_hash, SpecId::CANCUN).unwrap();
    assert_eq!(p.kind, ProgramKind::Aot);

    // No new compilation should have been dispatched — loaded from store.
    assert_eq!(
        tb.stats().compilations_dispatched,
        dispatched_before,
        "should not recompile when artifact is already persisted",
    );
}

/// AOT preparation persists JIT-resident code and allows reloading without recompilation.
#[test]
#[cfg(feature = "llvm")]
fn prepare_aot_persists_jit_resident() {
    let store = Arc::new(RuntimeArtifactStore::new().unwrap());
    let code_hash = alloy_primitives::keccak256(BYTECODE_RET42);

    let tb = TestBackend::new(RuntimeConfig {
        enabled: true,
        store: Some(store.clone()),
        tuning: RuntimeTuning { jit_worker_count: 1, ..Default::default() },
        ..Default::default()
    });

    // JIT-compile first.
    tb.compile_jit_sync(TestBackend::req_cancun(BYTECODE_RET42)).unwrap();
    let p = tb.get_compiled(code_hash, SpecId::CANCUN).unwrap();
    assert_eq!(p.kind, ProgramKind::Jit);
    assert_eq!(tb.stats().resident_entries, 1);
    assert!(store.is_empty());
    let dispatched_before = tb.stats().compilations_dispatched;

    // JIT requests still reuse the resident program.
    tb.compile_jit_sync(TestBackend::req_cancun(BYTECODE_RET42)).unwrap();
    assert_eq!(tb.stats().compilations_dispatched, dispatched_before);

    let req =
        AotRequest { code_hash, code: Bytes::from_static(BYTECODE_RET42), spec_id: SpecId::CANCUN };
    tb.prepare_aot_sync(req.clone()).unwrap();

    assert_eq!(store.len(), 1);
    assert_eq!(tb.get_compiled(code_hash, SpecId::CANCUN).unwrap().kind, ProgramKind::Aot);
    let dispatched_after = tb.stats().compilations_dispatched;
    assert_eq!(dispatched_after, dispatched_before + 1);

    // Both request kinds reuse the resident AOT program.
    tb.prepare_aot_sync(req.clone()).unwrap();
    tb.compile_jit_sync(TestBackend::req_cancun(BYTECODE_RET42)).unwrap();
    assert_eq!(tb.stats().compilations_dispatched, dispatched_after);
    assert_eq!(tb.get_compiled(code_hash, SpecId::CANCUN).unwrap().kind, ProgramKind::Aot);

    // Clearing memory leaves an artifact that can be loaded without recompiling.
    tb.clear_resident();
    tb.wait_resident_count(0);
    tb.prepare_aot_sync(req).unwrap();
    assert_eq!(tb.get_compiled(code_hash, SpecId::CANCUN).unwrap().kind, ProgramKind::Aot);
    assert_eq!(tb.stats().compilations_dispatched, dispatched_after);
}

/// Failure to reload the AOT artifact leaves the original JIT program available.
#[test]
#[cfg(feature = "llvm")]
fn prepare_aot_preserves_jit_resident_on_failure() {
    let tb = TestBackend::new(RuntimeConfig {
        enabled: true,
        store: Some(Arc::new(EmptyStore)),
        tuning: RuntimeTuning { jit_worker_count: 1, ..Default::default() },
        ..Default::default()
    });
    let req = TestBackend::req_cancun(BYTECODE_RET42);
    tb.compile_jit_sync(req.clone()).unwrap();
    let jit = tb.get_compiled(req.key.code_hash, req.key.spec_id).unwrap();
    assert_eq!(jit.kind, ProgramKind::Jit);
    let before = tb.stats();

    // EmptyStore accepts writes but cannot return the artifact for loading.
    tb.prepare_aot_sync(AotRequest {
        code_hash: req.key.code_hash,
        code: req.code,
        spec_id: req.key.spec_id,
    })
    .unwrap();

    assert_eq!(tb.stats().compilations_dispatched, before.compilations_dispatched + 1);
    assert_eq!(tb.stats().compilations_failed, before.compilations_failed + 1);
    assert!(Arc::ptr_eq(&jit, &tb.get_compiled(req.key.code_hash, req.key.spec_id).unwrap()));
}

// ===========================================================================
// Tests: cold-entry eviction.
// ===========================================================================

#[test]
#[cfg(feature = "llvm")]
fn cold_entries_are_evicted() {
    let tb = TestBackend::with_tuning_1w(RuntimeTuning {
        jit_hot_threshold: 2,
        jit_max_pending_jobs: 1,
        idle_evict_duration: Some(std::time::Duration::from_millis(20)),
        eviction_sweep_interval: std::time::Duration::from_millis(10),
        event_drain_interval: std::time::Duration::from_millis(10),
        ..Default::default()
    });

    for i in 0..10 {
        let _ = tb.lookup(TestBackend::req_cancun(&indexed_bytecode(i)));
    }

    let compile_me = indexed_bytecode(42);
    for _ in 0..2 {
        let _ = tb.lookup(TestBackend::req_cancun(&compile_me));
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(tb.stats().resident_entries, 0);

    std::thread::sleep(std::time::Duration::from_millis(100));
    tb.wait_compiled(&compile_me, SpecId::CANCUN);
}

// ===========================================================================
// Tests: compiler recycling.
// ===========================================================================

/// Compile several distinct contracts with `compiler_recycle_threshold: 1` so
/// the worker recreates its LLVM context after every single compilation.
/// All contracts must still compile successfully.
#[test]
#[cfg(feature = "llvm")]
fn compiler_recycle_every_compilation() {
    let n = 5u8;
    let tb = TestBackend::with_tuning_1w(RuntimeTuning {
        jit_hot_threshold: 1,
        compiler_recycle_threshold: 1,
        ..Default::default()
    });

    for i in 0..n {
        let bc = indexed_bytecode(i);
        tb.trigger_jit_cancun(&bc);
    }

    assert_eq!(tb.stats().resident_entries, n as u64);
}

#[test]
fn shutdown_propagates_worker_error() {
    let backend = JitBackend::disabled();
    let (done_tx, done_rx) = chan::bounded(1);
    *backend.inner.thread.lock().unwrap() = Some(BackendThread {
        handle: std::thread::spawn(move || {
            let _ = done_tx.send(());
            eyre::bail!("controlled compile worker failure")
        }),
        done_rx,
    });
    let error = backend.inner.shutdown().unwrap_err();
    assert!(error.to_string().contains("controlled compile worker failure"));
    assert!(backend.inner.thread.lock().unwrap().is_none());
}
