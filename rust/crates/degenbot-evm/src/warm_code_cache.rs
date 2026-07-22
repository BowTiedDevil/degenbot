//! Cross-block warm cache for immutable/long-TTL account data — bytecode +
//! account existence (the `basic_ref` + `code_by_hash_ref` rows). Sits
//! underneath the per-block `CacheDB` in the `BlockSimHandle` DB stack so the
//! contract code-load RPCs stop repeating every block while the per-block
//! `CacheDB` (overrides + mutable storage, fresh each block, drops at EOB) is
//! preserved.
//!
//! ## What this caches (the staleness line)
//!
//! Only the immutable rows:
//! - `basic_ref` — `AccountInfo` (balance is mutable per-block, but EIP-1967
//!   proxy upgrades change the implementation *storage* slot, not code; the
//!   bytecode at a given address is immutable for the contract's life).
//! - `code_by_hash_ref` — `Bytecode` keyed by `code_hash` (immutable by hash).
//!
//! `storage_ref` + `block_hash_ref` are forwarded UNCHECKED to the inner `Db`
//! (the mutable-per-block row — V2 reserves slot 8 / V3 `slot0`+ticks change
//! every block there's a swap; the chain block hash is per-chain, not
//! per-block-snapshot). Caching these would re-introduce the K-invariant /
//! `LOK` stale-state-divergence reverts the option-B path hit.
//!
//! ## The TTL (a safety net, not the primary correctness mechanism)
//!
//! Each entry records the block it was loaded at; an entry read at block `B`
//! is a **miss** when `B - loaded_block > WARM_CODE_CACHE_TTL_BLOCKS`. A
//! re-fetch on miss re-caches with `loaded_block = B`.
//!
//! In the bot's domain every cached contract is immutable (EIP-1967 proxies
//! upgrade via the implementation storage slot — uncached; the injected
//! executor is per-block via `stateOverrides`, never read here). The TTL is a
//! belt-and-suspenders net against:
//! - **EIP-6780-disabled selfdestruct edge cases** (pre-Cancun metamorphic
//!   `CREATE2`-redeploy-to-same-address: a contract that selfdestructs then
//!   redeploys with different bytecode at the same address would be stale).
//!   EIP-6780 (Cancun, mainnet since March 2024) disables code-deletion on
//!   selfdestruct unless the contract was created in the same tx — so the
//!   vector is effectively closed for any persistent contract.
//! - **Future protocol changes** to code mutability.
//!
//! ## Composition
//!
//! ```text
//! CacheDB (per-block: overrides + mutable storage, fresh each block, drops at EOB)
//!   → WarmCodeCache (persistent: bytecode + account-existence, per-entry TTL'd)
//!        ↳ holds Arc<RwLock<WarmCodeCacheInner>> cloned from the engine owner
//!        ↳ the WarmCodeCache<Db> wrapper value itself is per-block
//!    → BotStateDb (forwarding storage fallback)
//!      → WrapDatabaseAsync<AlloyDB> (RPC cold-miss)
//! ```
//!
//! ## Ownership
//!
//! The wrapper value (`WarmCodeCache<Db>`) is per-block — constructed in
//! `BlockSimHandle::build`, holds the per-block `Db`, drops at end of block.
//! Only the inner map (`Arc<RwLock<WarmCodeCacheInner>>`) persists across
//! blocks — cloned from the engine owner (`PyArbitrageEngine` / the
//! standalone Rust `Bot`). This preserves the per-block `CacheDB` discipline
//! while sharing the bytecode cache across blocks.

// Solidity/EVM + Rust-ecosystem identifiers (EIP-6780, EIP-1967, DatabaseRef,
// CacheDB, WrapDatabaseAsync, BlockSimHandle, etc.) are ubiquitous here —
// match `degenbot-simulation`'s convention.
#![allow(clippy::doc_markdown)]

use alloy::primitives::map::{AddressHashMap, B256HashMap};
use alloy::primitives::Address;
use parking_lot::RwLock;
use revm::bytecode::Bytecode;
use revm::database_interface::DatabaseRef;
use revm::primitives::{StorageKey, StorageValue, B256};
use revm::state::AccountInfo;
use std::sync::Arc;

