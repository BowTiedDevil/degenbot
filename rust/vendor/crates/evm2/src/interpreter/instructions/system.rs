//! System opcode implementations.

use super::require_non_staticcall;
use crate::{
    EvmFeatures, EvmTypesHost,
    bytecode::Bytecode,
    interpreter::{
        Gas, Host, InstrStop, InterpreterState, Message, MessageExt, MessageKind, MessageResult,
        Result, StackMut, Word, create_preaccess_failure, derive_create_destination,
    },
    utils::WordExt,
    version::GasId,
};
use alloy_primitives::{Address, B256, Bytes};
use core::{cmp::min, ops::Range};
use evm2_macros::instruction;

#[inline]
const fn should_charge_new_account_gas(
    eip161: bool,
    transfers_value: bool,
    target_is_empty_for_new_account_gas: bool,
) -> bool {
    target_is_empty_for_new_account_gas && (!eip161 || transfers_value)
}

fn resize_memory_range<T: EvmTypesHost>(
    gas: &mut Gas,
    state: &mut InterpreterState<'_, '_, T>,
    offset: Word,
    len: Word,
) -> Result<Range<usize>> {
    let len = len.to_usize()?;
    let offset = if len != 0 {
        let offset = offset.to_usize()?;
        state.resize_memory(gas, offset, len)?;
        offset
    } else {
        usize::MAX
    };
    Ok(offset..offset + len)
}

fn get_memory_input_and_out_ranges<T: EvmTypesHost>(
    gas: &mut Gas,
    state: &mut InterpreterState<'_, '_, T>,
    input_offset: Word,
    input_len: Word,
    return_offset: Word,
    return_len: Word,
) -> Result<(Range<usize>, Range<usize>)> {
    let input = resize_memory_range(gas, state, input_offset, input_len)?;
    let output = resize_memory_range(gas, state, return_offset, return_len)?;
    Ok((input, output))
}

fn memory_range_bytes<T: EvmTypesHost>(
    state: &mut InterpreterState<'_, '_, T>,
    range: Range<usize>,
) -> Result<Bytes> {
    if range.is_empty() {
        return Ok(Bytes::new());
    }
    Ok(Bytes::copy_from_slice(state.memory().slice(range.start, range.len())))
}

fn load_acc_and_calc_gas<T: EvmTypesHost>(
    gas: &mut Gas,
    state: &mut InterpreterState<'_, '_, T>,
    to: Address,
    transfers_value: bool,
    create_empty_account: bool,
    stack_gas_limit: u64,
) -> Result<(u64, u64, Bytecode, Address)> {
    if transfers_value {
        gas.spend(state.gas_params().get(GasId::TransferValueCost))?;
    }

    let additional_cold_cost = state.gas_params().cold_account_additional_cost();
    let remaining_gas = gas.remaining();
    let skip_cold_load = remaining_gas < additional_cold_cost;
    let account =
        state.host().load_account(&to, true, skip_cold_load).map_err(|error| state.fail(error))?;

    let mut cost = 0;
    if account.is_cold {
        cost += additional_cold_cost;
    }
    let mut code = account.code;
    let mut code_address = to;
    if state.feature(EvmFeatures::EIP7702)
        && let Some(delegated_address) = code.eip7702_address()
    {
        cost += state.gas_params().get(GasId::WarmStorageReadCost);
        if cost > remaining_gas {
            return Err(InstrStop::OutOfGas);
        }
        let skip_cold_load = remaining_gas < cost.saturating_add(additional_cold_cost);
        let delegated_account = state
            .host()
            .load_account(&delegated_address, true, skip_cold_load)
            .map_err(|error| state.fail(error))?;
        if delegated_account.is_cold {
            cost += additional_cold_cost;
        }
        code = delegated_account.code;
        code_address = delegated_address;
    }
    let mut new_account_state_gas = 0;
    if create_empty_account
        && should_charge_new_account_gas(
            state.feature(EvmFeatures::EIP161),
            transfers_value,
            account.is_empty_for_new_account_gas,
        )
    {
        cost += state.gas_params().get(GasId::NewAccountCost);
        // EIP-8037: value transfer to a new account also charges state gas from
        // the reservoir. Charged before the 63/64 child gas split below.
        if state.feature(EvmFeatures::EIP8037) && transfers_value {
            new_account_state_gas = state.gas_params().new_account_state_gas();
        }
    }
    gas.spend(cost)?;
    gas.spend_state(new_account_state_gas)?;

    let mut gas_limit = if state.feature(EvmFeatures::EIP150) {
        min(state.gas_params().call_stipend_reduction(gas.remaining()), stack_gas_limit)
    } else {
        stack_gas_limit
    };
    gas.spend(gas_limit)?;

    if transfers_value {
        gas_limit = gas_limit.saturating_add(state.gas_params().get(GasId::CallStipend));
    }

    Ok((gas_limit, new_account_state_gas, code, code_address))
}

