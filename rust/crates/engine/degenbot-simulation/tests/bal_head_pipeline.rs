//! The canonical-head pipeline's own lifecycle pins: every accepted head is
//! delivered on the producer's edge, prepared on a phase the test drains, and
//! applied to the exact anchor it was prepared for.
//!
//! # Why the deterministic spawn queue is the seam
//!
//! The pipeline's producer side (`deliver_head`) must never await a BAL — the
//! pump task hands over a header and moves on — so the whole prepare/apply
//! phase runs through [`HeadSpawn`]. These pins inject a queue instead of the
//! ambient runtime: every spawned phase parks until the test runs it, so "the
//! delivery requested nothing" and "the late list landed after the supersede"
//! are assertions about the queue, never a timing hope. No test here sleeps,
//! races a wall clock, or dials a node: the chain stub answers one scripted
//! storage word and one raw list per exact hash, and the modeled chain derives
//! both from one deterministic write history.
//!
//! The driver's own transition pins (lineage refusals, the read window, the
//! retention, compaction) live in `bal_head_lifecycle.rs`; this file
//! pins the same behaviors at the PIPELINE seam, where the phase that produces
//! them is spawned rather than awaited by the test, and adds the one behavior
//! the seam owns outright: a head is prepared with no candidate ever running,
//! because the canonical head — not the fan-out — is the clock.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "the stub chain and its assertions fail loudly on a broken fixture"
)]

use alloy::primitives::{Address, B256, U256};
use degenbot_core::canonical_head::{CanonicalHead, CanonicalHeadSink};
use degenbot_simulation::sim::evm2::bal_heads::{BalHeadPipeline, HeadSpawn, NoRelevance};
use degenbot_simulation::sim::evm2::delta_snapshot::{
    BalHeadDriver, DeltaReport, HeadCold, HeadReadiness,
};
use degenbot_simulation::sim::evm2::read_hub::{CarryOutcome, CarryRefusal};
use degenbot_simulation::sim::ReadAnchor;
use futures_util::future::BoxFuture;
use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::task::{Context, Waker};

#[path = "../examples/shared/chain_stub.rs"]
#[expect(dead_code)]
mod chain_stub;
#[path = "../examples/shared/delta_parity.rs"]
#[expect(dead_code)]
mod delta_parity;
#[path = "../examples/shared/modeled_chain.rs"]
#[expect(dead_code)]
mod modeled_chain;
#[path = "../examples/shared/real_coverage.rs"]
#[expect(dead_code)]
mod real_coverage;

use chain_stub::ChainStub;
use modeled_chain::{bal_bytes, block_hash, ChainState, Key, StubChain};

/// The height every test advances from, unless the test is about lineage.
const HEIGHT: u64 = 100;

/// The deterministic spawn seam: every phase the pipeline spawns parks here
/// until the test drains it, in the order the test chooses.
#[derive(Clone, Default)]
struct SpawnQueue {
    parked: Arc<Mutex<Vec<BoxFuture<'static, ()>>>>,
}

impl SpawnQueue {
    fn new() -> Self {
        Self::default()
    }

    /// The [`HeadSpawn`] the pipeline is constructed with.
    fn spawner(&self) -> HeadSpawn {
        let parked = Arc::clone(&self.parked);
        Arc::new(move |phase: BoxFuture<'static, ()>| parked.lock().unwrap().push(phase))
    }

    /// Phases spawned and not yet run.
    fn len(&self) -> usize {
        self.parked.lock().unwrap().len()
    }

    /// Take the oldest parked phase WITHOUT running it: the test decides when a
    /// head's preparation lands, and in which order.
    fn take(&self) -> Option<BoxFuture<'static, ()>> {
        let mut parked = self.parked.lock().unwrap();
        if parked.is_empty() {
            None
        } else {
            Some(parked.remove(0))
        }
    }

