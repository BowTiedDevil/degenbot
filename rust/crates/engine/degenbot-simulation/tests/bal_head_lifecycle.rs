//! Behavior pins for the asynchronous canonical-head lifecycle of the BAL delta
//! snapshot (`degenbot_simulation::sim::evm2::delta_snapshot`): a head's BAL is
//! prepared off the caller's critical path, applied only to the exact-hash head
//! whose lineage it was prepared for, and a caller that arrives before the apply
//! lands reads a cold generation with the reason reported.
//!
//! Every test name states the behavior it pins. The scripted chain stub answers
//! one storage word per request and one raw list per exact block hash, so a
//! dispatched member count is exactly what took the dynamic path.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "the stub chain and its assertions fail loudly on a broken fixture"
)]

use alloy::primitives::{Address, B256, U256};
use degenbot_simulation::sim::evm2::bal_prefetch::BalSkip;
use degenbot_simulation::sim::evm2::delta_snapshot::{
    BalHeadDriver, DeltaHub, DeltaReport, DeltaSnapshotCache, HeadCold, HeadCompletion,
    HeadOutcome, HeadReadiness, HeadStats, Retention, DEFAULT_MAX_DEPTH,
};
use degenbot_simulation::sim::evm2::read_hub::{
    CarryOutcome, CarryRefusal, PreparedBal, ReadHubCache,
};
use degenbot_simulation::sim::ReadAnchor;
use std::collections::BTreeSet;
use std::future::Future;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

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
use delta_parity::{real_keys, real_parity_blocks, ParityBlock};
use modeled_chain::{bal_bytes, block_hash, ChainState, Coverage, Key, StubChain};
use real_coverage::RealCoverage;

/// The anchor height every test advances at, unless the test is about lineage.
const HEIGHT: u64 = 100;

/// An address distinct from the write fixtures' account.
fn address(tag: u8) -> Address {
    Address::new([tag; 20])
}

/// An exact-hash anchor.
fn identified(number: u64, hash: B256) -> ReadAnchor {
    ReadAnchor::identified(number, hash)
}

/// A hash no fixture links to, for the parent of a session's first head.
fn genesis() -> B256 {
    B256::new([0x99; 32])
}

/// The delta control's report and the lifecycle's report agree on everything but
/// timing: the same transition, the same writes, the same retention counts, and
/// the same compaction shape.
fn assert_reports_agree(control: &DeltaReport, lifecycle: &DeltaReport) {
    assert_eq!(
        control.outcome, lifecycle.outcome,
        "the same transition happened"
    );
    assert_eq!(control.writes, lifecycle.writes, "the same writes applied");
    assert_eq!(
        control.writes_overwritten, lifecycle.writes_overwritten,
        "the same overwrite count"
    );
    assert_eq!(
        control.writes_skipped, lifecycle.writes_skipped,
        "the same skip count"
    );
    match (&control.compaction, &lifecycle.compaction) {
        (None, None) => {}
        (Some(left), Some(right)) => {
            assert_eq!(left.nodes_merged, right.nodes_merged, "the same fold");
            assert_eq!(left.entries, right.entries, "the same retained entries");
            assert_eq!(left.dropped, right.dropped, "the same dropped entries");
        }
        (left, right) => panic!("compaction disagreed: {left:?} vs {right:?}"),
    }
}

/// The control's generation and the lifecycle's hold the same entries and answer
/// them alike — the loaded fixtures' write, the carried parent word, the relevant
/// seed, and the identity neither representation knows.
fn assert_same_snapshot(control: &DeltaHub, lifecycle: &DeltaHub) {
    assert_eq!(
        control.snapshot_len(),
        lifecycle.snapshot_len(),
        "the same live entries"
    );
    for (test_address, slot, expected) in [
        (address(0x11), 1u64, 5u64),
        (address(0x11), 2, 2),
        (address(0x22), 9, 7),
        (address(0x33), 4, 0),
    ] {
        let control_value = control.snapshot_value(test_address, U256::from(slot));
        assert_eq!(
            control_value,
            lifecycle.snapshot_value(test_address, U256::from(slot)),
            "the same answer for {test_address}/{slot}"
        );
        let expected = if expected == 0 {
            None
        } else {
            Some(U256::from(expected))
        };
        assert_eq!(control_value, expected, "the expected answer at {slot}");
    }
}

/// The settled report of one successful apply.
fn settled(outcome: &HeadOutcome) -> Arc<DeltaReport> {
    match outcome {
        HeadOutcome::Settled(report) => Arc::clone(report),
        other => panic!("expected a settled head, got {other:?}"),
    }
}

/// A prepared list applies to its exact-hash head with the transition the
/// synchronous control performs: the same lineage decision, the same writes
/// under the same relevance rule, the same carried values, and the same
/// compaction shape — and the apply itself fetches nothing.
#[tokio::test]
async fn applying_a_prepared_list_matches_the_synchronous_advance_item_for_item() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, address(0x11), U256::from(1u64), 1);
    stub.serve_at(HEIGHT, address(0x11), U256::from(2u64), 2);
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
    let relevance = [address(0x22)];

    // The control: the synchronous advance over a parent generation that settled
    // two words dynamically.
    let control = DeltaSnapshotCache::new(&provider);
    let control_parent = control.for_anchor(identified(HEIGHT, parent_hash));
    for slot in [1u64, 2] {
        control_parent
            .read_storage(address(0x11), U256::from(slot))
            .await
            .expect("the stub answers");
    }
    stub.clear();
    let (control_child, control_report) = control
        .advance_bal(identified(HEIGHT + 1, child_hash), parent_hash, &relevance)
        .await;
    assert_eq!(
        stub.bal_requests(),
        1,
        "the control fetched the child list once"
    );

    // The lifecycle: the same parent state reached through a delivered head, then
    // the child's BAL prepared ahead of the head that will use it.
    let driver = BalHeadDriver::new(&provider);
    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    let parent_hub = driver.snapshot(identified(HEIGHT, parent_hash)).hub;
    for slot in [1u64, 2] {
        parent_hub
            .read_storage(address(0x11), U256::from(slot))
            .await
            .expect("the stub answers");
    }
    stub.clear();

    let child_ticket = driver.deliver(identified(HEIGHT + 1, child_hash), parent_hash);
    let prepared = driver.prepare(child_ticket).await;
    assert!(prepared.is_decoded(), "the child list decoded");
    assert_eq!(prepared.anchor(), identified(HEIGHT + 1, child_hash));
    assert_eq!(
        stub.bal_requests(),
        1,
        "the prepared phase fetched the list once"
    );
    let lifecycle_report = settled(&driver.apply(child_ticket, &prepared, &relevance));
    assert_eq!(stub.bal_requests(), 1, "the apply fetched nothing");

    assert_reports_agree(&control_report, &lifecycle_report);
    assert_eq!(
        lifecycle_report.outcome,
        CarryOutcome::Carried { carried_values: 2 },
        "the parent chain carried"
    );
    assert_eq!(
        lifecycle_report.bal_latency,
        prepared.latency(),
        "the report attributes the prepared cost"
    );

    let lifecycle_child = driver.snapshot(identified(HEIGHT + 1, child_hash)).hub;
    assert_same_snapshot(&control_child, &lifecycle_child);
    assert_eq!(
        driver
            .snapshot(identified(HEIGHT + 1, child_hash))
            .readiness,
        HeadReadiness::Ready,
        "the settled head is ready"
    );
}

/// A delivered head serves nothing carried until its own BAL applies: callers at
/// that head read a cold generation sharing one hub, the unwritten identity is
/// answered dynamically, and only the apply makes the parent chain visible.
#[tokio::test]
async fn a_delivered_head_serves_the_cold_path_until_its_bal_applies() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, address(0x11), U256::from(2u64), 9);
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(3u64), 77);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);

    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    driver
        .snapshot(identified(HEIGHT, parent_hash))
        .hub
        .read_storage(address(0x11), U256::from(2u64))
        .await
        .expect("the stub answers");
    stub.clear();

    let child_ticket = driver.deliver(identified(HEIGHT + 1, child_hash), parent_hash);
    assert_eq!(stub.bal_requests(), 0, "a delivery fetches nothing");

    let view = driver.snapshot(identified(HEIGHT + 1, child_hash));
    assert_eq!(
        view.readiness,
        HeadReadiness::Cold(HeadCold::Unprepared),
        "an unprepared head reports the cold path"
    );
    assert_eq!(
        view.hub.snapshot_value(address(0x11), U256::from(2u64)),
        None,
        "the parent's settled word is withheld at the unprepared head"
    );
    assert_eq!(view.hub.snapshot_len(), 0, "no value is carried yet");
    assert!(
        Arc::ptr_eq(
            &view.hub,
            &driver.snapshot(identified(HEIGHT + 1, child_hash)).hub
        ),
        "every caller at the head shares one cold generation"
    );
    let fresh = view
        .hub
        .read_storage(address(0x11), U256::from(3u64))
        .await
        .expect("the stub answers");
    assert_eq!(fresh, U256::from(77u64), "the dynamic path answers");

    let prepared = driver.prepare(child_ticket).await;
    settled(&driver.apply(child_ticket, &prepared, &[]));
    let view = driver.snapshot(identified(HEIGHT + 1, child_hash));
    assert_eq!(view.readiness, HeadReadiness::Ready);
    assert_eq!(
        view.hub.snapshot_value(address(0x11), U256::from(2u64)),
        Some(U256::from(9u64)),
        "the settled head carries the parent's word"
    );
    assert_eq!(
        driver.snapshot(ReadAnchor::at(HEIGHT + 5)).readiness,
        HeadReadiness::Cold(HeadCold::Undelivered),
        "a head never delivered reports itself"
    );
}

/// A prepared list applies only to the exact header it was prepared for: a list
/// fetched at another hash is discarded, installs nothing, and leaves the head
/// free to settle with its own list.
#[tokio::test]
async fn a_prepared_list_applies_only_to_the_head_it_was_prepared_for() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, address(0x11), U256::from(1u64), 1);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    let other_hash = B256::new([0xcc; 32]);
    stub.serve_bal_at(
        other_hash,
        bal_bytes(&[(address(0x11), U256::from(1u64), U256::from(7u64))]),
    );
    stub.serve_bal_at(child_hash, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);

    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    driver
        .snapshot(identified(HEIGHT, parent_hash))
        .hub
        .read_storage(address(0x11), U256::from(1u64))
        .await
        .expect("the stub answers");

    let child_ticket = driver.deliver(identified(HEIGHT + 1, child_hash), parent_hash);
    let foreign = PreparedBal::prepare(&provider, identified(HEIGHT + 1, other_hash)).await;
    assert_eq!(
        driver.apply(child_ticket, &foreign, &[]),
        HeadOutcome::Foreign {
            prepared: identified(HEIGHT + 1, other_hash),
            head: identified(HEIGHT + 1, child_hash),
        },
        "the foreign list is refused"
    );
    assert_eq!(
        driver.installed(),
        Some(identified(HEIGHT, parent_hash)),
        "nothing was installed"
    );
    assert_eq!(
        driver
            .snapshot(identified(HEIGHT + 1, child_hash))
            .readiness,
        HeadReadiness::Cold(HeadCold::Unprepared),
        "the head is still unprepared"
    );
    assert_eq!(driver.stats().discarded, 1, "the refusal is counted");

    let own = driver.prepare(child_ticket).await;
    let report = settled(&driver.apply(child_ticket, &own, &[]));
    assert_eq!(
        report.writes,
        Vec::<(Address, U256)>::new(),
        "the head's own empty list applied"
    );
    assert_eq!(
        driver.installed(),
        Some(identified(HEIGHT + 1, child_hash)),
        "the head settled with its own list"
    );
}

/// A preparation that finishes after a later head arrived installs nothing: the
/// abandoned head stays dead, its values never enter the new branch, and the
/// later head settles from the parent the abandoned one never replaced.
#[tokio::test]
async fn a_late_preparation_for_a_superseded_head_installs_nothing() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, address(0x11), U256::from(1u64), 1);
    let parent_hash = B256::new([0xaa; 32]);
    let abandoned = B256::new([0xa1; 32]);
    let replacement = B256::new([0xa2; 32]);
    // The abandoned branch would have written a value the replacement never
    // mentions: if the late list installed, that value would be served there.
    stub.serve_bal_at(
        abandoned,
        bal_bytes(&[(address(0x33), U256::from(4u64), U256::from(666u64))]),
    );
    stub.serve_bal_at(replacement, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);

    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    driver
        .snapshot(identified(HEIGHT, parent_hash))
        .hub
        .read_storage(address(0x11), U256::from(1u64))
        .await
        .expect("the stub answers");

    let abandoned_ticket = driver.deliver(identified(HEIGHT + 1, abandoned), parent_hash);
    let late = driver.prepare(abandoned_ticket).await;
    assert!(late.is_decoded(), "the abandoned list decoded");

    // The same-height reorg arrives while the abandoned list is in flight.
    let replacement_ticket = driver.deliver(identified(HEIGHT + 1, replacement), parent_hash);
    assert_eq!(
        driver.apply(abandoned_ticket, &late, &[]),
        HeadOutcome::Superseded {
            head: identified(HEIGHT + 1, replacement),
        },
        "the late response cannot install into the new head"
    );
    assert_eq!(
        driver.installed(),
        Some(identified(HEIGHT, parent_hash)),
        "the abandoned head installed nothing"
    );

    let report = settled(&driver.advance(replacement_ticket, &[]).await);
    assert_eq!(
        report.outcome,
        CarryOutcome::Carried { carried_values: 1 },
        "the replacement head carries the installed parent"
    );
    let view = driver.snapshot(identified(HEIGHT + 1, replacement));
    assert_eq!(view.readiness, HeadReadiness::Ready);
    assert_eq!(
        view.hub.snapshot_value(address(0x33), U256::from(4u64)),
        None,
        "the abandoned branch's write never reached the replacement"
    );
    assert_eq!(
        driver.snapshot(identified(HEIGHT + 1, abandoned)).readiness,
        HeadReadiness::Cold(HeadCold::Superseded),
        "the abandoned head reports that it was superseded"
    );
    assert_eq!(driver.stats().superseded, 1);
}

