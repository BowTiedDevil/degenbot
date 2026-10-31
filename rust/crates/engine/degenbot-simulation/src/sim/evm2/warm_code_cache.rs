//! The evm2 port of the engine's cross-block warm cache
//! (`sim::evm::warm_code_cache`).
//!
//! Same staleness line and TTL semantics as the revm-side original: only the
//! immutable rows are cached — contract `AccountInfo` (existence + code hash,
//! EOA rows never: their nonce/balance mutate every block) and `Bytecode`
//! keyed by code hash — while storage and block hashes forward untouched.
//!
//! One structural difference is forced by evm2's DB seam: the original
//! implements revm's shared `DatabaseRef` (revm's `CacheDB` wraps a shared
//! ref); evm2's `CacheDB` composes over the mutable `Database` trait
//! (objectified as `DynDatabase`), so this port implements `Database` — the
//! `RwLock` already provided the interior mutability.
//!
//! The lab counters (`sim::sim_metrics`) live on this seam for the same reason
//! they lived on the original's: every call reaching the fallback is BY
//! DEFINITION a `CacheDB` miss, so the forward is the cold fetch. A fresh
//! entry served here is the cross-block reuse signal, and the latency split
//! separates an in-process serve from an RPC-reached one.

use alloy::primitives::map::{AddressHashMap, B256HashMap};
use alloy::primitives::{Address, B256, KECCAK256_EMPTY};
use parking_lot::RwLock;
use std::sync::Arc;

use ::evm2::bytecode::Bytecode;
use ::evm2::evm::{AccountInfo, Database};
use ::evm2::interpreter::Word;

use crate::sim::sim_metrics;

/// The default per-entry TTL in blocks — the same safety-net value as the
/// revm-side original (≈ 1 day of mainnet blocks). A safety net, not the
/// primary correctness mechanism: in the bot's domain every cached contract
/// is immutable.
pub const WARM_CODE_CACHE_TTL_BLOCKS: u64 = 10_000;

/// The cross-block persistent inner state — bytecode + account-existence
/// maps, each entry tagged with its load block for per-entry TTL expiry.
/// Same ownership shape as the original: held behind `Arc<RwLock<...>>` by
/// the engine owner, cloned into each per-block wrapper value.
pub struct WarmCodeCacheInner {
    /// `(loaded_block, AccountInfo)` per address. `None` (no-account) IS
    /// cached (existence-negative) — saves the cold read on the
    /// balanceOf-to-fresh-address case.
    accounts: AddressHashMap<(u64, Option<AccountInfo>)>,
    /// `(loaded_block, Bytecode)` per `code_hash` (immutable by hash).
    bytecode: B256HashMap<(u64, Bytecode)>,
    /// An entry loaded at `loaded_block` is a miss when
    /// `block - loaded_block > ttl_blocks` (strictly greater — an entry
    /// loaded THIS block is always fresh).
    ttl_blocks: u64,
}

impl WarmCodeCacheInner {
    /// An empty inner state with the given per-entry TTL.
    #[must_use]
    fn new(ttl_blocks: u64) -> Self {
        Self {
            accounts: AddressHashMap::default(),
            bytecode: B256HashMap::default(),
            ttl_blocks,
        }
    }

    /// The engine-owner construction shape: a shared empty inner state with
    /// the default TTL, cloned into each per-block wrapper.
    #[must_use]
    pub fn shared_default() -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(Self::new(WARM_CODE_CACHE_TTL_BLOCKS)))
    }

    /// A shared inner state with a custom TTL (TTL-boundary tests).
    #[must_use]
    pub fn shared_with_ttl(ttl_blocks: u64) -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(Self::new(ttl_blocks)))
    }

    /// Is an entry loaded at `loaded_block` still fresh at `block`?
    fn is_fresh(&self, loaded_block: u64, block: u64) -> bool {
        block.saturating_sub(loaded_block) <= self.ttl_blocks
    }
}

