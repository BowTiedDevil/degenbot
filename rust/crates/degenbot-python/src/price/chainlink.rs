//! `PyChainlinkPriceFeed` — `PyO3` wrapper over
//! [`degenbot_price::ChainlinkPriceFeed`].
//!
//! Python companion shell `ChainlinkPriceContract`
//! (`src/degenbot/chainlink/price_feed.py`) constructs this with the bot's
//! `AlloyProvider` + the aggregator address, then delegates `decimals` /
//! `latest_round_data` to it (the float `price` is computed by the shell from
//! the raw `int256` answer + `decimals`, preserving the prior float-exact
//! behavior — the Rust `price()` truncating-integer convenience is also
//! exposed here for standalone Rust consumers).

use crate::conversion::alloy::{i256_to_py, u256_to_py};
use crate::prelude::*;
use crate::rpc::provider::PyAlloyProvider;
use degenbot_price::ChainlinkPriceFeed;
use runtime::get_runtime;

/// A typed Chainlink aggregator price feed.
///
/// Constructed with the aggregator address + a shared `AlloyProvider`; routes
/// `decimals()` / `latestRoundData()` through the Rust `eth_call` + ABI decode
/// path (no Python `abi_decode` / `provider.call_raw` round-trip).
#[pyclass(name = "PyChainlinkPriceFeed", skip_from_py_object)]
pub struct PyChainlinkPriceFeed {
    feed: ChainlinkPriceFeed,
}

#[pymethods]
impl PyChainlinkPriceFeed {
    /// Create a new Chainlink price-feed reader.
    ///
    /// Args:
    ///     address: Aggregator contract address (hex string)
    ///     provider: An existing `AlloyProvider` instance
    ///     `chain_id`: Optional chain id (bookkeeping only; does not affect RPC)
    #[new]
    #[pyo3(signature = (address, provider, chain_id=None))]
    fn new(address: &str, provider: &PyAlloyProvider, chain_id: Option<u64>) -> PyResult<Self> {
        let feed = ChainlinkPriceFeed::new(address, provider.provider.clone(), chain_id)
            .map_err(Into::<PyErr>::into)?;
        Ok(Self { feed })
    }

    /// The aggregator contract address (EIP-55 checksummed hex string).
    #[getter]
    fn address(&self) -> String {
        format!("{:#x}", self.feed.address())
    }

    /// The chain id supplied at construction (bookkeeping only).
    #[getter]
    const fn chain_id(&self) -> Option<u64> {
        self.feed.chain_id()
    }

    /// Call `decimals()` and return the feed's decimal places (`uint8`).
    fn decimals(&self, py: Python<'_>) -> PyResult<u8> {
        let feed = self.feed.clone();
        py.detach(|| get_runtime().block_on(async { feed.decimals(None).await }))
            .map_err(Into::<PyErr>::into)
    }

    /// Call `latestRoundData()` and return the decoded tuple as Python ints:
    /// `(round_id, answer, started_at, updated_at, answered_in_round)`.
    ///
    /// `answer` is the raw `int256` aggregate price (in the feed's native
    /// decimals); callers apply the decimal correction.
    fn latest_round_data<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let feed = self.feed.clone();
        let round = py
            .detach(|| get_runtime().block_on(async { feed.latest_round_data(None).await }))
            .map_err(Into::<PyErr>::into)?;

        let py_tuple = PyTuple::new(
            py,
            [
                round.round_id.into_pyobject(py)?.into_any(),
                i256_to_py(py, &round.answer)?.into_any(),
                u256_to_py(py, &round.started_at)?.into_any(),
                u256_to_py(py, &round.updated_at)?.into_any(),
                round.answered_in_round.into_pyobject(py)?.into_any(),
            ],
        )?;
        Ok(py_tuple.into_any())
    }

    /// The decimal-corrected nominal price as a `U256` (whole units) — the
    /// integer analogue of `int(answer // 10**decimals)`. Negative answers
    /// (never for live USD feeds, but allowed by the `int256` ABI type) clamp
    /// to zero.
    ///
    /// Note: the Python shell `ChainlinkPriceContract.price` (a float)
    /// computes `float(answer) / 10**decimals` from `latest_round_data` +
    /// `decimals` to preserve the prior float-exact behavior *including* the
    /// fractional part; this `price` truncates and is exposed for direct
    /// Rust/standalone consumers.
    fn price<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let feed = self.feed.clone();
        let price = py
            .detach(|| get_runtime().block_on(async { feed.price(None).await }))
            .map_err(Into::<PyErr>::into)?;
        u256_to_py(py, &price)
    }
}

use pyo3::types::PyTuple;