    /// Run the oldest parked phase to completion.
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

/// A hash no fixture links to, for the parent of a session's first head.
fn genesis() -> B256 {
    B256::new([0x99; 32])
}

/// An address distinct from the fixtures' other identities.
fn address(tag: u8) -> Address {
    Address::new([tag; 20])
}

/// One exact-hash anchor.
fn identified(number: u64, hash: B256) -> ReadAnchor {
    ReadAnchor::identified(number, hash)
}

/// One accepted canonical head.
fn head(number: u64, hash: B256, parent_hash: B256) -> CanonicalHead {
    CanonicalHead::new(number, hash, parent_hash)
}

/// An armed pipeline whose phases the test drains, over ONE session driver.
fn armed_pipeline(driver: &Arc<BalHeadDriver>, queue: &SpawnQueue) -> Arc<BalHeadPipeline> {
    let pipeline =
        BalHeadPipeline::with_spawn_and_relevance(queue.spawner(), Arc::new(NoRelevance));
    assert!(pipeline.arm(Arc::clone(driver)), "the session arms it once");
    pipeline
}

/// The readiness the session reports at one exact head.
fn readiness(driver: &BalHeadDriver, canonical: CanonicalHead) -> HeadReadiness {
    driver
        .snapshot(identified(canonical.number, canonical.hash))
        .readiness
}

/// The report the pipeline's own apply produced for the head it just settled.
fn settled(driver: &BalHeadDriver, canonical: CanonicalHead) -> Arc<DeltaReport> {
    assert_eq!(
        driver.delivered(),
        Some(identified(canonical.number, canonical.hash)),
        "the driver holds the head the pipeline settled"
    );
    driver
        .last_report()
        .expect("the pipeline's apply produced a report")
}

/// A head the pump skipped settles cold, and the next consecutive head carries
/// the gap head's own generation — the chain is re-established, never spliced
/// across the gap it could not verify.
///
/// The pump drops and coalesces headers (WS duplicates, header-gap backfill,
/// recovery), so a skipped head is the normal shape of a cold head. What matters
/// is that the skip is a POINT, not a break: the pipeline's spawned phase settles
/// the gap head cold and the very next accepted head links it.
#[tokio::test]
async fn a_skipped_head_settles_cold_and_the_next_consecutive_head_carries() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let base = B256::new([0xb0; 32]);
    let skipped = B256::new([0xb2; 32]);
    let consecutive = B256::new([0xb3; 32]);
    stub.serve_bal_at(base, bal_bytes(&[]));
    stub.serve_bal_at(skipped, bal_bytes(&[]));
    stub.serve_bal_at(consecutive, bal_bytes(&[]));
    // The gap head's own post-block word: the value its generation settles
    // dynamically and the head above it must carry.
    stub.serve_at(HEIGHT + 2, address(0x11), U256::from(2u64), 6);
    let driver = Arc::new(BalHeadDriver::new(&provider));
    let queue = SpawnQueue::new();
    let pipeline = armed_pipeline(&driver, &queue);

    // The session's first head: delivered immediately, prepared off the edge.
    let base_head = head(HEIGHT, base, genesis());
    pipeline.on_canonical_head(base_head);
    assert_eq!(queue.len(), 1, "the edge spawned the head's one phase");
    assert_eq!(
        stub.bal_requests(),
        0,
        "the delivery itself requests nothing: the BAL rides the spawned phase"
    );
    assert_eq!(
        readiness(&driver, base_head),
        HeadReadiness::Cold(HeadCold::Unprepared),
        "an undrained head is the cold path, never a blocked producer"
    );
    assert!(queue.run_one().await, "the parked phase runs");
    assert_eq!(
        stub.bal_requests(),
        1,
        "the phase fetched the head's own list"
    );
    assert_eq!(
        settled(&driver, base_head).outcome,
        CarryOutcome::Cold {
            lineage: Some(CarryRefusal::NoParent),
            bal: None,
        },
        "the cold start has no parent chain to verify"
    );

