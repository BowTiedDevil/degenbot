#![expect(clippy::expect_used, clippy::print_stderr, clippy::print_stdout)]
//! Synthetic benchmark — production-shaped: one warm-up trigger + many
//! read-only `eth_call`-style transacts against a shared cache.
//!
//! # The production shape
//!
//! The bot's pump loop is: one block trigger updates state, then a fan-out of
//! read-only `eth_call`-shaped simulations runs against that frozen state.
//! REVM's API for "isolated read-only call" is `transact()` (which internally
//! does `transact_one` + `finalize` — `finalize` clears the journal so the
//! next call starts from clean chain state, with no accumulated SSTOREs).
//!
//! This benchmark models that shape: one warm-up `transact()` (the "trigger")
//! pays the cold RPCs; then `FANOUT` read-only `transact()` calls fan out
//! against the same shared DB, each isolated via `finalize()`.
//!
//! # Workload
//!
//! Each call is `getReserves()` on a real mainnet V2 pair — 1 storage SLOAD
//! (slot 8) + the pair's code load. Trivially cacheable (one address, one
//! slot, one code hash), which isolates the cache-layer variable.
//!
//! # Configurations
//!
//! All three use `transact()` (isolated journals — no accumulation confound):
//!
//! - **A (bare)** — no `CacheDB`. Every call builds a fresh EVM around a bare
//!   `WrapDatabaseAsync<AlloyDB>`. Every call RPCs.
//!   Expected: ~2 RPCs/call, `2 * (1 + FANOUT)` total.
//! - **B (shared `CacheDB`)** — ONE shared EVM + ONE shared `CacheDB`. The
//!   trigger `transact()` warms the cache (RPCs: 2). Every fan-out call hits
//!   the warmed cache.
//!   Expected: 2 RPCs total (first call only).
//! - **C (proactive)** — shared `AggressiveCachedProvider` (caches
//!   `storage_ref` + `basic_ref`), with a *fresh* `CacheDB` overlay + fresh
//!   EVM per call (mirrors a production shape where each sim is a fresh
//!   simulation sharing only the proactive cache layer).
//!   Expected: 2 RPCs total (first call only).
//! - **D (cross-block `WarmCodeCache`)** — fresh `CacheDB`+EVM per block over
//!   N consecutive mainnet blocks, with a persistent `WarmCodeCacheInner`
//!   arc shared across all N blocks. The immutable `basic`/code RPC fires
//!   once (cold) then hits the warm cache until the per-entry TTL; the
//!   mutable `storage` RPC fires every block (never cached).
//!   Expected: N+2 RPCs over N blocks (1 basic cold + 1 basic TTL re-cold +
//!   N storage) vs 2N for the no-warm-cache baseline.
//!
//! # Decision rule
//!
//! - B ≈ C → the custom proactive layer isn't worth it; share REVM's
//!   `CacheDB` via a persistent EVM with `transact()` calls.
//! - B ≫ C → the proactive layer has a real advantage (e.g. if `CacheDB`
//!   sharing across `transact()` calls is broken in some way).
//! - D ≪ B-over-N-blocks → the cross-block `WarmCodeCache` layer earns its
//!   keep: it lets the per-block `CacheDB` rebuild (correct — fresh storage
//!   each block) without re-paying the immutable `basic`/code RPC every block.
//!
//! # Observed result (chain=1, block ~25.5M, 1 trigger + 50 fan-out reads)
//!
//! ```text
//! | config                              | total   | p50      | p99     | stg | bas |
//! | A (bare, no cache)                   | 32-38 ms| ~570 µs  | ~2 ms   |  51 |  51 |
//! | B (shared CacheDB, transact())      | ~590 µs | ~1 µs    | ~520 µs |   1 |   1 |
//! | C (proactive, fresh CacheDB/call)   | ~680 µs | ~1.5 µs  | ~530 µs |   2 |   3 |
//! ```
//!
//! **B ≈ C (B slightly faster).** Both cut RPCs to ~1-3 (vs 102 for A) and
//! total wall time to ~600 µs (vs ~35 ms for A). The custom proactive layer
//! (C) is marginally slower than REVM's built-in `CacheDB` shared via a
//! persistent EVM with `transact()` (B), because the mutex + `Arc` layers add
//! overhead without benefit. The decision rule says: **share REVM's `CacheDB`
//! via a persistent EVM using `transact()` — don't build a separate proactive
//! cache layer.**
//!
//! **D ≪ B-over-N-blocks (measured, chain=1, N=12 blocks, TTL=10):** config D
//! fires 2 `basic` RPCs (cold load at block 1 + TTL re-cold at block 12) + 12
//! `storage` RPCs = 14 total; the no-warm-cache baseline (fresh `CacheDB` per
//! block) fires 12 `basic` + 12 `storage` = 24 total. **The `WarmCodeCache`
//! layer saves 10 RPCs / 42% over 12 blocks** — the saved `basic` RPC on
//! every block past the cold load. The TTL re-cold-load lands precisely at
//! block 12 (loaded at ordinal 1, `12 - 1 = 11 > 10` → stale), asserted by
//! [`assert_warm_cache_ttl_boundary`].
//!
//! # Run
//!
//! ```bash
//! RPC=http://host.containers.internal:8545 \
//! cargo run --release --example rpc_cache_fanout --manifest-path rust/Cargo.toml
//! ```

