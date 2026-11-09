#![expect(
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::print_stdout,
    clippy::print_stderr
)]

//! The per-worker EVM fan-out probe (ADR-089 concurrency trio
//! item 4): does splitting one dispatch fan-out across per-worker EVMs beat
//! today's serial shared-`&mut evm` drive, and which of the two costs that
//! remains in production — the serial cold-read chain or the fast-forward CPU
//! load — is the win made of?
//!
//! # Load contract
//!
//! The corpus is shape-matched to the measured production fan-out (`run_bot.sh`
//! drive, mainnet 26374751..26374757): 2-hop V2 cycles over pool-disjoint stub
//! pairs sharing one hot set (WETH + the executor), candidates mixed across
//! four knob depths — healthy, short-balance (mid revert), inflated-hop-2
//! expectation (late revert), uninitialized pool (early revert) — at the
//! measured verdict weights. Cost model from the same drive: one modeled round
//! trip per account read (the production adapter batches balance+nonce+code),
//! one per storage read, zero wall for a memo serve (counted, never slept).
//!
//! # Legs
//!
//! - `serial`  — ONE `Evm` + ONE `CacheDB` over the shared memo (today's shape).
//! - `threads` — one `Evm` + `CacheDB` per worker thread, sharing the memo.
//! - `fibers`  — one Evm per candidate on an evm2 `async` fiber over
//!   `AsyncDb`, driven by the batched-round scheduler: every queued read
//!   settles in ONE modeled round trip per scheduler round.
//! - `fibers-warm` — the fiber drive re-run with the previous answers served:
//!   the seam's overhead alone, zero pending reads.
//!
//! # Gate
//!
//! Cross-leg verdict identity (status, gas, output hash) — a drive strategy
//! that changes a verdict is a correctness bug, not a performance result.
//! `WORKER_FANOUT_PLANT_DIVERGENCE=1` plants one divergence so the gate's own
//! sensitivity is demonstrable (the negative-probe idiom). Wall is REPORTED,
//! never asserted; `charged_us` is the deterministic modeled cost (counts x
//! measured unit prices) and is the granularity-free comparison.
//!
//! # Usage
//!
//! ```sh
//! just worker-fanout-probe                    # production corpus, 21 cands
//! just worker-fanout-probe                    # env knobs:
//! WORKER_FANOUT_SHAPE=lockstep|disjoint WORKER_FANOUT_CANDIDATES=N \
//! WORKER_FANOUT_WORKERS=N COLD_MISS_LATENCY_US=700
//! ```

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use alloy::primitives::{keccak256, Address, Bytes, TxKind, B256, U256};
use alloy::sol_types::{SolCall, SolValue};

use ::evm2::bytecode::Bytecode;
use ::evm2::env::BlockEnvExt;
use ::evm2::ethereum::{ethereum_tx_registry, RecoveredTxEnvelope};
use ::evm2::evm::{
    r#async::{AsyncDatabase, AsyncDb},
    AccountInfo, Cache, CacheDB, Db, DynDatabase,
};
use ::evm2::interpreter::Word;
use ::evm2::{BaseEvmTypes, Evm, EvmFeatures, ExecutionConfig, Precompiles, SpecId, Version};

use degenbot_execution::{HopInfo, PathInfo, V2HopInfo};
use degenbot_executor::composers::{EncodeContext, EncodeOptions, EncodeRequest};

// The shared fixture is a toolbox wider than this probe's corpus (its
// single-frame builder half stays unused here); the module is included per
// example, so its unused items are per-unit noise, not dead code.
#[expect(dead_code)]
#[path = "shared/bench_corpus.rs"]
mod bench_corpus;

use bench_corpus::{bench_evm, deploy, envelope, Deployed, BENCH_BASE_FEE, BENCH_BLOCK, DEPLOYER};

alloy::sol! {
    /// The harness stubs' funding + seed surface (the bench corpus's stubs).
    interface BenchToken {
        function mint(address to, uint256 amount) external;
        function approve(address spender, uint256 amount) external returns (bool);
    }

    interface BenchPair {
        function initialize(address tokenA, address tokenB) external;
        function sync() external;
    }
}

/// The production engine spec (the handle's pin).
const SPEC: SpecId = SpecId::OSAKA;

/// The replayed frame's gas limit — a production pending-tx shape.
const EXECUTE_GAS: u64 = 16_000_000;

/// The one modeled round trip for a storage read (measured: `s_ms`/`s_slow`
/// ~= 700us over the production drive).
fn storage_latency() -> Duration {
    std::env::var("COLD_MISS_LATENCY_US")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(Duration::from_micros(700), Duration::from_micros)
}

/// The modeled cost of one account read (the batched balance+nonce+code
/// round trip: `b_ms`/`b_slow` ~= 1.2ms measured).
fn account_latency() -> Duration {
    std::env::var("WORKER_FANOUT_ACCOUNT_US")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(Duration::from_micros(1200), Duration::from_micros)
}

