//! Ethereum RPC provider implementation using Alloy.
//!
//! Provides high-performance HTTP/HTTPS/WS connections with connection pooling,
//! retry logic, and chunked log fetching. Also supports IPC endpoints for
//! local node connections.

use crate::errors::{ProviderError, ProviderResult};
use alloy::consensus::{Header as ConsensusHeader, TxEnvelope};
use alloy::network::Ethereum;
use alloy::primitives::{Address, Bytes, B256, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::eth::{Block, Header as RpcHeader, Transaction};
use alloy::rpc::types::{Filter, Log};
use alloy::transports::ipc::IpcConnect;
use alloy::transports::ws::WsConnect;
use alloy::transports::{RpcError, TransportErrorKind};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

/// Constants for retry logic.
const INITIAL_RETRY_DELAY_MS: u64 = 100;
const MAX_RETRY_DELAY_MS: u64 = 30_000; // 30 seconds
const BACKOFF_MULTIPLIER: u64 = 2;
const MAX_JITTER_MS: u64 = 100; // Add up to 100ms of jitter

/// Default maximum retry attempts for provider operations.
pub const DEFAULT_MAX_RETRIES: u32 = 10;

/// Convert an Alloy `RpcError` to a `ProviderError` with appropriate classification.
///
/// Uses type-based matching on the Alloy error enum instead of string scraping:
/// - `RpcError::Transport(TransportErrorKind::HttpError)` with 429 → `RateLimited`
/// - `RpcError::Transport(TransportErrorKind::HttpError)` with 5xx → `ConnectionFailed`
/// - `RpcError::Transport` with retryable transport errors → `Timeout`
/// - `RpcError::ErrorResp` → `RpcError` with the JSON-RPC error code
/// - `RpcError::LocalUsageError` → `Other`
/// - Other → `RpcError` with code -1
fn alloy_error_to_provider_error(e: &RpcError<TransportErrorKind>, context: &str) -> ProviderError {
    let message = format!("{context}: {e}");

    // Check for transport-level errors
    if let Some(transport_err) = e.as_transport_err() {
        // HTTP 429 = rate limited
        if let Some(http_err) = transport_err.as_http_error() {
            let status = http_err.status;
            if status == 429 {
                return ProviderError::RateLimited { message };
            }
            // 5xx server errors are retryable as connection failures
            if (500..600).contains(&status) {
                return ProviderError::ConnectionFailed { message };
            }
        }

        // Backend gone or pubsub unavailable = connection failed
        if transport_err.is_backend_gone() || transport_err.is_pubsub_unavailable() {
            return ProviderError::ConnectionFailed { message };
        }

        // Use Alloy's built-in retry heuristic for other transport errors
        if transport_err.is_retry_err() {
            return ProviderError::Timeout {
                timeout: 30,
                message,
            };
        }

        // Other transport errors → RPC error
        return ProviderError::RpcError { code: -1, message };
    }

    // Server returned an error response (JSON-RPC error)
    if let Some(error_resp) = e.as_error_resp() {
        return ProviderError::RpcError {
            code: error_resp.code,
            message,
        };
    }

    // Local usage errors (signer errors, pre-processing failures)
    if e.is_local_usage_error() {
        return ProviderError::Other { message };
    }

    // Serialization/deserialization errors
    if e.is_ser_error() || e.is_deser_error() {
        return ProviderError::SerializationError { message };
    }

    // Fallback
    ProviderError::RpcError { code: -1, message }
}

/// Filter criteria for log fetching.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LogFilter {
    pub from_block: Option<u64>,
    pub to_block: Option<u64>,
    pub addresses: Vec<String>,
    pub topics: Vec<Vec<String>>,
}

impl LogFilter {
    /// Create a new `LogFilter`.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::InvalidBlockRange` if `from_block > to_block`.
    pub fn new(
        from_block: u64,
        to_block: u64,
        addresses: Option<Vec<String>>,
        topics: Option<Vec<Vec<String>>>,
    ) -> ProviderResult<Self> {
        if from_block > to_block {
            return Err(ProviderError::InvalidBlockRange {
                from: from_block,
                to: to_block,
            });
        }

        Ok(Self {
            from_block: Some(from_block),
            to_block: Some(to_block),
            addresses: addresses.unwrap_or_default(),
            topics: topics.unwrap_or_default(),
        })
    }

