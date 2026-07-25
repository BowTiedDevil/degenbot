//! Typed async fetcher wrappers + decode-to-row-input mapping for the
//! Rust-owned pool-updater chunk loop (epic `2SFL6I`, task SBICJJ).
//!
//! These wrap [`degenbot_rpc::provider::LogFetcher::fetch_logs_chunked`]
//! and the `degenbot-decoders` leaves (the `PoolCreated` decoders from
//! [`degenbot_decoders::pool_created_decoder`] and the existing V3/V4
//! liquidity decoders), returning either decoder-native structs (pool
//! creation — mapped to `V*PoolRowInput` by the chunk loop in Task `CKXCOB`)
//! or DB-ready [`LiquidityUpdateEvent`]s (liquidity — the apply step
//! consumes these directly).
//!
//! The pure decode helpers ([`decode_pool_created_log`] /
//! [`decode_v3_liquidity_log`] / [`decode_v4_liquidity_log`]) are exposed so
//! they can be unit-tested WITHOUT a live RPC node (the fetchers themselves
//! need one — round-trip tests for them are integration-only).

use std::collections::HashMap;

use alloy::primitives::{Address, B256, I256};
use alloy::rpc::types::Log;
use degenbot_core::errors::ProviderResult;
use degenbot_db::LiquidityUpdateEvent;
use degenbot_decoders::pool_created_decoder::{
    decode_aerodrome_v2_pool_created_log, decode_v2_pair_created_log, decode_v3_pool_created_log,
    decode_v4_initialize_log, AerodromeV2PoolCreatedEvent, V2PairCreatedEvent, V3PoolCreatedEvent,
    V4InitializeEvent, AERODROME_V2_POOL_CREATED_TOPIC, V2_PAIR_CREATED_TOPIC,
    V3_POOL_CREATED_TOPIC, V4_INITIALIZE_TOPIC,
};
use degenbot_decoders::v3_mint_burn_decoder::{decode_v3_burn_log, decode_v3_mint_log};
use degenbot_decoders::v4_modify_liquidity_decoder::decode_v4_modify_liquidity_log;
use degenbot_rpc::provider::LogFetcher;

use crate::spec::ExchangeSpec;

/// The Uniswap family a pool-creation event belongs to — drives the topic0
/// filter + the decode leaf the fetcher dispatches to.
///
/// Discriminators mirror the Python `kind` strings in
/// `src/degenbot/cli/pool.py` (the `POOL_UPDATER[chain_id, exchange.name]`
/// dispatch keys).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PoolFamily {
    /// Canonical Uniswap V2 `PairCreated` (no `stable` flag). Shared by
    /// `Uniswap` V2, `PancakeSwap` V2, `SushiSwap` V2, `Swapbased` V2.
    V2,
    /// Aerodrome V2 `PoolCreated` (the `stable`-flag variant).
    AerodromeV2,
    /// Uniswap V3 `PoolCreated`. Shared by `PancakeSwap` V3 + `SushiSwap` V3.
    V3,
    /// Uniswap V4 `Initialize` (the V4 `PoolCreated`).
    V4,
}

impl PoolFamily {
    /// The topic0 hash for this family's pool-creation event.
    #[must_use]
    pub const fn topic0(&self) -> B256 {
        match self {
            Self::V2 => V2_PAIR_CREATED_TOPIC,
            Self::AerodromeV2 => AERODROME_V2_POOL_CREATED_TOPIC,
            Self::V3 => V3_POOL_CREATED_TOPIC,
            Self::V4 => V4_INITIALIZE_TOPIC,
        }
    }
}

/// A decoded `PoolCreated`/`Initialize` event, family-tagged.
///
/// The chunk loop (Task `CKXCOB`) maps each variant to the family-specific
/// `V*PoolRowInput` (carrying the `exchange_id`/`fee_denominator`/`kind`
/// context the leaf correctly avoids).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodedPoolCreated {
    /// Canonical V2 `PairCreated` (no `stable` flag).
    V2(V2PairCreatedEvent),
    /// Aerodrome V2 `PoolCreated` (with `stable`).
    AerodromeV2(AerodromeV2PoolCreatedEvent),
    /// V3 `PoolCreated`.
    V3(V3PoolCreatedEvent),
    /// V4 `Initialize`.
    V4(V4InitializeEvent),
}

/// Decode a single `PoolCreated`/`Initialize` log for the given family.
///
/// Pure (no RPC). Returns `None` if the log's topic0 doesn't match the
/// family's signature, the data is malformed, or the indexed fields are
/// out of range (see each decode leaf's reject conditions).
#[must_use]
pub fn decode_pool_created_log(log: &Log, family: PoolFamily) -> Option<DecodedPoolCreated> {
    match family {
        PoolFamily::V2 => decode_v2_pair_created_log(log).map(DecodedPoolCreated::V2),
        PoolFamily::AerodromeV2 => {
            decode_aerodrome_v2_pool_created_log(log).map(DecodedPoolCreated::AerodromeV2)
        }
        PoolFamily::V3 => decode_v3_pool_created_log(log).map(DecodedPoolCreated::V3),
        PoolFamily::V4 => decode_v4_initialize_log(log).map(DecodedPoolCreated::V4),
    }
}

