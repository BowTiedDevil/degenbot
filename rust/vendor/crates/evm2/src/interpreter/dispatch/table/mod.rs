use super::{DynInspector, InspectMode, NoInspector, inc_pc, run_state};
use crate::{
    EvmConfig, EvmTypesHost, OpcodeSet,
    interpreter::{
        InstrStop, Interpreter, InterpreterState, Pc, RawStack, Result, StackMut,
        private::InstructionFlags,
    },
};
use core::hint::cold_path;

core::cfg_select! {
    dispatch_packed => {
        mod packed;
        use packed as imp;
    }
    _ => {
        mod unpacked;
        use unpacked as imp;
    }
}

pub(super) use imp::{RawInstrFn, dispatch};

/// Table instruction dispatch table.
pub(super) type RawInstrTable<T> = [RawInstrFn<T>; 256];

/// Program counter and stack length carried between instructions by the dispatch loop.
trait Cursor: Copy {
    fn new(pc: Pc, stack_len: usize) -> Self;

    fn pc(self) -> Pc;

    fn stack_len(self) -> usize;
}

impl Cursor for (Pc, usize) {
    #[inline(always)]
    fn new(pc: Pc, stack_len: usize) -> Self {
        (pc, stack_len)
    }

    #[inline(always)]
    fn pc(self) -> Pc {
        self.0
    }

    #[inline(always)]
    fn stack_len(self) -> usize {
        self.1
    }
}

trait DispatchGas: Copy {
    fn pre_step<T: EvmTypesHost, C: EvmConfig<T>>(
        &mut self,
        state: &mut InterpreterState<'_, '_, T>,
        op: u8,
    ) -> Result;

    fn sync_before_exec<T: EvmTypesHost>(
        &self,
        state: &mut InterpreterState<'_, '_, T>,
        dynamic_gas: bool,
    );

    fn sync_after_exec<T: EvmTypesHost>(
        &mut self,
        state: &mut InterpreterState<'_, '_, T>,
        dynamic_gas: bool,
    );
}

impl DispatchGas for () {
    #[inline(always)]
    fn pre_step<T: EvmTypesHost, C: EvmConfig<T>>(
        &mut self,
        state: &mut InterpreterState<'_, '_, T>,
        op: u8,
    ) -> Result {
        state.gas_mut().spend(C::OPCODE_CONFIG.static_gas(op) as _)
    }

    #[inline(always)]
    fn sync_before_exec<T: EvmTypesHost>(
        &self,
        _state: &mut InterpreterState<'_, '_, T>,
        _dynamic_gas: bool,
    ) {
    }

    #[inline(always)]
    fn sync_after_exec<T: EvmTypesHost>(
        &mut self,
        _state: &mut InterpreterState<'_, '_, T>,
        _dynamic_gas: bool,
    ) {
    }
}

#[cold] // Not cold, but avoids MIR inlining.
#[inline(always)]
fn dispatch_inner<T: EvmTypesHost, C: EvmConfig<T>, M: InspectMode<T>, G: DispatchGas>(
    mut pc: Pc,
    mut stack: StackMut<'_>,
    mut gas: G,
    state: &mut InterpreterState<'_, '_, T>,
    op: u8,
) -> (Pc, G) {
    let instruction = C::OPCODE_CONFIG.instruction(op);
    let instr = instruction.instr;
    let dynamic_gas = instruction.flags.contains(InstructionFlags::DYNAMIC_GAS);
    let hooked = instruction.flags.contains(InstructionFlags::HOOKED);
    let r;
    match gas.pre_step::<T, C>(state, op) {
        Ok(()) => {
            gas.sync_before_exec(state, dynamic_gas);
            if hooked {
                state.set_pc_stack_len(pc.as_ptr(), *stack.len);
                state.set_hooked_running(true);
            }
            r = instr(&mut pc, stack.reborrow(), state);
            if hooked {
                state.set_hooked_running(false);
            }
            if r.is_ok() {
                inc_pc(&mut pc, op);
            }
            gas.sync_after_exec(state, dynamic_gas);
        }
        Err(e) => {
            gas.sync_before_exec(state, false);
            r = Err(e);
        }
    }
    if M::INSPECT {
        state.set_result(r);
    } else if let Err(e) = r {
        state.set_result(Err(e));
        cold_path();
        return (Pc::new(core::ptr::null()), gas);
    }
    (pc, gas)
}

#[inline(always)]
pub(in crate::interpreter) fn run<T: EvmTypesHost>(
    interpreter: &mut Interpreter<'_, '_, T>,
    instructions: &RawInstrTable<T>,
) -> InstrStop {
    let (state, pc, stack) = run_state(interpreter);
    // An inspector that steps nothing runs the plain loop.
    if state.is_inspecting() && state.steps() != OpcodeSet::EMPTY {
        if state.steps() == OpcodeSet::ALL {
            return run_inner::<T, DynInspector<true>>(state, pc, stack, instructions);
        }
        return run_inner::<T, DynInspector<false>>(state, pc, stack, instructions);
    }
    run_inner::<T, NoInspector>(state, pc, stack, instructions)
}