    // A block two above the installed head: the pump never delivered the one
    // between them, so there is no lineage to link.
    let skipped_head = head(HEIGHT + 2, skipped, B256::new([0xb1; 32]));
    pipeline.on_canonical_head(skipped_head);
    assert!(queue.run_one().await, "the gap head's phase runs");
    assert_eq!(
        settled(&driver, skipped_head).outcome,
        CarryOutcome::Cold {
            lineage: Some(CarryRefusal::NotConsecutive),
            bal: None,
        },
        "a skipped head settles cold"
    );
    assert_eq!(
        readiness(&driver, skipped_head),
        HeadReadiness::Cold(HeadCold::Settled {
            lineage: Some(CarryRefusal::NotConsecutive),
            bal: None,
        }),
        "the gap head reports why it carries nothing"
    );

    // The gap head's generation settles one word of its own.
    let gap_view = driver.snapshot(identified(HEIGHT + 2, skipped)).hub;
    assert_eq!(
        gap_view
            .read_storage(address(0x11), U256::from(2u64))
            .await
            .expect("the stub answers"),
        U256::from(6u64),
        "the gap head reads the chain's own word"
    );

    // The head above the gap is consecutive to what installed, so it carries.
    let consecutive_head = head(HEIGHT + 3, consecutive, skipped);
    pipeline.on_canonical_head(consecutive_head);
    assert!(queue.run_one().await, "the consecutive head's phase runs");
    let report = settled(&driver, consecutive_head);
    assert_eq!(
        report.outcome,
        CarryOutcome::Carried { carried_values: 1 },
        "the head above the gap re-establishes the chain"
    );
    assert_eq!(
        readiness(&driver, consecutive_head),
        HeadReadiness::Ready,
        "the re-established head is ready"
    );
    let members = stub.storage_members();
    let view = driver.snapshot(identified(HEIGHT + 3, consecutive));
    assert_eq!(
        view.hub.snapshot_value(address(0x11), U256::from(2u64)),
        Some(U256::from(6u64)),
        "the gap head's own settle carried forward"
    );
    assert_eq!(
        stub.storage_members(),
        members,
        "the carried word is served from the generation, not fetched"
    );
}

