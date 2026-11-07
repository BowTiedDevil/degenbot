#![allow(
    clippy::needless_update,
    unreachable_pub,
    dead_code,
    missing_docs,
    missing_debug_implementations
)]

use crate::*;
use alloy_primitives::{
    Address, B256, Bytes, KECCAK256_EMPTY as KECCAK_EMPTY, Log, LogData, U256, hex, keccak256,
    map::HashMap, uint,
};
use evm2::{
    SpecId,
    interpreter::{InstrStop, op},
};
use evm2_jit_builtins::gas;

/// Opcode gas cost of `base` plus `per_word` for each 32-byte word of `len`.
const fn word_cost(base: u64, per_word: u64, len: u64) -> Option<u64> {
    match len.div_ceil(32).checked_mul(per_word) {
        Some(dyn_cost) => Some(base.saturating_add(dyn_cost)),
        None => None,
    }
}

/// `KECCAK256` opcode gas cost (base + dynamic).
const fn keccak256_cost(len: u64) -> Option<u64> {
    word_cost(gas::KECCAK256 as u64, gas::KECCAK256WORD as u64, len)
}

/// `LOG` opcode gas cost (base + topics + dynamic).
const fn log_cost(n_topics: u8, len: u64) -> Option<u64> {
    match (gas::LOGDATA as u64).checked_mul(len) {
        Some(dyn_cost) => Some(
            ((gas::LOG as u64) + (gas::LOGTOPIC as u64) * n_topics as u64).saturating_add(dyn_cost),
        ),
        None => None,
    }
}

/// `CALLDATACOPY`, `CODECOPY`, `RETURNDATACOPY` opcode gas cost (base + dynamic).
const fn verylowcopy_cost(len: u64) -> Option<u64> {
    word_cost(gas::VERYLOW as u64, gas::COPY as u64, len)
}

#[macro_use]
mod macros;

mod meta;

mod fibonacci;

mod entry_stack;

mod large_section_gas;

mod runner;
pub use runner::*;

#[cfg(feature = "llvm")]
pub fn with_jit_compiler<R>(
    opt_level: evm2_jit_backend::OptimizationLevel,
    f: fn(&mut EvmCompiler<evm2_jit_llvm::EvmLlvmBackend>) -> R,
) -> R {
    init_tracing();
    let mut compiler = EvmCompiler::new_llvm(false).unwrap();
    compiler.set_opt_level(opt_level);
    compiler.set_single_error(false);
    f(&mut compiler)
}

/// A section that falls through a `JUMPI` whose condition is zero at runtime but unknown to the
/// analysis, so the next section is entered only by falling through.
const FALL_THROUGH: [u8; 4] = [op::CALLVALUE, op::ISZERO, op::PUSH0, op::JUMPI];
/// Gas charged by [`FALL_THROUGH`].
const FALL_THROUGH_GAS: u64 = 2 + 3 + 2 + 10;

/// An `MSTORE` that grows memory, in a section entered only by falling through, then `STOP`.
const MSTORE_THEN_STOP: [u8; 8] =
    [op::PUSH0, op::PUSH0, op::MSTORE, op::CALLVALUE, op::ISZERO, op::PUSH0, op::JUMPI, op::STOP];

/// Returns `n` copies of [`FALL_THROUGH`] followed by `tail`.
fn fall_through_then(n: usize, tail: &[u8]) -> Vec<u8> {
    [&FALL_THROUGH.repeat(n)[..], tail].concat()
}

const I256_MIN: U256 = U256::from_limbs([
    0x0000000000000000,
    0x0000000000000000,
    0x0000000000000000,
    0x8000000000000000,
]);

const I256_MAX: U256 = U256::from_limbs([
    0xFFFFFFFFFFFFFFFF,
    0xFFFFFFFFFFFFFFFF,
    0xFFFFFFFFFFFFFFFF,
    0x7FFFFFFFFFFFFFFF,
]);

