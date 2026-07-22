//! Synthetic benchmark — does a proactive cache layer reduce repeated-EVM-call
//! latency beyond REVM's built-in `CacheDB`?
//!
//! In a backrun fan-out, the bot may simulate the same path shape many times
//! per block. Each `simulate_in_process` currently builds a *fresh*
//! `CacheDB<WrapDatabaseAsync<AlloyDB>>` (no cross-path cache sharing) — the
//! first SLOAD of every path hits RPC. Would a cache layer shared across
//! paths cut that out?
//!
//! # Workload
//!
//! `ITERATIONS` iterations × `TRANSACTS_PER_ITER` `transact_one` calls per
//! iter (mirrors `simulate_in_process`'s 7-call vector shape). Each transact
//! calls `getReserves()` on the SAME V2 pair — 1 storage SLOAD (slot 8) + the
//! pair's code load. The cache value persists across iters, so the question
//! is whether the cache *layer* persists too.
//!
//! # Configurations
//!
//! - **A (bare)** — fresh `WrapDatabaseAsync<AlloyDB>` per iter, no cache layer.
//!   Expected: every transact RPCs (code+slot), ~14 RPCs/iter.
//! - **B-fresh** — fresh `CacheDB<WrapDatabaseAsync<AlloyDB>>` per iter. Mirrors
//!   the current `simulate_in_process` architecture (`CacheDB` scoped per sim,
//!   no cross-path sharing). Within an iter, calls 2-7 hit the journal cache
//!   (warmed by call 1). Expected: 2 RPCs/iter, `2 * ITERATIONS` total.
//! - **B-shared** — ONE long-lived `CacheDB` shared across all iters via a
//!   shared EVM (the journal + `CacheDB` warm together on call 1). Tests whether
//!   REVM's `CacheDB` alone is enough — by sharing the EVM across iters.
//!   Expected: 2 RPCs total.
//! - **C (proactive)** — shared `AggressiveCachedProvider` (caches both
//!   `storage_ref` AND `basic_ref` across iters) with a *fresh* EVM per iter
//!   (mirrors the production shape, where each path is a fresh simulation
//!   sharing only the cache layer). Expected: 2 RPCs total.
//!
//! # Decision rule
//!
//! - B-shared ≈ C → the custom layer isn't worth it; share REVM's `CacheDB`.
//! - B-shared ≫ C → REVM's cache has a limitation the proactive layer fixes.
//! - B-shared ≈ B-fresh → REVM's `CacheDB` isn't caching across transacts.
//!
//! # Run
//!
//! ```bash
//! RPC=http://host.containers.internal:8545 \
//! cargo run --release --example rpc_cache_fanout --manifest-path rust/Cargo.toml
//! ```

#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

use std::collections::HashMap;
use std::env;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alloy::eips::BlockId;
use alloy::primitives::{address, Address, Bytes, U256};
use alloy::providers::{Provider, ProviderBuilder, RootProvider};
use revm::context::TxEnv;
use revm::database::{AlloyDB, CacheDB};
use revm::database_interface::{DatabaseRef, WrapDatabaseAsync, WrapDatabaseRef};
use revm::primitives::{StorageKey, StorageValue, TxKind, B256};
use revm::{ExecuteEvm, MainBuilder, MainContext};

/// The Uniswap V2 USDC/WETH pair — a real mainnet V2 pair with code at the
/// pinned block. Override via `PAIR_ADDRESS` env var.
const DEFAULT_PAIR: Address = address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc");

/// `getReserves()` selector — `bytes4(keccak256("getReserves()")) = 0x0902f1ac`.
const GET_RESERVES: [u8; 4] = [0x09, 0x02, 0xf1, 0xac];

/// Iterations per config — mimics a small backrun fan-out.
const ITERATIONS: usize = 50;

/// Transacts per iter — mirrors `simulate_in_process`'s 7-call vector.
const TRANSACTS_PER_ITER: usize = 7;

/// Default local RPC (devcontainer convention).
const RPC_URL_DEFAULT: &str = "http://host.containers.internal:8545";

