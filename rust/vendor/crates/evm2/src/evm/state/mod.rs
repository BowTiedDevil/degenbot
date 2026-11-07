//! Basic in-memory EVM host state.

mod account;
mod block;
#[cfg(feature = "account-ext")]
mod extension;
mod journal;
mod pending;
mod storage;
mod storage_pool;
mod stream;
mod tracked;

pub(crate) use account::Account;
pub use account::{AccountFlags, AccountHandle, AccountInfo};
pub use block::BlockStateAccumulator;
#[cfg(feature = "account-ext")]
pub use extension::AccountExtension;
pub use journal::{JournalEntry, StateCheckpoint};
pub use pending::{PendingState, TxStateChanges};
pub use storage::{StorageHandle, StorageOverlay, StorageSlot, StorageSlotHandle};
pub use stream::{
    AccountChangeRef, AccountChanges, NoopChangeSink, StateChangeSink, StateChangeSource,
    StorageChange, Tee,
};
pub use tracked::Tracked;

use super::{
    PrewarmSet,
    bal::{Bal, BalContext, BlockAccessIndex},
    db::{Cache, CacheDB, DbResult, DynDatabase, EmptyDB, boxed_dyn_database},
};
use crate::{
    EvmFeatures, LoadError, Version,
    bytecode::Bytecode,
    interpreter::{InstrStop, Word},
    storage_key::{StorageKey, StorageKeyMap},
};
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use alloy_primitives::{
    Address, B256, KECCAK256_EMPTY, Log,
    map::{AddressMap, hash_map},
};
use core::{
    mem,
    ops::{Deref, DerefMut},
};
use derive_where::derive_where;

/// Mutable EVM state with an accepted-state cache, transaction layer, and reversible journal.
#[derive(Debug)]
#[non_exhaustive]
pub struct State<'a> {
    /// Account writes plus touch and warm-access metadata for the current transaction.
    accounts: AddressMap<Account>,
    /// Empty slot-map allocations retained between transactions, under fixed capacity limits.
    storage_pool: storage_pool::StoragePool,
    /// Transaction-scoped EIP-1153 transient storage keyed by account address and slot.
    transient_storage: StorageKeyMap<Word>,
    /// Inner state.
    inner: StateInner<'a>,
}

/// Owned in-memory state without a backing database.
///
/// This can be kept across executions or sent to another thread. Restoring it requires supplying
/// the database that should serve uncached reads.
#[derive(Clone, Debug)]
pub struct StateSnapshot {
    accounts: AddressMap<Account>,
    transient_storage: StorageKeyMap<Word>,
    cache: Cache,
    bal_context: BalContext,
    prewarm_set: PrewarmSet,
    journal: Vec<JournalEntry>,
    logs: Vec<Log>,
}

impl StateSnapshot {
    /// Restores the captured state over a backing database.
    pub fn into_state<'a>(self, db: impl DynDatabase + 'a) -> State<'a> {
        State {
            accounts: self.accounts,
            storage_pool: storage_pool::StoragePool::default(),
            transient_storage: self.transient_storage,
            inner: StateInner {
                database: CacheDB {
                    cache: self.cache,
                    db: boxed_dyn_database(db),
                    bal_context: self.bal_context,
                    _non_exhaustive: (),
                },
                prewarm_set: self.prewarm_set,
                journal: self.journal,
                logs: self.logs,
            },
        }
    }
}

/// Clones in-memory state with [`EmptyDB`] as the backing database.
/// Use [`State::clone_with`] to supply a database.
impl Clone for State<'_> {
    fn clone(&self) -> Self {
        self.clone_with(EmptyDB::default())
    }
}

impl State<'_> {
    /// Captures all in-memory state without retaining the backing database.
    pub fn snapshot(&self) -> StateSnapshot {
        StateSnapshot {
            accounts: self.accounts.clone(),
            transient_storage: self.transient_storage.clone(),
            cache: self.database.cache.clone(),
            bal_context: self.database.bal_context.clone(),
            prewarm_set: self.prewarm_set.clone(),
            journal: self.journal.clone(),
            logs: self.logs.clone(),
        }
    }

    /// Clones in-memory state with `db` as the backing database.
    pub fn clone_with<'a>(&self, db: impl DynDatabase + 'a) -> State<'a> {
        self.snapshot().into_state(db)
    }

    /// Returns a state over `db` whose accepted overlay is this state's accepted overlay with the
    /// current transaction's writes committed, and whose transaction scratch is empty.
    ///
    /// This is `clone_with` followed by [`Self::commit_transaction`] and
    /// [`Self::clear_transaction_state`], without copying the journal, logs, transient storage or
    /// pre-warmed set, so its cost does not grow with the running transaction's journal. Use it as
    /// the base state of a transaction nested in the running one.
    pub fn committed_copy<'a>(&self, db: impl DynDatabase + 'a) -> State<'a> {
        let mut database = CacheDB {
            cache: self.database.cache.clone(),
            db: boxed_dyn_database(db),
            bal_context: self.database.bal_context.clone(),
            _non_exhaustive: (),
        };
        database.commit(&self.accounts);
        State {
            accounts: AddressMap::default(),
            storage_pool: storage_pool::StoragePool::default(),
            transient_storage: StorageKeyMap::default(),
            inner: StateInner {
                database,
                journal: Vec::new(),
                logs: Vec::new(),
                prewarm_set: PrewarmSet::new(),
            },
        }
    }
}

impl<'a> Deref for State<'a> {
    type Target = StateInner<'a>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<'a> DerefMut for State<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// Shared inner state borrowed by journaled mutation handles.
///
/// Holds the parts of [`State`] that a [`AccountHandle`] or [`StorageHandle`] needs while it
/// borrows an account or storage overlay: the backing database, the revert journal, and the
/// pre-warmed set. Splitting these out of [`State`] lets a handle borrow them
/// together as one `&mut StateInner` disjointly from the account/storage maps it mutates. [`State`]
/// derefs to this type, so its fields and methods are reachable directly on a [`State`].
#[derive_where(Debug)]
#[non_exhaustive]
pub struct StateInner<'a> {
    /// Database plus accepted transaction-boundary state overlay.
    #[derive_where(skip)]
    database: CacheDB<Box<dyn DynDatabase + 'a>>,
    /// Revert journal.
    journal: Vec<JournalEntry>,
    /// Logs emitted by the current transaction.
    logs: Vec<Log>,
    /// Pre-warmed set: precompiles, coinbase, and the EIP-2930 access list.
    prewarm_set: PrewarmSet,
}

impl StateInner<'_> {
    /// Appends a journal entry, building it straight in the journal's buffer.
    ///
    /// `Vec::push` builds the entry on the stack, because it must survive a possible reallocation,
    /// and then copies it with wide loads. Those loads cannot take their data from the narrower
    /// stores that built the entry and stall until the stores reach the cache.
    #[inline(always)]
    fn push_journal(&mut self, entry: impl FnOnce() -> JournalEntry) {
        self.journal.reserve(1);
        let len = self.journal.len();
        // SAFETY: `reserve` made room for one more entry.
        unsafe {
            self.journal.as_mut_ptr().add(len).write(entry());
            self.journal.set_len(len + 1);
        }
    }

    /// Reads a slot absent from the overlay.
    ///
    /// A slot of a wiped account, or of an account whose creation discarded its old storage, reads
    /// zero without a database read, though a strict BAL must still cover it.
    #[inline]
    fn read_unloaded_slot(
        &mut self,
        address: &Address,
        key: &Word,
        reads_zero: bool,
    ) -> DbResult<Word> {
        if reads_zero {
            self.database.check_bal_storage(address, key)?;
            Ok(Word::ZERO)
        } else {
            self.database.get_storage(address, key)
        }
    }
}

impl<'a> State<'a> {
    /// Creates a new state over an initial database.
    pub fn new(initial: impl DynDatabase + 'a) -> Self {
        Self::new_mono(boxed_dyn_database(initial))
    }

    pub(crate) fn new_mono(initial: Box<dyn DynDatabase + 'a>) -> Self {
        Self {
            accounts: AddressMap::default(),
            storage_pool: storage_pool::StoragePool::default(),
            transient_storage: StorageKeyMap::default(),
            inner: StateInner {
                database: CacheDB::new(initial),
                journal: Vec::new(),
                logs: Vec::new(),
                prewarm_set: PrewarmSet::new(),
            },
        }
    }

    /// Returns a checkpoint for later rollback.
    #[inline]
    pub const fn checkpoint(&self) -> StateCheckpoint {
        StateCheckpoint::new(self.inner.journal.len(), self.inner.logs.len())
    }

