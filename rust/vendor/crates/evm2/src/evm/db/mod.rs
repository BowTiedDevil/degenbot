//! Database helpers for the EVM state overlay.

use super::{NonStaticAny, state::AccountInfo};
use crate::{DatabaseError, bytecode::Bytecode, interpreter::Word};
use alloc::{boxed::Box, string::ToString};
use alloy_primitives::{Address, B256, keccak256};
use auto_impl::auto_impl;
use core::error::Error;

mod cache;
pub use cache::{AccountStorageCache, Cache, CacheDB, InMemoryDB};

/// Result of a database operation.
pub type DbResult<T> = Result<T, DatabaseError>;

/// Backing database implementation with a concrete error type.
#[auto_impl(&mut, Box)]
pub trait Database: NonStaticAny {
    /// Database error type.
    type Error: Error + Send + Sync + 'static;

    /// Whether a concrete database error is an internal failure rather than invalid input.
    fn is_fatal(error: &Self::Error) -> bool {
        let _ = error;
        true
    }

    /// Loads account information.
    fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error>;

    /// Loads bytecode by code hash.
    fn get_code_by_hash(&mut self, code_hash: &B256) -> Result<Bytecode, Self::Error>;

    /// Loads a persistent storage slot.
    fn get_storage(&mut self, address: &Address, key: &Word) -> Result<Word, Self::Error>;

    /// Loads a historical block hash.
    ///
    /// Callers only request numbers inside the `BLOCKHASH` window, so a hash the database cannot
    /// provide is a database failure reported through the error, never a benign miss.
    fn get_block_hash(&mut self, number: &Word) -> Result<B256, Self::Error>;
}

/// Object-safe database adapter for typed database implementations.
#[derive(Clone, Debug)]
pub struct Db<T: Database> {
    db: T,
}

impl<T: Database + Default> Default for Db<T> {
    #[inline]
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: Database> Db<T> {
    /// Creates a new database adapter.
    #[inline]
    pub const fn new(db: T) -> Self {
        Self { db }
    }

    /// Returns the wrapped database.
    #[inline]
    pub const fn inner(&self) -> &T {
        &self.db
    }

    /// Returns the wrapped database mutably.
    #[inline]
    pub const fn inner_mut(&mut self) -> &mut T {
        &mut self.db
    }

    /// Consumes the adapter and returns the wrapped database.
    #[inline]
    pub fn into_inner(self) -> T {
        self.db
    }
}

impl<T: Database> DynDatabase for Db<T> {
    #[inline]
    fn get_account(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        self.db.get_account(address).map_err(|err| {
            let fatal = T::is_fatal(&err);
            DatabaseError::new(err, fatal)
        })
    }

    #[inline]
    fn get_code_by_hash(&mut self, code_hash: &B256) -> DbResult<Bytecode> {
        self.db.get_code_by_hash(code_hash).map_err(|err| {
            let fatal = T::is_fatal(&err);
            DatabaseError::new(err, fatal)
        })
    }

    #[inline]
    fn get_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        self.db.get_storage(address, key).map_err(|err| {
            let fatal = T::is_fatal(&err);
            DatabaseError::new(err, fatal)
        })
    }

    #[inline]
    fn get_block_hash(&mut self, number: &Word) -> DbResult<B256> {
        self.db.get_block_hash(number).map_err(|err| {
            let fatal = T::is_fatal(&err);
            DatabaseError::new(err, fatal)
        })
    }
}

/// Backing database view used to initialize mutable [`super::State`].
#[auto_impl(&mut, Box)]
pub trait DynDatabase: NonStaticAny {
    /// Loads account information.
    fn get_account(&mut self, address: &Address) -> DbResult<Option<AccountInfo>>;

    /// Loads bytecode by code hash.
    fn get_code_by_hash(&mut self, code_hash: &B256) -> DbResult<Bytecode>;

    /// Loads a persistent storage slot.
    fn get_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word>;

    /// Loads a historical block hash.
    ///
    /// Callers only request numbers inside the `BLOCKHASH` window, so a hash the database cannot
    /// provide is a database failure reported through the error, never a benign miss.
    fn get_block_hash(&mut self, number: &Word) -> DbResult<B256>;
}

/// Shared backing database implementation with a concrete error type.
///
/// The counterpart of [`Database`] whose reads take `&self`.
#[auto_impl(&, &mut, Box, Rc, Arc)]
pub trait DatabaseRef: NonStaticAny {
    /// Database error type.
    type Error: Error + Send + Sync + 'static;

