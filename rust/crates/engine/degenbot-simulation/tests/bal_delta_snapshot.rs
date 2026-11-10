//! Behavior pins for the parent-linked delta-chain snapshot prototype
//! (`degenbot_simulation::sim::evm2::delta_snapshot`), read side by side with
//! the full-clone control (`degenbot_simulation::sim::evm2::read_hub`): what a
//! delta child may serve, how compaction bounds the walk, and where parent and
//! child part ways.
//!
//! Every test name states the behavior it pins. The stub chain answers one
//! storage word per request and one raw list per exact block hash, so a
//! dispatched member count is exactly what took the dynamic path.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "the stub chain and its assertions fail loudly on a broken fixture"
)]

use alloy::primitives::{Address, B256, U256};
use degenbot_simulation::sim::evm2::delta_snapshot::{
    DeltaHub, DeltaReport, DeltaSnapshotCache, DEFAULT_MAX_DEPTH,
};
use degenbot_simulation::sim::evm2::read_hub::{
    CarryOutcome, CarryRefusal, CarryReport, ReadHub, ReadHubCache,
};
use degenbot_simulation::sim::ReadAnchor;
use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

// The shared helpers carry more surface than one consumer drives; this suite
// reads the scripted chain stub, the modeled list encoder, and the fixture
// loader.
#[path = "../examples/shared/chain_stub.rs"]
mod chain_stub;
#[path = "../examples/shared/delta_parity.rs"]
mod delta_parity;
#[path = "../examples/shared/modeled_chain.rs"]
#[expect(dead_code)]
mod modeled_chain;
#[path = "../examples/shared/real_coverage.rs"]
#[expect(dead_code)]
mod real_coverage;

use chain_stub::ChainStub;
use delta_parity::{real_keys, real_parity_blocks, replay};
use modeled_chain::{bal_bytes, block_hash, ChainState, Coverage, Key, StubChain};
use real_coverage::RealCoverage;

/// The anchor height every test advances at, unless the test is about lineage.
const HEIGHT: u64 = 100;

/// An address distinct from the write fixtures' account.
fn address(tag: u8) -> Address {
    Address::new([tag; 20])
}

/// The delta child resolves its own final writes before ancestors, carries the
/// parent's settled words through the chain, and settles its own dynamic
/// reads — while the parent stays immutable and the child's own map holds
/// exactly its writes and settles, never a copy of the parent's map.
#[tokio::test]
async fn delta_child_resolves_its_own_writes_before_ancestors_and_keeps_parents_immutable() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, address(0x11), U256::from(1u64), 1);
    stub.serve_at(HEIGHT, address(0x11), U256::from(2u64), 2);
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(3u64), 77);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(
        child_hash,
        bal_bytes(&[(address(0x11), U256::from(1u64), U256::from(3u64))]),
    );
    let cache = DeltaSnapshotCache::new(&provider);

    let parent = cache.for_anchor(ReadAnchor::identified(HEIGHT, parent_hash));
    let x = parent.read_storage(address(0x11), U256::from(1u64)).await;
    let y = parent.read_storage(address(0x11), U256::from(2u64)).await;
    assert_eq!(x.unwrap(), U256::from(1u64));
    assert_eq!(y.unwrap(), U256::from(2u64));
    stub.clear();

    let (child, report) = cache
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 1, child_hash),
            parent_hash,
            &[address(0x11)],
        )
        .await;
    assert_eq!(report.outcome, CarryOutcome::Carried { carried_values: 2 });
    assert_eq!(report.writes, vec![(address(0x11), U256::from(1u64))]);
    assert_eq!(report.writes_overwritten, 1);

    // The child's own final write shadows the ancestor's value.
    assert_eq!(
        child.snapshot_value(address(0x11), U256::from(1u64)),
        Some(U256::from(3u64))
    );
    let x = child.read_storage(address(0x11), U256::from(1u64)).await;
    assert_eq!(x.unwrap(), U256::from(3u64));
    // The carried parent word resolves through the chain — no fetch.
    assert_eq!(
        child.snapshot_value(address(0x11), U256::from(2u64)),
        Some(U256::from(2u64))
    );
    let y = child.read_storage(address(0x11), U256::from(2u64)).await;
    assert_eq!(y.unwrap(), U256::from(2u64));
    // An identity no source knows stays unknown until a dynamic read settles
    // it into the child's own map.
    assert_eq!(child.snapshot_value(address(0x11), U256::from(3u64)), None);
    let z = child.read_storage(address(0x11), U256::from(3u64)).await;
    assert_eq!(z.unwrap(), U256::from(77u64));
    assert_eq!(
        child.snapshot_value(address(0x11), U256::from(3u64)),
        Some(U256::from(77u64))
    );
    assert_eq!(
        stub.storage_members(),
        1,
        "only the unknown read was dynamic"
    );

    // The child's own map holds exactly its write plus its settle, and the
    // visible total is the parent's two words plus the one new identity.
    assert_eq!(child.delta_len(), 2);
    assert_eq!(child.snapshot_len(), 3);

    // The parent is immutable and independent: its own values are untouched by
    // the child's write or its settles.
    assert_eq!(
        parent.snapshot_value(address(0x11), U256::from(1u64)),
        Some(U256::from(1u64))
    );
    assert_eq!(parent.delta_len(), 2);
    assert_eq!(parent.snapshot_len(), 2);
    let x = parent.read_storage(address(0x11), U256::from(1u64)).await;
    assert_eq!(
        x.unwrap(),
        U256::from(1u64),
        "the child's write never mutates the parent"
    );
}

