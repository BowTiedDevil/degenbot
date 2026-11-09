//! Account models held by the state overlay and emitted in transitions.

use super::{
    AccountChanges, DbResult, DynDatabase, JournalEntry, StateInner, StorageHandle, StorageOverlay,
    storage_pool::StoragePool,
};
use crate::{EvmFeatures, bytecode::Bytecode, interpreter::Word};
use alloy_primitives::{Address, B256, KECCAK256_EMPTY, U256};
use bitflags::bitflags;
use derive_where::derive_where;

/// Account information loaded from the backing database or emitted in a state
/// transition.
///
/// Equality compares the persisted account fields, including the optional extension.
/// [`Self::code`] is a cache keyed by the code hash and may or may not be populated.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AccountInfo {
    /// Account balance.
    pub balance: Word,
    /// Account nonce.
    pub nonce: u64,
    /// Hash of the raw bytes in `code`, or the empty code hash.
    pub code_hash: B256,
    /// Bytecode associated with this account.
    pub code: Option<Bytecode>,
    /// Raw chain-specific data committed to the account leaf by the state provider.
    ///
    /// Empty extensions are omitted by Serde. Binary formats must delimit structs
    /// (e.g. MessagePack); bincode and Postcard account encodings are unsupported.
    #[cfg(feature = "account-ext")]
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "super::AccountExtension::is_empty")
    )]
    pub extension: super::AccountExtension,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub _non_exhaustive: (),
}

/// Compares [`AccountInfo`] by `balance`, `nonce`, and `code_hash`, skipping the
/// `code` field: `code_hash` already uniquely identifies the bytecode, so
/// comparing the bytecode itself is redundant.
impl PartialEq for AccountInfo {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        let equal = self.balance == other.balance
            && self.nonce == other.nonce
            && self.code_hash == other.code_hash;
        #[cfg(feature = "account-ext")]
        let equal = equal && self.extension == other.extension;
        equal
    }
}

impl Eq for AccountInfo {}

/// Hashes the Ethereum account fields. Extension-only differences may collide,
/// but equal accounts always hash equally.
impl core::hash::Hash for AccountInfo {
    #[inline]
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.balance.hash(state);
        self.nonce.hash(state);
        self.code_hash.hash(state);
    }
}

impl Default for AccountInfo {
    #[inline]
    fn default() -> Self {
        Self::new(U256::ZERO, 0, KECCAK256_EMPTY, Some(Bytecode::default()))
    }
}

impl AccountInfo {
    /// Creates an empty account with zero balance and nonce, the empty code hash,
    /// and no cached bytecode.
    #[inline]
    pub const fn empty() -> Self {
        Self::new(U256::ZERO, 0, KECCAK256_EMPTY, None)
    }

    /// Creates a new [`AccountInfo`] with the given fields and optional cached bytecode.
    ///
    /// `None` means the bytecode is not cached; `code_hash` still identifies the account's code.
    /// The caller is responsible for `code_hash` matching the bytecode when it is provided.
    #[inline]
    pub const fn new(balance: Word, nonce: u64, code_hash: B256, code: Option<Bytecode>) -> Self {
        Self {
            balance,
            nonce,
            code_hash,
            code,
            #[cfg(feature = "account-ext")]
            extension: super::AccountExtension::new(),
            _non_exhaustive: (),
        }
    }

    /// Clones this account without bytecode.
    #[inline]
    #[allow(clippy::missing_const_for_fn)] // Cloning a shared extension cannot be const.
    pub(crate) fn clone_no_code(&self) -> Self {
        Self {
            balance: self.balance,
            nonce: self.nonce,
            code_hash: self.code_hash,
            code: None,
            #[cfg(feature = "account-ext")]
            extension: self.extension.clone(),
            _non_exhaustive: (),
        }
    }

    /// Creates a new [`AccountInfo`] with the given code.
    #[inline]
    pub fn with_code(self, code: Bytecode) -> Self {
        Self { code_hash: code.hash_slow(), code: Some(code), ..self }
    }

    /// Creates a new [`AccountInfo`] with the given balance.
    #[inline]
    pub const fn with_balance(mut self, balance: Word) -> Self {
        self.balance = balance;
        self
    }

    /// Creates a new [`AccountInfo`] with the given nonce.
    #[inline]
    pub const fn with_nonce(mut self, nonce: u64) -> Self {
        self.nonce = nonce;
        self
    }

    /// Sets account bytecode and updates the code hash.
    #[inline]
    pub fn set_code(&mut self, code: Bytecode) {
        self.code_hash = code.hash_slow();
        self.code = Some(code);
    }

    /// Returns whether this account is empty by the Spurious Dragon definition.
    #[inline]
    pub fn is_empty(&self) -> bool {
        let empty = self.balance.is_zero() && self.nonce == 0 && self.is_empty_code_hash();
        #[cfg(feature = "account-ext")]
        let empty = empty && self.extension.is_empty();
        empty
    }

    /// Returns whether the code hash is the empty code hash.
    #[inline]
    pub fn is_empty_code_hash(&self) -> bool {
        self.code_hash == KECCAK256_EMPTY
    }
}

