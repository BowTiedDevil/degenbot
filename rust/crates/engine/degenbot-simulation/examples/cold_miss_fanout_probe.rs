#![expect(
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::print_stdout
)]

//! The cold-miss cost + coroutine fan-out probe (phase-1 investigation for the
//! evm2 coroutine-fan-out chunk).
//!
//! # Question
//!
//! Where do cold storage misses cost wall time in the evm2 engine today, and
//! does evm2's coroutine seam (`Evm::transact_async` + the `AsyncDatabase`
//! adapter, `corosensei` fibers) let N pending frames overlap them?
//!
//! # Method
//!
//! A deterministic offline RPC stand-in: [`LatencyDb`] / [`FanOutDb`] serve the
//! corpus's settled state from a snapshot and charge `COLD_MISS_LATENCY_US`
//! (default 200 us) per modeled RPC round trip, so the measurement is a pure
//! function of the cold-read count — no network, no jitter. An account miss is
//! charged ONE round trip: the production adapter (`AlloyAsyncDb::get_account`)
//! issues `eth_getBalance` + `eth_getTransactionCount` + `eth_getCode` as one
//! JSON-RPC batch (`AlloyProvider::get_account_at`).
//!
//! # Legs
//!
//! 1. one executor frame, cold (fresh `CacheDB`) vs warm (reused `CacheDB`);
//! 2. the 7-call settlement bundle (3 balance reads, `execute()`, 3 balance
//!    reads) as one journaled sequence, cold vs warm;
//! 3. the strategy's per-candidate loop over ONE shared `CacheDB` (the
//!    production shape: candidate 1 pays the cold chain, 2..N hit the cache),
//!    against N independent `CacheDB`s;
//! 4. the coroutine fan-out: N frames driven as fibers off one shared
//!    cold-miss cache, one batched RPC round trip per scheduler round, over a
//!    DISJOINT corpus (each frame reads its own caller, its own token account
//!    and its own balance slot), against the serial shapes it would replace;
//! 5. the same fan-out over N copies of the SAME executor frame — the lockstep
//!    bound, where every read is shared and the cold set is one dependency
//!    chain;
//! 6. the fiber overhead alone (the fan-out re-driven with every answer
//!    already cached), for N and for one frame;
//! 7. steady state: the same frame with the production cross-block
//!    `WarmCodeCache` in front (contract accounts + code are TTL-cached across
//!    blocks; EOA rows and storage always forward);
//! 8. two fan-outs at ONE read anchor over the session's shared per-anchor
//!    storage memo (the shape the arbitrage dispatch moved to): the second
//!    fan-out pays no storage forwards, against the per-fan-out scope;
//! 9. invalidation: the anchor moves -> a fresh memo, and a value that changed
//!    at the new anchor is served rather than shadowed by the retired memo;
//! 10. verdict identity between the cold and zero-latency paths.
//!
//! Usage:
//!
//! ```sh
//! cargo run --release -p degenbot-simulation --example cold_miss_fanout_probe
//! ```
//!
//! `COLD_MISS_LATENCY_US` overrides the modeled per-round-trip latency;
//! `COLD_MISS_FRAMES` sets N (default 8).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use alloy::primitives::{keccak256, Address, Bytes, TxKind, B256, U256};
use alloy::sol_types::SolCall;

use ::evm2::bytecode::Bytecode;
use ::evm2::env::BlockEnvExt;
use ::evm2::ethereum::{ethereum_tx_registry, RecoveredTxEnvelope};
use ::evm2::evm::r#async::{AsyncDatabase, AsyncDb};
use ::evm2::evm::{AccountInfo, Cache, CacheDB, Database, Db, DynDatabase};
use ::evm2::interpreter::Word;
use ::evm2::{BaseEvmTypes, Evm, EvmFeatures, ExecutionConfig, Precompiles, Version};
use alloy::consensus::Transaction;

use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::handle::EVM2_PRODUCTION_SPEC;
use degenbot_simulation::sim::evm2::session::{Evm2CaptureMode, Evm2SimSession, Evm2SimStep};
use degenbot_simulation::sim::evm2::warm_code_cache::{WarmCodeCache, WarmCodeCacheInner};
use degenbot_simulation::sim::hot_slots::HOT_SLOT_DECAY_BLOCKS;
use degenbot_simulation::sim::prefetch::{
    path_prefetch_slots, prefetch_paths_into_memo, prewarm_plan, FamilySlotLayout, LearnedSets,
    PrefetchFacts, V2SlotIndices,
};
use degenbot_simulation::sim::storage_memo::ReadAnchor;
use degenbot_simulation::{HotSlotStore, StorageMemo};

#[path = "shared/bench_corpus.rs"]
mod bench_corpus;

use bench_corpus::{
    bench_evm, build_corpus, deploy, envelope, BENCH_BASE_FEE, BENCH_BLOCK, DEPLOYER,
};

alloy::sol! {
    /// The harness token stub's funding + read surface.
    interface BenchToken {
        function mint(address to, uint256 amount) external;
        function balanceOf(address owner) external view returns (uint256);
    }
}

/// Modeled RPC round-trip latency (200 us: a local-node storage read; the
/// `sim_metrics` FAST/SLOW boundary treats >=100 us as "reached the
/// transport").
fn modeled_latency() -> Duration {
    std::env::var("COLD_MISS_LATENCY_US")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(Duration::from_micros(200), Duration::from_micros)
}

