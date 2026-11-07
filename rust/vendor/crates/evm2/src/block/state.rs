//! Block state accumulated from committed transactions.

use super::bundle_state::{
    AccountStatus, BundleRetention, BundleState, StorageSlot, TransitionAccount, TransitionState,
};
use crate::{bytecode::Bytecode, evm::AccountInfo};
use alloc::vec::Vec;
use alloy_primitives::{
    Address, B256, U256,
    map::{AddressMap, B256Map, U256Map, hash_map::Entry},
};

/// One block's account transitions, finalized into a [`BundleState`] with their reverts.
#[derive(Debug, Default)]
pub struct BlockState {
    transitions: AddressMap<TransitionAccount>,
    contracts: B256Map<Bytecode>,
}

impl BlockState {
    /// Creates an empty block accumulator.
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies a collected transaction to the block's transitions.
    pub fn commit(&mut self, changes: &TransactionChanges) {
        self.contracts.extend(changes.contracts.iter().map(|(hash, code)| (*hash, code.clone())));
        for (address, account) in &changes.accounts {
            self.commit_account(
                *address,
                account.original.clone(),
                (!account.selfdestructed).then(|| account.info.clone()),
                account.created,
                account.wiped,
                account.storage.iter().filter_map(|(key, slot)| {
                    // A wipe reinserts every nonzero value that follows it.
                    (slot.is_changed() || account.wiped && !slot.present_value().is_zero())
                        .then_some((*key, *slot))
                }),
            );
        }
    }

    /// Commits one account's finalized evm2 transaction changes into the block without
    /// materializing EVM state. Accounts that were only loaded are skipped.
    ///
    /// When `update` is set, the committed account is also recorded into it for execution state
    /// hooks.
    pub fn commit_account_changes(
        &mut self,
        changes: crate::evm::AccountChanges<'_>,
        update: Option<&mut StateUpdate>,
    ) {
        if !changes.is_changed() {
            return;
        }
        if let Some((hash, code)) = changes.code {
            self.contracts.entry(hash).or_insert_with(|| code.clone());
        }
        let address = changes.address;
        let created = changes.created;
        let wiped = changes.storage.wiped;
        let original = changes.original.map(AccountInfo::clone_no_code);
        let current = changes.current.map(AccountInfo::clone_no_code);
        let slots = changes
            .storage
            .changed_slots()
            .map(|(&key, value)| (key, StorageSlot::new_changed(value.original, value.current)));
        let Some(update) = update else {
            self.commit_account(address, original, current, created, wiped, slots);
            return;
        };
        let storage: Vec<_> = slots.collect();
        self.commit_account(
            address,
            original.clone(),
            current.clone(),
            created,
            wiped,
            storage.iter().copied(),
        );
        update.accounts.push(AccountUpdate { address, original, current, created, wiped, storage });
    }

