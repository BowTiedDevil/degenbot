//! Owned pending transaction state detached from the EVM.

use super::{
    Account, AccountChanges, AccountFlags, AccountInfo, StateChangeSink, StateChangeSource,
    StorageSlot, Tracked,
};
use crate::interpreter::Word;
use alloy_primitives::{Address, map::AddressMap};

/// A transaction's finalized-but-uncommitted state, moved out of the EVM.
///
/// Produced by [`ExecutedTx::detach`](crate::ExecutedTx::detach), this is the transaction overlay
/// exactly as execution left it: every account and storage slot loaded during the transaction,
/// each carrying its transaction-boundary original value next to its present value. Two consumers
/// draw from it:
///
/// - [`Bal::commit`](crate::evm::Bal::commit) folds it into an EIP-7928 Block Access List,
///   recording loaded-but-unchanged entries as reads and changed ones as writes — the same fold the
///   EVM applies on transaction commit when its builder is enabled.
/// - [`StateChangeSource::visit`] streams it to a [`StateChangeSink`] in deterministic application
///   order: changed entries through the change callbacks (how persistence consumers, e.g. reth,
///   apply the transaction to the database) and loaded-but-unchanged entries through the read
///   callbacks.
///
/// A detached pending state can also be reattached to an EVM with
/// [`State::set_pending_state`](super::State::set_pending_state).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PendingState {
    /// Accounts loaded by the transaction: transaction-boundary original info, present info, and
    /// account-lifetime flags.
    pub(crate) accounts: AddressMap<Account>,
}

impl PendingState {
    /// Returns whether the transaction loaded no accounts and no storage.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    /// Returns the current account information when the account is present in pending state.
    #[inline]
    pub fn account_info(&self, address: &Address) -> Option<&AccountInfo> {
        self.accounts.get(address).and_then(|account| account.present.as_ref())
    }

    /// Inserts an account's transaction-boundary original and current values.
    pub fn insert_account(
        &mut self,
        address: Address,
        original: Option<AccountInfo>,
        current: Option<AccountInfo>,
    ) {
        let account = self.accounts.entry(address).or_default();
        *account = Account {
            original,
            present: current,
            flags: AccountFlags::empty(),
            storage: core::mem::take(&mut account.storage),
        };
    }

    /// Inserts a storage slot's transaction-boundary original and current values.
    ///
    /// # Panics
    ///
    /// Panics if the owning account has not been inserted with [`Self::insert_account`].
    pub fn insert_storage(&mut self, address: Address, key: Word, original: Word, current: Word) {
        self.accounts
            .get_mut(&address)
            .expect("insert the account before its storage")
            .storage
            .slots
            .insert(
                key,
                StorageSlot {
                    value: Tracked::from_parts(original, current),
                    is_warm: false,
                    _non_exhaustive: (),
                },
            );
    }

    /// Returns whether the transaction contains any account or storage change.
    ///
    /// Loaded-but-unchanged accounts and storage slots are ignored.
    #[cfg(test)]
    pub(crate) fn is_changed(&self) -> bool {
        self.accounts.values().any(|account| account.is_changed() || account.storage.is_changed())
    }

    /// Returns a borrowed view of the transaction's changes.
    #[inline]
    pub const fn changes(&self) -> TxStateChanges<'_> {
        TxStateChanges::new(&self.accounts)
    }

    /// Returns each loaded account's changes, in an unspecified account order.
    #[inline]
    pub fn account_changes(&self) -> impl Iterator<Item = AccountChanges<'_>> {
        self.changes().account_changes()
    }

    /// Returns the changes of the account at `address`, or `None` if the transaction did not load
    /// it.
    #[inline]
    pub fn account_changes_for(&self, address: &Address) -> Option<AccountChanges<'_>> {
        self.changes().account_changes_for(address)
    }
}

impl StateChangeSource for PendingState {
    /// Visits the transaction's loaded entries account by account, in an unspecified account
    /// order. Each account's bytecode and storage precede its metadata.
    ///
    /// The same code hash may be visited more than once when several accounts share bytecode; sinks
    /// key bytecode by hash, so repeated visits are idempotent.
    ///
    /// Changed accounts — including created or selfdestructed accounts whose info ended up
    /// unchanged — go through [`StateChangeSink::account`]; loaded-but-unchanged entries go
    /// through the read callbacks.
    fn visit<S: StateChangeSink>(&self, sink: &mut S) -> Result<(), S::Error> {
        self.changes().visit(sink)
    }
}

