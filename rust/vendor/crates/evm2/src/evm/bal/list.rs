//! The top-level Block Access List structure.

use super::{AccountBal, BlockAccessIndex};
use crate::{
    bytecode::BytecodeDecodeError,
    evm::state::{
        AccountChangeRef, AccountInfo, PendingState, StateChangeSink, StateChangeSource,
        StorageChange,
    },
    interpreter::Word,
};
use alloc::vec::Vec;
use alloy_eip7928::{
    AccountChanges as AlloyAccountChanges, BlockAccessList as AlloyBal,
    CodeChange as AlloyCodeChange, SlotChanges as AlloySlotChanges,
};
use alloy_primitives::{Address, map::AddressMap};
use core::convert::Infallible;

/// BAL structure.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Bal {
    /// Accounts bal.
    pub accounts: AddressMap<AccountBal>,
}

impl FromIterator<(Address, AccountBal)> for Bal {
    fn from_iter<I: IntoIterator<Item = (Address, AccountBal)>>(iter: I) -> Self {
        Self { accounts: iter.into_iter().collect() }
    }
}

impl Bal {
    /// Create a new BAL builder.
    pub fn new() -> Self {
        Self { accounts: AddressMap::default() }
    }

    /// Returns a canonical EIP-7928 [`AlloyBal`] without consuming this BAL.
    ///
    /// Builds the output directly from borrowed entries, cloning the change lists and sharing
    /// the original bytecode bytes without cloning the intermediate maps or decoded bytecode.
    /// Accounts are sorted by address, storage reads and writes by slot, and changes by block
    /// access index. The source BAL is left unchanged.
    pub fn to_alloy_bal(&self) -> AlloyBal {
        let mut alloy_bal = AlloyBal::from_iter(self.accounts.iter().map(|(&address, account)| {
            let mut storage_reads = Vec::new();
            let mut storage_changes = Vec::new();
            for (&slot, changes) in &account.storage.storage {
                if changes.is_empty() {
                    storage_reads.push(slot);
                } else {
                    storage_changes.push(AlloySlotChanges::new(slot, changes.changes.clone()));
                }
            }

            let mut changes = AlloyAccountChanges {
                address,
                storage_changes,
                storage_reads,
                balance_changes: account.account_info.balance.changes.clone(),
                nonce_changes: account.account_info.nonce.changes.clone(),
                code_changes: account
                    .account_info
                    .code
                    .changes
                    .iter()
                    .map(|change| {
                        AlloyCodeChange::new(
                            change.block_access_index,
                            change.code.1.original_bytes(),
                        )
                    })
                    .collect(),
            };
            changes.sort();
            changes
        }));
        alloy_bal.sort_unstable_by_key(|account| account.address());
        alloy_bal
    }

    /// Extend BAL with a transaction's detached [`PendingState`] at `bal_index`.
    ///
    /// This is the fold [`Evm`](crate::Evm) applies on transaction commit when its BAL builder is
    /// enabled: loaded-but-unchanged accounts and storage slots are recorded as reads, changed
    /// ones as writes at `bal_index`. Apply pending states in transaction order, since writes
    /// must be appended with ascending indices. Commit each transaction once, at its own index: a
    /// wiped slot is recorded as a read, which does not retract a write an earlier commit made at
    /// the same index.
    ///
    /// A selfdestructed account needs no special-casing: transaction finalization already resolved
    /// its present info to the EIP-8246 balance-only remnant or to a removed account, and its
    /// destroyed storage writes surface as reads (execution-specs `destroy_storage`).
    pub fn commit(&mut self, bal_index: BlockAccessIndex, pending: PendingState) {
        let Ok(()) = pending.visit(&mut BalWriter::new(self, bal_index));
    }
}

