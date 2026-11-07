use crate::{
    EvmTypesHost,
    interpreter::{InstrStop, InterpreterState, Result},
};

mod arithmetic;
pub use arithmetic::*;

mod bitwise;
pub use bitwise::*;

mod block;
pub use block::*;

mod control;
pub use control::*;

mod crypto;
pub use crypto::*;

mod env;
pub use env::*;

mod host;
pub use host::*;

mod memory;
pub use memory::*;

mod stack;
pub use stack::*;

mod system;
pub use system::*;

pub mod i256;

#[inline]
const fn require_non_staticcall<T: EvmTypesHost>(state: &InterpreterState<'_, '_, T>) -> Result {
    if state.is_static() {
        return Err(InstrStop::StateChangeDuringStaticCall);
    }
    Ok(())
}

#[cfg(test)]
mod macro_tests;
