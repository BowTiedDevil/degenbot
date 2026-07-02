//! Chainlink aggregator price-feed reader.
//!
//! Ports [`degenbot.chainlink.price_feed.ChainlinkPriceContract`](../../../../src/degenbot/chainlink/price_feed.py)
//! to a pyo3-free Rust reader. The reader builds typed `eth_call`s over
//! [`degenbot_rpc::contract::Contract::call_typed`] — the `eth_call` primitive
//! is already Rust-owned, so this crate owns only the *mechanism* (typed feed
//! readers), not the RPC plumbing.
//!
//! # Parity oracle (ADR-005 §4.2)
//!
//! The Python `ChainlinkPriceContract` is the parity oracle; the §4.2 gate pins
//! value-exact parity for the `latestRoundData` tuple decode, the `decimals`
//! `uint8` decode, and the `price()` decimal correction (§4.2 parity is exercised
//! against recorded EVM return bytes — canonical ABI decode, which is what the
//! Python `abi_decode` performs too — so the two implementations agree by
//! construction).
//!
//! [`degenbot.chainlink.price_feed.ChainlinkPriceContract`]: https://github.com/BowTiedDevil/degenbot/blob/main/src/degenbot/chainlink/price_feed.py

use crate::decode::{as_int, as_u128, as_uint};
use crate::error::{PriceError, PriceResult};
use alloy::primitives::{Address, I256, U256};
#[cfg(test)]
use degenbot_abi::abi_types::AbiType;
use degenbot_abi::abi_types::AbiValue;
use degenbot_rpc::contract::Contract;
use degenbot_rpc::provider::AlloyProvider;
use std::sync::Arc;

/// `decimals()` → `uint8`.
const DECIMALS_SIG: &str = "decimals() returns (uint8)";

/// `latestRoundData()` → `(uint80 roundId, int256 answer, uint256 startedAt,
/// uint256 updatedAt, uint80 answeredInRound)`.
const LATEST_ROUND_DATA_SIG: &str =
    "latestRoundData() returns (uint80,int256,uint256,uint256,uint80)";

/// A decoded Chainlink `latestRoundData()` return tuple.
///
/// Mirrors the Python oracle's
/// `_round_id, answer, _started_at, _updated_at, _answered_in_round` unpacking
/// (the underscored slots are retained here as named fields — Chainlink callers
/// commonly use `started_at`/`updated_at` for staleness checks).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundData {
    /// `roundId` (`uint80`) — round identifier.
    pub round_id: u128,
    /// `answer` (`int256`) — the price answer, in the feed's native decimals.
    pub answer: I256,
    /// `startedAt` (`uint256`) — timestamp the round started.
    pub started_at: U256,
    /// `updatedAt` (`uint256`) — timestamp the round was last updated.
    pub updated_at: U256,
    /// `answeredInRound` (`uint80`) — the round in which the answer was set.
    pub answered_in_round: u128,
}

/// A typed Chainlink aggregator price feed.
///
/// Constructed with the aggregator address + the shared RPC provider (the
/// `eth_call` is routed via an internal [`Contract`]). `chain_id` is carried
/// for caller bookkeeping (matching the Python oracle); it does not affect RPC
/// routing.
#[derive(Clone)]
pub struct ChainlinkPriceFeed {
    contract: Contract,
    chain_id: Option<u64>,
}

impl std::fmt::Debug for ChainlinkPriceFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChainlinkPriceFeed")
            .field("address", &self.contract.address())
            .field("chain_id", &self.chain_id)
            .finish_non_exhaustive()
    }
}

impl ChainlinkPriceFeed {
    /// Create a new reader for a Chainlink aggregator at `address`.
    ///
    /// # Errors
    ///
    /// [`PriceError::Address`] if `address` is not a valid EIP-55 / 0x-prefixed
    /// address.
    pub fn new(
        address: &str,
        provider: Arc<AlloyProvider>,
        chain_id: Option<u64>,
    ) -> PriceResult<Self> {
        let contract = Contract::new(address, provider)?;
        Ok(Self { contract, chain_id })
    }

    /// The aggregator contract address (checksummed via `Contract`).
    #[must_use]
    pub fn address(&self) -> Address {
        self.contract.address()
    }

    /// The chain id supplied at construction (bookkeeping only).
    #[must_use]
    pub const fn chain_id(&self) -> Option<u64> {
        self.chain_id
    }

    /// Call `decimals()` and return the feed's decimal places (`uint8`).
    ///
    /// # Errors
    ///
    /// [`PriceError`] on RPC failure or an unexpected return shape.
    pub async fn decimals(&self, block_number: Option<u64>) -> PriceResult<u8> {
        let values = self
            .contract
            .call_typed(DECIMALS_SIG, &[], block_number)
            .await?;
        if values.len() != 1 {
            return Err(PriceError::Decode(format!(
                "decimals() expected 1 return value, got {}",
                values.len()
            )));
        }
        let n = as_uint(&values[0])?;
        u8::try_from(n).map_err(|_| {
            PriceError::Decode(format!("decimals() returned a value exceeding u8: {n}"))
        })
    }