/// A cross-block warm `Database` wrapper — caches accounts + bytecode (the
/// immutable rows) with a per-entry TTL, forwards storage + block hashes
/// (the mutable-per-block rows, never cached).
pub struct WarmCodeCache<Db>
where
    Db: Database,
{
    /// The cross-block persistent maps, shared via the engine owner's arc.
    cache: Arc<RwLock<WarmCodeCacheInner>>,
    /// The block this cache view is for (drives per-entry TTL expiry).
    block: u64,
    /// The per-block cold-miss fallback.
    db: Db,
}

impl<Db> WarmCodeCache<Db>
where
    Db: Database,
{
    /// Build a per-block view over a shared inner cache + a per-block
    /// fallback (`BotStateDb` over the RPC adapter in the target stack).
    #[must_use]
    pub fn with_owner(cache: Arc<RwLock<WarmCodeCacheInner>>, block: u64, db: Db) -> Self {
        Self { cache, block, db }
    }

    /// The number of cold reads the fallback has served — the parity gate's
    /// cache-effectiveness probe.
    #[cfg(test)]
    pub fn inner(&self) -> &Db {
        &self.db
    }
}

impl<Db> Database for WarmCodeCache<Db>
where
    Db: Database,
{
    type Error = Db::Error;

    /// Serve from the warm cache if the entry is fresh; else forward + cache
    /// the result (existence-negative `None` IS cached). EOA rows are NEVER
    /// cached — an EOA's nonce + balance mutate every block and the warm
    /// layer is cross-block.
    fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        {
            let guard = self.cache.read();
            if let Some((loaded_block, info)) = guard.accounts.get(address) {
                if guard.is_fresh(*loaded_block, self.block) {
                    // Lab: the cross-block warm cache served a `CacheDB` miss
                    // (reuse signal — no underlying call happened).
                    sim_metrics::record_basic_warm_hit();
                    return Ok(info.clone());
                }
            }
        }
        let (info, dur) = sim_metrics::timed(|| self.db.get_account(address));
        sim_metrics::record_basic(dur);
        let info = info?;
        let is_contract = info
            .as_ref()
            .is_some_and(|i| i.code_hash != KECCAK256_EMPTY);
        if is_contract {
            let mut guard = self.cache.write();
            guard.accounts.insert(*address, (self.block, info.clone()));
        }
        Ok(info)
    }

    /// Serve bytecode from the warm cache if fresh; else forward + cache.
    /// Defensive like the original: a fallback whose `get_account` eager-loads
    /// code (the RPC adapter's shape) never reaches this.
    fn get_code_by_hash(&mut self, code_hash: &B256) -> Result<Bytecode, Self::Error> {
        {
            let guard = self.cache.read();
            if let Some((loaded_block, code)) = guard.bytecode.get(code_hash) {
                if guard.is_fresh(*loaded_block, self.block) {
                    return Ok(code.clone());
                }
            }
        }
        let (code, dur) = sim_metrics::timed(|| self.db.get_code_by_hash(code_hash));
        sim_metrics::record_code(dur);
        let code = code?;
        let mut guard = self.cache.write();
        guard
            .bytecode
            .insert(*code_hash, (self.block, code.clone()));
        Ok(code)
    }

    /// Forward — storage is the mutable-per-block row, NEVER cached.
    fn get_storage(&mut self, address: &Address, key: &Word) -> Result<Word, Self::Error> {
        // Lab: storage ALWAYS forwards — the latency split sizes the
        // in-process-served (memo/anchor) vs RPC-reached shares.
        let (value, dur) = sim_metrics::timed(|| self.db.get_storage(address, key));
        sim_metrics::record_storage(dur);
        value
    }

    /// Forward — block hashes are per-chain, NEVER cached.
    fn get_block_hash(&mut self, number: &Word) -> Result<B256, Self::Error> {
        sim_metrics::record_block_hash();
        self.db.get_block_hash(number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{Bytes, U256};
    use std::cell::Cell;
    use std::convert::Infallible;

    const CONTRACT: Address = Address::new([0xaa; 20]);
    const EOA: Address = Address::new([0xee; 20]);

    fn contract_info() -> AccountInfo {
        AccountInfo::default()
            .with_balance(U256::from(1u64))
            .with_code(Bytecode::new_legacy(Bytes::from_static(&[0x60, 0x00])))
    }

    /// A counting mock fallback: scripted account answers, zero storage.
    struct CountingDb {
        account: Option<AccountInfo>,
        account_calls: Cell<u64>,
        storage_calls: Cell<u64>,
    }

    impl CountingDb {
        fn new(account: Option<AccountInfo>) -> Self {
            Self {
                account,
                account_calls: Cell::new(0),
                storage_calls: Cell::new(0),
            }
        }
    }

    impl Database for CountingDb {
        type Error = Infallible;
        fn get_account(&mut self, _address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
            self.account_calls.set(self.account_calls.get() + 1);
            Ok(self.account.clone())
        }
        fn get_code_by_hash(&mut self, _code_hash: &B256) -> Result<Bytecode, Self::Error> {
            Ok(Bytecode::default())
        }
        fn get_storage(&mut self, _address: &Address, _key: &Word) -> Result<Word, Self::Error> {
            self.storage_calls.set(self.storage_calls.get() + 1);
            Ok(Word::ZERO)
        }
        fn get_block_hash(&mut self, _number: &Word) -> Result<B256, Self::Error> {
            Ok(B256::ZERO)
        }
    }

    #[test]
    fn contract_account_is_cached_across_wrappers() {
        let inner = WarmCodeCacheInner::shared_default();
        let mut first =
            WarmCodeCache::with_owner(inner.clone(), 100, CountingDb::new(Some(contract_info())));
        let info = first.get_account(&CONTRACT).unwrap();
        assert!(info.is_some());
        assert_eq!(first.inner().account_calls.get(), 1, "cold miss forwarded");

        let mut second =
            WarmCodeCache::with_owner(inner, 101, CountingDb::new(Some(contract_info())));
        let info = second.get_account(&CONTRACT).unwrap();
        assert!(info.is_some());
        assert_eq!(
            second.inner().account_calls.get(),
            0,
            "warm hit served the next block"
        );
    }

    #[test]
    fn eoa_account_is_never_cached() {
        let inner = WarmCodeCacheInner::shared_default();
        let mut first = WarmCodeCache::with_owner(inner.clone(), 100, CountingDb::new(None));
        assert!(first.get_account(&EOA).unwrap().is_none());

        let mut second = WarmCodeCache::with_owner(inner, 100, CountingDb::new(None));
        assert!(second.get_account(&EOA).unwrap().is_none());
        assert_eq!(
            second.inner().account_calls.get(),
            1,
            "EOA rows always forward"
        );
    }

    #[test]
    fn storage_always_forwards() {
        let inner = WarmCodeCacheInner::shared_default();
        let mut cache = WarmCodeCache::with_owner(inner, 100, CountingDb::new(None));
        for _ in 0..3 {
            cache.get_storage(&CONTRACT, &Word::from(8u64)).unwrap();
        }
        assert_eq!(cache.inner().storage_calls.get(), 3);
    }

    #[test]
    fn stale_entry_refreshes_past_the_ttl() {
        let inner = WarmCodeCacheInner::shared_with_ttl(10);
        let mut first =
            WarmCodeCache::with_owner(inner.clone(), 100, CountingDb::new(Some(contract_info())));
        first.get_account(&CONTRACT).unwrap();

        let mut fresh =
            WarmCodeCache::with_owner(inner.clone(), 110, CountingDb::new(Some(contract_info())));
        fresh.get_account(&CONTRACT).unwrap();
        assert_eq!(
            fresh.inner().account_calls.get(),
            0,
            "at the TTL boundary the entry is fresh"
        );

        let mut stale =
            WarmCodeCache::with_owner(inner, 111, CountingDb::new(Some(contract_info())));
        stale.get_account(&CONTRACT).unwrap();
        assert_eq!(
            stale.inner().account_calls.get(),
            1,
            "past the TTL the entry re-fetches"
        );
    }
}