/// How many frames the fan-out legs drive.
fn frame_count() -> usize {
    std::env::var("COLD_MISS_FRAMES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 1)
        .unwrap_or(8)
}

// ── The snapshot: the corpus's settled state, the "chain" behind the stub ──

/// Per-address chain state: balance, nonce, code hash.
type AccountRow = (Word, u64, B256);

/// The corpus state the RPC stub serves.
#[derive(Clone, Debug, Default)]
struct Snapshot {
    accounts: HashMap<Address, AccountRow>,
    /// Code bytes by code hash (the chain's `eth_getCode`).
    code: HashMap<B256, Bytes>,
    storage: HashMap<(Address, Word), Word>,
    block_hashes: HashMap<U256, B256>,
}

impl Snapshot {
    fn of(cache: &Cache) -> Self {
        let accounts = cache
            .accounts
            .iter()
            .filter_map(|(address, info)| {
                info.as_ref()
                    .map(|info| (*address, (info.balance, info.nonce, info.code_hash)))
            })
            .collect();
        let code = cache
            .contracts
            .iter()
            .map(|(hash, bytecode)| (*hash, bytecode.original_bytes()))
            .collect();
        let mut storage = HashMap::new();
        for (address, row) in &cache.storage {
            for (slot, value) in &row.slots {
                storage.insert((*address, *slot), *value);
            }
        }
        let block_hashes = cache.block_hashes.iter().map(|(n, h)| (*n, *h)).collect();
        Self {
            accounts,
            code,
            storage,
            block_hashes,
        }
    }

    fn account(&self, address: Address) -> Option<AccountRow> {
        self.accounts.get(&address).copied()
    }

    fn code_of(&self, address: Address) -> Bytes {
        self.account(address)
            .and_then(|(_, _, hash)| self.code.get(&hash).cloned())
            .unwrap_or_default()
    }

    /// The `AccountInfo` a full read returns: code inlined, exactly the
    /// production adapter's eager-load shape (an all-empty account reads as
    /// non-existent).
    fn info(&self, address: Address) -> Option<AccountInfo> {
        let (balance, nonce, code_hash) = self.account(address)?;
        let code = self.code.get(&code_hash).cloned().unwrap_or_default();
        if balance.is_zero() && nonce == 0 && code.is_empty() {
            return None;
        }
        Some(
            AccountInfo::default()
                .with_balance(balance)
                .with_nonce(nonce)
                .with_code(Bytecode::new_raw(code)),
        )
    }

    fn slot(&self, address: Address, key: Word) -> Word {
        self.storage
            .get(&(address, key))
            .copied()
            .unwrap_or(Word::ZERO)
    }
}

// ── The prefetch lane's chain: a snapshot-backed batch transport ──────────

/// One dispatched packet's members, as `(method, params)`.
type PrefetchPacket = Vec<(String, String)>;

/// The modeled node behind the prefetch lane: answers `eth_getStorageAt` from
/// the corpus snapshot at the pinned block, charging one modeled round trip per
/// dispatched packet (a batch is ONE) and recording each packet's member count
/// and pinned block tag.
#[derive(Clone)]
struct StorageTransport {
    snapshot: Arc<Snapshot>,
    latency: Duration,
    packets: Arc<Mutex<Vec<PrefetchPacket>>>,
}

impl StorageTransport {
    fn new(snapshot: Arc<Snapshot>, latency: Duration) -> Self {
        Self {
            snapshot,
            latency,
            packets: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn packets(&self) -> Vec<PrefetchPacket> {
        self.packets.lock().expect("packet recorder").clone()
    }

    fn provider(&self) -> AlloyProvider {
        let client = alloy::rpc::client::RpcClient::new(self.clone(), true);
        let root: alloy::providers::RootProvider = alloy::providers::RootProvider::new(client);
        AlloyProvider::from_provider(Arc::new(root))
    }
}

impl tower::Service<alloy::rpc::json_rpc::RequestPacket> for StorageTransport {
    type Response = alloy::rpc::json_rpc::ResponsePacket;
    type Error = alloy::transports::TransportError;
    type Future = alloy::transports::TransportFut<'static>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: alloy::rpc::json_rpc::RequestPacket) -> Self::Future {
        use alloy::rpc::json_rpc::{
            RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
        };
        let transport = self.clone();
        Box::pin(async move {
            let members = |req: &SerializedRequest| -> (String, String) {
                (
                    req.method().to_string(),
                    req.params()
                        .map_or_else(String::new, |p| p.get().to_string()),
                )
            };
            let answer = |req: &SerializedRequest| -> Response {
                let params: serde_json::Value = serde_json::from_str(
                    req.params().expect("a storage read carries params").get(),
                )
                .expect("params are JSON");
                let address = params[0].as_str().expect("the address param");
                let slot = params[1].as_str().expect("the slot param");
                let address = address
                    .parse::<Address>()
                    .expect("the node decodes the address it was asked for");
                let slot = U256::from_str_radix(slot.trim_start_matches("0x"), 16)
                    .expect("the slot is a hex quantity");
                let value = transport.snapshot.slot(address, slot);
                let result = serde_json::Value::String(format!("0x{value:064x}"));
                Response {
                    id: req.id().clone(),
                    payload: ResponsePayload::Success(
                        serde_json::value::to_raw_value(&result).expect("a JSON string"),
                    ),
                }
            };
            std::thread::sleep(transport.latency);
            Ok(match req {
                RequestPacket::Single(one) => {
                    transport
                        .packets
                        .lock()
                        .expect("packet recorder")
                        .push(vec![members(&one)]);
                    ResponsePacket::Single(answer(&one))
                }
                RequestPacket::Batch(many) => {
                    transport
                        .packets
                        .lock()
                        .expect("packet recorder")
                        .push(many.iter().map(members).collect());
                    ResponsePacket::Batch(many.iter().map(answer).collect())
                }
            })
        })
    }
}

// ── The accounting ────────────────────────────────────────────────────────

/// Cold-path accounting: forwarded account reads (1 batched RPC round trip
/// each) and storage reads (1 round trip each).
#[derive(Debug, Default)]
struct Counters {
    account_reads: AtomicU64,
    storage_reads: AtomicU64,
    /// Every distinct forwarded read, in first-touch order (the cold-set
    /// listing).
    keys: Mutex<Vec<String>>,
}

impl Counters {
    fn snapshot(&self) -> (u64, u64) {
        (
            self.account_reads.load(Ordering::Relaxed),
            self.storage_reads.load(Ordering::Relaxed),
        )
    }

    fn rpc_calls(counts: (u64, u64)) -> u64 {
        counts.0 + counts.1
    }

    fn record_account(&self, address: Address) {
        self.account_reads.fetch_add(1, Ordering::Relaxed);
        self.record_key(format!("account {}", short(address)));
    }

    fn record_storage(&self, address: Address, key: Word) {
        self.storage_reads.fetch_add(1, Ordering::Relaxed);
        self.record_key(format!("slot    {}[{key:#x}]", short(address)));
    }

    fn record_key(&self, key: String) {
        let mut keys = self.keys.lock().expect("key recorder");
        if !keys.contains(&key) {
            keys.push(key);
        }
    }

    fn key_listing(&self) -> Vec<String> {
        self.keys.lock().expect("key recorder").clone()
    }
}

// ── The blocking stand-in (production's `BlockingAsyncDb` shape) ──────────

/// A blocking cold path: every forwarded read sleeps one modeled round trip
/// (the production adapter reads the account triple in one JSON-RPC batch).
struct LatencyDb {
    snapshot: Arc<Snapshot>,
    latency: Duration,
    counters: Arc<Counters>,
}

impl Database for LatencyDb {
    type Error = std::convert::Infallible;

    fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.counters.record_account(*address);
        std::thread::sleep(self.latency);
        Ok(self.snapshot.info(*address))
    }

    fn get_code_by_hash(&mut self, _code_hash: &B256) -> Result<Bytecode, Self::Error> {
        panic!("the adapter eager-loads code — a code-by-hash read never happens")
    }

    fn get_storage(&mut self, address: &Address, key: &Word) -> Result<Word, Self::Error> {
        self.counters.record_storage(*address, *key);
        std::thread::sleep(self.latency);
        Ok(self.snapshot.slot(*address, *key))
    }

    fn get_block_hash(&mut self, number: &Word) -> Result<B256, Self::Error> {
        self.counters.record_storage(Address::ZERO, *number);
        std::thread::sleep(self.latency);
        Ok(self
            .snapshot
            .block_hashes
            .get(number)
            .copied()
            .unwrap_or(B256::ZERO))
    }
}

// ── The async stand-in + the fan-out scheduler ────────────────────────────

/// One modeled RPC read.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ReadKey {
    Balance(Address),
    Nonce(Address),
    Code(Address),
    ChainCode(B256),
    Storage(Address, Word),
    BlockHash(U256),
}

/// One modeled RPC answer.
#[derive(Clone, Debug)]
enum Answer {
    Word(Word),
    Code(Bytes),
    BlockHash(B256),
}

impl Answer {
    fn word(&self) -> Word {
        match self {
            Self::Word(w) => *w,
            _ => panic!("answer is not a word"),
        }
    }

    fn code(&self) -> Bytes {
        match self {
            Self::Code(c) => c.clone(),
            _ => panic!("answer is not code"),
        }
    }

    fn block_hash(&self) -> B256 {
        match self {
            Self::BlockHash(h) => *h,
            _ => panic!("answer is not a block hash"),
        }
    }
}

/// The shared cold-miss cache + batcher: one queue per scheduler round, one
/// answer map for the whole fan-out (the dedup seam — production's
/// `StorageMemo` is the same idea, but it cannot dedup an *in-flight* read).
#[derive(Debug, Default)]
struct BatchState {
    queued: Vec<ReadKey>,
    answers: HashMap<ReadKey, Answer>,
    waker: Option<Waker>,
    rounds: u64,
    batch_sizes: Vec<usize>,
    rpc_calls: u64,
    dedup: u64,
}

struct FanOutCtx {
    snapshot: Arc<Snapshot>,
    latency: Duration,
    state: Mutex<BatchState>,
}

impl FanOutCtx {
    fn new(snapshot: Arc<Snapshot>, latency: Duration) -> Self {
        Self {
            snapshot,
            latency,
            state: Mutex::new(BatchState::default()),
        }
    }

    fn resolve(&self, key: &ReadKey) -> Answer {
        match key {
            ReadKey::Balance(a) => {
                Answer::Word(self.snapshot.account(*a).map_or(Word::ZERO, |r| r.0))
            }
            ReadKey::Nonce(a) => {
                Answer::Word(Word::from(self.snapshot.account(*a).map_or(0, |r| r.1)))
            }
            ReadKey::Code(a) => Answer::Code(self.snapshot.code_of(*a)),
            ReadKey::ChainCode(hash) => {
                Answer::Code(self.snapshot.code.get(hash).cloned().unwrap_or_default())
            }
            ReadKey::Storage(a, k) => Answer::Word(self.snapshot.slot(*a, *k)),
            ReadKey::BlockHash(n) => Answer::BlockHash(
                self.snapshot
                    .block_hashes
                    .get(n)
                    .copied()
                    .unwrap_or(B256::ZERO),
            ),
        }
    }
}

/// One pending read: resolved by the scheduler's batch, never by itself.
struct ReadReq {
    ctx: Arc<FanOutCtx>,
    key: ReadKey,
}

impl Future for ReadReq {
    type Output = Answer;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Answer> {
        let mut state = self.ctx.state.lock().expect("batch state");
        if let Some(answer) = state.answers.get(&self.key) {
            return Poll::Ready(answer.clone());
        }
        if state.queued.contains(&self.key) {
            state.dedup += 1;
        } else {
            state.queued.push(self.key.clone());
        }
        state.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

/// The `AsyncDatabase` over the batcher — three serial round trips per account,
/// one per storage read (the production adapter's verb shape).
struct FanOutDb {
    ctx: Arc<FanOutCtx>,
}

impl FanOutDb {
    async fn read(&self, key: ReadKey) -> Answer {
        ReadReq {
            ctx: Arc::clone(&self.ctx),
            key,
        }
        .await
    }
}

impl AsyncDatabase for FanOutDb {
    type Error = std::convert::Infallible;

    async fn get_account(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        let balance = self.read(ReadKey::Balance(address)).await.word();
        let nonce = self.read(ReadKey::Nonce(address)).await.word();
        let code = self.read(ReadKey::Code(address)).await.code();
        if balance.is_zero() && nonce.is_zero() && code.is_empty() {
            return Ok(None);
        }
        Ok(Some(
            AccountInfo::default()
                .with_balance(balance)
                .with_nonce(u64::try_from(nonce).unwrap_or(u64::MAX))
                .with_code(Bytecode::new_raw(code)),
        ))
    }

    async fn get_code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        Ok(Bytecode::new_raw(
            self.read(ReadKey::ChainCode(code_hash)).await.code(),
        ))
    }

    async fn get_storage(&mut self, address: Address, key: Word) -> Result<Word, Self::Error> {
        Ok(self.read(ReadKey::Storage(address, key)).await.word())
    }

    async fn get_block_hash(&mut self, number: Word) -> Result<B256, Self::Error> {
        Ok(self.read(ReadKey::BlockHash(number)).await.block_hash())
    }
}

/// One frame's settled identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Verdict {
    status: bool,
    gas: u64,
    output: B256,
}

