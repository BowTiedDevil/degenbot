use crate::{
    EvmCompilerFn,
    eyre::{self, WrapErr},
    runtime::{
        LookupRequest,
        api::{CompiledProgram, ProgramKind},
        config::{CompilationEvent, CompilationKind, RuntimeConfig, RuntimeTuning},
        storage::{
            ArtifactKey, ArtifactManifest, ArtifactStore, RuntimeCacheKey, StoredArtifact,
            compile_config_hash,
        },
        worker::{
            AotSuccess, CompileJob, JitCodeBacking, JitObjectSuccess, SyncNotifier, WorkerPool,
            WorkerResult, WorkerSuccess, builtin_symbol_names,
        },
    },
};
use alloy_primitives::{
    B256, Bytes, keccak256,
    map::{DefaultHashBuilder, HashMap, hash_map::Entry},
};
use crossbeam_channel as chan;
use crossbeam_queue::ArrayQueue;
use dashmap::DashMap;
use quanta::Instant;
#[cfg(feature = "llvm")]
use std::ffi::CString;
use std::{
    fs, mem,
    ops::ControlFlow,
    sync::{Arc, atomic::Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(feature = "llvm")]
use crate::llvm::jit_memory_usage;
#[cfg(feature = "llvm")]
use evm2_jit_context::RawEvmCompilerFn;

/// The resident map type: code_hash+spec_id → compiled program.
pub(crate) type ResidentMap = DashMap<RuntimeCacheKey, Arc<CompiledProgram>, DefaultHashBuilder>;

/// Bounded MPMC lock-free queue of lookup-observed events.
///
/// Producers (lookup hot path) push without blocking; on overflow the event
/// is silently dropped (`stats.events_dropped` is bumped). The backend
/// drains via `pop` on every loop iteration. Hotness signal is best-effort.
pub(crate) type EventQueue = ArrayQueue<LookupRequest>;

/// Per-entry metadata tracked alongside the resident map for eviction decisions.
struct ResidentMeta {
    /// When this entry was last hit by a lookup.
    last_hit_at: Instant,
}

/// Returns the total bytes of JIT-allocated memory via the memory plugin.
fn jit_total_bytes() -> usize {
    #[cfg(feature = "llvm")]
    {
        jit_memory_usage().map(|u| u.total_bytes()).unwrap_or(0)
    }
    #[cfg(not(feature = "llvm"))]
    {
        0
    }
}

#[cfg(feature = "llvm")]
pub(crate) struct JitObjectLinker {
    backend: Option<crate::EvmLlvmBackend>,
    /// Resolves the builtins of a custom host, which the helper leaves as symbols.
    resolver: Option<fn(evm2_jit_builtins::Builtin) -> usize>,
}

#[cfg(not(feature = "llvm"))]
pub(crate) struct JitObjectLinker;

#[cfg(feature = "llvm")]
impl JitObjectLinker {
    pub(crate) const fn new(resolver: Option<fn(evm2_jit_builtins::Builtin) -> usize>) -> Self {
        Self { backend: None, resolver }
    }

    fn link(
        &mut self,
        symbol_name: &str,
        object: &[u8],
        builtin_symbols: &[String],
    ) -> eyre::Result<(EvmCompilerFn, Arc<JitCodeBacking>)> {
        let backend = match &mut self.backend {
            Some(backend) => backend,
            None => self.backend.insert(crate::EvmLlvmBackend::new(false)?),
        };

        let symbol_name = CString::new(symbol_name)?;
        let builtin_symbols = builtin_symbols
            .iter()
            .map(|name| {
                // The helper suffixed the symbols with the resolver's address.
                let plain = self
                    .resolver
                    .and_then(|resolve| name.strip_suffix(&format!("_{:x}", resolve as usize)))
                    .unwrap_or(name);
                let builtin = evm2_jit_builtins::Builtin::parse(plain)
                    .ok_or_else(|| eyre::eyre!("unknown builtin symbol: {name}"))?;
                let addr = self.resolver.map_or_else(
                    || builtin.addr::<evm2::BaseEvmTypes>(),
                    |resolve| resolve(builtin),
                );
                Ok((CString::new(name.as_str())?, addr))
            })
            .collect::<eyre::Result<Vec<_>>>()?;
        let (addr, tracker, jd_guard) =
            backend.link_jit_object_in_fresh_dylib(&symbol_name, object, &builtin_symbols)?;
        let func =
            EvmCompilerFn::new(unsafe { std::mem::transmute::<usize, RawEvmCompilerFn>(addr) });
        Ok((func, Arc::new(JitCodeBacking::new(tracker, jd_guard))))
    }
}

#[cfg(not(feature = "llvm"))]
impl JitObjectLinker {
    pub(crate) const fn new(_resolver: Option<fn(evm2_jit_builtins::Builtin) -> usize>) -> Self {
        Self
    }

    fn link(
        &mut self,
        _symbol_name: &str,
        _object: &[u8],
        _builtin_symbols: &[String],
    ) -> eyre::Result<(EvmCompilerFn, Arc<JitCodeBacking>)> {
        eyre::bail!("LLVM backend not available");
    }
}

impl JitObjectLinker {
    /// Reads and links the object of a stored AOT artifact.
    pub(crate) fn link_stored(
        &mut self,
        key: RuntimeCacheKey,
        stored: &StoredArtifact,
    ) -> eyre::Result<CompiledProgram> {
        let object = fs::read(&stored.object_path)
            .wrap_err_with(|| format!("read {:?}", stored.object_path))?;
        // The object may reference any builtin of the base host.
        let (func, backing) =
            self.link(&stored.manifest.symbol_name, &object, &builtin_symbol_names(None))?;
        Ok(CompiledProgram::new_aot(key, func, backing))
    }
}

/// Commands sent to the backend thread on the bounded command channel.
///
/// Lookup-observed events are NOT carried here — they go through the
/// [`EventQueue`] to avoid waking the backend on every lookup.
pub(crate) enum Command {
    /// Explicit request to JIT-compile a bytecode.
    CompileJit(CompileJitRequest),
    /// Explicit request to prepare AOT artifacts.
    PrepareAot(Vec<PrepareAotRequest>),
    /// Clear the resident compiled map.
    ClearResident,
    /// Clear persisted artifacts from the artifact store.
    ClearPersisted,
    /// Clear both resident and persisted.
    ClearAll,
    /// Pause out-of-process helper execution.
    Pause,
    /// Resume out-of-process helper execution.
    Resume,
    /// Shut down the backend.
    Shutdown,
}

/// An explicit JIT compilation request.
pub(crate) struct CompileJitRequest {
    /// The key to compile for.
    pub(crate) key: RuntimeCacheKey,
    /// The raw bytecode.
    pub(crate) bytecode: Bytes,
    /// Optional notifier for synchronous callers.
    pub(crate) sync_notifier: SyncNotifier,
}

/// An explicit AOT preparation request.
pub(crate) struct PrepareAotRequest {
    /// The key to compile for.
    pub(crate) key: RuntimeCacheKey,
    /// The raw bytecode.
    pub(crate) bytecode: Bytes,
    /// Optional notifier for synchronous callers.
    pub(crate) sync_notifier: SyncNotifier,
}

/// Per-key state tracked by the backend.
struct EntryState {
    /// Number of observed misses.
    hotness: u32,
    /// Whether the last compile failed only because the helper went away under it.
    transient_failed: bool,
    /// Current phase.
    phase: EntryPhase,
    /// The bytecode for this key (captured from a miss event).
    bytecode: Bytes,
    /// When this entry was last observed.
    last_observed_at: Instant,
    /// Sync notifiers waiting for this entry to finish compiling.
    pending_notifiers: Vec<SyncNotifier>,
    /// Links in the cold-entry FIFO, which also queues failed entries; absent while compiling.
    cold_links: Option<ColdLinks>,
}

/// Phase of a backend entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryPhase {
    /// Not yet hot enough for JIT.
    Cold,
    /// JIT compilation in progress on a worker.
    Working,
    /// Compilation of this kind failed at this time; the key is not retried for that kind until
    /// the entry is swept.
    Failed(Instant, CompilationKind),
}

