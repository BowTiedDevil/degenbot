//! The process-shared JIT runner holder (ADR-089 phase 6): one
//! `JitBackend` per process, Arc-shared into per-epoch handles, gated by
//! `simulation.sim_jit`.
//!
//! The runner type the engine holds is the evm2-core trait
//! (`InterpreterRunner`), not the JIT crate's types, so the engine compiles
//! identically with and without the `evm2-jit` feature: without the
//! feature (or with the key off) [`shared_runner`] is `None` and every EVM
//! runs the stock interpreter. Fail-soft is the runner's own contract —
//! any lookup miss interprets — so a miss can never diverge a verdict.
//!
//! The backend opens the durable AOT artifact store under the configured
//! `persistence.state_dir`, so a later process's first frame already executes
//! the artifact a prep run persisted. A store that cannot be opened or
//! linked costs the process its persisted artifacts, never its JIT: the
//! holder falls back to a store-less backend.
//!
//! # Compile mode: the worker pool
//!
//! Production runs the worker pool (`blocking: false`): a lookup miss returns
//! `NotReady`, that frame interprets, and the compiled program takes over a
//! later frame once the pool delivers it. `blocking: true` — every lookup
//! compiles synchronously before its frame runs — survives only as the
//! JIT-parity gate's configuration, because it makes the first frame wait on
//! LLVM, which no per-block latency budget can afford.
//!
//! # Tuning
//!
//! [`production_tuning`] sizes the pool from the fleet's declared fan-out
//! width (`simulation.pipeline_concurrency`: sims in flight per block) instead
//! of taking every upstream default. Each decision, and the default it
//! replaces, lives in `armed::runtime_tuning`'s field comments.
//!
//! # Out-of-process compilation: not wired
//!
//! The upstream runtime can also compile in a helper process
//! (`JitMode::OutOfProcess` + `maybe_run_jit_helper`), re-exec'd from the host
//! binary and driven over stdio. This backend stays in-process, because:
//!
//! - The helper entry must run in the HOST process at startup. Under the
//!   Python-driven bot the host is CPython, so `EVM2_JIT_HELPER=1` would brand
//!   a plain interpreter run that never calls the entry — the parent would
//!   wait out `jit_timeout` and restart a helper on every job.
//! - An explicit `jit_helper_path` could name a helper-capable binary, but the
//!   one console binary (`degenbot-cli`) is chartered to name no
//!   domain-engine dependency (`just check-cli-shell-purity`), so its entry
//!   point cannot reach the JIT crate; the Python shim that also lands in
//!   `run_args` would pay a CPython start plus an extension import per helper
//!   spawn.
//! - The isolation it buys is crash containment for compiling untrusted code,
//!   and this backend only submits bytecode that passes
//!   `RuntimeTuning::should_compile`, whose compile failure is already
//!   fail-soft (the frame interprets). Out-of-process is also incompatible
//!   with `aot: true`, the AOT prep lane's mode.
//!
//! So `jit_mode` stays `JitMode::InProcess`, and the helper's memory/CPU-limit
//! knobs stay at `0` (unused).
//!
//! # Telemetry
//!
//! [`shared_runner`] samples the backend's cumulative counters into the
//! pipeline census on every call ([`sample_telemetry`], greedy delta sampling)
//! and the backend's `on_compilation` callback reports each compilation attempt
//! as it completes, so the compile rate and compile time are visible without a
//! probe attached.

use std::sync::Arc;

use ::evm2::evm::InterpreterRunner;
use ::evm2::BaseEvmTypes;

/// The process-shared interpreter runner for production sequence EVMs, or
/// `None` when the JIT is unavailable or unconfigured. Initialized once from
/// the resolved config: `simulation.sim_jit` arms it (worker-pool compile
/// mode). Every call reports the backend's counters into the metrics census as
/// a side effect.
#[must_use]
pub fn shared_runner() -> Option<Arc<dyn InterpreterRunner<BaseEvmTypes>>> {
    #[cfg(feature = "evm2-jit")]
    {
        let holder = armed::holder()?;
        armed::sample_into_census(holder);
        Some(Arc::clone(&holder.runner))
    }
    #[cfg(not(feature = "evm2-jit"))]
    {
        None
    }
}