bitflags! {
    /// Per-account lifecycle flags tracked by the transaction overlay.
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub struct AccountFlags: u8 {
        /// The account is warm in the current transaction.
        const WARM = 1 << 0;
        /// The account is touched for transaction-finalization account-lifetime rules. Kept
        /// through finalization.
        const TOUCHED = 1 << 1;
        /// The account has been self-destructed in the current transaction. Kept through
        /// finalization, also on an EIP-8246 survivor.
        const DESTROYED = 1 << 3;
        /// The present overlay account was created in the current transaction.
        const CREATED = 1 << 4;
        /// The present overlay account's creation discarded its old storage, so unloaded slots
        /// read as zero.
        const DISCARDS_STORAGE = 1 << 5;
    }
}

/// Current-transaction account overlay and EIP-2929/account-lifetime metadata.
///
/// An entry exists in the overlay only because it was loaded, so `original` and `present` are
/// always meaningful. `original` captures the account at the start of the transaction when it is
/// first loaded, while `present` is the live overlay after EVM mutations. Keeping both lets
/// [`State`](super::State) emit the transaction's account transition without re-reading the backing
/// database. `present` is `None` for an account that was loaded as non-existent or deleted.
///
/// [`AccountFlags::CREATED`] tracks creation in the current transaction and drives
/// transaction-finalization rules. It survives selfdestruct finalization, so it can stay set while
/// `present` is `None`. [`AccountFlags::DISCARDS_STORAGE`] marks a creation that discarded the
/// account's old storage.
///
/// Detached from the EVM as part of a [`PendingState`](super::PendingState), the entry describes
/// the account's transaction transition: `original` against `present`, with
/// [`Self::is_created`] flagging in-transaction creation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Account {
    /// Account info at the start of the transaction. `None` means the account did not exist.
    pub(crate) original: Option<AccountInfo>,
    /// Present account overlay after mutations. `None` means the account is absent/deleted.
    pub(crate) present: Option<AccountInfo>,
    /// Lifecycle flags: warm, touched, destroyed, created, and discards-storage.
    pub(crate) flags: AccountFlags,
    /// Persistent transaction storage owned by this account.
    pub(crate) storage: StorageOverlay,
}

impl Account {
    /// Creates an account overlay entry from transaction-boundary and present info.
    #[cfg(test)]
    pub(crate) fn new(original: Option<AccountInfo>, present: Option<AccountInfo>) -> Self {
        Self { original, present, ..Self::default() }
    }

    /// Marks the account as created during the transaction.
    #[inline]
    pub(crate) const fn mark_created(&mut self) {
        self.flags = self.flags.union(AccountFlags::CREATED);
    }

    /// Marks the account as created and discards its old storage.
    #[inline]
    pub(crate) const fn mark_created_discarding_storage(&mut self) {
        self.mark_created();
        self.flags = self.flags.union(AccountFlags::DISCARDS_STORAGE);
    }

    /// Returns whether the account was created during the transaction.
    ///
    /// Creation is preserved across selfdestruct finalization, so it also covers accounts that
    /// were created and then destroyed in the same transaction.
    #[inline]
    pub(crate) const fn is_created(&self) -> bool {
        self.flags.contains(AccountFlags::CREATED)
    }

    /// Returns whether the account's creation discarded its storage baseline, so its unloaded
    /// slots read as zero and committing it drops cached slots.
    #[inline]
    pub(crate) const fn discards_storage(&self) -> bool {
        self.flags.contains(AccountFlags::DISCARDS_STORAGE)
    }

    /// Returns whether the account's info changed during the transaction.
    ///
    /// This compares persisted account fields and existence; it does not consider storage.
    #[inline]
    pub(crate) fn is_changed(&self) -> bool {
        self.original != self.present
    }

    /// See [`AccountHandle::is_existing_dead`].
    #[inline]
    pub(crate) fn is_existing_dead(&self) -> bool {
        self.present.as_ref().is_some_and(AccountInfo::is_empty)
    }

    /// Returns the account's new bytecode keyed by code hash when its code changed during the
    /// transaction to non-empty code.
    #[inline]
    pub(crate) fn changed_code(&self) -> Option<(B256, &Bytecode)> {
        let account = self.present.as_ref()?;
        let code = account.code.as_ref()?;
        let code_hash = account.code_hash;
        (self.original.as_ref().map(|original| original.code_hash) != Some(code_hash)
            && !code.is_empty())
        .then_some((code_hash, code))
    }

    /// Returns this account's transaction changes.
    pub(super) fn changes(&self, address: Address) -> AccountChanges<'_> {
        AccountChanges {
            address,
            original: self.original.as_ref(),
            current: self.present.as_ref(),
            created: self.is_created(),
            selfdestructed: self.flags.contains(AccountFlags::DESTROYED),
            touched: self.flags.contains(AccountFlags::TOUCHED),
            code: self.changed_code(),
            storage: &self.storage,
        }
    }
}

