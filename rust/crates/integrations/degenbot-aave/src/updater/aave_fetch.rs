//! Typed async fetcher wrappers for the Rust-owned Aave V3 chunk loop
//! (`run_aave_update`).
//!
//! These wrap [`degenbot_rpc::provider::LogFetcher::fetch_logs_chunked`] with
//! the Aave V3 topic groups for the 7 fetch passes (pool, reserve-init,
//! scaled-token, stkAAVE, address-provider, discount-config, oracle). Each fn returns raw
//! [`alloy::rpc::types::Log`]s sorted by `(block_number, log_index)` (the

#![expect(clippy::missing_errors_doc, clippy::doc_markdown)]
//! fetcher's contract — deterministic `(block_number, log_index)` ordering);
//! the per-tx grouping + the `DecodedAaveEvent` decode are the orchestrator's
//! concern — these
//! fetchers do NOT decode: a fetch returns raw `LogReceipt`s and the decode leaf
//! owns the interpretation.
//!
//! The topic groups are sourced verbatim from
//! `degenbot_decoders::aave_event_dispatcher`'s `*_TOPIC` constants (the
//! single source of truth — the decoder's `decode_aave_log` recognises the
//! same topics, so a fetch + a decode cannot drift).
//!
//! # Why raw `Log`s (not decoded)
//!
//! Raw `LogReceipt`s are grouped by `transactionHash` BEFORE any decode —
//! the orchestrator must do the same (the sort + group is a parity-gate
//! surface). Decoding here would force
//! a re-batch + a re-sort at the group boundary. The decoder is pure +
//! synchronous; the orchestrator calls it after grouping, per tx, on the
//! in-memory tx-bundle (no extra RPC).

use std::collections::HashMap;

use alloy::primitives::{Address, Bytes, B256, U256};
use alloy::rpc::types::Log;
use alloy::sol_types::SolValue;
use degenbot_core::address_utils::address_to_checksum_string;
use degenbot_core::errors::{ProviderError, ProviderResult};
use degenbot_decoders::aave_event_decoder::{decode_aave_log, DecodedAaveEvent};
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
use degenbot_rpc::abi::ContractRead;
use degenbot_rpc::provider::LogFetcher;

use super::config_dispatch::strip_trailing_nulls_from_ascii;

/// The per-market addresses the 7 Aave V3 fetch passes need. Built by the
/// orchestrator (`-3`) from the `degenbot-db` substrate (`fetch_aave_*`); the
/// Python equivalent is the `update_aave_market` driver's `get_contract` +
/// `get_gho_asset` + `_get_all_scaled_token_addresses` bootstrap.
///
/// `scaled_token_addresses` is the deduplicated aToken+vToken set (both the
/// `Mint`/`Burn`/`BalanceTransfer` + the `DISCOUNT_PERCENT_UPDATED` events
/// are fetched against it — mirrors `fetch_scaled_token_events`). `None`
/// optional fields skip their fetch: an absent `discount_token` returns `[]`.
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

// ── the 7 fetch passes ───────────────────────────────────────────────────

/// Fetch Pool contract events (`Supply`/`Borrow`/`Repay`/`Withdraw`/
/// `LiquidationCall`/`MintedToTreasury`/`DeficitCreated`/`ReserveDataUpdated`/
/// `ReserveUsedAsCollateral{Enabled,Disabled}`/`UserEModeSet`).
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
/// `AssetCollateralInEmodeChanged`).
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
/// inclusion is required for paired transfer matching: the stkAAVE
/// discount-token Transfer pairing needs the paired ERC20 Transfer events.
/// Returns `Ok(empty)` when `token_addresses` is empty — nothing to fetch is a
/// success, not an error.
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
/// token. Returns `Ok(empty)` when `discount_token` is `None`.
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
/// `AddressSet`).
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
/// contract).
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
/// `oracle_address` is `None`, fetches chain-wide (discovery mode).
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

// ── the same-chunk endpoint replacement plan ──────────────────────────────

/// The endpoint replacements the chunk's own logs announce: per fetch pass,
/// the replacement emitter addresses an `AddressSet` rotation/registration or
/// a `PriceOracleUpdated` inside the chunk points at and the pass's
/// chunk-start address does not already cover.
///
/// The main pass fetched from the current DB endpoints (the chunk-start
/// [`AaveFetchSpec`]); logs emitted by a replacement endpoint before its
/// apply lands are only reachable through a supplemental pass over the same
/// block range. A `PRICE_ORACLE` replacement while the spec address is
/// `None` needs no supplement: the main pass was chain-wide (discovery).
#[derive(Debug, Default)]
pub struct ChunkEndpointReplacements {
    pub address_provider: Vec<Address>,
    pub configurator: Vec<Address>,
    pub pool: Vec<Address>,
    pub oracle: Vec<Address>,
}

impl ChunkEndpointReplacements {
    /// Whether any pass needs a supplemental fetch.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.address_provider.is_empty()
            && self.configurator.is_empty()
            && self.pool.is_empty()
            && self.oracle.is_empty()
    }
}