    /// Whether a concrete database error is an internal failure rather than invalid input.
    fn is_fatal(error: &Self::Error) -> bool {
        let _ = error;
        true
    }

    /// Loads account information.
    fn get_account(&self, address: &Address) -> Result<Option<AccountInfo>, Self::Error>;

    /// Loads bytecode by code hash.
    fn get_code_by_hash(&self, code_hash: &B256) -> Result<Bytecode, Self::Error>;

    /// Loads a persistent storage slot.
    fn get_storage(&self, address: &Address, key: &Word) -> Result<Word, Self::Error>;

    /// Loads a historical block hash.
    ///
    /// Callers only request numbers inside the `BLOCKHASH` window, so a hash the database cannot
    /// provide is a database failure reported through the error, never a benign miss.
    fn get_block_hash(&self, number: &Word) -> Result<B256, Self::Error>;
}

/// Object-safe database adapter for typed shared database implementations.
#[derive(Clone, Debug)]
pub struct DbRef<T: DatabaseRef> {
    db: T,
}

impl<T: DatabaseRef + Default> Default for DbRef<T> {
    #[inline]
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: DatabaseRef> DbRef<T> {
    /// Creates a new database adapter.
    #[inline]
    pub const fn new(db: T) -> Self {
        Self { db }
    }

    /// Returns the wrapped database.
    #[inline]
    pub const fn inner(&self) -> &T {
        &self.db
    }

    /// Returns the wrapped database mutably.
    #[inline]
    pub const fn inner_mut(&mut self) -> &mut T {
        &mut self.db
    }

    /// Consumes the adapter and returns the wrapped database.
    #[inline]
    pub fn into_inner(self) -> T {
        self.db
    }
}

impl<T: DatabaseRef> DynDatabaseRef for DbRef<T> {
    #[inline]
    fn get_account(&self, address: &Address) -> DbResult<Option<AccountInfo>> {
        self.db.get_account(address).map_err(|err| {
            let fatal = T::is_fatal(&err);
            DatabaseError::new(err, fatal)
        })
    }

    #[inline]
    fn get_code_by_hash(&self, code_hash: &B256) -> DbResult<Bytecode> {
        self.db.get_code_by_hash(code_hash).map_err(|err| {
            let fatal = T::is_fatal(&err);
            DatabaseError::new(err, fatal)
        })
    }

    #[inline]
    fn get_storage(&self, address: &Address, key: &Word) -> DbResult<Word> {
        self.db.get_storage(address, key).map_err(|err| {
            let fatal = T::is_fatal(&err);
            DatabaseError::new(err, fatal)
        })
    }

    #[inline]
    fn get_block_hash(&self, number: &Word) -> DbResult<B256> {
        self.db.get_block_hash(number).map_err(|err| {
            let fatal = T::is_fatal(&err);
            DatabaseError::new(err, fatal)
        })
    }
}

impl<T: DatabaseRef> DynDatabase for DbRef<T> {
    #[inline]
    fn get_account(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        DynDatabaseRef::get_account(self, address)
    }

    #[inline]
    fn get_code_by_hash(&mut self, code_hash: &B256) -> DbResult<Bytecode> {
        DynDatabaseRef::get_code_by_hash(self, code_hash)
    }

    #[inline]
    fn get_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        DynDatabaseRef::get_storage(self, address, key)
    }

    #[inline]
    fn get_block_hash(&mut self, number: &Word) -> DbResult<B256> {
        DynDatabaseRef::get_block_hash(self, number)
    }
}

/// Shared backing database view used to initialize mutable [`super::State`].
///
/// The counterpart of [`DynDatabase`] whose reads take `&self`.
#[auto_impl(&, &mut, Box, Rc, Arc)]
pub trait DynDatabaseRef: NonStaticAny {
    /// Loads account information.
    fn get_account(&self, address: &Address) -> DbResult<Option<AccountInfo>>;

    /// Loads bytecode by code hash.
    fn get_code_by_hash(&self, code_hash: &B256) -> DbResult<Bytecode>;

    /// Loads a persistent storage slot.
    fn get_storage(&self, address: &Address, key: &Word) -> DbResult<Word>;

    /// Loads a historical block hash.
    ///
    /// Callers only request numbers inside the `BLOCKHASH` window, so a hash the database cannot
    /// provide is a database failure reported through the error, never a benign miss.
    fn get_block_hash(&self, number: &Word) -> DbResult<B256>;
}