/// A mutable, journaled handle to an account loaded into the transaction overlay.
///
/// Returned by [`State::account`](super::State::account). The account has
/// already been read from the backing database and preserved in the transaction overlay; this
/// handle ties that overlay slot to the revert journal so a mutation and its rollback bookkeeping
/// cannot drift apart, mirroring revm's `AccountHandle`.
///
/// The first mutation made through the handle captures a snapshot of the overlay entry (present
/// value plus the warm/touched/destroyed/created flags); on drop the handle flushes
/// that snapshot as a single [`JournalEntry::AccountChange`], which
/// [`State::rollback`](super::State::rollback) replays to restore the whole entry at once. A handle
/// used only for reads records nothing, so it emits no journal entry.
///
/// The handle also carries the shared [`StateInner`] (backing database, revert journal, and
/// transaction-initial base warm set), so it can journal mutations, load code on demand, and answer
/// warm-access queries without going back through [`State`](super::State), mirroring the database
/// and access-list references revm's `AccountHandle` holds.
#[derive_where(Debug)]
pub struct AccountHandle<'a, 'db> {
    /// Address of the account.
    address: Address,
    /// Transaction overlay entry: account overlay plus warm/touched access metadata.
    tracked: &'a mut Account,
    /// Shared inner state: backing database, revert journal, and base warm set.
    #[derive_where(skip)]
    inner: &'a mut StateInner<'db>,
    /// Spare slot-map allocations reused between transactions.
    #[derive_where(skip)]
    storage_pool: &'a mut StoragePool,
    /// Revert entry capturing the overlay as it was before the first mutation made through this
    /// handle. `Some` once a change has been recorded; on drop it is pushed onto the journal as a
    /// single [`JournalEntry::AccountChange`].
    snapshot: Option<JournalEntry>,
}

impl Drop for AccountHandle<'_, '_> {
    #[inline]
    fn drop(&mut self) {
        if let Some(entry) = self.snapshot.take() {
            self.inner.journal.push(entry);
        }
    }
}

/// Returns a freshly materialized empty account overlay.
#[inline]
fn empty_account() -> AccountInfo {
    AccountInfo::default()
}

impl<'a, 'db> AccountHandle<'a, 'db> {
    /// Creates a handle over a loaded account overlay slot and the shared inner state (backing
    /// database, revert journal, and transaction-initial base warm set).
    #[inline]
    pub(super) const fn new(
        address: Address,
        tracked: &'a mut Account,
        inner: &'a mut StateInner<'db>,
        storage_pool: &'a mut StoragePool,
    ) -> Self {
        Self { address, tracked, inner, storage_pool, snapshot: None }
    }

    /// Returns this account's storage overlay without another address-map lookup.
    #[inline]
    pub fn storage(&mut self) -> StorageHandle<'_, 'db> {
        if self.tracked.storage.slots.capacity() == 0 {
            self.tracked.storage.slots = self.storage_pool.take();
        }
        let discards_storage = self.tracked.discards_storage();
        StorageHandle::new(&self.address, &mut self.tracked.storage, discards_storage, self.inner)
    }

    /// Records the pre-mutation revert entry the first time a change is made through this handle.
    /// Subsequent calls are no-ops, so the whole handle session flushes a single
    /// [`JournalEntry::AccountChange`] on drop.
    #[inline]
    fn record_change(&mut self) {
        if self.snapshot.is_none() {
            self.snapshot = Some(JournalEntry::AccountChange {
                address: self.address,
                previous: self.tracked.present.clone(),
                previous_flags: self.tracked.flags,
            });
        }
    }

    /// Returns the account address.
    #[inline]
    pub const fn address(&self) -> Address {
        self.address
    }

    /// Returns the present account overlay, or `None` when the account is absent.
    #[inline]
    pub const fn get(&self) -> Option<&AccountInfo> {
        self.tracked.present.as_ref()
    }

    /// Returns the account information at the transaction boundary, before any frame mutated it.
    ///
    /// `None` means the account did not exist at the start of the transaction. Loading bytecode
    /// into the present overlay does not change this baseline; committing and starting another
    /// transaction does. Use [`Self::original_code`] to load the account's start-of-transaction
    /// bytecode.
    #[inline]
    pub const fn original(&self) -> Option<&AccountInfo> {
        self.tracked.original.as_ref()
    }

    /// Returns whether the account currently exists in the overlay.
    #[inline]
    pub const fn exists(&self) -> bool {
        self.tracked.present.is_some()
    }

    /// Returns the account balance, or zero when the account is absent.
    #[inline]
    pub fn balance(&self) -> Word {
        self.tracked.present.as_ref().map_or(U256::ZERO, |account| account.balance)
    }

    /// Returns the account nonce, or zero when the account is absent.
    #[inline]
    pub fn nonce(&self) -> u64 {
        self.tracked.present.as_ref().map_or(0, |account| account.nonce)
    }

    /// Returns the account code hash, or the empty code hash when the account is absent.
    #[inline]
    pub fn code_hash(&self) -> B256 {
        self.tracked.present.as_ref().map_or(KECCAK256_EMPTY, |account| account.code_hash)
    }

