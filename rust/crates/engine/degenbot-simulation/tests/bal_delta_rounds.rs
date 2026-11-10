//! Behavior pins for round-coalesced delta reads: a delta generation answers a
//! chain hit locally and registers every miss with its own [`ReadHub`], so
//! duplicate misses coalesce onto one member, distinct misses drain in one
//! storage batch, and a settled answer joins the generation that asked.
//!
//! The suite also pins what a round must NOT do: a refused member and an
//! abandoned round settle nothing, a late settle after an advance stays
//! handle-local, and an expired re-read takes the round path and re-settles
//! with a fresh window. The last pin replays both committed real write-coverage
//! captures through the round seam and compares every answer against an
//! independent cold batch read.
//!
//! Every test name states the behavior it pins. The scripted chain
//! (`chain_stub`) records the wire shape of every dispatched read — single
//! request versus batch member — so "no serial fallback" is a counted fact, and
//! it can hold or refuse a specific request.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "the stub chain and its assertions fail loudly on a broken fixture"
)]

use alloy::primitives::{Address, B256, U256};
use degenbot_simulation::sim::evm2::delta_snapshot::{
    DeltaHub, DeltaSnapshotCache, ReadOrigin, Retention,
};
use degenbot_simulation::sim::evm2::read_hub::ReadHubError;
use degenbot_simulation::sim::ReadAnchor;
use std::collections::BTreeSet;
use std::future::Future;
use std::sync::Arc;
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

use chain_stub::{ChainStub, SERVED};
use modeled_chain::{ChainState, Coverage, StubChain};
use real_coverage::{Key, RealCoverage};

/// The anchor height every scenario chain starts at.
const HEIGHT: u64 = 100;

/// An address for one scenario account.
fn address(tag: u8) -> Address {
    Address::new([tag; 20])
}

/// The block hash generation `step` is keyed by: generation `step` stands at
/// `HEIGHT + step`.
fn hash(step: u8) -> B256 {
    B256::new([0xa0 + step; 32])
}

/// Install the scenario chain's first generation: cold, at `HEIGHT`.
fn start(cache: &DeltaSnapshotCache) -> Arc<DeltaHub> {
    cache.for_anchor(ReadAnchor::identified(HEIGHT, hash(0)))
}

/// Advance one linked generation carrying `writes`.
async fn advance(
    cache: &DeltaSnapshotCache,
    stub: &ChainStub,
    step: u8,
    writes: &[(Address, U256, U256)],
) -> Arc<DeltaHub> {
    stub.serve_bal_at(hash(step), modeled_chain::bal_bytes(writes));
    let (hub, _) = cache
        .advance_bal(
            ReadAnchor::identified(HEIGHT + u64::from(step), hash(step)),
            hash(step - 1),
            &[],
        )
        .await;
    hub
}

/// Scenario identity `x`: one slot of one account.
fn x() -> (Address, U256) {
    (address(0x11), U256::from(1u64))
}

/// Scenario identity `y`.
fn y() -> (Address, U256) {
    (address(0x11), U256::from(2u64))
}

/// A chain hit answers from the generation's own state and dispatches nothing.
#[tokio::test]
async fn a_chain_hit_resolves_from_the_generation_and_dispatches_no_read() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, x().0, x().1, 7);
    let cache = DeltaSnapshotCache::new(&provider);
    let hub = start(&cache);
    assert_eq!(
        hub.read_storage(x().0, x().1).await.unwrap(),
        U256::from(7u64),
        "the first read settles the chain's word"
    );
    stub.clear();

    let read = hub.read_storage_handle(x().0, x().1);
    assert!(read.is_chain_hit(), "the settled value is a chain hit");
    assert_eq!(read.origin(), ReadOrigin::Snapshot);
    assert_eq!(read.await.unwrap(), U256::from(7u64));
    assert_eq!(stub.storage_members(), 0, "a chain hit pays no round trip");
    assert!(hub.drain_round().await.is_empty(), "nothing was queued");
}