/// The async-backed RPC DB (`WrapDatabaseAsync<AlloyDB>`) blocks on the ambient
/// tokio runtime — so we need a multi-thread runtime.
#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let rpc_url = env::var("RPC").unwrap_or_else(|_| RPC_URL_DEFAULT.to_string());
    let pair = env::var("PAIR_ADDRESS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PAIR);

    eprintln!("RPC URL: {rpc_url}");
    eprintln!("Pair:    {pair}");
    eprintln!("Iters:   {ITERATIONS}, transacts/iter: {TRANSACTS_PER_ITER}");

    let provider = ProviderBuilder::default().connect_http(rpc_url.parse().expect("valid URL"));

    // Pin to the latest block so all configs hit the same state (V2 pair slot
    // 8 changes per block, so cache validity is block-scoped).
    let block_number = provider
        .get_block_number()
        .await
        .expect("RPC reachable: get_block_number failed");
    let block_id = BlockId::number(block_number);
    eprintln!("Block:    {block_number}");
    eprintln!();

    let calldata = Bytes::from(GET_RESERVES.to_vec());

    let cfg_a = run_config_bare(&provider, pair, &calldata, block_id);
    let cfg_b_fresh = run_config_cachdb_fresh(&provider, pair, &calldata, block_id);
    let cfg_b_shared = run_config_cachdb_shared(&provider, pair, &calldata, block_id);
    let cfg_c = run_config_proactive_cached(&provider, pair, &calldata, block_id);

    print_table(&[
        ("A (bare)", &cfg_a),
        ("B-fresh (fresh CacheDB/iter)", &cfg_b_fresh),
        ("B-shared (one shared EVM)", &cfg_b_shared),
        ("C (proactive, fresh EVM/iter)", &cfg_c),
    ]);
}

// =================================================================
// Counter + DB wrapper layers
// =================================================================

/// The caller account used for the simulated `getReserves()` calls. Funded by
/// [`FundedCallerDb`] so revm's balance/gas check passes without an RPC.
const CALLER: Address = Address::ZERO;

/// A shared RPC round-trip counter. The `Counting*` wrappers below bump it.
#[derive(Default)]
struct Counter {
    storage_rpcs: AtomicU64,
    basic_rpcs: AtomicU64,
}

impl Counter {
    fn bump_storage(&self) {
        self.storage_rpcs.fetch_add(1, Ordering::Relaxed);
    }
    fn bump_basic(&self) {
        self.basic_rpcs.fetch_add(1, Ordering::Relaxed);
    }
    fn storage_rpcs(&self) -> u64 {
        self.storage_rpcs.load(Ordering::Relaxed)
    }
    fn basic_rpcs(&self) -> u64 {
        self.basic_rpcs.load(Ordering::Relaxed)
    }
}

/// A wrap around an RPC-backed `DatabaseRef` that counts RPC round-trips.
/// `Db` is the inner RPC DB type (`WrapDatabaseAsync<AlloyDB<...>>` in
/// production). Counted per `storage_ref` + `basic_ref` call.
struct CountingRpcDb<Db: DatabaseRef> {
    inner: Db,
    counter: Arc<Counter>,
}

impl<Db: DatabaseRef> DatabaseRef for CountingRpcDb<Db> {
    type Error = Db::Error;

    fn storage_ref(&self, address: Address, slot: StorageKey) -> Result<StorageValue, Self::Error> {
        self.counter.bump_storage();
        self.inner.storage_ref(address, slot)
    }

    fn basic_ref(&self, address: Address) -> Result<Option<revm::state::AccountInfo>, Self::Error> {
        self.counter.bump_basic();
        self.inner.basic_ref(address)
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<revm::bytecode::Bytecode, Self::Error> {
        // Unreachable when `basic_ref` eagerly loads code (QGJGWI spike). Not
        // counted — a successful `basic_ref` RPC fetches account info AND code.
        self.inner.code_by_hash_ref(code_hash)
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        self.inner.block_hash_ref(number)
    }
}

/// A proactive cache layer — caches `storage_ref` + `basic_ref` across iters.
/// Wrapped in `Arc` for sharing; the [`CachedRef`] newtype provides the
/// `DatabaseRef` impl without tripping orphan rules (`Arc<T>` is upstream).
struct AggressiveCachedProvider<Db: DatabaseRef> {
    inner: Db,
    storage: Mutex<HashMap<(Address, StorageKey), StorageValue>>,
    basic: Mutex<HashMap<Address, Option<revm::state::AccountInfo>>>,
    counter: Arc<Counter>,
}

impl<Db: DatabaseRef> AggressiveCachedProvider<Db> {
    fn new(inner: Db, counter: Arc<Counter>) -> Self {
        Self {
            inner,
            storage: Mutex::new(HashMap::new()),
            basic: Mutex::new(HashMap::new()),
            counter,
        }
    }
}

/// A newtype around `Arc<AggressiveCachedProvider>` to satisfy orphan rules —
/// upstream `Arc<T>` can't get an `impl DatabaseRef` from this crate.
struct CachedRef<Db: DatabaseRef>(Arc<AggressiveCachedProvider<Db>>);

impl<Db: DatabaseRef> Clone for CachedRef<Db> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<Db: DatabaseRef> DatabaseRef for CachedRef<Db> {
    type Error = Db::Error;