/// # Safety
///
/// The message must be passed to the host as in
/// [`Interpreter::call_input`](crate::interpreter::Interpreter::call_input).
#[inline(never)]
unsafe fn prepare_call<T: EvmTypesHost>(
    mut stack: StackMut<'_>,
    gas: &mut Gas,
    state: &mut InterpreterState<'_, '_, T>,
    kind: MessageKind,
    message: &mut Message<T>,
    return_memory_range: &mut Range<usize>,
) -> Result<u64> {
    let has_value = match kind {
        MessageKind::Call | MessageKind::CallCode => true,
        MessageKind::DelegateCall | MessageKind::StaticCall => false,
        _ => unreachable!("invalid call message kind"),
    };
    let [local_gas_limit, to] = stack.popn::<2>()?;
    let value = if has_value { stack.pop()? } else { Word::ZERO };
    let [input_offset, input_len, return_offset, return_len] = stack.popn::<4>()?;
    let to = to.to_address();
    let has_transfer = !value.is_zero();
    if state.is_static() && kind == MessageKind::Call && has_transfer {
        return Err(InstrStop::CallNotAllowedInsideStatic);
    }

    let local_gas_limit = local_gas_limit.saturating_to::<u64>();
    let (input_range, prepared_return_memory_range) = get_memory_input_and_out_ranges(
        gas,
        state,
        input_offset,
        input_len,
        return_offset,
        return_len,
    )?;
    let (gas_limit, new_account_state_gas, loaded_code, resolved_code_address) =
        load_acc_and_calc_gas(
            gas,
            state,
            to,
            has_transfer,
            kind == MessageKind::Call,
            local_gas_limit,
        )?;
    // SAFETY: Upheld by the caller.
    let input = unsafe { state.0.call_input(input_range.start, input_range.len()) };

    let current = state.message();
    let (destination, caller, call_value, code_address) = match kind {
        MessageKind::Call => (to, current.destination, value, resolved_code_address),
        MessageKind::CallCode => {
            (current.destination, current.destination, value, resolved_code_address)
        }
        MessageKind::DelegateCall => {
            (current.destination, current.caller, current.value, resolved_code_address)
        }
        MessageKind::StaticCall => (to, current.destination, Word::ZERO, resolved_code_address),
        _ => unreachable!("invalid call message kind"),
    };
    *message = MessageExt {
        kind,
        depth: current.depth.saturating_add(1),
        gas_limit,
        reservoir: gas.reservoir(),
        destination,
        call_target: to,
        caller,
        input,
        value: call_value,
        code: loaded_code,
        code_address,
        caller_is_static: state.is_static(),
        salt: B256::ZERO,
        ext: T::MessageExt::default(),
        _non_exhaustive: (),
    };
    *return_memory_range = prepared_return_memory_range;

    Ok(new_account_state_gas)
}

#[instruction(no_stack_preamble, hooked)]
pub fn call(cx: _) -> Result {
    call_inner(stack, cx.gas, cx.state, MessageKind::Call)
}

#[instruction(no_stack_preamble, hooked)]
pub fn callcode(cx: _) -> Result {
    call_inner(stack, cx.gas, cx.state, MessageKind::CallCode)
}

#[instruction(no_stack_preamble, hooked)]
pub fn delegatecall(cx: _) -> Result {
    call_inner(stack, cx.gas, cx.state, MessageKind::DelegateCall)
}

#[instruction(no_stack_preamble, hooked)]
pub fn staticcall(cx: _) -> Result {
    call_inner(stack, cx.gas, cx.state, MessageKind::StaticCall)
}

#[instruction(no_stack_preamble, hooked)]
pub fn create<const IS_CREATE2: bool>(cx: _) -> Result {
    create_inner(stack, cx.gas, cx.state, IS_CREATE2)
}

#[inline(never)]
fn call_inner<T: EvmTypesHost>(
    stack: StackMut<'_>,
    gas: &mut Gas,
    state: &mut InterpreterState<'_, '_, T>,
    kind: MessageKind,
) -> Result {
    // SAFETY: The hooks do nothing.
    unsafe { call_impl(stack, gas, state, kind, |_, _, _| Ok(()), |_, _, _, _| Ok(())) }
}

/// Executes a call opcode of `kind` with hooks around its child message.
///
/// This is the body of [`call`], [`callcode`], [`delegatecall`] and [`staticcall`], for custom
/// call instructions that need extra behavior. Use it from an
/// `#[instruction(no_stack_preamble, hooked)]` function with `stack`, `cx.gas` and
/// `cx.state`. The instruction must be `hooked`, so that the hooks and the host see its pc, opcode,
/// stack and gas.
///
/// `before` runs after every charge the opcode makes, the EIP-8037 new-account state gas
/// included, and after the forwarded gas is taken from `gas`, right before the child message
/// executes. It may charge or credit `gas`, edit the child message (for example its `gas_limit`,
/// `reservoir` or `ext`), or fail the instruction.
///
/// `after` runs once the child's gas is merged into `gas` and the upfront new-account state gas of
/// a failed child is refunded, before the return data is copied and the success word is pushed.
/// It may charge or credit `gas` or fail the instruction. A fatal child result returns before
/// `after` runs.
///
/// An `Err` from either hook stops the instruction with that error.
///
/// The result can be the stop for a config reload. Return it as the last step of the instruction.
#[inline]
pub fn call_with<T: EvmTypesHost>(
    stack: StackMut<'_>,
    gas: &mut Gas,
    state: &mut InterpreterState<'_, '_, T>,
    kind: MessageKind,
    before: impl FnOnce(&mut Gas, &mut InterpreterState<'_, '_, T>, &mut Message<T>) -> Result,
    after: impl FnOnce(
        &mut Gas,
        &mut InterpreterState<'_, '_, T>,
        &Message<T>,
        &MessageResult<T>,
    ) -> Result,
) -> Result {
    let before =
        |gas: &mut Gas, state: &mut InterpreterState<'_, '_, T>, message: &mut Message<T>| {
            message.input.make_owned();
            before(gas, state, message)
        };
    // SAFETY: `before` copies the input before the hooks can reach it.
    unsafe { call_impl(stack, gas, state, kind, before, after) }
}