#[allow(clippy::let_unit_value)]
fn run_inner<T: EvmTypesHost, M: InspectMode<T>>(
    state: &mut InterpreterState<'_, '_, T>,
    pc: Pc,
    mut stack: RawStack<'_>,
    instructions: &RawInstrTable<T>,
) -> InstrStop {
    let mut loop_state = imp::loop_state(state.gas_mut());
    let mut cursor = imp::Cursor::new(pc, stack.len());
    let steps = if M::INSPECT { state.steps() } else { OpcodeSet::EMPTY };
    loop {
        let op = cursor.pc().op();
        // A sparse set's table holds inspect handlers at the stepped opcodes and plain handlers
        // elsewhere, so each path calls the handlers it matches.
        if M::INSPECT && (M::ALL_STEPS || steps.contains(op)) {
            let stepped = if M::ALL_STEPS {
                run_stepped::<T, M>(state, cursor, stack, instructions[op as usize], loop_state)
            } else {
                run_stepped_out_of_line::<T, M>(
                    state,
                    cursor,
                    stack,
                    instructions[op as usize],
                    loop_state,
                )
            };
            match stepped {
                Ok(next) => (cursor, loop_state) = next,
                Err(stop) => return stop,
            }
        } else {
            cursor = {
                // SAFETY: This is the only stack borrow outside inspector callbacks.
                let stack_view = unsafe { stack.borrow() };
                imp::dispatch_loop_call(
                    instructions[op as usize],
                    cursor,
                    stack_view.stack,
                    state,
                    &mut loop_state,
                )
            };
            if cursor.pc().as_ptr().is_null() {
                return finish_run(state, cursor, loop_state);
            }
        }
    }
}

/// Runs one instruction between its step hooks, or returns the stop that ended the frame.
///
/// The hooks run here, outside the stack borrow, because a hook may access the stack through the
/// interpreter and no handler may hold the borrow meanwhile.
#[inline(always)]
fn run_stepped<T: EvmTypesHost, M: InspectMode<T>>(
    state: &mut InterpreterState<'_, '_, T>,
    mut cursor: imp::Cursor,
    mut stack: RawStack<'_>,
    instr: imp::RawInstrFn<T>,
    mut loop_state: imp::LoopState,
) -> core::result::Result<(imp::Cursor, imp::LoopState), InstrStop> {
    imp::sync_loop_state(state, loop_state);
    M::step(state, cursor.pc(), cursor.stack_len());
    loop_state = imp::loop_state(state.gas_mut());
    if state.result().is_err() {
        return Err(finish_run(state, cursor, loop_state));
    }

    cursor = {
        // SAFETY: This is the only stack borrow outside inspector callbacks.
        let stack_view = unsafe { stack.borrow() };
        imp::dispatch_loop_call(instr, cursor, stack_view.stack, state, &mut loop_state)
    };

    imp::sync_loop_state(state, loop_state);
    M::step_end(state, cursor.pc(), cursor.stack_len());
    loop_state = imp::loop_state(state.gas_mut());
    if state.result().is_err() {
        return Err(finish_run(state, cursor, loop_state));
    }
    Ok((cursor, loop_state))
}

/// [`run_stepped`] behind a call, so that the registers the loop holds for the unstepped
/// instructions of a sparse set are not shared with the hook calls.
#[inline(never)]
fn run_stepped_out_of_line<T: EvmTypesHost, M: InspectMode<T>>(
    state: &mut InterpreterState<'_, '_, T>,
    cursor: imp::Cursor,
    stack: RawStack<'_>,
    instr: imp::RawInstrFn<T>,
    loop_state: imp::LoopState,
) -> core::result::Result<(imp::Cursor, imp::LoopState), InstrStop> {
    run_stepped::<T, M>(state, cursor, stack, instr, loop_state)
}

#[inline(always)]
fn finish_run<T: EvmTypesHost>(
    state: &mut InterpreterState<'_, '_, T>,
    cursor: imp::Cursor,
    loop_state: imp::LoopState,
) -> InstrStop {
    cold_path();
    let (pc, stack_len) = (cursor.pc(), cursor.stack_len());
    // The NULL `pc` is only a loop-exit sentinel; never publish it.
    let pc = if pc.as_ptr().is_null() { state.0.pc } else { pc.as_ptr() };
    state.set_pc_stack_len(pc, stack_len);
    imp::finish_loop(state.gas_mut(), loop_state);
    state.result().unwrap_err()
}