/// Fetch `PoolCreated`/`Initialize` logs for a single factory address across a
/// block range + decode each into a [`DecodedPoolCreated`].
///
/// The `factory_address` is the emitter (the `address` filter on
/// `eth_getLogs`) — for V2/V3 it's the factory contract; for V4 it's the
/// singleton `PoolManager`. The family drives the topic0 + the decode leaf.
///
/// # Errors
///
/// Returns [`ProviderError`] on RPC failure, a malformed filter (bad address
/// / topic), or an invalid block range.
pub async fn fetch_pool_created_logs(
    fetcher: &LogFetcher,
    from_block: u64,
    to_block: u64,
    factory_address: Address,
    family: PoolFamily,
) -> ProviderResult<Vec<DecodedPoolCreated>> {
    let topic0 = family.topic0();
    let logs = fetch_logs(fetcher, from_block, to_block, Some(factory_address), topic0).await?;
    Ok(logs
        .iter()
        .filter_map(|log| decode_pool_created_log(log, family))
        .collect())
}

/// Spec-aware `PoolCreated`/`Initialize` fetch (CKXCOB 3c): like
/// [`fetch_pool_created_logs`] but resolved from an [`ExchangeSpec`] — uses
/// `spec.event_topic` (NOT `spec.family.topic0()`) so Aerodrome V3 (which
/// shares the V3 decode structure but has its own
/// [`AERODROME_V3_POOL_CREATED_TOPIC`]) fetches the correct topic. The chunk
/// loop (`run_pool_update`) calls this per in-scope exchange.
///
/// # Errors
///
/// Returns [`ProviderError`](degenbot_core::errors::ProviderError) on RPC
/// failure or an invalid block range.
pub async fn fetch_pool_created_logs_for_spec(
    fetcher: &LogFetcher,
    from_block: u64,
    to_block: u64,
    spec: &ExchangeSpec,
) -> ProviderResult<Vec<DecodedPoolCreated>> {
    let logs = fetch_logs(
        fetcher,
        from_block,
        to_block,
        Some(spec.factory),
        spec.event_topic,
    )
    .await?;
    Ok(logs
        .iter()
        .filter_map(|log| decode_pool_created_log(log, spec.family))
        .collect())
}

/// Decode a V3 `Mint`/`Burn` log into a DB-ready [`LiquidityUpdateEvent`].
///
/// Pure (no RPC). Mint events carry a positive `liquidity_delta`; Burn events
/// carry a negative one. Zero-amount events are skipped (the Python chunk
/// loop's "Ignore zero-amount events" filter — matched here on the decoded
/// `amount` field rather than the raw-data-offset check, which was asymmetric
/// between Mint [sender word] + Burn [amount word]). Logs missing
/// `block_number`/`log_index` are skipped (the apply guard requires them as a
/// `(block, log_index)` ordering invariant).
#[must_use]
pub fn decode_v3_liquidity_log(log: &Log) -> Option<LiquidityUpdateEvent> {
    let (tick_lower, tick_upper, magnitude) = decode_v3_mint_log(log)
        .map(|mint| (mint.tick_lower, mint.tick_upper, mint.amount))
        .or_else(|| {
            decode_v3_burn_log(log).map(|burn| (burn.tick_lower, burn.tick_upper, burn.amount))
        })?;
    if magnitude == 0 {
        return None; // ignore zero-amount events
    }
    let block_number = log.block_number?;
    let log_index = log.log_index?;
    // Mint → positive delta; Burn → negative. `u128` → `I256` always succeeds
    // (u128::MAX < 2^255); negation via `Neg`.
    let signed_magnitude = I256::try_from(magnitude).ok()?;
    let liquidity_delta = match decode_v3_burn_log(log) {
        Some(_) => -signed_magnitude,
        None => signed_magnitude,
    };
    Some(LiquidityUpdateEvent {
        block_number,
        log_index,
        tick_lower,
        tick_upper,
        liquidity_delta,
    })
}

/// Decode a V4 `ModifyLiquidity` log into a DB-ready [`LiquidityUpdateEvent`].
///
/// Pure (no RPC). The V4 `liquidity_delta` is already signed (positive for
/// additions, negative for removals — the V4 equivalent of V3's Mint/Burn).
/// Zero-delta events are skipped (mirror the V3 zero-amount skip). Logs
/// missing `block_number`/`log_index` are skipped.
#[must_use]
pub fn decode_v4_liquidity_log(log: &Log) -> Option<LiquidityUpdateEvent> {
    let event = decode_v4_modify_liquidity_log(log)?;
    if event.liquidity_delta.is_zero() {
        return None;
    }
    let block_number = log.block_number?;
    let log_index = log.log_index?;
    Some(LiquidityUpdateEvent {
        block_number,
        log_index,
        tick_lower: event.tick_lower,
        tick_upper: event.tick_upper,
        liquidity_delta: event.liquidity_delta,
    })
}

