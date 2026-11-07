//! Block-level state accumulation.

use super::{
    AccountChangeRef, AccountInfo, StateChangeSink, StateChangeSource, StorageChange, Tracked,
};
use crate::{bytecode::Bytecode, interpreter::Word, storage_key::StorageKey};
use alloc::vec::Vec;
use alloy_primitives::{
    Address, B256,
    map::{AddressMap, AddressSet, B256Map, U256Map, hash_map},
};
use core::convert::Infallible;

/// Mutable block-level state accumulator.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BlockStateAccumulator {
    accounts: AddressMap<Tracked<Option<AccountInfo>>>,
    storage_wipes: AddressSet,
    /// Storage deltas keyed by account, then slot. Accounts have no empty slot maps, so wiping or
    /// recreating an account touches only its own slots.
    storage: AddressMap<U256Map<Tracked<Word>>>,
    code: B256Map<Bytecode>,
}

impl BlockStateAccumulator {
    /// Creates an empty block state accumulator.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns whether the accumulator contains no state changes.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
            && self.storage_wipes.is_empty()
            && self.storage.is_empty()
            && self.code.is_empty()
    }

    /// Returns account deltas with addresses in arbitrary map order.
    #[inline]
    pub fn accounts(&self) -> impl Iterator<Item = (Address, &Tracked<Option<AccountInfo>>)> {
        self.accounts.iter().map(|(&address, delta)| (address, delta))
    }

    /// Returns storage-wipe addresses in arbitrary set order.
    ///
    /// An account deleted by the block is not listed: its deletion already wipes its storage.
    #[inline]
    pub fn storage_wipes(&self) -> impl Iterator<Item = Address> + '_ {
        self.storage_wipes.iter().copied().filter(|address| {
            self.accounts.get(address).is_none_or(|delta| delta.current.is_some())
        })
    }

    /// Returns storage deltas with storage keys in arbitrary map order.
    ///
    /// If a slot's address appears in [`Self::storage_wipes`], consumers should apply the wipe
    /// before this slot and treat [`Tracked::current`] as the value to write after the wipe.
    #[inline]
    pub fn storage(&self) -> impl Iterator<Item = (StorageKey, &Tracked<Word>)> {
        self.storage.iter().flat_map(|(&address, slots)| {
            slots.iter().map(move |(&key, delta)| (StorageKey::new(address, key), delta))
        })
    }

    /// Returns bytecode entries in arbitrary map order.
    #[inline]
    pub fn code(&self) -> impl Iterator<Item = (&B256, &Bytecode)> {
        self.code.iter()
    }

    /// Returns account deltas with addresses sorted by address.
    pub fn accounts_sorted(&self) -> Vec<(Address, &Tracked<Option<AccountInfo>>)> {
        let mut accounts = self.accounts().collect::<Vec<_>>();
        accounts.sort_by_key(|(address, _)| *address);
        accounts
    }

    /// Returns storage-wipe addresses sorted by address.
    pub fn storage_wipes_sorted(&self) -> Vec<Address> {
        let mut storage_wipes = self.storage_wipes().collect::<Vec<_>>();
        storage_wipes.sort_unstable();
        storage_wipes
    }

    /// Returns storage deltas with storage keys sorted by address and slot.
    pub fn storage_sorted(&self) -> Vec<(StorageKey, &Tracked<Word>)> {
        let mut storage = self.storage().collect::<Vec<_>>();
        storage.sort_unstable_by_key(|(key, _)| *key);
        storage
    }

    /// Returns bytecode entries sorted by code hash.
    ///
    /// The counterpart to [`Self::accounts_sorted`], [`Self::storage_sorted`], and
    /// [`Self::storage_wipes_sorted`]: [`Self::code`] iterates a map, so a consumer
    /// that needs a deterministic enumeration of the whole accumulator would
    /// otherwise have to sort this one collection itself.
    pub fn code_sorted(&self) -> Vec<(&B256, &Bytecode)> {
        let mut code = self.code().collect::<Vec<_>>();
        code.sort_unstable_by_key(|(hash, _)| *hash);
        code
    }
}

