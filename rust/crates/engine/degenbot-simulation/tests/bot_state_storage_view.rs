//! Behavior pins for the storage-view adapter in front of `BotStateDb`: the
//! one seam through which the synchronous wrapper reads a cache — the legacy
//! per-anchor memo, or a delta-chain generation — while the RPC fallback keeps
//! its authority.
//!
//! Every test name states the behavior it pins. The scripted fallback counts
//! the forwards that actually left the view, so "served from the cache" and
//! "paid a fallback read" are counted facts, and the chain stub answers the
//! rounds the delta generation's own read seam needs to seed a hit.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "the scripted fallback and its assertions fail loudly on a broken fixture"
)]

use std::collections::BTreeSet;
use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use alloy::primitives::aliases::U112;
use alloy::primitives::{Address, B256, U256};
use degenbot_core::fee::fee_ratio;
use degenbot_pools::RegisterV2PoolParams;
use degenbot_simulation::divergence_probe::DivergenceTallyHandle;
use degenbot_simulation::sim::evm2::alloy_async_db::AlloyAsyncDb;
use degenbot_simulation::sim::evm2::blocking_async_db::BlockingAsyncDb;
use degenbot_simulation::sim::evm2::bot_state_db::BotStateDb;
use degenbot_simulation::sim::evm2::delta_snapshot::{DeltaHub, DeltaSnapshotCache, Retention};
use degenbot_simulation::sim::{ReadAnchor, StorageMemo, StorageMemoCache};
use degenbot_substrate::{BotState, SimAnchorState, NO_SIM_ANCHOR};
use degenbot_uniswap::dex_identity::DexVariant;
use evm2::bytecode::Bytecode;
use evm2::evm::{AccountInfo, Database};
use evm2::interpreter::Word;

#[path = "../examples/shared/chain_stub.rs"]
#[expect(dead_code)]
mod chain_stub;

#[path = "../examples/shared/modeled_chain.rs"]
#[expect(dead_code)]
mod modeled_chain;

#[path = "../examples/shared/real_coverage.rs"]
#[expect(dead_code)]
mod real_coverage;

use chain_stub::ChainStub;
use modeled_chain::{ChainState, Coverage, StubChain};
use real_coverage::{Key, RealCoverage};

const POOL: Address = Address::new([0xaa; 20]);

/// The anchor height every delta scenario starts at.
const HEIGHT: u64 = 100;

/// Scenario generation `step`'s block hash (`step` stands at `HEIGHT + step`).
fn hash(step: u8) -> B256 {
    B256::new([0xa0 + step; 32])
}

/// The scenario slot every view test reads.
fn slot() -> Word {
    Word::from(8u64)
}

/// A second scenario slot, so one generation can hold a refreshed entry and an
/// unrefreshed one at once.
fn other_slot() -> Word {
    Word::from(9u64)
}

/// Install the scenario cache's first generation: cold, at `HEIGHT`.
fn start<'p>(cache: &DeltaSnapshotCache<'p>) -> Arc<DeltaHub<'p>> {
    cache.for_anchor(ReadAnchor::identified(HEIGHT, hash(0)))
}

/// Advance one linked generation carrying no writes, so the retention clock
/// moves and nothing else does.
async fn advance<'p>(
    cache: &DeltaSnapshotCache<'p>,
    stub: &ChainStub,
    step: u8,
) -> Arc<DeltaHub<'p>> {
    stub.serve_bal_at(hash(step), modeled_chain::bal_bytes(&[]));
    let (hub, _) = cache
        .advance_bal(
            ReadAnchor::identified(HEIGHT + u64::from(step), hash(step)),
            hash(step - 1),
            &[],
        )
        .await;
    hub
}

/// A counting fallback serving one scripted storage word.
struct CountingDb {
    word: u64,
    reads: u64,
}

impl CountingDb {
    fn serving(word: u64) -> Self {
        Self { word, reads: 0 }
    }
}

impl Database for CountingDb {
    type Error = Infallible;

    fn get_account(&mut self, _address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        Ok(None)
    }

    fn get_code_by_hash(&mut self, _code_hash: &B256) -> Result<Bytecode, Self::Error> {
        Ok(Bytecode::default())
    }