/// A same-height reorg settles cold, and the head built on it re-establishes the
/// chain: the new branch carries only its own generation, never the abandoned
/// branch's settled values.
#[tokio::test]
async fn a_same_height_reorg_settles_cold_and_the_next_head_carries_only_the_new_branch() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(1u64), 11);
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(2u64), 12);
    stub.serve_at(HEIGHT + 2, address(0x11), U256::from(1u64), 22);
    stub.serve_at(HEIGHT + 2, address(0x11), U256::from(2u64), 13);
    let parent_hash = B256::new([0xaa; 32]);
    let first = B256::new([0xb1; 32]);
    let reorg = B256::new([0xc1; 32]);
    let successor = B256::new([0xc2; 32]);
    stub.serve_bal_at(first, bal_bytes(&[]));
    stub.serve_bal_at(reorg, bal_bytes(&[]));
    stub.serve_bal_at(successor, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);

    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    let first_ticket = driver.deliver(identified(HEIGHT + 1, first), parent_hash);
    settled(&driver.advance(first_ticket, &[]).await);
    // The abandoned branch settles a value at its own anchor.
    let first_view = driver.snapshot(identified(HEIGHT + 1, first)).hub;
    let abandoned = first_view
        .read_storage(address(0x11), U256::from(2u64))
        .await
        .expect("the stub answers");
    assert_eq!(abandoned, U256::from(12u64));

    // The reorg replaces the block at the same height: its parent is the
    // installed block's own parent, so the installed anchor is not its parent.
    let reorg_ticket = driver.deliver(identified(HEIGHT + 1, reorg), parent_hash);
    let report = settled(&driver.advance(reorg_ticket, &[]).await);
    assert_eq!(
        report.outcome,
        CarryOutcome::Cold {
            lineage: Some(CarryRefusal::NotConsecutive),
            bal: None,
        },
        "a same-height reorg never links the head it replaced"
    );
    assert_eq!(
        driver.snapshot(identified(HEIGHT + 1, reorg)).readiness,
        HeadReadiness::Cold(HeadCold::Settled {
            lineage: Some(CarryRefusal::NotConsecutive),
            bal: None,
        }),
        "the settled-cold head reports why it carries nothing"
    );

    // The next block links the reorged head and carries only ITS generation.
    let successor_ticket = driver.deliver(identified(HEIGHT + 2, successor), reorg);
    let report = settled(&driver.advance(successor_ticket, &[]).await);
    assert_eq!(
        report.outcome,
        CarryOutcome::Carried { carried_values: 0 },
        "the new branch's second block re-establishes the chain"
    );
    let view = driver.snapshot(identified(HEIGHT + 2, successor));
    assert_eq!(
        view.hub.snapshot_value(address(0x11), U256::from(2u64)),
        None,
        "the abandoned branch's settle never resurrects"
    );
    let value = view
        .hub
        .read_storage(address(0x11), U256::from(2u64))
        .await
        .expect("the stub answers");
    assert_eq!(
        value,
        U256::from(13u64),
        "a read takes the new branch's own word, not the abandoned value"
    );
}

/// A gap cannot carry, and the next consecutive head re-establishes the chain.
#[tokio::test]
async fn a_gap_settles_cold_and_the_next_consecutive_head_re_establishes_the_chain() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, address(0x11), U256::from(1u64), 1);
    let parent_hash = B256::new([0xaa; 32]);
    let skipped = B256::new([0xb1; 32]);
    let next = B256::new([0xb2; 32]);
    let after = B256::new([0xb3; 32]);
    stub.serve_bal_at(skipped, bal_bytes(&[]));
    stub.serve_bal_at(next, bal_bytes(&[]));
    stub.serve_bal_at(after, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);

    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    driver
        .snapshot(identified(HEIGHT, parent_hash))
        .hub
        .read_storage(address(0x11), U256::from(1u64))
        .await
        .expect("the stub answers");

    // Two blocks above the installed generation: a gap.
    let skipped_ticket = driver.deliver(identified(HEIGHT + 2, skipped), B256::new([0xfe; 32]));
    let report = settled(&driver.advance(skipped_ticket, &[]).await);
    assert_eq!(
        report.outcome,
        CarryOutcome::Cold {
            lineage: Some(CarryRefusal::NotConsecutive),
            bal: None,
        }
    );

    // The gap head is cold, and it settles a word of its own.
    stub.serve_at(HEIGHT + 2, address(0x11), U256::from(2u64), 2);
    driver
        .snapshot(identified(HEIGHT + 2, skipped))
        .hub
        .read_storage(address(0x11), U256::from(2u64))
        .await
        .expect("the stub answers");

    // The head above the gap is consecutive to what was installed, so it carries
    // the gap head's own generation — and never the pre-gap head's value.
    let next_ticket = driver.deliver(identified(HEIGHT + 3, next), skipped);
    let report = settled(&driver.advance(next_ticket, &[]).await);
    assert_eq!(
        report.outcome,
        CarryOutcome::Carried { carried_values: 1 },
        "the head above the gap re-establishes the chain"
    );
    let view = driver.snapshot(identified(HEIGHT + 3, next));
    assert_eq!(
        view.hub.snapshot_value(address(0x11), U256::from(2u64)),
        Some(U256::from(2u64)),
        "the gap head's own settle carried forward"
    );
    assert_eq!(
        view.hub.snapshot_value(address(0x11), U256::from(1u64)),
        None,
        "the pre-gap head's value never reappears"
    );
    let after_ticket = driver.deliver(identified(HEIGHT + 4, after), next);
    let report = settled(&driver.advance(after_ticket, &[]).await);
    assert_eq!(
        report.outcome,
        CarryOutcome::Carried { carried_values: 1 },
        "the chain continues from the re-established head"
    );
}

/// A number-only head prepares no list at all: it settles cold with the skip
/// reported, and a read there takes the dynamic path.
#[tokio::test]
async fn a_number_only_head_prepares_no_list_and_settles_cold() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(1u64), 5);
    let parent_hash = B256::new([0xaa; 32]);
    let driver = BalHeadDriver::new(&provider);

    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    stub.clear();

    let ticket = driver.deliver(ReadAnchor::at(HEIGHT + 1), parent_hash);
    let prepared = driver.prepare(ticket).await;
    assert_eq!(prepared.skip(), Some(&BalSkip::NoAnchorHash));
    assert_eq!(prepared.hash(), None);
    assert_eq!(stub.bal_requests(), 0, "no list can be requested");
    let report = settled(&driver.apply(ticket, &prepared, &[]));
    assert_eq!(
        report.outcome,
        CarryOutcome::Cold {
            lineage: None,
            bal: Some(BalSkip::NoAnchorHash),
        }
    );
    let view = driver.snapshot(ReadAnchor::at(HEIGHT + 1));
    assert_eq!(
        view.readiness,
        HeadReadiness::Cold(HeadCold::Settled {
            lineage: None,
            bal: Some(BalSkip::NoAnchorHash),
        })
    );
    assert_eq!(view.hub.snapshot_len(), 0, "a cold head carries nothing");
    let value = view
        .hub
        .read_storage(address(0x11), U256::from(1u64))
        .await
        .expect("the stub answers");
    assert_eq!(value, U256::from(5u64), "the dynamic path answers");
}

/// An unreadable list — missing, unsupported, or malformed — settles the head
/// cold with the skip reported and no parent value served.
#[tokio::test]
async fn an_unreadable_list_settles_a_cold_head_with_the_skip_reported() {
    for skip in [
        ColdSkip::Missing,
        ColdSkip::Null,
        ColdSkip::Unsupported,
        ColdSkip::Malformed,
    ] {
        cold_list_case(skip).await;
    }
}

/// A list that cannot be read.
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

/// One unreadable-list case: the head settles cold, reports the skip, and serves
/// the dynamic word rather than a parent value.
async fn cold_list_case(skip: ColdSkip) {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, address(0x11), U256::from(1u64), 1);
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(1u64), 77);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    match skip {
        ColdSkip::Missing => {}
        ColdSkip::Null => stub.set_bal_null(true),
        ColdSkip::Unsupported => stub.set_bal_unsupported(true),
        ColdSkip::Malformed => stub.serve_bal_at(child_hash, vec![0xff, 0x00]),
    }
    let driver = BalHeadDriver::new(&provider);
    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    driver
        .snapshot(identified(HEIGHT, parent_hash))
        .hub
        .read_storage(address(0x11), U256::from(1u64))
        .await
        .expect("the stub answers");

    let ticket = driver.deliver(identified(HEIGHT + 1, child_hash), parent_hash);
    let prepared = driver.prepare(ticket).await;
    assert!(!prepared.is_decoded(), "the list did not decode");
    let report = settled(&driver.apply(ticket, &prepared, &[]));
    stub.set_bal_null(false);
    stub.set_bal_unsupported(false);
    let CarryOutcome::Cold {
        lineage: None,
        bal: Some(_),
    } = &report.outcome
    else {
        panic!("an unreadable list settles cold: {:?}", report.outcome);
    };
    let view = driver.snapshot(identified(HEIGHT + 1, child_hash));
    assert!(
        matches!(
            &view.readiness,
            HeadReadiness::Cold(HeadCold::Settled { bal: Some(_), .. })
        ),
        "the settled head reports the skip: {:?}",
        view.readiness
    );
    assert_eq!(view.hub.snapshot_len(), 0, "a cold head carries nothing");
    let value = view
        .hub
        .read_storage(address(0x11), U256::from(1u64))
        .await
        .expect("the stub answers");
    assert_eq!(
        value,
        U256::from(77u64),
        "the head's own word, never the parent's 1"
    );
}

/// Idempotent delivery and idempotent advance repeat no BAL work: a repeated
/// delivery hands back the same ticket, and a settled head never fetches again.
#[tokio::test]
async fn an_idempotent_delivery_and_advance_repeat_no_bal_work() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);

    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    stub.clear();
    let ticket = driver.deliver(identified(HEIGHT + 1, child_hash), parent_hash);
    assert_eq!(
        driver.deliver(identified(HEIGHT + 1, child_hash), parent_hash),
        ticket,
        "the repeated delivery hands back the same ticket"
    );
    assert_eq!(driver.stats().repeated, 1);
    assert_eq!(driver.stats().delivered, 2, "one per real head");
    assert_eq!(stub.bal_requests(), 0, "no delivery fetched anything");

    settled(&driver.advance(ticket, &[]).await);
    assert_eq!(stub.bal_requests(), 1, "the head fetched once");
    assert_eq!(
        driver.advance(ticket, &[]).await,
        HeadOutcome::AlreadySettled,
        "the settled head is idempotent"
    );
    assert_eq!(
        driver.stats().repeated,
        1,
        "an advance is not a delivery, so it is not counted as a repeat"
    );
    assert_eq!(
        stub.bal_requests(),
        1,
        "the idempotent advance repeated no BAL work"
    );
}

/// A settled head keeps answering after the next head arrives: a caller still
/// working at it gets its generation (and its readiness), not a fresh cold one,
/// while the new head stays cold until its own list applies.
#[tokio::test]
async fn a_settled_head_keeps_answering_after_the_next_head_arrives() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, address(0x11), U256::from(1u64), 7);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    let next_hash = B256::new([0xcc; 32]);
    stub.serve_bal_at(child_hash, bal_bytes(&[]));
    stub.serve_bal_at(next_hash, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);

    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    driver
        .snapshot(identified(HEIGHT, parent_hash))
        .hub
        .read_storage(address(0x11), U256::from(1u64))
        .await
        .expect("the stub answers");
    let child_ticket = driver.deliver(identified(HEIGHT + 1, child_hash), parent_hash);
    settled(&driver.advance(child_ticket, &[]).await);
    stub.clear();

    // The next head arrives and is unprepared: the child keeps answering.
    driver.deliver(identified(HEIGHT + 2, next_hash), child_hash);
    let settled_view = driver.snapshot(identified(HEIGHT + 1, child_hash));
    assert_eq!(
        settled_view.readiness,
        HeadReadiness::Ready,
        "the settled head is still ready"
    );
    assert_eq!(
        settled_view
            .hub
            .read_storage(address(0x11), U256::from(1u64))
            .await
            .expect("the stub answers"),
        U256::from(7u64),
        "the carried word still resolves from the settled generation"
    );
    assert_eq!(
        stub.storage_members(),
        0,
        "a carried read costs no fetch after the head moved"
    );
    assert_eq!(
        driver.snapshot(identified(HEIGHT + 2, next_hash)).readiness,
        HeadReadiness::Cold(HeadCold::Unprepared),
        "the new head is cold until its own list applies"
    );
}

/// A delivery of a head the cache already has installed settles on the spot: a
/// re-delivered installed block repeats no BAL work and answers ready.
#[tokio::test]
async fn a_delivery_of_the_installed_head_settles_without_bal_work() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    let other_hash = B256::new([0xcc; 32]);
    stub.serve_bal_at(child_hash, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);

    // A linked pair: the child carries the parent's generation, so the child is
    // the installed head whose own settle made it ready.
    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    let child_ticket = driver.deliver(identified(HEIGHT + 1, child_hash), parent_hash);
    settled(&driver.advance(child_ticket, &[]).await);
    stub.clear();

    // A head above the installed one is delivered and abandoned before it
    // settles: the installed block is the head the stream returns to.
    let _abandoned = driver.deliver(identified(HEIGHT + 2, other_hash), child_hash);
    let back = driver.deliver(identified(HEIGHT + 1, child_hash), parent_hash);
    assert_eq!(
        driver.stats().already_current,
        1,
        "the installed head settles on delivery"
    );
    assert_eq!(
        driver
            .snapshot(identified(HEIGHT + 1, child_hash))
            .readiness,
        HeadReadiness::Ready,
        "a ready generation keeps answering ready"
    );
    assert_eq!(
        driver.advance(back, &[]).await,
        HeadOutcome::AlreadySettled,
        "nothing to prepare for an installed head"
    );
    assert_eq!(stub.bal_requests(), 0, "no BAL work at all");
}