fn candidate_count() -> usize {
    std::env::var("WORKER_FANOUT_CANDIDATES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(21)
}

fn worker_count() -> usize {
    std::env::var("WORKER_FANOUT_WORKERS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(2)
}

// ── Verdict + the identity gate ───────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
struct Verdict {
    status: bool,
    gas: u64,
    output: B256,
    /// Raw return data + halt reason, carried for the debug listing (the
    /// identity gate compares the hash above, not these).
    raw: Bytes,
    stop: String,
}

impl Verdict {
    fn of(result: &::evm2::evm::TxResult) -> Self {
        Self {
            status: result.status,
            gas: result.tx_gas_used(),
            output: keccak256(&result.output),
            raw: result.output.clone(),
            stop: format!("{:?}", result.stop),
        }
    }
}

/// Compare every leg against the baseline verdicts, candidate by candidate.
/// Returns the list of mismatches; empty is green.
fn identity_gate(baseline: &[Verdict], legs: &[(&str, Vec<Verdict>)]) -> Vec<String> {
    let mut mismatches = Vec::new();
    for (label, verdicts) in legs {
        if verdicts.len() != baseline.len() {
            mismatches.push(format!(
                "{label}: drove {} candidates, baseline {}",
                verdicts.len(),
                baseline.len()
            ));
            continue;
        }
        for (index, (got, want)) in verdicts.iter().zip(baseline.iter()).enumerate() {
            if got != want {
                mismatches.push(format!(
                    "{label}: candidate {index} verdict {got:?} != baseline {want:?}"
                ));
            }
        }
    }
    mismatches
}

// ── The snapshot: the corpus's settled state, the "chain" behind the stub ──

type AccountRow = (Word, u64, B256);

#[derive(Clone, Debug, Default)]
struct Snapshot {
    accounts: HashMap<Address, AccountRow>,
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

    /// The `AccountInfo` a full read returns — code inlined, the production
    /// adapter's eager-load shape.
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

// ── Cost accounting (scoped per leg — the production counters are global) ──

#[derive(Debug, Default)]
struct Counters {
    account_forwards: AtomicU64,
    storage_forwards: AtomicU64,
    memo_hits: AtomicU64,
}

impl Counters {
    fn charge(&self, account: Duration, storage: Duration) -> Duration {
        let a = self.account_forwards.load(Ordering::Relaxed);
        let s = self.storage_forwards.load(Ordering::Relaxed);
        let m = self.memo_hits.load(Ordering::Relaxed);
        account * u32::try_from(a).unwrap_or(u32::MAX)
            + storage * u32::try_from(s).unwrap_or(u32::MAX)
            + Duration::from_micros(m)
    }
}

/// The shared per-anchor read memo — the `StorageMemo` stand-in: every value
/// in it was fetched from the modeled node at this anchor, so a hit is
/// indistinguishable from a fresh read (production's memo contract).
#[derive(Debug, Default)]
struct SharedMemo {
    entries: Mutex<HashMap<(Address, Word), Word>>,
}

impl SharedMemo {
    fn get(&self, address: Address, key: Word) -> Option<Word> {
        self.entries
            .lock()
            .expect("memo")
            .get(&(address, key))
            .copied()
    }

    fn put(&self, address: Address, key: Word, value: Word) {
        self.entries
            .lock()
            .expect("memo")
            .insert((address, key), value);
    }
}

// ── The blocking cold path + the memo seam ────────────────────────────────

/// Every forwarded read sleeps one modeled round trip (the serial read chain
/// the fiber legs exist to overlap).
struct ColdDb {
    snapshot: Arc<Snapshot>,
    latency: Duration,
    account_latency: Duration,
    counters: Arc<Counters>,
}

impl ::evm2::evm::Database for ColdDb {
    type Error = std::convert::Infallible;

    fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.counters
            .account_forwards
            .fetch_add(1, Ordering::Relaxed);
        std::thread::sleep(self.account_latency);
        Ok(self.snapshot.info(*address))
    }

    fn get_code_by_hash(&mut self, _code_hash: &B256) -> Result<Bytecode, Self::Error> {
        panic!("the adapter eager-loads code — a code-by-hash read never happens")
    }

    fn get_storage(&mut self, address: &Address, key: &Word) -> Result<Word, Self::Error> {
        self.counters
            .storage_forwards
            .fetch_add(1, Ordering::Relaxed);
        std::thread::sleep(self.latency);
        Ok(self.snapshot.slot(*address, *key))
    }

    fn get_block_hash(&mut self, number: &Word) -> Result<B256, Self::Error> {
        self.counters
            .storage_forwards
            .fetch_add(1, Ordering::Relaxed);
        std::thread::sleep(self.latency);
        Ok(self
            .snapshot
            .block_hashes
            .get(number)
            .copied()
            .unwrap_or(B256::ZERO))
    }
}

/// The production memo seam in front of the cold path: consult the anchor's
/// memo, else forward and learn. Accounts and code forward untouched.
struct MemoDb {
    cold: ColdDb,
    memo: Arc<SharedMemo>,
}

impl ::evm2::evm::Database for MemoDb {
    type Error = std::convert::Infallible;

    fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.cold.get_account(address)
    }

    fn get_code_by_hash(&mut self, code_hash: &B256) -> Result<Bytecode, Self::Error> {
        self.cold.get_code_by_hash(code_hash)
    }

    fn get_storage(&mut self, address: &Address, key: &Word) -> Result<Word, Self::Error> {
        if let Some(value) = self.memo.get(*address, *key) {
            self.cold.counters.memo_hits.fetch_add(1, Ordering::Relaxed);
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

// ── EVM construction (the probe's production frame policy) ────────────────

fn block_env() -> BlockEnvExt {
    BlockEnvExt {
        basefee: U256::from(BENCH_BASE_FEE),
        number: U256::from(BENCH_BLOCK),
        ..Default::default()
    }
}

fn policy_evm<'a>(db: impl DynDatabase + 'a) -> Evm<'a, BaseEvmTypes> {
    let mut version = Version::new(SPEC);
    version.features.remove(EvmFeatures::NONCE_CHECK);
    Evm::new_with_execution_config(
        ExecutionConfig::for_spec_and_version(SPEC, version),
        SPEC,
        block_env(),
        ethereum_tx_registry(SPEC),
        db,
        Precompiles::base(SPEC),
    )
}

// ── The corpus: pool-disjoint 2-hop cycles over one hot set ───────────────

/// How a candidate's frame ends. The knobs stand for the measured verdict
/// DEPTHS (the depth is the read-walk length, which is what the drive cost
/// scales with): healthy (full walk + settle), late (the config min-profit
/// floor rejects AFTER every command and the balance measurement — the
/// `CurrencyNotSettled` depth), mid (the hop-2 pool is unseeded — the hop-1
/// walk runs fully before the revert), early (the hop-1 pool is unseeded —
/// the revert lands before any swap executes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Knob {
    Healthy,
    LateRevert,
    MidRevert,
    EarlyRevert,
}

/// The measured production verdict weights over a 20-slot pattern
/// (late ~50%, mid ~35%, early ~10%, healthy ~5%).
fn knob_at(index: usize) -> Knob {
    match index % 20 {
        0..=9 => Knob::LateRevert,
        10..=16 => Knob::MidRevert,
        17..=18 => Knob::EarlyRevert,
        _ => Knob::Healthy,
    }
}

struct Candidate {
    frame: RecoveredTxEnvelope,
    knob: Knob,
    pools: Vec<Address>,
}

/// The shared hot set: WETH + `PoolManager` + the one executor every candidate's
/// frame calls into.
struct World {
    weth: Deployed,
    pool_manager: Deployed,
    executor: Deployed,
}

fn take(nonce: &mut u64) -> u64 {
    let n = *nonce;
    *nonce += 1;
    n
}

fn load_hex(rel: &str) -> Vec<u8> {
    let raw = std::fs::read_to_string(repo_root().join(rel))
        .unwrap_or_else(|e| panic!("read {rel}: {e}"));
    let text = raw
        .trim()
        .strip_prefix("0x")
        .unwrap_or(raw.trim())
        .to_string();
    alloy::hex::decode(&text).unwrap_or_else(|e| panic!("hex-decode {rel}: {e}"))
}

fn load_stub_creation(name: &str) -> Vec<u8> {
    let rel = format!("tier3-oracle/artifacts/harness/{name}.json");
    let raw = std::fs::read_to_string(repo_root().join(&rel))
        .unwrap_or_else(|e| panic!("read {rel}: {e}"));
    degenbot_simulation::oracle::parse_foundry_creation_bytecode(&raw)
        .unwrap_or_else(|e| panic!("parse {name}: {e}"))
}

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../..")
}

/// A V2 constant-product output quote (0.3% fee).
fn v2_out(input: u128, reserve_in: u128, reserve_out: u128) -> u128 {
    let with_fee = U256::from(input) * U256::from(997u64);
    let num = with_fee * U256::from(reserve_out);
    let den = U256::from(reserve_in) * U256::from(1000u64) + with_fee;
    u128::try_from(num / den).expect("v2 quote fits u128")
}

fn call_commit(
    evm: &mut Evm<'static, BaseEvmTypes>,
    from: Address,
    to: Address,
    data: Vec<u8>,
    nonce: u64,
) -> Result<(), String> {
    let frame = envelope(from, TxKind::Call(to), Bytes::from(data), 30_000_000, nonce);
    let executed = evm
        .transact(&frame)
        .map_err(|e| format!("setup call to {to}: {e:?}"))?;
    let result = executed.commit();
    if !result.status {
        return Err(format!("setup call to {to} failed: {:?}", result.stop));
    }
    Ok(())
}