/// The child's write policy is the carry's: a known key takes the write
/// whatever the relevance set says, an unknown write seeds only when its
/// address is relevant, and an irrelevant unknown write stays unknown for the
/// dynamic path.
#[tokio::test]
async fn final_write_relevance_matches_the_carry_policy() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, address(0x11), U256::from(1u64), 1);
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(1u64), 5);
    stub.serve_at(HEIGHT + 1, address(0x22), U256::from(9u64), 7);
    stub.serve_at(HEIGHT + 1, address(0x33), U256::from(4u64), 8);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(
        child_hash,
        bal_bytes(&[
            (address(0x11), U256::from(1u64), U256::from(5u64)),
            (address(0x22), U256::from(9u64), U256::from(7u64)),
            (address(0x33), U256::from(4u64), U256::from(8u64)),
        ]),
    );
    let cache = DeltaSnapshotCache::new(&provider);
    let parent = cache.for_anchor(ReadAnchor::identified(HEIGHT, parent_hash));
    let x = parent.read_storage(address(0x11), U256::from(1u64)).await;
    x.unwrap();
    stub.clear();

    let (child, report) = cache
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 1, child_hash),
            parent_hash,
            &[address(0x22)],
        )
        .await;
    assert_eq!(
        report.writes,
        vec![
            (address(0x11), U256::from(1u64)),
            (address(0x22), U256::from(9u64))
        ]
    );
    assert_eq!(report.writes_overwritten, 1, "the known key took the write");
    assert_eq!(
        report.writes_skipped, 1,
        "the irrelevant unknown was counted"
    );

    let x = child.read_storage(address(0x11), U256::from(1u64)).await;
    assert_eq!(x.unwrap(), U256::from(5u64), "known beats relevance");
    assert_eq!(
        child.snapshot_value(address(0x22), U256::from(9u64)),
        Some(U256::from(7u64)),
        "the relevant unknown write seeded"
    );
    assert_eq!(
        child.snapshot_value(address(0x33), U256::from(4u64)),
        None,
        "the irrelevant unknown write stays unknown"
    );
    let w = child.read_storage(address(0x33), U256::from(4u64)).await;
    assert_eq!(w.unwrap(), U256::from(8u64), "the dynamic path answers");
    assert_eq!(
        stub.storage_members(),
        1,
        "only the left-unknown identity took the dynamic path"
    );
}