    /// Returns the initial database.
    #[inline]
    pub fn initial(&self) -> &(dyn DynDatabase + 'a) {
        self.database.db.as_ref()
    }

    /// Returns the initial database mutably.
    #[inline]
    pub fn initial_mut(&mut self) -> &mut (dyn DynDatabase + 'a) {
        self.database.db.as_mut()
    }

    /// Replaces the initial database and clears all in-memory state layers.
    #[inline]
    pub fn set_initial(&mut self, initial: impl DynDatabase + 'a) {
        self.database = CacheDB::new(boxed_dyn_database(initial));
        self.clear_transaction_state();
    }

    /// Consumes the state and returns the current backing database, dropping every in-memory
    /// layer.
    ///
    /// The in-memory layers — accepted state, the journal, and transaction scratch — are never
    /// written through to the database, so nothing the EVM executed reaches it. The database
    /// itself is returned as-is and may differ from the one passed to [`Self::new`]: it can be
    /// replaced through [`Self::set_initial`], mutated through [`Self::initial_mut`], or
    /// changed by its own reads.
    #[inline]
    pub fn into_db(self) -> Box<dyn DynDatabase + 'a> {
        self.inner.database.db
    }

    /// Returns the accepted-state overlay database.
    #[inline]
    pub fn overlay_db(&self) -> &CacheDB<Box<dyn DynDatabase + 'a>> {
        &self.inner.database
    }

    /// Returns the accepted-state overlay database mutably.
    #[inline]
    pub fn overlay_db_mut(&mut self) -> &mut CacheDB<Box<dyn DynDatabase + 'a>> {
        &mut self.inner.database
    }

    /// Applies borrowed changes to the accepted state overlay.
    #[inline]
    pub fn commit_source<S: StateChangeSource>(&mut self, source: &S) {
        self.inner.database.commit_source(source);
    }

    /// Attaches an EIP-7928 BAL that the accepted-overlay database consults on reads.
    ///
    /// Once attached, account-info and storage reads are served from the BAL at the current block
    /// access index (layered over the cache/database). Reads not covered by the BAL error unless
    /// [`Self::set_allow_bal_db_fallback`] is enabled.
    #[inline]
    pub fn set_bal(&mut self, bal: Arc<Bal>) {
        self.inner.database.bal_context.set_bal(bal);
    }

    /// Returns the attached read BAL, or `None` when no BAL is attached.
    #[inline]
    pub const fn bal(&self) -> Option<&Arc<Bal>> {
        self.inner.database.bal_context.bal()
    }

    /// Detaches the read BAL, so reads resolve from the cache/database again.
    #[inline]
    pub fn clear_bal(&mut self) {
        self.inner.database.bal_context.clear_bal();
    }

    /// Sets whether reads not covered by the attached BAL fall back to the cache/database instead
    /// of erroring.
    #[inline]
    pub const fn set_allow_bal_db_fallback(&mut self, allow: bool) {
        self.inner.database.bal_context.set_allow_db_fallback(allow);
    }

    /// Enables EIP-7928 Block Access List construction on the accepted-overlay database.
    ///
    /// Once enabled, every committed transaction is folded into the builder at the current block
    /// access index. Bump the index once per transaction with [`Self::bump_bal_index`]. The BAL
    /// state lives in the accepted-overlay [`CacheDB`]'s
    /// [`BalContext`](crate::evm::BalContext).
    #[inline]
    pub fn enable_bal_builder(&mut self) {
        self.inner.database.bal_context.enable_bal_builder();
    }

    /// Returns the in-progress BAL builder, or `None` when BAL construction is disabled.
    #[inline]
    pub const fn bal_builder(&self) -> Option<&Bal> {
        self.inner.database.bal_context.bal_builder()
    }

    /// Returns the current EIP-7928 block access index.
    #[inline]
    pub const fn bal_index(&self) -> BlockAccessIndex {
        self.inner.database.bal_context.bal_index()
    }

    /// Resets the BAL block access index to the pre-execution slot. Call before a block's
    /// transactions.
    #[inline]
    pub const fn reset_bal_index(&mut self) {
        self.inner.database.bal_context.reset_bal_index();
    }

    /// Sets the BAL block access index to the given value. Use to position reads at an arbitrary
    /// transaction index, e.g. when executing on top of a read BAL mid-block.
    #[inline]
    pub const fn set_bal_index(&mut self, index: BlockAccessIndex) {
        self.inner.database.bal_context.set_bal_index(index);
    }

    /// Bumps the BAL block access index by one. Call once per transaction.
    #[inline]
    pub const fn bump_bal_index(&mut self) {
        self.inner.database.bal_context.bump_bal_index();
    }

    /// Takes the built BAL, resetting the block access index. Returns `None` when BAL construction
    /// is disabled.
    #[inline]
    pub const fn take_bal_builder(&mut self) -> Option<Bal> {
        self.inner.database.bal_context.take_bal_builder()
    }

    /// Loads a historical block hash.
    #[inline]
    pub(crate) fn block_hash(&mut self, number: &Word) -> DbResult<B256> {
        self.database.get_block_hash(number)
    }

    /// Returns logs emitted by the current in-flight transaction.
    #[inline]
    pub fn logs(&self) -> &[Log] {
        &self.logs
    }

    /// Returns the transaction logs mutably, without notifying the inspector.
    ///
    /// This allows restoring state while retaining the current logs.
    #[inline]
    pub const fn logs_mut(&mut self) -> &mut Vec<Log> {
        &mut self.inner.logs
    }

    /// Returns the revert journal for the current in-flight transaction.
    #[inline]
    pub fn journal(&self) -> &[JournalEntry] {
        &self.inner.journal
    }

    /// Returns the current value of a storage slot from the transaction overlay, if it has been
    /// loaded or written this transaction.
    ///
    /// This is a non-loading `&self` peek of the transaction overlay only: it does not consult the
    /// accepted overlay or backing database, so it returns `None` for a slot not touched this
    /// transaction. Use [`Self::storage_slot_untracked`] to read through to the database.
    #[inline]
    pub fn get_storage(&self, address: &Address, key: &Word) -> Option<Word> {
        self.accounts.get(address)?.storage.slots.get(key).map(|slot| slot.value.current)
    }

    /// Returns the accounts in the transaction overlay, each with the keys of its loaded storage
    /// slots.
    ///
    /// This includes accounts loaded as absent and slots that were only read. Overlay entries
    /// survive [`Self::rollback`], so reads in reverted frames are included, as access-list
    /// builders need. Accounts installed by [`Self::set_pending_state`] are included; entries of
    /// the accepted-state overlay that the transaction did not load are not.
    pub fn loaded_accounts(&self) -> impl Iterator<Item = (&Address, impl Iterator<Item = &Word>)> {
        self.accounts.iter().map(|(address, account)| (address, account.storage.slots.keys()))
    }

    /// Sets the warmth of an already-loaded storage slot without journaling the change.
    ///
    /// Does nothing for unloaded slots and leaves values and the pre-warmed set unchanged.
    /// Slots in the pre-warmed set remain effectively warm even when their runtime warmth is
    /// cleared.
    ///
    /// This call adds no journal entry; rolling back an existing storage-warming entry can
    /// still clear the slot's runtime warmth.
    pub fn set_storage_warm(&mut self, address: &Address, key: Word, warm: bool) {
        if let Some(slot) =
            self.accounts.get_mut(address).and_then(|account| account.storage.slots.get_mut(&key))
        {
            slot.is_warm = warm;
        }
    }

    /// Sets the warmth of an already-loaded account without journaling the change.
    ///
    /// Does nothing for unloaded accounts and leaves account values, storage, and the pre-warmed
    /// set unchanged. Addresses in the pre-warmed set remain effectively warm even when their
    /// runtime warmth is cleared.
    ///
    /// This call adds no journal entry; rolling back an existing account-change entry still
    /// restores the account's recorded runtime warmth.
    pub fn set_account_warm(&mut self, address: &Address, warm: bool) {
        if let Some(account) = self.accounts.get_mut(address) {
            account.flags.set(AccountFlags::WARM, warm);
        }
    }

    /// Reads a storage slot from the committed state (accepted overlay and backing database),
    /// ignoring the in-flight transaction overlay.
    #[inline]
    pub fn read_committed_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        self.inner.database.get_storage(address, key)
    }

    /// Takes logs emitted by the current in-flight transaction.
    #[inline]
    pub(crate) fn take_logs(&mut self) -> Vec<Log> {
        mem::take(&mut self.inner.logs)
    }

    /// Records a transaction log.
    #[inline]
    pub(crate) fn log(&mut self, log: Log) {
        self.logs.push(log);
    }

    /// Returns the pre-warmed set (precompiles, coinbase, access list).
    ///
    /// This is not the complete EIP-2929 initial warm set -- sender and recipient are warmed per
    /// account instead. See [`PrewarmSet`].
    #[inline]
    #[must_use]
    pub const fn prewarm_set(&self) -> &PrewarmSet {
        &self.inner.prewarm_set
    }

    /// Returns the pre-warmed set mutably.
    ///
    /// This lets callers replace the set (`*state.prewarm_set_mut() = set`) or edit it in place,
    /// for example with [`PrewarmSet::remove`] to cool an address for a simulated transaction
    /// boundary. Changes are not journaled and leave the runtime warmth of loaded accounts and
    /// slots unchanged.
    #[inline]
    pub const fn prewarm_set_mut(&mut self) -> &mut PrewarmSet {
        &mut self.inner.prewarm_set
    }

    /// Marks an address as warm in the pre-warmed set. See [`PrewarmSet::warm`].
    #[inline]
    pub fn prewarm(&mut self, address: &Address) {
        self.inner.prewarm_set.warm(address);
    }

    /// Marks an address and a set of storage slots as warm in the pre-warmed set. See
    /// [`PrewarmSet::warm_storage`].
    #[inline]
    pub fn prewarm_storage(&mut self, address: &Address, slots: impl IntoIterator<Item = Word>) {
        self.inner.prewarm_set.warm_storage(address, slots);
    }

    /// Marks an address and a single storage slot as warm in the pre-warmed set.
    /// See [`PrewarmSet::warm_storage`].
    #[inline]
    pub fn prewarm_storage_slot(&mut self, address: &Address, key: Word) {
        self.prewarm_storage(address, [key]);
    }

    /// Clears transaction-scoped substate.
    pub fn clear_transaction_state(&mut self) {
        let Self {
            accounts,
            storage_pool,
            transient_storage,
            inner: StateInner { prewarm_set, journal, logs, database: _ },
            ..
        } = self;
        for account in accounts.values_mut() {
            storage_pool.clear_overlay(&mut account.storage);
        }
        accounts.clear();
        transient_storage.clear();
        prewarm_set.clear();
        journal.clear();
        logs.clear();
    }

    /// Loads an account without skipping cold accesses.
    #[inline(always)]
    fn account_raw<'h>(
        inner: &mut StateInner<'a>,
        accounts: &'h mut AddressMap<Account>,
        address: &Address,
    ) -> DbResult<&'h mut Account> {
        Self::account_raw_with_skip(inner, accounts, address, false).map_err(|error| match error {
            LoadError::Database(error) => error,
            LoadError::ColdLoadSkipped => unreachable!("cold-load skipping is disabled"),
        })
    }

    /// Ensures the account is present in the transaction overlay, loading it from the backing
    /// database when it has not been loaded yet.
    ///
    /// When `skip_cold` is true and the account is cold, the access
    /// is skipped and [`LoadError::ColdLoadSkipped`] is returned, leaving the overlay
    /// untouched. This mirrors revm's `skip_cold_load`/`ColdLoadSkipped` so callers can detect a
    /// cold access without paying for the load. With `skip_cold` false (or when the account is
    /// warm) the entry is loaded.
    ///
    /// The load itself is not journaled: the loaded entry holds `original == present` and is a
    /// harmless read cache that [`Self::rollback`] leaves in place. Only later warmth and value
    /// changes are journaled and reverted.
    #[inline(always)]
    fn account_raw_with_skip<'h>(
        inner: &mut StateInner<'a>,
        accounts: &'h mut AddressMap<Account>,
        address: &Address,
        skip_cold: bool,
    ) -> Result<&'h mut Account, LoadError> {
        match accounts.entry(*address) {
            hash_map::Entry::Occupied(entry) => {
                // An already-loaded account has no cold database read to skip, so the skip only
                // signals an unaffordable *cold* access. Runtime warmth (`AccountFlags::WARM`,
                // seeded from the prewarm set on load) decides coldness: an account warmed earlier
                // this execution is a cheap warm access and must not be forced out of gas.
                let account = entry.into_mut();
                if skip_cold
                    && !account.flags.contains(AccountFlags::WARM)
                    && inner.prewarm_set.is_cold(address)
                {
                    return Err(LoadError::ColdLoadSkipped);
                }
                Ok(account)
            }
            hash_map::Entry::Vacant(entry) => {
                let is_warm = inner.prewarm_set.is_warm(address);
                if skip_cold && !is_warm {
                    return Err(LoadError::ColdLoadSkipped);
                }
                let original = inner.database.get_account(address)?;
                let present = original.clone();
                Ok(entry.insert(Account {
                    original,
                    present,
                    flags: if is_warm { AccountFlags::WARM } else { AccountFlags::empty() },
                    ..Account::default()
                }))
            }
        }
    }

    /// Loads `address` into the transaction overlay and returns a journaled mutation handle.
    ///
    /// Unlike [`Self::account_info_untracked`], which reads the backing database without caching,
    /// this reads the account once and preserves it in the transaction overlay. The returned
    /// [`AccountHandle`] records a revert snapshot on its first mutation, so any changes made
    /// through it are undone together by [`Self::rollback`]. The account is materialized as empty
    /// only when it is first mutated while absent. This mirrors revm's `AccountHandle`.
    ///
    /// Cold accounts are always loaded. Use [`Self::account_with_skip`] to skip them.
    pub fn account(&mut self, address: &Address) -> DbResult<AccountHandle<'_, 'a>> {
        let State { accounts, storage_pool, inner, .. } = self;
        Self::account_raw(inner, accounts, address)
            .map(|tracked| AccountHandle::new(*address, tracked, inner, storage_pool))
    }

    /// Loads an account, optionally skipping a cold access before reading the database.
    ///
    /// With `skip_cold_load` enabled, cold accounts return [`LoadError::ColdLoadSkipped`].
    /// Warm accounts are loaded even when not yet present in the overlay. Otherwise this has
    /// the same loading and journaling semantics as [`Self::account`].
    pub fn account_with_skip(
        &mut self,
        address: &Address,
        skip_cold_load: bool,
    ) -> Result<AccountHandle<'_, 'a>, LoadError> {
        let State { accounts, storage_pool, inner, .. } = self;
        Self::account_raw_with_skip(inner, accounts, address, skip_cold_load)
            .map(|tracked| AccountHandle::new(*address, tracked, inner, storage_pool))
    }

    /// Loads the owning account and returns a journaled handle to its persistent storage.
    ///
    /// Loading account metadata neither touches nor warms the account. Slot values remain lazy;
    /// loading or warming a slot records the same journal entries as [`AccountHandle::storage`].
    #[inline(always)]
    pub fn storage<'s>(&'s mut self, address: &'s Address) -> DbResult<StorageHandle<'s, 'a>> {
        let State { accounts, storage_pool, inner, .. } = self;
        let account = Self::account_raw(inner, accounts, address)?;
        if account.storage.slots.capacity() == 0 {
            account.storage.slots = storage_pool.take();
        }
        let discards_storage = account.discards_storage();
        Ok(StorageHandle::new(address, &mut account.storage, discards_storage, inner))
    }

    /// Loads a single persistent storage slot and returns a journaled mutation handle.
    ///
    /// Cold slots are always loaded. See [`StorageHandle::into_slot`] for loading semantics.
    pub fn storage_slot<'s>(
        &'s mut self,
        address: &'s Address,
        key: Word,
    ) -> DbResult<StorageSlotHandle<'s, 'a>> {
        self.storage(address)?.into_slot(key)
    }

    /// Returns account info from the overlay or the backing database.
    ///
    /// This is a non-loading peek: it returns the overlay account when one has been loaded this
    /// transaction, otherwise it reads the backing database directly without caching the result in
    /// the overlay. Use [`Self::account`] when the account should be loaded and preserved.
    #[inline(never)]
    pub fn account_info_untracked(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        if let Some(entry) = self.accounts.get(address) {
            return Ok(entry.present.clone());
        }
        self.database.get_account(address)
    }

    /// Returns a single persistent storage slot's value from the overlay or the backing database.
    ///
    /// This is the storage-side mirror of [`Self::account_info_untracked`]: a non-loading peek that
    /// returns the overlay slot value when one has been loaded or written this transaction,
    /// otherwise it reads the backing database directly without caching the result in the overlay.
    /// A slot of a wiped account, or of one whose creation discarded its old storage, that has not
    /// been loaded or written reads as zero, though an attached strict BAL must still cover it. Use
    /// [`Self::storage`] when the slot should be loaded and preserved.
    #[inline(never)]
    pub fn storage_slot_untracked(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        let account = self.accounts.get(address);
        if let Some(slot) = account.and_then(|account| account.storage.slots.get(key)) {
            return Ok(slot.value.current);
        }
        let reads_zero =
            account.is_some_and(|account| account.storage.wiped || account.discards_storage());
        self.inner.read_unloaded_slot(address, key, reads_zero)
    }

    /// Returns an account's bytecode from the overlay or the backing database.
    ///
    /// This is the code-side mirror of [`Self::account_info_untracked`]: a non-loading peek that
    /// resolves the account without caching it in the overlay, then returns the code held by the
    /// account info or reads it from the database by code hash. It returns empty bytecode for an
    /// absent account or the empty code hash. Use [`Self::account`] and
    /// [`AccountHandle::load_code`] when the account should be loaded and preserved.
    #[inline(never)]
    pub fn code_untracked(&mut self, address: &Address) -> DbResult<Bytecode> {
        let Some(info) = self.account_info_untracked(address)? else {
            return Ok(Bytecode::default());
        };
        if info.is_empty_code_hash() {
            return Ok(Bytecode::default());
        }
        let code = match info.code {
            Some(code) if !code.is_empty() => code,
            _ => self.database.get_code_by_hash(&info.code_hash)?,
        };
        code.seed_hash(info.code_hash);
        Ok(code)
    }

    /// Transfers value between accounts.
    pub fn transfer(&mut self, from: &Address, to: &Address, value: &Word) -> DbResult<bool> {
        if value.is_zero() {
            self.account(to)?.touch();
            return Ok(true);
        }

        if from == to {
            let mut account = self.account(from)?;
            if account.balance() < *value {
                return Ok(false);
            }
            account.touch();
            return Ok(true);
        }

        {
            let mut from_account = self.account(from)?;
            let Some(new_from_balance) = from_account.balance().checked_sub(*value) else {
                return Ok(false);
            };
            // `set_balance` touches the account, matching the touch the prior `transfer` performed.
            from_account.set_balance(new_from_balance);
            from_account.touch();
        }
        {
            let mut to_account = self.account(to)?;
            let new_to_balance = to_account.balance().saturating_add(*value);
            to_account.set_balance(new_to_balance);
            to_account.touch();
        }
        Ok(true)
    }

    /// Creates a contract account and transfers endowment from the caller.
    #[inline(never)]
    pub fn create_account(
        &mut self,
        caller: &Address,
        address: &Address,
        value: &Word,
        features: EvmFeatures,
    ) -> DbResult<Result<(), InstrStop>> {
        // TODO check order of operations, we could potentially simplify it and do a lot more with
        // only one hashmap lookup.
        if self
            .account(address)?
            .get()
            .is_some_and(|account| account.nonce != 0 || !account.is_empty_code_hash())
        {
            return Ok(Err(InstrStop::CreateCollision));
        }

        // Deduct the endowment from the caller. A zero endowment moves nothing and leaves the
        // caller untouched, matching the prior `transfer` behaviour.
        if !value.is_zero() {
            let mut caller_account = self.account(caller)?;
            let Some(new_caller_balance) = caller_account.balance().checked_sub(*value) else {
                return Ok(Err(InstrStop::OutOfFunds));
            };
            caller_account.set_balance(new_caller_balance);
        }

        let mut target = self.account(address)?;
        // Preserve any balance the address already held (e.g. funds sent before creation) and add
        // the endowment.
        let balance = target.balance().wrapping_add(*value);
        #[cfg(feature = "account-ext")]
        let extension = target.get().map(|info| info.extension.clone()).unwrap_or_default();
        *target.get_or_insert() = AccountInfo {
            nonce: u64::from(features.contains(EvmFeatures::EIP161)),
            balance,
            code_hash: KECCAK256_EMPTY,
            code: Some(Bytecode::default()),
            _non_exhaustive: (),
            #[cfg(feature = "account-ext")]
            extension,
        };
        target.mark_created_discarding_storage();
        target.touch();
        Ok(Ok(()))
    }

    /// Loads transient (EIP-1153) storage.
    #[must_use]
    pub fn tload(&mut self, address: &Address, key: &Word) -> Word {
        self.transient_storage.get(&StorageKey::new(*address, *key)).copied().unwrap_or_default()
    }

    /// Stores transient (EIP-1153) storage.
    pub fn tstore(&mut self, address: &Address, key: &Word, value: &Word) {
        match self.transient_storage.entry(StorageKey::new(*address, *key)) {
            hash_map::Entry::Occupied(mut entry) => {
                let previous = *entry.get();
                if previous == *value {
                    return;
                }
                self.inner.push_journal(|| JournalEntry::TransientStorageChange {
                    address: *address,
                    key: *key,
                    previous: Some(previous),
                });
                if value.is_zero() {
                    entry.remove();
                } else {
                    *entry.get_mut() = *value;
                }
            }
            hash_map::Entry::Vacant(entry) => {
                if value.is_zero() {
                    return;
                }
                self.inner.push_journal(|| JournalEntry::TransientStorageChange {
                    address: *address,
                    key: *key,
                    previous: None,
                });
                entry.insert(*value);
            }
        }
    }

    /// Removes and returns every transient storage slot owned by `address`.
    ///
    /// This is intended for transaction-finalization policies that encode per-account pending
    /// work in EIP-1153 storage and must consume it before transaction scratch is cleared.
    pub fn take_transient_storage(&mut self, address: &Address) -> Vec<(Word, Word)> {
        let keys = self
            .transient_storage
            .keys()
            .copied()
            .filter(|key| key.address() == *address)
            .collect::<Vec<_>>();
        keys.into_iter()
            .map(|key| {
                let value = self
                    .transient_storage
                    .remove(&key)
                    .expect("collected transient storage key must exist");
                (key.key(), value)
            })
            .collect()
    }

    /// Reverts state changes after the checkpoint.
    #[inline(never)]
    pub fn rollback(&mut self, checkpoint: StateCheckpoint, features: EvmFeatures) {
        // Restoring an older snapshot can leave active cursors past the current lengths. Preserve
        // shorter prefixes and revert only entries beyond each cursor.
        self.logs.truncate(checkpoint.logs_len());
        while self.journal.len() > checkpoint.journal_len() {
            let Some(entry) = self.journal.pop() else {
                unreachable!("journal length is checked above")
            };
            match entry {
                JournalEntry::AccountChange { address, previous, previous_flags } => {
                    if let Some(entry) = self.accounts.get_mut(&address) {
                        entry.present = previous;
                        // EIP-161 preserves the historical Yellow Paper K.1 precompile-3 touch.
                        let keep_touch = features.contains(EvmFeatures::EIP161)
                            && address == Address::with_last_byte(3);
                        let kept =
                            if keep_touch { AccountFlags::TOUCHED } else { AccountFlags::empty() };
                        entry.flags = (previous_flags - kept) | (entry.flags & kept);
                    }
                }
                JournalEntry::StorageChange { address, key, previous } => {
                    if let Some(slot) = self
                        .accounts
                        .get_mut(&address)
                        .and_then(|account| account.storage.slots.get_mut(&key))
                    {
                        slot.value.set_current(previous);
                    }
                }
                JournalEntry::TransientStorageChange { address, key, previous } => match previous {
                    Some(previous) if !previous.is_zero() => {
                        self.transient_storage.insert(StorageKey::new(address, key), previous);
                    }
                    _ => {
                        self.transient_storage.remove(&StorageKey::new(address, key));
                    }
                },
                JournalEntry::StorageWarmed { address, key } => {
                    self.set_storage_warm(&address, key, false);
                }
            }
        }
    }

    fn materialize_empty_account_for_finalization(&mut self, address: &Address) -> DbResult<()> {
        // `account_raw` loads the backing-database account into `original`,
        // so its existence is read from the same source rather than via a separate database read.
        let entry = Self::account_raw(&mut self.inner, &mut self.accounts, address)?;
        if entry.original.is_none() && entry.present.is_none() {
            // Finalization runs after the last revertible scope, so this is not journaled: the
            // entry would never be replayed before `clear_transaction_state` clears it.
            entry.present = Some(AccountInfo::default());
            entry.mark_created_discarding_storage();
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn finalize_transaction_(&mut self, version: &Version) {
        self.finalize_transaction(version).unwrap();
    }

    /// Applies transaction-finalization account-lifetime rules to the overlay.
    ///
    /// This mutates the in-memory post-transaction state before it is streamed to a
    /// [`StateChangeSink`] or detached as a [`PendingState`]. Runtime records
    /// transaction substate such as touches and selfdestructs, while finalization
    /// turns that substate into account deletions, storage wipes, balance-only
    /// selfdestruct resets (EIP-8246), or pre-EIP-161 empty-account materialization.
    pub(crate) fn finalize_transaction(&mut self, version: &Version) -> DbResult<()> {
        // Finalization neither touches nor destroys accounts, so one pass over the overlay selects
        // both sets. Selfdestructed accounts are always touched, so they are not selected twice.
        // Only the selected accounts need another lookup, and they are rare. EIP-161 deletes
        // touched dead accounts at transaction finalization. Before EIP-161, touching a
        // non-existent account materializes it as empty.
        let eip161 = version.feature(EvmFeatures::EIP161);
        let mut destroyed = Vec::new();
        let mut finalize = Vec::new();
        for (&address, entry) in &self.accounts {
            if entry.flags.contains(AccountFlags::DESTROYED) {
                destroyed.push(address);
            } else if entry.flags.contains(AccountFlags::TOUCHED)
                && if eip161 { entry.is_existing_dead() } else { entry.present.is_none() }
            {
                finalize.push(address);
            }
        }

        let eip8246 = version.feature(EvmFeatures::EIP8246);
        for address in &destroyed {
            // EIP-8246: a self-destructed account that still holds balance is preserved as a
            // balance-only account instead of being burned. One with no balance is removed. The
            // handle is scoped so its `AccountChange` flushes on drop before the storage wipe.
            let survives = {
                let mut account = self.account(address)?;
                let survives = eip8246 && !account.balance().is_zero();
                if survives {
                    account.reset_selfdestructed_for_finalization();
                } else {
                    account.delete_for_finalization();
                }
                survives
            };
            // A created survivor that discarded its old storage keeps it unloaded, and later
            // transactions read it as zero. One that did not discard has no such reads, so its
            // storage is wiped.
            let zero_loaded_only = survives && self.accounts[address].discards_storage();
            let mut storage = self.storage(address)?;
            if zero_loaded_only { storage.zero_loaded() } else { storage.wipe() }
        }

        for address in &finalize {
            if eip161 {
                self.account(address)?.delete_for_finalization();
                self.storage(address)?.wipe();
            } else {
                self.materialize_empty_account_for_finalization(address)?;
            }
        }
        Ok(())
    }

    /// Returns a borrowed view of the transaction layer's changes, without detaching it.
    #[inline]
    pub(crate) const fn transaction_changes(&self) -> TxStateChanges<'_> {
        TxStateChanges::new(&self.accounts)
    }

    /// Detaches the transaction overlay into an owned [`PendingState`].
    ///
    /// The remaining transaction scratch (journal, logs, warm sets, transient storage) is left for
    /// [`Self::clear_transaction_state`].
    pub(crate) fn take_pending_state(&mut self) -> PendingState {
        PendingState { accounts: mem::take(&mut self.accounts) }
    }

    /// Reattaches a detached [`PendingState`] as the current transaction overlay, replacing it.
    ///
    /// This is the inverse of the detach performed by
    /// [`ExecutedTx::detach`](crate::ExecutedTx::detach): the pending accounts and storage become
    /// the transaction layer again, as if the transaction had just been finalized. Touch and
    /// selfdestruct flags are kept, so finalizing again would reset EIP-8246 survivors a second
    /// time: call [`Self::commit_transaction`] before executing another
    /// transaction. Other transaction scratch (journal, logs, warm sets, transient storage) is not
    /// affected.
    pub fn set_pending_state(&mut self, pending: PendingState) {
        for account in self.accounts.values_mut() {
            self.storage_pool.clear_overlay(&mut account.storage);
        }
        self.accounts = pending.accounts;
    }

    /// Accepts the current transaction's state transition into the accepted overlay.
    ///
    /// This advances the in-memory accepted overlay by the transaction's write-set and clears the
    /// transaction account/storage layers. It does not take logs or write to the wrapped backing
    /// database.
    pub fn commit_transaction(&mut self) {
        // The transaction overlay is folded into the accepted-overlay database directly, without
        // detaching it.
        self.inner.database.commit(&self.accounts);
        for account in self.accounts.values_mut() {
            self.storage_pool.clear_overlay(&mut account.storage);
        }
        self.accounts.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn committed_copy_matches_committed_clone() {
        let (written, read, absent) =
            (Address::with_last_byte(1), Address::with_last_byte(2), Address::with_last_byte(3));
        let mut db = CacheDB::default();
        db.insert_account_info(&read, AccountInfo::default().with_nonce(3));
        let mut state = State::new(db);
        state.account(&written).unwrap().set_balance(Word::from(5));
        state.storage_slot(&written, Word::ONE).unwrap().set(Word::from(6));
        state.commit_transaction();
        // The running transaction overwrites accepted values, reads others, and leaves scratch.
        state.storage_slot(&written, Word::ONE).unwrap().set(Word::from(7));
        state.storage_slot(&written, Word::from(2)).unwrap().set(Word::from(8));
        state.account(&read).unwrap();
        state.prewarm(&absent);
        state.tstore(&written, &Word::ZERO, &Word::from(9));
        state.log(Log::default());

        let mut expected = state.clone();
        expected.commit_transaction();
        expected.clear_transaction_state();
        let mut copy = state.committed_copy(EmptyDB::default());

        assert!(copy.journal().is_empty() && copy.logs().is_empty());
        assert_eq!(copy.checkpoint(), expected.checkpoint());
        for state in [&mut expected, &mut copy] {
            assert_eq!(
                state.account_info_untracked(&written).unwrap().unwrap().balance,
                Word::from(5)
            );
            assert_eq!(state.account_info_untracked(&read).unwrap().unwrap().nonce, 3);
            assert_eq!(state.storage_slot_untracked(&written, &Word::ONE).unwrap(), Word::from(7));
            assert_eq!(
                state.storage_slot_untracked(&written, &Word::from(2)).unwrap(),
                Word::from(8)
            );
            assert_eq!(state.tload(&written, &Word::ZERO), Word::ZERO);
            assert!(!state.account(&absent).unwrap().is_warm());
            let slot = state.storage_slot(&written, Word::ONE).unwrap();
            assert_eq!((slot.original(), slot.current()), (Word::from(7), Word::from(7)));
        }
    }

    #[test]
    fn storage_loads_account_without_touching_or_warming_it() {
        let address = Address::with_last_byte(42);
        let info = AccountInfo::default().with_nonce(7);
        let mut db = CacheDB::default();
        db.insert_account_info(&address, info.clone());
        let mut state = State::new(db);

        state.storage(&address).unwrap();

        let account = &state.accounts[&address];
        assert_eq!(account.original, Some(info.clone()));
        assert_eq!(account.present, Some(info));
        assert!(account.flags.is_empty());
        assert!(state.journal().is_empty());
    }

    #[test]
    fn created_account_reads_old_storage_as_zero() {
        let caller = Address::with_last_byte(1);
        let address = Address::with_last_byte(2);
        let (loaded, unloaded, fresh) = (Word::ONE, Word::from(2), Word::from(3));
        let features = EvmFeatures::empty();
        let mut db = CacheDB::default();
        db.insert_account_info(&address, AccountInfo::default().with_balance(Word::ONE));
        db.insert_account_storage(&address, &loaded, &Word::from(5));
        db.insert_account_storage(&address, &unloaded, &Word::from(6));
        db.insert_account_storage(&address, &fresh, &Word::from(7));
        let mut state = State::new(db);
        assert_eq!(state.storage_slot(&address, loaded).unwrap().current(), Word::from(5));

        let checkpoint = state.checkpoint();
        state.create_account(&caller, &address, &Word::ZERO, features).unwrap().unwrap();
        // A slot loaded before the creation keeps its value.
        assert_eq!(state.storage_slot(&address, loaded).unwrap().current(), Word::from(5));
        assert_eq!(state.storage_slot(&address, unloaded).unwrap().current(), Word::ZERO);
        assert_eq!(state.storage_slot(&address, unloaded).unwrap().original(), Word::ZERO);
        assert_eq!(state.storage_slot_untracked(&address, &unloaded).unwrap(), Word::ZERO);
        assert_eq!(state.get_storage(&address, &unloaded), Some(Word::ZERO));
        state.storage_slot(&address, unloaded).unwrap().write(Word::from(9));

        state.rollback(checkpoint, features);
        // Rolling back the creation keeps the zero it loaded.
        assert_eq!(state.storage_slot(&address, loaded).unwrap().current(), Word::from(5));
        assert_eq!(state.storage_slot(&address, unloaded).unwrap().current(), Word::ZERO);
        let checkpoint = state.checkpoint();
        state.create_account(&caller, &address, &Word::ZERO, features).unwrap().unwrap();
        assert_eq!(state.storage_slot(&address, fresh).unwrap().current(), Word::ZERO);
        assert_eq!(state.storage_slot(&address, unloaded).unwrap().current(), Word::ZERO);
        state.rollback(checkpoint, features);
        assert!(!state.take_pending_state().is_changed());
    }

    #[test]
    fn marked_created_account_reads_and_commits_its_baseline() {
        let caller = Address::with_last_byte(1);
        let address = Address::with_last_byte(2);
        let (written, unwritten) = (Word::ONE, Word::from(2));
        let features = EvmFeatures::empty();
        let mut db = CacheDB::default();
        db.insert_account_info(&address, AccountInfo::default().with_nonce(1));
        db.insert_account_storage(&address, &written, &Word::from(5));
        db.insert_account_storage(&address, &unwritten, &Word::from(6));
        let mut state = State::new(db);

        let mut account = state.account(&address).unwrap();
        account.mark_created();
        // Still created for EIP-6780 `SELFDESTRUCT`.
        assert!(account.is_created());
        drop(account);
        assert_eq!(state.storage_slot_untracked(&address, &unwritten).unwrap(), Word::from(6));
        let mut slot = state.storage_slot(&address, written).unwrap();
        assert_eq!((slot.original(), slot.current()), (Word::from(5), Word::from(5)));
        slot.write(Word::from(9));

        // Committing keeps the slots the transaction did not write.
        state.commit_transaction();
        assert_eq!(state.storage_slot(&address, written).unwrap().current(), Word::from(9));
        assert_eq!(state.storage_slot(&address, unwritten).unwrap().current(), Word::from(6));

        // Marking an account created in the overlay does not undo its discarded storage.
        let other = Address::with_last_byte(3);
        state
            .overlay_db_mut()
            .insert_account_info(&other, AccountInfo::default().with_balance(Word::ONE));
        state.overlay_db_mut().insert_account_storage(&other, &unwritten, &Word::from(6));
        state.create_account(&caller, &other, &Word::ZERO, features).unwrap().unwrap();
        state.account(&other).unwrap().mark_created();
        assert_eq!(state.storage_slot_untracked(&other, &unwritten).unwrap(), Word::ZERO);
        assert_eq!(state.storage_slot(&other, unwritten).unwrap().current(), Word::ZERO);
    }

    #[test]
    fn finalization_keeps_storage_of_prefunded_created_account() {
        let caller = Address::with_last_byte(1);
        let address = Address::with_last_byte(2);
        let written = Word::from(2);
        let version = Version::base(crate::SpecId::CANCUN);
        let mut db = CacheDB::default();
        db.insert_account_info(&address, AccountInfo::default().with_balance(Word::ONE));
        let mut state = State::new(db);

        state.create_account(&caller, &address, &Word::ZERO, version.features).unwrap().unwrap();
        state.storage_slot(&address, written).unwrap().write(Word::from(9));
        state.finalize_transaction_(version);

        let pending = state.take_pending_state();
        let storage = &pending.accounts[&address].storage;
        assert!(!storage.wiped);
        assert_eq!(storage.changed_slots().map(|(key, _)| *key).collect::<Vec<_>>(), [written]);
    }

    #[test]
    fn eip8246_created_survivor_keeps_old_storage() {
        let caller = Address::with_last_byte(1);
        let address = Address::with_last_byte(2);
        let (written, old) = (Word::ONE, Word::from(2));
        let version = Version::base(crate::SpecId::AMSTERDAM);
        let mut db = CacheDB::default();
        db.insert_account_info(&address, AccountInfo::default().with_balance(Word::ONE));
        db.insert_account_storage(&address, &old, &Word::from(6));
        let mut state = State::new(db);

        state.create_account(&caller, &address, &Word::ZERO, version.features).unwrap().unwrap();
        state.storage_slot(&address, written).unwrap().write(Word::from(9));
        state.account(&address).unwrap().mark_destructed();
        state.finalize_transaction_(version);

        let storage = &state.accounts[&address].storage;
        assert!(!storage.wiped);
        assert_eq!(storage.changed_slots().count(), 0);
        state.commit_transaction();
        for key in [written, old] {
            assert_eq!(state.storage_slot(&address, key).unwrap().current(), Word::ZERO);
        }
    }

    #[test]
    fn eip8246_marked_created_survivor_wipes_storage() {
        let address = Address::with_last_byte(2);
        let (written, earlier) = (Word::ONE, Word::from(2));
        let version = Version::base(crate::SpecId::AMSTERDAM);
        let mut db = CacheDB::default();
        db.insert_account_info(&address, AccountInfo::default().with_balance(Word::ONE));
        // A slot the account's creation wrote before the transaction was suspended.
        db.insert_account_storage(&address, &earlier, &Word::from(6));
        let mut state = State::new(db);

        state.account(&address).unwrap().mark_created();
        state.storage_slot(&address, written).unwrap().write(Word::from(9));
        state.account(&address).unwrap().mark_destructed();
        state.finalize_transaction_(version);

        assert!(state.accounts[&address].storage.wiped);
        state.commit_transaction();
        for key in [written, earlier] {
            assert_eq!(state.storage_slot(&address, key).unwrap().current(), Word::ZERO);
        }
    }

    #[test]
    fn rollback_of_storage_discard_restores_storage_baseline() {
        let address = Address::with_last_byte(2);
        let key = Word::ONE;
        let features = Version::base(crate::SpecId::CANCUN).features;
        let mut db = CacheDB::default();
        db.insert_account_info(&address, AccountInfo::default().with_balance(Word::ONE));
        db.insert_account_storage(&address, &key, &Word::from(6));
        let mut state = State::new(db);

        state.account(&address).unwrap().mark_created();
        let checkpoint = state.checkpoint();
        state.account(&address).unwrap().mark_created_discarding_storage();
        assert!(state.accounts[&address].discards_storage());
        state.rollback(checkpoint, features);

        assert!(state.accounts[&address].is_created());
        assert!(!state.accounts[&address].discards_storage());
        assert_eq!(state.storage_slot(&address, key).unwrap().current(), Word::from(6));
    }

    #[test]
    fn rollback_of_creation_keeps_slots_read_as_zero() {
        let caller = Address::with_last_byte(1);
        let address = Address::with_last_byte(2);
        let (read, written, empty) = (Word::ONE, Word::from(2), Word::from(3));
        let version = Version::base(crate::SpecId::CANCUN);
        let mut db = CacheDB::default();
        db.insert_account_info(&address, AccountInfo::default().with_balance(Word::ONE));
        db.insert_account_storage(&address, &read, &Word::from(6));
        db.insert_account_storage(&address, &written, &Word::from(7));
        let mut state = State::new(db);

        let checkpoint = state.checkpoint();
        state.create_account(&caller, &address, &Word::ZERO, version.features).unwrap().unwrap();
        assert_eq!(state.storage_slot(&address, read).unwrap().current(), Word::ZERO);
        state.storage_slot(&address, written).unwrap().write(Word::from(9));
        assert_eq!(state.storage_slot(&address, empty).unwrap().current(), Word::ZERO);
        state.rollback(checkpoint, version.features);

        assert!(!state.accounts[&address].discards_storage());
        for key in [read, written, empty] {
            assert_eq!(state.storage_slot_untracked(&address, &key).unwrap(), Word::ZERO);
            let slot = state.storage_slot(&address, key).unwrap();
            assert_eq!((slot.original(), slot.current()), (Word::ZERO, Word::ZERO));
        }
        assert!(!state.take_pending_state().is_changed());
    }

    #[test]
    fn rollback_keeps_slot_read_as_zero_under_the_creation() {
        let caller = Address::with_last_byte(1);
        let address = Address::with_last_byte(2);
        let key = Word::ONE;
        let features = Version::base(crate::SpecId::CANCUN).features;
        let mut db = CacheDB::default();
        db.insert_account_info(&address, AccountInfo::default().with_balance(Word::ONE));
        db.insert_account_storage(&address, &key, &Word::from(6));
        let mut state = State::new(db);

        let outer = state.checkpoint();
        state.create_account(&caller, &address, &Word::ZERO, features).unwrap().unwrap();
        let inner = state.checkpoint();
        assert_eq!(state.storage_slot(&address, key).unwrap().current(), Word::ZERO);
        state.rollback(inner, features);
        assert_eq!(state.storage_slot(&address, key).unwrap().current(), Word::ZERO);
        state.rollback(outer, features);
        assert_eq!(state.storage_slot(&address, key).unwrap().current(), Word::ZERO);
    }

    #[test]
    fn rollback_of_creation_mark_leaves_slots_alone() {
        let address = Address::with_last_byte(2);
        let (read, zero) = (Word::ONE, Word::from(2));
        let features = Version::base(crate::SpecId::CANCUN).features;
        let mut db = CacheDB::default();
        db.insert_account_info(&address, AccountInfo::default().with_balance(Word::ONE));
        db.insert_account_storage(&address, &read, &Word::from(6));
        let mut state = State::new(db);

        let checkpoint = state.checkpoint();
        state.account(&address).unwrap().mark_created();
        assert_eq!(state.storage_slot(&address, read).unwrap().current(), Word::from(6));
        assert_eq!(state.storage_slot(&address, zero).unwrap().current(), Word::ZERO);
        state.rollback(checkpoint, features);

        assert_eq!(state.storage_slot(&address, read).unwrap().current(), Word::from(6));
        assert_eq!(state.storage_slot(&address, zero).unwrap().current(), Word::ZERO);
    }

    #[test]
    fn finalization_wipe_after_rolled_back_creation_reads_zero() {
        let caller = Address::with_last_byte(1);
        let address = Address::with_last_byte(2);
        let key = Word::ONE;
        let version = Version::base(crate::SpecId::LONDON);
        let mut db = CacheDB::default();
        db.insert_account_info(&address, AccountInfo::default().with_balance(Word::ONE));
        db.insert_account_storage(&address, &key, &Word::from(6));
        let mut state = State::new(db);

        let checkpoint = state.checkpoint();
        state.create_account(&caller, &address, &Word::ZERO, version.features).unwrap().unwrap();
        assert_eq!(state.storage_slot(&address, key).unwrap().current(), Word::ZERO);
        state.rollback(checkpoint, version.features);

        state.account(&address).unwrap().mark_destructed();
        state.finalize_transaction_(version);

        assert_eq!(state.storage_slot_untracked(&address, &key).unwrap(), Word::ZERO);
        assert_eq!(state.storage_slot(&address, key).unwrap().current(), Word::ZERO);
    }

    #[test]
    fn rollback_of_creation_keeps_slots_loaded_under_it() {
        let caller = Address::with_last_byte(1);
        let address = Address::with_last_byte(2);
        let (before, under) = (Word::ONE, Word::from(2));
        let features = Version::base(crate::SpecId::CANCUN).features;
        let mut db = CacheDB::default();
        db.insert_account_info(&address, AccountInfo::default().with_balance(Word::ONE));
        let mut state = State::new(db);
        assert_eq!(state.storage_slot(&address, before).unwrap().current(), Word::ZERO);
        // Only a reread can observe these values, and rollback does not reread.
        for key in [before, under] {
            state.overlay_db_mut().insert_account_storage(&address, &key, &Word::from(5));
        }

        let checkpoint = state.checkpoint();
        state.create_account(&caller, &address, &Word::ZERO, features).unwrap().unwrap();
        assert_eq!(state.storage_slot(&address, under).unwrap().current(), Word::ZERO);
        state.rollback(checkpoint, features);

        assert_eq!(state.storage_slot(&address, before).unwrap().current(), Word::ZERO);
        assert_eq!(state.storage_slot(&address, under).unwrap().current(), Word::ZERO);
    }

    #[test]
    fn rollback_keeps_zero_of_slot_written_through_the_creating_handle() {
        let address = Address::with_last_byte(2);
        let key = Word::ONE;
        let features = Version::base(crate::SpecId::CANCUN).features;
        let mut db = CacheDB::default();
        db.insert_account_info(&address, AccountInfo::default().with_balance(Word::ONE));
        db.insert_account_storage(&address, &key, &Word::from(6));
        let mut state = State::new(db);

        let checkpoint = state.checkpoint();
        let mut account = state.account(&address).unwrap();
        account.mark_created_discarding_storage();
        // The slot write is journaled before the handle flushes its creation entry on drop.
        account.storage().into_slot(key).unwrap().write(Word::from(9));
        drop(account);
        state.rollback(checkpoint, features);

        assert_eq!(state.storage_slot(&address, key).unwrap().current(), Word::ZERO);
    }

    #[test]
    fn later_transactions_read_old_storage_of_created_account_as_zero() {
        let caller = Address::with_last_byte(1);
        let address = Address::with_last_byte(2);
        let (cached, unloaded, written) = (Word::ONE, Word::from(2), Word::from(3));
        let version = Version::base(crate::SpecId::CANCUN);
        let mut db = CacheDB::default();
        db.insert_account_info(&address, AccountInfo::default().with_balance(Word::ONE));
        db.insert_account_storage(&address, &cached, &Word::from(5));
        db.insert_account_storage(&address, &unloaded, &Word::from(6));
        let mut state = State::new(db);
        assert_eq!(state.storage_slot(&address, cached).unwrap().current(), Word::from(5));
        state.commit_transaction();

        state.create_account(&caller, &address, &Word::ZERO, version.features).unwrap().unwrap();
        state.storage_slot(&address, written).unwrap().write(Word::from(9));
        state.finalize_transaction_(version);
        state.commit_transaction();

        for (key, value) in [(cached, Word::ZERO), (unloaded, Word::ZERO), (written, Word::from(9))]
        {
            assert_eq!(state.storage_slot(&address, key).unwrap().current(), value);
        }
    }

    #[test]
    fn account_storage_handle_reuses_pooled_slot_map() {
        let address = Address::with_last_byte(1);
        let mut state = State::new(EmptyDB::default());
        let key = Word::ONE;
        state.storage_slot(&address, key).unwrap();
        let expected_capacity = state.accounts[&address].storage.slots.capacity();
        assert!(expected_capacity > 0);
        state.commit_transaction();

        let mut account = state.account(&address).unwrap();
        account.storage();
        drop(account);

        assert_eq!(state.accounts[&address].storage.slots.capacity(), expected_capacity);
    }

    #[test]
    fn clone_preserves_state_and_detaches_database() {
        let address = Address::with_last_byte(42);
        let key = Word::ONE;
        let mut db = CacheDB::default();
        db.insert_account_info(&address, AccountInfo::default());
        db.insert_account_storage(&address, &key, &Word::from(10));
        let mut state = State::new(db);
        state.storage_slot(&address, key).unwrap().write(Word::from(20));
        state.account(&address).unwrap().set_balance(Word::from(5));
        let mut cloned = state.clone();

        assert!(cloned.initial().downcast_ref::<EmptyDB>().is_some());
        assert!(state.initial().downcast_ref::<CacheDB>().is_some());
        assert_eq!(cloned.database.cache, state.database.cache);
        assert_eq!(cloned.journal(), state.journal());
        assert_eq!(cloned.account(&address).unwrap().balance(), Word::from(5));
        assert_eq!(cloned.storage_slot(&address, key).unwrap().current(), Word::from(20));

        cloned.storage_slot(&address, key).unwrap().write(Word::from(30));
        cloned.account(&address).unwrap().set_balance(Word::from(6));
        assert_eq!(state.storage_slot(&address, key).unwrap().current(), Word::from(20));
        assert_eq!(state.account(&address).unwrap().balance(), Word::from(5));
    }

    #[test]
    fn clone_with_uses_database_and_preserves_state() {
        let address = Address::with_last_byte(42);
        let mut state = State::new(EmptyDB::default());
        state.tstore(&address, &Word::ZERO, &Word::ONE);
        let mut db = CacheDB::default();
        db.insert_account_info(&address, AccountInfo::default());

        let mut cloned = state.clone_with(db);

        assert_eq!(cloned.tload(&address, &Word::ZERO), Word::ONE);
        assert!(cloned.account_info_untracked(&address).unwrap().is_some());
    }

    #[test]
    fn detached_snapshot_is_send_sync_and_uses_new_database() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StateSnapshot>();

        let uncached = Address::with_last_byte(43);
        let state = State::new(EmptyDB::default());
        let snapshot = state.snapshot();

        let mut db = CacheDB::default();
        db.insert_account_info(&uncached, AccountInfo::default().with_balance(Word::from(9)));
        let mut restored = snapshot.into_state(db);
        assert_eq!(restored.account(&uncached).unwrap().balance(), Word::from(9));
    }

    #[test]
    fn set_storage_warm_preserves_loaded_values() {
        let address = Address::with_last_byte(42);
        let key = Word::ONE;
        for prewarmed in [false, true] {
            let mut state = State::new(EmptyDB::default());
            state.set_storage_warm(&address, key, true);
            assert!(!state.storage(&address).unwrap().is_loaded(&key));
            if prewarmed {
                state.prewarm_storage(&address, [key, Word::from(2)]);
            }
            let mut slot = state.storage_slot(&address, key).unwrap();
            slot.set(Word::from(7));
            slot.warm();
            let checkpoint = state.checkpoint();
            state.set_storage_warm(&address, key, false);
            assert_eq!(state.storage(&address).unwrap().is_warm(&key), prewarmed);
            assert_eq!(state.storage(&address).unwrap().is_warm(&Word::from(2)), prewarmed);
            state.rollback(checkpoint, crate::EvmFeatures::empty());
            let mut slot = state.storage_slot(&address, key).unwrap();
            assert_eq!((slot.original(), slot.current()), (Word::ZERO, Word::from(7)));
            assert_eq!(slot.warm(), !prewarmed);
            state.set_storage_warm(&address, key, false);
            state.set_storage_warm(&address, key, true);
            assert!(!state.storage_slot(&address, key).unwrap().warm());
        }
    }

    #[test]
    fn set_account_warm_preserves_loaded_values() {
        let address = Address::with_last_byte(42);
        let key = Word::ONE;
        for prewarmed in [false, true] {
            let mut state = State::new(EmptyDB::default());
            state.set_account_warm(&address, true);
            assert!(!state.accounts.contains_key(&address));
            if prewarmed {
                state.prewarm(&address);
            }
            let mut account = state.account(&address).unwrap();
            account.set_balance(Word::from(7));
            account.warm();
            drop(account);
            state.storage_slot(&address, key).unwrap().warm();
            let checkpoint = state.checkpoint();
            state.set_account_warm(&address, false);
            assert_eq!(state.account(&address).unwrap().is_warm(), prewarmed);
            assert!(state.storage(&address).unwrap().is_warm(&key));
            state.rollback(checkpoint, crate::EvmFeatures::empty());
            let mut account = state.account(&address).unwrap();
            assert_eq!(account.balance(), Word::from(7));
            assert_eq!(account.warm(), !prewarmed);
            drop(account);
            state.set_account_warm(&address, false);
            state.set_account_warm(&address, true);
            assert!(!state.account(&address).unwrap().warm());
        }
    }

    #[test]
    fn rollback_after_snapshot_restore() {
        // Restoring a snapshot does not rebase active checkpoints. Only entries beyond each saved
        // cursor are reverted, even after the journal and logs regrow.
        for (writes, new_logs, expected, expected_logs) in
            [(0, 0, 7, 1), (1, 2, 10, 2), (2, 1, 11, 2), (3, 0, 11, 1)]
        {
            let mut state = State::new(EmptyDB::default());
            let parent = state.checkpoint();
            state.tstore(&Address::ZERO, &Word::ZERO, &Word::from(7));
            let kept_log: Log = Log { address: Address::with_last_byte(1), ..Default::default() };
            state.log(kept_log.clone());
            let snapshot = state.snapshot();
            for slot in 1..=2 {
                state.tstore(&Address::ZERO, &Word::from(slot), &Word::from(8));
            }
            state.log(Log::default());
            let child = state.checkpoint();
            state = snapshot.into_state(EmptyDB::default());
            for n in 0..writes {
                state.tstore(&Address::ZERO, &Word::ZERO, &Word::from(10 + n));
            }
            for n in 0..new_logs {
                state.log(Log {
                    address: Address::with_last_byte(10 + n as u8),
                    ..Default::default()
                });
            }
            state.rollback(child, EvmFeatures::empty());
            assert_eq!(state.tload(&Address::ZERO, &Word::ZERO), Word::from(expected));
            assert_eq!(state.logs().len(), expected_logs);
            assert_eq!(state.logs()[0], kept_log);
            if expected_logs == 2 {
                assert_eq!(state.logs()[1].address, Address::with_last_byte(10));
            }
            state.rollback(parent, EvmFeatures::empty());
            assert_eq!(state.tload(&Address::ZERO, &Word::ZERO), Word::ZERO);
            assert!(state.logs().is_empty());
        }
    }

    #[test]
    fn wiped_slot_is_reported_as_a_read_of_its_original_value() {
        #[derive(Default)]
        struct Reads(Vec<(Word, Word)>);

        impl StateChangeSink for Reads {
            type Error = core::convert::Infallible;

            fn storage_read(
                &mut self,
                _address: Address,
                key: Word,
                value: Word,
            ) -> Result<(), Self::Error> {
                self.0.push((key, value));
                Ok(())
            }
        }

        let address = Address::with_last_byte(42);
        let (read_key, written_key) = (Word::ONE, Word::from(2));
        let mut db = CacheDB::default();
        db.insert_account_info(&address, AccountInfo::default().with_balance(Word::ONE));
        db.insert_account_storage(&address, &read_key, &Word::from(5));
        db.insert_account_storage(&address, &written_key, &Word::from(6));
        let mut state = State::new(db);
        state.account(&address).unwrap();
        state.storage_slot(&address, read_key).unwrap();
        state.storage_slot(&address, written_key).unwrap().set(Word::from(7));
        state.storage(&address).unwrap().wipe();

        let mut reads = Reads::default();
        state.take_pending_state().visit(&mut reads).unwrap();
        reads.0.sort();
        assert_eq!(reads.0, [(read_key, Word::from(5)), (written_key, Word::from(6))]);
    }

    #[test]
    fn replaced_prewarm_set_keeps_runtime_warmth_and_journal() {
        let old_base = Address::with_last_byte(41);
        let new_base = Address::with_last_byte(42);
        let key = Word::ONE;
        let mut state = State::new(EmptyDB::default());
        state.prewarm_storage_slot(&old_base, key);
        state.account(&new_base).unwrap().set_balance(Word::from(7));
        let journal = state.journal().to_vec();
        let mut prewarm_set = PrewarmSet::new();
        prewarm_set.warm_storage(&new_base, [key]);
        *state.prewarm_set_mut() = prewarm_set.clone();

        assert_eq!(state.prewarm_set(), &prewarm_set);
        assert_eq!(state.journal(), journal);
        assert!(!state.account(&old_base).unwrap().is_warm());
        assert!(!state.storage_slot(&old_base, key).unwrap().is_warm());
        assert!(state.account(&new_base).unwrap().is_warm());
        assert!(state.storage_slot(&new_base, key).unwrap().is_warm());

        let cooled = Address::with_last_byte(43);
        state.prewarm(&cooled);
        state.prewarm_set_mut().remove(&cooled);
        assert!(!state.account(&cooled).unwrap().is_warm());
    }

    fn sorted_loaded_accounts(state: &State<'_>) -> Vec<(Address, Vec<Word>)> {
        let mut loaded = state
            .loaded_accounts()
            .map(|(address, slots)| {
                let mut slots = slots.copied().collect::<Vec<_>>();
                slots.sort_unstable();
                (*address, slots)
            })
            .collect::<Vec<_>>();
        loaded.sort_unstable();
        loaded
    }

    #[test]
    fn loaded_accounts_include_reads_and_absent_accounts() {
        let read = Address::repeat_byte(0x74);
        let absent = Address::repeat_byte(0x75);
        let accepted = Address::repeat_byte(0x76);
        let mut database = CacheDB::default();
        database.insert_account_info(&read, AccountInfo::default().with_balance(Word::ONE));
        database.insert_account_storage(&read, &Word::ONE, &Word::from(10));
        let mut state = State::new(database);
        state.account(&accepted).unwrap().set_balance(Word::ONE);
        state.commit_transaction();

        state.storage_slot(&read, Word::ONE).unwrap();
        state.storage_slot(&read, Word::from(2)).unwrap();
        state.account(&absent).unwrap();

        assert_eq!(
            sorted_loaded_accounts(&state),
            [(read, vec![Word::ONE, Word::from(2)]), (absent, vec![])]
        );
    }

    #[test]
    fn loaded_accounts_include_reads_in_reverted_frames() {
        let read = Address::with_last_byte(0x77);
        let mut database = CacheDB::default();
        database.insert_account_info(&read, AccountInfo::default().with_balance(Word::ONE));
        let mut state = State::new(database);

        let checkpoint = state.checkpoint();
        state.storage_slot(&read, Word::ONE).unwrap();
        state.rollback(checkpoint, EvmFeatures::empty());

        assert_eq!(sorted_loaded_accounts(&state), [(read, vec![Word::ONE])]);
    }
}
