//! The per-anchor storage memo layer (ADR-090).
//!
//! [`StorageMemo`](crate::sim::StorageMemo) is the engine's per-anchor read
//! memo: values the RPC fallback itself fetched at ONE block, retired the
//! moment the read anchor moves. The simulation fan-out consults it inside
//! [`BotStateDb`](super::bot_state_db) — the layer that owns the engine's
//! anchor.
//!
//! The black-box V4 leg re-quote (ADR-090 H3) does not run over `BotStateDb`:
//! it prices against its own anchor-pinned scratch so it can never touch
//! `BotState` while the clamp holds the core read guard. Its reads therefore
//! reached the RPC leaf on every quote, one serial `eth_getStorageAt` per slot,
//! even for a leg quoted moments earlier at the SAME anchor — measured at ~17
//! cold reads (~3.3 ms p50) per hooked leg, where the anchor memo makes the
//! same quote ~70 µs with zero reads (ADR-090).
//!
//! This layer closes that gap without giving the re-quote a `BotStateDb`: it
//! consults an anchor memo it is handed, records every forwarded answer back
//! into it, and forwards everything else untouched.
//!
//! # The anchoring contract
//!
//! The memo answers with values read at its own anchor, so the caller must hand
//! the layer the memo minted for the block its reads pin. A memo from another
//! block is a wrong price, never a stale-but-harmless one — that is the whole
//! reason [`StorageMemoCache`](crate::sim::StorageMemoCache) retires a memo when
//! the anchor moves, and why this layer never mints one itself.
//!
//! # What it deliberately does not do
//!
//! No account or bytecode caching: [`WarmCodeCache`](super::warm_code_cache)
//! already owns the immutable rows cross-block, and a second cache of them here
//! would only add a lock. Block hashes are per-chain facts, never memoized.

use std::sync::Arc;

use alloy::primitives::{Address, B256};

use ::evm2::bytecode::Bytecode;
use ::evm2::evm::{AccountInfo, Database};
use ::evm2::interpreter::Word;

use crate::sim::StorageMemo;

/// A `Database` that answers storage from an anchor memo before the cold
/// fallback (see the module docs).
pub struct MemoizedStorage<Db> {
    /// The cold-miss fallback (the RPC adapter, optionally a counting layer).
    inner: Db,
    /// The memo minted for the block every read is pinned to.
    memo: Arc<StorageMemo>,
}

impl<Db> MemoizedStorage<Db> {
    /// Wrap `inner` with `memo` — the memo of the anchor `inner`'s reads pin.
    #[must_use]
    pub fn new(inner: Db, memo: Arc<StorageMemo>) -> Self {
        Self { inner, memo }
    }
}

impl<Db: Database> Database for MemoizedStorage<Db> {
    type Error = Db::Error;

    fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.inner.get_account(address)
    }

    fn get_code_by_hash(&mut self, code_hash: &B256) -> Result<Bytecode, Self::Error> {
        self.inner.get_code_by_hash(code_hash)
    }

    /// The memo first: a hit is this anchor's own earlier fallback answer (the
    /// fallback stays the authority, and the memo never holds a projected
    /// engine word). A miss forwards and records.
    fn get_storage(&mut self, address: &Address, key: &Word) -> Result<Word, Self::Error> {
        if let Some(value) = self.memo.get(*address, *key) {
            return Ok(value);
        }
        let value = self.inner.get_storage(address, key)?;
        self.memo.put(*address, *key, value);
        Ok(value)
    }

    fn get_block_hash(&mut self, number: &Word) -> Result<B256, Self::Error> {
        self.inner.get_block_hash(number)
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {

    use std::convert::Infallible;
    use std::sync::Arc;

    use alloy::primitives::{Address, Bytes, B256, U256};

    use ::evm2::bytecode::Bytecode;
    use ::evm2::evm::{AccountInfo, Database};
    use ::evm2::interpreter::Word;

    use super::MemoizedStorage;
    use crate::sim::StorageMemo;

    const POOL: Address = Address::new([0xaa; 20]);

    /// A counting fallback serving scripted storage words.
    #[derive(Default)]
    struct CountingDb {
        reads: u64,
    }

    impl Database for CountingDb {
        type Error = Infallible;

        fn get_account(&mut self, _address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
            Ok(Some(AccountInfo::default().with_code(
                Bytecode::new_legacy(Bytes::from_static(&[0x00])),
            )))
        }

        fn get_code_by_hash(&mut self, _code_hash: &B256) -> Result<Bytecode, Self::Error> {
            Ok(Bytecode::default())
        }

        fn get_storage(&mut self, _address: &Address, key: &Word) -> Result<Word, Self::Error> {
            self.reads += 1;
            Ok(*key + U256::ONE)
        }

        fn get_block_hash(&mut self, _number: &Word) -> Result<B256, Self::Error> {
            Ok(B256::ZERO)
        }
    }

    /// The first read forwards and records; every later read of the same word
    /// is served from the memo, so a re-quote at one anchor pays its slots once.
    #[test]
    fn a_repeated_read_is_served_from_the_anchor_memo() {
        let memo = Arc::new(StorageMemo::new());
        let mut db = MemoizedStorage::new(CountingDb::default(), Arc::clone(&memo));
        let slot = Word::from(7u64);

        assert_eq!(
            db.get_storage(&POOL, &slot).expect("the first read"),
            U256::from(8u64)
        );
        assert_eq!(db.inner.reads, 1, "a miss forwards to the fallback");
        assert_eq!(memo.get(POOL, slot), Some(U256::from(8u64)));

        assert_eq!(
            db.get_storage(&POOL, &slot).expect("the second read"),
            U256::from(8u64)
        );
        assert_eq!(
            db.inner.reads, 1,
            "the second read never reaches the fallback"
        );
    }

    /// A memo that already holds a word (the prefetch's seeded answer) serves it
    /// without any fallback read, and distinct slots are memoized separately.
    #[test]
    fn a_seeded_word_short_circuits_and_slots_are_independent() {
        let memo = Arc::new(StorageMemo::new());
        memo.seed(POOL, Word::from(1u64), U256::from(100u64));
        let mut db = MemoizedStorage::new(CountingDb::default(), Arc::clone(&memo));

        assert_eq!(
            db.get_storage(&POOL, &Word::from(1u64)).expect("read"),
            U256::from(100u64)
        );
        assert_eq!(
            db.inner.reads, 0,
            "the seeded word never reaches the fallback"
        );
        assert_eq!(
            db.get_storage(&POOL, &Word::from(2u64)).expect("read"),
            U256::from(3u64)
        );
        assert_eq!(db.inner.reads, 1);
    }

    /// Accounts and code forward untouched — the warm code cache owns them
    /// cross-block, and this layer caches nothing but storage.
    #[test]
    fn accounts_and_code_forward_untouched() {
        let memo = Arc::new(StorageMemo::new());
        let mut db = MemoizedStorage::new(CountingDb::default(), memo);
        let info = db.get_account(&POOL).expect("read").expect("present");
        assert!(info.code.is_some());
        let _ = db.get_code_by_hash(&B256::ZERO).expect("read");
    }
}
