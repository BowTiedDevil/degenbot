//! Behavior pins for idle-block recency retention on the delta-chain snapshot
//! prototype (`degenbot_simulation::sim::evm2::delta_snapshot`): what keeps an
//! entry alive, what expires it, and what an expired entry does — invisible
//! through every ancestor and compaction, and answered dynamically when read
//! again. The unbounded delta chain is the control: the retention suite drives
//! the same shapes with the policy on and pins where the two must agree
//! (write policy, cold semantics, isolation) and where they must differ
//! (expiry).
//!
//! Every test name states the behavior it pins. The scripted chain
//! (`chain_stub`) answers one storage word per request and one raw list per
//! exact block hash, so a dispatched member count is exactly what took the
//! dynamic path.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "the stub chain and its assertions fail loudly on a broken fixture"
)]

use alloy::primitives::{Address, B256, U256};
use degenbot_simulation::sim::evm2::delta_snapshot::{
    DeltaHub, DeltaReport, DeltaSnapshotCache, Retention,
};
use degenbot_simulation::sim::evm2::read_hub::{CarryOutcome, CarryRefusal};
use degenbot_simulation::sim::ReadAnchor;
use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

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
use modeled_chain::{bal_bytes, read_selected, ChainState, Coverage, StubChain};
use real_coverage::RealCoverage;

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
fn start<'p>(cache: &DeltaSnapshotCache<'p>) -> Arc<DeltaHub<'p>> {
    cache.for_anchor(ReadAnchor::identified(HEIGHT, hash(0)))
}

/// Advance one linked generation carrying `writes`: the child's list is served
/// at its exact hash first (a list the stub does not hold would make the child
/// COLD, which the cold-semantics pins exercise on purpose).
async fn advance<'p>(
    cache: &DeltaSnapshotCache<'p>,
    stub: &ChainStub,
    step: u8,
    writes: &[(Address, U256, U256)],
    relevance: &[Address],
) -> (Arc<DeltaHub<'p>>, DeltaReport) {
    stub.serve_bal_at(hash(step), bal_bytes(writes));
    cache
        .advance_bal(
            ReadAnchor::identified(HEIGHT + u64::from(step), hash(step)),
            hash(step - 1),
            relevance,
        )
        .await
}

/// The value of scenario identity `x` — one slot of one account.
fn x() -> (Address, U256) {
    (address(0x11), U256::from(1u64))
}

/// A candidate read of `x` refreshes its recency, so the entry lives a full
/// new idle window after the read — where an unrefreshed entry would have
/// expired.
#[tokio::test]
async fn a_candidate_read_refreshes_recency_and_keeps_the_entry_past_its_original_idle_window() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, x().0, x().1, 1);
    let cache = DeltaSnapshotCache::with_retention(&provider, Retention::IdleBlocks(2));
    let hub = start(&cache);
    assert_eq!(
        hub.read_storage(x().0, x().1).await.unwrap(),
        U256::from(1u64),
        "the first read settles x=1"
    );
    stub.clear();

    // Used at block 100 only: visible at 101 and 102 (u + n), gone at 103.
    let (hub, _) = advance(&cache, &stub, 1, &[], &[]).await;
    assert_eq!(
        hub.snapshot_value(x().0, x().1),
        Some(U256::from(1u64)),
        "one idle block is inside the window"
    );
    let (hub, _) = advance(&cache, &stub, 2, &[], &[]).await;
    assert_eq!(
        hub.snapshot_value(x().0, x().1),
        Some(U256::from(1u64)),
        "the window boundary u + n still serves"
    );
    // The candidate reads it again at 102: recency refreshes to 102.
    assert_eq!(
        hub.read_storage(x().0, x().1).await.unwrap(),
        U256::from(1u64),
        "the carried value answers the refresh read"
    );

    // The refreshed entry lives a full new window — 103 and 104 — where the
    // unrefreshed one would have died at 103, and expires at 105.
    let (hub, _) = advance(&cache, &stub, 3, &[], &[]).await;
    assert_eq!(
        hub.snapshot_value(x().0, x().1),
        Some(U256::from(1u64)),
        "the refresh at 102 kept it alive"
    );
    let (hub, _) = advance(&cache, &stub, 4, &[], &[]).await;
    assert_eq!(
        hub.snapshot_value(x().0, x().1),
        Some(U256::from(1u64)),
        "the refreshed window reaches u + n = 104"
    );
    let (hub, _) = advance(&cache, &stub, 5, &[], &[]).await;
    assert_eq!(
        hub.snapshot_value(x().0, x().1),
        None,
        "the refreshed window closed at 105"
    );
    assert_eq!(
        stub.storage_members(),
        0,
        "every read so far was a chain hit, never a dynamic fetch"
    );
}

