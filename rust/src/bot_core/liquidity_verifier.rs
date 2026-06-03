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

use alloy::primitives::{Address, Bytes};

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

    // 1. Discover on-chain tick bitmap words
    // Scan words from our tick_data plus ±2 around the current tick
    let mut words_to_check: std::collections::HashSet<i16> = std::collections::HashSet::new();
    for &tick_idx in pool.tick_data.keys() {
        let word = (i64::from(tick_idx) / (i64::from(tick_spacing) * 256)) as i16;
        words_to_check.insert(word);
    }
    let current_word = (i64::from(pool.tick) / (i64::from(tick_spacing) * 256)) as i16;
    for w in (current_word - 2)..=(current_word + 2) {
        words_to_check.insert(w);
    }

    // Collect all on-chain tick indices from bitmap scanning
    let mut on_chain_tick_indices: std::collections::HashSet<i32> = std::collections::HashSet::new();

    for word in &words_to_check {
        // Encode: tickBitmap(int16)
        let mut calldata = Vec::with_capacity(36);
        calldata.extend_from_slice(&V3_TICK_BITMAP_SELECTOR);
        let mut word_bytes = [0u8; 32];
        let word_i256 = i128::from(i64::from(*word));
        word_bytes[30..32].copy_from_slice(&(word_i256 as i16).to_be_bytes());
        calldata.extend_from_slice(&word_bytes);

        let result = provider
            .eth_call(&pool_addr, Bytes::from(calldata), block_number)
            .await
            .map_err(|e| VerificationMismatch {
                message: format!(
                    "V3 pool {pool_addr}: tickBitmap({word}) RPC call failed: {e}"
                ),
            })?;

        let bitmap_val = decode_uint256(&result[0..32]);
        if bitmap_val.is_zero() {
            continue;
        }

        for bit in 0..256u64 {
            if bitmap_val.bit(bit as usize) {
                let tick = (i64::from(*word) * 256 + (bit as i64)) * i64::from(tick_spacing);
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
                    "V3 pool {pool_addr}: tick {tick_idx} liquidityGross mismatch — engine: {our_lg}, on-chain: {on_chain_lg}"
                ),
            });
        }
        if our_ln != on_chain_ln {
            return Err(VerificationMismatch {
                message: format!(
                    "V3 pool {pool_addr}: tick {tick_idx} liquidityNet mismatch — engine: {our_ln}, on-chain: {on_chain_ln}"
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
                "V3 pool {pool_addr}: tick {tick_idx} exists on-chain (lg={on_chain_lg}, ln={on_chain_ln}) but NOT in engine"
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
    let mut calldata = Vec::with_capacity(36);
    calldata.extend_from_slice(&V3_TICKS_SELECTOR);
    // Encode int24 as sign-extended 32-byte word
    let tick_i256 = i128::from(tick);
    let mut tick_bytes = [0u8; 32];
    tick_bytes[16..32].copy_from_slice(&tick_i256.to_be_bytes());
    calldata.extend_from_slice(&tick_bytes);

    let result = provider
        .eth_call(&pool_addr, Bytes::from(calldata), block_number)
        .await
        .map_err(|e| VerificationMismatch {
            message: format!(
                "V3 pool {pool_addr}: ticks({tick}) RPC call failed: {e}"
            ),
        })?;

    // ticks() returns (uint128 liquidityGross, int128 liquidityNet, ...)
    // We only need the first two fields.
    if result.len() < 64 {
        return Err(VerificationMismatch {
            message: format!(
                "V3 pool {pool_addr}: ticks({tick}) returned {} bytes, expected at least 64",
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

    // 1. Discover on-chain populated bitmap words
    let mut words_to_check: std::collections::HashSet<i16> = std::collections::HashSet::new();

    // Check words from our tick_data
    for &tick_idx in pool.tick_data.keys() {
        let word = (i64::from(tick_idx) / (i64::from(tick_spacing) * 256)) as i16;
        words_to_check.insert(word);
    }

    // Also check a few words around the current tick
    let current_word = (i64::from(pool.tick) / (i64::from(tick_spacing) * 256)) as i16;
    for w in (current_word - 2)..=(current_word + 2) {
        words_to_check.insert(w);
    }

    let mut on_chain_ticks: HashMap<i32, (u128, i128)> = HashMap::new();

    for word in &words_to_check {
        // Encode: getTickBitmap(bytes32, int16)
        let mut bitmap_calldata = Vec::with_capacity(68);
        bitmap_calldata.extend_from_slice(&STATE_VIEW_GET_TICK_BITMAP_SELECTOR);
        bitmap_calldata.extend_from_slice(&pool_id_bytes);
        // int16 (left-padded to 32 bytes)
        let mut word_bytes = [0u8; 32];
        let word_i256 = i128::from(i64::from(*word));
        word_bytes[30..32].copy_from_slice(&(word_i256 as i16).to_be_bytes());
        bitmap_calldata.extend_from_slice(&word_bytes);

        let bitmap_result = provider
            .eth_call(&state_view, Bytes::from(bitmap_calldata), block_number)
            .await
            .map_err(|e| VerificationMismatch {
                message: format!(
                    "V4 pool {pool_id_bytes:?}: getTickBitmap({word}) RPC call failed: {e}"
                ),
            })?;

        let bitmap_val = decode_uint256(&bitmap_result[0..32]);
        if bitmap_val.is_zero() {
            continue;
        }

        // Enumerate set bits in the bitmap
        for bit in 0..256u64 {
            if bitmap_val.bit(bit as usize) {
                let tick = (i64::from(*word) * 256 + (bit as i64)) * i64::from(tick_spacing);
                let tick_i32 = tick as i32;

                // Call getTickLiquidity for this tick
                let mut tick_liq_calldata = Vec::with_capacity(68);
                tick_liq_calldata.extend_from_slice(&STATE_VIEW_GET_TICK_LIQUIDITY_SELECTOR);
                tick_liq_calldata.extend_from_slice(&pool_id_bytes);
                // Encode int24 as sign-extended 32-byte word
                let tick_i256 = i128::from(tick_i32);
                let mut tick_bytes = [0u8; 32];
                tick_bytes[16..32].copy_from_slice(&tick_i256.to_be_bytes());
                tick_liq_calldata.extend_from_slice(&tick_bytes);

                let tick_liq_result = provider
                    .eth_call(&state_view, Bytes::from(tick_liq_calldata), block_number)
                    .await
                    .map_err(|e| VerificationMismatch {
                        message: format!(
                            "V4 pool {pool_id_bytes:?}: getTickLiquidity({tick_i32}) RPC call failed: {e}"
                        ),
                    })?;

                // Decode: (uint128 liquidityGross, int128 liquidityNet)
                if tick_liq_result.len() < 64 {
                    return Err(VerificationMismatch {
                        message: format!(
                            "V4 pool {:?}: getTickLiquidity({tick_i32}) returned {} bytes, expected 64",
                            pool_id_bytes,
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
                        "V4 pool {pool_id_bytes:?}: tick {tick_idx} liquidityGross mismatch — engine: {our_lg}, on-chain: {on_chain_lg}"
                    ),
                });
            }
            if our_ln != on_chain_ln {
                return Err(VerificationMismatch {
                    message: format!(
                        "V4 pool {pool_id_bytes:?}: tick {tick_idx} liquidityNet mismatch — engine: {our_ln}, on-chain: {on_chain_ln}"
                    ),
                });
            }
        } else {
            return Err(VerificationMismatch {
                message: format!(
                    "V4 pool {pool_id_bytes:?}: tick {tick_idx} exists in engine (lg={our_lg}, ln={our_ln}) but NOT on-chain"
                ),
            });
        }
    }

    // 3. Check for on-chain ticks we're missing
    for (&tick_idx, &(on_chain_lg, on_chain_ln)) in &on_chain_ticks {
        if !pool.tick_data.contains_key(&tick_idx) {
            return Err(VerificationMismatch {
                message: format!(
                    "V4 pool {pool_id_bytes:?}: tick {tick_idx} exists on-chain (lg={on_chain_lg}, ln={on_chain_ln}) but NOT in engine"
                ),
            });
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// ABI decoding helpers
// ---------------------------------------------------------------------------

/// Decode a uint128 from a 32-byte ABI word.
fn decode_uint128(word: &[u8]) -> u128 {
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&word[16..32]);
    u128::from_be_bytes(buf)
}

/// Decode a uint256 from a 32-byte ABI word.
fn decode_uint256(word: &[u8]) -> alloy::primitives::U256 {
    alloy::primitives::U256::from_be_bytes::<32>(
        word.try_into().unwrap_or([0u8; 32]),
    )
}

/// Decode an int128 from a 32-byte ABI word.
fn decode_int128(word: &[u8]) -> i128 {
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&word[16..32]);
    i128::from_be_bytes(buf)
}