impl<T: Database + DatabaseRef> DatabaseRef for Db<T> {
    type Error = <T as DatabaseRef>::Error;

    fn is_fatal(error: &Self::Error) -> bool {
        <T as DatabaseRef>::is_fatal(error)
    }

    #[inline]
    fn get_account(&self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.inner().get_account(address)
    }

    #[inline]
    fn get_code_by_hash(&self, code_hash: &B256) -> Result<Bytecode, Self::Error> {
        self.inner().get_code_by_hash(code_hash)
    }

    #[inline]
    fn get_storage(&self, address: &Address, key: &Word) -> Result<Word, Self::Error> {
        self.inner().get_storage(address, key)
    }

    #[inline]
    fn get_block_hash(&self, number: &Word) -> Result<B256, Self::Error> {
        self.inner().get_block_hash(number)
    }
}

/// Counts calls made through a [`DynDatabase`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DbStatsCounts {
    /// Number of account loads.
    pub get_account: u64,
    /// Number of bytecode loads by hash.
    pub get_code_by_hash: u64,
    /// Number of storage slot loads.
    pub get_storage: u64,
    /// Number of storage loads whose address matched the previous storage load.
    pub get_storage_same_address_repeats: u64,
    /// Longest run of storage loads for the same address.
    pub get_storage_same_address_longest_streak: u64,
    /// Number of block hash loads.
    pub get_block_hash: u64,
}

impl core::ops::AddAssign for DbStatsCounts {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        self.get_account += rhs.get_account;
        self.get_code_by_hash += rhs.get_code_by_hash;
        self.get_storage += rhs.get_storage;
        self.get_storage_same_address_repeats += rhs.get_storage_same_address_repeats;
        self.get_storage_same_address_longest_streak = self
            .get_storage_same_address_longest_streak
            .max(rhs.get_storage_same_address_longest_streak);
        self.get_block_hash += rhs.get_block_hash;
    }
}

/// Database wrapper that records method call counts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DbStats<D> {
    db: D,
    counts: DbStatsCounts,
    last_storage_address: Option<Address>,
    storage_address_streak: u64,
}

impl<D> DbStats<D> {
    /// Creates a stats wrapper around `db`.
    #[inline]
    pub const fn new(db: D) -> Self {
        Self {
            db,
            counts: DbStatsCounts {
                get_account: 0,
                get_code_by_hash: 0,
                get_storage: 0,
                get_storage_same_address_repeats: 0,
                get_storage_same_address_longest_streak: 0,
                get_block_hash: 0,
            },
            last_storage_address: None,
            storage_address_streak: 0,
        }
    }

    /// Returns the wrapped database.
    #[inline]
    pub const fn inner(&self) -> &D {
        &self.db
    }

    /// Returns the wrapped database mutably.
    #[inline]
    pub const fn inner_mut(&mut self) -> &mut D {
        &mut self.db
    }

    /// Consumes the wrapper and returns the wrapped database.
    #[inline]
    pub fn into_inner(self) -> D {
        self.db
    }

    /// Returns recorded method call counts.
    #[inline]
    pub const fn counts(&self) -> DbStatsCounts {
        self.counts
    }

    #[inline]
    fn record_storage_load(&mut self, address: &Address) {
        self.counts.get_storage += 1;
        if self.last_storage_address.as_ref() == Some(address) {
            self.counts.get_storage_same_address_repeats += 1;
            self.storage_address_streak += 1;
        } else {
            self.last_storage_address = Some(*address);
            self.storage_address_streak = 1;
        }
        self.counts.get_storage_same_address_longest_streak =
            self.counts.get_storage_same_address_longest_streak.max(self.storage_address_streak);
    }
}

impl<D: DynDatabase> DynDatabase for DbStats<D> {
    #[inline]
    fn get_account(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        self.counts.get_account += 1;
        self.db.get_account(address)
    }

    #[inline]
    fn get_code_by_hash(&mut self, code_hash: &B256) -> DbResult<Bytecode> {
        self.counts.get_code_by_hash += 1;
        self.db.get_code_by_hash(code_hash)
    }

    #[inline]
    fn get_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        self.record_storage_load(address);
        self.db.get_storage(address, key)
    }

    #[inline]
    fn get_block_hash(&mut self, number: &Word) -> DbResult<B256> {
        self.counts.get_block_hash += 1;
        self.db.get_block_hash(number)
    }
}