    /// Convert to Alloy Filter.
    pub fn to_alloy_filter(&self) -> ProviderResult<Filter> {
        let mut filter = Filter::new();

        if let Some(from) = self.from_block {
            filter = filter.from_block(from);
        }

        if let Some(to) = self.to_block {
            filter = filter.to_block(to);
        }

        // Convert addresses
        if !self.addresses.is_empty() {
            let addresses: Vec<Address> = self
                .addresses
                .iter()
                .map(|addr| {
                    crate::address_utils::parse_address(addr).map_err(|e| ProviderError::InvalidAddress {
                        address: addr.clone(),
                        reason: format!("{e}"),
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;

            filter = filter.address(addresses);
        }

        // Convert topics — each element maps to a topic position (0-3)
        if !self.topics.is_empty() {
            for (i, topic_list) in self.topics.iter().enumerate() {
                if topic_list.is_empty() {
                    continue;
                }
                let topics: Vec<B256> = topic_list
                    .iter()
                    .map(|t| {
                        B256::from_str(t).map_err(|e| ProviderError::InvalidTopic {
                            topic: t.clone(),
                            reason: format!("{e}"),
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                filter = match i {
                    0 => filter.event_signature(topics),
                    1 => filter.topic1(topics),
                    2 => filter.topic2(topics),
                    3 => filter.topic3(topics),
                    _ => {
                        return Err(ProviderError::InvalidTopic {
                            topic: String::new(),
                            reason: format!("topic position {i} is out of range (max 3)"),
                        })
                    }
                };
            }
        }

        Ok(filter)
    }
}

/// High-performance Ethereum RPC provider.
pub struct AlloyProvider {
    inner: Arc<dyn Provider<Ethereum>>,
    rpc_url: String,
    max_retries: u32,
}

impl Clone for AlloyProvider {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            rpc_url: self.rpc_url.clone(),
            max_retries: self.max_retries,
        }
    }
}

impl AlloyProvider {
    /// Create a new provider with the given RPC URL.
    ///
    /// Automatically detects the connection type based on the URL:
    /// - HTTP/HTTPS URLs use HTTP transport with connection pooling
    /// - File paths (starting with / or \) use IPC transport
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::ConnectionFailed` if the HTTP client cannot be created
    /// or IPC connection fails.
    pub async fn new(rpc_url: &str, max_retries: u32) -> ProviderResult<Self> {
        let provider: Arc<dyn Provider<Ethereum>> =
            if rpc_url.starts_with("http://") || rpc_url.starts_with("https://") {
                // HTTP endpoint - use Alloy's built-in HTTP transport
                let url = rpc_url
                    .parse()
                    .map_err(|e| ProviderError::ConnectionFailed {
                        message: format!("Invalid RPC URL: {e}"),
                    })?;

                let provider = ProviderBuilder::<_, _, Ethereum>::new().connect_http(url);
                Arc::new(provider)
            } else if rpc_url.starts_with("ws://") || rpc_url.starts_with("wss://") {
                // WebSocket connection
                let ws_connect = WsConnect::new(rpc_url.to_string());
                let provider = ProviderBuilder::<_, _, Ethereum>::new()
                    .connect_ws(ws_connect)
                    .await
                    .map_err(|e| ProviderError::ConnectionFailed {
                        message: format!("Failed to connect to WebSocket endpoint: {e}"),
                    })?;
                Arc::new(provider)
            } else {
                // IPC connection via Unix domain socket or Windows named pipe
                let ipc_connect: IpcConnect<String> = IpcConnect::new(rpc_url.to_string());
                let provider = ProviderBuilder::<_, _, Ethereum>::new()
                    .connect_ipc(ipc_connect)
                    .await
                    .map_err(|e| ProviderError::ConnectionFailed {
                        message: format!("Failed to connect to IPC endpoint: {e}"),
                    })?;
                Arc::new(provider)
            };

        Ok(Self {
            inner: provider,
            rpc_url: rpc_url.to_string(),
            max_retries,
        })
    }

    /// Get current block number.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_block_number(&self) -> ProviderResult<u64> {
        self.retry_with_backoff(|| async {
            let result: u64 = self
                .inner
                .get_block_number()
                .await
                .map_err(|e| alloy_error_to_provider_error(&e, "Failed to get block number"))?;
            Ok(result)
        })
        .await
    }

    /// Get chain ID.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_chain_id(&self) -> ProviderResult<u64> {
        self.retry_with_backoff(|| async {
            let result: u64 = self
                .inner
                .get_chain_id()
                .await
                .map_err(|e| alloy_error_to_provider_error(&e, "Failed to get chain ID"))?;
            Ok(result)
        })
        .await
    }

    /// Fetch logs with retry logic.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError` if the RPC call fails or filter is invalid.
    pub async fn get_logs(&self, filter: &LogFilter) -> ProviderResult<Vec<Log>> {
        let alloy_filter = filter.to_alloy_filter()?;

        self.retry_with_backoff(|| async {
            let result: Vec<Log> = self
                .inner
                .get_logs(&alloy_filter)
                .await
                .map_err(|e| alloy_error_to_provider_error(&e, "Failed to get logs"))?;
            Ok(result)
        })
        .await
    }

    /// Get contract code at an address.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_code(
        &self,
        address: &Address,
        block_number: Option<u64>,
    ) -> ProviderResult<Bytes> {
        self.retry_with_backoff(|| async {
            let result = if let Some(block) = block_number {
                self.inner
                    .get_code_at(*address)
                    .block_id(block.into())
                    .await
            } else {
                self.inner.get_code_at(*address).await
            }
            .map_err(|e| alloy_error_to_provider_error(&e, "Failed to get code"))?;

            Ok(result)
        })
        .await
    }

    /// Get a block by number.
    ///
    /// Returns the full block data including header and transactions.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_block(
        &self,
        block_number: u64,
    ) -> ProviderResult<Option<Block<Transaction<TxEnvelope>, RpcHeader<ConsensusHeader>>>> {
        use alloy::eips::BlockNumberOrTag;

        self.retry_with_backoff(|| async {
            let block_num_tag = BlockNumberOrTag::Number(block_number);
            let result = self
                .inner
                .get_block_by_number(block_num_tag)
                .await
                .map_err(|e| alloy_error_to_provider_error(&e, "Failed to get block"))?;

            Ok(result)
        })
        .await
    }

    /// Retry an async operation with exponential backoff.
    ///
    /// Uses exponential backoff with jitter to avoid thundering herd problems.
    /// All retryable errors (rate limit, timeout, connection failures) receive
    /// the same backoff treatment.
    async fn retry_with_backoff<F, Fut, T>(&self, operation: F) -> ProviderResult<T>
    where
        F: Fn() -> Fut + Send + Sync,
        Fut: std::future::Future<Output = ProviderResult<T>> + Send,
        T: Send,
    {
        let mut attempt = 0;
        let mut delay_ms = INITIAL_RETRY_DELAY_MS;

        loop {
            match operation().await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    attempt += 1;
                    if attempt >= self.max_retries {
                        return Err(e);
                    }

                    // Check if it's a retryable error
                    let is_retryable = matches!(
                        &e,
                        ProviderError::RateLimited { .. }
                            | ProviderError::Timeout { .. }
                            | ProviderError::ConnectionFailed { .. }
                    );

                    if !is_retryable {
                        return Err(e);
                    }

                    // Calculate delay with exponential backoff and jitter
                    // Use random_range for uniform distribution (avoids modulo bias)
                    let jitter = rand::rng().random_range(0..MAX_JITTER_MS);

                    let sleep_duration = Duration::from_millis(delay_ms + jitter);
                    tokio::time::sleep(sleep_duration).await;

                    // Exponential backoff with cap
                    delay_ms = std::cmp::min(delay_ms * BACKOFF_MULTIPLIER, MAX_RETRY_DELAY_MS);
                }
            }
        }
    }

    /// Get the RPC URL.
    #[must_use]
    pub fn rpc_url(&self) -> &str {
        &self.rpc_url
    }

    /// Execute an `eth_call`.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn eth_call(
        &self,
        to: &Address,
        data: Bytes,
        block_number: Option<u64>,
    ) -> ProviderResult<Bytes> {
        use alloy::rpc::types::TransactionRequest;

        self.retry_with_backoff(|| async {
            let tx = TransactionRequest::default()
                .to(*to)
                .input(data.clone().into());

            // Call at specific block if provided, otherwise use latest
            let result = if let Some(block) = block_number {
                self.inner.call(tx).block(block.into()).await
            } else {
                self.inner.call(tx).await
            }
            .map_err(|e| alloy_error_to_provider_error(&e, "eth_call failed"))?;

            Ok(result)
        })
        .await
    }

    /// Get the current gas price.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_gas_price(&self) -> ProviderResult<u128> {
        self.retry_with_backoff(|| async {
            let result: u128 = self
                .inner
                .get_gas_price()
                .await
                .map_err(|e| alloy_error_to_provider_error(&e, "Failed to get gas price"))?;
            Ok(result)
        })
        .await
    }

