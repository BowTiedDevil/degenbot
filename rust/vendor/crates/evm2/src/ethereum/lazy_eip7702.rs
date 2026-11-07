use crate::precompiles::secp256k1::ecrecover_bytes;
use alloc::vec::Vec;
use alloy_consensus::{Transaction, TxEip7702, TxType};
use alloy_eips::{
    Typed2718,
    eip2930::AccessList,
    eip7702::{Authorization, SignedAuthorization, constants::SECP256K1N_HALF},
};
use alloy_primitives::{Address, B256, Bytes, ChainId, TxKind, U256};

/// Borrowed EIP-7702 authorization of a [`LazyTxEip7702`], with its authority possibly recovered.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum LazyAuthorization<'a> {
    /// Signed authorization whose authority is recovered on demand.
    Signed(&'a SignedAuthorization),
    /// Signed authorization with a cached recovery result.
    Recovered {
        /// The signed authorization.
        authorization: &'a SignedAuthorization,
        /// The cached authority, or `None` if the signature is invalid.
        authority: Option<Address>,
    },
}

impl<'a> LazyAuthorization<'a> {
    /// Returns the signed authorization.
    const fn signed(&self) -> &'a SignedAuthorization {
        match *self {
            Self::Signed(authorization) | Self::Recovered { authorization, .. } => authorization,
        }
    }

    /// Returns the inner unsigned authorization.
    pub const fn inner(&self) -> &'a Authorization {
        self.signed().inner()
    }

    /// Returns the authorization chain ID.
    pub const fn chain_id(&self) -> &'a U256 {
        self.inner().chain_id()
    }

    /// Returns the delegated address.
    pub const fn address(&self) -> &'a Address {
        self.inner().address()
    }

    /// Returns the authorization nonce.
    pub const fn nonce(&self) -> u64 {
        self.inner().nonce()
    }

    /// Returns the recovered authority address, if the authorization signature is valid.
    ///
    /// Signed authorizations perform recovery on demand. Recovered authorizations return their
    /// cached result without doing secp256k1 recovery.
    pub fn authority(&self) -> Option<Address> {
        match *self {
            Self::Signed(authorization) => recover_authority(authorization),
            Self::Recovered { authority, .. } => authority,
        }
    }
}

/// EIP-7702 transaction used by the executable Ethereum transaction model.
///
/// This preserves the consensus [`TxEip7702`] fields and optionally caches the recovered authority
/// of each authorization, so callers can supply EIP-7702 authority recovery results.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct LazyTxEip7702 {
    /// EIP-155 replay protection chain ID.
    pub chain_id: ChainId,
    /// Sender nonce.
    pub nonce: u64,
    /// Maximum gas that may be used by the transaction.
    pub gas_limit: u64,
    /// Maximum total fee per gas.
    pub max_fee_per_gas: u128,
    /// Maximum priority fee per gas.
    pub max_priority_fee_per_gas: u128,
    /// Message-call recipient.
    pub to: Address,
    /// Wei value transferred to the recipient.
    pub value: U256,
    /// EIP-2930 access list.
    pub access_list: AccessList,
    /// Transaction input calldata.
    pub input: Bytes,
    authorization_list: Vec<SignedAuthorization>,
    /// Recovered authority of each entry of `authorization_list`, or `None` if not recovered yet.
    /// An authority is `None` if its signature is invalid.
    authorities: Option<Vec<Option<Address>>>,
}

impl LazyTxEip7702 {
    /// Converts a consensus transaction while keeping signed authorizations unresolved.
    pub fn from_signed_authorizations(tx: TxEip7702) -> Self {
        Self::new(tx, None)
    }

    /// Converts a consensus transaction and eagerly recovers all authorization authorities.
    ///
    /// Invalid authorization signatures are cached as invalid authorities so execution skips them
    /// in the same way as a failed on-demand recovery.
    pub fn from_recovered_authorizations(tx: TxEip7702) -> Self {
        let authorities = tx.authorization_list.iter().map(recover_authority).collect();
        Self::new(tx, Some(authorities))
    }

    /// Converts a consensus transaction using externally supplied recovered authorities, one per
    /// authorization of `tx`; `None` marks an invalid signature.
    ///
    /// The caller is responsible for ensuring that each authority matches the signature of its
    /// authorization.
    ///
    /// # Panics
    ///
    /// Panics if `authorities` and the authorization list of `tx` differ in length.
    pub fn from_cached_recovered_authorizations(
        tx: TxEip7702,
        authorities: Vec<Option<Address>>,
    ) -> Self {
        assert_eq!(
            tx.authorization_list.len(),
            authorities.len(),
            "one recovered authority per authorization"
        );
        Self::new(tx, Some(authorities))
    }

    fn new(tx: TxEip7702, authorities: Option<Vec<Option<Address>>>) -> Self {
        let TxEip7702 {
            chain_id,
            nonce,
            gas_limit,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            to,
            value,
            access_list,
            authorization_list,
            input,
        } = tx;
        Self {
            chain_id,
            nonce,
            gas_limit,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            to,
            value,
            access_list,
            input,
            authorization_list,
            authorities,
        }
    }

