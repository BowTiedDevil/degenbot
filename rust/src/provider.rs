//! Ethereum RPC provider implementation using Alloy.
//!
//! Provides high-performance HTTP/HTTPS connections with connection pooling,
//! retry logic, and batch request support. Also supports IPC endpoints for
//! local node connections.

use crate::errors::{ProviderError, ProviderResult};
use alloy::network::Ethereum;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::{Filter, Log};
use alloy_primitives::{Address, Bytes, B256};
use alloy_transport_ipc::IpcConnect;
use alloy_transport_ws::WsConnect;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

/// Recursively convert camelCase keys in a JSON Value to `snake_case`.
fn convert_keys_to_snake_case(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut new_map = serde_json::Map::new();
            for (key, val) in map {
                let snake_key = camel_to_snake_case(&key);
                new_map.insert(snake_key, convert_keys_to_snake_case(val));
            }
            serde_json::Value::Object(new_map)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(convert_keys_to_snake_case).collect())
        }
        other => other,
    }
}

/// Convert a camelCase string to `snake_case`.
fn camel_to_snake_case(s: &str) -> String {
    let mut result = String::with_capacity(s.len() * 2);
    let mut prev_upper = false;

    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if !prev_upper && i > 0 {
                result.push('_');
            }
            result.push(c.to_ascii_lowercase());
            prev_upper = true;
        } else if c.is_ascii_digit() && !result.is_empty() && !prev_upper {
            // Insert underscore before digit if preceded by lowercase letter
            result.push('_');
            result.push(c);
            prev_upper = false;
        } else {
            result.push(c);
            prev_upper = false;
        }
    }

    result
}

/// Constants for retry logic.
const INITIAL_RETRY_DELAY_MS: u64 = 100;
const MAX_RETRY_DELAY_MS: u64 = 30_000; // 30 seconds
const BACKOFF_MULTIPLIER: u64 = 2;
const MAX_JITTER_MS: u64 = 100; // Add up to 100ms of jitter

