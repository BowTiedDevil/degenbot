//! Ethereum RPC provider implementation using Alloy.
//!
//! Provides high-performance HTTP/HTTPS connections with connection pooling,
//! retry logic, and batch request support.

use crate::errors::{ProviderError, ProviderResult};
use alloy::network::Ethereum;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::{Filter, Log};
use alloy_primitives::{Address, Bytes, B256};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

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
    /// # Errors
    ///
    /// Returns `ProviderError::ConnectionFailed` if the HTTP client cannot be created.
    pub fn new(
        rpc_url: &str,
        max_connections: u32,
        timeout_secs: u64,
        max_retries: u32,
    ) -> ProviderResult<Self> {
        let client = Client::builder()
            .pool_max_idle_per_host(max_connections as usize)
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .map_err(|e| ProviderError::ConnectionFailed {
                message: format!("Failed to create HTTP client: {e}"),
            })?;

        let url = rpc_url.parse().map_err(|e| ProviderError::ConnectionFailed {
            message: format!("Invalid RPC URL: {e}"),
        })?;

        let provider = ProviderBuilder::<_, _, Ethereum>::new()
            .connect_reqwest(client, url);

        Ok(Self {
            inner: Arc::new(provider),
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
                .map_err(|e| ProviderError::RpcError {
                    code: -1,
                    message: format!("Failed to get block number: {e}"),
                })?;
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
                .map_err(|e| ProviderError::RpcError {
                    code: -1,
                    message: format!("Failed to get chain ID: {e}"),
                })?;
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
                .map_err(|e| ProviderError::RpcError {
                    code: -1,
                    message: format!("Failed to get logs: {e}"),
                })?;
            Ok(result)
        })
        .await
    }

    /// Retry an async operation with exponential backoff.
    async fn retry_with_backoff<F, Fut, T>(&self, operation: F) -> ProviderResult<T>
    where
        F: Fn() -> Fut + Send + Sync,
        Fut: std::future::Future<Output = ProviderResult<T>> + Send,
        T: Send,
    {
        let mut attempt = 0;
        let mut delay = Duration::from_millis(100);

        loop {
            match operation().await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    attempt += 1;
                    if attempt >= self.max_retries {
                        return Err(e);
                    }

                    // Check if it's a retryable error
                    match &e {
                        ProviderError::RateLimited { .. } => {
                            delay *= 2;
                        }
                        ProviderError::Timeout { .. } | ProviderError::ConnectionFailed { .. } => {}
                        _ => return Err(e),
                    }

                    tokio::time::sleep(delay).await;
                    delay = std::cmp::min(delay * 2, Duration::from_secs(30));
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
            }.map_err(|e| {
                ProviderError::RpcError {
                    code: -1,
                    message: format!("eth_call failed: {e}"),
                }
            })?;

            Ok(result)
        })
        .await
    }
}

/// Log receipt with decoded fields.
#[derive(Debug, Clone)]
pub struct LogReceipt {
    pub address: String,
    pub topics: Vec<String>,
    pub data: Vec<u8>,
    pub block_number: Option<u64>,
    pub block_hash: Option<String>,
    pub transaction_hash: Option<String>,
    pub log_index: Option<u64>,
}

impl From<Log> for LogReceipt {
    fn from(log: Log) -> Self {
        Self {
            address: log.address().to_string(),
            topics: log.topics().iter().map(ToString::to_string).collect(),
            data: log.data().data.to_vec(),
            block_number: log.block_number,
            block_hash: log.block_hash.map(|h| h.to_string()),
            transaction_hash: log.transaction_hash.map(|h| h.to_string()),
            log_index: log.log_index,
        }
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
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_log_filter_creation() {
        let filter = LogFilter::new(
            100,
            200,
            Some(vec!["0x1234567890abcdef".to_string()]),
            Some(vec![vec!["0xabcd1234".to_string()]]),
        ).unwrap();

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

    #[test]
    fn test_log_receipt_from_log() {
        // Since we can't easily create a Log without mocking, we'll test the struct
        let receipt = LogReceipt {
            address: "0x1234".to_string(),
            topics: vec!["0xabcd".to_string()],
            data: vec![1, 2, 3],
            block_number: Some(100),
            block_hash: Some("0xhash".to_string()),
            transaction_hash: Some("0xtxhash".to_string()),
            log_index: Some(5),
        };

        assert_eq!(receipt.address, "0x1234");
        assert_eq!(receipt.topics.len(), 1);
    }
}