/// The armed backend's cumulative compile/lookup telemetry, or `None` when the
/// JIT is not armed in this process (feature off, or `simulation.sim_jit` off).
///
/// A probe/test accessor: the bot's metrics ride [`sample_telemetry`] and the
/// compile callback, which read the same snapshot.
#[cfg(feature = "evm2-jit")]
#[must_use]
pub fn backend_stats() -> Option<::evm2_jit::runtime::RuntimeStatsSnapshot> {
    armed::holder().map(|holder| holder.backend.stats())
}

/// Reports the armed backend's counter deltas and live gauges into the bot's
/// metrics census. [`shared_runner`] already samples on every call (each
/// per-epoch handle build); this entry point is for a host that wants a sample
/// from its own cadence. A quiet sample reports nothing.
#[cfg(feature = "evm2-jit")]
pub fn sample_telemetry() {
    if let Some(holder) = armed::holder() {
        armed::sample_into_census(holder);
    }
}

/// The production worker pool's tuning, sized from the resolved config — the
/// same value the armed backend is built with. Exposed for the offline
/// benchmarks that must measure the production configuration rather than
/// upstream defaults.
#[cfg(feature = "evm2-jit")]
#[must_use]
pub fn production_tuning() -> ::evm2_jit::runtime::RuntimeTuning {
    armed::runtime_tuning(armed::sim_fanout_width())
}

#[cfg(feature = "evm2-jit")]
mod armed {
    use std::sync::{Arc, Mutex, OnceLock};

    use ::evm2::evm::InterpreterRunner;
    use ::evm2::BaseEvmTypes;
    use ::evm2_jit::runtime::{
        ArtifactStore, CompilationEvent, CompilationKind, JitBackend, RuntimeConfig,
        RuntimeStatsSnapshot, RuntimeTuning,
    };
    use degenbot_bot::instruments::{record, CensusEvent};

    /// The process-shared holder: the backend (its telemetry and its durable
    /// store) plus the runner the engine holds.
    pub(super) struct Holder {
        pub(super) backend: JitBackend,
        pub(super) runner: Arc<dyn InterpreterRunner<BaseEvmTypes>>,
        /// The cumulative snapshot the previous telemetry sample reported.
        last_sample: Mutex<Option<RuntimeStatsSnapshot>>,
    }

    static HOLDER: OnceLock<Option<Arc<Holder>>> = OnceLock::new();

