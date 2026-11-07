//! Public types and handle methods.

use crate::{
    EvmCompilerFn,
    runtime::{storage::RuntimeCacheKey, worker::JitCodeBacking},
};
use alloy_primitives::{B256, Bytes};
use evm2::SpecId;
use std::sync::Arc;

/// Request to look up a compiled function.
#[derive(Clone, Debug)]
pub struct LookupRequest {
    /// The cache key (code hash + spec).
    pub key: RuntimeCacheKey,
    /// The raw contract bytecode.
    pub code: Bytes,
}

/// Result of a lookup.
#[derive(Clone, Debug)]
pub enum LookupDecision {
    /// A compiled function is available.
    Compiled(Arc<CompiledProgram>),
    /// A compiled function is not available for this lookup.
    Unavailable(LookupMissReason),
}

/// Reason why a lookup did not return a compiled function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LookupMissReason {
    /// The runtime is disabled.
    Disabled,
    /// The contract is not eligible for compilation.
    Ineligible,
    /// No compiled program is ready for this key.
    NotReady,
    /// JIT compilation failed for this contract.
    JitFailed,
}

/// A compiled EVM program kept alive in the resident map.
pub struct CompiledProgram {
    /// The callable compiled function.
    pub func: EvmCompilerFn,
    /// The cache key this program was compiled for.
    pub key: RuntimeCacheKey,
    /// Whether this is an AOT or JIT program.
    pub kind: ProgramKind,
    /// Keeps the linked machine code alive.
    _backing: Arc<JitCodeBacking>,
}

impl std::fmt::Debug for CompiledProgram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledProgram")
            .field("key", &self.key)
            .field("kind", &self.kind)
            .field("func", &self.func)
            .finish_non_exhaustive()
    }
}

impl CompiledProgram {
    /// Creates a new compiled program backed by a linked AOT object.
    pub(crate) const fn new_aot(
        key: RuntimeCacheKey,
        func: EvmCompilerFn,
        backing: Arc<JitCodeBacking>,
    ) -> Self {
        Self { key, kind: ProgramKind::Aot, func, _backing: backing }
    }

    /// Creates a new compiled program backed by a JIT module's
    /// [`ResourceTracker`](crate::llvm::orc::ResourceTracker).
    pub(crate) const fn new_jit(
        key: RuntimeCacheKey,
        func: EvmCompilerFn,
        backing: Arc<JitCodeBacking>,
    ) -> Self {
        Self { key, kind: ProgramKind::Jit, func, _backing: backing }
    }
}

/// Request to prepare an AOT artifact.
#[derive(Clone, Debug)]
pub struct AotRequest {
    /// The code hash of the contract bytecode.
    pub code_hash: B256,
    /// The raw contract bytecode.
    pub code: Bytes,
    /// The EVM spec (hardfork) for compilation.
    pub spec_id: SpecId,
}

/// Whether this program was compiled AOT or JIT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgramKind {
    /// Loaded from a pre-compiled artifact at startup.
    Aot,
    /// Compiled at runtime by the JIT.
    Jit,
}
