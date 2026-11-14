//! PancakeSwap V3 Swap event decoder.
//!
//! **Storage-layout divergence :** this event decoder
//! decodes Swap events (which carry the state transition in their data). It
//! does NOT read raw storage slots. But be aware the fork's on-chain **storage
//! layout** also diverges from Uniswap V3 — `slot0.feeProtocol` is a `uint32`
//! so `Slot0` spans two words and `liquidity`/`ticks`/`tickBitmap` shift to
//! slots 5/6/7 (Uniswap 4/5/6). Any direct slot-based seed/serve of a pancake
//! pool MUST use `degenbot_pools::v3_pancakeswap_storage_slots`, never the
//! Uniswap `v3_storage_slots` encoders. Event-driven state sync is unaffected.
//!
//! Decodes `Swap(address,address,int256,int256,uint160,uint128,int24,uint128,uint128)`
//! events from PancakeSwap V3 pool contracts. PancakeSwap V3 forked Uniswap V3
//! but REPLACED Uniswap's single trailing `uint24 fee` field with two
//! `uint128 protocolFeesToken0/1` fields, which changed its `topic[0]` from the
//! canonical Uniswap V3 `0xc42079f9…` to `0x19b47279…`. The degenbot
//! `V3_SWAP_TOPIC` matches ONLY the Uniswap V3 hash, so PancakeSwap V3 swaps
//! were never decoded; their pool state froze while on-chain price drifted, and
//! the solver manufactured phantom arbitrage from the stale state (see the
//! exploration-no-profit-crash writeup, removed in the stale-docs cleanup `71ec78b2`).
//!
//! The fields that drive pool-state updates are byte-identical to Uniswap V3:
//! `amount0`, `amount1`, `sqrtPriceX96`, `liquidity`, `tick` occupy the same
//! first 160 data bytes (verified against on-chain `slot0()`/`liquidity()` at
//! the event block — each word matches exactly). Only `topic[0]` differs and
//! the trailing two words (the protocol-fee accumulators, unused by the
//! swap-state update) add 64 bytes of data. ABI confirmed against the verified
//! `PancakeV3Pool.sol` source (Etherscan, solidity 0.7.6).
//!
//! ```text
//! event Swap(
//!     address indexed sender,
//!     address indexed recipient,
//!     int256 amount0,
//!     int256 amount1,
//!     uint160 sqrtPriceX96,
//!     uint128 liquidity,
//!     int24  tick,
//!     uint128 protocolFeesToken0,  // extra (recorded 0)
//!     uint128 protocolFeesToken1   // extra (recorded ~1.2e13..1.6e13)
//! )
//!
//! signature = Swap(address,address,int256,int256,uint160,uint128,int24,uint128,uint128)
//! topic[0] = 0x19b47279256b2a23a1665c810c8d55a1758940ee09377d4f8d26497a3577dc83
//! topic[1] = sender (indexed)
//! topic[2] = recipient (indexed)
//! data    = abi.encode(int256,int256,uint160,uint128,int24,uint128,uint128)
//!         = 224 bytes (7 × 32-byte words)
//! ```

#![expect(clippy::doc_markdown)]

use crate::uniswap_tick_range::validate_tick;
use alloy::primitives::{b256, B256, U128, U256};
use alloy::rpc::types::Log;
use alloy::sol_types::SolEvent;

alloy::sol! {
    /// The PancakeSwap V3 fork's swap event: the V3 state words plus the fork's
    /// two `uint128` protocol-fee accumulators, which are what moved its
    /// topic0.
    event Swap(
        address indexed sender,
        address indexed recipient,
        int256 amount0,
        int256 amount1,
        uint160 sqrtPriceX96,
        uint128 liquidity,
        int24 tick,
        uint128 protocolFeesToken0,
        uint128 protocolFeesToken1,
    );
}

/// Keccak256 of `Swap(address,address,int256,int256,uint160,uint128,int24,uint128,uint128)`
/// — the PancakeSwap V3 Swap topic0 (confirmed against the verified
/// `PancakeV3Pool.sol` source; differs from the canonical Uniswap V3
/// `V3_SWAP_TOPIC`).
pub const V3_PANCAKESWAP_SWAP_TOPIC: B256 =
    b256!("0x19b47279256b2a23a1665c810c8d55a1758940ee09377d4f8d26497a3577dc83");

