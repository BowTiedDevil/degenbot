//! EIP-7928 Block Access List state carried alongside the accepted-overlay database.

use super::{AccountBal, Bal, BalError, BlockAccessIndex, list::BalWriter};
use crate::{
    evm::state::{
        AccountChangeRef, AccountInfo, PendingState, StateChangeSink, StateChangeSource,
        StorageChange, TxStateChanges,
    },
    interpreter::Word,
};
use alloc::sync::Arc;
use alloy_eip7928::{BalAccountInfo, BlockAccessList};
use alloy_primitives::Address;
use core::convert::Infallible;

/// Result of an EIP-7928 BAL lookup during a read.
type BalResult<T> = Result<T, BalError>;

/// EIP-7928 Block Access List state: an attached BAL consulted on reads plus an optional builder
/// that accumulates one from executed transactions, both keyed at a shared block access index.
///
/// This bundles the read and write BAL machinery so the database wrapper that carries it (evm2's
/// [`CacheDB`](crate::evm::CacheDB)) is not itself BAL-oriented; the state lives on the database
/// wrapper rather than the journaled state.
///
/// The two roles are independent:
///
/// - **Reads** ([`Self::bal`]): when an attached BAL is present, [`Self::get_bal_account`] /
///   [`Self::populate_bal_account`] and [`Self::bal_storage`] serve account info and storage from
///   it at [`Self::bal_index`] (post-state per transaction). A read not covered by the BAL is
///   either an error or falls through to the database, depending on whether fallback is enabled.
/// - **Writes** ([`Self::bal_builder`]): when enabled, `Self::commit_pending` folds each committed
///   transaction's pending post-state into the builder at [`Self::bal_index`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BalContext {
    /// Optional attached EIP-7928 BAL consulted on reads.
    ///
    /// `None` (the default) disables BAL-served reads, so reads go straight to the cache/database.
    /// When `Some`, reads resolve account info and storage from the BAL at [`Self::bal_index`].
    /// Shared via [`Arc`] so the same BAL can back multiple executions.
    bal: Option<Arc<Bal>>,
    /// Optional EIP-7928 Block Access List builder.
    ///
    /// `None` (the default) disables BAL construction so normal execution pays nothing. When
    /// `Some`, [`Self::commit_pending`] folds each committed transaction's post-state into it.
    bal_builder: Option<Bal>,
    /// Current EIP-7928 block access index used by both BAL-served reads and
    /// [`Self::commit_pending`].
    ///
    /// Callers bump this once per transaction (see [`Self::bump_bal_index`]) so each transaction's
    /// writes are recorded under, and reads served at, a distinct index.
    bal_index: BlockAccessIndex,
    /// Whether reads not covered by the attached [`Self::bal`] fall back to the cache/database
    /// instead of returning a BAL coverage error.
    ///
    /// During block validation an access outside the BAL means the BAL is invalid, so this
    /// defaults to `false`. Enabling it allows executing transactions that are not part of the
    /// block (e.g. RPC calls) on top of BAL-positioned state.
    allow_db_fallback: bool,
}

impl BalContext {
    /// Creates an empty context with no attached BAL and no builder.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Attaches an EIP-7928 BAL consulted on reads, and returns `self`.
    #[inline]
    pub fn with_bal(mut self, bal: Arc<Bal>) -> Self {
        self.set_bal(bal);
        self
    }

    /// Attaches an EIP-7928 BAL consulted on reads.
    #[inline]
    pub fn set_bal(&mut self, bal: Arc<Bal>) {
        self.bal = Some(bal);
    }

    /// Returns the attached read BAL, or `None` when no BAL is attached.
    #[inline]
    pub const fn bal(&self) -> Option<&Arc<Bal>> {
        self.bal.as_ref()
    }

    /// Detaches the read BAL, so reads resolve from the cache/database again.
    ///
    /// The counterpart to [`Self::set_bal`]: without it an attached BAL cannot be
    /// removed, and a caller wanting unrestricted reads has to attach an empty one
    /// with [`Self::set_allow_db_fallback`] enabled to get the same behavior.
    #[inline]
    pub fn clear_bal(&mut self) {
        self.bal = None;
    }