/// Convert an Alloy error to a `ProviderError` with appropriate classification.
fn alloy_error_to_provider_error(e: impl std::fmt::Display, context: &str) -> ProviderError {
    let error_str = e.to_string();
    
    // Check for specific error patterns and classify appropriately
    if error_str.contains("429") || error_str.contains("rate limit") || error_str.contains("RateLimit") {
        ProviderError::RateLimited {
            message: format!("{context}: {error_str}"),
        }
    } else if error_str.contains("timeout") || error_str.contains("timed out") {
        ProviderError::Timeout {
            timeout: 30, // Default timeout
            message: format!("{context}: {error_str}"),
        }
    } else if error_str.contains("connection") || error_str.contains("Connection") {
        ProviderError::ConnectionFailed {
            message: format!("{context}: {error_str}"),
        }
    } else {
        // Try to extract error code from JSON-RPC error
        let code = error_str.find("code:").map_or(-1, |code_start| {
            error_str[code_start..].find(',').map_or(-1, |code_end| {
                error_str[code_start + 5..code_start + code_end]
                    .trim()
                    .parse::<i64>()
                    .unwrap_or(-1)
            })
        });
        
        ProviderError::RpcError {
            code,
            message: format!("{context}: {error_str}"),
        }
    }
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
            return Err(ProviderError::InvalidBlockRange { from: from_block, to: to_block });
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
                    Address::from_str(addr).map_err(|e| ProviderError::InvalidAddress {
                        address: addr.clone(),
                        reason: format!("{e}"),
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;

            filter = filter.address(addresses);
        }

        // Convert topics
        if !self.topics.is_empty() {
            for topic_list in &self.topics {
                let topics: Vec<B256> = topic_list
                    .iter()
                    .map(|t| {
                        B256::from_str(t).map_err(|e| ProviderError::InvalidTopic {
                            topic: t.clone(),
                            reason: format!("{e}"),
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                filter = filter.event_signature(topics);
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
    pub async fn new(
        rpc_url: &str,
        _max_connections: u32,
        _timeout_secs: u64,
        max_retries: u32,
    ) -> ProviderResult<Self> {
        let provider: Arc<dyn Provider<Ethereum>> = if rpc_url.starts_with("http://") || rpc_url.starts_with("https://") {
            // HTTP endpoint - use Alloy's built-in HTTP transport
            let url = rpc_url.parse().map_err(|e| ProviderError::ConnectionFailed {
                message: format!("Invalid RPC URL: {e}"),
            })?;

            let provider = ProviderBuilder::<_, _, Ethereum>::new()
                .connect_http(url);
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
            let result: u64 = self.inner
                .get_block_number()
                .await
                .map_err(|e| alloy_error_to_provider_error(e, "Failed to get block number"))?;
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
            let result: u64 = self.inner
                .get_chain_id()
                .await
                .map_err(|e| alloy_error_to_provider_error(e, "Failed to get chain ID"))?;
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
            let result: Vec<Log> = self.inner
                .get_logs(&alloy_filter)
                .await
                .map_err(|e| alloy_error_to_provider_error(e, "Failed to get logs"))?;
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
                self.inner.get_code_at(*address).block_id(block.into()).await
            } else {
                self.inner.get_code_at(*address).await
            }
            .map_err(|e| alloy_error_to_provider_error(e, "Failed to get code"))?;

            Ok(result)
        })
        .await
    }

    /// Get a block by number.
    ///
    /// Returns the full block data including header and transactions.
    /// All field names are converted from camelCase to `snake_case` for Python
    /// consistency.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::RpcError` if the RPC call fails.
    pub async fn get_block(&self, block_number: u64) -> ProviderResult<Option<serde_json::Value>> {
        use alloy::eips::BlockNumberOrTag;
        
        self.retry_with_backoff(|| async {
            let block_num_tag = BlockNumberOrTag::Number(block_number);
            let result = self.inner
                .get_block_by_number(block_num_tag)
                .await
                .map_err(|e| alloy_error_to_provider_error(e, "Failed to get block"))?;
            
            // Serialize the full block and convert keys to snake_case
            let json_value = result.map(|block| {
                let value = serde_json::to_value(block)
                    .unwrap_or(serde_json::Value::Null);
                convert_keys_to_snake_case(value)
            });
            
            Ok(json_value)
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
                    // Add random jitter to prevent thundering herd
                    let jitter = if MAX_JITTER_MS > 0 {
                        rand::random::<u64>() % MAX_JITTER_MS
                    } else {
                        0
                    };
                    
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
            }.map_err(|e| alloy_error_to_provider_error(e, "eth_call failed"))?;

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
            let result: u128 = self.inner
                .get_gas_price()
                .await
                .map_err(|e| alloy_error_to_provider_error(e, "Failed to get gas price"))?;
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
                tx = tx.value(alloy_primitives::U256::from(val));
            }

            // Estimate at specific block if provided, otherwise use pending
            let result = if let Some(block) = block_number {
                self.inner.estimate_gas(tx).block(block.into()).await
            } else {
                self.inner.estimate_gas(tx).await
            }.map_err(|e| alloy_error_to_provider_error(e, "Failed to estimate gas"))?;

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

        let hash = FixedBytes::from_str(tx_hash)
            .map_err(|e| ProviderError::InvalidParams {
                message: format!("Invalid transaction hash: {e}"),
            })?;

        self.retry_with_backoff(|| async {
            let result = self.inner
                .get_transaction_by_hash(hash)
                .await
                .map_err(|e| alloy_error_to_provider_error(e, "Failed to get transaction"))?;

            // Convert to JSON value - use serde to handle the inner transaction structure
            let json_value = result.map(|tx| {
                // First serialize to JSON string, then parse back to Value
                // This handles the nested structure correctly
                let json_str = serde_json::to_string(&tx).unwrap_or_default();
                serde_json::from_str(&json_str).unwrap_or_else(|_| serde_json::json!({}))
            });

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

        let hash = FixedBytes::from_str(tx_hash)
            .map_err(|e| ProviderError::InvalidParams {
                message: format!("Invalid transaction hash: {e}"),
            })?;

        self.retry_with_backoff(|| async {
            let result = self.inner
                .get_transaction_receipt(hash)
                .await
                .map_err(|e| alloy_error_to_provider_error(e, "Failed to get transaction receipt"))?;

            // Convert to JSON value - use serde to handle the receipt structure
            let json_value = result.map(|receipt| {
                // First serialize to JSON string, then parse back to Value
                let json_str = serde_json::to_string(&receipt).unwrap_or_default();
                serde_json::from_str(&json_str).unwrap_or_else(|_| serde_json::json!({}))
            });

            Ok(json_value)
        })
        .await
    }
}

/// Log fetcher with dynamic block sizing.
pub struct LogFetcher {
    provider: Arc<AlloyProvider>,
    max_blocks_per_request: u64,
}

impl LogFetcher {
    /// Create a new log fetcher.
    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn new(provider: Arc<AlloyProvider>, max_blocks_per_request: u64) -> Self {
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
            return Err(ProviderError::InvalidBlockRange { from: from_block, to: to_block });
        }

        let mut all_logs = Vec::new();
        let mut current_block = from_block;

        while current_block <= to_block {
            let chunk_end =
                std::cmp::min(current_block + self.max_blocks_per_request - 1, to_block);

            let filter = LogFilter::new(current_block, chunk_end, addresses.clone(), topics.clone())?;
            let logs = self.provider.get_logs(&filter).await?;
            all_logs.extend(logs);

            current_block = chunk_end + 1;
        }

        Ok(all_logs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_filter_creation() {
        let filter = LogFilter::new(
            100,
            200,
            Some(vec!["0x1234567890abcdef".to_string()]),
            Some(vec![vec!["0xabcd1234".to_string()]]),
        ).expect("valid log filter should be created");

        assert_eq!(filter.from_block, Some(100));
        assert_eq!(filter.to_block, Some(200));
        assert_eq!(filter.addresses.len(), 1);
    }

    #[test]
    fn test_log_filter_invalid_range() {
        let result = LogFilter::new(
            200,
            100,
            None,
            None,
        );

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