/// Scan the chunk's fetched logs for endpoint replacement events and build
/// the supplemental per-pass fetch targets. A replacement whose new address
/// equals the pass's chunk-start address is already covered by the main pass;
/// the oracle pass only replaces against a `Some` spec address.
#[must_use]
pub fn chunk_endpoint_replacements(
    logs: &[Log],
    spec: &AaveFetchSpec,
) -> ChunkEndpointReplacements {
    let mut plan = ChunkEndpointReplacements::default();
    for log in logs {
        let Some(decoded) = decode_aave_log(log) else {
            continue;
        };
        match decoded {
            DecodedAaveEvent::PriceOracleUpdated(ev) => {
                push_replacement(spec.oracle_address, ev.new_address, &mut plan.oracle);
            }
            DecodedAaveEvent::AddressSet(ev) => {
                let name = strip_trailing_nulls_from_ascii(&ev.id);
                match name.as_str() {
                    "POOL" => {
                        push_replacement(Some(spec.pool_address), ev.new_address, &mut plan.pool);
                    }
                    "POOL_CONFIGURATOR" => {
                        push_replacement(
                            Some(spec.configurator_address),
                            ev.new_address,
                            &mut plan.configurator,
                        );
                    }
                    "POOL_ADDRESS_PROVIDER" => {
                        push_replacement(
                            Some(spec.address_provider_address),
                            ev.new_address,
                            &mut plan.address_provider,
                        );
                    }
                    "PRICE_ORACLE" => {
                        push_replacement(spec.oracle_address, ev.new_address, &mut plan.oracle);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    plan
}

fn push_replacement(covered: Option<Address>, new_address: Address, out: &mut Vec<Address>) {
    if covered.is_none_or(|c| c == new_address) || out.contains(&new_address) {
        return;
    }
    out.push(new_address);
}

/// Whether the log is an endpoint replacement event for a tracked fetch
/// endpoint (`PriceOracleUpdated`, or an `AddressSet` naming one) — the
/// events whose apply is the ONLY way the DB's endpoint rows change. The
/// orchestrator uses this to scope the next chunk's DB endpoint refresh to
/// the chunks that follow a replacement, so a no-replacement run issues no
/// extra statements.
#[must_use]
pub fn is_endpoint_replacement_event(log: &Log) -> bool {
    let Some(decoded) = decode_aave_log(log) else {
        return false;
    };
    match decoded {
        DecodedAaveEvent::PriceOracleUpdated(_) => true,
        DecodedAaveEvent::AddressSet(ev) => {
            let name = strip_trailing_nulls_from_ascii(&ev.id);
            matches!(
                name.as_str(),
                "POOL" | "POOL_CONFIGURATOR" | "POOL_ADDRESS_PROVIDER" | "PRICE_ORACLE"
            )
        }
        _ => false,
    }
}

/// Fetch the endpoint replacement passes' logs over the chunk range — one
/// `eth_getLogs` per uncovered replacement address, in pass order.
pub async fn fetch_endpoint_replacement_logs(
    fetcher: &LogFetcher,
    from_block: u64,
    to_block: u64,
    replacements: &ChunkEndpointReplacements,
) -> ProviderResult<Vec<Log>> {
    let mut extra = Vec::new();
    for address in &replacements.address_provider {
        extra.extend(fetch_address_provider_logs(fetcher, from_block, to_block, *address).await?);
    }
    for address in &replacements.configurator {
        extra.extend(fetch_reserve_init_logs(fetcher, from_block, to_block, *address).await?);
    }
    for address in &replacements.pool {
        extra.extend(fetch_pool_logs(fetcher, from_block, to_block, *address).await?);
    }
    for address in &replacements.oracle {
        extra.extend(fetch_oracle_logs(fetcher, from_block, to_block, Some(*address)).await?);
    }
    Ok(extra)
}

// ── the batch driver (calls all 7 + merges + re-sorts) ────────────────────

/// Run all 7 Aave V3 fetch passes for the chunk + merge into one
/// `(block_number, log_index)`-sorted `Vec<Log>` accumulated across every event
/// family (proxy_events +
/// config_events + pool_events + oracle_events + scaled_token_events +
/// discount_config_events + stk_aave_events) before the orchestrator's per-tx
/// grouping sorts.
///
/// The merge re-sorts because each pass's `fetch_logs_chunked` returns its own
/// sorted batch, but the cross-pass merge is unordered. The sort key is
/// `(block_number, log_index)` — the parity-gate ordering contract.
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

/// Stable-sort `logs` by `(block_number, log_index)` ascending — the
/// `LogFetcher::fetch_logs_chunked` per-batch ordering contract. Exposed so
/// the orchestrator can re-sort an appended batch (e.g. when a
/// `ReserveInitialized`-derived synthetic event needs interleaving).
///
/// The sort states no block fact: a log missing its height or index orders by the
/// absence itself (`None` first), never by a fabricated zero. Whether a log HAS an
/// exact position is `ChunkLogPosition::of`'s answer, and a consumer that must place
/// a log at an exact coordinate asks there (ADR-105 D1, D2).
pub fn sort_logs_by_block_and_index(logs: &mut [Log]) {
    logs.sort_by(|left, right| {
        left.block_number
            .cmp(&right.block_number)
            .then(left.log_index.cmp(&right.log_index))
    });
}

/// A chunk log's exact coordinates: the block and transaction it belongs to, plus
/// its block-wide `logIndex`.
///
/// A `(block_number, log_index)` pair is not identity: two branches at one height
/// share a height, two transactions in one block share a log range, and a log with
/// neither fact has no coordinate at all. Every fact here is carried by the log
/// itself, so a filtered fetch that omits one has no position — a refusal, never a
/// default (ADR-105 D1, D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChunkLogPosition {
    pub block_number: u64,
    pub block_hash: B256,
    pub transaction_hash: B256,
    pub transaction_index: u64,
    pub log_index: u64,
}

impl ChunkLogPosition {
    /// The exact position `log` was emitted at.
    ///
    /// # Errors
    ///
    /// One [`LogIdentityRefusal`] per fact the log does not carry.
    pub fn of(log: &Log) -> Result<Self, LogIdentityRefusal> {
        let Some(block_number) = log.block_number else {
            return Err(LogIdentityRefusal::MissingBlockNumber);
        };
        let Some(block_hash) = log.block_hash else {
            return Err(LogIdentityRefusal::MissingBlockHash { block_number });
        };
        let Some(transaction_hash) = log.transaction_hash else {
            return Err(LogIdentityRefusal::MissingTransactionHash { block_number });
        };
        let Some(transaction_index) = log.transaction_index else {
            return Err(LogIdentityRefusal::MissingTransactionIndex {
                block_number,
                transaction_hash,
            });
        };
        let Some(log_index) = log.log_index else {
            return Err(LogIdentityRefusal::MissingLogIndex {
                block_number,
                transaction_hash,
            });
        };
        if log.removed {
            return Err(LogIdentityRefusal::RemovedLog {
                block_number,
                log_index,
            });
        }
        Ok(Self {
            block_number,
            block_hash,
            transaction_hash,
            transaction_index,
            log_index,
        })
    }
}

/// One coordinate's log, stated in full: the exact position PLUS the facts that make
/// it that log rather than another.
///
/// The coordinate alone cannot decide whether a repeated answer is the same log: two
/// logs of one block and one index with different emitters, topics, or data are two
/// logs claiming one position, which is contradictory evidence rather than a duplicate
/// to drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChunkLogFacts {
    pub(crate) position: ChunkLogPosition,
    address: Address,
    topics: Vec<B256>,
    data: Bytes,
    removed: bool,
}

impl ChunkLogFacts {
    pub(crate) fn of(log: &Log) -> Result<Self, LogIdentityRefusal> {
        Ok(Self {
            position: ChunkLogPosition::of(log)?,
            address: log.address(),
            topics: log.topics().to_vec(),
            data: log.data().data.clone(),
            removed: log.removed,
        })
    }
}

/// Name the refusal for two logs claiming one coordinate: an identical repeat is the
/// duplicate/order refusal (one log stated twice), while differing facts are two logs
/// fighting over one position, which is contradictory evidence.
fn conflict_or_duplicate(existing: &ChunkLogFacts, claimed: &ChunkLogFacts) -> LogIdentityRefusal {
    if *existing == *claimed {
        LogIdentityRefusal::LogsOutOfOrder {
            previous: claimed.position.log_index,
            actual: claimed.position.log_index,
        }
    } else {
        LogIdentityRefusal::ConflictingCoordinates {
            block_number: claimed.position.block_number,
            log_index: claimed.position.log_index,
        }
    }
}

/// Why a chunk log has no exact coordinate, or why a chunk's log set contradicts
/// itself.
///
/// Every variant is a deterministic statement about the evidence: retrying the same
/// fetch cannot change it, so a chunk that raises one must fail before its cursor
/// advances rather than dropping the log or defaulting its position to zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LogIdentityRefusal {
    /// The log names no block height, so it belongs to no block.
    #[error("a chunk log names no block number")]
    MissingBlockNumber,
    /// The log names no block hash: a height alone cannot distinguish a same-height
    /// branch.
    #[error("the chunk log at block {block_number} names no block hash")]
    MissingBlockHash {
        /// The height the log claims.
        block_number: u64,
    },
    /// The log names no transaction, so it has no transaction identity.
    #[error("the chunk log at block {block_number} names no transaction hash")]
    MissingTransactionHash {
        /// The height the log claims.
        block_number: u64,
    },
    /// The log names no transaction index, so its transaction has no position in the
    /// block.
    #[error("transaction {transaction_hash} of block {block_number} names no index")]
    MissingTransactionIndex {
        /// The height the transaction is in.
        block_number: u64,
        /// The transaction the log claims.
        transaction_hash: B256,
    },
    /// The log names no block-wide index, so it has no position within its block.
    #[error("transaction {transaction_hash} of block {block_number} has a log with no index")]
    MissingLogIndex {
        /// The height the transaction is in.
        block_number: u64,
        /// The transaction the log claims.
        transaction_hash: B256,
    },
    /// Two different logs claim one coordinate: the chunk's own log set contains
    /// contradictory evidence (a same-height branch, or two transactions reported at
    /// one position).
    #[error("log {log_index} of block {block_number} is claimed by two different logs")]
    ConflictingCoordinates {
        /// The height both logs claim.
        block_number: u64,
        /// The block-wide index both logs claim.
        log_index: u64,
    },
    /// The log is reported as removed: it describes a log the chain no longer states,
    /// so it has no coordinate to place and cannot enter a chunk's evidence.
    #[error("the chunk log at block {block_number} index {log_index} is removed")]
    RemovedLog {
        /// The height the log claims.
        block_number: u64,
        /// The block-wide index the log claims.
        log_index: u64,
    },
    /// The log's own block is not the block a group claims.
    #[error("a group of block {block_number} holds a log of block {actual}")]
    ForeignBlockInGroup {
        /// The group's height.
        block_number: u64,
        /// The log's height.
        actual: u64,
    },
    /// The log's transaction is not the transaction a group claims.
    #[error("a group of transaction {transaction_hash} holds a log of another transaction")]
    ForeignTransactionInGroup {
        /// The group's transaction hash.
        transaction_hash: B256,
    },
    /// The group's logs do not ascend by block-wide index.
    #[error("a group's log index {actual} does not follow {previous}")]
    LogsOutOfOrder {
        /// The index of the log before it.
        previous: u64,
        /// The offending index.
        actual: u64,
    },
    /// The group's block is not the anchor the caller is attributing it to: the
    /// coordinates cannot become a transaction reference of that block.
    #[error(
        "block {block_number} hashes to {block_hash}, but the anchor is block {anchor_number} {anchor_hash}"
    )]
    AnchorMismatch {
        /// The group's height.
        block_number: u64,
        /// The group's block hash.
        block_hash: B256,
        /// The anchor's height.
        anchor_number: u64,
        /// The anchor's own hash.
        anchor_hash: B256,
    },
}

