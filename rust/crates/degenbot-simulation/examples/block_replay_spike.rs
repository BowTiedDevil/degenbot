#![expect(clippy::expect_used, clippy::print_stderr)]
//! Block-replay spike — measure the wall-clock delta between receiving a
//! fresh mainnet block and replaying ALL of its transactions through a
//! persistent `CacheDB<AlloyDB>` EVM (`transact_commit` per tx).
//!
//! # The question
//!
//! Can a `CacheDB` layered over an `AlloyDB` pinned at the parent block serve
//! as a faithful in-memory state that advances block-by-block — each block's
//! transactions `transact_commit`ted so their SSTOREs land in the `CacheDB`,
//! and the next block replays against the committed result? And what is the
//! wall-clock cost of replaying a full block's transactions locally vs the
//! ~12 s slot budget?
//!
//! # What this spike measures (small-scoped — ONE block)
//!
//! - Fetch the latest mainnet block WITH full transaction objects.
//! - Build one `CacheDB<CountingRpcDb<WrapDatabaseAsync<AlloyDB>>>`, pinned at
//!   the PARENT block (start-of-block-N state).
//! - Set the EVM block env to block N's header (number, timestamp, basefee,
//!   `beneficiary`, `gas_limit`, `prevrandao`) so `BLOCKHASH`/`TIMESTAMP`/`BASEFEE`/
//!   `COINBASE` opcodes replay faithfully.
//! - Replay every transaction in block N in order via `transact_commit` (each
//!   tx's SSTOREs + nonce bumps commit into the `CacheDB`, so later txs in the
//!   block see prior txs' effects — same as on-chain).
//! - Report: wall time (block-received → all-txs-replayed), RPC count (basic +
//!   storage cold loads the `CacheDB` forwarded), and a per-tx outcome tally
//!   (success / revert / halt / error).
//!
//! # What it does NOT do (out of scope, deliberately)
//!
//! - It does not loop new blocks as they arrive (one block is enough to prove
//!   feasibility + measure the per-block delta). The production shape would
//!   subscribe to newHeads + replay each; that's a follow-up if the delta is
//!   inside the slot budget.
//! - It does not verify the replayed post-state against the block's
//!   `stateRoot` (correctness of replay vs the consensus post-state). That's
//!   the real correctness gate for a chain-following fork — a follow-up if the
//!   delta measurement justifies it.
//!
//! # Known limitation: upstream revm 41.0.0 blob-fraction lag
//!
//! degenbot pins revm 41.0.0, which encodes only the Cancun (3,338,477) +
//! Prague (5,007,716) blob base-fee update fractions. The live mainnet chain
//! is on the Osaka execution hardfork with the BPO2 blob-parameter update
//! (EIP-7892) active, whose update fraction is `BPO2_BASE_UPDATE_FRACTION =
//! 11_684_671` — a constant `alloy-eips` carries but revm 41.0.0 does not
//! re-export. `block_env_from_header` hardcodes the BPO2 value; using revm's
//! shipped PRAGUE constant wrongly rejects every blob (EIP-4844) tx as
//! underpriced. Replace with the `BlobScheduleBlobParams`-derived constant
//! when degenbot bumps revm.
//!
//! # Run
//!
//! ```bash
//! RPC=http://host.containers.internal:8545 \
//! cargo run --release --example block_replay_spike --manifest-path rust/Cargo.toml
//! ```

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::too_many_lines
)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use alloy::consensus::{BlockHeader, Transaction as ConsensusTransaction};
use alloy::eips::{BlockId, BlockNumberOrTag, Typed2718};
use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::Block as AlloyBlock;

use revm::context::{BlockEnv, TxEnv};
use revm::context_interface::either::Either;
use revm::database::{AlloyDB, CacheDB};
use revm::database_interface::{DatabaseRef, WrapDatabaseAsync};
use revm::primitives::{TxKind, B256};
use revm::{ExecuteCommitEvm, ExecuteEvm, MainBuilder, MainContext};

