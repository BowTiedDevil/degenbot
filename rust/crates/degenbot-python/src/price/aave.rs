//! `PyAavePriceOracle` — `PyO3` wrapper over
//! [`degenbot_price::AavePriceOracle`].
//!
//! Python companion shell `OraclePriceFetcher`
//! (:mod:`degenbot.aave.analysis.orchestrator`) delegates `fetch` to this,
//! replacing the inline `raw_call` / `encode_function_calldata` loop. The
//! tolerant per-asset skip-on-error behavior (matching the Python
//! `ContractLogicError` / `ValueError` catch) lives in the Rust core.

use crate::prelude::*;
use crate::rpc::provider::PyAlloyProvider;
use degenbot_price::AavePriceOracle;
use pyo3::exceptions::PyValueError;
use pyo3::types::PyDict;
use runtime::get_runtime;
use std::collections::HashMap;

/// A typed Aave price-oracle reader.
///
/// Constructed with the oracle contract address + a shared `AlloyProvider`;
/// `fetch` issues one `getAssetPrice(address)` `eth_call` per asset through
/// the Rust path, tolerantly skipping assets that revert or fail to decode.
#[pyclass(name = "PyAavePriceOracle", skip_from_py_object)]
pub struct PyAavePriceOracle {
    oracle: AavePriceOracle,
}

#[pymethods]
impl PyAavePriceOracle {
    /// Create a new Aave price-oracle reader.
    ///
    /// Args:
    ///     `oracle_address`: Aave price-oracle contract address (hex string)
    ///     provider: An existing `AlloyProvider` instance
    #[new]
    #[pyo3(signature = (oracle_address, provider))]
    fn new(oracle_address: &str, provider: &PyAlloyProvider) -> PyResult<Self> {
        let oracle = AavePriceOracle::new(oracle_address, provider.provider.clone())
            .map_err(Into::<PyErr>::into)?;
        Ok(Self { oracle })
    }

    /// The oracle contract address (EIP-55 checksummed hex string).
    #[getter]
    fn address(&self) -> String {
        format!("{:#x}", self.oracle.address())
    }

    /// Call `getAssetPrice(asset)` and return the `uint256` price.
    ///
    /// Args:
    ///     `asset_address`: Token address (hex string)
    ///
    /// Returns:
    ///     The price as a Python int.
    #[pyo3(signature = (asset_address,))]
    fn get_asset_price<'py>(
        &self,
        py: Python<'py>,
        asset_address: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let asset = crate::address_utils::parse_address(asset_address)
            .map_err(|e| PyValueError::new_err(format!("Invalid asset address: {e}")))?;
        let oracle = self.oracle.clone();
        let price = py
            .detach(|| get_runtime().block_on(async { oracle.get_asset_price(asset, None).await }))
            .map_err(Into::<PyErr>::into)?;
        crate::conversion::alloy::u256_to_py(py, &price)
    }

    /// Fetch prices for a batch of assets, tolerantly.
    ///
    /// Mirrors `OraclePriceFetcher.fetch`: a failure on one asset (contract
    /// revert, decode error, or any other RPC failure) is logged and skipped,
    /// and the successfully-fetched prices are returned.
    ///
    /// Args:
    ///     `asset_addresses`: Iterable of token addresses (hex strings)
    ///
    /// Returns:
    ///     A dict mapping each checksummed asset address to its price (int).
    ///     Assets that failed to fetch are omitted.
    #[pyo3(signature = (asset_addresses,))]
    fn fetch<'py>(
        &self,
        py: Python<'py>,
        asset_addresses: Vec<String>,
    ) -> PyResult<Bound<'py, PyDict>> {
        // Parse + checksum all inputs up front (raises on a malformed address,
        // matching Contract::new's validation on the oracle address itself).
        let parsed: Vec<(String, alloy::primitives::Address)> = asset_addresses
            .into_iter()
            .map(|addr| {
                let parsed = crate::address_utils::parse_address(&addr)
                    .map_err(|e| PyValueError::new_err(format!("Invalid asset address: {e}")))?;
                Ok((parsed.to_checksum(None), parsed))
            })
            .collect::<PyResult<Vec<_>>>()?;

        let oracle = self.oracle.clone();
        let prices: HashMap<alloy::primitives::Address, alloy::primitives::U256> =
            py.detach(|| {
                get_runtime().block_on(async {
                    let assets: Vec<alloy::primitives::Address> =
                        parsed.iter().map(|(_, a)| *a).collect();
                    oracle.fetch_prices(&assets, None).await
                })
            });

        // Map back to the checksummed input strings; the core already logged
        // a warning per failed asset.
        let result = PyDict::new(py);
        for (checksum, addr) in parsed {
            if let Some(price) = prices.get(&addr) {
                result.set_item(checksum, crate::conversion::alloy::u256_to_py(py, price)?)?;
            }
        }
        Ok(result)
    }
}