    /// The process-shared holder, initialized once from the resolved config.
    pub(super) fn holder() -> Option<&'static Arc<Holder>> {
        HOLDER.get_or_init(init).as_ref()
    }

    fn init() -> Option<Arc<Holder>> {
        if !degenbot_config::holder::config().simulation.sim_jit {
            return None;
        }
        holder_with_store(durable_store())
    }

    /// Builds the holder over `store`, or store-less when the backend refuses
    /// it.
    ///
    /// `None` only when the JIT itself cannot start: a store the backend
    /// refuses to preload (an unlinkable object, an unreadable directory) must
    /// not cost the process its JIT — the retry is exactly the store-less
    /// backend, where every miss compiles or interprets as before.
    pub(super) fn holder_with_store(store: Option<Arc<dyn ArtifactStore>>) -> Option<Arc<Holder>> {
        use degenbot_core::op_warn;

        let had_store = store.is_some();
        let backend = match JitBackend::new(backend_config(store)) {
            Ok(backend) => backend,
            Err(error) if had_store => {
                op_warn!(
                    domain = sim,
                    error = %error,
                    "the AOT artifact store was rejected; the JIT continues without persisted artifacts",
                );
                JitBackend::new(backend_config(None)).ok()?
            }
            Err(_) => return None,
        };
        let runner: Arc<dyn InterpreterRunner<BaseEvmTypes>> = Arc::new(
            ::evm2_jit::evm2_evm::JitInterpreterRunner::new(backend.clone()),
        );
        Some(Arc::new(Holder {
            backend,
            runner,
            last_sample: Mutex::new(None),
        }))
    }

    /// The production runner's backend config: worker-pool compile mode over
    /// the durable artifact store when one opens, tuned by [`runtime_tuning`],
    /// with the compile-telemetry callback installed.
    pub(super) fn backend_config(store: Option<Arc<dyn ArtifactStore>>) -> RuntimeConfig {
        let tuning = runtime_tuning(sim_fanout_width());
        let on_compilation: Arc<dyn Fn(CompilationEvent) + Send + Sync> =
            Arc::new(report_compilation);
        RuntimeConfig {
            enabled: true,
            blocking: false,
            store,
            tuning,
            on_compilation: Some(on_compilation),
            ..RuntimeConfig::default()
        }
    }

    /// `simulation.pipeline_concurrency` — sims in flight per block, the width
    /// every pool decision below is sized against (the schema clamps it to at
    /// least 1 at the use site, so this clamps too).
    pub(super) fn sim_fanout_width() -> usize {
        degenbot_config::holder::config()
            .simulation
            .pipeline_concurrency
            .max(1)
    }

    /// The worker pool's tuning, sized from the sim fan-out width.
    ///
    /// Two knobs track the fleet, because upstream's values are machine-generic
    /// in a backend that sits inside a declared per-block fan-out:
    ///
    /// - `jit_hot_threshold` (upstream 8) becomes the fan-out width: that is how
    ///   many sims touch the same bytecode inside ONE block, so a contract any
    ///   single block's candidate set exercises is promoted within that block's
    ///   own latency budget instead of after several blocks of counting. On the
    ///   reference fleet (width 8) the value coincides with the default;
    ///   a widened or narrowed fan-out tracks with it.
    /// - `jit_worker_count` (upstream `clamp(ceil(cpus/2), 1, 4)`) becomes half
    ///   the fan-out width, capped by that upstream ceiling: compilation
    ///   competes for the same cores the fan-out runs on, so the pool takes a
    ///   smaller slice than the sims it serves, and more workers than distinct
    ///   hot contracts would only idle threads.
    ///
    /// Every other knob stays at [`RuntimeTuning::default`], for these reasons:
    ///
    /// - `channel_capacity` / `max_events_per_drain` (4096): one lookup event
    ///   per executed frame. A full fan-out wave is a few dozen events, three
    ///   orders below the ring, and the overflow cost is only hotness credit
    ///   (the next wave re-counts).
    /// - `event_drain_interval` (100 ms): the promotion is dispatched within
    ///   100 ms of the miss; the compile itself costs hundreds of ms and only
    ///   helps LATER blocks anyway, so this granularity is not the binding term.
    /// - `jit_max_pending_jobs` (2048): it also sizes the hotness bookkeeping
    ///   table at 10x (20480 keys), the room the tracked contract set needs;
    ///   the in-flight job cap itself is never approached, since jobs are one
    ///   per distinct hot code hash.
    /// - `jit_worker_queue_capacity` (64 per worker): queued jobs are distinct
    ///   hot keys, not frames.
    /// - `jit_max_bytecode_len` (24 KiB): above the injected executor runtime
    ///   (~15 KiB, the hottest contract), so the legitimate set compiles; larger
    ///   one-offs interpret rather than paying a multi-second compile.
    /// - `resident_code_cache_bytes` (1 GiB), `idle_evict_duration` (600 s),
    ///   `eviction_sweep_interval` (60 s): a few hundred compiled pool contracts
    ///   cost tens of MB, so the byte budget only watches for a runaway, while
    ///   the idle sweep keeps one-off bytecode (an attacker's throwaway
    ///   contracts) from accumulating — the hot set is re-hit every block and
    ///   never idle.
    /// - `compiler_recycle_threshold` (1000): a process that runs for days
    ///   reclaims LLVM's accumulated memory periodically.
    /// - `sync_compile_timeout` (30 s), `jit_timeout` (5 s), and the helper
    ///   memory/CPU limits: the first is unreachable in pool mode (no
    ///   synchronous waits on the lookup path), and the rest belong to the
    ///   out-of-process helper this backend does not use.
    /// - the JIT/AOT optimization levels and the `single_error` / dedup / DSE /
    ///   debug-assertion switches: compiler correctness and speed knobs, not
    ///   fleet sizing.
    pub(super) fn runtime_tuning(fanout_width: usize) -> RuntimeTuning {
        let upstream = RuntimeTuning::default();
        RuntimeTuning {
            jit_hot_threshold: fanout_width,
            jit_worker_count: (fanout_width / 2).clamp(1, upstream.jit_worker_count),
            ..upstream
        }
    }

    /// Opens the durable AOT artifact store the prep lane
    /// (`aot-prepare-executor`) fills.
    ///
    /// A store that fails to open is a warn-and-continue `None`: a damaged
    /// cache directory must never keep the engine from running, and the JIT is
    /// fine without it (misses compile or interpret exactly as before).
    pub(super) fn durable_store() -> Option<Arc<dyn ArtifactStore>> {
        use degenbot_core::op_warn;

        let dir = crate::sim::evm2::aot_store::default_artifact_dir();
        match crate::sim::evm2::aot_store::DurableArtifactStore::new(&dir) {
            Ok(store) => Some(Arc::new(store)),
            Err(error) => {
                op_warn!(
                    domain = sim,
                    dir = %dir.display(),
                    error = %error,
                    "the AOT artifact store did not open; the JIT runs without persisted artifacts",
                );
                None
            }
        }
    }

    /// The backend's `on_compilation` callback: one census event per
    /// compilation attempt, carrying the event's own wall time.
    fn report_compilation(event: CompilationEvent) {
        record(CensusEvent::JitCompilation {
            kind: match event.kind {
                CompilationKind::Jit => "jit",
                CompilationKind::Aot => "aot",
            },
            success: event.success,
            secs: event.duration.as_secs_f64(),
        });
    }

    /// Reports the backend's counters as deltas since the previous sample, plus
    /// its live gauges. A quiet sample (no lookups since the last one, gauges
    /// unchanged) reports nothing, so the emit site may call this as
    /// often as it likes and the totals stay exact regardless of cadence.
    pub(super) fn sample_into_census(holder: &Holder) {
        let snapshot = holder.backend.stats();
        let mut last = match holder.last_sample.lock() {
            Ok(last) => last,
            // The sampler holds the lock only for pure arithmetic, so a
            // poisoned lock still carries a usable previous snapshot.
            Err(poisoned) => poisoned.into_inner(),
        };
        let previous = last.replace(snapshot);
        let hits = previous.map_or(snapshot.lookup_hits, |previous| {
            snapshot.lookup_hits.saturating_sub(previous.lookup_hits)
        });
        let misses = previous.map_or(snapshot.lookup_misses, |previous| {
            snapshot
                .lookup_misses
                .saturating_sub(previous.lookup_misses)
        });
        let gauges_changed = previous.is_none_or(|previous| {
            previous.resident_entries != snapshot.resident_entries
                || previous.jit_code_bytes != snapshot.jit_code_bytes
        });
        if hits == 0 && misses == 0 && !gauges_changed {
            return;
        }
        record(CensusEvent::JitBackendSample {
            lookup_hits: hits,
            lookup_misses: misses,
            resident_programs: snapshot.resident_entries,
            code_bytes: snapshot.jit_code_bytes,
        });
    }
}

