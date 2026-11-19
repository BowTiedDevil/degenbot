//! Integration coverage: a *successful* RPC answering malformed feed data must
//! surface a typed error, never a valid zero price.
//!
//! The provider is the offline recorded-chain transport — every `eth_call`
//! returns canonical ABI bytes, exactly as a node answers a call that did not
//! revert. Nothing in these cases fails at the transport: only the data is
//! wrong. That makes the reader's own refusal the sole thing under test, over
//! the whole custody chain (`decimals()` + `latestRoundData()` decode → decimal
//! correction → `PriceResult`).

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test assertions fail loudly"
)]

use alloy::primitives::{Address, I256, U256};
use degenbot_abi::abi_types::{AbiType, AbiValue};
use degenbot_abi::encoder::encode_for_types;
use degenbot_price::{ChainlinkPriceFeed, FeedDataReason, PriceError};
use degenbot_rpc::offline::{OfflineProvider, RecordedBlock};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// The aggregator under test: the deployed Chainlink ETH/USD proxy.
const FEED: &str = "0x5f4eC3Df9cbd43714FE2740f5e3616155c5b8419";

/// `decimals()` calldata.
const DECIMALS_CALLDATA: &str = "0x313ce567";

/// `latestRoundData()` calldata.
const LATEST_ROUND_DATA_CALLDATA: &str = "0xfeaf968c";

const RECORDED_TIMESTAMP: u64 = 17_000_000_000;

// One recorded block per data shape: same aggregator, same answering node, and
// only the returned answer (or `decimals`) differing.
const BLOCK_PRICEABLE: u64 = 21_000_000;
const BLOCK_ZERO: u64 = 21_000_001;
const BLOCK_NEGATIVE: u64 = 21_000_002;
const BLOCK_DECIMALS_OVERFLOW: u64 = 21_000_003;

const ROUND_TYPES: [AbiType; 5] = [
    AbiType::Uint(80),
    AbiType::Int(256),
    AbiType::Uint(256),
    AbiType::Uint(256),
    AbiType::Uint(80),
];

/// The canonical ABI return bytes of `latestRoundData()` for `answer`.
fn latest_round_data_return(answer: I256) -> String {
    let values = [
        AbiValue::Uint(U256::from(1u64), 80),
        AbiValue::Int(answer, 256),
        AbiValue::Uint(U256::from(RECORDED_TIMESTAMP), 256),
        AbiValue::Uint(U256::from(RECORDED_TIMESTAMP + 1), 256),
        AbiValue::Uint(U256::from(1u64), 80),
    ];
    alloy::hex::encode(encode_for_types(&ROUND_TYPES, &values).expect("encode latestRoundData"))
}

/// The canonical ABI return word of `decimals()`.
fn decimals_return(decimals: u8) -> String {
    let values = [AbiValue::Uint(U256::from(decimals), 8)];
    alloy::hex::encode(encode_for_types(&[AbiType::Uint(8)], &values).expect("encode decimals"))
}

/// A recorded block answering the reader's two calls for `FEED`.
fn recorded_block(answer: I256, decimals: u8) -> RecordedBlock {
    let to = FEED.to_lowercase();
    let calls = HashMap::from([
        (
            format!("{to}:{DECIMALS_CALLDATA}"),
            Some(decimals_return(decimals)),
        ),
        (
            format!("{to}:{LATEST_ROUND_DATA_CALLDATA}"),
            Some(latest_round_data_return(answer)),
        ),
    ]);
    RecordedBlock {
        timestamp: RECORDED_TIMESTAMP,
        calls,
        ..RecordedBlock::default()
    }
}

/// The reader over an offline node that answers `block` with `answer`/
/// `decimals`.
fn feed_answering(block: u64, answer: I256, decimals: u8) -> ChainlinkPriceFeed {
    let blocks = BTreeMap::from([(block, recorded_block(answer, decimals))]);
    let recorded = serde_json::json!({ "chain_id": 1, "blocks": blocks }).to_string();
    let provider = OfflineProvider::from_json_str(&recorded)
        .expect("recorded chain data parses")
        .as_alloy_provider();
    ChainlinkPriceFeed::new(FEED, Arc::new(provider), Some(1)).expect("feed address parses")
}

fn feed_address() -> Address {
    FEED.parse().expect("pinned feed address parses")
}