/// Folds streamed state changes into a [`Bal`] at one block access index.
///
/// Changed accounts and storage slots are recorded as writes, loaded-but-unchanged ones -- the
/// read callbacks -- as reads. An absent account side is the non-existent (default) account, so an
/// account removed by the transaction records zeroed writes. The bytecode and storage-wipe
/// callbacks need no BAL action: code changes surface through [`StateChangeSink::account`], and a
/// wiped account's storage surfaces through the storage callbacks.
#[derive(Debug)]
pub(super) struct BalWriter<'a> {
    bal: &'a mut Bal,
    index: BlockAccessIndex,
}

impl<'a> BalWriter<'a> {
    #[inline]
    pub(super) const fn new(bal: &'a mut Bal, index: BlockAccessIndex) -> Self {
        Self { bal, index }
    }
}

impl StateChangeSink for BalWriter<'_> {
    type Error = Infallible;

    #[inline]
    fn account(&mut self, change: AccountChangeRef<'_>) -> Result<(), Self::Error> {
        let absent = AccountInfo::default();
        self.bal.accounts.entry(change.address).or_default().account_info.update(
            self.index,
            change.original.unwrap_or(&absent),
            change.current.unwrap_or(&absent),
        );
        Ok(())
    }

    #[inline]
    fn storage(&mut self, change: StorageChange) -> Result<(), Self::Error> {
        self.bal
            .accounts
            .entry(change.address)
            .or_default()
            .storage
            .storage
            .entry(change.key)
            .or_default()
            .update(self.index, &change.original, change.current);
        Ok(())
    }

    #[inline]
    fn account_read(
        &mut self,
        address: Address,
        _info: Option<&AccountInfo>,
    ) -> Result<(), Self::Error> {
        self.bal.accounts.entry(address).or_default();
        Ok(())
    }

    #[inline]
    fn storage_read(
        &mut self,
        address: Address,
        key: Word,
        _value: Word,
    ) -> Result<(), Self::Error> {
        self.bal.accounts.entry(address).or_default().storage.storage.entry(key).or_default();
        Ok(())
    }
}

impl From<&Bal> for AlloyBal {
    /// Converts a borrowed BAL using [`Bal::to_alloy_bal`].
    #[inline]
    fn from(bal: &Bal) -> Self {
        bal.to_alloy_bal()
    }
}

impl From<Bal> for AlloyBal {
    /// Consume `Bal` and create a canonical EIP-7928 [`AlloyBal`].
    ///
    /// The returned access list is ordered deterministically: accounts are
    /// sorted lexicographically by address, and each account's nested reads and
    /// changes are sorted by the [`AccountBal`] `From` conversion into
    /// [`AlloyAccountChanges`].
    ///
    /// This matches the EIP-7928 ordering requirements:
    /// <https://eips.ethereum.org/EIPS/eip-7928#ordering-uniqueness-and-determinism>.
    fn from(bal: Bal) -> Self {
        let mut alloy_bal = Self::from_iter(
            bal.accounts
                .into_iter()
                .map(|(address, account)| AlloyAccountChanges { address, ..account.into() }),
        );
        alloy_bal.sort_unstable_by_key(|a| a.address());
        alloy_bal
    }
}

impl TryFrom<&[AlloyAccountChanges]> for Bal {
    type Error = BytecodeDecodeError;

    /// Convert borrowed EIP-7928 [`AlloyAccountChanges`] into a [`Bal`] without consuming
    /// the source.
    ///
    /// # Errors
    ///
    /// Returns [`BytecodeDecodeError`] if any account code change contains bytecode
    /// rejected by [`Bytecode::new_raw_checked`](crate::bytecode::Bytecode::new_raw_checked). This
    /// currently happens for malformed EIP-7702 bytecode, such as bytes with the EIP-7702 magic
    /// prefix but an invalid length or unsupported version.
    #[inline]
    fn try_from(alloy_bal: &[AlloyAccountChanges]) -> Result<Self, Self::Error> {
        let mut accounts =
            AddressMap::with_capacity_and_hasher(alloy_bal.len(), Default::default());
        for alloy_account in alloy_bal {
            accounts.insert(alloy_account.address(), AccountBal::try_from(alloy_account)?);
        }

        Ok(Self { accounts })
    }
}

