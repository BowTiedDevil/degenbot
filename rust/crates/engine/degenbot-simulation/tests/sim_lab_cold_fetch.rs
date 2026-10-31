//! `[sim-lab]` cold-fetch telemetry on the evm2 path: the counters
//! `degenbot-arbitrage`'s fan-out line reads (`sim::sim_metrics::snapshot` /
//! `delta` / `slow_total_ns` / `format_delta`) move on a cold sim and stay
//! put on a warm one.
//!
//! The counters are process-global atomics, so the whole ladder lives in ONE
//! test in its own binary — a sibling test would race the same statics.
//!
//! The stack under test is the production layer order over a mock cold leaf:
//! `CacheDB<WarmCodeCache<BotStateDb<MockCold>>>`. `WarmCodeCache` is the
//! `CacheDB`-miss seam (the deleted revm call site), so the mock sees exactly
//! the reads the RPC leaf would.

#![expect(clippy::expect_used)]

use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ::evm2::bytecode::Bytecode;
use ::evm2::ethereum::{RecoveredTxEnvelope, TxEnvelope};
use ::evm2::evm::{AccountInfo, CacheDB, Database, Db};
use ::evm2::interpreter::Word;
use alloy::consensus::transaction::Recovered;
use alloy::consensus::TxLegacy;
use alloy::primitives::{address, Address, Bytes, TxKind, B256, U256};
use degenbot_simulation::sim::evm2::bot_state_db::BotStateDb;
use degenbot_simulation::sim::evm2::session::Evm2SimSession;
use degenbot_simulation::sim::evm2::warm_code_cache::{WarmCodeCache, WarmCodeCacheInner};
use degenbot_simulation::sim::sim_metrics::{self, SimLabSnapshot};
use degenbot_substrate::NO_SIM_ANCHOR;
use parking_lot::RwLock;

const CONTRACT: Address = address!("0x1111111111111111111111111111111111111111");
const SENDER: Address = address!("0x3333333333333333333333333333333333333333");

/// The value the mock cold leaf answers for every slot read.
const SLOT_VALUE: u64 = 0x2A;

const BLOCK: u64 = 2050;
const TIMESTAMP: u64 = 1_780_000_000;
const BASE_FEE_WEI: u128 = 1_000_000_000;
/// Comfortably above `sim_metrics`' 100 µs SLOW threshold.
const SLOW_READ: Duration = Duration::from_micros(500);

/// Runtime: SLOAD slot 7, return it as one word.
const SLOT_READER: &[u8] = &[
    0x60, 0x07, // PUSH1 7
    0x54, // SLOAD
    0x60, 0x00, // PUSH1 0
    0x52, // MSTORE
    0x60, 0x20, 0x60, 0x00, // PUSH1 32, PUSH1 0
    0xF3, // RETURN
];

/// Every read the mock cold leaf answered — the layer above decides what
/// reaches it, so an absent address here was served upstream.
#[derive(Debug, Default)]
struct ColdReads {
    accounts: Vec<Address>,
    storage: usize,
}

type ColdLog = Arc<Mutex<ColdReads>>;

/// The mock cold leaf: an in-memory chain at one block, with an injectable
/// per-read latency (an RPC stand-in).
#[derive(Debug, Clone)]
struct MockCold {
    reads: ColdLog,
    delay: Duration,
}

impl MockCold {
    fn new(delay: Duration) -> Self {
        Self {
            reads: Arc::new(Mutex::new(ColdReads::default())),
            delay,
        }
    }
}

impl Database for MockCold {
    type Error = Infallible;

    fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.reads.lock().expect("cold log").accounts.push(*address);
        std::thread::sleep(self.delay);
        let info = if *address == SENDER {
            Some(AccountInfo::default().with_balance(U256::from(1_000_000_000_000_000_000_u64)))
        } else if *address == CONTRACT {
            Some(
                AccountInfo::default()
                    .with_code(Bytecode::new_raw(Bytes::copy_from_slice(SLOT_READER))),
            )
        } else {
            None
        };
        Ok(info)
    }

    fn get_code_by_hash(&mut self, _code_hash: &B256) -> Result<Bytecode, Self::Error> {
        Ok(Bytecode::default())
    }

    fn get_storage(&mut self, _address: &Address, _key: &Word) -> Result<Word, Self::Error> {
        self.reads.lock().expect("cold log").storage += 1;
        std::thread::sleep(self.delay);
        Ok(Word::from(SLOT_VALUE))
    }

    fn get_block_hash(&mut self, _number: &Word) -> Result<B256, Self::Error> {
        Ok(B256::ZERO)
    }
}

/// The production layer order over the mock cold leaf.
type LabStack = CacheDB<Db<WarmCodeCache<BotStateDb<'static, MockCold>>>>;

fn stack(inner: &Arc<RwLock<WarmCodeCacheInner>>, block: u64, cold: MockCold) -> LabStack {
    let bot_state_db = BotStateDb::new(&NO_SIM_ANCHOR, cold);
    let warm = WarmCodeCache::with_owner(Arc::clone(inner), block, bot_state_db);
    CacheDB::new(Db::new(warm))
}

/// An EVM over a fresh stack at `block`.
fn evm_at(
    inner: &Arc<RwLock<WarmCodeCacheInner>>,
    block: u64,
    cold: MockCold,
) -> ::evm2::Evm<'static, ::evm2::BaseEvmTypes> {
    degenbot_simulation::sim::evm2::build_block_evm2(
        stack(inner, block, cold),
        BASE_FEE_WEI,
        block,
        TIMESTAMP,
    )
}

fn read_slot_tx() -> RecoveredTxEnvelope {
    Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            to: TxKind::Call(CONTRACT),
            input: Bytes::new(),
            gas_limit: 100_000,
            gas_price: BASE_FEE_WEI,
            ..Default::default()
        }),
        SENDER,
    )
}

/// Run one slot read and report the counters it moved.
fn measured_run(session: &mut Evm2SimSession<'_, '_>) -> (U256, SimLabSnapshot) {
    let before = sim_metrics::snapshot();
    let run = session.run(&read_slot_tx()).expect("the run settles");
    let delta = sim_metrics::delta(sim_metrics::snapshot(), before);
    let output = &run.output;
    (U256::from_be_slice(&output[output.len() - 32..]), delta)
}

/// Every cold-forward counter, summed — zero means the sim forwarded nothing.
fn forwarded_total(d: &SimLabSnapshot) -> u64 {
    d.cold_basic_fast
        + d.cold_basic_slow
        + d.cold_code_slow
        + d.cold_storage_fast
        + d.cold_storage_slow
        + d.cold_block_hash
}

#[test]
fn cold_sims_record_forwards_and_warm_sims_record_none() {
    let inner = WarmCodeCacheInner::shared_default();
    an_in_process_cold_leaf_records_fast_forwards_and_a_rerun_records_none(&inner);
    an_rpc_latency_leaf_records_the_slow_bucket();
    the_cross_block_warm_cache_records_a_hit_not_a_cold_fetch(&inner);
}

