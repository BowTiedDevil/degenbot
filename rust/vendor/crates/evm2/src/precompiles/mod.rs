//! EVM precompiled contracts.

use crate::{
    Evm, EvmTypesHost, SpecId,
    evm::{PrewarmSet, precompile::PrecompileProvider},
    interpreter::{GasTracker, Message},
};
#[cfg(feature = "std")]
use alloc::boxed::Box;
use alloc::{borrow::Cow, vec::Vec};
use alloy_primitives::Address;
#[cfg(feature = "std")]
use core::any::{Any, TypeId};

pub mod blake2;
pub mod bls12_381;
pub mod bls12_381_const;
pub mod bls12_381_utils;
pub mod bn254;
mod crypto;
pub use crypto::{Crypto, DefaultCrypto, crypto, install_crypto};

pub mod hash;
pub mod identity;
pub mod kzg_point_evaluation;
pub mod modexp;
pub mod secp256k1;
pub mod secp256r1;

mod id;
pub use id::PrecompileId;

mod table;
pub use table::*;

/// EIP-7823 constants.
pub(crate) mod eip7823 {
    /// Each of the modexp length inputs must be less than or equal to 1024 bytes.
    pub(crate) const INPUT_SIZE_LIMIT: usize = 1024;
}

mod result;
pub use result::{PrecompileError, PrecompileHalt, PrecompileResult};

pub(crate) use crate::{
    evm::precompile::PrecompileOutput,
    once_lock::OnceLock,
    utils::{calc_linear_cost, u64_to_address},
};

// Silence backend dependency lints when another backend takes precedence.
core::cfg_select! {
    feature = "bn" => {
        use ark_bn254 as _;
        use ark_ec as _;
        use ark_ff as _;
        use ark_serialize as _;
        use bn as _;
    }
    _ => {}
}

use arrayref as _;

// silence arkworks-bls12-381 lint as blst will be used as default if both are enabled.
core::cfg_select! {
    feature = "blst" => {
        use ark_bls12_381 as _;
        use ark_ec as _;
        use ark_ff as _;
        use ark_serialize as _;
    }
    _ => {}
}

// silence aurora-engine-modexp if gmp is enabled
#[cfg(feature = "gmp")]
use aurora_engine_modexp as _;

// silence p256 lint as aws-lc-rs will be used if both are enabled.
#[cfg(feature = "p256-aws-lc-rs")]
use p256 as _;

/// Default precompile provider.
#[derive(Clone, Debug)]
pub struct Precompiles<T: EvmTypesHost = crate::BaseEvmTypes> {
    map: Cow<'static, PrecompileMap<T>>,
}

impl<T: EvmTypesHost> Precompiles<T> {
    /// Creates a precompile provider from a static precompile map.
    #[inline]
    pub const fn new(map: Cow<'static, PrecompileMap<T>>) -> Self {
        Self { map }
    }

    /// Creates a precompile provider for a base EVM specification.
    #[inline]
    pub fn base(spec_id: SpecId) -> Self {
        #[cfg(feature = "std")]
        {
            Self::new(Cow::Borrowed(cached_base_precompiles(spec_id)))
        }

        #[cfg(not(feature = "std"))]
        {
            Self::new(Cow::Owned(base_precompiles(spec_id)))
        }
    }

    /// Creates a precompile map from precompile descriptors.
    #[inline]
    pub fn map(precompiles: impl IntoIterator<Item = Precompile<T>>) -> PrecompileMap<T> {
        PrecompileMap::from_precompiles(precompiles)
    }

    /// Returns the underlying precompile map.
    #[inline]
    pub fn as_map(&self) -> &PrecompileMap<T> {
        self.map.as_ref()
    }

    /// Returns the underlying precompile map mutably.
    #[inline]
    pub fn as_map_mut(&mut self) -> &mut PrecompileMap<T> {
        self.map.to_mut()
    }
}

impl<T: EvmTypesHost> PrecompileProvider<T> for Precompiles<T> {
    #[inline]
    fn addresses(&self) -> Vec<Address> {
        self.map.as_ref().addresses().collect()
    }

    #[inline]
    fn warm_addresses(&self, warm: &mut PrewarmSet) {
        self.map.as_ref().warm_addresses(warm);
    }

    #[inline]
    fn precompile_ids(&self) -> Vec<(Address, PrecompileId)> {
        self.map
            .as_ref()
            .addresses()
            .filter_map(|address| {
                Some((address, self.map.as_ref().get_data(&address)?.id().clone()))
            })
            .collect()
    }

    #[inline]
    fn contains(&self, address: &Address) -> bool {
        self.map.as_ref().contains(address)
    }

    fn move_precompiles(
        &mut self,
        moves: &[(Address, Address)],
    ) -> Result<(), MovePrecompileError> {
        self.as_map_mut().move_precompiles(moves.iter().copied())
    }

    #[inline]
    fn execute(
        &mut self,
        evm: &mut Evm<'_, T>,
        message: &Message<T>,
        gas: &mut GasTracker,
    ) -> Option<PrecompileResult> {
        let precompile = self.map.as_ref().get_data(&message.call_target)?;
        Some(precompile.execute(evm, message, gas))
    }
}