/// Fetch V3 `Mint`/`Burn` liquidity events across a block range + decode each
/// into a DB-ready [`LiquidityUpdateEvent`].
///
/// `pool_address` filters to a single pool's Mint/Burn events when `Some`
/// (the per-pool chunk update); `None` fetches ALL V3 pool Mint/Burn events
/// in the range (the backfill scan over the whole chain's V3 activity —
/// expensive but necessary when the pool set is large).
///
/// # Errors
///
/// Returns [`ProviderError`] on RPC failure or an invalid block range.
pub async fn fetch_v3_liquidity_logs(
    fetcher: &LogFetcher,
    from_block: u64,
    to_block: u64,
    pool_address: Option<Address>,
) -> ProviderResult<Vec<LiquidityUpdateEvent>> {
    let mint_topic = degenbot_decoders::v3_mint_burn_decoder::V3_MINT_TOPIC;
    let burn_topic = degenbot_decoders::v3_mint_burn_decoder::V3_BURN_TOPIC;
    let logs = fetch_logs_with_topics(
        fetcher,
        from_block,
        to_block,
        pool_address.map(|a| vec![a]),
        Some(vec![mint_topic, burn_topic]),
    )
    .await?;
    Ok(logs.iter().filter_map(decode_v3_liquidity_log).collect())
}

/// Fetch V4 `ModifyLiquidity` events across a block range + decode each into a
/// DB-ready [`LiquidityUpdateEvent`].
///
/// `pool_manager_address` filters to a single `PoolManager`'s events when
/// `Some`; `None` fetches ALL V4 `PoolManager` `ModifyLiquidity` events in the
/// range. (V4 pools all live inside the singleton `PoolManager`, so the
/// emitter filter is the `PoolManager` address, NOT a per-pool address — the
/// per-pool filter is the topic1 `PoolId`, which the caller can post-filter
/// on if needed; this fetcher returns the whole manager's events for the
/// range.)
///
/// # Errors
///
/// Returns [`ProviderError`] on RPC failure or an invalid block range.
pub async fn fetch_v4_liquidity_logs(
    fetcher: &LogFetcher,
    from_block: u64,
    to_block: u64,
    pool_manager_address: Option<Address>,
) -> ProviderResult<Vec<LiquidityUpdateEvent>> {
    let modify_liquidity_topic =
        degenbot_decoders::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC;
    let logs = fetch_logs_with_topics(
        fetcher,
        from_block,
        to_block,
        pool_manager_address.map(|a| vec![a]),
        Some(vec![modify_liquidity_topic]),
    )
    .await?;
    Ok(logs.iter().filter_map(decode_v4_liquidity_log).collect())
}

// ── pool-address-preserving variants (CKXCOB 3b) ─────────────────────────
//
// Task 1's `fetch_v3/v4_liquidity_logs` flatten away the per-pool grouping
// the chunk loop needs: V3 Mint/Burn events are emitted by individual pool
// contracts (the log emitter = the pool address) + V4 `ModifyLiquidity`
// events carry the `PoolId` in topic1 (all emitted by the singleton
// `PoolManager`). `main`'s liquidity path groups by pool + per-pool calls
// `apply_v*_liquidity_updates(..., pool_address, events)` with an in-scope
// filter (V3: the pool's `exchange_id` must be in `exchanges_to_update`;
// V4: per-PoolManager is already per-exchange-scoped). The variants below
// preserve the pool discriminator so 3c's chunk loop can do the same.

/// Like [`decode_v3_liquidity_log`] but preserves the emitting pool address
/// (V3 Mint/Burn events are emitted BY the pool contract — the log emitter
/// IS the pool address the apply step needs). Pure (no RPC). CKXCOB 3b.
///
/// `main`'s `get_v3_liquidity_events` returns a `{pool_address: [events]}`
/// dict — this decode is the building block for that grouping.
#[must_use]
pub fn decode_v3_liquidity_log_with_pool(log: &Log) -> Option<(Address, LiquidityUpdateEvent)> {
    // Decode once (avoid the original `decode_v3_liquidity_log`'s double
    // `decode_v3_burn_log` — it re-decodes to decide the sign). Capture
    // `pool_address` from the leaf (it set it from `log.address()`).
    let (pool_address, tick_lower, tick_upper, magnitude, is_burn) = decode_v3_mint_log(log)
        .map(|mint| {
            (
                mint.pool_address,
                mint.tick_lower,
                mint.tick_upper,
                mint.amount,
                false,
            )
        })
        .or_else(|| {
            decode_v3_burn_log(log).map(|burn| {
                (
                    burn.pool_address,
                    burn.tick_lower,
                    burn.tick_upper,
                    burn.amount,
                    true,
                )
            })
        })?;
    if magnitude == 0 {
        return None; // ignore zero-amount events
    }
    let block_number = log.block_number?;
    let log_index = log.log_index?;
    let signed_magnitude = I256::try_from(magnitude).ok()?;
    let liquidity_delta = if is_burn {
        -signed_magnitude
    } else {
        signed_magnitude
    };
    Some((
        pool_address,
        LiquidityUpdateEvent {
            block_number,
            log_index,
            tick_lower,
            tick_upper,
            liquidity_delta,
        },
    ))
}

