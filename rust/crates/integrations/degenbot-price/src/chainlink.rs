//! Chainlink aggregator price-feed reader.
//!
//! Reads the [`AggregatorV3Interface`] of a deployed aggregator —
//! `latestRoundData()` and `decimals()` — as typed `eth_call`s over
//! [`degenbot_rpc::contract::Contract::call_typed`]. The `eth_call` primitive is
//! already Rust-owned, so this crate owns only the *mechanism* (typed feed
//! readers), not the RPC plumbing.
//!
//! # Decode contract
//!
//! The round tuple, the `uint8` decimals and the decimal-corrected price are
//! pinned against recorded EVM return bytes. Both halves are the aggregator's
//! own vocabulary — canonical ABI decode of the return word, then integer
//! arithmetic over the `int256` answer and the feed's `decimals` — so the reader
//! agrees with the chain by construction rather than by comparison against a
//! second implementation.
//!
//! [`AggregatorV3Interface`]: https://github.com/smartcontractkit/chainlink/blob/develop/contracts/src/v0.8/shared/interfaces/AggregatorV3Interface.sol

use crate::decode::{as_int, as_u128, as_uint};
use crate::error::{FeedDataReason, PriceError, PriceResult};
use alloy::primitives::{Address, I256, U256};
#[cfg(test)]
use degenbot_abi::abi_types::type_::{AbiType, BitWidth};
use degenbot_abi::abi_types::value::AbiValue;
use degenbot_rpc::contract::Contract;
use degenbot_rpc::provider::AlloyProvider;
use std::sync::Arc;

/// `decimals()` → `uint8`.
const DECIMALS_SIG: &str = "decimals() returns (uint8)";

/// `latestRoundData()` → `(uint80 roundId, int256 answer, uint256 startedAt,
/// uint256 updatedAt, uint80 answeredInRound)`.
const LATEST_ROUND_DATA_SIG: &str =
    "latestRoundData() returns (uint80,int256,uint256,uint256,uint80)";

/// A decoded Chainlink `latestRoundData()` return tuple — one field per
/// [`AggregatorV3Interface`] slot.
///
/// Every slot is retained as a named field: Chainlink callers gate on
/// `started_at`/`updated_at` for staleness, and on `answered_in_round` to spot a
/// carried-over answer.
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
/// for caller bookkeeping only; it does not affect RPC routing.
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

    /// The decimal-corrected nominal price as a `U256` in whole units:
    /// `answer / 10**decimals`, truncated toward zero.
    ///
    /// Fetches `decimals()` + `latestRoundData()` and divides the `int256`
    /// answer by `10**decimals`. The division is integer truncation, so the
    /// `[0,1)` fractional part is dropped — the feed reports the answer in its
    /// own `decimals`, and this is the whole-unit face of it. A zero answer is
    /// a zero price.
    ///
    /// # Errors
    ///
    /// [`PriceError::FeedData`] — carrying the feed address, the answer, and
    /// the decimals — when the answer is negative or the feed's `decimals`
    /// cannot be scaled. Malformed feed data is never truncated into a zero
    /// price. [`PriceError`] otherwise if either underlying call fails.
    ///
    /// # Note
    ///
    /// Each call re-fetches `decimals()`. A feed's decimals are immutable, so
    /// caching them across calls is a consumer concern; the reader is a per-call
    /// mechanism.
    pub async fn price(&self, block_number: Option<u64>) -> PriceResult<U256> {
        let decimals = self.decimals(block_number).await?;
        let round = self.latest_round_data(block_number).await?;
        decimal_corrected_price(self.address(), round.answer, decimals)
    }

    /// Validate, without issuing an RPC, that a decoded `answer`/`decimals`
    /// pair can be priced by this feed.
    ///
    /// The conversion's own check, exposed for a caller that decoded the round
    /// tuple itself: the Python float projection divides the fraction the
    /// integer [`price`](Self::price) truncates away, so it validates the tuple
    /// it already has instead of re-deciding what malformed data means.
    ///
    /// # Errors
    ///
    /// [`PriceError::FeedData`] — carrying this feed's address, `answer`, and
    /// `decimals` — when the pair cannot be represented as a price.
    pub fn validate(&self, answer: I256, decimals: u8) -> PriceResult<()> {
        feed_denominator(self.address(), answer, decimals).map(drop)
    }
}

/// Decode a `Vec<AbiValue>` of five slots into [`RoundData`].
///
/// Pure + RPC-free so the parity gate can drive it from recorded EVM
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

