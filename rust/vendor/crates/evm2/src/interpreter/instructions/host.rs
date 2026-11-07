use super::require_non_staticcall;
use crate::{
    EvmFeatures, EvmTypesHost,
    evm::SStore,
    interpreter::{
        Gas, Host, InstrStop, InterpreterState, Result, StackMut, Word, private::GasInstructionCx,
    },
    utils::WordExt,
    version::GasId,
};
use alloy_primitives::{Address, B256};
use evm2_macros::instruction;

#[instruction(dynamic_gas)]
pub fn sload(cx: _, [key]: [Word]) -> Result<out> {
    // EIP-2929: SLOAD pays the warm read cost as static opcode gas, then only
    // charges the additional cold cost when the slot was not already warm. Avoid
    // touching the host/database if the frame cannot afford that cold surcharge.
    let additional_cold_cost = cx.state.gas_params().get(GasId::ColdStorageAdditionalCost);
    let skip_cold_load =
        cx.state.feature(EvmFeatures::EIP2929) && cx.gas.remaining() < additional_cold_cost;
    let destination = &cx.state.message().destination;
    let load = cx
        .state
        .host()
        .sload(destination, key, skip_cold_load)
        .map_err(|error| cx.state.fail(error))?;
    if load.is_cold {
        cx.gas.spend(additional_cold_cost)?;
    }
    out.write(load.value);
}

#[instruction(dynamic_gas)]
pub fn sstore(cx: _, [key, value]: [Word]) -> Result {
    sstore_with(cx.gas, cx.state, key, value, |_, _, _, _| Ok(()))
}

/// Executes `SSTORE` with a hook between the storage write and its dynamic gas.
///
/// This is the body of [`sstore`], for custom `SSTORE` instructions that need extra behavior.
/// Use it from an `#[instruction(dynamic_gas)]` function with `cx.gas` and `cx.state`.
///
/// `after_write` runs right after the host write and before any dynamic gas, state gas, or refund
/// is charged. It receives the interpreter [`Gas`], the state, the destination address, and the
/// [`SStore`] result of the write. It may charge gas through `Gas` or fail through
/// [`InterpreterState::fail`] and return the error. An `Err` stops the instruction and skips the
/// remaining charges. The JIT does not run the hook.
#[inline(always)]
pub fn sstore_with<T: EvmTypesHost>(
    gas: &mut Gas,
    state: &mut InterpreterState<'_, '_, T>,
    key: &Word,
    value: &Word,
    after_write: impl FnOnce(&mut Gas, &mut InterpreterState<'_, '_, T>, &Address, &SStore) -> Result,
) -> Result {
    require_non_staticcall(state)?;
    let is_eip2200 = state.feature(EvmFeatures::EIP2200);

    // EIP-2200: SSTORE may not execute with only the value-transfer stipend left. This
    // check happens before any gas is charged or host storage is touched.
    if is_eip2200 && gas.remaining() <= state.gas_params().get(GasId::CallStipend) {
        return Err(InstrStop::ReentrancySentryOOG);
    }

    // Frontier through Petersburg charge a fixed SSTORE_RESET static cost. Istanbul
    // (EIP-2200) turns this into the SLOAD-equivalent cost, and Berlin (EIP-2929)
    // makes that the warm storage read cost.
    gas.spend(state.gas_params().get(GasId::SstoreStatic))?;

    // EIP-2929: avoid performing a cold storage load if the frame cannot afford the
    // additional cold-load charge. The host performs the write and returns
    // original/present/new values for net metering.
    let skip_cold_load = state.feature(EvmFeatures::EIP2929)
        && gas.remaining() < state.gas_params().get(GasId::ColdStorageAdditionalCost);
    let destination = &state.message().destination;
    let state_load = state
        .host()
        .sstore(destination, key, value, skip_cold_load)
        .map_err(|error| state.fail(error))?;
    after_write(gas, state, destination, &state_load)?;

    // EIP-2200 net gas metering depends on original, present, and new slot values:
    // clean slots pay set/reset costs, dirty slots generally only pay the load cost,
    // and reset-to-original transitions are handled through refunds.
    gas.spend(state.gas_params().sstore_dynamic_gas(is_eip2200, &state_load))?;

    // EIP-8037 / Amsterdam: creating a new storage slot (original == present == 0,
    // new != 0) also consumes state gas from the reservoir before spilling into
    // execution gas.
    if state.feature(EvmFeatures::EIP8037) {
        gas.spend_state(state.gas_params().sstore_state_gas(&state_load))?;

        // EIP-8037: a 0→x→0 storage restoration returns the state gas charged for
        // the initial 0→x transition directly to the reservoir, rather than
        // routing it through the capped refund counter below.
        let refill = state.gas_params().sstore_state_gas_refill(&state_load);
        gas.refill_reservoir(refill);
    }

    // EIP-2200 and EIP-3529 refund rules, including negative refund adjustments for
    // dirty nonzero slots and London's reduced clearing-slot refund via gas params.
    gas.record_refund(state.gas_params().sstore_refund(is_eip2200, &state_load));
    Ok(())
}