/// Fetch V3 `Mint`/`Burn` liquidity events across a block range + group them
/// by emitting pool address (the chunk loop's whole-chain V3 liquidity scan —
/// `main`'s `get_v3_liquidity_events(w3, start, end)` returns the same
/// `{pool_address: [events]}` shape). The chunk loop then per-pool calls
/// `apply_v3_liquidity_updates_on_conn(&tx, chain_id, pool_address, events)`
/// after the in-scope filter (look up the pool's `exchange_id` via
/// `fetch_pool_by_address_on_conn` + skip if not in `exchanges_to_update`).
///
/// `pool_address = Some(addr)` scopes to a single pool (the targeted backfill);
/// `None` = whole-chain scan (the default — V3 events can't be efficiently
/// RPC-filtered to in-scope exchanges, per `main`'s comment). Events within
/// each pool preserve `(block, log_index)` order (the apply guard requires
/// in-order delivery — `LogFetcher::fetch_logs_chunked` already sorts).
///
/// # Errors
///
/// Returns [`ProviderError`](degenbot_core::errors::ProviderError) on RPC
/// failure or an invalid block range.
pub async fn fetch_v3_liquidity_logs_grouped(
    fetcher: &LogFetcher,
    from_block: u64,
    to_block: u64,
    pool_address: Option<Address>,
) -> ProviderResult<HashMap<Address, Vec<LiquidityUpdateEvent>>> {
    let mint_topic = degenbot_decoders::v3_mint_burn_decoder::V3_MINT_TOPIC;
    let burn_topic = degenbot_decoders::v3_mint_burn_decoder::V3_BURN_TOPIC;
    let logs = fetch_logs_with_topics(
        fetcher,
        from_block,
        to_block,
        pool_address.map(|a| vec![a]),
        Some(vec![mint_topic, burn_topic]),
    )
    .await?;
    let mut grouped: HashMap<Address, Vec<LiquidityUpdateEvent>> = HashMap::new();
    for log in &logs {
        if let Some((addr, event)) = decode_v3_liquidity_log_with_pool(log) {
            grouped.entry(addr).or_default().push(event);
        }
    }
    Ok(grouped)
}

/// Like [`decode_v4_liquidity_log`] but preserves the V4 `PoolId` (topic1 of
/// the `ModifyLiquidity` event) as a `0x`-prefixed 66-char hex string (the
/// `pool_hash` form `apply_v4_liquidity_updates(pool_hash, ...)` + the
/// `uniswap_v4_pools.pool_hash` column expect). Pure (no RPC). CKXCOB 3b.
///
/// `main`'s `get_v4_liquidity_events` returns a `{pool_id: [events]}` dict —
/// this decode is the building block for that grouping.
#[must_use]
pub fn decode_v4_liquidity_log_with_pool(log: &Log) -> Option<(String, LiquidityUpdateEvent)> {
    let event = decode_v4_modify_liquidity_log(log)?;
    if event.liquidity_delta.is_zero() {
        return None;
    }
    let block_number = log.block_number?;
    let log_index = log.log_index?;
    // `V4PoolId` is `[u8; 32]`; widen to `B256` for the `0x`-hex `Display`
    // (matches the Python `HexBytes(pool_id).to_0x_hex()` + the DB column form).
    let pool_hash = B256::from(event.pool_id).to_string();
    Some((
        pool_hash,
        LiquidityUpdateEvent {
            block_number,
            log_index,
            tick_lower: event.tick_lower,
            tick_upper: event.tick_upper,
            liquidity_delta: event.liquidity_delta,
        },
    ))
}

/// Fetch V4 `ModifyLiquidity` events across a block range + group them by
/// `pool_hash` (the chunk loop's V4 liquidity scan — `main`'s
/// `get_v4_liquidity_events(w3, start, end, address=pool_manager_address)`
/// returns the same `{pool_id: [events]}` shape). The chunk loop then per-pool
/// calls `apply_v4_liquidity_updates_on_conn(&tx, &pool_hash, chain, events)`.
///
/// `pool_manager_address = Some(addr)` scopes to a single `PoolManager` (one
/// exchange's V4 activity — the per-exchange chunk path); `None` = all V4
/// activity across all `PoolManager`s (the cross-exchange backfill — the caller
/// post-filters by manager). Events within each pool preserve `(block,
/// log_index)` order.
///
/// # Errors
///
/// Returns [`ProviderError`](degenbot_core::errors::ProviderError) on RPC
/// failure or an invalid block range.
pub async fn fetch_v4_liquidity_logs_grouped(
    fetcher: &LogFetcher,
    from_block: u64,
    to_block: u64,
    pool_manager_address: Option<Address>,
) -> ProviderResult<HashMap<String, Vec<LiquidityUpdateEvent>>> {
    let modify_liquidity_topic =
        degenbot_decoders::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC;
    let logs = fetch_logs_with_topics(
        fetcher,
        from_block,
        to_block,
        pool_manager_address.map(|a| vec![a]),
        Some(vec![modify_liquidity_topic]),
    )
    .await?;
    let mut grouped: HashMap<String, Vec<LiquidityUpdateEvent>> = HashMap::new();
    for log in &logs {
        if let Some((pool_hash, event)) = decode_v4_liquidity_log_with_pool(log) {
            grouped.entry(pool_hash).or_default().push(event);
        }
    }
    Ok(grouped)
}

