use super::{EXIT_RESULT_OFFSET, EXIT_SP_OFFSET};
use crate::{EvmContext, EvmStack, InstrStop, RawEvmCompilerFn};
use core::ptr::NonNull;
use evm2::EvmTypesHost;

/// Entry trampoline: saves callee-saved registers, stores RSP into
/// `ecx.exit_sp`, and calls the JIT function.
///
/// On normal return the JIT function's `InstrStop` is forwarded.
/// On abnormal exit (builtin error), [`evm2_jit_exit`] restores RSP and
/// returns the `exit_result` stored in `EvmContext`.
#[unsafe(naked)]
pub(crate) unsafe extern "C" fn evm2_jit_entry<T: EvmTypesHost>(
    ecx: NonNull<EvmContext<'_, '_, '_, T>>,
    stack: NonNull<EvmStack>,
    stack_len: NonNull<usize>,
    f: RawEvmCompilerFn,
) -> InstrStop {
    // System V AMD64: rdi=ecx, rsi=stack, rdx=stack_len, rcx=f
    // The JIT function takes (ecx, stack, stack_len) — already in rdi, rsi, rdx.
    core::arch::naked_asm!(
        // Save callee-saved registers.
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        // Save RSP into ecx->exit_sp (after pushes, before alignment).
        "mov [rdi + {exit_sp}], rsp",
        // Align stack to 16 bytes for the call.
        "sub rsp, 8",
        "call rcx",
        // Normal return: eax = InstrStop.
        "add rsp, 8",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "ret",
        exit_sp = const EXIT_SP_OFFSET,
    )
}

/// Exit trampoline: loads `ecx.exit_result`, restores the saved RSP,
/// pops callee-saved registers and returns to the caller of `evm2_jit_entry`.
///
/// # Safety
///
/// Must only be called from a builtin that was invoked through `evm2_jit_entry`.
#[unsafe(naked)]
pub unsafe extern "C" fn evm2_jit_exit<T: EvmTypesHost>(
    ecx: *const EvmContext<'_, '_, '_, T>,
) -> ! {
    core::arch::naked_asm!(
        // Load exit_result into the return register.
        "movzx eax, byte ptr [rdi + {exit_result}]",
        // Restore the saved RSP (points at the callee-saved saves).
        "mov rsp, [rdi + {exit_sp}]",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "ret",
        exit_result = const EXIT_RESULT_OFFSET,
        exit_sp = const EXIT_SP_OFFSET,
    )
}
