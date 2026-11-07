use alloy_primitives::{Address, U256};
use core::{hint::cold_path, num::NonZero};
use evm2::{EvmTypesHost, evm::AccountLoad, interpreter::Host};
use evm2_jit_context::{EvmContext, EvmWord, InstrStop};

pub type BuiltinResult = Result<(), BuiltinError>;

/// Represents an error that occurred during a builtin execution.
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct BuiltinError(NonZero<u8>);

impl From<BuiltinError> for InstrStop {
    #[inline]
    fn from(value: BuiltinError) -> Self {
        // SAFETY: BuiltinError is always created from a valid InstrStop.
        unsafe { core::mem::transmute::<_, _>(value.0.get()) }
    }
}

impl From<InstrStop> for BuiltinError {
    #[inline]
    fn from(value: InstrStop) -> Self {
        cold_path();
        Self(unsafe { NonZero::new_unchecked(value as u8) })
    }
}

#[inline]
pub(crate) fn require_non_staticcall<T: EvmTypesHost>(
    ecx: &EvmContext<'_, '_, '_, T>,
) -> BuiltinResult {
    if ecx.is_static() {
        return Err(InstrStop::StateChangeDuringStaticCall.into());
    }
    Ok(())
}

// JIT code writes stack words with 8-byte stores, so read them back with scalar loads. The
// interpreter's helpers use one 32-byte load, which can't take its data from those stores.

/// Converts a word to `usize`, returning an invalid-operand OOG stop on overflow.
#[inline]
pub(crate) fn word_to_usize(value: U256) -> Result<usize, InstrStop> {
    value.try_into().map_err(|_| {
        cold_path();
        InstrStop::InvalidOperandOOG
    })
}

/// Loads an account, handling cold load gas accounting.
///
/// Pre-Berlin, `cold_account_additional_cost` is 0, so the cold load logic is a no-op.
pub(crate) fn load_account<T: EvmTypesHost>(
    ecx: &mut EvmContext<'_, '_, '_, T>,
    address: &Address,
    load_code: bool,
) -> Result<AccountLoad, BuiltinError> {
    let cold_load_gas = ecx.gas_params().cold_account_additional_cost();
    let skip_cold_load = ecx.gas.remaining() < cold_load_gas;
    let account = ecx
        .host()
        .load_account(address, load_code, skip_cold_load)
        .map_err(|stop| ecx.interpreter_mut().fail(stop))?;
    if account.is_cold {
        ecx.gas.spend(cold_load_gas)?;
    }
    Ok(account)
}

/// Splits the stack pointer into `N` elements by casting it to an array.
///
/// NOTE: this returns the arguments in **reverse order**. Use `rev!` to get them in order.
///
/// The returned lifetime is valid for the entire duration of the builtin.
///
/// # Safety
///
/// Caller must ensure that `N` matches the number of elements popped in JIT code.
#[inline(always)]
pub(crate) unsafe fn read_words_rev<'a, const N: usize>(sp: *mut EvmWord) -> &'a mut [EvmWord; N] {
    unsafe { &mut *sp.cast::<[EvmWord; N]>() }
}

#[inline]
pub(crate) fn ensure_memory<T: EvmTypesHost>(
    ecx: &mut EvmContext<'_, '_, '_, T>,
    offset: usize,
    len: usize,
) -> BuiltinResult {
    ecx.resize_memory(offset, len)?;
    Ok(())
}

pub(crate) unsafe fn copy_operation<T: EvmTypesHost>(
    ecx: &mut EvmContext<'_, '_, '_, T>,
    rev![memory_offset, data_offset, len]: &mut [EvmWord; 3],
    data: &[u8],
) -> BuiltinResult {
    let len = word_to_usize(len.to_u256())?;
    if len != 0 {
        ecx.gas.spend(ecx.gas_params().copy_cost(len))?;
        let memory_offset = word_to_usize(memory_offset.to_u256())?;
        ensure_memory(ecx, memory_offset, len)?;
        let data_offset = data_offset.to_u256().saturating_to::<usize>();
        ecx.memory_mut().set_data(memory_offset, data_offset, len, data);
    }
    Ok(())
}