/// A borrowed view of one transaction's state changes, whether still attached to the EVM or
/// detached into a [`PendingState`].
///
/// Returned by [`ExecutedTx::changes`](crate::ExecutedTx::changes), which borrows the
/// transaction layer in place, and by [`PendingState::changes`]. It lets a caller inspect what a
/// transaction wrote before deciding to commit or discard it, without detaching the state.
#[derive(Clone, Copy, Debug, Default)]
pub struct TxStateChanges<'a> {
    accounts: Option<&'a AddressMap<Account>>,
}

impl<'a> TxStateChanges<'a> {
    #[inline]
    pub(crate) const fn new(accounts: &'a AddressMap<Account>) -> Self {
        Self { accounts: Some(accounts) }
    }

    /// Returns a view with no changes.
    #[inline]
    pub const fn empty() -> Self {
        Self { accounts: None }
    }

    /// Returns whether the transaction loaded no accounts and no storage.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.accounts.is_none_or(AddressMap::is_empty)
    }

    /// Returns the current account information when the transaction loaded the account and it
    /// exists after the transaction.
    #[inline]
    pub fn account_info(&self, address: &Address) -> Option<&'a AccountInfo> {
        self.accounts?.get(address).and_then(|account| account.present.as_ref())
    }

    /// Returns each loaded account's changes, in an unspecified account order.
    #[inline]
    pub fn account_changes(self) -> impl Iterator<Item = AccountChanges<'a>> {
        self.accounts.into_iter().flat_map(|accounts| {
            accounts.iter().map(|(&address, account)| account.changes(address))
        })
    }

    /// Returns the changes of the account at `address`, or `None` if the transaction did not load
    /// it.
    #[inline]
    pub fn account_changes_for(&self, address: &Address) -> Option<AccountChanges<'a>> {
        self.accounts?.get(address).map(|account| account.changes(*address))
    }
}

impl StateChangeSource for TxStateChanges<'_> {
    /// Visits the transaction's loaded entries as [`PendingState`]'s implementation does.
    fn visit<S: StateChangeSink>(&self, sink: &mut S) -> Result<(), S::Error> {
        for changes in self.account_changes() {
            changes.visit(sink)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserts_account_and_storage() {
        let address = Address::with_last_byte(0xaa);
        let key = Word::ONE;
        let original = AccountInfo { nonce: 1, ..AccountInfo::default() };
        let current = AccountInfo { nonce: 2, ..AccountInfo::default() };
        let mut state = PendingState::default();

        state.insert_account(address, Some(original.clone()), Some(current.clone()));
        state.insert_storage(address, key, Word::from(2), Word::from(3));

        assert_eq!(state.account_info(&address), Some(&current));
        assert_eq!(state.accounts[&address].original, Some(original));
        assert_eq!(state.accounts[&address].present, Some(current));
        assert_eq!(
            state.accounts[&address].storage.slots[&key].value,
            Tracked::from_parts(Word::from(2), Word::from(3))
        );
    }

    #[test]
    fn account_changes_for_looks_up_one_address() {
        let changed = Address::with_last_byte(0xaa);
        let unchanged = Address::with_last_byte(0xbb);
        let info = AccountInfo { nonce: 1, ..AccountInfo::default() };
        let mut state = PendingState::default();
        state.insert_account(
            changed,
            Some(info.clone()),
            Some(AccountInfo { nonce: 2, ..info.clone() }),
        );
        state.insert_account(unchanged, Some(info.clone()), Some(info.clone()));
        let destroyed = Address::with_last_byte(0xdd);
        state.insert_account(destroyed, Some(info.clone()), Some(info));
        state.accounts.get_mut(&destroyed).unwrap().flags.insert(AccountFlags::DESTROYED);

        let changes = state.account_changes_for(&changed).unwrap();
        assert_eq!(changes.address, changed);
        assert!(changes.is_changed());
        let changes = state.account_changes_for(&unchanged).unwrap();
        assert_eq!(changes.address, unchanged);
        assert!(!changes.is_changed());
        let changes = state.account_changes_for(&destroyed).unwrap();
        assert!(changes.selfdestructed);
        assert!(changes.is_changed());
        assert!(state.account_changes_for(&Address::with_last_byte(0xcc)).is_none());
    }
}