impl Verdict {
    fn of(result: &::evm2::evm::TxResult) -> Self {
        Self {
            status: result.status,
            gas: result.tx_gas_used(),
            output: keccak256(&result.output),
        }
    }
}

type FrameFuture = Pin<Box<dyn Future<Output = Result<Verdict, String>>>>;

/// What one fan-out drive measured.
#[derive(Debug, Default)]
struct FanOutReport {
    wall: Duration,
    rounds: u64,
    rpc_calls: u64,
    dedup: u64,
    batch_sizes: Vec<usize>,
    verdicts: Vec<Verdict>,
}

impl FanOutReport {
    fn max_batch(&self) -> usize {
        self.batch_sizes.iter().copied().max().unwrap_or(0)
    }
}

/// Drive `frames` as coroutines: poll every unfinished fiber once per round,
/// then settle the whole queued read set in ONE modeled round trip.
fn drive(ctx: &Arc<FanOutCtx>, frames: &mut [FrameFuture]) -> Result<FanOutReport, String> {
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut verdicts: Vec<Option<Verdict>> = vec![None; frames.len()];
    let mut remaining = frames.len();
    let started = Instant::now();
    let (rounds_before, rpc_before, dedup_before) = {
        let state = ctx.state.lock().expect("batch state");
        (state.rounds, state.rpc_calls, state.dedup)
    };
    while remaining > 0 {
        for (index, frame) in frames.iter_mut().enumerate() {
            if verdicts[index].is_some() {
                continue;
            }
            if let Poll::Ready(result) = frame.as_mut().poll(&mut cx) {
                verdicts[index] = Some(result?);
                remaining -= 1;
            }
        }
        if remaining == 0 {
            break;
        }
        let batch = {
            let mut state = ctx.state.lock().expect("batch state");
            std::mem::take(&mut state.queued)
        };
        if batch.is_empty() {
            return Err("deadlock: every frame is blocked with no queued read".into());
        }
        // ONE modeled RPC round trip for the whole batch.
        std::thread::sleep(ctx.latency);
        {
            let mut state = ctx.state.lock().expect("batch state");
            state.rounds += 1;
            state.batch_sizes.push(batch.len());
            state.rpc_calls += u64::try_from(batch.len()).unwrap_or(u64::MAX);
            for key in &batch {
                state.answers.insert(key.clone(), ctx.resolve(key));
            }
            if let Some(waker) = &state.waker {
                waker.wake_by_ref();
            }
        }
    }
    let wall = started.elapsed();
    let state = ctx.state.lock().expect("batch state");
    Ok(FanOutReport {
        wall,
        rounds: state.rounds - rounds_before,
        rpc_calls: state.rpc_calls - rpc_before,
        dedup: state.dedup - dedup_before,
        batch_sizes: state
            .batch_sizes
            .iter()
            .copied()
            .skip(usize::try_from(rounds_before).unwrap_or(usize::MAX))
            .collect(),
        verdicts: verdicts.into_iter().flatten().collect(),
    })
}

// ── EVM construction ─────────────────────────────────────────────────────

fn block_env() -> BlockEnvExt {
    block_env_at(BENCH_BLOCK)
}

/// The corpus block env pinned to an arbitrary height — a capture's learning
/// anchor and the fan-out's read anchor are different heights by construction.
fn block_env_at(number: u64) -> BlockEnvExt {
    BlockEnvExt {
        basefee: U256::from(BENCH_BASE_FEE),
        number: U256::from(number),
        ..Default::default()
    }
}

/// The production frame policy: `EVM2_PRODUCTION_SPEC`, the nonce check off
/// (each bundle call is `eth_call`-shaped), no inspector (so the interpreter
/// cost matches `sim_mode_bench`'s `interp` row).
fn policy_evm<'a>(db: impl DynDatabase + 'a) -> Evm<'a, BaseEvmTypes> {
    let mut version = Version::new(EVM2_PRODUCTION_SPEC);
    version.features.remove(EvmFeatures::NONCE_CHECK);
    Evm::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(EVM2_PRODUCTION_SPEC, version),
        EVM2_PRODUCTION_SPEC,
        block_env(),
        ethereum_tx_registry(EVM2_PRODUCTION_SPEC),
        db,
        Precompiles::base(EVM2_PRODUCTION_SPEC),
    )
}

fn latency_db(snapshot: &Arc<Snapshot>, latency: Duration, counters: &Arc<Counters>) -> LatencyDb {
    LatencyDb {
        snapshot: Arc::clone(snapshot),
        latency,
        counters: Arc::clone(counters),
    }
}

/// The production memo seam (`BotStateDb::get_storage`) in front of the RPC
/// stub: consult the anchor's memo, else forward and learn the answer. Accounts,
/// code and block hashes forward untouched — the memo covers storage only.
struct MemoDb {
    cold: LatencyDb,
    memo: Arc<StorageMemo>,
}

impl Database for MemoDb {
    type Error = std::convert::Infallible;

    fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.cold.get_account(address)
    }

    fn get_code_by_hash(&mut self, code_hash: &B256) -> Result<Bytecode, Self::Error> {
        self.cold.get_code_by_hash(code_hash)
    }

    fn get_storage(&mut self, address: &Address, key: &Word) -> Result<Word, Self::Error> {
        if let Some(value) = self.memo.get(*address, *key) {
            return Ok(value);
        }
        let value = self.cold.get_storage(address, key)?;
        self.memo.put(*address, *key, value);
        Ok(value)
    }

    fn get_block_hash(&mut self, number: &Word) -> Result<B256, Self::Error> {
        self.cold.get_block_hash(number)
    }
}

/// One fan-out per frame at the read anchor `block`, sharing the session's
/// read caches: the cross-block `WarmCodeCacheInner` AND — through its
/// per-anchor slot — the storage memo. Each frame still builds its own
/// `CacheDB`/warm-cache view, exactly as each dispatch builds its own handle.
fn run_sync_series_anchored(
    snapshot: &Arc<Snapshot>,
    frames: &[RecoveredTxEnvelope],
    latency: Duration,
    block: u64,
    session: &Arc<parking_lot::RwLock<WarmCodeCacheInner>>,
) -> Result<Vec<SyncRun>, String> {
    let counters = Arc::new(Counters::default());
    let mut runs = Vec::with_capacity(frames.len());
    for frame in frames {
        // The session-scoped slot: same anchor, same memo (the sharing); a
        // moved anchor, a fresh one (the invalidation).
        let memo = {
            let cache = session.read();
            cache.storage_memo_for(ReadAnchor::at(block))
        };
        let cold = MemoDb {
            cold: latency_db(snapshot, latency, &counters),
            memo,
        };
        let warm = WarmCodeCache::with_owner(Arc::clone(session), block, cold);
        let mut evm: Evm<'static, BaseEvmTypes> = policy_evm(CacheDB::new(Db::new(warm)));
        let before = counters.snapshot();
        let started = Instant::now();
        let executed = evm
            .transact(frame)
            .map_err(|e| format!("sync frame: {e:?}"))?;
        let result = executed.discard();
        let wall = started.elapsed();
        let after = counters.snapshot();
        let counts = (after.0 - before.0, after.1 - before.1);
        runs.push(SyncRun {
            wall,
            accounts: counts.0,
            storage: counts.1,
            round_trips: Counters::rpc_calls(counts),
            verdict: Verdict::of(&result),
        });
    }
    Ok(runs)
}

/// A copy of `snapshot` with every storage value bumped by one — the "the chain
/// answers differently at the new anchor" stand-in (the stub's `balanceOf`
/// returndata IS the slot value, so the settled verdict moves with it).
fn bumped(snapshot: &Snapshot) -> Snapshot {
    let mut copy = snapshot.clone();
    for value in copy.storage.values_mut() {
        *value = value.wrapping_add(Word::from(1u64));
    }
    copy
}

// ── The legs ─────────────────────────────────────────────────────────────

/// One blocking run's measurement.
#[derive(Debug)]
struct SyncRun {
    wall: Duration,
    accounts: u64,
    storage: u64,
    /// Modeled RPC round trips (one per account read — the batched triple —
    /// plus one per storage read).
    round_trips: u64,
    verdict: Verdict,
}

/// Run `frames` serially over a blocking cold path. `fresh_per_frame` gives
/// each frame its own `CacheDB` (the retired per-path shape); otherwise all
/// frames share one (the production fan-out shape).
fn run_sync_series(
    snapshot: &Arc<Snapshot>,
    frames: &[RecoveredTxEnvelope],
    latency: Duration,
    fresh_per_frame: bool,
) -> Result<Vec<SyncRun>, String> {
    let counters = Arc::new(Counters::default());
    let build = || {
        policy_evm(CacheDB::new(Db::new(latency_db(
            snapshot, latency, &counters,
        ))))
    };
    let mut evm: Evm<'static, BaseEvmTypes> = build();
    let mut runs = Vec::with_capacity(frames.len());
    for frame in frames {
        if fresh_per_frame {
            evm = build();
        }
        let before = counters.snapshot();
        let started = Instant::now();
        let executed = evm
            .transact(frame)
            .map_err(|e| format!("sync frame: {e:?}"))?;
        let result = executed.discard();
        let wall = started.elapsed();
        let after = counters.snapshot();
        let counts = (after.0 - before.0, after.1 - before.1);
        runs.push(SyncRun {
            wall,
            accounts: counts.0,
            storage: counts.1,
            round_trips: Counters::rpc_calls(counts),
            verdict: Verdict::of(&result),
        });
    }
    Ok(runs)
}