/// A redelivered installed head keeps the readiness its own settle derived: a
/// generation that settled COLD still reports its cold reason after the stream
/// delivers that same installed anchor again, because a redelivery repeats no
/// transition and therefore cannot substitute `Ready` for the one that happened.
#[tokio::test]
async fn a_redelivered_installed_cold_head_keeps_its_settle_derived_readiness() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let parent_hash = B256::new([0xaa; 32]);
    let cold_hash = B256::new([0xbb; 32]);
    let later_hash = B256::new([0xcc; 32]);
    stub.serve_bal_at(cold_hash, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);

    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;

    // Two blocks above the installed generation: the lineage cannot carry, so
    // the head installs cold with that refusal as its readiness.
    let cold_anchor = identified(HEIGHT + 2, cold_hash);
    let cold_ticket = driver.deliver(cold_anchor, B256::new([0xfe; 32]));
    let report = settled(&driver.advance(cold_ticket, &[]).await);
    assert_eq!(
        report.outcome,
        CarryOutcome::Cold {
            lineage: Some(CarryRefusal::NotConsecutive),
            bal: None,
        }
    );
    let cold_readiness = HeadReadiness::Cold(HeadCold::Settled {
        lineage: Some(CarryRefusal::NotConsecutive),
        bal: None,
    });
    let settled_view = driver.snapshot(cold_anchor);
    assert_eq!(settled_view.readiness, cold_readiness);
    assert_eq!(
        settled_view.hub.snapshot_len(),
        0,
        "a settled-cold head carries nothing"
    );

    // The stream moves on and then returns to the installed cold block: the
    // redelivery names the same anchor with another parent hash, so it is a
    // real delivery — not the idempotent repeat — that answers the installed
    // generation on the spot.
    let _abandoned = driver.deliver(identified(HEIGHT + 3, later_hash), cold_hash);
    stub.clear();
    let back = driver.deliver(cold_anchor, genesis());
    assert_eq!(driver.stats().repeated, 0, "the redelivery is not a repeat");
    assert_eq!(
        driver.stats().already_current,
        1,
        "the redelivery answers the installed generation on the spot"
    );
    assert_eq!(driver.stats().delivered, 4, "a real delivery was counted");
    assert_eq!(stub.bal_requests(), 0, "the redelivery fetched nothing");

    let before = driver.stats();
    let view = driver.snapshot(cold_anchor);
    assert_eq!(
        view.readiness, cold_readiness,
        "the redelivery keeps the settle-derived cold reason, never `Ready`"
    );
    assert!(
        Arc::ptr_eq(&view.hub, &settled_view.hub),
        "the redelivery answers the same installed cold generation"
    );
    assert_eq!(view.hub.snapshot_len(), 0, "still carrying nothing");
    assert_eq!(
        driver.installed(),
        Some(cold_anchor),
        "no other generation was installed"
    );
    assert_eq!(
        driver.stats().cold_views,
        before.cold_views + 1,
        "the view at the redelivered cold anchor is a cold view"
    );
    assert_eq!(
        driver.stats().ready_views,
        before.ready_views,
        "no view at the cold anchor became ready"
    );
    assert_eq!(
        driver.advance(back, &[]).await,
        HeadOutcome::AlreadySettled,
        "nothing to prepare for an installed head"
    );
}

/// A cancelled preparation installs nothing and releases the claim: the head is
/// still unprepared afterwards, and a later advance prepares and settles it.
#[tokio::test]
async fn a_cancelled_preparation_releases_the_claim_and_the_head_retries() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);

    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    let ticket = driver.deliver(identified(HEIGHT + 1, child_hash), parent_hash);
    stub.clear();
    stub.set_slow(true);

    let mut cancelled = Box::pin(driver.advance(ticket, &[]));
    assert!(
        cancelled
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending(),
        "the held round keeps the advance pending"
    );
    drop(cancelled);
    assert_eq!(
        driver.installed(),
        Some(identified(HEIGHT, parent_hash)),
        "the cancelled preparation installed nothing"
    );
    assert_eq!(
        driver
            .snapshot(identified(HEIGHT + 1, child_hash))
            .readiness,
        HeadReadiness::Cold(HeadCold::Unprepared),
        "the head is still unprepared"
    );

    stub.set_slow(false);
    let report = settled(&driver.advance(ticket, &[]).await);
    assert_eq!(
        report.outcome,
        CarryOutcome::Carried { carried_values: 0 },
        "the retry settled the head against the installed parent"
    );
    assert_eq!(stub.bal_requests(), 1, "the retry fetched the list once");
}

/// Two concurrent advances for one head share a single fetch: the second caller
/// is told the preparation is already owned instead of duplicating the request.
#[tokio::test]
async fn a_concurrent_advance_joins_the_claimed_preparation_without_a_second_fetch() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);

    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    let ticket = driver.deliver(identified(HEIGHT + 1, child_hash), parent_hash);
    stub.clear();
    stub.set_slow(true);

    let (first, second) = tokio::join!(driver.advance(ticket, &[]), driver.advance(ticket, &[]));
    stub.set_slow(false);
    assert!(
        matches!(first, HeadOutcome::Settled(_)),
        "the claiming call settles: {first:?}"
    );
    assert_eq!(
        second,
        HeadOutcome::InFlight,
        "the second call neither waits nor fetches"
    );
    assert_eq!(stub.bal_requests(), 1, "one fetch for one head");
    assert_eq!(driver.stats().settled, 2, "the parent and the child");
}

/// Concurrent reads at an unprepared head share one cold generation, so the
/// hub's single flight still holds: one slot, two readers, one dispatched member.
#[tokio::test]
async fn concurrent_reads_at_an_unprepared_head_share_one_cold_generation() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(1u64), 3);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    let driver = BalHeadDriver::new(&provider);
    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    driver.deliver(identified(HEIGHT + 1, child_hash), parent_hash);
    stub.clear();

    let left = driver.snapshot(identified(HEIGHT + 1, child_hash)).hub;
    let right = driver.snapshot(identified(HEIGHT + 1, child_hash)).hub;
    assert!(Arc::ptr_eq(&left, &right), "one generation per head");
    let first = left.read_storage_handle(address(0x11), U256::from(1u64));
    let second = right.read_storage_handle(address(0x11), U256::from(1u64));
    assert_eq!(
        right.read_stats().coalesced,
        1,
        "the duplicate joined the queued identity"
    );
    left.drain_round().await;
    assert_eq!(first.await.expect("the stub answers"), U256::from(3u64));
    assert_eq!(second.await.expect("the stub answers"), U256::from(3u64));
    assert_eq!(stub.storage_members(), 1, "one member for the pair");
}

/// A value a caller settles into the unprepared head's cold generation stays
/// handle-local: the settled generation that replaces it never sees it, and the
/// held handle keeps its own value.
#[tokio::test]
async fn a_settle_into_an_unprepared_head_stays_handle_local() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT + 1, address(0x11), U256::from(1u64), 4);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);
    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;

    let ticket = driver.deliver(identified(HEIGHT + 1, child_hash), parent_hash);
    let held = driver.snapshot(identified(HEIGHT + 1, child_hash)).hub;
    let late_settle = held
        .read_storage(address(0x11), U256::from(1u64))
        .await
        .expect("the stub answers");
    assert_eq!(late_settle, U256::from(4u64));

    let prepared = driver.prepare(ticket).await;
    settled(&driver.apply(ticket, &prepared, &[]));
    let settled_hub = driver.snapshot(identified(HEIGHT + 1, child_hash)).hub;
    assert!(
        !Arc::ptr_eq(&held, &settled_hub),
        "the settled hub replaces it"
    );
    assert_eq!(
        settled_hub.snapshot_value(address(0x11), U256::from(1u64)),
        None,
        "the provisional settle never crosses into the settled generation"
    );
    assert_eq!(
        held.snapshot_value(address(0x11), U256::from(1u64)),
        Some(U256::from(4u64)),
        "the held generation keeps its own settle"
    );
}

/// The lifecycle compacts on the caller's threshold exactly as the synchronous
/// control does: every head's report carries the same fold, the walk stays inside
/// the bound, and the oldest settled value survives every compaction.
#[tokio::test]
async fn the_lifecycle_compacts_on_the_control_threshold() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let max_depth = 1u32;
    let control = DeltaSnapshotCache::with_max_depth(&provider, max_depth);
    let driver = BalHeadDriver::with_config(&provider, max_depth, Retention::Unbounded);
    let hashes: Vec<B256> = (0..5u8)
        .map(|index| B256::new([0xa0 + index; 32]))
        .collect();
    let first_slot = U256::from(1u64);
    stub.serve_at(HEIGHT, address(0x11), first_slot, 1);
    stub.serve_bal_at(hashes[0], bal_bytes(&[]));

    let base = identified(HEIGHT, hashes[0]);
    control
        .for_anchor(base)
        .read_storage(address(0x11), first_slot)
        .await
        .expect("the stub answers");
    let base_ticket = driver.deliver(base, genesis());
    driver.advance(base_ticket, &[]).await;
    driver
        .snapshot(base)
        .hub
        .read_storage(address(0x11), first_slot)
        .await
        .expect("the stub answers");

    let mut saw_compaction = false;
    for step in 1..5usize {
        let number = HEIGHT + u64::try_from(step).expect("the step fits");
        let slot = U256::from(10 + u64::try_from(step).expect("the slot fits"));
        let value = 100 + u64::try_from(step).expect("the value fits");
        stub.serve_at(number, address(0x11), slot, value);
        stub.serve_bal_at(
            hashes[step],
            bal_bytes(&[(address(0x11), slot, U256::from(value))]),
        );
        let anchor = identified(number, hashes[step]);
        let parent = hashes[step - 1];
        let (control_hub, control_report) =
            control.advance_bal(anchor, parent, &[address(0x11)]).await;
        let ticket = driver.deliver(anchor, parent);
        let lifecycle_report = settled(&driver.advance(ticket, &[address(0x11)]).await);
        assert_reports_agree(&control_report, &lifecycle_report);
        let view = driver.snapshot(anchor);
        assert!(
            view.hub.chain_depth() <= max_depth + 1,
            "the walk stays inside the threshold: {}",
            view.hub.chain_depth()
        );
        if let Some(fold) = &lifecycle_report.compaction {
            saw_compaction = true;
            assert_eq!(
                fold.nodes_merged,
                control_report
                    .compaction
                    .as_ref()
                    .map_or(0, |c| c.nodes_merged)
            );
            assert!(fold.nodes_merged > 1);
        }
        assert_eq!(
            control_hub.snapshot_value(address(0x11), first_slot),
            view.hub.snapshot_value(address(0x11), first_slot),
            "the oldest settled value survives the fold on both"
        );
    }
    assert!(saw_compaction, "a threshold of one must fold every link");
    assert_eq!(
        driver
            .snapshot(identified(HEIGHT + 4, hashes[4]))
            .hub
            .snapshot_value(address(0x11), first_slot),
        Some(U256::from(1u64))
    );
}

/// The lifecycle applies the caller's retention policy exactly as the
/// synchronous control does: an entry the policy has expired at a later head is
/// unknown again on both representations, and re-reading it settles a fresh
/// window through the same hub round.
#[tokio::test]
async fn the_lifecycle_applies_the_callers_retention_policy() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, address(0x11), U256::from(1u64), 1);
    stub.serve_at(HEIGHT + 2, address(0x11), U256::from(1u64), 9);
    let base = B256::new([0xaa; 32]);
    let one = B256::new([0xb1; 32]);
    let two = B256::new([0xb2; 32]);
    for hash in [one, two] {
        stub.serve_bal_at(hash, bal_bytes(&[]));
    }
    let retention = Retention::IdleBlocks(1);
    let control = DeltaSnapshotCache::with_retention(&provider, retention);
    let driver = BalHeadDriver::with_config(&provider, DEFAULT_MAX_DEPTH, retention);

    let control_base = control.for_anchor(identified(HEIGHT, base));
    assert_eq!(
        control_base
            .read_storage(address(0x11), U256::from(1u64))
            .await
            .expect("the stub answers"),
        U256::from(1u64)
    );
    let base_ticket = driver.deliver(identified(HEIGHT, base), genesis());
    driver.advance(base_ticket, &[]).await;
    assert_eq!(
        driver
            .snapshot(identified(HEIGHT, base))
            .hub
            .read_storage(address(0x11), U256::from(1u64))
            .await
            .expect("the stub answers"),
        U256::from(1u64)
    );

    // One block on, the entry is still inside its idle window on both.
    let control_one = control
        .advance_bal(identified(HEIGHT + 1, one), base, &[])
        .await
        .0;
    let one_ticket = driver.deliver(identified(HEIGHT + 1, one), base);
    settled(&driver.advance(one_ticket, &[]).await);
    let lifecycle_one = driver.snapshot(identified(HEIGHT + 1, one)).hub;
    assert_eq!(
        control_one.snapshot_value(address(0x11), U256::from(1u64)),
        Some(U256::from(1u64))
    );
    assert_eq!(
        lifecycle_one.snapshot_value(address(0x11), U256::from(1u64)),
        Some(U256::from(1u64)),
        "the idle window still serves the carried word"
    );

    // Two blocks on, the same entry has expired on both, and a read takes the
    // dynamic path and re-settles.
    let control_two = control
        .advance_bal(identified(HEIGHT + 2, two), one, &[])
        .await
        .0;
    let two_ticket = driver.deliver(identified(HEIGHT + 2, two), one);
    settled(&driver.advance(two_ticket, &[]).await);
    let lifecycle_two = driver.snapshot(identified(HEIGHT + 2, two)).hub;
    assert_eq!(
        control_two.snapshot_value(address(0x11), U256::from(1u64)),
        None,
        "the control expired the entry"
    );
    assert_eq!(
        lifecycle_two.snapshot_value(address(0x11), U256::from(1u64)),
        None,
        "the lifecycle expired it on the same schedule"
    );
    stub.clear();
    assert_eq!(
        control_two
            .read_storage(address(0x11), U256::from(1u64))
            .await
            .expect("the stub answers"),
        U256::from(9u64)
    );
    assert_eq!(
        lifecycle_two
            .read_storage(address(0x11), U256::from(1u64))
            .await
            .expect("the stub answers"),
        U256::from(9u64)
    );
    assert_eq!(
        stub.storage_members(),
        2,
        "each representation paid exactly one dynamic read"
    );
    assert_eq!(
        lifecycle_two.snapshot_value(address(0x11), U256::from(1u64)),
        Some(U256::from(9u64)),
        "the re-read settled a fresh window"
    );
}