/// Merge a supplemental fetch batch into the chunk's log set: de-duplicate by exact
/// coordinates against `logs`, extend, and restore the chronological sort. The ONE
/// merge seam for every same-chunk supplemental fetch (scaled tokens, stkAAVE,
/// endpoint replacements).
///
/// Identity is the full coordinate — block hash, height, transaction, transaction
/// index, and block-wide log index — so a supplemental pass that answers a log of a
/// DIFFERENT block at the same height is contradictory evidence rather than a
/// duplicate to drop (ADR-105 D1).
///
/// # Errors
///
/// One [`LogIdentityRefusal`] per log of either side that carries no exact position,
/// and [`LogIdentityRefusal::ConflictingCoordinates`] when two different logs claim
/// one coordinate.
pub fn merge_chunk_log_extension(
    logs: &mut Vec<Log>,
    extra: Vec<Log>,
) -> Result<(), LogIdentityRefusal> {
    let mut held: HashMap<(u64, u64), ChunkLogFacts> = HashMap::with_capacity(logs.len());
    for log in logs.iter() {
        let facts = ChunkLogFacts::of(log)?;
        let key = (facts.position.block_number, facts.position.log_index);
        if let Some(existing) = held.get(&key) {
            return Err(conflict_or_duplicate(existing, &facts));
        }
        held.insert(key, facts);
    }
    if extra.is_empty() {
        return Ok(());
    }
    let mut fresh = Vec::with_capacity(extra.len());
    for log in extra {
        let facts = ChunkLogFacts::of(&log)?;
        let key = (facts.position.block_number, facts.position.log_index);
        if let Some(existing) = held.get(&key) {
            if *existing == facts {
                continue;
            }
            return Err(conflict_or_duplicate(existing, &facts));
        }
        held.insert(key, facts);
        fresh.push(log);
    }
    if fresh.is_empty() {
        return Ok(());
    }
    logs.extend(fresh);
    sort_logs_by_block_and_index(logs);
    Ok(())
}

