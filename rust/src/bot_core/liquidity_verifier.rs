//! Liquidity map verification — compares engine's in-memory tick data against on-chain state.
//!
//! The "liquidity map" is the set of initialized ticks (the bitmap) and their
//! `(liquidityGross, liquidityNet)` values. Mutable scalar state like
//! `sqrtPriceX96`, `tick`, and `liquidity` changes on every swap and is NOT
//! verified here — it would always be stale. The map is only updated by
//! Mint/Burn (V3) or `ModifyLiquidity` (V4) events, which is what we validate.
//!
//! This is the {Slot0 Head / Tick Bookkeeping Map} split (see `rust/CONTEXT.md`
//! and ADR-004): the live variants (`verify_v3_pool` / `verify_v4_pool`) take
//! `&V3PoolState` / `&V4PoolState` and recover the "don't read slot0" rule
//! from this module doc; ADR-004 introduces a `TickMap` trait that narrows
//! these to take `&impl TickMap`, carrying the rule in the type system. The
//! snapshot-block variants (`verify_v3_liquidity_map` / `verify_v4_liquidity_map`)
//! already take a typed `&HashMap<i32, TickInfo>` + `Address` + block — the
//! typed-boundary precedent ADR-004 generalizes.
//!
//! - **V3**: `pool.tickBitmap(word)` discovers populated words,
//!   `pool.ticks(tickIndex)` verifies per-tick data.
//! - **V4**: `StateView.getTickBitmap(poolId, word)` discovers populated words,
//!   `StateView.getTickLiquidity(poolId, tick)` verifies per-tick data.
//!
//! On ANY mismatch, returns `Err` — the bot must not operate with stale tick data.

use std::collections::HashMap;

use alloy::dyn_abi::DynSolValue;
use alloy::primitives::{Address, Bytes, B256, I256, U256};

use crate::bot_core::{TickMap, V3PoolState, V4PoolState};
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

/// Outcome of a liquidity-map verification call (VP42BP).
///
/// Distinguishes a genuine on-chain **mismatch** (fatal — the engine's
/// in-memory tick data disagrees with the chain; the bot must not operate
/// with stale data) from a per-call **RPC transport failure** (transient —
/// the `eth_call` / `getTickBitmap` / `getTickLiquidity` read could not
/// reach the node or decode; a retry/backoff candidate, NOT evidence of a
/// mismatch).
///
/// Pre-VP42BP every verification function returned `Result<_,
/// VerificationMismatch>`, folding RPC transport failures into a
/// `VerificationMismatch` whose message said "... RPC call failed: {e}". The
/// `EngineVerifyRpc` seam then mapped all `VerificationMismatch` →
/// `VerifyError::Snapshot` → `VerificationMismatchError`, conflating a
/// transient transport error (potentially retryable) with a genuine mismatch
/// (always fatal). This enum makes the two categories distinguishable so the
/// seam can route transport failures to `VerificationRpcError` (retryable)
/// and genuine mismatches to `VerificationMismatchError` (fatal).
#[derive(Debug)]
pub enum LiquidityVerifyError {
    /// Genuine on-chain tick-data mismatch (bitmap divergence, `liquidityGross`
    /// / `liquidityNet` disagreement). Fatal.
    Mismatch(VerificationMismatch),
    /// Per-call RPC transport failure (the `eth_call` couldn't reach the node
    /// or its response was undecodable). Transient. The `message` carries the
    /// pool label, block tag, call site (e.g. `tickBitmap(word)`), and the
    /// underlying transport error.
    Rpc { message: String },
}

impl std::fmt::Display for LiquidityVerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mismatch(m) => write!(f, "{m}"),
            Self::Rpc { message } => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for LiquidityVerifyError {}

