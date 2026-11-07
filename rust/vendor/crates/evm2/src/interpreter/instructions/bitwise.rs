use super::i256::i256_lt;
use crate::{interpreter::Word, utils::WordExt};
use evm2_macros::instruction;

#[instruction]
pub fn lt([a, b]: [Word]) -> out {
    out.write_u64((a < b) as u64);
}

#[instruction]
pub fn gt([a, b]: [Word]) -> out {
    out.write_u64((a > b) as u64);
}

#[instruction]
pub fn slt([a, b]: [Word]) -> out {
    out.write_u64(i256_lt(a, b) as u64);
}

#[instruction]
pub fn sgt([a, b]: [Word]) -> out {
    out.write_u64(i256_lt(b, a) as u64);
}

#[instruction]
pub fn eq([a, b]: [Word]) -> out {
    out.write_u64((a == b) as u64);
}

#[instruction]
pub fn iszero([value]: [Word]) -> out {
    out.write_u64(value.is_zero() as u64);
}

#[instruction]
pub fn bitand([a, b]: [Word]) -> out {
    *out = *a & *b;
}

#[instruction]
pub fn bitor([a, b]: [Word]) -> out {
    *out = *a | *b;
}

#[instruction]
pub fn bitxor([a, b]: [Word]) -> out {
    *out = *a ^ *b;
}

#[instruction]
pub fn not([value]: [Word]) -> out {
    *out = !*value;
}

#[instruction]
pub fn byte([index, value]: [Word]) -> out {
    let index = index.to_usize_saturated();
    out.write_u64(if index < 32 { value.byte(31 - index).into() } else { 0 });
}

#[instruction]
pub fn shl([shift, value]: [Word]) -> out {
    let shift = shift.to_usize_saturated();
    out.write(if shift < 256 { *value << shift } else { Word::ZERO });
}

#[instruction]
pub fn shr([shift, value]: [Word]) -> out {
    let shift = shift.to_usize_saturated();
    out.write(if shift < 256 { *value >> shift } else { Word::ZERO });
}

#[instruction]
pub fn sar([shift, value]: [Word]) -> out {
    let shift = shift.to_usize_saturated();
    let value = if shift < 256 {
        value.arithmetic_shr(shift)
    } else if value.bit(255) {
        Word::MAX
    } else {
        Word::ZERO
    };
    out.write(value);
}

#[instruction]
pub fn clz([value]: [Word]) -> out {
    out.write_u64(value.leading_zeros() as u64);
}

#[cfg(test)]
mod tests {
    use crate::{
        interpreter::Word,
        test_utils::{assert_stack, neg},
    };

    #[test]
    fn lt_opcode() {
        assert_stack!(LT(1, 2), 1);
        assert_stack!(LT(2, 1), 0);
        assert_stack!(LT(2, 2), 0);
    }

    #[test]
    fn gt_opcode() {
        assert_stack!(GT(2, 1), 1);
        assert_stack!(GT(1, 2), 0);
        assert_stack!(GT(2, 2), 0);
    }

    #[test]
    fn slt_opcode() {
        assert_stack!(SLT(neg(1), 0), 1);
        assert_stack!(SLT(0, neg(1)), 0);
        assert_stack!(SLT(neg(2), neg(1)), 1);
        assert_stack!(SLT(neg(1), neg(1)), 0);
        assert_stack!(SLT(!(Word::MAX >> 1usize), Word::MAX >> 1usize), 1);
    }

    #[test]
    fn sgt_opcode() {
        assert_stack!(SGT(0, neg(1)), 1);
        assert_stack!(SGT(neg(1), 0), 0);
        assert_stack!(SGT(neg(1), neg(2)), 1);
        assert_stack!(SGT(neg(1), neg(1)), 0);
        assert_stack!(SGT(Word::MAX >> 1usize, !(Word::MAX >> 1usize)), 1);
    }

    #[test]
    fn eq_opcode() {
        assert_stack!(EQ(3, 3), 1);
        assert_stack!(EQ(3, 4), 0);
        assert_stack!(EQ(neg(1), neg(1)), 1);
    }

    #[test]
    fn iszero_opcode() {
        assert_stack!(ISZERO(0), 1);
        assert_stack!(ISZERO(1), 0);
        assert_stack!(ISZERO(neg(1)), 0);
    }

    #[test]
    fn and_opcode() {
        assert_stack!(AND(0b1100, 0b1010), 0b1000);
        assert_stack!(AND(neg(1), 0x55), 0x55);
        assert_stack!(AND(0, neg(1)), 0);
    }

    #[test]
    fn or_opcode() {
        assert_stack!(OR(0b1100, 0b1010), 0b1110);
        assert_stack!(OR(0, 0x55), 0x55);
        assert_stack!(OR(neg(1), 0), neg(1));
    }

    #[test]
    fn xor_opcode() {
        assert_stack!(XOR(0b1100, 0b1010), 0b0110);
        assert_stack!(XOR(neg(1), neg(1)), 0);
        assert_stack!(XOR(0, 0x55), 0x55);
    }

    #[test]
    fn not_opcode() {
        assert_stack!(NOT(0), neg(1));
        assert_stack!(NOT(neg(1)), 0);
        assert_stack!(NOT(0xff), neg(1) - Word::from(0xff));
    }

    #[test]
    fn byte_opcode() {
        assert_stack!(BYTE(31, 0x1234), 0x34);
        assert_stack!(BYTE(30, 0x1234), 0x12);
        assert_stack!(BYTE(32, 0x1234), 0);
    }

    #[test]
    fn shl_opcode() {
        assert_stack!(SHL(8, 1), 256);
        assert_stack!(SHL(0, 7), 7);
        assert_stack!(SHL(256, 1), 0);
    }

    #[test]
    fn shr_opcode() {
        assert_stack!(SHR(8, 256), 1);
        assert_stack!(SHR(0, 7), 7);
        assert_stack!(SHR(256, neg(1)), 0);
    }

    #[test]
    fn sar_opcode() {
        assert_stack!(SAR(1, neg(1) - Word::ONE), neg(1));
        assert_stack!(SAR(1, 4), 2);
        assert_stack!(SAR(256, neg(1)), neg(1));
    }

    #[test]
    fn clz_opcode() {
        assert_stack!(CLZ(1), 255);
        assert_stack!(CLZ(0), 256);
        assert_stack!(CLZ(neg(1)), 0);
    }
}