/// The shared hot set, deployed once (production shape) or per candidate
/// (the disjoint upper bound).
fn deploy_world(setup: &mut Evm<'static, BaseEvmTypes>, nonce: &mut u64) -> Result<World, String> {
    let weth = deploy(setup, Bytes::from(load_stub_creation("Token")), take(nonce))?;
    let pool_manager = deploy(
        setup,
        Bytes::from(load_stub_creation("PoolManager")),
        take(nonce),
    )?;
    let mut executor_init = load_hex("tier3-oracle/artifacts/executor/cmd_executor.creation.hex");
    executor_init.extend_from_slice(
        &(
            weth.address,
            pool_manager.address,
            Address::repeat_byte(0x33),
            Address::repeat_byte(0x44),
        )
            .abi_encode(),
    );
    let executor = deploy(setup, Bytes::from(executor_init), take(nonce))?;
    call_commit(
        setup,
        DEPLOYER,
        weth.address,
        BenchToken::mintCall {
            to: executor.address,
            amount: U256::from(10_000_000_000_000_000_000u128),
        }
        .abi_encode(),
        take(nonce),
    )?;
    Ok(World {
        weth,
        pool_manager,
        executor,
    })
}

fn init_pair(
    setup: &mut Evm<'static, BaseEvmTypes>,
    pair: Address,
    weth: Address,
    token: Address,
    nonce: &mut u64,
) -> Result<(), String> {
    call_commit(
        setup,
        DEPLOYER,
        pair,
        BenchPair::initializeCall {
            tokenA: weth,
            tokenB: token,
        }
        .abi_encode(),
        take(nonce),
    )
}

fn seed_pair(
    setup: &mut Evm<'static, BaseEvmTypes>,
    pair: Address,
    weth: Address,
    token: Address,
    weth_side: u128,
    tkn_side: u128,
    nonce: &mut u64,
) -> Result<(), String> {
    for (asset, amount) in [(weth, weth_side), (token, tkn_side)] {
        call_commit(
            setup,
            DEPLOYER,
            asset,
            BenchToken::mintCall {
                to: pair,
                amount: U256::from(amount),
            }
            .abi_encode(),
            take(nonce),
        )?;
    }
    call_commit(
        setup,
        DEPLOYER,
        pair,
        BenchPair::syncCall {}.abi_encode(),
        take(nonce),
    )
}

/// One candidate: its own token + two pairs (the disjoint deep slots), frame
/// encoded through the real `cmd_executor` grammar at the knob's depth.
fn build_candidate(
    setup: &mut Evm<'static, BaseEvmTypes>,
    nonce: &mut u64,
    world: &World,
    knob: Knob,
) -> Result<Candidate, String> {
    let token = deploy(setup, Bytes::from(load_stub_creation("Token")), take(nonce))?;
    let pair_a = deploy(setup, Bytes::from(load_stub_creation("Pair")), take(nonce))?;
    let pair_b = deploy(setup, Bytes::from(load_stub_creation("Pair")), take(nonce))?;

    // pair_a prices TKN at 1 WETH, pair_b at 2 — the dislocation the cycle
    // harvests. The depth knobs leave one pool unseeded (its swap reverts):
    // hop-1 unseeded = early, hop-2 unseeded = mid.
    if knob != Knob::EarlyRevert {
        init_pair(
            setup,
            pair_a.address,
            world.weth.address,
            token.address,
            nonce,
        )?;
        seed_pair(
            setup,
            pair_a.address,
            world.weth.address,
            token.address,
            1_000_000_000_000_000_000_000,
            1_000_000_000_000_000_000_000,
            nonce,
        )?;
    }
    if knob != Knob::MidRevert {
        init_pair(
            setup,
            pair_b.address,
            world.weth.address,
            token.address,
            nonce,
        )?;
        seed_pair(
            setup,
            pair_b.address,
            world.weth.address,
            token.address,
            1_000_000_000_000_000_000_000,
            500_000_000_000_000_000_000,
            nonce,
        )?;
    }

    // The executor approves both pairs (a frame originating from the
    // executor contract itself, the bench corpus's setup shape).
    for pair in [pair_a.address, pair_b.address] {
        for asset in [world.weth.address, token.address] {
            call_commit(
                setup,
                world.executor.address,
                asset,
                BenchToken::approveCall {
                    spender: pair,
                    amount: U256::MAX,
                }
                .abi_encode(),
                0,
            )?;
        }
    }

    let optimal_input = 1_000_000_000_000_000u128;
    let hop1_out = v2_out(
        optimal_input,
        1_000_000_000_000_000_000_000,
        1_000_000_000_000_000_000_000,
    );
    let hop2_out = v2_out(
        hop1_out,
        500_000_000_000_000_000_000,
        1_000_000_000_000_000_000_000,
    );

    let (t0, t1) = if world.weth.address < token.address {
        (world.weth.address, token.address)
    } else {
        (token.address, world.weth.address)
    };
    let weth_zfo = world.weth.address < token.address;
    let hop = |pair: Address, zfo: bool| {
        HopInfo::V2(V2HopInfo {
            pool_address: pair,
            token0_address: t0,
            token1_address: t1,
            fee: 30,
            zfo,
        })
    };
    let path = PathInfo::new(vec![
        hop(pair_a.address, weth_zfo),
        hop(pair_b.address, !weth_zfo),
    ]);

    let cmd = degenbot_executor::composers::encode_cmd_stream(
        &EncodeContext::new(
            world.executor.address,
            world.pool_manager.address,
            world.weth.address,
        ),
        &EncodeRequest::new(
            path,
            optimal_input,
            vec![hop1_out, hop2_out],
            vec![optimal_input, hop1_out],
            EncodeOptions::default(),
        ),
    )
    .ok_or_else(|| "encode_cmd_stream returned None".to_string())?;
    // config bits 0-7 = profit_mode (1 = physical WETH + native ETH — the
    // measured production mode), 8-23 = bribe_bips, 32-255 = min_profit.
    // A zero config reverts at entry with InvalidProfitMode(0) — the defect
    // `bench_corpus`'s encode carries (its frames revert at entry; its gates
    // only assert verdict identity, so nothing noticed).
    let min_profit = match knob {
        // An unreachable floor: `InsufficientProfit` AFTER every command and
        // the balance measurement — the late (CurrencyNotSettled) depth.
        Knob::LateRevert => U256::from(10).pow(U256::from(30)),
        _ => U256::ZERO,
    };
    let config = (min_profit << 32) | U256::from(1u64);
    let data = degenbot_executor::composers::encode_execute_call(Address::ZERO, &cmd, config).data;

    Ok(Candidate {
        frame: envelope(
            DEPLOYER,
            TxKind::Call(world.executor.address),
            Bytes::from(data),
            EXECUTE_GAS,
            0,
        ),
        knob,
        pools: vec![pair_a.address, pair_b.address],
    })
}

/// The corpus shapes: production (one hot set, pool-disjoint deep slots),
/// lockstep (one frame repeated — every read shared), disjoint (a private
/// world per candidate — the fan-out upper bound).
enum Shape {
    Production,
    Lockstep,
    Disjoint,
}

fn shape_of(name: &str) -> Shape {
    match name {
        "lockstep" => Shape::Lockstep,
        "disjoint" => Shape::Disjoint,
        _ => Shape::Production,
    }
}

fn build_corpus(
    setup: &mut Evm<'static, BaseEvmTypes>,
    candidates: usize,
    shape: &Shape,
) -> Result<Vec<Candidate>, String> {
    let mut nonce = 0u64;
    match shape {
        Shape::Lockstep => {
            let world = deploy_world(setup, &mut nonce)?;
            let one = build_candidate(setup, &mut nonce, &world, Knob::Healthy)?;
            Ok((0..candidates)
                .map(|_| Candidate {
                    frame: one.frame.clone(),
                    knob: one.knob,
                    pools: one.pools.clone(),
                })
                .collect())
        }
        Shape::Production => {
            let world = deploy_world(setup, &mut nonce)?;
            (0..candidates)
                .map(|i| build_candidate(setup, &mut nonce, &world, knob_at(i)))
                .collect()
        }
        Shape::Disjoint => (0..candidates)
            .map(|i| {
                let world = deploy_world(setup, &mut nonce)?;
                build_candidate(setup, &mut nonce, &world, knob_at(i))
            })
            .collect(),
    }
}