/// Every generation of a modeled chain settles identically through the lifecycle
/// and the synchronous control — reports, entry counts, and every resolved value
/// — while each head is cold until its own list applies.
#[tokio::test]
async fn every_generation_of_a_modeled_chain_settles_identically_through_the_lifecycle() {
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
    let blocks: Vec<ParityBlock> = (first..=last)
        .map(|number| ParityBlock {
            anchor: identified(number, block_hash(number)),
            parent_hash: block_hash(number - 1),
            relevance: if number % 2 == 0 {
                vec![address(0x11), address(0x22)]
            } else {
                vec![address(0x33)]
            },
            reads: reads.clone(),
        })
        .collect();
    let result = lifecycle_replay(&provider, &blocks).await;
    assert!(result.is_empty(), "{result:#?}");
}

/// The lifecycle replay: at every block, deliver the head, prove it is cold, then
/// settle it and compare everything against the synchronous delta control.
async fn lifecycle_replay(
    provider: &degenbot_rpc::provider::AlloyProvider,
    blocks: &[ParityBlock],
) -> Vec<String> {
    let control = DeltaSnapshotCache::new(provider);
    let driver = BalHeadDriver::new(provider);
    let mut mismatches = Vec::new();
    for block in blocks {
        let ticket = driver.deliver(block.anchor, block.parent_hash);
        let cold = driver.snapshot(block.anchor);
        let is_cold = matches!(cold.readiness, HeadReadiness::Cold(_));
        if !is_cold || cold.hub.snapshot_len() != 0 {
            mismatches.push(format!(
                "block {}: a delivered head served {:?} with {} entries",
                block.anchor.number,
                cold.readiness,
                cold.hub.snapshot_len()
            ));
        }
        let lifecycle = driver.advance(ticket, &block.relevance).await;
        let HeadOutcome::Settled(lifecycle_report) = lifecycle else {
            mismatches.push(format!(
                "block {}: the advance did not settle: {lifecycle:?}",
                block.anchor.number
            ));
            continue;
        };
        let (control_hub, control_report) = control
            .advance_bal(block.anchor, block.parent_hash, &block.relevance)
            .await;
        if control_report.outcome != lifecycle_report.outcome
            || control_report.writes != lifecycle_report.writes
            || control_report.writes_overwritten != lifecycle_report.writes_overwritten
            || control_report.writes_skipped != lifecycle_report.writes_skipped
        {
            mismatches.push(format!(
                "block {}: control {control_report:?} vs lifecycle {lifecycle_report:?}",
                block.anchor.number
            ));
        }
        let view = driver.snapshot(block.anchor);
        if control_hub.snapshot_len() != view.hub.snapshot_len() {
            mismatches.push(format!(
                "block {}: control {} entries vs lifecycle {}",
                block.anchor.number,
                control_hub.snapshot_len(),
                view.hub.snapshot_len()
            ));
        }
        for &(address, slot) in &block.reads {
            let control_value = control_hub.read_storage(address, slot).await;
            let lifecycle_value = view.hub.read_storage(address, slot).await;
            let same = match (control_value, lifecycle_value) {
                (Ok(left), Ok(right)) => left == right,
                _ => false,
            };
            if !same {
                mismatches.push(format!(
                    "block {} {address}/{slot}: control vs lifecycle disagreed",
                    block.anchor.number
                ));
            }
        }
    }
    mismatches
}

/// The committed real write-coverage capture settles every head through the
/// lifecycle with zero divergence from the synchronous control, at every
/// captured block and every captured identity.
#[tokio::test]
async fn the_real_coverage_capture_settles_every_head_with_zero_divergence() {
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
    let mismatches = lifecycle_replay(&provider, &blocks).await;
    assert!(
        mismatches.is_empty(),
        "{} disagreements over {} blocks: {mismatches:#?}",
        mismatches.len(),
        blocks.len()
    );
}

/// The lifecycle's control comparison is the control's own report shapes: the
/// full-clone cache reports the same transition the delta control does, so the
/// parity claim above is not an artifact of comparing one representation to
/// itself.
#[tokio::test]
async fn the_full_clone_control_agrees_with_the_lifecycle_transition() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, address(0x11), U256::from(1u64), 1);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(
        child_hash,
        bal_bytes(&[(address(0x11), U256::from(1u64), U256::from(5u64))]),
    );
    let control = ReadHubCache::new(&provider);
    let _ = control.for_anchor(identified(HEIGHT, parent_hash));
    let driver = BalHeadDriver::new(&provider);
    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;

    let ticket = driver.deliver(identified(HEIGHT + 1, child_hash), parent_hash);
    let prepared = driver.prepare(ticket).await;
    let report = settled(&driver.apply(ticket, &prepared, &[address(0x11)]));
    let (control_hub, carry) = control
        .advance_bal(
            identified(HEIGHT + 1, child_hash),
            parent_hash,
            &[address(0x11)],
        )
        .await;
    assert_eq!(report.outcome, carry.outcome);
    assert_eq!(report.writes, carry.writes);
    assert_eq!(
        driver
            .snapshot(identified(HEIGHT + 1, child_hash))
            .hub
            .snapshot_value(address(0x11), U256::from(1u64)),
        Some(U256::from(5u64))
    );
    let _ = control_hub;
}

/// The read window is two consecutive canonical anchors: the anchor the
/// installed head replaced keeps answering from the generation that WAS
/// installed there — the same handle, the same values, the readiness it reported
/// at its own settle — after the newer head moved in.
///
/// This is the window the fan-out needs, not a retention policy: a fan-out's
/// anchor is `max(candidate.solve_block)`, which trails the newest head by one
/// epoch, so "only the newest head answers" would leave nearly every fan-out on
/// the cold path.
#[tokio::test]
async fn the_previous_anchor_answers_from_the_generation_that_was_installed_there() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let h0 = B256::new([0xa0; 32]);
    let h1 = B256::new([0xa1; 32]);
    let h2 = B256::new([0xa2; 32]);
    stub.serve_bal_at(
        h1,
        bal_bytes(&[(address(0x11), U256::from(1u64), U256::from(5u64))]),
    );
    stub.serve_bal_at(
        h2,
        bal_bytes(&[(address(0x11), U256::from(2u64), U256::from(7u64))]),
    );
    let driver = BalHeadDriver::new(&provider);
    let relevance = [address(0x11)];

    let base = driver.deliver(identified(HEIGHT, h0), genesis());
    settled(&driver.advance(base, &relevance).await);
    let one = driver.deliver(identified(HEIGHT + 1, h1), h0);
    settled(&driver.advance(one, &relevance).await);
    assert_eq!(
        driver.previous(),
        Some(identified(HEIGHT, h0)),
        "the first link puts the anchor it replaced in the window"
    );
    let installed_handle = driver.snapshot(identified(HEIGHT + 1, h1)).hub;

    let two = driver.deliver(identified(HEIGHT + 2, h2), h1);
    settled(&driver.advance(two, &relevance).await);
    assert_eq!(
        driver.installed(),
        Some(identified(HEIGHT + 2, h2)),
        "the newest head is installed"
    );
    assert_eq!(
        driver.previous(),
        Some(identified(HEIGHT + 1, h1)),
        "the window holds exactly ONE previous anchor"
    );

    let view = driver.snapshot(identified(HEIGHT + 1, h1));
    assert_eq!(
        view.readiness,
        HeadReadiness::Ready,
        "the previous anchor answers with the readiness its own settle reported"
    );
    assert!(
        Arc::ptr_eq(&installed_handle, &view.hub),
        "the generation that was installed there - not a copy, not the newer one"
    );
    assert_eq!(
        view.hub.snapshot_value(address(0x11), U256::from(1u64)),
        Some(U256::from(5u64)),
        "its own head's write"
    );
    assert_eq!(
        view.hub.snapshot_value(address(0x11), U256::from(2u64)),
        None,
        "and NOT the newer head's word: the window answers an older anchor from \
         the older generation, never from the head above it"
    );
}

/// An anchor two below the installed head is OUTSIDE the window: it answers
/// cold and can never read the newer generation's words, and a gap CLEARS the
/// previous slot instead of reassigning it to some older generation.
#[tokio::test]
async fn an_anchor_two_below_the_installed_head_is_cold_and_never_served_from_the_newer_generation()
{
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let hashes: Vec<B256> = (0..5u8)
        .map(|index| B256::new([0xb0 + index; 32]))
        .collect();
    let relevance = [address(0x11)];
    let mut heads = Vec::new();
    for (step, hash) in hashes.iter().enumerate() {
        let number = HEIGHT + u64::try_from(step).unwrap_or_default();
        let slot = U256::from(u64::try_from(step).unwrap_or_default() + 1);
        let value = U256::from(u64::try_from(step).unwrap_or_default() + 100);
        stub.serve_bal_at(*hash, bal_bytes(&[(address(0x11), slot, value)]));
        heads.push(identified(number, *hash));
    }
    let driver = BalHeadDriver::new(&provider);
    for (step, head) in heads.iter().take(4).enumerate() {
        let parent = if step == 0 {
            genesis()
        } else {
            hashes[step - 1]
        };
        let ticket = driver.deliver(*head, parent);
        settled(&driver.advance(ticket, &relevance).await);
    }
    assert_eq!(
        driver.installed(),
        Some(heads[3]),
        "four consecutive heads settled"
    );
    assert_eq!(
        driver.previous(),
        Some(heads[2]),
        "the window is the installed head and exactly one below it"
    );

    let view = driver.snapshot(heads[1]);
    assert!(
        matches!(view.readiness, HeadReadiness::Cold(_)),
        "an anchor two below the installed head is cold, got {:?}",
        view.readiness
    );
    for slot in [1u64, 2, 3, 4] {
        assert_eq!(
            view.hub.snapshot_value(address(0x11), U256::from(slot)),
            None,
            "no word of the newer generation is reachable from an older anchor"
        );
    }

    // A gap settle clears the slot rather than reassigning it: the window gets
    // SMALLER across a gap, never wrong.
    let gap_hash = B256::new([0xbf; 32]);
    stub.serve_bal_at(gap_hash, bal_bytes(&[]));
    let gap = driver.deliver(identified(HEIGHT + 6, gap_hash), B256::new([0xfe; 32]));
    let report = settled(&driver.advance(gap, &relevance).await);
    assert_eq!(
        report.outcome,
        CarryOutcome::Cold {
            lineage: Some(CarryRefusal::NotConsecutive),
            bal: None,
        },
        "the gap head settles cold"
    );
    assert_eq!(
        driver.previous(),
        None,
        "a gap leaves no previous anchor: the window is two CONSECUTIVE anchors"
    );
    assert!(
        matches!(driver.snapshot(heads[2]).readiness, HeadReadiness::Cold(_)),
        "the anchor the cleared slot named is cold again"
    );
}

// ── The terminal-state wait: a caller joins the in-flight preparation ──────

/// A waiter at an in-flight head joins the EXISTING preparation: it fetches
/// nothing of its own, holds no lock across the wait, and resolves when the
/// head settles — with the settled generation's exact values.
#[tokio::test]
async fn a_waiter_joins_the_in_flight_preparation_and_resolves_with_the_settled_values() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, address(0x11), U256::from(2u64), 9);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(
        child_hash,
        bal_bytes(&[(address(0x11), U256::from(1u64), U256::from(5u64))]),
    );
    let driver = BalHeadDriver::new(&provider);
    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    driver
        .snapshot(identified(HEIGHT, parent_hash))
        .hub
        .read_storage(address(0x11), U256::from(2u64))
        .await
        .expect("the stub answers");
    stub.clear();
    stub.defer_bal();

    let anchor = identified(HEIGHT + 1, child_hash);
    let ticket = driver.deliver(anchor, parent_hash);
    let relevance = [address(0x11)];
    let mut prepare = Box::pin(driver.advance(ticket, &relevance));
    assert!(
        prepare
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending(),
        "the preparation is genuinely in flight, parked on the deferred gate"
    );

    let mut waiter = Box::pin(driver.wait_for_head(anchor));
    assert!(
        waiter
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending(),
        "the head is still preparing: the waiter registers and waits"
    );

    stub.release_bal();
    let (outcome, view) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(prepare, waiter)
    })
    .await
    .expect("the wait ends on the settle, never on a timer");

    assert!(
        matches!(outcome, HeadOutcome::Settled(_)),
        "the joined preparation settled: {outcome:?}"
    );
    assert_eq!(
        driver.completion(anchor),
        Some(HeadCompletion::Settled),
        "the head's own terminal result is its settle"
    );
    assert_eq!(view.readiness, HeadReadiness::Ready, "and it answers ready");
    assert_eq!(
        view.hub.snapshot_value(address(0x11), U256::from(2u64)),
        Some(U256::from(9u64)),
        "the carried parent word"
    );
    assert_eq!(
        view.hub.snapshot_value(address(0x11), U256::from(1u64)),
        Some(U256::from(5u64)),
        "the head's own write"
    );
    assert_eq!(
        stub.bal_requests(),
        1,
        "the waiter joined the fetch: one BAL request for the head, none of its own"
    );
}