/// A snapshot lookup is not candidate use: peeping at a value refreshes
/// nothing, so the entry expires on its original schedule.
#[tokio::test]
async fn a_snapshot_lookup_does_not_refresh_recency_only_a_candidate_read_does() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, x().0, x().1, 1);
    let cache = DeltaSnapshotCache::with_retention(&provider, Retention::IdleBlocks(2));
    let hub = start(&cache);
    hub.read_storage(x().0, x().1).await.unwrap();

    // Lookups at 101 and 102 answer but do not refresh.
    let (hub, _) = advance(&cache, &stub, 1, &[], &[]).await;
    assert_eq!(hub.snapshot_value(x().0, x().1), Some(U256::from(1u64)));
    let (hub, _) = advance(&cache, &stub, 2, &[], &[]).await;
    assert_eq!(hub.snapshot_value(x().0, x().1), Some(U256::from(1u64)));
    // Had those lookups counted as use, 103 would still serve — it must not.
    let (hub, _) = advance(&cache, &stub, 3, &[], &[]).await;
    assert_eq!(
        hub.snapshot_value(x().0, x().1),
        None,
        "the lookups did not extend the window"
    );
}

/// A BAL write is not candidate use: an overwritten entry still expires on its
/// original schedule, however often the list rewrites it.
#[tokio::test]
async fn a_bal_overwrite_does_not_refresh_recency_so_a_written_entry_still_expires() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, x().0, x().1, 1);
    let cache = DeltaSnapshotCache::with_retention(&provider, Retention::IdleBlocks(2));
    let hub = start(&cache);
    assert_eq!(
        hub.read_storage(x().0, x().1).await.unwrap(),
        U256::from(1u64)
    );
    stub.clear();

    // The list rewrites x at 101 and 102 — each write lands (a served key
    // takes the write) but none of them counts as use.
    let (hub, report) = advance(&cache, &stub, 1, &[(x().0, x().1, U256::from(7u64))], &[]).await;
    assert_eq!(report.writes, vec![(x().0, x().1)]);
    assert_eq!(report.writes_overwritten, 1);
    assert_eq!(
        hub.snapshot_value(x().0, x().1),
        Some(U256::from(7u64)),
        "the write replaced the carried value"
    );
    let (hub, report) = advance(&cache, &stub, 2, &[(x().0, x().1, U256::from(8u64))], &[]).await;
    assert_eq!(report.writes_overwritten, 1);
    assert_eq!(hub.snapshot_value(x().0, x().1), Some(U256::from(8u64)));

    // Read at 100 only: the entry expires at 103 despite both writes.
    let (hub, _) = advance(&cache, &stub, 3, &[], &[]).await;
    assert_eq!(
        hub.snapshot_value(x().0, x().1),
        None,
        "the writes kept no recency"
    );
    assert_eq!(
        stub.storage_members(),
        0,
        "nothing read since 100, so nothing fetched"
    );
}