// ── The drives ────────────────────────────────────────────────────────────

struct LegResult {
    label: &'static str,
    wall: Duration,
    charged: Duration,
    accounts: u64,
    storage: u64,
    memo_hits: u64,
    rounds: u64,
    max_batch: usize,
    verdicts: Vec<Verdict>,
}

fn tally(counters: &Counters) -> (u64, u64, u64) {
    (
        counters.account_forwards.load(Ordering::Relaxed),
        counters.storage_forwards.load(Ordering::Relaxed),
        counters.memo_hits.load(Ordering::Relaxed),
    )
}

/// Today's shape: ONE `Evm` + ONE `CacheDB` over the shared memo, candidates
/// serial, each candidate's writes discarded (per-path isolation).
fn drive_serial(
    candidates: &[Candidate],
    snapshot: &Arc<Snapshot>,
    memo: &Arc<SharedMemo>,
    storage_latency: Duration,
    account_latency: Duration,
) -> Result<LegResult, String> {
    let counters = Arc::new(Counters::default());
    let cold = ColdDb {
        snapshot: Arc::clone(snapshot),
        latency: storage_latency,
        account_latency,
        counters: Arc::clone(&counters),
    };
    let mut evm: Evm<'static, BaseEvmTypes> = policy_evm(CacheDB::new(Db::new(MemoDb {
        cold,
        memo: Arc::clone(memo),
    })));
    let started = Instant::now();
    let mut verdicts = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let executed = evm
            .transact(&candidate.frame)
            .map_err(|e| format!("serial frame: {e:?}"))?;
        verdicts.push(Verdict::of(&executed.discard()));
    }
    let wall = started.elapsed();
    let (accounts, storage, memo_hits) = tally(&counters);
    Ok(LegResult {
        label: "serial",
        wall,
        charged: counters.charge(account_latency, storage_latency),
        accounts,
        storage,
        memo_hits,
        rounds: 0,
        max_batch: 0,
        verdicts,
    })
}

/// One `Evm` + `CacheDB` per worker thread, the memo shared; candidates
/// round-robin (the per-worker partition the dispatch would adopt).
fn drive_threads(
    workers: usize,
    candidates: &[Candidate],
    snapshot: &Arc<Snapshot>,
    memo: &Arc<SharedMemo>,
    storage_latency: Duration,
    account_latency: Duration,
) -> Result<LegResult, String> {
    let started = Instant::now();
    let per_worker: Vec<Arc<Counters>> = (0..workers)
        .map(|_| Arc::new(Counters::default()))
        .collect();
    let verdicts = Mutex::new(vec![None; candidates.len()]);
    let failure = Mutex::new(None::<String>);

    std::thread::scope(|scope| {
        for (worker, counters) in per_worker.iter().enumerate() {
            let snapshot = Arc::clone(snapshot);
            let memo = Arc::clone(memo);
            let counters = Arc::clone(counters);
            let verdicts = &verdicts;
            let failure = &failure;
            let mine: Vec<(usize, &Candidate)> = candidates
                .iter()
                .enumerate()
                .filter(|(index, _)| index % workers == worker)
                .collect();
            scope.spawn(move || {
                let cold = ColdDb {
                    snapshot,
                    latency: storage_latency,
                    account_latency,
                    counters,
                };
                let mut evm: Evm<'static, BaseEvmTypes> =
                    policy_evm(CacheDB::new(Db::new(MemoDb { cold, memo })));
                for (index, candidate) in mine {
                    match evm.transact(&candidate.frame) {
                        Ok(executed) => {
                            let verdict = Verdict::of(&executed.discard());
                            verdicts.lock().expect("verdicts")[index] = Some(verdict);
                        }
                        Err(e) => {
                            let mut failure = failure.lock().expect("failure");
                            if failure.is_none() {
                                *failure = Some(format!("thread frame {index}: {e:?}"));
                            }
                            return;
                        }
                    }
                }
            });
        }
    });

    if let Some(error) = failure.into_inner().expect("failure") {
        return Err(error);
    }
    let wall = started.elapsed();
    let verdicts = into_verdicts(verdicts.into_inner().expect("verdicts"));
    let mut accounts = 0u64;
    let mut storage = 0u64;
    let mut memo_hits = 0u64;
    let mut charged = Duration::ZERO;
    for counters in &per_worker {
        let (a, s, m) = tally(counters);
        accounts += a;
        storage += s;
        memo_hits += m;
        charged = charged.max(counters.charge(account_latency, storage_latency));
    }
    Ok(LegResult {
        label: "threads",
        wall,
        charged,
        accounts,
        storage,
        memo_hits,
        rounds: 0,
        max_batch: 0,
        verdicts,
    })
}

fn into_verdicts(slots: Vec<Option<Verdict>>) -> Vec<Verdict> {
    slots
        .into_iter()
        .enumerate()
        .map(|(index, slot)| {
            slot.unwrap_or_else(|| panic!("candidate {index} produced no verdict"))
        })
        .collect()
}

// ── The async stand-in + the batched-round scheduler ──────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ReadKey {
    Account(Address),
    ChainCode(B256),
    Storage(Address, Word),
    BlockHash(U256),
}

#[derive(Clone, Debug)]
enum Answer {
    Account(Option<AccountInfo>),
    Code(Bytes),
    Word(Word),
    BlockHash(B256),
}

#[derive(Debug, Default)]
struct BatchState {
    queued: Vec<ReadKey>,
    answers: HashMap<ReadKey, Answer>,
    waker: Option<Waker>,
    rounds: u64,
    batch_sizes: Vec<usize>,
    account_reads: u64,
    storage_reads: u64,
    memo_hits: u64,
}

struct FanOutCtx {
    snapshot: Arc<Snapshot>,
    state: Mutex<BatchState>,
}

impl FanOutCtx {
    fn new(snapshot: Arc<Snapshot>, seed: HashMap<ReadKey, Answer>) -> Self {
        Self {
            snapshot,
            state: Mutex::new(BatchState {
                answers: seed,
                ..Default::default()
            }),
        }
    }