#[cfg(feature = "std")]
type BasePrecompileCache<T> = [OnceLock<PrecompileMap<T>>; 7];

/// Append-only list of the [`BasePrecompileCache`] of each host type, starting at
/// [`BASE_PRECOMPILES`].
///
/// There are only a handful of host types per process, so a linear walk is cheap. Nodes are never
/// removed or replaced, and each link is set once, so readers only load: unlike a read lock, a hit
/// writes no shared cache line.
#[cfg(feature = "std")]
struct BasePrecompileNode {
    type_id: TypeId,
    cache: Box<dyn Any + Send + Sync>,
    next: OnceLock<Box<Self>>,
}

#[cfg(feature = "std")]
impl BasePrecompileNode {
    fn new<T: EvmTypesHost>() -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            cache: Box::new(BasePrecompileCache::<T>::default()),
            next: OnceLock::new(),
        }
    }
}

#[cfg(feature = "std")]
static BASE_PRECOMPILES: OnceLock<BasePrecompileNode> = OnceLock::new();

#[cfg(feature = "std")]
#[inline]
fn cached_base_precompiles<T: EvmTypesHost>(spec: SpecId) -> &'static PrecompileMap<T> {
    let index = match spec {
        SpecId::FRONTIER | SpecId::HOMESTEAD | SpecId::TANGERINE | SpecId::SPURIOUS_DRAGON => 0,
        SpecId::BYZANTIUM | SpecId::PETERSBURG => 1,
        SpecId::ISTANBUL => 2,
        SpecId::BERLIN | SpecId::LONDON | SpecId::MERGE | SpecId::SHANGHAI => 3,
        SpecId::CANCUN => 4,
        SpecId::PRAGUE => 5,
        SpecId::OSAKA | SpecId::AMSTERDAM => 6,
    };

    shared_base_precompiles::<T>()
        .downcast_ref::<BasePrecompileCache<T>>()
        .expect("base precompile cache type mismatch")[index]
        .get_or_init(|| base_precompiles::<T>(spec))
}

/// Returns the cache of `T`, appending it to [`BASE_PRECOMPILES`] on first use.
#[cfg(feature = "std")]
fn shared_base_precompiles<T: EvmTypesHost>() -> &'static (dyn Any + Send + Sync) {
    let type_id = TypeId::of::<T>();
    // Only an empty link is initialized, and racing creators agree on the winner's node.
    let mut node = BASE_PRECOMPILES.get_or_init(BasePrecompileNode::new::<T>);
    while node.type_id != type_id {
        node = node.next.get_or_init(|| Box::new(BasePrecompileNode::new::<T>()));
    }
    &*node.cache
}

fn base_precompiles<T: EvmTypesHost>(spec: SpecId) -> PrecompileMap<T> {
    let mut precompiles = PrecompileMap::with_capacity(32);

    {
        precompiles.extend([SECP256K1_ECRECOVER(), SHA256(), RIPEMD160(), IDENTITY()]);
    }

    if spec.enables(SpecId::BYZANTIUM) {
        precompiles.extend([
            MODEXP_BYZANTIUM(),
            BN254_ADD_BYZANTIUM(),
            BN254_MUL_BYZANTIUM(),
            BN254_PAIR_BYZANTIUM(),
        ]);
    }

    if spec.enables(SpecId::ISTANBUL) {
        precompiles.extend([
            BN254_ADD_ISTANBUL(),
            BN254_MUL_ISTANBUL(),
            BN254_PAIR_ISTANBUL(),
            BLAKE2F(),
        ]);
    }

    if spec.enables(SpecId::BERLIN) {
        precompiles.extend([MODEXP_BERLIN()]);
    }

    if spec.enables(SpecId::CANCUN) {
        precompiles.extend([KZG_POINT_EVALUATION()]);
    }

    if spec.enables(SpecId::PRAGUE) {
        precompiles.extend([
            BLS12_381_G1_ADD(),
            BLS12_381_G1_MSM(),
            BLS12_381_G2_ADD(),
            BLS12_381_G2_MSM(),
            BLS12_381_PAIRING(),
            BLS12_381_MAP_FP_TO_G1(),
            BLS12_381_MAP_FP2_TO_G2(),
        ]);
    }

    if spec.enables(SpecId::OSAKA) {
        precompiles.extend([MODEXP_OSAKA(), P256VERIFY_OSAKA()]);
    }

    precompiles.shrink_to_fit();

    precompiles
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    #[test]
    fn provider_helpers_mutate_base_map() {
        let identity = IDENTITY::<crate::BaseEvmTypes>().address();
        let moved = address!("0x0000000000000000000000000000000000001000");
        let mut precompiles = Precompiles::<crate::BaseEvmTypes>::base(SpecId::BERLIN);

        precompiles.as_map_mut().move_precompiles([(identity, moved)]).unwrap();

        assert!(!precompiles.as_map().contains(&identity));
        assert!(precompiles.as_map().contains(&moved));
        assert!(
            Precompiles::<crate::BaseEvmTypes>::base(SpecId::BERLIN).as_map().contains(&identity)
        );
    }
}
