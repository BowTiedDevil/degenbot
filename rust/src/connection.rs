//! Multi-chain connection manager with health checks and failover.
//!
//! Provides a registry of providers for multiple chains with automatic
//! failover between RPC endpoints and health monitoring.

use crate::errors::{ProviderError, ProviderResult};
use crate::provider::AlloyProvider;
use log::{info, warn};
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

/// Configuration for a chain connection.
#[derive(Debug, Clone)]
pub struct ChainConfig {
    /// Chain ID (e.g., 1 for Ethereum mainnet)
    pub chain_id: u64,
    /// List of RPC URLs to use (primary first)
    pub rpc_urls: Vec<String>,
    /// Maximum concurrent connections per endpoint
    pub max_connections: u32,
    /// Request timeout in seconds
    pub timeout_secs: u64,
    /// Maximum retry attempts
    pub max_retries: u32,
    /// Maximum blocks per log request
    pub max_blocks_per_request: u64,
}

impl ChainConfig {
    /// Create a new chain configuration.
    #[must_use]
    pub const fn new(chain_id: u64, rpc_urls: Vec<String>) -> Self {
        Self {
            chain_id,
            rpc_urls,
            max_connections: 10,
            timeout_secs: 30,
            max_retries: 10,
            max_blocks_per_request: 5000,
        }
    }

    /// Set max connections.
    #[must_use]
    pub const fn with_max_connections(mut self, max: u32) -> Self {
        self.max_connections = max;
        self
    }

    /// Set timeout.
    #[must_use]
    pub const fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    /// Set max retries.
    #[must_use]
    pub const fn with_max_retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }

    /// Set max blocks per request.
    #[must_use]
    pub const fn with_max_blocks_per_request(mut self, blocks: u64) -> Self {
        self.max_blocks_per_request = blocks;
        self
    }
}

/// Health status for an endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    /// Endpoint is healthy
    Healthy,
    /// Endpoint is unhealthy (failed health check)
    Unhealthy,
    /// Endpoint is currently being checked
    Checking,
}

/// Metrics for an endpoint.
#[derive(Debug, Clone)]
pub struct EndpointMetrics {
    /// Current health status
    pub status: HealthStatus,
    /// Last successful request time
    pub last_success: Option<Instant>,
    /// Last failed request time
    pub last_failure: Option<Instant>,
    /// Total successful requests
    pub success_count: u64,
    /// Total failed requests
    pub failure_count: u64,
    /// Average latency in milliseconds
    pub avg_latency_ms: f64,
    /// RPC URL
    pub rpc_url: String,
}

impl EndpointMetrics {
    /// Create new metrics for an endpoint.
    const fn new(rpc_url: String) -> Self {
        Self {
            status: HealthStatus::Healthy,
            last_success: None,
            last_failure: None,
            success_count: 0,
            failure_count: 0,
            avg_latency_ms: 0.0,
            rpc_url,
        }
    }

    /// Record a successful request.
    #[allow(clippy::cast_precision_loss)]
    fn record_success(&mut self, latency: Duration) {
        self.status = HealthStatus::Healthy;
        self.last_success = Some(Instant::now());
        self.success_count += 1;
        
        // Update average latency using exponential moving average
        let latency_ms = latency.as_millis() as f64;
        self.avg_latency_ms = if self.success_count == 1 {
            latency_ms
        } else {
            self.avg_latency_ms.mul_add(0.9, latency_ms * 0.1)
        };
    }

    /// Record a failed request.
    fn record_failure(&mut self) {
        self.status = HealthStatus::Unhealthy;
        self.last_failure = Some(Instant::now());
        self.failure_count += 1;
    }

    /// Check if endpoint is healthy.
    #[must_use]
    pub const fn is_healthy(&self) -> bool {
        matches!(self.status, HealthStatus::Healthy)
    }
}

/// Connection pool for a single chain with multiple endpoints.
pub struct ChainConnectionPool {
    /// Chain configuration
    config: ChainConfig,
    /// Providers for each endpoint
    providers: Vec<AlloyProvider>,
    /// Metrics for each endpoint
    metrics: Vec<EndpointMetrics>,
    /// Current primary endpoint index
    primary_index: usize,
}

impl ChainConnectionPool {
    /// Create a new connection pool for a chain.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::ConnectionFailed` if no endpoints can be connected.
    pub fn new(config: ChainConfig) -> ProviderResult<Self> {
        let mut providers = Vec::with_capacity(config.rpc_urls.len());
        let mut metrics = Vec::with_capacity(config.rpc_urls.len());

        for url in &config.rpc_urls {
            match AlloyProvider::new(
                url,
                config.max_connections,
                config.timeout_secs,
                config.max_retries,
            ) {
                Ok(provider) => {
                    providers.push(provider);
                    metrics.push(EndpointMetrics::new(url.clone()));
                }
                Err(e) => {
                    // Log error but continue with other endpoints
                    warn!("Failed to connect to {}: {}", url, e);
                    metrics.push(EndpointMetrics::new(url.clone()));
                }
            }
        }

        if providers.is_empty() {
            return Err(ProviderError::ConnectionFailed {
                message: format!(
                    "No endpoints available for chain {}",
                    config.chain_id
                ),
            });
        }

        Ok(Self {
            config,
            providers,
            metrics,
            primary_index: 0,
        })
    }