    fn resolve(&self, key: &ReadKey) -> Answer {
        match key {
            ReadKey::Account(a) => Answer::Account(self.snapshot.info(*a)),
            ReadKey::ChainCode(h) => {
                Answer::Code(self.snapshot.code.get(h).cloned().unwrap_or_default())
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

/// One pending read: settled by the scheduler's batch, never by itself.
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
        if !state.queued.contains(&self.key) {
            state.queued.push(self.key.clone());
        }
        state.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

/// The `AsyncDatabase` over the batcher — ONE round trip per account read
/// (the production adapter's batched triple), one per storage read.
struct FanOutDb {
    ctx: Arc<FanOutCtx>,
}

impl FanOutDb {
    async fn read(&self, key: ReadKey) -> Answer {
        {
            // Per-READ accounting: a read served from the settled answers is
            // a memo hit; one that goes to the batch is a forward.
            let mut state = self.ctx.state.lock().expect("batch state");
            if state.answers.contains_key(&key) {
                state.memo_hits += 1;
            } else {
                match &key {
                    ReadKey::Account(_) | ReadKey::ChainCode(_) => state.account_reads += 1,
                    ReadKey::Storage(_, _) | ReadKey::BlockHash(_) => state.storage_reads += 1,
                }
            }
        }
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
        Ok(self.read(ReadKey::Account(address)).await.account())
    }

    async fn get_code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        Ok(Bytecode::new_raw(
            self.read(ReadKey::ChainCode(code_hash)).await.code(),
        ))
    }

    async fn get_storage(&mut self, address: Address, key: Word) -> Result<Word, Self::Error> {
        Ok(self.read(ReadKey::Storage(address, key)).await.word())
    }

    async fn get_block_hash(&mut self, number: U256) -> Result<B256, Self::Error> {
        Ok(self.read(ReadKey::BlockHash(number)).await.block_hash())
    }
}

impl Answer {
    fn account(self) -> Option<AccountInfo> {
        match self {
            Self::Account(a) => a,
            _ => panic!("answer is not an account"),
        }
    }

    fn word(self) -> Word {
        match self {
            Self::Word(w) => w,
            _ => panic!("answer is not a word"),
        }
    }

    fn block_hash(self) -> B256 {
        match self {
            Self::BlockHash(h) => h,
            _ => panic!("answer is not a block hash"),
        }
    }

    fn code(self) -> Bytes {
        match self {
            Self::Code(c) => c,
            _ => panic!("answer is not code"),
        }
    }
}

type FrameFuture = Pin<Box<dyn Future<Output = Result<Verdict, String>>>>;

/// One Evm per candidate on its own fiber, all sharing the batcher.
fn build_fan_out(ctx: &Arc<FanOutCtx>, candidates: &[Candidate]) -> Vec<FrameFuture> {
    candidates
        .iter()
        .map(|candidate| {
            let db = CacheDB::new(AsyncDb::new(FanOutDb {
                ctx: Arc::clone(ctx),
            }));
            let evm: &'static mut Evm<'static, BaseEvmTypes> = Box::leak(Box::new(policy_evm(db)));
            let frame: &'static RecoveredTxEnvelope = Box::leak(Box::new(candidate.frame.clone()));
            Box::pin(async move {
                let executed = evm
                    .transact_async(frame)
                    .await
                    .map_err(|e| format!("async transact: {e}"))?;
                Ok(Verdict::of(&executed.discard()))
            }) as FrameFuture
        })
        .collect()
}

/// Drive fiber futures: poll every unfinished fiber once per round, then
/// settle the whole queued read set in ONE modeled round trip. Shared by the
/// corpus legs and the recorded-fan-out replay.
fn settle_fibers(
    mut frames: Vec<FrameFuture>,
    ctx: &Arc<FanOutCtx>,
    storage_latency: Duration,
) -> Result<(Duration, Vec<Verdict>), String> {
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut verdicts: Vec<Option<Verdict>> = vec![None; frames.len()];
    let mut remaining = frames.len();
    let started = Instant::now();
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
            return Err("deadlock: every fiber is blocked with no queued read".into());
        }
        // ONE modeled round trip for the whole batch.
        std::thread::sleep(storage_latency);
        let mut state = ctx.state.lock().expect("batch state");
        state.rounds += 1;
        state.batch_sizes.push(batch.len());
        for key in &batch {
            let answer = ctx.resolve(key);
            state.answers.insert(key.clone(), answer);
        }
        if let Some(waker) = &state.waker {
            waker.wake_by_ref();
        }
    }
    Ok((started.elapsed(), into_verdicts(verdicts)))
}

/// The fiber drive's report facts out of the shared batcher.
fn fiber_leg(
    label: &'static str,
    ctx: &FanOutCtx,
    storage_latency: Duration,
    wall: Duration,
    verdicts: Vec<Verdict>,
) -> (LegResult, HashMap<ReadKey, Answer>) {
    let state = ctx.state.lock().expect("batch state");
    let rounds = state.rounds;
    let max_batch = state.batch_sizes.iter().copied().max().unwrap_or(0);
    (
        LegResult {
            label,
            wall,
            charged: storage_latency * u32::try_from(rounds).unwrap_or(u32::MAX),
            accounts: state.account_reads,
            storage: state.storage_reads,
            memo_hits: state.memo_hits,
            rounds,
            max_batch,
            verdicts,
        },
        state.answers.clone(),
    )
}

fn drive_fibers(
    candidates: &[Candidate],
    snapshot: &Arc<Snapshot>,
    storage_latency: Duration,
    seed: HashMap<ReadKey, Answer>,
) -> Result<(LegResult, HashMap<ReadKey, Answer>), String> {
    let ctx = Arc::new(FanOutCtx::new(Arc::clone(snapshot), seed));
    let frames = build_fan_out(&ctx, candidates);
    let (wall, verdicts) = settle_fibers(frames, &ctx, storage_latency)?;
    Ok(fiber_leg("fibers", &ctx, storage_latency, wall, verdicts))
}

// ── Report ────────────────────────────────────────────────────────────────

fn us(duration: Duration) -> u128 {
    duration.as_micros()
}

fn report_leg(result: &LegResult) {
    let extra = if result.rounds > 0 {
        format!(" rounds={} max_batch={}", result.rounds, result.max_batch)
    } else {
        String::new()
    };
    println!(
        "  [{:<11}] wall={:>7}us charged={:>7}us forwards: acct={:<4} storage={:<5} memo_hits={:<5}{}",
        result.label,
        us(result.wall),
        us(result.charged),
        result.accounts,
        result.storage,
        result.memo_hits,
        extra,
    );
}

#[expect(clippy::too_many_lines)] // the probe's leg sequence + corpus build + gate read top-down
fn main() {
    let storage_latency = storage_latency();
    let account_latency = account_latency();
    let candidates_count = candidate_count();
    let workers = worker_count();
    let shape_name = std::env::var("WORKER_FANOUT_SHAPE").unwrap_or_else(|_| "production".into());
    let shape = shape_of(&shape_name);

    if let Ok(trace_path) = std::env::var("WORKER_FANOUT_TRACE") {
        if let Err(e) = replay_main(&trace_path, workers, storage_latency, account_latency) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return;
    }

    // ── the corpus ────────────────────────────────────────────────────────
    let mut setup = bench_evm(false);
    let candidates = match build_corpus(&mut setup, candidates_count, &shape) {
        Ok(built) => built,
        Err(e) => {
            eprintln!("corpus build failed: {e}");
            std::process::exit(2);
        }
    };
    let snapshot = Arc::new(Snapshot::of(&setup.state_mut().overlay_db().cache));

    println!(
        "worker_fanout_probe: shape={shape_name} cands={candidates_count} workers={workers} \
         modeled storage {}us / account {}us (snapshot {} accounts / {} code / {} slots)",
        us(storage_latency),
        us(account_latency),
        snapshot.accounts.len(),
        snapshot.code.len(),
        snapshot.storage.len(),
    );
    let mut knob_mix: Vec<(Knob, usize)> = Vec::new();
    for candidate in &candidates {
        match knob_mix
            .iter_mut()
            .find(|(knob, _)| *knob == candidate.knob)
        {
            Some((_, count)) => *count += 1,
            None => knob_mix.push((candidate.knob, 1)),
        }
    }
    println!(
        "  corpus knobs: {}",
        knob_mix
            .iter()
            .map(|(knob, count)| format!("{knob:?}={count}"))
            .collect::<Vec<_>>()
            .join(" ")
    );

    // ── the legs ──────────────────────────────────────────────────────────
    // Every leg starts on a FRESH memo (leg-scoped): the comparison is
    // cold-fan-out vs cold-fan-out. The shared memo across blocks is
    // production's warm regime and would measure memo reuse, not the drive.
    let serial = match drive_serial(
        &candidates,
        &snapshot,
        &Arc::new(SharedMemo::default()),
        storage_latency,
        account_latency,
    ) {
        Ok(result) => result,
        Err(e) => {
            eprintln!("serial leg failed: {e}");
            std::process::exit(2);
        }
    };
    let threads = match drive_threads(
        workers,
        &candidates,
        &snapshot,
        &Arc::new(SharedMemo::default()),
        storage_latency,
        account_latency,
    ) {
        Ok(result) => result,
        Err(e) => {
            eprintln!("threads leg failed: {e}");
            std::process::exit(2);
        }
    };
    let fibers = match drive_fibers(&candidates, &snapshot, storage_latency, HashMap::new()) {
        Ok((result, answers)) => (result, answers),
        Err(e) => {
            eprintln!("fibers leg failed: {e}");
            std::process::exit(2);
        }
    };
    let (fibers, answers) = fibers;
    let mut fibers_warm = match drive_fibers(&candidates, &snapshot, storage_latency, answers) {
        Ok((result, _)) => result,
        Err(e) => {
            eprintln!("fibers-warm leg failed: {e}");
            std::process::exit(2);
        }
    };
    fibers_warm.label = "fibers-warm";

    println!("\nlegs:");
    report_leg(&serial);
    report_leg(&threads);
    report_leg(&fibers);
    report_leg(&fibers_warm);

    // The measured production rows for comparison (run_bot.sh drive,
    // burst fan-outs): fanout_ms 104/57/274 at 21/50/49 candidates.
    println!("\n  serial verdict mix: {}", {
        let mut ok = 0usize;
        let mut revert = 0usize;
        for verdict in &serial.verdicts {
            if verdict.status {
                ok += 1;
            } else {
                revert += 1;
            }
        }
        format!("ok={ok} revert={revert}")
    });

    if std::env::var("WORKER_FANOUT_DEBUG").is_ok() {
        println!("\ndebug: baseline per-candidate outcomes:");
        for (index, (candidate, verdict)) in candidates.iter().zip(&serial.verdicts).enumerate() {
            let revert = String::from_utf8_lossy(&verdict.raw);
            println!(
                "  [{index:>2}] {:?} ({} pools) status={} gas={} stop={} raw={}",
                candidate.knob,
                candidate.pools.len(),
                verdict.status,
                verdict.gas,
                verdict.stop,
                if revert.contains('{') || revert.contains('(') {
                    revert.chars().take(120).collect::<String>()
                } else {
                    format!("0x{}", alloy::hex::encode(&verdict.raw))
                }
            );
        }
    }

    // ── the gate ──────────────────────────────────────────────────────────
    let baseline = serial.verdicts.clone();
    let mut legs: Vec<(&str, Vec<Verdict>)> = vec![
        ("threads", threads.verdicts.clone()),
        ("fibers", fibers.verdicts.clone()),
        ("fibers-warm", fibers_warm.verdicts.clone()),
    ];
    if std::env::var("WORKER_FANOUT_PLANT_DIVERGENCE").is_ok() {
        // The gate's own sensitivity probe: one planted divergence must turn
        // the verdict RED (the negative-probe idiom for a machine-run gate).
        legs[0].1[0].gas = legs[0].1[0].gas.saturating_add(1);
    }
    let mismatches = identity_gate(&baseline, &legs);
    println!();
    if mismatches.is_empty() {
        println!(
            "identity gate: GREEN ({} legs x {} candidates)",
            legs.len(),
            baseline.len()
        );
    } else {
        println!("identity gate: RED ({} mismatches)", mismatches.len());
        for mismatch in &mismatches {
            println!("  {mismatch}");
        }
        std::process::exit(1);
    }
}

// ── Recorded-fan-out replay (Tier B) ──────────────────────────────────────

/// One recorded candidate: its read walk + cost facts. The read walk is the
/// EIP-2930 access list the simulation seam drained (reverts included);
/// `sim_ns` is the seam's measured wall (encode + sequence + assess).
#[derive(Clone)]
struct Scripted {
    block: u64,
    cand: String,
    sim_ns: u64,
    verdict: String,
    gas: u64,
    reads: Vec<(Address, Vec<Word>)>,
}

/// Parse one log line's trace row (the substring after `fanout-trace `).
fn parse_trace_line(line: &str) -> Option<Scripted> {
    let start = line.find("fanout-trace ")? + "fanout-trace ".len();
    let value: serde_json::Value = serde_json::from_str(line[start..].trim()).ok()?;
    let reads = value
        .get("reads")?
        .as_array()?
        .iter()
        .filter_map(|entry| {
            let mut parts = entry.as_str()?.split('|');
            let address = parts.next()?.parse::<Address>().ok()?;
            let slots = parts
                .next()?
                .split(',')
                .filter(|slot| !slot.is_empty())
                .filter_map(|slot| slot.parse::<Word>().ok())
                .collect();
            Some((address, slots))
        })
        .collect();
    Some(Scripted {
        block: value.get("block")?.as_u64()?,
        cand: value.get("cand")?.as_str()?.to_string(),
        sim_ns: value.get("sim_ns")?.as_u64()?,
        verdict: value.get("verdict")?.as_str()?.to_string(),
        gas: value.get("gas")?.as_u64()?,
        reads,
    })
}

/// Load trace rows from a log or JSONL file, grouped into fan-outs: one per
/// recorded block in file order, or fixed-size chunks when
/// `WORKER_FANOUT_TRACE_CHUNK` names a size (the dispatch's bimodal batch
/// shape — rolling ones plus capped bursts — is recoverable either way).
fn load_trace_groups(path: &str) -> Result<Vec<Vec<Scripted>>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
    let rows: Vec<Scripted> = text.lines().filter_map(parse_trace_line).collect();
    if rows.is_empty() {
        return Err(format!("no fanout-trace rows in {path}"));
    }
    if let Some(size) = std::env::var("WORKER_FANOUT_TRACE_CHUNK")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
    {
        return Ok(rows.chunks(size).map(<[Scripted]>::to_vec).collect());
    }
    let mut groups: Vec<Vec<Scripted>> = Vec::new();
    let mut current_block = None::<u64>;
    let mut current: Vec<Scripted> = Vec::new();
    for row in rows {
        if current_block != Some(row.block) {
            if !current.is_empty() {
                groups.push(std::mem::take(&mut current));
            }
            current_block = Some(row.block);
        }
        current.push(row);
    }
    if !current.is_empty() {
        groups.push(current);
    }
    Ok(groups)
}