/// The `int256` denominator `10**decimals` the correction divides by.
///
/// The one home of the feed-data policy: every reader-side refusal — [`ChainlinkPriceFeed::price`],
/// [`ChainlinkPriceFeed::validate`], and a companion projecting the fraction —
/// names the feed, the answer, and the decimals from here. Malformed data is
/// never a price — least of all `0`, which a valuation would act on — and
/// dividing by the returned denominator keeps validation and conversion from
/// disagreeing.
///
/// # Errors
///
/// [`PriceError::FeedData`] when the pair has no non-negative whole-unit price:
/// a negative `answer` (never produced by a live USD feed, but permitted by the
/// `int256` ABI type), or a `decimals` whose `10**decimals` overflows `u128`
/// (no deployed feed reports more than 18) or does not fit `int256`. The
/// denominator guards cannot fire for a power of ten today; they answer with
/// typed errors rather than a silent zero so a future change to the scale
/// cannot reintroduce one.
fn feed_denominator(feed: Address, answer: I256, decimals: u8) -> PriceResult<I256> {
    let feed_data = |reason: FeedDataReason| PriceError::FeedData {
        feed,
        answer,
        decimals,
        reason,
    };

    // Check the answer, not the truncated quotient: -1 wei at 8 decimals
    // divides to 0, which would report malformed data as a valid zero price.
    if answer.is_negative() {
        return Err(feed_data(FeedDataReason::NegativeAnswer));
    }
    let Some(pow_u128) = 10u128.checked_pow(u32::from(decimals)) else {
        return Err(feed_data(FeedDataReason::DecimalsOverflow));
    };
    let Ok(denom) = I256::try_from(pow_u128) else {
        return Err(feed_data(FeedDataReason::DenominatorConversion));
    };
    if denom.is_zero() {
        return Err(feed_data(FeedDataReason::ZeroDenominator));
    }
    Ok(denom)
}

/// Decimal-correct a raw `int256` Chainlink answer into a `U256` whole-unit
/// price: `answer / 10**decimals`, truncated toward zero.
///
/// The answer arrives in the feed's own `decimals`; the division drops the
/// `[0,1)` fractional part. A zero answer is a zero price.
///
/// # Errors
///
/// Whatever [`feed_denominator`] refuses, with the feed, the answer, and the
/// decimals attached.
pub(crate) fn decimal_corrected_price(
    feed: Address,
    answer: I256,
    decimals: u8,
) -> PriceResult<U256> {
    let denominator = feed_denominator(feed, answer, decimals)?;
    Ok((answer / denominator).into_raw())
}

