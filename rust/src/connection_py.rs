//! `PyO3` bindings for the connection manager module.

use crate::connection::{ChainConfig, ConnectionManager, EndpointMetrics, HealthStatus};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

/// Python wrapper for `ChainConfig`.
#[pyclass(name = "ChainConfig", skip_from_py_object)]
#[derive(Clone)]
pub struct PyChainConfig {
    pub inner: ChainConfig,
}

#[pymethods]
impl PyChainConfig {
    /// Create a new chain configuration.
    #[new]
    #[pyo3(signature = (chain_id, rpc_urls, max_connections=10, timeout=30.0, max_retries=10, max_blocks_per_request=5000))]
    #[allow(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        clippy::missing_const_for_fn
    )]
    fn new(
        chain_id: u64,
        rpc_urls: Vec<String>,
        max_connections: u32,
        timeout: f64,
        max_retries: u32,
        max_blocks_per_request: u64,
    ) -> Self {
        Self {
            inner: ChainConfig {
                chain_id,
                rpc_urls,
                max_connections,
                timeout_secs: timeout as u64,
                max_retries,
                max_blocks_per_request,
            },
        }
    }

    #[getter]
    const fn chain_id(&self) -> u64 {
        self.inner.chain_id
    }

    #[getter]
    fn rpc_urls(&self) -> &[String] {
        &self.inner.rpc_urls
    }

    #[getter]
    const fn max_connections(&self) -> u32 {
        self.inner.max_connections
    }

    #[getter]
    const fn timeout(&self) -> u64 {
        self.inner.timeout_secs
    }

    #[getter]
    const fn max_retries(&self) -> u32 {
        self.inner.max_retries
    }

    #[getter]
    const fn max_blocks_per_request(&self) -> u64 {
        self.inner.max_blocks_per_request
    }
}

/// Python wrapper for `EndpointMetrics`.
#[pyclass(name = "EndpointMetrics")]
pub struct PyEndpointMetrics {
    metrics: EndpointMetrics,
}

#[pymethods]
impl PyEndpointMetrics {
    #[getter]
    fn status(&self) -> String {
        match self.metrics.status {
            HealthStatus::Healthy => "healthy".to_string(),
            HealthStatus::Unhealthy => "unhealthy".to_string(),
            HealthStatus::Checking => "checking".to_string(),
        }
    }

    #[getter]
    fn rpc_url(&self) -> String {
        self.metrics.rpc_url.clone()
    }

    #[getter]
    const fn success_count(&self) -> u64 {
        self.metrics.success_count
    }

    #[getter]
    const fn failure_count(&self) -> u64 {
        self.metrics.failure_count
    }

    #[getter]
    #[allow(clippy::missing_const_for_fn)]
    fn avg_latency_ms(&self) -> f64 {
        self.metrics.avg_latency_ms
    }

    #[getter]
    const fn is_healthy(&self) -> bool {
        self.metrics.is_healthy()
    }
}

impl From<EndpointMetrics> for PyEndpointMetrics {
    fn from(metrics: EndpointMetrics) -> Self {
        Self { metrics }
    }
}

/// Python wrapper for `ConnectionManager`.
#[pyclass(name = "ConnectionManager")]
pub struct PyConnectionManager {
    inner: ConnectionManager,
}

#[pymethods]
impl PyConnectionManager {
    /// Create a new connection manager.
    #[new]
    fn new() -> Self {
        Self {
            inner: ConnectionManager::new(),
        }
    }

    /// Register a new chain.
    fn register_chain(&self, config: &PyChainConfig) -> PyResult<()> {
        self.inner
            .register_chain(config.inner.clone())
            .map_err(|e| PyValueError::new_err(format!("Failed to register chain: {e}")))?;
        Ok(())
    }

    /// Set default chain.
    fn set_default_chain(&self, chain_id: u64) -> PyResult<()> {
        self.inner
            .set_default_chain(chain_id)
            .map_err(|e| PyValueError::new_err(format!("Failed to set default chain: {e}")))?;
        Ok(())
    }

    /// Get default chain ID.
    fn get_default_chain_id(&self) -> PyResult<u64> {
        self.inner
            .get_default_chain_id()
            .map_err(|e| PyValueError::new_err(format!("Failed to get default chain: {e}")))
    }

    /// Perform health check on a chain.
    fn health_check(&self, py: Python<'_>, chain_id: u64) -> PyResult<Py<PyDict>> {
        let handle = tokio::runtime::Handle::try_current()
            .unwrap_or_else(|_| {
                tokio::runtime::Runtime::new()
                    .expect("Failed to create tokio runtime")
                    .handle()
                    .clone()
            });

        let results = handle
            .block_on(async { self.inner.health_check(chain_id).await })
            .map_err(|e| PyValueError::new_err(format!("Health check failed: {e}")))?;

        let dict = PyDict::new(py);
        for (url, is_healthy) in results {
            dict.set_item(url, is_healthy)?;
        }

        Ok(dict.into())
    }

    /// Get metrics for all endpoints of a chain.
    fn get_metrics(&self, py: Python<'_>, chain_id: u64) -> PyResult<Py<PyList>> {
        let metrics = self
            .inner
            .get_metrics(chain_id)
            .map_err(|e| PyValueError::new_err(format!("Failed to get metrics: {e}")))?;

        let list = PyList::empty(py);
        for m in metrics {
            list.append(PyEndpointMetrics::from(m))?;
        }
        Ok(list.into())
    }

    /// Close all connections.
    fn close(&self) -> PyResult<()> {
        self.inner
            .close()
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to close connections: {e}")))?;
        Ok(())
    }
}

/// Add connection module to Python module.
pub fn add_connection_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyChainConfig>()?;
    m.add_class::<PyEndpointMetrics>()?;
    m.add_class::<PyConnectionManager>()?;
    Ok(())
}