    /// Estimate gas for a transaction.
    ///
    /// # Arguments
    /// * `to` - Target address
    /// * `data` - Transaction data
    /// * `from` - Optional sender address
    /// * `value` - Optional value in wei
    /// * `block_number` - Optional block number to estimate at
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn estimate_gas(
        &self,
        to: &Address,
        data: Bytes,
        from: Option<&Address>,
        value: Option<u128>,
        block_number: Option<u64>,
    ) -> ProviderResult<u64> {
        use alloy::rpc::types::TransactionRequest;

        self.retry_with_backoff(|| async {
            let mut tx = TransactionRequest::default()
                .to(*to)
                .input(data.clone().into());

            if let Some(addr) = from {
                tx = tx.from(*addr);
            }

            if let Some(val) = value {
                tx = tx.value(alloy::primitives::U256::from(val));
            }

            // Estimate at specific block if provided, otherwise use pending
            let result = if let Some(block) = block_number {
                self.inner.estimate_gas(tx).block(block.into()).await
            } else {
                self.inner.estimate_gas(tx).await
            }
            .map_err(|e| alloy_error_to_provider_error(&e, "Failed to estimate gas"))?;

            Ok(result)
        })
        .await
    }

    /// Get a transaction by hash.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_transaction(
        &self,
        tx_hash: &str,
    ) -> ProviderResult<Option<serde_json::Value>> {
        use alloy::primitives::FixedBytes;
        use std::str::FromStr;

        let hash = FixedBytes::from_str(tx_hash).map_err(|e| ProviderError::InvalidParams {
            message: format!("Invalid transaction hash: {e}"),
        })?;

        self.retry_with_backoff(|| async {
            let result = self
                .inner
                .get_transaction_by_hash(hash)
                .await
                .map_err(|e| alloy_error_to_provider_error(&e, "Failed to get transaction"))?;

            // Convert to JSON value
            let json_value = result
                .map(|tx| {
                    serde_json::to_value(&tx).map_err(|e| ProviderError::SerializationError {
                        message: format!("Failed to serialize transaction: {e}"),
                    })
                })
                .transpose()?;

            Ok(json_value)
        })
        .await
    }

    /// Get a transaction receipt by hash.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_transaction_receipt(
        &self,
        tx_hash: &str,
    ) -> ProviderResult<Option<serde_json::Value>> {
        use alloy::primitives::FixedBytes;
        use std::str::FromStr;

        let hash = FixedBytes::from_str(tx_hash).map_err(|e| ProviderError::InvalidParams {
            message: format!("Invalid transaction hash: {e}"),
        })?;

        self.retry_with_backoff(|| async {
            let result = self
                .inner
                .get_transaction_receipt(hash)
                .await
                .map_err(|e| {
                    alloy_error_to_provider_error(&e, "Failed to get transaction receipt")
                })?;

            // Convert to JSON value
            let json_value = result
                .map(|receipt| {
                    serde_json::to_value(&receipt).map_err(|e| ProviderError::SerializationError {
                        message: format!("Failed to serialize transaction receipt: {e}"),
                    })
                })
                .transpose()?;

            Ok(json_value)
        })
        .await
    }

    /// Get storage at a given address and position.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_storage_at(
        &self,
        address: &Address,
        position: U256,
        block_number: Option<u64>,
    ) -> ProviderResult<B256> {
        use alloy::eips::BlockNumberOrTag;

        self.retry_with_backoff(|| async {
            let result = if let Some(block) = block_number {
                self.inner
                    .get_storage_at(*address, position)
                    .block_id(BlockNumberOrTag::Number(block).into())
                    .await
            } else {
                self.inner.get_storage_at(*address, position).await
            }
            .map_err(|e| alloy_error_to_provider_error(&e, "Failed to get storage"))?;

            // Convert U256 to B256 (32-byte storage slot)
            Ok(B256::from(result.to_be_bytes::<32>()))
        })
        .await
    }
}