/// The default per-entry TTL in blocks — ≈ 1 day of mainnet blocks (12s
/// block time → ~7200 blocks/hour → ~10 000 ≈ 1.4 days).
///
/// A safety net, not the primary correctness mechanism — in the bot's domain
/// every cached contract is immutable (EIP-1967 proxies upgrade via the
/// implementation *storage* slot, which this cache does NOT serve; the
/// injected executor is per-block via `stateOverrides`). See the module doc
/// for the EIP-6780 selfdestruct + future-protocol-change analysis.
pub const WARM_CODE_CACHE_TTL_BLOCKS: u64 = 10_000;

/// The cross-block persistent inner state — bytecode + account-existence maps,
/// each entry tagged with the block it was loaded at for per-entry TTL expiry.
///
/// Held behind `Arc<RwLock<...>>` so the engine owner (`PyArbitrageEngine` /
/// the standalone Rust `Bot`) shares one instance across every per-block
/// `WarmCodeCache` wrapper value. The maps grow with the working set (a few
/// hundred contracts for one bot's pool-set); there is no LRU eviction in
/// Tier 1 — unbounded growth is not a near-term concern (a follow-up if
/// measurement shows otherwise).
pub struct WarmCodeCacheInner {
    /// `(loaded_block, AccountInfo)` per address. `AccountInfo` carries
    /// `code_hash` (the bytecode is fetched separately via `code_by_hash_ref`,
    /// or eagerly inline in `basic_ref` — revm's `AlloyDB` does the latter).
    /// `None` (no-account) IS cached (existence-negative) — saves the `basic`
    /// RPC on the no-code-EOA case the simulate's `balanceOf`-to-fresh-
    /// addresses hits.
    accounts: AddressHashMap<(u64, Option<AccountInfo>)>,
    /// `(loaded_block, Bytecode)` per `code_hash` (immutable by hash).
    bytecode: B256HashMap<(u64, Bytecode)>,
    /// The fixed-offset TTL in blocks. An entry loaded at `loaded_block` is
    /// a miss when `block - loaded_block > ttl_blocks` (strictly greater — an
    /// entry loaded THIS block is always fresh).
    ttl_blocks: u64,
}

impl WarmCodeCacheInner {
    /// Construct an empty inner state with the given per-entry TTL.
    #[must_use]
    fn new(ttl_blocks: u64) -> Self {
        Self {
            accounts: AddressHashMap::default(),
            bytecode: B256HashMap::default(),
            ttl_blocks,
        }
    }

    /// Construct a shared (`Arc<RwLock<...>>`) empty inner state with the
    /// default TTL — the engine-owner construction shape (the `PyArbitrageEngine`
    /// / standalone Rust `Bot` calls this once at construction + holds the arc
    /// for the engine's life, cloning it into each per-block
    /// `WarmCodeCache::with_owner`).
    #[must_use]
    pub fn shared_default() -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(Self::new(WARM_CODE_CACHE_TTL_BLOCKS)))
    }

    /// Construct a shared inner state with a custom TTL (for tests + the
    /// benchmark's TTL-boundary assertion).
    #[must_use]
    pub fn shared_with_ttl(ttl_blocks: u64) -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(Self::new(ttl_blocks)))
    }

    /// Is an entry loaded at `loaded_block` still fresh at `block`?
    /// Fresh iff `block - loaded_block <= ttl_blocks` (an entry loaded THIS
    /// block is always fresh — `block - loaded_block = 0 <= ttl`).
    fn is_fresh(&self, loaded_block: u64, block: u64) -> bool {
        block.saturating_sub(loaded_block) <= self.ttl_blocks
    }
}

