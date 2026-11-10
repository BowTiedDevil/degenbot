//! Apply-time session relevance: the union, the registry revision, and the two
//! registration races (§7 tests 15-19 of
//! `logs/bal_consecutive_head_design.md`).
//!
//! # What these pins own
//!
//! A head's final writes seed under a relevance set that must exist BEFORE any
//! candidate is solved: the union of every REGISTERED path's addresses and the
//! deployment's settlement identities, derived once per registry revision and
//! read after the head's list lands, immediately before it applies. No pin here
//! counts a candidate's read set — that is the oracle this design refuses.
//!
//! The engine is real (a `BotState` with two V2 pools behind the engine's stage
//! surface) and the chain is the suite's scripted stub, so every assertion is
//! about a seeding decision, never about an internal map: a seeded identity is
//! answered from the generation, a skipped one takes the dynamic path and pays
//! one storage read.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "the stub chain and its assertions fail loudly on a broken fixture"
)]

use alloy::primitives::aliases::U112;
use alloy::primitives::{Address, B256, U256};
use degenbot_bot::arb_engine::EngineStages;
use degenbot_core::canonical_head::{CanonicalHead, CanonicalHeadSink};
use degenbot_core::fee::fee_ratio;
use degenbot_pools::solve::PoolHop;
use degenbot_pools::RegisterV2PoolParams;
use degenbot_simulation::sim::evm2::bal_heads::{
    BalHeadPipeline, HeadRelevanceSource, HeadSpawn, SessionRelevance,
};
use degenbot_simulation::sim::evm2::delta_snapshot::{BalHeadDriver, DeltaReport, RelevanceSet};
use degenbot_simulation::sim::prefetch::PrefetchFacts;
use degenbot_simulation::sim::ReadAnchor;
use degenbot_simulation::sim::SettlementWarmup;
use degenbot_substrate::state_lock::{LockSite, StateLock};
use degenbot_substrate::{BotState, EpochDelta};
use degenbot_uniswap::dex_identity::DexVariant;
use futures_util::future::BoxFuture;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::task::{Context, Waker};

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
use modeled_chain::bal_bytes;

/// The height every test advances from, unless the test is about lineage.
const HEIGHT: u64 = 200;

fn address(tag: u8) -> Address {
    Address::new([tag; 20])
}

/// The session's WETH9 identity — a settlement address no fixture path names.
fn weth() -> Address {
    address(0xee)
}

/// The session's V4 `PoolManager` identity — the settlement declaration's second
/// address, also unnamed by the fixtures' V2 paths.
fn pool_manager() -> Address {
    address(0xef)
}

/// The executor the warmup slots key on (relevance reads only the settlement's
/// two ADDRESSES, so this value never reaches an assertion).
fn executor() -> Address {
    address(0xed)
}

/// The deployment facts a session declares: WETH9 + the `PoolManager`.
fn settlement_facts() -> PrefetchFacts {
    PrefetchFacts {
        settlement: Some(SettlementWarmup {
            weth_address: weth(),
            pool_manager_address: Some(pool_manager()),
            slots: degenbot_executor::compute_simulation_warmup_slots(executor(), weth()),
        }),
    }
}

/// The deterministic spawn seam: every phase the pipeline spawns parks here
/// until the test drains it.
#[derive(Clone, Default)]
struct SpawnQueue {
    parked: Arc<Mutex<Vec<BoxFuture<'static, ()>>>>,
}

impl SpawnQueue {
    fn new() -> Self {
        Self::default()
    }

    fn spawner(&self) -> HeadSpawn {
        let parked = Arc::clone(&self.parked);
        Arc::new(move |phase: BoxFuture<'static, ()>| parked.lock().unwrap().push(phase))
    }

    fn take(&self) -> Option<BoxFuture<'static, ()>> {
        let mut parked = self.parked.lock().unwrap();
        if parked.is_empty() {
            None
        } else {
            Some(parked.remove(0))
        }
    }