    /// Sets whether reads not covered by the attached BAL fall back to the cache/database instead
    /// of returning a BAL coverage error, and returns `self`.
    #[inline]
    pub const fn with_allow_db_fallback(mut self, allow: bool) -> Self {
        self.allow_db_fallback = allow;
        self
    }

    /// Sets whether reads not covered by the attached BAL fall back to the cache/database.
    #[inline]
    pub const fn set_allow_db_fallback(&mut self, allow: bool) {
        self.allow_db_fallback = allow;
    }

    /// Enables EIP-7928 BAL construction, installing an empty builder, and returns `self`.
    #[inline]
    pub fn with_bal_builder(mut self) -> Self {
        self.enable_bal_builder();
        self
    }

    /// Enables EIP-7928 BAL construction in place, installing an empty builder.
    #[inline]
    pub fn enable_bal_builder(&mut self) {
        self.bal_builder = Some(Bal::new());
    }

    /// Returns the in-progress BAL builder, or `None` when BAL construction is disabled.
    #[inline]
    pub const fn bal_builder(&self) -> Option<&Bal> {
        self.bal_builder.as_ref()
    }

    /// Returns whether BAL construction is enabled.
    #[inline]
    pub const fn has_builder(&self) -> bool {
        self.bal_builder.is_some()
    }

    /// Returns the current EIP-7928 block access index.
    #[inline]
    pub const fn bal_index(&self) -> BlockAccessIndex {
        self.bal_index
    }

    /// Resets the block access index to the pre-execution slot (index `0`).
    ///
    /// Call this before executing a block's transactions.
    #[inline]
    pub const fn reset_bal_index(&mut self) {
        self.bal_index = BlockAccessIndex::PRE_EXECUTION;
    }

    /// Sets the block access index to the given value.
    #[inline]
    pub const fn set_bal_index(&mut self, index: BlockAccessIndex) {
        self.bal_index = index;
    }

    /// Bumps the block access index by one.
    ///
    /// Call this once per transaction so each transaction's writes are recorded under a distinct
    /// index, matching the EIP-7928 layout where transaction `i` maps to index `i + 1`.
    #[inline]
    pub const fn bump_bal_index(&mut self) {
        self.bal_index.increment();
    }

    /// Folds a detached [`PendingState`] -- a committed transaction's post-state -- into the BAL
    /// builder at the current [`Self::bal_index`].
    ///
    /// No-op when BAL construction is disabled. Loaded-but-unchanged accounts and storage slots
    /// are recorded as BAL reads; changed ones as writes.
    #[inline]
    pub fn commit_pending(&mut self, pending: &PendingState) {
        self.commit(pending.changes());
    }

    /// Folds a committed transaction's changes into the BAL builder at the current
    /// [`Self::bal_index`].
    ///
    /// Same as [`Self::commit_pending`], operating on the transaction layers directly so the
    /// overlay need not be detached. No-op when BAL construction is disabled.
    #[inline]
    pub(crate) fn commit(&mut self, changes: TxStateChanges<'_>) {
        let Some(mut writer) = self.writer() else {
            return;
        };
        let Ok(()) = changes.visit(&mut writer);
    }