impl StateChangeSink for BlockStateAccumulator {
    type Error = Infallible;

    #[inline]
    fn bytecode(&mut self, code_hash: B256, code: &Bytecode) -> Result<(), Self::Error> {
        self.code.entry(code_hash).or_insert_with(|| code.clone());
        Ok(())
    }

    fn account(&mut self, change: AccountChangeRef<'_>) -> Result<(), Self::Error> {
        let original = change.original.map(AccountInfo::clone_no_code);
        let current = change.current.map(AccountInfo::clone_no_code);
        let deletes_account = current.is_none();
        let mut created = false;

        match self.accounts.entry(change.address) {
            hash_map::Entry::Occupied(mut entry) => {
                let delta = entry.get_mut();
                delta.set_current(current);
                if !delta.is_changed() {
                    entry.remove();
                }
            }
            hash_map::Entry::Vacant(entry) => {
                if original != current {
                    created = original.is_none();
                    entry.insert(Tracked::from_parts(original, current));
                }
            }
        }

        if deletes_account {
            // A delta left after a deletion means the account existed before the block, so its
            // pre-block storage is destroyed. The marker stays so a later revival still wipes it.
            if self.accounts.contains_key(&change.address) {
                self.storage_wipes.insert(change.address);
            } else {
                self.storage_wipes.remove(&change.address);
            }
            self.storage.remove(&change.address);
        } else if created {
            // Only a newly created delta can have slots recorded against pre-block storage: while
            // the delta exists, `storage` and `storage_wipe` already treat the account as created.
            if let hash_map::Entry::Occupied(mut entry) = self.storage.entry(change.address) {
                entry.get_mut().retain(|_, delta| {
                    delta.original = Word::ZERO;
                    !delta.current.is_zero()
                });
                if entry.get().is_empty() {
                    entry.remove();
                }
            }
            self.storage_wipes.remove(&change.address);
        }
        Ok(())
    }

    fn storage_wipe(&mut self, address: Address) -> Result<(), Self::Error> {
        let record_wipe = self.accounts.get(&address).is_none_or(|delta| delta.original.is_some());
        if record_wipe {
            self.storage_wipes.insert(address);
        }
        self.storage.remove(&address);
        Ok(())
    }

    fn storage(&mut self, change: StorageChange) -> Result<(), Self::Error> {
        let account_created =
            self.accounts.get(&change.address).is_some_and(|account| account.original.is_none());
        let storage_replaced = self.storage_wipes.contains(&change.address) || account_created;
        let slots = self.storage.entry(change.address).or_default();
        match slots.entry(change.key) {
            hash_map::Entry::Occupied(mut entry) => {
                let delta = entry.get_mut();
                delta.set_current(change.current);
                if (storage_replaced && delta.current.is_zero())
                    || (!storage_replaced && !delta.is_changed())
                {
                    entry.remove();
                }
            }
            hash_map::Entry::Vacant(entry) => {
                if (storage_replaced && !change.current.is_zero())
                    || (!storage_replaced && change.original != change.current)
                {
                    let original = if account_created { Word::ZERO } else { change.original };
                    entry.insert(Tracked::from_parts(original, change.current));
                }
            }
        }
        if slots.is_empty() {
            self.storage.remove(&change.address);
        }
        Ok(())
    }
}

impl StateChangeSource for BlockStateAccumulator {
    #[inline]
    fn visit<S: StateChangeSink>(&self, sink: &mut S) -> Result<(), S::Error> {
        visit_block_changes(
            &self.accounts,
            &self.storage_wipes_sorted(),
            &self.storage,
            &self.code,
            sink,
        )
    }
}