    fn get_storage(&mut self, _address: &Address, _key: &Word) -> Result<Word, Self::Error> {
        self.reads += 1;
        Ok(Word::from(self.word))
    }

    fn get_block_hash(&mut self, _number: &Word) -> Result<B256, Self::Error> {
        Ok(B256::ZERO)
    }
}

/// A fallback that fails every read — the error-path fixture.
struct FallbackDown;

#[derive(Debug)]
struct FallbackOffline;

impl std::fmt::Display for FallbackOffline {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the fallback is down")
    }
}

impl std::error::Error for FallbackOffline {}

impl Database for FallbackDown {
    type Error = FallbackOffline;

    fn get_account(&mut self, _address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        Err(FallbackOffline)
    }

    fn get_code_by_hash(&mut self, _code_hash: &B256) -> Result<Bytecode, Self::Error> {
        Err(FallbackOffline)
    }

    fn get_storage(&mut self, _address: &Address, _key: &Word) -> Result<Word, Self::Error> {
        Err(FallbackOffline)
    }

    fn get_block_hash(&mut self, _number: &Word) -> Result<B256, Self::Error> {
        Err(FallbackOffline)
    }
}

/// A miss forwards to the fallback once and settles the fallback's word into
/// the view, so the next read of the same word is a hit with no fallback read.
#[test]
fn a_storage_view_serves_a_hit_and_settles_a_miss() {
    let memo = Arc::new(StorageMemo::new());
    let mut db =
        BotStateDb::new(&NO_SIM_ANCHOR, CountingDb::serving(42)).with_storage_view(memo.clone());

    assert_eq!(db.get_storage(&POOL, &slot()).unwrap(), Word::from(42u64));
    assert_eq!(db.fallback.reads, 1, "a miss forwards to the fallback once");
    assert!(
        memo.holds(POOL, slot()),
        "the fallback's own answer settles into the view"
    );

    assert_eq!(db.get_storage(&POOL, &slot()).unwrap(), Word::from(42u64));
    assert_eq!(db.fallback.reads, 1, "the second read is a view hit");
}

/// `with_storage_memo` remains the legacy-compatible wrapper: the same
/// get/put calls the anchor memo already had, counters included.
#[test]
fn a_legacy_memo_view_keeps_the_existing_call_sites_working() {
    let cache = StorageMemoCache::new();
    let memo = cache.for_anchor(ReadAnchor::at(HEIGHT));
    let mut db =
        BotStateDb::new(&NO_SIM_ANCHOR, CountingDb::serving(42)).with_storage_memo(memo.clone());

    assert_eq!(db.get_storage(&POOL, &slot()).unwrap(), Word::from(42u64));
    assert_eq!(
        memo.stats(),
        (0, 1),
        "a cold read is one memo miss, settled by one put"
    );
    assert_eq!(db.get_storage(&POOL, &slot()).unwrap(), Word::from(42u64));
    assert_eq!(db.fallback.reads, 1);
    assert_eq!(memo.stats(), (1, 1), "the second read is one memo hit");
}

/// A zero word is a value like any other: served, settled, and hit — never
/// conflated with a miss.
#[test]
fn a_zero_word_is_served_and_settled_like_any_other() {
    let memo = Arc::new(StorageMemo::new());
    let mut db =
        BotStateDb::new(&NO_SIM_ANCHOR, CountingDb::serving(0)).with_storage_memo(memo.clone());

    assert_eq!(db.get_storage(&POOL, &slot()).unwrap(), Word::ZERO);
    assert_eq!(
        db.fallback.reads, 1,
        "the zero word comes from the fallback"
    );
    assert!(memo.holds(POOL, slot()), "and settles into the view");
    assert_eq!(db.get_storage(&POOL, &slot()).unwrap(), Word::ZERO);
    assert_eq!(db.fallback.reads, 1, "the settled zero is a hit");
}