    #[inline]
    fn writer(&mut self) -> Option<BalWriter<'_>> {
        let index = self.bal_index;
        self.bal_builder.as_mut().map(|bal| BalWriter::new(bal, index))
    }

    /// Takes the built BAL, resetting the block access index. Returns `None` when BAL construction
    /// is disabled.
    #[inline]
    pub const fn take_bal_builder(&mut self) -> Option<Bal> {
        self.reset_bal_index();
        self.bal_builder.take()
    }

    /// Takes the built BAL as a canonical EIP-7928 [`BlockAccessList`], resetting the block access
    /// index. Returns `None` when BAL construction is disabled.
    #[inline]
    pub fn take_alloy_bal(&mut self) -> Option<BlockAccessList> {
        self.take_bal_builder().map(BlockAccessList::from)
    }

    /// Resolves `address` in the attached read BAL.
    ///
    /// Returns `Ok(None)` when no BAL is attached, or when the account is uncovered but
    /// [`Self::set_allow_db_fallback`] is enabled. Returns [`BalError::AccountNotFound`] when the
    /// account is uncovered and fallback is disabled.
    #[inline]
    pub fn get_bal_account(&self, address: &Address) -> BalResult<Option<&AccountBal>> {
        let Some(bal) = &self.bal else {
            return Ok(None);
        };
        match bal.accounts.get(address) {
            Some(bal_account) => Ok(Some(bal_account)),
            None if self.allow_db_fallback => Ok(None),
            None => Err(BalError::AccountNotFound { address: *address }),
        }
    }

    /// Looks up account fields visible strictly before the current block access index.
    ///
    /// Returns [`BalAccountLookup::Complete`] when the BAL supplies balance, nonce, and code,
    /// allowing the caller to skip the account read from its backing database. Otherwise,
    /// [`BalAccountLookup::Partial`] carries the available fields as [`BalAccountInfo`]: `None`
    /// means the field must come from the backing account, not that it is zero or empty.
    /// Read-only entries and entries with no writes before the index are partial with no fields.
    ///
    /// Unlike [`BalAccountInfo::from_changes`], this observes the configured read position rather
    /// than taking the block's final values. To include post-execution writes for a block with
    /// `n` transactions, set the index to `n + 2`, past the post-execution index `n + 1`.
    ///
    /// Returns [`BalAccountLookup::NotCovered`] when no BAL is attached, or when the address is
    /// missing and database fallback is enabled. A missing address with fallback disabled returns
    /// [`BalError::AccountNotFound`], just like [`Self::get_bal_account`].
    #[inline]
    pub fn get_bal_account_info(&self, address: &Address) -> BalResult<BalAccountLookup> {
        let Some(account) = self.get_bal_account(address)? else {
            return Ok(BalAccountLookup::NotCovered);
        };
        let account = &account.account_info;
        let code = account.code.get(self.bal_index);
        let info = BalAccountInfo {
            balance: account.balance.get(self.bal_index).copied(),
            nonce: account.nonce.get(self.bal_index).copied(),
            code_hash: code.map(|(hash, _)| *hash),
        };
        match (info.balance, info.nonce, code) {
            (Some(balance), Some(nonce), Some((hash, code))) => Ok(BalAccountLookup::Complete(
                AccountInfo::new(balance, nonce, *hash, Some(code.clone())),
            )),
            _ => Ok(BalAccountLookup::Partial(info)),
        }
    }

    /// Applies a resolved BAL account's info writes at the current index to `account`.
    ///
    /// `bal_account` comes from [`Self::get_bal_account`], resolved before the raw account is
    /// read from the cache/database.
    #[inline]
    pub fn populate_bal_account(
        &self,
        bal_account: &AccountBal,
        account: &mut Option<AccountInfo>,
    ) {
        let was_present = account.is_some();
        let mut info = account.take().unwrap_or_default();
        let changed = bal_account.populate_account_info(self.bal_index, &mut info);
        // An account absent from the database with no BAL writes stays absent.
        if changed || was_present {
            *account = Some(info);
        }
    }

    /// Resolves storage slot `key` for `address` from the attached read BAL at the current
    /// index.
    ///
    /// Returns `Ok(Some(value))` when the BAL has a write for the slot strictly before the current
    /// index. Returns `Ok(None)` when no BAL is attached, when the slot is covered but has no
    /// applicable write (caller should read the cache/database), or when the account/slot is
    /// uncovered but [`Self::set_allow_db_fallback`] is enabled. Returns an error when the account
    /// or slot is uncovered and fallback is disabled.
    #[inline]
    pub fn bal_storage(&self, address: &Address, key: &Word) -> BalResult<Option<Word>> {
        let Some(bal) = &self.bal else {
            return Ok(None);
        };
        let Some(bal_account) = bal.accounts.get(address) else {
            if self.allow_db_fallback {
                return Ok(None);
            }
            return Err(BalError::AccountNotFound { address: *address });
        };

        match bal_account.storage.get_bal_changes(address, *key) {
            Ok(changes) => Ok(changes.get(self.bal_index).copied()),
            Err(BalError::SlotNotFound { .. }) if self.allow_db_fallback => Ok(None),
            Err(err) => Err(err),
        }
    }
}