    /// Returns whether the account code hash is the empty code hash, which includes an absent
    /// account.
    #[inline]
    pub fn is_empty_code_hash(&self) -> bool {
        self.tracked.present.as_ref().is_none_or(AccountInfo::is_empty_code_hash)
    }

    /// Returns whether the account is warm for EIP-2929 gas accounting, consulting both the
    /// transaction's base warm set (precompiles, coinbase, EIP-2930 access list) and runtime
    /// warmth recorded during execution.
    #[inline]
    pub fn is_warm(&self) -> bool {
        self.tracked.flags.contains(AccountFlags::WARM)
            || self.inner.prewarm_set.is_warm(&self.address)
    }

    /// Returns whether the account has been marked self-destructed in the current transaction.
    #[inline]
    pub const fn is_destructed(&self) -> bool {
        self.tracked.flags.contains(AccountFlags::DESTROYED)
    }

    /// Returns whether the account is touched for transaction-finalization account-lifetime rules.
    #[inline]
    pub const fn is_touched(&self) -> bool {
        self.tracked.flags.contains(AccountFlags::TOUCHED)
    }

    /// Returns whether the account was created in the current transaction.
    #[inline]
    pub const fn is_created(&self) -> bool {
        self.exists() && self.tracked.is_created()
    }

    /// Returns whether the account is dead by the EIP-161 definition while existing in the overlay.
    ///
    /// An account is existing-dead when it has zero nonce, zero balance, and empty code. Spurious
    /// Dragon deletes such touched accounts during transaction finalization.
    #[inline]
    pub fn is_existing_dead(&self) -> bool {
        self.tracked.is_existing_dead()
    }

    /// Returns whether the account is empty for EIP-150 new-account gas checks.
    ///
    /// Under EIP-161 an absent or empty account is empty; before EIP-161 only an absent, untouched
    /// account counts as empty.
    #[inline]
    pub fn is_empty_for_new_account_gas(&self, features: EvmFeatures) -> bool {
        if features.contains(EvmFeatures::EIP161) {
            return self.tracked.present.as_ref().is_none_or(AccountInfo::is_empty);
        }
        self.tracked.present.is_none() && !self.is_touched()
    }

    /// Loads the account's bytecode, reading it from the backing database by code hash when it is
    /// not already cached in the overlay.
    ///
    /// Returns empty bytecode when the account is absent or has the empty code hash.
    #[inline]
    pub fn load_code(&mut self) -> DbResult<Bytecode> {
        let Some(account) = self.tracked.present.as_ref() else {
            return Ok(Bytecode::default());
        };
        if account.is_empty_code_hash() {
            return Ok(Bytecode::default());
        }
        let code_hash = account.code_hash;
        let code = match account.code.as_ref() {
            Some(code) if !code.is_empty() => code.clone(),
            _ => self.inner.database.get_code_by_hash(&code_hash)?,
        };
        code.seed_hash(code_hash);
        Ok(code)
    }

    /// Loads the account's bytecode as it stood at the start of the transaction.
    ///
    /// Used by EIP-7702 to tell whether an authority was already delegated before this transaction
    /// (distinct from [`Self::load_code`], which reflects delegations applied by earlier
    /// authorizations in the same transaction). Returns empty bytecode when the account was absent
    /// or had empty code at the transaction boundary.
    #[inline]
    pub fn original_code(&mut self) -> DbResult<Bytecode> {
        let Some(account) = self.tracked.original.as_ref() else {
            return Ok(Bytecode::default());
        };
        if account.is_empty_code_hash() {
            return Ok(Bytecode::default());
        }
        let code_hash = account.code_hash;
        let code = match account.code.as_ref() {
            Some(code) if !code.is_empty() => code.clone(),
            _ => self.inner.database.get_code_by_hash(&code_hash)?,
        };
        code.seed_hash(code_hash);
        Ok(code)
    }

    /// Touches the account, recording a revert snapshot the first time it is mutated.
    ///
    /// Touched accounts participate in EIP-158/161 empty-account cleanup at transaction
    /// finalization even when no field changes.
    #[inline]
    pub fn touch(&mut self) {
        if !self.is_touched() {
            self.record_change();
            self.tracked.flags.insert(AccountFlags::TOUCHED);
        }
    }

    /// Marks the account self-destructed in the current transaction, recording a revert snapshot
    /// the first time and touching the account.
    ///
    /// Touching makes the account participate in EIP-158/161 cleanup, and
    /// [`State::rollback`](super::State::rollback) restores the destroyed flag.
    #[inline]
    pub fn mark_destructed(&mut self) {
        if !self.tracked.flags.contains(AccountFlags::DESTROYED) {
            self.record_change();
            self.tracked.flags.insert(AccountFlags::DESTROYED);
        }
        self.touch();
    }