impl<'a> core::ops::Deref for dyn DynDatabase + 'a {
    type Target = dyn NonStaticAny + 'a;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self
    }
}

impl<'a> core::ops::DerefMut for dyn DynDatabase + 'a {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self
    }
}

impl<'a> core::ops::Deref for dyn DynDatabaseRef + 'a {
    type Target = dyn NonStaticAny + 'a;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self
    }
}

impl<'a> core::ops::DerefMut for dyn DynDatabaseRef + 'a {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self
    }
}

#[inline]
pub(crate) fn boxed_dyn_database<'a>(database: impl DynDatabase + 'a) -> Box<dyn DynDatabase + 'a> {
    Box::new(database)
}

/// Empty backing database.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EmptyDB(());

impl Database for EmptyDB {
    type Error = core::convert::Infallible;

    #[inline]
    fn get_account(&mut self, _address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        Ok(None)
    }

    #[inline]
    fn get_code_by_hash(&mut self, _code_hash: &B256) -> Result<Bytecode, Self::Error> {
        Ok(Bytecode::default())
    }

    #[inline]
    fn get_storage(&mut self, _address: &Address, _key: &Word) -> Result<Word, Self::Error> {
        Ok(Word::ZERO)
    }

    #[inline]
    fn get_block_hash(&mut self, number: &Word) -> Result<B256, Self::Error> {
        Ok(keccak256(number.to_string().as_bytes()))
    }
}

impl DynDatabase for EmptyDB {
    #[inline]
    fn get_account(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        Db::new(*self).get_account(address)
    }

    #[inline]
    fn get_code_by_hash(&mut self, code_hash: &B256) -> DbResult<Bytecode> {
        Db::new(*self).get_code_by_hash(code_hash)
    }