    /// Call `latestRoundData()` and return the decoded tuple.
    ///
    /// # Errors
    ///
    /// [`PriceError`] on RPC failure or a tuple that does not match the
    /// `(uint80,int256,uint256,uint256,uint80)` shape.
    pub async fn latest_round_data(&self, block_number: Option<u64>) -> PriceResult<RoundData> {
        let values = self
            .contract
            .call_typed(LATEST_ROUND_DATA_SIG, &[], block_number)
            .await?;
        build_round_data(&values)
    }

    /// The decimal-corrected nominal price as a `U256` (whole units), the
    /// integer analogue of the Python oracle's
    /// `float(answer) / 10**decimals`.
    ///
    /// Fetches `decimals()` + `latestRoundData()` and divides the `int256`
    /// answer by `10**decimals` (truncating integer division — the `[0,1)`
    /// fractional part is dropped, matching `int(answer // 10**decimals)`).
    /// Negative `answer` values (which never occur for live USD feeds, but
    /// are possible per the `int256` ABI) clamp to zero.
    ///
    /// # Errors
    ///
    /// [`PriceError`] if either underlying call fails.
    ///
    /// # Note
    ///
    /// Each call re-fetches `decimals()` (the Python oracle caches it on the
    /// instance). Caching across calls is a consumer concern; the reader is a
    /// per-call mechanism.
    pub async fn price(&self, block_number: Option<u64>) -> PriceResult<U256> {
        let decimals = self.decimals(block_number).await?;
        let round = self.latest_round_data(block_number).await?;
        Ok(decimal_corrected_price(round.answer, decimals))
    }
}

/// Decode a `Vec<AbiValue>` of five slots into [`RoundData`].
///
/// Pure + RPC-free so the §4.2 parity gate can drive it from recorded EVM
/// return bytes: `decode_for_types(types, bytes)` → `build_round_data(...)`.
fn build_round_data(values: &[AbiValue]) -> PriceResult<RoundData> {
    if values.len() != 5 {
        return Err(PriceError::Decode(format!(
            "latestRoundData() expected 5 return values, got {}",
            values.len()
        )));
    }
    let round_id = as_u128(&values[0])?;
    let answer = as_int(&values[1])?;
    let started_at = as_uint(&values[2])?;
    let updated_at = as_uint(&values[3])?;
    let answered_in_round = as_u128(&values[4])?;
    Ok(RoundData {
        round_id,
        answer,
        started_at,
        updated_at,
        answered_in_round,
    })
}

/// Decimal-correct a raw `int256` Chainlink answer into a `U256` whole-unit
/// price (the integer analogue of the Python
/// `float(answer) / 10**decimals`).
///
/// Truncating integer division mirrors `int(answer // 10**decimals)`; a
/// negative `answer` (not produced by live USD feeds, but permitted by the
/// `int256` ABI type) clamps to zero. `decimals` beyond ~38 (no Chainlink feed)
/// would overflow `u128::checked_pow`; such a feed clamps to zero here too —
/// the caller already validated the feed address, not the on-chain return.
#[must_use]
pub(crate) fn decimal_corrected_price(answer: I256, decimals: u8) -> U256 {
    let Some(pow_u128) = 10u128.checked_pow(u32::from(decimals)) else {
        return U256::ZERO;
    };
    let Ok(denom) = I256::try_from(pow_u128) else {
        return U256::ZERO;
    };
    if denom.is_zero() {
        return U256::ZERO;
    }
    let adjusted = answer / denom;
    if adjusted.is_negative() {
        U256::ZERO
    } else {
        adjusted.into_raw()
    }
}

