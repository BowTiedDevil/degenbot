//! Headless standalone-Rust boot over the `degenbot-ingestion` public surface —
//! NO Python, NO network.
//!
//! Boots the crate's public surface end-to-end over a synthetic fused event
//! stream (what `WsIngestor::subscribe_events` emits on a live WS):
//!
//! 1. A merged `IngestEvent` stream (headers + logs) flows through the
//!    crate's topic filter — irrelevant-topics logs are dropped, relevant
//!    ones are surfaced as `PoolEvent { epoch, log_index, payload }`.
//! 2. The watchdog windows construct + evaluate (windows the runtime's FSM
//!    consumes).
//! 3. The backfill filter builder produces a server-side-filterable
//!    `eth_getLogs` filter for the gap-backfill range.

use std::time::Duration;

use degenbot_ingestion::{
    build_backfill_filter, is_relevant_topic, IngestEvent, PoolEvent, Watchdog, RELEVANT_TOPICS,
};
use futures_util::{stream, FutureExt, StreamExt};

fn synthetic_log(
    topic0: alloy::primitives::B256,
    block: u64,
    log_index: u64,
) -> alloy::rpc::types::Log {
    let inner = alloy::primitives::Log::new_unchecked(
        alloy::primitives::Address::from([0x41u8; 20]),
        vec![topic0],
        alloy::primitives::Bytes::from(vec![0u8; 32]),
    );
    alloy::rpc::types::Log {
        inner,
        block_hash: None,
        block_number: Some(block),
        block_timestamp: None,
        transaction_hash: None,
        transaction_index: None,
        log_index: Some(log_index),
        removed: false,
    }
}

fn synthetic_fused_events() -> Vec<IngestEvent> {
    let relevant = RELEVANT_TOPICS[0]; // V2 Sync
    let irrelevant = alloy::primitives::B256::from([0u8; 32]);

    // Synthetic fused stream (substitute WsIngestor::subscribe_events on a
    // live WS — the surface is identical: BoxStream<'static, IngestEvent>).
    vec![
        IngestEvent::BlockHeader {
            number: 100,
            timestamp: 1_700_000_000,
            base_fee_per_gas: Some(1),
            gas_used: 0,
            gas_limit: 30_000_000,
            hash: alloy::primitives::B256::ZERO,
            parent_hash: alloy::primitives::B256::ZERO,
        },
        IngestEvent::Pool(PoolEvent::from_log(synthetic_log(irrelevant, 100, 0))),
        IngestEvent::Pool(PoolEvent::from_log(synthetic_log(relevant, 100, 1))),
        IngestEvent::Pool(PoolEvent::from_log(synthetic_log(relevant, 100, 2))),
        IngestEvent::BlockHeader {
            number: 101,
            timestamp: 1_700_000_012,
            base_fee_per_gas: Some(1),
            gas_used: 0,
            gas_limit: 30_000_000,
            hash: alloy::primitives::B256::ZERO,
            parent_hash: alloy::primitives::B256::ZERO,
        },
    ]
}

#[test]
fn topic_filter_passes_exactly_the_two_relevant_logs() {
    let events = synthetic_fused_events();

    let relevant_pool_events: Vec<PoolEvent> = stream::iter(events)
        .filter_map(|ev| async move {
            match ev {
                IngestEvent::BlockHeader { number, .. } => {
                    // headers drive the runtime’s block clock
                    tracing::debug!(number, "head");
                    None
                }
                IngestEvent::Pool(pe) => is_relevant_topic(pe.payload.topic0()).then_some(pe),
            }
        })
        .collect()
        .now_or_never()
        .unwrap_or_default();

    assert_eq!(
        relevant_pool_events.len(),
        2,
        "the topic filter must pass exactly the two relevant logs"
    );
    assert_eq!(relevant_pool_events[0].epoch, 100);
    assert_eq!(relevant_pool_events[0].log_index, Some(1));
    assert_eq!(relevant_pool_events[1].log_index, Some(2));
}

#[test]
fn watchdog_windows_construct_with_production_defaults_and_count_alarms() {
    let mut watchdog = Watchdog::new();
    assert_eq!(watchdog.header_staleness, Duration::from_secs(30));
    assert_eq!(watchdog.silence_alarm_count(), 0);
    assert_eq!(watchdog.record_silence_alarm(), 1);
    assert_eq!(watchdog.silence_alarm_count(), 1);
}

#[test]
fn backfill_filter_carries_the_full_relevant_topic_or_list() {
    let filter = build_backfill_filter(101, 120);
    for expected in RELEVANT_TOPICS {
        assert!(
            filter.topics[0].iter().any(|t| *t == expected),
            "filter carries the full relevant-topic OR-list"
        );
    }
}