    async fn run_one(&self) -> bool {
        match self.take() {
            Some(phase) => {
                phase.await;
                true
            }
            None => false,
        }
    }
}

/// The session under test: one engine over its own `BotState`, with one V2 pool
/// registered per entry of `pools`. NO path is registered — a test that needs one
/// registers it at the moment the behavior under test names.
fn session(pools: &[Address]) -> (Arc<EngineStages>, Vec<u64>) {
    let core = Arc::new(StateLock::new(BotState::new()));
    let pool_ids: Vec<u64> = {
        let mut state = core.write_at(LockSite::Solver);
        pools
            .iter()
            .map(|&pool| {
                state
                    .register_v2_pool(&RegisterV2PoolParams {
                        address: pool,
                        token0: address(0x01),
                        token1: address(0x02),
                        reserve0: U112::from(1_000_000u64),
                        reserve1: U112::from(2_000_000u64),
                        fee_token0: fee_ratio(997, 1000),
                        fee_token1: fee_ratio(997, 1000),
                        factory: Address::ZERO,
                        deployer: Address::ZERO,
                        init_hash: B256::ZERO,
                        update_block: 0,
                        variant: DexVariant::UniswapV2,
                        stable_swap: false,
                    })
                    .expect("test setup: V2 registration")
            })
            .collect()
    };
    let stages = Arc::new(EngineStages::with_core(
        Arc::clone(&core),
        Arc::new(EpochDelta::new(0u64)),
    ));
    (stages, pool_ids)
}

/// Register the fixture path over both of the session's pools — the registration
/// whose addresses the session's relevance must then name.
fn register_fixture_path(stages: &EngineStages, pool_ids: &[u64]) {
    let (path_id, created) = stages
        .register_path(vec![
            PoolHop {
                pool_id: pool_ids[0],
                zero_for_one: true,
            },
            PoolHop {
                pool_id: pool_ids[1],
                zero_for_one: true,
            },
        ])
        .expect("the fixture path registers");
    assert!(path_id > 0, "the registration allocated a path id");
    assert!(
        created,
        "the fixture path is GROWTH: it moves the identity clock"
    );
}

/// The session's relevance source: the engine's registered paths joined with the
/// deployment facts (the translation the Python composition installs).
struct SessionFixture {
    stages: Arc<EngineStages>,
    facts: PrefetchFacts,
}

impl HeadRelevanceSource for SessionFixture {
    fn session_relevance(&self) -> SessionRelevance {
        let inputs = self.stages.session_relevance();
        SessionRelevance {
            revision: inputs.revision,
            paths: inputs.paths,
            facts: self.facts,
        }
    }
}

fn source(stages: &Arc<EngineStages>, facts: PrefetchFacts) -> Arc<dyn HeadRelevanceSource> {
    Arc::new(SessionFixture {
        stages: Arc::clone(stages),
        facts,
    })
}

/// An armed pipeline whose phases the test drains, whose apply-time relevance is
/// `relevance`, over ONE session driver.
fn armed_pipeline(
    driver: &Arc<BalHeadDriver>,
    queue: &SpawnQueue,
    relevance: Arc<dyn HeadRelevanceSource>,
) -> Arc<BalHeadPipeline> {
    let pipeline = BalHeadPipeline::with_spawn_and_relevance(queue.spawner(), relevance);
    assert!(pipeline.arm(Arc::clone(driver)), "the session arms it once");
    pipeline
}

fn head(number: u64, hash: B256, parent_hash: B256) -> CanonicalHead {
    CanonicalHead::new(number, hash, parent_hash)
}

fn identified(number: u64, hash: B256) -> ReadAnchor {
    ReadAnchor::identified(number, hash)
}

fn genesis() -> B256 {
    B256::new([0x99; 32])
}

fn report_of(driver: &BalHeadDriver) -> Arc<DeltaReport> {
    driver
        .last_report()
        .expect("the pipeline's apply produced a report")
}