/// `validate` answers from its arguments alone, so a companion projecting the
/// fractional float can refuse malformed data without a second read. Its
/// rejection is the reading path's own, context included.
#[test]
fn validate_reports_malformed_feed_data_without_a_call() {
    let priceable = I256::try_from(372_038_000_000i64).unwrap();
    let feed = feed_answering(BLOCK_PRICEABLE, priceable, 8);

    assert!(feed.validate(I256::ZERO, 8).is_ok());
    assert!(feed.validate(priceable, 8).is_ok());

    let err = feed
        .validate(I256::MINUS_ONE, 8)
        .expect_err("a negative answer is not priceable");
    assert!(
        matches!(
            err,
            PriceError::FeedData {
                feed: got_feed,
                answer,
                decimals: 8,
                reason: FeedDataReason::NegativeAnswer,
            } if got_feed == feed_address() && answer == I256::MINUS_ONE
        ),
        "validate must reject a negative answer as FeedData{{NegativeAnswer}}, got {err:?}"
    );

    let err = feed
        .validate(priceable, 40)
        .expect_err("40 decimals is not priceable");
    assert!(
        matches!(
            err,
            PriceError::FeedData {
                feed: got_feed,
                decimals: 40,
                reason: FeedDataReason::DecimalsOverflow,
                ..
            } if got_feed == feed_address()
        ),
        "validate must reject an unscalable decimals as FeedData{{DecimalsOverflow}}, got {err:?}"
    );
}

#[tokio::test]
async fn rpc_success_with_priceable_data_keeps_whole_unit_truncation() {
    // $3720.38 at 8 decimals: the fractional part is dropped, as before.
    let feed = feed_answering(
        BLOCK_PRICEABLE,
        I256::try_from(372_038_000_000i64).unwrap(),
        8,
    );

    assert_eq!(
        feed.price(Some(BLOCK_PRICEABLE))
            .await
            .expect("a priceable answer is not an error"),
        U256::from(3720u64)
    );
}

#[tokio::test]
async fn rpc_success_with_zero_answer_is_a_zero_price() {
    let feed = feed_answering(BLOCK_ZERO, I256::ZERO, 8);

    assert_eq!(
        feed.price(Some(BLOCK_ZERO))
            .await
            .expect("a zero answer is data, not malformed data"),
        U256::ZERO
    );
}

#[tokio::test]
async fn rpc_success_with_negative_answer_is_not_a_zero_price() {
    // -1 at 8 decimals divides to zero, so a post-division clamp reports this
    // broken answer as a valid $0 price. It must be an error carrying the feed,
    // the answer, and the decimals.
    let feed = feed_answering(BLOCK_NEGATIVE, I256::MINUS_ONE, 8);

    let err = feed
        .price(Some(BLOCK_NEGATIVE))
        .await
        .expect_err("a negative answer has no price");

    assert!(
        matches!(
            err,
            PriceError::FeedData {
                feed: got_feed,
                answer,
                decimals: 8,
                reason: FeedDataReason::NegativeAnswer,
            } if got_feed == feed_address() && answer == I256::MINUS_ONE
        ),
        "a negative answer must surface as FeedData{{NegativeAnswer}} for the feed, got {err:?}"
    );

    let message = err.to_string();
    assert!(
        message.contains(&format!("{}", feed_address())),
        "feed context: {message}"
    );
    assert!(message.contains("-1"), "answer context: {message}");
    assert!(
        message.contains("decimals 8"),
        "decimals context: {message}"
    );
}

#[tokio::test]
async fn rpc_success_with_overflowing_decimals_is_not_a_price() {
    // 40 decimals cannot be scaled: `10**40` overflows the exponent range, so
    // a priceable-looking answer is still unrepresentable.
    let feed = feed_answering(
        BLOCK_DECIMALS_OVERFLOW,
        I256::try_from(372_038_000_000i64).unwrap(),
        40,
    );

    let err = feed
        .price(Some(BLOCK_DECIMALS_OVERFLOW))
        .await
        .expect_err("40 decimals has no representable price");

    assert!(
        matches!(
            err,
            PriceError::FeedData {
                feed: got_feed,
                decimals: 40,
                reason: FeedDataReason::DecimalsOverflow,
                ..
            } if got_feed == feed_address()
        ),
        "an overflowing scale must surface as FeedData{{DecimalsOverflow}}, got {err:?}"
    );
}