/// Several fan-outs at one in-flight head share ONE terminal completion and one
/// fetch: every waiter resolves from the same settle, and none of them
/// duplicated the preparation.
#[tokio::test]
async fn several_waiters_share_one_terminal_completion_and_one_fetch() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, address(0x11), U256::from(2u64), 9);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);
    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    stub.clear();
    stub.defer_bal();

    let anchor = identified(HEIGHT + 1, child_hash);
    let ticket = driver.deliver(anchor, parent_hash);
    let mut prepare = Box::pin(driver.advance(ticket, &[]));
    assert!(
        prepare
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending(),
        "the preparation is in flight"
    );

    let mut first = Box::pin(driver.wait_for_head(anchor));
    let mut second = Box::pin(driver.wait_for_head(anchor));
    let mut third = Box::pin(driver.wait_for_head(anchor));
    for waiter in [&mut first, &mut second, &mut third] {
        assert!(
            waiter
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending(),
            "every waiter waits on the one in-flight head"
        );
    }

    stub.release_bal();
    let (outcome, one, two, three) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(prepare, first, second, third)
    })
    .await
    .expect("every waiter is woken by the one settle");

    assert!(matches!(outcome, HeadOutcome::Settled(_)));
    for view in [&one, &two, &three] {
        assert_eq!(view.readiness, HeadReadiness::Ready);
    }
    assert!(
        Arc::ptr_eq(&one.hub, &two.hub) && Arc::ptr_eq(&two.hub, &three.hub),
        "every waiter reads the ONE settled generation"
    );
    assert_eq!(
        stub.bal_requests(),
        1,
        "three waiters, one fetch: the join duplicates no RPC"
    );
}

/// Dropping one waiter cancels nothing: the preparation still settles, the
/// remaining waiters still resolve, and no waiter is stranded by its sibling's
/// cancellation.
#[tokio::test]
async fn dropping_one_waiter_never_strands_the_others_or_cancels_the_preparation() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);
    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    stub.clear();
    stub.defer_bal();

    let anchor = identified(HEIGHT + 1, child_hash);
    let ticket = driver.deliver(anchor, parent_hash);
    let mut prepare = Box::pin(driver.advance(ticket, &[]));
    assert!(prepare
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());

    let mut kept_first = Box::pin(driver.wait_for_head(anchor));
    let cancelled = Box::pin(driver.wait_for_head(anchor));
    let mut kept_second = Box::pin(driver.wait_for_head(anchor));
    for waiter in [&mut kept_first, &mut kept_second] {
        assert!(waiter
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending());
    }

    drop(cancelled);
    stub.release_bal();
    let (outcome, one, two) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(prepare, kept_first, kept_second)
    })
    .await
    .expect("the cancelled waiter's siblings still resolve");

    assert!(matches!(outcome, HeadOutcome::Settled(_)));
    assert_eq!(one.readiness, HeadReadiness::Ready);
    assert_eq!(two.readiness, HeadReadiness::Ready);
    assert_eq!(
        stub.bal_requests(),
        1,
        "the preparation was never cancelled"
    );
}

/// No lost notification: a settle that lands between a caller's first look and
/// its registration resolves the wait immediately — the registration is made
/// under the same lock hold that reads the head's state.
#[tokio::test]
async fn a_settle_between_the_first_look_and_the_registration_resolves_the_wait_immediately() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);
    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;

    let anchor = identified(HEIGHT + 1, child_hash);
    let ticket = driver.deliver(anchor, parent_hash);
    assert_eq!(
        driver.snapshot(anchor).readiness,
        HeadReadiness::Cold(HeadCold::Unprepared),
        "the first look sees the unprepared head"
    );

    // The settle lands before the caller registers.
    settled(&driver.advance(ticket, &[]).await);

    let view = tokio::time::timeout(Duration::from_secs(1), driver.wait_for_head(anchor))
        .await
        .expect("an already-settled head resolves the wait on its first poll");
    assert_eq!(view.readiness, HeadReadiness::Ready);
    assert_eq!(driver.completion(anchor), Some(HeadCompletion::Settled));
}

/// A head whose list cannot be read settles cold on the real reason: the
/// waiter resolves once, with the skip reported and no parent value served.
#[tokio::test]
async fn a_waiter_at_a_head_whose_list_cannot_be_read_resolves_with_the_real_cold_reason() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, address(0x11), U256::from(2u64), 9);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    // No list at `child_hash`: the node answers the fetch with a failure.
    let driver = BalHeadDriver::new(&provider);
    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    driver
        .snapshot(identified(HEIGHT, parent_hash))
        .hub
        .read_storage(address(0x11), U256::from(2u64))
        .await
        .expect("the stub answers");
    stub.clear();
    stub.defer_bal();

    let anchor = identified(HEIGHT + 1, child_hash);
    let ticket = driver.deliver(anchor, parent_hash);
    let mut prepare = Box::pin(driver.advance(ticket, &[]));
    assert!(prepare
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());
    let waiter = driver.wait_for_head(anchor);

    stub.release_bal();
    let (outcome, view) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(prepare, waiter)
    })
    .await
    .expect("the failed fetch is a real terminal outcome");

    assert!(matches!(outcome, HeadOutcome::Settled(_)));
    assert!(
        matches!(
            &view.readiness,
            HeadReadiness::Cold(HeadCold::Settled { bal: Some(_), .. })
        ),
        "the wait resolves with the real skip, not a timing label: {:?}",
        view.readiness
    );
    assert_eq!(
        view.hub.snapshot_value(address(0x11), U256::from(2u64)),
        None,
        "no parent value leaks into the cold answer"
    );
    assert_eq!(driver.completion(anchor), Some(HeadCompletion::Settled));
}

/// A head whose lineage cannot be carried settles cold on the exact refusal:
/// the waiter resolves with that reason and serves nothing carried.
#[tokio::test]
async fn a_waiter_at_a_lineage_refused_head_resolves_with_the_lineage_reason() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_bal_at(B256::new([0xbb; 32]), bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);
    let parent_ticket = driver.deliver(identified(HEIGHT, B256::new([0xaa; 32])), genesis());
    driver.advance(parent_ticket, &[]).await;
    stub.clear();
    stub.defer_bal();

    // Two blocks above the installed generation: the lineage cannot carry.
    let anchor = identified(HEIGHT + 2, B256::new([0xbb; 32]));
    let ticket = driver.deliver(anchor, B256::new([0xfe; 32]));
    let mut prepare = Box::pin(driver.advance(ticket, &[]));
    assert!(prepare
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());
    let waiter = driver.wait_for_head(anchor);

    stub.release_bal();
    let (_, view) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(prepare, waiter)
    })
    .await
    .expect("the lineage refusal is a real terminal outcome");

    assert_eq!(
        view.readiness,
        HeadReadiness::Cold(HeadCold::Settled {
            lineage: Some(CarryRefusal::NotConsecutive),
            bal: None,
        }),
        "the wait reports the exact refusal"
    );
    assert_eq!(view.hub.snapshot_len(), 0, "a refused head carries nothing");
}

/// A same-height reorg supersedes the waiting head: the waiter resolves with
/// the supersession, never with the replacement's result, and nothing of the
/// abandoned branch is ever served.
#[tokio::test]
async fn a_waiter_on_a_superseded_head_resolves_and_never_serves_the_abandoned_branch() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let parent_hash = B256::new([0xaa; 32]);
    let abandoned = B256::new([0xa1; 32]);
    let replacement = B256::new([0xa2; 32]);
    stub.serve_bal_at(
        abandoned,
        bal_bytes(&[(address(0x33), U256::from(4u64), U256::from(666u64))]),
    );
    stub.serve_bal_at(
        replacement,
        bal_bytes(&[(address(0x33), U256::from(4u64), U256::from(777u64))]),
    );
    let driver = BalHeadDriver::new(&provider);
    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    stub.clear();
    stub.defer_bal();

    let anchor = identified(HEIGHT + 1, abandoned);
    let ticket = driver.deliver(anchor, parent_hash);
    let relevance = [address(0x33)];
    let mut prepare = Box::pin(driver.advance(ticket, &relevance));
    assert!(prepare
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());
    let mut waiter = Box::pin(driver.wait_for_head(anchor));
    assert!(waiter
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());

    // The reorg replaces the block at the same height while the list is in
    // flight, and the abandoned fetch is released into a discarded apply.
    stub.release_bal();
    let replacement_ticket = driver.deliver(identified(HEIGHT + 1, replacement), parent_hash);
    let view = tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("the supersession wakes the waiter");
    drop(prepare);

    assert_eq!(
        view.readiness,
        HeadReadiness::Cold(HeadCold::Superseded),
        "the waiter resolves with its own generation's end, not the new head's"
    );
    assert_eq!(
        view.hub.snapshot_value(address(0x33), U256::from(4u64)),
        None,
        "the abandoned branch's value is never served"
    );

    // The replacement settles its own generation; a fresh wait joins THAT
    // generation under its own registration — never the stale one's.
    let (outcome, replacement_view) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            driver.advance(replacement_ticket, &relevance),
            driver.wait_for_head(identified(HEIGHT + 1, replacement))
        )
    })
    .await
    .expect("the replacement settles");
    assert!(matches!(outcome, HeadOutcome::Settled(_)));
    let settled_view = driver.snapshot(identified(HEIGHT + 1, replacement));
    assert_eq!(settled_view.readiness, HeadReadiness::Ready);
    assert_eq!(
        settled_view
            .hub
            .snapshot_value(address(0x33), U256::from(4u64)),
        Some(U256::from(777u64)),
        "the replacement serves its own word"
    );
    let _ = replacement_view;
}

/// A gap supersedes the waiting head just like a reorg: the waiter resolves,
/// and the head above the gap settles its own chain.
#[tokio::test]
async fn a_waiter_across_a_gap_resolves_with_the_actual_current_reason() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let parent_hash = B256::new([0xaa; 32]);
    let skipped = B256::new([0xb1; 32]);
    let above = B256::new([0xb2; 32]);
    stub.serve_bal_at(skipped, bal_bytes(&[]));
    stub.serve_bal_at(above, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);
    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    stub.clear();
    stub.defer_bal();

    let anchor = identified(HEIGHT + 1, skipped);
    let ticket = driver.deliver(anchor, parent_hash);
    let mut prepare = Box::pin(driver.advance(ticket, &[]));
    assert!(prepare
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());
    let waiter = driver.wait_for_head(anchor);

    // The pump skipped the block: the next delivered head is two above the
    // installed generation.
    stub.release_bal();
    let above_ticket = driver.deliver(identified(HEIGHT + 2, above), B256::new([0xfe; 32]));
    let view = tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("the gap's delivery wakes the waiter");
    drop(prepare);

    assert_eq!(
        view.readiness,
        HeadReadiness::Cold(HeadCold::Superseded),
        "the skipped head reports its own end"
    );
    let outcome = driver.advance(above_ticket, &[]).await;
    assert!(matches!(outcome, HeadOutcome::Settled(_)));
    assert_eq!(
        driver.snapshot(anchor).readiness,
        HeadReadiness::Cold(HeadCold::Superseded),
        "and the skipped head never resolves as the head above it"
    );
}

/// An abandoned preparation is TERMINAL for the waiters that joined it: the
/// claim's end wakes every one of them to the `abandoned` cold answer at that
/// exact anchor — no retry, no re-registration, and no stranded waiter.
#[tokio::test]
async fn an_abandoned_prepare_claim_wakes_its_waiters_with_the_terminal_abandonment() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, address(0x11), U256::from(2u64), 9);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);
    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    stub.clear();
    stub.defer_bal();

    let anchor = identified(HEIGHT + 1, child_hash);
    let ticket = driver.deliver(anchor, parent_hash);
    let mut cancelled = Box::pin(driver.advance(ticket, &[]));
    assert!(cancelled
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());
    let mut first = Box::pin(driver.wait_for_head(anchor));
    let mut second = Box::pin(driver.wait_for_head(anchor));
    for waiter in [&mut first, &mut second] {
        assert!(waiter
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending());
    }

    // The claimed preparation is cancelled: the claim's end is terminal for
    // this generation's waiters, and both wake to it — no retry runs.
    drop(cancelled);
    assert_eq!(
        driver.completion(anchor),
        Some(HeadCompletion::Abandoned),
        "the abandoned claim publishes its end"
    );

    let (one, two) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(first, second)
    })
    .await
    .expect("the abandonment is terminal: it wakes its waiters instead of stranding them");

    for view in [&one, &two] {
        assert_eq!(
            view.readiness,
            HeadReadiness::Cold(HeadCold::Abandoned),
            "the wait's terminal answer at this exact anchor"
        );
        assert_eq!(
            view.hub.snapshot_value(address(0x11), U256::from(2u64)),
            None,
            "no parent value leaks into the abandoned answer"
        );
    }
    assert_eq!(
        stub.bal_requests(),
        0,
        "no retry ran: the abandoned fetch never answered"
    );
}

/// A waiter arriving AFTER the abandonment resolves immediately: the head has
/// no live preparation owner, so the wait's terminal answer is already
/// decided and nothing is left to wait for.
#[tokio::test]
async fn a_waiter_arriving_after_an_abandonment_resolves_immediately() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);
    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    stub.clear();
    stub.defer_bal();

    let anchor = identified(HEIGHT + 1, child_hash);
    let ticket = driver.deliver(anchor, parent_hash);
    let mut cancelled = Box::pin(driver.advance(ticket, &[]));
    assert!(cancelled
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());
    drop(cancelled);

    let mut late = Box::pin(driver.wait_for_head(anchor));
    let view = match late.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(view) => view,
        Poll::Pending => panic!(
            "a waiter arriving after the abandonment resolves immediately, \
             never parks on a preparation that is gone"
        ),
    };
    assert_eq!(view.readiness, HeadReadiness::Cold(HeadCold::Abandoned));
    assert_eq!(driver.completion(anchor), Some(HeadCompletion::Abandoned));
}

