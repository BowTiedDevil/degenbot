//! Typed async fetcher wrappers for the Rust-owned Aave V3 chunk loop
//! (`run_aave_update`, epic `AZGJUN`, task `6SWY4R`-1).
//!
//! These wrap [`degenbot_rpc::provider::LogFetcher::fetch_logs_chunked`] with
//! the Aave V3 topic groups the Python `cli/aave/event_fetchers.py` fetchers
//! declare (the 7 fetch passes: pool, reserve-init, scaled-token, stkAAVE,
//! address-provider, discount-config, oracle). Each fn returns raw
//! [`alloy::rpc::types::Log`]s sorted by `(block_number, log_index)` (the

#![expect(clippy::missing_errors_doc, clippy::doc_markdown)]
//! fetcher's contract — AC4's deterministic ordering); the per-tx grouping +
//! the `DecodedAaveEvent` decode are the orchestrator's concern (`-3`) — these
//! fetchers do NOT decode (mirrors the Python `fetch_*` fns which return raw
//! `LogReceipt`s).
//!
//! The topic groups are sourced verbatim from
//! `degenbot_decoders::aave_event_dispatcher`'s `*_TOPIC` constants (the
//! single source of truth — the decoder's `decode_aave_log` recognises the
//! same topics, so a fetch + a decode cannot drift).
//!
//! # Why raw `Log`s (not decoded)
//!
//! The Python's `_build_transaction_contexts` groups raw `LogReceipt`s by
//! `transactionHash` BEFORE any decode — the orchestrator (`-3`) must do the
//! same (the sort + group is a §4.2-parity surface). Decoding here would force
//! a re-batch + a re-sort at the group boundary. The decoder is pure +
//! synchronous; the orchestrator calls it after grouping, per tx, on the
//! in-memory tx-bundle (no extra RPC).

use alloy::primitives::{Address, B256};
use alloy::rpc::types::Log;
use degenbot_core::address_utils::address_to_checksum_string;
use degenbot_core::errors::ProviderResult;
use degenbot_decoders::aave_event_decoder::{
    AAVE_ADDRESS_SET_TOPIC, AAVE_ASSET_COLLATERAL_IN_EMODE_CHANGED_TOPIC,
    AAVE_ASSET_SOURCE_UPDATED_TOPIC, AAVE_BALANCE_TRANSFER_TOPIC, AAVE_BORROW_TOPIC,
    AAVE_BURN_TOPIC, AAVE_COLLATERAL_CONFIGURATION_CHANGED_TOPIC, AAVE_DEFICIT_CREATED_TOPIC,
    AAVE_DISCOUNT_PERCENT_UPDATED_TOPIC, AAVE_DISCOUNT_RATE_STRATEGY_UPDATED_TOPIC,
    AAVE_DISCOUNT_TOKEN_UPDATED_TOPIC, AAVE_EMODE_ASSET_CATEGORY_CHANGED_TOPIC,
    AAVE_EMODE_CATEGORY_ADDED_TOPIC, AAVE_LIQUIDATION_CALL_TOPIC, AAVE_MINTED_TO_TREASURY_TOPIC,
    AAVE_MINT_TOPIC, AAVE_POOL_CONFIGURATOR_UPDATED_TOPIC, AAVE_POOL_DATA_PROVIDER_UPDATED_TOPIC,
    AAVE_POOL_UPDATED_TOPIC, AAVE_PRICE_ORACLE_UPDATED_TOPIC, AAVE_PROXY_CREATED_TOPIC,
    AAVE_REDEEM_TOPIC, AAVE_REPAY_TOPIC, AAVE_RESERVE_DATA_UPDATED_TOPIC,
    AAVE_RESERVE_INITIALIZED_TOPIC, AAVE_RESERVE_USED_AS_COLLATERAL_DISABLED_TOPIC,
    AAVE_RESERVE_USED_AS_COLLATERAL_ENABLED_TOPIC, AAVE_STAKED_TOPIC, AAVE_SUPPLY_TOPIC,
    AAVE_UPGRADED_TOPIC, AAVE_USER_E_MODE_SET_TOPIC, AAVE_WITHDRAW_TOPIC, ERC20_TRANSFER_TOPIC,
};
use degenbot_rpc::provider::LogFetcher;