// ── internal fetch helpers ───────────────────────────────────────────────

/// Single-topic fetch (PoolCreated/Initialize). Builds the
/// `eth_getLogs` filter with one topic0 + one address + delegates to the
/// provider's chunked fetcher (big block ranges → per-request chunks with
/// retry). Returns the raw `alloy::Log`s — the caller decodes.
async fn fetch_logs(
    fetcher: &LogFetcher,
    from_block: u64,
    to_block: u64,
    address: Option<Address>,
    topic0: B256,
) -> ProviderResult<Vec<Log>> {
    let addresses = address.map(|a| vec![a.to_string()]);
    let topic_filter = Some(vec![vec![topic0.to_string()]]);
    fetcher
        .fetch_logs_chunked(from_block, to_block, addresses, topic_filter)
        .await
}

/// Multi-topic fetch (V3 `Mint`|`Burn` or V4 `ModifyLiquidity` — a topic0 OR-group).
async fn fetch_logs_with_topics(
    fetcher: &LogFetcher,
    from_block: u64,
    to_block: u64,
    addresses: Option<Vec<Address>>,
    topic0_group: Option<Vec<B256>>,
) -> ProviderResult<Vec<Log>> {
    let addresses = addresses.map(|addrs| {
        addrs
            .iter()
            .map(degenbot_core::address_utils::address_to_checksum_string)
            .collect::<Vec<_>>()
    });
    let topic_filter =
        topic0_group.map(|opts| vec![opts.iter().map(B256::to_string).collect::<Vec<_>>()]);
    fetcher
        .fetch_logs_chunked(from_block, to_block, addresses, topic_filter)
        .await
}

// (No trailing re-export — the fetchers above take `&LogFetcher` directly;
// the chunk loop (Task `CKXCOB`) constructs the `LogFetcher` from the
// provider + `max_blocks_per_request` pool config.)

#[cfg(test)]
#[allow(clippy::unreadable_literal, clippy::unwrap_used)]
mod tests {
    use super::*;
    use alloy::primitives::{Bytes, Log as AlloyLog, I256, U256};

    /// Build a minimal `alloy::rpc::types::Log` with the given emitter,
    /// topics, data, block number + log index (the liquidity decoders need
    /// `block_number`/`log_index` to materialize a `LiquidityUpdateEvent`).
    fn make_log(
        address: Address,
        topics: Vec<B256>,
        data: Vec<u8>,
        block_number: u64,
        log_index: u64,
    ) -> Log {
        let inner = AlloyLog::new_unchecked(address, topics, Bytes::from(data));
        Log {
            inner,
            block_hash: None,
            block_number: Some(block_number),
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: Some(log_index),
            removed: false,
        }
    }

    fn addr_word(addr: Address) -> B256 {
        let mut word = [0u8; 32];
        word[12..32].copy_from_slice(addr.as_slice());
        B256::new(word)
    }

    fn int24_word(tick: i32) -> B256 {
        let i = I256::try_from(i128::from(tick)).unwrap_or(I256::ZERO);
        B256::from(i.to_be_bytes::<32>())
    }

    fn uint128_word(amount: u128) -> [u8; 32] {
        let mut word = [0u8; 32];
        word[16..32].copy_from_slice(&amount.to_be_bytes());
        word
    }

    // ── PoolFamily.topic0 ────────────────────────────────────────────────

    #[test]
    fn pool_family_topic0_roundtrips_to_constants() {
        assert_eq!(PoolFamily::V2.topic0(), V2_PAIR_CREATED_TOPIC);
        assert_eq!(
            PoolFamily::AerodromeV2.topic0(),
            AERODROME_V2_POOL_CREATED_TOPIC,
        );
        assert_eq!(PoolFamily::V3.topic0(), V3_POOL_CREATED_TOPIC);
        assert_eq!(PoolFamily::V4.topic0(), V4_INITIALIZE_TOPIC);
    }

    // ── decode_pool_created_log dispatch ─────────────────────────────────

    #[test]
    fn decode_pool_created_log_dispatches_v3() {
        let factory = Address::from([0xfa; 20]);
        let token0 = Address::from([0x11; 20]);
        let token1 = Address::from([0x22; 20]);
        let pool = Address::from([0x99; 20]);
        let fee: u32 = 500;
        let tick_spacing: i32 = 10;

        let mut data = vec![0u8; 64];
        data[0..32].copy_from_slice(&int24_word(tick_spacing).0);
        data[12 + 32..32 + 32].copy_from_slice(pool.as_slice());

        let mut fee_topic = [0u8; 32];
        fee_topic[29..32].copy_from_slice(&fee.to_be_bytes()[1..4]);
        let log = make_log(
            factory,
            vec![
                V3_POOL_CREATED_TOPIC,
                addr_word(token0),
                addr_word(token1),
                B256::new(fee_topic),
            ],
            data,
            100,
            0,
        );
        let decoded = decode_pool_created_log(&log, PoolFamily::V3).unwrap();
        match decoded {
            DecodedPoolCreated::V3(ev) => {
                assert_eq!(ev.factory_address, factory);
                assert_eq!(ev.token0, token0);
                assert_eq!(ev.token1, token1);
                assert_eq!(ev.fee, fee);
                assert_eq!(ev.tick_spacing, tick_spacing);
                assert_eq!(ev.pool_address, pool);
            }
            other => panic!("expected V3, got {other:?}"),
        }
    }