/// A delta generation's chain hit answers the wrapper with the generation's own
/// word and no fallback read — and with no round either: a synchronous view
/// read never reaches the async hub.
#[tokio::test]
async fn a_delta_view_hit_answers_from_the_generation_with_no_fallback_read() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, POOL, slot(), 7);
    let cache = DeltaSnapshotCache::new(&provider);
    let hub = start(&cache);
    assert_eq!(
        hub.read_storage(POOL, slot()).await.unwrap(),
        U256::from(7u64),
        "the generation settles the chain's word at its anchor"
    );
    stub.clear();

    let mut db =
        BotStateDb::new(&NO_SIM_ANCHOR, CountingDb::serving(0)).with_storage_view(hub.clone());
    assert_eq!(db.get_storage(&POOL, &slot()).unwrap(), Word::from(7u64));
    assert_eq!(
        db.fallback.reads, 0,
        "the view answers, the fallback is idle"
    );
    assert_eq!(
        stub.storage_members(),
        0,
        "a synchronous view read queues no hub round"
    );
}

/// A delta miss is the wrapper's fallback to pay, and the fallback's own word
/// settles into the generation that asked — the next read is a chain hit.
#[tokio::test]
async fn a_delta_view_miss_settles_the_fallback_word_into_its_generation() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let cache = DeltaSnapshotCache::new(&provider);
    let hub = start(&cache);
    let mut db =
        BotStateDb::new(&NO_SIM_ANCHOR, CountingDb::serving(42)).with_storage_view(hub.clone());

    assert_eq!(db.get_storage(&POOL, &slot()).unwrap(), Word::from(42u64));
    assert_eq!(
        db.fallback.reads, 1,
        "an unknown identity takes the fallback"
    );
    assert_eq!(
        hub.snapshot_value(POOL, slot()),
        Some(U256::from(42u64)),
        "the fallback's answer settles into the generation"
    );
    assert_eq!(hub.live_len(), 1, "and it is served");

    assert_eq!(db.get_storage(&POOL, &slot()).unwrap(), Word::from(42u64));
    assert_eq!(db.fallback.reads, 1, "the settled word is a chain hit");
    assert_eq!(stub.storage_members(), 0, "neither read queued a hub round");
}

/// Both representations answer a zero-word miss-then-hit sequence identically:
/// the same values, the same fallback forwards. The wrapper cannot tell them
/// apart.
#[tokio::test]
async fn a_legacy_memo_view_and_a_delta_view_answer_a_zero_word_alike() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let cache = DeltaSnapshotCache::new(&provider);
    let hub = start(&cache);
    let memo = Arc::new(StorageMemo::new());

    let mut legacy =
        BotStateDb::new(&NO_SIM_ANCHOR, CountingDb::serving(0)).with_storage_memo(memo);
    let mut delta =
        BotStateDb::new(&NO_SIM_ANCHOR, CountingDb::serving(0)).with_storage_view(hub.clone());

    let mut legacy_shape = Vec::new();
    for _ in 0..3 {
        let value = legacy.get_storage(&POOL, &slot()).unwrap();
        legacy_shape.push((value, legacy.fallback.reads));
    }

    let mut delta_shape = Vec::new();
    for _ in 0..3 {
        let value = delta.get_storage(&POOL, &slot()).unwrap();
        delta_shape.push((value, delta.fallback.reads));
    }

    assert_eq!(
        legacy_shape,
        vec![(Word::ZERO, 1), (Word::ZERO, 1), (Word::ZERO, 1)],
        "the memo answers a zero word as a miss then two hits"
    );
    assert_eq!(
        delta_shape, legacy_shape,
        "the delta generation answers the same sequence, word and forward alike"
    );
    assert_eq!(stub.storage_members(), 0, "neither view queued a round");
}

