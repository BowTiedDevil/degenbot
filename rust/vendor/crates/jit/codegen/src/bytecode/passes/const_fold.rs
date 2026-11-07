//! Constant folding for EVM arithmetic during abstract interpretation.

use super::block_analysis::AbsValue;
use crate::{
    InstData,
    bytecode::{U256Imm, U256Interner},
};
use alloy_primitives::U256;
use evm2::interpreter::{
    i256::{i256_cmp, i256_div, i256_mod},
    op,
    opcode::OpCode,
};
use std::cmp::Ordering;

/// Returns the compiler gas cost of constant-folding the given opcode with the given inputs.
///
/// Uses the real EVM gas schedule: most arithmetic is cheap (3–5 gas), but `EXP` costs
/// `10 + 50 * byte_size(exponent)` which can be weaponised with large exponents.
///
/// Returns `None` for opcodes that `try_const_fold` does not handle.
pub(crate) fn const_fold_gas(
    opcode: u8,
    inputs: &[AbsValue],
    interner: &U256Interner,
) -> Option<u64> {
    let gas: u64 = match opcode {
        op::CODESIZE | op::PC => 2,
        op::ISZERO | op::NOT => 3,
        op::CLZ => 3,
        op::ADD | op::SUB => 3,
        op::MUL | op::DIV | op::SDIV | op::MOD | op::SMOD => 5,
        op::ADDMOD | op::MULMOD => 8,
        op::SIGNEXTEND => 5,
        op::LT | op::GT | op::SLT | op::SGT | op::EQ => 3,
        op::AND | op::OR | op::XOR => 3,
        op::BYTE | op::SHL | op::SHR | op::SAR => 3,
        op::EXP => {
            // EXP: 10 + 50 * byte_size(exponent).
            // The exponent is the second operand (TOS - 1 in EVM stack order, inputs[0] here).
            let exponent = match *inputs.first()? {
                AbsValue::Const(imm) => imm.get(interner),
                value => U256::from(value.bounds()?.last),
            };
            let exponent_bytes = (256 - exponent.leading_zeros()).div_ceil(8);
            10 + 50 * exponent_bytes as u64
        }
        _ => return None,
    };
    Some(gas)
}

/// Try to constant-fold an instruction.
///
/// `code_len` is the length of the bytecode (for `CODESIZE`).
pub(crate) fn try_const_fold(
    inst: &InstData,
    inputs: &[AbsValue],
    interner: &mut U256Interner,
    code_len: usize,
) -> Option<AbsValue> {
    let opcode = inst.opcode;
    let result = match opcode {
        // 0 -> 1
        op::CODESIZE => U256::from(code_len),
        op::PC => U256::from(inst.pc_imm()),

        // 1 -> 1
        op::ISZERO | op::NOT | op::CLZ => {
            let &[AbsValue::Const(ai)] = inputs else {
                return try_range_fold(opcode, inputs, interner);
            };
            let a = ai.get(interner);
            match opcode {
                op::ISZERO => U256::from(a.is_zero()),
                op::NOT => !a,
                op::CLZ => U256::from(a.leading_zeros()),
                _ => unreachable!(),
            }
        }

        // 2 -> 1
        op::ADD
        | op::MUL
        | op::SUB
        | op::DIV
        | op::SDIV
        | op::MOD
        | op::SMOD
        | op::EXP
        | op::SIGNEXTEND
        | op::LT
        | op::GT
        | op::SLT
        | op::SGT
        | op::EQ
        | op::AND
        | op::OR
        | op::XOR
        | op::BYTE
        | op::SHL
        | op::SHR
        | op::SAR => {
            let &[AbsValue::Const(bi), AbsValue::Const(ai)] = inputs else {
                return try_range_fold(opcode, inputs, interner);
            };
            let a = ai.get(interner);
            let b = bi.get(interner);
            match opcode {
                op::ADD => a.wrapping_add(b),
                op::MUL => a.wrapping_mul(b),
                op::SUB => a.wrapping_sub(b),
                op::DIV => {
                    if !b.is_zero() {
                        a.wrapping_div(b)
                    } else {
                        U256::ZERO
                    }
                }
                op::SDIV => i256_div(a, b),
                op::MOD => {
                    if !b.is_zero() {
                        a.wrapping_rem(b)
                    } else {
                        U256::ZERO
                    }
                }
                op::SMOD => i256_mod(a, b),
                op::EXP => a.pow(b),
                op::SIGNEXTEND => {
                    if a < U256::from(31) {
                        let ext = a.wrapping_to::<u64>();
                        let bit_index = (8 * ext + 7) as usize;
                        let bit = b.bit(bit_index);
                        let mask = (U256::ONE << bit_index) - U256::ONE;
                        if bit { b | !mask } else { b & mask }
                    } else {
                        b
                    }
                }
                op::LT => U256::from(a < b),
                op::GT => U256::from(a > b),
                op::SLT => U256::from(i256_cmp(&a, &b) == Ordering::Less),
                op::SGT => U256::from(i256_cmp(&a, &b) == Ordering::Greater),
                op::EQ => U256::from(a == b),
                op::AND => a & b,
                op::OR => a | b,
                op::XOR => a ^ b,
                op::BYTE => {
                    let i = a.saturating_to::<usize>();
                    if i < 32 { U256::from(b.byte(31 - i)) } else { U256::ZERO }
                }
                op::SHL => {
                    let shift = a.saturating_to::<usize>();
                    if shift < 256 { b << shift } else { U256::ZERO }
                }
                op::SHR => {
                    let shift = a.saturating_to::<usize>();
                    if shift < 256 { b >> shift } else { U256::ZERO }
                }
                op::SAR => {
                    let shift = a.saturating_to::<usize>();
                    if shift < 256 {
                        b.arithmetic_shr(shift)
                    } else if b.bit(255) {
                        U256::MAX
                    } else {
                        U256::ZERO
                    }
                }
                _ => unreachable!(),
            }
        }

        // 3 -> 1
        op::ADDMOD | op::MULMOD => {
            let &[AbsValue::Const(ci), AbsValue::Const(bi), AbsValue::Const(ai)] = inputs else {
                return try_range_fold(opcode, inputs, interner);
            };
            let a = ai.get(interner);
            let b = bi.get(interner);
            let n = ci.get(interner);
            match opcode {
                op::ADDMOD => a.add_mod(b, n),
                op::MULMOD => a.mul_mod(b, n),
                _ => unreachable!(),
            }
        }

        _ => return None,
    };
    debug_assert!(
        const_fold_gas(inst.opcode, inputs, interner).is_some(),
        "try_const_fold handled opcode {} but const_fold_gas did not",
        inst.opcode,
    );
    Some(AbsValue::Const(U256Imm::new(result, interner)))
}