    #[test]
    fn decode_pool_created_log_wrong_family_returns_none() {
        // A V3 topic fed to the V2 family → must not decode.
        let log = make_log(
            Address::ZERO,
            vec![V3_POOL_CREATED_TOPIC, B256::ZERO, B256::ZERO, B256::ZERO],
            vec![0u8; 64],
            100,
            0,
        );
        assert!(decode_pool_created_log(&log, PoolFamily::V2).is_none());
    }

    // ── decode_v3_liquidity_log (Mint → positive, Burn → negative) ──────

    #[test]
    fn decode_v3_mint_log_yields_positive_delta() {
        let pool = Address::from([0xaa; 20]);
        let sender = Address::from([0xbb; 20]);
        let owner = Address::from([0xcc; 20]);
        let tick_lower = -60;
        let tick_upper = 60;
        let amount = 1_000_000u128;

        // data = abi.encode(address sender, uint128 amount, uint256, uint256)
        let mut data = Vec::with_capacity(128);
        let mut sender_word = [0u8; 32];
        sender_word[12..32].copy_from_slice(sender.as_slice());
        data.extend_from_slice(&sender_word);
        data.extend_from_slice(&uint128_word(amount));
        data.extend_from_slice(&[0u8; 32]); // amount0
        data.extend_from_slice(&[0u8; 32]); // amount1

        let log = make_log(
            pool,
            vec![
                degenbot_decoders::v3_mint_burn_decoder::V3_MINT_TOPIC,
                owner.into_word(),
                int24_word(tick_lower),
                int24_word(tick_upper),
            ],
            data,
            1234,
            7,
        );
        let event = decode_v3_liquidity_log(&log).unwrap();
        assert_eq!(event.block_number, 1234);
        assert_eq!(event.log_index, 7);
        assert_eq!(event.tick_lower, tick_lower);
        assert_eq!(event.tick_upper, tick_upper);
        assert_eq!(event.liquidity_delta, I256::try_from(amount).unwrap());
        assert!(event.liquidity_delta.is_positive());
    }

    #[test]
    fn decode_v3_burn_log_yields_negative_delta() {
        let pool = Address::from([0xaa; 20]);
        let owner = Address::from([0xcc; 20]);
        let tick_lower = -60;
        let tick_upper = 60;
        let amount = 1_000_000u128;

        // Burn data = abi.encode(uint128 amount, uint256, uint256)
        let mut data = Vec::with_capacity(96);
        data.extend_from_slice(&uint128_word(amount));
        data.extend_from_slice(&[0u8; 32]);
        data.extend_from_slice(&[0u8; 32]);

        let log = make_log(
            pool,
            vec![
                degenbot_decoders::v3_mint_burn_decoder::V3_BURN_TOPIC,
                owner.into_word(),
                int24_word(tick_lower),
                int24_word(tick_upper),
            ],
            data,
            1235,
            0,
        );
        let event = decode_v3_liquidity_log(&log).unwrap();
        assert_eq!(event.tick_lower, tick_lower);
        assert_eq!(event.tick_upper, tick_upper);
        assert_eq!(event.liquidity_delta, -I256::try_from(amount).unwrap(),);
        assert!(event.liquidity_delta.is_negative());
    }

    #[test]
    fn decode_v3_zero_amount_mint_returns_none() {
        // zero-amount Mint → skipped (the Python chunk loop's zero-amount filter)
        let pool = Address::from([0xaa; 20]);
        let owner = Address::ZERO;
        let mut data = vec![0u8; 128]; // all zero → amount = 0
        data[12..32].copy_from_slice(Address::from([0xbb; 20]).as_slice()); // sender (non-zero, irrelevant)
        let log = make_log(
            pool,
            vec![
                degenbot_decoders::v3_mint_burn_decoder::V3_MINT_TOPIC,
                owner.into_word(),
                int24_word(0),
                int24_word(60),
            ],
            data,
            1234,
            0,
        );
        assert!(decode_v3_liquidity_log(&log).is_none());
    }

    #[test]
    fn decode_v3_liquidity_log_missing_block_number_returns_none() {
        // block_number = None → can't apply (the (block, log_index) guard
        // stamp requires both). Skipped.
        let pool = Address::from([0xaa; 20]);
        let owner = Address::ZERO;
        let mut data = vec![0u8; 96];
        data[16..32].copy_from_slice(&1u128.to_be_bytes()); // amount = 1
        let inner = AlloyLog::new_unchecked(
            pool,
            vec![
                degenbot_decoders::v3_mint_burn_decoder::V3_BURN_TOPIC,
                owner.into_word(),
                B256::ZERO,
                B256::ZERO,
            ],
            Bytes::from(data),
        );
        let log = Log {
            inner,
            block_hash: None,
            block_number: None, // missing
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: Some(0),
            removed: false,
        };
        assert!(decode_v3_liquidity_log(&log).is_none());
    }