#![expect(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

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

// The example lives in the `degenbot-simulation` crate (the folded engine
// home — ADR-019 D4), so it reaches the cross-block warm-cache types via
// the library's public surface.
use degenbot_simulation::{WarmCodeCache, WarmCodeCacheInner};

/// The Uniswap V2 USDC/WETH pair — a real mainnet V2 pair with code at the
/// pinned block. Override via `PAIR_ADDRESS` env var.
const DEFAULT_PAIR: Address = address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc");

/// `getReserves()` selector — `bytes4(keccak256("getReserves()")) = 0x0902f1ac`.
const GET_RESERVES: [u8; 4] = [0x09, 0x02, 0xf1, 0xac];

/// Fan-out size — number of read-only `transact()` calls after the trigger.
/// Models an arbi-path fan-out of 50 candidate paths per block.
const FANOUT: usize = 50;

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
    eprintln!("Fan-out: 1 trigger + {FANOUT} read-only calls");

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
    let tx = build_tx(pair, &calldata);

    let cfg_a = run_config_bare(&provider, &tx, block_id);
    let cfg_b = run_config_shared_cachedb(&provider, &tx, block_id);
    let cfg_c = run_config_proactive(&provider, &tx, block_id);

    print_table(&[
        ("A (bare, no cache)", &cfg_a),
        ("B (shared CacheDB, transact())", &cfg_b),
        ("C (proactive, fresh CacheDB/call)", &cfg_c),
    ]);

    // -----------------------------------------------------------------
    // Config D: cross-block WarmCodeCache over N consecutive mainnet blocks.
    // Sweeps `WARM_CACHE_N_BLOCKS` blocks ending at `block_number` so the
    // last block is the TTL re-cold boundary (loaded at ordinal 1, stale at
    // ordinal ttl_blocks + 2).
    // -----------------------------------------------------------------
    eprintln!();
    eprintln!(
        "=== Config D: cross-block WarmCodeCache (N={WARM_CACHE_N_BLOCKS} blocks, TTL={WARM_CACHE_TTL_BLOCKS}) ==="
    );
    let start_block = block_number.saturating_sub(WARM_CACHE_N_BLOCKS as u64 - 1);
    eprintln!("Sweep: block {start_block} ..= {block_number}");
    let cfg_d = run_config_warm_cache_multi_block(
        &provider,
        &tx,
        start_block,
        WARM_CACHE_N_BLOCKS,
        WARM_CACHE_TTL_BLOCKS,
    );
    let cfg_b_mb =
        run_config_shared_cachedb_multi_block(&provider, &tx, start_block, WARM_CACHE_N_BLOCKS);
    print_multi_block_breakdown("D", &cfg_d);
    print_multi_block_breakdown("B-noblock", &cfg_b_mb);
    let d_total = cfg_d.total_basic + cfg_d.total_storage;
    let b_total = cfg_b_mb.total_basic + cfg_b_mb.total_storage;
    let saved = b_total.saturating_sub(d_total);
    let pct = if b_total == 0 {
        0.0
    } else {
        100.0 * saved as f64 / b_total as f64
    };
    eprintln!(
        "[warm-cache] config D total RPCs = {d_total} vs config B (no warm cache) total RPCs = {b_total} over {WARM_CACHE_N_BLOCKS} blocks (saved {saved} RPCs, {pct:.0}% reduction)"
    );
    assert_warm_cache_ttl_boundary(&cfg_d, WARM_CACHE_TTL_BLOCKS);
    eprintln!(
        "[warm-cache] PASS: TTL boundary re-cold-load verified (basic RPCs: cold at block 1, warm through block {warm}, re-cold at block {cold})",
        warm = WARM_CACHE_TTL_BLOCKS + 1,
        cold = WARM_CACHE_TTL_BLOCKS + 2
    );
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

