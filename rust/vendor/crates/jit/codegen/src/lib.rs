#![allow(clippy::needless_doctest_main)]
#![doc = include_str!("../README.md")]
#![cfg_attr(not(test), warn(unused_extern_crates))]
#![cfg_attr(docsrs, feature(doc_cfg))]

#[macro_use]
extern crate tracing;

use evm2_jit_backend::{eyre, *};
use evm2_jit_context::*;

mod bytecode;
pub use bytecode::*;

mod compiler;
pub use compiler::{CompileTimings, EvmCompiler, HostLayout};

mod linker;
pub use linker::{Linker, shared_library_path};

/// ABI version of compiled artifacts. Bump when the calling convention, the execution layout, the
/// `InstrStop` discriminants, or the stored artifact format change.
pub const ABI_VERSION: u32 = 7;

/// Internal tests and testing utilities. Not public API.
#[cfg(test)]
pub mod tests;

type FxHashMap<K, V> = alloy_primitives::map::HashMap<K, V, alloy_primitives::map::FxBuildHasher>;
type FxHashSet<K> = alloy_primitives::map::HashSet<K, alloy_primitives::map::FxBuildHasher>;

/// Enable for `cargo asm -p evm2_jit --lib`.
#[cfg(any())]
pub fn generate_all_assembly() -> EvmCompiler<evm2_jit_llvm::EvmLlvmBackend> {
    let mut compiler = EvmCompiler::new_llvm(false).unwrap();
    let _ = compiler.jit(None, &[], evm2::SpecId::LONDON).unwrap();
    unsafe { compiler.clear().unwrap() };
    compiler
}