    #[test]
    fn decode_v3_liquidity_log_wrong_topic_returns_none() {
        // A Swap topic must not be decoded as liquidity.
        let log = make_log(
            Address::ZERO,
            vec![
                degenbot_decoders::v3_swap_decoder::V3_SWAP_TOPIC,
                B256::ZERO,
                B256::ZERO,
            ],
            vec![0u8; 160],
            100,
            0,
        );
        assert!(decode_v3_liquidity_log(&log).is_none());
    }

    // ── decode_v4_liquidity_log ──────────────────────────────────────────

    #[test]
    fn decode_v4_modify_liquidity_yields_signed_delta() {
        let pool_manager = Address::from([0xfc; 20]);
        let pool_id = B256::repeat_byte(0x42);
        let sender = Address::from([0xdd; 20]);
        let tick_lower = -100;
        let tick_upper = 100;
        let delta = I256::try_from(500_000i64).unwrap();

        // ModifyLiquidity data = abi.encode(int24 tickLower, int24 tickUpper,
        // int256 liquidityDelta, bytes32 salt)
        let mut data = Vec::with_capacity(128);
        data.extend_from_slice(&int24_word(tick_lower).0);
        data.extend_from_slice(&int24_word(tick_upper).0);
        data.extend_from_slice(&delta.to_be_bytes::<32>());
        data.extend_from_slice(&[0u8; 32]); // salt

        let log = make_log(
            pool_manager,
            vec![
                degenbot_decoders::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC,
                pool_id,
                sender.into_word(),
            ],
            data,
            99,
            3,
        );
        let event = decode_v4_liquidity_log(&log).unwrap();
        assert_eq!(event.block_number, 99);
        assert_eq!(event.log_index, 3);
        assert_eq!(event.tick_lower, tick_lower);
        assert_eq!(event.tick_upper, tick_upper);
        assert_eq!(event.liquidity_delta, delta);
    }

    #[test]
    fn decode_v4_zero_delta_returns_none() {
        let pool_manager = Address::ZERO;
        let pool_id = B256::ZERO;
        let sender = Address::ZERO;
        let data = vec![0u8; 128]; // all zero → delta = 0
        let log = make_log(
            pool_manager,
            vec![
                degenbot_decoders::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC,
                pool_id,
                sender.into_word(),
            ],
            data,
            100,
            0,
        );
        assert!(decode_v4_liquidity_log(&log).is_none());
    }

    #[test]
    fn decode_v4_negative_delta_preserved() {
        // A removal (negative delta) must pass through signed.
        let pool_manager = Address::ZERO;
        let pool_id = B256::ZERO;
        let sender = Address::ZERO;
        let delta = -I256::try_from(123i64).unwrap();
        let mut data = vec![0u8; 128];
        data[0..32].copy_from_slice(&int24_word(-10).0);
        data[32..64].copy_from_slice(&int24_word(10).0);
        data[64..96].copy_from_slice(&delta.to_be_bytes::<32>());
        let log = make_log(
            pool_manager,
            vec![
                degenbot_decoders::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC,
                pool_id,
                sender.into_word(),
            ],
            data,
            200,
            1,
        );
        let event = decode_v4_liquidity_log(&log).unwrap();
        assert!(event.liquidity_delta.is_negative());
        assert_eq!(event.liquidity_delta, delta);
    }

    // ── batch decode: a mixed bag of logs ────────────────────────────────