/// A same-anchor delta advance is idempotent: the installed generation comes
/// back unchanged and no BAL work repeats.
#[tokio::test]
async fn same_anchor_advance_is_idempotent_and_repeats_no_bal_work() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, address(0x11), U256::from(1u64), 1);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(
        child_hash,
        bal_bytes(&[(address(0x11), U256::from(1u64), U256::from(3u64))]),
    );
    let cache = DeltaSnapshotCache::new(&provider);
    let parent = cache.for_anchor(ReadAnchor::identified(HEIGHT, parent_hash));
    let x = parent.read_storage(address(0x11), U256::from(1u64)).await;
    x.unwrap();
    stub.clear();

    let (child, report) = cache
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 1, child_hash),
            parent_hash,
            &[],
        )
        .await;
    assert_eq!(report.outcome, CarryOutcome::Carried { carried_values: 1 });
    assert_eq!(stub.bal_requests(), 1, "one BAL fetch");

    let (again, report) = cache
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 1, child_hash),
            parent_hash,
            &[],
        )
        .await;
    assert!(
        Arc::ptr_eq(&child, &again),
        "the installed generation comes back"
    );
    assert_eq!(report.outcome, CarryOutcome::AlreadyCurrent);
    assert!(report.writes.is_empty(), "no writes re-applied");
    assert!(report.bal_latency.is_zero(), "no BAL work");
    assert!(report.link_time.is_zero(), "no link");
    assert_eq!(
        stub.bal_requests(),
        1,
        "the idempotent call repeated no BAL work"
    );
    assert_eq!(child.snapshot_len(), 1, "the snapshot is unchanged");
    let via_for_anchor = cache.for_anchor(ReadAnchor::identified(HEIGHT + 1, child_hash));
    assert!(Arc::ptr_eq(&child, &via_for_anchor));
    let x = child.read_storage(address(0x11), U256::from(1u64)).await;
    assert_eq!(x.unwrap(), U256::from(3u64));
}

/// Parent and child stay independent after the transition: a value the parent
/// settles afterwards stays handle-local (the child reads its own anchor's
/// word dynamically, never the parent's late value), and a generation the
/// cache replaced leaves none of its values behind.
#[tokio::test]
async fn parent_and_child_stay_independent_after_the_transition() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, address(0x11), U256::from(1u64), 1);
    stub.serve_at(HEIGHT, address(0x11), U256::from(2u64), 2);
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(2u64), 99);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_bytes(&[]));
    let cache = DeltaSnapshotCache::new(&provider);

    let parent = cache.for_anchor(ReadAnchor::identified(HEIGHT, parent_hash));
    let x = parent.read_storage(address(0x11), U256::from(1u64)).await;
    x.unwrap();
    stub.clear();

    let (child, report) = cache
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 1, child_hash),
            parent_hash,
            &[],
        )
        .await;
    assert_eq!(report.outcome, CarryOutcome::Carried { carried_values: 1 });

    // The parent settles a NEW identity after the transition. It stays
    // handle-local — the same visibility boundary as the control's
    // clone-at-transition.
    let late = parent.read_storage(address(0x11), U256::from(2u64)).await;
    assert_eq!(late.unwrap(), U256::from(2u64));
    assert_eq!(
        parent.snapshot_value(address(0x11), U256::from(2u64)),
        Some(U256::from(2u64))
    );
    assert_eq!(
        child.snapshot_value(address(0x11), U256::from(2u64)),
        None,
        "the parent's late settle never reached the child"
    );
    stub.clear();
    let y = child.read_storage(address(0x11), U256::from(2u64)).await;
    assert_eq!(
        y.unwrap(),
        U256::from(99u64),
        "the child reads its own anchor's word, never the parent's late value"
    );
    assert_eq!(
        stub.storage_members(),
        1,
        "only the child's read was dynamic"
    );

    // A replaced generation's values never travel either.
    let replacement = cache.for_anchor(ReadAnchor::identified(HEIGHT + 2, B256::new([0xcc; 32])));
    assert_eq!(replacement.snapshot_len(), 0, "the replacement starts cold");
    assert_eq!(
        child.delta_len(),
        1,
        "the child keeps its own settle and holds no parent copy"
    );
}

/// The control's read shape: register, drain one round, await.
async fn control_read(hub: &ReadHub<'_>, address: Address, slot: U256) -> U256 {
    let read = hub.read_storage(address, slot);
    hub.drain_round().await;
    read.await.expect("the round settles")
}

/// One identity resolved through both representations: the control's drained
/// round and the delta's chain-or-fetch read must agree.
async fn assert_dual_read(
    control_hub: &ReadHub<'_>,
    delta_hub: &DeltaHub<'_>,
    address: Address,
    slot: U256,
) -> U256 {
    let control_value = control_read(control_hub, address, slot).await;
    let delta_value = delta_hub
        .read_storage(address, slot)
        .await
        .expect("the stub answers");
    assert_eq!(
        control_value, delta_value,
        "both representations answer {address}/{slot} alike"
    );
    delta_value
}