/// A BAL-only entry — seeded because its address was relevant, never read —
/// expires after its idle window, while a candidate read of a sibling entry
/// refreshes that one's window.
#[tokio::test]
async fn a_bal_only_entry_expires_after_its_idle_window_unless_a_candidate_read_refreshes_it() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let account = address(0x22);
    let (y, z) = ((account, U256::from(9u64)), (account, U256::from(10u64)));
    let cache = DeltaSnapshotCache::with_retention(&provider, Retention::IdleBlocks(2));
    let _ = start(&cache);

    // Both identities seed at 101 because their address is relevant — births,
    // not uses.
    let (hub, report) = advance(
        &cache,
        &stub,
        1,
        &[(y.0, y.1, U256::from(3u64)), (z.0, z.1, U256::from(4u64))],
        &[account],
    )
    .await;
    assert_eq!(report.writes, vec![y, z]);
    assert_eq!(report.writes_overwritten, 0, "nothing was held before");
    assert_eq!(report.writes_skipped, 0);
    assert_eq!(hub.snapshot_value(y.0, y.1), Some(U256::from(3u64)));
    assert_eq!(hub.snapshot_value(z.0, z.1), Some(U256::from(4u64)));

    // A candidate reads z at 102 — a chain hit that refreshes z alone.
    let (hub, _) = advance(&cache, &stub, 2, &[], &[]).await;
    assert_eq!(hub.read_storage(z.0, z.1).await.unwrap(), U256::from(4u64));
    assert_eq!(stub.storage_members(), 0, "the refresh read was a hit");
    let (hub, _) = advance(&cache, &stub, 3, &[], &[]).await;
    assert_eq!(
        hub.snapshot_value(y.0, y.1),
        Some(U256::from(3u64)),
        "y is at its boundary u + n"
    );
    assert_eq!(hub.snapshot_value(z.0, z.1), Some(U256::from(4u64)));

    // y (never read) expires at 104; z (read at 102) lives one window longer.
    let (hub, _) = advance(&cache, &stub, 4, &[], &[]).await;
    assert_eq!(
        hub.snapshot_value(y.0, y.1),
        None,
        "the unread BAL-only entry expired"
    );
    assert_eq!(
        hub.snapshot_value(z.0, z.1),
        Some(U256::from(4u64)),
        "the read refreshed z past y's schedule"
    );
    let (hub, _) = advance(&cache, &stub, 5, &[], &[]).await;
    assert_eq!(
        hub.snapshot_value(z.0, z.1),
        None,
        "z expires one window after its own last use"
    );
}

/// The write rule at the retention boundary: a write to a still-served key
/// overwrites whatever `relevance` says, while a write to an expired key is a
/// write to an UNKNOWN slot — left unknown when its address is irrelevant, and
/// seeding a fresh idle window when relevant.
#[tokio::test]
async fn a_write_overwrites_a_served_key_regardless_of_relevance_and_re_seeds_an_expired_key_only_when_relevant(
) {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, x().0, x().1, 1);
    let other = address(0x33);
    let cache = DeltaSnapshotCache::with_retention(&provider, Retention::IdleBlocks(2));
    let hub = start(&cache);
    hub.read_storage(x().0, x().1).await.unwrap();

    // Served (age 1 and 2): the writes land even though x's address is
    // outside the relevance set.
    let (hub, report) = advance(
        &cache,
        &stub,
        1,
        &[(x().0, x().1, U256::from(7u64))],
        &[other],
    )
    .await;
    assert_eq!(report.writes, vec![(x().0, x().1)]);
    assert_eq!(report.writes_overwritten, 1);
    assert_eq!(report.writes_skipped, 0);
    assert_eq!(hub.snapshot_value(x().0, x().1), Some(U256::from(7u64)));
    let (hub, report) = advance(
        &cache,
        &stub,
        2,
        &[(x().0, x().1, U256::from(8u64))],
        &[other],
    )
    .await;
    assert_eq!(report.writes_overwritten, 1);
    assert_eq!(hub.snapshot_value(x().0, x().1), Some(U256::from(8u64)));

    // Expired (age 3): the write is a write to an unknown slot, and x's
    // address is irrelevant — it stays unknown and counted.
    let (hub, report) = advance(
        &cache,
        &stub,
        3,
        &[(x().0, x().1, U256::from(9u64))],
        &[other],
    )
    .await;
    assert_eq!(report.writes, Vec::<(Address, U256)>::new());
    assert_eq!(report.writes_skipped, 1);
    assert_eq!(hub.snapshot_value(x().0, x().1), None, "expired is unknown");

    // Still expired (age 4), but now the address is relevant: the write seeds
    // a fresh entry with a fresh idle window.
    let (hub, report) = advance(
        &cache,
        &stub,
        4,
        &[(x().0, x().1, U256::from(10u64))],
        &[x().0],
    )
    .await;
    assert_eq!(report.writes, vec![(x().0, x().1)]);
    assert_eq!(
        report.writes_overwritten, 0,
        "the re-seed took the seeding path, not the overwrite path"
    );
    assert_eq!(report.writes_skipped, 0);
    assert_eq!(
        hub.snapshot_value(x().0, x().1),
        Some(U256::from(10u64)),
        "the re-seeded value is served"
    );
    let (hub, _) = advance(&cache, &stub, 5, &[], &[]).await;
    assert_eq!(hub.snapshot_value(x().0, x().1), Some(U256::from(10u64)));
    let (hub, _) = advance(&cache, &stub, 6, &[], &[]).await;
    assert_eq!(
        hub.snapshot_value(x().0, x().1),
        Some(U256::from(10u64)),
        "the re-seed lives u + n"
    );
    let (hub, _) = advance(&cache, &stub, 7, &[], &[]).await;
    assert_eq!(
        hub.snapshot_value(x().0, x().1),
        None,
        "the re-seeded window closes on its own schedule"
    );
}