impl TryFrom<AlloyBal> for Bal {
    type Error = BytecodeDecodeError;

    /// Convert an EIP-7928 [`AlloyBal`] into a [`Bal`].
    ///
    /// # Errors
    ///
    /// Returns [`BytecodeDecodeError`] if any account code change contains bytecode
    /// rejected by [`Bytecode::new_raw_checked`](crate::bytecode::Bytecode::new_raw_checked). This
    /// currently happens for malformed EIP-7702 bytecode, such as bytes with the EIP-7702 magic
    /// prefix but an invalid length or unsupported version.
    #[inline]
    fn try_from(alloy_bal: AlloyBal) -> Result<Self, Self::Error> {
        let mut accounts =
            AddressMap::with_capacity_and_hasher(alloy_bal.len(), Default::default());
        for alloy_account in alloy_bal {
            let address = alloy_account.address();
            accounts.insert(address, AccountBal::try_from(alloy_account)?);
        }

        Ok(Self { accounts })
    }
}

impl core::fmt::Display for Bal {
    /// Pretty prints the entire BAL structure in a human-readable format.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        writeln!(f, "=== Block Access List (BAL) ===")?;
        writeln!(f, "Total accounts: {}", self.accounts.len())?;
        writeln!(f)?;

        if self.accounts.is_empty() {
            return writeln!(f, "(empty)");
        }

        // Sort accounts by address before printing
        let mut sorted_accounts: Vec<_> = self.accounts.iter().collect();
        sorted_accounts.sort_unstable_by_key(|(address, _)| *address);

