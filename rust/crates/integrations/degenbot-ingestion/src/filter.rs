//! Backfill `eth_getLogs` filter construction (the gap-backfill transport).

use alloy::rpc::types::{Filter, Topic};

use crate::topics::RELEVANT_TOPICS;

/// Build an Alloy `Filter` for backfill via `eth_getLogs`.
///
/// Uses topic filtering server-side to reduce response size. No address
/// filter — all topic-filtered logs are passed through to the engine.
#[must_use]
pub fn build_backfill_filter(from_block: u64, to_block: u64) -> Filter {
    let mut filter = Filter::new().from_block(from_block).to_block(to_block);

    // Build a single Topic that matches ANY of the relevant event signatures.
    // Alloy's event_signature() overwrites topics[0] on each call, so we must
    // build the OR-list ourselves and set it once.
    let mut topic = Topic::default();
    for sig in &RELEVANT_TOPICS {
        topic = topic.extend(*sig);
    }
    filter.topics[0] = topic;

    filter
}

/// Same filter restricted to a single block (the WS-completeness cross-check
/// callsite's shape).
#[must_use]
pub fn backfill_filter(block: u64) -> Filter {
    build_backfill_filter(block, block)
}