/// Whether a JIT admission request was triggered by hot-path observation
/// (gated on hotness + cold-entry cap) or by an explicit user request
/// (unconditional, may carry a sync notifier).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AdmitMode {
    Observed,
    Explicit,
}

/// Links stored directly on cold entries, avoiding stale queue keys.
struct ColdLinks {
    prev: Option<RuntimeCacheKey>,
    next: Option<RuntimeCacheKey>,
}

/// All backend-thread-owned mutable state.
struct BackendState {
    /// Shared state (resident map, event queue, stats).
    inner: Arc<super::BackendShared>,
    /// Per-key metadata for eviction (backend-only).
    resident_meta: HashMap<RuntimeCacheKey, ResidentMeta>,
    /// Per-key tracking state (backend-only).
    entries: HashMap<RuntimeCacheKey, EntryState>,
    /// Oldest and newest cold entries in insertion order.
    cold_head: Option<RuntimeCacheKey>,
    cold_tail: Option<RuntimeCacheKey>,
    /// Worker pool for JIT compilation.
    workers: WorkerPool,
    /// Backend-thread-owned linker for out-of-process JIT objects.
    jit_object_linker: JitObjectLinker,
    /// Receiver for worker results.
    result_rx: chan::Receiver<WorkerResult>,
    /// Artifact store for persisted artifacts.
    store: Option<Arc<dyn ArtifactStore>>,
    /// Tuning knobs.
    tuning: RuntimeTuning,
    /// Digest of the compile-time configuration, see [`compile_config_hash`].
    config_hash: B256,
    /// Whether observed misses compile AOT artifacts instead of JIT code.
    aot: bool,
    /// Number of keys currently in Working phase.
    pending_jobs: usize,
    /// Monotonically increasing generation counter, bumped on clear/invalidation.
    generation: u64,
    /// Last time an eviction sweep was run.
    last_sweep: Instant,
    /// Optional user callback for compilation events.
    on_compilation: Option<Arc<dyn Fn(CompilationEvent) + Send + Sync>>,
}

impl BackendState {
    fn handle(&mut self, cmd: Command) -> ControlFlow<()> {
        match cmd {
            Command::CompileJit(req) => self.handle_compile_jit(req),
            Command::PrepareAot(reqs) => self.handle_prepare_aot(reqs),
            Command::ClearResident => self.handle_clear_resident(),
            Command::ClearPersisted => self.handle_clear_persisted(),
            Command::ClearAll => self.handle_clear_all(),
            Command::Pause => self.workers.pause(),
            Command::Resume => self.workers.resume(),
            Command::Shutdown => return ControlFlow::Break(()),
        }
        ControlFlow::Continue(())
    }

    fn tick(&mut self) {
        self.drain_events();
        self.run_eviction_sweep();
    }

    /// Drains all currently-queued lookup events.
    fn drain_events(&mut self) {
        // Cap per-iteration drain so a flood of events can't starve other
        // work (commands, worker results, sweeps). Surplus events stay in the
        // queue and are picked up next iteration.
        for _ in 0..self.tuning.max_events_per_drain {
            let Some(event) = self.inner.events.pop() else { break };
            self.handle_lookup_observed(event);
        }
    }

    fn handle_lookup_observed(&mut self, event: LookupRequest) {
        let hit = event.code.is_empty();
        if hit {
            self.inner.stats.lookup_hits.fetch_add(1, Ordering::Relaxed);
            if let Some(meta) = self.resident_meta.get_mut(&event.key) {
                meta.last_hit_at = Instant::now();
            }
        } else {
            self.inner.stats.lookup_misses.fetch_add(1, Ordering::Relaxed);
            let kind = if self.aot { CompilationKind::Aot } else { CompilationKind::Jit };
            self.try_admit(kind, event.key, event.code, SyncNotifier::none(), AdmitMode::Observed);
        }
    }

    fn handle_compile_jit(&mut self, req: CompileJitRequest) {
        let kind = if self.aot { CompilationKind::Aot } else { CompilationKind::Jit };
        self.try_admit(kind, req.key, req.bytecode, req.sync_notifier, AdmitMode::Explicit);
    }

    fn handle_prepare_aot(&mut self, reqs: Vec<PrepareAotRequest>) {
        for req in reqs {
            self.try_admit(
                CompilationKind::Aot,
                req.key,
                req.bytecode,
                req.sync_notifier,
                AdmitMode::Explicit,
            );
        }
    }