/// An expired entry is invisible through every ancestor, so a read of it takes
/// the dynamic path and gets the chain's CURRENT word — while the older handle
/// that used it keeps its own frozen view.
#[tokio::test]
async fn an_expired_entry_is_invisible_through_ancestors_and_the_old_reader_keeps_its_own_view() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, x().0, x().1, 1);
    stub.serve_at(HEIGHT + 3, x().0, x().1, 5);
    let cache = DeltaSnapshotCache::with_retention(&provider, Retention::IdleBlocks(2));
    let first = start(&cache);
    first.read_storage(x().0, x().1).await.unwrap();
    stub.clear();

    // The value sits in generation 100's node and is served through two
    // generations of ancestors...
    let (hub, _) = advance(&cache, &stub, 1, &[], &[]).await;
    assert_eq!(hub.snapshot_value(x().0, x().1), Some(U256::from(1u64)));
    let (hub, _) = advance(&cache, &stub, 2, &[], &[]).await;
    assert_eq!(hub.snapshot_value(x().0, x().1), Some(U256::from(1u64)));
    // ...and is invisible from the next one on, ancestor or not.
    let (hub, _) = advance(&cache, &stub, 3, &[], &[]).await;
    assert_eq!(
        hub.snapshot_value(x().0, x().1),
        None,
        "the ancestor's value is expired for this generation"
    );
    let value = hub.read_storage(x().0, x().1).await.unwrap();
    assert_eq!(
        value,
        U256::from(5u64),
        "the dynamic fallback answers the chain's current word, never the stale 1"
    );
    assert_eq!(stub.storage_members(), 1, "only that read was dynamic");

    // The old reader's view is its own anchor's: the value it used is still
    // served there, from its own chain, with no fetch.
    assert_eq!(first.snapshot_value(x().0, x().1), Some(U256::from(1u64)));
    assert_eq!(
        first.read_storage(x().0, x().1).await.unwrap(),
        U256::from(1u64)
    );
    assert_eq!(stub.storage_members(), 1, "the old reader fetched nothing");
}

/// A re-read of an expired entry settles a fresh entry with a fresh idle
/// window: the fallback is dynamic each time the window closes, and the old
/// value never resurrects.
#[tokio::test]
async fn a_re_read_of_an_expired_entry_falls_back_dynamically_and_settles_a_fresh_idle_window() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, x().0, x().1, 1);
    stub.serve_at(HEIGHT + 3, x().0, x().1, 5);
    stub.serve_at(HEIGHT + 7, x().0, x().1, 9);
    let cache = DeltaSnapshotCache::with_retention(&provider, Retention::IdleBlocks(2));
    let hub = start(&cache);
    assert_eq!(
        hub.read_storage(x().0, x().1).await.unwrap(),
        U256::from(1u64)
    );
    stub.clear();

    let (hub, _) = advance(&cache, &stub, 3, &[], &[]).await;
    assert_eq!(
        hub.read_storage(x().0, x().1).await.unwrap(),
        U256::from(5u64),
        "the first re-read fetches the current word"
    );
    assert_eq!(stub.storage_members(), 1);

    // The settled 5 answers the next window from the chain — no fetch (that
    // read is itself a use, so the window now runs from 104).
    let (hub, _) = advance(&cache, &stub, 4, &[], &[]).await;
    assert_eq!(
        hub.read_storage(x().0, x().1).await.unwrap(),
        U256::from(5u64)
    );
    let (hub, _) = advance(&cache, &stub, 5, &[], &[]).await;
    assert_eq!(hub.snapshot_value(x().0, x().1), Some(U256::from(5u64)));
    let (hub, _) = advance(&cache, &stub, 6, &[], &[]).await;
    assert_eq!(
        hub.snapshot_value(x().0, x().1),
        Some(U256::from(5u64)),
        "the refreshed window reaches 106"
    );
    assert_eq!(stub.storage_members(), 1, "the whole window was carried");

    // The window closes at 107 and the next read fetches again — the answer
    // moves with the chain (9), never back to 5 or 1.
    let (hub, _) = advance(&cache, &stub, 7, &[], &[]).await;
    assert_eq!(hub.snapshot_value(x().0, x().1), None);
    assert_eq!(
        hub.read_storage(x().0, x().1).await.unwrap(),
        U256::from(9u64)
    );
    assert_eq!(
        stub.storage_members(),
        2,
        "each closed window costs one fetch"
    );
}

