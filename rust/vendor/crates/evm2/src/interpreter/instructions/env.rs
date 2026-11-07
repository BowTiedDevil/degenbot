use crate::{
    EvmTypesHost,
    evm::AccountLoad,
    interpreter::{Gas, Host, InstrStop, Memory, Result, Word, private::GasInstructionCx},
    utils::{WordExt, read_word_padded},
    version::GasParams,
};
use evm2_macros::instruction;

fn load_account<T: EvmTypesHost>(
    cx: &mut GasInstructionCx<'_, '_, '_, T>,
    addr: Word,
    load_code: bool,
) -> Result<AccountLoad> {
    let cold_load_gas = cx.state.gas_params().cold_account_additional_cost();
    let skip_cold_load = cx.gas.remaining() < cold_load_gas;
    let account_address = addr.to_address();
    let account = cx
        .state
        .host()
        .load_account(&account_address, load_code, skip_cold_load)
        .map_err(|error| cx.state.fail(error))?;
    if account.is_cold {
        cx.gas.spend(cold_load_gas)?;
    }
    Ok(account)
}

#[inline]
fn copy_data(
    gas: &mut Gas,
    memory: &mut Memory,
    gas_params: &GasParams,
    memory_offset: &Word,
    data_offset: &Word,
    len: usize,
    data: &[u8],
) -> Result {
    if len != 0 {
        let memory_offset = memory_offset.to_usize()?;
        memory.resize_evm(gas, gas_params, memory_offset, len)?;
        memory.set_data(memory_offset, data_offset.to_usize_saturated(), len, data);
    }
    Ok(())
}

#[instruction]
pub fn address(cx: _) -> out {
    out.write(cx.state.message().destination.into_word().into());
}

#[instruction(dynamic_gas)]
pub fn balance(cx: _, [addr]: [Word]) -> Result<out> {
    *out = load_account(&mut cx, *addr, false)?.balance;
}

#[instruction]
pub fn origin(cx: _) -> out {
    out.write(cx.state.host().tx_env().origin.into_word().into());
}

#[instruction]
pub fn caller(cx: _) -> out {
    out.write(cx.state.message().caller.into_word().into());
}

#[instruction]
pub fn callvalue(cx: _) -> out {
    *out = cx.state.message().value;
}

#[instruction]
pub fn calldataload(cx: _, [offset]: [Word]) -> out {
    let offset = offset.to_usize_saturated();
    *out = read_word_padded(&cx.state.message().input, offset).into();
}

#[instruction]
pub fn calldatasize(cx: _) -> out {
    out.write_u64(cx.state.message().input.len() as u64);
}

#[instruction(dynamic_gas)]
pub fn calldatacopy(cx: _, [memory_offset, data_offset, len]: [Word]) -> Result {
    let len = len.to_usize()?;
    // SAFETY: Charging and copying data make no host calls.
    let gas_params = unsafe { cx.state.gas_params_detached() };
    cx.gas.spend(gas_params.copy_cost(len))?;
    let input = cx.state.message().input.as_ref();
    copy_data(cx.gas, &mut cx.state.0.memory, gas_params, memory_offset, data_offset, len, input)
}

#[instruction]
pub fn codesize(cx: _) -> out {
    out.write_u64(cx.state.0.bytecode.len() as u64);
}

#[instruction(dynamic_gas)]
pub fn codecopy(cx: _, [memory_offset, code_offset, len]: [Word]) -> Result {
    let len = len.to_usize()?;
    // SAFETY: Charging and copying data make no host calls.
    let gas_params = unsafe { cx.state.gas_params_detached() };
    cx.gas.spend(gas_params.copy_cost(len))?;
    let data = cx.state.0.bytecode.original_byte_slice();
    copy_data(cx.gas, &mut cx.state.0.memory, gas_params, memory_offset, code_offset, len, data)
}

#[instruction]
pub fn gasprice(cx: _) -> out {
    *out = cx.state.host().tx_env().gas_price;
}

#[instruction(dynamic_gas)]
pub fn extcodesize(cx: _, [addr]: [Word]) -> Result<out> {
    out.write_u64(load_account(&mut cx, *addr, true)?.code.len() as u64);
}

#[instruction(dynamic_gas)]
pub fn extcodehash(cx: _, [addr]: [Word]) -> Result<out> {
    let account = load_account(&mut cx, *addr, false)?;
    *out = if account.is_empty { Word::ZERO } else { account.code_hash.into() };
}