// ── warm-boot substrate resolution (the chain-current reserve set) ──────
//
// The chunk loop's warm boot needs the Pool/Configurator/Oracle addresses,
// the two contract revisions, and the reserve assets seeded before it runs.
// The reserve set comes from the Pool's own `getReservesList()` rather than
// from a span's Pool logs: an upgrade window's Pool pass carries almost no
// logs (an `Upgraded` event emits from the proxy, not the Pool), so a
// span-log-derived candidate set is empty and the resolver has nothing to
// seed. `getReservesList()` is the chain's authoritative reserve set,
// independent of any window's log density.

/// One reserve's `(underlying, a_token, v_token)` triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AaveReserve {
    /// The reserve underlying ERC-20.
    pub underlying: Address,
    /// The reserve's aToken.
    pub a_token: Address,
    /// The reserve's variable-debt token.
    pub v_token: Address,
}

/// The warm-boot substrate the chunk loop's seeded DB carries: the Pool-side
/// contract addresses, their revisions, and the chain-current reserve set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AaveSubstrate {
    /// The Pool contract.
    pub pool: Address,
    /// The Pool configurator contract.
    pub configurator: Address,
    /// The price oracle contract.
    pub price_oracle: Address,
    /// The Pool data provider contract (`getPoolDataProvider()`). The
    /// `PoolDataProviderUpdated` apply's UPDATE-by-old-address path needs the
    /// warm-boot row present, so the seed carries it at the cursor's address.
    pub data_provider: Address,
    /// `POOL_REVISION()` — the ops parser's scaled-amount tolerance gate
    /// reads it (a NULL revision hard-errors the parse).
    pub pool_revision: u64,
    /// `CONFIGURATOR_REVISION()` — seeded for row fidelity with the loop's
    /// own cold-boot substrate.
    pub configurator_revision: u64,
    /// The reserve set, sorted by underlying (a deterministic seed order).
    pub reserves: Vec<AaveReserve>,
}

/// Resolve the warm-boot substrate over a provider (a side channel — these
/// are harness inputs, not the chunk loop's recorded RPC surface).
///
/// `at_block` pins the resolution to one block: a warm boot that spans a
/// contract-migration event (`PoolDataProviderUpdated`/`PoolUpdated`) must
/// seed the address the event's UPDATE-by-old-address path expects (the
/// pre-window address), so the caller passes the window cursor. `None`
/// resolves at the node's latest head.
///
/// # Errors
///
/// Returns the underlying [`degenbot_core::errors::ProviderError`] when an
/// `eth_call` fails or a short/malformed return comes back.
pub async fn resolve_aave_substrate(
    transport: &impl ContractRead,
    address_provider: Address,
) -> ProviderResult<AaveSubstrate> {
    let pool = call_address(transport, address_provider, "getPool()").await?;
    let configurator = call_address(transport, address_provider, "getPoolConfigurator()").await?;
    let price_oracle = call_address(transport, address_provider, "getPriceOracle()").await?;
    let data_provider = call_address(transport, address_provider, "getPoolDataProvider()").await?;
    let pool_revision = call_uint(transport, pool, "POOL_REVISION()").await?;
    let configurator_revision =
        call_uint(transport, configurator, "CONFIGURATOR_REVISION()").await?;

    let list_ret = transport
        .call(pool, no_arg_calldata("getReservesList()"), None)
        .await?;
    let underlyings = decode_address_array(&list_ret);

    let mut reserves = Vec::with_capacity(underlyings.len());
    for underlying in underlyings {
        if let Some((a_token, v_token)) = reserve_tokens(transport, pool, underlying).await? {
            reserves.push(AaveReserve {
                underlying,
                a_token,
                v_token,
            });
        }
    }
    reserves.sort_by_key(|r| r.underlying);

    Ok(AaveSubstrate {
        pool,
        configurator,
        price_oracle,
        data_provider,
        pool_revision,
        configurator_revision,
        reserves,
    })
}

/// Build the 4-byte selector for a no-arg signature.
fn no_arg_calldata(signature: &str) -> Bytes {
    Bytes::from(alloy::primitives::keccak256(signature.as_bytes())[..4].to_vec())
}

/// The decode failure a fixed-width substrate read reports: the signature the
/// caller asked for, then the decoder's own reason.
fn decode_failure(signature: &str, error: &alloy::sol_types::Error) -> ProviderError {
    ProviderError::DecodingError {
        message: format!("{signature} decode: {error}"),
    }
}