    #[inline]
    fn get_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        Db::new(*self).get_storage(address, key)
    }

    #[inline]
    fn get_block_hash(&mut self, number: &Word) -> DbResult<B256> {
        Db::new(*self).get_block_hash(number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::op;
    use alloy_primitives::Bytes;

    #[cfg(feature = "std")]
    use alloc::{sync::Arc, vec, vec::Vec};
    #[cfg(feature = "std")]
    use std::thread;

    struct BorrowingDb<'a> {
        value: &'a u8,
    }

    impl DynDatabase for BorrowingDb<'_> {
        fn get_account(&mut self, _address: &Address) -> DbResult<Option<AccountInfo>> {
            Ok(None)
        }

        fn get_code_by_hash(&mut self, _code_hash: &B256) -> DbResult<Bytecode> {
            Ok(Bytecode::default())
        }

        fn get_storage(&mut self, _address: &Address, _key: &Word) -> DbResult<Word> {
            Ok(Word::ZERO)
        }

        fn get_block_hash(&mut self, _number: &Word) -> DbResult<B256> {
            Ok(B256::repeat_byte(*self.value))
        }
    }

    #[test]
    fn borrowed_database_can_be_erased() {
        let value = 1;
        let mut erased = boxed_dyn_database(BorrowingDb { value: &value });

        assert_eq!(erased.get_block_hash(&Word::ZERO).unwrap(), B256::repeat_byte(value));
    }

    #[test]
    fn static_database_can_be_downcast() {
        let erased: Box<dyn DynDatabase + 'static> = boxed_dyn_database(EmptyDB::default());

        assert!(erased.downcast_ref::<EmptyDB>().is_some());
    }

    const REF_ACCOUNT: Address = Address::with_last_byte(0xaa);
    const REF_SLOT: Word = Word::from_limbs([7, 0, 0, 0]);

    /// Read-only fixture: one account with code, one slot, one block hash.
    struct RefDb {
        balance: Word,
        code: Bytecode,
        block_hash: B256,
    }

    impl Default for RefDb {
        fn default() -> Self {
            Self {
                balance: Word::from(5),
                code: Bytecode::new_legacy(Bytes::from_static(&[op::STOP])),
                block_hash: keccak256([0x02]),
            }
        }
    }

    impl DatabaseRef for RefDb {
        type Error = core::convert::Infallible;

        fn get_account(&self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
            Ok((*address == REF_ACCOUNT).then(|| {
                AccountInfo::default().with_balance(self.balance).with_code(self.code.clone())
            }))
        }

        fn get_code_by_hash(&self, code_hash: &B256) -> Result<Bytecode, Self::Error> {
            Ok(if *code_hash == self.code.hash_slow() {
                self.code.clone()
            } else {
                Bytecode::default()
            })
        }

        fn get_storage(&self, address: &Address, key: &Word) -> Result<Word, Self::Error> {
            Ok(if *address == REF_ACCOUNT && *key == REF_SLOT { self.balance } else { Word::ZERO })
        }

        fn get_block_hash(&self, number: &Word) -> Result<B256, Self::Error> {
            Ok(if *number == Word::from(9) { self.block_hash } else { B256::ZERO })
        }
    }

    #[test]
    fn db_ref_reads_through_a_borrowed_view() {
        let db = RefDb::default();
        let mut cache = CacheDB::new(DbRef::new(&db));

        let account =
            DynDatabase::get_account(&mut cache, &REF_ACCOUNT).unwrap().expect("account exists");
        assert_eq!(account.balance, Word::from(5));
        assert_eq!(account.code_hash, db.code.hash_slow());
        assert_eq!(
            DynDatabase::get_storage(&mut cache, &REF_ACCOUNT, &REF_SLOT).unwrap(),
            Word::from(5)
        );
        cache.cache.contracts.clear();
        assert_eq!(
            DynDatabase::get_code_by_hash(&mut cache, &db.code.hash_slow()).unwrap().len(),
            1
        );
        assert_eq!(DynDatabase::get_block_hash(&mut cache, &Word::from(9)).unwrap(), db.block_hash);

        // The borrow is released with the adapter, so the same database serves later reads.
        assert_eq!(db.balance, Word::from(5));
    }

    #[test]
    #[cfg(feature = "std")]
    fn db_ref_shares_one_database_across_workers() {
        let db = Arc::new(RefDb::default());
        let balances = thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    let db = Arc::clone(&db);
                    scope.spawn(move || {
                        let mut cache = CacheDB::new(DbRef::new(db));
                        DynDatabase::get_account(&mut cache, &REF_ACCOUNT)
                            .unwrap()
                            .expect("account exists")
                            .balance
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("worker panicked"))
                .collect::<Vec<_>>()
        });

        assert_eq!(balances, vec![Word::from(5); 4]);
        // Every worker dropped its handle when it finished.
        assert_eq!(Arc::strong_count(&db), 1);
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
    #[error("test database error (fatal: {0})")]
    struct RefDbError(bool);

    struct FailingRefDb(bool);

    impl DatabaseRef for FailingRefDb {
        type Error = RefDbError;

        fn is_fatal(error: &Self::Error) -> bool {
            error.0
        }

        fn get_account(&self, _: &Address) -> Result<Option<AccountInfo>, Self::Error> {
            Err(RefDbError(self.0))
        }

        fn get_code_by_hash(&self, _: &B256) -> Result<Bytecode, Self::Error> {
            Err(RefDbError(self.0))
        }

        fn get_storage(&self, _: &Address, _: &Word) -> Result<Word, Self::Error> {
            Err(RefDbError(self.0))
        }

        fn get_block_hash(&self, _: &Word) -> Result<B256, Self::Error> {
            Err(RefDbError(self.0))
        }
    }

    #[test]
    fn db_ref_preserves_error_type_and_classification() {
        for fatal in [false, true] {
            let db = FailingRefDb(fatal);
            assert_eq!(db.get_account(&REF_ACCOUNT).unwrap_err(), RefDbError(fatal));

            let mut adapter = DbRef::new(&db);
            let shared: &dyn DynDatabaseRef = &adapter;
            let errors = [
                shared.get_account(&REF_ACCOUNT).unwrap_err(),
                shared.get_code_by_hash(&B256::ZERO).unwrap_err(),
                shared.get_storage(&REF_ACCOUNT, &REF_SLOT).unwrap_err(),
                shared.get_block_hash(&Word::ZERO).unwrap_err(),
            ];
            let mutable: &mut dyn DynDatabase = &mut adapter;
            let mutable_errors = [
                mutable.get_account(&REF_ACCOUNT).unwrap_err(),
                mutable.get_code_by_hash(&B256::ZERO).unwrap_err(),
                mutable.get_storage(&REF_ACCOUNT, &REF_SLOT).unwrap_err(),
                mutable.get_block_hash(&Word::ZERO).unwrap_err(),
            ];
            for error in errors.into_iter().chain(mutable_errors) {
                assert_eq!(error.is_fatal(), fatal);
                assert_eq!(error.downcast_ref::<RefDbError>(), Some(&RefDbError(fatal)));
            }
        }
    }
}
