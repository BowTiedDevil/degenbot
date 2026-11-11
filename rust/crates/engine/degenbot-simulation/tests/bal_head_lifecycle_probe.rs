//! Behavior pins for the canonical-head lifecycle probe's control legs
//! (`examples/shared/head_legs.rs`): the fresh knob's OFF switch, the waiter
//! leg's real wait resolution over the controllably deferred transport, and the
//! percentile math every reported summary is built from.
//!
//! Every test name states the behavior it pins. The waiter leg runs offline over
//! the chain stub, so the pins are deterministic and need no node.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "the scripted chain and its assertions fail loudly on a broken fixture"
)]

use std::cell::Cell;
use std::time::Duration;

use degenbot_simulation::sim::evm2::delta_snapshot::BalHeadDriver;

#[path = "../examples/shared/chain_stub.rs"]
#[expect(dead_code)]
mod chain_stub;

#[path = "../examples/shared/head_legs.rs"]
mod head_legs;

#[path = "../examples/shared/modeled_chain.rs"]
#[expect(dead_code)]
mod modeled_chain;

#[path = "../examples/shared/real_coverage.rs"]
#[expect(dead_code)]
mod real_coverage;

use head_legs::{fresh_heads, percentile, waiter_leg, WaiterChain};

/// The committed real write-coverage capture the waiter leg's heads come from.
const RELEVANCE_CAPTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/bal_real_coverage.json"
);

/// A zero fresh sample size is the leg's OFF switch: the window is empty and the
/// node is never asked for its tip head.
#[tokio::test]
async fn a_zero_fresh_sample_size_selects_no_head_and_asks_for_no_tip() {
    let asked = Cell::new(false);
    let heads = fresh_heads(0, || async {
        asked.set(true);
        Ok(500_u64)
    })
    .await
    .expect("a disabled fresh leg cannot fail: it selects nothing");
    assert!(
        heads.is_empty(),
        "the disabled fresh leg walks no head: {heads:?}"
    );
    assert!(
        !asked.get(),
        "the disabled fresh leg must not ask the node for its tip head"
    );
}

/// A non-zero sample size selects the tip head and its predecessors, oldest
/// first.
#[tokio::test]
async fn a_non_zero_fresh_sample_size_selects_the_tip_and_its_predecessors() {
    let heads = fresh_heads(6, || async { Ok(100_u64) })
        .await
        .expect("the node reported its tip");
    assert_eq!(
        heads,
        (95..=100).collect::<Vec<u64>>(),
        "the window is the last six canonical heads, oldest first"
    );
    let short = fresh_heads(6, || async { Ok(3_u64) })
        .await
        .expect("the node reported its tip");
    assert_eq!(
        short,
        (0..=3).collect::<Vec<u64>>(),
        "a tip younger than the window cannot select a negative height"
    );
}

/// The waiter leg resolves a REAL wait: per head the advance parks on the closed
/// transport gate, the wait registers on that live attempt, and the release
/// settles the head — so the sample reports a wait net of the prepare wall it
/// overlapped, never the prepare cost again.
#[tokio::test(flavor = "multi_thread")]
async fn the_waiter_leg_resolves_a_real_wait_over_the_deferred_transport() {
    let chain = WaiterChain::load(RELEVANCE_CAPTURE, 4).expect("the capture scripts");
    assert_eq!(
        chain.heads().len(),
        4,
        "the leg walks the heads it scripted"
    );
    let relevance = chain.relevance();
    assert!(
        !relevance.is_empty(),
        "the scripted lists apply with the capture's own write addresses"
    );

    let driver = BalHeadDriver::new(&chain.provider());
    let hold = Duration::from_millis(5);
    let samples = waiter_leg(&driver, &chain, hold)
        .await
        .expect("every scripted head parks, joins, and settles");

    assert_eq!(samples.len(), 4, "one sample per scripted head");
    for (index, (sample, head)) in samples.iter().zip(chain.heads()).enumerate() {
        assert_eq!(
            sample.head, head.anchor.number,
            "the sample names the head it delivered"
        );
        // The leg's first head has no installed parent, so its settle is the
        // lineage refusal; every later head carries and answers ready.
        assert_eq!(
            sample.readiness,
            if index == 0 {
                "lineage-refused"
            } else {
                "ready"
            },
            "the wait answered the head's settled generation, not a cold path"
        );
        assert_eq!(
            sample.hold, hold,
            "the sample waited out the parked window the caller asked for"
        );
        assert!(
            sample.wait > hold,
            "the wait covers the parked window and the settle after it: {:?}",
            sample.wait
        );
        assert!(
            sample.wait > sample.prepare,
            "the wait overlapped the prepare wall it subtracts: wait {:?} prepare {:?}",
            sample.wait,
            sample.prepare
        );
        assert_eq!(
            sample.net,
            sample
                .wait
                .checked_sub(sample.prepare)
                .expect("the wait overlapped the prepare wall it subtracts"),
            "the reported resolution is the wait net of the prepare wall"
        );
        assert!(
            !sample.net.is_zero(),
            "the settle and wake after the prepare are real work: {:?}",
            sample.net
        );
    }
    assert_eq!(
        driver.installed(),
        chain.heads().last().map(|head| head.anchor),
        "every scripted head settled: the waits resolved on the settle"
    );
    assert!(
        driver.completion(chain.heads()[0].anchor).is_none(),
        "a superseded head publishes no current completion: its attempt is gone"
    );
}

/// A zero waiter sample count runs no head at all.
#[tokio::test]
async fn a_zero_waiter_sample_count_runs_no_head() {
    let chain = WaiterChain::load(RELEVANCE_CAPTURE, 0).expect("the capture scripts");
    assert!(chain.heads().is_empty(), "a zero limit scripts no head");
    let driver = BalHeadDriver::new(&chain.provider());
    let samples = waiter_leg(&driver, &chain, Duration::from_millis(5))
        .await
        .expect("no head means no wait to resolve");
    assert!(samples.is_empty(), "the disabled leg measures nothing");
    assert_eq!(
        driver.installed(),
        None,
        "nothing settled and nothing moved"
    );
}

/// The percentile every reported summary uses: nearest-rank over the sorted
/// series, and empty for an empty series.
#[test]
fn the_reported_percentile_is_nearest_rank_over_the_sorted_series() {
    let series: Vec<Duration> = (1..=20).map(Duration::from_millis).collect();
    assert_eq!(
        percentile(&series, 50),
        Duration::from_millis(10),
        "even-length p50 is the lower of the two middle samples"
    );
    assert_eq!(
        percentile(&series, 95),
        Duration::from_millis(19),
        "p95 ranks ceil(20 * 0.95) = 19"
    );
    assert_eq!(
        percentile(&series[..], 100),
        Duration::from_millis(20),
        "p100 is the maximum"
    );
    let unsorted = [
        Duration::from_millis(9),
        Duration::from_millis(1),
        Duration::from_millis(4),
    ];
    assert_eq!(
        percentile(&unsorted, 50),
        Duration::from_millis(4),
        "the series is sorted before ranking"
    );
    assert_eq!(
        percentile(&[], 50),
        Duration::ZERO,
        "an empty series has no rank to report"
    );
}