/// Body of [`call_with`]. The child message may borrow its input from memory.
///
/// # Safety
///
/// `before` and `after` must not read a borrowed input after changing memory, or let it outlive
/// the call.
#[inline]
unsafe fn call_impl<T: EvmTypesHost>(
    mut stack: StackMut<'_>,
    gas: &mut Gas,
    state: &mut InterpreterState<'_, '_, T>,
    kind: MessageKind,
    before: impl FnOnce(&mut Gas, &mut InterpreterState<'_, '_, T>, &mut Message<T>) -> Result,
    after: impl FnOnce(
        &mut Gas,
        &mut InterpreterState<'_, '_, T>,
        &Message<T>,
        &MessageResult<T>,
    ) -> Result,
) -> Result {
    let mut message = Message::<T>::default();
    let mut return_memory_range = 0..0;
    // SAFETY: Upheld by the caller for the hooks. Nothing else reads the message after the host
    // returns.
    let new_account_state_gas = unsafe {
        prepare_call(stack.reborrow(), gas, state, kind, &mut message, &mut return_memory_range)
    }?;
    before(gas, state, &mut message)?;

    let mut result =
        state.host().execute_message(&mut message).map_err(|error| state.fail(error))?;
    if result.stop.is_fatal() {
        return Err(result.stop);
    }
    state.note_inspector_config_change();
    gas.merge_child_gas(result.gas, result.stop);
    // EIP-8037: a value-bearing CALL that creates the target charges NEW_ACCOUNT state
    // gas upfront on this frame. If the call does not succeed (depth/balance failure,
    // child revert or halt) the target is not created, so refund the upfront charge to
    // the reservoir (execution-specs `credit_state_gas_refund`), mirroring CREATE.
    if new_account_state_gas != 0 && !result.is_success() {
        gas.refill_reservoir(new_account_state_gas);
    }
    after(gas, state, &message, &result)?;
    let copy_len = min(return_memory_range.len(), result.output.len());
    unsafe {
        let output = result.output.get_unchecked(..copy_len);
        state.memory().set_unchecked(return_memory_range.start, output);
    }
    state.swap_return_data(&mut result.output);
    stack.push_u64(result.is_success() as u64)?;
    // A hook's stop or a config reload.
    state.result()
}

#[inline(never)]
fn create_inner<T: EvmTypesHost>(
    stack: StackMut<'_>,
    gas: &mut Gas,
    state: &mut InterpreterState<'_, '_, T>,
    is_create2: bool,
) -> Result {
    create_with(stack, gas, state, is_create2, |_, _, _| Ok(()), |_, _, _, _| Ok(()))
}