impl From<VerificationMismatch> for LiquidityVerifyError {
    fn from(m: VerificationMismatch) -> Self {
        Self::Mismatch(m)
    }
}

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
const fn compress_tick(tick: i32, tick_spacing: i32) -> i32 {
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
///
/// In Uniswap V3, `compressedTick` is an `int24` (24-bit signed), so the
/// range is [-887272, 887272]. After `>> 8`, `wordPos` fits in `i16`
/// and `bitPos` fits in `u8`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
const fn tick_bitmap_position(compressed_tick: i32) -> (i16, u8) {
    // wordPos = compressed_tick >> 8 (arithmetic shift right = floor by 256)
    let word_pos = (compressed_tick >> 8) as i16;
    // bitPos = compressed_tick & 0xFF (low 8 bits, always 0..=255)
    let bit_pos = (compressed_tick & 0xFF) as u8;
    (word_pos, bit_pos)
}

// ---------------------------------------------------------------------------
// V3 verification
// ---------------------------------------------------------------------------

/// Verify a V3 pool's raw tick data (before buffer) against on-chain state.
///
/// This is used for the snapshot-block verification: compare the DB-derived
/// `tick_data` against on-chain at the snapshot block, before any buffer events
/// are applied. Catches snapshot loading/serialization bugs.
/// # Errors
///
/// Returns `Err(VerificationMismatch)` if any tick's `liquidityGross` or
/// `liquidityNet` differs from on-chain.
pub async fn verify_v3_liquidity_map<S: std::hash::BuildHasher>(
    provider: &AlloyProvider,
    pool_address: Address,
    tick_data: &HashMap<i32, crate::bot_core::TickInfo, S>,
    block_number: u64,
) -> Result<(), LiquidityVerifyError> {
    for (&tick_idx, our_info) in tick_data {
        let our_gross = our_info.liquidity_gross.to::<u128>();
        let our_net: i128 = our_info.liquidity_net.try_into().unwrap_or_default();

        let (on_chain_gross, on_chain_net) =
            call_v3_ticks(provider, pool_address, tick_idx, Some(block_number)).await?;

        if our_gross != on_chain_gross || our_net != on_chain_net {
            return Err(LiquidityVerifyError::Mismatch(VerificationMismatch {
                message: format!(
                    "V3 pool {pool_address} at snapshot block {block_number}: tick {tick_idx} mismatch — snapshot: (lg={our_gross}, ln={our_net}), on-chain: (lg={on_chain_gross}, ln={on_chain_net})"
                ),
            }));
        }
    }
    Ok(())
}

/// Verify a V4 pool's raw tick data (before buffer) against on-chain state.
///
/// This is used for the snapshot-block verification: compare the DB-derived
/// `tick_data` against on-chain at the snapshot block, before any buffer events
/// are applied. Catches snapshot loading/serialization bugs.
/// # Errors
///
/// Returns `Err(VerificationMismatch)` if any tick's `liquidityGross` or
/// `liquidityNet` differs from on-chain.
pub async fn verify_v4_liquidity_map<S: std::hash::BuildHasher>(
    provider: &AlloyProvider,
    state_view: Address,
    pool_id: [u8; 32],
    tick_data: &HashMap<i32, crate::bot_core::TickInfo, S>,
    block_number: u64,
) -> Result<(), LiquidityVerifyError> {
    for (&tick_idx, our_info) in tick_data {
        let our_gross = our_info.liquidity_gross.to::<u128>();
        let our_net: i128 = our_info.liquidity_net.try_into().unwrap_or_default();

        let (on_chain_gross, on_chain_net) = call_state_view_tick_liquidity(
            provider,
            state_view,
            pool_id,
            tick_idx,
            Some(block_number),
        )
        .await?;

        if our_gross != on_chain_gross || our_net != on_chain_net {
            let pool_id_hex = crate::hex_utils::encode_hex(&pool_id);
            return Err(LiquidityVerifyError::Mismatch(VerificationMismatch {
                message: format!(
                    "V4 pool {pool_id_hex} at snapshot block {block_number}: tick {tick_idx} mismatch — snapshot: (lg={our_gross}, ln={our_net}), on-chain: (lg={on_chain_gross}, ln={on_chain_net})"
                ),
            }));
        }
    }
    Ok(())
}

/// Verify all V3 pools' liquidity maps against on-chain state.
///
/// For each V3 pool, verifies that:
/// 1. The tick bitmap matches on-chain (same set of initialized ticks)
/// 2. Each tick's `liquidityGross` and `liquidityNet` match on-chain
///
/// Only the liquidity map is verified — mutable scalar state (`sqrtPriceX96`,
/// `tick`, `liquidity`) is NOT checked since it changes on every swap.
/// # Errors
///
/// Returns `Err(VerificationMismatch)` if any pool's tick bitmap or tick data
/// differs from on-chain state.
pub async fn verify_v3_pools<S: std::hash::BuildHasher>(
    provider: &AlloyProvider,
    _tick_lens: Address, // Kept for API compatibility; not used
    pools: &HashMap<u64, V3PoolState, S>,
    block_number: Option<u64>,
) -> Result<(), LiquidityVerifyError> {
    for pool in pools.values() {
        verify_v3_pool(provider, pool, block_number).await?;
    }
    Ok(())
}

async fn verify_v3_pool<T: TickMap + ?Sized>(
    provider: &AlloyProvider,
    pool: &T,
    block_number: Option<u64>,
) -> Result<(), LiquidityVerifyError> {
    // Read the tick bookkeeping map + immutable identification through the ADR-004
    // `TickMap` trait — the slot0 head scalars (`sqrt_price_x96`, `liquidity`)
    // are deliberately out of reach here. `active_tick()` is read-only (only
    // seeds the ±2 bitmap-word scan around the current tick; NOT verified —
    // would always be stale by the time the RPC round-trips). See ADR-004.
    let pool_addr = pool.address();
    let tick_spacing = pool.tick_spacing();
    let active_tick = pool.active_tick();
    let tick_data = pool.tick_data();
    let block_tag = match block_number {
        Some(b) => format!("block={b}"),
        None => "block=pending".to_string(),
    };

    // 1. Discover on-chain tick bitmap words
    // Scan words from our tick_data plus ±2 around the current tick
    let mut words_to_check: std::collections::HashSet<i16> = std::collections::HashSet::new();
    for &tick_idx in tick_data.keys() {
        let compressed = compress_tick(tick_idx, tick_spacing);
        let (word, _) = tick_bitmap_position(compressed);
        words_to_check.insert(word);
    }
    let compressed_current = compress_tick(active_tick, tick_spacing);
    let (current_word, _) = tick_bitmap_position(compressed_current);
    for w in (current_word - 2)..=(current_word + 2) {
        words_to_check.insert(w);
    }

    // Collect all on-chain tick indices from bitmap scanning
    let mut on_chain_tick_indices: std::collections::HashSet<i32> =
        std::collections::HashSet::new();

    for word in &words_to_check {
        // Encode: tickBitmap(int16)
        let calldata = encode_calldata(
            V3_TICK_BITMAP_SELECTOR,
            &[DynSolValue::Int(
                I256::unchecked_from(i128::from(i64::from(*word))),
                16,
            )],
        );

        let result = provider
            .eth_call(&pool_addr, calldata, block_number)
            .await
            .map_err(|e| LiquidityVerifyError::Rpc {
                message: format!(
                    "V3 pool {pool_addr} {block_tag}: tickBitmap({word}) RPC call failed: {e}"
                ),
            })?;

        let bitmap_val = decode_uint256(&result[0..32]);
        if bitmap_val.is_zero() {
            continue;
        }

        for bit in 0..256u64 {
            // SAFETY: bit is 0..255, so cast to usize is safe on any target.
            #[allow(clippy::cast_possible_truncation)]
            if bitmap_val.bit(bit as usize) {
                let compressed_tick = i32::from(*word) * 256 + i32::try_from(bit).unwrap();
                // SAFETY: compressed_tick * tick_spacing fits in the int24 range
                // that Uniswap V3 uses, so the truncation to i32 is safe.
                let tick = compressed_tick * tick_spacing;
                on_chain_tick_indices.insert(tick);
            }
        }
    }

    // 2. Verify each tick in our tick_data by calling pool.ticks() directly
    for (&tick_idx, our_info) in tick_data {
        let our_gross = our_info.liquidity_gross.to::<u128>();
        let our_net: i128 = our_info.liquidity_net.try_into().unwrap_or_default();

        let (on_chain_gross, on_chain_net) =
            call_v3_ticks(provider, pool_addr, tick_idx, block_number).await?;

        if our_gross != on_chain_gross {
            return Err(LiquidityVerifyError::Mismatch(VerificationMismatch {
                message: format!(
                    "V3 pool {pool_addr} {block_tag}: tick {tick_idx} liquidityGross mismatch — engine: {our_gross}, on-chain: {on_chain_gross}"
                ),
            }));
        }
        if our_net != on_chain_net {
            return Err(LiquidityVerifyError::Mismatch(VerificationMismatch {
                message: format!(
                    "V3 pool {pool_addr} {block_tag}: tick {tick_idx} liquidityNet mismatch — engine: {our_net}, on-chain: {on_chain_net}"
                ),
            }));
        }

        // Remove from on-chain set (we've verified this tick)
        on_chain_tick_indices.remove(&tick_idx);
    }

    // 3. Check for on-chain ticks we're missing
    if let Some(&tick_idx) = on_chain_tick_indices.iter().next() {
        let (on_chain_gross, on_chain_net) =
            call_v3_ticks(provider, pool_addr, tick_idx, block_number).await?;
        return Err(LiquidityVerifyError::Mismatch(VerificationMismatch {
            message: format!(
                "V3 pool {pool_addr} {block_tag}: tick {tick_idx} exists on-chain (lg={on_chain_gross}, ln={on_chain_net}) but NOT in engine"
            ),
        }));
    }

    Ok(())
}

/// Call `ticks(int24)` on a V3 pool and return `(liquidityGross, liquidityNet)`.
async fn call_v3_ticks(
    provider: &AlloyProvider,
    pool_addr: Address,
    tick: i32,
    block_number: Option<u64>,
) -> Result<(u128, i128), LiquidityVerifyError> {
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
        .map_err(|e| LiquidityVerifyError::Rpc {
            message: format!("V3 pool {pool_addr} {block_tag}: ticks({tick}) RPC call failed: {e}"),
        })?;

    // ticks() returns (uint128 liquidityGross, int128 liquidityNet, ...)
    // We only need the first two fields.
    if result.len() < 64 {
        return Err(LiquidityVerifyError::Mismatch(VerificationMismatch {
            message: format!(
                "V3 pool {pool_addr} {block_tag}: ticks({tick}) returned {} bytes, expected at least 64",
                result.len()
            ),
        }));
    }

    let lg = decode_uint128(&result[0..32]);
    let ln = decode_int128(&result[32..64]);
    Ok((lg, ln))
}

/// Call `StateView.getTickLiquidity(bytes32,int24)` and return `(liquidityGross, liquidityNet)`.
async fn call_state_view_tick_liquidity(
    provider: &AlloyProvider,
    state_view: Address,
    pool_id: [u8; 32],
    tick: i32,
    block_number: Option<u64>,
) -> Result<(u128, i128), LiquidityVerifyError> {
    let pool_id_hex = crate::hex_utils::encode_hex(&pool_id);
    let block_tag = match block_number {
        Some(b) => format!("block={b}"),
        None => "block=pending".to_string(),
    };

    let calldata = encode_calldata(
        STATE_VIEW_GET_TICK_LIQUIDITY_SELECTOR,
        &[
            DynSolValue::FixedBytes(B256::from(pool_id), 32),
            DynSolValue::Int(I256::unchecked_from(i128::from(tick)), 24),
        ],
    );

    let result = provider
        .eth_call(&state_view, calldata, block_number)
        .await
        .map_err(|e| LiquidityVerifyError::Rpc {
            message: format!(
                "V4 pool 0x{pool_id_hex} {block_tag}: getTickLiquidity({tick}) RPC call failed: {e}"
            ),
        })?;

    if result.len() < 64 {
        return Err(LiquidityVerifyError::Mismatch(VerificationMismatch {
            message: format!(
                "V4 pool 0x{pool_id_hex} {block_tag}: getTickLiquidity({tick}) returned {} bytes, expected 64",
                result.len()
            ),
        }));
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
/// # Errors
///
/// Returns `Err(VerificationMismatch)` if any pool's tick bitmap or tick data
/// differs from on-chain state.
pub async fn verify_v4_pools<S: std::hash::BuildHasher>(
    provider: &AlloyProvider,
    state_view: Address,
    pools: &HashMap<u64, V4PoolState, S>,
    block_number: Option<u64>,
) -> Result<(), LiquidityVerifyError> {
    // Deduplicate by pool_id — both forward and reverse orientations share the same
    // on-chain state, so we only need to verify each pool_id once.
    let mut seen_pool_ids: HashMap<[u8; 32], &V4PoolState> = HashMap::new();
    for pool in pools.values() {
        seen_pool_ids.entry(pool.pool_id).or_insert(pool);
    }

    for (pool_id, pool) in seen_pool_ids {
        verify_v4_pool(provider, state_view, pool_id, pool, block_number).await?;
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn verify_v4_pool<T: TickMap + ?Sized>(
    provider: &AlloyProvider,
    state_view: Address,
    pool_id: [u8; 32],
    pool: &T,
    block_number: Option<u64>,
) -> Result<(), LiquidityVerifyError> {
    // Read the tick bookkeeping map + immutable identification through the ADR-004
    // `TickMap` trait — the slot0 head scalars are deliberately out of reach.
    // `pool_id` and `state_view` stay as separate args (V4-specific RPC concerns
    // that don't fit the trait; the V4 verifier calls `state_view`, not the pool
    // address, for its RPC target — see ADR-004).
    let pool_id_bytes = pool_id;
    let tick_spacing = pool.tick_spacing();
    let active_tick = pool.active_tick();
    let tick_data = pool.tick_data();
    let block_tag = match block_number {
        Some(b) => format!("block={b}"),
        None => "block=pending".to_string(),
    };
    let pool_id_hex = crate::hex_utils::encode_hex(&pool_id_bytes);

    // 1. Discover on-chain populated bitmap words
    let mut words_to_check: std::collections::HashSet<i16> = std::collections::HashSet::new();
    collect_bitmap_words(tick_data, active_tick, tick_spacing, &mut words_to_check);

    let mut on_chain_ticks: HashMap<i32, (u128, i128)> = HashMap::new();

    for word in &words_to_check {
        let bitmap_val = fetch_v4_tick_bitmap(
            provider,
            state_view,
            pool_id_bytes,
            pool_id_hex.as_str(),
            block_tag.as_str(),
            *word,
            block_number,
        )
        .await?;

        if bitmap_val.is_zero() {
            continue;
        }

        // Enumerate set bits in the bitmap
        #[allow(clippy::cast_possible_truncation)]
        for bit in 0..256u64 {
            if bitmap_val.bit(bit as usize) {
                let compressed_tick = i32::from(*word) * 256 + i32::try_from(bit).unwrap();
                let tick_i32 = compressed_tick * tick_spacing;

                let (gross, net) = fetch_v4_tick_liquidity(
                    provider,
                    state_view,
                    pool_id_bytes,
                    pool_id_hex.as_str(),
                    block_tag.as_str(),
                    tick_i32,
                    block_number,
                )
                .await?;

                on_chain_ticks.insert(tick_i32, (gross, net));
            }
        }
    }

    // 2. Compare every tick in our tick_data against on-chain
    for (&tick_idx, our_info) in tick_data {
        let our_gross = our_info.liquidity_gross.to::<u128>();
        let our_net: i128 = our_info.liquidity_net.try_into().unwrap_or_default();

        if let Some(&(on_chain_gross, on_chain_net)) = on_chain_ticks.get(&tick_idx) {
            if our_gross != on_chain_gross {
                return Err(LiquidityVerifyError::Mismatch(VerificationMismatch {
                    message: format!(
                        "V4 pool 0x{pool_id_hex} {block_tag}: tick {tick_idx} liquidityGross mismatch — engine: {our_gross}, on-chain: {on_chain_gross}"
                    ),
                }));
            }
            if our_net != on_chain_net {
                return Err(LiquidityVerifyError::Mismatch(VerificationMismatch {
                    message: format!(
                        "V4 pool 0x{pool_id_hex} {block_tag}: tick {tick_idx} liquidityNet mismatch — engine: {our_net}, on-chain: {on_chain_net}"
                    ),
                }));
            }
        } else {
            return Err(LiquidityVerifyError::Mismatch(VerificationMismatch {
                message: format!(
                    "V4 pool 0x{pool_id_hex} {block_tag}: tick {tick_idx} exists in engine (lg={our_gross}, ln={our_net}) but NOT on-chain"
                ),
            }));
        }
    }

    // 3. Check for on-chain ticks we're missing
    for (&tick_idx, &(on_chain_gross, on_chain_net)) in &on_chain_ticks {
        if !tick_data.contains_key(&tick_idx) {
            return Err(LiquidityVerifyError::Mismatch(VerificationMismatch {
                message: format!(
                    "V4 pool 0x{pool_id_hex} {block_tag}: tick {tick_idx} exists on-chain (lg={on_chain_gross}, ln={on_chain_net}) but NOT in engine"
                ),
            }));
        }
    }

    Ok(())
}

/// Collect bitmap words from tick data and around the current tick.
fn collect_bitmap_words<S: std::hash::BuildHasher>(
    tick_data: &HashMap<i32, crate::bot_core::TickInfo, S>,
    current_tick: i32,
    tick_spacing: i32,
    words: &mut std::collections::HashSet<i16>,
) {
    for &tick_idx in tick_data.keys() {
        let compressed = compress_tick(tick_idx, tick_spacing);
        let (word, _) = tick_bitmap_position(compressed);
        words.insert(word);
    }

    let compressed_current = compress_tick(current_tick, tick_spacing);
    let (current_word, _) = tick_bitmap_position(compressed_current);
    for w in (current_word - 2)..=(current_word + 2) {
        words.insert(w);
    }
}

/// Fetch a V4 tick bitmap word from the `StateView` contract.
async fn fetch_v4_tick_bitmap(
    provider: &AlloyProvider,
    state_view: Address,
    pool_id_bytes: [u8; 32],
    pool_id_hex: &str,
    block_tag: &str,
    word: i16,
    block_number: Option<u64>,
) -> Result<U256, LiquidityVerifyError> {
    let bitmap_calldata = encode_calldata(
        STATE_VIEW_GET_TICK_BITMAP_SELECTOR,
        &[
            DynSolValue::FixedBytes(B256::from(pool_id_bytes), 32),
            DynSolValue::Int(I256::unchecked_from(i128::from(i64::from(word))), 16),
        ],
    );

    let bitmap_result = provider
        .eth_call(&state_view, bitmap_calldata, block_number)
        .await
        .map_err(|e| LiquidityVerifyError::Rpc {
            message: format!(
                "V4 pool 0x{pool_id_hex} {block_tag}: getTickBitmap({word}) RPC call failed: {e}"
            ),
        })?;

    Ok(decode_uint256(&bitmap_result[0..32]))
}

/// Fetch a V4 tick's `(liquidityGross, liquidityNet)` from the `StateView` contract.
async fn fetch_v4_tick_liquidity(
    provider: &AlloyProvider,
    state_view: Address,
    pool_id_bytes: [u8; 32],
    pool_id_hex: &str,
    block_tag: &str,
    tick_i32: i32,
    block_number: Option<u64>,
) -> Result<(u128, i128), LiquidityVerifyError> {
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
        .map_err(|e| LiquidityVerifyError::Rpc {
            message: format!(
                "V4 pool 0x{pool_id_hex} {block_tag}: getTickLiquidity({tick_i32}) RPC call failed: {e}"
            ),
        })?;

    if tick_liq_result.len() < 64 {
        return Err(LiquidityVerifyError::Mismatch(VerificationMismatch {
            message: format!(
                "V4 pool 0x{pool_id_hex} {block_tag}: getTickLiquidity({tick_i32}) returned {} bytes, expected 64",
                tick_liq_result.len()
            ),
        }));
    }

    let gross = decode_uint128(&tick_liq_result[0..32]);
    let net = decode_int128(&tick_liq_result[32..64]);
    Ok((gross, net))
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
        assert_eq!(compress_tick(-292_420, 1), -292_420);
    }

    #[test]
    fn compress_tick_spacing_10() {
        // Real-world V4 pool tick_spacing
        assert_eq!(compress_tick(0, 10), 0);
        assert_eq!(compress_tick(10, 10), 1);
        assert_eq!(compress_tick(-10, 10), -1);
        assert_eq!(compress_tick(-20, 10), -2);
        assert_eq!(compress_tick(-11, 10), -2); // floor(-1.1) = -2
        assert_eq!(compress_tick(-292_420, 10), -29_242); // The bug case
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
        assert_eq!(compress_tick(887_272, 10), 88_727);
        assert_eq!(compress_tick(-887_272, 10), -88_728); // floor(-88727.2) = -88728
                                                          // int24 min = -887272
        assert_eq!(compress_tick(-887_272, 1), -887_272);
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
        let tick: i32 = 292_420;
        let tick_spacing: i32 = 10;
        let compressed = compress_tick(tick, tick_spacing);
        let (word, bit) = tick_bitmap_position(compressed);
        // Reverse: compressed_tick = word * 256 + bit, tick = compressed_tick * tick_spacing
        let recovered_tick = (i64::from(word) * 256 + i64::from(bit)) * i64::from(tick_spacing);
        assert_eq!(recovered_tick, i64::from(tick));
    }

    #[test]
    fn round_trip_negative_tick() {
        let tick: i32 = -292_420;
        let tick_spacing: i32 = 10;
        let compressed = compress_tick(tick, tick_spacing);
        let (word, bit) = tick_bitmap_position(compressed);
        let recovered_tick = (i64::from(word) * 256 + i64::from(bit)) * i64::from(tick_spacing);
        assert_eq!(recovered_tick, i64::from(tick));
    }

    #[test]
    fn round_trip_negative_tick_spacing_60() {
        let tick: i32 = -120;
        let tick_spacing: i32 = 60;
        let compressed = compress_tick(tick, tick_spacing);
        let (word, bit) = tick_bitmap_position(compressed);
        let recovered_tick = (i64::from(word) * 256 + i64::from(bit)) * i64::from(tick_spacing);
        assert_eq!(recovered_tick, i64::from(tick));
    }

    #[test]
    fn round_trip_negative_non_aligned_tick() {
        // tick=-61, spacing=60: not a multiple, but compress still works
        // compressed = floor(-61/60) = -2
        // position(-2): word = -1 >> 8... wait, -2 >> 8 in arithmetic = -1
        // Python: (-2) >> 8 = -1, (-2) % 256 = 254
        let tick: i32 = -61;
        let tick_spacing: i32 = 60;
        let compressed = compress_tick(tick, tick_spacing);
        assert_eq!(compressed, -2);
        let (word, bit) = tick_bitmap_position(compressed);
        assert_eq!((word, bit), (-1, 254));
        // Reverse: compressed_tick = -1 * 256 + 254 = -2
        let recovered_compressed = i64::from(word) * 256 + i64::from(bit);
        assert_eq!(recovered_compressed, i64::from(compressed));
        // tick = -2 * 60 = -120 (the nearest aligned tick, not -61)
        let recovered_tick = recovered_compressed * i64::from(tick_spacing);
        assert_eq!(recovered_tick, -120);
    }

    // --- Exhaustive test: all negative tick/spacing combos in int24 range ---
    #[test]
    fn compress_matches_python_floor_division() {
        // Test every tick_spacing and a range of ticks that produced the original bug
        for &tick_spacing in &[1i32, 10, 60, 200] {
            for tick in (-500i32..=500).chain([-292_420_i32, 887_272, -887_272]) {
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
        for tick in (-1000i32..=1000).chain([-29_242_i32, 29_242, -887_272, 887_272]) {
            let (rust_word, rust_bit) = tick_bitmap_position(tick);
            // Python semantics
            let py_word = tick >> 8; // arithmetic shift right
            let py_bit = tick.rem_euclid(256); // Python's % always non-negative for positive divisor
                                               // SAFETY: py_word fits in i16 and py_bit fits in u8 for any int24 tick value
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
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
}