/// The replay's modeled node: every recorded key present, its value derived
/// deterministically from the key (identical across legs, so the digest gate
/// catches a dropped or duplicated read).
fn replay_snapshot(rows: &[Scripted]) -> Arc<Snapshot> {
    let mut snapshot = Snapshot::default();
    for (address, slots) in rows.iter().flat_map(|row| &row.reads) {
        let code_hash = keccak256(address.as_slice());
        snapshot
            .accounts
            .insert(*address, (Word::from(1u64), 1, code_hash));
        snapshot
            .code
            .insert(code_hash, Bytes::copy_from_slice(address.as_slice()));
        for slot in slots {
            let mut preimage = [0u8; 52];
            preimage[..20].copy_from_slice(address.as_slice());
            preimage[20..].copy_from_slice(&slot.to_be_bytes::<32>());
            snapshot
                .storage
                .insert((*address, *slot), keccak256(preimage).into());
        }
    }
    Arc::new(snapshot)
}

/// Issue one recorded candidate's read walk against `db`; the digest of every
/// answer is the leg's verdict identity.
fn walk_row<D: ::evm2::evm::Database>(db: &mut D, row: &Scripted) -> Result<Verdict, String>
where
    D::Error: std::fmt::Debug,
{
    let mut material: Vec<u8> = Vec::new();
    for (address, slots) in &row.reads {
        let info = db
            .get_account(address)
            .map_err(|e| format!("replay account read: {e:?}"))?;
        if let Some(info) = info {
            material.extend_from_slice(&info.balance.to_be_bytes::<32>());
            material.extend_from_slice(&info.nonce.to_be_bytes());
            material.extend_from_slice(info.code_hash.as_slice());
        }
        for slot in slots {
            let value = db
                .get_storage(address, slot)
                .map_err(|e| format!("replay storage read: {e:?}"))?;
            material.extend_from_slice(&value.to_be_bytes::<32>());
        }
    }
    exec_pause(row.sim_ns);
    Ok(Verdict {
        status: row.verdict == "result",
        gas: row.gas,
        output: keccak256(material),
        raw: Bytes::new(),
        stop: row.verdict.clone(),
    })
}