    /// Returns the authorizations, each with its cached authority if recovered.
    pub fn authorizations(&self) -> impl ExactSizeIterator<Item = LazyAuthorization<'_>> {
        let authorities = self.authorities.as_deref();
        self.authorization_list.iter().enumerate().map(
            move |(i, authorization)| match authorities {
                Some(authorities) => {
                    LazyAuthorization::Recovered { authorization, authority: authorities[i] }
                }
                None => LazyAuthorization::Signed(authorization),
            },
        )
    }

    /// Calculates a heuristic for the in-memory size of the transaction.
    #[inline]
    pub fn size(&self) -> usize {
        size_of::<Self>()
            + self.access_list.size()
            + self.input.len()
            + self.authorization_list.capacity() * size_of::<SignedAuthorization>()
            + self.authorities.as_ref().map_or(0, |a| a.capacity() * size_of::<Option<Address>>())
    }
}

impl From<TxEip7702> for LazyTxEip7702 {
    fn from(value: TxEip7702) -> Self {
        Self::from_signed_authorizations(value)
    }
}

impl Typed2718 for LazyTxEip7702 {
    fn ty(&self) -> u8 {
        TxType::Eip7702 as u8
    }
}

impl Transaction for LazyTxEip7702 {
    fn chain_id(&self) -> Option<ChainId> {
        Some(self.chain_id)
    }

    fn nonce(&self) -> u64 {
        self.nonce
    }

    fn gas_limit(&self) -> u64 {
        self.gas_limit
    }

    fn gas_price(&self) -> Option<u128> {
        None
    }

    fn max_fee_per_gas(&self) -> u128 {
        self.max_fee_per_gas
    }

    fn max_priority_fee_per_gas(&self) -> Option<u128> {
        Some(self.max_priority_fee_per_gas)
    }

    fn max_fee_per_blob_gas(&self) -> Option<u128> {
        None
    }

    fn priority_fee_or_price(&self) -> u128 {
        self.max_priority_fee_per_gas
    }

    fn effective_gas_price(&self, base_fee: Option<u64>) -> u128 {
        alloy_eips::eip1559::calc_effective_gas_price(
            self.max_fee_per_gas,
            self.max_priority_fee_per_gas,
            base_fee,
        )
    }

    fn is_dynamic_fee(&self) -> bool {
        true
    }

    fn kind(&self) -> TxKind {
        TxKind::Call(self.to)
    }

    fn is_create(&self) -> bool {
        false
    }

    fn value(&self) -> U256 {
        self.value
    }

    fn input(&self) -> &Bytes {
        &self.input
    }

    fn access_list(&self) -> Option<&AccessList> {
        Some(&self.access_list)
    }

    fn blob_versioned_hashes(&self) -> Option<&[B256]> {
        None
    }

    fn authorization_list(&self) -> Option<&[SignedAuthorization]> {
        Some(&self.authorization_list)
    }
}

/// Recovers the authority of a signed authorization with the same secp256k1 backend as the
/// ecrecover precompile, instead of whichever backend alloy-primitives was built with.
///
/// Keeps the checks of [`SignedAuthorization::recover_authority`]: the y-parity must be 0 or 1,
/// `s` must not exceed secp256k1n/2, and the backend rejects a zero or out-of-range `r` or `s`.
fn recover_authority(authorization: &SignedAuthorization) -> Option<Address> {
    let signature = authorization.signature().ok()?;
    if signature.s() > SECP256K1N_HALF {
        return None;
    }
    let rsy = signature.as_rsy();
    let hash = ecrecover_bytes(rsy.first_chunk()?, rsy[64], &authorization.signature_hash())?;
    Some(Address::from_word(hash.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use k256::ecdsa::SigningKey;

    #[test]
    fn authority_matches_alloy_recovery() {
        let key = SigningKey::from_slice(&[0x11; 32]).unwrap();
        let expected =
            Address::from_raw_public_key(&key.verifying_key().to_sec1_point(false).as_bytes()[1..]);
        let inner =
            Authorization { chain_id: U256::ONE, address: Address::with_last_byte(0xbb), nonce: 7 };
        let (sig, recid) = key.sign_prehash_recoverable(inner.signature_hash().as_slice());
        let r = U256::from_be_slice(&sig.r().to_bytes());
        let s = U256::from_be_slice(&sig.s().to_bytes());
        let y = recid.to_byte();
        let n = SECP256K1N_HALF * U256::from(2) + U256::ONE;
        let signed = |y, r, s| SignedAuthorization::new_unchecked(inner.clone(), y, r, s);

        let valid = signed(y, r, s);
        assert_eq!(LazyAuthorization::Signed(&valid).authority(), Some(expected));
        // Without the low-s check, the high-s signature would recover `expected` too.
        let authorization_list = vec![
            valid,
            signed(y ^ 1, r, n - s),
            signed(2, r, s),
            signed(y, U256::ZERO, s),
            signed(y, n, s),
            signed(y, r, U256::ZERO),
        ];
        let tx = TxEip7702 { authorization_list, ..Default::default() };
        let signed = LazyTxEip7702::from_signed_authorizations(tx.clone());
        let recovered = LazyTxEip7702::from_recovered_authorizations(tx.clone());
        let entries = signed.authorizations().zip(recovered.authorizations());
        for (i, (authorization, (signed, recovered))) in
            tx.authorization_list.iter().zip(entries).enumerate()
        {
            let want = authorization.recover_authority().ok();
            assert_eq!(want.is_some(), i == 0, "case {i}");
            assert_eq!(signed, LazyAuthorization::Signed(authorization), "signed entry {i}");
            assert_eq!(signed.authority(), want, "signed case {i}");
            assert_eq!(
                recovered,
                LazyAuthorization::Recovered { authorization, authority: want },
                "recovered entry {i}"
            );
        }
    }
}