/// A head whose list carries `writes`, delivered and drained through the
/// pipeline's own spawned phase.
async fn settle(pipeline: &BalHeadPipeline, queue: &SpawnQueue, canonical: CanonicalHead) {
    pipeline.on_canonical_head(canonical);
    assert!(queue.run_one().await, "the head's phase runs");
}

/// Test 15 — relevance is the UNION of the registered paths' addresses and the
/// settlement identities, never the future candidate's read set.
///
/// Three writes land on one head: one at a registered path's pool (path-named),
/// one at the settlement's WETH (declaration-named, named by no path here), and
/// one at an identity neither half names. The first two seed; the third stays
/// unknown and takes the dynamic path.
#[tokio::test]
async fn relevance_is_the_union_of_the_registered_paths_and_the_settlement_identities() {
    let pool_a = address(0x11);
    let pool_b = address(0x12);
    let unknown = address(0x99);
    let (stages, ids) = session(&[pool_a, pool_b]);
    register_fixture_path(&stages, &ids);
    // The projection the session hands up really names the fixture's path.
    assert_eq!(
        stages.session_relevance().paths.len(),
        1,
        "one registered path projects"
    );

    let hash = B256::new([0xa1; 32]);
    let stub = ChainStub::serving();
    stub.serve_bal_at(
        hash,
        bal_bytes(&[
            (pool_a, U256::from(1u64), U256::from(11u64)),
            (weth(), U256::from(2u64), U256::from(22u64)),
            (unknown, U256::from(3u64), U256::from(33u64)),
        ]),
    );
    stub.serve_at(HEIGHT, unknown, U256::from(3u64), 33);
    let provider = stub.provider();
    let driver = Arc::new(BalHeadDriver::new(&provider));
    let queue = SpawnQueue::new();
    let pipeline = armed_pipeline(&driver, &queue, source(&stages, settlement_facts()));

    let canonical = head(HEIGHT, hash, genesis());
    settle(&pipeline, &queue, canonical).await;

    let report = report_of(&driver);
    assert_eq!(
        report.writes,
        vec![(pool_a, U256::from(1u64)), (weth(), U256::from(2u64))],
        "the path-named pool and the settlement's WETH both seed"
    );
    assert_eq!(
        report.writes_skipped, 1,
        "the identity neither half names stays unknown"
    );
    let view = driver.snapshot(identified(HEIGHT, hash));
    assert_eq!(
        view.hub.snapshot_value(pool_a, U256::from(1u64)),
        Some(U256::from(11u64)),
        "the path-named identity is served from the generation"
    );
    assert_eq!(
        view.hub.snapshot_value(weth(), U256::from(2u64)),
        Some(U256::from(22u64)),
        "the settlement identity is served from the generation"
    );
    assert_eq!(
        view.hub.snapshot_value(unknown, U256::from(3u64)),
        None,
        "the unknown identity was never seeded"
    );
}

/// Test 16 — a path registered DURING preparation is included when it lands
/// before the apply.
#[tokio::test]
async fn a_path_registered_during_preparation_is_included_when_it_lands_before_the_apply() {
    let pool_a = address(0x21);
    let pool_b = address(0x22);
    let (stages, ids) = session(&[pool_a, pool_b]);
    let hash = B256::new([0xb1; 32]);
    let slot = U256::from(4u64);
    let stub = ChainStub::serving();
    stub.serve_bal_at(hash, bal_bytes(&[(pool_a, slot, U256::from(44u64))]));
    let provider = stub.provider();
    let driver = Arc::new(BalHeadDriver::new(&provider));
    let queue = SpawnQueue::new();
    let pipeline = armed_pipeline(&driver, &queue, source(&stages, settlement_facts()));

    let canonical = head(HEIGHT, hash, genesis());
    pipeline.on_canonical_head(canonical);
    let mut phase = queue.take().expect("the head's phase is parked");
    stub.set_slow(true);
    assert!(
        phase
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending(),
        "the head's list is genuinely in flight"
    );

    // The registration lands inside the fetch window — the race this test owns.
    register_fixture_path(&stages, &ids);

    stub.set_slow(false);
    phase.await;

    let report = report_of(&driver);
    assert_eq!(
        report.writes,
        vec![(pool_a, slot)],
        "the path committed before the apply seeded this head"
    );
    assert_eq!(
        report.writes_skipped, 0,
        "nothing relevant was left unknown"
    );
    assert_eq!(
        driver
            .snapshot(identified(HEIGHT, hash))
            .hub
            .snapshot_value(pool_a, slot),
        Some(U256::from(44u64)),
        "the generation serves the identity the late registration named"
    );
}