    /// Common admission path for JIT and AOT compilation requests.
    ///
    /// Handles the cold→working state machine, hotness gating, in-flight
    /// dedup, persisted AOT probing, and worker dispatch. Observed promotion
    /// is gated by hotness; explicit requests are unconditional.
    fn try_admit(
        &mut self,
        kind: CompilationKind,
        key: RuntimeCacheKey,
        bytecode: Bytes,
        sync_notifier: SyncNotifier,
        mode: AdmitMode,
    ) {
        // A resident JIT program still needs an AOT artifact to be persisted.
        if self
            .inner
            .resident
            .get(&key)
            .is_some_and(|p| p.kind == ProgramKind::Aot || kind == CompilationKind::Jit)
        {
            sync_notifier.notify();
            return;
        }

        if !self.tuning.should_compile(&bytecode) {
            sync_notifier.notify();
            return;
        }

        if matches!(mode, AdmitMode::Observed)
            && !self.entries.contains_key(&key)
            && self.entries.len() >= self.max_entries()
            && !self.evict_oldest_cold_entry()
        {
            return;
        }

        if kind == CompilationKind::Aot && self.try_load_persisted_aot(&key) {
            sync_notifier.notify();
            return;
        }

        let now = Instant::now();
        let inserted = match self.entries.entry(key) {
            Entry::Occupied(_) => false,
            Entry::Vacant(entry) => {
                entry.insert(EntryState {
                    hotness: 0,
                    transient_failed: false,
                    phase: EntryPhase::Cold,
                    bytecode,
                    last_observed_at: now,
                    pending_notifiers: Vec::new(),
                    cold_links: None,
                });
                true
            }
        };
        if inserted {
            self.link_cold_entry(&key);
        }
        let entry = self.entries.get_mut(&key).unwrap();
        entry.last_observed_at = now;

        if entry.phase == EntryPhase::Working {
            sync_notifier.retain_waiter(&mut entry.pending_notifiers);
            return;
        }

        if let EntryPhase::Failed(_, failed_kind) = entry.phase {
            if failed_kind == kind {
                sync_notifier.notify();
                return;
            }
            // The other kind may still succeed, so it starts over from cold.
            entry.phase = EntryPhase::Cold;
            entry.hotness = 0;
        }

        if matches!(mode, AdmitMode::Observed) {
            entry.hotness = entry.hotness.saturating_add(1);
            if (entry.hotness as usize) < self.tuning.jit_hot_threshold {
                return;
            }
        }

        if self.pending_jobs >= self.tuning.jit_max_pending_jobs {
            sync_notifier.notify();
            return;
        }

        let prefix = match kind {
            CompilationKind::Jit => "jit",
            CompilationKind::Aot => "aot",
        };
        let opt_level = match kind {
            CompilationKind::Jit => self.tuning.jit_opt_level,
            CompilationKind::Aot => self.tuning.aot_opt_level,
        };
        let symbol = format!("{prefix}_{:x}_{:?}", key.code_hash, key.spec_id);
        let job = CompileJob {
            kind,
            key,
            bytecode: entry.bytecode.clone(),
            symbol_name: symbol,
            opt_level,
            sync_notifier,
            generation: self.generation,
        };

        match self.workers.try_send(job) {
            Ok(()) => {
                debug!(
                    code_hash = %key.code_hash,
                    spec_id = ?key.spec_id,
                    ?kind,
                    hotness = entry.hotness,
                    pending_jobs = self.pending_jobs + 1,
                    "dispatched compilation",
                );
                entry.phase = EntryPhase::Working;
                self.unlink_cold_entry(&key);
                self.pending_jobs += 1;
                self.inner.stats.compilations_dispatched.fetch_add(1, Ordering::Relaxed);
            }
            Err(job) => {
                warn!(code_hash = %key.code_hash, "worker pool saturated, dropping request");
                job.sync_notifier.notify();
            }
        }
    }

    /// Tries to load an already-persisted AOT artifact from the store into the resident map.
    /// Returns `true` if the artifact was loaded successfully.
    fn try_load_persisted_aot(&mut self, key: &RuntimeCacheKey) -> bool {
        let store = match &self.store {
            Some(s) => s,
            None => return false,
        };

        let artifact_key = self.artifact_key(*key);

        match store.load(&artifact_key) {
            Ok(Some(stored)) if stored.manifest.artifact_key != artifact_key => {
                debug!(
                    ?artifact_key,
                    manifest_key = ?stored.manifest.artifact_key,
                    "artifact manifest key mismatch, will recompile",
                );
                false
            }
            Ok(Some(stored)) => match self.jit_object_linker.link_stored(*key, &stored) {
                Ok(program) => {
                    debug!(
                        code_hash = %key.code_hash,
                        spec_id = ?key.spec_id,
                        "loaded existing AOT artifact from store, skipping recompilation",
                    );
                    self.insert_resident(*key, Arc::new(program));
                    true
                }
                Err(e) => {
                    warn!(
                        code_hash = %key.code_hash,
                        error = %e,
                        "failed to load persisted AOT artifact, will recompile",
                    );
                    false
                }
            },
            Ok(None) => false,
            Err(e) => {
                warn!(
                    code_hash = %key.code_hash,
                    error = %e,
                    "failed to probe artifact store",
                );
                false
            }
        }
    }

    fn handle_clear_resident(&mut self) {
        self.workers.cancel_in_flight();
        self.inner.resident.clear();
        self.resident_meta.clear();
        self.cold_head = None;
        self.cold_tail = None;
        // Notify any pending sync callers before clearing entries.
        for (_, entry) in self.entries.drain() {
            for n in entry.pending_notifiers {
                n.notify();
            }
        }
        // Discard pending lookup events: they were observed before the clear
        // and would otherwise get processed against the new generation.
        while self.inner.events.pop().is_some() {}
        // Bump generation so in-flight worker results from before the clear are discarded.
        self.generation += 1;
        debug!(generation = self.generation, "resident map cleared");
    }

    fn handle_clear_persisted(&mut self) {
        if let Some(store) = &self.store {
            if let Err(e) = store.clear() {
                warn!(error = %e, "failed to clear artifact store");
            } else {
                debug!("artifact store cleared");
            }
        }
    }

    fn handle_clear_all(&mut self) {
        self.handle_clear_resident();
        self.handle_clear_persisted();
    }

    fn insert_resident(&mut self, key: RuntimeCacheKey, program: Arc<CompiledProgram>) {
        self.inner.resident.insert(key, program);
        self.resident_meta.insert(key, ResidentMeta { last_hit_at: Instant::now() });
    }

    fn remove_resident(&mut self, key: &RuntimeCacheKey) {
        self.inner.resident.remove(key);
        self.resident_meta.remove(key);
    }

    fn handle_worker_result(&mut self, result: WorkerResult) {
        self.pending_jobs = self.pending_jobs.saturating_sub(1);

        // Discard stale results from a previous generation (e.g. after clear). Any entry for the
        // key belongs to the current generation, because clearing drains every entry, so leave it
        // and its waiters alone: it is the dedup for a job that is still running.
        if result.generation != self.generation {
            debug!(
                code_hash = %result.key.code_hash,
                result_gen = result.generation,
                current_gen = self.generation,
                "discarding stale worker result",
            );
            self.inner.stats.compilations_discarded.fetch_add(1, Ordering::Relaxed);
            result.sync_notifier.notify();
            return;
        }

        // Drain pending notifiers from the entry before processing.
        let pending_notifiers = self
            .entries
            .get_mut(&result.key)
            .map(|e| mem::take(&mut e.pending_notifiers))
            .unwrap_or_default();

        let notify = || {
            result.sync_notifier.notify();
            for n in pending_notifiers {
                n.notify();
            }
        };

        let kind = result.kind;
        let success = result.outcome.is_ok();

        if let Some(cb) = &self.on_compilation {
            cb(CompilationEvent {
                code_hash: result.key.code_hash,
                spec_id: result.key.spec_id,
                duration: result.compile_duration,
                kind,
                success,
                timings: result.timings,
            });
        }

        match result.outcome {
            Ok(WorkerSuccess::Jit(success)) => {
                let program =
                    Arc::new(CompiledProgram::new_jit(result.key, success.func, success.backing));
                self.insert_resident(result.key, program);
                self.remove_entry(&result.key);
                self.inner.stats.compilations_succeeded.fetch_add(1, Ordering::Relaxed);

                debug!(
                    code_hash = %result.key.code_hash,
                    spec_id = ?result.key.spec_id,
                    compile_time = ?result.compile_duration,
                    "JIT program published to resident map",
                );
            }
            Ok(WorkerSuccess::Aot(success)) => {
                self.handle_aot_success(result.key, success);
            }
            Ok(WorkerSuccess::JitObject(success)) => {
                self.handle_jit_object_success(result.key, success, result.compile_duration);
            }
            Err(err) => {
                self.fail_entry(&result.key, kind, result.transient);
                self.inner.stats.compilations_failed.fetch_add(1, Ordering::Relaxed);

                warn!(
                    code_hash = %result.key.code_hash,
                    error = %err,
                    compile_time = ?result.compile_duration,
                    "compilation failed",
                );
            }
        }

        notify();
    }