/// Idle entries expire across many generations at their own schedules: the
/// held count keeps every identity, the live count follows the last-use clock
/// of each entry independently.
#[tokio::test]
async fn idle_entries_expire_across_many_generations_at_their_own_schedules() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    let (a, b, c, d) = (
        (address(0x11), U256::from(1u64)),
        (address(0x22), U256::from(9u64)),
        (address(0x11), U256::from(2u64)),
        (address(0x11), U256::from(3u64)),
    );
    stub.serve_at(HEIGHT, a.0, a.1, 1);
    stub.serve_at(HEIGHT + 2, c.0, c.1, 7);
    let cache = DeltaSnapshotCache::with_retention(&provider, Retention::IdleBlocks(3));
    let hub = start(&cache);
    assert_eq!(hub.read_storage(a.0, a.1).await.unwrap(), U256::from(1u64));
    assert_eq!(hub.live_len(), 1);

    let (hub, _) = advance(&cache, &stub, 1, &[(b.0, b.1, U256::from(2u64))], &[b.0]).await;
    assert_eq!(hub.live_len(), 2, "a (age 1) and the seeded b");
    let (hub, _) = advance(&cache, &stub, 2, &[], &[]).await;
    assert_eq!(hub.read_storage(c.0, c.1).await.unwrap(), U256::from(7u64));
    assert_eq!(hub.live_len(), 3);
    let (hub, _) = advance(&cache, &stub, 3, &[(d.0, d.1, U256::from(4u64))], &[d.0]).await;
    assert_eq!(
        hub.live_len(),
        4,
        "a is at its boundary u + n = 103 and still live"
    );

    // Each entry expires one idle window after its own last use: a (100), b
    // (101), c (102), d (103) — while the held count keeps all four.
    let (hub, _) = advance(&cache, &stub, 4, &[], &[]).await;
    assert_eq!(hub.snapshot_value(a.0, a.1), None);
    assert_eq!(hub.live_len(), 3);
    assert_eq!(
        hub.snapshot_len(),
        4,
        "held identities do not shrink by expiry"
    );
    let (hub, _) = advance(&cache, &stub, 5, &[], &[]).await;
    assert_eq!(hub.snapshot_value(b.0, b.1), None);
    assert_eq!(hub.live_len(), 2);
    let (hub, _) = advance(&cache, &stub, 6, &[], &[]).await;
    assert_eq!(hub.snapshot_value(c.0, c.1), None);
    assert_eq!(hub.live_len(), 1);
    let (hub, _) = advance(&cache, &stub, 7, &[], &[]).await;
    assert_eq!(hub.snapshot_value(d.0, d.1), None);
    assert_eq!(hub.live_len(), 0);
    assert_eq!(hub.snapshot_len(), 4);
}