/// A miss registers with the generation's hub, drains as ONE batch member, and
/// the answer settles into that generation — not into a single serial read.
#[tokio::test]
async fn a_miss_drains_as_a_batch_member_and_settles_into_its_generation() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let cache = DeltaSnapshotCache::new(&provider);
    let hub = start(&cache);

    let read = hub.read_storage_handle(x().0, x().1);
    assert_eq!(read.origin(), ReadOrigin::Hub, "an unknown identity misses");
    assert_eq!(
        hub.snapshot_value(x().0, x().1),
        None,
        "nothing is guessed before the round settles"
    );

    let report = hub.drain_round().await;
    assert_eq!(report.storage_members, 1, "one distinct miss, one member");
    assert_eq!(
        read.await.unwrap(),
        U256::from(SERVED),
        "the round answers the read"
    );
    assert_eq!(
        hub.snapshot_value(x().0, x().1),
        Some(U256::from(SERVED)),
        "the answer settles into the generation that asked"
    );

    let stats = hub.read_stats();
    assert_eq!(stats.rounds, 1);
    assert_eq!(stats.storage_members, 1);
    assert_eq!(stats.memo_hits, 0);
    assert_eq!(stub.storage_singles(), 0, "no serial storage request");
    assert_eq!(stub.storage_batched(), 1, "one batch member");
}

/// Duplicate misses register one identity: the queue holds them together, both
/// handles are answered, and only one member is dispatched.
#[tokio::test]
async fn duplicate_misses_coalesce_onto_one_dispatched_member() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let cache = DeltaSnapshotCache::new(&provider);
    let hub = start(&cache);

    let first = hub.read_storage_handle(x().0, x().1);
    let second = hub.read_storage_handle(x().0, x().1);
    assert_eq!(first.origin(), ReadOrigin::Hub);
    assert_eq!(second.origin(), ReadOrigin::Hub);

    let report = hub.drain_round().await;
    assert_eq!(report.storage_members, 1, "one distinct identity");
    assert_eq!(stub.storage_members(), 1, "one fetch answered both reads");
    assert_eq!(hub.read_stats().coalesced, 1, "the duplicate joined");
    assert_eq!(first.await.unwrap(), U256::from(SERVED));
    assert_eq!(second.await.unwrap(), U256::from(SERVED));

    // The planted duplicate: the third registration is a hit, and no round is
    // dispatched for it.
    let third = hub.read_storage_handle(x().0, x().1);
    assert_eq!(third.origin(), ReadOrigin::Snapshot);
    assert!(hub.drain_round().await.is_empty());
    assert_eq!(stub.storage_members(), 1, "the duplicate never refetched");
}

/// A planted burst of duplicate reads of one identity proves single-flight: one
/// member is dispatched, and every handle gets the same answer.
#[tokio::test]
async fn a_planted_duplicate_read_of_one_identity_dispatches_exactly_one_member() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let cache = DeltaSnapshotCache::new(&provider);
    let hub = start(&cache);

    let reads: Vec<_> = (0..16)
        .map(|_| hub.read_storage_handle(x().0, x().1))
        .collect();
    assert!(
        reads.iter().all(|read| read.origin() == ReadOrigin::Hub),
        "every planted duplicate is unresolved before its round"
    );
    let report = hub.drain_round().await;
    assert_eq!(
        report.storage_members, 1,
        "sixteen requesters, one dispatched member"
    );
    assert_eq!(stub.storage_members(), 1, "and one fetch on the wire");
    assert_eq!(hub.read_stats().coalesced, 15, "fifteen duplicates joined");
    for read in reads {
        assert_eq!(read.await.unwrap(), U256::from(SERVED));
    }
}

/// A duplicate registered while a round is IN FLIGHT joins that round instead
/// of queueing another member: single-flight holds across the await point.
#[tokio::test]
async fn a_duplicate_registered_while_a_round_is_in_flight_joins_that_single_flight() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.set_slow(true);
    let cache = DeltaSnapshotCache::new(&provider);
    let hub = start(&cache);

    let first = hub.read_storage_handle(x().0, x().1);
    let mut drain = Box::pin(hub.drain_round());
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    assert!(
        drain.as_mut().poll(&mut cx).is_pending(),
        "the round is in flight, not settled"
    );

    let second = hub.read_storage_handle(x().0, x().1);
    assert_eq!(
        second.origin(),
        ReadOrigin::Hub,
        "the value is still unresolved"
    );

    let report = drain.await;
    assert_eq!(
        report.storage_members, 1,
        "the in-flight round kept one member"
    );
    assert_eq!(first.await.unwrap(), U256::from(SERVED));
    assert_eq!(second.await.unwrap(), U256::from(SERVED));
    assert_eq!(hub.read_stats().coalesced, 1);
    assert_eq!(stub.storage_members(), 1);
}