    #[test]
    fn batch_decode_v3_liquidity_mixed_mint_burn_skip_zero() {
        let owner = Address::ZERO;
        let make_mint = |amount: u128, block: u64| {
            let mut data = vec![0u8; 128];
            // V3 Mint data: word0=sender, word1=uint128 amount (low 16 bytes).
            data[48..64].copy_from_slice(&amount.to_be_bytes());
            make_log(
                Address::from([0xaa; 20]),
                vec![
                    degenbot_decoders::v3_mint_burn_decoder::V3_MINT_TOPIC,
                    owner.into_word(),
                    int24_word(-60),
                    int24_word(60),
                ],
                data,
                block,
                0,
            )
        };
        let make_burn = |amount: u128, block: u64| {
            let mut data = vec![0u8; 96];
            data[16..32].copy_from_slice(&amount.to_be_bytes());
            make_log(
                Address::from([0xaa; 20]),
                vec![
                    degenbot_decoders::v3_mint_burn_decoder::V3_BURN_TOPIC,
                    owner.into_word(),
                    int24_word(-60),
                    int24_word(60),
                ],
                data,
                block,
                0,
            )
        };
        let logs = [
            make_mint(100, 1),
            make_burn(0, 2), // zero → skipped
            make_burn(50, 3),
            make_mint(0, 4), // zero → skipped
            make_mint(200, 5),
        ];
        let events: Vec<LiquidityUpdateEvent> =
            logs.iter().filter_map(decode_v3_liquidity_log).collect();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].liquidity_delta, I256::try_from(100).unwrap());
        assert_eq!(events[1].liquidity_delta, -I256::try_from(50).unwrap());
        assert_eq!(events[2].liquidity_delta, I256::try_from(200).unwrap());
        // Block ordering preserved (the apply guard requires in-order delivery).
        assert_eq!(events[0].block_number, 1);
        assert_eq!(events[1].block_number, 3);
        assert_eq!(events[2].block_number, 5);
    }

    // ── u256 sanity (silence unused-import in case the build trims it) ───
    #[test]
    fn u256_compile_check() {
        let _ = U256::ZERO;
    }

    // ── CKXCOB 3b: pool-address-preserving decode ────────────────────────

    #[test]
    fn decode_v3_liquidity_log_with_pool_preserves_emitter() {
        let pool = Address::from([0xaa; 20]);
        let owner = Address::from([0xcc; 20]);
        let tick_lower = -60;
        let tick_upper = 60;
        let amount = 1_000_000u128;

        let mut data = Vec::with_capacity(128);
        let mut sender_word = [0u8; 32];
        sender_word[12..32].copy_from_slice(Address::from([0xbb; 20]).as_slice());
        data.extend_from_slice(&sender_word);
        data.extend_from_slice(&uint128_word(amount));
        data.extend_from_slice(&[0u8; 32]);
        data.extend_from_slice(&[0u8; 32]);

        let log = make_log(
            pool,
            vec![
                degenbot_decoders::v3_mint_burn_decoder::V3_MINT_TOPIC,
                owner.into_word(),
                int24_word(tick_lower),
                int24_word(tick_upper),
            ],
            data,
            1234,
            7,
        );
        let (addr, event) = decode_v3_liquidity_log_with_pool(&log).unwrap();
        assert_eq!(addr, pool, "pool address preserved (the emitter)");
        assert_eq!(event.block_number, 1234);
        assert_eq!(event.tick_lower, tick_lower);
        assert_eq!(event.tick_upper, tick_upper);
        assert!(event.liquidity_delta.is_positive());
    }

    #[test]
    fn decode_v3_liquidity_log_with_pool_burn_is_negative_and_addressed() {
        let pool = Address::from([0xee; 20]);
        let owner = Address::from([0xcc; 20]);
        let amount = 42_000u128;

        let mut data = Vec::with_capacity(96);
        data.extend_from_slice(&uint128_word(amount));
        data.extend_from_slice(&[0u8; 32]);
        data.extend_from_slice(&[0u8; 32]);

        let log = make_log(
            pool,
            vec![
                degenbot_decoders::v3_mint_burn_decoder::V3_BURN_TOPIC,
                owner.into_word(),
                int24_word(-10),
                int24_word(10),
            ],
            data,
            99,
            3,
        );
        let (addr, event) = decode_v3_liquidity_log_with_pool(&log).unwrap();
        assert_eq!(addr, pool, "burn preserves its emitter too");
        assert!(event.liquidity_delta.is_negative());
    }

    #[test]
    fn decode_v4_liquidity_log_with_pool_preserves_pool_hash_hex() {
        let pool_manager = Address::from([0xfc; 20]);
        let pool_id = B256::repeat_byte(0x42);
        let sender = Address::from([0xdd; 20]);
        let tick_lower = -100;
        let tick_upper = 100;
        let delta = I256::try_from(500_000i64).unwrap();

        let mut data = Vec::with_capacity(128);
        data.extend_from_slice(&int24_word(tick_lower).0);
        data.extend_from_slice(&int24_word(tick_upper).0);
        data.extend_from_slice(&delta.to_be_bytes::<32>());
        data.extend_from_slice(&[0u8; 32]); // salt

        let log = make_log(
            pool_manager,
            vec![
                degenbot_decoders::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC,
                pool_id,
                sender.into_word(),
            ],
            data,
            99,
            3,
        );
        let (pool_hash, event) = decode_v4_liquidity_log_with_pool(&log).unwrap();
        // `pool_hash` must be the 0x-prefixed hex of the bytes32 pool_id.
        assert_eq!(pool_hash, pool_id.to_string());
        assert_eq!(event.tick_lower, tick_lower);
        assert_eq!(event.tick_upper, tick_upper);
        assert_eq!(event.liquidity_delta, delta);
    }

    #[test]
    fn decode_with_pool_skips_zero_amount_v3_and_zero_delta_v4() {
        // V3 zero-amount mint → None.
        let empty_mint = make_log(
            Address::from([0xaa; 20]),
            vec![
                degenbot_decoders::v3_mint_burn_decoder::V3_MINT_TOPIC,
                Address::ZERO.into_word(),
                int24_word(0),
                int24_word(60),
            ],
            vec![0u8; 128],
            1,
            0,
        );
        assert!(decode_v3_liquidity_log_with_pool(&empty_mint).is_none());

        // V4 zero-delta → None.
        let empty_v4 = make_log(
            Address::ZERO,
            vec![
                degenbot_decoders::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC,
                B256::ZERO,
                Address::ZERO.into_word(),
            ],
            vec![0u8; 128],
            2,
            0,
        );
        assert!(decode_v4_liquidity_log_with_pool(&empty_v4).is_none());
    }
}