#[instruction]
pub fn tload(cx: _, [key]: [Word]) -> out {
    let destination = &cx.state.message().destination;
    *out = cx.state.host().tload(destination, key);
}

#[instruction]
pub fn tstore(cx: _, [key, value]: [Word]) -> Result {
    require_non_staticcall(cx.state)?;
    let destination = &cx.state.message().destination;
    cx.state.host().tstore(destination, key, value);
}

#[instruction(no_stack_preamble, dynamic_gas)]
pub fn log<const N: usize>(cx: _) -> Result {
    log_common(cx, stack, N)
}

#[inline(never)]
fn log_common<T: EvmTypesHost>(
    cx: GasInstructionCx<'_, '_, '_, T>,
    mut stack: StackMut<'_>,
    n: usize,
) -> Result {
    require_non_staticcall(cx.state)?;
    let [offset, len] = stack.popn()?;
    let len = len.to_usize()?;
    cx.gas.spend(cx.state.gas_params().log_cost(n as u8, len))?;

    let offset = if len == 0 {
        0
    } else {
        let offset = offset.to_usize()?;
        cx.state.resize_memory(cx.gas, offset, len)?;
        offset
    };

    let mut topics = [B256::ZERO; 4];
    for (topic, word) in topics.iter_mut().zip(stack.popn_dyn(n)?) {
        *topic = B256::from(word);
    }
    let destination = &cx.state.message().destination;
    let (memory, host) = cx.state.0.memory_host();
    // SAFETY: `log` is only dispatched for LOG0 through LOG4.
    host.log(
        destination,
        unsafe { topics.get_unchecked(..n) }.into(),
        memory.slice(offset, len).into(),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{
        SpecId,
        interpreter::{InstrStop, MessageExt, MessageKind, Word, op},
        storage_key::StorageKey,
        test_utils::{RunConfig, TestHost, push, run},
    };
    use alloc::vec::Vec;
    use alloy_primitives::{Address, B256, Bytes};
    use core::assert_matches;

    #[test]
    fn sload_opcode() {
        let mut host = TestHost::default();
        host.storage.insert(StorageKey::new(Address::ZERO, Word::ONE), Word::from(0xbeef));

        let mut code = Vec::new();
        push(&mut code, 1);
        code.extend([op::SLOAD, op::STOP]);
        let interp = run(RunConfig::new(code).host(&mut host));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::from(0xbeef)]);

        let mut code = Vec::new();
        push(&mut code, 2);
        code.extend([op::SLOAD, op::STOP]);
        let interp = run(RunConfig::new(code).host(&mut host));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [0]);
    }

    #[test]
    fn sstore_opcode() {
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push(&mut code, 0xbeef);
        push(&mut code, 1);
        code.push(op::SSTORE);
        push(&mut code, 1);
        code.extend([op::SLOAD, op::STOP]);

        let interp = run(RunConfig::new(code).host(&mut host).gas_limit(30_000));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::from(0xbeef)]);
        assert_eq!(
            host.storage.get(&StorageKey::new(Address::ZERO, Word::ONE)),
            Some(&Word::from(0xbeef))
        );
    }

    #[test]
    fn sstore_staticcall_check() {
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push(&mut code, 0xbeef);
        push(&mut code, 1);
        code.extend([op::SSTORE, op::STOP]);

        let interp = run(RunConfig::new(code).host(&mut host).staticcall());
        assert_matches!(interp.err, InstrStop::StateChangeDuringStaticCall);
        assert!(interp.stack().is_empty());
        assert_eq!(host.storage.get(&StorageKey::new(Address::ZERO, Word::ONE)), None);
    }

    #[test]
    fn sstore_staticcall_message_kind_check() {
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push(&mut code, 0xbeef);
        push(&mut code, 1);
        code.extend([op::SSTORE, op::STOP]);
        let message =
            MessageExt { kind: MessageKind::StaticCall, gas_limit: 10_000, ..Default::default() };

        let interp = run(RunConfig::new(code).host(&mut host).message(message));

        assert_matches!(interp.err, InstrStop::StateChangeDuringStaticCall);
        assert!(interp.stack().is_empty());
        assert_eq!(host.storage.get(&StorageKey::new(Address::ZERO, Word::ONE)), None);
    }

    #[test]
    fn sstore_stipend_check() {
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push(&mut code, 0xbeef);
        push(&mut code, 1);
        code.extend([op::SSTORE, op::STOP]);

        let interp =
            run(RunConfig::new(code).host(&mut host).spec(SpecId::ISTANBUL).gas_limit(2306));
        assert_matches!(interp.err, InstrStop::ReentrancySentryOOG);
        assert_eq!(host.storage.get(&StorageKey::new(Address::ZERO, Word::ONE)), None);
    }

    #[test]
    fn sstore_static_gas() {
        let mut host = TestHost::default();
        let interp = run(RunConfig::new([op::PUSH1, 0, op::PUSH1, 0, op::SSTORE, op::STOP])
            .host(&mut host)
            .spec(SpecId::FRONTIER)
            .gas_limit(6000));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.gas_remaining(), 994);
    }

    #[test]
    fn sstore_noop_uses_warm_load_gas() {
        let mut host = TestHost::default();
        let interp = run(RunConfig::new([op::PUSH1, 0, op::PUSH1, 0, op::SSTORE, op::STOP])
            .host(&mut host)
            .spec(SpecId::BERLIN)
            .gas_limit(3000));

        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.gas_remaining(), 2894);
    }

    #[test]
    fn sstore_dirty_slot_write_only_pays_load_cost() {
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push(&mut code, 1);
        push(&mut code, 0);
        code.push(op::SSTORE);
        push(&mut code, 2);
        push(&mut code, 0);
        code.extend([op::SSTORE, op::STOP]);

        let interp =
            run(RunConfig::new(code).host(&mut host).spec(SpecId::BERLIN).gas_limit(50_000));

        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.gas_remaining(), 29_888);
        assert_eq!(interp.gas_refunded(), 0);
    }

    #[test]
    fn sstore_reset_to_original_records_refund() {
        let mut host = TestHost::default();
        host.storage.insert(StorageKey::new(Address::ZERO, Word::ZERO), Word::from(5));
        let mut code = Vec::new();
        push(&mut code, 7);
        push(&mut code, 0);
        code.push(op::SSTORE);
        push(&mut code, 5);
        push(&mut code, 0);
        code.extend([op::SSTORE, op::STOP]);

        let interp =
            run(RunConfig::new(code).host(&mut host).spec(SpecId::BERLIN).gas_limit(50_000));

        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.gas_remaining(), 46_988);
        assert_eq!(interp.gas_refunded(), 2_800);
    }

    #[test]
    fn sstore_amsterdam_new_slot_charges_state_gas() {
        let mut host = TestHost::default();
        let interp = run(RunConfig::new([op::PUSH1, 1, op::PUSH1, 0, op::SSTORE, op::STOP])
            .host(&mut host)
            .spec(SpecId::AMSTERDAM)
            .gas_limit(200_000));

        assert_matches!(interp.err, InstrStop::Stop);
        // Regular: PUSH1(3)·2 + SSTORE warm (100 static + EIP-8038 STORAGE_WRITE
        // 10_000 set) = 10_106.
        // State gas (64 × 1530 = 97_920) spills into execution gas (no reservoir).
        assert_eq!(interp.state_gas_spent(), 97_920);
        assert_eq!(interp.gas_remaining(), 200_000 - 10_106 - 97_920);
    }

    #[test]
    fn tload_opcode() {
        let mut host = TestHost::default();
        host.transient_storage
            .insert(StorageKey::new(Address::ZERO, Word::ONE), Word::from(0xcafe));

        let mut code = Vec::new();
        push(&mut code, 1);
        code.extend([op::TLOAD, op::STOP]);
        let interp = run(RunConfig::new(code).host(&mut host).spec(SpecId::CANCUN));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::from(0xcafe)]);

        let interp = run(RunConfig::new([op::PUSH1, 0, op::TLOAD, op::STOP])
            .host(&mut host)
            .spec(SpecId::SHANGHAI));
        assert_matches!(interp.err, InstrStop::NotActivated);
        assert_eq!(interp.stack(), [0]);
    }

    #[test]
    fn tstore_opcode() {
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push(&mut code, 0xcafe);
        push(&mut code, 1);
        code.push(op::TSTORE);
        push(&mut code, 1);
        code.extend([op::TLOAD, op::STOP]);

        let interp = run(RunConfig::new(code).host(&mut host).spec(SpecId::CANCUN));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::from(0xcafe)]);
        assert_eq!(
            host.transient_storage.get(&StorageKey::new(Address::ZERO, Word::ONE)),
            Some(&Word::from(0xcafe))
        );

        let interp = run(RunConfig::new([op::PUSH1, 0, op::PUSH1, 0, op::TSTORE, op::STOP])
            .host(&mut host)
            .spec(SpecId::SHANGHAI));
        assert_matches!(interp.err, InstrStop::NotActivated);
        assert_eq!(interp.stack(), [0, 0]);
    }

    #[test]
    fn tstore_staticcall_check() {
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push(&mut code, 0xcafe);
        push(&mut code, 1);
        code.extend([op::TSTORE, op::STOP]);

        let interp = run(RunConfig::new(code).host(&mut host).spec(SpecId::CANCUN).staticcall());
        assert_matches!(interp.err, InstrStop::StateChangeDuringStaticCall);
        assert!(interp.stack().is_empty());
        assert_eq!(host.transient_storage.get(&StorageKey::new(Address::ZERO, Word::ONE)), None);
    }

    fn log_code<const N: usize>(offset: usize, len: usize, topics: [Word; N]) -> Vec<u8> {
        let mut code = Vec::new();
        push(&mut code, Word::from(0xbeef));
        push(&mut code, 0);
        code.push(op::MSTORE);
        for topic in topics.into_iter().rev() {
            push(&mut code, topic);
        }
        push(&mut code, len);
        push(&mut code, offset);
        code.push(op::LOG0 + N as u8);
        code.push(op::STOP);
        code
    }

    #[test]
    fn log0_opcode() {
        let mut host = TestHost::default();
        let address = Address::repeat_byte(0x11);
        let message = MessageExt { destination: address, gas_limit: 10_000, ..Default::default() };
        let interp = run(RunConfig::new(log_code(30, 2, [])).host(&mut host).message(message));
        assert_matches!(interp.err, InstrStop::Stop);
        assert!(interp.stack().is_empty());
        assert_eq!(host.logs.len(), 1);
        assert_eq!(host.logs[0].address, address);
        assert!(host.logs[0].topics().is_empty());
        assert_eq!(host.logs[0].data.data, Bytes::from_static(&[0xbe, 0xef]));
    }

    #[test]
    fn log1_opcode() {
        let mut host = TestHost::default();
        let interp = run(RunConfig::new(log_code(30, 2, [Word::ONE])).host(&mut host));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(host.logs[0].topics(), &[B256::with_last_byte(1)]);
        assert_eq!(host.logs[0].data.data, Bytes::from_static(&[0xbe, 0xef]));
    }

    #[test]
    fn log2_opcode() {
        let mut host = TestHost::default();
        let interp =
            run(RunConfig::new(log_code(30, 0, [Word::ONE, Word::from(2)])).host(&mut host));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(host.logs[0].topics(), &[B256::with_last_byte(1), B256::with_last_byte(2),]);
        assert!(host.logs[0].data.data.is_empty());
    }

    #[test]
    fn log3_opcode() {
        let mut host = TestHost::default();
        let interp =
            run(RunConfig::new(log_code(30, 1, [Word::ONE, Word::from(2), Word::from(3)]))
                .host(&mut host));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(host.logs[0].topics().len(), 3);
        assert_eq!(host.logs[0].data.data, Bytes::from_static(&[0xbe]));
    }

    #[test]
    fn log4_opcode() {
        let mut host = TestHost::default();
        let interp = run(RunConfig::new(log_code(
            30,
            1,
            [Word::ONE, Word::from(2), Word::from(3), Word::from(4)],
        ))
        .host(&mut host));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(host.logs[0].topics().len(), 4);
        assert_eq!(host.logs[0].data.data, Bytes::from_static(&[0xbe]));
    }

    #[test]
    fn log_staticcall_check() {
        let mut host = TestHost::default();
        let interp = run(RunConfig::new(log_code(30, 2, [])).host(&mut host).staticcall());
        assert_matches!(interp.err, InstrStop::StateChangeDuringStaticCall);
        assert!(host.logs.is_empty());
    }

    #[test]
    fn log_memory_oog() {
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push(&mut code, 1);
        push(&mut code, Word::MAX);
        code.extend([op::LOG0, op::STOP]);
        let interp = run(RunConfig::new(code).host(&mut host));
        assert_matches!(interp.err, InstrStop::InvalidOperandOOG);
        assert!(host.logs.is_empty());
    }
}