/// Distinct misses share one batch: N identities, one round, N members.
#[tokio::test]
async fn distinct_misses_drain_in_one_storage_batch() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let cache = DeltaSnapshotCache::new(&provider);
    let hub = start(&cache);

    let reads: Vec<_> = (0..5u64)
        .map(|slot| hub.read_storage_handle(address(0x22), U256::from(slot)))
        .collect();
    let report = hub.drain_round().await;
    assert_eq!(report.storage_members, 5, "five distinct identities");
    assert_eq!(stub.storage_singles(), 0, "the batch carries every member");
    assert_eq!(stub.storage_batched(), 5);
    for read in reads {
        assert_eq!(read.await.unwrap(), U256::from(SERVED));
    }
    assert_eq!(hub.read_stats().rounds, 1, "one round served all five");
    assert_eq!(hub.read_stats().coalesced, 0, "no duplicate was planted");
}

/// A refused member fails only its own read and settles nothing; the neighbour
/// in the same packet is answered, and the refused identity is readable again.
#[tokio::test]
async fn a_refused_member_fails_only_its_own_read_and_settles_nothing() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.refuse_at(HEIGHT, x().0, x().1);
    let cache = DeltaSnapshotCache::new(&provider);
    let hub = start(&cache);

    let refused = hub.read_storage_handle(x().0, x().1);
    let answered = hub.read_storage_handle(y().0, y().1);
    let report = hub.drain_round().await;
    assert_eq!(report.storage_members, 2);
    assert_eq!(report.storage_member_errors, 1, "one member was refused");

    match refused.await {
        Err(ReadHubError::Rpc(error)) => {
            assert!(error.to_string().contains("refuses"), "{error}");
        }
        other => panic!("a refused member is that read's own failure, got {other:?}"),
    }
    assert_eq!(
        hub.snapshot_value(x().0, x().1),
        None,
        "a refusal settles nothing into the generation"
    );
    assert_eq!(
        answered.await.unwrap(),
        U256::from(SERVED),
        "the neighbour in the same batch is answered"
    );
    assert_eq!(hub.snapshot_value(y().0, y().1), Some(U256::from(SERVED)));

    // The identity is readable again: a later round retries it normally.
    stub.allow_at(HEIGHT, x().0, x().1);
    let retried = hub.read_storage_handle(x().0, x().1);
    assert_eq!(retried.origin(), ReadOrigin::Hub);
    hub.drain_round().await;
    assert_eq!(retried.await.unwrap(), U256::from(SERVED));
    assert_eq!(hub.snapshot_value(x().0, x().1), Some(U256::from(SERVED)));
}

/// An abandoned round settles nothing and clears its in-flight entry, so the
/// identity is readable again on the next round.
#[tokio::test]
async fn an_abandoned_round_settles_nothing_and_the_read_retries_normally() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.set_hold(true);
    let cache = DeltaSnapshotCache::new(&provider);
    let hub = start(&cache);

    let abandoned = hub.read_storage_handle(x().0, x().1);
    {
        let mut drain = Box::pin(hub.drain_round());
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        assert!(
            drain.as_mut().poll(&mut cx).is_pending(),
            "the round never answers"
        );
        // Dropped mid-flight: the round guard abandons its members.
    }
    assert!(matches!(abandoned.await, Err(ReadHubError::Abandoned)));
    assert_eq!(
        hub.snapshot_value(x().0, x().1),
        None,
        "an abandoned round settles nothing"
    );

    stub.set_hold(false);
    let retried = hub.read_storage_handle(x().0, x().1);
    assert_eq!(retried.origin(), ReadOrigin::Hub);
    hub.drain_round().await;
    assert_eq!(retried.await.unwrap(), U256::from(SERVED));
    assert_eq!(hub.snapshot_value(x().0, x().1), Some(U256::from(SERVED)));
}

/// A settle that lands after a child linked the generation stays handle-local:
/// the parent answers it, the child never sees it, and a read at the child
/// takes its own round.
#[tokio::test]
async fn a_settle_that_lands_after_an_advance_stays_handle_local() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, y().0, y().1, 5);
    stub.serve_at(HEIGHT + 1, y().0, y().1, 9);
    let cache = DeltaSnapshotCache::new(&provider);
    let parent = start(&cache);

    let late = parent.read_storage_handle(y().0, y().1);
    assert_eq!(late.origin(), ReadOrigin::Hub);

    let child = advance(&cache, &stub, 1, &[]).await;
    assert_eq!(
        child.snapshot_len(),
        0,
        "the child linked an empty parent chain"
    );

    parent.drain_round().await;
    assert_eq!(
        late.await.unwrap(),
        U256::from(5u64),
        "the parent's round answers at the parent's anchor"
    );
    assert_eq!(
        parent.snapshot_value(y().0, y().1),
        Some(U256::from(5u64)),
        "the late settle is visible to the handle that asked"
    );
    assert_eq!(
        child.snapshot_value(y().0, y().1),
        None,
        "and invisible to the child that already linked"
    );
    assert_eq!(
        child.snapshot_len(),
        0,
        "the sealed parent node is immutable"
    );

    let child_read = child.read_storage_handle(y().0, y().1);
    assert_eq!(
        child_read.origin(),
        ReadOrigin::Hub,
        "the child fetches its own anchor's word"
    );
    child.drain_round().await;
    assert_eq!(child_read.await.unwrap(), U256::from(9u64));
}

