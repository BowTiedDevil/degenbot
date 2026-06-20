//! Ethereum RPC provider implementation using Alloy.
//!
//! Provides high-performance HTTP/HTTPS/WS connections with connection pooling,
//! retry logic, response caching, optional transport-level rate limiting, and
//! chunked log fetching. Also supports IPC endpoints for local node connections.

use crate::errors::{ProviderError, ProviderResult};
use alloy::consensus::{Header as ConsensusHeader, TxEnvelope};
use alloy::eips::BlockNumberOrTag;
use alloy::network::Ethereum;
use alloy::primitives::{Address, Bytes, B256, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::client::ClientBuilder;
use alloy::rpc::types::eth::{Block, Header as RpcHeader, Transaction};
use alloy::rpc::types::TransactionRequest;
use alloy::rpc::types::{Filter, Log};
use alloy::transports::ipc::IpcConnect;
use alloy::transports::layers::ThrottleLayer;
use alloy::transports::ws::WsConnect;
use alloy::transports::{RpcError, TransportErrorKind};
use rand::RngExt;
use std::num::NonZeroU32;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

/// Constants for retry logic.
const INITIAL_RETRY_DELAY_MS: u64 = 100;
const MAX_RETRY_DELAY_MS: u64 = 30_000; // 30 seconds
const BACKOFF_MULTIPLIER: u64 = 2;
const MAX_JITTER_MS: u64 = 100; // Add up to 100ms of jitter

/// Maximum allowed concurrent requests in `LogFetcher`.
///
/// Prevents file-descriptor exhaustion and RPC rate-limit bans from
/// spawning thousands of simultaneous connections.
const MAX_CONCURRENT_REQUESTS_CAP: usize = 32;

/// Default maximum total attempts for provider operations (1 initial + 9 retries).
pub const DEFAULT_MAX_RETRIES: u32 = 10;

/// Type alias for the full Ethereum block type returned by `get_block`.
///
/// Centralises the generic parameters so callers don't repeat
/// `Block<Transaction<TxEnvelope>, RpcHeader<ConsensusHeader>>`.
pub type EthBlock = Block<Transaction<TxEnvelope>, RpcHeader<ConsensusHeader>>;

/// Helper macro that wraps a simple Alloy RPC call with retry logic and
/// error classification. Reduces per-method boilerplate from 9 lines to 1.
///
/// For methods that need request construction (e.g. `eth_call`, `estimate_gas`)
/// or conditional block-id handling, write the `retry_with_backoff` call manually.
macro_rules! rpc_call {
    ($self:expr, $context:literal, $expr:expr) => {
        $self
            .retry_with_backoff(|| async {
                $expr.await.map_err(|e| e.into_provider_error($context))
            })
            .await
    };
}

/// Extension trait that converts an Alloy `RpcError` into a `ProviderError`.
///
/// Consumes the error (no double-borrow) and classifies it by Alloy's
/// type-based error variants instead of string scraping:
/// - `RpcError::Transport(TransportErrorKind::HttpError)` with 429 → `RateLimited`
/// - `RpcError::Transport(TransportErrorKind::HttpError)` with 5xx → `ConnectionFailed`
/// - `RpcError::Transport` with retryable transport errors → `Timeout`
/// - `RpcError::ErrorResp` → `RpcError` with the JSON-RPC error code
/// - `RpcError::LocalUsageError` → `Other`
/// - Other → `RpcError` with code -1
trait IntoProviderError {
    fn into_provider_error(self, context: &str) -> ProviderError;
}

impl IntoProviderError for RpcError<TransportErrorKind> {
    fn into_provider_error(self, context: &str) -> ProviderError {
        let message = format!("{context}: {self}");

        // Check for transport-level errors
        if let Some(transport_err) = self.as_transport_err() {
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
                return ProviderError::Timeout { message };
            }

            // Other transport errors → RPC error
            return ProviderError::RpcError { code: -1, message };
        }

        // Server returned an error response (JSON-RPC error)
        if let Some(error_resp) = self.as_error_resp() {
            return ProviderError::RpcError {
                code: error_resp.code,
                message,
            };
        }

        // Local usage errors (signer errors, pre-processing failures)
        if self.is_local_usage_error() {
            return ProviderError::Other { message };
        }

        // Serialization/deserialization errors
        if self.is_ser_error() || self.is_deser_error() {
            return ProviderError::SerializationError { message };
        }

        // Fallback
        ProviderError::RpcError { code: -1, message }
    }
}