#[cfg(all(test, feature = "evm2-jit"))]
#[expect(clippy::expect_used)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Component;
    use std::sync::Arc;

    use ::evm2_jit::eyre;
    use ::evm2_jit::runtime::{
        ArtifactKey, ArtifactManifest, ArtifactStore, JitBackend, LookupDecision, LookupMissReason,
        LookupRequest, RuntimeCacheKey, RuntimeConfig, RuntimeTuning, StoredArtifact,
    };
    use alloy::primitives::{keccak256, Bytes};

    use crate::sim::evm2::aot_store;
    use crate::sim::evm2::handle::EVM2_PRODUCTION_SPEC;

    /// A store every read fails on — the damaged cache directory.
    struct BrokenStore;

    impl ArtifactStore for BrokenStore {
        fn load_all(&self) -> eyre::Result<Vec<(ArtifactKey, StoredArtifact)>> {
            Err(eyre::eyre!("read artifact store directory failed"))
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

    /// A one-opcode program: compilable, and fast enough that the blocking
    /// contrast below costs one translation rather than a contract's worth.
    fn lookup_request() -> LookupRequest {
        let code = Bytes::from_static(&[0x00]);
        LookupRequest {
            key: RuntimeCacheKey {
                code_hash: keccak256(&code),
                spec_id: EVM2_PRODUCTION_SPEC,
            },
            code,
        }
    }

    #[test]
    fn the_holder_opens_the_durable_store_under_the_state_dir() {
        let dir = aot_store::default_artifact_dir();
        assert_eq!(
            dir.file_name(),
            Some(OsStr::new(aot_store::ARTIFACT_DIR_NAME)),
            "the store is the state root's artifact subdirectory: {}",
            dir.display()
        );
        assert!(
            !dir.components()
                .any(|component| matches!(component, Component::Normal(name) if name == "~")),
            "the state root expands instead of leaving a literal `~` component: {}",
            dir.display()
        );

        let store =
            super::armed::durable_store().expect("the durable store opens at the derived path");
        store.load_all().expect("the opened store lists");
    }

    #[test]
    fn a_broken_store_costs_the_artifacts_not_the_jit() {
        let holder = super::armed::holder_with_store(Some(Arc::new(BrokenStore)));
        assert!(
            holder.is_some(),
            "a store the backend cannot preload must fall back to a store-less backend, \
             never to no runner"
        );
    }

    /// The production backend starts in worker-pool mode: the config says
    /// `blocking: false`, and a cold lookup answers `NotReady` — the frame
    /// interprets instead of waiting on LLVM. The blocking contrast below pins
    /// the same lookup the other way, so this cannot pass on a backend that
    /// silently ignored the knob.
    #[test]
    fn the_production_backend_starts_in_pool_mode() {
        let config = super::armed::backend_config(None);
        assert!(
            !config.blocking,
            "production must never compile on the lookup path"
        );
        assert!(config.enabled, "the armed holder enables the backend");

        let backend = JitBackend::new(config).expect("the pool-mode backend starts");
        let decision = backend.lookup(lookup_request());
        assert!(
            matches!(
                decision,
                LookupDecision::Unavailable(LookupMissReason::NotReady)
            ),
            "a cold pool-mode lookup defers to the interpreter; got {decision:?}"
        );
    }

    /// The blocking mode the JIT-parity gate keeps is the opposite behaviour on
    /// the same lookup: it compiles before the frame runs.
    #[test]
    fn blocking_mode_compiles_where_the_pool_defers() {
        let backend = JitBackend::new(RuntimeConfig {
            enabled: true,
            blocking: true,
            ..RuntimeConfig::default()
        })
        .expect("the blocking backend starts");
        let decision = backend.lookup(lookup_request());
        assert!(
            matches!(decision, LookupDecision::Compiled(_)),
            "blocking mode compiles synchronously; got {decision:?}"
        );
    }

    /// The tuning is the fleet's, not upstream's machine-generic guess: the hot
    /// threshold is the declared fan-out width, the worker count is half of it
    /// capped by upstream's machine-aware ceiling, and everything else is the
    /// upstream default the tuning rationale argues for.
    #[test]
    fn the_production_tuning_tracks_the_sim_fanout() {
        let upstream = RuntimeTuning::default();
        for width in [1_usize, 2, 8, 64] {
            let tuning = super::armed::runtime_tuning(width);
            assert_eq!(tuning.jit_hot_threshold, width);
            assert_eq!(
                tuning.jit_worker_count,
                (width / 2).clamp(1, upstream.jit_worker_count),
                "workers track half the fan-out width, capped by upstream's ceiling"
            );
            assert!(
                tuning.jit_worker_count >= 1,
                "a pool with no worker never compiles"
            );
            assert_eq!(tuning.channel_capacity, upstream.channel_capacity);
            assert_eq!(tuning.max_events_per_drain, upstream.max_events_per_drain);
            assert_eq!(tuning.jit_max_pending_jobs, upstream.jit_max_pending_jobs);
            assert_eq!(tuning.jit_max_bytecode_len, upstream.jit_max_bytecode_len);
            assert_eq!(
                tuning.resident_code_cache_bytes,
                upstream.resident_code_cache_bytes
            );
        }

        let configured = degenbot_config::holder::config()
            .simulation
            .pipeline_concurrency
            .max(1);
        assert_eq!(
            super::armed::backend_config(None).tuning.jit_hot_threshold,
            configured,
            "the armed backend is tuned from simulation.pipeline_concurrency"
        );
    }

    /// The holder keeps the backend reachable for telemetry, and the sampler
    /// reads the compile it reports: a compile through the holder's backend
    /// lands in the snapshot the metrics sampler turns into deltas.
    #[test]
    fn the_holder_exposes_the_backend_compile_telemetry() {
        let holder = super::armed::holder_with_store(None).expect("the store-less backend starts");
        holder
            .backend
            .compile_jit_sync(lookup_request())
            .expect("a one-opcode program compiles");
        let stats = holder.backend.stats();
        assert!(
            stats.compilations_succeeded >= 1,
            "the adapter reports the backend's successful compilations: {stats:?}"
        );

        // The sampler reports first-seen counters, then goes quiet.
        super::armed::sample_into_census(&holder);
        super::armed::sample_into_census(&holder);
    }
}