/// `eth_call` a no-arg view returning an `address` (word 0).
///
/// The word crosses Alloy's VALIDATING `address` decoder: a return shorter than
/// one word, or one whose high twelve bytes are dirty, is a typed decode
/// failure rather than a panic on the slice or an address assembled from bits
/// the contract never answered.
async fn call_address(
    transport: &impl ContractRead,
    target: Address,
    signature: &str,
) -> ProviderResult<Address> {
    let ret = transport
        .call(target, no_arg_calldata(signature), None)
        .await?;
    decode_address_answer(signature, &ret)
}

/// The decode half of [`call_address`], over the return bytes.
fn decode_address_answer(signature: &str, ret: &[u8]) -> ProviderResult<Address> {
    Address::abi_decode_validate(ret).map_err(|e| decode_failure(signature, &e))
}

/// `eth_call` a no-arg view returning a `uint256` (word 0), as a `u64`.
///
/// The word is decoded at its declared `uint256` width and then narrowed with a
/// checked conversion: a short answer and a value past `u64` are both typed
/// failures, never a zero or a truncated low eight bytes.
async fn call_uint(
    transport: &impl ContractRead,
    target: Address,
    signature: &str,
) -> ProviderResult<u64> {
    let ret = transport
        .call(target, no_arg_calldata(signature), None)
        .await?;
    decode_uint_answer(signature, &ret)
}

/// The decode half of [`call_uint`], over the return bytes.
fn decode_uint_answer(signature: &str, ret: &[u8]) -> ProviderResult<u64> {
    let word: U256 = U256::abi_decode_validate(ret).map_err(|e| decode_failure(signature, &e))?;
    u64::try_from(word).map_err(|_| ProviderError::DecodingError {
        message: format!("{signature} answered {word}, which does not fit a u64"),
    })
}

/// Decode an ABI-encoded `address[]` return.
///
/// The array and every address word inside it cross Alloy's VALIDATING decoder,
/// so a malformed return — a short or misaligned array, or an element whose high
/// twelve bytes are dirty — yields an empty set instead of addresses assembled
/// from bytes the contract never answered.
fn decode_address_array(ret: &[u8]) -> Vec<Address> {
    Vec::<Address>::abi_decode_validate(ret).unwrap_or_default()
}