    /// Marks the account warm for EIP-2929 gas accounting, recording a revert snapshot when this
    /// access transitions it from cold to warm.
    ///
    /// Returns `true` if the account was cold before this call. Accounts already warm through the
    /// base warm set stay warm across rollback, so warming them again records nothing.
    #[inline]
    pub fn warm(&mut self) -> bool {
        if self.is_warm() {
            return false;
        }
        self.record_change();
        self.tracked.flags.insert(AccountFlags::WARM);
        true
    }

    /// Sets the account balance, touching the account and recording a revert snapshot.
    #[inline]
    pub fn set_balance(&mut self, balance: Word) {
        self.touch();
        self.present_mut().balance = balance;
    }

    /// Adds a signed balance delta by wrapping two's-complement values, touching the account.
    ///
    /// A zero delta only touches the account, matching the EVM's value-bearing-call semantics.
    #[inline]
    pub fn add_balance(&mut self, delta: Word) {
        if delta.is_zero() {
            self.touch();
            return;
        }
        let balance = self.balance().wrapping_add(delta);
        self.set_balance(balance);
    }

    /// Sets the account nonce, touching the account and recording a revert snapshot.
    #[inline]
    pub fn set_nonce(&mut self, nonce: u64) {
        self.touch();
        self.present_mut().nonce = nonce;
    }

    /// Bumps the account nonce by one, touching the account and recording a revert snapshot.
    ///
    /// Returns `false` without changing the nonce when it is already at the maximum value.
    #[inline]
    pub fn bump_nonce(&mut self) -> bool {
        self.touch();
        let Some(nonce) = self.nonce().checked_add(1) else {
            return false;
        };
        self.present_mut().nonce = nonce;
        true
    }

    /// Sets the account code and its hash, touching the account and recording a revert snapshot.
    ///
    /// The caller is responsible for `code_hash` matching `code`; use [`Self::set_code_slow`] to
    /// have the hash computed.
    #[inline]
    pub fn set_code(&mut self, code_hash: B256, code: Bytecode) {
        self.touch();
        let account = self.present_mut();
        account.code_hash = code_hash;
        account.code = Some(code);
    }

    /// Sets the account code, computing its hash, touching the account and recording a revert
    /// snapshot.
    #[inline]
    pub fn set_code_slow(&mut self, code: Bytecode) {
        let code_hash = code.hash_slow();
        self.set_code(code_hash, code);
    }

    /// Applies an [EIP-7702](https://eips.ethereum.org/EIPS/eip-7702) delegation to the account.
    ///
    /// Installs an EIP-7702 delegation designator pointing at `delegated_address`, or clears the
    /// account code when `delegated_address` is zero, then bumps the account nonce.
    #[inline]
    pub fn set_delegation(&mut self, delegated_address: Address) {
        let code = if delegated_address.is_zero() {
            Bytecode::default()
        } else {
            Bytecode::new_eip7702(delegated_address)
        };
        self.set_code_slow(code);
        self.bump_nonce();
    }

    /// Records a revert snapshot and returns the live account, materializing an empty one when it
    /// is currently absent.
    #[inline]
    fn present_mut(&mut self) -> &mut AccountInfo {
        self.record_change();
        self.tracked.present.get_or_insert_with(empty_account)
    }

    /// Marks the present overlay account as created in the current transaction. The creation
    /// status is reverted together with the rest of the overlay by the
    /// [`JournalEntry::AccountChange`] snapshot recorded for this handle.
    ///
    /// Created accounts follow the EIP-6780 rules for `SELFDESTRUCT` and are emitted as created by
    /// [`PendingState`](super::PendingState). The mark does not discard the account's old storage:
    /// unloaded slots still read from the database and committing the account keeps its cached
    /// slots. This suits an embedder that continues a transaction on a fresh overlay whose database
    /// already holds the account's creation and storage writes, such as after suspending the
    /// transaction. Slots then load with the database values as their transaction originals, so
    /// `SSTORE` gas and refunds can differ from an uninterrupted run, where a created account's
    /// originals are zero.
    ///
    /// The account must be present, for example through [`Self::get_or_insert`].
    #[inline]
    pub fn mark_created(&mut self) {
        self.record_change();
        self.tracked.mark_created();
    }

    /// Marks the account as created like [`Self::mark_created`] and discards its old storage, so
    /// unloaded slots read as zero.
    #[inline]
    pub(super) fn mark_created_discarding_storage(&mut self) {
        self.record_change();
        self.tracked.mark_created_discarding_storage();
    }

    /// Records a revert snapshot and returns the live account, materializing an empty one when it
    /// is currently absent.
    #[inline]
    pub fn get_or_insert(&mut self) -> &mut AccountInfo {
        self.present_mut()
    }

    /// Updates the account payload, touching the account and recording a revert snapshot.
    #[cfg(feature = "account-ext")]
    pub fn set_extension(&mut self, extension: super::AccountExtension) {
        self.touch();
        self.present_mut().extension = extension;
    }