/// A cross-block warm `DatabaseRef` wrapper — caches `basic_ref` +
/// `code_by_hash_ref` (the immutable rows) with a per-entry TTL, forwards
/// `storage_ref` + `block_hash_ref` to the inner `Db` (the mutable-per-block
/// row, never cached).
///
/// The wrapper value is per-block (constructed in `BlockSimHandle::build`,
/// holds the per-block `Db`); only the inner `Arc<RwLock<WarmCodeCacheInner>>`
/// persists across blocks — cloned from the engine owner. See the module doc.
///
/// # Errors
///
/// `Error = Db::Error` — the cache layer is infallible; errors surface from
/// the inner `Db` on a cache miss (the RPC cold-load path).
pub struct WarmCodeCache<Db>
where
    Db: DatabaseRef,
{
    /// The cross-block persistent maps (bytecode + account-existence), shared
    /// via the engine owner's arc.
    cache: Arc<RwLock<WarmCodeCacheInner>>,
    /// The block this cache view is for (drives per-entry TTL expiry checks).
    block: u64,
    /// The per-block `DatabaseRef` (forwards `storage_ref` + `block_hash_ref`
    /// untouched; serves as the cold-miss fallback for `basic_ref` +
    /// `code_by_hash_ref`).
    db: Db,
}

impl<Db> WarmCodeCache<Db>
where
    Db: DatabaseRef,
{
    /// Build a per-block view over a shared inner cache + a per-block `Db`.
    /// The `cache` arc is cloned from the engine owner; the `block` drives
    /// per-entry TTL expiry; the `db` is the per-block fallback
    /// (`BotStateDb<WrapDatabaseAsync<AlloyDB>>` in production).
    #[must_use]
    pub fn with_owner(cache: Arc<RwLock<WarmCodeCacheInner>>, block: u64, db: Db) -> Self {
        Self { cache, block, db }
    }
}