/// A same-height reorg supersedes the branch it replaces, and a reorg that lands
/// over an INSTALLED branch starts cold: the abandoned branch's values never
/// resurrect, and the head built on the survivor carries only the survivor's
/// generation.
///
/// Both halves are the one behavior: the replacement is a different canonical
/// block at the same number, so the installed anchor is never one below it. The
/// only difference is what was installed when it arrived — the branch the reorg
/// abandoned (cold, the chain re-establishes at the next head) or the last
/// settled parent (carried, because the replacement DOES link it).
#[tokio::test]
async fn a_same_height_reorg_supersedes_and_the_new_branch_starts_cold() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let base = B256::new([0xc0; 32]);
    let abandoned = B256::new([0xc1; 32]);
    let replacement = B256::new([0xc2; 32]);
    let reorged = B256::new([0xc3; 32]);
    let successor = B256::new([0xc4; 32]);
    // The abandoned branch would have written a word the replacement never
    // mentions: if either branch leaked, that value would be reachable.
    let slot = U256::from(4u64);
    stub.serve_bal_at(base, bal_bytes(&[]));
    stub.serve_bal_at(
        abandoned,
        bal_bytes(&[(address(0x33), slot, U256::from(666u64))]),
    );
    stub.serve_bal_at(replacement, bal_bytes(&[]));
    stub.serve_bal_at(reorged, bal_bytes(&[]));
    stub.serve_bal_at(successor, bal_bytes(&[]));
    stub.serve_at(HEIGHT, address(0x33), slot, 777);
    stub.serve_at(HEIGHT + 1, address(0x33), slot, 888);
    let driver = Arc::new(BalHeadDriver::new(&provider));
    let queue = SpawnQueue::new();
    let pipeline = armed_pipeline(&driver, &queue);

    let base_head = head(HEIGHT, base, genesis());
    pipeline.on_canonical_head(base_head);
    assert!(queue.run_one().await);
    // The base generation settles the word the abandoned branch would overwrite.
    driver
        .snapshot(identified(HEIGHT, base))
        .hub
        .read_storage(address(0x33), slot)
        .await
        .expect("the stub answers");

    // The head at HEIGHT + 1 is delivered and its phase is left parked: the
    // reorg arrives while that preparation is still in flight.
    let abandoned_head = head(HEIGHT + 1, abandoned, base);
    pipeline.on_canonical_head(abandoned_head);
    assert_eq!(queue.len(), 1, "the abandoned head's phase is parked");
    assert_eq!(
        readiness(&driver, abandoned_head),
        HeadReadiness::Cold(HeadCold::Unprepared),
        "the in-flight head is cold"
    );

    // The same height, a different canonical block, the SAME parent: the new
    // delivery supersedes the in-flight head.
    let replacement_head = head(HEIGHT + 1, replacement, base);
    pipeline.on_canonical_head(replacement_head);
    assert_eq!(queue.len(), 2, "one parked phase per delivered head");
    assert_eq!(
        driver.stats().superseded,
        1,
        "the replacement superseded the in-flight head"
    );
    assert_eq!(
        readiness(&driver, abandoned_head),
        HeadReadiness::Cold(HeadCold::Superseded),
        "the abandoned head reports that it was superseded"
    );
    let abandoned_view = driver.snapshot(identified(HEIGHT + 1, abandoned));
    assert_eq!(
        abandoned_view.hub.snapshot_len(),
        0,
        "the abandoned branch carries nothing at its own anchor"
    );

    // The abandoned head's fenced phase installs nothing; the survivor links
    // the last INSTALLED generation — the branch that never landed is gone.
    assert!(queue.run_one().await, "the abandoned head's phase runs");
    assert_eq!(
        driver.installed(),
        Some(identified(HEIGHT, base)),
        "a superseded phase installs nothing"
    );
    assert!(queue.run_one().await, "the replacement's phase runs");
    assert_eq!(
        settled(&driver, replacement_head).outcome,
        CarryOutcome::Carried { carried_values: 1 },
        "the survivor carries the generation it actually links"
    );
    let survivor_view = driver.snapshot(identified(HEIGHT + 1, replacement));
    assert_eq!(
        survivor_view.readiness,
        HeadReadiness::Ready,
        "the survivor settled on the installed parent"
    );
    assert_eq!(
        survivor_view.hub.snapshot_value(address(0x33), slot),
        Some(U256::from(777u64)),
        "the base's word, never the abandoned list's 666"
    );
    assert_eq!(
        driver.previous(),
        Some(identified(HEIGHT, base)),
        "the linked transition puts the base in the read window"
    );

    // The reorg lands over an INSTALLED branch this time: the replacement's
    // parent is the installed branch's own parent, so the installed anchor is
    // not one below it and the new branch starts cold.
    reorg_over_an_installed_branch_starts_cold(
        &pipeline, &queue, &driver, base, reorged, successor, slot,
    )
    .await;
}