/// Account information available from a BAL at the configured read position.
///
/// Returned by [`BalContext::get_bal_account_info`] without consulting the backing database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BalAccountLookup {
    /// Every account field is known, including decoded bytecode.
    ///
    /// This describes field completeness, not account existence. An empty account is returned as
    /// an [`AccountInfo`]; the caller decides whether state-clearing rules make it absent.
    Complete(AccountInfo),
    /// Only some account fields are known. Missing fields must be read from the backing account.
    ///
    /// An empty [`BalAccountInfo`] means no account fields are known at this position; it does
    /// not mean the account itself is empty or absent. Code changes carry their hash only.
    Partial(BalAccountInfo),
    /// No BAL is attached, or the account is missing and database fallback is enabled.
    NotCovered,
}

/// Folds streamed state changes into the BAL builder at the current [`BalContext::bal_index`], as
/// [`Bal::commit`] does. Every callback is a no-op when BAL construction is disabled.
impl StateChangeSink for BalContext {
    type Error = Infallible;

    #[inline]
    fn account(&mut self, change: AccountChangeRef<'_>) -> Result<(), Self::Error> {
        self.writer().map_or(Ok(()), |mut writer| writer.account(change))
    }

    #[inline]
    fn storage(&mut self, change: StorageChange) -> Result<(), Self::Error> {
        self.writer().map_or(Ok(()), |mut writer| writer.storage(change))
    }

    #[inline]
    fn account_read(
        &mut self,
        address: Address,
        info: Option<&AccountInfo>,
    ) -> Result<(), Self::Error> {
        self.writer().map_or(Ok(()), |mut writer| writer.account_read(address, info))
    }

