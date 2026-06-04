//! Liquidity map verification — compares engine's in-memory tick data against on-chain state.
//!
//! The "liquidity map" is the set of initialized ticks (the bitmap) and their
//! `(liquidityGross, liquidityNet)` values. Mutable scalar state like
//! `sqrtPriceX96`, `tick`, and `liquidity` changes on every swap and is NOT
//! verified here — it would always be stale. The map is only updated by
//! Mint/Burn (V3) or `ModifyLiquidity` (V4) events, which is what we validate.
//!
//! - **V3**: `pool.tickBitmap(word)` discovers populated words,
//!   `pool.ticks(tickIndex)` verifies per-tick data.
//! - **V4**: `StateView.getTickBitmap(poolId, word)` discovers populated words,
//!   `StateView.getTickLiquidity(poolId, tick)` verifies per-tick data.
//!
//! On ANY mismatch, returns `Err` — the bot must not operate with stale tick data.

use std::collections::HashMap;

use alloy::dyn_abi::DynSolValue;
use alloy::primitives::{Address, Bytes, I256, B256, U256};

use crate::optimizers::v3_block_engine::V3PoolState;
use crate::optimizers::v4_block_engine::V4PoolState;
use crate::provider::AlloyProvider;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// A single verification mismatch.
#[derive(Debug)]
pub struct VerificationMismatch {
    /// Human-readable description of the mismatch.
    pub message: String,
}

impl std::fmt::Display for VerificationMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for VerificationMismatch {}

// ---------------------------------------------------------------------------
// ABI selectors
// ---------------------------------------------------------------------------

/// `ticks(int24)` — returns (uint128, int128, uint256, uint256, uint256, uint256, uint256, uint256)
const V3_TICKS_SELECTOR: [u8; 4] = [0xf3, 0x0d, 0xba, 0x93];

/// `tickBitmap(int16)` — returns uint256
const V3_TICK_BITMAP_SELECTOR: [u8; 4] = [0x53, 0x39, 0xc2, 0x96];

/// `getTickBitmap(bytes32,int16)` — returns uint256
const STATE_VIEW_GET_TICK_BITMAP_SELECTOR: [u8; 4] = [0x1c, 0x7c, 0xcb, 0x4c];

/// `getTickLiquidity(bytes32,int24)` — returns (uint128, int128)
const STATE_VIEW_GET_TICK_LIQUIDITY_SELECTOR: [u8; 4] = [0xca, 0xed, 0xab, 0x54];

// ---------------------------------------------------------------------------
// Tick math helpers (matching Uniswap V4 Solidity)
// ---------------------------------------------------------------------------

/// Compress a tick using floor-division toward negative infinity.
/// Matches Solidity `TickBitmap.compress(tick, tickSpacing)`.
fn compress_tick(tick: i64, tick_spacing: i64) -> i64 {
    let q = tick / tick_spacing;
    // If tick < 0 and tick % tick_spacing != 0, subtract 1 to floor
    if tick < 0 && tick % tick_spacing != 0 {
        q - 1
    } else {
        q
    }
}

/// Compute (wordPos, bitPos) for a compressed tick.
/// Matches Solidity `TickBitmap.position(compressedTick)`.
fn tick_bitmap_position(compressed_tick: i64) -> (i16, u8) {
    // wordPos = compressed_tick >> 8 (arithmetic shift right = floor by 256)
    let word_pos = (compressed_tick >> 8) as i16;
    // bitPos = compressed_tick & 0xFF (low 8 bits, treating as unsigned)
    let bit_pos = (compressed_tick & 0xFF) as u8;
    (word_pos, bit_pos)
}

// ---------------------------------------------------------------------------
// V3 verification
// ---------------------------------------------------------------------------