/// The same series with the production cross-block `WarmCodeCache` in front of
/// the cold path (one shared inner across calls — the engine owner's shape).
fn run_sync_series_warm_cache(
    snapshot: &Arc<Snapshot>,
    frames: &[RecoveredTxEnvelope],
    latency: Duration,
    block: u64,
    inner: &Arc<parking_lot::RwLock<WarmCodeCacheInner>>,
) -> Result<Vec<SyncRun>, String> {
    let counters = Arc::new(Counters::default());
    let cold = latency_db(snapshot, latency, &counters);
    let warm = WarmCodeCache::with_owner(Arc::clone(inner), block, cold);
    let mut evm: Evm<'static, BaseEvmTypes> = policy_evm(CacheDB::new(Db::new(warm)));
    let mut runs = Vec::with_capacity(frames.len());
    for frame in frames {
        let before = counters.snapshot();
        let started = Instant::now();
        let executed = evm
            .transact(frame)
            .map_err(|e| format!("sync frame: {e:?}"))?;
        let result = executed.discard();
        let wall = started.elapsed();
        let after = counters.snapshot();
        let counts = (after.0 - before.0, after.1 - before.1);
        runs.push(SyncRun {
            wall,
            accounts: counts.0,
            storage: counts.1,
            round_trips: Counters::rpc_calls(counts),
            verdict: Verdict::of(&result),
        });
    }
    Ok(runs)
}

/// The 7-call settlement bundle as one journaled sequence over a blocking cold
/// path.
fn run_bundle_sync(
    snapshot: &Arc<Snapshot>,
    steps: &[Evm2SimStep],
    latency: Duration,
) -> Result<SyncRun, String> {
    let counters = Arc::new(Counters::default());
    let mut evm = policy_evm(CacheDB::new(Db::new(latency_db(
        snapshot, latency, &counters,
    ))));
    let mut session = Evm2SimSession::with_block_env(&mut evm, block_env());
    let started = Instant::now();
    let sequence = session
        .run_sequence(steps, Evm2CaptureMode::Seam)
        .map_err(|e| format!("bundle sequence: {e}"))?;
    let wall = started.elapsed();
    let (accounts, storage) = counters.snapshot();
    let execute = sequence
        .calls
        .get(3)
        .ok_or("the bundle lost its execute step")?;
    Ok(SyncRun {
        wall,
        accounts,
        storage,
        round_trips: Counters::rpc_calls((accounts, storage)),
        verdict: Verdict {
            status: matches!(
                execute.verdict,
                degenbot_simulation::sim::RunVerdict::Success
            ),
            gas: execute.gas_used,
            output: keccak256(&execute.output),
        },
    })
}

/// Build the N coroutine frames: each on its own leaked EVM (`transact_async`
/// needs `&mut Evm`, and the fiber stack lives on the EVM) over its own fresh
/// `CacheDB`, all sharing one cold-miss batcher.
fn build_fan_out(
    ctx: &Arc<FanOutCtx>,
    frames: &[&'static RecoveredTxEnvelope],
) -> Vec<FrameFuture> {
    frames
        .iter()
        .map(|frame| {
            let db = CacheDB::new(AsyncDb::new(FanOutDb {
                ctx: Arc::clone(ctx),
            }));
            let evm: &'static mut Evm<'static, BaseEvmTypes> = Box::leak(Box::new(policy_evm(db)));
            let frame: &'static RecoveredTxEnvelope = frame;
            let future: FrameFuture = Box::pin(async move {
                let executed = evm
                    .transact_async(frame)
                    .await
                    .map_err(|e| format!("async transact: {e}"))?;
                let result = executed.discard();
                Ok(Verdict::of(&result))
            });
            future
        })
        .collect()
}

fn leak(frame: RecoveredTxEnvelope) -> &'static RecoveredTxEnvelope {
    Box::leak(Box::new(frame))
}

fn us(duration: Duration) -> u128 {
    duration.as_micros()
}

/// The last three address bytes — enough to read a cold-set listing.
fn short(address: Address) -> String {
    let tail = &address.0[17..];
    format!("0x{:02x}{:02x}{:02x}", tail[0], tail[1], tail[2])
}

/// The distinct cold read set of ONE frame (fresh `CacheDB`, no memo).
fn profile_cold_reads(
    snapshot: &Arc<Snapshot>,
    frame: &RecoveredTxEnvelope,
    latency: Duration,
) -> Result<(SyncRun, Vec<String>), String> {
    let counters = Arc::new(Counters::default());
    let mut evm = policy_evm(CacheDB::new(Db::new(latency_db(
        snapshot, latency, &counters,
    ))));
    let started = Instant::now();
    let executed = evm
        .transact(frame)
        .map_err(|e| format!("profile frame: {e:?}"))?;
    let result = executed.discard();
    let wall = started.elapsed();
    let (accounts, storage) = counters.snapshot();
    Ok((
        SyncRun {
            wall,
            accounts,
            storage,
            round_trips: Counters::rpc_calls((accounts, storage)),
            verdict: Verdict::of(&result),
        },
        counters.key_listing(),
    ))
}