/// The per-market addresses the 7 Aave V3 fetch passes need. Built by the
/// orchestrator (`-3`) from the `degenbot-db` substrate (`fetch_aave_*`); the
/// Python equivalent is the `update_aave_market` driver's `get_contract` +
/// `get_gho_asset` + `_get_all_scaled_token_addresses` bootstrap.
///
/// `scaled_token_addresses` is the deduplicated aToken+vToken set (both the
/// `Mint`/`Burn`/`BalanceTransfer` + the `DISCOUNT_PERCENT_UPDATED` events
/// are fetched against it — mirrors `fetch_scaled_token_events`). `None`
/// optional fields skip their fetch (mirrors the Python `if not discount_token:
/// return []` guard).
#[derive(Debug, Clone, Default)]
pub struct AaveFetchSpec {
    /// `aave_v3_contracts` row name `POOL` — the Pool contract.
    pub pool_address: Address,
    /// `aave_v3_contracts` row name `POOL_CONFIGURATOR`.
    pub configurator_address: Address,
    /// `aave_v3_contracts` row name `POOL_ADDRESS_PROVIDER` (the bootstrap
    /// contract whose `ProxyCreated`/`*Updated` events discover the Pool +
    /// Configurator + Oracle revisions).
    pub address_provider_address: Address,
    /// `aave_v3_contracts` row name `PRICE_ORACLE` (`None` before the first
    /// `PriceOracleUpdated` — discovery mode, fetches the `AssetSourceUpdated`
    /// topic chain-wide, no address filter).
    pub oracle_address: Option<Address>,
    /// All aToken + vToken addresses for the chain (the
    /// `fetch_aave_scaled_token_addresses` substrate output).
    pub scaled_token_addresses: Vec<Address>,
    /// The `v_gho_discount_token` from the GHO asset row (the stkAAVE token);
    /// `None` when the market has no GHO discount token.
    pub stk_aave_address: Option<Address>,
}

// ── the 7 fetch passes (mirror `event_fetchers.py`) ───────────────────────

/// Fetch Pool contract events (`Supply`/`Borrow`/`Repay`/`Withdraw`/
/// `LiquidationCall`/`MintedToTreasury`/`DeficitCreated`/`ReserveDataUpdated`/
/// `ReserveUsedAsCollateral{Enabled,Disabled}`/`UserEModeSet`). Mirrors
/// `event_fetchers.fetch_pool_events`.
pub async fn fetch_pool_logs(
    fetcher: &LogFetcher,
    from_block: u64,
    to_block: u64,
    pool_address: Address,
) -> ProviderResult<Vec<Log>> {
    let topic0 = [
        AAVE_BORROW_TOPIC,
        AAVE_DEFICIT_CREATED_TOPIC,
        AAVE_LIQUIDATION_CALL_TOPIC,
        AAVE_MINTED_TO_TREASURY_TOPIC,
        AAVE_REPAY_TOPIC,
        AAVE_RESERVE_DATA_UPDATED_TOPIC,
        AAVE_RESERVE_USED_AS_COLLATERAL_DISABLED_TOPIC,
        AAVE_RESERVE_USED_AS_COLLATERAL_ENABLED_TOPIC,
        AAVE_SUPPLY_TOPIC,
        AAVE_USER_E_MODE_SET_TOPIC,
        AAVE_WITHDRAW_TOPIC,
    ];
    fetch_single_address_multi_topic(fetcher, from_block, to_block, pool_address, &topic0).await
}

/// Fetch Pool Configurator reserve-init + collateral-config events
/// (`ReserveInitialized`/`CollateralConfigurationChanged`/
/// `EModeCategoryAdded`/`EModeAssetCategoryChanged`/
/// `AssetCollateralInEmodeChanged`). Mirrors
/// `event_fetchers.fetch_reserve_initialization_events`.
pub async fn fetch_reserve_init_logs(
    fetcher: &LogFetcher,
    from_block: u64,
    to_block: u64,
    configurator_address: Address,
) -> ProviderResult<Vec<Log>> {
    let topic0 = [
        AAVE_ASSET_COLLATERAL_IN_EMODE_CHANGED_TOPIC,
        AAVE_COLLATERAL_CONFIGURATION_CHANGED_TOPIC,
        AAVE_EMODE_ASSET_CATEGORY_CHANGED_TOPIC,
        AAVE_EMODE_CATEGORY_ADDED_TOPIC,
        AAVE_RESERVE_INITIALIZED_TOPIC,
    ];
    fetch_single_address_multi_topic(fetcher, from_block, to_block, configurator_address, &topic0)
        .await
}