    #[inline]
    fn storage_read(
        &mut self,
        address: Address,
        key: Word,
        value: Word,
    ) -> Result<(), Self::Error> {
        self.writer().map_or(Ok(()), |mut writer| writer.storage_read(address, key, value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{bytecode::Bytecode, evm::bal::BalCodeChange};
    use alloc::vec;
    use alloy_eip7928::{BalanceChange, NonceChange};
    use alloy_primitives::{Address, U256, bytes};

    const ADDRESS: Address = Address::repeat_byte(0xab);

    #[test]
    fn account_info_lookup_obeys_coverage_and_fallback() {
        let mut context = BalContext::new();
        assert_eq!(context.get_bal_account_info(&ADDRESS), Ok(BalAccountLookup::NotCovered));

        context.set_bal(Arc::new(Bal::new()));
        assert_eq!(
            context.get_bal_account_info(&ADDRESS),
            Err(BalError::AccountNotFound { address: ADDRESS }),
        );
        context.set_allow_db_fallback(true);
        assert_eq!(context.get_bal_account_info(&ADDRESS), Ok(BalAccountLookup::NotCovered));

        let mut bal = Bal::new();
        bal.accounts.insert(ADDRESS, AccountBal::default());
        context.set_bal(Arc::new(bal));
        context.set_allow_db_fallback(false);
        assert_eq!(
            context.get_bal_account_info(&ADDRESS),
            Ok(BalAccountLookup::Partial(BalAccountInfo::default())),
        );
    }

    #[test]
    fn account_info_lookup_uses_exclusive_index_and_includes_post_execution() {
        let code = Bytecode::new_raw(bytes!("6001"));
        let mut account = AccountBal::default();
        account.account_info.balance = vec![
            BalanceChange::new(BlockAccessIndex::new(0), U256::ONE),
            BalanceChange::new(BlockAccessIndex::new(1), U256::from(7)),
            // Post-execution for a two-transaction block.
            BalanceChange::new(BlockAccessIndex::new(3), U256::from(42)),
        ]
        .into();
        account.account_info.nonce = vec![NonceChange::new(BlockAccessIndex::new(2), 5)].into();
        account.account_info.code =
            vec![BalCodeChange::new(BlockAccessIndex::new(3), (code.hash_slow(), code.clone()))]
                .into();
        let mut bal = Bal::new();
        bal.accounts.insert(ADDRESS, account);
        let mut context = BalContext::new().with_bal(Arc::new(bal));

        for (index, balance, nonce) in [
            (0, None, None),
            (1, Some(U256::ONE), None),
            (2, Some(U256::from(7)), None),
            (3, Some(U256::from(7)), Some(5)),
        ] {
            context.set_bal_index(BlockAccessIndex::new(index));
            assert_eq!(
                context.get_bal_account_info(&ADDRESS),
                Ok(BalAccountLookup::Partial(BalAccountInfo { balance, nonce, code_hash: None })),
            );
        }

        context.set_bal_index(BlockAccessIndex::new(4));
        let BalAccountLookup::Complete(info) = context.get_bal_account_info(&ADDRESS).unwrap()
        else {
            panic!("all fields must be known after post-execution");
        };
        assert_eq!(info.balance, U256::from(42));
        assert_eq!(info.nonce, 5);
        assert_eq!(info.code_hash, code.hash_slow());
        assert_eq!(info.code, Some(code));
    }

    #[test]
    fn account_info_lookup_distinguishes_missing_fields_from_empty_values() {
        for fields in 0..8 {
            let index = BlockAccessIndex::new(1);
            let code = Bytecode::default();
            let mut account = AccountBal::default();
            let mut expected = BalAccountInfo::default();
            if fields & 1 != 0 {
                account.account_info.balance = vec![BalanceChange::new(index, U256::ZERO)].into();
                expected.balance = Some(U256::ZERO);
            }
            if fields & 2 != 0 {
                account.account_info.nonce = vec![NonceChange::new(index, 0)].into();
                expected.nonce = Some(0);
            }
            if fields & 4 != 0 {
                expected.code_hash = Some(code.hash_slow());
                account.account_info.code =
                    vec![BalCodeChange::new(index, (code.hash_slow(), code.clone()))].into();
            }
            let mut bal = Bal::new();
            bal.accounts.insert(ADDRESS, account);
            let mut context = BalContext::new().with_bal(Arc::new(bal));
            context.set_bal_index(BlockAccessIndex::new(2));

            let lookup = context.get_bal_account_info(&ADDRESS).unwrap();
            if expected.is_complete() {
                let BalAccountLookup::Complete(info) = lookup else {
                    panic!("all three recorded fields must produce a complete account");
                };
                assert!(info.is_empty());
                assert_eq!(info.code, Some(code));
            } else {
                assert_eq!(lookup, BalAccountLookup::Partial(expected));
            }
        }
    }

    #[test]
    fn clear_bal_restores_database_reads() {
        let mut context = BalContext::new().with_bal(Arc::new(Bal::new()));

        // Attached and empty, so an uncovered account is an error.
        assert!(context.get_bal_account(&ADDRESS).is_err());

        context.clear_bal();

        // Detached, so the read resolves from the cache/database instead.
        assert_eq!(context.get_bal_account(&ADDRESS), Ok(None));
        assert!(context.bal().is_none());
    }

    #[test]
    fn uncovered_read_returns_the_lookup_failure() {
        let context = BalContext::new().with_bal(Arc::new(Bal::new()));
        assert_eq!(
            context.get_bal_account(&ADDRESS).unwrap_err(),
            BalError::AccountNotFound { address: ADDRESS }
        );
    }
}