/// Default local RPC (devcontainer convention — an anvil mainnet fork).
const RPC_URL_DEFAULT: &str = "http://host.containers.internal:8545";

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let rpc_url = std::env::var("RPC").unwrap_or_else(|_| RPC_URL_DEFAULT.to_string());
    eprintln!("RPC URL: {rpc_url}");

    let provider: alloy::providers::RootProvider =
        ProviderBuilder::default().connect_http(rpc_url.parse().expect("valid URL"));

    // ---- 1. Fetch the latest block WITH full transaction objects. ----------
    let fetch_start = Instant::now();
    let block: AlloyBlock = provider
        .get_block_by_number(BlockNumberOrTag::Latest)
        .full()
        .await
        .expect("RPC reachable: get_block_by_number failed")
        .expect("latest block exists");
    let fetch_elapsed = fetch_start.elapsed();
    let block_number = block.header.number();
    let parent_block = block_number.saturating_sub(1);
    let n_txs = block.transactions.len();
    eprintln!(
        "Block {block_number}: {n_txs} txs, fetched in {fetch_elapsed:?} (parent pin {parent_block})"
    );

    // ---- 2. Build the persistent CacheDB over AlloyDB pinned at parent. ----
    // State as of the START of block N (parent's end-state). Each tx's
    // transact_commit mutates this CacheDB; later txs see prior txs' effects.
    let counter = Arc::new(Counter::default());
    let alloy_db = AlloyDB::new(provider.clone(), BlockId::number(parent_block));
    let wrap_db = WrapDatabaseAsync::new(alloy_db).expect("multi-thread runtime");
    let counting_db = CountingRpcDb {
        inner: wrap_db,
        counter: counter.clone(),
    };
    let cache_db = CacheDB::new(counting_db);

    let mut ctx = revm::context::Context::mainnet();
    // Nonce check stays ON (faithful replay): each sender's nonce at parent
    // block matches their first tx; subsequent txs in this block see the
    // committed nonce bumps from prior txs, exactly as on-chain.
    ctx.cfg.disable_nonce_check = false;
    let mut evm = ctx.with_db(cache_db).build_mainnet();

    // ---- 3. Set the block env to block N's header. ------------------------
    evm.set_block(block_env_from_header(&block.header));

    // ---- 4. Replay every transaction in order via transact_commit. -------
    let txs = block.transactions.txns();
    let replay_start = Instant::now();
    let mut success = 0u64;
    let mut revert = 0u64;
    let mut halt = 0u64;
    let mut errors = 0u64;
    let mut first_outcome: Option<String> = None;
    for (i, tx) in txs.enumerate() {
        let tx_env = match tx_env_from_alloy_tx(tx) {
            Ok(env) => env,
            Err(why) => {
                eprintln!("[replay] tx {i}: skipping ({why})");
                errors += 1;
                continue;
            }
        };
        let res = evm.transact_commit(tx_env);
        if let Err(e) = &res {
            eprintln!("[replay] tx {i}: ERR {e}");
        }
        let outcome = match res {
            Ok(r) => match &r {
                revm::context_interface::result::ExecutionResult::Success { reason, .. } => {
                    success += 1;
                    format!("Success({reason:?})")
                }
                revm::context_interface::result::ExecutionResult::Revert { output, .. } => {
                    revert += 1;
                    format!("Revert({})", revm::primitives::hex::encode(output))
                }
                revm::context_interface::result::ExecutionResult::Halt { reason, .. } => {
                    halt += 1;
                    format!("Halt({reason})")
                }
            },
            Err(e) => {
                errors += 1;
                format!("Err({e})")
            }
        };
        if first_outcome.is_none() {
            first_outcome = Some(outcome);
        }
    }
    let replay_elapsed = replay_start.elapsed();

    // ---- 5. Report. --------------------------------------------------------
    eprintln!();
    eprintln!("=== Block-replay spike result ===");
    eprintln!("block            : {block_number}");
    eprintln!("txs in block     : {n_txs}");
    eprintln!("block fetch time : {fetch_elapsed:?}");
    eprintln!("replay wall time : {replay_elapsed:?}  (block-received → all-txs-replayed)");
    eprintln!(
        "  per-tx median  : {:.2} µs",
        if n_txs > 0 {
            replay_elapsed.as_nanos() as f64 / n_txs as f64 / 1_000.0
        } else {
            0.0
        }
    );
    eprintln!(
        "outcomes         : success={success} revert={revert} halt={halt} errors={errors} (first tx: {})",
        first_outcome.as_deref().unwrap_or("n/a")
    );
    eprintln!(
        "RPC cold loads   : {} basic + {} storage = {} total RPC round-trips",
        counter.basic_rpcs(),
        counter.storage_rpcs(),
        counter.basic_rpcs() + counter.storage_rpcs()
    );
    let slot_budget = std::time::Duration::from_secs(12);
    eprintln!(
        "slot budget (12s): {:.1}% used by replay",
        100.0 * replay_elapsed.as_secs_f64() / slot_budget.as_secs_f64()
    );
}

// =================================================================
// Block env lift: alloy RPC header → revm BlockEnv
// =================================================================