#[expect(clippy::too_many_lines)] // the probe's leg sequence reads top-down
fn main() {
    let latency = modeled_latency();
    let n = frame_count();
    let frames_label = n;

    // ── setup: the corpus, plus a DISJOINT view corpus ────────────────────
    let mut setup = bench_evm(false);
    let (corpus, corpus_path) = match build_corpus(&mut setup) {
        Ok(built) => built,
        Err(e) => {
            println!("corpus build failed: {e}");
            return;
        }
    };
    let Some(executor) = corpus.frame.inner().to() else {
        println!("the corpus frame must call the executor");
        return;
    };
    // N disjoint view frames: each reads its own token (account + code) and its
    // own `balances[user]` slot — the maximum-overlap fan-out corpus.
    let mut users = Vec::with_capacity(n);
    let mut disjoint_frames = Vec::with_capacity(n);
    let mut nonce = 1_000u64;
    for index in 0..n {
        let creation = load_stub_creation("Token");
        let token = match deploy(&mut setup, Bytes::from(creation), nonce) {
            Ok(deployed) => deployed,
            Err(e) => {
                println!("stub deploy failed: {e}");
                return;
            }
        };
        nonce += 1;
        let mut user_bytes = [0u8; 20];
        user_bytes[0] = 0x77;
        user_bytes[19] = u8::try_from(index + 1).unwrap_or(1);
        let user = Address::new(user_bytes);
        if let Err(e) = mint(
            &mut setup,
            token.address,
            user,
            U256::from(1_000 + index as u64),
            nonce,
        ) {
            println!("  FAILED: {e}");
            return;
        }
        nonce += 1;
        users.push(user);
        disjoint_frames.push(envelope(
            user,
            TxKind::Call(token.address),
            Bytes::from(BenchToken::balanceOfCall { owner: user }.abi_encode()),
            100_000,
            0,
        ));
    }

    // N copies of the SAME executor frame: the lockstep/dedup corpus (every
    // read is shared, so the whole cold chain is one dependency chain).
    let same_frames: Vec<RecoveredTxEnvelope> = (0..n).map(|_| corpus.frame.clone()).collect();

    let mut snapshot = Snapshot::of(&setup.state_mut().overlay_db().cache);
    // The settle-time funding the production overrides supply: every frame's
    // caller must cover `gas_limit * max_fee` under the balance check.
    for caller in &users {
        snapshot.accounts.insert(
            *caller,
            (U256::from(u128::MAX), 0, alloy::primitives::KECCAK256_EMPTY),
        );
    }
    let snapshot = Arc::new(snapshot);
    println!(
        "cold_miss_fanout_probe: {} frames, modeled round trip {} us, snapshot {} accounts / {} code / {} slots",
        frames_label,
        us(latency),
        snapshot.accounts.len(),
        snapshot.code.len(),
        snapshot.storage.len(),
    );

    // ── leg 1: one executor frame, cold vs warm ───────────────────────────
    println!("\n[1] ONE executor frame (the 7-call bundle's execute() step)");
    match run_sync_series(
        &snapshot,
        std::slice::from_ref(&corpus.frame),
        latency,
        false,
    ) {
        Ok(runs) => report_series("cold (fresh CacheDB)", &runs),
        Err(e) => println!("  FAILED: {e}"),
    }
    match profile_cold_reads(&snapshot, &corpus.frame, latency) {
        Ok((_, keys)) => {
            println!("  distinct cold set ({} keys):", keys.len());
            for key in &keys {
                println!("    {key}");
            }
        }
        Err(e) => println!("  FAILED: {e}"),
    }
    match run_sync_series(
        &snapshot,
        &[
            corpus.frame.clone(),
            corpus.frame.clone(),
            corpus.frame.clone(),
        ],
        latency,
        false,
    ) {
        Ok(runs) => {
            report_series("run 1 of a shared CacheDB", &runs[..1]);
            report_series("runs 2-3 (warm)", &runs[1..]);
        }
        Err(e) => println!("  FAILED: {e}"),
    }

    // ── leg 2: the 7-call bundle, cold vs warm ────────────────────────────
    println!("\n[2] the 7-call settlement bundle (3 balance reads, execute(), 3 balance reads)");
    let weth = corpus.contracts.get(3).map(|c| c.address);
    let token = corpus.contracts.get(4).map(|c| c.address);
    let (Some(weth), Some(token)) = (weth, token) else {
        println!("  FAILED: corpus contract order changed");
        return;
    };
    let codeless = Address::new([0x11; 20]);
    let balance_step = |to: Address| {
        Evm2SimStep::Transact(envelope(
            DEPLOYER,
            TxKind::Call(to),
            Bytes::from(BenchToken::balanceOfCall { owner: executor }.abi_encode()),
            100_000,
            0,
        ))
    };
    let steps = vec![
        balance_step(weth),
        balance_step(codeless),
        balance_step(token),
        Evm2SimStep::Inspect(corpus.frame.clone()),
        balance_step(weth),
        balance_step(codeless),
        balance_step(token),
    ];
    match run_bundle_sync(&snapshot, &steps, latency) {
        Ok(run) => report_bundle("cold (fresh CacheDB)", &run),
        Err(e) => println!("  FAILED: {e}"),
    }
    // Warm: rerun the same bundle over a cache warmed by the cold pass.
    match warm_bundle(&snapshot, &steps, latency) {
        Ok((_cold, warm)) => report_bundle("warm (reused CacheDB)", &warm),
        Err(e) => println!("  FAILED: {e}"),
    }

    // ── leg 3: the per-candidate loop over one shared CacheDB ─────────────
    println!("\n[3] per-candidate loop, {frames_label} candidates");
    let candidates: Vec<RecoveredTxEnvelope> = (0..n).map(|_| corpus.frame.clone()).collect();
    if let Ok(runs) = run_sync_series(&snapshot, &candidates, latency, false) {
        report_series("shared CacheDB (production shape)", &runs);
        let total: Duration = runs.iter().map(|r| r.wall).sum();
        let first = runs.first().map_or(Duration::ZERO, |r| r.wall);
        println!(
            "  loop total {} us; candidate 1 = {} us ({:.0}% of the loop)",
            us(total),
            us(first),
            100.0 * first.as_secs_f64() / total.as_secs_f64().max(f64::EPSILON)
        );
    }
    if let Ok(runs) = run_sync_series(&snapshot, &candidates, latency, true) {
        let total: Duration = runs.iter().map(|r| r.wall).sum();
        let rpc: u64 = runs.iter().map(|r| r.round_trips).sum();
        println!(
            "  independent CacheDBs: {} frames, {} us total, {} modeled RPC round trips",
            runs.len(),
            us(total),
            rpc
        );
    }

    // ── leg 4: the coroutine fan-out over the DISJOINT corpus ─────────────
    println!(
        "\n[4] coroutine fan-out, DISJOINT corpus (each frame: own caller, own token, own slot)"
    );
    let disjoint_leaked: Vec<&'static RecoveredTxEnvelope> =
        disjoint_frames.iter().cloned().map(leak).collect();
    if let Ok(runs) = run_sync_series(&snapshot, &disjoint_frames, latency, true) {
        report_series("serial, independent CacheDBs", &runs);
    }
    if let Ok(runs) = run_sync_series(&snapshot, &disjoint_frames, latency, false) {
        report_series("serial, one shared CacheDB", &runs);
    }
    let disjoint_ctx = Arc::new(FanOutCtx::new(Arc::clone(&snapshot), latency));
    let mut disjoint = build_fan_out(&disjoint_ctx, &disjoint_leaked);
    match drive(&disjoint_ctx, &mut disjoint) {
        Ok(report) => report_fan_out("fan-out (one batched round trip per round)", &report),
        Err(e) => println!("  FAILED: {e}"),
    }

    // ── leg 5: the fan-out over N copies of the SAME frame ────────────────
    println!(
        "\n[5] coroutine fan-out, IDENTICAL corpus frame x{frames_label} (the lockstep bound)"
    );
    if let Ok(runs) = run_sync_series(&snapshot, &same_frames, latency, true) {
        report_series("serial, independent CacheDBs", &runs);
    }
    if let Ok(runs) = run_sync_series(&snapshot, &same_frames, latency, false) {
        let total: Duration = runs.iter().map(|r| r.wall).sum();
        report_series("serial, one shared CacheDB", &runs);
        println!("  shared-cache loop total {} us", us(total));
    }
    let same_ctx = Arc::new(FanOutCtx::new(Arc::clone(&snapshot), latency));
    let same_leaked: Vec<&'static RecoveredTxEnvelope> =
        same_frames.iter().cloned().map(leak).collect();
    let mut same = build_fan_out(&same_ctx, &same_leaked);
    match drive(&same_ctx, &mut same) {
        Ok(report) => report_fan_out("fan-out (one batched round trip per round)", &report),
        Err(e) => println!("  FAILED: {e}"),
    }

    // ── leg 6: fiber overhead alone (every answer already cached) ─────
    println!("\n[6] fiber overhead alone: the same fan-out re-driven, every answer cached");
    let mut warm = build_fan_out(&same_ctx, &same_leaked);
    match drive(&same_ctx, &mut warm) {
        Ok(report) => report_fan_out("fan-out, no cold misses", &report),
        Err(e) => println!("  FAILED: {e}"),
    }
    // The same measurement for ONE frame: the per-frame fiber/EVm cost with
    // no cold miss to hide behind.
    let single_ctx = Arc::new(FanOutCtx::new(Arc::clone(&snapshot), latency));
    let single = same_leaked.first().copied();
    if let Some(frame) = single {
        let mut cold = build_fan_out(&single_ctx, &[frame]);
        let _ = drive(&single_ctx, &mut cold);
        let mut warm = build_fan_out(&single_ctx, &[frame]);
        match drive(&single_ctx, &mut warm) {
            Ok(report) => report_fan_out("one frame, no cold misses", &report),
            Err(e) => println!("  FAILED: {e}"),
        }
    }

    // ── leg 7: steady state with the cross-block warm cache in front ──────
    println!("\n[7] steady state: the production cross-block WarmCodeCache in front");
    let warm_inner = WarmCodeCacheInner::shared_default();
    for block in [BENCH_BLOCK, BENCH_BLOCK + 1] {
        match run_sync_series_warm_cache(
            &snapshot,
            std::slice::from_ref(&corpus.frame),
            latency,
            block,
            &warm_inner,
        ) {
            Ok(runs) => {
                let label = if block == BENCH_BLOCK {
                    "block 1 (cache cold)"
                } else {
                    "block 2 (cache warm)"
                };
                report_series(label, &runs);
            }
            Err(e) => println!("  FAILED: {e}"),
        }
    }
    match run_sync_series_warm_cache(
        &snapshot,
        &candidates,
        latency,
        BENCH_BLOCK + 2,
        &warm_inner,
    ) {
        Ok(runs) => {
            let total: Duration = runs.iter().map(|r| r.wall).sum();
            report_series("per-candidate loop, block 3 (cache warm)", &runs);
            println!("  loop total {} us", us(total));
        }
        Err(e) => println!("  FAILED: {e}"),
    }

    // ── leg 8: two fan-outs at ONE anchor over the shared memo ────────────
    println!(
        "\n[8] two fan-outs at ONE read anchor ({}): the session's per-anchor storage memo",
        BENCH_BLOCK + 3
    );
    let anchor = BENCH_BLOCK + 3;
    // BEFORE (the per-fan-out scope the dispatch used): each fan-out pays the
    // anchor's cold storage chain itself. The warm-code cache is shared, so the
    // account rows are already served cross-block — only storage re-forwards.
    let scope_inner = WarmCodeCacheInner::shared_default();
    let mut per_fanout: Vec<SyncRun> = Vec::new();
    for _ in 0..2 {
        match run_sync_series_warm_cache(
            &snapshot,
            std::slice::from_ref(&corpus.frame),
            latency,
            anchor,
            &scope_inner,
        ) {
            Ok(mut runs) => per_fanout.append(&mut runs),
            Err(e) => {
                println!("  FAILED: {e}");
                return;
            }
        }
    }
    report_series("per-fan-out memo, fan-out 1 (before)", &per_fanout[..1]);
    report_series("per-fan-out memo, fan-out 2 (before)", &per_fanout[1..]);

    // AFTER (the cross-fan-out scope): the same anchor hands both fan-outs one
    // memo, so fan-out 2 pays no storage forward.
    let shared_inner = WarmCodeCacheInner::shared_default();
    let anchored = match run_sync_series_anchored(
        &snapshot,
        &[corpus.frame.clone(), corpus.frame.clone()],
        latency,
        anchor,
        &shared_inner,
    ) {
        Ok(runs) => runs,
        Err(e) => {
            println!("  FAILED: {e}");
            return;
        }
    };
    report_series("shared per-anchor memo, fan-out 1 (after)", &anchored[..1]);
    report_series("shared per-anchor memo, fan-out 2 (after)", &anchored[1..]);
    let (memo_hits, memo_misses) = {
        let cache = shared_inner.read();
        cache.storage_memo_for(ReadAnchor::at(anchor)).stats()
    };
    println!(
        "  anchor {anchor} memo: {memo_hits} hits / {memo_misses} misses; storage forwards per fan-out \
         {} -> {} ({} saved on fan-out 2)",
        per_fanout[0].storage,
        anchored[0].storage,
        per_fanout[1].storage.saturating_sub(anchored[1].storage),
    );
    assert_eq!(
        anchored[1].storage, 0,
        "the second fan-out at the same anchor must pay no storage forward"
    );
    assert_eq!(
        anchored[0].verdict, anchored[1].verdict,
        "the memo scope changed the settled verdict between fan-outs"
    );
    assert_eq!(
        per_fanout[1].verdict, anchored[1].verdict,
        "the memo scope changed the settled verdict across scopes"
    );
    println!(
        "  verdict identity across memo scopes: {:?}",
        anchored[1].verdict
    );

    // ── leg 9: the anchor moves -> a fresh memo, no stale reads ───────────
    println!(
        "\n[9] invalidation: the anchor moves ({} -> {}), the retired memo is dropped",
        anchor,
        anchor + 1
    );
    let chain_moved = Arc::new(bumped(&snapshot));
    let moved_inner = WarmCodeCacheInner::shared_default();
    let old_anchor_runs = match run_sync_series_anchored(
        &snapshot,
        std::slice::from_ref(&corpus.frame),
        latency,
        anchor,
        &moved_inner,
    ) {
        Ok(runs) => runs,
        Err(e) => {
            println!("  FAILED: {e}");
            return;
        }
    };
    let old_memo = {
        let cache = moved_inner.read();
        cache.storage_memo_for(ReadAnchor::at(anchor))
    };
    // The retired memo's successor, captured BEFORE it is filled: the anchor
    // moved, so the slot must hand out an EMPTY memo (nothing carried over).
    let moved_memo_before = {
        let cache = moved_inner.read();
        cache.storage_memo_for(ReadAnchor::at(anchor + 1))
    };
    assert!(
        !Arc::ptr_eq(&old_memo, &moved_memo_before),
        "the moved anchor must mint a fresh memo"
    );
    assert_eq!(
        moved_memo_before.stats(),
        (0, 0),
        "the moved anchor's memo starts empty — the retired entries are dropped"
    );
    let moved = match run_sync_series_anchored(
        &chain_moved,
        std::slice::from_ref(&corpus.frame),
        latency,
        anchor + 1,
        &moved_inner,
    ) {
        Ok(runs) => runs,
        Err(e) => {
            println!("  FAILED: {e}");
            return;
        }
    };
    report_series(&format!("anchor {anchor} (memo filled)"), &old_anchor_runs);
    report_series(
        &format!("anchor {} (fresh memo, chain bumped)", anchor + 1),
        &moved,
    );
    println!(
        "  the moved anchor minted a fresh memo (the old entries are dropped); \
         storage re-paid = {}",
        moved[0].storage
    );
    assert!(
        moved[0].storage > 0,
        "the moved anchor re-reads storage — the retired memo served nothing"
    );
    assert_ne!(
        moved[0].verdict, old_anchor_runs[0].verdict,
        "the bumped chain is observable — a reused memo would have shadowed it"
    );
    // The end-to-end no-shadow check: the fresh memo's run settles exactly as a
    // memo-less cold run over the same (changed) chain does.
    let reference = match run_sync_series(
        &chain_moved,
        std::slice::from_ref(&corpus.frame),
        latency,
        false,
    ) {
        Ok(runs) => runs,
        Err(e) => {
            println!("  FAILED: {e}");
            return;
        }
    };
    assert_eq!(
        moved[0].verdict, reference[0].verdict,
        "the moved anchor's memo changed the settled verdict (a stale shadow)"
    );
    println!(
        "  no stale shadow: the moved anchor settles like a memo-less cold run ({:?})",
        reference[0].verdict
    );

    // ── leg 10: path-derived slot prefetch into the per-anchor memo ───────
    println!(
        "\n[10] path-derived slot prefetch: ONE batch read at the anchor, seeded before candidate #1"
    );
    let Some(weth) = corpus.contracts.get(3).map(|c| c.address) else {
        println!("  FAILED: corpus contract order changed");
        return;
    };
    // The corpus's PoolManager stub stores no ERC6909 balances, so those two
    // executor slots are not named here (a production session always names it);
    // the frame never reads them either way.
    let facts = PrefetchFacts {
        weth_address: weth,
        pool_manager_address: None,
        warmup: degenbot_executor::compute_simulation_warmup_slots(executor, weth),
    };
    // The corpus's pools are the harness `Pair` stub, whose layout is
    // token0@0 / token1@1 / reserves@2 — the canonical Uniswap pair (6/7/8) is
    // measured separately below.
    let harness_layout = FamilySlotLayout {
        v2: V2SlotIndices {
            token0: 0,
            token1: 1,
            reserves: 2,
        },
        ..FamilySlotLayout::PRODUCTION
    };
    let paths = || std::iter::once(&corpus_path);
    let derived = path_prefetch_slots(paths(), &harness_layout, &facts);
    for (address, slot) in &derived {
        println!("    derived slot {}[{slot:#x}]", short(*address));
    }

    let transport = StorageTransport::new(Arc::clone(&snapshot), latency);
    let provider = transport.provider();
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    else {
        println!("  FAILED: multi-thread runtime");
        return;
    };
    let prefetch_anchor = BENCH_BLOCK + 5;
    let prefetch_inner = WarmCodeCacheInner::shared_default();
    let memo = {
        let cache = prefetch_inner.read();
        cache.storage_memo_for(ReadAnchor::at(prefetch_anchor))
    };
    let report = {
        let _guard = runtime.enter();
        prefetch_paths_into_memo(
            &provider,
            prefetch_anchor,
            paths(),
            &harness_layout,
            &facts,
            LearnedSets {
                hot: &HotSlotStore::new(),
                hooks: None,
            },
            &memo,
        )
    };
    let packets = transport.packets();
    println!(
        "  derived {} slots from the corpus path (+executor fixed slots); prefetch: {} packet(s), {} member(s), {} seeded, skip {:?}",
        derived.len(),
        packets.len(),
        packets.first().map_or(0, Vec::len),
        report.seeded,
        report.skipped
    );
    assert!(
        report.skipped.is_none(),
        "the prefetch degraded: {:?}",
        report.skipped
    );
    assert_eq!(report.seeded, derived.len(), "every derived slot is seeded");
    assert_eq!(
        packets.len(),
        1,
        "the prefetch is ONE dispatched batch round trip"
    );
    assert_eq!(
        packets[0].len(),
        derived.len(),
        "one member per derived slot"
    );
    assert!(
        packets[0]
            .iter()
            .all(|(method, _)| method == "eth_getStorageAt"),
        "the batch carries only storage reads"
    );
    assert!(
        packets[0]
            .iter()
            .all(|(_, params)| params.contains(&format!("{prefetch_anchor:#x}"))),
        "every member pins the fan-out's anchor"
    );

    // BEFORE: candidate #1's own cold chain (fresh CacheDB, no memo) — the
    // steady-state cost this lane attacks.
    let before = match run_sync_series(
        &snapshot,
        std::slice::from_ref(&corpus.frame),
        latency,
        true,
    ) {
        Ok(runs) => runs,
        Err(e) => {
            println!("  FAILED: {e}");
            return;
        }
    };
    // AFTER: the same fan-out with the anchor's memo prefetched, then the next
    // fan-out at the same anchor.
    let after_first = match run_sync_series_anchored(
        &snapshot,
        std::slice::from_ref(&corpus.frame),
        latency,
        prefetch_anchor,
        &prefetch_inner,
    ) {
        Ok(runs) => runs,
        Err(e) => {
            println!("  FAILED: {e}");
            return;
        }
    };
    let stats_after_first = {
        let cache = prefetch_inner.read();
        cache
            .storage_memo_for(ReadAnchor::at(prefetch_anchor))
            .stats()
    };
    let after_second = match run_sync_series_anchored(
        &snapshot,
        std::slice::from_ref(&corpus.frame),
        latency,
        prefetch_anchor,
        &prefetch_inner,
    ) {
        Ok(runs) => runs,
        Err(e) => {
            println!("  FAILED: {e}");
            return;
        }
    };
    let (covered, uncovered) = stats_after_first;
    report_series("fan-out 1 (no prefetch)", &before);
    report_series("fan-out 1 (prefetched)", &after_first);
    report_series("fan-out 2 at the same anchor (prefetched)", &after_second);
    println!(
        "  storage forwards per fan-out: {} -> {} ({} of {} slots covered up front, {} left to the chain)",
        before[0].storage, after_first[0].storage, covered, covered + uncovered, uncovered
    );
    assert!(
        after_first[0].storage < before[0].storage,
        "the prefetch must cut candidate #1's storage forwards"
    );
    assert_eq!(
        after_first[0].storage, uncovered,
        "every uncovered slot is exactly one forwarded read"
    );
    assert_eq!(
        covered + uncovered,
        before[0].storage,
        "the covered set is the un-prefetched cold set's prefix"
    );
    assert_eq!(
        after_second[0].storage, 0,
        "the anchor's second fan-out pays no storage forward"
    );
    assert_eq!(
        before[0].verdict, after_first[0].verdict,
        "the prefetch changed the settled verdict"
    );
    assert_eq!(
        after_first[0].verdict, after_second[0].verdict,
        "the prefetch changed the settled verdict between fan-outs"
    );
    println!(
        "  verdict identity prefetch == no-prefetch: {:?}",
        after_first[0].verdict
    );

    // The PRODUCTION layout on this stub corpus: the canonical V2 indices
    // (6/7/8) miss the stub Pair's words; only the executor's WETH balanceOf
    // slot lands. Reported, plus the relational claim that the production table
    // covers less of a stubbed corpus.
    let production_anchor = BENCH_BLOCK + 6;
    let production_inner = WarmCodeCacheInner::shared_default();
    let production_memo = {
        let cache = production_inner.read();
        cache.storage_memo_for(ReadAnchor::at(production_anchor))
    };
    let production_derived = path_prefetch_slots(paths(), &FamilySlotLayout::PRODUCTION, &facts);
    let production_report = {
        let _guard = runtime.enter();
        prefetch_paths_into_memo(
            &provider,
            production_anchor,
            paths(),
            &FamilySlotLayout::PRODUCTION,
            &facts,
            LearnedSets {
                hot: &HotSlotStore::new(),
                hooks: None,
            },
            &production_memo,
        )
    };
    let production_run = match run_sync_series_anchored(
        &snapshot,
        std::slice::from_ref(&corpus.frame),
        latency,
        production_anchor,
        &production_inner,
    ) {
        Ok(runs) => runs,
        Err(e) => {
            println!("  FAILED: {e}");
            return;
        }
    };
    let production_stats = {
        let cache = production_inner.read();
        cache
            .storage_memo_for(ReadAnchor::at(production_anchor))
            .stats()
    };
    println!(
        "  production layout (V2 6/7/8) on the stub corpus: {} derived / {} seeded, {} hit -> forwards {} (the stub Pair layout is 0/1/2)",
        production_derived.len(),
        production_report.seeded,
        production_stats.0,
        production_run[0].storage
    );
    assert_eq!(
        production_run[0].storage + production_stats.0,
        before[0].storage,
        "the production-layout run's covered set plus its forwards is the cold set"
    );
    assert!(
        production_stats.0 < covered,
        "the canonical V2 indices cover less of the stub corpus than the stub's own layout"
    );
    assert_eq!(
        production_run[0].verdict, before[0].verdict,
        "a sparser prefetch changed the settled verdict"
    );

    // ── stale anchor: a prefetch at N never serves a fan-out at N+1 ───────
    let moved_memo = {
        let cache = prefetch_inner.read();
        cache.storage_memo_for(ReadAnchor::at(prefetch_anchor + 1))
    };
    assert_eq!(
        moved_memo.stats(),
        (0, 0),
        "the moved anchor mints an empty memo"
    );
    for (address, slot) in &derived {
        assert!(
            !moved_memo.holds(*address, *slot),
            "a prefetch at {prefetch_anchor} must not appear at {}",
            prefetch_anchor + 1
        );
    }
    let moved_run = match run_sync_series_anchored(
        &snapshot,
        std::slice::from_ref(&corpus.frame),
        latency,
        prefetch_anchor + 1,
        &prefetch_inner,
    ) {
        Ok(runs) => runs,
        Err(e) => {
            println!("  FAILED: {e}");
            return;
        }
    };
    report_series(
        &format!(
            "anchor {} (the prefetched memo is retired)",
            prefetch_anchor + 1
        ),
        &moved_run,
    );
    assert_eq!(
        moved_run[0].storage, before[0].storage,
        "the moved anchor pays the full cold chain — no seeded entry carried"
    );
    // The shadow check: prefetch against the settled chain at N, then run at
    // N+1 against a BUMPED chain. A leaked entry would settle differently from
    // a memo-less cold run over the same bumped chain.
    let shadow_inner = WarmCodeCacheInner::shared_default();
    let shadow_anchor = BENCH_BLOCK + 7;
    let shadow_memo = {
        let cache = shadow_inner.read();
        cache.storage_memo_for(ReadAnchor::at(shadow_anchor))
    };
    let _shadow_report = {
        let _guard = runtime.enter();
        prefetch_paths_into_memo(
            &provider,
            shadow_anchor,
            paths(),
            &harness_layout,
            &facts,
            LearnedSets {
                hot: &HotSlotStore::new(),
                hooks: None,
            },
            &shadow_memo,
        )
    };
    let bumped_chain = Arc::new(bumped(&snapshot));
    let shadow_run = match run_sync_series_anchored(
        &bumped_chain,
        std::slice::from_ref(&corpus.frame),
        latency,
        shadow_anchor + 1,
        &shadow_inner,
    ) {
        Ok(runs) => runs,
        Err(e) => {
            println!("  FAILED: {e}");
            return;
        }
    };
    let bumped_reference = match run_sync_series(
        &bumped_chain,
        std::slice::from_ref(&corpus.frame),
        latency,
        true,
    ) {
        Ok(runs) => runs,
        Err(e) => {
            println!("  FAILED: {e}");
            return;
        }
    };
    assert_eq!(
        shadow_run[0].verdict, bumped_reference[0].verdict,
        "the retired memo shadowed the moved anchor's value"
    );
    println!(
        "  no stale shadow: the anchor move settles like a memo-less cold run ({:?})",
        bumped_reference[0].verdict
    );

    // ── leg 10b: the capture-learned hot set (the derivation's complement) ─
    println!(
        "\n[10b] capture-learned hot set: a completed capture teaches the next anchor's prewarm"
    );
    let learned_anchor = BENCH_BLOCK + 9;
    let learned_inner = WarmCodeCacheInner::shared_default();
    let hot = {
        let cache = learned_inner.read();
        cache.hot_slots()
    };
    // LEARN: the production capture shape — the 7-call bundle as ONE Seam-mode
    // sequence, whose `Inspect` (execute()) step is the only captured one — on a
    // session carrying the store, exactly as the dispatch fan-out's sessions do
    // (`Evm2BlockSimHandle::session` hands every session the store).
    let capture_counters = Arc::new(Counters::default());
    let mut capture_evm = policy_evm(CacheDB::new(Db::new(latency_db(
        &snapshot,
        latency,
        &capture_counters,
    ))));
    let learned_capture = {
        let mut session =
            Evm2SimSession::with_block_env(&mut capture_evm, block_env_at(learned_anchor - 1))
                .with_hot_slots(Arc::clone(&hot));
        session.run_sequence(&steps, Evm2CaptureMode::Seam)
    };
    let learned_capture = match learned_capture {
        Ok(capture) => capture,
        Err(e) => {
            println!("  FAILED: {e}");
            return;
        }
    };
    let captured_accounts = learned_capture.capture.access_list.0.len();
    let captured_slots: usize = learned_capture
        .capture
        .access_list
        .0
        .iter()
        .map(|item| item.storage_keys.len())
        .sum();
    println!(
        "  a completed capture at anchor {} touched {captured_slots} slot(s) over {captured_accounts} account(s); the store learned them",
        learned_anchor - 1
    );

    let learned_plan = prewarm_plan(
        paths(),
        &harness_layout,
        &facts,
        LearnedSets {
            hot: &hot,
            hooks: None,
        },
        learned_anchor,
    );
    println!(
        "  the next anchor's plan: {} derived + {} learned = {} slot(s), one batch",
        learned_plan.derived,
        learned_plan.learned,
        learned_plan.slots.len()
    );
    assert!(
        learned_plan.learned > 0,
        "the completed capture taught the store nothing — the learning path is broken"
    );

    let learned_memo = {
        let cache = learned_inner.read();
        cache.storage_memo_for(ReadAnchor::at(learned_anchor))
    };
    let packets_before = transport.packets().len();
    let learned_report = {
        let _guard = runtime.enter();
        prefetch_paths_into_memo(
            &provider,
            learned_anchor,
            paths(),
            &harness_layout,
            &facts,
            LearnedSets {
                hot: &hot,
                hooks: None,
            },
            &learned_memo,
        )
    };
    let learned_packets = transport.packets();
    let learned_dispatched = learned_packets.len() - packets_before;
    let learned_members = learned_packets.last().map_or(0, Vec::len);
    assert_eq!(
        learned_dispatched, 1,
        "the derivation + learned complement is ONE batch round trip"
    );
    assert_eq!(
        learned_members,
        learned_plan.slots.len(),
        "the batch carries the derivation and the learned complement together"
    );
    assert!(
        learned_packets.last().is_some_and(|packet| packet
            .iter()
            .all(|(_, params)| params.contains(&format!("{learned_anchor:#x}")))),
        "every member (derived or learned) reads the node at the fan-out's anchor"
    );
    println!(
        "  prewarm: {} derived + {} learned slot(s), {} seeded in {} packet(s) x {} member(s)",
        learned_report.derived,
        learned_report.learned,
        learned_report.seeded,
        learned_dispatched,
        learned_members,
    );

    let learned_run = match run_sync_series_anchored(
        &snapshot,
        std::slice::from_ref(&corpus.frame),
        latency,
        learned_anchor,
        &learned_inner,
    ) {
        Ok(runs) => runs,
        Err(e) => {
            println!("  FAILED: {e}");
            return;
        }
    };
    report_series("fan-out 1 (path prefetch + learned set)", &learned_run);
    println!(
        "  storage forwards per fan-out: no prefetch {} -> path prefetch only {} -> path + learned {}",
        before[0].storage, after_first[0].storage, learned_run[0].storage
    );
    assert_eq!(
        learned_run[0].storage, 0,
        "with a primed store, fan-out 1 pays no storage forward"
    );
    assert_eq!(
        learned_run[0].verdict, before[0].verdict,
        "the learned prewarm changed the settled verdict"
    );
    println!(
        "  verdict identity prewarm+learned == no-prewarm: {:?}",
        learned_run[0].verdict
    );

    // STALE PREDICTION, FRESH READ: the store learned at `learned_anchor - 1`;
    // prewarm at a LATER anchor whose chain has moved. The learned members still
    // dispatch (they are inside the decay window), but their values come from
    // the node at THIS anchor — so the settled verdict is the memo-less cold
    // run's, not a stale serve's.
    let stale_anchor = learned_anchor + 1;
    let stale_chain = Arc::new(bumped(&snapshot));
    let stale_transport = StorageTransport::new(Arc::clone(&stale_chain), latency);
    let stale_provider = stale_transport.provider();
    let stale_plan = prewarm_plan(
        paths(),
        &harness_layout,
        &facts,
        LearnedSets {
            hot: &hot,
            hooks: None,
        },
        stale_anchor,
    );
    assert_eq!(
        stale_plan.learned, learned_plan.learned,
        "the earlier capture's prediction is still inside the decay window"
    );
    let stale_memo = {
        let cache = learned_inner.read();
        cache.storage_memo_for(ReadAnchor::at(stale_anchor))
    };
    let stale_report = {
        let _guard = runtime.enter();
        prefetch_paths_into_memo(
            &stale_provider,
            stale_anchor,
            paths(),
            &harness_layout,
            &facts,
            LearnedSets {
                hot: &hot,
                hooks: None,
            },
            &stale_memo,
        )
    };
    let stale_run = match run_sync_series_anchored(
        &stale_chain,
        std::slice::from_ref(&corpus.frame),
        latency,
        stale_anchor,
        &learned_inner,
    ) {
        Ok(runs) => runs,
        Err(e) => {
            println!("  FAILED: {e}");
            return;
        }
    };
    let stale_reference = match run_sync_series(
        &stale_chain,
        std::slice::from_ref(&corpus.frame),
        latency,
        true,
    ) {
        Ok(runs) => runs,
        Err(e) => {
            println!("  FAILED: {e}");
            return;
        }
    };
    println!(
        "  stale prediction: {} learned slot(s) dispatched at anchor {stale_anchor} against a moved chain, {} seeded from the node at that anchor",
        stale_report.learned, stale_report.seeded
    );
    assert_eq!(
        stale_run[0].verdict, stale_reference[0].verdict,
        "the learned prewarm served a stale value instead of the node's at the fan-out's anchor"
    );
    println!(
        "  no stale serve: the moved chain settles like a memo-less cold run ({:?})",
        stale_reference[0].verdict
    );

    // DECAY (last: the query prunes the decayed slots out of the store): the
    // same store asked past the drift window prewarms nothing learned — the hot
    // set is working memory, not permanent.
    let decayed_anchor = learned_anchor + HOT_SLOT_DECAY_BLOCKS;
    let decayed_plan = prewarm_plan(
        paths(),
        &harness_layout,
        &facts,
        LearnedSets {
            hot: &hot,
            hooks: None,
        },
        decayed_anchor,
    );
    println!(
        "  decay: at anchor {decayed_anchor} (window {HOT_SLOT_DECAY_BLOCKS} blocks) the learned complement is {} slot(s), the plan {} slot(s)",
        decayed_plan.learned,
        decayed_plan.slots.len()
    );
    assert_eq!(
        decayed_plan.learned, 0,
        "a slot untouched for the whole decay window must drop out of the prewarm"
    );
    assert_eq!(decayed_plan.slots.len(), decayed_plan.derived);

    // ── verdict identity: the cold path must not change the settled result ─
    println!("\n[11] verdict identity");
    let mut ok = true;
    if let (Ok(a), Ok(b)) = (
        run_sync_series(
            &snapshot,
            std::slice::from_ref(&corpus.frame),
            latency,
            true,
        ),
        run_sync_series(
            &snapshot,
            std::slice::from_ref(&corpus.frame),
            Duration::ZERO,
            true,
        ),
    ) {
        if a[0].verdict == b[0].verdict {
            println!(
                "  executor frame: cold == zero-latency ({:?})",
                a[0].verdict
            );
        } else {
            ok = false;
            println!(
                "  FAILED: executor frame verdict differs {:?} vs {:?}",
                a[0].verdict, b[0].verdict
            );
        }
    }
    println!("  (the fan-out legs above assert their own per-frame identity via drive())");
    assert!(ok, "the cold path changed the settled verdict");
}