/// Verify all V3 pools' liquidity maps against on-chain state.
///
/// For each V3 pool, verifies that:
/// 1. The tick bitmap matches on-chain (same set of initialized ticks)
/// 2. Each tick's `liquidityGross` and `liquidityNet` match on-chain
///
/// Only the liquidity map is verified — mutable scalar state (`sqrtPriceX96`,
/// `tick`, `liquidity`) is NOT checked since it changes on every swap.
pub async fn verify_v3_pools(
    provider: &AlloyProvider,
    _tick_lens: Address, // Kept for API compatibility; not used
    pools: &HashMap<u64, V3PoolState>,
    block_number: Option<u64>,
) -> Result<(), VerificationMismatch> {
    for pool in pools.values() {
        verify_v3_pool(provider, pool, block_number).await?;
    }
    Ok(())
}

async fn verify_v3_pool(
    provider: &AlloyProvider,
    pool: &V3PoolState,
    block_number: Option<u64>,
) -> Result<(), VerificationMismatch> {
    let pool_addr = pool.address;
    let tick_spacing = pool.tick_spacing;
    let block_tag = match block_number {
        Some(b) => format!("block={b}"),
        None => "block=pending".to_string(),
    };

    // 1. Discover on-chain tick bitmap words
    // Scan words from our tick_data plus ±2 around the current tick
    let mut words_to_check: std::collections::HashSet<i16> = std::collections::HashSet::new();
    for &tick_idx in pool.tick_data.keys() {
        let compressed = compress_tick(i64::from(tick_idx), i64::from(tick_spacing));
        let (word, _) = tick_bitmap_position(compressed);
        words_to_check.insert(word);
    }
    let compressed_current = compress_tick(i64::from(pool.tick), i64::from(tick_spacing));
    let (current_word, _) = tick_bitmap_position(compressed_current);
    for w in (current_word - 2)..=(current_word + 2) {
        words_to_check.insert(w);
    }

    // Collect all on-chain tick indices from bitmap scanning
    let mut on_chain_tick_indices: std::collections::HashSet<i32> = std::collections::HashSet::new();

    for word in &words_to_check {
        // Encode: tickBitmap(int16)
        let calldata = encode_calldata(
            V3_TICK_BITMAP_SELECTOR,
            &[DynSolValue::Int(I256::unchecked_from(i64::from(*word) as i128), 16)],
        );

        let result = provider
            .eth_call(&pool_addr, calldata, block_number)
            .await
            .map_err(|e| VerificationMismatch {
                message: format!(
                    "V3 pool {pool_addr} {block_tag}: tickBitmap({word}) RPC call failed: {e}"
                ),
            })?;

        let bitmap_val = decode_uint256(&result[0..32]);
        if bitmap_val.is_zero() {
            continue;
        }

        for bit in 0..256u64 {
            if bitmap_val.bit(bit as usize) {
                let compressed_tick = i64::from(*word) * 256 + bit as i64;
                let tick = compressed_tick * i64::from(tick_spacing);
                on_chain_tick_indices.insert(tick as i32);
            }
        }
    }

    // 2. Verify each tick in our tick_data by calling pool.ticks() directly
    for (&tick_idx, our_info) in &pool.tick_data {
        let our_lg = our_info.liquidity_gross.to::<u128>();
        let our_ln: i128 = match our_info.liquidity_net.try_into() {
            Ok(v) => v,
            Err(_) => 0i128,
        };

        let (on_chain_lg, on_chain_ln) = call_v3_ticks(provider, pool_addr, tick_idx, block_number).await?;

        if our_lg != on_chain_lg {
            return Err(VerificationMismatch {
                message: format!(
                    "V3 pool {pool_addr} {block_tag}: tick {tick_idx} liquidityGross mismatch — engine: {our_lg}, on-chain: {on_chain_lg}"
                ),
            });
        }
        if our_ln != on_chain_ln {
            return Err(VerificationMismatch {
                message: format!(
                    "V3 pool {pool_addr} {block_tag}: tick {tick_idx} liquidityNet mismatch — engine: {our_ln}, on-chain: {on_chain_ln}"
                ),
            });
        }

        // Remove from on-chain set (we've verified this tick)
        on_chain_tick_indices.remove(&tick_idx);
    }

    // 3. Check for on-chain ticks we're missing
    if let Some(&tick_idx) = on_chain_tick_indices.iter().next() {
        let (on_chain_lg, on_chain_ln) = call_v3_ticks(provider, pool_addr, tick_idx, block_number).await?;
        return Err(VerificationMismatch {
            message: format!(
                "V3 pool {pool_addr} {block_tag}: tick {tick_idx} exists on-chain (lg={on_chain_lg}, ln={on_chain_ln}) but NOT in engine"
            ),
        });
    }

    Ok(())
}

