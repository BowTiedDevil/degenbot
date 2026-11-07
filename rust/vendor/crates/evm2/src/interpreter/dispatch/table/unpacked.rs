use super::Cursor as _;
use crate::{
    EvmConfig, EvmTypesHost,
    interpreter::{InterpreterState, Pc, Stack, StackBacking, gas::Gas},
};

pub(super) type LoopState = ();

core::cfg_select! {
    all(target_family = "wasm", target_pointer_width = "32") => {
        pub(super) type Cursor = WasmCursor;

        /// Program counter in the high 32 bits and stack length in the low 32 bits.
        ///
        /// Rust's wasm ABI returns a `(Pc, usize)` pair through a result pointer in linear memory:
        /// every handler takes an extra pointer argument and stores both values, and the dispatch
        /// loop loads them back. A single `u64` is returned as one wasm `i64` result instead.
        #[derive(Clone, Copy)]
        #[repr(transparent)]
        pub(crate) struct WasmCursor(u64);

        impl super::Cursor for WasmCursor {
            #[inline(always)]
            fn new(pc: Pc, stack_len: usize) -> Self {
                Self(((pc.as_ptr() as usize as u64) << 32) | stack_len as u64)
            }

            #[inline(always)]
            fn pc(self) -> Pc {
                Pc::new((self.0 >> 32) as usize as *const u8)
            }

            #[inline(always)]
            fn stack_len(self) -> usize {
                self.0 as u32 as usize
            }
        }
    }
    _ => {
        pub(super) type Cursor = (Pc, usize);
    }
}

/// Unpacked instruction return value.
type InstrFnRet = Cursor;

/// Unpacked instruction function pointer.
pub(in crate::interpreter::dispatch) type RawInstrFn<T> = extern_table!(
    fn(pc: Pc, stack: Stack<'_>, state: &mut InterpreterState<'_, '_, T>) -> InstrFnRet
);

#[inline(always)]
pub(super) fn dispatch_loop_call<T: EvmTypesHost>(
    instr: RawInstrFn<T>,
    cursor: Cursor,
    stack: &mut StackBacking,
    state: &mut InterpreterState<'_, '_, T>,
    _loop_state: &mut LoopState,
) -> Cursor {
    instr(cursor.pc(), Stack::new(stack, cursor.stack_len()), state)
}

#[inline(always)]
pub(super) const fn loop_state(_gas: &Gas) -> LoopState {}

#[inline(always)]
pub(super) const fn finish_loop(_gas: &mut Gas, _loop_state: LoopState) {}

#[inline(always)]
pub(super) const fn sync_loop_state<T: EvmTypesHost>(
    _state: &mut InterpreterState<'_, '_, T>,
    _loop_state: LoopState,
) {
}

extern_table! {
    pub(in crate::interpreter::dispatch) fn dispatch<
        T: EvmTypesHost,
        C: EvmConfig<T>,
        M: super::InspectMode<T>,
        const OP: u8,
    >(
        pc: Pc,
        mut stack: Stack<'_>,
        state: &mut InterpreterState<'_, '_, T>,
    ) -> InstrFnRet {
        let (pc, ()) = super::dispatch_inner::<T, C, M, ()>(pc, stack.as_mut(), (), state, OP);
        Cursor::new(pc, stack.len)
    }
}