/// A proactive cache layer — caches `storage_ref` + `basic_ref` across calls.
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
/// account, bypassing revm's `CallerLackOfMaxFee` rejection. Other addresses
/// flow through to the inner DB unchanged.
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
    per_call_ns: Vec<u64>,
    storage_rpcs: u64,
    basic_rpcs: u64,
}

impl ConfigResult {
    fn p50(&self) -> u64 {
        percentile(&self.per_call_ns, 50)
    }
    fn p99(&self) -> u64 {
        percentile(&self.per_call_ns, 99)
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
        "| {:<36} | {:>10} | {:>10} | {:>10} | {:>13} | {:>11} |",
        "config", "total", "p50", "p99", "storage RPCs", "basic RPCs"
    );
    println!(
        "| {:->36} | {:->10} | {:->10} | {:->10} | {:->13} | {:->11} |",
        "", "", "", "", "", ""
    );
    for (label, r) in rows {
        println!(
            "| {:<36} | {:>10} | {:>10} | {:>10} | {:>13} | {:>11} |",
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
// Tx builder + isolated-transact helpers
// =================================================================

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

/// Build an EVM with `disable_nonce_check` set (the 7-call vector in
/// `simulate_path_on_evm` shares one caller; `eth_simulateV1` doesn't bump
/// the nonce per call, so revm's per-tx nonce floor would reject calls 2..N).
/// Returns the concrete `MainnetEvm<Context<Db, ...>>` — `impl` is avoided
/// so callers can pass a `&mut` to `run_isolated_transacts`.
fn build_evm<Db: revm::database_interface::Database>(
    db: Db,
) -> impl revm::ExecuteEvm<
    Tx = TxEnv,
    ExecutionResult = revm::context_interface::result::ExecutionResult,
    Error: std::fmt::Display,
> {
    let mut ctx = revm::context::Context::mainnet();
    ctx.cfg.disable_nonce_check = true;
    ctx.with_db(db).build_mainnet()
}

/// Run `transact()` (not `transact_one()`!) so `finalize()` clears the journal
/// after each call — no accumulated state across calls. This is the
/// production-realistic `eth_call` shape: each call is isolated.
///
/// `evm` is a `&mut` persistent EVM (shared across calls).
fn run_isolated_transacts<E>(
    evm: &mut E,
    tx: &TxEnv,
    n_calls: usize,
    print_first_result: bool,
) -> Vec<u64>
where
    E: revm::ExecuteEvm<
        Tx = TxEnv,
        ExecutionResult = revm::context_interface::result::ExecutionResult,
    >,
    <E as revm::ExecuteEvm>::Error: std::fmt::Display,
{
    let mut first_printed = !print_first_result;
    (0..n_calls)
        .map(|_| {
            let start = Instant::now();
            let res = evm.transact(tx.clone());
            if !first_printed {
                first_printed = true;
                print_transact_result(&res.map(|r| r.result));
            }
            start.elapsed().as_nanos() as u64
        })
        .collect()
}

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

// =================================================================
// Config A: bare RPC, no CacheDB. Fresh EVM per call. Every call RPCs.
// =================================================================

fn run_config_bare(provider: &RootProvider, tx: &TxEnv, block_id: BlockId) -> ConfigResult {
    let counter = Arc::new(Counter::default());
    let counter_clone = counter.clone();
    let provider_clone = provider.clone();
    let start = Instant::now();
    let mut per_call = Vec::with_capacity(1 + FANOUT);
    // Trigger + fan-out: fresh EVM w/ bare RPC DB each call.
    for i in 0..=FANOUT {
        let alloy_db = AlloyDB::new(provider_clone.clone(), block_id);
        let wrap_db = WrapDatabaseAsync::new(alloy_db).expect("multi-thread runtime");
        let counting_db = CountingRpcDb {
            inner: wrap_db,
            counter: counter_clone.clone(),
        };
        let funded_db = FundedCallerDb { inner: counting_db };
        let db = WrapDatabaseRef(funded_db);
        let mut evm = build_evm(db);
        let t = Instant::now();
        let _ = evm.transact(tx.clone());
        per_call.push(t.elapsed().as_nanos() as u64);
        if i == 0 {
            eprintln!(
                "[A] trigger call done: {} storage RPCs, {} basic RPCs",
                counter.storage_rpcs(),
                counter.basic_rpcs()
            );
        }
    }
    ConfigResult {
        total_wall: start.elapsed(),
        per_call_ns: per_call,
        storage_rpcs: counter.storage_rpcs(),
        basic_rpcs: counter.basic_rpcs(),
    }
}

// =================================================================
// Config B: shared CacheDB, shared EVM, isolated transact() calls
// =================================================================

fn run_config_shared_cachedb(
    provider: &RootProvider,
    tx: &TxEnv,
    block_id: BlockId,
) -> ConfigResult {
    let counter = Arc::new(Counter::default());
    let alloy_db = AlloyDB::new(provider.clone(), block_id);
    let wrap_db = WrapDatabaseAsync::new(alloy_db).expect("multi-thread runtime");
    let counting_db = CountingRpcDb {
        inner: wrap_db,
        counter: counter.clone(),
    };
    let funded_db = FundedCallerDb { inner: counting_db };
    let cache_db = CacheDB::new(funded_db);
    let mut evm = build_evm(cache_db);
    let start = Instant::now();
    // Trigger (call 0) warms the cache; fan-out (1..=FANOUT) hits it.
    let per_call = run_isolated_transacts(&mut evm, tx, 1 + FANOUT, true);
    eprintln!(
        "[B] after trigger+fan-out: {} storage RPCs, {} basic RPCs",
        counter.storage_rpcs(),
        counter.basic_rpcs()
    );
    ConfigResult {
        total_wall: start.elapsed(),
        per_call_ns: per_call,
        storage_rpcs: counter.storage_rpcs(),
        basic_rpcs: counter.basic_rpcs(),
    }
}

// =================================================================
// Config C: proactive cache (AggressiveCachedProvider), fresh CacheDB+EVM
// per call (mirrors fresh-EVM-per-sim production shape with a shared
// proactive cache layer underneath).
// =================================================================

fn run_config_proactive(provider: &RootProvider, tx: &TxEnv, block_id: BlockId) -> ConfigResult {
    let counter = Arc::new(Counter::default());
    let alloy_db = AlloyDB::new(provider.clone(), block_id);
    let wrap_db = WrapDatabaseAsync::new(alloy_db).expect("multi-thread runtime");
    let counting_db = CountingRpcDb {
        inner: wrap_db,
        counter: counter.clone(),
    };
    let funded_db = FundedCallerDb { inner: counting_db };
    let shared = Arc::new(AggressiveCachedProvider::new(funded_db, counter.clone()));
    let cached_ref = CachedRef(shared);
    let start = Instant::now();
    let mut per_call = Vec::with_capacity(1 + FANOUT);
    for i in 0..=FANOUT {
        // Fresh CacheDB + EVM per call; the shared CachedRef persists.
        let cache_db = CacheDB::new(WrapDatabaseRef(cached_ref.clone()));
        let mut evm = build_evm(cache_db);
        let t = Instant::now();
        let _ = evm.transact(tx.clone());
        per_call.push(t.elapsed().as_nanos() as u64);
        if i == 0 {
            eprintln!(
                "[C] trigger call done: {} storage RPCs, {} basic RPCs",
                counter.storage_rpcs(),
                counter.basic_rpcs()
            );
        }
    }
    ConfigResult {
        total_wall: start.elapsed(),
        per_call_ns: per_call,
        storage_rpcs: counter.storage_rpcs(),
        basic_rpcs: counter.basic_rpcs(),
    }
}

// =================================================================
// Config D: cross-block WarmCodeCache — fresh CacheDB+EVM per block, with
// the persistent WarmCodeCacheInner arc shared across N blocks.
// =================================================================
//
// The production shape is multi-block: block N's `getReserves()` runs against
// a `CacheDB` rebuilt fresh for block N (so the mutable storage row — V2
// reserves slot 8 — is always current), layered over a `WarmCodeCache` whose
// inner `Arc<RwLock<WarmCodeCacheInner>>` persists across all N blocks. The
// immutable `basic`/code RPC (the pair's account info + bytecode) fires once
// (cold) then hits the warm cache until the per-entry TTL expires; the
// mutable `storage` RPC fires every block (it is never cached — caching it
// would re-introduce stale-state divergence).
//
// Expected RPC counts (one `getReserves()` per block):
//   - Block 1 (cold):           1 basic + 1 storage  = 2 RPCs
//   - Blocks 2..=TTL+1 (warm):  0 basic + 1 storage  = 1 RPC/block
//   - Block TTL+2 (re-cold):    1 basic + 1 storage  = 2 RPCs  (TTL expiry)
//   - Total over N=TTL+2:       2 basic + N storage   = N+2 RPCs
//
// vs config-B-over-N-blocks (fresh `CacheDB` per block, NO warm-cache layer):
//   - Every block: 1 basic + 1 storage = 2 RPCs/block → 2N RPCs total.
//
// The win is the saved `basic` RPC on every block past the cold load — the
// row the `WarmCodeCache` layer exists to eliminate.

/// The `WarmCodeCache` per-entry TTL, in blocks. An entry loaded at block
/// ordinal 1 is fresh while `block - 1 <= TTL`; the first stale (re-cold)
/// block is ordinal `TTL + 2`. With the default 10, that's block 12.
const WARM_CACHE_TTL_BLOCKS: u64 = 10;

/// Number of consecutive mainnet blocks config D sweeps. Equals
/// `WARM_CACHE_TTL_BLOCKS + 2` so the run captures the cold load (block 1),
/// the warm run (blocks 2..=TTL+1), AND the TTL re-cold (block TTL+2 = last).
const WARM_CACHE_N_BLOCKS: usize = WARM_CACHE_TTL_BLOCKS as usize + 2;

/// Per-block RPC deltas for the multi-block configs.
#[derive(Clone, Copy)]
struct PerBlockRpc {
    /// 1-indexed ordinal within the run (block 1 = first).
    ordinal: usize,
    /// The actual mainnet block number.
    block_number: u64,
    /// `basic_ref` RPCs that reached the alloy layer this block.
    basic_rpcs: u64,
    /// `storage_ref` RPCs that reached the alloy layer this block.
    storage_rpcs: u64,
}

/// Aggregate over N blocks for the multi-block configs.
struct MultiBlockResult {
    per_block: Vec<PerBlockRpc>,
    total_basic: u64,
    total_storage: u64,
    #[expect(dead_code)]
    total_wall: Duration,
}

/// Config D — cross-block `WarmCodeCache`: per block, rebuild a fresh
/// `CacheDB`+EVM (correct — fresh storage each block) over the stack
/// `CacheDB<WarmCodeCache<FundedCallerDb<CountingRpcDb<WrapDatabaseAsync<AlloyDB>>>>>`.
/// The `WarmCodeCacheInner` arc persists across all N blocks.
fn run_config_warm_cache_multi_block(
    provider: &RootProvider,
    tx: &TxEnv,
    start_block: u64,
    n_blocks: usize,
    ttl_blocks: u64,
) -> MultiBlockResult {
    let counter = Arc::new(Counter::default());
    let warm_inner: Arc<parking_lot::RwLock<WarmCodeCacheInner>> =
        WarmCodeCacheInner::shared_with_ttl(ttl_blocks);
    let start = Instant::now();
    let mut per_block = Vec::with_capacity(n_blocks);
    for i in 0..n_blocks {
        let block_number = start_block + i as u64;
        let before_basic = counter.basic_rpcs();
        let before_storage = counter.storage_rpcs();
        let alloy_db = AlloyDB::new(provider.clone(), BlockId::number(block_number));
        let wrap_db = WrapDatabaseAsync::new(alloy_db).expect("multi-thread runtime");
        let counting_db = CountingRpcDb {
            inner: wrap_db,
            counter: counter.clone(),
        };
        let funded_db = FundedCallerDb { inner: counting_db };
        let warm_db = WarmCodeCache::with_owner(warm_inner.clone(), block_number, funded_db);
        let cache_db = CacheDB::new(warm_db);
        let mut evm = build_evm(cache_db);
        let _ = evm.transact(tx.clone());
        per_block.push(PerBlockRpc {
            ordinal: i + 1,
            block_number,
            basic_rpcs: counter.basic_rpcs() - before_basic,
            storage_rpcs: counter.storage_rpcs() - before_storage,
        });
    }
    MultiBlockResult {
        per_block,
        total_basic: counter.basic_rpcs(),
        total_storage: counter.storage_rpcs(),
        total_wall: start.elapsed(),
    }
}

/// Config-B-over-N-blocks comparator: the same per-block fresh-`CacheDB`
/// rebuild WITHOUT the `WarmCodeCache` layer. Correct (fresh storage each
/// block) but the `basic`/code RPC re-fires every block — the row config D
/// eliminates. This is the "no warm cache" baseline the `[warm-cache]`
/// summary reports the win against.
fn run_config_shared_cachedb_multi_block(
    provider: &RootProvider,
    tx: &TxEnv,
    start_block: u64,
    n_blocks: usize,
) -> MultiBlockResult {
    let counter = Arc::new(Counter::default());
    let start = Instant::now();
    let mut per_block = Vec::with_capacity(n_blocks);
    for i in 0..n_blocks {
        let block_number = start_block + i as u64;
        let before_basic = counter.basic_rpcs();
        let before_storage = counter.storage_rpcs();
        let alloy_db = AlloyDB::new(provider.clone(), BlockId::number(block_number));
        let wrap_db = WrapDatabaseAsync::new(alloy_db).expect("multi-thread runtime");
        let counting_db = CountingRpcDb {
            inner: wrap_db,
            counter: counter.clone(),
        };
        let funded_db = FundedCallerDb { inner: counting_db };
        let cache_db = CacheDB::new(funded_db);
        let mut evm = build_evm(cache_db);
        let _ = evm.transact(tx.clone());
        per_block.push(PerBlockRpc {
            ordinal: i + 1,
            block_number,
            basic_rpcs: counter.basic_rpcs() - before_basic,
            storage_rpcs: counter.storage_rpcs() - before_storage,
        });
    }
    MultiBlockResult {
        per_block,
        total_basic: counter.basic_rpcs(),
        total_storage: counter.storage_rpcs(),
        total_wall: start.elapsed(),
    }
}

/// Print the per-block RPC breakdown (basic-ref count, storage-ref count) so
/// the warm-cache hit (basic→0 from block 2) + the TTL re-cold (basic→1 at
/// the boundary) are visible in the output.
fn print_multi_block_breakdown(label: &str, r: &MultiBlockResult) {
    eprintln!();
    eprintln!("[{label}] per-block RPC breakdown:");
    eprintln!(
        "  {:>6} {:>14} {:>12} {:>14}",
        "block", "number", "basic RPCs", "storage RPCs"
    );
    for b in &r.per_block {
        eprintln!(
            "  {:>6} {:>14} {:>12} {:>14}",
            b.ordinal, b.block_number, b.basic_rpcs, b.storage_rpcs
        );
    }
    let total = r.total_basic + r.total_storage;
    eprintln!(
        "[{label}] total: {} basic + {} storage = {} RPCs over {} blocks",
        r.total_basic,
        r.total_storage,
        total,
        r.per_block.len()
    );
}

/// Assert the `WarmCodeCache` TTL boundary behavior — the test-style
/// acceptance assertion for config D (mirrors how the single-block configs
/// print their expected counts, but as a real `assert!` so a regression in
/// the warm-cache layer fails the example).
///
/// Expected shape (N = `ttl_blocks + 2`):
///   - block 1: cold load → `basic_rpcs == 1`
///   - blocks `2..=ttl_blocks+1`: warm hit → `basic_rpcs == 0`
///   - block `ttl_blocks+2`: TTL re-cold → `basic_rpcs == 1`
///   - total `basic_rpcs == 2`
///   - every block: `storage_rpcs == 1` (storage is never cached)
fn assert_warm_cache_ttl_boundary(r: &MultiBlockResult, ttl_blocks: u64) {
    let n = r.per_block.len();
    let ttl = ttl_blocks as usize;
    let first_stale_ordinal = ttl + 2; // 1-indexed
    assert_eq!(
        n, first_stale_ordinal,
        "config D run length must equal ttl_blocks + 2 to capture the TTL boundary"
    );
    // Block 1: cold load.
    assert_eq!(
        r.per_block[0].basic_rpcs, 1,
        "block 1 should cold-load basic (expected 1 basic RPC)"
    );
    // Blocks 2..=ttl_blocks+1: warm-cache hits (0 basic RPCs each).
    for b in &r.per_block[1..first_stale_ordinal - 1] {
        assert_eq!(
            b.basic_rpcs, 0,
            "block {} should hit the warm cache (expected 0 basic RPCs)",
            b.ordinal
        );
    }
    // Block ttl_blocks+2: TTL re-cold load.
    let stale = &r.per_block[first_stale_ordinal - 1];
    assert_eq!(
        stale.basic_rpcs, 1,
        "block {} (TTL boundary, loaded at 1) should re-cold-load basic (expected 1 basic RPC)",
        stale.ordinal
    );
    // Storage fires every block (never cached).
    for b in &r.per_block {
        assert_eq!(
            b.storage_rpcs, 1,
            "block {} should fire 1 storage RPC (storage is never cached)",
            b.ordinal
        );
    }
    assert_eq!(
        r.total_basic, 2,
        "config D should fire basic RPC exactly twice (cold load + TTL re-cold)"
    );
}