/// The second half of the same-height reorg: a replacement whose parent is the
/// INSTALLED branch's own parent links nothing, so it starts cold, and the head
/// above it carries only the survivor's generation.
async fn reorg_over_an_installed_branch_starts_cold(
    pipeline: &BalHeadPipeline,
    queue: &SpawnQueue,
    driver: &BalHeadDriver,
    base: B256,
    reorged: B256,
    successor: B256,
    slot: U256,
) {
    let reorged_head = head(HEIGHT + 1, reorged, base);
    pipeline.on_canonical_head(reorged_head);
    assert!(
        queue.run_one().await,
        "the installed-branch reorg's phase runs"
    );
    assert_eq!(
        settled(driver, reorged_head).outcome,
        CarryOutcome::Cold {
            lineage: Some(CarryRefusal::NotConsecutive),
            bal: None,
        },
        "a same-height reorg never links the head it replaced"
    );
    let reorged_view = driver.snapshot(identified(HEIGHT + 1, reorged));
    assert_eq!(
        reorged_view.readiness,
        HeadReadiness::Cold(HeadCold::Settled {
            lineage: Some(CarryRefusal::NotConsecutive),
            bal: None,
        }),
        "the new branch starts cold and says why"
    );
    assert_eq!(
        reorged_view.hub.snapshot_len(),
        0,
        "no parent value is carried into the new branch"
    );
    assert_eq!(
        driver.previous(),
        None,
        "a cold settle leaves no previous anchor: the window shrinks across a reorg"
    );

    // The survivor's own word, and the head above it carries only that.
    assert_eq!(
        reorged_view
            .hub
            .read_storage(address(0x33), slot)
            .await
            .expect("the stub answers"),
        U256::from(888u64)
    );
    let successor_head = head(HEIGHT + 2, successor, reorged);
    pipeline.on_canonical_head(successor_head);
    assert!(queue.run_one().await);
    assert_eq!(
        settled(driver, successor_head).outcome,
        CarryOutcome::Carried { carried_values: 1 },
        "the new branch's second block re-establishes the chain"
    );
    let successor_view = driver.snapshot(identified(HEIGHT + 2, successor));
    assert_eq!(
        successor_view.readiness,
        HeadReadiness::Ready,
        "the re-established head is ready"
    );
    assert_eq!(
        successor_view.hub.snapshot_value(address(0x33), slot),
        Some(U256::from(888u64)),
        "the survivor's own word carried"
    );
    assert_eq!(
        successor_view
            .hub
            .read_storage(address(0x33), slot)
            .await
            .expect("the stub answers"),
        U256::from(888u64),
        "and never the abandoned branch's 666 or the pre-reorg 777"
    );
}

/// Every consecutive head of a modeled stream carries after the cold start: the
/// FIRST delivered head settles cold (no installed parent), and every accepted
/// head after it settles READY, carrying the parent generation and resolving
/// each identity to the modeled chain's own word.
#[tokio::test]
async fn every_consecutive_head_of_a_modeled_stream_carries_after_the_cold_start() {
    let first = 100u64;
    let last = 105u64;
    let keys: BTreeSet<Key> = [
        (address(0x11), U256::from(1u64)),
        (address(0x22), U256::from(1u64)),
        (address(0x33), U256::from(7u64)),
    ]
    .into_iter()
    .collect();
    let chain = ChainState::build(&keys, first, last).expect("a modeled chain over the keys");
    let stub = StubChain {
        chain: Arc::new(chain),
        plant: Arc::new(AtomicBool::new(false)),
    };
    let provider = stub.provider();
    let keys: Vec<Key> = keys.into_iter().collect();
    let driver = Arc::new(BalHeadDriver::new(&provider));
    let queue = SpawnQueue::new();
    let pipeline = armed_pipeline(&driver, &queue);

    for number in first..=last {
        let canonical = head(number, block_hash(number), block_hash(number - 1));
        pipeline.on_canonical_head(canonical);
        assert_eq!(queue.len(), 1, "head {number} spawned exactly one phase");
        assert_eq!(
            readiness(&driver, canonical),
            HeadReadiness::Cold(HeadCold::Unprepared),
            "head {number} is cold until its own phase runs"
        );
        assert!(queue.run_one().await, "head {number}'s phase runs");
        let report = settled(&driver, canonical);
        let view = driver.snapshot(identified(number, block_hash(number)));
        if number == first {
            assert_eq!(
                report.outcome,
                CarryOutcome::Cold {
                    lineage: Some(CarryRefusal::NoParent),
                    bal: None,
                },
                "the stream's cold start: no installed parent to carry"
            );
            assert_eq!(
                view.readiness,
                HeadReadiness::Cold(HeadCold::Settled {
                    lineage: Some(CarryRefusal::NoParent),
                    bal: None,
                })
            );
        } else {
            assert_eq!(
                report.outcome,
                CarryOutcome::Carried {
                    carried_values: keys.len(),
                },
                "head {number} carries the chain its parent settled"
            );
            assert_eq!(
                view.readiness,
                HeadReadiness::Ready,
                "head {number} answers ready"
            );
        }
        for &(key_address, slot) in &keys {
            let truth = stub
                .chain
                .value_at(key_address, slot, number)
                .expect("the chain models this block");
            let resolved = match view.hub.snapshot_value(key_address, slot) {
                Some(value) => value,
                None => view
                    .hub
                    .read_storage(key_address, slot)
                    .await
                    .expect("the stub answers"),
            };
            assert_eq!(
                resolved, truth,
                "head {number}: {key_address}/{slot} resolves the chain's own word"
            );
        }
    }
}

