//! Runtime JIT backend: O(1) compiled-function lookup with background compilation.
//!
//! - Startup AOT preload from [`ArtifactStore::load_all`] into an immutable in-memory map.
//! - O(1) [`JitBackend::lookup`] that only reads the resident map.
//! - Fire-and-forget lookup-observed events to the backend thread.
//! - Background JIT compilation for hot keys (threshold-based promotion).

use crate::eyre::{self, WrapErr};
use alloy_primitives::B256;
use backend::{
    Command, CompileJitRequest, EventQueue, JitObjectLinker, PrepareAotRequest, ResidentMap,
};
use crossbeam_channel as chan;
use crossbeam_queue::ArrayQueue;
use evm2::SpecId;
use stats::RuntimeStats;
use std::{
    hint::cold_path,
    ops::ControlFlow,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use worker::SyncNotifier;

mod api;
pub use api::{
    AotRequest, CompiledProgram, LookupDecision, LookupMissReason, LookupRequest, ProgramKind,
};

mod config;
pub use config::{
    CompilationEvent, CompilationKind, HostConfig, JitMode, RuntimeConfig, RuntimeTuning,
};

mod backend;

mod stats;
pub use stats::RuntimeStatsSnapshot;

mod storage;
pub use storage::{
    ArtifactKey, ArtifactManifest, ArtifactStore, BackendSelection, RuntimeArtifactStore,
    RuntimeCacheKey, StoredArtifact,
};

#[cfg(all(feature = "llvm", unix))]
mod out_of_process;

mod worker;

/// Runs the out-of-process JIT helper if this process was launched as one.
///
/// Returns [`ControlFlow::Break`] after the helper request has been handled and
/// the caller should exit immediately. Normal application startup should
/// continue on [`ControlFlow::Continue`].
pub fn maybe_run_jit_helper() -> eyre::Result<ControlFlow<()>> {
    #[cfg(all(feature = "llvm", unix))]
    {
        out_of_process::maybe_run_jit_helper()
    }
    #[cfg(not(all(feature = "llvm", unix)))]
    {
        if std::env::var_os("EVM2_JIT_HELPER").is_some() {
            eyre::bail!("out-of-process JIT helper is only available on Unix with LLVM");
        }
        Ok(ControlFlow::Continue(()))
    }
}

#[cfg(test)]
mod tests;

/// State shared between [`JitBackend`] (via [`BackendInner`]) and the backend
/// thread.
///
/// Held by the backend thread as `Arc<BackendShared>`. Crucially, the backend
/// thread does NOT hold an `Arc<BackendInner>` — that would create a reference
/// cycle since `BackendInner::Drop` is what signals the thread to stop. Keeping
/// thread-lifecycle fields outside this struct lets `BackendInner` drop (and
/// trigger shutdown) as soon as the last `JitBackend` clone is released.
#[derive(derive_more::Debug)]
pub(crate) struct BackendShared {
    /// Shared resident compiled map.
    #[debug(skip)]
    resident: ResidentMap,
    /// Lock-free queue of events.
    #[debug(skip)]
    events: EventQueue,
    /// Number of active out-of-process helper pauses.
    pause_depth: AtomicUsize,
    /// Shared stats counters.
    #[debug(skip)]
    stats: Arc<RuntimeStats>,
}

/// Inner state for [`JitBackend`]. Owns the backend thread lifecycle.
#[derive(derive_more::Debug)]
pub(crate) struct BackendInner {
    /// State shared with the backend thread.
    shared: Arc<BackendShared>,
    /// Global enable flag.
    enabled: AtomicBool,
    /// Whether the backend thread has been spawned and is draining the command channel.
    started: AtomicBool,
    /// Blocking mode: every lookup synchronously compiles and counts each fallback.
    blocking: bool,
    host: config::HostConfig,
    gas_params: Option<evm2::version::GasParams>,
    /// Tuning knobs (Copy). Cached for hot-path eligibility checks.
    tuning: crate::runtime::config::RuntimeTuning,
    /// Bounded channel for control commands (compile_jit, prepare_aot, clears,
    /// shutdown). The lookup hot path does NOT use this — see
    /// [`BackendShared::events`].
    #[debug(skip)]
    tx: chan::Sender<Command>,
    /// Backend thread + done signal. `None` after shutdown.
    #[debug(skip)]
    thread: std::sync::Mutex<Option<BackendThread>>,
    /// Shutdown timeout.
    shutdown_timeout: Duration,
    /// State for lazily spawning the backend thread.
    #[debug(skip)]
    lazy_spawn: std::sync::Mutex<Option<LazySpawnState>>,
}

/// State kept around for lazily spawning the backend thread.
struct LazySpawnState {
    rx: chan::Receiver<Command>,
    config: RuntimeConfig,
}

/// Backend thread handle and its completion signal.
struct BackendThread {
    handle: std::thread::JoinHandle<eyre::Result<()>>,
    done_rx: chan::Receiver<()>,
}

/// JIT compilation backend with O(1) compiled-function lookup.
///
/// Created via [`JitBackend::new`] or [`JitBackend::disabled`].
/// This type is cheaply cloneable (backed by `Arc`).
/// All clones share the same backend thread, resident map, and statistics.
#[derive(Clone, Debug)]
pub struct JitBackend {
    inner: Arc<BackendInner>,
}

impl JitBackend {
    /// Creates a disabled backend that performs no compilation and spawns no threads.
    ///
    /// All [`lookup`](Self::lookup) calls return
    /// `LookupDecision::Unavailable(LookupMissReason::Disabled)`.
    /// Call [`set_enabled`](Self::set_enabled) to lazily spawn the backend thread with
    /// a default [`RuntimeConfig`].
    pub fn disabled() -> Self {
        Self::new_inner(RuntimeConfig::default()).expect("default config cannot fail")
    }

    /// Creates a backend from the given config.
    ///
    /// If [`enabled`](RuntimeConfig::enabled) is `true`, the backend thread is spawned
    /// immediately and AOT artifacts are preloaded. Otherwise, both are deferred until the
    /// first [`set_enabled(true)`](Self::set_enabled) call.
    pub fn new(mut config: RuntimeConfig) -> eyre::Result<Self> {
        config = config.with_env_overrides()?;
        Self::new_inner(config)
    }

    fn new_inner(mut config: RuntimeConfig) -> eyre::Result<Self> {
        if config.blocking {
            config.enabled = true;
            config.tuning.jit_hot_threshold = 0;
            config.tuning.jit_max_bytecode_len = 0;
        }
        #[cfg(not(unix))]
        if config.jit_mode == JitMode::OutOfProcess {
            eyre::bail!("out-of-process JIT is only available on Unix");
        }
        eyre::ensure!(!config.aot || config.store.is_some(), "AOT mode requires an artifact store");
        eyre::ensure!(
            !config.aot || config.jit_mode != JitMode::OutOfProcess,
            "AOT mode does not support out-of-process compilation"
        );

        eyre::ensure!(
            config.host.is_default() || (!config.aot && config.store.is_none()),
            "custom hosts require JIT without an artifact store"
        );
        let enabled = config.enabled;
        let (tx, rx) = chan::bounded::<Command>(config.tuning.channel_capacity);
        let events = ArrayQueue::new(config.tuning.channel_capacity);
        let tuning = config.tuning;
        let shared = Arc::new(BackendShared {
            resident: ResidentMap::default(),
            events,
            pause_depth: AtomicUsize::new(0),
            stats: Arc::new(RuntimeStats::default()),
        });
        let this = Self {
            inner: Arc::new(BackendInner {
                shared,
                enabled: AtomicBool::new(false),
                started: AtomicBool::new(false),
                blocking: config.blocking,
                host: config.host.clone(),
                gas_params: config.gas_params,
                tx,
                thread: std::sync::Mutex::new(None),
                shutdown_timeout: config.tuning.shutdown_timeout,
                tuning,
                lazy_spawn: std::sync::Mutex::new(Some(LazySpawnState { rx, config })),
            }),
        };
        this.set_enabled(enabled)?;
        Ok(this)
    }

    /// Counts a frame that ran on the interpreter because the runtime could not run it.
    pub(crate) fn count_interpreter_fallback(&self) {
        self.inner.shared.stats.interpreter_fallbacks.fetch_add(1, Ordering::Relaxed);
    }

    /// Checks that generated code uses the field layout of the interpreter type `T`.
    #[inline]
    pub(crate) fn supports_host_type<T: evm2::EvmTypesHost>(&self) -> bool {
        self.inner.host.type_id == std::any::TypeId::of::<T>()
    }

    /// Checks that generated code matches the interpreter type, gas schedule, and opcode table.
    ///
    /// An enabled backend counts a rejection as an interpreter fallback and logs the reason,
    /// because the caller then runs every frame on the interpreter. Compiled code is not keyed
    /// by gas schedule, so a host whose schedule differs from the backend's is never compiled
    /// for.
    pub(crate) fn supports_host<T: evm2::EvmTypesHost>(
        &self,
        config: &evm2::ExecutionConfig<T>,
    ) -> bool {
        let Some(reason) = self.host_mismatch(config) else { return true };
        if self.enabled() {
            self.count_interpreter_fallback();
            warn!(
                spec_id = ?config.base_spec_id(),
                reason,
                "host unsupported, running the interpreter",
            );
        }
        false
    }

    /// Returns why generated code does not match `config`, or `None` if it does.
    pub(crate) fn host_mismatch<T: evm2::EvmTypesHost>(
        &self,
        config: &evm2::ExecutionConfig<T>,
    ) -> Option<&'static str> {
        if !self.supports_host_type::<T>() {
            return Some("host type differs from the one the backend was built for");
        }
        let gas_params = self
            .inner
            .gas_params
            .as_ref()
            .unwrap_or(&evm2::Version::base(config.base_spec_id()).gas_params);
        if config.version().gas_params != *gas_params {
            return Some("gas schedule differs from the one the backend compiles with");
        }
        if !self.opcodes_match(config) {
            return Some("opcode table differs from the one the backend compiles with");
        }
        None
    }

    /// Checks the active opcode config against the host's opcode table.
    fn opcodes_match<T: evm2::EvmTypesHost>(&self, config: &evm2::ExecutionConfig<T>) -> bool {
        match &self.inner.host.opcodes {
            None => config.has_base_opcode_table(),
            Some(opcodes) => {
                // SAFETY: The caller checked that the host type is `T`, so `opcodes.config` is a
                // `&'static OpcodeConfig<T>`.
                config
                    .opcode_config()
                    .same_table(unsafe { &*(opcodes.config as *const evm2::OpcodeConfig<T>) })
            }
        }
    }

    /// Looks up a compiled function for the given request.
    ///
    /// In normal mode this never blocks. In [`blocking`](RuntimeConfig::blocking) mode,
    /// a miss triggers synchronous JIT compilation and the call blocks until it completes.
    pub fn lookup(&self, mut req: LookupRequest) -> LookupDecision {
        let inner = &*self.inner;
        let shared = &*inner.shared;

        if !inner.enabled.load(Ordering::Relaxed) {
            cold_path();
            return LookupDecision::Unavailable(LookupMissReason::Disabled);
        }
        if inner.blocking {
            cold_path();
            return self.lookup_blocking(req);
        }
        if !inner.tuning.should_compile(&req.code) {
            cold_path();
            return LookupDecision::Unavailable(LookupMissReason::Ineligible);
        }

        let decision = if let Some(program_ref) = shared.resident.try_get(&req.key).try_unwrap() {
            let program = Arc::clone(&program_ref);
            drop(program_ref);
            req.code.clear();
            LookupDecision::Compiled(program)
        } else {
            LookupDecision::Unavailable(LookupMissReason::NotReady)
        };

        if let Err(_v) = shared.events.push(req) {
            cold_path();
            shared.stats.events_dropped.fetch_add(1, Ordering::Relaxed);
        }

        decision
    }

    /// Checks the resident map for a compiled program without enqueuing an event.
    pub fn get_compiled(&self, code_hash: B256, spec_id: SpecId) -> Option<Arc<CompiledProgram>> {
        let key = RuntimeCacheKey { code_hash, spec_id };
        self.inner.shared.resident.get(&key).map(|entry| Arc::clone(&entry))
    }

    /// Like [`get_compiled`](Self::get_compiled), but also records hit/miss stats.
    pub fn get_compiled_tracked(
        &self,
        code_hash: B256,
        spec_id: SpecId,
    ) -> Option<Arc<CompiledProgram>> {
        let result = self.get_compiled(code_hash, spec_id);
        if result.is_some() {
            self.inner.shared.stats.lookup_hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.inner.shared.stats.lookup_misses.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    /// Looks up a compiled function, blocking until compilation completes if not yet ready.
    ///
    /// If the function is already compiled, returns it immediately. Otherwise, enqueues
    /// a synchronous JIT compilation request and blocks until the result is available.
    /// Returns [`LookupDecision::Unavailable`] if the bytecode is ineligible for
    /// compilation (see [`RuntimeTuning::should_compile`]) or compilation does not produce a
    /// resident program. Each such miss is counted in
    /// [`interpreter_fallbacks`](RuntimeStatsSnapshot::interpreter_fallbacks).
    pub fn lookup_blocking(&self, req: LookupRequest) -> LookupDecision {
        let decision = self.lookup_blocking_inner(req);
        if let LookupDecision::Unavailable(_) = decision {
            self.count_interpreter_fallback();
        }
        decision
    }

    fn lookup_blocking_inner(&self, req: LookupRequest) -> LookupDecision {
        if !self.inner.tuning.should_compile(&req.code) {
            return LookupDecision::Unavailable(LookupMissReason::Ineligible);
        }
        let code_hash = req.key.code_hash;
        let spec_id = req.key.spec_id;
        if let Some(program) = self.get_compiled_tracked(code_hash, spec_id) {
            return LookupDecision::Compiled(program);
        }
        if self.compile_jit_sync(req).is_err() {
            return LookupDecision::Unavailable(LookupMissReason::JitFailed);
        }
        match self.get_compiled_tracked(code_hash, spec_id) {
            Some(program) => LookupDecision::Compiled(program),
            None => LookupDecision::Unavailable(LookupMissReason::JitFailed),
        }
    }

    /// Enqueues an explicit JIT compilation request for the given bytecode.
    ///
    /// Blocks if the command channel is full to guarantee delivery. If the backend
    /// could not be started, the request is dropped and a warning is logged.
    pub fn compile_jit(&self, req: LookupRequest) {
        if !self.ensure_started_for_enqueue() {
            return;
        }
        let cmd = Command::CompileJit(CompileJitRequest {
            key: req.key,
            bytecode: req.code,
            sync_notifier: SyncNotifier::none(),
        });
        let _ = self.inner.tx.send(cmd);
    }

    /// Enqueues a JIT compilation request and blocks until the compilation completes.
    ///
    /// Returns `Ok(())` when the compiled function is available in the resident map,
    /// or when the compilation fails. Use [`get_compiled`](Self::get_compiled) to
    /// retrieve the result after this returns.
    pub fn compile_jit_sync(&self, req: LookupRequest) -> eyre::Result<()> {
        self.ensure_started()?;
        let (tx, rx) = chan::bounded(1);
        let cmd = Command::CompileJit(CompileJitRequest {
            key: req.key,
            bytecode: req.code,
            sync_notifier: SyncNotifier::new(tx),
        });
        self.inner.tx.send(cmd).map_err(|_| eyre::eyre!("backend channel closed"))?;
        self.recv_compile_completion(rx)
    }

    fn recv_compile_completion(&self, rx: chan::Receiver<()>) -> eyre::Result<()> {
        match rx.recv_timeout(self.inner.tuning.sync_compile_timeout) {
            Ok(()) => Ok(()),
            Err(chan::RecvTimeoutError::Timeout) => {
                eyre::bail!("timed out waiting for compilation to complete");
            }
            Err(chan::RecvTimeoutError::Disconnected) => {
                eyre::bail!("backend shut down before compilation completed");
            }
        }
    }

    /// Enqueues a single AOT preparation request.
    ///
    /// Compilation happens asynchronously on the worker pool. The resulting artifact is
    /// persisted via [`ArtifactStore::store`] and loaded into the resident map.
    /// May block during startup or when the command channel is full. If startup fails,
    /// the request is dropped and a warning is logged.
    pub fn prepare_aot(&self, req: AotRequest) {
        self.prepare_aot_batch(vec![req]);
    }

    /// Enqueues a single AOT preparation request and blocks until it completes.
    ///
    /// Returns `Ok(())` when the preparation attempt completed, whether it
    /// succeeded or failed. Use [`get_compiled`](Self::get_compiled) to retrieve
    /// the result after this returns.
    pub fn prepare_aot_sync(&self, req: AotRequest) -> eyre::Result<()> {
        self.prepare_aot_batch_sync(vec![req])
    }

    /// Enqueues a batch of AOT preparation requests.
    ///
    /// Blocks if the command channel is full to guarantee delivery. If the backend
    /// could not be started, the requests are dropped and a warning is logged.
    pub fn prepare_aot_batch(&self, reqs: Vec<AotRequest>) {
        if !self.ensure_started_for_enqueue() {
            return;
        }
        let owned: Vec<PrepareAotRequest> = reqs
            .into_iter()
            .map(|r| PrepareAotRequest {
                key: RuntimeCacheKey { code_hash: r.code_hash, spec_id: r.spec_id },
                bytecode: r.code,
                sync_notifier: SyncNotifier::none(),
            })
            .collect();
        let cmd = Command::PrepareAot(owned);
        let _ = self.inner.tx.send(cmd);
    }

    /// Enqueues a batch of AOT preparation requests and blocks until they complete.
    ///
    /// Returns `Ok(())` when all preparation attempts completed, whether they
    /// succeeded or failed. Use [`get_compiled`](Self::get_compiled) to retrieve
    /// the results after this returns.
    pub fn prepare_aot_batch_sync(&self, reqs: Vec<AotRequest>) -> eyre::Result<()> {
        self.ensure_started()?;
        if reqs.is_empty() {
            return Ok(());
        }

        let (tx, rx) = chan::bounded(reqs.len());
        let owned: Vec<PrepareAotRequest> = reqs
            .into_iter()
            .map(|r| PrepareAotRequest {
                key: RuntimeCacheKey { code_hash: r.code_hash, spec_id: r.spec_id },
                bytecode: r.code,
                sync_notifier: SyncNotifier::new(tx.clone()),
            })
            .collect();
        let completions = owned.len();
        let cmd = Command::PrepareAot(owned);
        self.inner.tx.send(cmd).map_err(|_| eyre::eyre!("backend channel closed"))?;
        for _ in 0..completions {
            self.recv_compile_completion(rx.clone())?;
        }
        Ok(())
    }

    /// Clears the in-memory resident compiled map.
    ///
    /// All compiled programs are removed from the map. Active references
    /// held by callers remain valid until dropped.
    ///
    /// Before startup, clears the map in place instead of queueing an undrained command.
    /// May block waiting for startup or for space in the running backend's command channel.
    pub fn clear_resident(&self) {
        self.send_clear(Command::ClearResident);
    }

    /// Clears persisted artifacts from the artifact store.
    ///
    /// Before startup, clears the store in place instead of queueing an undrained command.
    /// May block on store I/O, startup, or the running backend's command channel.
    pub fn clear_persisted(&self) {
        self.send_clear(Command::ClearPersisted);
    }

    /// Clears both the resident map and persisted artifacts.
    ///
    /// Before startup, clears both in place instead of queueing an undrained command.
    /// May block on store I/O, startup, or the running backend's command channel.
    pub fn clear_all(&self) {
        self.send_clear(Command::ClearAll);
    }

    /// Returns whether the runtime is enabled.
    pub fn enabled(&self) -> bool {
        self.inner.enabled.load(Ordering::Relaxed)
    }

    /// Pauses out-of-process helper execution.
    ///
    /// Resident compiled functions are still returned by [`lookup`](Self::lookup), and lookup
    /// events are still processed for stats, hotness tracking, and compilation dispatch. In
    /// out-of-process mode, the helper process group is stopped until the pause depth returns to
    /// zero, so dispatched helper requests remain buffered and resume once the helper continues.
    /// In in-process mode, pause only tracks pause depth. In
    /// [`blocking`](RuntimeConfig::blocking) mode the helper is never stopped, because the caller
    /// itself waits on compilation: pause only tracks pause depth there too.
    ///
    /// Pause delivery is best-effort and never blocks: callers pause around block validation on
    /// the engine's critical path. If the backend thread is not running or the command channel
    /// is full, the command is skipped or dropped instead of blocking.
    pub fn pause(&self) {
        if self.inner.shared.pause_depth.fetch_add(1, Ordering::Relaxed) == 0 {
            self.try_send_control(Command::Pause);
        }
    }

    /// Resumes out-of-process helper execution once all active pauses have been released.
    ///
    /// Resume delivery is best-effort and never blocks, like [`pause`](Self::pause).
    pub fn resume(&self) {
        if self
            .inner
            .shared
            .pause_depth
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |depth| Some(depth.saturating_sub(1)))
            .is_ok_and(|depth| depth == 1)
        {
            self.try_send_control(Command::Resume);
        }
    }

    /// Sends a best-effort control command without ever blocking the caller.
    ///
    /// The command channel is bounded, so a plain `send` can block. Pause/resume are issued on
    /// the caller's block-validation critical path and must never block there:
    ///
    /// - If the backend thread has not been started (the runtime was constructed but compilation
    ///   was never enabled), nothing drains the channel. Queued commands would only accumulate
    ///   until the channel fills and `send` blocks the caller forever, so the send is skipped
    ///   entirely — there is no helper to pause anyway.
    /// - In blocking mode the caller waits on the helper, so stopping it would hang lookups until
    ///   resume; the command is skipped.
    /// - If the channel is full, the command is dropped and recorded in
    ///   [`commands_dropped`](RuntimeStatsSnapshot::commands_dropped). Pause/resume signals are
    ///   idempotent and re-issued on the next pause cycle, so a dropped command self-heals.
    fn try_send_control(&self, cmd: Command) {
        if self.inner.blocking || !self.inner.started.load(Ordering::Relaxed) {
            return;
        }
        if self.inner.tx.try_send(cmd).is_err() {
            self.inner.shared.stats.commands_dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Returns whether out-of-process helper execution is paused.
    pub fn is_paused(&self) -> bool {
        self.inner.shared.pause_depth.load(Ordering::Relaxed) != 0
    }

    /// Sets whether the runtime is enabled, spawning the backend thread on first enable.
    ///
    /// When `enabled` is `true` and the backend thread has not been spawned yet, this
    /// lazily starts it using the config provided at construction time.
    pub fn set_enabled(&self, enabled: bool) -> eyre::Result<()> {
        debug!(enabled, "set_enabled");
        if enabled {
            self.ensure_started()?;
        }
        self.inner.enabled.store(enabled, Ordering::Relaxed);
        Ok(())
    }

    /// Returns a point-in-time snapshot of runtime statistics.
    pub fn stats(&self) -> RuntimeStatsSnapshot {
        self.inner.stats()
    }

    /// Spawns the backend thread if it hasn't been started yet.
    fn ensure_started(&self) -> eyre::Result<()> {
        self.ensure_started_with(std::thread::Builder::new())
    }

    fn ensure_started_with(&self, builder: std::thread::Builder) -> eyre::Result<()> {
        let mut guard = self.inner.lazy_spawn.lock().unwrap();
        let Some(lazy) = guard.take() else {
            return Ok(());
        };

        let LazySpawnState { rx, config } = lazy;

        debug!(
            blocking = self.inner.blocking,
            workers = config.tuning.jit_worker_count,
            jit_mode = ?config.jit_mode,
            hot_threshold = config.tuning.jit_hot_threshold,
            channel_capacity = config.tuning.channel_capacity,
            "spawning backend thread",
        );

        // Preload AOT artifacts into the already-allocated resident map.
        match Self::preload_aot(&config) {
            Ok(entries) => {
                for (key, prog) in entries {
                    self.inner.shared.resident.insert(key, prog);
                }
            }
            Err(e) => {
                // Restore lazy_spawn so a subsequent attempt can retry.
                *guard = Some(LazySpawnState { rx, config });
                return Err(e);
            }
        }

        let (done_tx, done_rx) = chan::bounded::<()>(1);
        let shared = Arc::clone(&self.inner.shared);
        let backend_rx = rx.clone();
        let backend_config = config.clone();

        let thread = match builder
            .name(config.thread_name.clone())
            .spawn(move || {
                let result = backend::run(shared, backend_rx, backend_config);
                let _ = done_tx.send(());
                result
            })
            .wrap_err("failed to spawn backend thread")
        {
            Ok(thread) => thread,
            Err(error) => {
                *guard = Some(LazySpawnState { rx, config });
                return Err(error);
            }
        };

        *self.inner.thread.lock().unwrap() = Some(BackendThread { handle: thread, done_rx });
        self.inner.started.store(true, Ordering::Relaxed);
        drop(guard);
        Ok(())
    }

    /// Preloads AOT artifacts from the store as `Arc<CompiledProgram>`s ready to insert.
    ///
    /// Skips entries that do not match the key this backend would persist and probe for their
    /// runtime key. They stay in the store, since another configuration may still use them.
    fn preload_aot(
        config: &RuntimeConfig,
    ) -> eyre::Result<Vec<(RuntimeCacheKey, Arc<CompiledProgram>)>> {
        let Some(store) = config.store.as_deref() else {
            debug!("no artifact store configured, skipping AOT preload");
            return Ok(Vec::new());
        };

        let span = info_span!("aot_preload");
        let _enter = span.enter();

        let artifacts = store.load_all()?;
        info!(count = artifacts.len(), "loading AOT artifacts");

        let config_hash = storage::compile_config_hash(config);
        let mut linker = JitObjectLinker::new(config.host.resolver);
        let mut out: Vec<(RuntimeCacheKey, Arc<CompiledProgram>)> =
            Vec::with_capacity(artifacts.len());
        let mut loaded = 0u64;
        let mut failed = 0u64;
        let mut skipped = 0u64;

        for (artifact_key, stored) in artifacts {
            let key = artifact_key.runtime;
            let expected = ArtifactKey::llvm(key, config.tuning.aot_opt_level, config_hash);
            if artifact_key != expected || stored.manifest.artifact_key != artifact_key {
                debug!(
                    ?artifact_key,
                    manifest_key = ?stored.manifest.artifact_key,
                    ?expected,
                    "artifact key mismatch, skipping",
                );
                skipped += 1;
                continue;
            }

            match linker.link_stored(key, &stored) {
                Ok(program) => {
                    out.push((key, Arc::new(program)));
                    loaded += 1;
                }
                Err(e) => {
                    warn!(
                        code_hash = %artifact_key.runtime.code_hash,
                        error = %e,
                        "failed to load artifact, skipping",
                    );
                    failed += 1;
                }
            }
        }

        info!(loaded, failed, skipped, "AOT preload complete");
        Ok(out)
    }

    /// Applies a clear before startup, or queues it after the backend takes ownership.
    /// The startup lock serializes this decision with AOT preload and thread creation.
    fn send_clear(&self, command: Command) {
        let guard = self.inner.lazy_spawn.lock().unwrap();
        let Some(lazy) = guard.as_ref() else {
            drop(guard);
            let _ = self.inner.tx.send(command);
            return;
        };

        if matches!(command, Command::ClearResident | Command::ClearAll) {
            self.inner.shared.resident.clear();
        }
        if matches!(command, Command::ClearPersisted | Command::ClearAll)
            && let Some(store) = lazy.config.store.as_deref()
            && let Err(error) = store.clear()
        {
            warn!(%error, "failed to clear artifact store");
        }
    }

    /// Starts the backend before enqueueing a command, logging startup failures.
    /// Failed startup retains an undrained receiver, so callers must not send to the
    /// bounded command channel until startup succeeds.
    fn ensure_started_for_enqueue(&self) -> bool {
        if let Err(err) = self.ensure_started() {
            warn!(%err, "failed to start JIT backend, dropping command");
            return false;
        }
        true
    }
}

impl BackendShared {
    /// Returns a point-in-time snapshot of runtime statistics.
    ///
    /// `command_queue_len` is omitted because the command channel sender lives
    /// on [`BackendInner`], not [`BackendShared`].
    pub(crate) fn stats(&self) -> RuntimeStatsSnapshot {
        self.stats.snapshot(stats::RuntimeStatsGauges {
            resident_entries: self.resident.len() as u64,
            events_queued: self.events.len() as u64,
            command_queue_len: 0,
        })
    }
}

impl BackendInner {
    /// Returns a point-in-time snapshot of runtime statistics.
    pub(crate) fn stats(&self) -> RuntimeStatsSnapshot {
        let mut snap = self.shared.stats();
        snap.command_queue_len = self.tx.len() as u64;
        snap
    }

    fn shutdown(&self) -> eyre::Result<()> {
        debug!("shutting down JIT backend");
        if let Some(ct) = self.thread.lock().unwrap().take() {
            // Ignoring send error — backend may already be gone.
            let _ = self.tx.send(Command::Shutdown);

            // Wait for the thread to signal completion, with a timeout.
            match ct.done_rx.recv_timeout(self.shutdown_timeout) {
                Ok(()) | Err(chan::RecvTimeoutError::Disconnected) => {}
                Err(chan::RecvTimeoutError::Timeout) => {
                    eyre::bail!(
                        "backend thread did not exit within timeout ({:?})",
                        self.shutdown_timeout
                    );
                }
            }

            // Thread signaled done, join should return immediately.
            ct.handle.join().map_err(|_| eyre::eyre!("backend thread panicked"))??;
        }
        Ok(())
    }
}

impl Drop for BackendInner {
    fn drop(&mut self) {
        if let Err(err) = self.shutdown() {
            warn!(%err, "failed to shutdown JIT backend");
        }
    }
}
