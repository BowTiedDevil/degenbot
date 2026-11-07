//! Precompile dispatch interface.

use super::{Evm, NonStaticAny, PrewarmSet};
use crate::{
    EvmTypesHost, PrecompileError,
    interpreter::{GasTracker, Message},
    precompiles::{MovePrecompileError, PrecompileId},
};
use alloc::{boxed::Box, vec::Vec};
use alloy_primitives::{Address, Bytes};
use auto_impl::auto_impl;

/// Result returned by a precompile.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct PrecompileOutput {
    /// Returned bytes.
    bytes: Bytes,
}

impl PrecompileOutput {
    /// Creates a new precompile output.
    #[inline]
    pub const fn new(bytes: Bytes) -> Self {
        Self { bytes }
    }

    /// Returns the output bytes.
    #[inline]
    pub fn bytes(&self) -> &[u8] {
        self.bytes.as_ref()
    }

    /// Consumes the output and returns its bytes.
    #[inline]
    pub fn into_bytes(self) -> Bytes {
        self.bytes
    }
}

/// Precompile execution hook.
#[auto_impl(&mut, Box)]
pub trait PrecompileProvider<T: EvmTypesHost>: NonStaticAny {
    /// Returns precompile addresses.
    fn addresses(&self) -> Vec<Address> {
        Vec::new()
    }

    /// Marks every precompile address as warm.
    ///
    /// Defaults to warming each of [`Self::addresses`]; providers should override it to avoid the
    /// allocation, since it runs once per transaction.
    fn warm_addresses(&self, warm: &mut PrewarmSet) {
        for address in self.addresses() {
            warm.warm(&address);
        }
    }

    /// Returns precompile addresses and identifiers.
    fn precompile_ids(&self) -> Vec<(Address, PrecompileId)>;

    /// Returns whether `address` has a registered precompile.
    fn contains(&self, address: &Address) -> bool;

    /// Relocates installed precompiles, validating all sources before changing the provider.
    ///
    /// Entries whose source and destination match are ignored. Wrappers must preserve the
    /// underlying provider and invalidate any address-dependent caches after a successful move.
    fn move_precompiles(&mut self, moves: &[(Address, Address)])
    -> Result<(), MovePrecompileError>;

    /// Executes the precompile at [`Message::call_target`], if one is registered.
    ///
    /// Dispatch uses the original callee even when its stored code is an EIP-7702 delegation.
    /// Returns `None` when the original callee has no registered precompile.
    fn execute(
        &mut self,
        evm: &mut Evm<'_, T>,
        message: &Message<T>,
        gas: &mut GasTracker,
    ) -> Option<Result<PrecompileOutput, PrecompileError>>;
}

#[inline]
pub(crate) fn boxed_precompile_provider<'a, T: EvmTypesHost>(
    precompiles: impl PrecompileProvider<T> + 'a,
) -> Box<dyn PrecompileProvider<T> + 'a> {
    Box::new(precompiles)
}

impl<'a, T: EvmTypesHost> core::ops::Deref for dyn PrecompileProvider<T> + 'a {
    type Target = dyn NonStaticAny + 'a;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self
    }
}

impl<'a, T: EvmTypesHost> core::ops::DerefMut for dyn PrecompileProvider<T> + 'a {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self
    }
}

/// Empty precompile provider.
#[allow(missing_copy_implementations)]
#[derive(Clone, Debug, Default)]
pub struct NoPrecompiles(());

impl<T: EvmTypesHost> PrecompileProvider<T> for NoPrecompiles {
    #[inline]
    fn addresses(&self) -> Vec<Address> {
        Vec::new()
    }

    #[inline]
    fn precompile_ids(&self) -> Vec<(Address, PrecompileId)> {
        Vec::new()
    }

    #[inline]
    fn contains(&self, _address: &Address) -> bool {
        false
    }

    fn move_precompiles(
        &mut self,
        moves: &[(Address, Address)],
    ) -> Result<(), MovePrecompileError> {
        match moves.iter().find(|(source, dest)| source != dest) {
            Some((source, _)) => Err(MovePrecompileError::NotAPrecompile(*source)),
            None => Ok(()),
        }
    }

    #[inline]
    fn execute(
        &mut self,
        _evm: &mut Evm<'_, T>,
        _message: &Message<T>,
        _gas: &mut GasTracker,
    ) -> Option<Result<PrecompileOutput, PrecompileError>> {
        None
    }
}