        for (idx, (address, account)) in sorted_accounts.into_iter().enumerate() {
            writeln!(f, "Account #{idx} - Address: {address:?}")?;
            writeln!(f, "  Account Info:")?;

            // Print nonce writes
            if account.account_info.nonce.is_empty() {
                writeln!(f, "    Nonce: (read-only, no writes)")?;
            } else {
                writeln!(f, "    Nonce writes:")?;
                for change in &account.account_info.nonce.changes {
                    writeln!(
                        f,
                        "      [{}] -> {}",
                        change.block_access_index(),
                        change.new_nonce()
                    )?;
                }
            }

            // Print balance writes
            if account.account_info.balance.is_empty() {
                writeln!(f, "    Balance: (read-only, no writes)")?;
            } else {
                writeln!(f, "    Balance writes:")?;
                for change in &account.account_info.balance.changes {
                    writeln!(
                        f,
                        "      [{}] -> {}",
                        change.block_access_index(),
                        change.post_balance()
                    )?;
                }
            }

            // Print code writes
            if account.account_info.code.is_empty() {
                writeln!(f, "    Code: (read-only, no writes)")?;
            } else {
                writeln!(f, "    Code writes:")?;
                for change in &account.account_info.code.changes {
                    let (code_hash, bytecode) = &change.code;
                    writeln!(
                        f,
                        "      [{}] -> hash: {:?}, size: {} bytes",
                        change.block_access_index,
                        code_hash,
                        bytecode.len()
                    )?;
                }
            }

            // Print storage writes
            writeln!(f, "  Storage:")?;
            if account.storage.storage.is_empty() {
                writeln!(f, "    (no storage slots)")?;
            } else {
                writeln!(f, "    Total slots: {}", account.storage.storage.len())?;
                for (storage_key, slot_changes) in &account.storage.storage {
                    writeln!(f, "    Slot: {storage_key:#x}")?;
                    if slot_changes.is_empty() {
                        writeln!(f, "      (read-only, no writes)")?;
                    } else {
                        writeln!(f, "      Writes:")?;
                        for change in &slot_changes.changes {
                            writeln!(
                                f,
                                "        [{}] -> {:?}",
                                change.block_access_index, change.new_value
                            )?;
                        }
                    }
                }
            }

            writeln!(f)?;
        }
        write!(f, "=== End of BAL ===")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bytecode::Bytecode,
        evm::{
            bal::{AccountInfoBal, BalChanges, BalCodeChange, StorageBal},
            state::{Account, AccountFlags, AccountInfo, StorageOverlay},
        },
        test_utils::storage_slot,
    };
    use alloc::{vec, vec::Vec};
    use alloy_eip7928::{
        AccountChanges as AlloyAccountChanges, BalanceChange as AlloyBalanceChange,
        CodeChange as AlloyCodeChange, NonceChange as AlloyNonceChange,
        SlotChanges as AlloySlotChanges, StorageChange as AlloyStorageChange,
    };
    use alloy_primitives::{B256, Bytes, U256, map::U256Map};

    fn code(byte: u8) -> (B256, Bytecode) {
        let bytecode = Bytecode::new_raw(vec![byte].into());
        (bytecode.hash_slow(), bytecode)
    }

    const fn idx(index: u64) -> BlockAccessIndex {
        BlockAccessIndex::new(index)
    }

    #[test]
    fn alloy_bal_conversions_canonicalize_eip_7928_ordering() {
        let low_address = Address::with_last_byte(1);
        let high_address = Address::with_last_byte(2);

        let unordered_account = AccountBal {
            account_info: AccountInfoBal {
                nonce: BalChanges::from(vec![
                    AlloyNonceChange::new(idx(9), 90),
                    AlloyNonceChange::new(idx(4), 40),
                ]),
                balance: BalChanges::from(vec![
                    AlloyBalanceChange::new(idx(5), U256::from(50)),
                    AlloyBalanceChange::new(idx(2), U256::from(20)),
                ]),
                code: BalChanges::from(vec![
                    BalCodeChange::new(idx(7), code(7)),
                    BalCodeChange::new(idx(3), code(3)),
                ]),
            },
            storage: StorageBal {
                storage: U256Map::from_iter([
                    (
                        U256::from(4),
                        BalChanges::from(vec![
                            AlloyStorageChange::new(idx(8), U256::from(80)),
                            AlloyStorageChange::new(idx(6), U256::from(60)),
                        ]),
                    ),
                    (U256::ONE, BalChanges::default()),
                    (
                        U256::from(2),
                        BalChanges::from(vec![
                            AlloyStorageChange::new(idx(3), U256::from(30)),
                            AlloyStorageChange::new(idx(1), U256::from(10)),
                        ]),
                    ),
                    (U256::from(3), BalChanges::default()),
                ]),
            },
        };

        let bal = Bal::from_iter([
            (high_address, AccountBal::default()),
            (low_address, unordered_account),
        ]);
        let original = bal.clone();
        let alloy_bal = bal.to_alloy_bal();
        assert_eq!(alloy_bal, AlloyBal::from(&bal));
        assert_eq!(bal, original);
        assert_eq!(alloy_bal, AlloyBal::from(bal));

        assert_eq!(
            alloy_bal.iter().map(|account| account.address()).collect::<Vec<_>>(),
            vec![low_address, high_address]
        );

        let account = &alloy_bal[0];
        assert_eq!(account.storage_reads, vec![U256::ONE, U256::from(3)]);
        assert_eq!(
            account.storage_changes.iter().map(|slot| slot.slot).collect::<Vec<_>>(),
            vec![U256::from(2), U256::from(4)]
        );
        assert_eq!(
            account.storage_changes[0]
                .changes
                .iter()
                .map(|change| change.block_access_index)
                .collect::<Vec<_>>(),
            vec![idx(1), idx(3)]
        );
        assert_eq!(
            account.storage_changes[1]
                .changes
                .iter()
                .map(|change| change.block_access_index)
                .collect::<Vec<_>>(),
            vec![idx(6), idx(8)]
        );
        assert_eq!(
            account
                .balance_changes
                .iter()
                .map(|change| change.block_access_index())
                .collect::<Vec<_>>(),
            vec![idx(2), idx(5)]
        );
        assert_eq!(
            account
                .nonce_changes
                .iter()
                .map(|change| change.block_access_index())
                .collect::<Vec<_>>(),
            vec![idx(4), idx(9)]
        );
        assert_eq!(
            account
                .code_changes
                .iter()
                .map(|change| change.block_access_index())
                .collect::<Vec<_>>(),
            vec![idx(3), idx(7)]
        );
        assert_eq!(account.code_changes[0].new_code().as_ref(), &[3]);
        assert_eq!(account.code_changes[1].new_code().as_ref(), &[7]);
    }

    #[test]
    fn borrowed_empty_bal_converts_to_empty_alloy_bal() {
        let bal = Bal::new();
        assert!(bal.to_alloy_bal().is_empty());
        assert!(AlloyBal::from(&bal).is_empty());
    }

    #[test]
    fn commit_builds_bal_from_pending_overlay() {
        let address = Address::with_last_byte(1);

        // A freshly created account: no original info, present nonce/balance set, and one changed
        // storage slot plus one loaded-but-unchanged (read) slot.
        let account = Account::new(
            None,
            Some(AccountInfo::default().with_nonce(1).with_balance(U256::from(100))),
        );
        let mut overlay = StorageOverlay::default();
        overlay.slots.insert(U256::from(5), storage_slot(U256::ZERO, U256::from(42)));
        overlay.slots.insert(U256::from(6), storage_slot(U256::from(7), U256::from(7)));
        let account = Account { storage: overlay, ..account };
        let pending = PendingState { accounts: AddressMap::from_iter([(address, account)]) };

        let mut bal = Bal::new();
        bal.commit(idx(1), pending);

        let account = bal.accounts.get(&address).unwrap();
        assert_eq!(account.account_info.nonce.changes, vec![AlloyNonceChange::new(idx(1), 1)]);
        assert_eq!(
            account.account_info.balance.changes,
            vec![AlloyBalanceChange::new(idx(1), U256::from(100))]
        );
        // No code change, so no code writes.
        assert!(account.account_info.code.is_empty());
        // Changed slot recorded as a write.
        assert_eq!(
            account.storage.storage.get(&U256::from(5)).unwrap().changes,
            vec![AlloyStorageChange::new(idx(1), U256::from(42))]
        );
        // Loaded-but-unchanged slot recorded as a read (empty writes).
        assert!(account.storage.storage.get(&U256::from(6)).unwrap().is_empty());
    }

    #[test]
    fn commit_selfdestruct_uses_finalized_post_state() {
        let address = Address::with_last_byte(1);

        // Transaction finalization already resolved the selfdestructed account: removed
        // (`present: None`), storage overlay wiped with the prior writes turned into
        // loaded-but-unchanged slots surfacing as reads. The BAL derives from that overlay
        // without special-casing.
        let account =
            Account::new(Some(AccountInfo::default().with_balance(U256::from(100))), None);
        let mut overlay = StorageOverlay { wiped: true, ..Default::default() };
        overlay.slots.insert(U256::from(5), storage_slot(U256::from(42), U256::from(42)));
        let account = Account { storage: overlay, flags: AccountFlags::DESTROYED, ..account };
        let pending = PendingState { accounts: AddressMap::from_iter([(address, account)]) };

        let mut bal = Bal::new();
        bal.commit(idx(2), pending);

        let account = bal.accounts.get(&address).unwrap();
        // Balance goes to zero on removal.
        assert_eq!(
            account.account_info.balance.changes,
            vec![AlloyBalanceChange::new(idx(2), U256::ZERO)]
        );
        // The loaded-but-unchanged slot stays a read (empty writes).
        assert!(account.storage.storage.get(&U256::from(5)).unwrap().is_empty());
    }

    #[test]
    fn commit_records_wiped_slot_zeroing_as_read() {
        let address = Address::with_last_byte(1);

        // A wiped account whose slot started nonzero and ended zero: the wipe is not a write.
        // Only the slot that is nonzero after the wipe is recorded as one.
        let info = AccountInfo::default().with_balance(U256::from(100));
        let mut overlay = StorageOverlay { wiped: true, ..Default::default() };
        overlay.slots.insert(U256::from(5), storage_slot(U256::from(42), U256::ZERO));
        overlay.slots.insert(U256::from(6), storage_slot(U256::from(7), U256::from(8)));
        let account = Account {
            original: Some(info.clone()),
            present: Some(info),
            storage: overlay,
            ..Default::default()
        };
        let pending = PendingState { accounts: AddressMap::from_iter([(address, account)]) };

        let mut bal = Bal::new();
        bal.commit(idx(1), pending);

        let storage = &bal.accounts.get(&address).unwrap().storage.storage;
        assert!(storage.get(&U256::from(5)).unwrap().is_empty());
        assert_eq!(
            storage.get(&U256::from(6)).unwrap().changes,
            vec![AlloyStorageChange::new(idx(1), U256::from(8))]
        );
    }

    #[test]
    fn try_from_alloy_decodes_block_access_list() {
        let address = Address::with_last_byte(1);
        let code_bytes = Bytes::from_static(&[0x60, 0x00]);
        let alloy_bal = vec![AlloyAccountChanges {
            address,
            code_changes: vec![AlloyCodeChange::new(idx(1), code_bytes.clone())],
            ..Default::default()
        }];

        let bal = Bal::try_from(alloy_bal).unwrap();
        let account = bal.accounts.get(&address).unwrap();
        let (_, bytecode) = &account.account_info.code.changes[0].code;

        assert_eq!(bytecode.original_bytes(), code_bytes);
    }

    #[test]
    fn try_from_alloy_ref_matches_owned_conversion() {
        let address = Address::with_last_byte(1);
        let code_bytes = Bytes::from_static(&[0x60, 0x00]);
        let alloy_bal = vec![AlloyAccountChanges {
            address,
            storage_changes: vec![AlloySlotChanges::new(
                U256::ONE,
                vec![AlloyStorageChange::new(idx(1), U256::from(10))],
            )],
            storage_reads: vec![U256::from(2)],
            balance_changes: vec![AlloyBalanceChange::new(idx(2), U256::from(20))],
            nonce_changes: vec![AlloyNonceChange::new(idx(3), 30)],
            code_changes: vec![AlloyCodeChange::new(idx(4), code_bytes.clone())],
        }];

        let borrowed = Bal::try_from(alloy_bal.as_slice()).unwrap();
        let owned = Bal::try_from(alloy_bal.clone()).unwrap();

        assert_eq!(borrowed, owned);
        assert_eq!(alloy_bal[0].code_changes[0].new_code(), &code_bytes);
    }

    #[test]
    fn try_from_alloy_errors_on_invalid_code_change() {
        let alloy_bal = vec![AlloyAccountChanges {
            address: Address::with_last_byte(1),
            code_changes: vec![AlloyCodeChange::new(idx(1), vec![0xef, 0x01, 0xde].into())],
            ..Default::default()
        }];

        assert!(Bal::try_from(alloy_bal).is_err());
    }

    #[test]
    fn try_from_alloy_ref_errors_on_invalid_code_change() {
        let alloy_bal = vec![AlloyAccountChanges {
            address: Address::with_last_byte(1),
            code_changes: vec![AlloyCodeChange::new(idx(1), vec![0xef, 0x01, 0xde].into())],
            ..Default::default()
        }];

        assert!(Bal::try_from(alloy_bal.as_slice()).is_err());
    }
}