/// Executes `CREATE` or `CREATE2` with hooks around its child message.
///
/// This is the body of [`create`], for custom creation instructions that need extra behavior, in
/// the same shape as [`call_with`].
///
/// `before` runs after every charge the opcode makes, the EIP-8037 account-creation state gas
/// included, and after the forwarded gas is taken from `gas`, right before the child message
/// executes. It does not run when the opcode refuses the creation before forwarding gas (the
/// EIP-8037 balance, nonce and depth checks).
///
/// `after` runs once the child's gas is merged into `gas` and the account-creation state gas of a
/// failed creation is refunded, before the return data is set and the created address is pushed.
/// A fatal child result returns before `after` runs.
///
/// Both hooks may charge or credit `gas` or fail the instruction, as in [`call_with`].
/// Its result can be the stop for a config reload, as in [`call_with`].
#[inline]
pub fn create_with<T: EvmTypesHost>(
    mut stack: StackMut<'_>,
    gas: &mut Gas,
    state: &mut InterpreterState<'_, '_, T>,
    is_create2: bool,
    before: impl FnOnce(&mut Gas, &mut InterpreterState<'_, '_, T>, &mut Message<T>) -> Result,
    after: impl FnOnce(
        &mut Gas,
        &mut InterpreterState<'_, '_, T>,
        &Message<T>,
        &MessageResult<T>,
    ) -> Result,
) -> Result {
    require_non_staticcall(state)?;

    let [value, offset, len] = stack.popn::<3>()?;
    let salt = if is_create2 { Some(stack.pop()?) } else { None };

    let len = len.to_usize()?;
    if state.feature(EvmFeatures::EIP3860) {
        if len > state.version().max_initcode_size {
            return Err(InstrStop::CreateInitCodeSizeLimit);
        }
        gas.spend(state.gas_params().initcode_cost(len))?;
    }
    let code_range = resize_memory_range(gas, state, offset, Word::from(len))?;
    let input = memory_range_bytes(state, code_range)?;
    let create_cost = if is_create2 {
        state.gas_params().create2_cost(len)
    } else {
        state.gas_params().get(GasId::Create)
    };
    gas.spend(create_cost)?;

    let kind = if is_create2 { MessageKind::Create2 } else { MessageKind::Create };
    let current = state.message();
    let caller = current.destination;
    let depth = current.depth.saturating_add(1);
    let salt = salt.map(B256::from).unwrap_or_default();

    // Derive the created contract address once. CREATE needs the creator's (pre-bump) nonce; the
    // caller is the currently-executing contract, already warm and in the access list, so this read
    // is neutral. CREATE2 needs no nonce. The same load feeds the EIP-8037 balance/nonce checks
    // below, so it is only performed when one of the two needs it.
    let caller_info = if is_create2 && !state.feature(EvmFeatures::EIP8037) {
        None
    } else {
        Some(state.host().load_account(&caller, false, false).map_err(|error| state.fail(error))?)
    };
    let destination = derive_create_destination(
        kind,
        &caller,
        &salt,
        &input,
        caller_info.as_ref().map_or(0, |info| info.nonce),
    );

    // EIP-8037: charge the CREATE account-creation state gas on this frame
    // before the child gas/reservoir split, conditional on the destination not already existing.
    // The single destination read decides the charge (and warms the address); a create at a
    // pre-existing leaf pays nothing, so its child is not shortchanged by an unneeded spill. The
    // charge is refunded via `refill_reservoir` if the create fails to deploy (see the
    // create-failure path after `execute_message`).
    let mut charged_create_state_gas = false;
    if state.feature(EvmFeatures::EIP8037) {
        // `caller_info` is always `Some` when EIP-8037 is enabled.
        if let Some(caller_info) = caller_info.as_ref()
            && create_preaccess_failure(
                caller_info.balance,
                caller_info.nonce,
                value,
                depth,
                state.version(),
            )
            .is_some()
        {
            state.clear_return_data();
            stack.push_u64(0)?;
            return Ok(());
        }

        // The destination is loaded here and made warm. Balance, nonce, and depth pre-access
        // failures return above before the destination is accessed.
        if state
            .host()
            .load_account(&destination, false, false)
            .map_err(|error| state.fail(error))?
            .is_empty_for_new_account_gas
        {
            gas.spend_state(state.gas_params().create_state_gas())?;
            charged_create_state_gas = true;
        }
    }
    let gas_limit = if state.feature(EvmFeatures::EIP150) {
        state.gas_params().call_stipend_reduction(gas.remaining())
    } else {
        gas.remaining()
    };
    gas.spend(gas_limit)?;

    let mut message = MessageExt {
        kind,
        depth,
        gas_limit,
        reservoir: gas.reservoir(),
        destination,
        call_target: destination,
        caller,
        code: Bytecode::new_legacy(input.clone()),
        input: input.into(),
        value,
        code_address: destination,
        // CREATE is rejected in a static context (see `require_non_staticcall`).
        caller_is_static: false,
        salt,
        ext: T::MessageExt::default(),
        _non_exhaustive: (),
    };
    before(gas, state, &mut message)?;
    let mut result =
        state.host().execute_message(&mut message).map_err(|error| state.fail(error))?;
    if result.stop.is_fatal() {
        return Err(result.stop);
    }
    state.note_inspector_config_change();
    gas.merge_child_gas(result.gas, result.stop);

    // EIP-8037: when the opcode charged the conditional `create_state_gas`
    // and the create then fails to deploy (revert, halt, or an early-fail leaving
    // `created_address == None` — depth, out-of-funds, nonce overflow), no new account leaf is
    // created, so refund the charge to the reservoir via `refill_reservoir` (matching 0→x→0 storage
    // restoration). A successful deployment keeps the charge; an alive target was never charged.
    let create_failed = result.created_address.is_none() || !result.is_success();
    if charged_create_state_gas && create_failed {
        gas.refill_reservoir(state.gas_params().create_state_gas());
    }
    after(gas, state, &message, &result)?;
    // EIP-211 exposes CREATE failure data only for REVERT; other failures clear returndata.
    if result.stop.is_revert() {
        state.swap_return_data(&mut result.output);
    } else {
        state.clear_return_data();
    }
    stack.push(result.created_address_for_parent())?;
    // A hook's stop or a config reload.
    state.result()
}

