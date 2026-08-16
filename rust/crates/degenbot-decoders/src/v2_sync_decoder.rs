//! Uniswap V2 Sync event decoder.
//!
//! Decodes `Sync(uint112, uint112)` events from Uniswap V2 pair contracts.
//! The Sync event is emitted whenever reserves change (swap, mint, burn)
//! and carries **absolute** reserve values — making it self-correcting
//! after missed events
//!
//! # Sync event format
//!
//! ```text
//! event Sync(uint112 reserve0, uint112 reserve1)
//!
//! topic[0] = 0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1
//! data    = abi.encode(uint112, uint112) = 64 bytes (each left-padded to 32 bytes)
//! ```

use alloy::primitives::{aliases::U112, b256, Address, B256, U256};
use alloy::rpc::types::Log;

/// Keccak256 of `Sync(uint112,uint112)`.
pub const V2_SYNC_TOPIC: B256 =
    b256!("0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1");

/// Decoded Sync event carrying absolute reserves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncEvent {
    /// The pair contract that emitted the event.
    pub pool_address: Address,
    /// Reserve of token0 (on-chain `uint112` — typed `U112` post-ZPHT6X;
    /// the decoder validates the high 144 bits of the ABI word are zero
    /// before narrowing).
    pub reserve0: U112,
    /// Reserve of token1 (on-chain `uint112`).
    pub reserve1: U112,
}

/// Decode a V2 Sync event from a log.
///
/// Returns `Some(SyncEvent)` if the log is a valid Sync event with
/// correctly formatted data. Returns `None` if:
/// - The topic doesn't match `V2_SYNC_TOPIC`
/// - The data is too short (< 64 bytes)
///
/// Extra data beyond 64 bytes is ignored (some V2 forks may pad differently).
#[must_use]
pub fn decode_sync_log(log: &Log) -> Option<SyncEvent> {
    // topic[0] must match V2_SYNC_TOPIC
    let topic0 = log.topics().first()?;
    if *topic0 != V2_SYNC_TOPIC {
        return None;
    }

    // data = abi.encode(uint112, uint112) = 32 bytes + 32 bytes = 64 bytes minimum
    let data = log.data().data.as_ref();
    if data.len() < 64 {
        return None;
    }

    // Decode reserve0 (bytes 0..32, left-padded uint112). The ABI word is
    // 32 bytes, but a real uint112 is left-padded with zeros — any non-zero
    // bits above position 112 indicate a corrupt / synthetic log → reject.
    let r0 = U256::from_be_bytes::<32>(data[..32].try_into().ok()?);
    let reserve0 = narrow_sync_reserve(r0)?;

    // Decode reserve1 (bytes 32..64, left-padded uint112).
    let r1 = U256::from_be_bytes::<32>(data[32..64].try_into().ok()?);
    let reserve1 = narrow_sync_reserve(r1)?;

    Some(SyncEvent {
        pool_address: log.address(),
        reserve0,
        reserve1,
    })
}