/// A candidate view read stamps the generation's recency, exactly as a
/// candidate read through the hub does: the refreshed entry outlives one the
/// wrapper stopped reading.
#[tokio::test]
async fn a_view_read_refreshes_recency_so_the_unread_entry_expires_first() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let cache = DeltaSnapshotCache::with_retention(&provider, Retention::IdleBlocks(2));
    let first = start(&cache);

    // Both identities settle at HEIGHT through the view's put, so both are
    // stamped as used at block 100.
    let mut settling =
        BotStateDb::new(&NO_SIM_ANCHOR, CountingDb::serving(1)).with_storage_view(first.clone());
    assert_eq!(
        settling.get_storage(&POOL, &slot()).unwrap(),
        Word::from(1u64)
    );
    assert_eq!(
        settling.get_storage(&POOL, &other_slot()).unwrap(),
        Word::from(1u64)
    );
    assert_eq!(
        settling.fallback.reads, 2,
        "both identities took the fallback once"
    );

    // Used at 100 with a two-block window: visible through 102.
    advance(&cache, &stub, 1).await;
    let at_102 = advance(&cache, &stub, 2).await;
    assert_eq!(at_102.snapshot_value(POOL, slot()), Some(U256::from(1u64)));
    assert_eq!(
        at_102.snapshot_value(POOL, other_slot()),
        Some(U256::from(1u64))
    );

    // A view read at 102 refreshes only the slot it read.
    let mut refreshing =
        BotStateDb::new(&NO_SIM_ANCHOR, CountingDb::serving(1)).with_storage_view(at_102.clone());
    assert_eq!(
        refreshing.get_storage(&POOL, &slot()).unwrap(),
        Word::from(1u64)
    );
    assert_eq!(
        refreshing.fallback.reads, 0,
        "the refresh was a chain hit, not a fallback read"
    );

    advance(&cache, &stub, 3).await;
    let at_104 = advance(&cache, &stub, 4).await;
    assert_eq!(
        at_104.snapshot_value(POOL, slot()),
        Some(U256::from(1u64)),
        "read at 102, still served at 104"
    );
    assert_eq!(
        at_104.snapshot_value(POOL, other_slot()),
        None,
        "unread since 100, gone at 104"
    );
}

/// An expired entry is a fallback miss, never a served stale word: the answer
/// is the chain's CURRENT value at the reading generation, and it re-settles
/// with a fresh window.
#[tokio::test]
async fn an_expired_entry_is_a_fallback_miss_and_never_serves_the_stale_word() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let cache = DeltaSnapshotCache::with_retention(&provider, Retention::IdleBlocks(1));
    let first = start(&cache);

    let mut settling =
        BotStateDb::new(&NO_SIM_ANCHOR, CountingDb::serving(5)).with_storage_view(first);
    assert_eq!(
        settling.get_storage(&POOL, &slot()).unwrap(),
        Word::from(5u64)
    );

    advance(&cache, &stub, 1).await;
    let at_102 = advance(&cache, &stub, 2).await;
    assert_eq!(
        at_102.snapshot_value(POOL, slot()),
        None,
        "used at 100 with a one-block window: expired at 102"
    );

    let mut later =
        BotStateDb::new(&NO_SIM_ANCHOR, CountingDb::serving(9)).with_storage_view(at_102.clone());
    assert_eq!(
        later.get_storage(&POOL, &slot()).unwrap(),
        Word::from(9u64),
        "the fallback's current word answers, not the expired one"
    );
    assert_eq!(
        later.fallback.reads, 1,
        "an expired entry is a miss like any other"
    );
    assert_eq!(
        at_102.snapshot_value(POOL, slot()),
        Some(U256::from(9u64)),
        "the answer re-settles into the reading generation"
    );
    assert_eq!(at_102.live_len(), 1, "with a fresh window");
    assert_eq!(later.get_storage(&POOL, &slot()).unwrap(), Word::from(9u64));
    assert_eq!(later.fallback.reads, 1, "and the fresh entry is a hit");
}

/// A settle through a view whose generation a child already sealed stays
/// handle-local: this handle serves it, no ancestor and no child does.
#[tokio::test]
async fn a_settle_through_a_sealed_generation_stays_handle_local() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let cache = DeltaSnapshotCache::new(&provider);
    let parent = start(&cache);
    let child = advance(&cache, &stub, 1).await;

    let mut late =
        BotStateDb::new(&NO_SIM_ANCHOR, CountingDb::serving(5)).with_storage_view(parent.clone());
    assert_eq!(
        late.get_storage(&POOL, &slot()).unwrap(),
        Word::from(5u64),
        "the parent's view takes its own fallback"
    );
    assert_eq!(late.fallback.reads, 1);
    assert_eq!(
        parent.snapshot_value(POOL, slot()),
        Some(U256::from(5u64)),
        "the settle is visible to the handle that asked"
    );
    assert_eq!(
        child.snapshot_value(POOL, slot()),
        None,
        "and invisible to the child linked before it"
    );
    assert_eq!(
        child.snapshot_len(),
        0,
        "the sealed parent node is immutable"
    );

    let mut at_child =
        BotStateDb::new(&NO_SIM_ANCHOR, CountingDb::serving(9)).with_storage_view(child.clone());
    assert_eq!(
        at_child.get_storage(&POOL, &slot()).unwrap(),
        Word::from(9u64),
        "the child pays its own anchor's read"
    );
    assert_eq!(at_child.fallback.reads, 1);
    assert_eq!(
        parent.snapshot_value(POOL, slot()),
        Some(U256::from(5u64)),
        "the child's settle never reaches the parent"
    );
}

