//! Shared, raw chain-specific account data.

use alloc::vec::Vec;
use alloy_primitives::Bytes;
use core::ops::Deref;
use triomphe::ThinArc;

/// Raw account payload with a one-pointer inline representation.
///
/// Empty payloads allocate nothing; nonempty payloads share one allocation containing
/// the length and bytes. Encoding into a trie leaf is the caller's responsibility.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountExtension(Option<ThinArc<(), u8>>);

impl AccountExtension {
    /// Creates an empty payload without allocating.
    pub const fn new() -> Self {
        Self(None)
    }

    /// Copies raw bytes into a shared allocation.
    pub fn copy_from_slice(bytes: &[u8]) -> Self {
        Self((!bytes.is_empty()).then(|| ThinArc::from_header_and_slice((), bytes)))
    }

    /// Takes ownership of a shared payload without copying its bytes.
    pub fn from_shared(payload: Option<ThinArc<(), u8>>) -> Self {
        Self(payload.filter(|arc| !arc.slice.is_empty()))
    }

    /// Transfers the shared allocation without copying its bytes.
    pub fn into_shared(self) -> Option<ThinArc<(), u8>> {
        self.0
    }

    /// Returns whether the payload is empty.
    pub const fn is_empty(&self) -> bool {
        self.0.is_none()
    }
}

impl AsRef<[u8]> for AccountExtension {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref().map_or(&[], |arc| &arc.slice)
    }
}

impl Deref for AccountExtension {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl From<Bytes> for AccountExtension {
    fn from(bytes: Bytes) -> Self {
        Self::copy_from_slice(&bytes)
    }
}

impl From<Vec<u8>> for AccountExtension {
    fn from(bytes: Vec<u8>) -> Self {
        Self::copy_from_slice(&bytes)
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for AccountExtension {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            alloy_primitives::hex::serialize(self.as_ref(), serializer)
        } else {
            serializer.serialize_bytes(self.as_ref())
        }
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for AccountExtension {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Bytes::deserialize(deserializer).map(Self::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        SpecId, Version,
        evm::{AccountInfo, BlockStateAccumulator, CacheDB, State, StateChangeSource},
    };
    use alloy_primitives::{Address, U256};

    #[test]
    fn shared_raw_payload() {
        assert_eq!(size_of::<AccountExtension>(), size_of::<usize>());
        assert!(AccountExtension::new().into_shared().is_none());
        let extension = AccountExtension::copy_from_slice(&[0x82; 32]);
        let shared = AccountExtension::from_shared(extension.clone().into_shared());
        assert_eq!(extension.as_ptr(), shared.as_ptr());
        assert_eq!(shared.as_ref(), &[0x82; 32]);
    }

    #[test]
    fn extension_changes_are_journaled_and_accumulated() {
        let address = Address::repeat_byte(1);
        let original = AccountInfo {
            extension: AccountExtension::copy_from_slice(&[1; 32]),
            ..Default::default()
        };
        let mut db = CacheDB::default();
        db.insert_account_info(&address, original.clone());
        let mut state = State::new(db);
        let version = Version::base(SpecId::CANCUN);
        let outer = state.checkpoint();
        let changed = AccountExtension::copy_from_slice(&[2; 32]);
        state.account(&address).unwrap().set_extension(changed.clone());
        let inner = state.checkpoint();
        state.account(&address).unwrap().set_extension(AccountExtension::new());
        state.rollback(inner, version.features);
        assert_eq!(state.account(&address).unwrap().get().unwrap().extension, changed);
        state.rollback(outer, version.features);
        assert_eq!(state.account(&address).unwrap().get(), Some(&original));

        state.account(&address).unwrap().set_extension(changed.clone());
        state.finalize_transaction(version).unwrap();
        let pending = state.take_pending_state();
        let mut block = BlockStateAccumulator::new();
        pending.visit(&mut block).unwrap();
        let (_, account) = block.accounts().next().unwrap();
        assert_eq!(account.original.as_ref().unwrap().extension, original.extension);
        assert_eq!(account.current.as_ref().unwrap().extension.as_ptr(), changed.as_ptr());
        state.set_pending_state(pending);
        state.commit_transaction();
        assert_eq!(state.account(&address).unwrap().get().unwrap().extension, changed);

        // Reverting the extension in a later transaction cancels the net block update.
        state.account(&address).unwrap().set_extension(original.extension);
        state.take_pending_state().visit(&mut block).unwrap();
        assert_eq!(block.accounts().count(), 0);
    }

    #[test]
    fn creation_preserves_extension_and_empty_cleanup_respects_it() {
        let address = Address::repeat_byte(2);
        let caller = Address::repeat_byte(3);
        let version = Version::base(SpecId::CANCUN);
        let mut state = State::new(CacheDB::default());
        let extension = AccountExtension::copy_from_slice(&[4; 32]);
        state.account(&address).unwrap().set_extension(extension.clone());
        state.finalize_transaction(version).unwrap();
        assert!(!state.account(&address).unwrap().get().unwrap().is_empty());
        state.commit_transaction();

        let checkpoint = state.checkpoint();
        state.create_account(&caller, &address, &U256::ZERO, version.features).unwrap().unwrap();
        let info = state.account(&address).unwrap().get().unwrap().clone();
        assert_eq!(info.extension, extension);
        assert_eq!(info.nonce, 1);
        state.rollback(checkpoint, version.features);
        assert_eq!(state.account(&address).unwrap().nonce(), 0);
        assert_eq!(state.account(&address).unwrap().get().unwrap().extension, extension);

        state.account(&address).unwrap().set_extension(AccountExtension::new());
        state.finalize_transaction(version).unwrap();
        assert!(state.account(&address).unwrap().get().is_none());
    }

    #[cfg(feature = "serde")]
    #[test]
    fn messagepack_preserves_empty_layout_and_nonempty_payloads() {
        #[derive(serde::Serialize, serde::Deserialize)]
        struct LegacyAccountInfo {
            balance: U256,
            nonce: u64,
            code_hash: alloy_primitives::B256,
            code: Option<crate::bytecode::Bytecode>,
        }
        let account = AccountInfo::default();
        let legacy = LegacyAccountInfo {
            balance: account.balance,
            nonce: account.nonce,
            code_hash: account.code_hash,
            code: account.code.clone(),
        };
        let encoded = rmp_serde::to_vec(&legacy).unwrap();
        assert_eq!(encoded, rmp_serde::to_vec(&account).unwrap());
        assert_eq!(rmp_serde::from_slice::<AccountInfo>(&encoded).unwrap(), account);
        assert!(serde_json::to_value(&account).unwrap().get("extension").is_none());
        let extended = AccountInfo {
            extension: AccountExtension::copy_from_slice(&[0x82; 32]),
            ..Default::default()
        };
        let record = (alloc::vec![account, extended], 99_u64);
        let encoded = rmp_serde::to_vec(&record).unwrap();
        let decoded: (Vec<AccountInfo>, u64) = rmp_serde::from_slice(&encoded).unwrap();
        assert_eq!(record, decoded);
    }
}
