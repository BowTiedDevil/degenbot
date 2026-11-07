//! Architecture-specific entry/exit trampolines for the JIT runtime.
//!
//! The entry trampoline ([`evm2_jit_entry`]) saves callee-saved registers, records
//! the stack pointer into [`EvmContext::exit_sp`](crate::EvmContext), and tail-calls
//! the JIT-compiled function.
//!
//! The exit trampoline ([`evm2_jit_exit`]) is called by builtins on error: it loads
//! [`EvmContext::exit_result`](crate::EvmContext), restores the saved stack pointer
//! and callee-saved registers, then returns directly to the caller of the entry
//! trampoline — bypassing all intermediate JIT and builtin frames.

use crate::EvmContext;

/// Offset of `EvmContext::exit_result`.
const EXIT_RESULT_OFFSET: usize = core::mem::offset_of!(EvmContext<'_, '_, '_>, exit_result);
/// Offset of `EvmContext::exit_sp`.
const EXIT_SP_OFFSET: usize = core::mem::offset_of!(EvmContext<'_, '_, '_>, exit_sp);

#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::evm2_jit_entry;
#[cfg(target_arch = "x86_64")]
pub use x86_64::*;

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::evm2_jit_entry;
#[cfg(target_arch = "aarch64")]
pub use aarch64::*;