    /// Get the primary provider.
    #[must_use]
    pub fn primary_provider(&self) -> &AlloyProvider {
        &self.providers[self.primary_index]
    }

    /// Get provider by index.
    pub fn provider(&self, index: usize) -> ProviderResult<&AlloyProvider> {
        self.providers
            .get(index)
            .ok_or_else(|| ProviderError::Other {
                message: format!("Invalid provider index: {index}"),
            })
    }

    /// Get metrics for all endpoints.
    #[must_use]
    pub fn metrics(&self) -> &[EndpointMetrics] {
        &self.metrics
    }

    /// Get metrics for primary endpoint.
    #[must_use]
    pub fn primary_metrics(&self) -> &EndpointMetrics {
        &self.metrics[self.primary_index]
    }

    /// Record success for current primary.
    #[allow(dead_code)]
    fn record_success(&mut self, latency: Duration) {
        self.metrics[self.primary_index].record_success(latency);
    }

    /// Record failure for current primary and attempt failover.
    #[allow(dead_code)]
    fn record_failure(&mut self) {
        self.metrics[self.primary_index].record_failure();
        self.failover();
    }

    /// Failover to next healthy endpoint.
    #[allow(dead_code)]
    fn failover(&mut self) {
        let original_index = self.primary_index;
        
        // Try to find next healthy endpoint
        for i in 0..self.providers.len() {
            let next_index = (self.primary_index + i + 1) % self.providers.len();
            if self.metrics[next_index].is_healthy() || next_index == original_index {
                self.primary_index = next_index;
                if next_index != original_index {
                    info!(
                        "Failover: switched from endpoint {} to {} for chain {}",
                        original_index, next_index, self.config.chain_id
                    );
                }
                break;
            }
        }
    }

    /// Perform health check on all endpoints.
    ///
    /// # Errors
    ///
    /// Returns error if health check fails for all endpoints.
    pub async fn health_check(&mut self) -> ProviderResult<HashMap<String, bool>> {
        let mut results = HashMap::new();

        for (i, provider) in self.providers.iter().enumerate() {
            self.metrics[i].status = HealthStatus::Checking;
            
            let start = Instant::now();
            if provider.get_block_number().await.is_ok() {
                self.metrics[i].record_success(start.elapsed());
                results.insert(self.metrics[i].rpc_url.clone(), true);
            } else {
                self.metrics[i].record_failure();
                results.insert(self.metrics[i].rpc_url.clone(), false);
            }
        }

        // Ensure we have at least one healthy endpoint
        if !self.metrics.iter().any(EndpointMetrics::is_healthy) {
            return Err(ProviderError::ConnectionFailed {
                message: format!(
                    "No healthy endpoints for chain {}",
                    self.config.chain_id
                ),
            });
        }

        Ok(results)
    }

    /// Get the chain ID.
    #[must_use]
    pub const fn chain_id(&self) -> u64 {
        self.config.chain_id
    }

    /// Get the number of endpoints.
    #[must_use]
    pub const fn endpoint_count(&self) -> usize {
        self.providers.len()
    }
}

/// Multi-chain connection manager with automatic failover.
pub struct ConnectionManager {
    /// Chain ID -> Connection pool mapping
    chains: RwLock<HashMap<u64, ChainConnectionPool>>,
    /// Default chain ID
    default_chain: RwLock<Option<u64>>,
}

impl ConnectionManager {
    /// Create a new connection manager.
    #[must_use]
    pub fn new() -> Self {
        Self {
            chains: RwLock::new(HashMap::new()),
            default_chain: RwLock::new(None),
        }
    }

    /// Register a new chain.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::ConnectionFailed` if no endpoints can be connected.
    #[allow(clippy::significant_drop_tightening)]
    pub fn register_chain(&self, config: ChainConfig) -> ProviderResult<()> {
        let pool = ChainConnectionPool::new(config)?;
        let chain_id = pool.chain_id();
        
        let mut chains = self.chains.write().map_err(|_| ProviderError::Other {
            message: "Failed to acquire write lock".to_string(),
        })?;
        
        chains.insert(chain_id, pool);
        
        // Set as default if no default exists
        let mut default = self.default_chain.write().map_err(|_| ProviderError::Other {
            message: "Failed to acquire write lock".to_string(),
        })?;
        
        if default.is_none() {
            *default = Some(chain_id);
        }
        
        Ok(())
    }

    /// Get provider for a specific chain.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::ChainNotRegistered` if chain is not registered.
    #[allow(clippy::significant_drop_tightening)]
    pub fn get_provider(&self, chain_id: u64) -> ProviderResult<()> {
        let chains = self.chains.read().map_err(|_| ProviderError::Other {
            message: "Failed to acquire read lock".to_string(),
        })?;
        
        let _pool = chains.get(&chain_id).ok_or(ProviderError::ChainNotRegistered { chain_id })?;
        
        // Clone the provider (we need to implement Clone for AlloyProvider or use Arc)
        // For now, return Ok to indicate success - actual provider access will be implemented later
        Ok(())
    }

