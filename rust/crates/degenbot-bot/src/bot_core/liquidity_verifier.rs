//! Liquidity map verification — compares engine's in-memory tick data against on-chain state.
//!
//! The "liquidity map" is the set of initialized ticks (the bitmap) and their
//! `(liquidityGross, liquidityNet)` values. Mutable scalar state like
//! `sqrtPriceX96`, `tick`, and `liquidity` changes on every swap and is NOT
//! verified here — it would always be stale. The map is only updated by
//! Mint/Burn (V3) or `ModifyLiquidity` (V4) events, which is what we validate.
//!
//! This is the {Slot0 Head / Tick Bookkeeping Map} split (see ADR-004): the
//! live variants (`verify_v3_pool` / `verify_v4_pool`) take `&V3PoolState` /
//! `&V4PoolState` and recover the "don't read slot0" rule from this module
//! doc; ADR-004 introduces a `TickMap` trait that narrows these to take
//! `&impl TickMap`, carrying the rule in the type system. The snapshot-block
//! variants (`verify_v3_liquidity_map` / `verify_v4_liquidity_map`) already
//! take a typed `&HashMap<i32, TickInfo>` + `Address` + block — the
//! typed-boundary precedent ADR-004 generalizes.
//!
//! - **V3**: `pool.tickBitmap(word)` discovers populated words,
//!   `pool.ticks(tickIndex)` verifies per-tick data.
//! - **V4**: `StateView.getTickBitmap(poolId, word)` discovers populated words,
//!   `StateView.getTickLiquidity(poolId, tick)` verifies per-tick data.
//!
//! On ANY mismatch, returns `Err` — the bot must not operate with stale tick data.

use std::collections::HashMap;

use alloy::primitives::{Address, Bytes, U256};

use crate::bot_core::{TickMap, V3PoolIdentity, V3PoolState, V4PoolIdentity, V4PoolState};
use degenbot_rpc::abi::{
    decode_tick_bitmap, decode_tick_data, decode_v4_tick_bitmap, decode_v4_tick_data,
    encode_tick_bitmap, encode_tick_data, encode_v4_tick_bitmap, encode_v4_tick_data,
};
use degenbot_rpc::multicall3::{multicall3_batch, MulticallResult};
use degenbot_rpc::provider::AlloyProvider;

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