/// A delivered head with no preparation owner — never claimed, or claimed by
/// a future nobody ever polled — resolves `abandoned` without parking: the
/// wait joins a preparation only while that preparation is genuinely in
/// flight.
#[tokio::test]
async fn a_delivered_head_with_no_preparation_owner_resolves_abandoned_without_parking() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    let driver = BalHeadDriver::new(&provider);
    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    stub.clear();

    let anchor = identified(HEIGHT + 1, child_hash);
    let ticket = driver.deliver(anchor, parent_hash);

    // Delivered and never claimed: the terminal answer is immediate.
    let mut waiter = Box::pin(driver.wait_for_head(anchor));
    let view = match waiter
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(view) => view,
        Poll::Pending => panic!("a head nothing prepared resolves immediately"),
    };
    assert_eq!(view.readiness, HeadReadiness::Cold(HeadCold::Abandoned));

    // A preparation that exists but was never polled owns nothing either.
    let unpolled = Box::pin(driver.advance(ticket, &[]));
    let mut waiter = Box::pin(driver.wait_for_head(anchor));
    let view = match waiter
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(view) => view,
        Poll::Pending => panic!("an unpolled preparation is no owner: the wait resolves"),
    };
    drop(unpolled);
    assert_eq!(view.readiness, HeadReadiness::Cold(HeadCold::Abandoned));
    assert_eq!(driver.completion(anchor), None, "no claim ever ran");
    assert_eq!(
        stub.bal_requests(),
        0,
        "nothing fetched: no preparation ran"
    );
}

/// An explicit retry after the abandonment settles the head for NEW callers
/// only: the generation answers ready from then on, and a fan-out that
/// already resolved `abandoned` is never retroactively changed.
#[tokio::test]
async fn a_later_explicit_retry_settles_the_head_for_new_callers_only() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);
    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    stub.clear();
    stub.defer_bal();

    let anchor = identified(HEIGHT + 1, child_hash);
    let ticket = driver.deliver(anchor, parent_hash);
    let mut cancelled = Box::pin(driver.advance(ticket, &[]));
    assert!(cancelled
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());
    let mut joined = Box::pin(driver.wait_for_head(anchor));
    assert!(joined
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());

    drop(cancelled);
    let failed = tokio::time::timeout(Duration::from_secs(5), joined)
        .await
        .expect("the abandonment wakes its waiter");
    assert_eq!(failed.readiness, HeadReadiness::Cold(HeadCold::Abandoned));

    // The explicit retry settles the head; NEW callers read the now-ready
    // generation.
    stub.release_bal();
    let outcome = tokio::time::timeout(Duration::from_secs(5), driver.advance(ticket, &[]))
        .await
        .expect("the explicit retry settles");
    assert!(matches!(outcome, HeadOutcome::Settled(_)));
    assert_eq!(driver.snapshot(anchor).readiness, HeadReadiness::Ready);
    let mut late = Box::pin(driver.wait_for_head(anchor));
    let view = match late.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(view) => view,
        Poll::Pending => panic!("the settled head resolves immediately"),
    };
    assert_eq!(view.readiness, HeadReadiness::Ready);
    assert_eq!(
        failed.readiness,
        HeadReadiness::Cold(HeadCold::Abandoned),
        "an already-failed fan-out is never retroactively changed"
    );
    assert_eq!(stub.bal_requests(), 1, "the retry fetched exactly once");
}

// ── Terminal completion is scoped to one preparation CLAIM ATTEMPT ────────
//
// A head is delivered once but can be claimed more than once: a dropped
// (cancelled) advance releases its claim and a retry prepares the same anchor
// again. Each attempt owns its waiters' terminal answer, so abandoning attempt N
// stays `abandoned` for the waiters that joined it even when attempt N+1 settles
// (is superseded, or is cancelled itself) before those waiters poll. The later
// attempt's result is the CURRENT attempt's completion, never a rewrite of an
// abandonment already decided.

/// Regression: abandon attempt N, settle attempt N+1 on the same anchor, and
/// poll attempt N's waiter only after that settle. The waiter still resolves
/// `abandoned` at its exact anchor carrying nothing, while `completion(anchor)`
/// reports the current attempt's `settled` and the snapshot is ready.
#[tokio::test]
async fn an_abandoned_claims_waiter_resolves_abandoned_even_when_the_retry_settles_first() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, address(0x11), U256::from(2u64), 9);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(
        child_hash,
        bal_bytes(&[(address(0x11), U256::from(1u64), U256::from(5u64))]),
    );
    let driver = BalHeadDriver::new(&provider);
    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    driver
        .snapshot(identified(HEIGHT, parent_hash))
        .hub
        .read_storage(address(0x11), U256::from(2u64))
        .await
        .expect("the stub answers");
    stub.clear();
    stub.defer_bal();

    let anchor = identified(HEIGHT + 1, child_hash);
    let ticket = driver.deliver(anchor, parent_hash);
    let mut cancelled = Box::pin(driver.advance(ticket, &[]));
    assert!(
        cancelled
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending(),
        "attempt N's preparation is in flight"
    );
    let mut abandoned = Box::pin(driver.wait_for_head(anchor));
    assert!(
        abandoned
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending(),
        "the waiter joins attempt N's live claim, not the bare anchor"
    );

    // Attempt N ends without applying; its waiter is deliberately left unpolled.
    drop(cancelled);
    assert_eq!(
        driver.completion(anchor),
        Some(HeadCompletion::Abandoned),
        "attempt N publishes its own abandonment"
    );
    assert_eq!(
        driver.deliver(anchor, parent_hash),
        ticket,
        "an abandonment mints no new public delivery: the retry reuses this ticket's seq"
    );

    // Attempt N+1 claims the same head at the same public seq and settles the
    // anchor before attempt N's waiter polls.
    stub.release_bal();
    let retried = tokio::time::timeout(Duration::from_secs(5), driver.advance(ticket, &[]))
        .await
        .expect("the retry settles");
    settled(&retried);
    assert_eq!(
        driver.completion(anchor),
        Some(HeadCompletion::Settled),
        "the CURRENT attempt's completion is its own settle"
    );
    assert_eq!(driver.snapshot(anchor).readiness, HeadReadiness::Ready);

    let view = tokio::time::timeout(Duration::from_secs(5), abandoned)
        .await
        .expect("attempt N's waiter wakes on attempt N's end, not on the retry's");
    assert_eq!(
        view.readiness,
        HeadReadiness::Cold(HeadCold::Abandoned),
        "attempt N's abandonment is terminal for attempt N's waiters"
    );
    assert_eq!(
        view.hub.snapshot_value(address(0x11), U256::from(2u64)),
        None,
        "the abandoned answer carries no parent value"
    );
    assert_eq!(
        view.hub.snapshot_value(address(0x11), U256::from(1u64)),
        None,
        "nor the retry's generation write"
    );
}

/// A repeat delivery of the live preparation reuses its claim attempt: the
/// waiter already registered on it still resolves from the one settle, a waiter
/// arriving after the repeat joins that same attempt, and the public ticket is
/// unchanged — one fetch, one attempt, one terminal result.
#[tokio::test]
async fn a_repeat_delivery_of_a_live_preparation_reuses_its_claim_attempt_and_waiters() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);
    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    stub.clear();
    stub.defer_bal();

    let anchor = identified(HEIGHT + 1, child_hash);
    let ticket = driver.deliver(anchor, parent_hash);
    let mut prepare = Box::pin(driver.advance(ticket, &[]));
    assert!(prepare
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());
    let mut joined_before = Box::pin(driver.wait_for_head(anchor));
    assert!(joined_before
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());

    assert_eq!(
        driver.deliver(anchor, parent_hash),
        ticket,
        "the repeat delivery mints no new public ticket"
    );
    assert_eq!(
        driver.stats().repeated,
        1,
        "the repeat is counted as a repeat delivery, not a delivery"
    );

    // A waiter arriving AFTER the repeat joins the same attempt — the repeat
    // minted no second epoch, so both waiters wake on the one settle below.
    let mut joined_after = Box::pin(driver.wait_for_head(anchor));
    assert!(joined_after
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());

    stub.release_bal();
    let (outcome, before, after) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(prepare, joined_before, joined_after)
    })
    .await
    .expect("the one attempt's settle wakes every waiter it owns");
    settled(&outcome);
    for view in [&before, &after] {
        assert_eq!(view.readiness, HeadReadiness::Ready);
    }
    assert!(
        Arc::ptr_eq(&before.hub, &after.hub),
        "both waiters read the one settled generation"
    );
    assert_eq!(driver.completion(anchor), Some(HeadCompletion::Settled));
    assert_eq!(
        stub.bal_requests(),
        1,
        "the repeat delivery duplicated no fetch"
    );
}

/// A cancelled retry is terminal for its own waiters: aborting attempt N leaves
/// its waiter `abandoned`, attempt N+1 claims the same anchor with its own
/// waiter, and aborting that leaves the second waiter `abandoned` too — each
/// attempt publishing on its own epoch, and the current attempt's abandonment
/// visible through `completion(anchor)`.
#[tokio::test]
async fn a_cancelled_retry_is_terminal_for_its_own_waiters() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);
    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    stub.clear();
    stub.defer_bal();

    let anchor = identified(HEIGHT + 1, child_hash);
    let ticket = driver.deliver(anchor, parent_hash);
    let mut first = Box::pin(driver.advance(ticket, &[]));
    assert!(first
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());
    let mut first_waiter = Box::pin(driver.wait_for_head(anchor));
    assert!(first_waiter
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());
    drop(first);

    // The retry claims the same head; the gate is still armed, so attempt N+1 is
    // genuinely in flight when its waiter joins it.
    let mut retry = Box::pin(driver.advance(ticket, &[]));
    assert!(retry
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());
    let mut second_waiter = Box::pin(driver.wait_for_head(anchor));
    assert!(second_waiter
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());
    drop(retry);
    assert_eq!(
        driver.completion(anchor),
        Some(HeadCompletion::Abandoned),
        "the current attempt's own abandonment is what completion reports"
    );

    let (one, two) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(first_waiter, second_waiter)
    })
    .await
    .expect("each attempt's end wakes its own waiters");
    for view in [&one, &two] {
        assert_eq!(view.readiness, HeadReadiness::Cold(HeadCold::Abandoned));
    }
    assert_eq!(
        stub.bal_requests(),
        0,
        "neither attempt's fetch answered: both ended abandoned"
    );
}

/// An apply keyed to the head's public ticket settles the CURRENT attempt: a
/// caller that prepared the list out of band applies it while a retry holds the
/// claim, the retry's waiter wakes to the settle, and the abandoned attempt's
/// waiter keeps its abandonment — the released attempt's claim publishes nothing
/// over the attempt that owns the head.
#[tokio::test]
async fn an_apply_settles_the_current_attempt_and_never_rewrites_an_earlier_abandonment() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);
    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;

    let anchor = identified(HEIGHT + 1, child_hash);
    let ticket = driver.deliver(anchor, parent_hash);
    // The out-of-band list, prepared before the fetch gate arms.
    stub.clear();
    let prepared = PreparedBal::prepare(&provider, anchor).await;
    stub.defer_bal();

    let mut cancelled = Box::pin(driver.advance(ticket, &[]));
    assert!(cancelled
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());
    let mut abandoned = Box::pin(driver.wait_for_head(anchor));
    assert!(abandoned
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());
    drop(cancelled);
    assert_eq!(
        driver.completion(anchor),
        Some(HeadCompletion::Abandoned),
        "attempt N ended abandoned"
    );

    // Attempt N+1 claims the head and parks on the gate; its waiter joins it.
    let mut retry = Box::pin(driver.advance(ticket, &[]));
    assert!(retry
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());
    let mut current = Box::pin(driver.wait_for_head(anchor));
    assert!(current
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());

    // The out-of-band apply lands while attempt N+1 is still live: the shared
    // public ticket settles the head, and the settle belongs to the CURRENT
    // attempt.
    let outcome = driver.apply(ticket, &prepared, &[]);
    assert!(
        matches!(outcome, HeadOutcome::Settled(_)),
        "the shared ticket applies: {outcome:?}"
    );
    assert_eq!(
        driver.completion(anchor),
        Some(HeadCompletion::Settled),
        "the settle is the current attempt's completion"
    );
    let current_view = tokio::time::timeout(Duration::from_secs(5), current)
        .await
        .expect("the current attempt's waiter wakes on the settle");
    assert_eq!(current_view.readiness, HeadReadiness::Ready);
    let abandoned_view = tokio::time::timeout(Duration::from_secs(5), abandoned)
        .await
        .expect("the abandoned attempt's waiter wakes on its own end");
    assert_eq!(
        abandoned_view.readiness,
        HeadReadiness::Cold(HeadCold::Abandoned),
        "the settle never rewrites the abandonment attempt N published"
    );

    // The retry's own fetch still lands and installs nothing.
    stub.release_bal();
    let late = tokio::time::timeout(Duration::from_secs(5), retry)
        .await
        .expect("the retry's fetch resolves");
    assert_eq!(
        late,
        HeadOutcome::AlreadySettled,
        "a settled head is idempotent for the attempt that arrives late"
    );
    assert_eq!(
        driver.completion(anchor),
        Some(HeadCompletion::Settled),
        "the released attempt publishes nothing over the settle"
    );
    assert_eq!(
        stub.bal_requests(),
        2,
        "one out-of-band prepare and one retry fetch; the cancelled attempt never answered"
    );
}

/// A supersession while a retry owns the head is the CURRENT attempt's end: the
/// retry's waiter resolves `superseded`, the abandoned attempt's waiter keeps
/// its `abandoned`, and the delivery that replaced the head writes neither
/// attempt's terminal answer over another's.
#[tokio::test]
async fn a_supersession_of_a_retried_attempt_leaves_the_earlier_abandonment_terminal() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    let replacement = B256::new([0xb2; 32]);
    stub.serve_bal_at(child_hash, bal_bytes(&[]));
    stub.serve_bal_at(replacement, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);
    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    stub.clear();
    stub.defer_bal();

    let anchor = identified(HEIGHT + 1, child_hash);
    let ticket = driver.deliver(anchor, parent_hash);
    let mut first = Box::pin(driver.advance(ticket, &[]));
    assert!(first
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());
    let mut abandoned = Box::pin(driver.wait_for_head(anchor));
    assert!(abandoned
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());
    drop(first);

    let mut retry = Box::pin(driver.advance(ticket, &[]));
    assert!(retry
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());
    let mut superseded = Box::pin(driver.wait_for_head(anchor));
    assert!(superseded
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());

    // A same-height replacement supersedes the retried head.
    driver.deliver(identified(HEIGHT + 1, replacement), parent_hash);
    let superseded_view = tokio::time::timeout(Duration::from_secs(5), superseded)
        .await
        .expect("the supersession wakes the current attempt's waiter");
    assert_eq!(
        superseded_view.readiness,
        HeadReadiness::Cold(HeadCold::Superseded)
    );
    let abandoned_view = tokio::time::timeout(Duration::from_secs(5), abandoned)
        .await
        .expect("the abandoned attempt's waiter wakes on its own end");
    assert_eq!(
        abandoned_view.readiness,
        HeadReadiness::Cold(HeadCold::Abandoned),
        "the replacement supersedes the current attempt, never the abandonment already published"
    );
    assert_eq!(
        driver.snapshot(anchor).readiness,
        HeadReadiness::Cold(HeadCold::Superseded)
    );
    drop(retry);
}

