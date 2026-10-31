//! The process-shared JIT runner holder (ADR-089 phase 6): one
//! `JitBackend` per process, Arc-shared into per-block handles, gated by
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

use std::sync::{Arc, OnceLock};

use ::evm2::evm::InterpreterRunner;
use ::evm2::BaseEvmTypes;

static RUNNER: OnceLock<Option<Arc<dyn InterpreterRunner<BaseEvmTypes>>>> = OnceLock::new();

/// The process-shared interpreter runner for production sequence EVMs, or
/// `None` when the JIT is unavailable or unconfigured. Initialized once
/// from the resolved config: `simulation.sim_jit` arms it (blocking
/// compile mode — the parity-gate-friendly posture; the worker-pool
/// productionization follows in the pool task).
pub fn shared_runner() -> Option<Arc<dyn InterpreterRunner<BaseEvmTypes>>> {
    RUNNER.get_or_init(init).clone()
}

#[cfg(feature = "evm2-jit")]
fn init() -> Option<Arc<dyn InterpreterRunner<BaseEvmTypes>>> {
    if !degenbot_config::holder::config().simulation.sim_jit {
        return None;
    }
    runner_with_store(durable_store())
}

/// Builds the runner over `store`, or store-less when the backend refuses it.
///
/// `None` only when the JIT itself cannot start: a store the backend refuses
/// to preload (an unlinkable object, an unreadable directory) must not cost
/// the process its JIT — the retry is exactly the store-less backend, where
/// every miss compiles or interprets as before.
#[cfg(feature = "evm2-jit")]
fn runner_with_store(
    store: Option<Arc<dyn ::evm2_jit::runtime::ArtifactStore>>,
) -> Option<Arc<dyn InterpreterRunner<BaseEvmTypes>>> {
    use degenbot_core::op_warn;

    let had_store = store.is_some();
    let backend = match ::evm2_jit::runtime::JitBackend::new(backend_config(store)) {
        Ok(backend) => backend,
        Err(error) if had_store => {
            op_warn!(
                domain = sim,
                error = %error,
                "the AOT artifact store was rejected; the JIT continues without persisted artifacts",
            );
            ::evm2_jit::runtime::JitBackend::new(backend_config(None)).ok()?
        }
        Err(_) => return None,
    };
    Some(Arc::new(::evm2_jit::evm2_evm::JitInterpreterRunner::new(
        backend,
    )))
}

/// The production runner's backend config: blocking compile mode over the
/// durable artifact store when one opens.
#[cfg(feature = "evm2-jit")]
fn backend_config(
    store: Option<Arc<dyn ::evm2_jit::runtime::ArtifactStore>>,
) -> ::evm2_jit::runtime::RuntimeConfig {
    ::evm2_jit::runtime::RuntimeConfig {
        enabled: true,
        blocking: true,
        store,
        ..Default::default()
    }
}

/// Opens the durable AOT artifact store the prep lane (`aot-prepare-executor`)
/// fills.
///
/// A store that fails to open is a warn-and-continue `None`: a damaged cache
/// directory must never keep the engine from running, and the JIT is fine
/// without it (misses compile or interpret exactly as before).
#[cfg(feature = "evm2-jit")]
fn durable_store() -> Option<Arc<dyn ::evm2_jit::runtime::ArtifactStore>> {
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

#[cfg(not(feature = "evm2-jit"))]
fn init() -> Option<Arc<dyn InterpreterRunner<BaseEvmTypes>>> {
    None
}

#[cfg(all(test, feature = "evm2-jit"))]
#[expect(clippy::expect_used)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Component;
    use std::sync::Arc;

    use ::evm2_jit::eyre;
    use ::evm2_jit::runtime::{ArtifactKey, ArtifactManifest, ArtifactStore, StoredArtifact};

    use crate::sim::evm2::aot_store;

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

        let store = super::durable_store().expect("the durable store opens at the derived path");
        store.load_all().expect("the opened store lists");
    }

    #[test]
    fn a_broken_store_costs_the_artifacts_not_the_jit() {
        let runner = super::runner_with_store(Some(Arc::new(BrokenStore)));
        assert!(
            runner.is_some(),
            "a store the backend cannot preload must fall back to a store-less backend, \
             never to no runner"
        );
    }
}