    /// Deletes the account at transaction finalization.
    ///
    /// Used for self-destructed accounts (pre-EIP-8246, and zero-balance accounts under EIP-8246)
    /// and EIP-161 dead-account cleanup. The caller is responsible for wiping the account's
    /// storage. Finalization runs after the last revertible scope, so the mutation is not
    /// journaled: the entry would never be replayed before [`State`](super::State) clears it.
    #[inline]
    pub(crate) fn delete_for_finalization(&mut self) {
        self.tracked.present = None;
    }

    /// Resets a self-destructed account to a balance-only account for EIP-8246 finalization.
    ///
    /// The balance is preserved while the nonce is reset to 0 and the code is cleared. The caller
    /// is responsible for clearing the account's storage. This is only called for accounts that
    /// still hold a balance; zero-balance self-destructed accounts are removed via
    /// [`Self::delete_for_finalization`] instead. Like [`Self::delete_for_finalization`], the
    /// mutation is not journaled because finalization runs after the last revertible scope.
    #[inline]
    pub(crate) fn reset_selfdestructed_for_finalization(&mut self) {
        let balance = self.balance();
        self.tracked.present = Some(empty_account().with_balance(balance));
    }

    /// Overrides the live balance and the balance restored by this handle's revert snapshot,
    /// materializing an empty account when absent without touching it.
    ///
    /// Earlier handles' journal entries remain unchanged and can still restore older values.
    #[inline]
    pub fn override_balance(&mut self, balance: Word) {
        self.present_mut().balance = balance;
        if let Some(JournalEntry::AccountChange { previous, .. }) = &mut self.snapshot {
            previous.get_or_insert_with(empty_account).balance = balance;
        }
    }

    /// Overrides the live nonce and the nonce restored by this handle's revert snapshot,
    /// materializing an empty account when absent without touching it.
    ///
    /// Earlier handles' journal entries remain unchanged and can still restore older values.
    #[inline]
    pub fn override_nonce(&mut self, nonce: u64) {
        self.present_mut().nonce = nonce;
        if let Some(JournalEntry::AccountChange { previous, .. }) = &mut self.snapshot {
            previous.get_or_insert_with(empty_account).nonce = nonce;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        LoadError, SpecId, Version,
        evm::{CacheDB, state::State},
    };
    use alloy_primitives::{Address, Bytes};

    #[test]
    fn transfer_to_self_requires_balance() {
        let address = Address::repeat_byte(0x77);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, empty_account().with_balance(Word::from(3)));
        let mut state = State::new(database);

        assert!(!state.transfer(&address, &address, &Word::from(4)).unwrap());
        assert!(state.transfer(&address, &address, &Word::from(3)).unwrap());
    }

    #[test]
    fn journaled_account_mutations_journal_and_roll_back() {
        let address = Address::repeat_byte(0x88);
        let mut state = State::new(CacheDB::default());

        let checkpoint = state.checkpoint();
        {
            let mut account = state.account(&address).unwrap();
            assert!(!account.exists());
            assert!(account.warm(), "first access is cold");
            assert!(!account.warm(), "second access is warm");
            account.set_balance(Word::from(100));
            account.set_nonce(7);
            assert!(account.bump_nonce());
            account.set_code_slow(Bytecode::new_raw(alloy_primitives::Bytes::from_static(&[
                0x60, 0x00,
            ])));
        }

        assert!(state.account(&address).unwrap().is_warm());
        let info = state
            .account_info_untracked(&address)
            .unwrap()
            .expect("account materialized by mutation");
        assert_eq!(info.balance, Word::from(100));
        assert_eq!(info.nonce, 8);
        assert!(!info.is_empty_code_hash());

        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert!(!state.account(&address).unwrap().is_warm());
        assert!(state.account_info_untracked(&address).unwrap().is_none());
        assert!(!state.take_pending_state().is_changed());
    }

    #[test]
    fn account_info_set_code_is_published_on_commit() {
        let address = Address::repeat_byte(0x8c);
        let code = Bytecode::new_raw(Bytes::from_static(&[0x60, 0x01]));
        let mut state = State::new(CacheDB::default());

        state.account(&address).unwrap().get_or_insert().set_code(code.clone());
        state.commit_transaction();

        let loaded = state.account(&address).unwrap().load_code().unwrap();
        assert_eq!(loaded, code);
    }

    #[test]
    fn changed_code_requires_new_non_empty_code_hash() {
        let code = Bytecode::new_raw(Bytes::from_static(&[0x60, 0x01]));
        let with_code = empty_account().with_code(code.clone());
        let other = empty_account().with_code(Bytecode::new_raw(Bytes::from_static(&[0])));

        let created = Account::new(None, Some(with_code.clone()));
        assert_eq!(created.changed_code(), Some((with_code.code_hash, &code)));

        let replaced = Account::new(Some(other), Some(with_code.clone()));
        assert_eq!(replaced.changed_code(), Some((with_code.code_hash, &code)));

        assert_eq!(
            Account::new(Some(with_code.clone()), Some(with_code.clone())).changed_code(),
            None
        );
        assert_eq!(Account::new(Some(with_code), Some(empty_account())).changed_code(), None);
        assert_eq!(Account::new(None, Some(empty_account())).changed_code(), None);
    }