/// A cold sim whose leaf answers in µs lands in the FAST buckets, and the
/// same run again (the per-block `CacheDB` now warm) forwards nothing.
fn an_in_process_cold_leaf_records_fast_forwards_and_a_rerun_records_none(
    inner: &Arc<RwLock<WarmCodeCacheInner>>,
) {
    let cold = MockCold::new(Duration::ZERO);
    let reads = Arc::clone(&cold.reads);
    let mut evm = evm_at(inner, BLOCK, cold);
    let mut session = Evm2SimSession::new(&mut evm);

    let (word, cold_delta) = measured_run(&mut session);
    assert_eq!(word, U256::from(SLOT_VALUE), "the cold run read the slot");
    assert!(
        reads.lock().expect("cold log").accounts.contains(&CONTRACT),
        "the contract's account read reached the cold leaf"
    );
    assert!(
        cold_delta.cold_basic_fast >= 1,
        "an account forward is counted: {cold_delta:?}"
    );
    assert!(
        cold_delta.cold_storage_fast >= 1,
        "the SLOAD forward is counted: {cold_delta:?}"
    );
    assert_eq!(
        (cold_delta.cold_basic_slow, cold_delta.cold_storage_slow),
        (0, 0),
        "a sub-100 µs serve lands in the FAST bucket: {cold_delta:?}"
    );
    assert_eq!(
        sim_metrics::slow_total_ns(&cold_delta),
        0,
        "no RPC-reached latency was recorded"
    );
    assert_eq!(
        cold_delta.basic_warm_hits, 0,
        "a first-block sim has no cross-block reuse to report"
    );
    assert!(
        sim_metrics::format_delta(&cold_delta, 1).contains("cold: b_fast="),
        "the fan-out line renders the restored counters"
    );

    let (word, warm_delta) = measured_run(&mut session);
    assert_eq!(
        word,
        U256::from(SLOT_VALUE),
        "the warm run returns the same"
    );
    assert_eq!(
        forwarded_total(&warm_delta),
        0,
        "a warm sim forwards nothing: {warm_delta:?}"
    );
}

/// A leaf that spends RPC-scale wall time per read lands in the SLOW buckets
/// with its latency, so the lab line's `slow_ms` column moves.
fn an_rpc_latency_leaf_records_the_slow_bucket() {
    let cold = MockCold::new(SLOW_READ);
    let reads = Arc::clone(&cold.reads);
    let mut evm = evm_at(&WarmCodeCacheInner::shared_default(), BLOCK, cold);
    let mut session = Evm2SimSession::new(&mut evm);

    let (_, slow_delta) = measured_run(&mut session);
    assert!(
        slow_delta.cold_basic_slow >= 1,
        "a 500 µs serve lands in the SLOW bucket: {slow_delta:?}"
    );
    assert!(
        slow_delta.cold_basic_slow_ns >= slow_delta.cold_basic_slow * 500_000,
        "the SLOW bucket carries the forward's wall latency: {slow_delta:?}"
    );
    assert!(
        sim_metrics::slow_total_ns(&slow_delta) > 0,
        "the lab line's slow_ms column is live: {slow_delta:?}"
    );
    assert_eq!(
        (slow_delta.cold_basic_fast, slow_delta.cold_storage_fast),
        (0, 0),
        "nothing was served fast: {slow_delta:?}"
    );
    assert!(
        reads.lock().expect("cold log").storage >= 1,
        "the RPC-latency leaf answered the SLOAD"
    );
}

/// The next block re-serves the contract from the cross-block cache: a warm
/// hit, no account cold fetch, while storage (mutable per block) forwards.
fn the_cross_block_warm_cache_records_a_hit_not_a_cold_fetch(
    inner: &Arc<RwLock<WarmCodeCacheInner>>,
) {
    let cold = MockCold::new(Duration::ZERO);
    let reads = Arc::clone(&cold.reads);
    let mut evm = evm_at(inner, BLOCK + 1, cold);
    let mut session = Evm2SimSession::new(&mut evm);

    let (_, warm_delta) = measured_run(&mut session);
    let accounts = reads.lock().expect("cold log").accounts.clone();
    assert!(
        warm_delta.basic_warm_hits >= 1,
        "the cross-block warm cache served the contract: {warm_delta:?}"
    );
    assert!(
        !accounts.contains(&CONTRACT),
        "the contract's account read never reached the cold leaf: {accounts:?}"
    );
    assert!(
        warm_delta.cold_storage_fast >= 1,
        "storage is mutable-per-block and always forwards: {warm_delta:?}"
    );
}