/// A compaction fold preserves every surviving entry's recency — the fold is
/// not a use and gives no entry a new life — drops expired values together
/// with their metadata, and leaves older readers' chains untouched.
#[tokio::test]
async fn compaction_preserves_recency_for_survivors_and_drops_expired_values_and_metadata() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, x().0, x().1, 1);
    stub.serve_at(HEIGHT + 2, address(0x11), U256::from(2u64), 7);
    let cache =
        DeltaSnapshotCache::with_max_depth_and_retention(&provider, 2, Retention::IdleBlocks(2));
    let first = start(&cache);
    first.read_storage(x().0, x().1).await.unwrap();
    let y = (address(0x11), U256::from(2u64));
    let _ = advance(&cache, &stub, 1, &[], &[]).await;
    let (hub, _) = advance(&cache, &stub, 2, &[], &[]).await;
    assert_eq!(hub.read_storage(y.0, y.1).await.unwrap(), U256::from(7u64));
    let second = Arc::clone(&hub);
    let (hub, _) = advance(&cache, &stub, 3, &[], &[]).await;
    assert!(
        hub.chain_depth() <= 3,
        "the walk stays within the threshold: {}",
        hub.chain_depth()
    );

    // The next link folds the chain at generation 103: x (used at 100) is
    // expired there and drops, y (used at 102) survives with its recency.
    let (hub, report) = advance(&cache, &stub, 4, &[], &[]).await;
    let compaction = report.compaction.expect("the fourth link folds the chain");
    assert!(compaction.nodes_merged > 1);
    assert_eq!(compaction.dropped, 1, "exactly the expired entry dropped");
    assert_eq!(compaction.entries, 1, "the survivor kept its value");
    assert_eq!(
        hub.snapshot_len(),
        1,
        "the dropped value is gone from the chain, not just invisible"
    );
    assert_eq!(hub.snapshot_value(x().0, x().1), None);
    assert_eq!(
        hub.snapshot_value(y.0, y.1),
        Some(U256::from(7u64)),
        "the survivor's value carries through the fold"
    );

    // The fold stamped nothing: y (used at 102) expires at 105 as it would
    // have without the fold.
    let (hub, _) = advance(&cache, &stub, 5, &[], &[]).await;
    assert_eq!(
        hub.snapshot_value(y.0, y.1),
        None,
        "the fold gave the survivor no new life"
    );
    assert_eq!(hub.live_len(), 0);

    // Older readers keep their own chains: the values they used are still
    // served at their anchors, untouched by the fold.
    assert_eq!(first.snapshot_value(x().0, x().1), Some(U256::from(1u64)));
    assert_eq!(second.snapshot_value(x().0, x().1), Some(U256::from(1u64)));
    assert_eq!(second.snapshot_value(y.0, y.1), Some(U256::from(7u64)));
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

/// One cold case: a generation whose list cannot be read — or whose lineage
/// refuses — serves NO parent values even when they are live under retention.
/// A refused lineage with a decodable list still applies the child's own
/// final write; an unreadable list applies nothing at all.
async fn cold_case(skip: Option<ColdSkip>) {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, x().0, x().1, 1);
    let cache = DeltaSnapshotCache::with_retention(&provider, Retention::IdleBlocks(8));
    let hub = start(&cache);
    hub.read_storage(x().0, x().1).await.unwrap();
    stub.clear();

    match &skip {
        None => stub.serve_bal_at(hash(1), bal_bytes(&[(x().0, x().1, U256::from(3u64))])),
        Some(ColdSkip::Missing) => {}
        Some(ColdSkip::Null) => stub.set_bal_null(true),
        Some(ColdSkip::Unsupported) => stub.set_bal_unsupported(true),
        Some(ColdSkip::Malformed) => stub.serve_bal_at(hash(1), vec![0xff, 0x00]),
    }
    let parent_hash = if skip.is_none() {
        B256::new([0xcc; 32])
    } else {
        hash(0)
    };
    let (hub, report) = cache
        .advance_bal(
            ReadAnchor::identified(HEIGHT + 1, hash(1)),
            parent_hash,
            &[x().0],
        )
        .await;
    if skip.is_none() {
        assert_eq!(
            report.outcome,
            CarryOutcome::Cold {
                lineage: Some(CarryRefusal::ParentHashMismatch),
                bal: None
            },
            "a refused lineage with a decodable list is cold"
        );
        assert_eq!(report.writes, vec![(x().0, x().1)]);
        assert_eq!(
            hub.snapshot_value(x().0, x().1),
            Some(U256::from(3u64)),
            "the child's own final write seeds even on a cold child"
        );
    } else {
        assert!(
            matches!(
                report.outcome,
                CarryOutcome::Cold {
                    lineage: None,
                    bal: Some(_)
                }
            ),
            "an unreadable list is cold: {:?}",
            report.outcome
        );
        assert!(
            report.writes.is_empty(),
            "an unreadable list applies no final writes"
        );
        assert_eq!(
            hub.snapshot_value(x().0, x().1),
            None,
            "and nothing seeds without a decoded list"
        );
    }
}

/// A missing, a null, an unsupported-method, and a malformed list — and a
/// refused lineage — keep the cold semantics unchanged under retention: no
/// parent value is served, and the child's own write still seeds.
#[tokio::test]
async fn cold_children_and_refused_lineage_keep_the_cold_semantics_under_retention() {
    cold_case(None).await;
    for skip in [
        ColdSkip::Missing,
        ColdSkip::Null,
        ColdSkip::Unsupported,
        ColdSkip::Malformed,
    ] {
        cold_case(Some(skip)).await;
    }
}