/// Call `ticks(int24)` on a V3 pool and return `(liquidityGross, liquidityNet)`.
async fn call_v3_ticks(
    provider: &AlloyProvider,
    pool_addr: Address,
    tick: i32,
    block_number: Option<u64>,
) -> Result<(u128, i128), VerificationMismatch> {
    let block_tag = match block_number {
        Some(b) => format!("block={b}"),
        None => "block=pending".to_string(),
    };
    let calldata = encode_calldata(
        V3_TICKS_SELECTOR,
        &[DynSolValue::Int(I256::unchecked_from(i128::from(tick)), 24)],
    );

    let result = provider
        .eth_call(&pool_addr, calldata, block_number)
        .await
        .map_err(|e| VerificationMismatch {
            message: format!(
                "V3 pool {pool_addr} {block_tag}: ticks({tick}) RPC call failed: {e}"
            ),
        })?;

    // ticks() returns (uint128 liquidityGross, int128 liquidityNet, ...)
    // We only need the first two fields.
    if result.len() < 64 {
        return Err(VerificationMismatch {
            message: format!(
                "V3 pool {pool_addr} {block_tag}: ticks({tick}) returned {} bytes, expected at least 64",
                result.len()
            ),
        });
    }

    let lg = decode_uint128(&result[0..32]);
    let ln = decode_int128(&result[32..64]);
    Ok((lg, ln))
}

// ---------------------------------------------------------------------------
// V4 verification
// ---------------------------------------------------------------------------

/// Verify all V4 pools' liquidity maps against on-chain state via `StateView`.
///
/// For each V4 pool, verifies that:
/// 1. The tick bitmap matches on-chain (same set of initialized ticks)
/// 2. Each tick's `liquidityGross` and `liquidityNet` match on-chain
///
/// Only the liquidity map is verified — mutable scalar state is NOT checked.
pub async fn verify_v4_pools(
    provider: &AlloyProvider,
    state_view: Address,
    pools: &HashMap<u64, V4PoolState>,
    block_number: Option<u64>,
) -> Result<(), VerificationMismatch> {
    // Deduplicate by pool_id — both forward and reverse orientations share the same
    // on-chain state, so we only need to verify each pool_id once.
    let mut seen_pool_ids: HashMap<[u8; 32], &V4PoolState> = HashMap::new();
    for pool in pools.values() {
        seen_pool_ids.entry(pool.pool_id).or_insert(pool);
    }

    for (_pool_id, pool) in seen_pool_ids {
        verify_v4_pool(provider, state_view, pool, block_number).await?;
    }
    Ok(())
}