/// A head delivered with no candidate ever running is still prepared and
/// applied: the canonical head is the clock, so coverage is every accepted
/// header — not the sparse set of blocks that happened to carry candidates.
#[tokio::test]
async fn a_head_delivered_with_no_candidate_ever_running_is_still_prepared() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let base = B256::new([0xd0; 32]);
    let child = B256::new([0xd1; 32]);
    let slot = U256::from(1u64);
    stub.serve_bal_at(base, bal_bytes(&[]));
    stub.serve_bal_at(child, bal_bytes(&[(address(0x11), slot, U256::from(5u64))]));
    stub.serve_at(HEIGHT, address(0x11), slot, 1);
    stub.serve_at(HEIGHT + 1, address(0x11), slot, 5);
    let driver = Arc::new(BalHeadDriver::new(&provider));
    let queue = SpawnQueue::new();
    let pipeline = armed_pipeline(&driver, &queue);

    // No dispatch, no fan-out, no candidate: only the head edge.
    let base_head = head(HEIGHT, base, genesis());
    pipeline.on_canonical_head(base_head);
    assert!(queue.run_one().await);
    assert_eq!(
        driver
            .snapshot(identified(HEIGHT, base))
            .hub
            .read_storage(address(0x11), slot)
            .await
            .expect("the stub answers"),
        U256::from(1u64),
        "the base generation settles the chain's word"
    );
    assert_eq!(
        stub.storage_members(),
        1,
        "the one read in this test is the fixture's, not a candidate's"
    );

    let child_head = head(HEIGHT + 1, child, base);
    pipeline.on_canonical_head(child_head);
    assert_eq!(
        stub.bal_requests(),
        1,
        "the second delivery requested nothing: its phase is parked"
    );
    assert!(queue.run_one().await);
    assert_eq!(
        stub.bal_requests(),
        2,
        "the head was prepared with no candidate ever dispatched"
    );
    let report = settled(&driver, child_head);
    assert_eq!(
        report.outcome,
        CarryOutcome::Carried { carried_values: 1 },
        "the parent chain carried with no fan-out involved"
    );
    assert_eq!(
        report.writes,
        vec![(address(0x11), slot)],
        "the head's own list is what applied"
    );
    assert_eq!(report.writes_overwritten, 1, "over the served parent word");
    assert_eq!(readiness(&driver, child_head), HeadReadiness::Ready);

    let members = stub.storage_members();
    let view = driver.snapshot(identified(HEIGHT + 1, child));
    assert_eq!(
        view.hub.snapshot_value(address(0x11), slot),
        Some(U256::from(5u64)),
        "the head's post-block word is served from its generation"
    );
    assert_eq!(
        stub.storage_members(),
        members,
        "and not from a fetch: the list, not a read, produced it"
    );
}

