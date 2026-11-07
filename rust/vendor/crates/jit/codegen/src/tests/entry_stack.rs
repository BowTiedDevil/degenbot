//! Functions called with a non-empty stack, which `inspect_stack` allows.

use super::{DEF_SPEC, with_evm_context};
use crate::{Backend, EvmCompiler};
use alloy_primitives::U256;
use evm2::interpreter::InstrStop;

/// Runs `code` with `input` on the stack and checks the final stack.
fn run<B: Backend>(compiler: &mut EvmCompiler<B>, code: &[u8], input: &[U256], expected: &[U256]) {
    unsafe { compiler.clear() }.unwrap();
    compiler.set_inspect_stack(true);
    let f = unsafe { compiler.jit("entry_stack", code, DEF_SPEC) }.unwrap();
    with_evm_context(code, DEF_SPEC, |ecx, stack, stack_len| {
        for (i, &value) in input.iter().enumerate() {
            stack.set(i, value.into());
        }
        *stack_len = input.len();
        let r = unsafe { f.call(ecx, stack, stack_len) };
        assert_eq!(r, InstrStop::Stop);
        let actual: Vec<_> =
            unsafe { stack.as_slice(*stack_len) }.iter().map(|word| word.to_u256()).collect();
        assert_eq!(actual, expected);
    });
}

// The entry `POP` consumes the input, so the trampoline at `tramp` returns to `done`; it must
// not be treated as unreachable.
matrix_tests!(
    entry_stack_trampoline = |compiler| {
        let code = crate::parse_asm(
            "
            POP
            PUSH %done
            PUSH %tramp
            JUMP
        tramp:
            JUMPDEST
            JUMP
        done:
            JUMPDEST
            STOP
            ",
        )
        .unwrap();
        run(compiler, &code, &[U256::from(0x2a)], &[]);
    }
);

// The test calldata is non-zero, so execution takes `alt`, which only an input lets reach
// `join`, with 9 instead of 7.
matrix_tests!(
    entry_stack_join_constant = |compiler| {
        let code = crate::parse_asm(
            "
            PUSH0
            CALLDATALOAD
            PUSH %alt
            JUMPI
            PUSH1 0x07
            PUSH %join
            JUMP
        alt:
            JUMPDEST
            POP
            PUSH1 0x09
            PUSH %join
            JUMP
        join:
            JUMPDEST
            PUSH1 0x01
            ADD
            STOP
            ",
        )
        .unwrap();
        run(compiler, &code, &[U256::from(0x2a)], &[U256::from(0x0a)]);
    }
);