#[instruction(dynamic_gas)]
pub fn extcodecopy(cx: _, [addr, memory_offset, code_offset, len]: [Word]) -> Result {
    let len = len.to_usize()?;
    cx.gas.spend(cx.state.gas_params().extcodecopy_cost(len))?;
    let memory_offset = if len != 0 {
        let memory_offset = memory_offset.to_usize()?;
        cx.state.resize_memory(cx.gas, memory_offset, len)?;
        memory_offset
    } else {
        0
    };
    let code = load_account(&mut cx, *addr, true)?.code;
    cx.state.0.memory.set_data(
        memory_offset,
        code_offset.to_usize_saturated(),
        len,
        code.original_byte_slice(),
    );
    Ok(())
}

#[instruction]
pub fn returndatasize(cx: _) -> Result<out> {
    out.write_u64(cx.state.return_data().len() as u64);
}

#[instruction(dynamic_gas)]
pub fn returndatacopy(cx: _, [memory_offset, data_offset, len]: [Word]) -> Result {
    let len = len.to_usize()?;
    let data_offset = data_offset.to_usize_saturated();
    if data_offset.saturating_add(len) > cx.state.return_data().len() {
        return Err(InstrStop::OutOfOffset);
    }

    // SAFETY: Charging and copying data make no host calls.
    let gas_params = unsafe { cx.state.gas_params_detached() };
    cx.gas.spend(gas_params.copy_cost(len))?;
    let data = &cx.state.0.return_data;
    let data_offset = Word::from(data_offset);
    copy_data(cx.gas, &mut cx.state.0.memory, gas_params, memory_offset, &data_offset, len, data)
}

#[cfg(test)]
mod tests {
    use crate::{
        SpecId,
        env::TxEnvExt,
        interpreter::{InstrStop, MessageExt, Word, op},
        test_utils::{
            RunConfig, TestHost, assert_stack, neg, push, run, run_stack, stack_code, test_message,
        },
        utils::{address_to_word, b256_to_word},
    };
    use alloc::{vec, vec::Vec};
    use alloy_primitives::{Address, B256, Bytes};
    use core::assert_matches;