/// `getReserveData(address)` -> the `(a_token, v_token)` pair (the address
/// words of the Aave 3.2+ return shape), or `None` for a non-reserve (the
/// Pool returns a zeroed struct).
async fn reserve_tokens(
    transport: &impl ContractRead,
    pool: Address,
    candidate: Address,
) -> ProviderResult<Option<(Address, Address)>> {
    let selector = &alloy::primitives::keccak256("getReserveData(address)".as_bytes())[..4];
    let mut calldata = Vec::with_capacity(36);
    calldata.extend_from_slice(selector);
    calldata.extend_from_slice(&[0u8; 12]);
    calldata.extend_from_slice(candidate.as_slice());
    let ret = transport.call(pool, Bytes::from(calldata), None).await?;
    if ret.len() < 11 * 32 {
        return Ok(None);
    }
    // The a/v token addresses are words 8 and 10 of the return; each crosses the
    // validating `address` decoder, so a non-canonical word is treated as "not a
    // reserve" exactly like a zeroed one instead of being truncated into an
    // address.
    let word = |i: usize| -> &[u8] { &ret[i * 32..(i + 1) * 32] };
    let (Ok(a_token), Ok(v_token)) = (
        Address::abi_decode_validate(word(8)),
        Address::abi_decode_validate(word(10)),
    ) else {
        return Ok(None);
    };
    if a_token.is_zero() || v_token.is_zero() {
        return Ok(None);
    }
    Ok(Some((a_token, v_token)))
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
#[expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use alloy::primitives::{Bytes, Log as AlloyLog};
    use std::sync::Arc;

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

    /// A same-chunk `PriceOracleUpdated` A→B replacement must produce a
    /// supplemental oracle-pass fetch against the NEW oracle address over the
    /// chunk range — the main pass fetched only from A, so the replacement
    /// endpoint's `AssetSourceUpdated` logs are otherwise unreachable.
    #[test]
    fn price_oracle_replacement_fetches_new_asset_source_logs() {
        use degenbot_rpc::cassette::Cassette;
        use degenbot_rpc::cassette_replay::CassetteReplayTransport;

        let old_oracle = Address::repeat_byte(0x5a);
        let new_oracle = Address::repeat_byte(0xab);
        let provider = Address::repeat_byte(0x77);
        let spec = AaveFetchSpec {
            oracle_address: Some(old_oracle),
            ..AaveFetchSpec::default()
        };
        let log = make_log(
            provider,
            vec![
                AAVE_PRICE_ORACLE_UPDATED_TOPIC,
                old_oracle.into_word(),
                new_oracle.into_word(),
            ],
            100,
            0,
            [0xaa; 32],
        );
        let replacements = chunk_endpoint_replacements(std::slice::from_ref(&log), &spec);
        assert_eq!(replacements.oracle, vec![new_oracle]);
        assert!(replacements.pool.is_empty());

        // The supplemental oracle-pass filter: the NEW address, the chunk
        // range, the AssetSourceUpdated topic — the canonical ledger-key form
        // (lowercase emitter, decimal block tags, flat single topic group).
        let key = format!(
            "[\"eth_getLogs\",[{{\"address\":\"0x{:x}\",\"fromBlock\":\"100\",\"toBlock\":\"105\",\"topics\":[\"0x{}\"]}}]]",
            new_oracle,
            alloy::hex::encode(AAVE_ASSET_SOURCE_UPDATED_TOPIC)
        );
        let served_log = format!(
            r#"{{"address":"0x{:x}","topics":["0x{}"],"data":"0x","blockNumber":"0x64","transactionHash":"0x{}","transactionIndex":"0x0","blockHash":"0x{}","logIndex":"0x1","removed":false}}"#,
            new_oracle,
            alloy::hex::encode(AAVE_ASSET_SOURCE_UPDATED_TOPIC),
            "aa".repeat(32),
            "bb".repeat(32),
        );
        let response_json = format!(r#"{{"result":[{served_log}]}}"#);
        let digest = alloy::primitives::keccak256(response_json.as_bytes()).to_string();
        let escaped_key = key.escape_default().to_string();
        let json = format!(
            "{{\"schema\":\"degenbot.cassette/v1\",\"chain_id\":1,             \"provenance\":{{\"source\":\"test\",\"recorded_at\":\"1970-01-01T00:00:00Z\",             \"span\":{{\"from_block\":0,\"to_block\":0}}}},\"entries\":{{\"{escaped_key}\": {{\"response\": {response_json}, \"digest\": \"{digest}\"}}}}}}"
        );
        let transport = CassetteReplayTransport::new(
            Cassette::from_json_str(&json).expect("the in-memory test cassette parses"),
        );
        let fetcher = LogFetcher::new(Arc::new(transport.as_alloy_provider()), 1_000);

        let extra = degenbot_core::runtime::get_runtime()
            .block_on(fetch_endpoint_replacement_logs(
                &fetcher,
                100,
                105,
                &replacements,
            ))
            .expect("the replacement pass serves its recorded filter");
        assert_eq!(
            extra.len(),
            1,
            "the new oracle's AssetSourceUpdated log arrives"
        );
        assert_eq!(extra[0].address(), new_oracle);

        let served = transport.served_snapshot();
        assert_eq!(served.requests, 1, "exactly one supplemental round trip");
        assert_eq!(
            served.per_method.get("eth_getLogs").map(|m| m.served),
            Some(1)
        );
    }

    /// The no-replacement wire shape: logs without endpoint replacement
    /// events — and replacements whose new address equals the chunk-start
    /// endpoint, and oracle replacements under a `None` spec address (the
    /// main pass is chain-wide in discovery mode) — produce an EMPTY plan, so
    /// an unchanged run issues no supplemental `eth_getLogs` and its request
    /// set stays identical.
    #[test]
    fn unchanged_endpoint_fetch_plan_is_unchanged() {
        let old_oracle = Address::repeat_byte(0x5a);
        let old_pool = Address::repeat_byte(0x01);
        let provider = Address::repeat_byte(0x77);
        let spec = AaveFetchSpec {
            pool_address: old_pool,
            oracle_address: Some(old_oracle),
            ..AaveFetchSpec::default()
        };
        let mut id_bytes = [0u8; 32];
        id_bytes[..4].copy_from_slice(b"POOL");
        let pool_id = B256::from(id_bytes);
        let mut other_id_bytes = [0u8; 32];
        other_id_bytes[..13].copy_from_slice(b"SOME_OTHER_ID");
        let other_id = B256::from(other_id_bytes);
        let logs = vec![
            make_log(
                provider,
                vec![
                    AAVE_PRICE_ORACLE_UPDATED_TOPIC,
                    old_oracle.into_word(),
                    old_oracle.into_word(),
                ],
                100,
                0,
                [0xaa; 32],
            ),
            make_log(
                provider,
                vec![
                    AAVE_ADDRESS_SET_TOPIC,
                    pool_id,
                    old_pool.into_word(),
                    old_pool.into_word(),
                ],
                100,
                1,
                [0xaa; 32],
            ),
            make_log(
                provider,
                vec![
                    AAVE_ADDRESS_SET_TOPIC,
                    other_id,
                    Address::ZERO.into_word(),
                    Address::repeat_byte(0x33).into_word(),
                ],
                100,
                2,
                [0xaa; 32],
            ),
        ];
        assert!(
            chunk_endpoint_replacements(&logs, &spec).is_empty(),
            "no replacement event means no supplemental fetch"
        );

        let discovery_spec = AaveFetchSpec {
            oracle_address: None,
            ..spec
        };
        let registration = make_log(
            provider,
            vec![
                AAVE_PRICE_ORACLE_UPDATED_TOPIC,
                Address::ZERO.into_word(),
                Address::repeat_byte(0x44).into_word(),
            ],
            100,
            3,
            [0xaa; 32],
        );
        assert!(
            chunk_endpoint_replacements(&[registration], &discovery_spec).is_empty(),
            "a chain-wide discovery pass already covers every oracle"
        );
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

    /// A dummy `AlloyProvider` pointing at a dead URL: every fetch that
    /// reached the wire would fail, so an `Ok` is proof the guard answered
    /// before any round trip.
    use degenbot_rpc::provider::AlloyProvider;

    fn dead_url_provider() -> AlloyProvider {
        degenbot_core::runtime::get_runtime()
            .block_on(AlloyProvider::new("http://127.0.0.1:1", 1))
            .expect("a dead-URL provider constructs without contact")
    }

    #[test]
    fn fetch_scaled_token_logs_empty_addresses_returns_empty() {
        let fetcher = LogFetcher::new(Arc::new(dead_url_provider()), 1_000);
        let logs = degenbot_core::runtime::get_runtime()
            .block_on(fetch_scaled_token_logs(&fetcher, 1, 1_000, &[]))
            .expect("the empty-address guard short-circuits before any round trip");
        assert!(logs.is_empty(), "no addresses means no logs");
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

    /// The golden capture records the `eth_getLogs` filter each pass issued for
    /// one chunk: one checksummed emitter (or the scaled-token set), one
    /// topic-position OR-set, and the chunk's block range. The cassette answers
    /// an exact-semantics filter only, so a changed address set, a changed topic
    /// set, a different block range, or an extra round trip is a red instead of
    /// a passing fetch. The stkAAVE pass has no recorded entry because this
    /// market's GHO row carries no discount token — its `None` answers
    /// `Ok(empty)` before the wire.
    #[test]
    fn each_fetch_pass_issues_the_recorded_log_filter() {
        use degenbot_rpc::cassette::Cassette;
        use degenbot_rpc::cassette_replay::CassetteReplayTransport;

        const CAPTURE: &str = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../../tests/fixtures/cassettes/aave_update_chunk_26130440-26130445.json"
        );

        let bytes = degenbot_rpc::cassette::read_artifact(CAPTURE)
            .expect("the committed chunk capture must exist");
        let transport = CassetteReplayTransport::new(
            Cassette::from_json_bytes(&bytes).expect("the committed capture is a v1 cassette"),
        );
        let fetcher = LogFetcher::new(Arc::new(transport.as_alloy_provider()), 1_000);
        let spec = AaveFetchSpec {
            pool_address: "0x87870bca3f3fd6335c3f4ce8392d69350b4fa4e2"
                .parse()
                .unwrap(),
            configurator_address: "0x64b761d848206f447fe2dd461b0c635ec39ebb27"
                .parse()
                .unwrap(),
            address_provider_address: "0x2f39d218133afab8f2b819b1066c7e434ad94e9e"
                .parse()
                .unwrap(),
            oracle_address: Some(
                "0x54586be62e3c3580375ae3723c145253060ca0c2"
                    .parse()
                    .unwrap(),
            ),
            scaled_token_addresses: [
                "0x23878914efe38d27c4d67ab83ed1b93a74d4086a",
                "0x4d5f47fa6a74757f35c14fd3a6ef8e3c9bc514e8",
                "0x6df1c1e379bc5a00a7b4c6e67a203333772f45a8",
                "0x72e95b8931767c79ba4eee721354d6e99a61d004",
                "0x98c23e9d8f34fefb1b7bd6a91b7ff122f4e16f5c",
                "0xea51d7853eefb32b6ee06b1c12e6dcca88be0ffe",
            ]
            .iter()
            .map(|a| a.parse().unwrap())
            .collect(),
            stk_aave_address: None,
        };

        let logs = degenbot_core::runtime::get_runtime()
            .block_on(fetch_aave_chunk_logs(
                &spec, &fetcher, 26_130_440, 26_130_445,
            ))
            .expect("every pass matches the filter the capture recorded for it");
        assert!(
            !logs.is_empty(),
            "the capture carries logs for the passes it recorded"
        );

        let served = transport.served_snapshot();
        assert_eq!(
            served.requests, 6,
            "one round trip per recorded filter, and none the capture omits"
        );
        assert_eq!(
            served.per_method.get("eth_getLogs").map(|m| m.served),
            Some(6),
            "six passes reach the wire; the stkAAVE pass has no discount token"
        );
    }

    /// The resolver seeds from the Pool's `getReservesList()`, not from a
    /// span's Pool logs — the sparse-logs upgrade-window case. The offline
    /// provider serves the resolver's exact call set; a `getReservesList()`
    /// reserve set comes back even though no Pool logs exist at that block.
    #[test]
    fn resolve_aave_substrate_seeds_reserves_without_pool_logs() {
        use degenbot_rpc::offline::OfflineProvider;

        let ap = Address::from([0xa0; 20]);
        let pool = Address::from([0xb0; 20]);
        let configurator = Address::from([0xc0; 20]);
        let oracle = Address::from([0xd0; 20]);
        let data_provider = Address::from([0xe0; 20]);
        let underlying_1 = Address::from([0x11; 20]);
        let a_token_1 = Address::from([0x12; 20]);
        let v_token_1 = Address::from([0x13; 20]);
        let underlying_2 = Address::from([0x21; 20]);
        let a_token_2 = Address::from([0x22; 20]);
        let v_token_2 = Address::from([0x23; 20]);

        let word =
            |a: Address| -> String { format!("000000000000000000000000{}", alloy::hex::encode(a)) };
        let uint = |n: u64| -> String { format!("{n:064x}") };
        // getReservesList(): offset 0x20, length 2, then the two addresses.
        let reserves_list = format!(
            "{}{}{}{}",
            uint(0x20),
            uint(2),
            word(underlying_1),
            word(underlying_2)
        );
        // getReserveData(): the aToken lands at word 8, the vToken at word 10.
        let reserve_data = |a_token: Address, v_token: Address| -> String {
            let mut words = vec![uint(0); 11];
            words[8] = word(a_token);
            words[10] = word(v_token);
            words.concat()
        };

        let key = |to: Address, selector: &str| {
            format!("{}:0x{}", to.to_checksum(None).to_lowercase(), selector)
        };
        // The selectors the resolver computes (keccak256 of each signature).
        let sel =
            |sig: &str| alloy::hex::encode(&alloy::primitives::keccak256(sig.as_bytes())[..4]);
        let get_reserve_data = |underlying: Address| {
            format!(
                "{}000000000000000000000000{}",
                sel("getReserveData(address)"),
                alloy::hex::encode(underlying)
            )
        };

        let calls = [
            (key(ap, &sel("getPool()")), word(pool)),
            (key(ap, &sel("getPoolConfigurator()")), word(configurator)),
            (key(ap, &sel("getPriceOracle()")), word(oracle)),
            (key(ap, &sel("getPoolDataProvider()")), word(data_provider)),
            (key(pool, &sel("POOL_REVISION()")), uint(11)),
            (key(configurator, &sel("CONFIGURATOR_REVISION()")), uint(8)),
            (key(pool, &sel("getReservesList()")), reserves_list),
            (
                key(pool, &get_reserve_data(underlying_1)),
                reserve_data(a_token_1, v_token_1),
            ),
            (
                key(pool, &get_reserve_data(underlying_2)),
                reserve_data(a_token_2, v_token_2),
            ),
        ];
        let calls_json: Vec<String> = calls
            .iter()
            .map(|(k, v)| format!("\"{k}\":\"{v}\""))
            .collect();
        let recorded = format!(
            "{{\"chain_id\":1,\"block_number\":22839357,\"calls\":{{{}}}}}",
            calls_json.join(",")
        );

        let provider = OfflineProvider::from_json_str(&recorded)
            .unwrap()
            .as_alloy_provider();
        let substrate = degenbot_core::runtime::get_runtime()
            .block_on(resolve_aave_substrate(&provider, ap))
            .unwrap();

        assert_eq!(substrate.pool, pool);
        assert_eq!(substrate.configurator, configurator);
        assert_eq!(substrate.price_oracle, oracle);
        assert_eq!(substrate.data_provider, data_provider);
        assert_eq!(substrate.pool_revision, 11);
        assert_eq!(substrate.configurator_revision, 8);
        assert_eq!(
            substrate.reserves,
            vec![
                AaveReserve {
                    underlying: underlying_1,
                    a_token: a_token_1,
                    v_token: v_token_1,
                },
                AaveReserve {
                    underlying: underlying_2,
                    a_token: a_token_2,
                    v_token: v_token_2,
                },
            ],
            "the resolver seeds the getReservesList set with no Pool logs present"
        );
    }

    // ── the fixed-width substrate decoders ──

    /// The no-arg calldata is the bare selector, arguments and all: `getPool()`
    /// hashes to `0x026b1d5f`, so a substrate call is byte-identical to what the
    /// contract's own ABI names.
    #[test]
    fn no_arg_calldata_is_the_unchanged_selector() {
        assert_eq!(
            no_arg_calldata("getPool()").to_vec(),
            vec![0x02, 0x6b, 0x1d, 0x5f],
            "getPool() selector, unchanged"
        );
        assert_eq!(
            no_arg_calldata("POOL_REVISION()").to_vec(),
            vec![0x01, 0x48, 0x17, 0x0e],
            "POOL_REVISION() selector, unchanged"
        );
    }

    /// A canonical `address` word — twelve zero bytes above the twenty — reads
    /// back whole.
    #[test]
    fn address_answer_reads_a_canonical_word() {
        let pool = Address::from([0x2a; 20]);

        assert_eq!(
            decode_address_answer("getPool()", &pool.abi_encode()).unwrap(),
            pool
        );
    }

    /// The padding is part of the type: a word whose high twelve bytes are dirty
    /// is a decode failure, never the low-twenty address a bare slice would
    /// assemble.
    #[test]
    fn address_answer_refuses_a_dirty_high_byte() {
        let mut word = [0u8; 32];
        word[0] = 0x01;
        word[31] = 0x2a;

        let err = decode_address_answer("getPool()", &word).unwrap_err();

        let ProviderError::DecodingError { message } = err else {
            panic!("a dirty address word is a decode failure, got {err:?}");
        };
        assert!(
            message.contains("getPool()"),
            "the failure names the signature it decoded: {message}"
        );
    }

    /// A return short of one word fails at 0 and at 31 bytes — never an
    /// all-zero address assembled from an answer that never arrived.
    #[test]
    fn address_answer_refuses_a_short_return() {
        for len in [0usize, 31] {
            assert!(
                matches!(
                    decode_address_answer("getPool()", &vec![0u8; len]),
                    Err(ProviderError::DecodingError { .. })
                ),
                "a {len}-byte address answer is not a word"
            );
        }
    }

    /// A full `uint256` word inside `u64` reads as itself: the word decodes at
    /// its declared width and is narrowed afterwards.
    #[test]
    fn uint_answer_reads_words_that_fit_a_u64() {
        for v in [0u64, 1, u64::MAX] {
            assert_eq!(
                decode_uint_answer("POOL_REVISION()", &U256::from(v).abi_encode()).unwrap(),
                v
            );
        }
    }

    /// The narrowing is checked, so a full-width word past `u64` is refused
    /// rather than truncated to its low eight bytes.
    #[test]
    fn uint_answer_refuses_a_word_past_u64() {
        for over in [
            U256::from(u64::MAX) + U256::from(1u8),
            U256::from(1u8) << 200,
            U256::MAX,
        ] {
            let err = decode_uint_answer("POOL_REVISION()", &over.abi_encode()).unwrap_err();

            let ProviderError::DecodingError { message } = err else {
                panic!("{over} does not fit a u64, got {err:?}");
            };
            assert!(
                message.contains("does not fit a u64"),
                "the failure says why: {message}"
            );
        }
    }

    /// A return short of one word fails at 0 and at 31 bytes — never a zero
    /// revision.
    #[test]
    fn uint_answer_refuses_a_short_return() {
        for len in [0usize, 31] {
            assert!(
                matches!(
                    decode_uint_answer("POOL_REVISION()", &vec![0u8; len]),
                    Err(ProviderError::DecodingError { .. })
                ),
                "a {len}-byte uint answer is not a word"
            );
        }
    }

    /// A canonically encoded `address[]` reads element for element.
    #[test]
    fn address_array_decodes_a_canonical_return() {
        let expected = vec![Address::from([0x11; 20]), Address::from([0x22; 20])];

        assert_eq!(
            decode_address_array(&expected.abi_encode()),
            expected,
            "offset word, length word, then one canonical word per element"
        );
    }

    /// One non-canonical element fails the whole array: a malformed return is an
    /// empty reserve set, never a list holding an address assembled from bytes
    /// the Pool never answered.
    #[test]
    fn address_array_refuses_a_non_canonical_element() {
        let canonical = vec![Address::from([0x11; 20]), Address::from([0x22; 20])].abi_encode();
        let mut dirty = canonical.clone();
        dirty[64] = 0x01; // the first element word's high byte

        assert_eq!(
            decode_address_array(&canonical).len(),
            2,
            "the baseline reads"
        );
        assert!(
            decode_address_array(&dirty).is_empty(),
            "a dirty element word is not a reserve"
        );
    }

    /// A truncated array fails the same way instead of returning the elements
    /// that happened to fit.
    #[test]
    fn address_array_refuses_a_truncated_return() {
        let canonical = vec![Address::from([0x11; 20])].abi_encode();

        assert!(
            decode_address_array(&canonical[..canonical.len() - 1]).is_empty(),
            "a short element word is not an address"
        );
        assert!(decode_address_array(&[0u8; 63]).is_empty());
    }
}