/// Log fetcher with fixed chunk sizing.
pub struct LogFetcher {
    provider: Arc<AlloyProvider>,
    max_blocks_per_request: u64,
}

impl LogFetcher {
    /// Create a new log fetcher.
    #[must_use]
    pub const fn new(provider: Arc<AlloyProvider>, max_blocks_per_request: u64) -> Self {
        Self {
            provider,
            max_blocks_per_request,
        }
    }

    /// Fetch logs across a block range with chunking.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::InvalidBlockRange` if `from_block > to_block`.
    pub async fn fetch_logs_chunked(
        &self,
        from_block: u64,
        to_block: u64,
        addresses: Option<Vec<String>>,
        topics: Option<Vec<Vec<String>>>,
    ) -> ProviderResult<Vec<Log>> {
        if from_block > to_block {
            return Err(ProviderError::InvalidBlockRange {
                from: from_block,
                to: to_block,
            });
        }

        let mut all_logs = Vec::new();
        let mut current_block = from_block;

        while current_block <= to_block {
            let chunk_end =
                std::cmp::min(current_block + self.max_blocks_per_request - 1, to_block);

            let filter =
                LogFilter::new(current_block, chunk_end, addresses.clone(), topics.clone())?;
            let logs = self.provider.get_logs(&filter).await?;
            all_logs.extend(logs);

            current_block = chunk_end + 1;
        }

        Ok(all_logs)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_log_filter_creation() {
        let filter = LogFilter::new(
            100,
            200,
            Some(vec!["0x1234567890abcdef".to_string()]),
            Some(vec![vec!["0xabcd1234".to_string()]]),
        )
        .expect("valid log filter should be created");

        assert_eq!(filter.from_block, Some(100));
        assert_eq!(filter.to_block, Some(200));
        assert_eq!(filter.addresses.len(), 1);
    }

    #[test]
    fn test_to_alloy_filter_maps_topic_positions() {
        let topic0_val = "0x0000000000000000000000000000000000000000000000000000000000000001";
        let topic1_val = "0x0000000000000000000000000000000000000000000000000000000000000002";
        let topic2_val = "0x0000000000000000000000000000000000000000000000000000000000000003";

        let filter = LogFilter::new(
            100,
            200,
            None,
            Some(vec![
                vec![topic0_val.to_string()],
                vec![topic1_val.to_string()],
                vec![topic2_val.to_string()],
            ]),
        )
        .expect("valid filter");

        let alloy_filter = filter.to_alloy_filter().expect("conversion succeeds");
        let topics = &alloy_filter.topics;

        assert_eq!(
            topics[0].clone().into_iter().collect::<Vec<_>>(),
            vec![B256::from_str(topic0_val).unwrap()]
        );
        assert_eq!(
            topics[1].clone().into_iter().collect::<Vec<_>>(),
            vec![B256::from_str(topic1_val).unwrap()]
        );
        assert_eq!(
            topics[2].clone().into_iter().collect::<Vec<_>>(),
            vec![B256::from_str(topic2_val).unwrap()]
        );
    }

    #[test]
    fn test_to_alloy_filter_rejects_topic_position_out_of_range() {
        let filter = LogFilter::new(
            100,
            200,
            None,
            Some(vec![
                vec![
                    "0x0000000000000000000000000000000000000000000000000000000000000001"
                        .to_string(),
                ],
                vec![],
                vec![],
                vec![],
                vec![
                    "0x0000000000000000000000000000000000000000000000000000000000000002"
                        .to_string(),
                ],
            ]),
        )
        .expect("valid filter");

        let result = filter.to_alloy_filter();
        assert!(result.is_err());
        match result {
            Err(ProviderError::InvalidTopic { reason, .. }) => {
                assert!(reason.contains("out of range"));
            }
            _ => panic!("Expected InvalidTopic error"),
        }
    }

    #[test]
    fn test_log_filter_invalid_range() {
        let result = LogFilter::new(200, 100, None, None);

        assert!(result.is_err());
        match result {
            Err(ProviderError::InvalidBlockRange { from, to }) => {
                assert_eq!(from, 200);
                assert_eq!(to, 100);
            }
            _ => panic!("Expected InvalidBlockRange error"),
        }
    }
}
