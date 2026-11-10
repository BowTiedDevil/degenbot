//! The accepted-head edge: the pump pokes the canonical-head fact exactly once
//! per header it accepts, carrying the identity the header already had, and
//! never for a header it drops as duplicate/stale.
//!
//! The pump is the ONE producer of head facts in both compositions that can
//! consume a head's storage (the Python engine wrapper and the pure-Rust
//! reference bot both run `EngineDriver` + `BlockPump`), so this edge is where
//! "every canonical head is seen" is either true or not. The pins here are the
//! pump's own answer to that: one poke per accepted header, in header order,
//! after the block clock and with no extra RPC (the identity rides the header
//! the ingestion stream already held).

use super::*;
use degenbot_core::canonical_head::CanonicalHead;

/// The hash one fixture header carries: distinct per block so an assertion can
/// prove the poke names the header's OWN identity rather than a re-derivation.
fn header_hash(number: u64) -> B256 {
    B256::new([u8::try_from(number % 251).unwrap_or(1); 32])
}

/// A `newHeads` fixture at `number`, with its parent linked to `number - 1` so
/// a consumer's lineage check sees a canonical chain.
fn header(number: u64) -> WsEvent {
    WsEvent::BlockHeader {
        number,
        timestamp: number * 12,
        base_fee_per_gas: Some(1_000_000_000),
        gas_used: 10_000_000,
        gas_limit: 30_000_000,
        hash: header_hash(number),
        parent_hash: header_hash(number.saturating_sub(1)),
    }
}

/// One poke per accepted header, naming that header's exact anchor, in header
/// order, after the block clock — and an identical poke count for a stream that
/// repeats a header (the duplicate is dropped, so neither poke fires again).
#[tokio::test]
async fn the_head_edge_fires_once_per_accepted_header_and_delivers_that_exact_anchor() {
    let (mut pump, sink) = pump_for_test(Some(100));
    let events: Vec<WsEvent> = vec![
        header(101),
        header(102),
        // A repeated header: the stage machine rejects it as stale, so it must
        // produce neither a block-clock poke nor a head fact.
        header(102),
        header(103),
    ];
    pump.run_test_loop(stream::iter(events).boxed(), 100).await;

    let heads = sink.canonical_heads();
    assert_eq!(
        heads,
        vec![
            CanonicalHead::new(101, header_hash(101), header_hash(100)),
            CanonicalHead::new(102, header_hash(102), header_hash(101)),
            CanonicalHead::new(103, header_hash(103), header_hash(102)),
        ],
        "one head fact per ACCEPTED header, each naming its own exact anchor"
    );
    assert_eq!(
        sink.notified.lock().unwrap().len(),
        heads.len(),
        "the head fact and the block clock agree header for header: no accepted \
         header is missing a head and no dropped header earns one"
    );
    assert_eq!(
        sink.header_pokes(),
        vec![
            ("notify", 101),
            ("canonical", 101),
            ("notify", 102),
            ("canonical", 102),
            ("notify", 103),
            ("canonical", 103),
        ],
        "the block clock is never delayed by storage work: notify precedes the \
         head fact on every header"
    );
}

/// The head fact is seated beside the block clock, not on top of it: a stream
/// that carries one accepted header produces exactly one of each, whatever the
/// consumer does with the head.
#[tokio::test]
async fn a_lone_accepted_header_pokes_the_clock_and_the_head_once() {
    let (mut pump, sink) = pump_for_test(Some(100));
    pump.run_test_loop(stream::iter(vec![header(101)]).boxed(), 100)
        .await;
    assert_eq!(
        sink.notified.lock().unwrap().len(),
        1,
        "the block clock is independent of any head consumer"
    );
    assert_eq!(
        sink.canonical_heads().len(),
        1,
        "the poke reaches the control seam whatever the seam does with it"
    );
}
