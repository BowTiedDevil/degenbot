use super::Cursor as _;
use crate::{
    EvmConfig, EvmTypesHost,
    constants::STACK_LIMIT,
    interpreter::{
        InterpreterState, Pc, Result, Stack, StackBacking,
        gas::{Gas, RemainingGas},
    },
};

pub(super) type LoopState = RemainingGas;

pub(super) type Cursor = PackedPc;

/// Packed instruction return value.
pub(crate) type InstrFnRet = (PackedPc, RemainingGas);

/// Packed instruction function pointer.
pub(in crate::interpreter::dispatch) type RawInstrFn<T> = extern_table!(
    fn(
        pc: PackedPc,
        stack: &mut StackBacking,
        remaining_gas: RemainingGas,
        state: &mut InterpreterState<'_, '_, T>,
    ) -> InstrFnRet
);

#[inline(always)]
pub(super) fn dispatch_loop_call<T: EvmTypesHost>(
    instr: RawInstrFn<T>,
    cursor: Cursor,
    stack: &mut StackBacking,
    state: &mut InterpreterState<'_, '_, T>,
    remaining_gas: &mut LoopState,
) -> Cursor {
    let (cursor, gas) = instr(cursor, stack, *remaining_gas, state);
    *remaining_gas = gas;
    cursor
}

#[inline(always)]
pub(super) const fn loop_state(gas: &Gas) -> LoopState {
    RemainingGas::new(gas.remaining())
}

#[inline(always)]
pub(super) const fn finish_loop(gas: &mut Gas, remaining_gas: LoopState) {
    gas.set_remaining(remaining_gas.get());
}

#[inline(always)]
pub(super) const fn sync_loop_state<T: EvmTypesHost>(
    state: &mut InterpreterState<'_, '_, T>,
    loop_state: LoopState,
) {
    state.gas_mut().set_remaining(loop_state.get());
}

impl super::DispatchGas for RemainingGas {
    #[inline(always)]
    fn pre_step<T: EvmTypesHost, C: EvmConfig<T>>(
        &mut self,
        _state: &mut InterpreterState<'_, '_, T>,
        op: u8,
    ) -> Result {
        self.spend(C::OPCODE_CONFIG.static_gas(op) as _)
    }

    #[inline(always)]
    fn sync_before_exec<T: EvmTypesHost>(
        &self,
        state: &mut InterpreterState<'_, '_, T>,
        dynamic_gas: bool,
    ) {
        if dynamic_gas {
            sync_loop_state(state, *self);
        }
    }

    #[inline(always)]
    fn sync_after_exec<T: EvmTypesHost>(
        &mut self,
        state: &mut InterpreterState<'_, '_, T>,
        dynamic_gas: bool,
    ) {
        if dynamic_gas {
            self.set(state.gas_mut().remaining());
        }
    }
}

extern_table! {
    pub(in crate::interpreter::dispatch) fn dispatch<
        T: EvmTypesHost,
        C: EvmConfig<T>,
        M: super::InspectMode<T>,
        const OP: u8,
    >(
        packed: PackedPc,
        stack: &mut StackBacking,
        remaining_gas: RemainingGas,
        state: &mut InterpreterState<'_, '_, T>,
    ) -> InstrFnRet {
        let (pc, stack_len) = (packed.pc(), packed.stack_len());
        let mut stack = Stack::new(stack, stack_len);
        let (next_pc, remaining_gas) =
            super::dispatch_inner::<T, C, M, RemainingGas>(
                pc,
                stack.as_mut(),
                remaining_gas,
                state,
                OP,
            );
        if next_pc.as_ptr().is_null() {
            return (PackedPc(stack.len), remaining_gas);
        }
        (packed.update(pc, next_pc, stack_len, stack.len), remaining_gas)
    }
}

const STACK_LEN_BITS: u32 = STACK_LIMIT.ilog2() + 1;
const STACK_LEN_MASK: usize = (1 << STACK_LEN_BITS) - 1;

const _: () = assert!(usize::BITS == 64);
const _: () = assert!(STACK_LIMIT < (1 << STACK_LEN_BITS));

/// Program counter shifted left, with the stack length packed into its low bits.
///
/// Handlers take and return this value, so one that only advances the program counter and changes
/// the stack length by constants returns `packed + constant`.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub(crate) struct PackedPc(usize);

impl super::Cursor for PackedPc {
    #[inline(always)]
    fn new(pc: Pc, stack_len: usize) -> Self {
        Self(((pc.as_ptr() as usize) << STACK_LEN_BITS) | stack_len)
    }

    #[inline(always)]
    fn pc(self) -> Pc {
        Pc::new((self.0 >> STACK_LEN_BITS) as *const u8)
    }

    #[inline(always)]
    fn stack_len(self) -> usize {
        self.0 & STACK_LEN_MASK
    }
}

impl PackedPc {
    /// Returns `Self::new(next_pc, next_stack_len)` given that `self` packs `pc` and `stack_len`.
    ///
    /// Computed as a difference so that constant changes fold into one addition.
    #[inline(always)]
    fn update(self, pc: Pc, next_pc: Pc, stack_len: usize, next_stack_len: usize) -> Self {
        let pc_delta = (next_pc.as_ptr() as usize).wrapping_sub(pc.as_ptr() as usize);
        let len_delta = next_stack_len.wrapping_sub(stack_len);
        Self(self.0.wrapping_add(pc_delta << STACK_LEN_BITS).wrapping_add(len_delta))
    }
}