/// Folds an instruction on the [bounds](AbsValue::bounds) of its inputs, of which at least one is
/// not a constant, into a range that holds every result.
///
/// Inputs that are not small constants or ranges may hold any value, including values that are
/// negative as signed integers. Results that may not fit in a range give `None`.
fn try_range_fold(
    opcode: u8,
    inputs: &[AbsValue],
    interner: &mut U256Interner,
) -> Option<AbsValue> {
    // The input at `depth`, where 0 is the top of the stack.
    let input = |depth: usize| inputs[inputs.len() - 1 - depth];
    let bounds =
        |depth: usize| input(depth).bounds().map(|r| (u64::from(r.start), u64::from(r.last)));
    let konst = |depth: usize| input(depth).as_const().map(|c| c.get(interner));
    // `v >> s` for values below 2^31.
    let shr = |v: u64, s: u64| if s < 64 { v >> s } else { 0 };
    // The smallest `2^n - 1` that is at least `v`.
    let mask = |v: u64| u64::MAX.checked_shr(v.leading_zeros()).unwrap_or(0);
    // A comparison that is `lt` where it holds for every input value and `ge` where it fails for
    // every one.
    let cmp = |lt: bool, ge: bool| {
        if lt {
            (1, 1)
        } else if ge {
            (0, 0)
        } else {
            (0, 1)
        }
    };

    let (lo, hi) = match opcode {
        // Bounds are below 2^31, so sums and products cannot overflow.
        op::ADD => {
            let ((a_lo, a_hi), (b_lo, b_hi)) = (bounds(0)?, bounds(1)?);
            (a_lo + b_lo, a_hi + b_hi)
        }
        op::MUL => {
            let ((a_lo, a_hi), (b_lo, b_hi)) = (bounds(0)?, bounds(1)?);
            (a_lo * b_lo, a_hi * b_hi)
        }
        // `a - b` wraps unless `a >= b` for every value.
        op::SUB => {
            let ((a_lo, a_hi), (b_lo, b_hi)) = (bounds(0)?, bounds(1)?);
            (a_lo.checked_sub(b_hi)?, a_hi - b_lo)
        }
        // `a / b` is at most `a`, and 0 when `b` is 0. A large constant divisor bounds any `a`.
        // Values below 2^31 are not negative, so the signed quotient of two of them is the same.
        op::DIV | op::SDIV => match (bounds(0), bounds(1)) {
            (Some((a_lo, a_hi)), Some((b_lo, b_hi))) if b_lo > 0 => (a_lo / b_hi, a_hi / b_lo),
            (Some((_, a_hi)), b) if opcode == op::DIV || b.is_some() => (0, a_hi),
            (None, _) if opcode == op::DIV => {
                let b = konst(1)?;
                (0, if b.is_zero() { 0 } else { (U256::MAX / b).try_into().ok()? })
            }
            _ => return None,
        },
        // `a % b` is at most `a` and below `b`, is `a` when `a < b`, and is 0 when `b` is 0.
        // The signed result takes the sign of `a`, so it needs a non-negative `a`.
        op::MOD | op::SMOD => {
            let a = bounds(0);
            let b = bounds(1);
            if opcode == op::SMOD && a.is_none() {
                return None;
            }
            match (a, b, konst(1)) {
                (Some(a), Some((b_lo, _)), _) if b_lo > a.1 => a,
                // A large constant divisor is above `a`, unless it is negative for `SMOD`.
                (Some(a), None, Some(_)) if opcode == op::MOD => a,
                _ => {
                    let a_hi = a.map(|(_, hi)| hi);
                    let b_hi = b.map(|(_, hi)| hi.saturating_sub(1));
                    (0, a_hi.into_iter().chain(b_hi).min()?)
                }
            }
        }
        // `(a + b) % n` and `(a * b) % n` are computed without overflow, so they are below `n`
        // and at most the plain result.
        op::ADDMOD | op::MULMOD => {
            let ab = (bounds(0), bounds(1));
            let ab = if let (Some((a_lo, a_hi)), Some((b_lo, b_hi))) = ab {
                Some(if opcode == op::ADDMOD {
                    (a_lo + b_lo, a_hi + b_hi)
                } else {
                    (a_lo * b_lo, a_hi * b_hi)
                })
            } else {
                None
            };
            match (ab, bounds(2)) {
                (Some(ab), Some((n_lo, _))) if n_lo > ab.1 => ab,
                (ab, n) => {
                    let n_hi = n.map(|(_, hi)| hi.saturating_sub(1));
                    (0, ab.map(|(_, hi)| hi).into_iter().chain(n_hi).min()?)
                }
            }
        }
        // `a ** b` grows with both inputs for `a >= 1`, `0 ** 0` is 1 and `0 ** b` is 0.
        op::EXP => {
            let ((a_lo, a_hi), (b_lo, b_hi)) = (bounds(0)?, bounds(1)?);
            let pow = |a: u64, b: u64| a.checked_pow(b.try_into().ok()?);
            let lo = if a_lo > 0 { pow(a_lo, b_lo)? } else { u64::from(b_hi == 0) };
            let hi = if a_hi > 0 { pow(a_hi, b_hi)? } else { u64::from(b_lo == 0) };
            (lo, hi)
        }
        // A value whose sign bit at byte `a` is clear is unchanged, as is every value for
        // `a >= 31`.
        op::SIGNEXTEND => {
            let b = bounds(1)?;
            let a_lo = bounds(0).map_or(0, |(lo, _)| lo);
            if a_lo < 31 && b.1 >> (8 * a_lo.min(7) + 7) != 0 {
                return None;
            }
            b
        }
        op::LT | op::GT | op::SLT | op::SGT | op::EQ => match (bounds(0), bounds(1)) {
            (Some((a_lo, a_hi)), Some((b_lo, b_hi))) => match opcode {
                op::LT | op::SLT => cmp(a_hi < b_lo, a_lo >= b_hi),
                op::GT | op::SGT => cmp(a_lo > b_hi, a_hi <= b_lo),
                _ => cmp(false, a_hi < b_lo || b_hi < a_lo),
            },
            _ => (0, 1),
        },
        op::ISZERO => match bounds(0) {
            Some((a_lo, a_hi)) => cmp(a_hi == 0, a_lo > 0),
            None => (0, 1),
        },
        // `a & b` is at most the smaller bound of either input.
        op::AND => (0, bounds(0).into_iter().chain(bounds(1)).map(|(_, hi)| hi).min()?),
        // `a | b` is at least either input, and neither sets bits above their highest bit.
        op::OR => {
            let ((a_lo, a_hi), (b_lo, b_hi)) = (bounds(0)?, bounds(1)?);
            (a_lo.max(b_lo), mask(a_hi | b_hi))
        }
        op::XOR => {
            let ((_, a_hi), (_, b_hi)) = (bounds(0)?, bounds(1)?);
            (0, mask(a_hi | b_hi))
        }
        // `!a` of a value below 2^31 is close to 2^256.
        op::NOT => return None,
        // A byte of a constant word at an index in a range, such as a jump table packed into a
        // `PUSH`, is between the smallest and largest bytes it can select, where indices past 31
        // select 0. Any other byte is at most the value it is taken from.
        op::BYTE => {
            let (i_lo, i_hi) = bounds(0).map_or((0, 32), |(lo, hi)| (lo.min(32), hi.min(32)));
            if let Some(w) = konst(1) {
                let w = w.to_be_bytes::<32>();
                let bytes = (i_lo..=i_hi).map(|i| w.get(i as usize).map_or(0, |&b| u64::from(b)));
                (bytes.clone().min().unwrap(), bytes.max().unwrap())
            } else {
                (0, bounds(1).map_or(255, |(_, hi)| hi.min(255)))
            }
        }
        // Shifts grow or shrink with the value and the shift. Shifts of 256 or more give 0.
        op::SHL => {
            let (s_lo, s_hi) = bounds(0)?;
            if s_lo >= 256 {
                (0, 0)
            } else if let Some((b_lo, b_hi)) = bounds(1)
                && s_hi < 32
            {
                (b_lo << s_lo, b_hi << s_hi)
            } else {
                return None;
            }
        }
        // A large shift bounds any value. Arithmetic shifts of non-negative values are logical.
        op::SHR | op::SAR => {
            let (s_lo, s_hi) = bounds(0).unwrap_or((0, 256));
            match (bounds(1), konst(1)) {
                (Some((b_lo, b_hi)), _) => (shr(b_lo, s_hi), shr(b_hi, s_lo)),
                (None, Some(b)) if opcode == op::SHR || !b.bit(255) => {
                    let shift = |s: u64| if s < 256 { b >> s as usize } else { U256::ZERO };
                    (shift(s_hi).try_into().ok()?, shift(s_lo).try_into().ok()?)
                }
                (None, None) if opcode == op::SHR && s_lo >= 225 => {
                    (0, if s_lo < 256 { u64::MAX >> (s_lo - 192) } else { 0 })
                }
                _ => return None,
            }
        }
        // The number of leading zero bits shrinks as the value grows.
        op::CLZ => match bounds(0) {
            Some((a_lo, a_hi)) => {
                let clz = |v: u64| 192 + u64::from(v.leading_zeros());
                (clz(a_hi), clz(a_lo))
            }
            None => (0, 256),
        },

        // Constants that `try_const_fold` folds exactly.
        op::CODESIZE | op::PC => return None,

        // Values without a known small bound. Sizes, gas and environment values depend on the
        // transaction and the gas limit, which the bytecode does not bound.
        op::STOP
        | op::KECCAK256
        | op::ADDRESS
        | op::BALANCE
        | op::ORIGIN
        | op::CALLER
        | op::CALLVALUE
        | op::CALLDATALOAD
        | op::CALLDATASIZE
        | op::CALLDATACOPY
        | op::CODECOPY
        | op::GASPRICE
        | op::EXTCODESIZE
        | op::EXTCODECOPY
        | op::RETURNDATASIZE
        | op::RETURNDATACOPY
        | op::EXTCODEHASH
        | op::BLOCKHASH
        | op::COINBASE
        | op::TIMESTAMP
        | op::NUMBER
        | op::DIFFICULTY
        | op::GASLIMIT
        | op::CHAINID
        | op::SELFBALANCE
        | op::BASEFEE
        | op::BLOBHASH
        | op::BLOBBASEFEE
        | op::SLOTNUM
        | op::POP
        | op::MLOAD
        | op::MSTORE
        | op::MSTORE8
        | op::SLOAD
        | op::SSTORE
        | op::JUMP
        | op::JUMPI
        | op::MSIZE
        | op::GAS
        | op::JUMPDEST
        | op::TLOAD
        | op::TSTORE
        | op::MCOPY
        | op::PUSH0..=op::PUSH32
        | op::DUP1..=op::DUP16
        | op::SWAP1..=op::SWAP16
        | op::LOG0..=op::LOG4
        | op::DUPN
        | op::SWAPN
        | op::EXCHANGE
        | op::CREATE
        | op::CALL
        | op::CALLCODE
        | op::RETURN
        | op::DELEGATECALL
        | op::CREATE2
        | op::STATICCALL
        | op::REVERT
        | op::INVALID
        | op::SELFDESTRUCT => return None,

        _ => {
            assert!(
                OpCode::new(opcode).is_none(),
                "opcode {opcode:#04x} needs a range transfer decision"
            );
            return None;
        }
    };
    AbsValue::range((lo..=hi).into(), interner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::{
        Bytecode,
        passes::block_analysis::tests::{Inst, analyze_asm},
    };
    use std::{fmt::Write, time::Instant};

    /// Builds bytecode that pushes operands, executes `opcode`, sinks the result
    /// with `PUSH0 MSTORE STOP`, analyzes it, and returns the folded result.
    ///
    /// Operands are in natural order matching the yellow paper specification:
    /// for `SUB(a, b)` = `a - b`, pass `&[a, b]` (first popped = first element).
    fn const_fold(opcode: u8, operands: &[U256]) -> Option<U256> {
        let mut src = String::new();
        let mut num_insts = 0usize;
        // Push in reverse so that operands[0] ends up on TOS.
        for v in operands.iter().rev() {
            writeln!(src, "PUSH {v}").unwrap();
            num_insts += 1;
        }
        writeln!(src, "{}", OpCode::new(opcode).unwrap()).unwrap();
        num_insts += 1;
        // Sink: PUSH0 + MSTORE + STOP to consume the result.
        let mstore_inst = num_insts + 1;
        writeln!(src, "PUSH0\nMSTORE\nSTOP").unwrap();

        let bytecode = analyze_asm(&src);
        // The folded result is operand 1 (second from top) at MSTORE.
        bytecode.const_operand(Inst::new(mstore_inst), 1)
    }

    #[test]
    fn const_fold_0_to_1() {
        // CODESIZE: code = CODESIZE(1) + PUSH0(1) + MSTORE(1) + STOP(1) = 4 bytes.
        assert_eq!(const_fold(op::CODESIZE, &[]), Some(U256::from(4)));

        // PC: at position 0.
        assert_eq!(const_fold(op::PC, &[]), Some(U256::ZERO));
    }

    #[test]
    fn const_fold_1_to_1() {
        // ISZERO.
        assert_eq!(const_fold(op::ISZERO, &[U256::ZERO]), Some(U256::ONE));
        assert_eq!(const_fold(op::ISZERO, &[U256::from(42)]), Some(U256::ZERO));

        // NOT.
        assert_eq!(const_fold(op::NOT, &[U256::ZERO]), Some(U256::MAX));
        assert_eq!(const_fold(op::NOT, &[U256::MAX]), Some(U256::ZERO));
    }

    #[test]
    fn const_fold_add() {
        assert_eq!(const_fold(op::ADD, &[U256::from(3), U256::from(4)]), Some(U256::from(7)));
        assert_eq!(const_fold(op::ADD, &[U256::MAX, U256::ONE]), Some(U256::ZERO));
    }

    #[test]
    fn const_fold_mul() {
        assert_eq!(const_fold(op::MUL, &[U256::from(3), U256::from(7)]), Some(U256::from(21)));
        assert_eq!(const_fold(op::MUL, &[U256::from(5), U256::ZERO]), Some(U256::ZERO));
    }

    #[test]
    fn const_fold_sub() {
        // SUB(10, 3) = 7.
        assert_eq!(const_fold(op::SUB, &[U256::from(10), U256::from(3)]), Some(U256::from(7)));
        assert_eq!(const_fold(op::SUB, &[U256::ZERO, U256::ONE]), Some(U256::MAX));
    }

    #[test]
    fn const_fold_div() {
        // DIV(10, 3) = 3.
        assert_eq!(const_fold(op::DIV, &[U256::from(10), U256::from(3)]), Some(U256::from(3)));
        assert_eq!(const_fold(op::DIV, &[U256::from(7), U256::ZERO]), Some(U256::ZERO));
    }

    #[test]
    fn const_fold_sdiv() {
        assert_eq!(const_fold(op::SDIV, &[U256::from(10), U256::from(3)]), Some(U256::from(3)));
        let neg10 = U256::ZERO.wrapping_sub(U256::from(10));
        let neg3 = U256::ZERO.wrapping_sub(U256::from(3));
        // SDIV(-10, 3) = -3.
        assert_eq!(const_fold(op::SDIV, &[neg10, U256::from(3)]), Some(neg3));
        assert_eq!(const_fold(op::SDIV, &[U256::from(7), U256::ZERO]), Some(U256::ZERO));
    }

    #[test]
    fn const_fold_mod() {
        // MOD(10, 3) = 1.
        assert_eq!(const_fold(op::MOD, &[U256::from(10), U256::from(3)]), Some(U256::ONE));
        assert_eq!(const_fold(op::MOD, &[U256::from(7), U256::ZERO]), Some(U256::ZERO));
    }

    #[test]
    fn const_fold_smod() {
        let neg8 = U256::ZERO.wrapping_sub(U256::from(8));
        let neg3 = U256::ZERO.wrapping_sub(U256::from(3));
        let neg2 = U256::ZERO.wrapping_sub(U256::from(2));
        // SMOD(-8, 3) = -2.
        assert_eq!(const_fold(op::SMOD, &[neg8, U256::from(3)]), Some(neg2));
        // SMOD(-8, -3) = -2.
        assert_eq!(const_fold(op::SMOD, &[neg8, neg3]), Some(neg2));
    }

    #[test]
    fn const_fold_addmod() {
        // ADDMOD(10, 10, 8) = 4.
        assert_eq!(
            const_fold(op::ADDMOD, &[U256::from(10), U256::from(10), U256::from(8)]),
            Some(U256::from(4)),
        );
        assert_eq!(
            const_fold(op::ADDMOD, &[U256::ONE, U256::from(2), U256::ZERO]),
            Some(U256::ZERO),
        );
    }

    #[test]
    fn const_fold_mulmod() {
        // MULMOD(10, 10, 8) = 4.
        assert_eq!(
            const_fold(op::MULMOD, &[U256::from(10), U256::from(10), U256::from(8)]),
            Some(U256::from(4)),
        );
    }

    #[test]
    fn const_fold_exp() {
        // EXP(2, 10) = 1024.
        assert_eq!(const_fold(op::EXP, &[U256::from(2), U256::from(10)]), Some(U256::from(1024)));
        // EXP(5, 0) = 1.
        assert_eq!(const_fold(op::EXP, &[U256::from(5), U256::ZERO]), Some(U256::ONE));
    }

    #[test]
    fn const_fold_signextend() {
        // SIGNEXTEND(0, 0xFF) -> all ones.
        assert_eq!(const_fold(op::SIGNEXTEND, &[U256::ZERO, U256::from(0xFF)]), Some(U256::MAX));
        // SIGNEXTEND(0, 0x7F) -> 0x7F.
        assert_eq!(
            const_fold(op::SIGNEXTEND, &[U256::ZERO, U256::from(0x7F)]),
            Some(U256::from(0x7F)),
        );
        // ext >= 31 -> no-op.
        assert_eq!(
            const_fold(op::SIGNEXTEND, &[U256::from(31), U256::from(0xFF)]),
            Some(U256::from(0xFF)),
        );
    }

    #[test]
    fn const_fold_lt_gt_eq() {
        // LT(1, 2) = 1.
        assert_eq!(const_fold(op::LT, &[U256::ONE, U256::from(2)]), Some(U256::ONE));
        assert_eq!(const_fold(op::LT, &[U256::from(2), U256::ONE]), Some(U256::ZERO));
        // GT(2, 1) = 1.
        assert_eq!(const_fold(op::GT, &[U256::from(2), U256::ONE]), Some(U256::ONE));
        assert_eq!(const_fold(op::GT, &[U256::ONE, U256::from(2)]), Some(U256::ZERO));
        assert_eq!(const_fold(op::EQ, &[U256::from(5), U256::from(5)]), Some(U256::ONE));
        assert_eq!(const_fold(op::EQ, &[U256::from(5), U256::from(6)]), Some(U256::ZERO));
    }

    #[test]
    fn const_fold_slt_sgt() {
        let neg1 = U256::MAX;
        // SLT(-1, 1) = 1.
        assert_eq!(const_fold(op::SLT, &[neg1, U256::ONE]), Some(U256::ONE));
        // SLT(1, -1) = 0.
        assert_eq!(const_fold(op::SLT, &[U256::ONE, neg1]), Some(U256::ZERO));
        // SGT(1, -1) = 1.
        assert_eq!(const_fold(op::SGT, &[U256::ONE, neg1]), Some(U256::ONE));
    }

    #[test]
    fn const_fold_and_or_xor() {
        assert_eq!(
            const_fold(op::AND, &[U256::from(0xFF), U256::from(0x0F)]),
            Some(U256::from(0x0F)),
        );
        assert_eq!(
            const_fold(op::OR, &[U256::from(0xF0), U256::from(0x0F)]),
            Some(U256::from(0xFF)),
        );
        assert_eq!(const_fold(op::XOR, &[U256::from(0xFF), U256::from(0xFF)]), Some(U256::ZERO));
    }

    #[test]
    fn const_fold_byte() {
        // BYTE(31, 0xFF) = 0xFF (last byte).
        assert_eq!(
            const_fold(op::BYTE, &[U256::from(31), U256::from(0xFF)]),
            Some(U256::from(0xFF)),
        );
        // BYTE(0, 0xFF) = 0 (0xFF is in byte 31 only).
        assert_eq!(const_fold(op::BYTE, &[U256::ZERO, U256::from(0xFF)]), Some(U256::ZERO));
        // Out of bounds.
        assert_eq!(const_fold(op::BYTE, &[U256::from(32), U256::from(0xFF)]), Some(U256::ZERO));
    }

    #[test]
    fn const_fold_shl_shr() {
        // SHL(1, 0x80) = 0x100.
        assert_eq!(const_fold(op::SHL, &[U256::ONE, U256::from(0x80)]), Some(U256::from(0x100)),);
        assert_eq!(const_fold(op::SHL, &[U256::from(256), U256::ONE]), Some(U256::ZERO));
        // SHR(1, 0x80) = 0x40.
        assert_eq!(const_fold(op::SHR, &[U256::ONE, U256::from(0x80)]), Some(U256::from(0x40)),);
        assert_eq!(const_fold(op::SHR, &[U256::from(256), U256::ONE]), Some(U256::ZERO));
    }

    #[test]
    fn const_fold_sar() {
        // SAR(1, 2) = 1.
        assert_eq!(const_fold(op::SAR, &[U256::ONE, U256::from(2)]), Some(U256::ONE));
        // SAR(4, -1) = -1.
        assert_eq!(const_fold(op::SAR, &[U256::from(4), U256::MAX]), Some(U256::MAX));
        // SAR(256, -1) = -1.
        assert_eq!(const_fold(op::SAR, &[U256::from(256), U256::MAX]), Some(U256::MAX));
        // SAR(256, 1) = 0.
        assert_eq!(const_fold(op::SAR, &[U256::from(256), U256::ONE]), Some(U256::ZERO));
    }

    /// Builds bytecode for N repetitions of `PUSH U256::MAX, PUSH U256::MAX, EXP, POP`.
    fn build_exp_bomb(n: usize) -> Vec<u8> {
        let mut src = String::new();
        for _ in 0..n {
            writeln!(src, "PUSH {}", U256::MAX).unwrap();
            writeln!(src, "PUSH {}", U256::MAX).unwrap();
            writeln!(src, "EXP").unwrap();
            writeln!(src, "POP").unwrap();
        }
        writeln!(src, "STOP").unwrap();
        crate::parse_asm(&src).unwrap()
    }

    /// Adversarial input: many EXP(U256::MAX, U256::MAX) operations.
    /// The gas limit must prevent the compiler from spending unbounded time on these.
    #[test]
    fn compiler_gas_limit_exp_bomb() {
        crate::tests::init_tracing();

        let code = build_exp_bomb(500);
        let start = Instant::now();
        let mut bytecode = Bytecode::test(code);
        bytecode.analyze().unwrap();
        let elapsed = start.elapsed();

        // With the default 100k gas budget, EXP(U256::MAX, U256::MAX) costs 10+50*32=1610 gas
        // each, so we can fold ~62 before hitting the limit. The remaining 438 stay dynamic,
        // but the point is the wall-clock bound.
        assert!(
            elapsed.as_secs() < 30,
            "compilation took too long ({elapsed:?}), gas limit may not be working",
        );
        assert!(bytecode.compiler_gas_used <= bytecode.compiler_gas_limit);
    }

    /// Proves that without a gas limit, high-volume folding is measurably slower.
    ///
    /// With the default 100k budget the same input finishes much faster because most folds
    /// are skipped after the budget runs out.
    #[test]
    fn compiler_gas_limit_no_limit_is_slower() {
        crate::tests::init_tracing();

        // 100k cheap ADD folds — enough to show a measurable difference.
        let mut src = String::new();
        for _ in 0..100_000 {
            writeln!(src, "PUSH 0x01").unwrap();
            writeln!(src, "PUSH 0x02").unwrap();
            writeln!(src, "ADD").unwrap();
            writeln!(src, "POP").unwrap();
        }
        writeln!(src, "STOP").unwrap();
        let code = crate::parse_asm(&src).unwrap();

        // With default (100k) gas limit.
        let start_limited = Instant::now();
        let mut limited = Bytecode::test(code.clone());
        limited.analyze().unwrap();
        let elapsed_limited = start_limited.elapsed();

        // With unlimited gas.
        let start_unlimited = Instant::now();
        let mut unlimited = Bytecode::test(code);
        unlimited.compiler_gas_limit = u64::MAX;
        unlimited.analyze().unwrap();
        let elapsed_unlimited = start_unlimited.elapsed();

        eprintln!("limited  (100k): {elapsed_limited:?} (gas used: {})", limited.compiler_gas_used);
        eprintln!(
            "unlimited (MAX): {elapsed_unlimited:?} (gas used: {})",
            unlimited.compiler_gas_used,
        );

        // The unlimited run folds everything so it must use more gas.
        assert!(unlimited.compiler_gas_used > limited.compiler_gas_used);
        // And the limited run should have hit the cap.
        assert!(limited.compiler_gas_used <= limited.compiler_gas_limit);
    }

    /// Adversarial input: thousands of cheap EXP to exhaust gas via volume.
    #[test]
    fn compiler_gas_limit_cheap_exp_volume() {
        crate::tests::init_tracing();

        // 20K repetitions of EXP(base, small_exponent) — cheap per-op but high volume.
        let mut src = String::new();
        for _ in 0..20_000 {
            writeln!(src, "PUSH {}", U256::MAX).unwrap();
            writeln!(src, "PUSH 0xff").unwrap(); // 1 byte exponent = 60 gas
            writeln!(src, "EXP").unwrap();
            writeln!(src, "POP").unwrap();
        }
        writeln!(src, "STOP").unwrap();

        let code = crate::parse_asm(&src).unwrap();
        let start = Instant::now();
        let mut bytecode = Bytecode::test(code);
        bytecode.analyze().unwrap();
        let elapsed = start.elapsed();

        assert!(elapsed.as_secs() < 30, "compilation took too long ({elapsed:?})",);
        assert!(bytecode.compiler_gas_used <= bytecode.compiler_gas_limit);
    }

    /// Verify that setting compiler_gas_limit to 0 disables constant folding entirely.
    #[test]
    fn compiler_gas_limit_zero_disables_folding() {
        crate::tests::init_tracing();

        let src = "PUSH 2\nPUSH 3\nADD\nPUSH0\nMSTORE\nSTOP\n";
        let code = crate::parse_asm(src).unwrap();
        let mut bytecode = Bytecode::test(code);
        bytecode.compiler_gas_limit = 0;
        bytecode.analyze().unwrap();

        // With gas limit 0, no folding should occur.
        assert_eq!(bytecode.compiler_gas_used, 0);
        // inst layout: PUSH(0), PUSH(1), ADD(2), PUSH0(3), MSTORE(4), STOP(5).
        // The ADD result should NOT be folded — operand 1 at MSTORE should be None.
        assert!(bytecode.const_operand(Inst::new(4), 1).is_none());
    }

    /// Verify that a very large gas limit allows all folding to proceed.
    #[test]
    fn compiler_gas_limit_unlimited() {
        crate::tests::init_tracing();

        let src = "PUSH 2\nPUSH 3\nADD\nPUSH0\nMSTORE\nSTOP\n";
        let code = crate::parse_asm(src).unwrap();
        let mut bytecode = Bytecode::test(code);
        bytecode.compiler_gas_limit = u64::MAX;
        bytecode.analyze().unwrap();

        assert!(bytecode.compiler_gas_used > 0);
        // inst layout: PUSH(0), PUSH(1), ADD(2), PUSH0(3), MSTORE(4), STOP(5).
        assert_eq!(bytecode.const_operand(Inst::new(4), 1), Some(U256::from(5)));
    }

    /// Exhaustively verify that `const_fold_gas` and `try_const_fold` agree on which opcodes
    /// they handle. For every opcode, if `try_const_fold` returns `Some` then `const_fold_gas`
    /// must also return `Some`, and vice versa.
    #[test]
    fn const_fold_gas_sync() {
        let mut interner: crate::bytecode::U256Interner = crate::bytecode::Interner::new();
        let one = U256Imm::new(U256::ONE, &mut interner);
        let two = U256Imm::new(U256::from(2), &mut interner);
        let three = U256Imm::new(U256::from(3), &mut interner);
        let c1 = AbsValue::Const(one);
        let c2 = AbsValue::Const(two);
        let c3 = AbsValue::Const(three);

        for opcode in 0..=u8::MAX {
            let inst = crate::InstData { opcode, ..crate::InstData::new(opcode) };
            let (inp, _) = inst.stack_io();

            let inputs: &[AbsValue] = match inp {
                0 => &[],
                1 => &[c1],
                2 => &[c1, c2],
                3 => &[c1, c2, c3],
                _ => continue,
            };

            let gas = const_fold_gas(opcode, inputs, &interner);
            let fold = try_const_fold(&inst, inputs, &mut interner, 100);

            assert_eq!(
                gas.is_some(),
                fold.is_some(),
                "const_fold_gas and try_const_fold disagree on opcode {opcode:#04x} ({}): \
                 gas={gas:?}, fold={fold:?}",
                OpCode::new(opcode).map_or("unknown", |o| o.as_str()),
            );
        }
    }

    /// Abstract inputs for the range fold tests: ranges, small and large constants, and `Top`.
    fn range_inputs(interner: &mut crate::bytecode::U256Interner) -> Vec<AbsValue> {
        let ranges = [
            (0, 3),
            (1, 4),
            (5, 9),
            (28, 34),
            (120, 130),
            (250, 258),
            ((1 << 31) - 4, (1 << 31) - 1),
        ];
        let consts = [U256::ZERO, U256::from(7), U256::from(225), U256::ONE << 224, U256::MAX];
        let mut inputs: Vec<_> =
            ranges.into_iter().map(|(lo, hi)| AbsValue::Range((lo..=hi).into())).collect();
        inputs.extend(consts.into_iter().map(|c| AbsValue::Const(U256Imm::new(c, interner))));
        inputs.push(AbsValue::Top);
        inputs
    }

    /// The concrete values that an abstract input may hold, with samples for `Top`.
    fn concrete_values(value: AbsValue, interner: &crate::bytecode::U256Interner) -> Vec<U256> {
        match value {
            AbsValue::Const(c) => vec![c.get(interner)],
            AbsValue::Range(r) => (r.start..=r.last).map(U256::from).collect(),
            AbsValue::Top => [0u64, 1, 2, 7, 31, 32, 255, 256, (1 << 31) - 1, 1 << 31]
                .into_iter()
                .map(U256::from)
                .chain([U256::ONE << 224, U256::ONE << 255, U256::MAX - U256::ONE])
                .chain([U256::MAX])
                .collect(),
            AbsValue::ConstSet(_) => unreachable!(),
        }
    }

    /// Checks every range fold against the exact constant fold of every concrete input it
    /// covers: each result must lie in the folded range.
    #[test]
    fn range_fold_soundness() {
        let mut interner: crate::bytecode::U256Interner = crate::bytecode::Interner::new();
        let candidates = range_inputs(&mut interner);
        for opcode in 0..=u8::MAX {
            let inst = crate::InstData::new(opcode);
            let (inp, out) = inst.stack_io();
            if out != 1
                || !(1..=3).contains(&inp)
                || const_fold_gas(opcode, &vec![AbsValue::Top; inp as usize], &interner).is_none()
                    && opcode != op::EXP
            {
                continue;
            }
            let n = candidates.len();
            for combo in 0..n.pow(u32::from(inp)) {
                let inputs: Vec<_> =
                    (0..inp as usize).map(|i| candidates[combo / n.pow(i as u32) % n]).collect();
                if inputs.iter().all(|v| matches!(v, AbsValue::Const(_))) {
                    continue;
                }
                let Some(folded) = try_range_fold(opcode, &inputs, &mut interner) else {
                    continue;
                };
                let range = folded.bounds().expect("range folds give ranges or small constants");
                let values: Vec<_> =
                    inputs.iter().map(|&v| concrete_values(v, &interner)).collect();
                let mut idx = vec![0; values.len()];
                loop {
                    let consts: Vec<_> = (0..values.len())
                        .map(|i| AbsValue::Const(U256Imm::new(values[i][idx[i]], &mut interner)))
                        .collect();
                    let exact = try_const_fold(&inst, &consts, &mut interner, 0)
                        .and_then(AbsValue::as_const)
                        .unwrap()
                        .get(&interner);
                    assert!(
                        exact >= U256::from(range.start) && exact <= U256::from(range.last),
                        "{}: {inputs:?} folded to {range:?}, but {:?} gives {exact}",
                        OpCode::new(opcode).unwrap(),
                        (0..values.len()).map(|i| values[i][idx[i]]).collect::<Vec<_>>(),
                    );
                    let Some(i) = (0..idx.len()).find(|&i| idx[i] + 1 < values[i].len()) else {
                        break;
                    };
                    idx[i] += 1;
                    idx[..i].fill(0);
                }
            }
        }
    }

    /// Range folds on representative inputs, including the edges where results wrap or leave
    /// the range domain.
    #[test]
    fn range_fold_precision() {
        let mut interner: crate::bytecode::U256Interner = crate::bytecode::Interner::new();
        let r = |lo: u32, hi: u32| AbsValue::Range((lo..=hi).into());
        let (r0_3, r1_4, r5_9, r0_39, r0_255) = (r(0, 3), r(1, 4), r(5, 9), r(0, 39), r(0, 255));
        let mut c = |v: u64| AbsValue::Const(U256Imm::new(U256::from(v), &mut interner));
        let (c0, c1, c2, c3, c5) = (c(0), c(1), c(2), c(3), c(5));
        let (c16, c40, c224, c255, c256) = (c(16), c(40), c(224), c(255), c(256));
        let (c0xffff, word) = (c(0xffff), c(0x1100_2233));
        let mut c = |v: U256| AbsValue::Const(U256Imm::new(v, &mut interner));
        let (c_big, c_max) = (c(U256::ONE << 226), c(U256::MAX));
        let top = AbsValue::Top;

        struct Case {
            opcode: u8,
            operands: Vec<AbsValue>,
            expected: Option<(u32, u32)>,
        }

        let case = |opcode, operands: &[AbsValue], expected| Case {
            opcode,
            operands: operands.to_vec(),
            expected,
        };
        let cases = [
            case(op::ADD, &[r0_3, r5_9], Some((5, 12))),
            case(op::ADD, &[r0_3, top], None),
            case(op::SUB, &[r5_9, r0_3], Some((2, 9))),
            // `1 - 4` wraps.
            case(op::SUB, &[r1_4, r1_4], None),
            case(op::SUB, &[r0_3, c1], None),
            case(op::MUL, &[r0_3, c40], Some((0, 120))),
            case(op::DIV, &[r5_9, r1_4], Some((1, 9))),
            case(op::DIV, &[r5_9, r0_3], Some((0, 9))),
            case(op::DIV, &[r5_9, top], Some((0, 9))),
            case(op::DIV, &[top, c_big], Some((0, (1 << 30) - 1))),
            case(op::DIV, &[top, c224], None),
            case(op::DIV, &[top, c0], Some((0, 0))),
            case(op::SDIV, &[r5_9, r1_4], Some((1, 9))),
            case(op::SDIV, &[r5_9, top], None),
            // Solar's selector dispatch indexes with `selector % 16`.
            case(op::MOD, &[top, c16], Some((0, 15))),
            case(op::MOD, &[r0_3, c5], Some((0, 3))),
            case(op::MOD, &[r5_9, r0_3], Some((0, 2))),
            case(op::MOD, &[r5_9, c_max], Some((5, 9))),
            case(op::MOD, &[top, c0], Some((0, 0))),
            case(op::MOD, &[top, top], None),
            case(op::SMOD, &[r5_9, c3], Some((0, 2))),
            case(op::SMOD, &[r5_9, c_max], Some((0, 9))),
            case(op::SMOD, &[top, c16], None),
            case(op::ADDMOD, &[r0_3, r0_3, c40], Some((0, 6))),
            case(op::ADDMOD, &[top, top, c40], Some((0, 39))),
            case(op::ADDMOD, &[top, top, c0], Some((0, 0))),
            case(op::MULMOD, &[r1_4, r1_4, top], Some((0, 16))),
            case(op::EXP, &[c2, r0_3], Some((1, 8))),
            case(op::EXP, &[r0_3, c2], Some((0, 9))),
            case(op::EXP, &[r0_3, r0_3], Some((0, 27))),
            case(op::EXP, &[c2, r0_39], None),
            case(op::SIGNEXTEND, &[c0, r0_3], Some((0, 3))),
            case(op::SIGNEXTEND, &[c0, r0_255], None),
            case(op::SIGNEXTEND, &[c1, r0_255], Some((0, 255))),
            case(op::SIGNEXTEND, &[top, r0_39], Some((0, 39))),
            case(op::LT, &[r0_3, r5_9], Some((1, 1))),
            case(op::LT, &[r5_9, r0_3], Some((0, 0))),
            case(op::LT, &[r0_3, r1_4], Some((0, 1))),
            case(op::GT, &[r5_9, r0_3], Some((1, 1))),
            case(op::SLT, &[top, c0], Some((0, 1))),
            case(op::SGT, &[r0_3, r5_9], Some((0, 0))),
            case(op::EQ, &[r0_3, r5_9], Some((0, 0))),
            case(op::EQ, &[top, top], Some((0, 1))),
            case(op::ISZERO, &[r1_4], Some((0, 0))),
            case(op::ISZERO, &[r0_3], Some((0, 1))),
            case(op::ISZERO, &[top], Some((0, 1))),
            case(op::AND, &[top, c0xffff], Some((0, 0xffff))),
            case(op::AND, &[r5_9, r0_3], Some((0, 3))),
            case(op::AND, &[top, c_max], None),
            case(op::OR, &[r5_9, r1_4], Some((5, 15))),
            case(op::OR, &[r0_3, top], None),
            case(op::XOR, &[r5_9, r0_3], Some((0, 15))),
            case(op::NOT, &[r0_3], None),
            case(op::BYTE, &[r(28, 31), word], Some((0x00, 0x33))),
            case(op::BYTE, &[r(30, 40), word], Some((0, 0x33))),
            case(op::BYTE, &[top, top], Some((0, 255))),
            case(op::BYTE, &[r(0, 32), r0_39], Some((0, 39))),
            case(op::SHL, &[r0_3, c1], Some((1, 8))),
            case(op::SHL, &[c256, top], Some((0, 0))),
            case(op::SHL, &[r(0, 40), c1], None),
            case(op::SHR, &[c224, top], None),
            case(op::SHR, &[r(225, 230), top], Some((0, (1 << 31) - 1))),
            case(op::SHR, &[c255, top], Some((0, 1))),
            case(op::SHR, &[r0_3, r(16, 31)], Some((2, 31))),
            case(op::SHR, &[r(0, 16), c_big], None),
            case(op::SHR, &[r(222, 230), c_big], Some((0, 16))),
            case(op::SAR, &[r0_3, r(16, 31)], Some((2, 31))),
            case(op::SAR, &[r(220, 230), c_max], None),
            case(op::CLZ, &[r1_4], Some((253, 255))),
            case(op::CLZ, &[r0_3], Some((254, 256))),
            case(op::CLZ, &[top], Some((0, 256))),
            case(op::CALLDATALOAD, &[r0_3], None),
        ];
        for Case { opcode, operands, expected } in cases {
            let inputs: Vec<_> = operands.iter().rev().copied().collect();
            let folded = try_range_fold(opcode, &inputs, &mut interner).map(|v| {
                let r = v.bounds().unwrap();
                (r.start, r.last)
            });
            assert_eq!(folded, expected, "{} {operands:?}", OpCode::new(opcode).unwrap());
        }
    }

    /// Every opcode has a range transfer decision.
    #[test]
    fn range_fold_exhaustive() {
        let mut interner: crate::bytecode::U256Interner = crate::bytecode::Interner::new();
        for opcode in 0..=u8::MAX {
            let (inp, _) = crate::InstData::new(opcode).stack_io();
            try_range_fold(opcode, &vec![AbsValue::Top; inp as usize], &mut interner);
        }
    }
}