/// A zero-write list advances under retention: live entries carry, idle ones
/// retire on their own schedules, and the report applies nothing.
#[tokio::test]
async fn a_zero_write_list_carries_live_entries_and_retires_idle_ones() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, x().0, x().1, 1);
    let cache = DeltaSnapshotCache::with_retention(&provider, Retention::IdleBlocks(2));
    let hub = start(&cache);
    hub.read_storage(x().0, x().1).await.unwrap();
    stub.clear();

    let (hub, report) = advance(&cache, &stub, 1, &[], &[]).await;
    assert_eq!(report.outcome, CarryOutcome::Carried { carried_values: 1 });
    assert!(report.writes.is_empty(), "zero writes applied");
    assert_eq!(hub.snapshot_value(x().0, x().1), Some(U256::from(1u64)));
    let (hub, _) = advance(&cache, &stub, 2, &[], &[]).await;
    assert_eq!(hub.snapshot_value(x().0, x().1), Some(U256::from(1u64)));
    let (hub, _) = advance(&cache, &stub, 3, &[], &[]).await;
    assert_eq!(
        hub.snapshot_value(x().0, x().1),
        None,
        "the idle entry retired despite the carries"
    );
    assert_eq!(stub.storage_members(), 0, "carries cost no fetch");
}

/// A same-anchor retention advance is idempotent: the installed generation
/// comes back, no BAL work repeats, and no recency moves.
#[tokio::test]
async fn a_same_anchor_retention_advance_is_idempotent_and_refreshes_no_recency() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, x().0, x().1, 1);
    let cache = DeltaSnapshotCache::with_retention(&provider, Retention::IdleBlocks(2));
    let hub = start(&cache);
    hub.read_storage(x().0, x().1).await.unwrap();
    stub.clear();

    let (hub, report) = advance(&cache, &stub, 1, &[], &[]).await;
    assert_eq!(report.outcome, CarryOutcome::Carried { carried_values: 1 });
    assert_eq!(stub.bal_requests(), 1, "one BAL fetch");
    let (again, report) = cache
        .advance_bal(ReadAnchor::identified(HEIGHT + 1, hash(1)), hash(0), &[])
        .await;
    assert!(
        Arc::ptr_eq(&hub, &again),
        "the installed generation comes back"
    );
    assert_eq!(report.outcome, CarryOutcome::AlreadyCurrent);
    assert_eq!(
        stub.bal_requests(),
        1,
        "the idempotent call repeated no BAL work"
    );
    let via_for_anchor = cache.for_anchor(ReadAnchor::identified(HEIGHT + 1, hash(1)));
    assert!(Arc::ptr_eq(&hub, &via_for_anchor));

    // The repeat moved no clock: x (used at 100) expires at 103 as before.
    let (hub, _) = advance(&cache, &stub, 2, &[], &[]).await;
    assert_eq!(hub.snapshot_value(x().0, x().1), Some(U256::from(1u64)));
    let (hub, _) = advance(&cache, &stub, 3, &[], &[]).await;
    assert_eq!(
        hub.snapshot_value(x().0, x().1),
        None,
        "idempotence refreshed nothing"
    );
}

