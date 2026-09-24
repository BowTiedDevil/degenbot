//! The subscription’s Rust-side topic filter.
//!
//! The WS `logs` subscription is UNFILTERED server-side (see the MJXP5Z
//! one-stream handshake — no resubscribe) so the hot pre-filter in the
//! runtime skips lock + decode work for irrelevant logs. `RELEVANT_TOPICS`
//! is the single source of truth for that filter, the backfill filter's
//! server-side OR-list, and the dispatcher's defensive re-check.

use alloy::primitives::B256;
use alloy::rpc::types::Log;
use degenbot_decoders::v2_sync_decoder::V2_SYNC_TOPIC;
use degenbot_decoders::v3_mint_burn_decoder::{V3_BURN_TOPIC, V3_MINT_TOPIC};
use degenbot_decoders::v3_pancakeswap_swap_decoder::V3_PANCAKESWAP_SWAP_TOPIC;
use degenbot_decoders::v3_swap_decoder::V3_SWAP_TOPIC;
use degenbot_decoders::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC;
use degenbot_decoders::v4_swap_decoder::V4_SWAP_TOPIC;

/// Topics we care about — used for in-Rust filtering of incoming logs.
pub const RELEVANT_TOPICS: [B256; 7] = [
    V2_SYNC_TOPIC,
    V3_SWAP_TOPIC,
    V3_PANCAKESWAP_SWAP_TOPIC,
    V3_MINT_TOPIC,
    V3_BURN_TOPIC,
    V4_SWAP_TOPIC,
    V4_MODIFY_LIQUIDITY_TOPIC,
];

/// Fast-path topic match: `topic0` ∈ [`RELEVANT_TOPICS`].
#[must_use]
pub fn is_relevant_topic(topic0: Option<&B256>) -> bool {
    topic0.is_some_and(|t| RELEVANT_TOPICS.contains(t))
}

/// Fast-path test for an incoming log (the hot loop's pre-filter).
#[must_use]
pub fn is_relevant_log(log: &Log) -> bool {
    is_relevant_topic(log.topic0())
}
