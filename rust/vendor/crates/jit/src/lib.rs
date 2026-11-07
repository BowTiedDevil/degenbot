#![allow(clippy::needless_doctest_main)]
#![doc = include_str!("../README.md")]
#![cfg_attr(not(test), warn(unused_extern_crates))]
#![cfg_attr(docsrs, feature(doc_cfg))]

#[doc(inline)]
pub use evm2_jit_codegen::*;
#[doc(inline)]
pub use evm2_jit_runtime::{evm2_evm, runtime};

#[allow(ambiguous_glob_reexports)]
#[doc(inline)]
pub use evm2_jit_backend::*;
#[allow(ambiguous_glob_reexports)]
#[doc(inline)]
pub use evm2_jit_context::*;

#[cfg(feature = "llvm")]
#[doc(inline)]
pub use evm2_jit_llvm as llvm;
#[cfg(feature = "llvm")]
#[doc(no_inline)]
pub use llvm::EvmLlvmBackend;

#[doc(hidden)]
pub use evm2_jit_builtins as builtins;

#[doc(no_inline)]
pub use evm2::SpecId;
