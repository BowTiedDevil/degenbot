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
    let backend = ::evm2_jit::runtime::JitBackend::new(::evm2_jit::runtime::RuntimeConfig {
        enabled: true,
        blocking: true,
        ..Default::default()
    })
    .ok()?;
    Some(Arc::new(::evm2_jit::evm2_evm::JitInterpreterRunner::new(
        backend,
    )))
}

#[cfg(not(feature = "evm2-jit"))]
fn init() -> Option<Arc<dyn InterpreterRunner<BaseEvmTypes>>> {
    None
}
