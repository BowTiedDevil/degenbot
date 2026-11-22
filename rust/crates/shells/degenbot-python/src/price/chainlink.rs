//! `PyChainlinkPriceFeed` — `PyO3` wrapper over
//! [`degenbot_price::ChainlinkPriceFeed`].
//!
//! Python companion shell `ChainlinkPriceContract`
//! (`src/degenbot/chainlink/price_feed.py`) constructs this with the bot's
//! `AlloyProvider` + the aggregator address, then delegates `decimals` /
//! `latest_round_data` to it (the float `price` is computed by the shell from
//! the raw `int256` answer + `decimals`, preserving the shell's float-exact
//! behavior — the Rust `price()` truncating-integer convenience is also
//! exposed here for standalone Rust consumers).

use crate::conversion::alloy::{extract_python_i256, i256_to_py, u256_to_py};
use crate::prelude::*;
use crate::rpc::provider::PyAlloyProvider;
use degenbot_price::ChainlinkPriceFeed;
use runtime::get_runtime;

/// A typed Chainlink aggregator price feed.
///
/// Constructed with the aggregator address + a shared `AlloyProvider`; routes
/// `decimals()` / `latestRoundData()` through the Rust `eth_call` + ABI decode
/// path (no Python `abi_decode` / `provider.call_raw` round-trip).
#[pyclass(name = "ChainlinkPriceFeed", module = "degenbot._ffi.price")]
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
    /// integer analogue of `int(answer // 10**decimals)`.
    ///
    /// A zero answer is a zero price. Malformed feed data is refused, never
    /// priced: a negative answer, or a `decimals` whose `10**decimals` cannot
    /// be scaled, raises `ValueError` carrying the aggregator, the answer, and
    /// the decimals (`PriceError::FeedData` as `degenbot-price` maps it).
    ///
    /// Note: the Python shell `ChainlinkPriceContract.price` (a float)
    /// computes `float(answer) / 10**decimals` from `latest_round_data` +
    /// `decimals` to preserve the shell's float-exact behavior *including* the
    /// fractional part; this `price` truncates and is exposed for direct
    /// Rust/standalone consumers.
    fn price<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let feed = self.feed.clone();
        let price = py
            .detach(|| get_runtime().block_on(async { feed.price(None).await }))
            .map_err(Into::<PyErr>::into)?;
        u256_to_py(py, &price)
    }

    /// Validate, without an `eth_call`, that a decoded `answer`/`decimals` pair
    /// can be priced by this feed.
    ///
    /// The reader's own check, for a companion that decoded the round tuple
    /// itself: the Python shell keeps the fractional float the integer `price`
    /// truncates away, so it validates the tuple it already has. Raises
    /// `ValueError` naming the aggregator, the answer, and the decimals.
    fn validate(&self, answer: &Bound<'_, PyAny>, decimals: u8) -> PyResult<()> {
        let answer = extract_python_i256(answer)?;
        self.feed
            .validate(answer, decimals)
            .map_err(Into::<PyErr>::into)
    }
}

use pyo3::types::PyTuple;

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "test assertions fail loudly"
    )]

    use alloy::primitives::{Address, I256};
    use degenbot_price::{ChainlinkPriceFeed, FeedDataReason, PriceError};
    use degenbot_rpc::offline::OfflineProvider;
    use pyo3::exceptions::PyValueError;
    use pyo3::prelude::*;
    use std::sync::Arc;

    use super::PyChainlinkPriceFeed;
    use crate::conversion::alloy::i256_to_py;

    /// A reader over a provider with no recorded chain data: `validate` answers
    /// from its arguments, so any RPC would have to fail.
    fn offline_feed() -> PyChainlinkPriceFeed {
        let provider = OfflineProvider::from_json_str("{\"chain_id\": 1}")
            .unwrap()
            .as_alloy_provider();
        PyChainlinkPriceFeed {
            feed: ChainlinkPriceFeed::new(
                "0x5f4eC3Df9cbd43714FE2740f5e3616155c5b8419",
                Arc::new(provider),
                Some(1),
            )
            .unwrap(),
        }
    }

    /// The no-RPC check answers with the same rejection the reading path
    /// raises, so a companion projecting the fraction cannot disagree with it.
    #[test]
    fn chainlink_validate_reports_malformed_feed_data_without_an_rpc() {
        let feed = offline_feed();

        Python::attach(|py| {
            let negative = i256_to_py(py, &I256::MINUS_ONE).unwrap();
            let err = feed
                .validate(&negative, 8)
                .expect_err("a negative answer is not priceable");
            assert!(err.is_instance_of::<PyValueError>(py));
            let message = err.value(py).str().unwrap().extract::<String>().unwrap();
            assert!(message.contains("negative answer"), "{message}");
            assert!(message.contains("-1"), "{message}");
            assert!(message.contains("decimals 8"), "{message}");

            let priceable = i256_to_py(py, &I256::try_from(372_038_000_000i64).unwrap()).unwrap();
            assert!(feed.validate(&priceable, 8).is_ok());

            let err = feed
                .validate(&priceable, 40)
                .expect_err("40 decimals is not priceable");
            let message = err.value(py).str().unwrap().extract::<String>().unwrap();
            assert!(message.contains("overflows u128"), "{message}");
            assert!(message.contains("decimals 40"), "{message}");
        });
    }

    /// Malformed feed data crosses the seam as a `ValueError` that names the
    /// feed, the answer, and the decimals — the values a driver acts on. A
    /// silently-clamped zero would arrive as a valid price instead.
    #[test]
    fn chainlink_feed_data_error_reaches_python_as_value_error() {
        let feed: Address = "0x5f4eC3Df9cbd43714FE2740f5e3616155c5b8419"
            .parse()
            .unwrap();
        let error: PyErr = PriceError::FeedData {
            feed,
            answer: I256::MINUS_ONE,
            decimals: 8,
            reason: FeedDataReason::NegativeAnswer,
        }
        .into();

        Python::attach(|py| {
            assert!(error.is_instance_of::<PyValueError>(py));
            let message = error.value(py).str().unwrap().extract::<String>().unwrap();
            assert!(
                message.contains(&format!("{feed}")),
                "feed context: {message}"
            );
            assert!(message.contains("-1"), "answer context: {message}");
            assert!(
                message.contains("decimals 8"),
                "decimals context: {message}"
            );
        });
    }
}