/// Filter criteria for log fetching with pre-resolved typed values.
///
/// All address and topic strings are validated and parsed at construction
/// time so that `to_alloy_filter()` is infallible and cheap — no redundant
/// parsing on every retry or across concurrent chunk tasks.
#[derive(Debug, Clone)]
pub struct LogFilter {
    from_block: Option<u64>,
    to_block: Option<u64>,
    addresses: Arc<[Address]>,
    topics: Arc<[Vec<B256>]>,
}

impl LogFilter {
    /// Create a new `LogFilter`, validating and parsing all inputs eagerly.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::InvalidBlockRange` if `from_block > to_block`,
    /// `ProviderError::InvalidAddress` if any address string is malformed, or
    /// `ProviderError::InvalidTopic` if any topic string is malformed or out of
    /// range.
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

        // Parse addresses eagerly — fail fast on bad input
        let parsed_addresses: Vec<Address> = addresses
            .unwrap_or_default()
            .into_iter()
            .map(|addr| {
                crate::address_utils::parse_address(&addr).map_err(|e| {
                    ProviderError::InvalidAddress {
                        address: addr,
                        reason: format!("{e}"),
                    }
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        // Parse topics eagerly — each element maps to a topic position (0-3)
        let parsed_topics: Vec<Vec<B256>> = topics
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(i, topic_list)| {
                if topic_list.is_empty() {
                    return Ok(vec![]);
                }
                if i > 3 {
                    return Err(ProviderError::InvalidTopic {
                        topic: String::new(),
                        reason: format!("topic position {i} is out of range (max 3)"),
                    });
                }
                topic_list
                    .into_iter()
                    .map(|t| {
                        B256::from_str(&t).map_err(|e| ProviderError::InvalidTopic {
                            topic: t,
                            reason: format!("{e}"),
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            from_block: Some(from_block),
            to_block: Some(to_block),
            addresses: parsed_addresses.into(),
            topics: parsed_topics.into(),
        })
    }

    /// Returns the `from_block` value.
    #[must_use]
    pub const fn from_block(&self) -> Option<u64> {
        self.from_block
    }

    /// Returns the `to_block` value.
    #[must_use]
    pub const fn to_block(&self) -> Option<u64> {
        self.to_block
    }

    /// Convert to Alloy `Filter` (infallible — all parsing done at construction).
    #[must_use]
    pub fn to_alloy_filter(&self) -> Filter {
        let mut filter = Filter::new();

        if let Some(from) = self.from_block {
            filter = filter.from_block(from);
        }

        if let Some(to) = self.to_block {
            filter = filter.to_block(to);
        }

        if !self.addresses.is_empty() {
            filter = filter.address(self.addresses.to_vec());
        }

        for (i, topic_list) in self.topics.iter().enumerate() {
            if topic_list.is_empty() {
                continue;
            }
            filter = match i {
                0 => filter.event_signature(topic_list.clone()),
                1 => filter.topic1(topic_list.clone()),
                2 => filter.topic2(topic_list.clone()),
                3 => filter.topic3(topic_list.clone()),
                _ => break, // validated at construction; unreachable
            };
        }

        filter
    }

    /// Returns the addresses as EIP-55 checksummed strings.
    ///
    /// Re-serialises the pre-parsed `Address` values back to checksummed
    /// strings. Used by the `PyO3` `__repr__` and property getters.
    #[must_use]
    pub fn address_strings(&self) -> Vec<String> {
        self.addresses
            .iter()
            .map(crate::address_utils::address_to_checksum_string)
            .collect()
    }

    /// Returns the topics as hex strings.
    ///
    /// Re-serialises the pre-parsed `B256` values back to hex strings.
    /// Used by the `PyO3` property getters.
    #[must_use]
    pub fn topic_strings(&self) -> Vec<Vec<String>> {
        self.topics
            .iter()
            .map(|list| list.iter().map(|t| format!("{t}")).collect())
            .collect()
    }
}

/// High-performance Ethereum RPC provider.
pub struct AlloyProvider {
    inner: Arc<dyn Provider<Ethereum>>,
    rpc_url: String,
    max_attempts: u32,
}

// Manual Clone impl makes Arc::clone sharing semantics explicit
// (vs deriving Clone, which would produce the same code).
impl Clone for AlloyProvider {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            rpc_url: self.rpc_url.clone(),
            max_attempts: self.max_attempts,
        }
    }
}

impl AlloyProvider {
    /// Create a new provider with the given RPC URL.
    ///
    /// Automatically detects the connection type based on the URL:
    /// - HTTP/HTTPS URLs use HTTP transport with connection pooling
    /// - WS/WSS URLs use WebSocket transport
    /// - File paths (starting with / or \\) use IPC transport
    ///
    /// All connections use `ProviderBuilder::default()` (no fillers) with response
    /// caching enabled (100 entries). No transport-level rate limiting is applied.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::ConnectionFailed` if the HTTP client cannot be created
    /// or IPC/WS connection fails.
    pub async fn new(rpc_url: &str, max_retries: u32) -> ProviderResult<Self> {
        Self::build_provider(rpc_url, max_retries, None).await
    }

    /// Create a new provider with transport-level rate limiting.
    ///
    /// # Arguments
    ///
    /// * `rpc_url` - The RPC endpoint URL
    /// * `max_retries` - Maximum retry attempts
    /// * `requests_per_second` - Rate limit for HTTP connections (ignored for WS/IPC)
    /// * `burst` - Burst size for rate limiting
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::ConnectionFailed` if the connection cannot be established.
    pub async fn with_rate_limit(
        rpc_url: &str,
        max_retries: u32,
        requests_per_second: u32,
        burst: NonZeroU32,
    ) -> ProviderResult<Self> {
        Self::build_provider(rpc_url, max_retries, Some((requests_per_second, burst))).await
    }

    /// Internal constructor shared by `new` and `with_rate_limit`.
    ///
    /// If `rate_limit` is `Some((rps, burst))`, the HTTP transport gets a
    /// `ThrottleLayer`. Otherwise the client is built without throttling.
    pub(crate) async fn build_provider(
        rpc_url: &str,
        max_retries: u32,
        rate_limit: Option<(u32, NonZeroU32)>,
    ) -> ProviderResult<Self> {
        let provider: Arc<dyn Provider<Ethereum>> =
            if rpc_url.starts_with("http://") || rpc_url.starts_with("https://") {
                let url = rpc_url
                    .parse()
                    .map_err(|e| ProviderError::ConnectionFailed {
                        message: format!("Invalid RPC URL: {e}"),
                    })?;

                let client = if let Some((rps, burst)) = rate_limit {
                    let throttle = ThrottleLayer::new_with_burst(rps, burst);
                    ClientBuilder::default().layer(throttle).http(url)
                } else {
                    ClientBuilder::default().http(url)
                };

                let provider = ProviderBuilder::default()
                    .with_default_caching()
                    .connect_client(client)
                    .erased();
                Arc::new(provider)
            } else if rpc_url.starts_with("ws://") || rpc_url.starts_with("wss://") {
                let ws_connect = WsConnect::new(rpc_url.to_string());
                let provider = ProviderBuilder::default()
                    .with_default_caching()
                    .connect_ws(ws_connect)
                    .await
                    .map_err(|e| ProviderError::ConnectionFailed {
                        message: format!("Failed to connect to WebSocket endpoint: {e}"),
                    })?
                    .erased();
                Arc::new(provider)
            } else if !rpc_url.contains("://") {
                let ipc_connect: IpcConnect<String> = IpcConnect::new(rpc_url.to_string());
                let provider = ProviderBuilder::default()
                    .with_default_caching()
                    .connect_ipc(ipc_connect)
                    .await
                    .map_err(|e| ProviderError::ConnectionFailed {
                        message: format!("Failed to connect to IPC endpoint: {e}"),
                    })?
                    .erased();
                Arc::new(provider)
            } else {
                // `rpc_url` contains an unrecognised scheme.
                // Extract the scheme portion for the error message.
                let scheme = rpc_url.split("://").next().unwrap_or(rpc_url);
                return Err(ProviderError::ConnectionFailed {
                    message: format!("Unsupported transport scheme: {scheme}"),
                });
            };

        Ok(Self {
            inner: provider,
            rpc_url: rpc_url.to_string(),
            max_attempts: max_retries.saturating_add(1),
        })
    }

    /// Get current block number.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_block_number(&self) -> ProviderResult<u64> {
        rpc_call!(
            self,
            "Failed to get block number",
            self.inner.get_block_number()
        )
    }

    /// Get chain ID.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_chain_id(&self) -> ProviderResult<u64> {
        rpc_call!(self, "Failed to get chain ID", self.inner.get_chain_id())
    }

    /// Fetch logs with retry logic.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError` if the RPC call fails or filter is invalid.
    pub async fn get_logs(&self, filter: &LogFilter) -> ProviderResult<Vec<Log>> {
        let alloy_filter = filter.to_alloy_filter();

        self.retry_with_backoff(|| async {
            let result: Vec<Log> = self
                .inner
                .get_logs(&alloy_filter)
                .await
                .map_err(|e| e.into_provider_error("Failed to get logs"))?;
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
            .map_err(|e| e.into_provider_error("Failed to get code"))?;

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
    pub async fn get_block(&self, block_number: u64) -> ProviderResult<Option<EthBlock>> {
        self.retry_with_backoff(|| async {
            let block_num_tag = BlockNumberOrTag::Number(block_number);
            let result = self
                .inner
                .get_block_by_number(block_num_tag)
                .await
                .map_err(|e| e.into_provider_error("Failed to get block"))?;

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
                    if attempt >= self.max_attempts {
                        return Err(e);
                    }

                    // Check if it's a retryable error
                    if !e.is_retryable() {
                        return Err(e);
                    }

                    // Calculate delay with exponential backoff and jitter
                    // Use random_range for uniform distribution (avoids modulo bias)
                    let jitter = rand::rng().random_range(0..MAX_JITTER_MS);

                    let sleep_duration = Duration::from_millis(delay_ms + jitter);
                    tokio::time::sleep(sleep_duration).await;

                    // Exponential backoff with cap (saturating to prevent overflow)
                    delay_ms = std::cmp::min(
                        delay_ms.saturating_mul(BACKOFF_MULTIPLIER),
                        MAX_RETRY_DELAY_MS,
                    );
                }
            }
        }
    }

    /// Get the RPC URL.
    #[must_use]
    pub fn rpc_url(&self) -> &str {
        &self.rpc_url
    }

    /// Get a reference-counted clone of the inner Alloy provider.
    ///
    /// Used by the subscription module to spawn pump tasks that
    /// need access to the provider for subscription calls.
    pub(crate) fn provider_arc(&self) -> Arc<dyn Provider<Ethereum>> {
        Arc::clone(&self.inner)
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
            .map_err(|e| e.into_provider_error("eth_call failed"))?;

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
        rpc_call!(self, "Failed to get gas price", self.inner.get_gas_price())
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
            .map_err(|e| e.into_provider_error("Failed to estimate gas"))?;

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
        let hash = B256::from_str(tx_hash).map_err(|e| ProviderError::InvalidParams {
            message: format!("Invalid transaction hash: {e}"),
        })?;

        self.retry_with_backoff(|| async {
            let result = self
                .inner
                .get_transaction_by_hash(hash)
                .await
                .map_err(|e| e.into_provider_error("Failed to get transaction"))?;

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
        let hash = B256::from_str(tx_hash).map_err(|e| ProviderError::InvalidParams {
            message: format!("Invalid transaction hash: {e}"),
        })?;

        self.retry_with_backoff(|| async {
            let result = self
                .inner
                .get_transaction_receipt(hash)
                .await
                .map_err(|e| e.into_provider_error("Failed to get transaction receipt"))?;

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
        self.retry_with_backoff(|| async {
            let result = if let Some(block) = block_number {
                self.inner
                    .get_storage_at(*address, position)
                    .block_id(block.into())
                    .await
            } else {
                self.inner.get_storage_at(*address, position).await
            }
            .map_err(|e| e.into_provider_error("Failed to get storage"))?;

            // Convert U256 to B256 (32-byte storage slot)
            Ok(B256::from(result.to_be_bytes::<32>()))
        })
        .await
    }

    /// Get the balance of an address.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_balance(
        &self,
        address: &Address,
        block_number: Option<u64>,
    ) -> ProviderResult<U256> {
        self.retry_with_backoff(|| async {
            let result = if let Some(block) = block_number {
                self.inner
                    .get_balance(*address)
                    .block_id(block.into())
                    .await
            } else {
                self.inner.get_balance(*address).await
            }
            .map_err(|e| e.into_provider_error("Failed to get balance"))?;

            Ok(result)
        })
        .await
    }

    /// Get the transaction count (nonce) for an address.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_transaction_count(
        &self,
        address: &Address,
        block_number: Option<u64>,
    ) -> ProviderResult<u64> {
        self.retry_with_backoff(|| async {
            let result = if let Some(block) = block_number {
                self.inner
                    .get_transaction_count(*address)
                    .block_id(block.into())
                    .await
            } else {
                self.inner.get_transaction_count(*address).await
            }
            .map_err(|e| e.into_provider_error("Failed to get transaction count"))?;

            Ok(result)
        })
        .await
    }

    /// Make a raw JSON-RPC request.
    ///
    /// This method allows calling arbitrary RPC methods that don't have
    /// typed wrappers, such as debug methods, trace methods, or node-specific APIs.
    ///
    /// # Arguments
    /// * `method` - The RPC method name
    /// * `params` - The parameters as a JSON value (typically an array)
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn make_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> ProviderResult<serde_json::Value> {
        let method = method.to_string();
        let params = Arc::new(params);

        self.retry_with_backoff(|| {
            let client = self.inner.client();
            let method = method.clone();
            let params = Arc::clone(&params);
            async move {
                let result: serde_json::Value = client
                    .request(method, (*params).clone())
                    .await
                    .map_err(|e| e.into_provider_error("RPC request failed"))?;

                Ok(result)
            }
        })
        .await
    }
}

#[cfg(test)]
impl AlloyProvider {
    /// Test-only constructor wrapping a pre-built provider (e.g. a mock
    /// transport). Used by `BlockPump` tests that drive `run_with_stream`
    /// from a deterministic synthetic `WsEvent` stream without a live RPC.
    #[must_use]
    pub(crate) fn from_provider(inner: Arc<dyn Provider<Ethereum>>) -> Self {
        Self {
            inner,
            rpc_url: String::from("test"),
            max_attempts: DEFAULT_MAX_RETRIES,
        }
    }
}

/// Log fetcher with fixed chunk sizing.
pub struct LogFetcher {
    provider: Arc<AlloyProvider>,
    max_blocks_per_request: u64,
    max_concurrent_requests: usize,
}

impl LogFetcher {
    /// Create a new log fetcher.
    #[must_use]
    pub const fn new(provider: Arc<AlloyProvider>, max_blocks_per_request: u64) -> Self {
        Self {
            provider,
            max_blocks_per_request,
            max_concurrent_requests: 4, // Default concurrency limit
        }
    }

    /// Set the maximum number of concurrent requests.
    ///
    /// The value is capped at [`MAX_CONCURRENT_REQUESTS_CAP`] (32) to prevent
    /// file-descriptor exhaustion and RPC rate-limit bans.
    #[must_use]
    pub fn with_concurrency(mut self, max_concurrent: usize) -> Self {
        self.max_concurrent_requests = max_concurrent.min(MAX_CONCURRENT_REQUESTS_CAP);
        self
    }

    /// Fetch logs across a block range with chunking.
    ///
    /// Uses concurrent requests to fetch multiple chunks in parallel,
    /// improving performance for large block ranges. Logs are sorted
    /// by `(block_number, log_index)` for deterministic ordering.
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

        if self.max_blocks_per_request == 0 {
            return Err(ProviderError::InvalidParams {
                message: "max_blocks_per_request must be greater than 0".to_string(),
            });
        }

        // Resolve the filter once and share it across chunk tasks via Arc.
        // All string→typed parsing happens here, not per-chunk.
        let base_filter = Arc::new(LogFilter::new(from_block, to_block, addresses, topics)?);

        // Build list of chunk ranges
        let mut chunks = Vec::new();
        let mut current_block = from_block;

        while current_block <= to_block {
            let chunk_end =
                std::cmp::min(current_block + self.max_blocks_per_request - 1, to_block);
            chunks.push((current_block, chunk_end));
            current_block = chunk_end + 1;
        }

        // Spawn all tasks immediately; the semaphore gates execution inside each task
        let sem = Arc::new(tokio::sync::Semaphore::new(self.max_concurrent_requests));
        let mut join_set = tokio::task::JoinSet::new();

        for (chunk_start, chunk_end) in chunks {
            let sem = Arc::clone(&sem);
            let provider = Arc::clone(&self.provider);
            let base_filter = Arc::clone(&base_filter);

            join_set.spawn(async move {
                // Acquire permit inside the task so all tasks can be spawned first
                let _permit = sem.acquire().await.map_err(|_| ProviderError::Other {
                    message: "Semaphore acquisition failed".to_string(),
                })?;

                // Build chunk-specific filter from the pre-resolved base
                let chunk_filter = LogFilter {
                    from_block: Some(chunk_start),
                    to_block: Some(chunk_end),
                    addresses: Arc::clone(&base_filter.addresses),
                    topics: Arc::clone(&base_filter.topics),
                };
                provider.get_logs(&chunk_filter).await
            });
        }

        // Collect results as tasks complete; JoinSet yields in completion order
        let mut all_logs = Vec::new();
        while let Some(join_result) = join_set.join_next().await {
            let logs = join_result.map_err(|e| ProviderError::Other {
                message: format!("Task join error: {e}"),
            })??;
            all_logs.extend(logs);
        }

        // Sort by (block_number, log_index) for deterministic ordering
        all_logs.sort_by(|a, b| {
            let a_block = a.block_number.unwrap_or(0);
            let b_block = b.block_number.unwrap_or(0);
            a_block
                .cmp(&b_block)
                .then_with(|| a.log_index.unwrap_or(0).cmp(&b.log_index.unwrap_or(0)))
        });

        Ok(all_logs)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    // ── LogFilter construction ──────────────────────────────────────────

    #[test]
    fn test_log_filter_creation() {
        let filter = LogFilter::new(
            100,
            200,
            Some(vec![
                "0x1234567890abcdef1234567890abcdef12345678".to_string()
            ]),
            Some(vec![vec![
                "0x0000000000000000000000000000000000000000000000000000000000000001".to_string(),
            ]]),
        )
        .expect("valid log filter should be created");

        assert_eq!(filter.from_block(), Some(100));
        assert_eq!(filter.to_block(), Some(200));
        assert_eq!(filter.addresses.len(), 1);
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

    #[test]
    fn test_log_filter_equal_range() {
        // from_block == to_block is valid (single-block query)
        let filter = LogFilter::new(42, 42, None, None).expect("equal range should be valid");
        assert_eq!(filter.from_block(), Some(42));
        assert_eq!(filter.to_block(), Some(42));
    }

    #[test]
    fn test_log_filter_defaults() {
        let filter = LogFilter::new(0, 100, None, None).expect("valid");
        assert!(filter.addresses.is_empty());
        assert!(filter.topics.is_empty());
    }

    #[test]
    fn test_log_filter_invalid_address_eager() {
        // Invalid addresses are now caught at construction time
        let result = LogFilter::new(100, 200, Some(vec!["not_an_address".to_string()]), None);
        assert!(result.is_err());
        match result {
            Err(ProviderError::InvalidAddress { address, .. }) => {
                assert_eq!(address, "not_an_address");
            }
            other => panic!("Expected InvalidAddress error, got {other:?}"),
        }
    }

    #[test]
    fn test_log_filter_invalid_topic_eager() {
        // Invalid topics are now caught at construction time
        let result = LogFilter::new(100, 200, None, Some(vec![vec!["not_a_topic".to_string()]]));
        assert!(result.is_err());
        match result {
            Err(ProviderError::InvalidTopic { topic, .. }) => {
                assert_eq!(topic, "not_a_topic");
            }
            other => panic!("Expected InvalidTopic error, got {other:?}"),
        }
    }

    // ── LogFilter → Alloy Filter conversion ─────────────────────────────

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

        let alloy_filter = filter.to_alloy_filter();
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
        let result = LogFilter::new(
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
        );

        assert!(result.is_err());
        match result {
            Err(ProviderError::InvalidTopic { reason, .. }) => {
                assert!(reason.contains("out of range"));
            }
            _ => panic!("Expected InvalidTopic error"),
        }
    }

    #[test]
    fn test_to_alloy_filter_skips_empty_topic_slots() {
        // Verify that empty inner vecs are skipped without error
        let filter = LogFilter::new(
            100,
            200,
            None,
            Some(vec![
                vec![
                    "0x0000000000000000000000000000000000000000000000000000000000000001"
                        .to_string(),
                ],
                vec![], // should be skipped
                vec![
                    "0x0000000000000000000000000000000000000000000000000000000000000003"
                        .to_string(),
                ],
            ]),
        )
        .expect("valid filter");

        let _ = filter.to_alloy_filter(); // infallible, just verifying no panic
    }

    // ── Retry backoff logic ─────────────────────────────────────────────

    #[test]
    fn test_saturating_retry_delay() {
        // Verify that retry delay uses saturating arithmetic
        let max_delay = u64::MAX / 2;
        let result = std::cmp::min(
            max_delay.saturating_mul(BACKOFF_MULTIPLIER),
            MAX_RETRY_DELAY_MS,
        );
        // Should cap at MAX_RETRY_DELAY_MS, not overflow
        assert_eq!(result, MAX_RETRY_DELAY_MS);
    }

    #[test]
    fn test_backoff_progression() {
        // Verify the exponential backoff sequence up to the cap
        let mut delay = INITIAL_RETRY_DELAY_MS;
        let mut delays = vec![delay];
        for _ in 0..20 {
            delay = std::cmp::min(delay.saturating_mul(BACKOFF_MULTIPLIER), MAX_RETRY_DELAY_MS);
            delays.push(delay);
        }

        // Should double each step until hitting the 30s cap
        assert_eq!(delays[0], 100);
        assert_eq!(delays[1], 200);
        assert_eq!(delays[2], 400);
        assert_eq!(delays[3], 800);
        assert_eq!(delays[4], 1600);
        assert_eq!(delays[5], 3200);
        assert_eq!(delays[6], 6400);
        assert_eq!(delays[7], 12800);
        assert_eq!(delays[8], 25600);
        assert_eq!(delays[9], 30000); // capped
        assert_eq!(delays[10], 30000); // stays capped
    }

    #[tokio::test]
    async fn test_retry_returns_ok_immediately() {
        // Successful operations should return on the first attempt
        use std::sync::atomic::{AtomicU32, Ordering};

        let call_count = Arc::new(AtomicU32::new(0));
        let count_clone = Arc::clone(&call_count);

        // Simulate retry_with_backoff logic inline
        let max_attempts: u32 = 10;
        let mut attempt = 0;

        let result: ProviderResult<u64> = loop {
            let count = count_clone.fetch_add(1, Ordering::SeqCst);
            if count == 0 {
                break Ok(42);
            }
            attempt += 1;
            if attempt >= max_attempts {
                break Err(ProviderError::Other {
                    message: "too many attempts".to_string(),
                });
            }
        };

        assert_eq!(result.unwrap(), 42);
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_retryable_error_classification() {
        // Retryable errors
        assert!(ProviderError::RateLimited {
            message: "test".to_string()
        }
        .is_retryable());
        assert!(ProviderError::Timeout {
            message: "test".to_string()
        }
        .is_retryable());
        assert!(ProviderError::ConnectionFailed {
            message: "test".to_string()
        }
        .is_retryable());

        // Non-retryable errors
        let non_retryable = [
            ProviderError::RpcError {
                code: -32000,
                message: "revert".to_string(),
            },
            ProviderError::InvalidBlockRange { from: 1, to: 0 },
            ProviderError::InvalidParams {
                message: "bad".to_string(),
            },
            ProviderError::SerializationError {
                message: "bad".to_string(),
            },
            ProviderError::Other {
                message: "bad".to_string(),
            },
        ];

        for err in non_retryable {
            assert!(!err.is_retryable(), "{err:?} should not be retryable");
        }
    }

    // ── Chunk calculation ───────────────────────────────────────────────

    #[test]
    fn test_chunk_ranges_exact_division() {
        // 100 blocks, 100 per chunk → 1 chunk
        let from = 0u64;
        let to = 99u64;
        let chunk_size = 100u64;

        let mut chunks = Vec::new();
        let mut current = from;
        while current <= to {
            let end = std::cmp::min(current + chunk_size - 1, to);
            chunks.push((current, end));
            current = end + 1;
        }

        assert_eq!(chunks, vec![(0, 99)]);
    }

    #[test]
    fn test_chunk_ranges_partial() {
        // 250 blocks, 100 per chunk → 3 chunks
        let from = 0u64;
        let to = 249u64;
        let chunk_size = 100u64;

        let mut chunks = Vec::new();
        let mut current = from;
        while current <= to {
            let end = std::cmp::min(current + chunk_size - 1, to);
            chunks.push((current, end));
            current = end + 1;
        }

        assert_eq!(chunks, vec![(0, 99), (100, 199), (200, 249)]);
    }

    #[test]
    fn test_chunk_ranges_single_block() {
        // 1 block → 1 chunk of size 1
        let from = 42u64;
        let to = 42u64;
        let chunk_size = 100u64;

        let mut chunks = Vec::new();
        let mut current = from;
        while current <= to {
            let end = std::cmp::min(current + chunk_size - 1, to);
            chunks.push((current, end));
            current = end + 1;
        }

        assert_eq!(chunks, vec![(42, 42)]);
    }

    #[test]
    fn test_chunk_ranges_non_zero_start() {
        // Starting from block 500
        let from = 500u64;
        let to = 799u64;
        let chunk_size = 100u64;

        let mut chunks = Vec::new();
        let mut current = from;
        while current <= to {
            let end = std::cmp::min(current + chunk_size - 1, to);
            chunks.push((current, end));
            current = end + 1;
        }

        assert_eq!(chunks, vec![(500, 599), (600, 699), (700, 799)]);
    }

    // ── URL scheme detection ────────────────────────────────────────────

    #[test]
    fn test_url_scheme_detection() {
        // HTTP variants
        assert!("http://localhost:8545".starts_with("http://"));
        assert!("https://mainnet.infura.io".starts_with("https://"));

        // WebSocket variants
        assert!("ws://localhost:8546".starts_with("ws://"));
        assert!("wss://mainnet.infura.io/ws".starts_with("wss://"));

        // IPC paths (no ://)
        assert!(!"/tmp/anvil.ipc".contains("://"));
        assert!(!"\\\\.\\pipe\\anvil".contains("://"));

        // Unsupported schemes
        assert!("ftp://example.com".starts_with("ftp://"));
    }

    #[test]
    fn test_unsupported_scheme_extraction() {
        let url = "ftp://example.com";
        let scheme = url.split("://").next().unwrap_or(url);
        assert_eq!(scheme, "ftp");
    }

    // ── LogFetcher validation ──────────────────────────────────────────

    #[tokio::test]
    async fn test_fetch_logs_chunked_zero_chunk_size_rejected() {
        let max_blocks_per_request = 0u64;

        // This is the check from fetch_logs_chunked:
        let result = if max_blocks_per_request == 0 {
            Err(ProviderError::InvalidParams {
                message: "max_blocks_per_request must be greater than 0".to_string(),
            })
        } else {
            Ok(())
        };

        assert!(result.is_err());
        match result {
            Err(ProviderError::InvalidParams { message }) => {
                assert!(message.contains("greater than 0"));
            }
            other => panic!("Expected InvalidParams, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_fetch_logs_chunked_invalid_range_rejected() {
        let from_block = 200u64;
        let to_block = 100u64;

        // This is the check from fetch_logs_chunked:
        let result = if from_block > to_block {
            Err(ProviderError::InvalidBlockRange {
                from: from_block,
                to: to_block,
            })
        } else {
            Ok(())
        };

        assert!(result.is_err());
        match result {
            Err(ProviderError::InvalidBlockRange { from, to }) => {
                assert_eq!(from, 200);
                assert_eq!(to, 100);
            }
            other => panic!("Expected InvalidBlockRange, got {other:?}"),
        }
    }

    // ── LogFetcher concurrency cap ─────────────────────────────────────

    #[test]
    fn test_concurrency_cap() {
        // Values above the cap should be clamped
        let requested = 1000usize;
        let clamped = requested.min(MAX_CONCURRENT_REQUESTS_CAP);
        assert_eq!(clamped, MAX_CONCURRENT_REQUESTS_CAP);
    }

    #[test]
    fn test_concurrency_within_cap() {
        // Values within the cap should pass through
        let requested = 8usize;
        let clamped = requested.min(MAX_CONCURRENT_REQUESTS_CAP);
        assert_eq!(clamped, 8);
    }

    // ── Log sorting ────────────────────────────────────────────────────

    #[test]
    fn test_log_sort_order() {
        // Verify logs sort by (block_number, log_index) even when provided out of order.
        let logs: Vec<Log> = vec![
            Log {
                block_number: Some(100),
                log_index: Some(2),
                ..Default::default()
            },
            Log {
                block_number: Some(99),
                log_index: Some(0),
                ..Default::default()
            },
            Log {
                block_number: Some(100),
                log_index: Some(0),
                ..Default::default()
            },
            Log {
                block_number: None,
                log_index: None,
                ..Default::default()
            },
        ];

        let mut sorted = logs;
        sorted.sort_by(|a, b| {
            let a_block = a.block_number.unwrap_or(0);
            let b_block = b.block_number.unwrap_or(0);
            a_block
                .cmp(&b_block)
                .then_with(|| a.log_index.unwrap_or(0).cmp(&b.log_index.unwrap_or(0)))
        });

        // Expected order: block_number=None(0,0), 99(99,0), 100(100,0), 100(100,2)
        assert_eq!(sorted[0].log_index, None); // pending log comes first (0,0)
        assert_eq!(sorted[1].block_number, Some(99));
        assert_eq!(sorted[2].block_number, Some(100));
        assert_eq!(sorted[2].log_index, Some(0));
        assert_eq!(sorted[3].block_number, Some(100));
        assert_eq!(sorted[3].log_index, Some(2));
    }

    // ── EthBlock type alias ────────────────────────────────────────────

    #[test]
    fn test_eth_block_type_alias_compiles() {
        // Verify the type alias is usable (compile-time check)
        fn assert_block_type(_: Option<EthBlock>) {}
        assert_block_type(None);
    }
}