fn block_env_from_header(h: &alloy::rpc::types::Header) -> BlockEnv {
    let mut env = BlockEnv {
        number: U256::from(h.number()),
        beneficiary: h.beneficiary(),
        timestamp: U256::from(h.timestamp()),
        gas_limit: h.gas_limit(),
        basefee: h.base_fee_per_gas().unwrap_or(0),
        difficulty: U256::ZERO,
        prevrandao: h.mix_hash(),
        blob_excess_gas_and_price: None,
        slot_num: 0,
    };
    if let Some(excess) = h.excess_blob_gas() {
        // The live mainnet chain is on the Osaka execution hardfork with the
        // BPO2 blob-parameter update (EIP-7892) active, whose blob base-fee
        // update fraction is `BPO2_BASE_UPDATE_FRACTION = 11_684_671`. revm
        // 41.0.0 (degenbot's pinned revm) does NOT encode this — it only
        // carries Cancun (3,338,477) + Prague (5,007,716), and its
        // `new_with_spec` blindly picks the PRAGUE fraction for every fork
        // >= Prague. Using PRAGUE computes a blob base fee of ~1.3e16 wei
        // (about 10,000,000× the real ~8M wei), which wrongly rejects every
        // blob (EIP-4844) tx as "blob gas price exceeds max fee per blob gas."
        // The constant below is the EIP-7892 BPO2 value (verified against
        // `alloy_eips::eip7892::BPO2_BASE_UPDATE_FRACTION` + the live reth
        // node's `eth_blobBaseFee`). Replace with the proper SpecId/
        // BlobScheduleBlobParams-derived constant when degenbot bumps revm.
        const OSAKA_BPO2_BLOB_BASE_FEE_UPDATE_FRACTION: u64 = 11_684_671;
        env.set_blob_excess_gas_and_price(excess, OSAKA_BPO2_BLOB_BASE_FEE_UPDATE_FRACTION);
    }
    env
}

// =================================================================
// TxEnv lift: alloy RPC Transaction → revm TxEnv
// =================================================================

fn tx_env_from_alloy_tx(tx: &alloy::rpc::types::Transaction) -> Result<TxEnv, String> {
    let caller = tx.inner.signer();
    let kind = tx.inner.to().map_or(TxKind::Create, TxKind::Call);
    let mut builder = TxEnv::builder()
        .caller(caller)
        .kind(kind)
        .value(tx.inner.value())
        .data(tx.inner.input().clone())
        .gas_limit(tx.inner.gas_limit())
        .gas_price(tx.inner.max_fee_per_gas())
        .nonce(tx.inner.nonce());
    if let Some(priority) = tx.inner.max_priority_fee_per_gas() {
        builder = builder.gas_priority_fee(Some(priority));
    }
    if let Some(chain_id) = tx.inner.chain_id() {
        builder = builder.chain_id(Some(chain_id));
    }
    if let Some(access_list) = tx.inner.access_list() {
        builder = builder.access_list(access_list.clone());
    }
    if let Some(blobs) = tx.inner.blob_versioned_hashes() {
        builder = builder.blob_hashes(blobs.to_vec());
    }
    if let Some(blob_fee) = tx.inner.max_fee_per_blob_gas() {
        builder = builder.max_fee_per_blob_gas(blob_fee);
    }
    let mut env = builder.build().map_err(|e| format!("TxEnv build: {e:?}"))?;
    env.tx_type = tx.inner.ty();
    if let Some(auth_list) = tx.inner.authorization_list() {
        env.authorization_list = auth_list.iter().cloned().map(Either::Left).collect();
    }
    Ok(env)
}

// =================================================================
// RPC counter + counting DB wrapper (mirrors rpc_cache_fanout.rs)
// =================================================================

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

struct CountingRpcDb<Db: DatabaseRef> {
    inner: Db,
    counter: Arc<Counter>,
}

impl<Db: DatabaseRef> DatabaseRef for CountingRpcDb<Db> {
    type Error = Db::Error;

    fn storage_ref(
        &self,
        address: Address,
        slot: revm::primitives::StorageKey,
    ) -> Result<revm::primitives::StorageValue, Self::Error> {
        self.counter.bump_storage();
        self.inner.storage_ref(address, slot)
    }

    fn basic_ref(&self, address: Address) -> Result<Option<revm::state::AccountInfo>, Self::Error> {
        self.counter.bump_basic();
        self.inner.basic_ref(address)
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<revm::bytecode::Bytecode, Self::Error> {
        self.inner.code_by_hash_ref(code_hash)
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        self.inner.block_hash_ref(number)
    }
}