/// The recorded execution cost of a group (the seam's measured walls; the
/// replay always charges it, and `WORKER_FANOUT_EXEC_SLEEP=1` also SPENDS it
/// as wall — the mode whose walls are directly comparable to the live
/// `fanout_ms` deltas, and the one where execution can overlap across workers).
fn exec_ns(group: &[Scripted]) -> Duration {
    Duration::from_nanos(group.iter().map(|row| row.sim_ns).sum())
}

/// Spend one candidate's recorded execution as wall when the knob is on.
fn exec_pause(sim_ns: u64) {
    if std::env::var("WORKER_FANOUT_EXEC_SLEEP").is_ok() {
        std::thread::sleep(Duration::from_nanos(sim_ns));
    }
}

fn replay_serial(
    group: &[Scripted],
    snapshot: &Arc<Snapshot>,
    memo: &Arc<SharedMemo>,
    storage_latency: Duration,
    account_latency: Duration,
) -> Result<LegResult, String> {
    let counters = Arc::new(Counters::default());
    let mut db = MemoDb {
        cold: ColdDb {
            snapshot: Arc::clone(snapshot),
            latency: storage_latency,
            account_latency,
            counters: Arc::clone(&counters),
        },
        memo: Arc::clone(memo),
    };
    let started = Instant::now();
    let mut verdicts = Vec::with_capacity(group.len());
    for row in group {
        verdicts.push(walk_row(&mut db, row)?);
    }
    let wall = started.elapsed();
    let (accounts, storage, memo_hits) = tally(&counters);
    Ok(LegResult {
        label: "serial",
        wall,
        charged: counters.charge(account_latency, storage_latency) + exec_ns(group),
        accounts,
        storage,
        memo_hits,
        rounds: 0,
        max_batch: 0,
        verdicts,
    })
}

fn replay_threads(
    workers: usize,
    group: &[Scripted],
    snapshot: &Arc<Snapshot>,
    memo: &Arc<SharedMemo>,
    storage_latency: Duration,
    account_latency: Duration,
) -> Result<LegResult, String> {
    let started = Instant::now();
    let per_worker: Vec<Arc<Counters>> = (0..workers)
        .map(|_| Arc::new(Counters::default()))
        .collect();
    let verdicts = Mutex::new(vec![None; group.len()]);
    let failure = Mutex::new(None::<String>);
    std::thread::scope(|scope| {
        for (worker, counters) in per_worker.iter().enumerate() {
            let snapshot = Arc::clone(snapshot);
            let memo = Arc::clone(memo);
            let counters = Arc::clone(counters);
            let verdicts = &verdicts;
            let failure = &failure;
            let mine: Vec<(usize, &Scripted)> = group
                .iter()
                .enumerate()
                .filter(|(index, _)| index % workers == worker)
                .collect();
            scope.spawn(move || {
                let mut db = MemoDb {
                    cold: ColdDb {
                        snapshot,
                        latency: storage_latency,
                        account_latency,
                        counters,
                    },
                    memo,
                };
                for (index, row) in mine {
                    match walk_row(&mut db, row) {
                        Ok(verdict) => {
                            verdicts.lock().expect("verdicts")[index] = Some(verdict);
                        }
                        Err(e) => {
                            let mut failure = failure.lock().expect("failure");
                            if failure.is_none() {
                                *failure = Some(format!("replay row {index}: {e}"));
                            }
                            return;
                        }
                    }
                }
            });
        }
    });
    if let Some(error) = failure.into_inner().expect("failure") {
        return Err(error);
    }
    let wall = started.elapsed();
    let mut accounts = 0u64;
    let mut storage = 0u64;
    let mut memo_hits = 0u64;
    let mut charged = Duration::ZERO;
    for counters in &per_worker {
        let (a, s, m) = tally(counters);
        accounts += a;
        storage += s;
        memo_hits += m;
        charged = charged.max(counters.charge(account_latency, storage_latency));
    }
    Ok(LegResult {
        label: "threads",
        wall,
        charged: charged + exec_ns(group),
        accounts,
        storage,
        memo_hits,
        rounds: 0,
        max_batch: 0,
        verdicts: into_verdicts(verdicts.into_inner().expect("verdicts")),
    })
}

/// One recorded candidate on its own fiber: the read walk through the
/// batched-round `AsyncDatabase`, then the digest.
fn replay_fiber(ctx: &Arc<FanOutCtx>, row: &Scripted) -> FrameFuture {
    let db = FanOutDb {
        ctx: Arc::clone(ctx),
    };
    let row = row.clone();
    Box::pin(async move {
        let mut db = db;
        let mut material: Vec<u8> = Vec::new();
        for (address, slots) in &row.reads {
            let info = db
                .get_account(*address)
                .await
                .map_err(|e| format!("replay account read: {e}"))?;
            if let Some(info) = info {
                material.extend_from_slice(&info.balance.to_be_bytes::<32>());
                material.extend_from_slice(&info.nonce.to_be_bytes());
                material.extend_from_slice(info.code_hash.as_slice());
            }
            for slot in slots {
                let value = db
                    .get_storage(*address, *slot)
                    .await
                    .map_err(|e| format!("replay storage read: {e}"))?;
                material.extend_from_slice(&value.to_be_bytes::<32>());
            }
        }
        exec_pause(row.sim_ns);
        Ok(Verdict {
            status: row.verdict == "result",
            gas: row.gas,
            output: keccak256(material),
            raw: Bytes::new(),
            stop: row.verdict.clone(),
        }) as Result<Verdict, String>
    }) as FrameFuture
}

fn build_replay_fan_out(ctx: &Arc<FanOutCtx>, group: &[Scripted]) -> Vec<FrameFuture> {
    group.iter().map(|row| replay_fiber(ctx, row)).collect()
}

/// One hybrid worker's report facts.
struct WorkerFacts {
    rounds: u64,
    max_batch: usize,
    accounts: u64,
    storage: u64,
    memo_hits: u64,
    charged: Duration,
}