/// The two reports agree on everything but timing.
fn assert_reports_match(control: &CarryReport, delta: &DeltaReport) {
    assert_eq!(
        control.outcome, delta.outcome,
        "the same transition happened"
    );
    assert_eq!(control.writes, delta.writes, "the same writes applied");
    assert_eq!(
        control.writes_overwritten, delta.writes_overwritten,
        "the same overwrite count"
    );
    assert_eq!(
        control.writes_skipped, delta.writes_skipped,
        "the same skip count"
    );
}

/// Every generation of a modeled chain resolves identically through the delta
/// chain and the full-clone control — reports, snapshot counts, and values
/// alike — over multi-generation reads, overwrites, unknown reads, and
/// rotating relevance sets.
#[tokio::test]
async fn every_generation_matches_the_full_clone_control_over_a_modeled_chain() {
    let first = 100u64;
    let last = 105u64;
    let keys: BTreeSet<Key> = [
        (address(0x11), U256::from(1u64)),
        (address(0x22), U256::from(1u64)),
        (address(0x33), U256::from(1u64)),
        (address(0x44), U256::from(1u64)),
    ]
    .into_iter()
    .collect();
    let chain = ChainState::build(&keys, first, last).expect("a modeled chain over the keys");
    let stub = StubChain {
        chain: Arc::new(chain),
        plant: Arc::new(AtomicBool::new(false)),
    };
    let provider = stub.provider();
    let reads: Vec<Key> = keys.iter().copied().collect();
    let blocks: Vec<delta_parity::ParityBlock> = (first..=last)
        .map(|number| delta_parity::ParityBlock {
            anchor: ReadAnchor::identified(number, block_hash(number)),
            parent_hash: block_hash(number - 1),
            relevance: if number % 2 == 0 {
                vec![address(0x11), address(0x22)]
            } else {
                vec![address(0x33)]
            },
            reads: reads.clone(),
        })
        .collect();
    let result = replay(&provider, &blocks).await;
    assert!(
        result.mismatches.is_empty(),
        "{} mismatches over {} comparisons: {:?}",
        result.mismatches.len(),
        result.comparisons,
        result.mismatches
    );
    assert_eq!(result.generations, 6);
    assert!(
        result.comparisons >= 50,
        "every key at every generation: {}",
        result.comparisons
    );
    assert!(
        result
            .delta_depths
            .iter()
            .all(|depth| *depth <= DEFAULT_MAX_DEPTH + 1),
        "the walk stays within the documented bound: {:?}",
        result.delta_depths
    );
}

/// The committed real write-coverage capture replays through both
/// representations with zero mismatches: every captured block advanced at its
/// exact hash with its real parent linkage, every captured write identity
/// compared at every generation.
#[tokio::test]
async fn the_real_coverage_fixture_replays_with_zero_delta_vs_full_clone_mismatches() {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/bal_real_coverage.json"
    );
    let real = RealCoverage::load(fixture).expect("the committed capture parses");
    let keys = real_keys(&real);
    assert!(!keys.is_empty(), "the capture wrote some identity");
    let chain = ChainState::build_coverage(&keys, real.first(), real.last(), &real, Coverage::Real)
        .expect("a chain over the captured stretch");
    let stub = StubChain {
        chain: Arc::new(chain),
        plant: Arc::new(AtomicBool::new(false)),
    };
    let provider = stub.provider();
    let blocks = real_parity_blocks(&real, &keys);
    let result = replay(&provider, &blocks).await;
    assert!(
        result.mismatches.is_empty(),
        "{} mismatches over {} comparisons: {:?}",
        result.mismatches.len(),
        result.comparisons,
        result.mismatches
    );
    assert_eq!(
        result.generations,
        usize::try_from(real.last() - real.first() + 1).expect("the stretch fits")
    );
    assert!(
        result.comparisons > 100,
        "every captured identity at every block: {}",
        result.comparisons
    );
}

/// Why a generation's list could not be read.
enum ColdSkip {
    /// No list at that hash: the node answers with a failure.
    Missing,
    /// The node answers the request with JSON `null`.
    Null,
    /// The node does not serve the method.
    Unsupported,
    /// The bytes are not one complete list.
    Malformed,
}