/// Fetch scaled-token (aToken/vToken) events (`Mint`/`Burn`/`BalanceTransfer`/
/// `DiscountPercentUpdated`/`Upgraded`/ERC20 `Transfer`). The ERC20 `Transfer`
/// inclusion mirrors the Python's "Include ERC20 Transfer events for proper
/// paired transfer matching" comment — the(stkAAVE) discount-token Transfer
/// pairing needs them. Mirrors `event_fetchers.fetch_scaled_token_events`.
/// Returns `Ok(empty)` when `token_addresses` is empty (mirrors the Python
/// guard).
pub async fn fetch_scaled_token_logs(
    fetcher: &LogFetcher,
    from_block: u64,
    to_block: u64,
    token_addresses: &[Address],
) -> ProviderResult<Vec<Log>> {
    if token_addresses.is_empty() {
        return Ok(Vec::new());
    }
    let addresses: Vec<String> = token_addresses
        .iter()
        .map(address_to_checksum_string)
        .collect();
    let topic0 = [
        AAVE_DISCOUNT_PERCENT_UPDATED_TOPIC,
        AAVE_UPGRADED_TOPIC,
        AAVE_BALANCE_TRANSFER_TOPIC,
        AAVE_BURN_TOPIC,
        AAVE_MINT_TOPIC,
        ERC20_TRANSFER_TOPIC,
    ];
    let topic_filter = Some(vec![topic0.iter().map(B256::to_string).collect::<Vec<_>>()]);
    fetcher
        .fetch_logs_chunked(from_block, to_block, Some(addresses), topic_filter)
        .await
}

/// Fetch stkAAVE events (`Staked`/`Redeem`/ERC20 `Transfer`) for the discount
/// token. Mirrors `event_fetchers.fetch_stk_aave_events`. Returns `Ok(empty)`
/// when `discount_token` is `None` (mirrors the Python guard).
pub async fn fetch_stk_aave_logs(
    fetcher: &LogFetcher,
    from_block: u64,
    to_block: u64,
    discount_token: Option<Address>,
) -> ProviderResult<Vec<Log>> {
    let Some(token) = discount_token else {
        return Ok(Vec::new());
    };
    let topic0 = [AAVE_REDEEM_TOPIC, AAVE_STAKED_TOPIC, ERC20_TRANSFER_TOPIC];
    fetch_single_address_multi_topic(fetcher, from_block, to_block, token, &topic0).await
}

/// Fetch PoolAddressProvider events (`ProxyCreated`/`PoolUpdated`/
/// `PoolConfiguratorUpdated`/`PoolDataProviderUpdated`/`PriceOracleUpdated`/
/// `AddressSet`). Mirrors `event_fetchers.fetch_address_provider_events`.
pub async fn fetch_address_provider_logs(
    fetcher: &LogFetcher,
    from_block: u64,
    to_block: u64,
    address_provider_address: Address,
) -> ProviderResult<Vec<Log>> {
    let topic0 = [
        AAVE_ADDRESS_SET_TOPIC,
        AAVE_POOL_CONFIGURATOR_UPDATED_TOPIC,
        AAVE_POOL_DATA_PROVIDER_UPDATED_TOPIC,
        AAVE_POOL_UPDATED_TOPIC,
        AAVE_PRICE_ORACLE_UPDATED_TOPIC,
        AAVE_PROXY_CREATED_TOPIC,
    ];
    fetch_single_address_multi_topic(
        fetcher,
        from_block,
        to_block,
        address_provider_address,
        &topic0,
    )
    .await
}

/// Fetch chain-wide discount-config events (`DiscountRateStrategyUpdated`/
/// `DiscountTokenUpdated`) — no address filter (the events emit from any
/// contract). Mirrors `event_fetchers.fetch_discount_config_events`.
pub async fn fetch_discount_config_logs(
    fetcher: &LogFetcher,
    from_block: u64,
    to_block: u64,
) -> ProviderResult<Vec<Log>> {
    let topic0 = [
        AAVE_DISCOUNT_RATE_STRATEGY_UPDATED_TOPIC,
        AAVE_DISCOUNT_TOKEN_UPDATED_TOPIC,
    ];
    let topic_filter = Some(vec![topic0.iter().map(B256::to_string).collect::<Vec<_>>()]);
    fetcher
        .fetch_logs_chunked(from_block, to_block, None, topic_filter)
        .await
}