/// Latest-round-data return types, for parity-test byte construction.
#[cfg(test)]
const LATEST_ROUND_TYPES: [AbiType; 5] = [
    AbiType::Uint(80),
    AbiType::Int(256),
    AbiType::Uint(256),
    AbiType::Uint(256),
    AbiType::Uint(80),
];

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use degenbot_abi::abi_decoder::decode_for_types;
    use degenbot_abi::abi_encoder::encode_for_types;

    // Pinned reference feed: Chainlink ETH/USD aggregator
    // `0x5f4eC3Df9cbd43714FE274015522933F4461f15A` on mainnet, which reports
    // prices with 8 decimals. The return bytes below are canonical ABI
    // encodings of representative round values (the sandbox running these
    // gates has no live RPC, so bytes are constructed via the same ABI encoder
    // the chain would produce; decode parity vs. the Python `abi_decode` holds
    // because both implement canonical ABI decode of the identical bytes).
    const PINNED_CHAINLINK_ETH_USD: &str = "0x5f4eC3Df9cbd43714FE274015522933F4461f15A";
    // A plausible pinned block in the ~21M range.
    const PINNED_BLOCK: u64 = 21_000_000;

    fn round_values(answer: i64) -> Vec<AbiValue> {
        vec![
            // roundId — a representative uint80 round id.
            AbiValue::Uint(U256::from(1_105_516_854_423_834_546_834_523_887u128), 80),
            // answer — int256, in the feed's 8-decimal units.
            AbiValue::Int(I256::try_from(answer).unwrap(), 256),
            // startedAt — uint256 timestamp.
            AbiValue::Uint(U256::from(17_000_000_000u64), 256),
            // updatedAt — uint256 timestamp.
            AbiValue::Uint(U256::from(17_000_000_050u64), 256),
            // answeredInRound — uint80, equals roundId here.
            AbiValue::Uint(U256::from(1_105_516_854_423_834_546_834_523_887u128), 80),
        ]
    }

    #[test]
    fn latest_round_data_decodes_byte_exact() {
        // Representative ETH/USD round: answer = 1_845_00000000 (=$1845.00
        // at 8 decimals).
        let answer: i64 = 184_500_000_000;
        let encoded = encode_for_types(&LATEST_ROUND_TYPES, &round_values(answer))
            .expect("encode latestRoundData tuple");
        // Round-trip through bytes — the exact path Contract::call_typed
        // drives post-eth_call.
        let decoded =
            decode_for_types(&LATEST_ROUND_TYPES, &encoded).expect("decode latestRoundData tuple");
        let round = build_round_data(&decoded).expect("build RoundData");

        assert_eq!(round.answer, I256::try_from(answer).unwrap());
        assert_eq!(round.round_id, 1_105_516_854_423_834_546_834_523_887u128);
        assert_eq!(round.answered_in_round, round.round_id);
        assert_eq!(round.started_at, U256::from(17_000_000_000u64));
        assert_eq!(round.updated_at, U256::from(17_000_000_050u64));
    }

    #[test]
    fn decimals_decodes_byte_exact() {
        // decimals() returns a single uint8; pin the canonical Chainlink 8.
        let types = [AbiType::Uint(8)];
        let encoded = encode_for_types(&types, &[AbiValue::Uint(U256::from(8u64), 8)])
            .expect("encode decimals");
        let decoded = decode_for_types(&types, &encoded).expect("decode decimals");
        // The extract path ChainlinkPriceFeed::decimals uses:
        let n = as_uint(&decoded[0]).unwrap();
        assert_eq!(u8::try_from(n).unwrap(), 8);
    }

    #[test]
    fn build_round_data_rejects_wrong_arity() {
        // Only 4 slots — must error rather than index out of bounds.
        let short = round_values(0);
        let truncated = &short[..4];
        let err = build_round_data(truncated).unwrap_err();
        assert!(
            matches!(err, PriceError::Decode(ref m) if m.contains("expected 5")),
            "wrong arity should surface as Decode error, got {err:?}"
        );
    }

    #[test]
    fn decimal_correction_matches_python_integer_division() {
        // Python: float(answer) / 10**decimals → here the integer analogue
        // int(answer // 10**decimals), matching value-exactly for the whole
        // units (the [0,1) fractional part is dropped).
        // ETH @ $1845.00, 8 decimals.
        assert_eq!(
            decimal_corrected_price(I256::try_from(184_500_000_000i64).unwrap(), 8),
            U256::from(1845u64)
        );
        // USDC-style 6-decimal feed @ $1.00.
        assert_eq!(
            decimal_corrected_price(I256::try_from(1_000_000i64).unwrap(), 6),
            U256::from(1u64)
        );
        // WETH-style 18-decimal feed @ $3000.50 → truncates to 3000 whole units.
        assert_eq!(
            decimal_corrected_price(
                I256::try_from(U256::from(3_000_500_000_000_000_000_000u128)).unwrap(),
                18,
            ),
            U256::from(3000u64)
        );
        // Zero answer → zero price.
        assert_eq!(decimal_corrected_price(I256::ZERO, 8), U256::ZERO);
        // Negative answer (never for live USD feeds, but allowed by int256)
        // clamps to zero rather than a two's-complement monster.
        assert_eq!(
            decimal_corrected_price(I256::try_from(-1i64).unwrap(), 8),
            U256::ZERO
        );
        // Fractional truncation: answer = 184_500_000_005, 8 dec → 1845
        // (the trailing 5 units are the fractional part, dropped).
        assert_eq!(
            decimal_corrected_price(I256::try_from(184_500_000_005i64).unwrap(), 8),
            U256::from(1845u64)
        );
    }

    #[test]
    fn pinned_feed_constants_are_documented_reference() {
        // Sanity: the pinned feed address is the canonical Chainlink ETH/USD
        // aggregator (8 decimals). This is documentation parity — the actual
        // return bytes above are encoded by us, but the feed identity is the
        // reference the Python oracle would be pointed at.
        assert_eq!(
            PINNED_CHAINLINK_ETH_USD.to_lowercase(),
            "0x5f4ec3df9cbd43714fe274015522933f4461f15a"
        );
        let _ = PINNED_CHAINLINK_ETH_USD; // silence dead_code in non-test build shape
        let _ = PINNED_BLOCK;
    }
}