/// Test 17 — a path registered AFTER the apply seeds from the NEXT head, and the
/// head that missed it reads its candidate's unknown slot dynamically.
#[tokio::test]
async fn a_path_registered_after_the_apply_seeds_from_the_next_head_and_its_candidate_reads_dynamically(
) {
    let pool_a = address(0x31);
    let pool_b = address(0x32);
    let (stages, ids) = session(&[pool_a, pool_b]);
    let first = B256::new([0xc1; 32]);
    let second = B256::new([0xc2; 32]);
    let slot = U256::from(5u64);
    let stub = ChainStub::serving();
    stub.serve_bal_at(first, bal_bytes(&[(pool_a, slot, U256::from(55u64))]));
    stub.serve_bal_at(second, bal_bytes(&[(pool_a, slot, U256::from(66u64))]));
    stub.serve_at(HEIGHT, pool_a, slot, 55);
    stub.serve_at(HEIGHT + 1, pool_a, slot, 66);
    let provider = stub.provider();
    let driver = Arc::new(BalHeadDriver::new(&provider));
    let queue = SpawnQueue::new();
    let pipeline = armed_pipeline(&driver, &queue, source(&stages, settlement_facts()));

    let first_head = head(HEIGHT, first, genesis());
    settle(&pipeline, &queue, first_head).await;
    let first_report = report_of(&driver);
    assert_eq!(
        first_report.writes_skipped, 1,
        "with nothing registered, the head's write is left unknown"
    );
    assert_eq!(first_report.writes, Vec::new(), "nothing seeded");

    // The candidate's read at that head takes the dynamic path and pays one
    // read: the unseeded identity is answered by the chain, not by the apply.
    let first_view = driver.snapshot(identified(HEIGHT, first));
    assert_eq!(
        first_view.hub.snapshot_value(pool_a, slot),
        None,
        "the unseeded identity is not served"
    );
    let reads_before = stub.storage_members();
    assert_eq!(
        first_view
            .hub
            .read_storage(pool_a, slot)
            .await
            .expect("the stub answers"),
        U256::from(55u64),
        "the dynamic read answers the chain's current word"
    );
    assert_eq!(
        stub.storage_members(),
        reads_before + 1,
        "the miss really went to the chain"
    );

    // The registration lands AFTER that apply, so the settled head is never
    // re-applied: one settle, one report, and the identity it left unknown stays
    // on the dynamic path for that head.
    register_fixture_path(&stages, &ids);
    assert_eq!(
        driver.stats().settled,
        1,
        "a later registration never re-applies a settled head"
    );
    assert_eq!(
        report_of(&driver).writes,
        Vec::new(),
        "and the settled head's own report is unchanged"
    );

    let second_head = head(HEIGHT + 1, second, first);
    settle(&pipeline, &queue, second_head).await;
    let second_report = report_of(&driver);
    assert_eq!(
        second_report.writes,
        vec![(pool_a, slot)],
        "the NEXT head seeds the identity the registration named"
    );
    assert_eq!(second_report.writes_skipped, 0, "nothing relevant unknown");
    assert_eq!(
        driver
            .snapshot(identified(HEIGHT + 1, second))
            .hub
            .snapshot_value(pool_a, slot),
        Some(U256::from(66u64)),
        "the next generation serves the seeded word"
    );
}