fn mint(
    evm: &mut Evm<'_, BaseEvmTypes>,
    token: Address,
    to: Address,
    amount: U256,
    nonce: u64,
) -> Result<(), String> {
    let frame = envelope(
        DEPLOYER,
        TxKind::Call(token),
        Bytes::from(BenchToken::mintCall { to, amount }.abi_encode()),
        30_000_000,
        nonce,
    );
    let executed = evm
        .transact(&frame)
        .map_err(|e| format!("mint transact: {e:?}"))?;
    if !executed.commit().status {
        return Err("mint reverted".into());
    }
    Ok(())
}

/// Cold then warm over ONE cache: the cold pass warms it, the warm pass re-runs
/// the same steps.
fn warm_bundle(
    snapshot: &Arc<Snapshot>,
    steps: &[Evm2SimStep],
    latency: Duration,
) -> Result<(SyncRun, SyncRun), String> {
    let counters = Arc::new(Counters::default());
    let mut evm = policy_evm(CacheDB::new(Db::new(latency_db(
        snapshot, latency, &counters,
    ))));
    let mut runs = Vec::with_capacity(2);
    for _ in 0..2 {
        let before = counters.snapshot();
        let started = Instant::now();
        let sequence = {
            let mut session = Evm2SimSession::with_block_env(&mut evm, block_env());
            session
                .run_sequence(steps, Evm2CaptureMode::Seam)
                .map_err(|e| format!("bundle sequence: {e}"))?
        };
        let wall = started.elapsed();
        let after = counters.snapshot();
        let execute = sequence
            .calls
            .get(3)
            .ok_or("the bundle lost its execute step")?;
        let counts = (after.0 - before.0, after.1 - before.1);
        runs.push(SyncRun {
            wall,
            accounts: counts.0,
            storage: counts.1,
            round_trips: Counters::rpc_calls(counts),
            verdict: Verdict {
                status: matches!(
                    execute.verdict,
                    degenbot_simulation::sim::RunVerdict::Success
                ),
                gas: execute.gas_used,
                output: keccak256(&execute.output),
            },
        });
    }
    let warm = runs.pop().expect("one warm run");
    let cold = runs.pop().expect("one cold run");
    if cold.verdict != warm.verdict {
        return Err("cold and warm bundle verdicts differ".into());
    }
    Ok((cold, warm))
}

