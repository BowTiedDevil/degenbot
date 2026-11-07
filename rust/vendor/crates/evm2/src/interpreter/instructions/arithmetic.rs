use super::i256::{i256_div, i256_mod};
use crate::{interpreter::Word, utils::WordExt};
use evm2_macros::instruction;

#[instruction]
pub fn add([a, b]: [Word]) -> out {
    out.write(a.wrapping_add(*b));
}

#[instruction]
pub fn mul([a, b]: [Word]) -> out {
    out.write(a.wrapping_mul(*b));
}

#[instruction]
pub fn sub([a, b]: [Word]) -> out {
    out.write(a.wrapping_sub(*b));
}

#[instruction]
pub fn div([a, b]: [Word]) -> out {
    out.write(if b.is_zero() { Word::ZERO } else { a.wrapping_div(*b) });
}

#[instruction]
pub fn sdiv([a, b]: [Word]) -> out {
    out.write(i256_div(*a, *b));
}

#[instruction]
pub fn rem([a, b]: [Word]) -> out {
    out.write(if b.is_zero() { Word::ZERO } else { a.wrapping_rem(*b) });
}

#[instruction]
pub fn smod([a, b]: [Word]) -> out {
    out.write(i256_mod(*a, *b));
}

#[instruction]
pub fn addmod([a, b, n]: [Word]) -> out {
    out.write(a.add_mod(*b, *n));
}

#[instruction]
pub fn mulmod([a, b, n]: [Word]) -> out {
    out.write(a.mul_mod(*b, *n));
}

#[instruction(dynamic_gas)]
pub fn exp(cx: _, [a, b]: [Word]) -> Result<out> {
    cx.gas.spend(cx.state.gas_params().exp_cost(*b))?;
    out.write(a.wrapping_pow(*b));
}

#[instruction]
pub fn signextend([ext, value]: [Word]) -> out {
    if *ext < 31 {
        let bit_index = (8 * ext.wrapping_to::<u64>() + 7) as usize;
        let mask = (Word::ONE << bit_index) - Word::ONE;
        out.write(if value.bit(bit_index) { *value | !mask } else { *value & mask });
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        SpecId,
        interpreter::{InstrStop, Word, op},
        test_utils::{RunConfig, assert_stack, neg, push, run},
    };
    use alloc::vec::Vec;
    use core::assert_matches;

    #[test]
    fn add_opcode() {
        assert_stack!(ADD(1, 2), 3);
        assert_stack!(ADD(neg(1), 1), 0);
        assert_stack!(ADD(0, 0), 0);
    }

    #[test]
    fn mul_opcode() {
        assert_stack!(MUL(3, 7), 21);
        assert_stack!(MUL(neg(1), 2), neg(1) - Word::ONE);
        assert_stack!(MUL(123, 0), 0);
    }

    #[test]
    fn sub_opcode() {
        assert_stack!(SUB(7, 3), 4);
        assert_stack!(SUB(0, 1), neg(1));
        assert_stack!(SUB(9, 9), 0);
    }

    #[test]
    fn div_opcode() {
        assert_stack!(DIV(7, 2), 3);
        assert_stack!(DIV(7, 0), 0);
        assert_stack!(DIV(0, 3), 0);
    }

    #[test]
    fn sdiv_opcode() {
        assert_stack!(SDIV(neg(4), 2), neg(2));
        assert_stack!(SDIV(7, neg(2)), neg(3));
        assert_stack!(SDIV(7, 0), 0);
        let min = !(Word::MAX >> 1usize);
        assert_stack!(SDIV(min, neg(1)), min);
        assert_stack!(SDIV(min, 1), min);
    }

    #[test]
    fn mod_opcode() {
        assert_stack!(MOD(7, 3), 1);
        assert_stack!(MOD(7, 0), 0);
        assert_stack!(MOD(9, 3), 0);
    }

    #[test]
    fn smod_opcode() {
        assert_stack!(SMOD(neg(4), 3), neg(1));
        assert_stack!(SMOD(4, neg(3)), 1);
        assert_stack!(SMOD(4, 0), 0);
        assert_stack!(SMOD(!(Word::MAX >> 1usize), neg(1)), 0);
    }

    #[test]
    fn addmod_opcode() {
        assert_stack!(ADDMOD(5, 6, 7), 4);
        assert_stack!(ADDMOD(neg(1), 1, 9), 7);
        assert_stack!(ADDMOD(1, 2, 0), 0);
    }

    #[test]
    fn mulmod_opcode() {
        assert_stack!(MULMOD(5, 6, 7), 2);
        assert_stack!(MULMOD(neg(1), 2, 9), 3);
        assert_stack!(MULMOD(2, 3, 0), 0);
    }

    #[test]
    fn exp_opcode() {
        assert_stack!(EXP(2, 10), 1024);
        assert_stack!(EXP(5, 0), 1);
        assert_stack!(EXP(0, 3), 0);
    }

    #[test]
    fn exp_charges_dynamic_gas() {
        let mut code = Vec::new();
        push(&mut code, 0xff);
        push(&mut code, 2);
        code.extend([op::EXP, op::STOP]);

        let interp = run(RunConfig::new(code).spec(SpecId::FRONTIER).gas_limit(25));

        assert_matches!(interp.err, InstrStop::OutOfGas);
    }

    #[test]
    fn exp_dynamic_gas_uses_active_spec() {
        let mut code = Vec::new();
        push(&mut code, 0xff);
        push(&mut code, 2);
        code.extend([op::EXP, op::STOP]);

        let frontier = run(RunConfig::new(code.clone()).spec(SpecId::FRONTIER).gas_limit(65));
        let spurious_dragon = run(RunConfig::new(code).spec(SpecId::SPURIOUS_DRAGON).gas_limit(65));

        assert_eq!(frontier.err, InstrStop::Stop);
        assert_matches!(spurious_dragon.err, InstrStop::OutOfGas);
    }

    #[test]
    fn signextend_opcode() {
        assert_stack!(SIGNEXTEND(0, 0x80), neg(1) - Word::from(0x7f));
        assert_stack!(SIGNEXTEND(0, 0x7f), 0x7f);
        assert_stack!(SIGNEXTEND(31, 0x80), 0x80);
    }
}