fn visit_block_changes<S: StateChangeSink>(
    accounts: &AddressMap<Tracked<Option<AccountInfo>>>,
    storage_wipes: &[Address],
    storage: &AddressMap<U256Map<Tracked<Word>>>,
    code: &B256Map<Bytecode>,
    sink: &mut S,
) -> Result<(), S::Error> {
    let mut code_entries = code.iter().collect::<Vec<_>>();
    code_entries.sort_by_key(|(code_hash, _)| **code_hash);
    for (&code_hash, code) in code_entries {
        sink.bytecode(code_hash, code)?;
    }

    for &address in storage_wipes {
        sink.storage_wipe(address)?;
    }

    let mut storage_accounts = storage.iter().collect::<Vec<_>>();
    storage_accounts.sort_unstable_by_key(|entry| *entry.0);
    let mut storage_deltas = Vec::new();
    for (&address, slots) in storage_accounts {
        storage_deltas.extend(slots.iter());
        storage_deltas.sort_unstable_by_key(|entry| *entry.0);
        for (&key, delta) in storage_deltas.drain(..) {
            sink.storage(StorageChange {
                address,
                key,
                original: delta.original,
                current: delta.current,
            })?;
        }
    }

    let mut account_deltas = accounts.iter().collect::<Vec<_>>();
    account_deltas.sort_by_key(|entry| *entry.0);
    for (address, delta) in account_deltas {
        sink.account(AccountChangeRef {
            address: *address,
            original: delta.original.as_ref(),
            current: delta.current.as_ref(),
            // Block-level aggregation loses per-transaction lifecycle flags.
            created: false,
            selfdestructed: false,
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        super::{
            Account, AccountChangeRef, AccountInfo, PendingState, StateChangeSource, StorageChange,
            StorageOverlay, StorageSlot, Tracked,
        },
        BlockStateAccumulator,
    };
    use crate::{interpreter::Word, test_utils::storage_slot};
    use alloy_primitives::{Address, map::U256Map};
    use core::convert::Infallible;

    use super::super::StateChangeSink;
    use crate::bytecode::Bytecode;
    use alloc::{vec, vec::Vec};
    use alloy_primitives::{B256, Bytes};

    #[test]
    fn code_sorted_orders_by_hash() {
        let mut block = BlockStateAccumulator::new();
        // Inserted descending, so map order cannot be mistaken for sorted order.
        for byte in [0xcc_u8, 0x11, 0x77] {
            block
                .bytecode(
                    B256::repeat_byte(byte),
                    &Bytecode::new_raw_checked(Bytes::from(vec![byte])).unwrap(),
                )
                .unwrap();
        }

        let hashes: Vec<_> = block.code_sorted().iter().map(|(hash, _)| **hash).collect();
        assert_eq!(
            hashes,
            vec![B256::repeat_byte(0x11), B256::repeat_byte(0x77), B256::repeat_byte(0xcc)]
        );
        // Same entries as the unsorted accessor, only ordered.
        assert_eq!(block.code_sorted().len(), block.code().count());
    }

    fn changes(
        address: Address,
        original: Option<AccountInfo>,
        current: Option<AccountInfo>,
        wiped: bool,
        slots: U256Map<StorageSlot>,
    ) -> PendingState {
        let mut pending = PendingState::default();
        pending.accounts.insert(address, Account::new(original, current));
        if wiped || !slots.is_empty() {
            pending.accounts.entry(address).or_default().storage =
                StorageOverlay { wiped, slots, _non_exhaustive: () };
        }
        pending
    }

    fn slot(key: Word, original: Word, current: Word) -> U256Map<StorageSlot> {
        U256Map::from_iter([(key, storage_slot(original, current))])
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_binary_roundtrip() {
        let mut accumulator = BlockStateAccumulator::new();
        let code_hash = B256::with_last_byte(1);
        let bytecode = Bytecode::new_raw_checked(vec![0x60, 0x00].into()).unwrap();
        accumulator.bytecode(code_hash, &bytecode).unwrap();

        let encoded = postcard::to_allocvec(&accumulator).unwrap();
        let deserialized: BlockStateAccumulator = postcard::from_bytes(&encoded).unwrap();

        assert_eq!(deserialized, accumulator);
    }

    #[test]
    fn block_accumulator_collapses_create_then_delete() {
        let address = Address::repeat_byte(0x50);
        let key = Word::ONE;
        let created = AccountInfo::default().with_nonce(1);
        let mut accumulator = BlockStateAccumulator::new();

        let create = changes(
            address,
            None,
            Some(created.clone()),
            true,
            slot(key, Word::ZERO, Word::from(7)),
        );
        create.visit(&mut accumulator).expect("block accumulator is infallible");

        let delete = changes(address, Some(created), None, true, U256Map::default());
        delete.visit(&mut accumulator).expect("block accumulator is infallible");

        assert!(accumulator.accounts_sorted().is_empty());
        assert!(accumulator.storage_wipes_sorted().is_empty());
        assert!(accumulator.storage_sorted().is_empty());
    }

    #[test]
    fn block_accumulator_preserves_original_for_delete_then_recreate() {
        let address = Address::repeat_byte(0x51);
        let key = Word::ONE;
        let original = AccountInfo::default().with_balance(Word::from(3));
        let recreated = AccountInfo::default().with_nonce(1);
        let mut accumulator = BlockStateAccumulator::new();

        let delete = changes(address, Some(original.clone()), None, true, U256Map::default());
        delete.visit(&mut accumulator).expect("block accumulator is infallible");

        let create = changes(
            address,
            None,
            Some(recreated.clone()),
            true,
            slot(key, Word::ZERO, Word::from(7)),
        );
        create.visit(&mut accumulator).expect("block accumulator is infallible");

        let accounts = accumulator.accounts_sorted();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].1.original.as_ref(), Some(&original));
        assert_eq!(accounts[0].1.current.as_ref(), Some(&recreated));
        assert_eq!(accumulator.storage_wipes_sorted(), [address]);

        let storage = accumulator.storage_sorted();
        assert_eq!(storage.len(), 1);
        assert_eq!(storage[0].0.key(), key);
        assert_eq!(storage[0].1.current, Word::from(7));
    }

    /// A change a sink observed, in arrival order.
    #[derive(Debug, Eq, PartialEq)]
    enum Event {
        Wipe(Address),
        Storage { address: Address, key: Word, original: Word, current: Word },
        Account { address: Address, original: Option<AccountInfo>, current: Option<AccountInfo> },
    }

    #[derive(Default)]
    struct Recorder(Vec<Event>);

    impl StateChangeSink for Recorder {
        type Error = Infallible;

        fn account(&mut self, change: AccountChangeRef<'_>) -> Result<(), Self::Error> {
            self.0.push(Event::Account {
                address: change.address,
                original: change.original.cloned(),
                current: change.current.cloned(),
            });
            Ok(())
        }

        fn storage_wipe(&mut self, address: Address) -> Result<(), Self::Error> {
            self.0.push(Event::Wipe(address));
            Ok(())
        }

        fn storage(&mut self, change: StorageChange) -> Result<(), Self::Error> {
            self.0.push(Event::Storage {
                address: change.address,
                key: change.key,
                original: change.original,
                current: change.current,
            });
            Ok(())
        }
    }

    fn emitted(accumulator: &BlockStateAccumulator) -> Vec<Event> {
        let mut recorder = Recorder::default();
        accumulator.visit(&mut recorder).expect("recorder is infallible");
        recorder.0
    }

    #[test]
    fn block_accumulator_emits_only_the_deletion_for_deleted_account() {
        // The deletion wipes the account's storage at a sink by itself, so a sink sees no
        // separate wipe even though the accumulator keeps the marker for a later revival.
        let address = Address::from([0x58; 20]);
        let original = AccountInfo::default().with_balance(Word::from(3));
        let mut accumulator = BlockStateAccumulator::new();

        changes(address, Some(original.clone()), None, true, U256Map::default())
            .visit(&mut accumulator)
            .expect("block accumulator is infallible");

        assert!(accumulator.storage_wipes_sorted().is_empty());
        assert_eq!(
            emitted(&accumulator),
            [Event::Account { address, original: Some(original), current: None }]
        );
    }

    #[test]
    fn block_accumulator_keeps_wipe_when_deleted_account_is_revived() {
        // Selfdestruct followed by a revival in a later transaction of the same block (a plain
        // transfer or a re-create): the revived account is a live account, which does not wipe
        // storage at a sink by itself, so the sink must still see the wipe.
        let address = Address::repeat_byte(0x55);
        let original = AccountInfo::default().with_balance(Word::from(3));
        let revived = AccountInfo::default().with_balance(Word::ONE);
        let mut accumulator = BlockStateAccumulator::new();

        changes(address, Some(original.clone()), None, true, U256Map::default())
            .visit(&mut accumulator)
            .expect("block accumulator is infallible");
        changes(address, None, Some(revived.clone()), false, U256Map::default())
            .visit(&mut accumulator)
            .expect("block accumulator is infallible");

        assert_eq!(accumulator.storage_wipes_sorted(), [address]);
        assert_eq!(
            emitted(&accumulator),
            [
                Event::Wipe(address),
                Event::Account {
                    address,
                    original: Some(original.clone()),
                    current: Some(revived)
                },
            ]
        );

        // Reviving back to a value equal to the original collapses the account delta, but the
        // wipe still stands: the account's pre-block storage was destroyed.
        let mut collapsing = BlockStateAccumulator::new();
        changes(address, Some(original.clone()), None, true, U256Map::default())
            .visit(&mut collapsing)
            .expect("block accumulator is infallible");
        changes(address, None, Some(original), false, U256Map::default())
            .visit(&mut collapsing)
            .expect("block accumulator is infallible");
        assert_eq!(emitted(&collapsing), [Event::Wipe(address)]);
    }

    #[test]
    fn block_accumulator_emits_wipe_and_new_slots_when_deleted_account_is_recreated() {
        let address = Address::from([0x59; 20]);
        let (kept, rewritten) = (Word::from(1), Word::from(2));
        let original = AccountInfo::default().with_balance(Word::from(3));
        let recreated = AccountInfo::default().with_nonce(1);
        let mut accumulator = BlockStateAccumulator::new();

        // A pre-block slot changes, then the account is deleted, then re-created with a new slot.
        changes(
            address,
            Some(original.clone()),
            Some(original.clone()),
            false,
            slot(kept, Word::from(5), Word::from(6)),
        )
        .visit(&mut accumulator)
        .expect("block accumulator is infallible");
        changes(address, Some(original.clone()), None, true, U256Map::default())
            .visit(&mut accumulator)
            .expect("block accumulator is infallible");
        changes(
            address,
            None,
            Some(recreated.clone()),
            true,
            slot(rewritten, Word::ZERO, Word::from(7)),
        )
        .visit(&mut accumulator)
        .expect("block accumulator is infallible");

        assert_eq!(
            emitted(&accumulator),
            [
                Event::Wipe(address),
                Event::Storage {
                    address,
                    key: rewritten,
                    original: Word::ZERO,
                    current: Word::from(7),
                },
                Event::Account { address, original: Some(original), current: Some(recreated) },
            ]
        );
    }

    #[test]
    fn block_accumulator_keeps_nonzero_write_after_storage_wipe() {
        let address = Address::repeat_byte(0x52);
        let key = Word::ONE;
        let original = AccountInfo::default().with_balance(Word::from(3));
        let mut accumulator = BlockStateAccumulator::new();

        let wipe_and_restore = changes(
            address,
            Some(original.clone()),
            Some(original),
            true,
            slot(key, Word::ZERO, Word::from(5)),
        );
        wipe_and_restore.visit(&mut accumulator).expect("block accumulator is infallible");

        assert!(accumulator.accounts_sorted().is_empty());
        assert_eq!(accumulator.storage_wipes_sorted(), [address]);

        let storage = accumulator.storage_sorted();
        assert_eq!(storage.len(), 1);
        assert_eq!(storage[0].0.key(), key);
        assert_eq!(storage[0].1.current, Word::from(5));
    }

    #[test]
    fn block_accumulator_reinserts_unchanged_slot_after_wiping_created_account() {
        let address = Address::repeat_byte(0x53);
        let key = Word::ONE;
        let account = AccountInfo::default().with_nonce(1);
        let mut accumulator = BlockStateAccumulator::new();

        changes(address, None, Some(account.clone()), true, slot(key, Word::ZERO, Word::from(5)))
            .visit(&mut accumulator)
            .expect("block accumulator is infallible");
        changes(
            address,
            Some(account.clone()),
            Some(account),
            true,
            slot(key, Word::from(5), Word::from(5)),
        )
        .visit(&mut accumulator)
        .expect("block accumulator is infallible");

        assert!(accumulator.storage_wipes_sorted().is_empty(), "creation subsumes the wipe");
        let storage = accumulator.storage_sorted();
        assert_eq!(storage.len(), 1);
        assert_eq!(storage[0].0.key(), key);
        assert_eq!(storage[0].1.original, Word::ZERO);
        assert_eq!(storage[0].1.current, Word::from(5));

        let mut streamed = BlockStateAccumulator::new();
        accumulator.visit(&mut streamed).expect("block accumulator is infallible");
        assert_eq!(streamed.storage_sorted(), storage);
    }

    #[test]
    fn block_accumulator_normalizes_storage_when_creation_arrives_after_slots() {
        let address = Address::repeat_byte(0x54);
        let key = Word::ONE;
        let account = AccountInfo::default().with_nonce(1);
        let mut accumulator = BlockStateAccumulator::new();

        changes(address, None, Some(account), true, slot(key, Word::from(5), Word::from(5)))
            .visit(&mut accumulator)
            .expect("block accumulator is infallible");

        assert!(accumulator.storage_wipes_sorted().is_empty(), "creation subsumes the wipe");
        let storage = accumulator.storage_sorted();
        assert_eq!(storage.len(), 1);
        assert_eq!(storage[0].1, &Tracked::from_parts(Word::ZERO, Word::from(5)));

        let mut streamed = BlockStateAccumulator::new();
        accumulator.visit(&mut streamed).expect("block accumulator is infallible");
        assert_eq!(streamed.storage_sorted(), storage);
    }

    #[test]
    fn block_accumulator_deletion_subsumes_storage_writes() {
        let address = Address::repeat_byte(0x56);
        let key = Word::ONE;
        let original = AccountInfo::default().with_balance(Word::from(3));
        let mut accumulator = BlockStateAccumulator::new();

        let delete =
            changes(address, Some(original), None, true, slot(key, Word::from(5), Word::from(7)));
        delete.visit(&mut accumulator).expect("block accumulator is infallible");

        let accounts = accumulator.accounts_sorted();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].0, address);
        assert!(accounts[0].1.current.is_none());
        assert!(accumulator.storage_wipes_sorted().is_empty());
        assert!(accumulator.storage_sorted().is_empty());
    }

    #[test]
    fn block_accumulator_collapses_storage_wipe_write_wipe() {
        let address = Address::repeat_byte(0x52);
        let key = Word::ONE;
        let mut accumulator = BlockStateAccumulator::new();

        let first = changes(address, None, None, true, slot(key, Word::from(5), Word::from(7)));
        first.visit(&mut accumulator).expect("block accumulator is infallible");
        changes(address, None, None, true, U256Map::default())
            .visit(&mut accumulator)
            .expect("block accumulator is infallible");

        assert!(accumulator.accounts_sorted().is_empty());
        assert_eq!(accumulator.storage_wipes_sorted(), [address]);
        assert!(accumulator.storage_sorted().is_empty());
    }

    #[test]
    fn block_accumulator_drops_storage_reverted_to_original() {
        let address = Address::repeat_byte(0x57);
        let key = Word::ONE;
        let mut accumulator = BlockStateAccumulator::new();

        changes(address, None, None, false, slot(key, Word::from(3), Word::from(4)))
            .visit(&mut accumulator)
            .expect("block accumulator is infallible");
        changes(address, None, None, false, slot(key, Word::from(4), Word::from(3)))
            .visit(&mut accumulator)
            .expect("block accumulator is infallible");

        assert!(accumulator.is_empty());
    }

    #[test]
    fn block_accumulator_isolates_storage_between_accounts() {
        let [a, b, c, d, e] = [0x61, 0x62, 0x63, 0x64, 0x65].map(Address::repeat_byte);
        let (k1, k2) = (Word::ONE, Word::from(2));
        let info = AccountInfo::default().with_nonce(1);
        let w = Word::from;
        let mut accumulator = BlockStateAccumulator::new();

        // The same slot keys change in several accounts.
        for address in [a, b, d] {
            let mut slots = slot(k1, w(3), w(4));
            slots.extend(slot(k2, w(5), w(6)));
            changes(address, None, None, false, slots)
                .visit(&mut accumulator)
                .expect("block accumulator is infallible");
        }
        // Slot 2 of `c` is written to zero, so only slot 1 survives its creation.
        let mut slots = slot(k1, w(3), w(4));
        slots.extend(slot(k2, w(5), Word::ZERO));
        changes(c, None, None, false, slots)
            .visit(&mut accumulator)
            .expect("block accumulator is infallible");
        // All of the slots of `e` are zero after its creation, so it keeps no slot map.
        changes(e, None, None, false, slot(k1, w(3), Word::ZERO))
            .visit(&mut accumulator)
            .expect("block accumulator is infallible");
        assert_eq!(accumulator.storage_sorted().len(), 9);

        // Deleting `b`, wiping `d`, and creating `c` and `e` change only their own slots.
        changes(b, Some(info.clone()), None, true, U256Map::default())
            .visit(&mut accumulator)
            .expect("block accumulator is infallible");
        changes(d, Some(info.clone()), Some(info.clone()), true, U256Map::default())
            .visit(&mut accumulator)
            .expect("block accumulator is infallible");
        for address in [c, e] {
            changes(address, None, Some(info.clone()), false, U256Map::default())
                .visit(&mut accumulator)
                .expect("block accumulator is infallible");
        }
        // Slot 1 of `a` is overwritten.
        changes(a, None, None, false, slot(k1, w(4), w(9)))
            .visit(&mut accumulator)
            .expect("block accumulator is infallible");

        assert_eq!(accumulator.storage_wipes_sorted(), [d]);
        let storage = accumulator.storage_sorted();
        let storage: Vec<_> =
            storage.iter().map(|(key, delta)| (key.address(), key.key(), **delta)).collect();
        assert_eq!(
            storage,
            [
                (a, k1, Tracked::from_parts(w(3), w(9))),
                (a, k2, Tracked::from_parts(w(5), w(6))),
                (c, k1, Tracked::from_parts(Word::ZERO, w(4))),
            ]
        );

        // Replaying the deltas into another accumulator yields the same state.
        let mut merged = BlockStateAccumulator::new();
        accumulator.visit(&mut merged).expect("block accumulator is infallible");
        assert_eq!(merged, accumulator);
    }

    #[test]
    fn block_accumulator_keeps_account_only_and_storage_only_changes_separate() {
        let account_address = Address::repeat_byte(0x53);
        let storage_address = Address::repeat_byte(0x54);
        let key = Word::ONE;
        let original = AccountInfo::default().with_balance(Word::ONE);
        let current = AccountInfo::default().with_balance(Word::from(2));
        let mut accumulator = BlockStateAccumulator::new();

        changes(
            account_address,
            Some(original.clone()),
            Some(current.clone()),
            false,
            U256Map::default(),
        )
        .visit(&mut accumulator)
        .expect("block accumulator is infallible");
        changes(storage_address, None, None, false, slot(key, Word::from(3), Word::from(4)))
            .visit(&mut accumulator)
            .expect("block accumulator is infallible");

        let accounts = accumulator.accounts_sorted();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].0, account_address);
        assert_eq!(accounts[0].1.original.as_ref(), Some(&original));
        assert_eq!(accounts[0].1.current.as_ref(), Some(&current));
        assert!(accumulator.storage_wipes_sorted().is_empty());

        let storage = accumulator.storage_sorted();
        assert_eq!(storage.len(), 1);
        assert_eq!(storage[0].0.address(), storage_address);
        assert_eq!(storage[0].0.key(), key);
        assert_eq!(storage[0].1.original, Word::from(3));
        assert_eq!(storage[0].1.current, Word::from(4));
    }
}