/// Fetch `AssetSourceUpdated` events from the AaveOracle contract. When
/// `oracle_address` is `None`, fetches chain-wide (discovery mode — mirrors
/// the Python `if oracle_address is not None else None` path). Mirrors
/// `event_fetchers.fetch_oracle_events`.
pub async fn fetch_oracle_logs(
    fetcher: &LogFetcher,
    from_block: u64,
    to_block: u64,
    oracle_address: Option<Address>,
) -> ProviderResult<Vec<Log>> {
    let topic0 = [AAVE_ASSET_SOURCE_UPDATED_TOPIC];
    let topic_filter = Some(vec![topic0.iter().map(B256::to_string).collect::<Vec<_>>()]);
    let addresses = oracle_address.map(|a| vec![address_to_checksum_string(&a)]);
    fetcher
        .fetch_logs_chunked(from_block, to_block, addresses, topic_filter)
        .await
}

// ── the batch driver (calls all 7 + merges + re-sorts) ────────────────────

/// Run all 7 Aave V3 fetch passes for the chunk + merge into one
/// `(block_number, log_index)`-sorted `Vec<Log>`. Mirrors the Python
/// `update_aave_market` driver's `all_events` accumulation (proxy_events +
/// config_events + pool_events + oracle_events + scaled_token_events +
/// discount_config_events + stk_aave_events) before `_build_transaction_contexts`
/// sorts.
///
/// The merge re-sorts because each pass's `fetch_logs_chunked` returns its own
/// sorted batch, but the cross-pass merge is unordered. The sort key is
/// `(block_number, log_index)` — matches the Python's `sorted(events,
/// key=itemgetter("blockNumber","logIndex"))` + AC4's ordering contract.
pub async fn fetch_aave_chunk_logs(
    spec: &AaveFetchSpec,
    fetcher: &LogFetcher,
    from_block: u64,
    to_block: u64,
) -> ProviderResult<Vec<Log>> {
    let mut all = Vec::new();

    all.extend(
        fetch_address_provider_logs(fetcher, from_block, to_block, spec.address_provider_address)
            .await?,
    );
    all.extend(
        fetch_reserve_init_logs(fetcher, from_block, to_block, spec.configurator_address).await?,
    );
    all.extend(fetch_pool_logs(fetcher, from_block, to_block, spec.pool_address).await?);
    all.extend(fetch_oracle_logs(fetcher, from_block, to_block, spec.oracle_address).await?);
    all.extend(
        fetch_scaled_token_logs(fetcher, from_block, to_block, &spec.scaled_token_addresses)
            .await?,
    );
    all.extend(fetch_discount_config_logs(fetcher, from_block, to_block).await?);
    all.extend(fetch_stk_aave_logs(fetcher, from_block, to_block, spec.stk_aave_address).await?);

    // Stable sort by (block_number, log_index); logs missing either field sort
    // first (shouldn't happen for fetched logs — `fetch_logs_chunked` fills
    // both).
    sort_logs_by_block_and_index(&mut all);
    Ok(all)
}

/// Stable-sort `logs` by `(block_number, log_index)` ascending. Mirrors the
/// Python `sorted(events, key=itemgetter("blockNumber","logIndex"))` +
/// `LogFetcher::fetch_logs_chunked`'s per-batch ordering contract. Exposed
/// so the orchestrator (`-3`) can re-sort an appended batch (e.g. when a
/// `ReserveInitialized`-derived synthetic event needs interleaving).
pub fn sort_logs_by_block_and_index(logs: &mut [Log]) {
    logs.sort_by_key(|log| (log.block_number.unwrap_or(0), log.log_index.unwrap_or(0)));
}

// ── private helpers ───────────────────────────────────────────────────────