    #[test]
    fn loaded_code_is_seeded_with_account_code_hash() {
        // A hash that is not the keccak of the code proves `hash_slow` returns the seeded value
        // instead of hashing.
        let seed = B256::repeat_byte(0x5e);
        let address = Address::repeat_byte(0x8d);
        let fresh = || Bytecode::new_raw(Bytes::from_static(&[0x60, 0x01]));
        assert_ne!(fresh().hash_slow(), seed);

        let mut database = CacheDB::default();
        database
            .insert_account_info(&address, AccountInfo::new(Word::ZERO, 1, seed, Some(fresh())));
        let mut state = State::new(database);

        assert_eq!(state.account(&address).unwrap().load_code().unwrap().hash_slow(), seed);
        assert_eq!(state.account(&address).unwrap().original_code().unwrap().hash_slow(), seed);

        let overlay_address = Address::repeat_byte(0x8e);
        state.account(&overlay_address).unwrap().set_code(seed, fresh());
        assert_eq!(state.account(&overlay_address).unwrap().load_code().unwrap().hash_slow(), seed);
    }

    #[test]
    fn account_original_reports_transaction_baseline() {
        let address = Address::repeat_byte(0x93);
        let code = Bytecode::new_raw(Bytes::from_static(&[0x60, 0x01]));
        let original =
            empty_account().with_balance(Word::from(10)).with_nonce(3).with_code(code.clone());
        let mut database = CacheDB::default();
        database.insert_account_info(&address, original.clone());
        let mut state = State::new(database);

        // An unchanged account reports its start-of-transaction info.
        {
            let account = state.account(&address).unwrap();
            assert_eq!(account.original(), Some(&original));
            assert_eq!(account.get(), Some(&original));
        }

        // Mutations move the present overlay but leave the baseline alone.
        {
            let mut account = state.account(&address).unwrap();
            account.set_balance(Word::from(99));
            account.set_nonce(4);
            assert_eq!(account.original(), Some(&original));
            assert_eq!(account.balance(), Word::from(99));
            assert_eq!(account.nonce(), 4);
        }

        // Loading bytecode into the present cache does not move the baseline either.
        {
            let mut account = state.account(&address).unwrap();
            assert_eq!(account.load_code().unwrap(), code);
            assert_eq!(account.original(), Some(&original));
        }

        // An account absent at the transaction boundary keeps no baseline, including after the
        // transaction creates it.
        let created = Address::repeat_byte(0x94);
        {
            let mut account = state.account(&created).unwrap();
            assert_eq!(account.original(), None);
            account.get_or_insert().balance = Word::ONE;
            account.mark_created();
            assert_eq!(account.original(), None);
            assert_eq!(account.balance(), Word::ONE);
        }
    }

    #[test]
    fn journaled_existing_account_field_changes_roll_back_granularly() {
        let address = Address::repeat_byte(0x8b);
        let original_code = Bytecode::new_raw(Bytes::from_static(&[0x60, 0x01]));
        let original_code_hash = original_code.hash_slow();
        let mut database = CacheDB::default();
        database.insert_account_info(
            &address,
            empty_account().with_balance(Word::from(10)).with_nonce(2).with_code(original_code),
        );
        let mut state = State::new(database);

        let checkpoint = state.checkpoint();
        {
            let mut account = state.account(&address).unwrap();
            account.set_balance(Word::from(999));
            account.set_nonce(5);
            assert!(account.bump_nonce());
            account.set_code_slow(Bytecode::new_raw(Bytes::from_static(&[0x60, 0x02])));
        }

        let info = state.account_info_untracked(&address).unwrap().expect("account exists");
        assert_eq!(info.balance, Word::from(999));
        assert_eq!(info.nonce, 6);
        assert_ne!(info.code_hash, original_code_hash);

        // Each field reverts independently from its own granular journal entry, leaving the loaded
        // account in place with its original values and emitting no state change.
        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        let info = state.account_info_untracked(&address).unwrap().expect("account still loaded");
        assert_eq!(info.balance, Word::from(10));
        assert_eq!(info.nonce, 2);
        assert_eq!(info.code_hash, original_code_hash);
        assert!(!state.take_pending_state().is_changed());
    }

    #[test]
    fn journaled_account_read_only_handle_journals_nothing() {
        let address = Address::repeat_byte(0x89);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, empty_account().with_balance(Word::from(5)));
        let mut state = State::new(database);