/// A generation whose list cannot be read starts COLD and breaks the chain on
/// both representations: no parent value is served there, and a later
/// generation carries only what the cold generation itself settled — an
/// earlier value never resurrects.
async fn cold_chain_case(skip: ColdSkip) {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    // Chain truth: A settles x=1, y=2, z=3; block 101 rewrote x=77, y=88 but
    // its list is unreadable; at block 102 z reads 6.
    stub.serve_at(HEIGHT, address(0x11), U256::from(1u64), 1);
    stub.serve_at(HEIGHT, address(0x11), U256::from(2u64), 2);
    stub.serve_at(HEIGHT, address(0x11), U256::from(9u64), 3);
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(1u64), 77);
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(2u64), 88);
    stub.serve_at(HEIGHT + 2, address(0x11), U256::from(9u64), 6);
    let hashes = [
        B256::new([0xa0; 32]),
        B256::new([0xa1; 32]),
        B256::new([0xa2; 32]),
    ];
    match skip {
        ColdSkip::Missing => {}
        ColdSkip::Null => stub.set_bal_null(true),
        ColdSkip::Unsupported => stub.set_bal_unsupported(true),
        ColdSkip::Malformed => stub.serve_bal_at(hashes[1], vec![0xff, 0x00]),
    }
    stub.serve_bal_at(hashes[2], bal_bytes(&[]));
    let control = ReadHubCache::new(&provider);
    let delta = DeltaSnapshotCache::new(&provider);

    // A settles x, y, z on both representations.
    let control_a = control.for_anchor(ReadAnchor::identified(HEIGHT, hashes[0]));
    let delta_a = delta.for_anchor(ReadAnchor::identified(HEIGHT, hashes[0]));
    for slot in [1u64, 2, 9] {
        assert_dual_read(&control_a, &delta_a, address(0x11), U256::from(slot)).await;
    }
    stub.clear();

    // B's list is unreadable: cold on both, and only B's own settles live there.
    let (control_b, control_report) = control
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 1, hashes[1]),
            hashes[0],
            &[],
        )
        .await;
    let (delta_b, delta_report) = delta
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 1, hashes[1]),
            hashes[0],
            &[],
        )
        .await;
    stub.set_bal_unsupported(false);
    stub.set_bal_null(false);
    assert_reports_match(&control_report, &delta_report);
    assert_eq!(
        delta_b.snapshot_len(),
        0,
        "a cold generation carries nothing"
    );
    let x = assert_dual_read(&control_b, &delta_b, address(0x11), U256::from(1u64)).await;
    assert_eq!(x, U256::from(77u64), "B's own chain word, never A's 1");
    let y = assert_dual_read(&control_b, &delta_b, address(0x11), U256::from(2u64)).await;
    assert_eq!(y, U256::from(88u64), "B's own chain word, never A's 2");

    // C carries ONLY what B settled (x and y): z never resurrects A's 3.
    let (control_c, control_report) = control
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 2, hashes[2]),
            hashes[1],
            &[],
        )
        .await;
    let (delta_c, delta_report) = delta
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 2, hashes[2]),
            hashes[1],
            &[],
        )
        .await;
    assert_reports_match(&control_report, &delta_report);
    assert_eq!(
        delta_report.outcome,
        CarryOutcome::Carried { carried_values: 2 },
        "only B's two settles carried"
    );
    assert_eq!(
        delta_c.snapshot_value(address(0x11), U256::from(9u64)),
        None,
        "A's z never reaches C"
    );
    let z = assert_dual_read(&control_c, &delta_c, address(0x11), U256::from(9u64)).await;
    assert_eq!(
        z,
        U256::from(6u64),
        "C's own chain word — A's 3 must not resurrect"
    );
    let x = assert_dual_read(&control_c, &delta_c, address(0x11), U256::from(1u64)).await;
    assert_eq!(x, U256::from(77u64), "B's settle, carried");
}

/// A missing, an unsupported-method, and a malformed list each break the chain
/// the same way on both representations.
#[tokio::test]
async fn cold_generations_match_the_control_and_cannot_resurrect_earlier_values() {
    for skip in [
        ColdSkip::Missing,
        ColdSkip::Null,
        ColdSkip::Unsupported,
        ColdSkip::Malformed,
    ] {
        cold_chain_case(skip).await;
    }
}