/// One corpus replay: every captured block advanced at its exact hash with its
/// real parent linkage and its real write identities, the candidate read
/// stream driven by the deterministic read script, and EVERY resolved value at
/// EVERY anchor compared against an independent cold read of the chain — with
/// the served values required to match exactly.
async fn assert_corpus_matches_cold_reads(real: &RealCoverage, idle: u64) -> usize {
    let keys: BTreeSet<(Address, U256)> = (real.first()..=real.last())
        .flat_map(|number| real.writes_at(number).iter().copied())
        .collect();
    assert!(!keys.is_empty(), "the capture wrote some identity");
    let chain = ChainState::build_coverage(&keys, real.first(), real.last(), real, Coverage::Real)
        .expect("a chain over the captured stretch");
    let stub = StubChain {
        chain: Arc::new(chain),
        plant: Arc::new(AtomicBool::new(false)),
    };
    let provider = stub.provider();
    let cache = DeltaSnapshotCache::with_retention(&provider, Retention::IdleBlocks(idle));
    let relevance: Vec<Address> = keys.iter().map(|(address, _)| *address).collect();
    let key_vec: Vec<(Address, U256)> = keys.iter().copied().collect();
    let mut mismatches = Vec::new();
    let mut comparisons = 0usize;

    for number in real.first()..=real.last() {
        let anchor =
            ReadAnchor::identified(number, real.hash(number).expect("the capture names hashes"));
        let hub = if number == real.first() {
            cache.for_anchor(anchor)
        } else {
            cache
                .advance_bal(
                    anchor,
                    real.parent_hash(number).expect("the capture links"),
                    &relevance,
                )
                .await
                .0
        };
        // The candidate read stream: deterministic and partial, so entries go
        // quiet and expire between reads.
        for &(address, slot) in &key_vec {
            if read_selected(number, address, slot) {
                let served = hub
                    .read_storage(address, slot)
                    .await
                    .expect("the modeled chain answers");
                let cold = provider
                    .get_storage_batch_results_at(&[(address, slot)], number)
                    .await
                    .expect("the cold batch answers");
                let cold = cold[0].clone().expect("the cold member answers");
                comparisons = comparisons.saturating_add(1);
                if served != cold {
                    mismatches.push(format!(
                        "block {number} {address}/{slot}: read {served} vs cold {cold}"
                    ));
                }
            }
        }
        // Every identity at every anchor: whatever the snapshot serves must be
        // the chain's word there (an unknown entry is not a mismatch — its read
        // would take the cold path).
        let cold = provider
            .get_storage_batch_results_at(&key_vec, number)
            .await
            .expect("the cold batch answers");
        for (index, &(address, slot)) in key_vec.iter().enumerate() {
            let cold = cold[index].clone().expect("the cold member answers");
            comparisons = comparisons.saturating_add(1);
            if let Some(served) = hub.snapshot_value(address, slot) {
                if served != cold {
                    mismatches.push(format!(
                        "block {number} {address}/{slot}: snapshot {served} vs cold {cold}"
                    ));
                }
            }
        }
    }
    assert!(
        mismatches.is_empty(),
        "idle={idle}: {} mismatches over {} comparisons: {:?}",
        mismatches.len(),
        comparisons,
        mismatches
    );
    comparisons
}

/// Both committed real captures replay through the retention delta at several
/// thresholds with zero mismatches against independent cold reads: every
/// served value at every tested anchor is the chain's word there.
#[tokio::test]
async fn both_real_fixtures_match_independent_cold_reads_at_every_anchor_with_retention() {
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
        for idle in [0u64, 2, 8] {
            let comparisons = assert_corpus_matches_cold_reads(&real, idle).await;
            assert!(
                comparisons > 1_000,
                "{fixture} idle={idle}: every identity at every anchor: {comparisons}"
            );
        }
    }
}

/// An identity settled again after expiry is still counted once the fold
/// drops its old record: the value lives in the new delta, so the chain's
/// identity count must count it there — held can never fall below live.
#[tokio::test]
async fn an_identity_re_settled_after_expiry_is_still_counted_when_the_fold_drops_its_old_record() {
    let stub = ChainStub::serving();
    let provider = stub.provider();
    stub.serve_at(HEIGHT, x().0, x().1, 1);
    stub.serve_at(HEIGHT + 3, x().0, x().1, 5);
    let cache =
        DeltaSnapshotCache::with_max_depth_and_retention(&provider, 2, Retention::IdleBlocks(2));
    let hub = start(&cache);
    assert_eq!(
        hub.read_storage(x().0, x().1).await.unwrap(),
        U256::from(1u64)
    );
    let _ = advance(&cache, &stub, 1, &[], &[]).await;
    let _ = advance(&cache, &stub, 2, &[], &[]).await;
    let (hub, _) = advance(&cache, &stub, 3, &[], &[]).await;
    // x (used at 100) is expired at 103: the read settles a fresh record.
    assert_eq!(
        hub.read_storage(x().0, x().1).await.unwrap(),
        U256::from(5u64)
    );

    // The next link folds the chain and drops x's OLD record; the fresh one
    // lives in this generation's delta and must stay counted.
    let (hub, _) = advance(&cache, &stub, 4, &[], &[]).await;
    assert_eq!(hub.live_len(), 1, "the re-settled value is still served");
    assert_eq!(
        hub.snapshot_len(),
        1,
        "held counts the surviving identity and can never fall below live"
    );
    assert_eq!(hub.snapshot_value(x().0, x().1), Some(U256::from(5u64)));
}