/// Test 18 — the set and slice relevance inputs produce an identical report: the
/// membership change is behaviour-free.
#[tokio::test]
async fn the_set_and_slice_relevance_inputs_produce_an_identical_report() {
    let pool_a = address(0x41);
    let pool_b = address(0x42);
    let hash = B256::new([0xd1; 32]);
    let addresses = vec![pool_a, pool_b, weth(), pool_manager()];

    // Two identical sessions over two identical chains: only the relevance
    // INPUT FORM differs, exactly as the two live callers differ (a per-fan-out
    // slice, the session's materialized set).
    let mut reports = Vec::new();
    for by_set in [false, true] {
        let (stages, ids) = session(&[pool_a, pool_b]);
        register_fixture_path(&stages, &ids);
        let stub = ChainStub::serving();
        stub.serve_bal_at(
            hash,
            bal_bytes(&[
                (pool_a, U256::from(1u64), U256::from(7u64)),
                (pool_b, U256::from(2u64), U256::from(8u64)),
                (weth(), U256::from(3u64), U256::from(9u64)),
            ]),
        );
        let provider = stub.provider();
        let driver = Arc::new(BalHeadDriver::new(&provider));
        let ticket = driver.deliver(identified(HEIGHT, hash), genesis());
        let outcome = if by_set {
            // The set is built from a REVERSED iterator: membership carries no
            // order, so a set that disagrees with the slice's order must not
            // move a single decision.
            let mut reversed = addresses.clone();
            reversed.reverse();
            let members = RelevanceSet::from_set(reversed.into_iter().collect::<HashSet<_>>());
            driver
                .advance_with_relevance(ticket, || Arc::new(members.clone()))
                .await
        } else {
            driver.advance(ticket, &addresses).await
        };
        let report = match outcome {
            degenbot_simulation::sim::evm2::delta_snapshot::HeadOutcome::Settled(report) => report,
            other => panic!("the head settles: {other:?}"),
        };
        reports.push((
            report.writes.clone(),
            report.writes_overwritten,
            report.writes_skipped,
            report.outcome.clone(),
        ));
    }
    assert_eq!(
        reports[0], reports[1],
        "the slice and the set are one behaviour, order included"
    );
    assert_eq!(
        reports[0].0,
        vec![
            (pool_a, U256::from(1u64)),
            (pool_b, U256::from(2u64)),
            (weth(), U256::from(3u64)),
        ],
        "every relevant identity seeded, in BAL order, from either input"
    );
    assert_eq!(reports[0].2, 0, "nothing relevant was skipped");
}

/// Test 19 — relevance is derived once per registry revision and reused across
/// heads.
#[tokio::test]
async fn relevance_is_derived_once_per_registry_revision_and_reused_across_heads() {
    let pool_a = address(0x51);
    let pool_b = address(0x52);
    let (stages, ids) = session(&[pool_a, pool_b]);
    let stub = ChainStub::serving();
    let mut previous = genesis();
    let mut hashes = Vec::new();
    for offset in 0..=4u64 {
        let hash = B256::new([0xe0 + u8::try_from(offset).unwrap(); 32]);
        stub.serve_bal_at(hash, bal_bytes(&[]));
        hashes.push((HEIGHT + offset, hash, previous));
        previous = hash;
    }
    let provider = stub.provider();
    let driver = Arc::new(BalHeadDriver::new(&provider));
    let queue = SpawnQueue::new();
    let pipeline = armed_pipeline(&driver, &queue, source(&stages, settlement_facts()));

    // Three heads with a static registry: ONE derivation serves all of them.
    for (number, hash, parent) in hashes.iter().take(3) {
        settle(&pipeline, &queue, head(*number, *hash, *parent)).await;
    }
    assert_eq!(
        pipeline.relevance_derivations(),
        1,
        "a static registry derives once and every head reuses it"
    );

    // A registration moves the identity clock: the next head derives once more
    // and the head after it reuses that derivation.
    register_fixture_path(&stages, &ids);
    for (number, hash, parent) in hashes.iter().skip(3) {
        settle(&pipeline, &queue, head(*number, *hash, *parent)).await;
    }
    assert_eq!(
        pipeline.relevance_derivations(),
        2,
        "each registry revision derives exactly once"
    );
}
