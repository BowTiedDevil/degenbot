#![doc = include_str!("../README.md")]
#![allow(clippy::needless_doctest_main)]
#![cfg_attr(not(test), warn(unused_extern_crates))]
#![cfg_attr(docsrs, feature(doc_cfg))]

#[macro_use]
extern crate tracing;

pub use ::eyre;
pub use evm2_jit_backend::OptimizationLevel;
pub use evm2_jit_codegen::*;

use evm2_jit_context::EvmCompilerFn;
#[cfg(feature = "llvm")]
use evm2_jit_llvm as llvm;
#[cfg(feature = "llvm")]
use evm2_jit_llvm::EvmLlvmBackend;

pub mod runtime;

pub mod evm2_evm;