/// A prepared list that lands after its head was superseded installs nothing,
/// and the chain survives: the survivor's branch settles, and the head built on
/// it carries only the survivor's generation.
#[tokio::test]
async fn a_prepared_list_that_lands_after_supersede_installs_nothing_and_the_chain_survives() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let base = B256::new([0xe0; 32]);
    let abandoned = B256::new([0xe1; 32]);
    let replacement = B256::new([0xe2; 32]);
    let heir = B256::new([0xe3; 32]);
    let slot = U256::from(4u64);
    stub.serve_bal_at(base, bal_bytes(&[]));
    stub.serve_bal_at(
        abandoned,
        bal_bytes(&[(address(0x33), slot, U256::from(666u64))]),
    );
    stub.serve_bal_at(replacement, bal_bytes(&[]));
    stub.serve_bal_at(heir, bal_bytes(&[]));
    stub.serve_at(HEIGHT + 1, address(0x33), slot, 777);
    stub.serve_at(HEIGHT + 2, address(0x33), slot, 888);
    let driver = Arc::new(BalHeadDriver::new(&provider));
    let queue = SpawnQueue::new();
    let pipeline = armed_pipeline(&driver, &queue);

    let base_head = head(HEIGHT, base, genesis());
    pipeline.on_canonical_head(base_head);
    assert!(queue.run_one().await);
    assert_eq!(
        driver.installed(),
        Some(identified(HEIGHT, base)),
        "the session has its parent generation"
    );
    stub.clear();

    // The abandoned head's phase is parked and then polled once, so its list
    // fetch is genuinely OUTSTANDING — the window a real apply widens.
    let abandoned_head = head(HEIGHT + 1, abandoned, base);
    pipeline.on_canonical_head(abandoned_head);
    let mut late = queue.take().expect("the abandoned head's phase is parked");
    stub.set_slow(true);
    assert!(
        late.as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending(),
        "the abandoned list is in flight"
    );

    // The same-height replacement arrives while that list is outstanding.
    let replacement_head = head(HEIGHT + 1, replacement, base);
    pipeline.on_canonical_head(replacement_head);
    assert_eq!(queue.len(), 1, "the replacement's own phase is parked");
    assert_eq!(
        driver.stats().superseded,
        1,
        "the replacement superseded the in-flight head"
    );

    stub.set_slow(false);
    late.await;
    assert_eq!(
        stub.bal_requests(),
        1,
        "the abandoned list was fetched once and landed after the supersede"
    );
    assert_eq!(
        driver.installed(),
        Some(identified(HEIGHT, base)),
        "the late list installed nothing"
    );
    assert_eq!(
        driver.stats().discarded,
        1,
        "the late apply was discarded rather than installed"
    );
    assert_eq!(
        readiness(&driver, abandoned_head),
        HeadReadiness::Cold(HeadCold::Superseded),
        "the abandoned head stays dead"
    );

    // The survivor's phase settles, and the chain continues from it.
    the_chain_survives_the_abandoned_list(&pipeline, &queue, &driver, replacement_head, heir, slot)
        .await;
}

/// The other half of the late arrival: the surviving head settles on the
/// generation the abandoned list never replaced, and the head above it carries
/// that branch only.
async fn the_chain_survives_the_abandoned_list(
    pipeline: &BalHeadPipeline,
    queue: &SpawnQueue,
    driver: &BalHeadDriver,
    replacement_head: CanonicalHead,
    heir: B256,
    slot: U256,
) {
    assert!(queue.run_one().await, "the replacement's phase runs");
    assert_eq!(
        settled(driver, replacement_head).outcome,
        CarryOutcome::Carried { carried_values: 0 },
        "the survivor links the generation the abandoned list never replaced"
    );
    let heir_head = head(HEIGHT + 2, heir, replacement_head.hash);
    pipeline.on_canonical_head(heir_head);
    assert!(queue.run_one().await, "the heir's phase runs");
    assert_eq!(
        settled(driver, heir_head).outcome,
        CarryOutcome::Carried { carried_values: 0 },
        "the chain survives the abandoned list"
    );
    let view = driver.snapshot(identified(HEIGHT + 2, heir));
    assert_eq!(
        view.readiness,
        HeadReadiness::Ready,
        "the head above the survivor answers ready"
    );
    assert_eq!(
        view.hub.snapshot_value(address(0x33), slot),
        None,
        "the abandoned branch's write never entered the survivor's chain"
    );
    assert_eq!(
        view.hub
            .read_storage(address(0x33), slot)
            .await
            .expect("the stub answers"),
        U256::from(888u64),
        "the survivor's chain answers, never the abandoned value"
    );
}