    fn storage_ref(&self, address: Address, slot: StorageKey) -> Result<StorageValue, Self::Error> {
        if let Some(hit) = self
            .0
            .storage
            .lock()
            .expect("poisoned")
            .get(&(address, slot))
        {
            return Ok(*hit);
        }
        self.0.counter.bump_storage();
        let v = self.0.inner.storage_ref(address, slot)?;
        self.0
            .storage
            .lock()
            .expect("poisoned")
            .insert((address, slot), v);
        Ok(v)
    }

    fn basic_ref(&self, address: Address) -> Result<Option<revm::state::AccountInfo>, Self::Error> {
        if let Some(hit) = self
            .0
            .basic
            .lock()
            .expect("poisoned")
            .get(&address)
            .cloned()
        {
            return Ok(hit);
        }
        self.0.counter.bump_basic();
        let v = self.0.inner.basic_ref(address)?;
        self.0
            .basic
            .lock()
            .expect("poisoned")
            .insert(address, v.clone());
        Ok(v)
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<revm::bytecode::Bytecode, Self::Error> {
        self.0.inner.code_by_hash_ref(code_hash)
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        self.0.inner.block_hash_ref(number)
    }
}

/// A wrapper that intercepts `basic_ref(CALLER)` to return a generously funded
/// account, bypassing revm's `CallerLackOfMaxFee` rejection. Without this,
/// every transact reverts pre-contract-execution and NO storage reads happen
/// (revm exits before SLOAD).
///
/// Other addresses flow through to the inner DB unchanged.
struct FundedCallerDb<Db: DatabaseRef> {
    inner: Db,
}

impl<Db: DatabaseRef> DatabaseRef for FundedCallerDb<Db> {
    type Error = Db::Error;

    fn basic_ref(&self, address: Address) -> Result<Option<revm::state::AccountInfo>, Self::Error> {
        if address == CALLER {
            return Ok(Some(funded_caller_account()));
        }
        self.inner.basic_ref(address)
    }