    fn commit_account(
        &mut self,
        address: Address,
        original: Option<AccountInfo>,
        current: Option<AccountInfo>,
        created: bool,
        wiped: bool,
        storage: impl Iterator<Item = (U256, StorageSlot)>,
    ) {
        let entry = self.transitions.entry(address);
        let previous_status = match &entry {
            Entry::Occupied(entry) => entry.get().status,
            Entry::Vacant(_) => loaded_status(original.as_ref()),
        };
        let status = if current.is_none() {
            previous_status.on_selfdestructed()
        } else if wiped {
            AccountStatus::DestroyedChanged
        } else if created {
            previous_status.on_created()
        } else {
            previous_status.on_changed(
                original.as_ref().is_none_or(|i| i.nonce == 0 && i.is_empty_code_hash()),
            )
        };
        match entry {
            Entry::Vacant(entry) => {
                entry.insert(TransitionAccount {
                    info: current,
                    status,
                    previous_info: original,
                    previous_status,
                    storage: storage.collect(),
                    storage_was_destroyed: wiped,
                });
            }
            Entry::Occupied(mut entry) => {
                let account = entry.get_mut();
                account.info = current;
                account.status = status;
                // A deletion or wipe starts a new storage lifetime.
                if wiped
                    || matches!(status, AccountStatus::Destroyed | AccountStatus::DestroyedAgain)
                {
                    account.storage.clear();
                    account.storage.extend(storage);
                    account.storage_was_destroyed = true;
                } else {
                    // After a wipe, every nonzero value is reinserted, so a slot that returns to
                    // its original value stays.
                    let wiped_lifetime = account.storage_was_destroyed || status.was_destroyed();
                    for (key, slot) in storage {
                        match account.storage.entry(key) {
                            Entry::Vacant(entry) => {
                                entry.insert(slot);
                            }
                            Entry::Occupied(mut entry) => {
                                if !wiped_lifetime
                                    && entry.get().original_value() == slot.present_value()
                                {
                                    entry.remove();
                                } else {
                                    entry.get_mut().present_value = slot.present_value();
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Finalizes one block's transitions and reverts.
    pub fn into_bundle(self) -> BundleState {
        self.into_bundle_with_retention(BundleRetention::Reverts)
    }

    /// Finalizes one block's transitions, creating reverts only if `retention` asks for them.
    pub fn into_bundle_with_retention(self, retention: BundleRetention) -> BundleState {
        let mut bundle = BundleState::default();
        bundle.apply_transitions_and_create_reverts(
            TransitionState { transitions: self.transitions },
            retention,
        );
        bundle.contracts.extend(self.contracts);
        bundle
    }
}

/// Transaction changes collected from an evm2 change stream for [`BlockState::commit`].
#[derive(Debug, Default)]
pub struct TransactionChanges {
    accounts: AddressMap<TransactionAccount>,
    contracts: B256Map<Bytecode>,
}

/// One account's changes within a [`TransactionChanges`].
#[derive(Debug)]
struct TransactionAccount {
    /// Account at the start of the transaction, without code. `None` means it did not exist.
    original: Option<AccountInfo>,
    /// Account after the transaction, without code.
    info: AccountInfo,
    created: bool,
    selfdestructed: bool,
    wiped: bool,
    storage: U256Map<StorageSlot>,
}

impl Default for TransactionAccount {
    fn default() -> Self {
        Self {
            original: Some(AccountInfo::empty()),
            info: AccountInfo::empty(),
            created: false,
            selfdestructed: false,
            wiped: false,
            storage: U256Map::default(),
        }
    }
}

impl TransactionAccount {
    fn update(&mut self, change: crate::evm::AccountChangeRef<'_>) {
        self.original = change.original.map(AccountInfo::clone_no_code);
        self.info =
            change.current.map(AccountInfo::clone_no_code).unwrap_or_else(AccountInfo::empty);
        self.created |= change.created;
        self.selfdestructed |= change.current.is_none();
    }
}

impl crate::evm::StateChangeSink for TransactionChanges {
    type Error = core::convert::Infallible;
    fn bytecode(&mut self, hash: B256, code: &Bytecode) -> Result<(), Self::Error> {
        self.contracts.entry(hash).or_insert_with(|| code.clone());
        Ok(())
    }
    fn storage_wipe(&mut self, address: Address) -> Result<(), Self::Error> {
        let account = self.accounts.entry(address).or_default();
        account.wiped = true;
        account.storage.clear();
        Ok(())
    }
    fn storage(&mut self, change: crate::evm::StorageChange) -> Result<(), Self::Error> {
        self.accounts
            .entry(change.address)
            .or_default()
            .storage
            .insert(change.key, StorageSlot::new_changed(change.original, change.current));
        Ok(())
    }
    fn account(&mut self, change: crate::evm::AccountChangeRef<'_>) -> Result<(), Self::Error> {
        self.accounts.entry(change.address).or_default().update(change);
        Ok(())
    }
    fn account_read(
        &mut self,
        address: Address,
        info: Option<&AccountInfo>,
    ) -> Result<(), Self::Error> {
        if let Some(account) = self.accounts.get_mut(&address) {
            account.update(crate::evm::AccountChangeRef {
                address,
                original: info,
                current: info,
                created: false,
                selfdestructed: false,
            });
        }
        Ok(())
    }
}

/// Per-transaction state changes streamed to execution state hooks.
///
/// Holds only the accounts a transaction committed, with account info but without bytecode, so
/// building it does not copy the transaction's state maps.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StateUpdate {
    /// Committed accounts in stream order.
    pub accounts: Vec<AccountUpdate>,
}

impl StateUpdate {
    /// Returns whether the update commits no accounts.
    pub const fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    /// Returns the update for `address`, if the transaction committed it.
    pub fn account(&self, address: &Address) -> Option<&AccountUpdate> {
        self.accounts.iter().find(|account| account.address == *address)
    }
}

/// One committed account in a [`StateUpdate`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountUpdate {
    /// Account address.
    pub address: Address,
    /// Account at the start of the transaction, without code. `None` means it did not exist.
    pub original: Option<AccountInfo>,
    /// Account after the transaction, without code. `None` is a deletion.
    pub current: Option<AccountInfo>,
    /// Whether the account was created during the transaction.
    pub created: bool,
    /// Whether the account's prior storage was wiped.
    pub wiped: bool,
    /// Storage slots whose value changed.
    pub storage: Vec<(U256, StorageSlot)>,
}

/// Visits a persisted bundle as native changes when seeding an execution cache.
#[derive(Debug)]
pub struct BundleSource<'a>(pub &'a BundleState);

impl crate::evm::StateChangeSource for BundleSource<'_> {
    fn visit<S: crate::evm::StateChangeSink>(&self, sink: &mut S) -> Result<(), S::Error> {
        for (hash, code) in &self.0.contracts {
            sink.bytecode(*hash, code)?;
        }
        for (address, account) in self.0.state() {
            let current = account.info.as_ref();
            if account.was_destroyed() {
                sink.storage_wipe(*address)?;
            }
            sink.account(crate::evm::AccountChangeRef {
                address: *address,
                original: account.original_info.as_ref(),
                current,
                created: account.status.is_storage_known() && current.is_some(),
                selfdestructed: current.is_none(),
            })?;
            for (key, value) in &account.storage {
                sink.storage(crate::evm::StorageChange {
                    address: *address,
                    key: *key,
                    original: value.original_value(),
                    current: value.present_value(),
                })?;
            }
        }
        Ok(())
    }
}

fn loaded_status(info: Option<&AccountInfo>) -> AccountStatus {
    match info {
        None => AccountStatus::LoadedNotExisting,
        Some(info) if info.is_empty() => AccountStatus::LoadedEmptyEIP161,
        Some(_) => AccountStatus::Loaded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        block::bundle_state::OriginalValuesKnown,
        evm::{AccountChangeRef, AccountChanges, StateChangeSink, StorageChange, StorageOverlay},
        test_utils::storage_slot,
    };

    /// Builds a storage overlay from `(key, original, current)` slots.
    fn overlay(wiped: bool, slots: &[(U256, U256, U256)]) -> StorageOverlay {
        let mut storage = StorageOverlay { wiped, ..Default::default() };
        for &(key, original, current) in slots {
            storage.slots.insert(key, storage_slot(original, current));
        }
        storage
    }

    #[cfg(feature = "account-ext")]
    #[test]
    fn account_extensions_survive_native_updates_and_reverts() {
        let address = Address::with_last_byte(1);
        let original = AccountInfo {
            balance: U256::from(10),
            extension: crate::evm::AccountExtension::copy_from_slice(b"original extension"),
            ..Default::default()
        };
        let mut updated = original.clone();
        updated.balance = U256::from(20);
        updated.extension = crate::evm::AccountExtension::copy_from_slice(b"updated extension");
        let mut state = BlockState::new();
        state.commit_account_changes(
            AccountChanges {
                address,
                original: Some(&original),
                current: Some(&updated),
                created: false,
                selfdestructed: false,
                touched: false,
                code: None,
                storage: &StorageOverlay::default(),
            },
            None,
        );
        let mut bundle = state.into_bundle();
        let account = bundle.state.get(&address).unwrap();
        assert_eq!(
            account.original_info.as_ref().unwrap().extension.as_ref(),
            b"original extension"
        );
        assert_eq!(account.info.as_ref().unwrap().extension.as_ref(), b"updated extension");
        assert!(bundle.revert_latest());
        assert_eq!(
            bundle.account(&address).unwrap().info.as_ref().unwrap().extension.as_ref(),
            b"original extension"
        );
    }

    #[test]
    fn native_sink_preserves_storage_lifetimes_and_reverts() {
        let address = Address::with_last_byte(1);
        let code = evm2::bytecode::Bytecode::new_raw(alloy_primitives::bytes!("60015b00"));
        let info = AccountInfo::default().with_nonce(1).with_code(code.clone());
        let mut converted = BlockState::new();
        let mut native = BlockState::new();
        // Storage-only writes, restoration, deletion, recreation, a surviving storage wipe,
        // and another deletion exercise both occupied and vacant transition entries.
        for step in 0..7 {
            let slots: &[_] = if step < 2 || step == 3 || step == 4 {
                let (original, current) = if step == 1 { (8, 7) } else { (7, 8) };
                &[(U256::from(3), U256::from(original), U256::from(current))]
            } else {
                &[]
            };
            let storage = overlay(step >= 2, slots);
            let changes = if step < 2 {
                AccountChanges {
                    address,
                    original: Some(&info),
                    current: Some(&info),
                    created: false,
                    selfdestructed: false,
                    touched: false,
                    code: None,
                    storage: &storage,
                }
            } else {
                AccountChanges {
                    address,
                    original: (step != 3 && step != 6).then_some(&info),
                    current: (step == 3 || step == 4).then_some(&info),
                    created: step == 3,
                    selfdestructed: step != 3,
                    touched: false,
                    code: (step == 3).then(|| (code.hash_slow(), &code)),
                    storage: &storage,
                }
            };
            let empty = StorageOverlay::default();
            // Reads alone must never introduce an account transition.
            let read = AccountChanges {
                address: Address::with_last_byte(2),
                original: Some(&info),
                current: Some(&info),
                created: false,
                selfdestructed: false,
                touched: false,
                code: None,
                storage: &empty,
            };
            let mut transaction = TransactionChanges::default();
            let Ok(()) = changes.visit(&mut transaction);
            let Ok(()) = read.visit(&mut transaction);
            converted.commit(&transaction);
            native.commit_account_changes(changes, None);
            native.commit_account_changes(read, None);
            assert_eq!(native.transitions, converted.transitions, "step {step}");
            assert_eq!(native.contracts, converted.contracts);
        }
        let mut native = native.into_bundle();
        let mut converted = converted.into_bundle();
        assert_eq!(native, converted);
        assert!(native.revert_latest());
        assert!(converted.revert_latest());
        assert_eq!(native, converted);
    }

    #[test]
    fn stream_commit_keeps_unchanged_slot_after_wipe() {
        let address = Address::with_last_byte(1);
        let info = AccountInfo { nonce: 1, ..Default::default() };
        let mut changes = TransactionChanges::default();
        changes.storage_wipe(address).unwrap();
        changes
            .storage(StorageChange {
                address,
                key: U256::from(3),
                original: U256::from(5),
                current: U256::from(5),
            })
            .unwrap();
        changes.account_read(address, Some(&info)).unwrap();
        let mut block = BlockState::new();
        block.commit(&changes);
        let bundle = block.into_bundle();
        assert_eq!(bundle.storage(&address, U256::from(3)), Some(U256::from(5)));
    }

    #[test]
    fn created_account_keeps_its_storage_wipe() {
        let address = Address::with_last_byte(1);
        let info = AccountInfo { balance: U256::ONE, ..Default::default() };
        let storage = overlay(true, &[]);
        let mut block = BlockState::new();
        block.commit_account_changes(
            AccountChanges {
                address,
                original: None,
                current: Some(&info),
                created: true,
                selfdestructed: true,
                touched: false,
                code: None,
                storage: &storage,
            },
            None,
        );
        let bundle = block.into_bundle();
        assert!(bundle.account(&address).unwrap().was_destroyed());
        let changeset = bundle.to_plain_state(OriginalValuesKnown::Yes);
        assert!(changeset.storage.iter().any(|storage| storage.wipe_storage));
    }

    #[test]
    fn slot_returning_to_original_after_wipe_is_reinserted() {
        let address = Address::with_last_byte(1);
        let info = AccountInfo { nonce: 1, ..Default::default() };
        let mut block = BlockState::new();
        for (wiped, slot) in [
            (true, (U256::from(3), U256::from(5), U256::from(6))),
            (false, (U256::from(3), U256::from(6), U256::from(5))),
        ] {
            let storage = overlay(wiped, &[slot]);
            block.commit_account_changes(
                AccountChanges {
                    address,
                    original: Some(&info),
                    current: Some(&info),
                    created: false,
                    selfdestructed: false,
                    touched: false,
                    code: None,
                    storage: &storage,
                },
                None,
            );
        }
        let bundle = block.into_bundle();
        assert_eq!(bundle.storage(&address, U256::from(3)), Some(U256::from(5)));
    }

    #[test]
    fn live_account_wipe_drops_earlier_slots() {
        let address = Address::with_last_byte(1);
        let info = AccountInfo { nonce: 1, ..Default::default() };
        let mut block = BlockState::new();
        for (wiped, slots) in [
            (false, &[(U256::from(3), U256::ZERO, U256::ONE)][..]),
            (true, &[(U256::from(4), U256::ZERO, U256::from(2))][..]),
        ] {
            let storage = overlay(wiped, slots);
            block.commit_account_changes(
                AccountChanges {
                    address,
                    original: Some(&info),
                    current: Some(&info),
                    created: false,
                    selfdestructed: false,
                    touched: false,
                    code: None,
                    storage: &storage,
                },
                None,
            );
        }
        let bundle = block.into_bundle();
        let account = bundle.account(&address).unwrap();
        assert_eq!(account.storage_slot(U256::from(3)), Some(U256::ZERO));
        assert_eq!(account.storage_slot(U256::from(4)), Some(U256::from(2)));
    }

    #[test]
    fn cached_bytecode_does_not_change_bundle_output() {
        let address = Address::with_last_byte(1);
        let code = evm2::bytecode::Bytecode::new_raw(alloy_primitives::bytes!("60015b00"));
        let info = AccountInfo {
            balance: U256::from(5),
            code_hash: code.hash_slow(),
            ..Default::default()
        };
        let bundle = |info: &AccountInfo| {
            let mut changes = TransactionChanges::default();
            changes
                .account(AccountChangeRef {
                    address,
                    original: None,
                    current: Some(info),
                    created: false,
                    selfdestructed: false,
                })
                .unwrap();
            let mut block = BlockState::new();
            block.commit(&changes);
            block.into_bundle()
        };
        let uncached = bundle(&info);
        let cached = bundle(&AccountInfo { code: Some(code), ..info });
        assert_eq!(uncached, cached);
        assert!(cached.contracts.is_empty());
    }

    #[test]
    fn deployed_bytecode_is_retained_separately_from_account_metadata() {
        let address = Address::with_last_byte(1);
        let code = evm2::bytecode::Bytecode::new_raw(alloy_primitives::bytes!("60015b00"));
        let hash = code.hash_slow();
        let info = AccountInfo {
            nonce: 1,
            code_hash: hash,
            code: Some(code.clone()),
            ..Default::default()
        };
        let mut changes = TransactionChanges::default();
        changes.bytecode(hash, &code).unwrap();
        changes
            .account(AccountChangeRef {
                address,
                original: None,
                current: Some(&info),
                created: true,
                selfdestructed: false,
            })
            .unwrap();
        let mut block = BlockState::new();
        block.commit(&changes);
        let bundle = block.into_bundle();
        assert_eq!(bundle.contracts[&hash].original_bytes(), code.original_bytes());
        assert_eq!(bundle.account(&address).unwrap().info.as_ref().unwrap().code_hash, hash);
    }

    #[test]
    fn storage_only_change_retains_account_and_reverts() {
        let address = Address::with_last_byte(1);
        let info = AccountInfo { nonce: 1, ..Default::default() };
        let mut changes = TransactionChanges::default();
        changes
            .storage(StorageChange {
                address,
                key: U256::from(3),
                original: U256::from(7),
                current: U256::from(8),
            })
            .unwrap();
        changes.account_read(address, Some(&info)).unwrap();
        let mut block = BlockState::new();
        block.commit(&changes);
        let mut bundle = block.into_bundle();
        assert_eq!(bundle.account(&address).unwrap().info.as_ref().unwrap().nonce, 1);
        assert_eq!(bundle.storage(&address, U256::from(3)), Some(U256::from(8)));
        assert_eq!(bundle.reverts.len(), 1);
        assert!(bundle.revert_latest());
        assert_eq!(bundle.storage(&address, U256::from(3)), Some(U256::from(7)));
    }

    #[test]
    fn destroy_then_recreate_retains_parent_storage_wipe() {
        let address = Address::with_last_byte(1);
        let original = AccountInfo { nonce: 1, balance: U256::from(7), ..Default::default() };
        let recreated = AccountInfo { nonce: 1, balance: U256::from(9), ..Default::default() };
        let mut block = BlockState::new();
        let mut destroyed = TransactionChanges::default();
        destroyed
            .account(AccountChangeRef {
                address,
                original: Some(&original),
                current: None,
                created: false,
                selfdestructed: true,
            })
            .unwrap();
        block.commit(&destroyed);
        let mut created = TransactionChanges::default();
        created.storage_wipe(address).unwrap();
        created
            .storage(StorageChange {
                address,
                key: U256::from(3),
                original: U256::ZERO,
                current: U256::from(8),
            })
            .unwrap();
        created
            .account(AccountChangeRef {
                address,
                original: None,
                current: Some(&recreated),
                created: true,
                selfdestructed: false,
            })
            .unwrap();
        block.commit(&created);
        let bundle = block.into_bundle();
        let account = bundle.account(&address).unwrap();
        assert!(account.was_destroyed());
        assert_eq!(account.original_info.as_ref().unwrap().balance, U256::from(7));
        assert_eq!(account.info.as_ref().unwrap().balance, U256::from(9));
        assert_eq!(account.storage_slot(U256::from(4)), Some(U256::ZERO));
        assert_eq!(account.storage_slot(U256::from(3)), Some(U256::from(8)));
        assert!(bundle.reverts[0][0].1.wipe_storage);
    }
}