/// The hybrid leg: `workers` OS threads, each driving its own LOCAL fiber set
/// over its own batcher (the integration shape — no `Send` verification, each
/// thread's EVMs are thread-local). Wall shows execution overlapping across
/// workers while each worker's reads batch into rounds.
fn replay_hybrid(
    workers: usize,
    group: &[Scripted],
    snapshot: &Arc<Snapshot>,
    storage_latency: Duration,
    account_latency: Duration,
) -> Result<LegResult, String> {
    let _ = account_latency;
    let started = Instant::now();
    let verdicts = Mutex::new(vec![None; group.len()]);
    let failure = Mutex::new(None::<String>);
    let facts: Mutex<Vec<WorkerFacts>> = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for worker in 0..workers {
            let snapshot = Arc::clone(snapshot);
            let verdicts = &verdicts;
            let failure = &failure;
            let facts = &facts;
            let mine: Vec<(usize, Scripted)> = group
                .iter()
                .enumerate()
                .filter(|(index, _)| index % workers == worker)
                .map(|(index, row)| (index, row.clone()))
                .collect();
            scope.spawn(move || {
                let ctx = Arc::new(FanOutCtx::new(snapshot, HashMap::new()));
                let frames: Vec<FrameFuture> = mine
                    .iter()
                    .map(|(_, row)| replay_fiber(&ctx, row))
                    .collect();
                let worker_exec: u64 = mine.iter().map(|(_, row)| row.sim_ns).sum();
                match settle_fibers(frames, &ctx, storage_latency) {
                    Ok((_, done)) => {
                        for ((index, _), verdict) in mine.iter().zip(done) {
                            verdicts.lock().expect("verdicts")[*index] = Some(verdict);
                        }
                        let state = ctx.state.lock().expect("batch state");
                        facts.lock().expect("facts").push(WorkerFacts {
                            rounds: state.rounds,
                            max_batch: state.batch_sizes.iter().copied().max().unwrap_or(0),
                            accounts: state.account_reads,
                            storage: state.storage_reads,
                            memo_hits: state.memo_hits,
                            charged: storage_latency
                                * u32::try_from(state.rounds).unwrap_or(u32::MAX)
                                + Duration::from_nanos(worker_exec),
                        });
                    }
                    Err(e) => {
                        let mut failure = failure.lock().expect("failure");
                        if failure.is_none() {
                            *failure = Some(format!("hybrid worker {worker}: {e}"));
                        }
                    }
                }
            });
        }
    });
    if let Some(error) = failure.into_inner().expect("failure") {
        return Err(error);
    }
    let wall = started.elapsed();
    let facts = facts.into_inner().expect("facts");
    let mut charged = Duration::ZERO;
    let mut rounds = 0u64;
    let mut max_batch = 0usize;
    let mut accounts = 0u64;
    let mut storage = 0u64;
    let mut memo_hits = 0u64;
    for fact in facts {
        charged = charged.max(fact.charged);
        rounds += fact.rounds;
        max_batch = max_batch.max(fact.max_batch);
        accounts += fact.accounts;
        storage += fact.storage;
        memo_hits += fact.memo_hits;
    }
    Ok(LegResult {
        label: "hybrid",
        wall,
        charged,
        accounts,
        storage,
        memo_hits,
        rounds,
        max_batch,
        verdicts: into_verdicts(verdicts.into_inner().expect("verdicts")),
    })
}

fn replay_group(
    group: &[Scripted],
    workers: usize,
    storage_latency: Duration,
    account_latency: Duration,
) -> Result<(), String> {
    let snapshot = replay_snapshot(group);
    let total_reads: usize = group
        .iter()
        .map(|row| row.reads.iter().map(|(_, s)| 1 + s.len()).sum::<usize>())
        .sum();
    let distinct = snapshot.accounts.len() + snapshot.storage.len();
    // Shared-read fraction in permille (integer math — the float cast lint).
    let overlap_permille = 1000u64 - (distinct as u64 * 1000 / (total_reads.max(1) as u64));
    println!(
        "\nfan-out: block={} cands={} (first {}) read overlap 0.{:03} ({} reads, {} distinct)",
        group[0].block,
        group.len(),
        group[0].cand,
        overlap_permille,
        total_reads,
        distinct,
    );

    let serial = replay_serial(
        group,
        &snapshot,
        &Arc::new(SharedMemo::default()),
        storage_latency,
        account_latency,
    )?;
    let threads = replay_threads(
        workers,
        group,
        &snapshot,
        &Arc::new(SharedMemo::default()),
        storage_latency,
        account_latency,
    )?;
    let (fibers, answers) = {
        let ctx = Arc::new(FanOutCtx::new(Arc::clone(&snapshot), HashMap::new()));
        let frames = build_replay_fan_out(&ctx, group);
        let (wall, verdicts) = settle_fibers(frames, &ctx, storage_latency)?;
        let (mut result, answers) = fiber_leg("fibers", &ctx, storage_latency, wall, verdicts);
        result.charged += exec_ns(group);
        (result, answers)
    };
    let fibers_warm = {
        let ctx = Arc::new(FanOutCtx::new(Arc::clone(&snapshot), answers));
        let frames = build_replay_fan_out(&ctx, group);
        let (wall, verdicts) = settle_fibers(frames, &ctx, storage_latency)?;
        let (mut result, _) = fiber_leg("fibers-warm", &ctx, storage_latency, wall, verdicts);
        result.charged += exec_ns(group);
        result
    };

    let hybrid = replay_hybrid(workers, group, &snapshot, storage_latency, account_latency)?;

    println!("legs:");
    report_leg(&serial);
    report_leg(&threads);
    report_leg(&fibers);
    report_leg(&hybrid);
    report_leg(&fibers_warm);

    let baseline = serial.verdicts.clone();
    let mut legs = vec![
        ("threads", threads.verdicts.clone()),
        ("fibers", fibers.verdicts.clone()),
        ("hybrid", hybrid.verdicts.clone()),
        ("fibers-warm", fibers_warm.verdicts.clone()),
    ];
    if std::env::var("WORKER_FANOUT_PLANT_DIVERGENCE").is_ok() {
        // The gate's own sensitivity probe: one planted divergence must turn
        // the verdict RED (the negative-probe idiom for a machine-run gate).
        legs[0].1[0].gas = legs[0].1[0].gas.saturating_add(1);
    }
    let mismatches = identity_gate(&baseline, &legs);
    if mismatches.is_empty() {
        println!(
            "identity gate: GREEN ({} legs x {} candidates)",
            legs.len(),
            baseline.len()
        );
        Ok(())
    } else {
        println!("identity gate: RED ({} mismatches)", mismatches.len());
        for mismatch in &mismatches {
            println!("  {mismatch}");
        }
        Err("replay identity gate red".to_string())
    }
}

/// The recorded-fan-out entry: one fan-out per recorded group, the same legs
/// and identity gate as the corpus mode.
fn replay_main(
    path: &str,
    workers: usize,
    storage_latency: Duration,
    account_latency: Duration,
) -> Result<(), String> {
    let groups = load_trace_groups(path)?;
    println!(
        "worker_fanout_probe: replay {path} — {} recorded fan-out(s), workers={workers}, modeled storage {}us / account {}us",
        groups.len(),
        us(storage_latency),
        us(account_latency),
    );
    let mut red = 0usize;
    for group in &groups {
        if let Err(e) = replay_group(group, workers, storage_latency, account_latency) {
            eprintln!("  group failed: {e}");
            red += 1;
        }
    }
    if red > 0 {
        Err(format!("{red} replay group(s) red"))
    } else {
        Ok(())
    }
}
