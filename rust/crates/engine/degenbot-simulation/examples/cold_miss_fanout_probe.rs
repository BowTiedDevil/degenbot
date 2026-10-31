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
//! charged THREE round trips because the production adapter
//! (`AlloyAsyncDb::get_account`) issues `eth_getBalance` +
//! `eth_getTransactionCount` + `eth_getCode` sequentially.
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
//! 7. the batched-account lever: the same measurements with the account read
//!    modeled as ONE JSON-RPC batch instead of three serial calls;
//! 8. steady state: the same frame with the production cross-block
//!    `WarmCodeCache` in front (contract accounts + code are TTL-cached across
//!    blocks; EOA rows and storage always forward);
//! 9. verdict identity between the cold and zero-latency paths.
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

use degenbot_simulation::sim::evm2::handle::EVM2_PRODUCTION_SPEC;
use degenbot_simulation::sim::evm2::session::{Evm2CaptureMode, Evm2SimSession, Evm2SimStep};
use degenbot_simulation::sim::evm2::warm_code_cache::{WarmCodeCache, WarmCodeCacheInner};

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
#[derive(Debug, Default)]
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

// ── The accounting ────────────────────────────────────────────────────────

/// Cold-path accounting: forwarded account reads (3 RPC round trips each) and
/// storage reads (1 round trip each).
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
        3 * counts.0 + counts.1
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
/// (three for an account, the production adapter's serial triple).
struct LatencyDb {
    snapshot: Arc<Snapshot>,
    latency: Duration,
    counters: Arc<Counters>,
    /// Model the account read as ONE batched round trip
    /// (`eth_getBalance`+`eth_getTransactionCount`+`eth_getCode` in one
    /// JSON-RPC batch) instead of the production adapter's three serial calls.
    batched_account: bool,
}

impl Database for LatencyDb {
    type Error = std::convert::Infallible;

    fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        if self.batched_account {
            self.counters.record_account(*address);
            std::thread::sleep(self.latency);
        } else {
            self.counters.record_account(*address);
            std::thread::sleep(self.latency.saturating_mul(3));
        }
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
    BlockEnvExt {
        basefee: U256::from(BENCH_BASE_FEE),
        number: U256::from(BENCH_BLOCK),
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
        batched_account: false,
    }
}

// ── The legs ─────────────────────────────────────────────────────────────

/// One blocking run's measurement.
#[derive(Debug)]
struct SyncRun {
    wall: Duration,
    accounts: u64,
    storage: u64,
    /// Modeled RPC round trips (3 per account read unless the account read is
    /// batched).
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
    run_sync_series_with(snapshot, frames, latency, fresh_per_frame, false)
}

/// [`run_sync_series`] with the account read modeled as one batched round trip.
fn run_sync_series_with(
    snapshot: &Arc<Snapshot>,
    frames: &[RecoveredTxEnvelope],
    latency: Duration,
    fresh_per_frame: bool,
    batched_account: bool,
) -> Result<Vec<SyncRun>, String> {
    let counters = Arc::new(Counters::default());
    let build = || {
        policy_evm(CacheDB::new(Db::new(LatencyDb {
            batched_account,
            ..latency_db(snapshot, latency, &counters)
        })))
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
            round_trips: if batched_account {
                counts.0 + counts.1
            } else {
                Counters::rpc_calls(counts)
            },
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
    let corpus = match build_corpus(&mut setup) {
        Ok(corpus) => corpus,
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

    // ── leg 7: the account triple as ONE batched round trip ───────────────
    println!("\n[7] lever check: the account read as one batched round trip (vs 3 serial)");
    if let Ok(runs) = run_sync_series_with(
        &snapshot,
        std::slice::from_ref(&corpus.frame),
        latency,
        false,
        true,
    ) {
        report_series("one executor frame, batched account read", &runs);
    }
    if let Ok(runs) = run_sync_series_with(&snapshot, &candidates, latency, false, true) {
        let total: Duration = runs.iter().map(|r| r.wall).sum();
        report_series("per-candidate loop, batched account read", &runs);
        println!("  loop total {} us", us(total));
    }

    // ── leg 8: steady state with the cross-block warm cache in front ──────
    println!("\n[8] steady state: the production cross-block WarmCodeCache in front");
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

    // ── verdict identity: the cold path must not change the settled result ─
    println!("\n[9] verdict identity");
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