// ── ABI encode/decode primitives live in `degenbot_rpc::abi` (the single deep
// home for onchain pool-state probing — see CONTEXT.md "Onchain pool-state
// probe"). This module keeps only the per-consumer adapter: `require_success`
// + the `ProviderError::DecodingError → LiquidityVerifyError::Mismatch` map
// (the revert-vs-mismatch distinction lives in `result.success`, inspected
// before decode, so it survives delegation unchanged).
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
///
/// # Panics
///
/// Panics if a tick in `tick_data` is absent from the internally-built
/// `ticks()` batch (logically impossible — the batch is built from
/// `tick_data.keys()` — so a panic indicates a logic bug, not bad input).
///
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
    let block_tag = format!("block={block_number}");
    // Batch-fetch all `ticks(tick)` in ONE Multicall3 `aggregate3` eth_call.
    let mut ticks: Vec<i32> = tick_data.keys().copied().collect();
    ticks.sort_unstable();
    let ticks_calls: Vec<(Address, Bytes)> = ticks
        .iter()
        .map(|&tick_idx| (pool_address, Bytes::from(encode_tick_data(tick_idx))))
        .collect();
    let results = multicall3_batch(provider, &ticks_calls, Some(block_number))
        .await
        .map_err(|e| LiquidityVerifyError::Rpc {
            message: format!("V3 pool {pool_address} {block_tag}: ticks batch failed: {e}"),
        })?;
    let on_chain: std::collections::HashMap<i32, &MulticallResult> =
        ticks.iter().copied().zip(&results).collect();
    for (&tick_idx, our_info) in tick_data {
        let our_gross = our_info.liquidity_gross.to::<u128>();
        let our_net: i128 = our_info.liquidity_net.try_into().unwrap_or_default();
        let result = on_chain
            .get(&tick_idx)
            .copied()
            .expect("tick in tick_data is in the batch");
        let (on_chain_gross, on_chain_net) =
            decode_v3_ticks_result(result, pool_address, tick_idx, &block_tag)?;
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
/// This is used for the snapshot-block verification: compare the DB-derived
/// `tick_data` against on-chain at the snapshot block, before any buffer events
/// are applied. Catches snapshot loading/serialization bugs.
///
/// # Panics
///
/// Panics if a tick in `tick_data` is absent from the internally-built
/// `getTickLiquidity` batch (logically impossible — the batch is built from
/// `tick_data.keys()` — so a panic indicates a logic bug, not bad input).
///
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
    let pool_id_hex = degenbot_core::hex_utils::encode_hex(&pool_id);
    let block_tag = format!("block={block_number}");
    // Batch all `StateView.getTickLiquidity(poolId, tick)` calls in ONE
    // Multicall3 `aggregate3` eth_call (replacing one serial eth_call per tick).
    let mut ticks: Vec<i32> = tick_data.keys().copied().collect();
    ticks.sort_unstable();
    let calls: Vec<(Address, Bytes)> = ticks
        .iter()
        .map(|&tick_idx| {
            (
                state_view,
                Bytes::from(encode_v4_tick_data(&pool_id, tick_idx)),
            )
        })
        .collect();
    let results = multicall3_batch(provider, &calls, Some(block_number))
        .await
        .map_err(|e| LiquidityVerifyError::Rpc {
            message: format!(
                "V4 pool {pool_id_hex} {block_tag}: getTickLiquidity batch failed: {e}"
            ),
        })?;
    let on_chain: HashMap<i32, &MulticallResult> = ticks.iter().copied().zip(&results).collect();
    for (&tick_idx, our_info) in tick_data {
        let our_gross = our_info.liquidity_gross.to::<u128>();
        let our_net: i128 = our_info.liquidity_net.try_into().unwrap_or_default();
        let result = on_chain
            .get(&tick_idx)
            .copied()
            .expect("tick in tick_data is in the batch");
        let (on_chain_gross, on_chain_net) =
            decode_v4_tick_liquidity_result(result, &pool_id_hex, &block_tag, tick_idx)?;
        if our_gross != on_chain_gross || our_net != on_chain_net {
            let pool_id_hex = degenbot_core::hex_utils::encode_hex(&pool_id);
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
    pools: &HashMap<u64, (V3PoolIdentity, V3PoolState), S>,
    block_number: Option<u64>,
) -> Result<(), LiquidityVerifyError> {
    for pool in pools.values() {
        verify_v3_pool(provider, pool, block_number).await?;
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
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
    // Scan words from our tick_data plus ±2 around the current tick.
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
    // Deterministic ordering (sorted ascending) so the Multicall3 sub-call
    // order — and thus the result-to-word mapping — is reproducible (also aids
    // caching/replay). Sort once, use everywhere.
    let mut words: Vec<i16> = words_to_check.into_iter().collect();
    words.sort_unstable();

    // Batch ALL `tickBitmap(word)` calls into ONE Multicall3 `aggregate3`
    // eth_call (replacing one serial eth_call per word). Reverted sub-call →
    // hard Rpc error (verified is a hard gate — a reverted bitmap read is
    // genuine RPC/contract trouble, not a zero bitmap).
    let bitmap_calls: Vec<(Address, Bytes)> = words
        .iter()
        .map(|&word| (pool_addr, Bytes::from(encode_tick_bitmap(word))))
        .collect();
    let bitmap_results = multicall3_batch(provider, &bitmap_calls, block_number)
        .await
        .map_err(|e| LiquidityVerifyError::Rpc {
            message: format!("V3 pool {pool_addr} {block_tag}: tickBitmap batch failed: {e}"),
        })?;

    // Collect all on-chain tick indices from bitmap scanning.
    let mut on_chain_tick_indices: std::collections::HashSet<i32> =
        std::collections::HashSet::new();
    for (word, result) in words.iter().zip(&bitmap_results) {
        let bitmap_val = decode_v3_bitmap_result(result, pool_addr, *word, &block_tag)?;
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

    // 2 + 3. Batch-fetch `ticks(tick)` for the UNION of engine ticks and
    // on-chain-discovered ticks in ONE Multicall3 `aggregate3` eth_call, then
    // compare. This folds the old phase-3 "re-fetch the first missing on-chain
    // tick" into the same batch (its lg/ln is already in the result set).
    let mut ticks_to_fetch: std::collections::HashSet<i32> = on_chain_tick_indices.clone();
    for &tick_idx in tick_data.keys() {
        ticks_to_fetch.insert(tick_idx);
    }
    let mut ticks: Vec<i32> = ticks_to_fetch.into_iter().collect();
    ticks.sort_unstable();
    let ticks_calls: Vec<(Address, Bytes)> = ticks
        .iter()
        .map(|&tick_idx| (pool_addr, Bytes::from(encode_tick_data(tick_idx))))
        .collect();
    let ticks_results = multicall3_batch(provider, &ticks_calls, block_number)
        .await
        .map_err(|e| LiquidityVerifyError::Rpc {
            message: format!("V3 pool {pool_addr} {block_tag}: ticks batch failed: {e}"),
        })?;
    // tick_index → (gross, net) from the batch (for O(1) lookup in the
    // comparison loop). unwrap_or_default() is unreachable: `ticks` and
    // `ticks_results` are the same length (multicall3_batch returns one
    // result per call, in order).
    let on_chain_ticks: std::collections::HashMap<i32, (u128, i128)> = ticks
        .iter()
        .zip(&ticks_results)
        .map(|(&tick_idx, result)| {
            let (gross, net) = decode_v3_ticks_result(result, pool_addr, tick_idx, &block_tag)?;
            Ok::<_, LiquidityVerifyError>((tick_idx, (gross, net)))
        })
        .collect::<Result<std::collections::HashMap<_, _>, _>>()?;

    // 2. Verify each tick in our tick_data against the batched on-chain values.
    for (&tick_idx, our_info) in tick_data {
        let our_gross = our_info.liquidity_gross.to::<u128>();
        let our_net: i128 = our_info.liquidity_net.try_into().unwrap_or_default();
        let (on_chain_gross, on_chain_net) =
            on_chain_ticks.get(&tick_idx).copied().unwrap_or((0, 0));

        if our_gross != on_chain_gross {
            // TEMP DEBUG: dump journal depth + update_block for the failing pool
            // so we can tell whether ANY events were applied since the snapshot seed.
            log::info!(
                "[dbg-verify] MISMATCH {pool_addr} tick={tick_idx} {block_tag} engine={our_gross} onchain={on_chain_gross} update_block={} journal_len={} total_ticks={}",
                pool.dbg_update_block(),
                pool.dbg_journal_len(),
                tick_data.len(),
            );
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

        // Remove from on-chain set (we've verified this tick).
        on_chain_tick_indices.remove(&tick_idx);
    }

    // 3. Check for on-chain ticks we're missing (any remaining discovered tick
    // not in engine). Its (lg, ln) is already in the batched map — no extra RPC.
    if let Some(&tick_idx) = on_chain_tick_indices.iter().next() {
        let (on_chain_gross, on_chain_net) =
            on_chain_ticks.get(&tick_idx).copied().unwrap_or((0, 0));
        return Err(LiquidityVerifyError::Mismatch(VerificationMismatch {
            message: format!(
                "V3 pool {pool_addr} {block_tag}: tick {tick_idx} exists on-chain (lg={on_chain_gross}, ln={on_chain_net}) but NOT in engine"
            ),
        }));
    }

    Ok(())
}

/// Decode a `tickBitmap(word)` `Multicall3` sub-call result into the bitmap
/// word. Preserves the pre-multicall hard-error contract: a reverted sub-call
/// (`success == false`) is a hard `LiquidityVerifyError::Rpc` (the verifier is
/// a hard gate — a reverted bitmap read is genuine RPC/contract trouble, not
/// a zero bitmap). In practice `tickBitmap()` never reverts for a valid int16,
/// so this only fires on real trouble.
fn decode_v3_bitmap_result(
    result: &MulticallResult,
    pool_addr: Address,
    word: i16,
    block_tag: &str,
) -> Result<U256, LiquidityVerifyError> {
    if !result.success {
        return Err(LiquidityVerifyError::Rpc {
            message: format!(
                "V3 pool {pool_addr} {block_tag}: tickBitmap({word}) RPC call failed: reverted"
            ),
        });
    }
    decode_tick_bitmap(&result.return_data).map_err(|_| {
        LiquidityVerifyError::Mismatch(VerificationMismatch {
            message: format!(
                "V3 pool {pool_addr} {block_tag}: tickBitmap({word}) returned {} bytes, expected at least 32",
                result.return_data.len()
            ),
        })
    })
}

/// Decode a `ticks(tick)` `Multicall3` sub-call result into
/// `(liquidityGross, liquidityNet)`. Preserves the pre-multicall hard-error
/// contract: a reverted sub-call is a hard `Rpc` error (the verifier is a hard
/// gate). A too-short successful return is a `Mismatch` (byte-identical to
/// the old `call_v3_ticks` inner check).
fn decode_v3_ticks_result(
    result: &MulticallResult,
    pool_addr: Address,
    tick: i32,
    block_tag: &str,
) -> Result<(u128, i128), LiquidityVerifyError> {
    if !result.success {
        return Err(LiquidityVerifyError::Rpc {
            message: format!(
                "V3 pool {pool_addr} {block_tag}: ticks({tick}) RPC call failed: reverted"
            ),
        });
    }
    let (gross, net) = decode_tick_data(&result.return_data).map_err(|_| {
        LiquidityVerifyError::Mismatch(VerificationMismatch {
            message: format!(
                "V3 pool {pool_addr} {block_tag}: ticks({tick}) returned {} bytes, expected at least 64",
                result.return_data.len()
            ),
        })
    })?;
    Ok((gross.to(), net.try_into().unwrap_or_default()))
}

/// Decode a `StateView.getTickBitmap(poolId, word)` Multicall3 sub-call result
/// into the bitmap word. Preserves the hard-error contract: a reverted
/// sub-call (`success == false`) is a hard `Rpc` error (the verifier is a hard
/// gate — a reverted bitmap read is genuine RPC/contract trouble, not a zero
/// bitmap). Adds a length check the old `fetch_v4_tick_bitmap` lacked (it
/// index-panicked on a <32-byte return).
fn decode_v4_bitmap_result(
    result: &MulticallResult,
    pool_id_hex: &str,
    block_tag: &str,
    word: i16,
) -> Result<U256, LiquidityVerifyError> {
    if !result.success {
        return Err(LiquidityVerifyError::Rpc {
            message: format!(
                "V4 pool 0x{pool_id_hex} {block_tag}: getTickBitmap({word}) RPC call failed: reverted"
            ),
        });
    }
    decode_v4_tick_bitmap(&result.return_data).map_err(|_| {
        LiquidityVerifyError::Mismatch(VerificationMismatch {
            message: format!(
                "V4 pool 0x{pool_id_hex} {block_tag}: getTickBitmap({word}) returned {} bytes, expected at least 32",
                result.return_data.len()
            ),
        })
    })
}

/// Decode a `StateView.getTickLiquidity(poolId, tick)` Multicall3 sub-call
/// result into `(liquidityGross, liquidityNet)`. Preserves the hard-error
/// contract: a reverted sub-call is a hard `Rpc` error; a too-short successful
/// return is the byte-identical `Mismatch` 'returned N bytes, expected 64'.
fn decode_v4_tick_liquidity_result(
    result: &MulticallResult,
    pool_id_hex: &str,
    block_tag: &str,
    tick: i32,
) -> Result<(u128, i128), LiquidityVerifyError> {
    if !result.success {
        return Err(LiquidityVerifyError::Rpc {
            message: format!(
                "V4 pool 0x{pool_id_hex} {block_tag}: getTickLiquidity({tick}) RPC call failed: reverted"
            ),
        });
    }
    let (gross, net) = decode_v4_tick_data(&result.return_data).map_err(|_| {
        LiquidityVerifyError::Mismatch(VerificationMismatch {
            message: format!(
                "V4 pool 0x{pool_id_hex} {block_tag}: getTickLiquidity({tick}) returned {} bytes, expected 64",
                result.return_data.len()
            ),
        })
    })?;
    Ok((gross.to(), net.try_into().unwrap_or_default()))
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
    pools: &HashMap<u64, (V4PoolIdentity, V4PoolState), S>,
    block_number: Option<u64>,
) -> Result<(), LiquidityVerifyError> {
    // Deduplicate by pool_id — both forward and reverse orientations share the same
    // on-chain state, so we only need to verify each pool_id once.
    let mut seen_pool_ids: HashMap<[u8; 32], &(V4PoolIdentity, V4PoolState)> = HashMap::new();
    for pool in pools.values() {
        seen_pool_ids.entry(pool.0.pool_id).or_insert(pool);
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
    let pool_id_hex = degenbot_core::hex_utils::encode_hex(&pool_id_bytes);

    // 1. Discover on-chain populated bitmap words
    let mut words_to_check: std::collections::HashSet<i16> = std::collections::HashSet::new();
    collect_bitmap_words(tick_data, active_tick, tick_spacing, &mut words_to_check);
    // Deterministic ordering (sorted ascending) so the Multicall3 sub-call
    // order — and thus the result-to-word mapping — is reproducible.
    let mut words: Vec<i16> = words_to_check.into_iter().collect();
    words.sort_unstable();

    // Batch ALL `StateView.getTickBitmap(poolId, word)` calls into ONE
    // Multicall3 `aggregate3` eth_call. A reverted sub-call → hard Rpc error
    // (the verifier is a hard gate). Reverted sub-calls do NOT mask a zero
    // bitmap (genuine RPC/contract trouble, not an empty word).
    let bitmap_calls: Vec<(Address, Bytes)> = words
        .iter()
        .map(|&word| {
            (
                state_view,
                Bytes::from(encode_v4_tick_bitmap(&pool_id_bytes, word)),
            )
        })
        .collect();
    let bitmap_results = multicall3_batch(provider, &bitmap_calls, block_number)
        .await
        .map_err(|e| LiquidityVerifyError::Rpc {
            message: format!(
                "V4 pool 0x{pool_id_hex} {block_tag}: getTickBitmap batch failed: {e}"
            ),
        })?;

    // Collect the on-chain tick indices (defer ALL `getTickLiquidity` fetches
    // to phase 2 — today's code fetches liquidity per-bit INSIDE this loop,
    // which is the biggest single fan-out: a dense word yields up to 256 serial
    // calls. Here we only discover tick indices; phase 2 batches every
    // getTickLiquidity across all words at once.)
    let mut on_chain_tick_indices: std::collections::HashSet<i32> =
        std::collections::HashSet::new();
    for (word, result) in words.iter().zip(&bitmap_results) {
        let bitmap_val = decode_v4_bitmap_result(result, &pool_id_hex, &block_tag, *word)?;
        if bitmap_val.is_zero() {
            continue;
        }
        #[allow(clippy::cast_possible_truncation)]
        for bit in 0..256u64 {
            if bitmap_val.bit(bit as usize) {
                let compressed_tick = i32::from(*word) * 256 + i32::try_from(bit).unwrap();
                let tick_i32 = compressed_tick * tick_spacing;
                on_chain_tick_indices.insert(tick_i32);
            }
        }
    }

    // 2 + 3. Batch-fetch `StateView.getTickLiquidity(poolId, tick)` for the UNION
    // of engine ticks and on-chain-discovered ticks in ONE Multicall3
    // `aggregate3` eth_call, then compare. Folds today's phase-3 "detect
    // missing on-chain tick" into the same batch (its (lg,ln) is already
    // fetched — today it re-calls getTickLiquidity lazily via the map).
    let mut ticks_to_fetch: std::collections::HashSet<i32> = on_chain_tick_indices.clone();
    for &tick_idx in tick_data.keys() {
        ticks_to_fetch.insert(tick_idx);
    }
    let mut ticks: Vec<i32> = ticks_to_fetch.into_iter().collect();
    ticks.sort_unstable();
    let ticks_calls: Vec<(Address, Bytes)> = ticks
        .iter()
        .map(|&tick_idx| {
            (
                state_view,
                Bytes::from(encode_v4_tick_data(&pool_id_bytes, tick_idx)),
            )
        })
        .collect();
    let ticks_results = multicall3_batch(provider, &ticks_calls, block_number)
        .await
        .map_err(|e| LiquidityVerifyError::Rpc {
            message: format!(
                "V4 pool 0x{pool_id_hex} {block_tag}: getTickLiquidity batch failed: {e}"
            ),
        })?;
    let on_chain_ticks: HashMap<i32, (u128, i128)> = ticks
        .iter()
        .zip(&ticks_results)
        .map(|(&tick_idx, result)| {
            let (gross, net) =
                decode_v4_tick_liquidity_result(result, &pool_id_hex, &block_tag, tick_idx)?;
            Ok::<_, LiquidityVerifyError>((tick_idx, (gross, net)))
        })
        .collect::<Result<HashMap<_, _>, _>>()?;

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

// ---------------------------------------------------------------------------
// Tests — tick bitmap word/bit calculation
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot_core::TickInfo;
    use alloy::dyn_abi::DynSolValue;
    use alloy::transports::mock::Asserter;
    use std::sync::Arc;

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

    // --- Multicall3 batching integration tests (MockTransport) ---
    //
    // These pin the contract that `verify_v3_pool` folds the per-word
    // `tickBitmap()` fan-out + the per-tick `ticks()` fan-out into ONE
    // Multicall3 `aggregate3` eth_call per phase (2 total), instead of
    // K_words + T_ticks serial round-trips. A revert on a batched sub-call is
    // a hard `Rpc` error (the verifier is a hard gate). See task DHAZQF.

    /// Minimal `TickMap` test double — owns a pool address, `tick_spacing`,
    /// `active_tick` + a `tick_data` map. `verify_v3_pool<T: TickMap>` takes `&T`,
    /// so this avoids constructing a full `V3PoolState`.
    struct TestV3Pool {
        address: Address,
        tick_spacing: i32,
        active_tick: i32,
        tick_data: HashMap<i32, TickInfo>,
    }

    impl TickMap for TestV3Pool {
        fn address(&self) -> Address {
            self.address
        }
        fn tick_spacing(&self) -> i32 {
            self.tick_spacing
        }
        fn active_tick(&self) -> i32 {
            self.active_tick
        }
        fn tick_data(&self) -> &HashMap<i32, TickInfo> {
            &self.tick_data
        }
    }

    /// Build a mock `AlloyProvider` whose `Asserter` queue is preloaded with
    /// `responses` (one JSON `result` per expected `eth_call`, FIFO). Mirrors the
    /// `block_pump` mock-transport harness.
    fn mock_provider(responses: Vec<String>) -> (AlloyProvider, Asserter) {
        use alloy::network::Ethereum as NetEth;
        use alloy::providers::{Provider, ProviderBuilder};
        use alloy::rpc::client::ClientBuilder;
        use alloy::transports::mock::MockTransport;
        let asserter = Asserter::new();
        for resp in responses {
            asserter.push_success(&resp);
        }
        let client = ClientBuilder::default().transport(MockTransport::new(asserter.clone()), true);
        let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
        let provider = AlloyProvider::from_provider(
            Arc::new(dyn_provider) as Arc<dyn alloy::providers::Provider<NetEth>>
        );
        (provider, asserter)
    }

    /// Encode a Multicall3 `aggregate3` return payload — `(bool,bytes)[]` —
    /// as a `0x`-prefixed hex string (the shape an `eth_call` result takes).
    /// One `(success, return_data)` tuple per sub-call, in batch order.
    fn encode_mc3_return(results: &[(bool, Vec<u8>)]) -> String {
        let arr = DynSolValue::Array(
            results
                .iter()
                .map(|(ok, data)| {
                    DynSolValue::Tuple(vec![
                        DynSolValue::Bool(*ok),
                        DynSolValue::Bytes(data.clone()),
                    ])
                })
                .collect(),
        );
        format!("0x{}", alloy::primitives::hex::encode(arr.abi_encode()))
    }

    /// 32-byte ABI word for a u256 bitmap value.
    fn u256_word(val: u64) -> Vec<u8> {
        U256::from(val).to_be_bytes::<32>().to_vec()
    }

    /// 64-byte `ticks()` return: `(uint128 liquidityGross, int128 liquidityNet)`
    /// (the remaining tuple fields are irrelevant — the decoder reads bytes 0..64).
    fn ticks_return(gross: u128, net: i128) -> Vec<u8> {
        let mut buf = vec![0u8; 64];
        buf[16..32].copy_from_slice(&gross.to_be_bytes());
        // ABI sign-extend `int128` to its 256-bit word (the real on-chain
        // return shape `ticks()` produces). High 16 bytes are the sign
        // extension: 0xFF for negative, 0x00 otherwise. The home's
        // `decode_tick_data` reads the full 32-byte word as an `I256`;
        // a non-sign-extended mock (the pre-slice-A shape) would decode
        // negative nets as huge positives and trip `try_into::<i128>()`.
        let sign = if net < 0 { 0xff } else { 0x00 };
        for b in &mut buf[32..48] {
            *b = sign;
        }
        buf[48..64].copy_from_slice(&net.to_be_bytes());
        buf
    }

    /// tick → (gross, net) the mock on-chain state reports.
    const TICK_0: (u128, i128) = (100, 50);
    const TICK_60: (u128, i128) = (200, -30);

    /// Build the bitmap-batch return for the 5-word ±2 scan around `active_tick=0`
    /// with `tick_spacing=60` (words sorted ascending: [-2,-1,0,1,2]). Only word 0
    /// has bits set (ticks 0 + 60 → bits 0 + 1 → bitmap value 3).
    fn bitmap_batch_return() -> String {
        // Sorted words: [-2, -1, 0, 1, 2].
        encode_mc3_return(&[
            (true, u256_word(0)), // word -2
            (true, u256_word(0)), // word -1
            (true, u256_word(3)), // word  0  → bits 0,1 (ticks 0, 60 @ spacing 60)
            (true, u256_word(0)), // word  1
            (true, u256_word(0)), // word  2
        ])
    }

    /// Build the ticks-batch return for ticks sorted ascending [0, 60].
    fn ticks_batch_return() -> String {
        encode_mc3_return(&[
            (true, ticks_return(TICK_0.0, TICK_0.1)),
            (true, ticks_return(TICK_60.0, TICK_60.1)),
        ])
    }

    fn tick_info(gross: u128, net: i128) -> TickInfo {
        TickInfo {
            liquidity_gross: alloy::primitives::U128::from(gross),
            liquidity_net: alloy::primitives::I256::unchecked_from(net),
            block: 0,
        }
    }

    fn v3_pool_matching() -> TestV3Pool {
        let mut tick_data = HashMap::new();
        tick_data.insert(0, tick_info(TICK_0.0, TICK_0.1));
        tick_data.insert(60, tick_info(TICK_60.0, TICK_60.1));
        TestV3Pool {
            address: Address::from([0xaau8; 20]),
            tick_spacing: 60,
            active_tick: 0,
            tick_data,
        }
    }

    #[tokio::test]
    async fn verify_v3_pool_batches_into_two_eth_calls() {
        // Preload EXACTLY 2 responses (1 bitmap batch + 1 ticks batch). If
        // verify_v3_pool makes any 3rd eth_call (e.g. still serial per-word /
        // per-tick), the asserter queue empties → transport error → verify
        // returns Err and the `is_ok()` assertion fails. If it makes FEWER,
        // `read_q()` is non-empty after.
        let (provider, asserter) = mock_provider(vec![bitmap_batch_return(), ticks_batch_return()]);
        let pool = v3_pool_matching();

        let result = verify_v3_pool(&provider, &pool, Some(42)).await;

        assert!(
            result.is_ok(),
            "matching scenario must verify green: {result:?}"
        );
        assert!(
            asserter.read_q().is_empty(),
            "exactly 2 eth_calls expected (1 bitmap batch + 1 ticks batch); leftover responses: {}",
            asserter.read_q().len()
        );
    }

    #[tokio::test]
    async fn verify_v3_pool_surfaces_gross_mismatch_byte_identical() {
        // Same batched fetch, but the engine's tick-0 gross disagrees with the
        // on-chain (mock) value → must surface the exact pre-multicall message.
        let (provider, asserter) = mock_provider(vec![bitmap_batch_return(), ticks_batch_return()]);
        let mut pool = v3_pool_matching();
        pool.tick_data.get_mut(&0).unwrap().liquidity_gross =
            alloy::primitives::U128::from(999u128); // ≠ on-chain TICK_0.0 (100)

        let expected_addr = format!("{}", pool.address());
        let err = verify_v3_pool(&provider, &pool, Some(42))
            .await
            .unwrap_err();
        match err {
            LiquidityVerifyError::Mismatch(m) => {
                assert_eq!(
                    m.message,
                    format!("V3 pool {expected_addr} block=42: tick 0 liquidityGross mismatch — engine: 999, on-chain: 100")
                );
            }
            other @ LiquidityVerifyError::Rpc { .. } => panic!("expected Mismatch, got {other:?}"),
        }
        // Both batches were still consumed (decode happens after fetch).
        assert!(asserter.read_q().is_empty());
    }

    // --- V4 batching integration tests ---
    //
    // `verify_v4_pool` calls `StateView` (not the pool address) for both
    // `getTickBitmap` + `getTickLiquidity`. The V4 fan-out today is the worst:
    // phase 1 fetches `getTickLiquidity` per-bit INSIDE the bitmap loop (a dense
    // word = up to 256 serial calls). The batched version defers ALL liquidity
    // fetches to one phase-2 batch → 2 eth_calls total.

    fn v4_pool_id() -> [u8; 32] {
        [0xbbu8; 32]
    }
    fn v4_state_view() -> Address {
        Address::from([0x55u8; 20])
    }

    #[tokio::test]
    async fn verify_v4_pool_batches_into_two_eth_calls() {
        let (provider, asserter) = mock_provider(vec![bitmap_batch_return(), ticks_batch_return()]);
        let pool = v3_pool_matching();

        let result =
            verify_v4_pool(&provider, v4_state_view(), v4_pool_id(), &pool, Some(42)).await;

        assert!(
            result.is_ok(),
            "matching V4 scenario must verify green: {result:?}"
        );
        assert!(
            asserter.read_q().is_empty(),
            "exactly 2 eth_calls expected (1 getTickBitmap batch + 1 getTickLiquidity batch); \
             leftover responses: {}",
            asserter.read_q().len()
        );
    }

    #[tokio::test]
    async fn verify_v4_pool_surfaces_gross_mismatch_byte_identical() {
        let (provider, asserter) = mock_provider(vec![bitmap_batch_return(), ticks_batch_return()]);
        let mut pool = v3_pool_matching();
        pool.tick_data.get_mut(&0).unwrap().liquidity_gross =
            alloy::primitives::U128::from(999u128); // ≠ on-chain TICK_0.0 (100)

        let expected_pid = degenbot_core::hex_utils::encode_hex(&v4_pool_id());
        let err = verify_v4_pool(&provider, v4_state_view(), v4_pool_id(), &pool, Some(42))
            .await
            .unwrap_err();
        match err {
            LiquidityVerifyError::Mismatch(m) => {
                assert_eq!(
                    m.message,
                    format!("V4 pool 0x{expected_pid} block=42: tick 0 liquidityGross mismatch — engine: 999, on-chain: 100")
                );
            }
            other @ LiquidityVerifyError::Rpc { .. } => panic!("expected Mismatch, got {other:?}"),
        }
        assert!(asserter.read_q().is_empty());
    }
}