tests! {
    ret {
        empty(@raw {}),
        no_stop(@raw {
            bytecode: &[op::PUSH0],
            expected_stack: &[U256::ZERO],
            expected_gas: 2,
        }),
        stop(@raw {
            bytecode: &[op::STOP],
            expected_gas: 0,
        }),
        invalid(@raw {
            bytecode: &[op::INVALID],
            expected_return: InstrStop::InvalidFEOpcode,
        }),
        unknown(@raw {
            bytecode: &[0x21],
            expected_return: InstrStop::OpcodeNotFound,
        }),
        underflow1(@raw {
            bytecode: &[op::ADD],
            expected_return: InstrStop::StackUnderflow,
        }),
        underflow2(@raw {
            bytecode: &[op::PUSH0, op::ADD],
            expected_return: InstrStop::StackUnderflow,
        }),
        underflow3(@raw {
            bytecode: &[op::PUSH0, op::POP, op::ADD],
            expected_return: InstrStop::StackUnderflow,
        }),
        underflow4(@raw {
            bytecode: &[op::PUSH0, op::ADD, op::POP],
            expected_return: InstrStop::StackUnderflow,
        }),
        overflow_not0(@raw {
            bytecode: &[op::PUSH0; 1023],
            expected_return: InstrStop::Stop,
            expected_stack: &[0_U256; 1023],
            expected_gas: 2 * 1023,
        }),
        overflow_not1(@raw {
            bytecode: &[op::PUSH0; 1024],
            expected_return: InstrStop::Stop,
            expected_stack: &[0_U256; 1024],
            expected_gas: 2 * 1024,
        }),
        overflow0(@raw {
            bytecode: &[op::PUSH0; 1025],
            expected_return: InstrStop::StackOverflow,
        }),
        overflow1(@raw {
            bytecode: &[op::PUSH0; 1026],
            expected_return: InstrStop::StackOverflow,
        }),
    }

    spec_id {
        push0_merge(@raw {
            bytecode: &[op::PUSH0],
            spec_id: SpecId::MERGE,
            expected_return: InstrStop::NotActivated,
        }),
        push0_shanghai(@raw {
            bytecode: &[op::PUSH0],
            spec_id: SpecId::SHANGHAI,
            expected_stack: &[U256::ZERO],
            expected_gas: 2,
        }),
        push0_cancun(@raw {
            bytecode: &[op::PUSH0],
            spec_id: SpecId::CANCUN,
            expected_stack: &[U256::ZERO],
            expected_gas: 2,
        }),
        clz_cancun(@raw {
            bytecode: &[op::MSIZE, op::CLZ],
            spec_id: SpecId::CANCUN,
            expected_return: InstrStop::NotActivated,
        }),
        clz_arrow_glacier(@raw {
            bytecode: &[op::MSIZE, op::CLZ],
            spec_id: SpecId::LONDON,
            expected_return: InstrStop::NotActivated,
        }),

        dupn_cancun(@raw {
            bytecode: &[op::PUSH0, op::DUPN, 0x00],
            spec_id: SpecId::CANCUN,
            expected_return: InstrStop::NotActivated,
        }),
        swapn_cancun(@raw {
            bytecode: &[op::PUSH0, op::SWAPN, 0x00],
            spec_id: SpecId::CANCUN,
            expected_return: InstrStop::NotActivated,
        }),
        exchange_cancun(@raw {
            bytecode: &[op::PUSH0, op::EXCHANGE, 0x01],
            spec_id: SpecId::CANCUN,
            expected_return: InstrStop::NotActivated,
        }),
        slotnum_cancun(@raw {
            bytecode: &[op::SLOTNUM],
            spec_id: SpecId::CANCUN,
            expected_return: InstrStop::NotActivated,
        }),

        // EXP gas: pre-SPURIOUS_DRAGON exp_byte_gas=10, post=50.
        // power=0 → dynamic=0 for all specs.
        exp_zero_frontier(@raw {
            bytecode: &bytecode_binop(op::EXP, 2_U256, 0_U256),
            spec_id: SpecId::FRONTIER,
            expected_stack: &[1_U256],
            expected_gas: 6 + 10,
        }),
        // power=1 → 1 byte, dynamic=10*1=10 pre-spurious, total=20.
        exp_one_frontier(@raw {
            bytecode: &bytecode_binop(op::EXP, 2_U256, 1_U256),
            spec_id: SpecId::FRONTIER,
            expected_stack: &[2_U256],
            expected_gas: 6 + 10 + 10,
        }),
        // power=256 → log2floor=8, bytes=2, dynamic=10*2=20 pre-spurious, total=30.
        exp_multi_byte_frontier(@raw {
            bytecode: &bytecode_binop(op::EXP, 2_U256, 256_U256),
            spec_id: SpecId::FRONTIER,
            expected_stack: &[0_U256],
            expected_gas: 6 + 10 + 20,
        }),
        // Same with ISTANBUL (post-spurious, exp_byte_gas=50).
        exp_zero_istanbul(@raw {
            bytecode: &bytecode_binop(op::EXP, 2_U256, 0_U256),
            spec_id: SpecId::ISTANBUL,
            expected_stack: &[1_U256],
            expected_gas: 6 + 10,
        }),
        exp_one_istanbul(@raw {
            bytecode: &bytecode_binop(op::EXP, 2_U256, 1_U256),
            spec_id: SpecId::ISTANBUL,
            expected_stack: &[2_U256],
            expected_gas: 6 + 10 + 50,
        }),
        exp_multi_byte_istanbul(@raw {
            bytecode: &bytecode_binop(op::EXP, 2_U256, 256_U256),
            spec_id: SpecId::ISTANBUL,
            expected_stack: &[0_U256],
            expected_gas: 6 + 10 + 100,
        }),
        // Dynamic exponent at the pre-SPURIOUS_DRAGON rate (the default spec's rate is covered by
        // the `_const_dyn` arith tests): storing and loading it costs 32 gas, pushing the base 3,
        // and EXP 10.
        exp_dyn_zero_frontier(@raw {
            bytecode: &bytecode_binop_mixed(op::EXP, 2_U256, 0_U256, true, false),
            spec_id: SpecId::FRONTIER,
            expected_stack: &[1_U256],
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: 45,
        }),
        exp_dyn_ff_frontier(@raw {
            bytecode: &bytecode_binop_mixed(op::EXP, 2_U256, 0xff_U256, true, false),
            spec_id: SpecId::FRONTIER,
            expected_stack: &[1_U256 << 255],
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: 45 + 10,
        }),
        exp_dyn_100_frontier(@raw {
            bytecode: &bytecode_binop_mixed(op::EXP, 2_U256, 0x100_U256, true, false),
            spec_id: SpecId::FRONTIER,
            expected_stack: &[0_U256],
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: 45 + 20,
        }),
        exp_dyn_max_frontier(@raw {
            bytecode: &bytecode_binop_mixed(op::EXP, 2_U256, U256::MAX, true, false),
            spec_id: SpecId::FRONTIER,
            expected_stack: &[0_U256],
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: 45 + 320,
        }),
    }

    stack {
        pop(@raw {
            bytecode: &[op::PUSH1, 1, op::POP],
            expected_gas: 3 + 2,
        }),
        dup(@raw {
            bytecode: &[op::PUSH1, 1, op::DUP1],
            expected_stack: &[1_U256, 1_U256],
            expected_gas: 3 + 3,
        }),

        swap(@raw {
            bytecode: &[op::PUSH1, 1, op::PUSH1, 2, op::SWAP1],
            expected_stack: &[2_U256, 1_U256],
            expected_gas: 3 + 3 + 3,
        }),
        swap2(@raw {
            bytecode: &[op::PUSH1, 1, op::PUSH1, 2, op::PUSH1, 3, op::SWAP2],
            expected_stack: &[3_U256, 2_U256, 1_U256],
            expected_gas: 3 + 3 + 3 + 3,
        }),
        swap3(@raw {
            bytecode: &[op::PUSH1, 1, op::PUSH1, 2, op::PUSH1, 3, op::PUSH1, 4, op::SWAP3],
            expected_stack: &[4_U256, 2_U256, 3_U256, 1_U256],
            expected_gas: 3 + 3 + 3 + 3 + 3,
        }),

        // DUPN 0x80: decode_single(0x80) = 17, duplicates 17th stack item.
        dupn(@raw {
            bytecode: &{
                let mut code = [0u8; 21];
                code[0] = op::PUSH1; code[1] = 1;   // bottom = 1
                // 16 zeros on top
                let mut i = 2;
                while i < 18 {
                    code[i] = op::PUSH0;
                    i += 1;
                }
                code[18] = op::DUPN;
                code[19] = 0x80; // n=17
                code
            },
            spec_id: SpecId::AMSTERDAM,
            expected_stack: &{
                let mut s = [U256::ZERO; 18];
                s[0] = 1_U256;  // duplicated from bottom
                // s[1..17] = ZERO (the 16 pushed zeros)
                s[17] = 1_U256; // original bottom
                s
            },
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),

        // SWAPN 0x80: decode_single(0x80) = 17, swaps top with 17th item.
        swapn(@raw {
            bytecode: &{
                let mut code = [0u8; 23];
                code[0] = op::PUSH1; code[1] = 1;   // bottom = 1
                // 16 zeros on top
                let mut i = 2;
                while i < 18 {
                    code[i] = op::PUSH0;
                    i += 1;
                }
                code[18] = op::PUSH1; code[19] = 2; // top = 2
                code[20] = op::SWAPN;
                code[21] = 0x80; // n=17
                code
            },
            spec_id: SpecId::AMSTERDAM,
            expected_stack: &{
                let mut s = [U256::ZERO; 18];
                s[0] = 2_U256;  // swapped from top
                // s[1..17] = ZERO
                s[17] = 1_U256; // swapped from bottom
                s
            },
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),

        // EXCHANGE 0x8E: decode_pair(0x8E) = (1, 2), swaps 2nd and 3rd from top.
        exchange_basic(@raw {
            bytecode: &[op::PUSH1, 1, op::PUSH1, 2, op::PUSH1, 3, op::EXCHANGE, 0x8E],
            spec_id: SpecId::AMSTERDAM,
            // stack before: [1, 2, 3] (3 on top)
            // exchange(1, 2): swaps items at depth 1 and 2 (0-indexed from top)
            // → swaps 2 and 1 → [2, 1, 3]
            expected_stack: &[2_U256, 1_U256, 3_U256],
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),

        // SLOTNUM: pushes the slot number from the host.
        slotnum(@raw {
            bytecode: &[op::SLOTNUM],
            spec_id: SpecId::AMSTERDAM,
            expected_stack: &[def_env().block.slot_num],
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),

        // Insufficient stack depth: DUPN 0x80 decodes to depth 17, but only 1 item on stack.
        dupn_underflow(@raw {
            bytecode: &[op::PUSH0, op::DUPN, 0x80],
            spec_id: SpecId::AMSTERDAM,
            expected_return: InstrStop::StackUnderflow,
        }),
        // Insufficient stack depth: SWAPN 0x80 decodes to depth 17, but only 1 item on stack.
        swapn_underflow(@raw {
            bytecode: &[op::PUSH0, op::SWAPN, 0x80],
            spec_id: SpecId::AMSTERDAM,
            expected_return: InstrStop::StackUnderflow,
        }),
        // Insufficient stack depth: EXCHANGE 0x8E decodes to (1,2), but only 2 items on stack.
        exchange_underflow(@raw {
            bytecode: &[op::PUSH0, op::PUSH0, op::EXCHANGE, 0x8E],
            spec_id: SpecId::AMSTERDAM,
            expected_return: InstrStop::StackUnderflow,
        }),
        swap16_dup10_selfdestruct_underflow(@raw {
            bytecode: &[op::SWAP16, op::DUP10, op::SELFDESTRUCT],
            spec_id: SpecId::OSAKA,
            expected_return: InstrStop::StackUnderflow,
        }),

        // Invalid immediate: 0x5B (91) is in the invalid range [91, 127] for decode_single.
        dupn_invalid_imm(@raw {
            bytecode: &[op::PUSH0, op::DUPN, 0x5B],
            spec_id: SpecId::AMSTERDAM,
            expected_return: InstrStop::InvalidImmediateEncoding,
        }),
        swapn_invalid_imm(@raw {
            bytecode: &[op::PUSH0, op::SWAPN, 0x5B],
            spec_id: SpecId::AMSTERDAM,
            expected_return: InstrStop::InvalidImmediateEncoding,
        }),
        // The invalid immediate halts before the following instructions are charged or
        // stack-checked, so an empty stack does not underflow at the section head.
        dupn_invalid_imm_before_underflow(@raw {
            bytecode: &[op::DUPN, 0x5B, op::ADD],
            spec_id: SpecId::AMSTERDAM,
            expected_return: InstrStop::InvalidImmediateEncoding,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // Invalid immediate: 82 is in the invalid range [82, 127] for decode_pair.
        exchange_invalid_imm(@raw {
            bytecode: &[op::PUSH0, op::PUSH0, op::PUSH0, op::EXCHANGE, 82],
            spec_id: SpecId::AMSTERDAM,
            expected_return: InstrStop::InvalidImmediateEncoding,
        }),

        // Truncated trailing DUPN at EOF: immediate byte missing, zero-padded to 0x00.
        // decode_single(0) = 145, so this is DUPN(145) with only 17 items on stack → overflow.
        dupn_truncated_eof(@raw {
            bytecode: &{
                let mut code = [0u8; 18];
                let mut i = 0;
                while i < 17 {
                    code[i] = op::PUSH0;
                    i += 1;
                }
                code[17] = op::DUPN; // no immediate byte
                code
            },
            spec_id: SpecId::AMSTERDAM,
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // Truncated trailing SWAPN at EOF: immediate byte missing, zero-padded to 0x00.
        // decode_single(0) = 145, so this is SWAPN(145) with only 18 items on stack → overflow.
        swapn_truncated_eof(@raw {
            bytecode: &{
                let mut code = [0u8; 19];
                let mut i = 0;
                while i < 18 {
                    code[i] = op::PUSH0;
                    i += 1;
                }
                code[18] = op::SWAPN; // no immediate byte
                code
            },
            spec_id: SpecId::AMSTERDAM,
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),

        // JUMP into the 0x5b immediate byte of DUPN. Per EIP-8024, JUMPDEST analysis is
        // unchanged by these opcodes, so 0x5b in the immediate position is a valid target.
        jump_into_dupn_immediate(@raw {
            bytecode: &[
                op::PUSH1, 4, op::JUMP,
                op::DUPN, op::JUMPDEST, // 0x5b immediate is a reachable JUMPDEST
                op::PUSH1, 0x42, op::PUSH0, op::MSTORE,
                op::PUSH1, 0x20, op::PUSH0, op::RETURN,
            ],
            spec_id: SpecId::AMSTERDAM,
            expected_return: InstrStop::Return,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // Same as above but with SWAPN.
        jump_into_swapn_immediate(@raw {
            bytecode: &[
                op::PUSH1, 4, op::JUMP,
                op::SWAPN, op::JUMPDEST,
                op::PUSH1, 0x42, op::PUSH0, op::MSTORE,
                op::PUSH1, 0x20, op::PUSH0, op::RETURN,
            ],
            spec_id: SpecId::AMSTERDAM,
            expected_return: InstrStop::Return,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // Same as above but with EXCHANGE.
        jump_into_exchange_immediate(@raw {
            bytecode: &[
                op::PUSH1, 4, op::JUMP,
                op::EXCHANGE, op::JUMPDEST,
                op::PUSH1, 0x42, op::PUSH0, op::MSTORE,
                op::PUSH1, 0x20, op::PUSH0, op::RETURN,
            ],
            spec_id: SpecId::AMSTERDAM,
            expected_return: InstrStop::Return,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // The reverse of the above: the byte after DUPN is analyzed as PUSH1 by the
        // unchanged JUMPDEST analysis, so its data byte masks the 0x5b even though
        // instruction decoding (which consumes DUPN's immediate) yields it as a
        // JUMPDEST instruction. The jump must fail.
        jump_to_push_masked_jumpdest_after_dupn(@raw {
            bytecode: &[
                op::PUSH1, 5, op::JUMP,
                op::DUPN, op::PUSH1, op::JUMPDEST, // JUMPDEST is PUSH1 data in the analysis
                op::PUSH1, 0x42, op::PUSH0, op::MSTORE,
                op::PUSH1, 0x20, op::PUSH0, op::RETURN,
            ],
            spec_id: SpecId::AMSTERDAM,
            expected_return: InstrStop::InvalidJump,
        }),
        // Same masked target, but reached through an unresolved dynamic jump
        // (CALLDATASIZE is unknown at compile time): the dynamic dispatch table must
        // not contain the analysis-masked JUMPDEST either.
        dynamic_jump_to_push_masked_jumpdest_after_dupn(@raw {
            bytecode: &[
                op::PUSH1, 57, op::CALLDATASIZE, op::SUB, op::JUMP, // 64 - 57 = pc 7
                op::DUPN, op::PUSH1, op::JUMPDEST,
                op::PUSH1, 0x42, op::PUSH0, op::MSTORE,
                op::PUSH1, 0x20, op::PUSH0, op::RETURN,
            ],
            spec_id: SpecId::AMSTERDAM,
            expected_return: InstrStop::InvalidJump,
        }),
        // A valid target inside a PUSH immediate: DUPN desynchronizes decoding, so the
        // 0x5b sits in the data of a decoded PUSH1 while the analysis (which does not
        // skip DUPN immediates) sees it at an instruction boundary. The jump must land.
        jump_into_desynced_push_immediate(@raw {
            bytecode: &[
                op::PUSH1, 6, op::JUMP,
                op::DUPN, op::PUSH1, op::PUSH1, op::JUMPDEST, // decoded: DUPN(PUSH1), PUSH1(0x5b)
                op::STOP,
            ],
            spec_id: SpecId::AMSTERDAM,
            expected_return: InstrStop::Stop,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // Same, but the target sits in the truncated immediate of a PUSH2 at the end of
        // the code.
        jump_into_desynced_truncated_push_immediate(@raw {
            bytecode: &[
                op::PUSH1, 6, op::JUMP,
                op::DUPN, op::PUSH1, op::PUSH2, op::JUMPDEST, // decoded: DUPN(PUSH1), PUSH2(0x5b)
            ],
            spec_id: SpecId::AMSTERDAM,
            expected_return: InstrStop::Stop,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // Same target, reached through an unresolved dynamic jump (CALLDATASIZE).
        dynamic_jump_into_desynced_truncated_push_immediate(@raw {
            bytecode: &[
                op::PUSH1, 56, op::CALLDATASIZE, op::SUB, op::JUMP, // 64 - 56 = pc 8
                op::DUPN, op::PUSH1, op::PUSH2, op::JUMPDEST,
            ],
            spec_id: SpecId::AMSTERDAM,
            expected_return: InstrStop::Stop,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),

        // Truncated PUSH2 at EOF: only 1 of 2 immediate bytes present.
        // EVM spec right-pads with zeros: PUSH2 0x42 → 0x4200.
        push2_truncated_eof(@raw {
            bytecode: &[op::PUSH2, 0x42],
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_stack: &[U256::from(0x4200u64)],
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // Truncated PUSH3 at EOF: only 2 of 3 immediate bytes present.
        push3_truncated_eof(@raw {
            bytecode: &[op::PUSH3, 0xAB, 0xCD],
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_stack: &[U256::from(0xABCD00u64)],
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // PUSH1 with no immediate bytes at all.
        push1_no_imm_eof(@raw {
            bytecode: &[op::PUSH1],
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_stack: &[U256::ZERO],
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),

        overflow_analysis_edge_case(@raw {
            bytecode: &[&[op::JUMPDEST][..], &[op::PUSH0; 1025][..], &[op::JUMPI][..]].concat(),
            expected_return: InstrStop::StackOverflow,
        }),
    }

    control_flow {
        basic_jump(@raw {
            bytecode: &[op::PUSH1, 3, op::JUMP, op::JUMPDEST, op::PUSH1, 69],
            expected_stack: &[69_U256],
            expected_gas: 3 + 8 + 1 + 3,
        }),
        unmodified_stack_after_push_jump(@raw {
            bytecode: &[op::PUSH1, 3, op::JUMP, op::JUMPDEST, op::PUSH0, op::ADD],
            expected_return: InstrStop::StackUnderflow,
        }),
        bad_jump(@raw {
            bytecode: &[op::JUMP],
            expected_return: InstrStop::StackUnderflow,
        }),
        bad_jumpi1(@raw {
            bytecode: &[op::JUMPI],
            expected_return: InstrStop::StackUnderflow,
        }),
        bad_jumpi2(@raw {
            bytecode: &[op::PUSH0, op::JUMPI],
            expected_return: InstrStop::StackUnderflow,
        }),
        bad_jumpi3(@raw {
            bytecode: &[op::JUMPDEST, op::PUSH0, op::JUMPI],
            expected_return: InstrStop::StackUnderflow,
        }),

        basic_jumpi1(@raw {
            bytecode: &[op::JUMPDEST, op::PUSH0, op::PUSH0, op::JUMPI, op::PUSH1, 69],
            expected_stack: &[69_U256],
            expected_gas: 1 + 2 + 2 + 10 + 3,
        }),
        basic_jumpi1_lazy_invalid_target(@raw {
            bytecode: &[op::PUSH0, op::PUSH0, op::JUMPI, op::PUSH1, 69],
            expected_stack: &[69_U256],
            expected_gas: 2 + 2 + 10 + 3,
        }),
        basic_jumpi2(@raw {
            bytecode: &[op::PUSH1, 1, op::PUSH1, 5, op::JUMPI, op::JUMPDEST, op::PUSH1, 69],
            expected_stack: &[69_U256],
            expected_gas: 3 + 3 + 10 + 1 + 3,
        }),
        basic_jumpi2_lazy_invalid_target(@raw {
            bytecode: &[op::PUSH1, 1, op::PUSH0, op::JUMPI, op::PUSH1, 69],
            expected_return: InstrStop::InvalidJump,
        }),
        unmodified_stack_after_push_jumpi(@raw {
            bytecode: &[op::PUSH1, 1, op::PUSH1, 5, op::JUMPI, op::JUMPDEST, op::PUSH0, op::ADD],
            expected_return: InstrStop::StackUnderflow,
        }),

        basic_loop(@raw {
            bytecode: &[
                op::PUSH1, 3,  // i=3
                op::JUMPDEST,  // i
                op::PUSH1, 1,  // 1, i
                op::SWAP1,     // i, 1
                op::SUB,       // i-1
                op::DUP1,      // i-1, i-1
                op::PUSH1, 2,  // dst, i-1, i-1
                op::JUMPI,     // i=i-1
                op::POP,       //
                op::PUSH1, 69, // 69
            ],
            expected_stack: &[69_U256],
            expected_gas: 3 + (1 + 3 + 3 + 3 + 3 + 3 + 10) * 3 + 2 + 3,
        }),

        // `outer` and `inner` are internal functions; `inner` is also called directly. The
        // return of `outer` reads its return address from below `inner`'s frame.
        nested_internal_returns(@raw {
            bytecode: &asm("
                PUSH %ret1
                PUSH1 0x01
                PUSH %inner
                JUMP
            ret1:
                JUMPDEST
                PUSH %ret2
                PUSH1 0x02
                PUSH %outer
                JUMP
            ret2:
                JUMPDEST
                STOP
            outer:
                JUMPDEST        ; [ret, x]
                PUSH %outer_ret
                SWAP1
                PUSH %inner
                JUMP
            outer_ret:
                JUMPDEST        ; [ret, inner(x)]
                PUSH1 0x10
                ADD
                SWAP1
                JUMP
            inner:
                JUMPDEST        ; [ret, x]
                PUSH1 0x03
                MUL
                SWAP1
                JUMP
            "),
            expected_stack: &[3_U256, 0x16_U256],
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        recursive_internal_returns(@raw {
            bytecode: &asm("
                PUSH %done
                PUSH1 0x03
                PUSH %rec
                JUMP
            done:
                JUMPDEST
                STOP
            rec:
                JUMPDEST        ; [ret, n]
                DUP1
                ISZERO
                PUSH %base
                JUMPI
                PUSH %back
                SWAP1
                PUSH1 0x01
                SWAP1
                SUB             ; [ret, back, n - 1]
                PUSH %rec
                JUMP
            back:
                JUMPDEST        ; [ret, r]
                PUSH1 0x02
                ADD
                SWAP1
                JUMP
            base:
                JUMPDEST        ; [ret, 0]
                SWAP1
                JUMP
            "),
            expected_stack: &[6_U256],
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),

        // Adversarial checks of the block analysis under the production configuration
        // (`inspect_stack: false`), which observes results through `RETURN`.
        //
        // One caller leaves the return address at the deepest tracked abstract stack slot
        // and the other one below it, with a flag selecting how many entries the callee pops.
        deep_frame_split_return(@raw {
            bytecode: &asm(&[
                "
                PUSH %ret1
                ",
                &"PUSH0\n".repeat(62),
                "
                PUSH0
                PUSH %f
                JUMP
            ret1:
                JUMPDEST
                PUSH %ret2
                ",
                &"PUSH0\n".repeat(63),
                "
                PUSH1 0x01
                PUSH %f
                JUMP
            ret2:
                JUMPDEST
                PUSH1 0x42
                PUSH0
                MSTORE
                PUSH1 0x20
                PUSH0
                RETURN
            f:
                JUMPDEST
                DUP1
                PUSH %path2
                JUMPI
                ",
                &"POP\n".repeat(63),
                "
                JUMP
            path2:
                JUMPDEST
                ",
                &"POP\n".repeat(64),
                "
                JUMP
                ",
            ].concat()),
            inspect_stack: Some(false),
            expected_return: InstrStop::Return,
            expected_memory: B256::with_last_byte(0x42).as_slice(),
            expected_output: Some(B256::with_last_byte(0x42).as_slice()),
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // A return through `JUMPI` whose condition is opaque and whose target has two callers.
        multi_jump_jumpi_unknown_condition(@raw {
            bytecode: &asm("
                PUSH %ret1
                PUSH1 0x01
                PUSH %f
                JUMP
            ret1:
                JUMPDEST
                PUSH1 0x42
                PUSH %ret2
                PUSH1 0x01
                PUSH %f
                JUMP
            ret2:
                JUMPDEST
                PUSH0
                MSTORE
                PUSH1 0x20
                PUSH0
                RETURN
            f:
                JUMPDEST        ; [ret, x]
                PUSH1 0x20
                MLOAD
                ADD             ; [ret, x + mem[0x20]]
                SWAP1
                JUMPI
                PUSH1 0xff
                PUSH0
                MSTORE
                PUSH1 0x20
                PUSH0
                RETURN
            "),
            inspect_stack: Some(false),
            expected_return: InstrStop::Return,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_output: Some(B256::with_last_byte(0x42).as_slice()),
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // Jumps into a table whose index the analysis only knows to be at most 3.
        range_jump_table_entry(@raw {
            bytecode: &asm(&range_jump_table(2)),
            inspect_stack: Some(false),
            expected_return: InstrStop::Return,
            expected_memory: B256::with_last_byte(0x22).as_slice(),
            expected_output: Some(B256::with_last_byte(0x22).as_slice()),
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        range_jump_table_non_jumpdest_entry(@raw {
            bytecode: &asm(&range_jump_table(1)),
            inspect_stack: Some(false),
            expected_return: InstrStop::InvalidJump,
        }),
        // The `JUMPI` condition is a different valid jump target in each caller, so its
        // constant set proves it nonzero.
        jumpi_condition_from_jumpdest_set(@raw {
            bytecode: &asm("
                PUSH %ret1
                PUSH %l1
                PUSH %f
                JUMP
            ret1:
                JUMPDEST
                PUSH %ret2
                PUSH %l2
                PUSH %f
                JUMP
            ret2:
                JUMPDEST
                PUSH1 0x42
                PUSH0
                MSTORE
                PUSH1 0x20
                PUSH0
                RETURN
            f:
                JUMPDEST        ; [ret, l]
                SWAP1
                JUMPI
                PUSH1 0xff
                PUSH0
                MSTORE
                PUSH1 0x20
                PUSH0
                RETURN
            l1:
                JUMPDEST
                STOP
            l2:
                JUMPDEST
                STOP
            "),
            inspect_stack: Some(false),
            expected_return: InstrStop::Return,
            expected_memory: B256::with_last_byte(0x42).as_slice(),
            expected_output: Some(B256::with_last_byte(0x42).as_slice()),
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // Like the above, but pc 0 is a `JUMPDEST` and one caller passes 0, so the condition
        // set mixes zero and nonzero values and the fallthrough must stay live.
        jumpi_condition_set_with_zero(@raw {
            bytecode: &asm("
                JUMPDEST
                PUSH %ret1
                PUSH0
                PUSH %f
                JUMP
            ret1:
                JUMPDEST
                PUSH %ret2
                PUSH %l2
                PUSH %f
                JUMP
            ret2:
                JUMPDEST
                PUSH1 0x42
                PUSH0
                MSTORE
                PUSH1 0x20
                PUSH0
                RETURN
            f:
                JUMPDEST        ; [ret, c]
                SWAP1
                JUMPI
                PUSH1 0x07
                PUSH0
                MSTORE
                PUSH1 0x20
                PUSH0
                RETURN
            l2:
                JUMPDEST
                STOP
            "),
            inspect_stack: Some(false),
            expected_return: InstrStop::Return,
            expected_memory: B256::with_last_byte(0x07).as_slice(),
            expected_output: Some(B256::with_last_byte(0x07).as_slice()),
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // The constant folding budget runs out between the fixpoint and the snapshot pass: 20
        // EXP folds and 377 ADD folds per pass leave 1 gas for the last ADD in the third pass,
        // so the jump operand reaches `b` folded while the ADD's recorded output is unknown.
        fold_budget_exhausted_before_jump(@raw {
            bytecode: &asm(&[
                &"PUSH32 0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff
                  PUSH32 0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff
                  EXP
                  POP\n".repeat(20),
                &"PUSH1 0x01 PUSH1 0x01 ADD POP\n".repeat(377),
                "
                PUSH %t
                PUSH0
                ADD
                PUSH %b
                JUMP
            b:
                JUMPDEST
                JUMP
            t:
                JUMPDEST
                PUSH1 0x42
                PUSH0
                MSTORE
                PUSH1 0x20
                PUSH0
                RETURN
                ",
            ].concat()),
            inspect_stack: Some(false),
            gas_limit: 1_000_000,
            expected_return: InstrStop::Return,
            expected_memory: B256::with_last_byte(0x42).as_slice(),
            expected_output: Some(B256::with_last_byte(0x42).as_slice()),
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // Here the budget runs out before the fold inside `b` in the snapshot pass: the fixpoint
        // discovered `b`'s edge to `t`, but the recorded operand is unknown.
        fold_budget_exhausted_in_snapshot_pass(@raw {
            bytecode: &asm(&[
                &"PUSH32 0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff
                  PUSH32 0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff
                  EXP
                  POP\n".repeat(20),
                &"PUSH1 0x01 PUSH1 0x01 ADD POP\n".repeat(378),
                "
                PUSH %t
                PUSH %b
                JUMP
            b:
                JUMPDEST
                PUSH0
                ADD
                JUMP
            t:
                JUMPDEST
                PUSH1 0x42
                PUSH0
                MSTORE
                PUSH1 0x20
                PUSH0
                RETURN
                ",
            ].concat()),
            inspect_stack: Some(false),
            gas_limit: 1_000_000,
            expected_return: InstrStop::Return,
            expected_memory: B256::with_last_byte(0x42).as_slice(),
            expected_output: Some(B256::with_last_byte(0x42).as_slice()),
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // A function returns to pc 0, which is a `JUMPDEST`, so the entry block is also a
        // jump target reached with a different stack.
        return_to_entry_jumpdest(@raw {
            bytecode: &asm("
                JUMPDEST
                PUSH0
                MLOAD
                PUSH %cont
                JUMPI
                PUSH1 0x01
                PUSH0
                MSTORE
                PUSH0           ; return address 0
                PUSH %f
                JUMP
            f:
                JUMPDEST
                JUMP
            cont:
                JUMPDEST
                PUSH1 0x42
                PUSH0
                MSTORE
                PUSH1 0x20
                PUSH0
                RETURN
            "),
            inspect_stack: Some(false),
            expected_return: InstrStop::Return,
            expected_memory: B256::with_last_byte(0x42).as_slice(),
            expected_output: Some(B256::with_last_byte(0x42).as_slice()),
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // An unreached block ends in a `JUMPI` with an opaque target; it must not disturb the
        // reachable code.
        unreached_opaque_jumpi(@raw {
            bytecode: &asm("
                PUSH %b
                JUMP
            a:
                JUMPDEST
                PUSH0
                CALLDATALOAD
                PUSH0
                CALLDATALOAD
                JUMPI
                PUSH1 0x01
                STOP
            b:
                JUMPDEST
                PUSH1 0x42
                PUSH0
                MSTORE
                PUSH1 0x20
                PUSH0
                RETURN
            c:
                JUMPDEST
                PUSH1 0x02
                STOP
            "),
            inspect_stack: Some(false),
            expected_return: InstrStop::Return,
            expected_memory: B256::with_last_byte(0x42).as_slice(),
            expected_output: Some(B256::with_last_byte(0x42).as_slice()),
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // A function pointer chosen by `JUMPI`, called through a shared call block, where the
        // callees return to a shared return site.
        function_pointer_call(@raw {
            bytecode: &asm("
                PUSH %g
                PUSH0
                CALLDATALOAD
                PUSH %use_f
                JUMPI
                PUSH %call
                JUMP
            use_f:
                JUMPDEST
                POP
                PUSH %f
                PUSH %call
                JUMP
            call:
                JUMPDEST        ; [ptr]
                PUSH %ret
                SWAP1
                JUMP
            f:
                JUMPDEST        ; [ret]
                PUSH1 0xf0
                SWAP1
                JUMP
            g:
                JUMPDEST
                PUSH1 0x90
                SWAP1
                JUMP
            ret:
                JUMPDEST
                PUSH0
                MSTORE
                PUSH1 0x20
                PUSH0
                RETURN
            "),
            inspect_stack: Some(false),
            expected_return: InstrStop::Return,
            expected_memory: B256::with_last_byte(0xf0).as_slice(),
            expected_output: Some(B256::with_last_byte(0xf0).as_slice()),
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // EXP folds to 1 in both call contexts, but its exponent differs, so its gas does too.
        context_joined_exp_gas(@raw {
            bytecode: &asm("
                PUSH %ret1
                PUSH1 0x01
                PUSH %f
                JUMP
            ret1:
                JUMPDEST
                POP
                PUSH %ret2
                PUSH2 0x0100
                PUSH %f
                JUMP
            ret2:
                JUMPDEST
                POP
                STOP
            f:
                JUMPDEST        ; [ret, x]
                PUSH1 0x01
                EXP             ; 1 ** x
                SWAP1
                JUMP
            "),
            inspect_stack: Some(false),
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // Like `context_joined_exp_gas`, but the exponent is produced inside the EXP block, so dead
        // store elimination must keep it.
        context_joined_exp_dup_exponent(@raw {
            bytecode: &asm("
                PUSH %ret1
                PUSH1 0x01
                PUSH %f
                JUMP
            ret1:
                JUMPDEST
                POP
                PUSH %ret2
                PUSH2 0x0100
                PUSH %f
                JUMP
            ret2:
                JUMPDEST
                POP
                STOP
            f:
                JUMPDEST        ; [ret, x]
                DUP1
                PUSH1 0x01
                EXP             ; [ret, x, 1 ** x]
                SWAP1
                POP
                SWAP1
                JUMP
            "),
            inspect_stack: Some(false),
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // Recursion deeper than the tracked abstract stack.
        deep_recursive_internal_returns(@raw {
            bytecode: &asm("
                PUSH %done
                PUSH1 0x46
                PUSH %rec
                JUMP
            done:
                JUMPDEST
                PUSH0
                MSTORE
                PUSH1 0x20
                PUSH0
                RETURN
            rec:
                JUMPDEST        ; [ret, n]
                DUP1
                ISZERO
                PUSH %base
                JUMPI
                PUSH %back
                SWAP1
                PUSH1 0x01
                SWAP1
                SUB             ; [ret, back, n - 1]
                PUSH %rec
                JUMP
            back:
                JUMPDEST        ; [ret, r]
                PUSH1 0x02
                ADD
                SWAP1
                JUMP
            base:
                JUMPDEST        ; [ret, 0]
                SWAP1
                JUMP
            "),
            inspect_stack: Some(false),
            expected_return: InstrStop::Return,
            expected_memory: B256::with_last_byte(140).as_slice(),
            expected_output: Some(B256::with_last_byte(140).as_slice()),
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // One caller passes an invalid return address, so the return must stay dynamic and
        // fail only when that caller's frame returns.
        return_set_with_invalid_member(@raw {
            bytecode: &asm("
                PUSH %ret1
                PUSH %f
                JUMP
            ret1:
                JUMPDEST
                PUSH1 0x01
                PUSH %f
                JUMP
            f:
                JUMPDEST
                JUMP
            "),
            inspect_stack: Some(false),
            expected_return: InstrStop::InvalidJump,
        }),
        // The return address sits below the tracked abstract stack and is brought up with
        // `SWAPN`, so the return must stay dynamic.
        swapn_deep_return(@raw {
            bytecode: &asm(&[
                "
                PUSH %ret
                ",
                &"PUSH0\n".repeat(64),
                "
                PUSH %f
                JUMP
            f:
                JUMPDEST
                SWAPN 64
                JUMP
            ret:
                JUMPDEST
                ",
                &"POP\n".repeat(64),
                "
                PUSH1 0x42
                PUSH0
                MSTORE
                PUSH1 0x20
                PUSH0
                RETURN
                ",
            ].concat()),
            spec_id: SpecId::AMSTERDAM,
            inspect_stack: Some(false),
            expected_return: InstrStop::Return,
            expected_memory: B256::with_last_byte(0x42).as_slice(),
            expected_output: Some(B256::with_last_byte(0x42).as_slice()),
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // Same with `DUPN` copying the deep return address.
        dupn_deep_return(@raw {
            bytecode: &asm(&[
                "
                PUSH %ret
                ",
                &"PUSH0\n".repeat(64),
                "
                PUSH %f
                JUMP
            f:
                JUMPDEST
                DUPN 65
                JUMP
            ret:
                JUMPDEST
                ",
                &"POP\n".repeat(65),
                "
                PUSH1 0x42
                PUSH0
                MSTORE
                PUSH1 0x20
                PUSH0
                RETURN
                ",
            ].concat()),
            spec_id: SpecId::AMSTERDAM,
            inspect_stack: Some(false),
            expected_return: InstrStop::Return,
            expected_memory: B256::with_last_byte(0x42).as_slice(),
            expected_output: Some(B256::with_last_byte(0x42).as_slice()),
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // Two callers of a function that moves its return address with `EXCHANGE`.
        exchange_return_two_callers(@raw {
            bytecode: &asm("
                PUSH %ret1
                PUSH1 0x0a
                PUSH1 0x0b
                PUSH %f
                JUMP
            ret1:
                JUMPDEST        ; [a, b]
                ADD
                PUSH %ret2
                PUSH1 0x0c
                PUSH1 0x0d
                PUSH %f
                JUMP
            ret2:
                JUMPDEST        ; [a + b, c, d]
                ADD
                ADD
                PUSH0
                MSTORE
                PUSH1 0x20
                PUSH0
                RETURN
            f:
                JUMPDEST        ; [ret, a, b]
                EXCHANGE 1 2    ; [a, ret, b]
                SWAP1           ; [a, b, ret]
                JUMP
            "),
            spec_id: SpecId::AMSTERDAM,
            inspect_stack: Some(false),
            expected_return: InstrStop::Return,
            expected_memory: &U256::from(0x0a + 0x0b + 0x0c + 0x0d).to_be_bytes::<32>(),
            expected_output: Some(&U256::from(0x0a + 0x0b + 0x0c + 0x0d).to_be_bytes::<32>()),
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // Eleven binary choices give one block 2^11 Full contexts, past the Full budget. The
        // Coarse fallback keys on the topmost target only and must still find both targets of
        // every return jump.
        coarse_fallback_binary_choice_chain(@raw {
            bytecode: &asm(&binary_choice_chain(11)),
            inspect_stack: Some(false),
            modify_message: Some(|message| {
                message.input = Bytes::from_static(&[0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1]).into();
            }),
            expected_return: InstrStop::Return,
            expected_memory: &U256::from(606).to_be_bytes::<32>(),
            expected_output: Some(&U256::from(606).to_be_bytes::<32>()),
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),

        // The JUMPI fall-throughs continue the section at `head`: each one reads deeper
        // below the section start, and the final taken edge stores the values that only
        // lived in SSA. DUP5 reads the deepest value, exactly at the checked range.
        jumpi_fallthrough_chain(@raw {
            bytecode: &asm(&jumpi_fallthrough_chain("CALLDATASIZE ISZERO", "DUP5")),
            expected_return: InstrStop::Stop,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // DUP6 reads one below the stack.
        jumpi_fallthrough_underflow(@raw {
            bytecode: &asm(&jumpi_fallthrough_chain("CALLDATASIZE ISZERO", "DUP6")),
            expected_return: InstrStop::StackUnderflow,
        }),
        // The underflow is on the fall-through, so it must not fail when the jump is taken.
        jumpi_taken_skips_fallthrough_underflow(@raw {
            bytecode: &asm(&jumpi_fallthrough_chain("CALLDATASIZE", "DUP6")),
            expected_return: InstrStop::Stop,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // The second fall-through fills the stack to exactly 1024.
        jumpi_fallthrough_full_stack(@raw {
            bytecode: &asm(&jumpi_fallthrough_overflow("CALLDATASIZE ISZERO", 3)),
            expected_return: InstrStop::Stop,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        jumpi_fallthrough_overflow(@raw {
            bytecode: &asm(&jumpi_fallthrough_overflow("CALLDATASIZE ISZERO", 4)),
            expected_return: InstrStop::StackOverflow,
        }),
        jumpi_taken_skips_fallthrough_overflow(@raw {
            bytecode: &asm(&jumpi_fallthrough_overflow("CALLDATASIZE", 4)),
            expected_return: InstrStop::Stop,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),

        pc(@raw {
            bytecode: &[op::PC, op::PC, op::PUSH1, 69, op::PC, op::PUSH0, op::PC],
            expected_stack: &[0_U256, 1_U256, 69_U256, 4_U256, 0_U256, 6_U256],
            expected_gas: 2 + 2 + 3 + 2 + 2 + 2,
        }),
        pc_after_dupn_swapn_exchange(@raw {
            bytecode: &{
                let mut code = [0u8; 101];
                let mut i = 0;
                let mut value = 0;
                while value <= 29 {
                    code[i] = op::PUSH1;
                    code[i + 1] = value;
                    i += 2;
                    value += 1;
                }
                code[i] = op::PC;
                code[i + 1] = op::DUPN;
                code[i + 2] = 0x80;
                code[i + 3] = op::SWAPN;
                code[i + 4] = 0x81;
                code[i + 5] = op::EXCHANGE;
                code[i + 6] = 0x8b;
                code[i + 7] = op::PC;
                i += 8;
                let mut pops = 0;
                while pops < 33 {
                    code[i] = op::POP;
                    i += 1;
                    pops += 1;
                }
                code
            },
            spec_id: SpecId::AMSTERDAM,
            inspect_stack: Some(false),
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),

        // Sections entered only by falling through skip their gas check, so these run out of
        // gas partway through such a run and must still fail at the next exit or builtin.
        fall_through_oog_then_stop(@raw {
            bytecode: &fall_through_then(6, &[op::STOP]),
            gas_limit: 3 * FALL_THROUGH_GAS + 5,
            expected_return: InstrStop::OutOfGas,
        }),
        fall_through_exact_gas_then_stop(@raw {
            bytecode: &fall_through_then(6, &[op::STOP]),
            gas_limit: 6 * FALL_THROUGH_GAS,
            expected_gas: 6 * FALL_THROUGH_GAS,
        }),
        fall_through_oog_by_one_then_stop(@raw {
            bytecode: &fall_through_then(6, &[op::STOP]),
            gas_limit: 6 * FALL_THROUGH_GAS - 1,
            expected_return: InstrStop::OutOfGas,
        }),
        fall_through_then_revert(@raw {
            bytecode: &fall_through_then(6, &[op::PUSH0, op::PUSH0, op::REVERT]),
            expected_return: InstrStop::Revert,
            expected_gas: 6 * FALL_THROUGH_GAS + 2 + 2,
        }),
        fall_through_oog_then_revert(@raw {
            bytecode: &fall_through_then(6, &[op::PUSH0, op::PUSH0, op::REVERT]),
            gas_limit: 6 * FALL_THROUGH_GAS,
            expected_return: InstrStop::OutOfGas,
        }),
        fall_through_oog_then_invalid(@raw {
            bytecode: &fall_through_then(6, &[op::INVALID]),
            gas_limit: 3 * FALL_THROUGH_GAS,
            expected_return: InstrStop::OutOfGas,
        }),
        fall_through_then_gas(@raw {
            bytecode: &fall_through_then(6, &[op::GAS]),
            expected_stack: &[U256::from(DEF_GAS_LIMIT - 6 * FALL_THROUGH_GAS - 2)],
            expected_gas: 6 * FALL_THROUGH_GAS + 2,
        }),
        fall_through_oog_then_sload(@raw {
            bytecode: &fall_through_then(6, &[op::PUSH0, op::SLOAD]),
            gas_limit: 3 * FALL_THROUGH_GAS,
            expected_return: InstrStop::OutOfGas,
        }),
        fall_through_oog_then_log(@raw {
            bytecode: &fall_through_then(6, &[op::PUSH0, op::PUSH0, op::LOG0]),
            gas_limit: 3 * FALL_THROUGH_GAS,
            expected_return: InstrStop::OutOfGas,
            assert_host: Some(|host| assert!(host.logs.is_empty())),
        }),
        // The memory resize builtin checks the gas inline on its cold path.
        fall_through_then_mstore(@raw {
            bytecode: &fall_through_then(6, &MSTORE_THEN_STOP),
            expected_memory: &[0; 32],
            expected_gas: 7 * FALL_THROUGH_GAS + 2 + 2 + 3 + 3,
        }),
        fall_through_oog_then_mstore(@raw {
            bytecode: &fall_through_then(6, &MSTORE_THEN_STOP),
            gas_limit: 3 * FALL_THROUGH_GAS,
            expected_return: InstrStop::OutOfGas,
            assert_ecx: Some(|ecx| assert!(ecx.memory().is_empty())),
        }),
        fall_through_memory_oog_then_mstore(@raw {
            bytecode: &fall_through_then(6, &MSTORE_THEN_STOP),
            gas_limit: 7 * FALL_THROUGH_GAS + 2 + 2 + 3 + 2,
            expected_return: InstrStop::OutOfGas,
        }),
        // The `MLOAD` skips its resize at runtime, so its cold-path check must not clear the debt
        // before `TSTORE`.
        fall_through_oog_then_mload_then_tstore(@raw {
            bytecode: &[
                &[op::PUSH0, op::PUSH0, op::MSTORE][..],
                &FALL_THROUGH.repeat(6),
                &[op::CALLVALUE, op::ISZERO, op::MLOAD, op::POP],
                &[op::CALLVALUE, op::PUSH0, op::TSTORE],
                &fall_through_then(1, &[op::STOP])[..],
            ]
            .concat(),
            gas_limit: 2 + 2 + 3 + 3 + 3 * FALL_THROUGH_GAS,
            expected_return: InstrStop::OutOfGas,
            assert_host: Some(|host| assert!(host.transient_storage.is_empty())),
        }),
        fall_through_past_threshold_then_stop(@raw {
            bytecode: &fall_through_then(70, &[op::STOP]),
            gas_limit: 70 * FALL_THROUGH_GAS,
            expected_gas: 70 * FALL_THROUGH_GAS,
        }),
        fall_through_oog_past_threshold_then_stop(@raw {
            bytecode: &fall_through_then(70, &[op::STOP]),
            gas_limit: 10 * FALL_THROUGH_GAS,
            expected_return: InstrStop::OutOfGas,
        }),
        // Unresolved jumps to valid targets in different table pairs, including the
        // first and last bit of a bitmap, and to invalid and out-of-range targets.
        dynamic_jump_to_63(@raw {
            bytecode: &bytecode_dynamic_jump(63_U256),
            expected_stack: &[64_U256],
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        dynamic_jump_to_96(@raw {
            bytecode: &bytecode_dynamic_jump(96_U256),
            expected_stack: &[97_U256],
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        dynamic_jump_to_127(@raw {
            bytecode: &bytecode_dynamic_jump(127_U256),
            expected_stack: &[128_U256],
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        dynamic_jump_to_130(@raw {
            bytecode: &bytecode_dynamic_jump(130_U256),
            expected_stack: &[131_U256],
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        dynamic_jump_to_65(@raw {
            bytecode: &bytecode_dynamic_jump(65_U256),
            expected_return: InstrStop::InvalidJump,
        }),
        dynamic_jump_to_97(@raw {
            bytecode: &bytecode_dynamic_jump(97_U256),
            expected_return: InstrStop::InvalidJump,
        }),
        dynamic_jump_to_code_end(@raw {
            bytecode: &bytecode_dynamic_jump(133_U256),
            expected_return: InstrStop::InvalidJump,
        }),
        dynamic_jump_to_table_end(@raw {
            bytecode: &bytecode_dynamic_jump(160_U256),
            expected_return: InstrStop::InvalidJump,
        }),
        dynamic_jump_to_u64_max(@raw {
            bytecode: &bytecode_dynamic_jump(U256::from(u64::MAX)),
            expected_return: InstrStop::InvalidJump,
        }),
        dynamic_jump_to_u64_overflow(@raw {
            bytecode: &bytecode_dynamic_jump((1_U256 << 64) + 63_U256),
            expected_return: InstrStop::InvalidJump,
        }),
    }

    arith {
        add1(op::ADD, 0_U256, 0_U256 => 0_U256),
        add2(op::ADD, 1_U256, 2_U256 => 3_U256),
        add3(op::ADD, 255_U256, 255_U256 => 510_U256),
        add4(op::ADD, U256::MAX, 1_U256 => 0_U256),
        add5(op::ADD, U256::MAX, 2_U256 => 1_U256),

        sub1(op::SUB, 3_U256, 2_U256 => 1_U256),
        sub2(op::SUB, 1_U256, 2_U256 => -1_U256),
        sub3(op::SUB, 1_U256, 3_U256 => (-1_U256).wrapping_sub(1_U256)),
        sub4(op::SUB, 255_U256, 255_U256 => 0_U256),

        mul1(op::MUL, 1_U256, 2_U256 => 2_U256),
        mul2(op::MUL, 32_U256, 32_U256 => 1024_U256),
        mul3(op::MUL, U256::MAX, 2_U256 => U256::MAX.wrapping_sub(1_U256)),

        div1(op::DIV, 32_U256, 32_U256 => 1_U256),
        div2(op::DIV, 1_U256, 2_U256 => 0_U256),
        div3(op::DIV, 2_U256, 2_U256 => 1_U256),
        div4(op::DIV, 3_U256, 2_U256 => 1_U256),
        div5(op::DIV, 4_U256, 2_U256 => 2_U256),
        div_by_one(op::DIV, 42_U256, 1_U256 => 42_U256),
        div_pow2_4(op::DIV, 100_U256, 4_U256 => 25_U256),
        div_pow2_256(op::DIV, 512_U256, 256_U256 => 2_U256),
        div_zero_dividend(op::DIV, 0_U256, 42_U256 => 0_U256),
        div_by_zero1(op::DIV, 0_U256, 0_U256 => 0_U256),
        div_by_zero2(op::DIV, 32_U256, 0_U256 => 0_U256),

        rem1(op::MOD, 32_U256, 32_U256 => 0_U256),
        rem2(op::MOD, 1_U256, 2_U256 => 1_U256),
        rem3(op::MOD, 2_U256, 2_U256 => 0_U256),
        rem4(op::MOD, 3_U256, 2_U256 => 1_U256),
        rem5(op::MOD, 4_U256, 2_U256 => 0_U256),
        rem_by_one(op::MOD, 42_U256, 1_U256 => 0_U256),
        rem_pow2_4(op::MOD, 100_U256, 4_U256 => 0_U256),
        rem_pow2_256(op::MOD, 513_U256, 256_U256 => 1_U256),
        rem_zero_dividend(op::MOD, 0_U256, 42_U256 => 0_U256),
        rem_by_zero1(op::MOD, 0_U256, 0_U256 => 0_U256),
        rem_by_zero2(op::MOD, 32_U256, 0_U256 => 0_U256),

        sdiv1(op::SDIV, 32_U256, 32_U256 => 1_U256),
        sdiv2(op::SDIV, 1_U256, 2_U256 => 0_U256),
        sdiv3(op::SDIV, 2_U256, 2_U256 => 1_U256),
        sdiv4(op::SDIV, 3_U256, 2_U256 => 1_U256),
        sdiv5(op::SDIV, 4_U256, 2_U256 => 2_U256),
        sdiv_by_zero1(op::SDIV, 0_U256, 0_U256 => 0_U256),
        sdiv_by_zero2(op::SDIV, 32_U256, 0_U256 => 0_U256),
        sdiv_min_by_1(op::SDIV, I256_MIN, 1_U256 => -I256_MIN),
        sdiv_min_by_minus_1(op::SDIV, I256_MIN, -1_U256 => I256_MIN),
        sdiv_max1(op::SDIV, I256_MAX, 1_U256 => I256_MAX),
        sdiv_max2(op::SDIV, I256_MAX, -1_U256 => -I256_MAX),
        sdiv_by_int_min(op::SDIV, I256_MIN, I256_MIN => 1_U256),
        sdiv_nonmin_by_int_min(op::SDIV, 42_U256, I256_MIN => 0_U256),
        sdiv_zero_dividend(op::SDIV, 0_U256, 42_U256 => 0_U256),
        sdiv_neg_by_2(op::SDIV, -3_U256, 2_U256 => -1_U256),
        sdiv_neg_by_4_exact(op::SDIV, -8_U256, 4_U256 => -2_U256),
        sdiv_neg_by_4(op::SDIV, -5_U256, 4_U256 => -1_U256),
        sdiv_by_minus_2(op::SDIV, 7_U256, -2_U256 => -3_U256),
        sdiv_neg_by_minus_2(op::SDIV, -7_U256, -2_U256 => 3_U256),
        sdiv_min_by_2(op::SDIV, I256_MIN, 2_U256 => -(I256_MIN >> 1usize)),
        sdiv_min_by_minus_2(op::SDIV, I256_MIN, -2_U256 => I256_MIN >> 1usize),
        sdiv_max_by_2_254(op::SDIV, I256_MAX, I256_MIN >> 1usize => 1_U256),
        sdiv_min_by_2_254(op::SDIV, I256_MIN, I256_MIN >> 1usize => -2_U256),
        sdiv_min_by_minus_2_254(op::SDIV, I256_MIN, -(I256_MIN >> 1usize) => 2_U256),
        sdiv_neg_by_minus_2_255(op::SDIV, -1_U256, -I256_MIN => 0_U256),
        sdiv_max_by_minus_2_255(op::SDIV, I256_MAX, -I256_MIN => 0_U256),

        srem1(op::SMOD, 32_U256, 32_U256 => 0_U256),
        srem2(op::SMOD, 1_U256, 2_U256 => 1_U256),
        srem3(op::SMOD, 2_U256, 2_U256 => 0_U256),
        srem4(op::SMOD, 3_U256, 2_U256 => 1_U256),
        srem5(op::SMOD, 4_U256, 2_U256 => 0_U256),
        srem_by_one(op::SMOD, 42_U256, 1_U256 => 0_U256),
        srem_by_minus_one(op::SMOD, 42_U256, -1_U256 => 0_U256),
        srem_zero_dividend(op::SMOD, 0_U256, 42_U256 => 0_U256),
        srem_by_zero1(op::SMOD, 0_U256, 0_U256 => 0_U256),
        srem_by_zero2(op::SMOD, 32_U256, 0_U256 => 0_U256),
        srem_neg_by_2(op::SMOD, -3_U256, 2_U256 => -1_U256),
        srem_neg_by_minus_4(op::SMOD, -7_U256, -4_U256 => -3_U256),
        srem_by_minus_4(op::SMOD, 7_U256, -4_U256 => 3_U256),
        srem_min_by_2(op::SMOD, I256_MIN, 2_U256 => 0_U256),
        srem_min_by_2_254(op::SMOD, I256_MIN, I256_MIN >> 1usize => 0_U256),
        srem_min_plus_1_by_minus_2_254(op::SMOD, I256_MIN + 1_U256, -(I256_MIN >> 1usize) => -((I256_MIN >> 1usize) - 1_U256)),
        srem_min_by_2_255(op::SMOD, I256_MIN, I256_MIN => 0_U256),
        srem_neg_by_2_255(op::SMOD, -1_U256, I256_MIN => -1_U256),
        srem_max_by_minus_2_255(op::SMOD, I256_MAX, -I256_MIN => I256_MAX),

        addmod_mod_zero(op::ADDMOD, 1_U256, 2_U256, 0_U256 => 0_U256),
        addmod_mod_one(op::ADDMOD, 5_U256, 3_U256, 1_U256 => 0_U256),
        addmod_both_zero(op::ADDMOD, 0_U256, 0_U256, 42_U256 => 0_U256),
        addmod1(op::ADDMOD, 1_U256, 2_U256, 3_U256 => 0_U256),
        addmod2(op::ADDMOD, 1_U256, 2_U256, 4_U256 => 3_U256),
        addmod3(op::ADDMOD, 1_U256, 2_U256, 2_U256 => 1_U256),
        addmod4(op::ADDMOD, 32_U256, 32_U256, 69_U256 => 64_U256),
        addmod_pow2(op::ADDMOD, 7_U256, 6_U256, 8_U256 => 5_U256),
        addmod_pow2_carry(op::ADDMOD, U256::MAX, 3_U256, 4_U256 => 2_U256),
        addmod_pow2_255_carry(op::ADDMOD, U256::MAX, U256::MAX, I256_MIN => I256_MAX - 1_U256),

        mulmod_mod_zero(op::MULMOD, 3_U256, 4_U256, 0_U256 => 0_U256),
        mulmod1(op::MULMOD, 0_U256, 0_U256, 1_U256 => 0_U256),
        mulmod2(op::MULMOD, 69_U256, 0_U256, 1_U256 => 0_U256),
        mulmod3(op::MULMOD, 0_U256, 1_U256, 2_U256 => 0_U256),
        mulmod_a_zero(op::MULMOD, 0_U256, 5_U256, 7_U256 => 0_U256),
        mulmod_b_zero(op::MULMOD, 5_U256, 0_U256, 7_U256 => 0_U256),
        mulmod4(op::MULMOD, 69_U256, 1_U256, 2_U256 => 1_U256),
        mulmod5(op::MULMOD, 69_U256, 1_U256, 30_U256 => 9_U256),
        mulmod6(op::MULMOD, 69_U256, 2_U256, 100_U256 => 38_U256),
        mulmod_pow2(op::MULMOD, 7_U256, 6_U256, 8_U256 => 2_U256),
        mulmod_pow2_wrap(op::MULMOD, U256::MAX, 3_U256, 16_U256 => 13_U256),
        mulmod_pow2_255_wrap(op::MULMOD, U256::MAX, U256::MAX, I256_MIN => 1_U256),

        exp1(op::EXP, 0_U256, 0_U256 => 1_U256; op_gas(10)),
        exp2(op::EXP, 2_U256, 0_U256 => 1_U256; op_gas(10)),
        exp3(op::EXP, 2_U256, 1_U256 => 2_U256; op_gas(60)),
        exp4(op::EXP, 2_U256, 2_U256 => 4_U256; op_gas(60)),
        exp5(op::EXP, 2_U256, 3_U256 => 8_U256; op_gas(60)),
        exp6(op::EXP, 2_U256, 4_U256 => 16_U256; op_gas(60)),
        exp_zero_base_zero_exp(op::EXP, 0_U256, 0_U256 => 1_U256; op_gas(10)),
        exp_zero_base_nonzero_exp(op::EXP, 0_U256, 5_U256 => 0_U256; op_gas(60)),
        exp_one_base_large_exp(op::EXP, 1_U256, U256::MAX => 1_U256; op_gas(1610)),
        exp_minus_one_base_even_exp(op::EXP, -1_U256, 4_U256 => 1_U256; op_gas(60)),
        exp_minus_one_base_odd_exp(op::EXP, -1_U256, 5_U256 => -1_U256; op_gas(60)),
        exp_pow4_base_in_range(op::EXP, 4_U256, 63_U256 => 1_U256 << 126; op_gas(60)),
        exp_pow4_base_overflow(op::EXP, 4_U256, 128_U256 => 0_U256; op_gas(60)),
        exp_non_pow2_base(op::EXP, 3_U256, 5_U256 => 243_U256; op_gas(60)),
        exp_overflow(op::EXP, 2_U256, 256_U256 => 0_U256; op_gas(110)),
        // Large exponent spanning multiple bytes.
        exp_large(op::EXP, 2_U256, 0xFFFF_U256 => 2_U256.pow(0xFFFF_U256); op_gas(110)),
        // Max exponent (32 bytes), dynamic = 50 * 32 = 1600, total = 10 + 1600 = 1610.
        exp_max_exponent(op::EXP, 2_U256, U256::MAX => 0_U256; op_gas(1610)),
        exp_byte_boundary_ff(op::EXP, 2_U256, 0xff_U256 => 1_U256 << 255; op_gas(60)),
        exp_byte_boundary_100(op::EXP, 1_U256, 0x100_U256 => 1_U256; op_gas(110)),
        exp_zero_base_max_exp(op::EXP, 0_U256, U256::MAX => 0_U256; op_gas(1610)),
        exp_minus_one_base_max_exp(op::EXP, -1_U256, U256::MAX => -1_U256; op_gas(1610)),

        signextend1(op::SIGNEXTEND, 0_U256, 0_U256 => 0_U256),
        signextend2(op::SIGNEXTEND, 1_U256, 0_U256 => 0_U256),
        signextend3(op::SIGNEXTEND, 0_U256, -1_U256 => -1_U256),
        signextend4(op::SIGNEXTEND, 1_U256, -1_U256 => -1_U256),
        signextend5(op::SIGNEXTEND, 0_U256, 0x7f_U256 => 0x7f_U256),
        signextend6(op::SIGNEXTEND, 0_U256, 0x80_U256 => -0x80_U256),
        signextend7(op::SIGNEXTEND, 0_U256, 0xff_U256 => U256::MAX),
        signextend8(op::SIGNEXTEND, 1_U256, 0x7fff_U256 => 0x7fff_U256),
        signextend8_extra(op::SIGNEXTEND, 1_U256, 0xff7fff_U256 => 0x7fff_U256),
        signextend9(op::SIGNEXTEND, 1_U256, 0x8000_U256 => -0x8000_U256),
        signextend9_extra(op::SIGNEXTEND, 1_U256, 0x118000_U256 => -0x8000_U256),
        signextend10(op::SIGNEXTEND, 1_U256, 0xffff_U256 => U256::MAX),
        signextend_noop_31(op::SIGNEXTEND, 31_U256, 0x42_U256 => 0x42_U256),
        signextend_noop_32(op::SIGNEXTEND, 32_U256, 0x42_U256 => 0x42_U256),
        signextend_noop_max(op::SIGNEXTEND, U256::MAX, 0x42_U256 => 0x42_U256),
    }

    cmp {
        lt1(op::LT, 1_U256, 2_U256 => 1_U256),
        lt2(op::LT, 2_U256, 1_U256 => 0_U256),
        lt3(op::LT, 1_U256, 1_U256 => 0_U256),
        lt4(op::LT, -1_U256, 1_U256 => 0_U256),

        gt1(op::GT, 1_U256, 2_U256 => 0_U256),
        gt2(op::GT, 2_U256, 1_U256 => 1_U256),
        gt3(op::GT, 1_U256, 1_U256 => 0_U256),
        gt4(op::GT, -1_U256, 1_U256 => 1_U256),

        slt1(op::SLT, 1_U256, 2_U256 => 1_U256),
        slt2(op::SLT, 2_U256, 1_U256 => 0_U256),
        slt3(op::SLT, 1_U256, 1_U256 => 0_U256),
        slt4(op::SLT, -1_U256, 1_U256 => 1_U256),

        sgt1(op::SGT, 1_U256, 2_U256 => 0_U256),
        sgt2(op::SGT, 2_U256, 1_U256 => 1_U256),
        sgt3(op::SGT, 1_U256, 1_U256 => 0_U256),
        sgt4(op::SGT, -1_U256, 1_U256 => 0_U256),

        eq1(op::EQ, 1_U256, 2_U256 => 0_U256),
        eq2(op::EQ, 2_U256, 1_U256 => 0_U256),
        eq3(op::EQ, 1_U256, 1_U256 => 1_U256),

        iszero1(op::ISZERO, 0_U256 => 1_U256),
        iszero2(op::ISZERO, 1_U256 => 0_U256),
        iszero3(op::ISZERO, 2_U256 => 0_U256),
    }

    bitwise {
        and1(op::AND, 0_U256, 0_U256 => 0_U256),
        and2(op::AND, 1_U256, 1_U256 => 1_U256),
        and3(op::AND, 1_U256, 2_U256 => 0_U256),
        and4(op::AND, 255_U256, 255_U256 => 255_U256),

        or1(op::OR, 0_U256, 0_U256 => 0_U256),
        or2(op::OR, 1_U256, 2_U256 => 3_U256),
        or3(op::OR, 1_U256, 3_U256 => 3_U256),
        or4(op::OR, 2_U256, 2_U256 => 2_U256),

        xor1(op::XOR, 0_U256, 0_U256 => 0_U256),
        xor2(op::XOR, 1_U256, 2_U256 => 3_U256),
        xor3(op::XOR, 1_U256, 3_U256 => 2_U256),
        xor4(op::XOR, 2_U256, 2_U256 => 0_U256),

        not1(op::NOT, 0_U256 => U256::MAX),
        not2(op::NOT, U256::MAX => 0_U256),
        not3(op::NOT, 1_U256 => U256::MAX.wrapping_sub(1_U256)),

        byte1(op::BYTE, 0_U256, 0x1122334400000000000000000000000000000000000000000000000000000000_U256 => 0x11_U256),
        byte2(op::BYTE, 1_U256, 0x1122334400000000000000000000000000000000000000000000000000000000_U256 => 0x22_U256),
        byte3(op::BYTE, 2_U256, 0x1122334400000000000000000000000000000000000000000000000000000000_U256 => 0x33_U256),
        byte4(op::BYTE, 3_U256, 0x1122334400000000000000000000000000000000000000000000000000000000_U256 => 0x44_U256),
        byte5(op::BYTE, 4_U256, 0x1122334400000000000000000000000000000000000000000000000000000000_U256 => 0x00_U256),
        byte_oob0(op::BYTE, 31_U256, U256::MAX => 0xFF_U256),
        byte_oob1(op::BYTE, 32_U256, U256::MAX => 0_U256),
        byte_oob2(op::BYTE, 33_U256, U256::MAX => 0_U256),

        // shift operand order is reversed for some reason:
        // shift, x
        shl1(op::SHL, 0_U256, 1_U256 => 1_U256),
        shl2(op::SHL, 1_U256, 1_U256 => 2_U256),
        shl3(op::SHL, 2_U256, 1_U256 => 4_U256),
        shl4(op::SHL, 255_U256, -1_U256 => -1_U256 << 255),
        shl5(op::SHL, 256_U256, -1_U256 => 0_U256),

        shr1(op::SHR, 0_U256, 1_U256 => 1_U256),
        shr2(op::SHR, 1_U256, 2_U256 => 1_U256),
        shr3(op::SHR, 2_U256, 4_U256 => 1_U256),
        shr4(op::SHR, 255_U256, -1_U256 => 1_U256),
        shr5(op::SHR, 256_U256, -1_U256 => 0_U256),

        sar1(op::SAR, 0_U256, 1_U256 => 1_U256),
        sar2(op::SAR, 1_U256, 2_U256 => 1_U256),
        sar3(op::SAR, 2_U256, 4_U256 => 1_U256),
        sar4(op::SAR, 1_U256, -1_U256 => -1_U256),
        sar5(op::SAR, 2_U256, -1_U256 => -1_U256),
        sar6(op::SAR, 255_U256, -1_U256 => -1_U256),
        sar7(op::SAR, 256_U256, -1_U256 => -1_U256),
    }

    system {
        gas0(@raw {
            bytecode: &[op::GAS, op::GAS, op::JUMPDEST, op::GAS],
            expected_stack: &[DEF_GAS_LIMIT_U256 - 2_U256, DEF_GAS_LIMIT_U256 - 4_U256, DEF_GAS_LIMIT_U256 - 7_U256],
            expected_gas: 2 + 2 + 1 + 2,
        }),
        // The section head charges the gas after each `GAS` up front.
        gas_mid_section(@raw {
            bytecode: &[op::PUSH0, op::GAS, op::PUSH1, 1, op::GAS, op::SWAP1, op::POP],
            expected_stack: &[0_U256, DEF_GAS_LIMIT_U256 - 4_U256, DEF_GAS_LIMIT_U256 - 9_U256],
            expected_gas: 2 + 2 + 3 + 2 + 3 + 2,
        }),
        // Folded `EXP` gas (10 + 50 * 2) after `GAS` is charged up front too.
        gas_before_folded_exp(@raw {
            bytecode: &[op::GAS, op::PUSH2, 1, 0, op::PUSH1, 2, op::EXP, op::POP, op::GAS],
            expected_stack: &[DEF_GAS_LIMIT_U256 - 2_U256, DEF_GAS_LIMIT_U256 - 122_U256],
            expected_gas: 2 + 3 + 3 + 110 + 2 + 2,
        }),
        // A constant exponent above 2 lowers to a gas-free builtin: its dynamic gas (10 + 50 * 2)
        // is charged up front with the section, once.
        gas_around_const_exponent_pow(@raw {
            bytecode: &[op::GAS, op::PUSH2, 1, 0, op::ADDRESS, op::EXP, op::POP, op::GAS],
            expected_stack: &[DEF_GAS_LIMIT_U256 - 2_U256, DEF_GAS_LIMIT_U256 - 121_U256],
            expected_gas: 2 + 3 + 2 + 110 + 2 + 2,
        }),
        // `SLOAD` ends the gas section, so only its base gas is owed by the first `GAS`.
        gas_around_sload(@raw {
            bytecode: &[op::GAS, op::PUSH1, 69, op::SLOAD, op::GAS],
            expected_stack: &[DEF_GAS_LIMIT_U256 - 2_U256, 42_U256, DEF_GAS_LIMIT_U256 - 107_U256],
            expected_gas: 2 + 3 + 100 + 2,
        }),
        keccak256_empty1(@raw {
            bytecode: &[op::PUSH0, op::PUSH0, op::KECCAK256],
            expected_stack: &[KECCAK_EMPTY.into()],
            expected_gas: 2 + 2 + gas::KECCAK256 as u64,
        }),
        keccak256_empty2(@raw {
            bytecode: &[op::PUSH0, op::PUSH1, 32, op::KECCAK256],
            expected_stack: &[KECCAK_EMPTY.into()],
            expected_gas: 2 + 3 + gas::KECCAK256 as u64,
        }),
        keccak256_empty_dynamic_offset(@raw {
            bytecode: &[op::PUSH0, op::ADDRESS, op::KECCAK256],
            expected_stack: &[KECCAK_EMPTY.into()],
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        keccak256_1(@raw {
            bytecode: &[op::PUSH1, 32, op::PUSH0, op::KECCAK256],
            expected_stack: &[keccak256([0; 32]).into()],
            expected_memory: &[0; 32],
            expected_gas: 3 + 2 + (keccak256_cost(32).unwrap() + 3),
        }),
        keccak256_2(@raw {
            bytecode: &[op::PUSH2, 0x69, 0x42, op::PUSH0, op::MSTORE, op::PUSH1, 0x20, op::PUSH0, op::KECCAK256],
            expected_stack: &[keccak256(0x6942_U256.to_be_bytes::<32>()).into()],
            expected_memory: &0x6942_U256.to_be_bytes::<32>(),
            expected_gas: 3 + 2 + (3 + 3) + 3 + 2 + keccak256_cost(32).unwrap(),
        }),

        address(@raw {
            bytecode: &[op::ADDRESS, op::ADDRESS],
            expected_stack: &[DEF_ADDR.into_word().into(), DEF_ADDR.into_word().into()],
            expected_gas: 4,
        }),
        origin(@raw {
            bytecode: &[op::ORIGIN, op::ORIGIN],
            expected_stack: &[def_env().tx.caller.into_word().into(), def_env().tx.caller.into_word().into()],
            expected_gas: 4,
        }),
        caller(@raw {
            bytecode: &[op::CALLER, op::CALLER],
            expected_stack: &[DEF_CALLER.into_word().into(), DEF_CALLER.into_word().into()],
            expected_gas: 4,
        }),
        callvalue(@raw {
            bytecode: &[op::CALLVALUE, op::CALLVALUE],
            expected_stack: &[DEF_VALUE, DEF_VALUE],
            expected_gas: 4,
        }),
    }

    calldata {
        calldataload1(@raw {
            bytecode: &[op::PUSH0, op::CALLDATALOAD],
            expected_stack: &[U256::from_be_slice(&DEF_CD[..32])],
            expected_gas: 2 + 3,
        }),
        calldataload2(@raw {
            bytecode: &[op::PUSH1, 63, op::CALLDATALOAD],
            expected_stack: &[0xaa00000000000000000000000000000000000000000000000000000000000000_U256],
            expected_gas: 3 + 3,
        }),
        calldatasize(@raw {
            bytecode: &[op::CALLDATASIZE, op::CALLDATASIZE],
            expected_stack: &[U256::from(DEF_CD.len()), U256::from(DEF_CD.len())],
            expected_gas: 2 + 2,
        }),
        calldatacopy(@raw {
            bytecode: &[op::PUSH1, 32, op::PUSH0, op::PUSH0, op::CALLDATACOPY],
            expected_memory: &DEF_CD[..32],
            expected_gas: 3 + 2 + 2 + (verylowcopy_cost(32).unwrap() + 3),
        }),
    }

    code {
        codesize(@raw {
            bytecode: &[op::CODESIZE, op::CODESIZE],
            expected_stack: &[2_U256, 2_U256],
            expected_gas: 2 + 2,
        }),
        codecopy(@raw {
            bytecode: &[op::PUSH1, 5, op::PUSH0, op::PUSH0, op::CODECOPY],
            expected_memory: &hex!("60055f5f39000000000000000000000000000000000000000000000000000000"),
            expected_gas: 3 + 2 + 2 + (verylowcopy_cost(32).unwrap() + memory_gas_cost(1)),
        }),
    }

    returndata {
        returndatasize(@raw {
            // No return data exists in this test context
            bytecode: &[op::RETURNDATASIZE, op::RETURNDATASIZE],
            expected_stack: &[0_U256, 0_U256],
            expected_gas: 2 + 2,
        }),
        returndatacopy(@raw {
            // No return data exists, so copying 32 bytes from offset 0 fails with OutOfOffset
            bytecode: &[op::PUSH1, 32, op::PUSH0, op::PUSH0, op::RETURNDATACOPY],
            expected_return: InstrStop::OutOfOffset,
        }),
    }

    extcode {
        extcodesize1(op::EXTCODESIZE, DEF_ADDR.into_word().into() => 0_U256;
            op_gas(100)),
        extcodesize2(op::EXTCODESIZE, OTHER_ADDR.into_word().into() => U256::from(def_codemap()[&OTHER_ADDR].len());
            op_gas(100)),
        extcodecopy1(@raw {
            bytecode: &[op::PUSH0, op::PUSH0, op::PUSH0, op::PUSH0, op::EXTCODECOPY],
            expected_memory: &[],
            expected_gas: 2 + 2 + 2 + 2 + 100,
        }),
        extcodecopy2(@raw {
            // bytecode: &[op::PUSH1, 64, op::PUSH0, op::PUSH0, op::PUSH20, OTHER_ADDR, op::EXTCODECOPY],
            bytecode: &hex!("6040 5f 5f 736969696969696969696969696969696969696969 3c"),
            expected_memory: &{
                let mut mem = [0; 64];
                let code = def_codemap()[&OTHER_ADDR].original_bytes();
                mem[..code.len()].copy_from_slice(&code);
                mem
            },
            expected_gas: 3 + 2 + 2 + 3 + (100 + 12),
        }),
        extcodehash1(op::EXTCODEHASH, DEF_ADDR.into_word().into() => KECCAK_EMPTY.into();
            op_gas(100)),
        extcodehash2(op::EXTCODEHASH, OTHER_ADDR.into_word().into() => def_codemap()[&OTHER_ADDR].hash_slow().into();
            op_gas(100)),
    }

    env {
        gas_price(@raw {
            bytecode: &[op::GASPRICE],
            expected_stack: &[def_env().effective_gas_price()],
            expected_gas: 2,
        }),
        // Host determines blockhash - EVM semantics:
        // - Current block returns 0
        // - Blocks 1-256 ago return valid hash
        // - Blocks more than 256 ago return 0
        blockhash0(op::BLOCKHASH, DEF_BN - 0_U256 => 0_U256),  // current block -> 0
        blockhash1(op::BLOCKHASH, DEF_BN - 1_U256 => DEF_BN - 1_U256),  // 1 block ago -> valid
        blockhash2(op::BLOCKHASH, DEF_BN - 255_U256 => DEF_BN - 255_U256),  // 255 blocks ago -> valid
        blockhash3(op::BLOCKHASH, DEF_BN - 256_U256 => DEF_BN - 256_U256),  // 256 blocks ago -> valid
        blockhash4(op::BLOCKHASH, DEF_BN - 257_U256 => 0_U256),  // 257 blocks ago -> 0 (too old)
        coinbase(@raw {
            bytecode: &[op::COINBASE, op::COINBASE],
            expected_stack: &[def_env().block.coinbase.into_word().into(), def_env().block.coinbase.into_word().into()],
            expected_gas: 4,
        }),
        timestamp(@raw {
            bytecode: &[op::TIMESTAMP, op::TIMESTAMP],
            expected_stack: &[def_env().block.timestamp, def_env().block.timestamp],
            expected_gas: 4,
        }),
        number(@raw {
            bytecode: &[op::NUMBER, op::NUMBER],
            expected_stack: &[def_env().block.number, def_env().block.number],
            expected_gas: 4,
        }),
        difficulty(@raw {
            bytecode: &[op::DIFFICULTY, op::DIFFICULTY],
            spec_id: SpecId::LONDON,
            expected_stack: &[def_env().block.difficulty, def_env().block.difficulty],
            expected_gas: 4,
        }),
        difficulty_prevrandao(@raw {
            bytecode: &[op::DIFFICULTY, op::DIFFICULTY],
            spec_id: SpecId::MERGE,
            expected_stack: &[def_env().block.prevrandao.unwrap().into(), def_env().block.prevrandao.unwrap().into()],
            expected_gas: 4,
        }),
        gaslimit(@raw {
            bytecode: &[op::GASLIMIT, op::GASLIMIT],
            expected_stack: &[def_env().block.gas_limit, def_env().block.gas_limit],
            expected_gas: 4,
        }),
        chainid(@raw {
            bytecode: &[op::CHAINID, op::CHAINID],
            expected_stack: &[U256::from(def_env().cfg.chain_id), U256::from(def_env().cfg.chain_id)],
            expected_gas: 4,
        }),
        selfbalance(@raw {
            bytecode: &[op::SELFBALANCE, op::SELFBALANCE],
            expected_stack: &[0xba_U256, 0xba_U256],
            expected_gas: 10,
        }),
        basefee(@raw {
            bytecode: &[op::BASEFEE, op::BASEFEE],
            expected_stack: &[def_env().block.basefee, def_env().block.basefee],
            expected_gas: 4,
        }),
        blobhash0(@raw {
            bytecode: &[op::PUSH0, op::BLOBHASH],
            expected_stack: &[def_env().tx.blob_hashes[0].into()],
            expected_gas: 2 + 3,
        }),
        blobhash1(@raw {
            bytecode: &[op::PUSH1, 1, op::BLOBHASH],
            expected_stack: &[def_env().tx.blob_hashes[1].into()],
            expected_gas: 3 + 3,
        }),
        blobhash2(@raw {
            bytecode: &[op::PUSH1, 2, op::BLOBHASH],
            expected_stack: &[0_U256],
            expected_gas: 3 + 3,
        }),
        blobbasefee(@raw {
            bytecode: &[op::BLOBBASEFEE, op::BLOBBASEFEE],
            expected_stack: &[def_env().block.blob_basefee, def_env().block.blob_basefee],
            expected_gas: 4,
        }),
        // Inlined environment values stay correct across a builtin call and feed arithmetic.
        env_across_builtin(@raw {
            bytecode: &[op::ORIGIN, op::TIMESTAMP, op::PUSH0, op::BLOBHASH, op::GASPRICE, op::ADD, op::COINBASE, op::XOR],
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
    }

    memory {
        mload1(@raw {
            bytecode: &[op::PUSH0, op::MLOAD],
            expected_stack: &[0_U256],
            expected_memory: &[0; 32],
            expected_gas: 2 + (3 + memory_gas_cost(1)),
        }),
        mload2(@raw {
            bytecode: &[op::PUSH1, 1, op::MLOAD],
            expected_stack: &[0_U256],
            expected_memory: &[0; 64],
            expected_gas: 3 + (3 + memory_gas_cost(2)),
        }),
        mload3(@raw {
            bytecode: &[op::PUSH1, 32, op::MLOAD],
            expected_stack: &[0_U256],
            expected_memory: &[0; 64],
            expected_gas: 3 + (3 + memory_gas_cost(2)),
        }),
        mload4(@raw {
            bytecode: &[op::PUSH1, 33, op::MLOAD],
            expected_stack: &[0_U256],
            expected_memory: &[0; 96],
            expected_gas: 3 + (3 + memory_gas_cost(3)),
        }),
        mload_overflow1(@raw {
            bytecode: &[op::PUSH8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, op::MLOAD],
            expected_return: InstrStop::MemoryOOG,
        }),
        mload_overflow2(@raw {
            bytecode: &[op::PUSH8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff - 1, op::MLOAD],
            expected_return: InstrStop::MemoryOOG,
        }),
        mload_overflow3(@raw {
            bytecode: &[op::PUSH8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff - 31, op::MLOAD],
            expected_return: InstrStop::MemoryOOG,
        }),
        mload_overflow4(@raw {
            bytecode: &[op::PUSH8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff - 32, op::MLOAD],
            expected_return: InstrStop::MemoryOOG,
        }),
        mload_overflow5(@raw {
            bytecode: &[op::PUSH8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff - 33, op::MLOAD],
            expected_return: InstrStop::MemoryOOG,
        }),
        mload_overflow6(@raw {
            bytecode: &[op::ADDRESS, op::MLOAD],
            expected_return: InstrStop::InvalidOperandOOG,
        }),
        mload_const_offset_too_large(@raw {
            bytecode: &[
                &[op::PUSH9][..],
                &0x1_0000_0000_0000_0000_U256.to_be_bytes::<32>()[23..],
                &[op::MLOAD][..],
            ]
            .concat(),
            expected_return: InstrStop::InvalidOperandOOG,
        }),
        mstore1(@raw {
            bytecode: &[op::PUSH0, op::PUSH0, op::MSTORE],
            expected_memory: &[0; 32],
            expected_gas: 2 + 2 + (3 + memory_gas_cost(1)),
        }),
        mstore_const_offset_dyn_value(@raw {
            bytecode: &bytecode_binop_mixed(op::MSTORE, 0_U256, 0x69_U256, true, false),
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        mstore_dyn_offset_const_value(@raw {
            bytecode: &bytecode_binop_mixed(op::MSTORE, 0_U256, 0x69_U256, false, true),
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        mstore_dyn_offset_dyn_value(@raw {
            bytecode: &bytecode_binop_mixed(op::MSTORE, 0_U256, 0x69_U256, false, false),
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        mstore8_1(@raw {
            bytecode: &[op::PUSH0, op::PUSH0, op::MSTORE8],
            expected_memory: &[0; 32],
            expected_gas: 2 + 2 + (3 + memory_gas_cost(1)),
        }),
        mstore8_2(@raw {
            bytecode: &[op::PUSH2, 0x69, 0x69, op::PUSH0, op::MSTORE8],
            expected_memory: &{
                let mut mem = [0; 32];
                mem[0] = 0x69;
                mem
            },
            expected_gas: 3 + 2 + (3 + memory_gas_cost(1)),
        }),
        msize1(@raw {
            bytecode: &[op::MSIZE, op::MSIZE],
            expected_stack: &[0_U256, 0_U256],
            expected_gas: 2 + 2,
        }),
        msize2(@raw {
            bytecode: &[op::MSIZE, op::PUSH0, op::MLOAD, op::POP, op::MSIZE, op::PUSH1, 1, op::MLOAD, op::POP, op::MSIZE],
            expected_stack: &[0_U256, 32_U256, 64_U256],
            expected_memory: &[0; 64],
            expected_gas: 2 + 2 + (3 + memory_gas_cost(1)) + 2 + 2 + 3 + (3 + (memory_gas_cost(2) - memory_gas_cost(1))) + 2 + 2,
        }),
        mcopy1(@raw {
            bytecode: &[op::PUSH1, 32, op::PUSH0, op::PUSH1, 32, op::MCOPY],
            expected_memory: &[0; 64],
            expected_gas: 3 + 2 + 3 + (verylowcopy_cost(32).unwrap() + memory_gas_cost(2)),
        }),
        mcopy2(@raw {
            bytecode: &[op::PUSH2, 0x42, 0x69, op::PUSH0, op::MSTORE,
                        op::PUSH1, 2, op::PUSH1, 30, op::PUSH1, 1, op::MCOPY],
            expected_memory: &{
                let mut mem = [0; 32];
                mem[30] = 0x42;
                mem[31] = 0x69;
                mem[1] = 0x42;
                mem[2] = 0x69;
                mem
            },
            expected_gas: 3 + 2 + (3 + memory_gas_cost(1)) +
                          3 + 3 + 3 + verylowcopy_cost(2).unwrap(),
        }),
        // Repeated accesses through one dynamic offset `p = 64`, growing and shrinking.
        dyn_offset_repeated(@raw {
            bytecode: &asm("
                CALLDATASIZE
                PUSH 0x11
                DUP2
                MSTORE          ; [p, p+32)
                PUSH 0x22
                DUP2
                MSTORE8         ; [p, p+1)
                DUP1
                PUSH 0x20
                ADD
                MLOAD           ; [p+32, p+64)
                POP
                DUP1
                PUSH 0x1f
                ADD
                MLOAD           ; [p+31, p+63)
                SWAP1
                MLOAD           ; [p, p+32)
            "),
            expected_stack: &[0x11_U256 << 248, (0x22_U256 << 248) | 0x11_U256],
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // Accesses through `p = 64` that straddle the ensured range and the memory size.
        dyn_offset_straddle(@raw {
            bytecode: &asm("
                CALLDATASIZE
                DUP1
                MLOAD           ; [p, p+32), memory 96
                PUSH 0x33
                PUSH 0x10
                DUP4
                ADD
                MSTORE          ; [p+16, p+48), memory 128
                DUP2
                PUSH 0x11
                ADD
                MLOAD           ; [p+17, p+49), no resize
                MSIZE
                DUP4
                PUSH 0x28
                ADD
                MLOAD           ; [p+40, p+72), memory 160
                MSIZE
            "),
            expected_stack: &[64_U256, 0_U256, 0x3300_U256, 128_U256, 0x33_U256 << 192, 160_U256],
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // `q + 65` wraps to 0, which proves that `q + 97` wraps to 32 but not that `q` itself
        // is in bounds.
        dyn_offset_wrapping_add(@raw {
            bytecode: &asm("
                CALLDATASIZE
                NOT
                DUP1
                PUSH 0x41
                ADD
                MLOAD
                POP
                DUP1
                PUSH 0x61
                ADD
                MLOAD
                POP
                MLOAD
            "),
            expected_return: InstrStop::InvalidOperandOOG,
        }),
        // `TLOAD` overwrites the stack slot holding `p = 64` with `0x1000`.
        dyn_offset_overwritten_by_builtin(@raw {
            bytecode: &asm("
                CALLDATASIZE
                DUP1
                MLOAD
                POP
                PUSH 0x1000
                DUP2
                TSTORE
                TLOAD
                MLOAD
                MSIZE
            "),
            expected_stack: &[0_U256, 0x1020_U256],
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // The store on the skipped path must not cover the load after the join.
        const_offset_after_join(@raw {
            bytecode: &asm("
                CALLDATASIZE
                PUSH %skip
                JUMPI
                PUSH 0x69
                PUSH 0x1000
                MSTORE
            skip:
                JUMPDEST
                PUSH 0x1000
                MLOAD
            "),
            expected_stack: &[0_U256],
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        mload_known_word(@raw {
            bytecode: &asm("
                PUSH 0x80
                PUSH 0x40
                MSTORE
                PUSH %next
                JUMP
            next:
                JUMPDEST
                PUSH 0x40
                MLOAD
            "),
            expected_stack: &[0x80_U256],
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        mload_known_word_join(@raw {
            bytecode: &asm("
                PUSH 0x80
                PUSH 0x40
                MSTORE
                CALLDATASIZE
                PUSH %a
                JUMPI
                PUSH 0x80
                PUSH 0x40
                MSTORE
                PUSH %join
                JUMP
            a:
                JUMPDEST
                PUSH 0xa0
                PUSH 0x40
                MSTORE
            join:
                JUMPDEST
                PUSH 0x40
                MLOAD
            "),
            expected_stack: &[0xa0_U256],
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        mload_known_word_after_dyn_mstore(@raw {
            bytecode: &asm("
                PUSH 0x80
                PUSH 0x40
                MSTORE
                PUSH 0x42
                CALLVALUE
                DUP1
                PUSH 0x40
                ADD
                SUB
                MSTORE
                PUSH 0x40
                MLOAD
            "),
            expected_stack: &[0x42_U256],
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        mload_known_word_after_unaligned_mstore(@raw {
            bytecode: &asm("
                PUSH 0x80
                PUSH 0x40
                MSTORE
                PUSH 0x42
                PUSH 0x30
                MSTORE
                PUSH 0x40
                MLOAD
            "),
            expected_stack: &[0x42_U256 << 128 | 0x80_U256],
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        mload_known_word_after_mstore8(@raw {
            bytecode: &asm("
                PUSH 0x80
                PUSH 0x40
                MSTORE
                PUSH 0x11
                PUSH 0x5f
                MSTORE8
                PUSH 0x40
                MLOAD
                PUSH 0x22
                PUSH 0x40
                MSTORE8
                PUSH 0x40
                MLOAD
            "),
            expected_stack: &[0x11_U256, 0x22_U256 << 248 | 0x11_U256],
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        mload_known_word_after_copy(@raw {
            bytecode: &asm("
                PUSH 0x80
                PUSH 0x40
                MSTORE
                PUSH 1
                PUSH0
                PUSH 0x5f
                CALLDATACOPY
                PUSH 0x40
                MLOAD
                PUSH 1
                PUSH0
                PUSH 0x40
                CODECOPY
                PUSH 0x40
                MLOAD
                PUSH 1
                PUSH 0x5f
                PUSH 0x41
                MCOPY
                PUSH 0x40
                MLOAD
                PUSH 1
                PUSH0
                PUSH 0x5f
                PUSH0
                EXTCODECOPY
                PUSH 0x40
                MLOAD
            "),
            expected_stack: &[
                0xaa_U256,
                0x60_U256 << 248 | 0xaa_U256,
                0x60aa_U256 << 240 | 0xaa_U256,
                0x60aa_U256 << 240,
            ],
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
    }

    host {
        balance(op::BALANCE, 0_U256 => 0_U256; op_gas(100)),
        sload1(@raw {
            bytecode: &[op::PUSH1, 69, op::SLOAD],
            expected_stack: &[42_U256],
            expected_gas: 3 + 100,
        }),
        sload2(@raw {
            bytecode: &[op::PUSH1, 70, op::SLOAD],
            expected_stack: &[0_U256],
            expected_gas: 3 + 100, // Test fixture warms storage.
        }),
        sload3(@raw {
            bytecode: &[op::PUSH1, 0xff, op::SLOAD],
            expected_stack: &[0_U256],
            expected_gas: 3 + 100, // Test fixture warms storage.
        }),
        sstore1(@raw {
            bytecode: &[op::PUSH1, 200, op::SLOAD, op::PUSH1, 100, op::PUSH1, 200, op::SSTORE, op::PUSH1, 200, op::SLOAD],
            expected_stack: &[0_U256, 100_U256],
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
            assert_host: Some(|host| {
                assert_eq!(host.storage.get(&200_U256), Some(&100_U256));
            }),
        }),
        // EIP-2200: `SSTORE` needs more than the call stipend left. The gas of the
        // instructions after it does not count.
        sstore_stipend_boundary_ok(@raw {
            bytecode: &[op::PUSH1, 1, op::PUSH0, op::SSTORE, op::PUSH0, op::POP],
            gas_limit: 3 + 2 + 2301,
            expected_gas: 3 + 2 + 100 + 2 + 2,
        }),
        sstore_stipend_boundary_fail(@raw {
            bytecode: &[op::PUSH1, 1, op::PUSH0, op::SSTORE, op::PUSH0, op::POP],
            gas_limit: 3 + 2 + 2300,
            expected_return: InstrStop::ReentrancySentryOOG,
        }),
        sstore_const_inputs(@raw {
            bytecode: &[op::PUSH1, 42, op::PUSH0, op::SSTORE],
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
            assert_host: Some(|host| {
                assert_eq!(host.storage.get(&U256::ZERO), Some(&42_U256));
            }),
        }),
        tstore_const_inputs(@raw {
            bytecode: &[op::PUSH1, 42, op::PUSH1, 69, op::TSTORE],
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
            assert_host: Some(|host| {
                assert_eq!(host.transient_storage.get(&69_U256), Some(&42_U256));
            }),
        }),
        sstore_constantinople(@raw {
            bytecode: &[op::PC, op::PC, op::SSTORE, op::PC, op::COINBASE],
            spec_id: SpecId::PETERSBURG,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        tload(@raw {
            bytecode: &[op::PUSH1, 69, op::TLOAD],
            expected_stack: &[0_U256],
            expected_gas: 3 + 100,
            assert_host: Some(|host| {
                assert!(host.transient_storage.is_empty());
            }),
        }),
        tstore(@raw {
            bytecode: &[op::PUSH1, 69, op::TLOAD, op::PUSH1, 42, op::PUSH1, 69, op::TSTORE, op::PUSH1, 69, op::TLOAD],
            expected_stack: &[0_U256, 42_U256],
            expected_gas: 3 + 100 + 3 + 3 + 100 + 3 + 100,
            assert_host: Some(|host| {
                assert_eq!(host.transient_storage.get(&69_U256), Some(&42_U256));
            }),
        }),
        log0(@raw {
            bytecode: &[op::PUSH0, op::PUSH0, op::LOG0],
            expected_gas: 2 + 2 + log_cost(0, 0).unwrap(),
            assert_host: Some(|host| {
                assert_eq!(host.logs, [Log {
                    address: DEF_ADDR,
                    data: LogData::new(vec![], Bytes::new()).unwrap(),
                }]);
            }),
        }),
        log0_data(@raw {
            bytecode: &[op::PUSH2, 0x69, 0x42, op::PUSH0, op::MSTORE, op::PUSH1, 32, op::PUSH0, op::LOG0],
            expected_memory: &0x6942_U256.to_be_bytes::<32>(),
            expected_gas: 3 + 2 + (3 + memory_gas_cost(1)) + 3 + 2 + log_cost(0, 32).unwrap(),
            assert_host: Some(|host| {
                assert_eq!(host.logs, [Log {
                    address: DEF_ADDR,
                    data: LogData::new(vec![], Bytes::from(0x6942_U256.to_be_bytes::<32>())).unwrap(),
                }]);
            }),
        }),
        log1_1(@raw {
            bytecode: &[op::PUSH0, op::PUSH0, op::PUSH0, op::LOG1],
            expected_gas: 2 + 2 + 2 + log_cost(1, 0).unwrap(),
            assert_host: Some(|host| {
                assert_eq!(host.logs, [Log {
                    address: DEF_ADDR,
                    data: LogData::new(vec![B256::ZERO], Bytes::new()).unwrap(),
                }]);
            }),
        }),
        log1_2(@raw {
            bytecode: &hex!(
                "7f000000000000000000000000ffffffffffffffffffffffffffffffffffffffff"
                "7f0000000000000000000000000000000000000000000000000000000000000032" // 50
                "59"
                "a1"
            ),
            expected_memory: &[0; 64],
            expected_gas: 3 + 3 + 2 + (log_cost(1, 50).unwrap() + memory_gas_cost(2)),
            assert_host: Some(|host| {
                assert_eq!(host.logs, [Log {
                    address: DEF_ADDR,
                    data: LogData::new(
                        vec![0xffffffffffffffffffffffffffffffffffffffff_U256.into()],
                        Bytes::from([0; 50]),
                    ).unwrap(),
                }]);
            }),
        }),
        log3_topic_does_not_resize_memory(@raw {
            bytecode: &[
                op::PUSH2, 0x08, 0x00, op::PUSH0, op::PUSH0, op::PUSH0, op::PUSH0, op::LOG3,
                op::PUSH0, op::PUSH0, op::MSTORE,
            ],
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        create(@raw {
            bytecode: &[op::PUSH1, 0x69, op::PUSH0, op::MSTORE, op::PUSH1, 32, op::PUSH0, op::PUSH1, 0x42, op::CREATE],
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        create2(@raw {
            bytecode: &[op::PUSH1, 0x69, op::PUSH0, op::MSTORE, op::PUSH1, 100, op::PUSH1, 32, op::PUSH0, op::PUSH1, 0x42, op::CREATE2],
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        call(@raw {
            bytecode: &[
                op::PUSH1, 1, // ret length
                op::PUSH1, 2, // ret offset
                op::PUSH1, 3, // args length
                op::PUSH1, 4, // args offset
                op::PUSH1, 5, // value
                op::PUSH1, 6, // address
                op::PUSH1, 7, // gas
                op::CALL,
            ],
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        callcode(@raw {
            bytecode: &[
                op::PUSH1, 1, // ret length
                op::PUSH1, 2, // ret offset
                op::PUSH1, 3, // args length
                op::PUSH1, 4, // args offset
                op::PUSH1, 5, // value
                op::PUSH1, 6, // address
                op::PUSH1, 7, // gas
                op::CALLCODE,
            ],
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        delegatecall(@raw {
            bytecode: &[
                op::PUSH1, 1, // ret length
                op::PUSH1, 2, // ret offset
                op::PUSH1, 3, // args length
                op::PUSH1, 4, // args offset
                op::PUSH1, 5, // address
                op::PUSH1, 6, // gas
                op::DELEGATECALL,
            ],
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        staticcall(@raw {
            bytecode: &[
                op::PUSH1, 1, // ret length
                op::PUSH1, 2, // ret offset
                op::PUSH1, 3, // args length
                op::PUSH1, 4, // args offset
                op::PUSH1, 5, // address
                op::PUSH1, 6, // gas
                op::STATICCALL,
            ],
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        ret_empty_dynamic_offset(@raw {
            bytecode: &[op::PUSH0, op::ADDRESS, op::RETURN],
            expected_return: InstrStop::Return,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        ret(@raw {
            bytecode: &[op::PUSH1, 0x69, op::PUSH0, op::MSTORE, op::PUSH1, 32, op::PUSH0, op::RETURN],
            expected_return: InstrStop::Return,
            expected_memory: &0x69_U256.to_be_bytes::<32>(),
            expected_gas: 3 + 2 + (3 + memory_gas_cost(1)) + 3 + 2,
            expected_output: Some(&0x69_U256.to_be_bytes::<32>()),
        }),
        revert(@raw {
            bytecode: &[op::PUSH1, 0x69, op::PUSH0, op::MSTORE, op::PUSH1, 32, op::PUSH0, op::REVERT],
            expected_return: InstrStop::Revert,
            expected_memory: &0x69_U256.to_be_bytes::<32>(),
            expected_gas: 3 + 2 + (3 + memory_gas_cost(1)) + 3 + 2,
            expected_output: Some(&0x69_U256.to_be_bytes::<32>()),
        }),
        selfdestruct(@raw {
            bytecode: &[op::PUSH1, 0x69, op::SELFDESTRUCT, op::INVALID],
            expected_return: InstrStop::SelfDestruct,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        selfdestruct_preserves_remaining_stack(@raw {
            bytecode: &[op::NUMBER, op::NUMBER, op::SELFDESTRUCT],
            spec_id: SpecId::BERLIN,
            expected_return: InstrStop::SelfDestruct,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // Static-context SELFDESTRUCT: gas < 5000 → OOG before static-call check.
        selfdestruct_static_oog(@raw {
            bytecode: &[op::PUSH1, 0x01, op::SELFDESTRUCT],
            is_static: true,
            gas_limit: 4000,
            expected_return: InstrStop::OutOfGas,
        }),
        // Static-context SELFDESTRUCT: gas >= 5000 → charges 5000, then StateChangeDuringStaticCall.
        selfdestruct_static_enough_gas(@raw {
            bytecode: &[op::PUSH1, 0x01, op::SELFDESTRUCT],
            is_static: true,
            expected_return: InstrStop::StateChangeDuringStaticCall,
        }),
    }

    // Tests for CALL gas accounting fix.
    // JIT CALL gas must match interpreter across specs and value-transfer scenarios.
    call_gas {
        call_value_cancun(@raw {
            bytecode: &[
                op::PUSH1, 1,   // ret length
                op::PUSH1, 2,   // ret offset
                op::PUSH1, 3,   // args length
                op::PUSH1, 4,   // args offset
                op::PUSH1, 5,   // value (non-zero -> triggers value transfer gas)
                op::PUSH1, 6,   // address
                op::PUSH1, 7,   // gas
                op::CALL,
            ],
            spec_id: SpecId::CANCUN,
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        call_value_static_cancun(@raw {
            bytecode: &[
                op::PUSH1, 1,   // ret length
                op::PUSH1, 2,   // ret offset
                op::PUSH1, 3,   // args length
                op::PUSH1, 4,   // args offset
                op::PUSH1, 5,   // value
                op::PUSH1, 6,   // address
                op::PUSH1, 7,   // gas
                op::CALL,
            ],
            spec_id: SpecId::CANCUN,
            is_static: true,
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        call_no_value_cancun(@raw {
            bytecode: &[
                op::PUSH1, 1,   // ret length
                op::PUSH1, 2,   // ret offset
                op::PUSH1, 3,   // args length
                op::PUSH1, 4,   // args offset
                op::PUSH0,      // value = 0 (no transfer)
                op::PUSH1, 6,   // address
                op::PUSH1, 7,   // gas
                op::CALL,
            ],
            spec_id: SpecId::CANCUN,
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        staticcall_cancun(@raw {
            bytecode: &[
                op::PUSH1, 1,   // ret length
                op::PUSH1, 2,   // ret offset
                op::PUSH1, 3,   // args length
                op::PUSH1, 4,   // args offset
                op::PUSH1, 5,   // address
                op::PUSH1, 6,   // gas
                op::STATICCALL,
            ],
            spec_id: SpecId::CANCUN,
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        staticcall_child_callvalue_is_zero(@raw {
            bytecode: &[
                op::PUSH1, 0x20, // ret length
                op::PUSH0,       // ret offset
                op::PUSH0,       // args length
                op::PUSH0,       // args offset
                op::PUSH1, 0x68, // address
                op::PUSH2, 0x27, 0x10, // gas
                op::STATICCALL,
                op::POP,
                op::PUSH1, 0x20,
                op::PUSH0,
                op::RETURN,
            ],
            spec_id: SpecId::CANCUN,
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_memory: &U256::ZERO.to_be_bytes::<32>(),
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
            expected_output: Some(&U256::ZERO.to_be_bytes::<32>()),
        }),
        delegatecall_cancun(@raw {
            bytecode: &[
                op::PUSH1, 1,   // ret length
                op::PUSH1, 2,   // ret offset
                op::PUSH1, 3,   // args length
                op::PUSH1, 4,   // args offset
                op::PUSH1, 5,   // address
                op::PUSH1, 6,   // gas
                op::DELEGATECALL,
            ],
            spec_id: SpecId::CANCUN,
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),

        // Pre-Berlin specs: different static gas costs for CALL (700 vs 100).
        call_value_petersburg(@raw {
            bytecode: &[
                op::PUSH1, 1,   // ret length
                op::PUSH1, 2,   // ret offset
                op::PUSH1, 3,   // args length
                op::PUSH1, 4,   // args offset
                op::PUSH1, 5,   // value (non-zero)
                op::PUSH1, 6,   // address
                op::PUSH1, 7,   // gas
                op::CALL,
            ],
            spec_id: SpecId::PETERSBURG,
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        call_no_value_petersburg(@raw {
            bytecode: &[
                op::PUSH1, 1,   // ret length
                op::PUSH1, 2,   // ret offset
                op::PUSH1, 3,   // args length
                op::PUSH1, 4,   // args offset
                op::PUSH1, 0,   // value = 0
                op::PUSH1, 6,   // address
                op::PUSH1, 7,   // gas
                op::CALL,
            ],
            spec_id: SpecId::PETERSBURG,
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        staticcall_petersburg(@raw {
            bytecode: &[
                op::PUSH1, 1,   // ret length
                op::PUSH1, 2,   // ret offset
                op::PUSH1, 3,   // args length
                op::PUSH1, 4,   // args offset
                op::PUSH1, 5,   // address
                op::PUSH1, 6,   // gas
                op::STATICCALL,
            ],
            spec_id: SpecId::PETERSBURG,
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        call_value_istanbul(@raw {
            bytecode: &[
                op::PUSH1, 1,   // ret length
                op::PUSH1, 2,   // ret offset
                op::PUSH1, 3,   // args length
                op::PUSH1, 4,   // args offset
                op::PUSH1, 5,   // value (non-zero)
                op::PUSH1, 6,   // address
                op::PUSH1, 7,   // gas
                op::CALL,
            ],
            spec_id: SpecId::ISTANBUL,
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        call_no_value_istanbul(@raw {
            bytecode: &[
                op::PUSH1, 1,   // ret length
                op::PUSH1, 2,   // ret offset
                op::PUSH1, 3,   // args length
                op::PUSH1, 4,   // args offset
                op::PUSH1, 0,   // value = 0
                op::PUSH1, 6,   // address
                op::PUSH1, 7,   // gas
                op::CALL,
            ],
            spec_id: SpecId::ISTANBUL,
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        staticcall_istanbul(@raw {
            bytecode: &[
                op::PUSH1, 1,   // ret length
                op::PUSH1, 2,   // ret offset
                op::PUSH1, 3,   // args length
                op::PUSH1, 4,   // args offset
                op::PUSH1, 5,   // address
                op::PUSH1, 6,   // gas
                op::STATICCALL,
            ],
            spec_id: SpecId::ISTANBUL,
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        call_value_byzantium(@raw {
            bytecode: &[
                op::PUSH1, 1,   // ret length
                op::PUSH1, 2,   // ret offset
                op::PUSH1, 3,   // args length
                op::PUSH1, 4,   // args offset
                op::PUSH1, 5,   // value (non-zero)
                op::PUSH1, 6,   // address
                op::PUSH1, 7,   // gas
                op::CALL,
            ],
            spec_id: SpecId::BYZANTIUM,
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        call_high_gas_value_istanbul(@raw {
            bytecode: &[
                op::PUSH1, 32,       // ret length
                op::PUSH1, 0,        // ret offset
                op::PUSH1, 64,       // args length
                op::PUSH1, 0,        // args offset
                op::PUSH1, 100,      // value = 100
                op::PUSH1, 0x69,     // address
                op::PUSH2, 0xFF, 0xFF, // gas = 65535
                op::CALL,
            ],
            spec_id: SpecId::ISTANBUL,
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // Muir Glacier uses ISTANBUL gas table.
        call_value_muir_glacier(@raw {
            bytecode: &[
                op::PUSH1, 1,   // ret length
                op::PUSH1, 2,   // ret offset
                op::PUSH1, 3,   // args length
                op::PUSH1, 4,   // args offset
                op::PUSH1, 5,   // value (non-zero)
                op::PUSH1, 6,   // address
                op::PUSH1, 7,   // gas
                op::CALL,
            ],
            spec_id: SpecId::ISTANBUL,
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        call_high_gas_value(@raw {
            bytecode: &[
                op::PUSH1, 32,       // ret length
                op::PUSH0,           // ret offset
                op::PUSH1, 64,       // args length
                op::PUSH0,           // args offset
                op::PUSH1, 100,      // value = 100
                op::PUSH1, 0x69,     // address
                op::PUSH2, 0xFF, 0xFF, // gas = 65535
                op::CALL,
            ],
            spec_id: SpecId::CANCUN,
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        call_known_bytecode(@raw {
            bytecode: &[
                op::PUSH1, 0,    // ret length
                op::PUSH1, 0,    // ret offset
                op::PUSH1, 0,    // args length
                op::PUSH1, 0,    // args offset
                op::PUSH1, 0,    // value
                op::PUSH1, 0x69, // address (OTHER_ADDR = 0x69..69, has code in fixture DB)
                op::PUSH1, 7,    // gas
                op::CALL,
            ],
            spec_id: SpecId::CANCUN,
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        // The callee returns its remaining gas: 63/64 of the gas left after the call's own
        // costs (16 before it, 100 static, 3 memory, 2500 cold), not counting the gas after it.
        call_forwards_gas(@raw {
            bytecode: &[
                op::PUSH1, 32,   // ret length
                op::PUSH0,       // ret offset
                op::PUSH0,       // args length
                op::PUSH0,       // args offset
                op::PUSH0,       // value
                op::PUSH1, 0x67, // address
                op::GAS,         // gas
                op::CALL,
                op::PUSH0,
                op::MLOAD,
            ],
            expected_stack: &[1_U256, 95858_U256],
            expected_memory: &95858_U256.to_be_bytes::<32>(),
            expected_gas: 16 + 100 + 3 + 2500 + 15 + 2 + 3,
        }),
        call_gas_after_dynamic_target(@raw {
            bytecode: &[
                op::PUSH1, 0,     // ret length
                op::PUSH1, 0,     // ret offset
                op::PUSH1, 0,     // args length
                op::PUSH1, 0,     // args offset
                op::CALLVALUE,    // value
                op::PUSH1, 0,
                op::CALLDATALOAD, // address
                op::GAS,          // gas
                op::CALL,
                op::PUSH1, 0,
                op::SSTORE,
                op::PUSH1, 1,
                op::PUSH1, 1,
                op::SSTORE,
                op::STOP,
            ],
            spec_id: SpecId::CANCUN,
            modify_message: Some(|message| {
                message.input = Bytes::from(B256::with_last_byte(0x68)).into();
                message.value = U256::from(10);
            }),
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
        staticcall_loop_memory_gas(@raw {
            bytecode: &asm("
            loop:
                JUMPDEST
                PUSH 0x32
                PUSH 0x80
                MLOAD
                LT
                ISZERO
                PUSH %done
                JUMPI
                PUSH0
                PUSH0
                PUSH0
                PUSH0
                PUSH 0x69
                PUSH 0x148c1c2280
                STATICCALL
                PUSH0
                SSTORE
                PUSH 0x01
                PUSH 0x80
                MLOAD
                ADD
                PUSH 0x80
                MSTORE
                PUSH %loop
                JUMP
            done:
                JUMPDEST
                PUSH 0x80
                MLOAD
                PUSH 0x01
                SSTORE
                STOP
            "),
            spec_id: SpecId::CANCUN,
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
    }

    // Tests for i256 correctness under LLVM optimization (a09436d1).
    // These exercise full 256-bit (4 x i64 lane) arithmetic that LLVM may miscompile
    // if i256 load/store alignment or alias attributes are incorrect.
    i256_lanes {
        // ADD of two large 256-bit values that use all 4 i64 lanes.
        add_full_width(op::ADD,
            0xDEADBEEF_12345678_ABCDEF01_23456789_FEDCBA98_76543210_01234567_89ABCDEF_U256,
            0x11111111_22222222_33333333_44444444_55555555_66666666_77777777_88888888_U256
            => 0xDEADBEEF_12345678_ABCDEF01_23456789_FEDCBA98_76543210_01234567_89ABCDEF_U256
             + 0x11111111_22222222_33333333_44444444_55555555_66666666_77777777_88888888_U256
        ),
        // MUL of two values producing a result spanning all lanes.
        mul_full_width(op::MUL,
            0x00000000_00000000_00000000_00000000_FFFFFFFF_FFFFFFFF_FFFFFFFF_FFFFFFFF_U256,
            0x00000000_00000000_00000000_00000000_00000000_00000000_00000000_00000002_U256
            => 0x00000000_00000000_00000000_00000001_FFFFFFFF_FFFFFFFF_FFFFFFFF_FFFFFFFE_U256
        ),
        // SUB across lane boundaries (borrow propagation through all 4 lanes).
        sub_cross_lane(op::SUB,
            0x00000000_00000000_00000000_00000001_00000000_00000000_00000000_00000000_U256,
            0x00000000_00000000_00000000_00000000_00000000_00000000_00000000_00000001_U256
            => 0x00000000_00000000_00000000_00000000_FFFFFFFF_FFFFFFFF_FFFFFFFF_FFFFFFFF_U256
        ),
        // Bitwise OR: each lane has different bit patterns.
        or_all_lanes(op::OR,
            0xFF000000_00000000_00FF0000_00000000_0000FF00_00000000_000000FF_00000000_U256,
            0x00000000_FF000000_00000000_00FF0000_00000000_0000FF00_00000000_000000FF_U256
            => 0xFF000000_FF000000_00FF0000_00FF0000_0000FF00_0000FF00_000000FF_000000FF_U256
        ),
        // SHL: shift left across lane boundaries (operand order: shift, value).
        shl_cross_lane(op::SHL,
            64_U256,
            0x00000000_00000000_00000000_00000000_00000000_00000000_00000000_00000001_U256
            => 0x00000000_00000000_00000000_00000000_00000000_00000001_00000000_00000000_U256
        ),
        // SHR: shift right across lane boundaries (operand order: shift, value).
        shr_cross_lane(op::SHR,
            64_U256,
            0x00000000_00000001_00000000_00000000_00000000_00000000_00000000_00000000_U256
            => 0x00000000_00000000_00000000_00000001_00000000_00000000_00000000_00000000_U256
        ),
    }

    // Tests for CODECOPY correctness.
    // CODECOPY must read bytecode from EvmContext at runtime, not from embedded compile-time
    // pointers, so that AOT-cached code works after deserialization.
    codecopy_fix {
        codecopy_exact(@raw {
            bytecode: &[op::PUSH1, 5, op::PUSH0, op::PUSH0, op::CODECOPY],
            expected_memory: &hex!("60055f5f39000000000000000000000000000000000000000000000000000000"),
            expected_gas: 3 + 2 + 2 + (verylowcopy_cost(32).unwrap() + memory_gas_cost(1)),
        }),
        codecopy_offset(@raw {
            bytecode: &[op::PUSH1, 3, op::PUSH1, 2, op::PUSH0, op::CODECOPY],
            expected_memory: &hex!("60025f0000000000000000000000000000000000000000000000000000000000"),
            expected_gas: 3 + 3 + 2 + (verylowcopy_cost(32).unwrap() + memory_gas_cost(1)),
        }),
        codecopy_beyond(@raw {
            bytecode: &[op::PUSH1, 10, op::PUSH0, op::PUSH0, op::CODECOPY],
            expected_memory: &hex!("600a5f5f39000000000000000000000000000000000000000000000000000000"),
            expected_gas: 3 + 2 + 2 + (verylowcopy_cost(32).unwrap() + memory_gas_cost(1)),
        }),
        codecopy_longer(@raw {
            bytecode: &[
                op::PUSH1, 42,  // dummy: push 42
                op::POP,        // pop it
                op::PUSH1, 99,  // another dummy
                op::POP,        // pop it
                op::PUSH1, 12,  // length = 12 (exact bytecode length)
                op::PUSH0,      // source offset = 0
                op::PUSH0,      // dest offset = 0
                op::CODECOPY,   // copy
            ],
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
    }

    dedup {
        // Transitive redirect chains in dedup caused MULTI_JUMP case PCs to resolve to
        // stale dead entry blocks, producing InvalidJump on valid bytecode.
        //
        // t0/t1/t2 are byte-identical trampolines (JUMPDEST JUMP) that get deduped into
        // a chain t2->t1->t0. `fn` dispatches to t0/t1/t2 via multi-jump. Without
        // transitive compression, t2's inst_entry points at the dead t1 block.
        //
        // Path taken at runtime: entry -> path1 -> path2 -> fn -> t2 -> u1 -> STOP
        transitive_redirect_multi_jump(@raw {
            bytecode: &asm("
                ; entry: dispatch on CALLDATASIZE (nonzero -> path1)
                CALLDATASIZE
                PUSH %path1
                JUMPI
                ; not taken: u0 via t0
                PUSH %u0
                PUSH %t0
                JUMP

            path1:
                JUMPDEST
                CALLVALUE
                PUSH %path2
                JUMPI
                ; not taken: u1 via t1 via fn
                PUSH %u1
                PUSH %t1
                PUSH %fn
                JUMP

            path2:
                JUMPDEST
                ; u1 via t2 via fn
                PUSH %u1
                PUSH %t2
                PUSH %fn
                JUMP

            dead0: INVALID
            dead1: INVALID
            dead2: INVALID

            ; three byte-identical trampolines that get deduped (t2->t1->t0)
            t0: JUMPDEST  JUMP
            t1: JUMPDEST  JUMP
            t2: JUMPDEST  JUMP

            u0: JUMPDEST  STOP
            u1: JUMPDEST  STOP

            fn: JUMPDEST  JUMP
            "),
            spec_id: SpecId::CANCUN,
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),

        // Dedup merges two byte-identical MULTI_JUMP dispatcher blocks after their
        // target blocks are deduped, but only the canonical dispatcher's case PCs
        // were emitted in the switch. DEF_CD has nonzero words, so the bytecode takes the
        // path through the second dispatcher (the one that gets deduped away).
        //
        // Each path pushes (stop_pc, dispatcher_pc) then JUMPs to the dispatcher.
        // fn1 and fn2 are byte-identical dispatchers (JUMPDEST JUMP) that get deduped.
        dedup_multi_jump_dispatcher(@raw {
            bytecode: &asm("
                ; dispatch on first calldata word
                PUSH0
                CALLDATALOAD
                PUSH %d1
                JUMPI

                ; dispatch on second calldata word
                PUSH 0x20
                CALLDATALOAD
                PUSH %d0
                JUMPI

                ; neither taken: stop0 via fn1
                PUSH %stop0
                PUSH %fn1
                JUMP

            d0:
                JUMPDEST
                PUSH %stop1
                PUSH %fn1
                JUMP

            d1:
                JUMPDEST
                PUSH 0x20
                CALLDATALOAD
                PUSH %d1_taken
                JUMPI
                ; not taken: stop2 via fn2
                PUSH %stop2
                PUSH %fn2
                JUMP

            d1_taken:
                JUMPDEST
                PUSH %stop3
                PUSH %fn2
                JUMP

            stop0: JUMPDEST  STOP
            stop1: JUMPDEST  STOP
            stop2: JUMPDEST  STOP
            stop3: JUMPDEST  STOP

            ; two byte-identical dispatchers that get deduped
            fn1: JUMPDEST  JUMP
            fn2: JUMPDEST  JUMP
            "),
            expected_return: InstrStop::Stop,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
    }

    regressions {
        // Dedup must not poison DSE via lost leader marks.
        //
        // When a JUMPI's fall-through block gets deduped, the leader mark on the first dead
        // instruction must propagate to the next alive instruction. Without this, the JUMPI
        // block absorbs the next alive instruction (e.g. INVALID) as its terminator, which
        // causes DSE to treat all exit stack positions as dead, incorrectly NOOP-ing live PUSHes.
        //
        // inspect_stack must be off so DSE's diverging-terminator optimisation is active.
        dedup_leader_propagation(@raw {
            bytecode: &asm("
            entry:
                JUMPDEST
                PUSH 0x00       ; cond = 0
                PUSH %jumpi2
                JUMPI           ; not taken
                ; canonical fall-through
                PUSH %jumpi2
                JUMP

            jumpi2:
                JUMPDEST
                PUSH 0x2a       ; live-out value DSE must preserve
                CALLDATASIZE    ; condition (nonzero)
                PUSH %dest
                JUMPI           ; taken
                ; duplicate fall-through (same bytes as canonical -> deduped)
                PUSH %jumpi2
                JUMP

                INVALID         ; alive after dead deduped block

            dest:
                JUMPDEST
                PUSH 0x00
                MSTORE          ; mem[0] = 0x2a
                PUSH 0x20
                PUSH 0x00
                RETURN
            "),
            inspect_stack: Some(false),
            expected_return: InstrStop::Return,
            expected_memory: B256::with_last_byte(0x2a).as_slice(),
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
            expected_output: Some(B256::with_last_byte(0x2a).as_slice()),
        }),

        // Same class of bug but triggered by a deduped reachable JUMPDEST block rather than
        // a JUMPI fall-through.
        //
        // Real-world trigger: tx 0x3a0ab5...31856 (block 5330710).
        dedup_jumpdest_leader(@raw {
            bytecode: &asm("
            entry:
                JUMPDEST
                PUSH 0x00
                CALLDATASIZE
                PUSH %alt_entry
                JUMPI
                ; not-taken -> dup_ret (makes it reachable for analysis)
                PUSH %dup_ret
                JUMP

            canonical_ret:
                JUMPDEST
                PUSH %dest
                JUMP

            alt_entry:
                JUMPDEST
                DUP1
                POP
                POP
                PUSH 0x2a       ; <- DSE must NOT kill

            dup_ret:
                ; byte-identical to canonical_ret -> deduped
                JUMPDEST
                PUSH %dest
                JUMP

                INVALID         ; alive after dead dup_ret

            dest:
                JUMPDEST
                PUSH 0x00
                MSTORE
                PUSH 0x20
                PUSH 0x00
                RETURN
            "),
            inspect_stack: Some(false),
            expected_return: InstrStop::Return,
            expected_memory: B256::with_last_byte(0x2a).as_slice(),
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
            expected_output: Some(B256::with_last_byte(0x2a).as_slice()),
        }),

        dedup_false_jumpi_non_jumpdest_fallthrough(@raw {
            bytecode: &asm("
                CALLDATASIZE
                PUSH %path2
                JUMPI

                PUSH0
                PUSH %done
                JUMPI
                PUSH0
                PUSH0
                REVERT

            path2:
                JUMPDEST
                PUSH0
                PUSH %done
                JUMPI
                PUSH0
                PUSH0
                REVERT

            done:
                JUMPDEST
                STOP
            "),
            expected_return: InstrStop::Revert,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),

        dedup_fallthrough_jump_tail_charges_jump_gas(@raw {
            bytecode: &asm("
                PUSH %done
                CALLER
                PUSH %path2
                JUMPI
                JUMP

            path2:
                JUMPDEST
                PUSH0
                PUSH %other
                JUMPI
                JUMP

            other:
                JUMPDEST
                STOP

            done:
                JUMPDEST
                STOP
            "),
            expected_return: InstrStop::Stop,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),

        dedup_fallthrough_redirect_materializes_stack(@raw {
            bytecode: &asm("
                CALLVALUE
                PUSH 123456789
                SUB
                PUSH %join_a
                JUMPI
                CALLVALUE
                PUSH 123456789
                SUB
                PUSH %join_b
                JUMPI

                CALLDATASIZE
                PUSH %case_b_entry
                JUMPI

                PUSH0
                PUSH0
                PUSH %case_a
                JUMP

            case_a:
                JUMPDEST
                PUSH 0xaa
                SWAP2
                POP
            join_a:
                JUMPDEST
                PUSH %done
                JUMP

            case_b_entry:
                JUMPDEST
                PUSH0
                PUSH0
                PUSH %case_b
                JUMP

            case_b:
                JUMPDEST
                PUSH 0xbb
                SWAP2
                POP
            join_b:
                JUMPDEST
                PUSH %done
                JUMP

            done:
                JUMPDEST
                POP
                ISZERO
                PUSH %bad
                JUMPI
                STOP

            bad:
                JUMPDEST
                PUSH0
                PUSH0
                REVERT
            "),
            inspect_stack: Some(false),
            expected_return: InstrStop::Stop,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),

        // Disabled opcodes must not poison stack sections.
        //
        // When a disabled opcode (e.g. TSTORE before Cancun) follows executable instructions
        // in the same section, its stack I/O requirements must not be folded into the
        // section-head underflow check.
        //
        // CALLDATASIZE(0->1) ; TSTORE(2->0, disabled before Cancun).
        // The interpreter executes CALLDATASIZE then halts at TSTORE with NotActivated.
        calldatasize_tstore_shanghai(@raw {
            bytecode: &[op::CALLDATASIZE, op::TSTORE],
            spec_id: SpecId::SHANGHAI,
            expected_return: InstrStop::NotActivated,
        }),

        // PUSH0(0->1) ; TLOAD(1->1, disabled before Cancun).
        push0_tload_shanghai(@raw {
            bytecode: &[op::PUSH0, op::TLOAD],
            spec_id: SpecId::SHANGHAI,
            expected_return: InstrStop::NotActivated,
        }),

        // Mismatched costs in < BERLIN.
        // GeneralStateTests/stSolidityTest/TestKeywords.json
        st_solidity_keywords(@raw {
            bytecode: &hex!("7c01000000000000000000000000000000000000000000000000000000006000350463380e439681146037578063c040622614604757005b603d6084565b8060005260206000f35b604d6057565b8060005260206000f35b6000605f6084565b600060006101000a81548160ff0219169083021790555060ff60016000540416905090565b6000808160011560cd575b600a82121560a157600190910190608f565b81600a1460ac5760c9565b50600a5b60008160ff16111560c85760019182900391900360b0565b5b60d5565b6000925060ed565b8160001460e05760e8565b6001925060ed565b600092505b50509056"),
            spec_id: SpecId::ISTANBUL,
            modify_message: Some(|message| {
                message.value = 1_U256;
                message.input = alloy_primitives::bytes!("c0406226").into();
            }),
            expected_return: RETURN_WHAT_INTERPRETER_SAYS,
            expected_stack: STACK_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_output: Some(&1_U256.to_be_bytes::<32>()),
            assert_host: Some(|host| {
                assert_eq!(host.storage.get(&0_U256), Some(&1_U256));
            }),
        }),

        // Block analysis keeps only the top 64 stack entries at joins. Popping past them must
        // not read as an underflow: `cut`'s JUMPI still reaches `body` with `done` below, so
        // `body`'s jump must not resolve to `other` alone.
        truncated_stack_underflow(@raw {
            bytecode: &asm(&[
                "
                PUSH0
                MLOAD
                PUSH %alt
                JUMPI
                PUSH %done
                PUSH1 0x01
                PUSH %body
                ",
                &"PUSH0\n".repeat(63),
                "
            cut:
                JUMPDEST
                ",
                &"POP\n".repeat(63),
                "
                JUMPI
                STOP
            alt:
                JUMPDEST
                PUSH %other
                PUSH %body
                JUMP
            body:
                JUMPDEST
                JUMP
            other:
                JUMPDEST
                PUSH1 0x42
                STOP
            done:
                JUMPDEST
                STOP
                ",
            ].concat()),
            expected_memory: &[0; 32],
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),

        // `tramp` is never reached: `exit` does not return.
        unreached_return_trampoline(@raw {
            bytecode: &asm("
                PUSH %tramp
                PUSH %ret
                PUSH %func
                JUMP
            ret:
                JUMPDEST
                PUSH %exit
                JUMP
            tramp:
                JUMPDEST
                JUMP
            func:
                JUMPDEST
                JUMP
            exit:
                JUMPDEST
                STOP
            "),
            expected_stack: &[11_U256],
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),

        // Capping the memory words tracked by block analysis made it non-monotone and left a
        // live `JUMPDEST` after a dead instruction inside one CFG block. Dedup then redirected
        // a reachable identical block into it, so compiled code ran into the dead instruction:
        // a wrong halt when optimized, a crash when not. The runtime default
        // `inspect_stack = false` is needed to reproduce it.
        mem_words_dedup_into_dead_gap(@raw {
            bytecode: &hex!("6020356100935760006100205260006100405260006100605260006100805260006100a05260006100c05260006100e05260006101005260006101205260006101405260006101605260006101805260006101a05260006101c05260006101e0526000610200525f3561008957610071565b60015f52610200516100875760405161008657505b5b005b6001602053610071565b60016040356100a157505b5b005b00"),
            spec_id: SpecId::AMSTERDAM,
            modify_message: Some(|message| {
                let mut input = [0; 96];
                input[63] = 1;
                message.input = Bytes::from(input).into();
            }),
            inspect_stack: Some(false),
            expected_return: InstrStop::Stop,
            expected_memory: MEMORY_WHAT_INTERPRETER_SAYS,
            expected_gas: GAS_WHAT_INTERPRETER_SAYS,
        }),
    }
}

fn bytecode_unop(op: u8, a: U256) -> [u8; 34] {
    let mut code = [0; 34];
    let mut i = 0;
    build_push32!(code[i], a);
    code[i] = op;
    code
}

fn bytecode_binop(op: u8, a: U256, b: U256) -> [u8; 67] {
    let mut code = [0; 67];
    let mut i = 0;
    build_push32!(code[i], b);
    build_push32!(code[i], a);
    code[i] = op;
    code
}

fn bytecode_ternop(op: u8, a: U256, b: U256, c: U256) -> [u8; 100] {
    let mut code = [0; 100];
    let mut i = 0;
    build_push32!(code[i], c);
    build_push32!(code[i], b);
    build_push32!(code[i], a);
    code[i] = op;
    code
}

/// Build opaque unop bytecode: MSTORE(a, 0), MLOAD(0), `<op>`.
fn bytecode_unop_opaque(opcode: u8, a: U256) -> Vec<u8> {
    let mut code = Vec::with_capacity(64);
    store_dynamic_operands(&mut code, &[(a, false, 0x00)]);
    push_const_or_load(&mut code, a, false, 0x00);
    code.push(opcode);
    code
}

fn push_const_or_load(code: &mut Vec<u8>, value: U256, is_const: bool, offset: u8) {
    if is_const {
        code.push(op::PUSH32);
        code.extend_from_slice(&value.to_be_bytes::<32>());
    } else {
        code.push(op::PUSH1);
        code.push(offset);
        code.push(op::MLOAD);
    }
}

fn store_dynamic_operands(code: &mut Vec<u8>, operands: &[(U256, bool, u8)]) {
    for &(value, is_const, offset) in operands {
        if !is_const {
            // Store through `offset + CALLVALUE - CALLVALUE`, which the compiler cannot fold,
            // so that loading the value back stays dynamic.
            code.push(op::PUSH32);
            code.extend_from_slice(&value.to_be_bytes::<32>());
            code.extend_from_slice(&[
                op::CALLVALUE,
                op::DUP1,
                op::PUSH1,
                offset,
                op::ADD,
                op::SUB,
                op::MSTORE,
            ]);
        }
    }
}

/// Jumps to `target` through memory, so the jump stays unresolved, with `JUMPDEST PC STOP`
/// at PCs 63, 96, 127 and 130.
fn bytecode_dynamic_jump(target: U256) -> Vec<u8> {
    let mut code = bytecode_unop_opaque(op::JUMP, target);
    for pc in [63, 96, 127, 130] {
        code.resize(pc, op::STOP);
        code.extend([op::JUMPDEST, op::PC, op::STOP]);
    }
    code
}

fn bytecode_binop_mixed(opcode: u8, a: U256, b: U256, a_const: bool, b_const: bool) -> Vec<u8> {
    let mut code = Vec::with_capacity(128);
    store_dynamic_operands(&mut code, &[(a, a_const, 0x00), (b, b_const, 0x20)]);
    push_const_or_load(&mut code, b, b_const, 0x20);
    push_const_or_load(&mut code, a, a_const, 0x00);
    code.push(opcode);
    code
}

fn bytecode_ternop_mixed(
    opcode: u8,
    a: U256,
    b: U256,
    c: U256,
    a_const: bool,
    b_const: bool,
    c_const: bool,
) -> Vec<u8> {
    let mut code = Vec::with_capacity(192);
    store_dynamic_operands(
        &mut code,
        &[(a, a_const, 0x00), (b, b_const, 0x20), (c, c_const, 0x40)],
    );
    push_const_or_load(&mut code, c, c_const, 0x40);
    push_const_or_load(&mut code, b, b_const, 0x20);
    push_const_or_load(&mut code, a, a_const, 0x00);
    code.push(opcode);
    code
}

fn asm(s: &str) -> Vec<u8> {
    crate::parse_asm(s).unwrap()
}

/// Three unknown values enter the section at `head`, followed by a chain of JUMPI
/// fall-throughs. `cond` decides whether the third JUMPI jumps to `end`; the third
/// fall-through then runs `deep_dup` with five values on the stack. The last JUMPI keeps
/// the sum below it only in SSA, as CHAINID writes its output to memory.
fn jumpi_fallthrough_chain(cond: &str, deep_dup: &str) -> String {
    format!(
        "
        CALLDATASIZE
        PUSH 1 CALLDATALOAD
        PUSH 2 CALLDATALOAD
        PUSH %head
        JUMP
    fail:
        JUMPDEST
        INVALID
    head:
        JUMPDEST
        CALLDATASIZE ISZERO PUSH %fail JUMPI
        DUP2 PUSH0 CALLDATALOAD ADD
        CALLDATASIZE ISZERO PUSH %fail JUMPI
        DUP4 DUP2 MUL
        {cond} PUSH %end JUMPI
        {deep_dup}
        CALLDATASIZE ISZERO PUSH %fail JUMPI
        ADD
        CHAINID PUSH %end JUMPI
        INVALID
    end:
        JUMPDEST
        STOP
        "
    )
}

/// 1020 values followed by a chain of JUMPI fall-throughs. `cond` decides whether the second
/// JUMPI jumps to `end`; the second fall-through then pushes `pushes` values onto 1021.
fn jumpi_fallthrough_overflow(cond: &str, pushes: usize) -> String {
    format!(
        "
        {}
        CALLDATASIZE ISZERO PUSH %end JUMPI
        PUSH0 {cond} PUSH %end JUMPI
        {}
        STOP
    end:
        JUMPDEST
        STOP
        ",
        "PUSH0 ".repeat(1020),
        "PUSH0 ".repeat(pushes),
    )
}

/// Jumps to entry `index` of a table of 7-byte entries, through `x & 3` where `x` is `index`
/// plus an opaque zero from fresh memory. Entry 1 is not a `JUMPDEST`; entry `i` otherwise returns
/// `0x20 + i`.
fn range_jump_table(index: u8) -> String {
    let entry = |i: u8| {
        let jumpdest = if i == 1 { "STOP" } else { "JUMPDEST" };
        format!("{jumpdest} PUSH1 {:#x} PUSH2 %done JUMP\n", 0x20 + i)
    };
    format!(
        "
        PUSH0 MLOAD PUSH1 {index:#x} ADD
        PUSH1 0x03 AND
        PUSH1 0x07 MUL PUSH %table ADD
        JUMP
    table:
        {}
    done:
        JUMPDEST
        PUSH0 MSTORE
        PUSH1 0x20 PUSH0 RETURN
        ",
        (0..4).map(entry).collect::<String>(),
    )
}

/// Pushes `done`, then for each stage pushes one of two targets depending on a calldata byte,
/// then jumps through the pushed targets. Stage `i` adds `i` to memory word 0 for `A` and
/// `16 * i` for `B`.
fn binary_choice_chain(stages: usize) -> String {
    let mut s = String::from("PUSH %done\n");
    for i in 1..=stages {
        s += &format!(
            "PUSH1 0x{i:02x} CALLDATALOAD PUSH1 0xf8 SHR PUSH %b{i} JUMPI\n\
             PUSH %A{i} PUSH %j{i} JUMP\n\
             b{i}:\n JUMPDEST PUSH %B{i}\n\
             j{i}:\n JUMPDEST\n"
        );
    }
    s += "JUMP\n";
    for i in 1..=stages {
        s += &format!(
            "A{i}:\n JUMPDEST PUSH0 MLOAD PUSH1 0x{i:02x} ADD PUSH0 MSTORE JUMP\n\
             B{i}:\n JUMPDEST PUSH0 MLOAD PUSH1 0x{:02x} ADD PUSH0 MSTORE JUMP\n",
            16 * i
        );
    }
    s += "done:\n JUMPDEST PUSH1 0x20 PUSH0 RETURN\n";
    s
}

/// A countdown loop from zero wraps through an `i256` trip count. Loop strength reduction rewrites
/// its exit value with `udiv i256`, after the codegen pipeline has already expanded wide divisions.
/// Without a second expansion right after LSR, x86-64 instruction selection aborts the process.
/// Uses the production compiler options.
#[cfg(feature = "llvm")]
#[test]
fn lsr_wrapping_countdown_compiles() {
    init_tracing();
    let code = alloy_primitives::hex!(
        "5f5f525f5b60019003806004575b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5f5f52602156"
    );
    let mut compiler = EvmCompiler::new_llvm(false).unwrap();
    compiler.set_opt_level(evm2_jit_backend::OptimizationLevel::default());
    compiler.set_single_error(true);
    compiler.set_inspect_stack(false);
    unsafe { compiler.jit("lsr_wrapping_countdown", &code[..], DEF_SPEC) }.unwrap();
}