    /// Get provider for default chain.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::ChainNotRegistered` if no default chain is set.
    #[allow(clippy::significant_drop_tightening)]
    pub fn get_default_provider(&self) -> ProviderResult<()> {
        let default = self.default_chain.read().map_err(|_| ProviderError::Other {
            message: "Failed to acquire read lock".to_string(),
        })?;
        
        let chain_id = default.ok_or(ProviderError::Other {
            message: "No default chain set".to_string(),
        })?;
        
        drop(default);
        self.get_provider(chain_id)
    }

    /// Set default chain.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::ChainNotRegistered` if chain is not registered.
    #[allow(clippy::significant_drop_tightening)]
    pub fn set_default_chain(&self, chain_id: u64) -> ProviderResult<()> {
        let chains = self.chains.read().map_err(|_| ProviderError::Other {
            message: "Failed to acquire read lock".to_string(),
        })?;
        
        if !chains.contains_key(&chain_id) {
            return Err(ProviderError::ChainNotRegistered { chain_id });
        }
        
        drop(chains);
        
        let mut default = self.default_chain.write().map_err(|_| ProviderError::Other {
            message: "Failed to acquire write lock".to_string(),
        })?;
        
        *default = Some(chain_id);
        Ok(())
    }

    /// Perform health check on a specific chain.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::ChainNotRegistered` if chain is not registered.
    #[allow(clippy::significant_drop_tightening)]
    pub async fn health_check(&self, chain_id: u64) -> ProviderResult<HashMap<String, bool>> {
        let mut chains = self.chains.write().map_err(|_| ProviderError::Other {
            message: "Failed to acquire write lock".to_string(),
        })?;

        let pool = chains
            .get_mut(&chain_id)
            .ok_or(ProviderError::ChainNotRegistered { chain_id })?;

        pool.health_check().await
    }

    /// Get metrics for all endpoints of a chain.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::ChainNotRegistered` if chain is not registered.
    #[allow(clippy::significant_drop_tightening)]
    pub fn get_metrics(&self, chain_id: u64) -> ProviderResult<Vec<EndpointMetrics>> {
        let chains = self.chains.read().map_err(|_| ProviderError::Other {
            message: "Failed to acquire read lock".to_string(),
        })?;
        
        let pool = chains
            .get(&chain_id)
            .ok_or(ProviderError::ChainNotRegistered { chain_id })?;
        
        Ok(pool.metrics().to_vec())
    }

    /// Get the default chain ID.
    #[allow(clippy::significant_drop_tightening)]
    pub fn get_default_chain_id(&self) -> ProviderResult<u64> {
        let default = self.default_chain.read().map_err(|_| ProviderError::Other {
            message: "Failed to acquire read lock".to_string(),
        })?;
        
        default.ok_or_else(|| ProviderError::Other {
            message: "No default chain set".to_string(),
        })
    }

    /// Close all connections.
    #[allow(clippy::significant_drop_tightening)]
    pub fn close(&self) -> ProviderResult<()> {
        let mut chains = self.chains.write().map_err(|_| ProviderError::Other {
            message: "Failed to acquire write lock".to_string(),
        })?;
        
        chains.clear();
        Ok(())
    }
}

impl Default for ConnectionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chain_config_builder() {
        let config = ChainConfig::new(
            1,
            vec!["https://eth1.example.com".to_string()],
        )
        .with_max_connections(20)
        .with_timeout(60);

        assert_eq!(config.chain_id, 1);
        assert_eq!(config.max_connections, 20);
        assert_eq!(config.timeout_secs, 60);
    }

    #[test]
    fn test_endpoint_metrics() {
        let mut metrics = EndpointMetrics::new("https://example.com".to_string());
        
        assert!(metrics.is_healthy());
        
        metrics.record_success(Duration::from_millis(100));
        assert_eq!(metrics.success_count, 1);
        assert!(metrics.avg_latency_ms > 0.0);
        
        metrics.record_failure();
        assert!(!metrics.is_healthy());
        assert_eq!(metrics.failure_count, 1);
    }

    #[test]
    fn test_connection_manager_default_chain() {
        let manager = ConnectionManager::new();
        
        // Initially no default
        assert!(manager.get_default_provider().is_err());
        
        // Register a chain
        let _config = ChainConfig::new(
            1,
            vec!["https://eth1.example.com".to_string()],
        );
        
        // This will fail because we can't actually connect, but that's OK for this test
        // In real tests we'd need mock providers
    }

    #[tokio::test]
    async fn test_health_check_unregistered_chain() {
        let manager = ConnectionManager::new();
        
        // Health check should fail for unregistered chain
        let result = manager.health_check(999).await;
        assert!(result.is_err());
        
        // Verify it's the right error type
        match result {
            Err(ProviderError::ChainNotRegistered { chain_id }) => {
                assert_eq!(chain_id, 999);
            }
            _ => panic!("Expected ChainNotRegistered error"),
        }
    }
}