/// Under retention an expired entry is a miss like any other: it takes the
/// round path, answers the chain's current word, and re-settles a fresh window.
#[tokio::test]
async fn an_expired_re_read_takes_the_round_path_and_settles_a_fresh_window() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, x().0, x().1, 3);
    stub.serve_at(HEIGHT + 3, x().0, x().1, 8);
    let cache = DeltaSnapshotCache::with_retention(&provider, Retention::IdleBlocks(2));
    let hub = start(&cache);
    assert_eq!(
        hub.read_storage(x().0, x().1).await.unwrap(),
        U256::from(3u64)
    );
    assert_eq!(stub.storage_members(), 1);

    advance(&cache, &stub, 1, &[]).await;
    advance(&cache, &stub, 2, &[]).await;
    let hub = advance(&cache, &stub, 3, &[]).await;
    assert_eq!(
        hub.snapshot_value(x().0, x().1),
        None,
        "used at 100 with a two-block window, the entry is expired at 103"
    );
    assert_eq!(hub.live_len(), 0);

    let read = hub.read_storage_handle(x().0, x().1);
    assert_eq!(
        read.origin(),
        ReadOrigin::Hub,
        "an expired entry is unknown again"
    );
    hub.drain_round().await;
    assert_eq!(
        read.await.unwrap(),
        U256::from(8u64),
        "the round answers the chain's current word"
    );
    assert_eq!(stub.storage_members(), 2, "one fresh fetch for the re-read");
    assert_eq!(
        hub.snapshot_value(x().0, x().1),
        Some(U256::from(8u64)),
        "the re-read re-settles the entry"
    );
    assert_eq!(hub.live_len(), 1, "with a fresh window");
}

/// Both committed real captures answer every identity through rounds that agree
/// with an independent cold batch read: zero value mismatches, and no serial
/// storage request anywhere.
#[tokio::test]
async fn both_real_fixtures_answer_every_identity_through_rounds_with_zero_mismatches() {
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
            plant: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let provider = stub.provider();
        let cache = DeltaSnapshotCache::new(&provider);
        let relevance: Vec<Address> = keys.iter().map(|(address, _)| *address).collect();
        let key_vec: Vec<Key> = keys.iter().copied().collect();
        let mut mismatches = Vec::new();

        for number in real.first()..=real.last() {
            let anchor = ReadAnchor::identified(
                number,
                real.hash(number).expect("the capture names hashes"),
            );
            let hub = if number == real.first() {
                cache.for_anchor(anchor)
            } else {
                let (hub, _) = cache
                    .advance_bal(
                        anchor,
                        real.parent_hash(number)
                            .expect("the capture links every block"),
                        &relevance,
                    )
                    .await;
                hub
            };

            let reads: Vec<_> = key_vec
                .iter()
                .map(|&(address, slot)| hub.read_storage_handle(address, slot))
                .collect();
            let report = hub.drain_round().await;
            assert_eq!(
                report.storage_members,
                reads.iter().filter(|read| !read.is_chain_hit()).count(),
                "block {number}: every miss, and only a miss, took a member"
            );
            let cold = provider
                .get_storage_batch_results_at(&key_vec, number)
                .await
                .expect("the cold batch answers");
            for (index, read) in reads.into_iter().enumerate() {
                let served = read.await.expect("the round answers every identity");
                let expected = cold[index].clone().expect("the cold member answers");
                if served != expected {
                    mismatches.push(format!(
                        "{} block {number} {}/{}: round {served} vs cold {expected}",
                        fixture, key_vec[index].0, key_vec[index].1
                    ));
                }
            }
        }

        assert!(
            mismatches.is_empty(),
            "{}: {} mismatches: {:?}",
            fixture,
            mismatches.len(),
            &mismatches[..mismatches.len().min(5)]
        );
    }
}