/// A zero-write list advances on both representations: every settled value
/// carries and no write applies.
#[tokio::test]
async fn a_zero_write_list_advances_and_keeps_every_value_on_both_representations() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, address(0x11), U256::from(1u64), 1);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_bytes(&[]));
    let control = ReadHubCache::new(&provider);
    let delta = DeltaSnapshotCache::new(&provider);
    let control_parent = control.for_anchor(ReadAnchor::identified(HEIGHT, parent_hash));
    let delta_parent = delta.for_anchor(ReadAnchor::identified(HEIGHT, parent_hash));
    assert_dual_read(
        &control_parent,
        &delta_parent,
        address(0x11),
        U256::from(1u64),
    )
    .await;
    stub.clear();

    let (control_child, control_report) = control
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 1, child_hash),
            parent_hash,
            &[],
        )
        .await;
    let (delta_child, delta_report) = delta
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 1, child_hash),
            parent_hash,
            &[],
        )
        .await;
    assert_reports_match(&control_report, &delta_report);
    assert_eq!(
        delta_report.outcome,
        CarryOutcome::Carried { carried_values: 1 }
    );
    assert!(delta_report.writes.is_empty(), "zero writes applied");
    assert_eq!(delta_child.snapshot_len(), 1);
    assert_eq!(delta_child.delta_len(), 0);
    let x = assert_dual_read(
        &control_child,
        &delta_child,
        address(0x11),
        U256::from(1u64),
    )
    .await;
    assert_eq!(
        x,
        U256::from(1u64),
        "the carried value survived a zero-write list"
    );
}

/// One refused-link case: the settled parent is installed and the advance is
/// handed `supplied_parent_hash`; both representations start the child cold,
/// agree on the report, and seed only the child's own final write.
async fn assert_refused_link(
    stub: &ChainStub,
    installed: ReadAnchor,
    child: ReadAnchor,
    supplied_parent_hash: B256,
    expected: CarryRefusal,
) {
    stub.serve_at(installed.number, address(0x11), U256::from(1u64), 1);
    let child_hash = child.hash.expect("a refused-link child carries its hash");
    stub.serve_bal_at(
        child_hash,
        bal_bytes(&[(address(0x11), U256::from(1u64), U256::from(3u64))]),
    );
    let provider = stub.provider();
    let control = ReadHubCache::new(&provider);
    let delta = DeltaSnapshotCache::new(&provider);
    let control_parent = control.for_anchor(installed);
    let delta_parent = delta.for_anchor(installed);
    assert_dual_read(
        &control_parent,
        &delta_parent,
        address(0x11),
        U256::from(1u64),
    )
    .await;
    stub.clear();

    let (control_child, control_report) = control
        .advance_bal(child, supplied_parent_hash, &[address(0x11)])
        .await;
    let (delta_child, delta_report) = delta
        .advance_bal(child, supplied_parent_hash, &[address(0x11)])
        .await;
    assert_eq!(
        delta_report.outcome,
        CarryOutcome::Cold {
            lineage: Some(expected),
            bal: None
        }
    );
    assert_reports_match(&control_report, &delta_report);
    assert_eq!(
        delta_report.writes,
        vec![(address(0x11), U256::from(1u64))],
        "the child's own final write seeds even on a cold child"
    );
    let x = assert_dual_read(
        &control_child,
        &delta_child,
        address(0x11),
        U256::from(1u64),
    )
    .await;
    assert_eq!(
        x,
        U256::from(3u64),
        "no parent value is served on a cold child"
    );
}

/// Every refused link — a wrong parent hash, a gap, a number-only parent, and
/// a same-height reorg naming the abandoned block — starts a cold child on
/// BOTH representations: the reports agree, no parent value is served there,
/// and the child's own final write still seeds.
#[tokio::test]
async fn refused_lineage_starts_cold_children_on_both_representations() {
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    let stub = ChainStub::serving();
    // The numbers line up; the supplied parent hash does not.
    assert_refused_link(
        &stub,
        ReadAnchor::identified(HEIGHT, parent_hash),
        ReadAnchor::identified(HEIGHT + 1, child_hash),
        B256::new([0xcc; 32]),
        CarryRefusal::ParentHashMismatch,
    )
    .await;
    // A gap: the child stands two blocks above the installed parent.
    assert_refused_link(
        &stub,
        ReadAnchor::identified(HEIGHT, parent_hash),
        ReadAnchor::identified(HEIGHT + 2, B256::new([0xee; 32])),
        parent_hash,
        CarryRefusal::NotConsecutive,
    )
    .await;
    // A number-only parent: its hash cannot be verified.
    assert_refused_link(
        &stub,
        ReadAnchor::at(HEIGHT),
        ReadAnchor::identified(HEIGHT + 1, B256::new([0xdd; 32])),
        parent_hash,
        CarryRefusal::ParentHashUnknown,
    )
    .await;
    // A same-height reorg: the installed anchor is the replacement block and
    // the child still names the abandoned block.
    assert_refused_link(
        &stub,
        ReadAnchor::identified(HEIGHT, B256::new([0xf1; 32])),
        ReadAnchor::identified(HEIGHT + 1, B256::new([0xf2; 32])),
        parent_hash,
        CarryRefusal::ParentHashMismatch,
    )
    .await;
}