/// Regression: attempt N is abandoned, then the SAME anchor arrives again with a
/// DIFFERENT parent — a new head identity — before any retry advance touches
/// attempt N. The redelivery supersedes the head attempt N held, but a
/// supersession is not a second terminal for an attempt that already ended: the
/// waiter that joined attempt N still resolves `abandoned`, while the
/// replacement head carries its own attempt, its own completion and its own
/// lifecycle.
#[tokio::test]
async fn a_redelivery_with_a_different_parent_never_rewrites_an_abandoned_attempt() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    let alternate_parent = B256::new([0xab; 32]);
    stub.serve_bal_at(
        child_hash,
        bal_bytes(&[(address(0x11), U256::from(1u64), U256::from(5u64))]),
    );
    let driver = BalHeadDriver::new(&provider);
    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    stub.clear();
    stub.defer_bal();

    let anchor = identified(HEIGHT + 1, child_hash);
    let ticket = driver.deliver(anchor, parent_hash);
    let mut cancelled = Box::pin(driver.advance(ticket, &[]));
    assert!(
        cancelled
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending(),
        "attempt N's preparation is in flight"
    );
    let mut abandoned = Box::pin(driver.wait_for_head(anchor));
    assert!(
        abandoned
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending(),
        "the waiter joins attempt N's live claim, not the bare anchor"
    );

    // Attempt N ends without applying; its waiter is deliberately left unpolled.
    drop(cancelled);
    assert_eq!(
        driver.completion(anchor),
        Some(HeadCompletion::Abandoned),
        "attempt N publishes its own abandonment"
    );

    // The same anchor arrives again with a DIFFERENT parent, before any retry
    // advance: a distinct head identity, so it mints a new public ticket — and
    // its delivery supersedes the head attempt N held.
    let replacement = driver.deliver(anchor, alternate_parent);
    assert_ne!(
        replacement, ticket,
        "a different parent is a different delivery, not a repeat"
    );

    // The replacement owns its own attempt and its own lifecycle: one fetch, one
    // settle, and its own completion on the head it replaced attempt N on.
    stub.release_bal();
    let replacement_outcome =
        tokio::time::timeout(Duration::from_secs(5), driver.advance(replacement, &[]))
            .await
            .expect("the replacement's preparation resolves");
    settled(&replacement_outcome);
    assert_eq!(
        driver.delivered(),
        Some(anchor),
        "the replacement head is the driver's current one"
    );
    assert_eq!(
        driver.installed(),
        Some(anchor),
        "the replacement's own apply installed its anchor"
    );
    assert_eq!(
        driver.completion(anchor),
        Some(HeadCompletion::Settled),
        "the CURRENT attempt's completion is the replacement's settle"
    );
    let replacement_view = driver.snapshot(anchor);
    assert_ne!(
        replacement_view.readiness,
        HeadReadiness::Cold(HeadCold::Abandoned),
        "the replacement's lifecycle is not an abandonment"
    );

    let view = tokio::time::timeout(Duration::from_secs(5), abandoned)
        .await
        .expect("attempt N's waiter wakes on attempt N's end");
    assert_eq!(
        view.readiness,
        HeadReadiness::Cold(HeadCold::Abandoned),
        "a supersession after the abandonment is not attempt N's terminal result"
    );
}

/// Regression: attempt N is abandoned and NO retry ever claims the head. An
/// out-of-band apply on the head's own public ticket settles that head anyway,
/// so the settled truth has to reach the head's CURRENT completion: the settle
/// mints a fresh private claim epoch/channel — the public `seq` is unchanged —
/// instead of rewriting the abandonment attempt N published, which stays
/// attempt N's waiters' answer.
#[tokio::test]
async fn an_out_of_band_apply_settles_an_abandoned_heads_current_completion() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(child_hash, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);
    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;

    let anchor = identified(HEIGHT + 1, child_hash);
    let ticket = driver.deliver(anchor, parent_hash);
    // The out-of-band list, prepared before the fetch gate arms.
    stub.clear();
    let prepared = PreparedBal::prepare(&provider, anchor).await;
    stub.defer_bal();

    let mut cancelled = Box::pin(driver.advance(ticket, &[]));
    assert!(cancelled
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());
    let mut abandoned = Box::pin(driver.wait_for_head(anchor));
    assert!(
        abandoned
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending(),
        "the waiter joins attempt N's live claim, not the bare anchor"
    );

    // Attempt N ends without applying, and no retry claims the head: its current
    // claim is a non-live attempt that already published its end, and its waiter
    // is deliberately left unpolled.
    drop(cancelled);
    assert_eq!(
        driver.completion(anchor),
        Some(HeadCompletion::Abandoned),
        "attempt N publishes its own abandonment"
    );

    // The head settles out of band — no retry advance, no live claim.
    let outcome = driver.apply(ticket, &prepared, &[]);
    assert!(
        matches!(outcome, HeadOutcome::Settled(_)),
        "the abandoned head still applies: {outcome:?}"
    );
    assert_eq!(
        driver.installed(),
        Some(anchor),
        "the settle installed the generation it applied"
    );
    assert_eq!(driver.snapshot(anchor).readiness, HeadReadiness::Ready);
    assert_eq!(
        driver.completion(anchor),
        Some(HeadCompletion::Settled),
        "the settled truth is the head's CURRENT completion, not attempt N's end"
    );
    assert_eq!(
        driver.deliver(anchor, parent_hash),
        ticket,
        "the settlement minted no new public delivery: the ticket's seq is unchanged"
    );

    let mut late = Box::pin(driver.wait_for_head(anchor));
    let late_view = match late.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(view) => view,
        Poll::Pending => panic!("a settled head resolves a new wait immediately"),
    };
    assert_eq!(late_view.readiness, HeadReadiness::Ready);

    let view = tokio::time::timeout(Duration::from_secs(5), abandoned)
        .await
        .expect("attempt N's waiter wakes on attempt N's end");
    assert_eq!(
        view.readiness,
        HeadReadiness::Cold(HeadCold::Abandoned),
        "the settle never rewrites the abandonment attempt N published"
    );
}

// ── The three current-head poses and the projections they feed ──────────────
//
// A head the driver holds is in exactly one pose: delivered and UNCLAIMED, owned
// by a live PREPARING attempt, or SETTLED. Everything a caller can observe about
// it — the shared cold generation, the settle-derived readiness, the completion
// its waiters wake on, the read window — is a projection of that pose plus the
// generation history, never a fourth pose and never a second source of truth.

/// A delivered head nobody has claimed accepts a DIRECT prepare: the caller
/// fetches and applies the list itself, so no preparation is ever in flight,
/// nothing is abandoned, and the head's current completion is the settle.
#[tokio::test]
async fn a_direct_prepare_settles_an_unclaimed_head_without_a_claim_attempt() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, address(0x11), U256::from(1u64), 1);
    let parent_hash = B256::new([0xaa; 32]);
    let child_hash = B256::new([0xbb; 32]);
    stub.serve_bal_at(
        child_hash,
        bal_bytes(&[(address(0x11), U256::from(1u64), U256::from(5u64))]),
    );
    let driver = BalHeadDriver::new(&provider);
    let parent_ticket = driver.deliver(identified(HEIGHT, parent_hash), genesis());
    driver.advance(parent_ticket, &[]).await;
    driver
        .snapshot(identified(HEIGHT, parent_hash))
        .hub
        .read_storage(address(0x11), U256::from(1u64))
        .await
        .expect("the stub answers");
    stub.clear();

    let anchor = identified(HEIGHT + 1, child_hash);
    let ticket = driver.deliver(anchor, parent_hash);
    assert_eq!(
        driver.completion(anchor),
        None,
        "a delivered head that nothing claimed has published no terminal result"
    );
    assert_eq!(
        driver.snapshot(anchor).readiness,
        HeadReadiness::Cold(HeadCold::Unprepared),
        "the unclaimed pose reads unprepared, never abandoned"
    );

    // The caller's own fetch-and-apply: no advance, no claim, no waiter.
    let prepared = driver.prepare(ticket).await;
    assert!(prepared.is_decoded(), "the head's own list decoded");
    let report = settled(&driver.apply(ticket, &prepared, &[address(0x11)]));
    assert_eq!(
        report.outcome,
        CarryOutcome::Carried { carried_values: 1 },
        "the direct apply runs the same settle a claimed advance would"
    );
    assert_eq!(
        driver.completion(anchor),
        Some(HeadCompletion::Settled),
        "the settle is the unclaimed head's own terminal answer"
    );
    assert_eq!(driver.snapshot(anchor).readiness, HeadReadiness::Ready);
    assert_eq!(
        driver.stats().settled,
        2,
        "the parent and the directly prepared child settled"
    );

    let mut late = Box::pin(driver.wait_for_head(anchor));
    match late.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(view) => assert_eq!(
            view.readiness,
            HeadReadiness::Ready,
            "a wait on a settled head answers the settled generation"
        ),
        Poll::Pending => panic!("a settled head resolves a wait immediately"),
    }
}

/// Supersession history is ONE slot: only the anchor the latest delivery
/// replaced reports `superseded`, an older abandoned anchor reports
/// `undelivered`, and every abandoned head's late list installs nothing.
#[tokio::test]
async fn a_distant_supersession_keeps_only_the_last_abandoned_anchor() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let hashes = [
        B256::new([0xc0; 32]),
        B256::new([0xc1; 32]),
        B256::new([0xc2; 32]),
    ];
    for hash in hashes {
        stub.serve_bal_at(hash, bal_bytes(&[]));
    }
    let driver = BalHeadDriver::new(&provider);

    let first = driver.deliver(identified(HEIGHT + 1, hashes[0]), genesis());
    let second = driver.deliver(identified(HEIGHT + 1, hashes[1]), genesis());
    assert_eq!(
        driver.snapshot(identified(HEIGHT + 1, hashes[0])).readiness,
        HeadReadiness::Cold(HeadCold::Superseded),
        "the anchor the latest delivery replaced reports itself"
    );
    let third = driver.deliver(identified(HEIGHT + 1, hashes[2]), genesis());
    assert_eq!(
        driver.stats().superseded,
        2,
        "each replacement of an unsettled head is counted"
    );
    assert_eq!(
        driver.snapshot(identified(HEIGHT + 1, hashes[1])).readiness,
        HeadReadiness::Cold(HeadCold::Superseded),
        "the newest replacement is the one the history names"
    );
    assert_eq!(
        driver.snapshot(identified(HEIGHT + 1, hashes[0])).readiness,
        HeadReadiness::Cold(HeadCold::Undelivered),
        "the older abandoned anchor is outside the one-slot history"
    );

    // Both abandoned lists are dead, and they are discarded without moving the
    // generation the driver still holds.
    let abandoned = driver.prepare(first).await;
    assert_eq!(
        driver.apply(first, &abandoned, &[]),
        HeadOutcome::Superseded {
            head: identified(HEIGHT + 1, hashes[2]),
        }
    );
    let stale = driver.prepare(second).await;
    assert_eq!(
        driver.apply(second, &stale, &[]),
        HeadOutcome::Superseded {
            head: identified(HEIGHT + 1, hashes[2]),
        }
    );
    assert_eq!(driver.stats().discarded, 2, "both refusals are counted");
    assert_eq!(driver.installed(), None, "neither abandoned list installed");

    settled(&driver.advance(third, &[]).await);
    assert_eq!(
        driver.installed(),
        Some(identified(HEIGHT + 1, hashes[2])),
        "the head the driver holds still settles"
    );
}

/// The previous-window slot restates the readiness the generation ITSELF settled
/// with: a parent that settled cold answers its cold reason through the window
/// once a child it links installs over it.
#[tokio::test]
async fn the_previous_window_reports_the_cold_readiness_its_generation_settled_with() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let base_hash = B256::new([0xd5; 32]);
    let cold_hash = B256::new([0xd6; 32]);
    let heir_hash = B256::new([0xd7; 32]);
    stub.serve_bal_at(base_hash, bal_bytes(&[]));
    // No list is served at `cold_hash`: that head settles cold.
    stub.serve_bal_at(heir_hash, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);

    let base = driver.deliver(identified(HEIGHT, base_hash), genesis());
    settled(&driver.advance(base, &[]).await);

    let cold = identified(HEIGHT + 1, cold_hash);
    let cold_ticket = driver.deliver(cold, base_hash);
    let report = settled(&driver.advance(cold_ticket, &[]).await);
    let CarryOutcome::Cold {
        lineage: None,
        bal: Some(bal),
    } = &report.outcome
    else {
        panic!(
            "an unreadable list settles the head cold: {:?}",
            report.outcome
        );
    };
    let cold_readiness = HeadReadiness::Cold(HeadCold::Settled {
        lineage: None,
        bal: Some(bal.clone()),
    });
    assert_eq!(
        driver.snapshot(cold).readiness,
        cold_readiness,
        "the settled cold head reports its own reason"
    );

    let heir = driver.deliver(identified(HEIGHT + 2, heir_hash), cold_hash);
    settled(&driver.advance(heir, &[]).await);
    assert_eq!(
        driver.previous(),
        Some(cold),
        "the child that linked the cold head puts it in the window"
    );
    let view = driver.snapshot(cold);
    assert_eq!(
        view.readiness, cold_readiness,
        "the window restates the cold reason, never `ready` for a generation that \
         carried nothing"
    );
    assert_eq!(
        view.hub.snapshot_len(),
        0,
        "the cold generation carried nothing and still carries nothing"
    );
}