/// Decoded PancakeSwap V3 Swap event carrying post-swap state. Field layout is
/// shared with the Uniswap V3 decoder; only the topic and trailing words differ.
pub type V3PancakeSwapEvent = crate::v3_swap_decoder::V3SwapEvent;

/// Decode a PancakeSwap V3 Swap event from a log.
///
/// Returns `Some(V3PancakeSwapEvent)` (reusing the Uniswap V3 event type — the
/// state fields are identical) if the log is a valid PancakeSwap Swap with
/// correctly formatted data. Returns `None` if:
/// - The log's topics are not exactly the declared three (signature + sender +
///   recipient) or the first one doesn't match
///   [`V3_PANCAKESWAP_SWAP_TOPIC`] — the declaration binds the topic count, so
///   a surplus topic is a refusal, not an ignored word
/// - The data is too short (< 224 bytes — 5 state words + 2 extra words) or a
///   state word is not canonical for its declared width, or an indexed word is
///   not a canonical `address` slot
/// - The tick is outside the Uniswap V3 tick range
#[must_use]
pub fn decode_v3_pancakeswap_swap_log(log: &Log) -> Option<V3PancakeSwapEvent> {
    // The fork's own declaration is the bound: it carries the two trailing
    // `uint128` protocol-fee words, so a log shorter than the fork's 224 bytes
    // fails here, while the five state words are decoded VALIDATING against the
    // widths they share with Uniswap V3.
    let log = Swap::decode_log_validate(&log.inner).ok()?;

    // The trailing protocol-fee accumulators are not surfaced on the shared
    // event type — no reader updates pool state from them.
    Some(V3PancakeSwapEvent {
        pool_address: log.address,
        sender: log.data.sender,
        recipient: log.data.recipient,
        amount0: log.data.amount0,
        amount1: log.data.amount1,
        sqrt_price_x96: U256::from(log.data.sqrtPriceX96),
        liquidity: U128::from(log.data.liquidity),
        tick: validate_tick(log.data.tick)?,
    })
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use alloy::primitives::{Address, Bytes, Log as InnerLog, I256};

    /// A real on-chain PancakeSwap V3 Swap at block 25655667, pool
    /// 0x1ac1a8fe (USDC/WETH). The five state words were independently verified
    /// against the pool's `slot0()`/`liquidity()` at that block.
    fn real_pancake_swap_log() -> Log {
        let pool: Address = "0x1ac1a8feaaea1900c4166deeed0c11cc10669d36"
            .parse()
            .unwrap();
        // amount0 = -174394871 (USDC, 6dp)
        let amount0 = I256::unchecked_from(-174_394_871_i128);
        // amount1 = 93723484111872751 (~0.0937 WETH * 1e18)
        let amount1 = I256::unchecked_from(93_723_484_111_872_751_i128);
        let sqrt16 = 1_836_421_284_110_994_083_605_581_794_057_088_u128.to_be_bytes();
        let mut sqrt_word = [0u8; 32];
        sqrt_word[16..32].copy_from_slice(&sqrt16);
        let sqrt = U256::from_be_bytes(sqrt_word);
        let liq = U128::from(19_957_515_104_251_009_u64);
        let tick = 201_029i32;

        let mut data = Vec::with_capacity(224);
        data.extend_from_slice(&amount0.to_be_bytes::<32>());
        data.extend_from_slice(&amount1.to_be_bytes::<32>());
        data.extend_from_slice(&sqrt.to_be_bytes::<32>());
        let liq_bytes = liq.to_be_bytes::<16>();
        let mut liq_word = [0u8; 32];
        liq_word[16..32].copy_from_slice(&liq_bytes);
        data.extend_from_slice(&liq_word);
        let tick_i256 = I256::try_from(i128::from(tick)).unwrap_or(I256::ZERO);
        data.extend_from_slice(&tick_i256.to_be_bytes::<32>());
        // two extra PancakeSwap words
        data.extend_from_slice(&[0u8; 32]);
        data.extend_from_slice(&U256::from(15_932_992_299_018_u64).to_be_bytes::<32>());

        let sender: Address = "0xbdb3ba9ffe392549e1f8658dd2630c141fdf47b6"
            .parse()
            .unwrap();
        let recipient = sender;
        let inner = InnerLog::new_unchecked(
            pool,
            vec![
                V3_PANCAKESWAP_SWAP_TOPIC,
                sender.into_word(),
                recipient.into_word(),
            ],
            Bytes::from(data),
        );
        Log {
            inner,
            block_hash: None,
            block_number: None,
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        }
    }

    #[test]
    fn decode_real_pancake_swap() {
        let event = decode_v3_pancakeswap_swap_log(&real_pancake_swap_log()).unwrap();
        // Pool + post-swap state match on-chain slot0()/liquidity() at 25655667.
        assert_eq!(
            event.pool_address,
            "0x1ac1a8feaaea1900c4166deeed0c11cc10669d36"
                .parse::<Address>()
                .unwrap()
        );
        assert_eq!(event.amount0, I256::unchecked_from(-174_394_871));
        assert_eq!(
            event.amount1,
            I256::unchecked_from(93_723_484_111_872_751_i128)
        );
        assert_eq!(event.sqrt_price_x96, {
            let b16 = 1_836_421_284_110_994_083_605_581_794_057_088_u128.to_be_bytes();
            let mut w = [0u8; 32];
            w[16..32].copy_from_slice(&b16);
            U256::from_be_bytes(w)
        });
        assert_eq!(event.liquidity, U128::from(19_957_515_104_251_009_u64));
        assert_eq!(event.tick, 201_029);
    }

    #[test]
    fn wrong_topic_returns_none() {
        // A canonical Uniswap V3 Swap log must NOT be accepted by the
        // PancakeSwap decoder (topic mismatch), and vice-versa.
        let mut log = real_pancake_swap_log();
        log.inner = InnerLog::new_unchecked(
            log.inner.address,
            vec![
                crate::v3_swap_decoder::V3_SWAP_TOPIC,
                log.inner.data.topics()[1],
                log.inner.data.topics()[2],
            ],
            log.inner.data.data.clone(),
        );
        assert!(decode_v3_pancakeswap_swap_log(&log).is_none());
        // And the canonical decoder must reject the PancakeSwap topic.
        assert!(crate::v3_swap_decoder::decode_v3_swap_log(&real_pancake_swap_log()).is_none());
    }

    #[test]
    fn truncated_data_returns_none() {
        let mut log = real_pancake_swap_log();
        log.inner = InnerLog::new_unchecked(
            log.inner.address,
            log.inner.data.topics().to_vec(),
            Bytes::from(vec![0u8; 96]),
        );
        assert!(decode_v3_pancakeswap_swap_log(&log).is_none());
    }

    // ── malformed narrow-field words ──────────────────────────────────────

    #[test]
    fn rejects_over_wide_sqrt_price() {
        // A `uint160` slot whose bits above 160 are set is not a price; the
        // junk bits would otherwise ride along in the shared `U256` field.
        let mut log = real_pancake_swap_log();
        let mut data = log.data().data.to_vec();
        data[64] = 0x01;
        log.inner =
            InnerLog::new_unchecked(log.address(), log.topics().to_vec(), Bytes::from(data));
        assert!(decode_v3_pancakeswap_swap_log(&log).is_none());
    }

    #[test]
    fn rejects_non_sign_extended_tick() {
        // A `-60` tick zero-extended instead of sign-extended: a tail-only read
        // would report -60 as if the word were canonical.
        let mut log = real_pancake_swap_log();
        let mut data = log.data().data.to_vec();
        let mut word = [0u8; 32];
        word[29..32].copy_from_slice(&(-60_i32).to_be_bytes()[1..]);
        data[128..160].copy_from_slice(&word);
        log.inner =
            InnerLog::new_unchecked(log.address(), log.topics().to_vec(), Bytes::from(data));
        assert!(decode_v3_pancakeswap_swap_log(&log).is_none());
    }
}
