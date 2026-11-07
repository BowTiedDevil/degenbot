//! EVM environment types.

use crate::{BaseEvmTypes, EvmTypesHost};
use alloc::{vec, vec::Vec};
use alloy_primitives::{Address, U256};

/// Transaction-global environment values visible to opcodes for an EVM type family.
pub type TxEnv<T = BaseEvmTypes> = TxEnvExt<<T as EvmTypesHost>::TxEnvExt>;

/// Transaction-global environment values visible to opcodes, parameterized by extension data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxEnvExt<E = ()> {
    /// Transaction origin.
    pub origin: Address,
    /// Effective gas price.
    pub gas_price: U256,
    /// Chain ID.
    pub chain_id: U256,
    /// Transaction blob versioned hashes.
    pub blob_hashes: Vec<U256>,
    /// EVM type-specific extension data.
    pub ext: E,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl<E> TxEnvExt<E> {
    /// Replaces the extension, preserving all other transaction environment fields.
    #[inline]
    pub fn with_ext<F>(self, ext: F) -> TxEnvExt<F> {
        self.map_ext(|_| ext)
    }

    /// Transforms the extension, preserving all other transaction environment fields.
    #[inline]
    pub fn map_ext<F>(self, f: impl FnOnce(E) -> F) -> TxEnvExt<F> {
        let Self { origin, gas_price, chain_id, blob_hashes, ext, _non_exhaustive } = self;
        TxEnvExt { origin, gas_price, chain_id, blob_hashes, ext: f(ext), _non_exhaustive }
    }
}

impl<E: Default> Default for TxEnvExt<E> {
    #[inline]
    fn default() -> Self {
        Self {
            origin: Address::ZERO,
            gas_price: U256::ZERO,
            chain_id: U256::ONE,
            blob_hashes: vec![],
            ext: E::default(),
            _non_exhaustive: (),
        }
    }
}

/// Block environment values visible to opcodes for an EVM type family.
pub type BlockEnv<T = BaseEvmTypes> = BlockEnvExt<<T as EvmTypesHost>::BlockEnvExt>;

/// Block environment values visible to opcodes, parameterized by extension data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockEnvExt<E = ()> {
    /// Block number.
    pub number: U256,
    /// Block beneficiary.
    pub beneficiary: Address,
    /// Block timestamp.
    pub timestamp: U256,
    /// Block gas limit.
    pub gas_limit: U256,
    /// Block base fee.
    pub basefee: U256,
    /// Pre-merge block difficulty.
    pub difficulty: U256,
    /// Post-merge randomness value.
    pub prevrandao: U256,
    /// Blob base fee.
    pub blob_basefee: U256,
    /// Beacon slot number.
    pub slot_num: U256,
    /// EVM type-specific extension data.
    pub ext: E,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl<E> BlockEnvExt<E> {
    /// Replaces the extension, preserving all other block environment fields.
    #[inline]
    pub fn with_ext<F>(self, ext: F) -> BlockEnvExt<F> {
        self.map_ext(|_| ext)
    }

    /// Transforms the extension, preserving all other block environment fields.
    #[inline]
    pub fn map_ext<F>(self, f: impl FnOnce(E) -> F) -> BlockEnvExt<F> {
        let Self {
            number,
            beneficiary,
            timestamp,
            gas_limit,
            basefee,
            difficulty,
            prevrandao,
            blob_basefee,
            slot_num,
            ext,
            _non_exhaustive,
        } = self;
        BlockEnvExt {
            number,
            beneficiary,
            timestamp,
            gas_limit,
            basefee,
            difficulty,
            prevrandao,
            blob_basefee,
            slot_num,
            ext: f(ext),
            _non_exhaustive,
        }
    }
}

impl<E: Default> Default for BlockEnvExt<E> {
    #[inline]
    fn default() -> Self {
        Self {
            number: U256::ZERO,
            beneficiary: Address::ZERO,
            timestamp: U256::ONE,
            gas_limit: U256::from_limbs([u64::MAX, 0, 0, 0]),
            basefee: U256::ZERO,
            difficulty: U256::ZERO,
            prevrandao: U256::ZERO,
            blob_basefee: U256::ONE,
            slot_num: U256::ZERO,
            ext: E::default(),
            _non_exhaustive: (),
        }
    }
}
