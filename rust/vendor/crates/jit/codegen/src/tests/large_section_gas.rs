//! Sections whose static gas is close to or past what compiled code can charge ahead.

use super::{DEF_SPEC, with_evm_context};
use crate::{Backend, EvmCompiler, parse_asm};
use alloy_primitives::U256;
use evm2::{
    Version,
    interpreter::{Gas, InstrStop},
    version::GasId,
};

/// A 32-byte exponent.
const MAX: &str = "0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

/// The largest `ExpByte` for which a section of `PUSH32` (3), `CALLVALUE` (2) and `EXP` (10 plus
/// 32 bytes of exponent) still costs less than `u32::MAX`.
const FITTING_EXP_BYTE: u32 = (1 << 27) - 1;
/// The gas of that section.
const FITTING_GAS: u64 = 3 + 2 + 10 + FITTING_EXP_BYTE as u64 * 32;

/// Compiles `src` with the given `ExpByte`, returning the compile error if any.
fn compile<B: Backend>(
    compiler: &mut EvmCompiler<B>,
    code: &[u8],
    exp_byte: u32,
) -> Result<evm2_jit_context::EvmCompilerFn, String> {
    unsafe { compiler.clear() }.unwrap();
    let mut gas_params = Version::base(DEF_SPEC).gas_params;
    gas_params.set(GasId::ExpByte, exp_byte);
    compiler.set_gas_params(gas_params);
    compiler.set_inspect_stack(true);
    unsafe { compiler.jit("large_section_gas", code, DEF_SPEC) }.map_err(|e| e.to_string())
}

/// Runs `src` with a gas limit of `limit`, returning the stop reason, the final stack and the gas
/// spent.
fn run<B: Backend>(
    compiler: &mut EvmCompiler<B>,
    src: &str,
    limit: u64,
) -> (InstrStop, Vec<U256>, u64) {
    let code = parse_asm(src).unwrap();
    let f = compile(compiler, &code, FITTING_EXP_BYTE).unwrap();
    with_evm_context(&code, DEF_SPEC, |ecx, stack, stack_len| {
        ecx.gas = Gas::new(limit);
        let r = unsafe { f.call(ecx, stack, stack_len) };
        let stack =
            unsafe { stack.as_slice(*stack_len) }.iter().map(|word| word.to_u256()).collect();
        (r, stack, ecx.gas.spent())
    })
}

// A section costing more than `u32::MAX` is not compiled, instead of being charged too little.
matrix_tests!(
    large_section_gas_rejected = |compiler| {
        let code = parse_asm(&format!("PUSH32 {MAX} CALLVALUE EXP STOP")).unwrap();
        let err = compile(compiler, &code, u32::MAX).err().unwrap();
        assert_eq!(
            err,
            "the gas section at instruction 0 costs at least 4294967295 gas, which compiled code \
             cannot charge ahead of its instructions",
        );
    }
);

// A section just under the bound compiles and charges its exact cost, with a constant exponent
// above 2.
matrix_tests!(
    large_section_gas_const_exponent = |compiler| {
        let src = format!("PUSH32 {MAX} CALLVALUE EXP STOP");
        let (r, _, spent) = run(compiler, &src, 2 * FITTING_GAS);
        assert_eq!((r, spent), (InstrStop::Stop, FITTING_GAS));
        // The exact cost is enough, and one less is not.
        let (r, _, spent) = run(compiler, &src, FITTING_GAS);
        assert_eq!((r, spent), (InstrStop::Stop, FITTING_GAS));
        let (r, _, _) = run(compiler, &src, FITTING_GAS - 1);
        assert_eq!(r, InstrStop::OutOfGas);
    }
);

// The section head charges the whole cost, so the `GAS` after it owes the rest back to the
// remaining gas it reads.
matrix_tests!(
    large_section_gas_owed = |compiler| {
        let limit = 2 * FITTING_GAS;
        let (r, stack, spent) =
            run(compiler, &format!("GAS PUSH32 {MAX} CALLVALUE EXP GAS STOP"), limit);
        assert_eq!((r, spent), (InstrStop::Stop, FITTING_GAS + 4));
        assert_eq!(stack[0], U256::from(limit - 2));
        assert_eq!(stack[2], U256::from(limit - FITTING_GAS - 4));
    }
);