    #[test]
    fn address_opcode() {
        let address = Address::repeat_byte(0x11);
        let mut host = TestHost::default();
        let message = MessageExt { destination: address, ..test_message() };
        let interp = run(RunConfig::new([op::ADDRESS, op::STOP]).host(&mut host).message(message));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [address_to_word(&address)]);
    }

    #[test]
    fn balance_opcode() {
        assert_stack!(BALANCE(0xbeef), 0xbeef);
        assert_stack!(BALANCE(0), 0);
        assert_stack!(BALANCE(neg(1)), address_to_word(&Address::repeat_byte(0xff)));
    }

    #[test]
    fn balance_cold_account_cost() {
        let mut host = TestHost { is_cold: true, ..TestHost::default() };
        let interp = run(RunConfig::new([op::PUSH1, 0xbe, op::BALANCE, op::STOP])
            .host(&mut host)
            .spec(SpecId::BERLIN));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::from(0xbe)]);
        assert_eq!(interp.gas_remaining(), 7_397);
    }

    #[test]
    fn balance_cold_account_skip_oog() {
        let mut host = TestHost { is_cold: true, ..TestHost::default() };
        let interp = run(RunConfig::new([op::PUSH1, 0xbe, op::BALANCE, op::STOP])
            .host(&mut host)
            .spec(SpecId::BERLIN)
            .gas_limit(103));
        assert_matches!(interp.err, InstrStop::OutOfGas);
    }

    #[test]
    fn origin_opcode() {
        let origin = Address::repeat_byte(0x22);
        let mut host = TestHost::default();
        let tx_env = TxEnvExt { origin, ..TxEnvExt::default() };
        let interp = run(RunConfig::new([op::ORIGIN, op::STOP]).host(&mut host).tx_env(tx_env));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [address_to_word(&origin)]);
    }

    #[test]
    fn caller_opcode() {
        let caller = Address::repeat_byte(0x33);
        let mut host = TestHost::default();
        let message = MessageExt { caller, ..test_message() };
        let interp = run(RunConfig::new([op::CALLER, op::STOP]).host(&mut host).message(message));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [address_to_word(&caller)]);
    }

    #[test]
    fn callvalue_opcode() {
        let mut host = TestHost::default();
        let message = MessageExt { value: Word::from(0xbeef), ..test_message() };
        let interp =
            run(RunConfig::new([op::CALLVALUE, op::STOP]).host(&mut host).message(message));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::from(0xbeef)]);
    }

    #[test]
    fn calldataload_opcode() {
        let input = Bytes::from(Vec::from([1_u8, 2, 3]));
        let mut host = TestHost::default();
        let message = MessageExt { input: input.into(), ..test_message() };

        let interp = run(RunConfig::new([op::PUSH0, op::CALLDATALOAD, op::STOP])
            .host(&mut host)
            .message(message.clone()));
        let mut expected = [0_u8; 32];
        expected[..3].copy_from_slice(&[1, 2, 3]);
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::from_be_bytes(expected)]);

        let interp = run(RunConfig::new([op::PUSH1, 0x20, op::CALLDATALOAD, op::STOP])
            .host(&mut host)
            .message(message));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [0]);
    }

    #[test]
    fn calldatasize_opcode() {
        let input = Bytes::from(Vec::from([1_u8, 2, 3, 4]));
        let mut host = TestHost::default();
        let message = MessageExt { input: input.into(), ..test_message() };
        let interp =
            run(RunConfig::new([op::CALLDATASIZE, op::STOP]).host(&mut host).message(message));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::from(4)]);
    }

    #[test]
    fn calldatacopy_opcode() {
        let input = Bytes::from(Vec::from([0xaa_u8, 0xbb, 0xcc]));
        let mut host = TestHost::default();
        let message = MessageExt { input: input.into(), ..test_message() };
        let mut code = Vec::new();
        push(&mut code, 2);
        push(&mut code, 1);
        push(&mut code, 0);
        code.push(op::CALLDATACOPY);
        push(&mut code, 0);
        code.push(op::MLOAD);
        code.push(op::STOP);

        let interp = run(RunConfig::new(code).host(&mut host).message(message.clone()));
        let mut expected = [0_u8; 32];
        expected[..2].copy_from_slice(&[0xbb, 0xcc]);
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::from_be_bytes(expected)]);

        let interp =
            run(RunConfig::new(stack_code([Word::MAX, Word::MAX, Word::ZERO], op::CALLDATACOPY))
                .host(&mut host)
                .message(message));
        assert_matches!(interp.err, InstrStop::Stop);
    }

    #[test]
    fn codesize_opcode() {
        let interp = run(RunConfig::new([op::CODESIZE, op::STOP]));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::from(2)]);

        let interp = run(RunConfig::new([op::PUSH1, 0x00, op::CODESIZE, op::STOP]));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::ZERO, Word::from(4)]);
    }

    #[test]
    fn codecopy_opcode() {
        let mut code = Vec::new();
        push(&mut code, Word::from(2));
        push(&mut code, Word::from(5));
        push(&mut code, 0);
        code.push(op::CODECOPY);
        push(&mut code, 0);
        code.push(op::MLOAD);
        code.push(op::STOP);

        let interp = run(RunConfig::new(code));
        let mut expected = [0u8; 32];
        expected[..2].copy_from_slice(&[0, op::CODECOPY]);
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::from_be_bytes(expected)]);

        let mut code = Vec::new();
        push(&mut code, Word::ONE);
        push(&mut code, Word::from(usize::MAX));
        push(&mut code, 0);
        code.push(op::CODECOPY);
        push(&mut code, 0);
        code.push(op::MLOAD);
        code.push(op::STOP);
        let interp = run(RunConfig::new(code));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [0]);

        let interp = run_stack([Word::MAX, Word::MAX, Word::ZERO], op::CODECOPY);
        assert_matches!(interp.err, InstrStop::Stop);

        let interp = run_stack([Word::MAX, Word::ZERO, Word::ONE], op::CODECOPY);
        assert_matches!(interp.err, InstrStop::InvalidOperandOOG);
    }

    #[test]
    fn gasprice_opcode() {
        let mut host = TestHost::default();
        let tx_env = TxEnvExt { gas_price: Word::from(0x1234), ..TxEnvExt::default() };
        let interp = run(RunConfig::new([op::GASPRICE, op::STOP]).host(&mut host).tx_env(tx_env));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::from(0x1234)]);
    }

    #[test]
    fn extcodesize_opcode() {
        let mut host = TestHost { code: Bytes::from(vec![0; 0x42]), ..TestHost::default() };
        let interp =
            run(RunConfig::new([op::PUSH1, 0xbe, op::EXTCODESIZE, op::STOP]).host(&mut host));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::from(0x42)]);
    }

    #[test]
    fn extcodecopy_opcode() {
        let mut host =
            TestHost { code: Bytes::from_static(&[0xaa, 0xbb, 0xcc]), ..TestHost::default() };
        let mut code = Vec::new();
        push(&mut code, 2);
        push(&mut code, 1);
        push(&mut code, 0);
        push(&mut code, 0xbeef);
        code.push(op::EXTCODECOPY);
        push(&mut code, 0);
        code.push(op::MLOAD);
        code.push(op::STOP);

        let interp = run(RunConfig::new(code).host(&mut host));
        let mut expected = [0_u8; 32];
        expected[..2].copy_from_slice(&[0xbb, 0xcc]);
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::from_be_bytes(expected)]);

        let mut code = Vec::new();
        push(&mut code, 4);
        push(&mut code, 2);
        push(&mut code, 0);
        push(&mut code, 0xbeef);
        code.push(op::EXTCODECOPY);
        push(&mut code, 0);
        code.push(op::MLOAD);
        code.push(op::STOP);
        let interp = run(RunConfig::new(code).host(&mut host));
        let mut expected = [0_u8; 32];
        expected[..1].copy_from_slice(&[0xcc]);
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::from_be_bytes(expected)]);

        let interp = run(RunConfig::new(stack_code(
            [Word::from(0xbeef), Word::MAX, Word::MAX, Word::ZERO],
            op::EXTCODECOPY,
        ))
        .host(&mut host));
        assert_matches!(interp.err, InstrStop::Stop);

        let interp = run(RunConfig::new(stack_code(
            [Word::from(0xbeef), Word::MAX, Word::ZERO, Word::ONE],
            op::EXTCODECOPY,
        ))
        .host(&mut host));
        assert_matches!(interp.err, InstrStop::InvalidOperandOOG);
    }

    #[test]
    fn returndatasize_opcode() {
        let interp = run(RunConfig::new([op::RETURNDATASIZE, op::STOP])
            .spec(SpecId::BYZANTIUM)
            .return_data(Bytes::from_static(&[0xaa, 0xbb, 0xcc])));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::from(3)]);

        let interp = run(RunConfig::new([op::RETURNDATASIZE]).spec(SpecId::FRONTIER));
        assert_matches!(interp.err, InstrStop::NotActivated);
    }

    #[test]
    fn returndatacopy_opcode() {
        let mut code = Vec::new();
        push(&mut code, 2);
        push(&mut code, 1);
        push(&mut code, 0);
        code.push(op::RETURNDATACOPY);
        push(&mut code, 0);
        code.push(op::MLOAD);
        code.push(op::STOP);

        let interp = run(RunConfig::new(code).return_data(Bytes::from_static(&[0xaa, 0xbb, 0xcc])));
        let mut expected = [0_u8; 32];
        expected[..2].copy_from_slice(&[0xbb, 0xcc]);
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::from_be_bytes(expected)]);

        let interp = run(RunConfig::new(stack_code(
            [Word::ZERO, Word::from(3), Word::ZERO],
            op::RETURNDATACOPY,
        ))
        .spec(SpecId::BYZANTIUM)
        .return_data(Bytes::from_static(&[0xaa, 0xbb, 0xcc])));
        assert_matches!(interp.err, InstrStop::Stop);

        let interp = run(RunConfig::new(stack_code(
            [Word::ZERO, Word::from(4), Word::ZERO],
            op::RETURNDATACOPY,
        ))
        .spec(SpecId::BYZANTIUM)
        .return_data(Bytes::from_static(&[0xaa, 0xbb, 0xcc])));
        assert_matches!(interp.err, InstrStop::OutOfOffset);

        let interp =
            run(RunConfig::new(stack_code([Word::MAX, Word::ZERO, Word::ONE], op::RETURNDATACOPY))
                .spec(SpecId::BYZANTIUM)
                .return_data(Bytes::from_static(&[0xaa])));
        assert_matches!(interp.err, InstrStop::InvalidOperandOOG);

        let interp = run(RunConfig::new(stack_code(
            [Word::ZERO, Word::ZERO, Word::ZERO],
            op::RETURNDATACOPY,
        ))
        .spec(SpecId::FRONTIER));
        assert_matches!(interp.err, InstrStop::NotActivated);
    }

    #[test]
    fn extcodehash_opcode() {
        let hash = B256::with_last_byte(0x77);
        let mut host = TestHost { code_hash: hash, ..TestHost::default() };
        let interp = run(RunConfig::new([op::PUSH1, 0xbe, op::EXTCODEHASH, op::STOP])
            .host(&mut host)
            .spec(SpecId::PETERSBURG));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [b256_to_word(hash)]);

        let interp = run(RunConfig::new([op::PUSH1, 0xbe, op::EXTCODEHASH, op::STOP])
            .host(&mut host)
            .spec(SpecId::BYZANTIUM));
        assert_matches!(interp.err, InstrStop::NotActivated);
    }
}