/// Narrow a 32-byte ABI word to `U112`, returning `None` if bits above
/// position 112 are set (i.e. the value exceeds `uint112::MAX`). A real
/// `Sync(uint112,uint112)` event's ABI slot is left-zero-padded; stray high
/// bits indicate a corrupt / synthetic log, treated as malformed (the existing
/// `None` paths for short data / topic mismatch).
fn narrow_sync_reserve(word: U256) -> Option<U112> {
    if word > U112::MAX.to::<U256>() {
        return None;
    }
    Some(word.to::<U112>())
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use alloy::primitives::Bytes;
    fn make_sync_log(pool_address: Address, reserve0: U256, reserve1: U256) -> Log {
        let data = {
            // ABI encode uint112, uint112 (each left-padded to 32 bytes)
            let r0_bytes = reserve0.to_be_bytes::<32>();
            let r1_bytes = reserve1.to_be_bytes::<32>();
            let mut data = Vec::with_capacity(64);
            data.extend_from_slice(&r0_bytes);
            data.extend_from_slice(&r1_bytes);
            data
        };

        let inner = alloy::primitives::Log::new_unchecked(
            pool_address,
            vec![V2_SYNC_TOPIC],
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

    fn make_wrong_topic_log(pool_address: Address) -> Log {
        let inner = alloy::primitives::Log::new_unchecked(
            pool_address,
            vec![B256::ZERO], // wrong topic
            Bytes::from(vec![0u8; 64]),
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
    fn decode_valid_sync() {
        let pool = Address::ZERO;
        let weth_800 = U256::from(800u64) * U256::from(10u64).pow(U256::from(18));
        let log = make_sync_log(pool, U256::from(1_500_000_000_000u64), weth_800);

        let result = decode_sync_log(&log);
        assert!(result.is_some());

        let event = result.unwrap();
        assert_eq!(event.pool_address, pool);
        assert_eq!(event.reserve0, U112::from(1_500_000_000_000u64));
        let weth_800 = U112::from(800u64) * U112::from(10u64).pow(U112::from(18));
        assert_eq!(event.reserve1, weth_800);
    }

    #[test]
    fn decode_sync_wrong_topic_returns_none() {
        let pool = Address::ZERO;
        let log = make_wrong_topic_log(pool);

        assert!(decode_sync_log(&log).is_none());
    }

    #[test]
    fn decode_sync_truncated_data_returns_none() {
        let pool = Address::ZERO;
        let inner = alloy::primitives::Log::new_unchecked(
            pool,
            vec![V2_SYNC_TOPIC],
            Bytes::from(vec![0u8; 32]), // only 32 bytes, need 64
        );
        let log = Log {
            inner,
            block_hash: None,
            block_number: None,
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        };

        assert!(decode_sync_log(&log).is_none());
    }

    #[test]
    fn decode_sync_zero_reserves() {
        // Valid — can happen on newly created pairs
        let pool = Address::ZERO;
        let log = make_sync_log(pool, U256::ZERO, U256::ZERO);

        let result = decode_sync_log(&log);
        assert!(result.is_some());

        let event = result.unwrap();
        assert_eq!(event.reserve0, U112::ZERO);
        assert_eq!(event.reserve1, U112::ZERO);
    }

    #[test]
    fn decode_sync_accepts_uint112_max() {
        // The highest value a real Sync(uint112,uint112) event can carry.
        let pool = Address::ZERO;
        let max_u256 = U112::MAX.to::<U256>();
        let log = make_sync_log(pool, max_u256, max_u256);

        let event = decode_sync_log(&log).expect("uint112::MAX must decode cleanly");
        assert_eq!(event.reserve0, U112::MAX);
        assert_eq!(event.reserve1, U112::MAX);
    }

    #[test]
    fn decode_sync_rejects_high_bits_set() {
        // A value > uint112::MAX in the ABI word indicates a corrupt /
        // synthetic log — the decoder must reject it (return None), not
        // silently truncate.
        let pool = Address::ZERO;
        let over = U112::MAX.to::<U256>() + U256::from(1u64);
        let log = make_sync_log(pool, over, U256::ZERO);

        assert!(decode_sync_log(&log).is_none());
    }

    #[test]
    fn decode_sync_no_topic_returns_none() {
        let pool = Address::ZERO;
        let inner = alloy::primitives::Log::new_unchecked(
            pool,
            vec![], // no topics
            Bytes::from(vec![0u8; 64]),
        );
        let log = Log {
            inner,
            block_hash: None,
            block_number: None,
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        };

        assert!(decode_sync_log(&log).is_none());
    }

    #[test]
    fn decode_sync_extra_data_decodes_first_two() {
        // Extra data beyond 64 bytes is ignored
        let pool = Address::ZERO;
        let mut data = vec![0u8; 64];
        // Set reserve0 in first 32 bytes (last 8 bytes of the first slot)
        let r0 = U256::from(42u64).to_be_bytes::<32>();
        data[..32].copy_from_slice(&r0);
        let r1 = U256::from(99u64).to_be_bytes::<32>();
        data[32..64].copy_from_slice(&r1);
        data.extend_from_slice(&[0xff; 32]); // extra data

        let inner =
            alloy::primitives::Log::new_unchecked(pool, vec![V2_SYNC_TOPIC], Bytes::from(data));
        let log = Log {
            inner,
            block_hash: None,
            block_number: None,
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        };

        let result = decode_sync_log(&log);
        assert!(result.is_some());

        let event = result.unwrap();
        assert_eq!(event.reserve0, U112::from(42u64));
        assert_eq!(event.reserve1, U112::from(99u64));
    }
}