    fn storage_ref(&self, address: Address, slot: StorageKey) -> Result<StorageValue, Self::Error> {
        self.inner.storage_ref(address, slot)
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<revm::bytecode::Bytecode, Self::Error> {
        self.inner.code_by_hash_ref(code_hash)
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        self.inner.block_hash_ref(number)
    }
}

/// Build the funded caller [`revm::state::AccountInfo`] — infinite balance,
/// empty code, nonce 0. Used by [`FundedCallerDb`] so the caller clears revm's
/// gas-payment check without an RPC round-trip.
fn funded_caller_account() -> revm::state::AccountInfo {
    revm::state::AccountInfo {
        balance: U256::MAX,
        nonce: 0,
        code_hash: revm::primitives::KECCAK_EMPTY,
        code: Some(revm::bytecode::Bytecode::default()),
        account_id: None,
    }
}

// =================================================================
// Result type + printing
// =================================================================

struct ConfigResult {
    total_wall: Duration,
    per_transact_us: Vec<u64>,
    storage_rpcs: u64,
    basic_rpcs: u64,
}

impl ConfigResult {
    fn p50(&self) -> u64 {
        percentile(&self.per_transact_us, 50)
    }
    fn p99(&self) -> u64 {
        percentile(&self.per_transact_us, 99)
    }
}

fn format_ns(ns: u64) -> String {
    if ns < 1_000 {
        format!("{ns} ns")
    } else if ns < 1_000_000 {
        format!("{:.2} µs", ns as f64 / 1_000.0)
    } else if ns < 1_000_000_000 {
        format!("{:.2} ms", ns as f64 / 1_000_000.0)
    } else {
        format!("{:.2} s", ns as f64 / 1_000_000_000.0)
    }
}

fn format_dur(d: Duration) -> String {
    format_ns(d.as_nanos() as u64)
}

fn percentile(samples: &[u64], pct: u8) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let idx = ((pct as usize * (sorted.len() - 1) + 50) / 100).min(sorted.len() - 1);
    sorted[idx]
}

fn print_table(rows: &[(&str, &ConfigResult)]) {
    println!(
        "| {:<32} | {:>10} | {:>10} | {:>10} | {:>13} | {:>11} |",
        "config", "total", "p50", "p99", "storage RPCs", "basic RPCs"
    );
    println!(
        "| {:->32} | {:->10} | {:->10} | {:->10} | {:->13} | {:->11} |",
        "", "", "", "", "", ""
    );
    for (label, r) in rows {
        println!(
            "| {:<32} | {:>10} | {:>10} | {:>10} | {:>13} | {:>11} |",
            label,
            format_dur(r.total_wall),
            format_ns(r.p50()),
            format_ns(r.p99()),
            r.storage_rpcs,
            r.basic_rpcs
        );
    }
}

// =================================================================
// Transact loop helpers
// =================================================================

fn print_transact_result<E: std::fmt::Display>(
    res: &Result<revm::context_interface::result::ExecutionResult, E>,
) {
    match res {
        Ok(r) => {
            let kind = match r {
                revm::context_interface::result::ExecutionResult::Success { reason, .. } => {
                    format!("Success({reason:?})")
                }
                revm::context_interface::result::ExecutionResult::Revert { output, .. } => {
                    format!("Revert({})", revm::primitives::hex::encode(output))
                }
                revm::context_interface::result::ExecutionResult::Halt { reason, .. } => {
                    format!("Halt({reason})")
                }
            };
            let out = r
                .output()
                .map_or_else(|| "<none>".to_string(), revm::primitives::hex::encode);
            eprintln!(
                "[transact-result] {kind} output_len={} output={out}",
                out.len()
            );
        }
        Err(e) => eprintln!("[transact-result] Err({e})"),
    }
}

fn build_tx(pair: Address, calldata: &Bytes) -> TxEnv {
    TxEnv::builder()
        .caller(CALLER)
        .kind(TxKind::Call(pair))
        .data(calldata.clone())
        .value(U256::ZERO)
        .gas_limit(100_000)
        .gas_price(1)
        .build()
        .expect("valid TxEnv")
}

/// Run `iters × transacts_per_iter` transacts, **building a fresh EVM per
/// iter**. The `db_factory` is called once per iter to produce the EVM's DB
/// (so a shared DB must be threaded through closures + Arc).
fn run_fresh_evm_per_iter<Db, F>(
    mut db_factory: F,
    pair: Address,
    calldata: &Bytes,
    iters: usize,
    transacts_per_iter: usize,
    print_first_result: bool,
) -> Vec<u64>
where
    Db: revm::database_interface::Database,
    F: FnMut() -> Db,
{
    let tx = build_tx(pair, calldata);
    let mut all_times = Vec::with_capacity(iters * transacts_per_iter);
    let mut first_printed = !print_first_result;
    for _ in 0..iters {
        let mut ctx = revm::context::Context::mainnet();
        ctx.cfg.disable_nonce_check = true;
        let mut evm = ctx.with_db(db_factory()).build_mainnet();
        for _ in 0..transacts_per_iter {
            let start = Instant::now();
            let res = evm.transact_one(tx.clone());
            if !first_printed {
                first_printed = true;
                print_transact_result(&res);
            }
            all_times.push(start.elapsed().as_nanos() as u64);
        }
    }
    all_times
}

/// Run `total_transacts` transacts on **one persistent EVM** (journal + cache
/// accumulate across all transacts). Used by B-shared.
fn run_one_evm<Db: revm::database_interface::Database>(
    db: Db,
    pair: Address,
    calldata: &Bytes,
    total_transacts: usize,
    print_first_result: bool,
) -> Vec<u64> {
    let mut ctx = revm::context::Context::mainnet();
    ctx.cfg.disable_nonce_check = true;
    let mut evm = ctx.with_db(db).build_mainnet();
    let tx = build_tx(pair, calldata);
    let mut first_printed = !print_first_result;
    (0..total_transacts)
        .map(|_| {
            let start = Instant::now();
            let res = evm.transact_one(tx.clone());
            if !first_printed {
                first_printed = true;
                print_transact_result(&res);
            }
            start.elapsed().as_nanos() as u64
        })
        .collect()
}

// =================================================================
// Config A: bare RPC, fresh WrapDatabaseAsync per iter, no cache layer
// =================================================================

fn run_config_bare(
    provider: &RootProvider,
    pair: Address,
    calldata: &Bytes,
    block_id: BlockId,
) -> ConfigResult {
    let counter = Arc::new(Counter::default());
    let counter_clone = counter.clone();
    let provider_clone = provider.clone();
    let start = Instant::now();
    let per_transact = run_fresh_evm_per_iter(
        move || {
            WrapDatabaseRef(FundedCallerDb {
                inner: CountingRpcDb {
                    inner: WrapDatabaseAsync::new(AlloyDB::new(provider_clone.clone(), block_id))
                        .expect("multi-thread runtime"),
                    counter: counter_clone.clone(),
                },
            })
        },
        pair,
        calldata,
        ITERATIONS,
        TRANSACTS_PER_ITER,
        true,
    );
    ConfigResult {
        total_wall: start.elapsed(),
        per_transact_us: per_transact,
        storage_rpcs: counter.storage_rpcs(),
        basic_rpcs: counter.basic_rpcs(),
    }
}

// =================================================================
// Config B-fresh: fresh CacheDB<CountingRpcDb> per iter
// =================================================================

fn run_config_cachdb_fresh(
    provider: &RootProvider,
    pair: Address,
    calldata: &Bytes,
    block_id: BlockId,
) -> ConfigResult {
    let counter = Arc::new(Counter::default());
    let counter_clone = counter.clone();
    let provider_clone = provider.clone();
    let start = Instant::now();
    let per_transact = run_fresh_evm_per_iter(
        move || {
            CacheDB::new(FundedCallerDb {
                inner: CountingRpcDb {
                    inner: WrapDatabaseAsync::new(AlloyDB::new(provider_clone.clone(), block_id))
                        .expect("multi-thread runtime"),
                    counter: counter_clone.clone(),
                },
            })
        },
        pair,
        calldata,
        ITERATIONS,
        TRANSACTS_PER_ITER,
        false,
    );
    ConfigResult {
        total_wall: start.elapsed(),
        per_transact_us: per_transact,
        storage_rpcs: counter.storage_rpcs(),
        basic_rpcs: counter.basic_rpcs(),
    }
}

// =================================================================
// Config B-shared: ONE CacheDB across all iters (one persistent EVM)
// =================================================================

fn run_config_cachdb_shared(
    provider: &RootProvider,
    pair: Address,
    calldata: &Bytes,
    block_id: BlockId,
) -> ConfigResult {
    let counter = Arc::new(Counter::default());
    let alloy_db = AlloyDB::new(provider.clone(), block_id);
    let wrap_db = WrapDatabaseAsync::new(alloy_db).expect("multi-thread runtime");
    let counting_db = CountingRpcDb {
        inner: wrap_db,
        counter: counter.clone(),
    };
    let cache_db = CacheDB::new(FundedCallerDb { inner: counting_db });
    let start = Instant::now();
    let per_transact = run_one_evm(
        cache_db,
        pair,
        calldata,
        ITERATIONS * TRANSACTS_PER_ITER,
        false,
    );
    ConfigResult {
        total_wall: start.elapsed(),
        per_transact_us: per_transact,
        storage_rpcs: counter.storage_rpcs(),
        basic_rpcs: counter.basic_rpcs(),
    }
}

// =================================================================
// Config C: shared AggressiveCachedProvider, fresh EVM per iter
// (the proposed proactive cache layer)
// =================================================================

fn run_config_proactive_cached(
    provider: &RootProvider,
    pair: Address,
    calldata: &Bytes,
    block_id: BlockId,
) -> ConfigResult {
    let counter = Arc::new(Counter::default());
    let alloy_db = AlloyDB::new(provider.clone(), block_id);
    let wrap_db = WrapDatabaseAsync::new(alloy_db).expect("multi-thread runtime");
    let counting_db = CountingRpcDb {
        inner: wrap_db,
        counter: counter.clone(),
    };
    let shared = Arc::new(AggressiveCachedProvider::new(
        FundedCallerDb { inner: counting_db },
        counter.clone(),
    ));
    let cached_ref = CachedRef(shared);
    let start = Instant::now();
    let per_transact = run_fresh_evm_per_iter(
        move || WrapDatabaseRef(cached_ref.clone()),
        pair,
        calldata,
        ITERATIONS,
        TRANSACTS_PER_ITER,
        false,
    );
    ConfigResult {
        total_wall: start.elapsed(),
        per_transact_us: per_transact,
        storage_rpcs: counter.storage_rpcs(),
        basic_rpcs: counter.basic_rpcs(),
    }
}