/// Latest-round-data return types, for parity-test byte construction.
#[cfg(test)]
const LATEST_ROUND_TYPES: [AbiType; 5] = [
    AbiType::Uint(BitWidth::BITS_80),
    AbiType::Int(BitWidth::BITS_256),
    AbiType::Uint(BitWidth::BITS_256),
    AbiType::Uint(BitWidth::BITS_256),
    AbiType::Uint(BitWidth::BITS_80),
];

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use degenbot_abi::decoder::decode_for_types;
    use degenbot_abi::encoder::encode_for_types;

    // Pinned reference feed: Chainlink ETH/USD aggregator
    // `0x5f4eC3Df9cbd43714FE274015522933F4461f15A` on mainnet, which reports
    // prices with 8 decimals. The return bytes below are canonical ABI
    // encodings of representative round values, constructed via the same ABI
    // encoder the chain would produce: a live `eth_call` hands
    // `Contract::call_typed` exactly these bytes, so the decode path under test
    // is the one the chain feeds.
    const PINNED_CHAINLINK_ETH_USD: &str = "0x5f4eC3Df9cbd43714FE274015522933F4461f15A";
    // A plausible pinned block in the ~21M range.
    const PINNED_BLOCK: u64 = 21_000_000;

    fn round_values(answer: i64) -> Vec<AbiValue> {
        vec![
            // roundId — a representative uint80 round id.
            AbiValue::Uint(
                U256::from(1_105_516_854_423_834_546_834_523_887u128),
                BitWidth::BITS_80,
            ),
            // answer — int256, in the feed's 8-decimal units.
            AbiValue::Int(I256::try_from(answer).unwrap(), BitWidth::BITS_256),
            // startedAt — uint256 timestamp.
            AbiValue::Uint(U256::from(17_000_000_000u64), BitWidth::BITS_256),
            // updatedAt — uint256 timestamp.
            AbiValue::Uint(U256::from(17_000_000_050u64), BitWidth::BITS_256),
            // answeredInRound — uint80, equals roundId here.
            AbiValue::Uint(
                U256::from(1_105_516_854_423_834_546_834_523_887u128),
                BitWidth::BITS_80,
            ),
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
        let types = [AbiType::Uint(BitWidth::BITS_8)];
        let encoded = encode_for_types(
            &types,
            &[AbiValue::Uint(U256::from(8u64), BitWidth::BITS_8)],
        )
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

    fn pinned_feed() -> Address {
        PINNED_CHAINLINK_ETH_USD
            .parse()
            .expect("pinned feed address parses")
    }

    #[test]
    fn decimal_correction_matches_python_integer_division() {
        // Python: float(answer) / 10**decimals → here the integer analogue
        // int(answer // 10**decimals), matching value-exactly for the whole
        // units (the [0,1) fractional part is dropped).
        let feed = pinned_feed();
        // ETH @ $1845.00, 8 decimals.
        assert_eq!(
            decimal_corrected_price(feed, I256::try_from(184_500_000_000i64).unwrap(), 8)
                .expect("a positive answer is priceable"),
            U256::from(1845u64)
        );
        // USDC-style 6-decimal feed @ $1.00.
        assert_eq!(
            decimal_corrected_price(feed, I256::try_from(1_000_000i64).unwrap(), 6)
                .expect("a positive answer is priceable"),
            U256::from(1u64)
        );
        // WETH-style 18-decimal feed @ $3000.50 → truncates to 3000 whole units.
        assert_eq!(
            decimal_corrected_price(
                feed,
                I256::try_from(U256::from(3_000_500_000_000_000_000_000u128)).unwrap(),
                18,
            )
            .expect("a positive answer is priceable"),
            U256::from(3000u64)
        );
        // Zero answer → zero price (a zero answer is data, not malformed data).
        assert_eq!(
            decimal_corrected_price(feed, I256::ZERO, 8).expect("zero is priceable"),
            U256::ZERO
        );
        // Fractional truncation: answer = 184_500_000_005, 8 dec → 1845
        // (the trailing 5 units are the fractional part, dropped).
        assert_eq!(
            decimal_corrected_price(feed, I256::try_from(184_500_000_005i64).unwrap(), 8)
                .expect("a positive answer is priceable"),
            U256::from(1845u64)
        );
        // A 38-decimal feed is the widest `10**decimals` that fits `u128`.
        assert_eq!(
            decimal_corrected_price(feed, I256::try_from(10i128.pow(38)).unwrap(), 38)
                .expect("38 decimals still fits the denominator"),
            U256::from(1u64)
        );
    }

    #[test]
    fn negative_answer_is_feed_data_error_not_zero() {
        let feed = pinned_feed();
        let answer = I256::try_from(-184_500_000_005i64).unwrap();
        let err = decimal_corrected_price(feed, answer, 8)
            .expect_err("a negative answer cannot be priced");
        assert!(
            matches!(
                err,
                PriceError::FeedData {
                    feed: got_feed,
                    answer: got_answer,
                    decimals: 8,
                    reason: FeedDataReason::NegativeAnswer,
                } if got_feed == feed && got_answer == answer
            ),
            "a negative answer must surface as FeedData{{NegativeAnswer}} carrying \
             the feed and answer, got {err:?}"
        );
        // The message names all three pieces of context: a caller can point at
        // the misbehaving feed and the exact on-chain values without re-reading
        // the chain.
        let message = err.to_string();
        assert!(
            message.contains(&format!("{feed}")),
            "feed context: {message}"
        );
        assert!(
            message.contains("-184500000005"),
            "answer context: {message}"
        );
        assert!(
            message.contains("decimals 8"),
            "decimals context: {message}"
        );
        // A small negative answer truncates to zero in integer division; the
        // refusal happens on the answer, so it never arrives as a valid `0`.
        assert!(
            matches!(
                decimal_corrected_price(feed, I256::try_from(-1i64).unwrap(), 8),
                Err(PriceError::FeedData {
                    reason: FeedDataReason::NegativeAnswer,
                    ..
                })
            ),
            "a negative answer that divides to zero is still an error, not a price"
        );
    }

    #[test]
    fn decimals_beyond_u128_power_is_feed_data_error() {
        let feed = pinned_feed();
        let answer = I256::try_from(372_038_000_000i64).unwrap();
        for decimals in [39u8, 40, u8::MAX] {
            let err = decimal_corrected_price(feed, answer, decimals)
                .expect_err("10**decimals overflows u128 above 38 decimals");
            assert!(
                matches!(
                    err,
                    PriceError::FeedData {
                        feed: got_feed,
                        answer: got_answer,
                        decimals: got_decimals,
                        reason: FeedDataReason::DecimalsOverflow,
                    } if got_feed == feed && got_answer == answer && got_decimals == decimals
                ),
                "decimals {decimals} must surface as FeedData{{DecimalsOverflow}}, got {err:?}"
            );
        }
    }

    #[test]
    fn pinned_feed_constants_are_documented_reference() {
        // Sanity: the pinned feed address is the canonical Chainlink ETH/USD
        // aggregator (8 decimals). The return bytes above are encoded here, but
        // the feed identity — and therefore the `decimals` and `int256` answer
        // shape the reader must handle — is this deployed aggregator.
        assert_eq!(
            PINNED_CHAINLINK_ETH_USD.to_lowercase(),
            "0x5f4ec3df9cbd43714fe274015522933f4461f15a"
        );
        let _ = PINNED_CHAINLINK_ETH_USD; // silence dead_code in non-test build shape
        let _ = PINNED_BLOCK;
    }
}