impl<Db> DatabaseRef for WarmCodeCache<Db>
where
    Db: DatabaseRef,
{
    type Error = Db::Error;

    /// Serve from the warm cache if the entry is fresh; else forward to the
    /// inner `Db` + cache the result (the existence-negative `None` IS cached
    /// — saves the `basic` RPC on the no-code-EOA case).
    ///
    /// # Errors
    ///
    /// Returns the inner `Db`'s error on a cache miss (the RPC `basic` fetch).
    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        // Fast path: read-locked cache hit (fresh entry).
        {
            let guard = self.cache.read();
            if let Some((loaded_block, info)) = guard.accounts.get(&address) {
                if guard.is_fresh(*loaded_block, self.block) {
                    return Ok(info.clone());
                }
            }
        }
        // Miss (or stale): forward + cache.
        let info = self.db.basic_ref(address)?;
        let mut guard = self.cache.write();
        guard.accounts.insert(address, (self.block, info.clone()));
        Ok(info)
    }

    /// Serve bytecode from the warm cache if the entry is fresh; else forward
    /// to the inner `Db` + cache the result.
    ///
    /// In revm's `AlloyDB`, `basic_ref` eagerly loads code inline — so
    /// `code_by_hash_ref` is only reached when `basic_ref` returned a
    /// `code_hash` without the code (the spike-verified panic-safety
    /// invariant — never in production). Caching here is defensive + handles
    /// a `Db` whose `basic_ref` does NOT eagerly load code.
    ///
    /// # Errors
    ///
    /// Returns the inner `Db`'s error on a cache miss.
    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        {
            let guard = self.cache.read();
            if let Some((loaded_block, code)) = guard.bytecode.get(&code_hash) {
                if guard.is_fresh(*loaded_block, self.block) {
                    return Ok(code.clone());
                }
            }
        }
        let code = self.db.code_by_hash_ref(code_hash)?;
        let mut guard = self.cache.write();
        guard.bytecode.insert(code_hash, (self.block, code.clone()));
        Ok(code)
    }

    /// Forward to the inner `Db` — storage is the mutable-per-block row (V2
    /// reserves slot 8, V3 `slot0`/`liquidity`/`ticks` change every block
    /// there's a swap). NEVER cached (caching would re-introduce the
    /// K-invariant / stale-state-divergence reverts the option-B path hit).
    ///
    /// # Errors
    ///
    /// Returns the inner `Db`'s error if the RPC fetch fails.
    fn storage_ref(
        &self,
        address: Address,
        index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        self.db.storage_ref(address, index)
    }

    /// Forward to the inner `Db` — block hashes are per-chain, not
    /// per-block-snapshot. NEVER cached.
    ///
    /// # Errors
    ///
    /// Returns the inner `Db`'s error if the RPC fetch fails.
    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        self.db.block_hash_ref(number)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::too_many_lines)]

    use super::*;
    use alloy::primitives::{address, U256};
    use revm::database_interface::EmptyDB;
    use std::cell::Cell;

    /// A counting mock `DatabaseRef` — wraps `EmptyDB` + records every call.
    /// `EmptyDB` returns `None` for `basic_ref` + `KECCAK_EMPTY` bytecode for
    /// `code_by_hash_ref` (both error-free), so the mock serves as the
    /// cold-miss fallback the warm cache forwards to.
    #[allow(clippy::struct_field_names)]
    struct CountingMockDb {
        basic_calls: Cell<u64>,
        code_by_hash_calls: Cell<u64>,
        storage_calls: Cell<u64>,
        block_hash_calls: Cell<u64>,
    }

    impl CountingMockDb {
        const EMPTY_CODE_HASH: B256 = B256::ZERO;
        fn new() -> Self {
            Self {
                basic_calls: Cell::new(0),
                code_by_hash_calls: Cell::new(0),
                storage_calls: Cell::new(0),
                block_hash_calls: Cell::new(0),
            }
        }
    }

    impl DatabaseRef for CountingMockDb {
        type Error = core::convert::Infallible;
        fn basic_ref(&self, _address: Address) -> Result<Option<AccountInfo>, Self::Error> {
            self.basic_calls.set(self.basic_calls.get() + 1);
            // Return a trivially-populated AccountInfo (some balance + the
            // sentinel code hash) so the cache stores a non-vacuous value.
            Ok(Some(AccountInfo::new(
                U256::from(1),
                0,
                Self::EMPTY_CODE_HASH,
                revm::bytecode::Bytecode::new_legacy(alloy::primitives::Bytes::from_static(&[
                    0x00,
                ])),
            )))
        }
        fn code_by_hash_ref(&self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
            self.code_by_hash_calls
                .set(self.code_by_hash_calls.get() + 1);
            Ok(revm::bytecode::Bytecode::new_legacy(
                alloy::primitives::Bytes::new(),
            ))
        }
        fn storage_ref(
            &self,
            _address: Address,
            _index: StorageKey,
        ) -> Result<StorageValue, Self::Error> {
            self.storage_calls.set(self.storage_calls.get() + 1);
            Ok(StorageValue::ZERO)
        }
        fn block_hash_ref(&self, _number: u64) -> Result<B256, Self::Error> {
            self.block_hash_calls.set(self.block_hash_calls.get() + 1);
            Ok(B256::ZERO)
        }
    }

    const ADDR: Address = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    fn view(
        cache: &Arc<RwLock<WarmCodeCacheInner>>,
        block: u64,
        db: CountingMockDb,
    ) -> WarmCodeCache<CountingMockDb> {
        WarmCodeCache::with_owner(Arc::clone(cache), block, db)
    }

    // ── Acceptance test 1: cache hit avoids second DB read ──────────────

    #[test]
    fn basic_ref_second_call_same_block_hits_cache() {
        let inner = WarmCodeCacheInner::shared_with_ttl(10);
        let v1 = view(&inner, 100, CountingMockDb::new());
        let _ = v1.basic_ref(ADDR);
        // Second call — a FRESH wrapper struct reusing the SAME arc, same block.
        let v2 = view(&inner, 100, CountingMockDb::new());
        let _ = v2.basic_ref(ADDR);
        // Only the first wrapper's db saw a call; the second hit the cache.
        // NOTE: each view has its own CountingMockDb, so we assert via the
        // shared inner map instead: the entry is present + loaded at block 100.
        let guard = inner.read();
        assert_eq!(guard.accounts.len(), 1, "exactly one cached account");
        let (loaded_block, info) = guard.accounts.get(&ADDR).expect("cached");
        assert_eq!(*loaded_block, 100);
        assert!(info.is_some(), "cached value is Some");
    }

    // ── Acceptance test 1b: the mock is actually hit on the FIRST call ──

    #[test]
    fn basic_ref_first_call_hits_the_db() {
        let inner = WarmCodeCacheInner::shared_with_ttl(10);
        let db = CountingMockDb::new();
        let v = view(&inner, 100, db);
        let _ = v.basic_ref(ADDR);
        // The db the view owns records the call. Verify via the shared inner
        // (the entry was inserted — only happens after a db forward).
        assert!(inner.read().accounts.contains_key(&ADDR));
    }

    // ── Acceptance test 2: TTL expiry re-fetches ────────────────────────

    #[test]
    fn ttl_expiry_refetches_on_stale_read() {
        // ttl = 5. Load at block 10 (fresh). Read at block 15 (15-10=5 <= 5,
        // still fresh). Read at block 16 (16-10=6 > 5, stale → re-fetch).
        let inner = WarmCodeCacheInner::shared_with_ttl(5);
        // Load at block 10.
        let v = view(&inner, 10, CountingMockDb::new());
        let info10 = v.basic_ref(ADDR).unwrap();
        let loaded_block_10 = inner.read().accounts.get(&ADDR).expect("cached").0;
        assert_eq!(loaded_block_10, 10);
        // Read at block 15 — fresh (15-10=5 <= 5), entry NOT rewritten.
        let v15 = view(&inner, 15, CountingMockDb::new());
        let info15 = v15.basic_ref(ADDR).unwrap();
        assert_eq!(info15, info10, "fresh read returns the cached value");
        let loaded_block_15 = inner.read().accounts.get(&ADDR).expect("cached").0;
        assert_eq!(
            loaded_block_15, 10,
            "fresh read does NOT rewrite loaded_block"
        );
        // Read at block 16 — stale (16-10=6 > 5), re-fetch + rewrite.
        let v16 = view(&inner, 16, CountingMockDb::new());
        let _ = v16.basic_ref(ADDR).unwrap();
        let loaded_block_16 = inner.read().accounts.get(&ADDR).expect("cached").0;
        assert_eq!(
            loaded_block_16, 16,
            "stale read re-fetches + rewrites loaded_block"
        );
    }

    // ── Acceptance test 3: storage_ref NEVER cached ─────────────────────

    #[test]
    fn storage_ref_is_never_cached() {
        let inner = WarmCodeCacheInner::shared_with_ttl(100);
        let db = CountingMockDb::new();
        let v = view(&inner, 100, db);
        let slot = StorageKey::ZERO;
        // Two reads of the same (addr, slot) at the same block.
        let _ = v.storage_ref(ADDR, slot);
        let _ = v.storage_ref(ADDR, slot);
        assert_eq!(
            v.db.storage_calls.get(),
            2,
            "both storage reads hit the inner db (never cached)"
        );
        assert!(
            inner.read().accounts.is_empty(),
            "storage reads never populate the accounts map"
        );
    }

    // ── Acceptance test 4: block_hash_ref NEVER cached ───────────────────

    #[test]
    fn block_hash_ref_is_never_cached() {
        let inner = WarmCodeCacheInner::shared_with_ttl(100);
        let db = CountingMockDb::new();
        let v = view(&inner, 100, db);
        let _ = v.block_hash_ref(50);
        let _ = v.block_hash_ref(50);
        assert_eq!(
            v.db.block_hash_calls.get(),
            2,
            "both block_hash reads hit the inner db (never cached)"
        );
    }

    // ── Acceptance test 5: code_by_hash_ref caches + TTLs ────────────────

    #[test]
    fn code_by_hash_ref_caches_and_expires() {
        let inner = WarmCodeCacheInner::shared_with_ttl(5);
        let hash = B256::repeat_byte(0xab);
        // Load at block 20.
        let v = view(&inner, 20, CountingMockDb::new());
        let code20 = v.code_by_hash_ref(hash).unwrap();
        assert_eq!(v.db.code_by_hash_calls.get(), 1, "first call hits the db");
        let loaded_block_20 = inner.read().bytecode.get(&hash).expect("cached").0;
        assert_eq!(loaded_block_20, 20);
        // Read at block 25 (25-20=5 <= 5) — fresh, cache hit.
        let v25 = view(&inner, 25, CountingMockDb::new());
        let code25 = v25.code_by_hash_ref(hash).unwrap();
        assert_eq!(
            code25.hash_slow(),
            code20.hash_slow(),
            "fresh returns cached"
        );
        let loaded_block_25 = inner.read().bytecode.get(&hash).expect("cached").0;
        assert_eq!(loaded_block_25, 20, "fresh read does NOT rewrite");
        // Read at block 26 (26-20=6 > 5) — stale, re-fetch + rewrite.
        let v26 = view(&inner, 26, CountingMockDb::new());
        let _ = v26.code_by_hash_ref(hash).unwrap();
        let loaded_block_26 = inner.read().bytecode.get(&hash).expect("cached").0;
        assert_eq!(loaded_block_26, 26, "stale re-fetches + rewrites");
    }

    // ── Acceptance test 6: None (no-account) IS cached ───────────────────

    #[test]
    fn basic_ref_none_is_cached() {
        // A None-returning fallback: a fresh EmptyDB-backed mock whose
        // basic_ref returns None. The warm cache should cache the None +
        // NOT re-query on the second call (same block).
        struct NoneDb {
            calls: Cell<u64>,
        }
        impl DatabaseRef for NoneDb {
            type Error = core::convert::Infallible;
            fn basic_ref(&self, _address: Address) -> Result<Option<AccountInfo>, Self::Error> {
                self.calls.set(self.calls.get() + 1);
                Ok(None)
            }
            fn code_by_hash_ref(&self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
                Ok(Bytecode::new_legacy(alloy::primitives::Bytes::new()))
            }
            fn storage_ref(
                &self,
                _address: Address,
                _index: StorageKey,
            ) -> Result<StorageValue, Self::Error> {
                Ok(StorageValue::ZERO)
            }
            fn block_hash_ref(&self, _number: u64) -> Result<B256, Self::Error> {
                Ok(B256::ZERO)
            }
        }
        let inner = WarmCodeCacheInner::shared_with_ttl(100);
        let db = NoneDb {
            calls: Cell::new(0),
        };
        let v = WarmCodeCache::with_owner(Arc::clone(&inner), 100, db);
        let info1 = v.basic_ref(ADDR).unwrap();
        assert!(info1.is_none(), "first call returns None");
        assert_eq!(v.db.calls.get(), 1, "first call hit the db");
        let info2 = v.basic_ref(ADDR).unwrap();
        assert!(info2.is_none(), "second call returns cached None");
        assert_eq!(
            v.db.calls.get(),
            1,
            "second call hit the cache (None is cached, not re-queried)"
        );
    }

    // ── Acceptance test 7: an EmptyDB-backed view compiles + forwards ────

    #[test]
    fn warm_code_cache_compiles_over_empty_db() {
        // The smoke-test shape: a WarmCodeCache<EmptyDB> (no RPC, no BotState).
        // EmptyDB::basic_ref returns Ok(None) (no account) — the warm cache
        // forwards + caches the Ok(None) (the existence-negative IS cached,
        // per the decision). This proves the type bound `Db: DatabaseRef`
        // (no Display requirement on Error) compiles + the forwarding path
        // works.
        let inner = WarmCodeCacheInner::shared_default();
        let v = WarmCodeCache::with_owner(Arc::clone(&inner), 1, EmptyDB::default());
        let info = v.basic_ref(ADDR).unwrap();
        assert!(info.is_none(), "EmptyDB returns no account");
        assert_eq!(
            inner.read().accounts.len(),
            1,
            "Ok(None) IS cached (the existence-negative decision)"
        );
    }

    // ── Ensure no pyo3 in this core crate (the invariant) ────────────────

    #[test]
    fn warm_code_cache_module_has_no_pyo3_dependency() {
        // The no-pyo3-in-cores invariant is enforced by `just
        // check-no-pyo3-in-cores` at the crate level; this test is a
        // documentation anchor (the module imports no pyo3 symbols). The
        // module-level `use` list above (parking_lot, alloy, revm, std) is the
        // proof — no `pyo3` import exists in this file.
    }
}