/// A fallback error propagates unchanged and settles nothing, whichever view
/// is installed: no value can be manufactured out of a failed read.
#[tokio::test]
async fn a_fallback_error_propagates_unchanged_and_settles_nothing() {
    let memo = Arc::new(StorageMemo::new());
    let mut legacy = BotStateDb::new(&NO_SIM_ANCHOR, FallbackDown).with_storage_memo(memo.clone());
    let error = legacy.get_storage(&POOL, &slot()).unwrap_err();
    assert_eq!(error.to_string(), "the fallback is down");
    assert_eq!(
        memo.stats(),
        (0, 1),
        "the erroring read is still the view's one miss"
    );
    assert!(
        !memo.holds(POOL, slot()),
        "an error settles nothing into the memo"
    );

    let stub = ChainStub::serving();
    let provider = stub.provider();
    let cache = DeltaSnapshotCache::new(&provider);
    let hub = start(&cache);
    let mut delta = BotStateDb::new(&NO_SIM_ANCHOR, FallbackDown).with_storage_view(hub.clone());
    let error = delta.get_storage(&POOL, &slot()).unwrap_err();
    assert_eq!(error.to_string(), "the fallback is down");
    assert_eq!(
        hub.snapshot_value(POOL, slot()),
        None,
        "an error settles nothing into the generation"
    );
    assert_eq!(hub.live_len(), 0);
}

/// A view changes nothing about the observation: a forwarded read still
/// observes once into the handed tally, and a view hit (which forwards nothing)
/// observes nothing.
#[tokio::test]
async fn a_forwarded_read_observes_and_a_view_hit_observes_nothing() {
    let core = bot_state_with_pool();
    let anchor = SimAnchorState::snapshot(&core);
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let cache = DeltaSnapshotCache::new(&provider);
    let hub = start(&cache);
    let tally = DivergenceTallyHandle::new();

    let mut db = BotStateDb::new(&anchor, CountingDb::serving(42))
        .with_storage_view(hub.clone())
        .with_divergence_tally(tally.clone());
    assert_eq!(db.get_storage(&POOL, &slot()).unwrap(), Word::from(42u64));
    assert_eq!(
        tally.snapshot().slots_compared,
        1,
        "the forwarded read observes into the handed tally"
    );
    assert_eq!(db.get_storage(&POOL, &slot()).unwrap(), Word::from(42u64));
    assert_eq!(db.fallback.reads, 1, "the second read is a view hit");
    assert_eq!(
        tally.snapshot().slots_compared,
        1,
        "a view hit forwards no RPC answer, so it observes nothing"
    );
}