/// The compaction threshold bounds every generation's walk — at thresholds 1,
/// 2, 3, and 64 — while values and reports match the full-clone control
/// throughout, a compaction preserves every visible value, old readers keep
/// their ancestors untouched, and the dynamic fallback survives.
async fn compaction_case(max_depth: u32) {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let control = ReadHubCache::new(&provider);
    let delta = DeltaSnapshotCache::with_max_depth(&provider, max_depth);
    let hashes: Vec<B256> = (0..6u8).map(|i| B256::new([0xa0 + i; 32])).collect();

    // Generation 0 settles x; every later generation writes one fresh slot.
    stub.serve_at(HEIGHT, address(0x11), U256::from(1u64), 1);
    let mut control_head = control.for_anchor(ReadAnchor::identified(HEIGHT, hashes[0]));
    let mut delta_head = delta.for_anchor(ReadAnchor::identified(HEIGHT, hashes[0]));
    assert_dual_read(&control_head, &delta_head, address(0x11), U256::from(1u64)).await;
    let first_reader = Arc::clone(&delta_head);
    let mut saw_compaction = false;

    for index in 1..6usize {
        let step = u64::try_from(index).expect("the step fits");
        let slot = U256::from(10 + step);
        let value = 100 + step;
        stub.serve_at(HEIGHT + step, address(0x11), slot, value);
        stub.serve_bal_at(
            hashes[index],
            bal_bytes(&[(address(0x11), slot, U256::from(value))]),
        );
        let anchor = ReadAnchor::identified(HEIGHT + step, hashes[index]);
        let (next_control, control_report) = control
            .advance_bal(anchor, hashes[index - 1], &[address(0x11)])
            .await;
        let (next_delta, delta_report) = delta
            .advance_bal(anchor, hashes[index - 1], &[address(0x11)])
            .await;
        assert_reports_match(&control_report, &delta_report);
        assert!(
            next_delta.chain_depth() <= max_depth + 1,
            "the walk stays within the threshold: {}",
            next_delta.chain_depth()
        );
        if let Some(compaction) = &delta_report.compaction {
            saw_compaction = true;
            assert_eq!(
                compaction.entries + delta_head.delta_len(),
                delta_head.snapshot_len(),
                "the compacted root holds every carried value"
            );
            assert!(compaction.nodes_merged > 1);
        }
        let x = assert_dual_read(&next_control, &next_delta, address(0x11), U256::from(1u64)).await;
        assert_eq!(
            x,
            U256::from(1u64),
            "the oldest settled value survives compaction"
        );
        let w = assert_dual_read(&next_control, &next_delta, address(0x11), slot).await;
        assert_eq!(w, U256::from(value), "each generation's own write resolves");
        control_head = next_control;
        delta_head = next_delta;
    }

    assert_eq!(
        saw_compaction,
        max_depth < 5,
        "compaction fires exactly when a link would exceed the threshold"
    );
    // An old reader's values are untouched by every later compaction.
    assert_eq!(first_reader.snapshot_len(), 1);
    let x = first_reader
        .read_storage(address(0x11), U256::from(1u64))
        .await;
    assert_eq!(x.expect("the old reader answers"), U256::from(1u64));
    // And a value no source mentions still falls back dynamically after every
    // compaction.
    stub.serve_at(HEIGHT + 5, address(0x11), U256::from(20u64), 77);
    stub.clear();
    let unknown =
        assert_dual_read(&control_head, &delta_head, address(0x11), U256::from(20u64)).await;
    assert_eq!(
        unknown,
        U256::from(77u64),
        "the dynamic fallback survives compaction"
    );
}

/// Compaction at thresholds 1, 2, 3, and 64 bounds the walk without changing a
/// single resolved value or report.
#[tokio::test]
async fn compaction_bounds_the_walk_preserves_values_and_never_mutates_ancestors() {
    for max_depth in [1u32, 2, 3, 64] {
        compaction_case(max_depth).await;
    }
}