fn report_series(label: &str, runs: &[SyncRun]) {
    let total: Duration = runs.iter().map(|r| r.wall).sum();
    let rpc: u64 = runs.iter().map(|r| r.round_trips).sum();
    let accounts: u64 = runs.iter().map(|r| r.accounts).sum();
    let storage: u64 = runs.iter().map(|r| r.storage).sum();
    let per_frame = if runs.is_empty() {
        0
    } else {
        us(total) / u128::try_from(runs.len()).unwrap_or(u128::MAX)
    };
    let uniform = runs.iter().all(|r| r.verdict == runs[0].verdict);
    println!(
        "  {label}: {} frame(s), {} us total ({} us/frame), {} account + {} storage forwards = {} modeled round trips, uniform verdict {} ({:?})",
        runs.len(),
        us(total),
        per_frame,
        accounts,
        storage,
        rpc,
        uniform,
        runs[0].verdict
    );
}

fn report_bundle(label: &str, run: &SyncRun) {
    println!(
        "  {label}: {} us, {} account + {} storage forwards = {} modeled round trips, execute verdict {:?}",
        us(run.wall),
        run.accounts,
        run.storage,
        run.round_trips,
        run.verdict
    );
}

fn report_fan_out(label: &str, report: &FanOutReport) {
    println!(
        "  {label}: {} us wall, {} rounds, {} modeled round trips, max batch {}, {} dedup hits",
        us(report.wall),
        report.rounds,
        report.rpc_calls,
        report.max_batch(),
        report.dedup
    );
    println!("    batch sizes per round: {:?}", report.batch_sizes);
    println!("    verdicts: {:?}", report.verdicts.len());
}

fn load_stub_creation(name: &str) -> Vec<u8> {
    let rel = format!("tier3-oracle/artifacts/harness/{name}.json");
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let raw = std::fs::read_to_string(root.join(&rel)).expect("read stub artifact");
    degenbot_simulation::oracle::parse_foundry_creation_bytecode(&raw).expect("parse stub artifact")
}