/// Both committed real captures answer every identity their block access lists
/// write through the COLD view path — a miss, the fallback's own read, the
/// settle — with zero mismatches against an independent cold batch read, and a
/// second pass over the same generation pays no storage request at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_real_fixtures_answer_every_written_identity_cold_through_the_view() {
    for fixture in [
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/bal_real_coverage.json"
        ),
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/bal_real_coverage_broader.json"
        ),
    ] {
        let real = RealCoverage::load(fixture).expect("the committed capture parses");
        let keys: BTreeSet<Key> = (real.first()..=real.last())
            .flat_map(|number| real.writes_at(number).iter().copied())
            .collect();
        let chain =
            ChainState::build_coverage(&keys, real.first(), real.last(), &real, Coverage::Real)
                .expect("a chain over the captured stretch");
        let stub = StubChain {
            chain: Arc::new(chain),
            plant: Arc::new(AtomicBool::new(false)),
        };
        let provider = stub.provider();
        let cache = DeltaSnapshotCache::new(&provider);
        let mut mismatches = Vec::new();
        let mut blocks = 0usize;

        for number in real.first()..=real.last() {
            let written: Vec<Key> = real.writes_at(number).to_vec();
            if written.is_empty() {
                continue;
            }
            blocks += 1;
            let anchor = ReadAnchor::identified(
                number,
                real.hash(number).expect("the capture names hashes"),
            );
            // A COLD generation at this exact anchor: the view carries nothing,
            // so every read is the wrapper's fallback to take.
            let hub = cache.for_anchor(anchor);
            let blocking = BlockingAsyncDb::new(AlloyAsyncDb::new(&provider, number))
                .expect("a multi-threaded runtime is ambient");
            let (fallback, forwarded) = CountingChain::wrapping(blocking);
            let mut db = BotStateDb::new(&NO_SIM_ANCHOR, fallback).with_storage_view(hub.clone());
            let cold = provider
                .get_storage_batch_results_at(&written, number)
                .await
                .expect("the cold batch answers");

            for (index, &(address, slot)) in written.iter().enumerate() {
                assert_eq!(
                    hub.snapshot_value(address, slot),
                    None,
                    "block {number}: a cold generation carries nothing"
                );
                let served = db
                    .get_storage(&address, &slot)
                    .expect("the cold view path answers");
                let expected = *cold[index].as_ref().expect("the cold member answers");
                if served != expected {
                    mismatches.push(format!(
                        "{fixture} block {number} {address}/{slot}: view {served} vs cold \
                         {expected}"
                    ));
                }
            }
            assert_eq!(
                forwarded.load(Ordering::Relaxed),
                written.len() as u64,
                "block {number}: every cold identity took exactly one fallback read"
            );

            // The settled generation answers the same words with no chain read:
            // the view's put is what the second pass serves.
            let warm_before = forwarded.load(Ordering::Relaxed);
            for (index, &(address, slot)) in written.iter().enumerate() {
                assert_eq!(
                    db.get_storage(&address, &slot)
                        .expect("the warm pass answers"),
                    *cold[index].as_ref().expect("the cold member answers"),
                    "block {number}: the warm pass serves the settled word"
                );
            }
            assert_eq!(
                forwarded.load(Ordering::Relaxed),
                warm_before,
                "block {number}: the warm pass paid no fallback read"
            );
        }

        assert!(blocks > 0, "{fixture}: the capture holds write coverage");
        assert!(
            mismatches.is_empty(),
            "{}: {} mismatches: {:?}",
            fixture,
            mismatches.len(),
            &mismatches[..mismatches.len().min(5)]
        );
    }
}

/// A fallback that counts the storage reads it forwarded, so "the view served
/// it" and "the chain was paid" are counted facts.
struct CountingChain<Ext> {
    inner: Ext,
    reads: Arc<AtomicU64>,
}

impl<Ext> CountingChain<Ext> {
    fn wrapping(inner: Ext) -> (Self, Arc<AtomicU64>) {
        let reads = Arc::new(AtomicU64::new(0));
        (
            Self {
                inner,
                reads: Arc::clone(&reads),
            },
            reads,
        )
    }
}

impl<Ext: Database> Database for CountingChain<Ext> {
    type Error = Ext::Error;

    fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.inner.get_account(address)
    }

    fn get_code_by_hash(&mut self, code_hash: &B256) -> Result<Bytecode, Self::Error> {
        self.inner.get_code_by_hash(code_hash)
    }

    fn get_storage(&mut self, address: &Address, key: &Word) -> Result<Word, Self::Error> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.inner.get_storage(address, key)
    }

    fn get_block_hash(&mut self, number: &Word) -> Result<B256, Self::Error> {
        self.inner.get_block_hash(number)
    }
}

/// A tracked V2 pool, so the divergence observation has a slot to compare.
fn bot_state_with_pool() -> BotState {
    let mut core = BotState::new();
    core.register_v2_pool(&RegisterV2PoolParams {
        address: POOL,
        token0: Address::from([0xbb; 20]),
        token1: Address::from([0xcc; 20]),
        reserve0: U112::from(1000),
        reserve1: U112::from(2000),
        fee_token0: fee_ratio(997, 1000),
        fee_token1: fee_ratio(997, 1000),
        factory: Address::from([0xdd; 20]),
        update_block: 0,
        variant: DexVariant::UniswapV2,
        stable_swap: false,
        deployer: Address::ZERO,
        init_hash: B256::ZERO,
    })
    .expect("test setup: V2 registration");
    core
}