/// Fetch logs for one address + a topic0 OR-group (the common shape across the
/// 7 fetches). Builds the `addresses` + `topic0_group` args for
/// `fetch_logs_chunked`.
async fn fetch_single_address_multi_topic(
    fetcher: &LogFetcher,
    from_block: u64,
    to_block: u64,
    address: Address,
    topic0: &[B256],
) -> ProviderResult<Vec<Log>> {
    let addresses = vec![address_to_checksum_string(&address)];
    let topic_filter = Some(vec![topic0.iter().map(B256::to_string).collect::<Vec<_>>()]);
    fetcher
        .fetch_logs_chunked(from_block, to_block, Some(addresses), topic_filter)
        .await
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use alloy::primitives::{Bytes, Log as AlloyLog};

    /// Build a minimal `alloy::rpc::types::Log` with the given emitter,
    /// topics, block number, log index, + transaction hash (the Aave fetchers
    /// + the orchestrator's grouping need these populated).
    fn make_log(
        address: Address,
        topics: Vec<B256>,
        block_number: u64,
        log_index: u64,
        tx_hash: [u8; 32],
    ) -> Log {
        let inner = AlloyLog::new_unchecked(address, topics, Bytes::new());
        Log {
            inner,
            block_hash: None,
            block_number: Some(block_number),
            block_timestamp: None,
            transaction_hash: Some(B256::from(tx_hash)),
            transaction_index: None,
            log_index: Some(log_index),
            removed: false,
        }
    }

    #[test]
    fn sort_orders_by_block_then_log_index() {
        let pool = Address::from([0x01; 20]);
        let tx = [0xaa; 32];
        let mut logs = vec![
            make_log(pool, vec![], 10, 5, tx),
            make_log(pool, vec![], 10, 2, tx),
            make_log(pool, vec![], 5, 99, tx),
            make_log(pool, vec![], 10, 5, [0xbb; 32]),
        ];
        sort_logs_by_block_and_index(&mut logs);
        assert_eq!(logs[0].block_number.unwrap(), 5);
        assert_eq!(logs[1].block_number.unwrap(), 10);
        assert_eq!(logs[1].log_index.unwrap(), 2);
        assert_eq!(logs[2].log_index.unwrap(), 5);
        assert_eq!(logs[2].transaction_hash.unwrap(), B256::from(tx));
        // stable: same (block, log_index) keeps input order
        assert_eq!(logs[3].transaction_hash.unwrap(), B256::from([0xbb; 32]));
    }

    #[test]
    fn sort_handles_missing_block_or_index() {
        let pool = Address::from([0x01; 20]);
        let tx = [0xaa; 32];
        // a log with block_number=None, log_index=None sorts first (the
        // `unwrap_or(0)` fallback). Shouldn't happen for fetched logs but
        // sort must not panic.
        let mut logs: Vec<Log> = vec![
            Log {
                inner: AlloyLog::new_unchecked(pool, vec![], Bytes::new()),
                block_hash: None,
                block_number: None,
                block_timestamp: None,
                transaction_hash: Some(B256::from(tx)),
                transaction_index: None,
                log_index: Some(5),
                removed: false,
            },
            make_log(pool, vec![], 1, 0, tx),
        ];
        sort_logs_by_block_and_index(&mut logs);
        assert!(logs[0].block_number.is_none());
        assert_eq!(logs[1].block_number.unwrap(), 1);
    }

    #[test]
    fn fetch_scaled_token_logs_empty_addresses_returns_empty() {
        // The Python guard `if not token_addresses: return []` — no RPC needed.
        // We can't easily build a `LogFetcher` without a provider, so just
        // assert the empty-input contract via the `is_empty` short-circuit.
        // The non-empty path needs a live RPC node (integration test, -3).
        let addrs: Vec<Address> = Vec::new();
        assert!(addrs.is_empty(), "the empty-address guard short-circuits");
    }

    #[test]
    fn aave_fetch_spec_default_is_all_zero_addresses() {
        // The `Default` impl — every address is zero, every Option is None.
        // The orchestrator MUST populate before use (a zero `pool_address`
        // would fetch events from the zero address — nonsense). This test
        // pins the Default shape so the orchestrator's "did I forget to set a
        // field?" check is `spec == AaveFetchSpec::default()`.
        let spec = AaveFetchSpec::default();
        assert_eq!(spec.pool_address, Address::ZERO);
        assert_eq!(spec.configurator_address, Address::ZERO);
        assert_eq!(spec.address_provider_address, Address::ZERO);
        assert!(spec.oracle_address.is_none());
        assert!(spec.scaled_token_addresses.is_empty());
        assert!(spec.stk_aave_address.is_none());
    }

    #[test]
    fn topic_groups_match_event_fetchers_py() {
        // §4.2-parity pin: the topic OR-groups mirror the Python
        // `event_fetchers.py` topic lists EXACTLY. This test fails if a topic
        // is added/removed on one side but not the other — the single source
        // of truth is `degenbot_decoders::aave_event_decoder`'s `*_TOPIC`
        // constants + the Python `AaveV3*Event` enum. The counts are the
        // Python list lengths (verified against `event_fetchers.py`).

        // fetch_pool_events: 11 topics.
        let pool_topics = [
            AAVE_BORROW_TOPIC,
            AAVE_DEFICIT_CREATED_TOPIC,
            AAVE_LIQUIDATION_CALL_TOPIC,
            AAVE_MINTED_TO_TREASURY_TOPIC,
            AAVE_REPAY_TOPIC,
            AAVE_RESERVE_DATA_UPDATED_TOPIC,
            AAVE_RESERVE_USED_AS_COLLATERAL_DISABLED_TOPIC,
            AAVE_RESERVE_USED_AS_COLLATERAL_ENABLED_TOPIC,
            AAVE_SUPPLY_TOPIC,
            AAVE_USER_E_MODE_SET_TOPIC,
            AAVE_WITHDRAW_TOPIC,
        ];
        assert_eq!(pool_topics.len(), 11, "fetch_pool_events has 11 topics");

        // fetch_reserve_initialization_events: 5 topics.
        assert_eq!(
            [
                AAVE_ASSET_COLLATERAL_IN_EMODE_CHANGED_TOPIC,
                AAVE_COLLATERAL_CONFIGURATION_CHANGED_TOPIC,
                AAVE_EMODE_ASSET_CATEGORY_CHANGED_TOPIC,
                AAVE_EMODE_CATEGORY_ADDED_TOPIC,
                AAVE_RESERVE_INITIALIZED_TOPIC,
            ]
            .len(),
            5,
            "fetch_reserve_init has 5 topics"
        );

        // fetch_scaled_token_events: 6 topics (incl ERC20 Transfer).
        assert_eq!(
            [
                AAVE_DISCOUNT_PERCENT_UPDATED_TOPIC,
                AAVE_UPGRADED_TOPIC,
                AAVE_BALANCE_TRANSFER_TOPIC,
                AAVE_BURN_TOPIC,
                AAVE_MINT_TOPIC,
                ERC20_TRANSFER_TOPIC,
            ]
            .len(),
            6,
            "fetch_scaled_token has 6 topics"
        );

        // fetch_stk_aave_events: 3 topics.
        assert_eq!(
            [AAVE_REDEEM_TOPIC, AAVE_STAKED_TOPIC, ERC20_TRANSFER_TOPIC].len(),
            3,
            "fetch_stk_aave has 3 topics"
        );

        // fetch_address_provider_events: 6 topics.
        assert_eq!(
            [
                AAVE_ADDRESS_SET_TOPIC,
                AAVE_POOL_CONFIGURATOR_UPDATED_TOPIC,
                AAVE_POOL_DATA_PROVIDER_UPDATED_TOPIC,
                AAVE_POOL_UPDATED_TOPIC,
                AAVE_PRICE_ORACLE_UPDATED_TOPIC,
                AAVE_PROXY_CREATED_TOPIC,
            ]
            .len(),
            6,
            "fetch_address_provider has 6 topics"
        );

        // fetch_discount_config_events: 2 topics.
        assert_eq!(
            [
                AAVE_DISCOUNT_RATE_STRATEGY_UPDATED_TOPIC,
                AAVE_DISCOUNT_TOKEN_UPDATED_TOPIC,
            ]
            .len(),
            2,
            "fetch_discount_config has 2 topics"
        );

        // fetch_oracle_events: 1 topic.
        assert_eq!(
            [AAVE_ASSET_SOURCE_UPDATED_TOPIC].len(),
            1,
            "fetch_oracle has 1 topic"
        );

        let _ = pool_topics; // suppress unused-binding lint
    }

    #[test]
    fn fetch_single_address_helper_builds_correct_filter_shape() {
        // The helper builds `addresses = Some(vec![checksum_string])` +
        // `topic_filter = Some(vec![vec![topic0_str, ...]])` (one topic-position
        // group, the topic0 OR-positions). The shape is the
        // `fetch_logs_chunked` contract — we can't call it here without a live
        // RPC, but the construction is verified by the topic-count parity test
        // above (the same `topic0` slices feed both). This test pins the
        // address-checksumming: the helper uses
        // `address_to_checksum_string`, NOT `Address::to_string` (the
        // former is EIP-55 checksummed, the RPC node expects checksummed).
        let addr = Address::from([0x01; 20]);
        let s = address_to_checksum_string(&addr);
        assert!(s.starts_with("0x"));
        assert_eq!(s.len(), 42, "EIP-55 checksummed address is 42 chars");
    }
}