async fn verify_v4_pool(
    provider: &AlloyProvider,
    state_view: Address,
    pool: &V4PoolState,
    block_number: Option<u64>,
) -> Result<(), VerificationMismatch> {
    let pool_id_bytes = pool.pool_id;
    let tick_spacing = pool.pool_key.tick_spacing;
    let block_tag = match block_number {
        Some(b) => format!("block={b}"),
        None => "block=pending".to_string(),
    };
    let pool_id_hex: String = pool_id_bytes.iter().map(|b| format!("{b:02x}")).collect();

    // 1. Discover on-chain populated bitmap words
    let mut words_to_check: std::collections::HashSet<i16> = std::collections::HashSet::new();

    // Check words from our tick_data
    for &tick_idx in pool.tick_data.keys() {
        let compressed = compress_tick(i64::from(tick_idx), i64::from(tick_spacing));
        let (word, _) = tick_bitmap_position(compressed);
        words_to_check.insert(word);
    }

    // Also check a few words around the current tick
    let compressed_current = compress_tick(i64::from(pool.tick), i64::from(tick_spacing));
    let (current_word, _) = tick_bitmap_position(compressed_current);
    for w in (current_word - 2)..=(current_word + 2) {
        words_to_check.insert(w);
    }

    let mut on_chain_ticks: HashMap<i32, (u128, i128)> = HashMap::new();

    for word in &words_to_check {
        // Encode: getTickBitmap(bytes32, int16)
        let bitmap_calldata = encode_calldata(
            STATE_VIEW_GET_TICK_BITMAP_SELECTOR,
            &[
                DynSolValue::FixedBytes(B256::from(pool_id_bytes), 32),
                DynSolValue::Int(I256::unchecked_from(i64::from(*word) as i128), 16),
            ],
        );

        let bitmap_result = provider
            .eth_call(&state_view, bitmap_calldata, block_number)
            .await
            .map_err(|e| VerificationMismatch {
                message: format!(
                    "V4 pool 0x{pool_id_hex} {block_tag}: getTickBitmap({word}) RPC call failed: {e}"
                ),
            })?;

        let bitmap_val = decode_uint256(&bitmap_result[0..32]);
        if bitmap_val.is_zero() {
            continue;
        }

        // Enumerate set bits in the bitmap
        for bit in 0..256u64 {
            if bitmap_val.bit(bit as usize) {
                let compressed_tick = i64::from(*word) * 256 + bit as i64;
                let tick = compressed_tick * i64::from(tick_spacing);
                let tick_i32 = tick as i32;

                // Call getTickLiquidity for this tick
                let tick_liq_calldata = encode_calldata(
                    STATE_VIEW_GET_TICK_LIQUIDITY_SELECTOR,
                    &[
                        DynSolValue::FixedBytes(B256::from(pool_id_bytes), 32),
                        DynSolValue::Int(I256::unchecked_from(i128::from(tick_i32)), 24),
                    ],
                );

                let tick_liq_result = provider
                    .eth_call(&state_view, tick_liq_calldata, block_number)
                    .await
                    .map_err(|e| VerificationMismatch {
                        message: format!(
                            "V4 pool 0x{pool_id_hex} {block_tag}: getTickLiquidity({tick_i32}) RPC call failed: {e}"
                        ),
                    })?;

                // Decode: (uint128 liquidityGross, int128 liquidityNet)
                if tick_liq_result.len() < 64 {
                    return Err(VerificationMismatch {
                        message: format!(
                            "V4 pool 0x{pool_id_hex} {block_tag}: getTickLiquidity({tick_i32}) returned {} bytes, expected 64",
                            tick_liq_result.len()
                        ),
                    });
                }

                let lg = decode_uint128(&tick_liq_result[0..32]);
                let ln = decode_int128(&tick_liq_result[32..64]);
                on_chain_ticks.insert(tick_i32, (lg, ln));
            }
        }
    }

    // 2. Compare every tick in our tick_data against on-chain
    for (&tick_idx, our_info) in &pool.tick_data {
        let our_lg = our_info.liquidity_gross.to::<u128>();
        let our_ln: i128 = match our_info.liquidity_net.try_into() {
            Ok(v) => v,
            Err(_) => 0i128,
        };

        if let Some(&(on_chain_lg, on_chain_ln)) = on_chain_ticks.get(&tick_idx) {
            if our_lg != on_chain_lg {
                return Err(VerificationMismatch {
                    message: format!(
                        "V4 pool 0x{pool_id_hex} {block_tag}: tick {tick_idx} liquidityGross mismatch — engine: {our_lg}, on-chain: {on_chain_lg}"
                    ),
                });
            }
            if our_ln != on_chain_ln {
                return Err(VerificationMismatch {
                    message: format!(
                        "V4 pool 0x{pool_id_hex} {block_tag}: tick {tick_idx} liquidityNet mismatch — engine: {our_ln}, on-chain: {on_chain_ln}"
                    ),
                });
            }
        } else {
            return Err(VerificationMismatch {
                message: format!(
                    "V4 pool 0x{pool_id_hex} {block_tag}: tick {tick_idx} exists in engine (lg={our_lg}, ln={our_ln}) but NOT on-chain"
                ),
            });
        }
    }

    // 3. Check for on-chain ticks we're missing
    for (&tick_idx, &(on_chain_lg, on_chain_ln)) in &on_chain_ticks {
        if !pool.tick_data.contains_key(&tick_idx) {
            return Err(VerificationMismatch {
                message: format!(
                    "V4 pool 0x{pool_id_hex} {block_tag}: tick {tick_idx} exists on-chain (lg={on_chain_lg}, ln={on_chain_ln}) but NOT in engine"
                ),
            });
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// ABI encoding helper
// ---------------------------------------------------------------------------

/// Build calldata = selector + ABI-encoded params using Alloy's `DynSolValue`.
/// This ensures correct sign-extension for negative integers (int16, int24).
fn encode_calldata(selector: [u8; 4], params: &[DynSolValue]) -> Bytes {
    let mut out = Vec::with_capacity(4 + 32 * params.len());
    out.extend_from_slice(&selector);
    for param in params {
        out.extend_from_slice(&param.abi_encode());
    }
    Bytes::from(out)
}

// ---------------------------------------------------------------------------
// ABI decoding helpers
// ---------------------------------------------------------------------------

/// Decode a uint256 from a 32-byte ABI word.
fn decode_uint256(word: &[u8]) -> U256 {
    U256::from_be_bytes::<32>(word.try_into().unwrap_or([0u8; 32]))
}

/// Decode a uint128 from a 32-byte ABI word (upper 16 bytes ignored).
fn decode_uint128(word: &[u8]) -> u128 {
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&word[16..32]);
    u128::from_be_bytes(buf)
}

/// Decode an int128 from a 32-byte ABI word (upper 16 bytes are sign extension).
fn decode_int128(word: &[u8]) -> i128 {
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&word[16..32]);
    i128::from_be_bytes(buf)
}

// ---------------------------------------------------------------------------
// Tests — tick bitmap word/bit calculation
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- compress_tick tests ---
    // Solidity V4 TickBitmap.compress(tick, tickSpacing) floors toward -∞:
    //   compressed = tick / tickSpacing;
    //   if (tick < 0 && tick % tickSpacing != 0) compressed--;
    //
    // Python: tick // tickSpacing (Python // already floors)

    #[test]
    fn compress_positive_tick_exact_division() {
        // 100 / 10 = 10 (no remainder)
        assert_eq!(compress_tick(100, 10), 10);
    }

    #[test]
    fn compress_positive_tick_with_remainder() {
        // 105 / 10 = 10 (trunc) = 10 (floor) — same for positive
        assert_eq!(compress_tick(105, 10), 10);
    }

    #[test]
    fn compress_negative_tick_exact_division() {
        // -100 / 10 = -10 (exact, no floor correction needed)
        assert_eq!(compress_tick(-100, 10), -10);
    }

    #[test]
    fn compress_negative_tick_with_remainder() {
        // -105 / 10: trunc(-10.5) = -10, but floor = -11
        // This is the critical case that was previously wrong in the verifier.
        assert_eq!(compress_tick(-105, 10), -11);
    }

    #[test]
    fn compress_tick_spacing_one() {
        // With tick_spacing=1, compress is identity
        assert_eq!(compress_tick(0, 1), 0);
        assert_eq!(compress_tick(100, 1), 100);
        assert_eq!(compress_tick(-100, 1), -100);
        assert_eq!(compress_tick(-292420, 1), -292420);
    }

    #[test]
    fn compress_tick_spacing_10() {
        // Real-world V4 pool tick_spacing
        assert_eq!(compress_tick(0, 10), 0);
        assert_eq!(compress_tick(10, 10), 1);
        assert_eq!(compress_tick(-10, 10), -1);
        assert_eq!(compress_tick(-20, 10), -2);
        assert_eq!(compress_tick(-11, 10), -2); // floor(-1.1) = -2
        assert_eq!(compress_tick(-292420, 10), -29242); // The bug case
    }

    #[test]
    fn compress_tick_spacing_60() {
        // V3 mainnet tick_spacing
        assert_eq!(compress_tick(0, 60), 0);
        assert_eq!(compress_tick(60, 60), 1);
        assert_eq!(compress_tick(-60, 60), -1);
        assert_eq!(compress_tick(-120, 60), -2);
        assert_eq!(compress_tick(-61, 60), -2); // floor(-1.0166..) = -2
    }

    #[test]
    fn compress_tick_spacing_200() {
        assert_eq!(compress_tick(0, 200), 0);
        assert_eq!(compress_tick(200, 200), 1);
        assert_eq!(compress_tick(-200, 200), -1);
        assert_eq!(compress_tick(-201, 200), -2); // floor(-1.005) = -2
    }

    #[test]
    fn compress_tick_max_values() {
        // int24 max = 887272
        assert_eq!(compress_tick(887272, 10), 88727);
        assert_eq!(compress_tick(-887272, 10), -88728); // floor(-88727.2) = -88728
        // int24 min = -887272
        assert_eq!(compress_tick(-887272, 1), -887272);
    }

    // --- tick_bitmap_position tests ---
    // Solidity V4 TickBitmap.position(compressedTick):
    //   wordPos := sar(8, signextend(2, tick))  // arithmetic shift right 8
    //   bitPos := and(tick, 0xff)                // low 8 bits
    //
    // Python: (tick >> 8, tick % 256)

    #[test]
    fn position_zero() {
        assert_eq!(tick_bitmap_position(0), (0, 0));
    }

    #[test]
    fn position_positive() {
        // compressed = 29242 => word = 114, bit = 58
        assert_eq!(tick_bitmap_position(29242), (114, 58));
    }

    #[test]
    fn position_negative() {
        // compressed = -29242
        // -29242 >> 8 = -115 (arithmetic shift right = floor division by 256)
        // -29242 & 0xFF = 0xFFFF8ECE & 0xFF = 0xCE = 206... wait let me compute
        // -29242 in binary: two's complement
        // Python: (-29242) >> 8 = -115, (-29242) % 256 = 198
        assert_eq!(tick_bitmap_position(-29242), (-115, 198));
    }

    #[test]
    fn position_negative_word_boundary() {
        // compressed = -256 => word = -1, bit = 0
        assert_eq!(tick_bitmap_position(-256), (-1, 0));
        // compressed = -257 => word = -2, bit = 255
        // Python: (-257) >> 8 = -2, (-257) % 256 = 255
        assert_eq!(tick_bitmap_position(-257), (-2, 255));
    }

    #[test]
    fn position_positive_word_boundary() {
        // compressed = 256 => word = 1, bit = 0
        assert_eq!(tick_bitmap_position(256), (1, 0));
        // compressed = 255 => word = 0, bit = 255
        assert_eq!(tick_bitmap_position(255), (0, 255));
    }

    // --- Round-trip tests: compress then position must recover the tick ---
    // The Solidity flipTick does:
    //   compressed = compress(tick, tickSpacing)
    //   (wordPos, bitPos) = position(compressed)
    //   wordPos stores the bitmap word, bitPos is the mask offset

    #[test]
    fn round_trip_positive_tick() {
        let tick: i64 = 292420;
        let tick_spacing: i64 = 10;
        let compressed = compress_tick(tick, tick_spacing);
        let (word, bit) = tick_bitmap_position(compressed);
        // Reverse: compressed_tick = word * 256 + bit, tick = compressed_tick * tick_spacing
        let recovered_tick = (i64::from(word) * 256 + i64::from(bit)) * tick_spacing;
        assert_eq!(recovered_tick, tick);
    }

    #[test]
    fn round_trip_negative_tick() {
        let tick: i64 = -292420;
        let tick_spacing: i64 = 10;
        let compressed = compress_tick(tick, tick_spacing);
        let (word, bit) = tick_bitmap_position(compressed);
        let recovered_tick = (i64::from(word) * 256 + i64::from(bit)) * tick_spacing;
        assert_eq!(recovered_tick, tick);
    }

    #[test]
    fn round_trip_negative_tick_spacing_60() {
        let tick: i64 = -120;
        let tick_spacing: i64 = 60;
        let compressed = compress_tick(tick, tick_spacing);
        let (word, bit) = tick_bitmap_position(compressed);
        let recovered_tick = (i64::from(word) * 256 + i64::from(bit)) * tick_spacing;
        assert_eq!(recovered_tick, tick);
    }

    #[test]
    fn round_trip_negative_non_aligned_tick() {
        // tick=-61, spacing=60: not a multiple, but compress still works
        // compressed = floor(-61/60) = -2
        // position(-2): word = -1 >> 8... wait, -2 >> 8 in arithmetic = -1
        // Python: (-2) >> 8 = -1, (-2) % 256 = 254
        let tick: i64 = -61;
        let tick_spacing: i64 = 60;
        let compressed = compress_tick(tick, tick_spacing);
        assert_eq!(compressed, -2);
        let (word, bit) = tick_bitmap_position(compressed);
        assert_eq!((word, bit), (-1, 254));
        // Reverse: compressed_tick = -1 * 256 + 254 = -2
        let recovered_compressed = i64::from(word) * 256 + i64::from(bit);
        assert_eq!(recovered_compressed, compressed);
        // tick = -2 * 60 = -120 (the nearest aligned tick, not -61)
        let recovered_tick = recovered_compressed * tick_spacing;
        assert_eq!(recovered_tick, -120);
    }

    // --- Exhaustive test: all negative tick/spacing combos in int24 range ---
    #[test]
    fn compress_matches_python_floor_division() {
        // Test every tick_spacing and a range of ticks that produced the original bug
        for &tick_spacing in &[1i64, 10, 60, 200] {
            for tick in (-500i64..=500).chain([-292420i64, 887272, -887272]) {
                let rust_result = compress_tick(tick, tick_spacing);
                // Python floor division
                let python_result = if tick_spacing != 0 {
                    tick.div_euclid(tick_spacing)
                } else {
                    panic!("tick_spacing=0");
                };
                assert_eq!(
                    rust_result, python_result,
                    "compress_tick({tick}, {tick_spacing}): Rust={rust_result}, Python floor={python_result}"
                );
            }
        }
    }

    #[test]
    fn position_matches_python() {
        // Test position() against Python's (tick >> 8, tick % 256)
        for tick in (-1000i64..=1000).chain([-29242i64, 29242, -887272, 887272]) {
            let (rust_word, rust_bit) = tick_bitmap_position(tick);
            // Python semantics
            let py_word = tick >> 8; // arithmetic shift right
            let py_bit = tick.rem_euclid(256); // Python's % always non-negative for positive divisor
            assert_eq!(
                rust_word, py_word as i16,
                "position({tick}): Rust word={rust_word}, Python word={py_word}"
            );
            assert_eq!(
                rust_bit, py_bit as u8,
                "position({tick}): Rust bit={rust_bit}, Python bit={py_bit}"
            );
        }
    }
}