        let checkpoint = state.checkpoint();
        {
            let account = state.account(&address).unwrap();
            assert_eq!(account.balance(), Word::from(5));
            assert_eq!(account.nonce(), 0);
        }
        // Loading preserves the account but a read-only handle records no transition.
        state.rollback(checkpoint, crate::Version::base(crate::SpecId::FRONTIER).features);
        assert!(!state.take_pending_state().is_changed());
    }

    #[test]
    fn journaled_account_skip_cold_load_signals_skip() {
        let address = Address::repeat_byte(0x8a);
        let mut database = CacheDB::default();
        database.insert_account_info(&address, empty_account().with_balance(Word::from(5)));
        let mut state = State::new(database);

        // A cold, not-yet-loaded account signals the skip instead of reading the database.
        assert!(matches!(state.account_with_skip(&address, true), Err(LoadError::ColdLoadSkipped)));
        // Skipping leaves the overlay untouched, so a later non-skipped load still works.
        assert_eq!(state.account(&address).unwrap().balance(), Word::from(5));
        // Residency alone does not make a cold access affordable: a loaded-but-cold account still
        // signals the skip, since warmth — not overlay residency — decides the cold surcharge.
        assert!(matches!(state.account_with_skip(&address, true), Err(LoadError::ColdLoadSkipped)));
        // Once warmed, the affordable warm access yields a handle even when skipping is requested.
        state.account(&address).unwrap().warm();
        assert!(state.account_with_skip(&address, true).is_ok());
    }

    #[test]
    fn account_overrides_update_only_selected_snapshot_fields() {
        let address = Address::with_last_byte(0x90);
        let original = empty_account().with_balance(Word::from(100)).with_nonce(7);
        for (override_balance, override_nonce) in [(true, false), (false, true), (true, true)] {
            let mut db = CacheDB::default();
            db.insert_account_info(&address, original.clone());
            let mut state = State::new(db);
            let checkpoint = state.checkpoint();
            {
                let mut account = state.account(&address).unwrap();
                account.set_balance(Word::from(200));
                account.set_nonce(8);
                account.warm();
                account.mark_created();
                account.mark_destructed();
                account.set_code_slow(Bytecode::new_eip7702(Address::with_last_byte(1)));
                if override_balance {
                    account.override_balance(Word::from(300));
                    account.override_balance(Word::from(400));
                    assert_eq!(account.balance(), Word::from(400));
                }
                if override_nonce {
                    account.override_nonce(9);
                    account.override_nonce(10);
                    assert_eq!(account.nonce(), 10);
                }
                account.add_balance(Word::from(5));
                assert!(account.bump_nonce());
            }
            assert_eq!(state.checkpoint().journal_len(), checkpoint.journal_len() + 1);

            state.rollback(checkpoint, crate::Version::base(crate::SpecId::CANCUN).features);
            let account = state.account(&address).unwrap();
            let mut expected = original.clone();
            if override_balance {
                expected.balance = Word::from(400);
            }
            if override_nonce {
                expected.nonce = 10;
            }
            assert_eq!(account.get(), Some(&expected));
            assert!(!account.is_touched());
            assert!(!account.is_warm());
            assert!(!account.is_created());
            assert!(!account.is_destructed());
        }
    }

    #[test]
    fn account_overrides_materialize_absent_snapshot_without_touching() {
        let address = Address::with_last_byte(0x91);
        for (balance, nonce) in [(Word::ZERO, 0), (Word::MAX, u64::MAX)] {
            for balance_first in [false, true] {
                let mut state = State::new(CacheDB::default());
                let checkpoint = state.checkpoint();
                {
                    let mut account = state.account(&address).unwrap();
                    if balance_first {
                        account.override_balance(balance);
                        account.override_nonce(nonce);
                    } else {
                        account.override_nonce(nonce);
                        account.override_balance(balance);
                    }
                    assert_eq!((account.balance(), account.nonce()), (balance, nonce));
                    assert!(!account.is_touched());
                    assert!(!account.is_warm());
                    account.set_balance(Word::ONE);
                    account.set_nonce(1);
                }
                assert_eq!(state.checkpoint().journal_len(), checkpoint.journal_len() + 1);

                state.rollback(checkpoint, crate::Version::base(crate::SpecId::CANCUN).features);
                let account = state.account(&address).unwrap();
                assert!(account.exists());
                assert_eq!((account.balance(), account.nonce()), (balance, nonce));
                assert!(!account.is_touched());
                assert!(!account.is_warm());
            }
        }
    }

    #[test]
    fn account_overrides_preserve_earlier_handles_snapshots() {
        let address = Address::with_last_byte(0x92);
        let original = empty_account().with_balance(Word::from(100)).with_nonce(7);
        let mut db = CacheDB::default();
        db.insert_account_info(&address, original.clone());
        let mut state = State::new(db);
        let features = crate::Version::base(crate::SpecId::CANCUN).features;
        let parent = state.checkpoint();
        {
            let mut account = state.account(&address).unwrap();
            account.set_balance(Word::from(200));
            account.set_nonce(8);
        }
        let child = state.checkpoint();
        {
            let mut account = state.account(&address).unwrap();
            account.override_balance(Word::from(300));
            account.override_nonce(9);
        }
        {
            let mut account = state.account(&address).unwrap();
            account.set_balance(Word::from(400));
            account.set_nonce(10);
        }

        state.rollback(child, features);
        {
            let account = state.account(&address).unwrap();
            assert_eq!((account.balance(), account.nonce()), (Word::from(300), 9));
        }
        state.rollback(parent, features);
        assert_eq!(state.account(&address).unwrap().get(), Some(&original));
    }
}