/// The lifecycle's counters count exactly the transitions it ran — no transition
/// is counted twice and none is invented: one delivery per head, one repeat per
/// repeated delivery, one supersession per unsettled head replaced, one settle
/// per installed generation, one already-current delivery, one discarded apply
/// per refusal, and one view per snapshot answer.
#[tokio::test]
async fn the_lifecycle_counters_count_exactly_the_transitions_it_ran() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let base_hash = B256::new([0xe0; 32]);
    let skipped_hash = B256::new([0xe1; 32]);
    let sibling_hash = B256::new([0xe2; 32]);
    let heir_hash = B256::new([0xe3; 32]);
    let next_hash = B256::new([0xe4; 32]);
    for hash in [base_hash, sibling_hash, heir_hash, next_hash] {
        stub.serve_bal_at(hash, bal_bytes(&[]));
    }
    let driver = BalHeadDriver::new(&provider);
    let base = identified(HEIGHT, base_hash);
    let skipped = identified(HEIGHT + 1, skipped_hash);
    let sibling = identified(HEIGHT + 1, sibling_hash);
    let heir = identified(HEIGHT + 2, heir_hash);
    let next = identified(HEIGHT + 3, next_hash);

    // One delivery, one cold view, one settle (the first head is lineage-refused
    // and therefore settles cold), one cold view of that settled generation.
    let base_ticket = driver.deliver(base, genesis());
    assert_eq!(
        driver.snapshot(base).readiness,
        HeadReadiness::Cold(HeadCold::Unprepared)
    );
    settled(&driver.advance(base_ticket, &[]).await);
    assert_eq!(
        driver.snapshot(base).readiness.cold_reason(),
        Some(&HeadCold::Settled {
            lineage: Some(CarryRefusal::NoParent),
            bal: None,
        })
    );

    // An idempotent repeat delivers nothing new.
    assert_eq!(driver.deliver(base, genesis()), base_ticket);

    // A head above the installed one, replaced by a delivery of the installed
    // anchor: one supersession, one already-current delivery. The delivered
    // installed anchor keeps answering cold, so its snapshot is a cold view.
    let skipped_ticket = driver.deliver(skipped, base_hash);
    assert_eq!(
        driver.snapshot(skipped).readiness,
        HeadReadiness::Cold(HeadCold::Unprepared)
    );
    driver.deliver(base, genesis());
    assert_eq!(
        driver.snapshot(base).readiness.cold_reason(),
        Some(&HeadCold::Settled {
            lineage: Some(CarryRefusal::NoParent),
            bal: None,
        }),
        "an already-current delivery restates the readiness the generation settled with"
    );
    let abandoned = driver.prepare(skipped_ticket).await;
    assert_eq!(
        driver.apply(skipped_ticket, &abandoned, &[]),
        HeadOutcome::Superseded { head: base },
        "the replaced head's list is discarded"
    );

    // A sibling that links the installed generation settles ready, and one cold
    // view of it while it is unprepared.
    let sibling_ticket = driver.deliver(sibling, base_hash);
    assert_eq!(
        driver.snapshot(sibling).readiness,
        HeadReadiness::Cold(HeadCold::Unprepared)
    );
    settled(&driver.advance(sibling_ticket, &[]).await);
    assert_eq!(
        driver.snapshot(sibling).readiness,
        HeadReadiness::Ready,
        "the linked sibling carries the installed parent"
    );

    // Two more heads, each settled ready and read once.
    for (anchor, parent) in [(heir, sibling_hash), (next, heir_hash)] {
        let ticket = driver.deliver(anchor, parent);
        settled(&driver.advance(ticket, &[]).await);
        assert_eq!(driver.snapshot(anchor).readiness, HeadReadiness::Ready);
    }

    assert_eq!(
        driver.stats(),
        HeadStats {
            delivered: 6,
            repeated: 1,
            superseded: 1,
            settled: 4,
            already_current: 1,
            discarded: 1,
            ready_views: 3,
            cold_views: 5,
        },
        "every counter counts exactly the transitions this driver ran"
    );
}

/// The head reports the transition the caller just got back, and names the
/// generation that transition installed: one settle, one report.
fn assert_head_reports(settled: &Arc<DeltaReport>, driver: &BalHeadDriver, anchor: ReadAnchor) {
    assert!(
        Arc::ptr_eq(
            settled,
            &driver
                .last_report()
                .expect("a settle stores the report it returned")
        ),
        "the head reports the transition the driver just ran"
    );
    assert_eq!(
        driver.installed(),
        Some(anchor),
        "a settle always names the generation it installed"
    );
}

/// An ordinary advance stores the report it returned: `last_report()` is
/// Arc-identical to the `DeltaReport` inside the `HeadOutcome::Settled` the
/// caller just got, and the head names the generation that settle installed.
#[tokio::test]
async fn an_advance_stores_the_report_it_settled_with() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let hash = B256::new([0xa8; 32]);
    stub.serve_bal_at(hash, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);
    let anchor = identified(HEIGHT, hash);

    let ticket = driver.deliver(anchor, genesis());
    let report = settled(&driver.advance(ticket, &[]).await);
    assert_head_reports(&report, &driver, anchor);
}

/// A retry that settles a head whose earlier attempt was abandoned stores ITS
/// report: the abandoned attempt produced none, so the head's `last_report()` is
/// the retry's transition.
#[tokio::test]
async fn a_retry_settles_an_abandoned_head_with_its_own_report() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let parent_hash = B256::new([0xa9; 32]);
    let child_hash = B256::new([0xaa; 32]);
    stub.serve_bal_at(parent_hash, bal_bytes(&[]));
    stub.serve_bal_at(child_hash, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);
    let parent = identified(HEIGHT, parent_hash);
    settled(&driver.advance(driver.deliver(parent, genesis()), &[]).await);

    stub.defer_bal();
    let anchor = identified(HEIGHT + 1, child_hash);
    let ticket = driver.deliver(anchor, parent_hash);
    let mut cancelled = Box::pin(driver.advance(ticket, &[]));
    assert!(
        cancelled
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending(),
        "the first attempt is in flight"
    );
    drop(cancelled);
    assert_eq!(
        driver.completion(anchor),
        Some(HeadCompletion::Abandoned),
        "the abandoned attempt published its own end"
    );
    assert_eq!(
        driver.last_report(),
        None,
        "the abandoned attempt settled nothing, so it left no report"
    );

    stub.release_bal();
    let retried = settled(&driver.advance(ticket, &[]).await);
    assert_head_reports(&retried, &driver, anchor);
}

/// An out-of-band apply on a released claim stores the report of the settle it
/// ran, on the fresh private attempt epoch, with the public ticket unchanged.
#[tokio::test]
async fn an_out_of_band_apply_stores_the_report_it_settled_with() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let parent_hash = B256::new([0xab; 32]);
    let child_hash = B256::new([0xac; 32]);
    stub.serve_bal_at(parent_hash, bal_bytes(&[]));
    stub.serve_bal_at(child_hash, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);
    let parent = identified(HEIGHT, parent_hash);
    settled(&driver.advance(driver.deliver(parent, genesis()), &[]).await);

    let anchor = identified(HEIGHT + 1, child_hash);
    let ticket = driver.deliver(anchor, parent_hash);
    let prepared = PreparedBal::prepare(&provider, anchor).await;
    stub.defer_bal();
    let mut released = Box::pin(driver.advance(ticket, &[]));
    assert!(
        released
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending(),
        "a claimed preparation owns the head before the release"
    );
    drop(released);
    stub.release_bal();

    let out_of_band = settled(&driver.apply(ticket, &prepared, &[]));
    assert_head_reports(&out_of_band, &driver, anchor);
    assert_eq!(
        driver.deliver(anchor, parent_hash),
        ticket,
        "the out-of-band settle minted no new public delivery"
    );
}

/// A repeat apply at the settled head repeats no transition. If a future change
/// ever lets a settle land on a head that already carries a report, that head
/// must report the new transition's report — never the older one it was holding.
/// The precondition is unreachable today: a settled pose always names the
/// generation the cache has installed, and no public driver call moves that
/// generation, so this pin is green on both sides of the report-assignment fix
/// and guards the shape if the precondition ever becomes reachable.
#[tokio::test]
async fn a_second_settle_at_the_same_head_never_keeps_the_older_report() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let hash = B256::new([0xad; 32]);
    stub.serve_bal_at(hash, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);
    let anchor = identified(HEIGHT, hash);
    let ticket = driver.deliver(anchor, genesis());
    let prepared = PreparedBal::prepare(&provider, anchor).await;
    let first = settled(&driver.apply(ticket, &prepared, &[]));
    assert_head_reports(&first, &driver, anchor);

    match driver.apply(ticket, &prepared, &[]) {
        HeadOutcome::AlreadySettled => {}
        HeadOutcome::Settled(report) => assert!(
            Arc::ptr_eq(
                &report,
                &driver
                    .last_report()
                    .expect("a settle always stores its own report")
            ),
            "a second settle replaces the head's report instead of keeping the older one"
        ),
        other => panic!("a settled head repeats no transition, got {other:?}"),
    }
    assert_eq!(
        driver.installed(),
        Some(anchor),
        "the settled head still names the generation it installed"
    );
}

/// A delivery of an installed anchor with a different parent hash runs no
/// transition, so it is the one head with no report of its own, while the
/// generation keeps the readiness its own settle derived.
#[tokio::test]
async fn an_installed_redelivery_stores_no_report_of_its_own() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let hash = B256::new([0xae; 32]);
    stub.serve_bal_at(hash, bal_bytes(&[]));
    let driver = BalHeadDriver::new(&provider);
    let anchor = identified(HEIGHT, hash);
    settled(&driver.advance(driver.deliver(anchor, genesis()), &[]).await);
    assert!(
        driver.last_report().is_some(),
        "the advance stored its report"
    );

    driver.deliver(anchor, B256::new([0xfe; 32]));
    assert_eq!(
        driver.stats().already_current,
        1,
        "the installed head settled on delivery"
    );
    assert_eq!(
        driver.last_report(),
        None,
        "an already-current delivery ran no transition, so it has no report"
    );
    assert_eq!(
        driver.snapshot(anchor).readiness,
        cold_first_head(),
        "the generation keeps the readiness its own settle derived, exactly as \
         before the redelivery"
    );
}

/// Under unbounded retention the installed generation's two report counts are
/// one number: nothing has expired, so the live count is the held count and
/// both are the hub's own `snapshot_len` — before a fold, after one, and after
/// a settle lands in a generation a child already sealed.
#[tokio::test]
async fn an_unbounded_chain_reports_held_equal_to_live_at_every_head() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let max_depth = 1u32;
    let driver = BalHeadDriver::with_config(&provider, max_depth, Retention::Unbounded);
    let hashes: Vec<B256> = (0..5u8)
        .map(|index| B256::new([0xb0 + index; 32]))
        .collect();
    let first_slot = U256::from(1u64);
    stub.serve_at(HEIGHT, address(0x11), first_slot, 1);
    stub.serve_bal_at(hashes[0], bal_bytes(&[]));

    let base = identified(HEIGHT, hashes[0]);
    let base_ticket = driver.deliver(base, genesis());
    let base_report = settled(&driver.advance(base_ticket, &[]).await);
    assert_eq!((base_report.held, base_report.live), (0, 0));
    driver
        .snapshot(base)
        .hub
        .read_storage(address(0x11), first_slot)
        .await
        .expect("the stub answers");

    let mut saw_compaction = false;
    for step in 1..5usize {
        let number = HEIGHT + u64::try_from(step).expect("the step fits");
        let slot = U256::from(10 + u64::try_from(step).expect("the slot fits"));
        let value = 100 + u64::try_from(step).expect("the value fits");
        stub.serve_at(number, address(0x11), slot, value);
        stub.serve_bal_at(
            hashes[step],
            bal_bytes(&[(address(0x11), slot, U256::from(value))]),
        );
        let anchor = identified(number, hashes[step]);
        let ticket = driver.deliver(anchor, hashes[step - 1]);
        let report = settled(&driver.advance(ticket, &[address(0x11)]).await);
        let view = driver.snapshot(anchor);
        assert!(view.readiness.is_ready(), "head {number} settled ready");
        assert_eq!(
            report.held, report.live,
            "head {number}: unbounded retention expires nothing"
        );
        assert_eq!(
            report.held,
            view.hub.snapshot_len(),
            "head {number}: the report counts what the chain holds"
        );
        assert_eq!(
            report.live,
            view.hub.live_len(),
            "head {number}: the report counts what the chain serves"
        );
        saw_compaction = saw_compaction || report.compaction.is_some();
    }
    assert!(saw_compaction, "a threshold of one must fold every link");

    // A settle that lands after a child sealed the generation stays in that
    // handle's late map: it is held and served, so both counts agree there too.
    let sealed = driver.snapshot(identified(HEIGHT + 1, hashes[1])).hub;
    let late_slot = U256::from(77u64);
    stub.serve_at(HEIGHT + 1, address(0x22), late_slot, 5);
    let (held_before, live_before) = (sealed.snapshot_len(), sealed.live_len());
    sealed
        .read_storage(address(0x22), late_slot)
        .await
        .expect("the stub answers");
    let (held_after, live_after) = (sealed.snapshot_len(), sealed.live_len());
    assert_eq!(
        held_after,
        held_before + 1,
        "the settle joined the late map"
    );
    assert_eq!(
        live_after, held_after,
        "a late settle is held and served alike"
    );
    assert_eq!(live_after, live_before + 1);
}

/// The readiness a chain's FIRST head settles with: nothing was installed to
/// carry from, so it settles cold and says so.
fn cold_first_head() -> HeadReadiness {
    HeadReadiness::Cold(HeadCold::Settled {
        lineage: Some(CarryRefusal::NoParent),
        bal: None,
    })
}