#[instruction(dynamic_gas)]
pub fn selfdestruct(cx: _, [target]: [Word]) -> Result {
    require_non_staticcall(cx.state)?;
    let target = target.to_address();
    let cold_load_gas = cx.state.gas_params().selfdestruct_cold_cost();
    let skip_cold_load = cx.gas.remaining() < cold_load_gas;
    let destination = &cx.state.message().destination;
    let res = cx
        .state
        .host()
        .selfdestruct(destination, &target, skip_cold_load)
        .map_err(|error| cx.state.fail(error))?;
    let should_charge_topup = should_charge_new_account_gas(
        cx.state.feature(EvmFeatures::EIP161),
        res.had_value,
        res.target_is_empty,
    );
    cx.gas.spend(cx.state.gas_params().selfdestruct_cost(should_charge_topup, res.is_cold))?;
    // EIP-8037: sending balance to a new account via SELFDESTRUCT charges the
    // new-account state gas from the reservoir.
    if should_charge_topup && cx.state.feature(EvmFeatures::EIP8037) {
        cx.gas.spend_state(cx.state.gas_params().new_account_state_gas())?;
    }
    if !res.previously_destroyed {
        cx.gas.record_refund(cx.state.gas_params().get(GasId::SelfdestructRefund) as i64);
    }
    cx.state.inspect_selfdestruct(destination, &target, &res.value);
    Err(InstrStop::SelfDestruct)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BaseEvmConfig, EvmConfig, ExecutionConfig, ExecutionError, SpecId,
        constants::{CALL_DEPTH_LIMIT, MAX_INITCODE_SIZE},
        interpreter::{Interpreter, MessageResultExt, op},
        test_utils::{
            RunConfig, TestHost, TestInterpreter, TestTypes, create_code, legacy_bytecode, push,
            push_all, run,
        },
        utils::address_to_word,
        version::OpcodeConfig,
    };
    use alloc::vec::Vec;
    use core::assert_matches;

    #[test]
    fn call_opcode() {
        let target = Address::repeat_byte(0x22);
        let caller = Address::repeat_byte(0x11);
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push_all(
            &mut code,
            [
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                target.into_word().into(),
                Word::from(1000),
            ],
        );
        code.extend([op::CALL, op::STOP]);

        let interp = run(RunConfig::new(code).host(&mut host).message(MessageExt {
            destination: caller,
            gas_limit: 10_000,
            ..Default::default()
        }));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::ONE]);
        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.calls[0].kind, MessageKind::Call);
        assert_eq!(host.calls[0].destination, target);
        assert_eq!(host.calls[0].caller, caller);
    }

    #[test]
    fn call_propagates_fatal_child_result() {
        let target = Address::repeat_byte(0x22);
        let error = ExecutionError::Fatal("child call failed".into());
        let mut host = TestHost { execute_error: Some(error.clone()), ..Default::default() };
        let mut code = crate::test_utils::call_code(target);
        code.extend([op::CALL, op::STOP]);

        let interp = run(RunConfig::new(code).host(&mut host));

        assert_matches!(interp.err, InstrStop::FatalExternalError);
        assert_eq!(interp.execution_error, Some(error));
        assert!(interp.stack().is_empty());
    }

    #[test]
    fn call_propagates_static_flag() {
        let target = Address::repeat_byte(0x22);
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push_all(
            &mut code,
            [
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                target.into_word().into(),
                Word::from(1000),
            ],
        );
        code.extend([op::CALL, op::STOP]);

        let interp = run(RunConfig::new(code).host(&mut host).staticcall());
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.calls[0].kind, MessageKind::Call);
        assert!(host.call_static_flags[0]);
    }

    #[test]
    fn call_too_deep_charges_dynamic_gas() {
        let target = Address::repeat_byte(0x22);
        let mut host = TestHost { is_cold: true, is_empty: true, ..Default::default() };
        let mut code = Vec::new();
        push_all(
            &mut code,
            [
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ONE,
                target.into_word().into(),
                Word::from(1000),
            ],
        );
        code.extend([op::CALL, op::STOP]);

        let interp = run(RunConfig::new(code)
            .host(&mut host)
            .spec(SpecId::BERLIN)
            .message(MessageExt { depth: CALL_DEPTH_LIMIT, ..Default::default() })
            .gas_limit(50_000));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::ZERO]);
        assert_eq!(interp.gas_remaining(), 15_679);
        assert!(host.calls.is_empty());
    }

    #[test]
    fn call_with_value_too_deep_credits_stipend() {
        let mut host = TestHost::default();
        let code = [
            op::PUSH1,
            0xff,
            op::PUSH1,
            0,
            op::PUSH1,
            0xff,
            op::PUSH1,
            0,
            op::PUSH1,
            1,
            op::PUSH1,
            0xaa,
            op::PUSH2,
            0x80,
            0,
            op::CALL,
            op::POP,
        ];

        let interp = run(RunConfig::new(code)
            .host(&mut host)
            .spec(SpecId::TANGERINE)
            .message(MessageExt { depth: CALL_DEPTH_LIMIT, ..Default::default() })
            .gas_limit(50_000));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.gas_remaining(), 42_553);
        assert!(host.calls.is_empty());
    }

    #[test]
    fn call_too_deep_charges_pre_spurious_empty_account() {
        let target = Address::repeat_byte(0x22);
        let mut host = TestHost { exists: false, is_empty: true, ..Default::default() };
        let mut code = Vec::new();
        push_all(
            &mut code,
            [
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                target.into_word().into(),
                Word::ZERO,
            ],
        );
        code.extend([op::CALL, op::STOP]);

        let interp = run(RunConfig::new(code)
            .host(&mut host)
            .spec(SpecId::TANGERINE)
            .message(MessageExt { depth: CALL_DEPTH_LIMIT, ..Default::default() })
            .gas_limit(50_000));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::ZERO]);
        assert_eq!(interp.gas_remaining(), 24_279);
        assert!(host.calls.is_empty());
    }

    #[test]
    fn call_too_deep_skips_pre_spurious_touched_empty_account_cost() {
        let target = Address::repeat_byte(0x22);
        let mut host =
            TestHost { exists: false, is_empty: true, is_touched: true, ..Default::default() };
        let mut code = Vec::new();
        push_all(
            &mut code,
            [
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                target.into_word().into(),
                Word::ZERO,
            ],
        );
        code.extend([op::CALL, op::STOP]);

        let interp = run(RunConfig::new(code)
            .host(&mut host)
            .spec(SpecId::TANGERINE)
            .message(MessageExt { depth: CALL_DEPTH_LIMIT, ..Default::default() })
            .gas_limit(50_000));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::ZERO]);
        assert_eq!(interp.gas_remaining(), 49_279);
        assert!(host.calls.is_empty());
    }

    #[test]
    fn callcode_propagates_static_flag_and_allows_apparent_value() {
        let code_address = Address::repeat_byte(0x33);
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push_all(
            &mut code,
            [
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::from(7),
                code_address.into_word().into(),
                Word::from(1000),
            ],
        );
        code.extend([op::CALLCODE, op::STOP]);

        let interp = run(RunConfig::new(code).host(&mut host).staticcall().gas_limit(20_000));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.calls[0].kind, MessageKind::CallCode);
        assert_eq!(host.calls[0].value, Word::from(7));
        assert!(host.call_static_flags[0]);
    }

    #[test]
    fn callcode_opcode() {
        let code_address = Address::repeat_byte(0x33);
        let destination = Address::repeat_byte(0x11);
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push_all(
            &mut code,
            [
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::from(7),
                code_address.into_word().into(),
                Word::from(1000),
            ],
        );
        code.extend([op::CALLCODE, op::STOP]);

        let interp = run(RunConfig::new(code)
            .host(&mut host)
            .message(MessageExt { destination, ..Default::default() })
            .gas_limit(20_000));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::ONE]);
        assert_eq!(host.calls[0].kind, MessageKind::CallCode);
        assert_eq!(host.calls[0].destination, destination);
        assert_eq!(host.calls[0].code_address, code_address);
        assert_eq!(host.calls[0].value, Word::from(7));
    }

    #[test]
    fn delegatecall_opcode() {
        let code_address = Address::repeat_byte(0x44);
        let caller = Address::repeat_byte(0x55);
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push_all(
            &mut code,
            [
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                code_address.into_word().into(),
                Word::from(1000),
            ],
        );
        code.extend([op::DELEGATECALL, op::STOP]);

        let interp = run(RunConfig::new(code).host(&mut host).message(MessageExt {
            caller,
            value: Word::from(9),
            gas_limit: 10_000,
            ..Default::default()
        }));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::ONE]);
        assert_eq!(host.calls[0].kind, MessageKind::DelegateCall);
        assert_eq!(host.calls[0].caller, caller);
        assert_eq!(host.calls[0].value, Word::from(9));
        assert_eq!(host.calls[0].code_address, code_address);

        let interp = run(RunConfig::new([
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::DELEGATECALL,
        ])
        .spec(SpecId::FRONTIER));
        assert_matches!(interp.err, InstrStop::NotActivated);
    }

    #[test]
    fn staticcall_opcode() {
        let target = Address::repeat_byte(0x66);
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push_all(
            &mut code,
            [
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                target.into_word().into(),
                Word::from(1000),
            ],
        );
        code.extend([op::STATICCALL, op::STOP]);

        let interp = run(RunConfig::new(code).host(&mut host));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::ONE]);
        assert_eq!(host.calls[0].kind, MessageKind::StaticCall);
        assert!(host.call_static_flags[0]);

        let interp = run(RunConfig::new([
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::STATICCALL,
        ])
        .spec(SpecId::HOMESTEAD));
        assert_matches!(interp.err, InstrStop::NotActivated);
    }

    #[test]
    fn create_opcode() {
        let created = Address::repeat_byte(0x77);
        let mut host = TestHost {
            execute_result: MessageResultExt {
                stop: InstrStop::Return,
                created_address: Some(created),
                ..MessageResultExt::default()
            },
            ..Default::default()
        };
        let mut code = Vec::new();
        push_all(&mut code, [Word::ZERO, Word::ZERO, Word::ZERO]);
        code.extend([op::CREATE, op::STOP]);

        let interp = run(RunConfig::new(code).host(&mut host).gas_limit(50_000));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [address_to_word(&created)]);
        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.calls[0].kind, MessageKind::Create);
    }

    #[test]
    fn create_propagates_fatal_child_result() {
        let error = ExecutionError::Fatal("child create failed".into());
        let mut host = TestHost { execute_error: Some(error.clone()), ..Default::default() };
        let mut code = create_code();
        code.extend([op::CREATE, op::STOP]);

        let interp = run(RunConfig::new(code).host(&mut host).gas_limit(50_000));

        assert_matches!(interp.err, InstrStop::FatalExternalError);
        assert_eq!(interp.execution_error, Some(error));
        assert!(interp.stack().is_empty());
    }

    #[test]
    fn create_clears_return_data_on_success() {
        let created = Address::repeat_byte(0x77);
        let mut host = TestHost {
            execute_result: MessageResultExt {
                stop: InstrStop::Return,
                output: Bytes::from_static(&[0xaa, 0xbb, 0xcc]),
                created_address: Some(created),
                ..MessageResultExt::default()
            },
            ..Default::default()
        };
        let mut code = Vec::new();
        push_all(&mut code, [Word::ZERO, Word::ZERO, Word::ZERO]);
        code.extend([op::CREATE, op::RETURNDATASIZE, op::STOP]);

        let interp = run(RunConfig::new(code).host(&mut host).gas_limit(50_000));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [created.into_word().into(), Word::ZERO]);
    }

    #[test]
    fn create_sets_return_data_on_revert() {
        let mut host = TestHost {
            execute_result: MessageResultExt {
                stop: InstrStop::Revert,
                output: Bytes::from_static(&[0xaa, 0xbb]),
                ..MessageResultExt::default()
            },
            ..Default::default()
        };
        let mut code = Vec::new();
        push_all(&mut code, [Word::ZERO, Word::ZERO, Word::ZERO]);
        code.extend([op::CREATE, op::RETURNDATASIZE, op::STOP]);

        let interp = run(RunConfig::new(code).host(&mut host).gas_limit(50_000));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::ZERO, Word::from(2)]);
    }

    #[test]
    fn create_too_deep_charges_create_gas() {
        let mut host = TestHost::default();
        let mut code = create_code();
        code.extend([op::CREATE, op::STOP]);

        let interp = run(RunConfig::new(code)
            .host(&mut host)
            .spec(SpecId::BERLIN)
            .message(MessageExt { depth: CALL_DEPTH_LIMIT, ..Default::default() })
            .gas_limit(50_000));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::ZERO]);
        assert_eq!(interp.gas_remaining(), 17_991);
        assert!(host.calls.is_empty());
    }

    #[test]
    fn create_too_deep_clears_return_data_and_skips_destination_read_eip8037() {
        // EIP-8037: a create failing the depth pre-check must not access the destination —
        // the read would leak the address into the EIP-7928 block access list.
        let caller = Address::repeat_byte(0x11);
        let mut host = TestHost { exists: false, ..Default::default() };
        let mut code = create_code();
        code.extend([op::CREATE, op::RETURNDATASIZE, op::STOP]);

        let interp = run(RunConfig::new(code)
            .host(&mut host)
            .spec(SpecId::AMSTERDAM)
            .message(MessageExt {
                depth: CALL_DEPTH_LIMIT,
                destination: caller,
                ..Default::default()
            })
            .return_data(Bytes::from_static(b"stale"))
            .gas_limit(50_000));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [Word::ZERO, Word::ZERO]);
        assert_eq!(host.account_loads, [caller]);
    }

    #[test]
    fn create_initcode_size_limit_halts_after_shanghai() {
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push_all(&mut code, [Word::from(MAX_INITCODE_SIZE + 1), Word::ZERO, Word::ZERO]);
        code.extend([op::CREATE, op::STOP]);

        let interp =
            run(RunConfig::new(code).host(&mut host).spec(SpecId::SHANGHAI).gas_limit(50_000));
        assert_matches!(interp.err, InstrStop::CreateInitCodeSizeLimit);
        assert!(host.calls.is_empty());
    }

    #[test]
    fn create2_opcode() {
        let created = Address::repeat_byte(0x88);
        let mut host = TestHost {
            execute_result: MessageResultExt {
                stop: InstrStop::Return,
                created_address: Some(created),
                ..MessageResultExt::default()
            },
            ..Default::default()
        };
        let mut code = Vec::new();
        push_all(&mut code, [Word::ZERO, Word::ZERO, Word::ZERO, Word::ZERO]);
        code.extend([op::CREATE2, op::STOP]);

        let interp = run(RunConfig::new(code).host(&mut host).gas_limit(50_000));
        assert_matches!(interp.err, InstrStop::Stop);
        assert_eq!(interp.stack(), [address_to_word(&created)]);
        assert_eq!(host.calls[0].kind, MessageKind::Create2);

        let interp = run(RunConfig::new([
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::PUSH1,
            0,
            op::CREATE2,
        ])
        .spec(SpecId::BYZANTIUM));
        assert_matches!(interp.err, InstrStop::NotActivated);
    }

    #[test]
    fn selfdestruct_opcode() {
        let contract = Address::repeat_byte(0x11);
        let target = Address::repeat_byte(0x99);
        let mut host = TestHost::default();
        let mut code = Vec::new();
        push(&mut code, address_to_word(&target));
        code.push(op::SELFDESTRUCT);

        let interp = run(RunConfig::new(code).host(&mut host).message(MessageExt {
            destination: contract,
            gas_limit: 10_000,
            ..Default::default()
        }));
        assert_matches!(interp.err, InstrStop::SelfDestruct);
        assert_eq!(host.selfdestructs, [(contract, target, false)]);
    }

    const HOOKED_CALL: u8 = 0x0c;
    const HOOKED_CREATE: u8 = 0x0d;
    const KEPT_GAS: u64 = 1_000;
    const AFTER_FEE: u64 = 7;

    // Keeps `KEPT_GAS` of the forwarded gas in the caller and charges `AFTER_FEE` after a
    // successful child; a failed child fails the instruction.
    #[instruction(no_stack_preamble, hooked)]
    fn hooked_call(cx: _) -> Result {
        call_with(
            stack,
            cx.gas,
            cx.state,
            MessageKind::Call,
            |gas, _, message| {
                message.gas_limit -= KEPT_GAS;
                gas.erase_cost(KEPT_GAS);
                Ok(())
            },
            |gas, _, _, result| {
                if result.is_success() { gas.spend(AFTER_FEE) } else { Err(InstrStop::Revert) }
            },
        )
    }

    #[instruction(no_stack_preamble, hooked)]
    fn hooked_create(cx: _) -> Result {
        create_with(
            stack,
            cx.gas,
            cx.state,
            false,
            |gas, _, message| {
                message.gas_limit -= KEPT_GAS;
                gas.erase_cost(KEPT_GAS);
                Ok(())
            },
            |gas, _, _, _| gas.spend(AFTER_FEE),
        )
    }

    struct HookedConfig;

    impl EvmConfig<TestTypes> for HookedConfig {
        const BASE_SPEC_ID: SpecId = SpecId::AMSTERDAM;
        const OPCODE_CONFIG: &'static OpcodeConfig<TestTypes> = &hooked_opcode_config();
    }

    const fn hooked_opcode_config() -> OpcodeConfig<TestTypes> {
        let mut config =
            OpcodeConfig::<TestTypes>::base::<BaseEvmConfig<{ SpecId::AMSTERDAM as u32 }>>();
        config.set_instruction::<hooked_call<TestTypes>>(HOOKED_CALL, config.static_gas(op::CALL));
        config.set_instruction::<hooked_create<TestTypes>>(
            HOOKED_CREATE,
            config.static_gas(op::CREATE),
        );
        config
    }

    fn run_hooked(config: RunConfig<'_>) -> TestInterpreter {
        let RunConfig { code, host, tx_env, message, gas_limit, return_data, .. } = config;
        let message = Message::<TestTypes> { code: legacy_bytecode(code), gas_limit, ..message };
        let mut inner = Interpreter::<TestTypes>::new(&message);
        *inner.return_data_mut() = return_data;
        let mut default_host = TestHost::default();
        let host = host.unwrap_or(&mut default_host);
        host.tx_env = tx_env;
        host.spec_id = SpecId::AMSTERDAM;
        host.execution_config = ExecutionConfig::<TestTypes>::for_config::<HookedConfig>();
        let err = inner.run(host).unwrap();
        let (stack, stack_len, gas, memory, output) = inner.into_parts();
        TestInterpreter { stack, stack_len, gas, memory, output, err, execution_error: None }
    }

    fn call_code(opcode: u8, value: u64) -> Vec<u8> {
        let mut code = Vec::new();
        push_all(
            &mut code,
            [
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::ZERO,
                Word::from(value),
                Address::repeat_byte(0x22).into_word().into(),
                Word::from(5_000),
            ],
        );
        code.extend([opcode, op::STOP]);
        code
    }

    #[test]
    fn call_with_hooks_adjust_child_and_caller_gas() {
        let mut base_host = TestHost::default();
        let base = run_hooked(
            RunConfig::new(call_code(op::CALL, 0)).host(&mut base_host).gas_limit(30_000),
        );
        let mut host = TestHost::default();
        let hooked =
            run_hooked(RunConfig::new(call_code(HOOKED_CALL, 0)).host(&mut host).gas_limit(30_000));

        assert_matches!(base.err, InstrStop::Stop);
        assert_matches!(hooked.err, InstrStop::Stop);
        assert_eq!(hooked.stack(), [Word::ONE]);
        assert_eq!(host.calls.len(), 1);
        assert_eq!(host.calls[0].gas_limit, base_host.calls[0].gas_limit - KEPT_GAS);
        assert_eq!(hooked.gas_remaining(), base.gas_remaining() + KEPT_GAS - AFTER_FEE);
    }

    #[test]
    fn call_with_before_runs_after_new_account_state_gas() {
        // A value call to an empty account charges EIP-8037 new-account state gas before the
        // forward; `before` sees the child message built after that charge.
        let mut base_host = TestHost { is_empty: true, ..Default::default() };
        let base = run_hooked(
            RunConfig::new(call_code(op::CALL, 1)).host(&mut base_host).gas_limit(200_000),
        );
        let mut host = TestHost { is_empty: true, ..Default::default() };
        let hooked = run_hooked(
            RunConfig::new(call_code(HOOKED_CALL, 1)).host(&mut host).gas_limit(200_000),
        );

        assert_matches!(hooked.err, InstrStop::Stop);
        assert_eq!(host.calls[0].gas_limit, base_host.calls[0].gas_limit - KEPT_GAS);
        assert_eq!(hooked.gas.state_gas_spent(), base.gas.state_gas_spent());
        assert_ne!(hooked.gas.state_gas_spent(), 0);
        assert_eq!(hooked.gas_remaining(), base.gas_remaining() + KEPT_GAS - AFTER_FEE);
    }

    #[test]
    fn call_with_after_can_fail_the_instruction() {
        let mut host = TestHost {
            execute_result: MessageResultExt { stop: InstrStop::Revert, ..Default::default() },
            ..Default::default()
        };
        let interp =
            run_hooked(RunConfig::new(call_code(HOOKED_CALL, 0)).host(&mut host).gas_limit(30_000));

        assert_matches!(interp.err, InstrStop::Revert);
        assert!(interp.stack().is_empty());
        assert_eq!(host.calls.len(), 1);
    }

    #[test]
    fn create_with_hooks_adjust_child_and_caller_gas() {
        let created = Address::repeat_byte(0x77);
        let execute_result = MessageResultExt {
            stop: InstrStop::Return,
            created_address: Some(created),
            ..MessageResultExt::default()
        };
        let mut code = create_code();
        let mut base_code = code.clone();
        base_code.extend([op::CREATE, op::STOP]);
        code.extend([HOOKED_CREATE, op::STOP]);

        let mut base_host = TestHost {
            execute_result: execute_result.clone(),
            is_empty: true,
            ..Default::default()
        };
        let base = run_hooked(RunConfig::new(base_code).host(&mut base_host).gas_limit(500_000));
        let mut host = TestHost { execute_result, is_empty: true, ..Default::default() };
        let hooked = run_hooked(RunConfig::new(code).host(&mut host).gas_limit(500_000));

        assert_matches!(hooked.err, InstrStop::Stop);
        assert_eq!(hooked.stack(), [address_to_word(&created)]);
        assert_eq!(host.calls[0].gas_limit, base_host.calls[0].gas_limit - KEPT_GAS);
        assert_eq!(hooked.gas.state_gas_spent(), base.gas.state_gas_spent());
        assert_eq!(hooked.gas_remaining(), base.gas_remaining() + KEPT_GAS - AFTER_FEE);
    }

    #[test]
    fn create_with_skips_hooks_when_creation_is_refused_before_forward() {
        let mut code = create_code();
        let mut base_code = code.clone();
        base_code.extend([op::CREATE, op::STOP]);
        code.extend([HOOKED_CREATE, op::STOP]);
        let message = MessageExt { depth: CALL_DEPTH_LIMIT, ..Default::default() };

        let base =
            run_hooked(RunConfig::new(base_code).message(message.clone()).gas_limit(500_000));
        let mut host = TestHost::default();
        let hooked =
            run_hooked(RunConfig::new(code).host(&mut host).message(message).gas_limit(500_000));

        assert_matches!(hooked.err, InstrStop::Stop);
        assert_eq!(hooked.stack(), [Word::ZERO]);
        assert!(host.calls.is_empty());
        assert_eq!(hooked.gas_remaining(), base.gas_remaining());
    }
}
