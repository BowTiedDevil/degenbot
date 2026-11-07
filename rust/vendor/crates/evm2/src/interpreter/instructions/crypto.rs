use crate::utils::WordExt;
use alloy_primitives::{KECCAK256_EMPTY, keccak256 as keccak256_hash};
use evm2_macros::instruction;

#[instruction(dynamic_gas)]
pub fn keccak256(cx: _, [offset, len]: [Word]) -> Result<out> {
    let len = len.to_usize()?;
    cx.gas.spend(cx.state.gas_params().keccak256_word_cost(len))?;
    let hash = if len == 0 {
        KECCAK256_EMPTY
    } else {
        let offset = offset.to_usize()?;
        cx.state.resize_memory(cx.gas, offset, len)?;
        keccak256_hash(cx.state.memory().slice(offset, len))
    };
    *out = hash.into();
}

#[cfg(test)]
mod tests {
    use crate::{
        interpreter::{InstrStop, Word, op},
        test_utils::{RunConfig, push, run, run_stack},
        utils::b256_to_word,
    };
    use alloc::vec::Vec;
    use alloy_primitives::keccak256;
    use core::assert_matches;

    #[test]
    fn keccak256_opcode() {
        let mut code = Vec::new();
        push(&mut code, 0);
        push(&mut code, 0);
        code.push(op::KECCAK256);
        code.push(op::STOP);
        let interp = run(RunConfig::new(code));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [b256_to_word(keccak256([]))]);

        let mut code = Vec::new();
        push(&mut code, Word::from(0x80));
        push(&mut code, 0);
        code.push(op::MSTORE8);
        push(&mut code, Word::ONE);
        push(&mut code, 0);
        code.push(op::KECCAK256);
        code.push(op::STOP);
        let interp = run(RunConfig::new(code));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [b256_to_word(keccak256([0x80]))]);

        let interp = run_stack([Word::MAX, Word::ZERO], op::KECCAK256);
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [b256_to_word(keccak256([]))]);

        let interp = run_stack([Word::MAX, Word::ONE], op::KECCAK256);
        assert_matches!(interp.err, InstrStop::InvalidOperandOOG);
    }
}