    fn handle_jit_object_success(
        &mut self,
        key: RuntimeCacheKey,
        success: JitObjectSuccess,
        compile_duration: std::time::Duration,
    ) {
        match self.jit_object_linker.link(
            &success.symbol_name,
            &success.object_bytes,
            &success.builtin_symbols,
        ) {
            Ok((func, backing)) => {
                let program = Arc::new(CompiledProgram::new_jit(key, func, backing));
                self.insert_resident(key, program);
                self.remove_entry(&key);
                self.inner.stats.compilations_succeeded.fetch_add(1, Ordering::Relaxed);

                debug!(
                    code_hash = %key.code_hash,
                    spec_id = ?key.spec_id,
                    compile_time = ?compile_duration,
                    object_len = success.object_bytes.len(),
                    "JIT object linked and published to resident map",
                );
            }
            Err(err) => {
                self.fail_entry(&key, CompilationKind::Jit, false);
                self.inner.stats.compilations_failed.fetch_add(1, Ordering::Relaxed);

                warn!(
                    code_hash = %key.code_hash,
                    error = %err,
                    compile_time = ?compile_duration,
                    "failed to link JIT object",
                );
            }
        }
    }

    fn handle_aot_success(&mut self, key: RuntimeCacheKey, success: AotSuccess) {
        let artifact_key = self.artifact_key(key);

        let content_hash = keccak256(&success.object_bytes).0;

        let manifest = ArtifactManifest {
            artifact_key,
            symbol_name: success.symbol_name.clone(),
            bytecode_len: success.bytecode_len,
            artifact_len: success.object_bytes.len(),
            created_at_unix_secs: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            content_hash,
        };

        // Persist to store if available.
        if let Some(store) = &self.store {
            if let Err(e) = store.store(&artifact_key, &manifest, &success.object_bytes) {
                warn!(
                    code_hash = %key.code_hash,
                    error = %e,
                    "failed to persist AOT artifact",
                );
                self.fail_entry(&key, CompilationKind::Aot, false);
                self.inner.stats.compilations_failed.fetch_add(1, Ordering::Relaxed);
                return;
            }

            debug!(
                code_hash = %key.code_hash,
                spec_id = ?key.spec_id,
                object_len = success.object_bytes.len(),
                "AOT artifact persisted to store",
            );

            // Load from store to get the canonical path, then link.
            match store.load(&artifact_key) {
                Ok(Some(stored)) => match self.jit_object_linker.link_stored(key, &stored) {
                    Ok(program) => {
                        self.insert_resident(key, Arc::new(program));
                        self.remove_entry(&key);
                        self.inner.stats.compilations_succeeded.fetch_add(1, Ordering::Relaxed);

                        debug!(
                            code_hash = %key.code_hash,
                            spec_id = ?key.spec_id,
                            "AOT program linked into resident map",
                        );
                    }
                    Err(e) => {
                        warn!(
                            code_hash = %key.code_hash,
                            error = %e,
                            "failed to link persisted AOT artifact",
                        );
                        // Back off AOT attempts; admission still allows a JIT request to retry.
                        self.fail_entry(&key, CompilationKind::Aot, false);
                        self.inner.stats.compilations_failed.fetch_add(1, Ordering::Relaxed);
                    }
                },
                Ok(None) => {
                    warn!(
                        code_hash = %key.code_hash,
                        "stored AOT artifact not found on reload",
                    );
                    self.fail_entry(&key, CompilationKind::Aot, false);
                    self.inner.stats.compilations_failed.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => {
                    warn!(
                        code_hash = %key.code_hash,
                        error = %e,
                        "failed to reload persisted AOT artifact",
                    );
                    self.fail_entry(&key, CompilationKind::Aot, false);
                    self.inner.stats.compilations_failed.fetch_add(1, Ordering::Relaxed);
                }
            }
        } else {
            // Only reachable through explicit `prepare_aot` requests; AOT mode requires a store.
            warn!(
                code_hash = %key.code_hash,
                "AOT compilation completed but no artifact store configured",
            );
            self.fail_entry(&key, CompilationKind::Aot, false);
            self.inner.stats.compilations_failed.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Runs an eviction sweep: removes idle entries and enforces the memory budget.
    fn run_eviction_sweep(&mut self) {
        if !self.should_sweep() {
            return;
        }

        let now = Instant::now();
        self.last_sweep = now;

        let idle_duration = self.tuning.idle_evict_duration;
        let budget = self.tuning.resident_code_cache_bytes;

        // Phase 1: evict idle entries.
        if let Some(idle) = idle_duration {
            let idle_keys: Vec<RuntimeCacheKey> = self
                .resident_meta
                .iter()
                .filter(|(_, meta)| now.duration_since(meta.last_hit_at) > idle)
                .map(|(key, _)| *key)
                .collect();

            for key in &idle_keys {
                debug!(
                    code_hash = %key.code_hash,
                    spec_id = ?key.spec_id,
                    "evicting idle entry",
                );
                self.remove_resident(key);
                self.remove_evicted_entry(key);
                self.inner.stats.evictions.fetch_add(1, Ordering::Relaxed);
            }
        }

        // Phase 2: evict stale cold entries that never became hot enough to compile, and failed
        // entries once the idle duration has passed since the failure.
        if let Some(idle) = idle_duration {
            let stale_keys = self
                .entries
                .iter()
                .filter(|(key, entry)| match entry.phase {
                    EntryPhase::Cold => {
                        now.duration_since(entry.last_observed_at) > idle
                            && !self.inner.resident.contains_key(key)
                    }
                    EntryPhase::Working => false,
                    EntryPhase::Failed(at, _) => now.duration_since(at) > idle,
                })
                .map(|(key, _)| *key)
                .collect::<Vec<_>>();
            for key in stale_keys {
                self.remove_entry(&key);
            }
        }

        // Phase 3: enforce memory budget by evicting LRU entries.
        if budget > 0 && jit_total_bytes() > budget {
            // Collect entries sorted by last_hit_at ascending (oldest first). AOT entries count
            // too: their linked objects are part of `jit_total_bytes()`, and an evicted one
            // reloads from the store.
            let mut entries: Vec<(RuntimeCacheKey, Instant)> =
                self.resident_meta.iter().map(|(key, meta)| (*key, meta.last_hit_at)).collect();
            entries.sort_by_key(|(_, t)| *t);

            for (key, _) in entries {
                if jit_total_bytes() <= budget {
                    break;
                }
                debug!(
                    code_hash = %key.code_hash,
                    spec_id = ?key.spec_id,
                    "evicting entry to stay within memory budget",
                );
                self.remove_resident(&key);
                self.remove_evicted_entry(&key);
                self.inner.stats.evictions.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Returns whether eviction is configured and a sweep is due.
    fn should_sweep(&self) -> bool {
        // Over budget — sweep immediately regardless of interval.
        let maxrss = self.tuning.resident_code_cache_bytes;
        if maxrss > 0 && jit_total_bytes() > maxrss {
            return true;
        }
        self.tuning.idle_evict_duration.is_some()
            && self.last_sweep.elapsed() >= self.tuning.eviction_sweep_interval
    }

    /// Evicts the first-in cold or failed entry with constant-time queue maintenance.
    fn evict_oldest_cold_entry(&mut self) -> bool {
        let Some(key) = self.cold_head else { return false };
        // Queued entries never accumulate waiters: those belong to working entries.
        debug_assert_ne!(self.entries[&key].phase, EntryPhase::Working);
        debug_assert!(self.entries[&key].pending_notifiers.is_empty());
        self.remove_entry(&key);
        true
    }

    const fn max_entries(&self) -> usize {
        self.tuning.jit_max_pending_jobs.saturating_mul(10)
    }

    /// Appends an unlinked entry to the cold FIFO.
    fn link_cold_entry(&mut self, key: &RuntimeCacheKey) {
        let prev = self.cold_tail;
        self.entries.get_mut(key).unwrap().cold_links = Some(ColdLinks { prev, next: None });
        if let Some(tail) = prev {
            self.entries.get_mut(&tail).unwrap().cold_links.as_mut().unwrap().next = Some(*key);
        } else {
            self.cold_head = Some(*key);
        }
        self.cold_tail = Some(*key);
    }

    /// Queues a failed working entry for eviction under the entry cap.
    ///
    /// The entry is marked failed, except after a first transient failure, which returns it to
    /// cold: a job that crashes its helper looks the same as one that lost its helper.
    fn fail_entry(&mut self, key: &RuntimeCacheKey, kind: CompilationKind, transient: bool) {
        let Some(entry) = self.entries.get_mut(key) else { return };
        if transient && !mem::replace(&mut entry.transient_failed, true) {
            entry.phase = EntryPhase::Cold;
            entry.hotness = 0;
        } else {
            entry.phase = EntryPhase::Failed(Instant::now(), kind);
        }
        self.link_cold_entry(key);
        // Explicit requests are not capped on admission, so cap their failures here.
        while self.entries.len() > self.max_entries() && self.evict_oldest_cold_entry() {}
    }

    /// Unlinks a cold entry on dispatch or removal, without scanning the table.
    fn unlink_cold_entry(&mut self, key: &RuntimeCacheKey) {
        let Some(links) = self.entries.get_mut(key).and_then(|entry| entry.cold_links.take())
        else {
            return;
        };
        if let Some(prev) = links.prev {
            self.entries.get_mut(&prev).unwrap().cold_links.as_mut().unwrap().next = links.next;
        } else {
            self.cold_head = links.next;
        }
        if let Some(next) = links.next {
            self.entries.get_mut(&next).unwrap().cold_links.as_mut().unwrap().prev = links.prev;
        } else {
            self.cold_tail = links.prev;
        }
    }

    /// Removes tracking state and its FIFO links together.
    fn remove_entry(&mut self, key: &RuntimeCacheKey) {
        self.unlink_cold_entry(key);
        self.entries.remove(key);
    }

    /// Removes the entry of an evicted resident key unless a job is still in flight for it.
    ///
    /// A working entry is the only deduplication for a job that cannot be cancelled, such as an
    /// AOT job for a JIT-resident key.
    fn remove_evicted_entry(&mut self, key: &RuntimeCacheKey) {
        if self.entries.get(key).is_none_or(|entry| entry.phase != EntryPhase::Working) {
            self.remove_entry(key);
        }
    }
    /// Returns the full key under which this backend persists and probes AOT artifacts.
    const fn artifact_key(&self, key: RuntimeCacheKey) -> ArtifactKey {
        ArtifactKey::llvm(key, self.tuning.aot_opt_level, self.config_hash)
    }
}

/// Runs the backend event loop. Called on the backend thread.
pub(crate) fn run(
    inner: Arc<super::BackendShared>,
    cmd_rx: chan::Receiver<Command>,
    config: RuntimeConfig,
) -> eyre::Result<()> {
    debug!("backend thread started");

    let (result_tx, result_rx) = chan::unbounded::<WorkerResult>();

    let workers = WorkerPool::new(result_tx, config.clone(), Arc::clone(&inner.stats));

    let sweep_interval = config.tuning.eviction_sweep_interval;
    let event_drain_interval = config.tuning.event_drain_interval;
    let config_hash = compile_config_hash(&config);

    // Seed resident metadata from startup-preloaded AOT entries.
    let now = Instant::now();
    let mut preload_meta = HashMap::default();
    for entry in inner.resident.iter() {
        preload_meta.insert(*entry.key(), ResidentMeta { last_hit_at: now });
    }

    let mut state = BackendState {
        inner,
        resident_meta: preload_meta,
        entries: HashMap::default(),
        cold_head: None,
        cold_tail: None,
        workers,
        jit_object_linker: JitObjectLinker::new(config.host.resolver),
        result_rx,
        store: config.store,
        tuning: config.tuning,
        config_hash,
        aot: config.aot,
        pending_jobs: 0,
        generation: 0,
        last_sweep: now,
        on_compilation: config.on_compilation,
    };

    // Tick interval is min(event_drain, sweep) so we never sleep longer than
    // either. Events are drained on every wakeup regardless of cause.
    let tick = event_drain_interval.min(sweep_interval);
    let shutdown_reason;

    loop {
        chan::select! {
            recv(cmd_rx) -> msg => {
                let Ok(cmd) = msg else {
                    shutdown_reason = "channel closed";
                    break;
                };
                if state.handle(cmd).is_break() {
                    shutdown_reason = "shutdown command";
                    break;
                }
            }
            recv(state.result_rx) -> msg => {
                match msg {
                    Ok(result) => state.handle_worker_result(result),
                    Err(_) => warn!("worker unexpectedly closed"),
                }
            }
            default(tick) => {}
        }
        state.tick();
    }

    debug!(?shutdown_reason, stats = ?state.inner.stats(), "backend task shutting down");

    let shutdown = state.workers.shutdown();
    while state.result_rx.try_recv().is_ok() {}
    shutdown
}

#[cfg(test)]
mod notifier_tests {
    use super::*;
    use crate::runtime::{BackendShared, stats::RuntimeStats};
    use evm2::{SpecId, interpreter::op};
    use std::{sync::atomic::AtomicUsize, time::Duration};

    /// Creates backend state without starting compiler workers.
    fn new_state(tuning: RuntimeTuning) -> BackendState {
        let config = RuntimeConfig {
            tuning: RuntimeTuning { jit_worker_count: 0, ..tuning },
            ..RuntimeConfig::default()
        };
        let inner = Arc::new(BackendShared {
            resident: ResidentMap::default(),
            events: EventQueue::new(config.tuning.channel_capacity),
            pause_depth: AtomicUsize::new(0),
            stats: Arc::new(RuntimeStats::default()),
        });
        let (result_tx, result_rx) = chan::unbounded();
        let workers = WorkerPool::new(result_tx, config.clone(), Arc::clone(&inner.stats));
        BackendState {
            inner,
            resident_meta: HashMap::default(),
            entries: HashMap::default(),
            cold_head: None,
            cold_tail: None,
            workers,
            jit_object_linker: JitObjectLinker::new(config.host.resolver),
            result_rx,
            config_hash: compile_config_hash(&config),
            store: config.store,
            tuning: config.tuning,
            aot: false,
            pending_jobs: 0,
            generation: 0,
            last_sweep: Instant::now(),
            on_compilation: None,
        }
    }

    fn observe(state: &mut BackendState, i: u32) -> RuntimeCacheKey {
        let mut code = vec![op::PUSH4];
        code.extend_from_slice(&i.to_be_bytes());
        code.push(op::STOP);
        let bytecode = Bytes::from(code);
        let key = RuntimeCacheKey { code_hash: keccak256(&bytecode), spec_id: SpecId::CANCUN };
        state.handle_lookup_observed(LookupRequest { key, code: bytecode });
        key
    }

    #[test]
    fn working_entry_retains_only_real_waiters_and_notifies_on_completion() {
        // Seed an in-flight entry without starting a compiler: its completion must
        // not race the repeated observations or require an LLVM backend.
        let mut state = new_state(RuntimeTuning::default());
        let bytecode = Bytes::from_static(&[0x00]);
        let key = RuntimeCacheKey { code_hash: keccak256(&bytecode), spec_id: SpecId::CANCUN };
        state.entries.insert(
            key,
            EntryState {
                hotness: 1,
                transient_failed: false,
                phase: EntryPhase::Working,
                bytecode: bytecode.clone(),
                last_observed_at: Instant::now(),
                pending_notifiers: Vec::new(),
                cold_links: None,
            },
        );
        state.pending_jobs = 1;
        for _ in 0..10_000 {
            state.handle_lookup_observed(LookupRequest { key, code: bytecode.clone() });
        }
        assert_eq!(state.entries[&key].pending_notifiers.len(), 0);
        assert_eq!(state.entries[&key].pending_notifiers.capacity(), 0);

        let (first, first_rx) = chan::bounded(1);
        let (second, second_rx) = chan::bounded(1);
        for sender in [first, second] {
            state.handle_compile_jit(CompileJitRequest {
                key,
                bytecode: bytecode.clone(),
                sync_notifier: SyncNotifier::new(sender),
            });
        }
        for _ in 0..10_000 {
            state.handle_lookup_observed(LookupRequest { key, code: bytecode.clone() });
        }
        assert_eq!(state.entries[&key].pending_notifiers.len(), 2);
        assert_eq!(first_rx.try_recv(), Err(chan::TryRecvError::Empty));
        assert_eq!(second_rx.try_recv(), Err(chan::TryRecvError::Empty));
        assert_eq!(state.inner.stats.lookup_misses.load(Ordering::Relaxed), 20_000);
        assert_eq!(state.inner.stats.compilations_dispatched.load(Ordering::Relaxed), 0);

        // Failure still completes both the original request and the later waiters.
        let (original, original_rx) = chan::bounded(1);
        state.handle_worker_result(WorkerResult {
            key,
            outcome: Err("test compilation failure".to_owned()),
            kind: CompilationKind::Jit,
            sync_notifier: SyncNotifier::new(original),
            generation: 0,
            compile_duration: Duration::ZERO,
            timings: crate::CompileTimings::default(),
            transient: false,
        });
        assert_eq!(state.pending_jobs, 0);
        assert!(matches!(state.entries[&key].phase, EntryPhase::Failed(..)));
        for receiver in [original_rx, first_rx, second_rx] {
            assert_eq!(receiver.try_recv(), Ok(()));
            assert_eq!(receiver.try_recv(), Err(chan::TryRecvError::Disconnected));
        }
    }

    #[test]
    fn stale_result_leaves_current_generation_working_entry() {
        let mut state = new_state(RuntimeTuning::default());
        let bytecode = Bytes::from_static(&[0x00]);
        let key = RuntimeCacheKey { code_hash: keccak256(&bytecode), spec_id: SpecId::CANCUN };
        let (waiter, waiter_rx) = chan::bounded(1);
        state.generation = 1;
        state.entries.insert(
            key,
            EntryState {
                hotness: 8,
                transient_failed: false,
                phase: EntryPhase::Working,
                bytecode,
                last_observed_at: Instant::now(),
                pending_notifiers: vec![SyncNotifier::new(waiter)],
                cold_links: None,
            },
        );
        state.pending_jobs = 2;

        let (stale, stale_rx) = chan::bounded(1);
        state.handle_worker_result(WorkerResult {
            key,
            outcome: Err("stale".to_owned()),
            kind: CompilationKind::Aot,
            sync_notifier: SyncNotifier::new(stale),
            generation: 0,
            compile_duration: Duration::ZERO,
            timings: crate::CompileTimings::default(),
            transient: false,
        });

        assert_eq!(state.pending_jobs, 1);
        assert_eq!(state.entries[&key].phase, EntryPhase::Working);
        assert_eq!(state.entries[&key].pending_notifiers.len(), 1);
        assert_eq!(stale_rx.try_recv(), Ok(()));
        assert_eq!(waiter_rx.try_recv(), Err(chan::TryRecvError::Empty));
    }

    #[test]
    fn idle_sweep_keeps_working_entry_of_evicted_resident_key() {
        let mut state = new_state(RuntimeTuning {
            idle_evict_duration: Some(Duration::ZERO),
            eviction_sweep_interval: Duration::ZERO,
            ..Default::default()
        });
        let bytecode = Bytes::from_static(&[0x00]);
        let key = RuntimeCacheKey { code_hash: keccak256(&bytecode), spec_id: SpecId::CANCUN };
        state.resident_meta.insert(key, ResidentMeta { last_hit_at: Instant::now() });
        state.entries.insert(
            key,
            EntryState {
                hotness: 0,
                transient_failed: false,
                phase: EntryPhase::Working,
                bytecode,
                last_observed_at: Instant::now(),
                pending_notifiers: Vec::new(),
                cold_links: None,
            },
        );
        state.pending_jobs = 1;
        std::thread::sleep(Duration::from_millis(1));

        state.run_eviction_sweep();

        assert!(state.resident_meta.is_empty());
        assert_eq!(state.entries[&key].phase, EntryPhase::Working);
    }

    #[test]
    fn full_cold_table_evicts_first_inserted_cold_entry() {
        let mut state = new_state(RuntimeTuning {
            jit_hot_threshold: usize::MAX,
            jit_max_pending_jobs: 1,
            ..RuntimeTuning::default()
        });
        let keys = (0..10).map(|i| observe(&mut state, i)).collect::<Vec<_>>();
        // A repeated observation does not change FIFO position.
        observe(&mut state, 0);

        let new_key = observe(&mut state, 10);
        assert_eq!(state.entries.len(), 10);
        assert!(state.entries.contains_key(&new_key));
        assert!(!state.entries.contains_key(&keys[0]));
        assert!(keys[1..].iter().all(|key| state.entries.contains_key(key)));
    }

    #[test]
    fn cold_table_cap_saturates() {
        let mut state = new_state(RuntimeTuning {
            jit_hot_threshold: usize::MAX,
            jit_max_pending_jobs: usize::MAX,
            ..RuntimeTuning::default()
        });
        let key = observe(&mut state, 0);
        assert!(state.entries.contains_key(&key));
    }

    #[test]
    fn removing_and_reinserting_cold_entries_preserves_fifo() {
        let mut state =
            new_state(RuntimeTuning { jit_hot_threshold: usize::MAX, ..Default::default() });
        let keys = (0..4).map(|i| observe(&mut state, i)).collect::<Vec<_>>();
        // Remove the middle, head, and tail without leaving stale links.
        for i in [1, 0, 3] {
            state.remove_entry(&keys[i]);
        }
        assert_eq!(state.cold_head, Some(keys[2]));
        assert_eq!(state.cold_tail, Some(keys[2]));
        observe(&mut state, 1);
        assert!(state.evict_oldest_cold_entry());
        assert!(!state.entries.contains_key(&keys[2]));
        assert_eq!(state.cold_head, Some(keys[1]));
        assert!(state.evict_oldest_cold_entry());
        assert_eq!(state.cold_head, None);
        assert_eq!(state.cold_tail, None);
        assert!(!state.evict_oldest_cold_entry());
    }

    #[test]
    fn cold_fifo_does_not_evict_working_entries_or_waiters() {
        let mut state = new_state(RuntimeTuning {
            jit_hot_threshold: usize::MAX,
            jit_max_pending_jobs: 1,
            ..Default::default()
        });
        let working = observe(&mut state, 0);
        state.entries.get_mut(&working).unwrap().phase = EntryPhase::Working;
        state.unlink_cold_entry(&working);
        let (sender, receiver) = chan::bounded(1);
        state.entries.get_mut(&working).unwrap().pending_notifiers.push(SyncNotifier::new(sender));
        for i in 1..=20 {
            observe(&mut state, i);
        }
        assert_eq!(state.entries.len(), 10);
        assert_eq!(state.entries[&working].phase, EntryPhase::Working);
        assert_eq!(receiver.try_recv(), Err(chan::TryRecvError::Empty));
        while state.evict_oldest_cold_entry() {}
        assert_eq!(state.entries.len(), 1);
        // No cold candidate: deny a new key when the table is full of working entries.
        for i in 21..30 {
            let key = observe(&mut state, i);
            state.entries.get_mut(&key).unwrap().phase = EntryPhase::Working;
            state.unlink_cold_entry(&key);
        }
        let denied = observe(&mut state, 30);
        assert!(!state.entries.contains_key(&denied));
        assert_eq!(state.entries.len(), 10);
    }

    #[test]
    fn idle_sweep_and_clear_remove_fifo_links() {
        let mut state = new_state(RuntimeTuning {
            jit_hot_threshold: usize::MAX,
            idle_evict_duration: Some(Duration::ZERO),
            eviction_sweep_interval: Duration::ZERO,
            ..Default::default()
        });
        for i in 0..10 {
            observe(&mut state, i);
        }
        std::thread::sleep(Duration::from_millis(1));
        state.run_eviction_sweep();
        assert!(state.entries.is_empty());
        assert_eq!(state.cold_head, None);
        assert_eq!(state.cold_tail, None);
        let key = observe(&mut state, 0);
        assert_eq!(state.cold_head, Some(key));
        state.handle_clear_resident();
        assert_eq!(state.cold_head, None);
        assert_eq!(state.cold_tail, None);
        assert!(!state.evict_oldest_cold_entry());
        observe(&mut state, 0);
        assert!(state.evict_oldest_cold_entry());
    }

    #[test]
    fn failed_entries_stay_bounded_and_expire_with_idle_sweep() {
        let mut state = new_state(RuntimeTuning {
            jit_hot_threshold: usize::MAX,
            jit_max_pending_jobs: 1,
            idle_evict_duration: Some(Duration::ZERO),
            eviction_sweep_interval: Duration::ZERO,
            ..Default::default()
        });
        let keys = (0..10).map(|i| observe(&mut state, i)).collect::<Vec<_>>();
        for key in &keys {
            state.entries.get_mut(key).unwrap().phase = EntryPhase::Working;
            state.unlink_cold_entry(key);
            state.fail_entry(key, CompilationKind::Jit, false);
        }
        assert_eq!(state.cold_head, Some(keys[0]));
        assert_eq!(state.cold_tail, Some(keys[9]));

        // A full table evicts the oldest failed entry for a new key.
        let new_key = observe(&mut state, 10);
        assert_eq!(state.entries.len(), 10);
        assert!(!state.entries.contains_key(&keys[0]));
        assert!(state.entries.contains_key(&new_key));

        // The sweep drops failed entries once the idle duration has passed since the failure.
        std::thread::sleep(Duration::from_millis(1));
        state.run_eviction_sweep();
        assert!(state.entries.is_empty());
        assert_eq!(state.cold_head, None);
        assert_eq!(state.cold_tail, None);
    }

    #[test]
    fn failed_entry_ignores_observed_and_explicit_requests() {
        let mut state = new_state(RuntimeTuning { jit_hot_threshold: 1, ..Default::default() });
        let key = observe(&mut state, 0);
        state.entries.get_mut(&key).unwrap().phase = EntryPhase::Working;
        state.unlink_cold_entry(&key);
        state.pending_jobs = 1;
        state.fail_entry(&key, CompilationKind::Jit, false);
        state.pending_jobs = 0;

        let bytecode = state.entries[&key].bytecode.clone();
        for _ in 0..10 {
            state.handle_lookup_observed(LookupRequest { key, code: bytecode.clone() });
        }
        let (sender, receiver) = chan::bounded(1);
        state.handle_compile_jit(CompileJitRequest {
            key,
            bytecode,
            sync_notifier: SyncNotifier::new(sender),
        });

        assert_eq!(receiver.try_recv(), Ok(()));
        assert!(matches!(state.entries[&key].phase, EntryPhase::Failed(..)));
        assert_eq!(state.pending_jobs, 0);
        assert_eq!(state.inner.stats.compilations_dispatched.load(Ordering::Relaxed), 0);

        // A failure of one kind does not block the other.
        state.handle_prepare_aot(vec![PrepareAotRequest {
            key,
            bytecode: state.entries[&key].bytecode.clone(),
            sync_notifier: SyncNotifier::none(),
        }]);
        assert_eq!(state.entries[&key].phase, EntryPhase::Cold);
    }

    #[test]
    fn transient_failure_returns_to_cold_once_then_fails() {
        let mut state = new_state(RuntimeTuning { jit_hot_threshold: 2, ..Default::default() });
        let key = observe(&mut state, 0);
        let fail = |state: &mut BackendState, transient: bool| {
            let entry = state.entries.get_mut(&key).unwrap();
            entry.phase = EntryPhase::Working;
            entry.hotness = 2;
            state.unlink_cold_entry(&key);
            state.pending_jobs = 1;
            state.handle_worker_result(WorkerResult {
                key,
                outcome: Err("helper went away".to_owned()),
                kind: CompilationKind::Jit,
                sync_notifier: SyncNotifier::none(),
                generation: 0,
                compile_duration: Duration::ZERO,
                timings: crate::CompileTimings::default(),
                transient,
            });
        };

        // The first transient failure restarts hotness instead of failing the key.
        fail(&mut state, true);
        assert_eq!(state.entries[&key].phase, EntryPhase::Cold);
        assert_eq!(state.entries[&key].hotness, 0);
        assert_eq!(state.cold_head, Some(key));

        // A second consecutive transient failure marks it failed.
        fail(&mut state, true);
        assert!(matches!(state.entries[&key].phase, EntryPhase::Failed(..)));
        assert_eq!(state.cold_head, Some(key));
        assert_eq!(state.cold_tail, Some(key));
    }

    #[test]
    fn explicit_failures_stay_within_entry_cap() {
        let mut state = new_state(RuntimeTuning { jit_max_pending_jobs: 1, ..Default::default() });
        let keys = (0..12u8)
            .map(|i| {
                let bytecode = Bytes::from(vec![op::PUSH1, i, op::STOP]);
                let key =
                    RuntimeCacheKey { code_hash: keccak256(&bytecode), spec_id: SpecId::CANCUN };
                state.handle_compile_jit(CompileJitRequest {
                    key,
                    bytecode,
                    sync_notifier: SyncNotifier::none(),
                });
                key
            })
            .collect::<Vec<_>>();
        assert_eq!(state.entries.len(), 12);
        for key in &keys {
            state.entries.get_mut(key).unwrap().phase = EntryPhase::Working;
            state.unlink_cold_entry(key);
        }
        for key in &keys {
            state.fail_entry(key, CompilationKind::Jit, false);
        }
        assert_eq!(state.entries.len(), 10);
        assert!(keys[2..].iter().all(|key| state.entries.contains_key(key)));
        assert_eq!(state.cold_head, Some(keys[2]));
        assert_eq!(state.cold_tail, Some(keys[11]));
    }

    #[test]
    fn cold_fifo_stays_bounded_without_idle_sweeps() {
        let mut state = new_state(RuntimeTuning {
            jit_hot_threshold: usize::MAX,
            jit_max_pending_jobs: 1,
            idle_evict_duration: None,
            ..Default::default()
        });
        for i in 0..10_000 {
            observe(&mut state, i);
        }
        assert_eq!(state.entries.len(), 10);
        for i in 9_990..10_000 {
            let key = observe(&mut state, i);
            assert!(state.entries.contains_key(&key));
        }
        for _ in 0..10 {
            assert!(state.evict_oldest_cold_entry());
        }
        assert!(state.entries.is_empty());
        assert_eq!(state.cold_head, None);
        assert_eq!(state.cold_tail, None);
    }

    #[test]
    fn rejected_dispatch_keeps_entry_in_cold_fifo() {
        let mut state = new_state(RuntimeTuning { jit_hot_threshold: 1, ..Default::default() });
        // The helper starts no workers, so dispatch must fail without unlinking.
        let key = observe(&mut state, 0);
        assert_eq!(state.entries[&key].phase, EntryPhase::Cold);
        assert_eq!(state.cold_head, Some(key));
        assert!(state.evict_oldest_cold_entry());
        assert!(state.entries.is_empty());
    }

    #[test]
    fn successful_dispatch_unlinks_cold_entry_before_completion() {
        let tuning =
            RuntimeTuning { jit_hot_threshold: 2, jit_worker_count: 1, ..Default::default() };
        let mut state = new_state(tuning);
        let (result_tx, result_rx) = chan::unbounded();
        state.workers = WorkerPool::new(
            result_tx,
            RuntimeConfig { tuning, ..Default::default() },
            Arc::clone(&state.inner.stats),
        );
        state.result_rx = result_rx;
        let first = observe(&mut state, 0);
        let second = observe(&mut state, 1);
        observe(&mut state, 0);
        assert_eq!(state.entries[&first].phase, EntryPhase::Working);
        assert!(state.entries[&first].cold_links.is_none());
        assert_eq!(state.cold_head, Some(second));
        assert_eq!(state.cold_tail, Some(second));
        assert!(state.evict_oldest_cold_entry());
        assert!(!state.entries.contains_key(&second));
        assert!(state.entries.contains_key(&first));
        // Worker completion removes the working entry on success, or requeues it as failed
        // (no LLVM backend), and cannot damage the FIFO.
        let result = state.result_rx.recv_timeout(Duration::from_secs(30)).unwrap();
        state.handle_worker_result(result);
        let failed = state.entries.contains_key(&first).then_some(first);
        assert_eq!(state.entries.len(), usize::from(failed.is_some()));
        assert_eq!(state.cold_head, failed);
        assert_eq!(state.cold_tail, failed);
        state.workers.shutdown().unwrap();
    }

    #[test]
    #[ignore = "local admission microbenchmark"]
    fn cold_table_churn_benchmark() {
        println!("entry_bytes={}", std::mem::size_of::<EntryState>());
        for capacity in [1_000, 20_480] {
            for _ in 0..3 {
                let mut state = new_state(RuntimeTuning {
                    jit_hot_threshold: usize::MAX,
                    jit_max_pending_jobs: capacity / 10,
                    idle_evict_duration: None,
                    ..Default::default()
                });
                for i in 0..capacity as u32 {
                    observe(&mut state, i);
                }
                let start = std::time::Instant::now();
                for i in capacity as u32..capacity as u32 + 2_000 {
                    std::hint::black_box(observe(&mut state, i));
                }
                println!(
                    "capacity={capacity} new_keys=2000 elapsed_us={}",
                    start.elapsed().as_micros()
                );
                assert_eq!(state.entries.len(), capacity);
                let existing = capacity as u32 + 1_999;
                let start = std::time::Instant::now();
                for _ in 0..100_000 {
                    std::hint::black_box(observe(&mut state, existing));
                }
                println!(
                    "capacity={capacity} repeated_keys=100000 elapsed_us={}",
                    start.elapsed().as_micros()
                );
            }
        }
    }
}

#[cfg(all(test, feature = "llvm"))]
mod tests {
    use super::*;
    use crate::runtime::worker::{
        CompileJob, WorkerSuccess, compile_jit_object_artifact, create_compiler,
    };
    use evm2::SpecId;

    /// PUSH1 0x42 PUSH0 MSTORE PUSH1 0x20 PUSH0 RETURN.
    const BYTECODE_RET42: &[u8] = &[0x60, 0x42, 0x5f, 0x52, 0x60, 0x20, 0x5f, 0xf3];

    fn compile_jit_object(symbol_name: &str) -> JitObjectSuccess {
        let config = RuntimeConfig::default();
        let mut compiler = create_compiler(&config, true).unwrap();
        let job = CompileJob {
            kind: CompilationKind::Jit,
            key: RuntimeCacheKey { code_hash: keccak256(BYTECODE_RET42), spec_id: SpecId::CANCUN },
            bytecode: Bytes::from_static(BYTECODE_RET42),
            symbol_name: symbol_name.to_owned(),
            opt_level: config.tuning.jit_opt_level,
            sync_notifier: SyncNotifier::none(),
            generation: 0,
        };

        match compile_jit_object_artifact(&job, &mut compiler).unwrap() {
            WorkerSuccess::JitObject(success) => success,
            _ => unreachable!(),
        }
    }

    #[test]
    fn jit_object_linker_relinks_live_symbol_name() {
        let success = compile_jit_object("jit_duplicate_symbol");

        let mut linker = JitObjectLinker::new(None);
        let link = |linker: &mut JitObjectLinker| {
            linker.link(&success.symbol_name, &success.object_bytes, &success.builtin_symbols)
        };
        let (_first_func, _first_backing) = link(&mut linker).unwrap();
        let (_second_func, _second_backing) = link(&mut linker)
            .expect("same symbol should link while previous backing remains alive");
    }
}
