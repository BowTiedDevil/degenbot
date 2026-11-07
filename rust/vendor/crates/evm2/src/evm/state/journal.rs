//! Revert journal and checkpoint types.

use super::{AccountFlags, AccountInfo};
use crate::interpreter::Word;
use alloy_primitives::Address;

/// State checkpoint for reverting state changes.
#[allow(missing_copy_implementations)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateCheckpoint {
    /// Revert journal length at the checkpoint.
    pub(crate) journal_len: usize,
    /// Emitted log count at the checkpoint.
    pub(crate) logs_len: usize,
}

impl StateCheckpoint {
    /// Creates a checkpoint from journal and log cursors.
    #[inline]
    pub const fn new(journal_len: usize, logs_len: usize) -> Self {
        Self { journal_len, logs_len }
    }

    /// Returns the revert-journal cursor captured by this checkpoint.
    #[inline]
    pub const fn journal_len(&self) -> usize {
        self.journal_len
    }

    /// Returns the emitted-log cursor captured by this checkpoint.
    #[inline]
    pub const fn logs_len(&self) -> usize {
        self.logs_len
    }
}

/// Compact journal entry for reverting state changes.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum JournalEntry {
    /// Account overlay snapshot recorded before the first mutation made through an
    /// [`AccountHandle`](super::AccountHandle), reverting the present account value and all
    /// per-account flags in one entry.
    AccountChange {
        /// Account address.
        address: Address,
        /// Previous present account value.
        previous: Option<AccountInfo>,
        /// Previous lifecycle flags.
        previous_flags: AccountFlags,
    },
    /// Persistent storage changed.
    StorageChange {
        /// Account address.
        address: Address,
        /// Storage key.
        key: Word,
        /// Previous current storage value.
        previous: Word,
    },
    /// Transient storage changed.
    TransientStorageChange {
        /// Account address.
        address: Address,
        /// Storage key.
        key: Word,
        /// Previous transient storage value.
        previous: Option<Word>,
    },
    /// Storage slot was warmed by EIP-2929 access tracking.
    StorageWarmed {
        /// Account address.
        address: Address,
        /// Storage key.
        key: Word,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        SpecId, Version,
        evm::{
            CacheDB,
            state::{AccountInfo, State},
        },
    };
    use alloc::vec::Vec;
    use alloy_primitives::{Bytes, Log};

    #[test]
    fn destruct_change_rolls_back_to_checkpoint() {
        let address = Address::repeat_byte(0x33);
        let mut state = State::new(CacheDB::default());

        let checkpoint = state.checkpoint();
        state.account(&address).unwrap().mark_destructed();

        assert!(state.account(&address).unwrap().is_destructed());
        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert!(!state.account(&address).unwrap().is_destructed());
    }

    #[test]
    fn log_rolls_back_to_checkpoint() {
        let mut state = State::new(CacheDB::default());
        let kept =
            Log::new_unchecked(Address::repeat_byte(0x44), Vec::new(), Bytes::from_static(&[0x01]));
        let reverted =
            Log::new_unchecked(Address::repeat_byte(0x55), Vec::new(), Bytes::from_static(&[0x02]));

        state.log(kept.clone());
        let checkpoint = state.checkpoint();
        state.log(reverted);

        assert_eq!(
            state.logs(),
            &[
                kept.clone(),
                Log::new_unchecked(
                    Address::repeat_byte(0x55),
                    Vec::new(),
                    Bytes::from_static(&[0x02])
                )
            ]
        );
        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert_eq!(state.logs(), &[kept]);
    }

    #[rstest::rstest]
    #[case(SpecId::HOMESTEAD, false)]
    #[case(SpecId::SPURIOUS_DRAGON, true)]
    fn rollback_preserves_precompile3_touch_only_after_eip161(
        #[case] spec: SpecId,
        #[case] touched: bool,
    ) {
        let precompile3 = Address::with_last_byte(3);
        let other = Address::with_last_byte(4);
        let mut database = CacheDB::default();
        database.insert_account_info(&precompile3, AccountInfo::default());
        database.insert_account_info(&other, AccountInfo::default());
        let mut state = State::new(database);

        let checkpoint = state.checkpoint();
        state.account(&precompile3).unwrap().touch();
        state.account(&other).unwrap().touch();

        state.rollback(checkpoint, Version::base(spec).features);
        assert_eq!(state.account(&precompile3).unwrap().is_touched(), touched);
        assert!(!state.account(&other).unwrap().is_touched());

        // Finalization keeps the touch in the reported changes, also when run twice.
        state.finalize_transaction(Version::base(spec)).unwrap();
        state.finalize_transaction(Version::base(spec)).unwrap();
        let pending = state.take_pending_state();
        let change = pending.account_changes_for(&precompile3).unwrap();
        assert_eq!(change.touched, touched);
        assert_eq!(change.current.is_none(), touched);
        assert!(!pending.account_changes_for(&other).unwrap().touched);
    }

    #[test]
    fn non_revertible_warmth_is_not_journaled_or_rolled_back() {
        let base_account = Address::with_last_byte(0x10);
        let frame_account = Address::with_last_byte(0x11);
        let base_storage = Address::with_last_byte(0x12);
        let frame_storage = Address::with_last_byte(0x13);
        let key = Word::ONE;
        let mut state = State::new(CacheDB::default());

        state.prewarm(&base_account);
        state.prewarm_storage_slot(&base_storage, key);
        assert!(state.journal.is_empty());

        let checkpoint = state.checkpoint();
        assert!(state.account(&frame_account).unwrap().warm());
        assert!(state.storage_slot(&frame_storage, key).unwrap().warm());
        // The load itself is an un-journaled read cache; warming the frame account records an
        // AccountChange and warming the slot records StorageWarmed: two revertible entries in
        // total.
        assert_eq!(state.journal.len(), 2);

        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert!(state.account(&base_account).unwrap().is_warm());
        assert!(state.storage_slot(&base_storage, key).unwrap().is_warm());
        assert!(!state.account(&frame_account).unwrap().is_warm());
        assert!(!state.storage_slot(&frame_storage, key).unwrap().is_warm());
    }

    #[test]
    fn warm_only_entries_do_not_emit_state_changes() {
        let account = Address::with_last_byte(0x14);
        let storage_account = Address::with_last_byte(0x15);
        let key = Word::ONE;
        let mut state = State::new(CacheDB::default());

        state.prewarm(&account);
        state.prewarm_storage_slot(&storage_account, key);

        let pending = state.take_pending_state();
        assert!(pending.is_empty());
        assert!(state.account(&account).unwrap().is_warm());
        assert!(state.storage_slot(&storage_account, key).unwrap().is_warm());

        state.clear_transaction_state();
        assert!(!state.account(&account).unwrap().is_warm());
        assert!(!state.storage_slot(&storage_account, key).unwrap().is_warm());
    }

    #[test]
    fn rollback_preserves_non_revertible_account_warmth_after_load() {
        let account = Address::with_last_byte(0x16);
        let mut database = CacheDB::default();
        database.insert_account_info(&account, AccountInfo::default().with_balance(Word::ONE));
        let mut state = State::new(database);

        state.prewarm(&account);
        let checkpoint = state.checkpoint();
        assert!(state.account(&account).unwrap().exists());
        assert!(state.account(&account).unwrap().get().is_some());

        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        // The load is an un-journaled read cache, so it survives rollback. Warmth comes from the
        // non-revertible base warm set, and the unchanged cached entry emits no state change.
        assert!(state.prewarm_set().is_warm(&account));
        assert!(state.account(&account).unwrap().is_warm());
        assert!(!state.take_pending_state().is_changed());
    }

    #[test]
    fn rollback_preserves_non_revertible_storage_warmth_after_write() {
        let account = Address::with_last_byte(0x17);
        let key = Word::ONE;
        let mut database = CacheDB::default();
        database.insert_account_info(&account, AccountInfo::default().with_balance(Word::ONE));
        let mut state = State::new(database);

        state.prewarm_storage_slot(&account, key);
        let checkpoint = state.checkpoint();
        state.storage_slot(&account, key).unwrap().write(Word::from(7));
        assert_eq!(state.storage_slot(&account, key).unwrap().current(), Word::from(7));

        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert!(state.storage_slot(&account, key).unwrap().is_warm());
        assert!(!state.take_pending_state().is_changed());
    }

    #[test]
    fn rollback_reverts_storage_warmth_without_discarding_cached_value() {
        let account = Address::with_last_byte(0x18);
        let key = Word::ONE;
        let value = Word::from(9);
        let mut database = CacheDB::default();
        database.insert_account_info(&account, AccountInfo::default().with_balance(Word::ONE));
        database.insert_account_storage(&account, &key, &value);
        let mut state = State::new(database);

        let checkpoint = state.checkpoint();
        assert!(state.storage_slot(&account, key).unwrap().warm());
        assert_eq!(state.storage_slot(&account, key).unwrap().current(), value);

        state.rollback(checkpoint, Version::base(SpecId::FRONTIER).features);
        assert!(!state.storage_slot(&account, key).unwrap().is_warm());
        assert_eq!(state.storage_slot(&account, key).unwrap().current(), value);
        assert!(!state.take_pending_state().is_changed());

        assert!(state.storage_slot(&account, key).unwrap().warm());
    }
}
