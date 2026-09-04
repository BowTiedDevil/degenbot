//! On-chain-truth verification of a V3/V4 pool's in-memory liquidity map
//! against on-chain truth at a block number — the pre-commit gate
//! (`run_pool_update --verify`, Full per-pool per-chunk).
//!
//! **V3** reads `IUniswapV3Pool.ticks(int24)` + `tickBitmap(int16)` (selectors
//! `0xf30dba93` / `0x5339c296`) via `multicall3_batch` — one call per in-memory
//! tick/word.
//!
//! **V4** reads the singleton `PoolManager`'s tick/bitmap storage via
//! `extsload(bytes32[])` (selector `0xdbd035ff`) — all tick + bitmap slots in a
//! single `eth_call` round trip (cheaper than V3). See
//! [`docs/architecture/v4_poolmanager_storage_layout.md`] for the slot layout.
//!
//! V3 encode/decode (`ticks(int24)`, `tickBitmap(int16)`) and the selector
//! constants live in `degenbot_rpc::abi` — the single deep home for onchain
//! pool-state probing (see CONTEXT.md "Onchain pool-state probe"). This module
//! keeps only the V3 per-consumer adapter (`decode_ticks_result` /
//! `decode_bitmap_result`, mapping a reverted / too-short `MulticallResult` to
//! `None`), the V4 `extsload` path (a separate mechanism — direct storage-slot
//! reads, not ABI calls), and the divergence surface (`compare_tick`,
//! `compare_bitmap_word`, `v4_*_slot`, `encode_extsload_call`,
//! `decode_extsload_return`). The V4 slot-derive + extsload helpers remain
//! offline-unit-tested here (no RPC): the reference slot vectors are built
//! with an INDEPENDENT `cast keccak` oracle so the slot math is checked
//! against a different keccak than this crate's alloy `keccak256`.
//!
//! Zero `pyo3` (enforced by `just check-no-pyo3-in-cores`).

use alloy::dyn_abi::{DynSolType, DynSolValue};
use alloy::primitives::{keccak256, Address, Bytes, B256, I256, U128, U256};
use degenbot_core::errors::ProviderError;
use degenbot_db::{ApplyLiquidityAtTick, ComputedLiquidityUpdate};
use degenbot_rpc::abi::{
    decode_tick_bitmap, decode_tick_data, encode_tick_bitmap, encode_tick_data,
};
use degenbot_rpc::multicall3::{multicall3_batch, MulticallResult};
use degenbot_rpc::provider::AlloyProvider;
use std::str::FromStr;

use crate::run::RunError;

// V3 ABI selectors + `ticks`/`tickBitmap` calldata builders / return
// decoders live in `degenbot_rpc::abi` (the single deep home for onchain
// pool-state probing — see CONTEXT.md "Onchain pool-state probe"). This
// module keeps only the per-consumer adapter: `decode_ticks_result` /
// `decode_bitmap_result` map a reverted / too-short `MulticallResult` to
// `None` (the V3 verify path treats a revert as a divergence, not an error).

/// A named, bisect-able divergence between the in-memory map and on-chain
/// truth. Empty list = GREEN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiquidityDivergence {
    /// In-memory tick gross != on-chain `ticks().liquidityGross`. A `actual ==
    /// U128::ZERO` with a non-zero `expected` is the ghost-row / stale-tick
    /// class (a drained pool's undrained row — the 6SRJRL corruption shape).
    TickGross {
        tick: i32,
        expected: U128,
        actual: U128,
    },
    /// In-memory tick net != on-chain `ticks().liquidityNet`.
    TickNet {
        tick: i32,
        expected: I256,
        actual: I256,
    },
    /// In-memory bitmap word != on-chain `tickBitmap(word)`.
    BitmapWord {
        word: i32,
        expected: U256,
        actual: U256,
    },
    /// The multicall sub-call reverted (or returned < 64 bytes). Treated as
    /// gross/net unknown — the in-memory tick's expected values are surfaced
    /// for triage. (V3 `ticks()` never reverts — it returns zeros for an
    /// uninitialized tick — so this fires only on a transport/node fault.)
    TickCallReverted { tick: i32 },
    /// The multicall sub-call for a bitmap word reverted / returned < 32 bytes.
    BitmapCallReverted { word: i32 },
}

/// Decode a `MulticallResult` (ticks call) into `Some((gross, net))` or `None`
/// on revert / short return. Delegates the byte decode to the home
/// `decode_tick_data`; a `< 64`-byte payload surfaces there as
/// `DecodingError`, mapped to `None` here (caller treats it as a revert /
/// divergence — V3 `ticks()` never reverts for a valid int24, it returns
/// zeros, so a `None` here is genuine RPC/contract trouble surfaced as a
/// divergence).
fn decode_ticks_result(r: &MulticallResult) -> Option<(U128, i128)> {
    if r.success {
        decode_tick_data(r.return_data.as_ref()).ok()
    } else {
        None
    }
}

/// Decode a `MulticallResult` (tickBitmap call) into `Some(word)` or `None`
/// on revert / short return. Delegates to the home `decode_tick_bitmap`.
fn decode_bitmap_result(r: &MulticallResult) -> Option<U256> {
    if r.success {
        decode_tick_bitmap(r.return_data.as_ref()).ok()
    } else {
        None
    }
}

/// Compare one in-memory tick against the decoded on-chain `(gross, net)`.
/// `None` chain values ⇒ the call reverted → `TickCallReverted`.
fn compare_tick(
    tick: i32,
    expected: &ApplyLiquidityAtTick,
    actual: Option<(U128, i128)>,
) -> Vec<LiquidityDivergence> {
    // The math-crate comparison input (`ApplyLiquidityAtTick`) keeps the wide
    // I256 intermediate by design; the on-chain decode arrives at the native
    // int128 width (LIBQKE) — widen losslessly for the comparison.
    let widened = actual.map(|(g, n)| (g, I256::try_from(n).unwrap_or(I256::ZERO)));
    let Some((gross, net)) = widened else {
        return vec![LiquidityDivergence::TickCallReverted { tick }];
    };
    let mut out = Vec::new();
    if gross != expected.liquidity_gross {
        out.push(LiquidityDivergence::TickGross {
            tick,
            expected: expected.liquidity_gross,
            actual: gross,
        });
    }
    if net != expected.liquidity_net {
        out.push(LiquidityDivergence::TickNet {
            tick,
            expected: expected.liquidity_net,
            actual: net,
        });
    }
    out
}

/// Compare one in-memory bitmap word against the decoded on-chain `uint256`.
fn compare_bitmap_word(
    word: i32,
    expected: U256,
    actual: Option<U256>,
) -> Vec<LiquidityDivergence> {
    let Some(bitmap) = actual else {
        return vec![LiquidityDivergence::BitmapCallReverted { word }];
    };
    if bitmap == expected {
        Vec::new()
    } else {
        vec![LiquidityDivergence::BitmapWord {
            word,
            expected,
            actual: bitmap,
        }]
    }
}

/// Verify a V3 pool's entire in-memory liquidity map against on-chain truth at
/// `block_number` (Full per-pool verification). Issues one `ticks(int24)` call
/// per in-memory tick + one `tickBitmap(int16)` call per in-memory bitmap
/// word, batched through Multicall3. Returns the divergence list (empty =
/// GREEN); a non-empty list from the chunk loop's pre-commit gate rolls back
/// the chunk's transaction + does NOT advance the exchange's
/// `last_update_block`.
///
/// # Errors
///
/// Returns [`RunError::Provider`] only if the Multicall3 batch + its
/// sequential `eth_call` fallback BOTH fail (a transport/node fault — not a
/// per-tick revert, which surfaces as a divergence, not an error).
pub async fn verify_v3_liquidity_map_on_chain(
    provider: &AlloyProvider,
    pool_address: Address,
    computed: &ComputedLiquidityUpdate,
    block_number: u64,
) -> Result<Vec<LiquidityDivergence>, RunError> {
    let mut divergences = Vec::new();

    let ticks: Vec<i32> = computed.tick_data.keys().copied().collect();
    let words: Vec<i32> = computed.tick_bitmap.keys().copied().collect();

    let mut calls: Vec<(Address, Bytes)> = Vec::with_capacity(ticks.len() + words.len());
    for &tick in &ticks {
        calls.push((pool_address, Bytes::from(encode_tick_data(tick))));
    }
    let tick_count = ticks.len();
    for &word in &words {
        // Bitmap word positions are `int16`; the in-memory `tick_bitmap` keys
        // them as `i32` for arithmetic, so the `as i16` narrowing is lossless
        // for any valid word (the old `int_selector_calldata` path treated the
        // same `i32` as its low-2-byte int16). Suppress `cast_possible_truncation`
        // because the narrowing is the documented semantic, not an accident.
        #[expect(clippy::cast_possible_truncation)]
        let word_i16 = word as i16;
        calls.push((pool_address, Bytes::from(encode_tick_bitmap(word_i16))));
    }

    let results = multicall3_batch(provider, &calls, Some(block_number)).await?;

    for (i, &tick) in ticks.iter().enumerate() {
        let expected = &computed.tick_data[&tick];
        divergences.extend(compare_tick(
            tick,
            expected,
            decode_ticks_result(&results[i]),
        ));
    }
    for (j, &word) in words.iter().enumerate() {
        let expected = computed.tick_bitmap[&word].bitmap;
        divergences.extend(compare_bitmap_word(
            word,
            expected,
            decode_bitmap_result(&results[tick_count + j]),
        ));
    }
    Ok(divergences)
}

// ═══════════════════════════════════════════════════════════════════════════
// V4 on-chain-truth verification via `PoolManager.extsload(bytes32[])`
// ═══════════════════════════════════════════════════════════════════════════
//
// V4 exposes no public `ticks()`/`tickBitmap()` view; tick state is internal
// storage read via raw slot reads. The PoolManager's only per-tick/per-word
// storage is the `ticks` + `tickBitmap` mappings nested INSIDE the
// per-pool `Pool.State` struct value (`_pools[PoolId]` at top-level slot 6).
// See `docs/architecture/v4_poolmanager_storage_layout.md` for the full
// derivation (sourced from the verified vendored V4-core `PoolManager.sol` +
// `forge inspect`).
//
// Slot math (all `abi.encode` = 32-byte words, keys sign-extended):
//   S_state      = keccak256(poolId || uint256(6))
//   ticks_base   = S_state + 4   (nested mapping base within Pool.State)
//   bitmap_base  = S_state + 5
//   TickInfo[t]  = keccak256(int256(t)  || ticks_base)
//   BitmapWord[w]= keccak256(int256(w) || bitmap_base)
// The TickInfo struct's first slot packs `liquidityGross`(low 128) +
// `liquidityNet`(high 128, int128 sign-extended). A slot for an
// uninitialized tick reads as `bytes32(0)` (gross=0, net=0) — the V4 analog
// of V3's `ticks()`-returns-zeros, so the ghost-row catch works identically.

/// Top-level base storage slot of `_pools: mapping(PoolId => Pool.State)`
/// in the deployed V4 `PoolManager` (slot 6 — behind `Owned`/`ProtocolFees`/
/// `ERC6909` ancestors; see the layout doc).
const V4_POOLS_BASE_SLOT: u64 = 6;
/// Relative slot offset of the `ticks` mapping within `Pool.State`.
const V4_TICKS_MAPPING_OFFSET: u64 = 4;
/// Relative slot offset of the `tickBitmap` mapping within `Pool.State`.
const V4_BITMAP_MAPPING_OFFSET: u64 = 5;
/// `keccak256("extsload(bytes32[])")[0..4]` = `0xdbd035ff`.
const EXTSLOAD_BATCH_SELECTOR: [u8; 4] = [0xdb, 0xd0, 0x35, 0xff];

/// Compute `S_state = keccak256(abi.encode(poolId, uint256(6)))` — the
/// `Pool.State` value base for `pool_id` (the on-chain `PoolId` bytes32).
#[expect(clippy::cast_possible_truncation)] // V4_POOLS_BASE_SLOT == 6, fits u8
fn v4_state_base_slot(pool_id: B256) -> U256 {
    let mut buf = [0u8; 64];
    buf[0..32].copy_from_slice(pool_id.as_slice());
    // uint256(V4_POOLS_BASE_SLOT) — 32 big-endian bytes.
    buf[63] = V4_POOLS_BASE_SLOT as u8;
    let hash = keccak256(buf);
    U256::from_be_bytes(hash.0)
}

/// Compute a nested-mapping value slot:
/// `keccak256(abi.encode(int256(key), uint256(base)))`. `key` (an `int24`/
/// `int16` in `i32`) is two's-complement sign-extended to 32 bytes; `base` is
/// the (already-computed) mapping base slot as a 32-byte big-endian word.
fn nested_mapping_slot(key: i32, base: U256) -> B256 {
    let mut buf = [0u8; 64];
    // Sign-extend the int24/int16 key to int256 (32 bytes).
    if key < 0 {
        buf[0..28].fill(0xff);
    }
    buf[28..32].copy_from_slice(&key.to_be_bytes());
    // `base` as 32 big-endian bytes.
    buf[32..64].copy_from_slice(&base.to_be_bytes::<32>());
    keccak256(buf)
}

/// Decode a V4 `TickInfo` storage slot (packed `liquidityGross` low-128 +
/// `liquidityNet` high-128) into `(gross, net)`. Unlike V3's `ticks()` (which
/// ABI-encodes gross + net as SEPARATE 32-byte words), V4 packs both into ONE
/// storage slot: gross in the low 128 bits (bytes [16..32]), net in the high
/// 128 bits (bytes [0..16]) as an `int128` that must be sign-extended to the
/// `I256` the in-memory map carries.
fn decode_tick_from_slot(word: B256) -> (U128, i128) {
    let bytes: &[u8; 32] = word.as_ref();
    // gross = low 128 bits (bytes [16..32]); fixed-length slice → array copy is
    // infallible — no `try_into().expect` needed.
    let mut gross_low = [0u8; 16];
    gross_low.copy_from_slice(&bytes[16..32]);
    let gross = U128::from_be_bytes(gross_low);
    // net = high 128 bits (bytes [0..16]) as the on-chain int128 (LIBQKE:
    // TickInfo stores the native width, so the I256 sign extension is gone).
    let mut net_buf = [0u8; 16];
    net_buf.copy_from_slice(&bytes[0..16]);
    let net = i128::from_be_bytes(net_buf);
    (gross, net)
}

/// Decode a V4 bitmap storage slot into its raw `uint256` word.
fn decode_bitmap_from_slot(word: B256) -> U256 {
    U256::from_be_bytes::<32>(word.0)
}

/// Encode `extsload(bytes32[] slots)` calldata: `selector(4) +
/// abi_encode_params(bytes32[])`.
fn encode_extsload_call(slots: &[B256]) -> Bytes {
    let arr: Vec<DynSolValue> = slots
        .iter()
        .map(|s| DynSolValue::FixedBytes(*s, 32))
        .collect();
    let params = DynSolValue::Tuple(vec![DynSolValue::Array(arr)]);
    let mut cd = Vec::with_capacity(4 + 64 + 32 * slots.len());
    cd.extend_from_slice(&EXTSLOAD_BATCH_SELECTOR);
    cd.extend_from_slice(&params.abi_encode_params());
    Bytes::from(cd)
}

/// Decode an `extsload(bytes32[])` return (`bytes32[]`) into the slot-value
/// list, in input order. Returns `None` on a malformed payload. An empty return
/// for a non-empty call is a hard decode failure (not the revert case — a
/// revert surfaces as an `eth_call` error from the provider).
fn decode_extsload_return(data: &[u8], n: usize) -> Option<Vec<B256>> {
    if n == 0 {
        return Some(Vec::new());
    }
    let ty: DynSolType = "bytes32[]".parse().ok()?;
    let decoded = ty.abi_decode(data).ok()?;
    let DynSolValue::Array(arr) = decoded else {
        return None;
    };
    Some(
        arr.iter()
            .filter_map(|v| match v {
                DynSolValue::FixedBytes(b, 32) => Some(*b),
                _ => None,
            })
            .collect(),
    )
}

/// Decode an `eth_call`'s extsload return into the slot-value list, mapping a
/// revert / decode failure into `RunError::Provider` (a V4 extsload revert is a
/// transport/deployment fault, not a per-tick divergence — unlike V3, V4 reads
/// ALL slots in one call, so a single revert blocks the whole pool's gate).
fn decode_extsload_or_error(data: &Bytes, n: usize, pool_id: B256) -> Result<Vec<B256>, RunError> {
    decode_extsload_return(data, n).ok_or_else(|| {
        RunError::Provider(ProviderError::DecodingError {
            message: format!("v4 extsload return decode failed for pool {pool_id} ({n} slots)"),
        })
    })
}

/// Verify a V4 pool's entire in-memory liquidity map against on-chain truth at
/// `block_number` (Full per-pool verification). Issues ONE `extsload(bytes32[])`
/// call covering every in-memory tick slot + bitmap-word slot (cheaper than the
/// V3 per-tick path — a single round trip per pool). Returns the named
/// divergence list (empty = GREEN); a non-empty list from the pre-commit gate
/// rolls back the chunk's transaction + does NOT advance `last_update_block`.
///
/// `pool_id` is the on-chain `PoolId` (`bytes32`); `pool_manager_address` is the
/// deployed V4 `PoolManager` singleton.
///
/// # Errors
///
/// Returns [`RunError::Provider`] if the `eth_call` fails or the extsload
/// return is malformed (a deployment without the batched `extsload(bytes32[])`
/// overload, or a transport fault). An uninitialized on-chain tick reads as
/// `bytes32(0)` (NOT an error) — it surfaces as a divergence if the in-memory
/// map has non-zero liquidity for that tick.
pub async fn verify_v4_liquidity_map_on_chain(
    provider: &AlloyProvider,
    pool_manager_address: Address,
    pool_id: B256,
    computed: &ComputedLiquidityUpdate,
    block_number: u64,
) -> Result<Vec<LiquidityDivergence>, RunError> {
    let ticks: Vec<i32> = computed.tick_data.keys().copied().collect();
    let words: Vec<i32> = computed.tick_bitmap.keys().copied().collect();

    let state_base = v4_state_base_slot(pool_id);
    let ticks_base = state_base + U256::from(V4_TICKS_MAPPING_OFFSET);
    let bitmap_base = state_base + U256::from(V4_BITMAP_MAPPING_OFFSET);

    let mut slots: Vec<B256> = Vec::with_capacity(ticks.len() + words.len());
    for &tick in &ticks {
        slots.push(nested_mapping_slot(tick, ticks_base));
    }
    for &word in &words {
        slots.push(nested_mapping_slot(word, bitmap_base));
    }

    if slots.is_empty() {
        return Ok(Vec::new()); // empty pool — nothing to verify
    }

    let calldata = encode_extsload_call(&slots);
    let return_data = provider
        .eth_call(&pool_manager_address, calldata, Some(block_number))
        .await?;
    let slot_values = decode_extsload_or_error(&return_data, slots.len(), pool_id)?;

    let mut divergences = Vec::new();
    for (i, &tick) in ticks.iter().enumerate() {
        let (gross, net) = decode_tick_from_slot(slot_values[i]);
        let expected = &computed.tick_data[&tick];
        divergences.extend(compare_tick(tick, expected, Some((gross, net))));
    }
    for (j, &word) in words.iter().enumerate() {
        let bitmap = decode_bitmap_from_slot(slot_values[ticks.len() + j]);
        let expected = computed.tick_bitmap[&word].bitmap;
        divergences.extend(compare_bitmap_word(word, expected, Some(bitmap)));
    }
    Ok(divergences)
}

// ── market-wide pre-commit full verification ────────────────────────────

/// The context the pre-commit FULL (market-wide) verification runs against.
/// Unlike [`crate::run::VerifyCtx`] (which verifies a single chunk's
/// computed deltas for touched pools), this loads ALL in-scope pools'
/// COMMITTED liquidity maps from the transaction's view of the DB and
/// compares each against on-chain truth at `block_number`.
///
/// Used at the interval-boundary crossing + run-completion gates.
pub struct FullVerifyCtx<'a> {
    /// The HTTP RPC provider for on-chain reads.
    pub provider: &'a AlloyProvider,
    /// The runtime to `block_on` the async verify calls.
    pub rt: &'a tokio::runtime::Runtime,
    /// The block number to read on-chain truth at (= the chunk's `chunk_end`).
    pub block_number: u64,
    /// The chain id the run targets (selects the in-scope V3+V4 pool set).
    pub chain_id: i64,
    /// The V4 `pool_manager_chain` (the `chain` column the V4 pool lookup uses
    /// — mirrors `ChunkInputs::pool_manager_chain`).
    pub pool_manager_chain: i64,
}

/// Run the market-wide pre-commit verification: every in-scope V3 + V4 pool's
/// committed liquidity map is loaded from `conn` (the chunk's transaction,
/// so uncommitted writes are visible) and compared against on-chain truth at
/// `ctx.block_number`. The first diverging pool yields a
/// [`RunError::Verification`] (the caller drops `tx` to roll back so
/// `last_update_block` does NOT advance).
///
/// Returns `Ok(())` on GREEN (all pools match). Empty-pool chains are GREEN.
///
/// # Errors
///
/// Returns [`RunError::Db`] on a DB read failure, [`RunError::Provider`] on
/// an RPC failure (incl. a missing V4 `PoolManager` address), or
/// [`RunError::Verification`] on the first diverging pool.
pub fn verify_all_pools_committed_on_conn(
    conn: &rusqlite::Connection,
    ctx: &FullVerifyCtx<'_>,
) -> Result<(), RunError> {
    use degenbot_db::DegenbotDb;

    // V3 — every in-scope V3 pool's committed map.
    let v3_addresses = DegenbotDb::fetch_v3_pool_addresses_on_conn(conn, ctx.chain_id)?;
    for addr in &v3_addresses {
        let addr_str = addr.to_checksum(None);
        let Some(state) =
            DegenbotDb::fetch_v3_pool_update_state_on_conn(conn, ctx.chain_id, &addr_str)?
        else {
            // A pool row with no V3 state yet (created but never applied) —
            // nothing committed to verify against on-chain truth. Skip.
            continue;
        };
        let (tick_bitmap, tick_data) =
            DegenbotDb::fetch_v3_liquidity_map_on_conn(conn, state.pool_id)?;
        let computed = ComputedLiquidityUpdate {
            pool_id: state.pool_id,
            tick_spacing: state.tick_spacing,
            tick_data,
            tick_bitmap,
            last_event: None,
        };
        let divergences = ctx.rt.block_on(verify_v3_liquidity_map_on_chain(
            ctx.provider,
            *addr,
            &computed,
            ctx.block_number,
        ))?;
        if !divergences.is_empty() {
            return Err(RunError::Verification {
                pool: addr_str,
                block_number: ctx.block_number,
                divergences,
            });
        }
    }

    // V4 — every in-scope V4 pool's committed map.
    // The PoolManager address comes straight from the DB query (via the
    // `managed_pools.manager_id` → `pool_managers.address` FK), NOT from a
    // chunk-local in-memory map — so pools created in any earlier chunk (with a
    // `pool_hash` row but no liquidity event in the current chunk) are covered.
    let v4_hashes = DegenbotDb::fetch_v4_pool_hashes_on_conn(conn, ctx.chain_id)?;
    for (pool_hash, pool_manager_address) in &v4_hashes {
        let Some(state) = DegenbotDb::fetch_v4_pool_update_state_on_conn(
            conn,
            pool_hash,
            ctx.pool_manager_chain,
        )?
        else {
            continue;
        };
        let (tick_bitmap, tick_data) =
            DegenbotDb::fetch_v4_liquidity_map_on_conn(conn, state.pool_id)?;
        let computed = ComputedLiquidityUpdate {
            pool_id: state.pool_id,
            tick_spacing: state.tick_spacing,
            tick_data,
            tick_bitmap,
            last_event: None,
        };
        let pool_id =
            B256::from_str(pool_hash.strip_prefix("0x").unwrap_or(pool_hash)).map_err(|e| {
                ProviderError::DecodingError {
                    message: format!("v4 full-verify: bad pool_hash {pool_hash:?}: {e}"),
                }
            })?;
        let divergences = ctx.rt.block_on(verify_v4_liquidity_map_on_chain(
            ctx.provider,
            *pool_manager_address,
            pool_id,
            &computed,
            ctx.block_number,
        ))?;
        if !divergences.is_empty() {
            return Err(RunError::Verification {
                pool: pool_hash.clone(),
                block_number: ctx.block_number,
                divergences,
            });
        }
    }

    Ok(())
}

// ─────────── internal ABI-encode reference for tests (independent of the
// decoder under test) ───────────

// ── verification-policy predicates (pure logic, no RPC) ───────────────

/// Whether a full (market-wide) verification should run at a block-number
/// interval boundary for the chunk `[working_start, chunk_end]`.
///
/// Fires when `interval` is `Some(n)` and the chunk CROSSES or LANDS-ON a
/// multiple of `n`: i.e. the multiple-of-`n` boundary in `[working_start,
/// chunk_end]` — equivalently `floor((working_start - 1) / n) !=
/// floor(chunk_end / n)`. A chunk larger than `n` (`chunk_size` > n) still
/// fires because it spans a multiple. `interval = None` never fires.
///
/// Edge cases: `chunk_end == 0` → false (no block processed); `n == 0` is
/// the caller's bug (the CLI/clamp it to >= 1) but is treated as never-fires
/// to avoid a divide-by-zero panic.
pub(crate) fn should_run_full_verify_at_interval(
    working_start: u64,
    chunk_end: u64,
    interval: Option<u64>,
) -> bool {
    let Some(n) = interval else {
        return false;
    };
    if n == 0 {
        return false;
    }
    if chunk_end == 0 {
        return false;
    }
    let before = working_start.saturating_sub(1) / n;
    let after = chunk_end / n;
    before != after
}

/// Whether `chunk_end` is the run's final block: `chunk_end >= last_block`
/// OR `max_chunks_hit` (the loop broke because `max_chunks` was reached).
// wired into the chunk loop in the YWEUIR task.
#[cfg(test)] // only exercised by tests today; move back to production when the YWEUIR task wires it
pub(crate) fn is_final_chunk(chunk_end: u64, last_block: u64, max_chunks_hit: bool) -> bool {
    max_chunks_hit || chunk_end >= last_block
}

#[cfg(test)]
/// Independently ABI-encode a V3 `ticks()` return tuple — built with a
/// DIFFERENT encoder path (`DynSolValue::abi_encode`) than the home's
/// `degenbot_rpc::abi::decode_tick_data`, so the home decoder's correctness
/// is checked against a second encoder. Used by the V3 verify integration
/// test (the home's own decode is independently-oracle-tested in
/// `degenbot-rpc::abi::tests`).
#[cfg(test)]
fn encode_ticks_return_ref(gross: u128, net: i128) -> Vec<u8> {
    #[expect(clippy::unwrap_used)] // i128 always fits in I256
    let val = DynSolValue::Tuple(vec![
        DynSolValue::Uint(U256::from(gross), 128),
        DynSolValue::Int(I256::try_from(net).unwrap(), 128),
        DynSolValue::Uint(U256::ZERO, 256),
        DynSolValue::Uint(U256::ZERO, 256),
        DynSolValue::Int(I256::ZERO, 56),
        DynSolValue::Uint(U256::ZERO, 160),
        DynSolValue::Uint(U256::ZERO, 32),
        DynSolValue::Bool(false),
    ]);
    val.abi_encode()
}

#[cfg(test)]
fn encode_uint256_return_ref(word: U256) -> Vec<u8> {
    DynSolValue::Uint(word, 256).abi_encode()
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use alloy::primitives::hex_literal::hex;
    use alloy::primitives::{U128, U256};
    use degenbot_db::ApplyBitmapAtWord as BitmapAtWord;
    use hashbrown::HashMap;

    // ── compare: the divergence surface ──

    fn tick(gross: u128, net: i128) -> ApplyLiquidityAtTick {
        ApplyLiquidityAtTick {
            liquidity_gross: U128::from(gross),
            liquidity_net: I256::try_from(net).unwrap(),
            block: 0,
        }
    }

    #[test]
    fn compare_tick_match_is_empty() {
        let expected = tick(500, -100);
        assert!(compare_tick(7, &expected, Some((U128::from(500u64), -100i128))).is_empty());
    }

    #[test]
    fn compare_tick_gross_differs_is_ghost_row_when_actual_zero() {
        // The 6SRJRL corruption shape: in-memory gross 500, on-chain drained
        // (gross 0) → TickGross with actual == 0.
        let expected = tick(500, 0);
        let d = compare_tick(12345, &expected, Some((U128::ZERO, 0)));
        assert_eq!(
            d,
            vec![LiquidityDivergence::TickGross {
                tick: 12345,
                expected: U128::from(500u64),
                actual: U128::ZERO,
            }]
        );
    }

    #[test]
    fn compare_tick_revert_emits_tickcallreverted() {
        let expected = tick(100, 0);
        let d = compare_tick(1, &expected, None);
        assert_eq!(d, vec![LiquidityDivergence::TickCallReverted { tick: 1 }]);
    }

    #[test]
    fn compare_bitmap_word_match_and_differ() {
        let w = U256::from(0xdeadu64);
        assert!(compare_bitmap_word(3, w, Some(w)).is_empty());
        let d = compare_bitmap_word(3, w, Some(U256::from(0xbeefu64)));
        assert_eq!(
            d,
            vec![LiquidityDivergence::BitmapWord {
                word: 3,
                expected: w,
                actual: U256::from(0xbeefu64),
            }]
        );
    }

    // ── end-to-end over a fake MulticallResult vector (no RPC) ──

    fn mult_result(payload: Vec<u8>) -> MulticallResult {
        MulticallResult {
            success: true,
            return_data: Bytes::from(payload),
        }
    }

    #[test]
    fn compare_results_drained_pool_catches_ghost_row() {
        // In-memory: one tick (12345, gross 500) + its bitmap word set.
        let mut tick_data = HashMap::new();
        tick_data.insert(12345, tick(500, 0));
        let mut tick_bitmap = HashMap::new();
        tick_bitmap.insert(
            0,
            BitmapAtWord {
                bitmap: U256::from(1u64),
                block: 0,
            },
        );
        let computed = ComputedLiquidityUpdate {
            pool_id: 1,
            tick_spacing: 1,
            tick_data,
            tick_bitmap,
            last_event: None,
        };
        // On-chain truth at chunk_end: tick 12345 drained (gross 0), bitmap
        // word 0 = 0 (not initialized).
        let results = [
            mult_result(encode_ticks_return_ref(0, 0)),
            mult_result(encode_uint256_return_ref(U256::ZERO)),
        ];
        // Re-run the compare path manually (mirrors verify_v3's loop).
        let mut divs = Vec::new();
        divs.extend(compare_tick(
            12345,
            &computed.tick_data[&12345],
            decode_ticks_result(&results[0]),
        ));
        divs.extend(compare_bitmap_word(
            0,
            computed.tick_bitmap[&0].bitmap,
            decode_bitmap_result(&results[1]),
        ));
        assert!(divs.contains(&LiquidityDivergence::TickGross {
            tick: 12345,
            expected: U128::from(500u64),
            actual: U128::ZERO,
        }));
        assert!(divs.contains(&LiquidityDivergence::BitmapWord {
            word: 0,
            expected: U256::from(1u64),
            actual: U256::ZERO,
        }));
    }

    // ════════ V4 tests ════════
    //
    // The reference slot vectors below were computed with an INDEPENDENT
    // `cast keccak` oracle (Python subshell) — a DIFFERENT keccak than this
    // crate's alloy `keccak256`. They are the source of truth the slot-derive
    // helpers must reproduce byte-for-byte. The pool_id = 0x00..01 sample
    // matches `tests/fixtures/v4_slot_vectors.json`.

    fn pool_id_one() -> B256 {
        B256::with_last_byte(1)
    }

    #[test]
    fn v4_state_base_slot_matches_cast_oracle() {
        // S_state for pool_id=0x..01 = 0x3e5f...6a31 (cast keccak).
        let expected = B256::from(hex!(
            "3e5fec24aa4dc4e5aee2e025e51e1392c72a2500577559fae9665c6d52bd6a31"
        ));
        let got = v4_state_base_slot(pool_id_one());
        assert_eq!(B256::from(got.to_be_bytes::<32>()), expected);
    }

    #[test]
    fn v4_tick_slot_matches_cast_oracle_positive_negative_and_zero() {
        let base = v4_state_base_slot(pool_id_one());
        let ticks_base = base + U256::from(V4_TICKS_MAPPING_OFFSET);
        // (tick, expected-slot) triplets from cast keccak.
        let cases = [
            (
                -887_220i32,
                "582bff295aa51422943b629229b543deed3e2a2f638aa846562d69521ecd2381",
            ),
            (
                -100,
                "0f335c0565e65bd65f7ae80502d13da8188bd2e85f79878450decf77501242af",
            ),
            (
                -1,
                "e144d190ab821ad2b3e01b2e5b215edb2324c5cd17330425e5274158d65ffe6e",
            ),
            (
                0,
                "071baefdc11a1a736f835256c2bc394c68d5ec5a0621b837dd5a1614ef57dbb2",
            ),
            (
                1,
                "deecf616af14ecf5af18296eeb4d0b420579c78910805f488c52af79ea8c286c",
            ),
            (
                100,
                "ea48cb33783a9d6f27628e3b906b0886c2cf1db3671b72db6ca71e543673be95",
            ),
            (
                887_220,
                "b28723db135d0772b80eb995ca4813d602baa17314fabbef5305aa54f792aeee",
            ),
        ];
        for (tick, expected_hex) in cases {
            let got = nested_mapping_slot(tick, ticks_base);
            let expected = B256::from_slice(&alloy::hex::decode(expected_hex).unwrap());
            assert_eq!(got, expected, "tick {tick}");
        }
    }

    #[test]
    fn v4_bitmap_word_slot_matches_cast_oracle() {
        let base = v4_state_base_slot(pool_id_one());
        let bitmap_base = base + U256::from(V4_BITMAP_MAPPING_OFFSET);
        let cases = [
            (
                -2047i32,
                "cb4199c2cdae663d28d9045a8c7947aaaef7f24e869f4646519b7fd0bb845750",
            ),
            (
                -1,
                "a9f531d32a7801e5fd7761efcdfabdd15b41ecb7a9d0fb9e79c7aa2304a844c8",
            ),
            (
                0,
                "6395bb2e70acf609fb69050da9fdfeb703963b9ae70f663b6a5d53a413ed6684",
            ),
            (
                1,
                "62806c3fa03ed76f2aa390bba57ea4df2426510079b444c05bb392b554bf4cee",
            ),
            (
                5,
                "5aeb90dcb24f09b6daf7ccac04db0e1f868602ca6deca7c5b13dd4bd40c58a92",
            ),
            (
                2047,
                "450cbb5a1ffffbb1130cffb3ff3ff3683f1cf1c7daaecf31c32e5c0738833166",
            ),
        ];
        for (word, expected_hex) in cases {
            let got = nested_mapping_slot(word, bitmap_base);
            let expected = B256::from_slice(&alloy::hex::decode(expected_hex).unwrap());
            assert_eq!(got, expected, "word {word}");
        }
    }

    #[test]
    fn extsload_selector_is_dbd035ff() {
        assert_eq!(alloy::hex::encode(EXTSLOAD_BATCH_SELECTOR), "dbd035ff");
    }

    #[test]
    fn encode_extsload_call_round_trips_through_decode() {
        // Encode a 3-slot call; the decoder under test (`decode_extsload_return`)
        // consumes the return format `length + elements`. Encode the same slots
        // in return format independently + confirm the decoder reproduces them.
        let slots = vec![
            B256::from(hex!(
                "071baefdc11a1a736f835256c2bc394c68d5ec5a0621b837dd5a1614ef57dbb2"
            )),
            B256::from(hex!(
                "0f335c0565e65bd65f7ae80502d13da8188bd2e85f79878450decf77501242af"
            )),
            B256::from(hex!(
                "6395bb2e70acf609fb69050da9fdfeb703963b9ae70f663b6a5d53a413ed6684"
            )),
        ];
        let cd = encode_extsload_call(&slots);
        assert_eq!(&cd[..4], &EXTSLOAD_BATCH_SELECTOR);
        // Independently build the return-format payload + decode it.
        let return_payload = DynSolValue::Array(
            slots
                .iter()
                .map(|s| DynSolValue::FixedBytes(*s, 32))
                .collect(),
        )
        .abi_encode();
        let decoded = decode_extsload_return(&return_payload, 3).expect("decode");
        assert_eq!(decoded, slots);
    }

    #[test]
    fn decode_extsload_return_matches_ref_encoder() {
        // Independently encode a bytes32[] return.
        let vals = [
            B256::repeat_byte(0xab),
            B256::ZERO,
            B256::with_last_byte(0xff),
        ];
        let encoded = DynSolValue::Array(
            vals.iter()
                .map(|b| DynSolValue::FixedBytes(*b, 32))
                .collect(),
        )
        .abi_encode();
        let decoded = decode_extsload_return(&encoded, 3).expect("decode");
        assert_eq!(decoded, vals.to_vec());
    }

    #[test]
    fn decode_extsload_return_empty_is_ok_short_is_none() {
        assert!(decode_extsload_return(&[], 0).unwrap().is_empty());
        assert!(decode_extsload_return(&[0u8; 5], 1).is_none());
    }

    #[test]
    fn decode_tick_from_slot_packs_gross_low_net_high() {
        // gross = 0x00..00_500 (low 128), net = 0xff..ff_9c (int128 -100, high
        // 128, sign-extended).
        let mut word_bytes = [0u8; 32];
        // net high: bytes[0..16] — int128(-100) = 0xff..ff9c (16 bytes).
        word_bytes[0..16].fill(0xff);
        word_bytes[15] = 0x9c;
        // gross low: bytes[16..32] — uint128(500) = 0x00..01F4.
        word_bytes[31] = 0xf4;
        word_bytes[30] = 0x01;
        let word = B256::from(word_bytes);
        let (gross, net) = decode_tick_from_slot(word);
        assert_eq!(gross, U128::from(500u64));
        assert_eq!(net, -100i128);
    }

    #[test]
    fn decode_bitmap_from_slot_is_raw_uint256() {
        // 0xdeadbeef in the LOW 4 bytes of a 32-byte big-endian word.
        let mut word = [0u8; 32];
        word[28..32].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(
            decode_bitmap_from_slot(B256::from(word)),
            U256::from(0xdead_beef_u64)
        );
    }

    #[test]
    fn v4_compare_drained_pool_catches_ghost_row() {
        // In-memory: one tick (12345, gross 500) + its bitmap word set.
        let mut tick_data = HashMap::new();
        tick_data.insert(12345, tick(500, 0));
        let mut tick_bitmap = HashMap::new();
        tick_bitmap.insert(
            0,
            BitmapAtWord {
                bitmap: U256::from(1u64),
                block: 0,
            },
        );
        let computed = ComputedLiquidityUpdate {
            pool_id: 1,
            tick_spacing: 1,
            tick_data,
            tick_bitmap,
            last_event: None,
        };
        // On-chain truth: tick slot = bytes32(0) (drained), bitmap slot = 0.
        let slot_values = [B256::ZERO, B256::ZERO];
        let mut divs = Vec::new();
        let (gross, net) = decode_tick_from_slot(slot_values[0]);
        divs.extend(compare_tick(
            12345,
            &computed.tick_data[&12345],
            Some((gross, net)),
        ));
        let bitmap = decode_bitmap_from_slot(slot_values[1]);
        divs.extend(compare_bitmap_word(
            0,
            computed.tick_bitmap[&0].bitmap,
            Some(bitmap),
        ));
        assert!(divs.contains(&LiquidityDivergence::TickGross {
            tick: 12345,
            expected: U128::from(500u64),
            actual: U128::ZERO,
        }));
        assert!(divs.contains(&LiquidityDivergence::BitmapWord {
            word: 0,
            expected: U256::from(1u64),
            actual: U256::ZERO,
        }));
    }

    // ── verification-policy predicates ─────────────────────────────────

    #[test]
    fn interval_verify_none_interval_never_fires() {
        assert!(!should_run_full_verify_at_interval(1, 999, None));
        assert!(!should_run_full_verify_at_interval(1, 1_000_000, None));
    }

    #[test]
    fn interval_verify_exact_multiple_lands() {
        // chunk ending exactly on a multiple fires.
        assert!(should_run_full_verify_at_interval(
            1,
            1_000_000,
            Some(1_000_000)
        ));
        assert!(should_run_full_verify_at_interval(
            990_001,
            1_000_000,
            Some(1_000_000)
        ));
    }

    #[test]
    fn interval_verify_chunk_crosses_multiple() {
        // chunk_size > n: a chunk spanning a multiple fires.
        assert!(should_run_full_verify_at_interval(
            999_999,
            1_000_001,
            Some(1_000_000)
        ));
        // a chunk that starts before + ends after a multiple fires.
        assert!(should_run_full_verify_at_interval(
            500_000,
            1_500_000,
            Some(1_000_000)
        ));
    }

    #[test]
    fn interval_verify_sub_interval_no_fire() {
        assert!(!should_run_full_verify_at_interval(
            1,
            999_999,
            Some(1_000_000)
        ));
        assert!(!should_run_full_verify_at_interval(
            100,
            200,
            Some(1_000_000)
        ));
        // a whole run shorter than the interval fires only at completion (not here).
        assert!(!should_run_full_verify_at_interval(
            1,
            5_000,
            Some(1_000_000)
        ));
    }

    #[test]
    fn interval_verify_n_one_fires_every_chunk() {
        // n=1: every block is a multiple → fires every chunk.
        assert!(should_run_full_verify_at_interval(1, 1, Some(1)));
        assert!(should_run_full_verify_at_interval(100, 200, Some(1)));
    }

    #[test]
    fn interval_verify_chunk_end_zero_does_not_fire() {
        assert!(!should_run_full_verify_at_interval(0, 0, Some(1_000_000)));
        assert!(!should_run_full_verify_at_interval(1, 0, Some(1_000_000)));
    }

    #[test]
    fn interval_verify_n_zero_does_not_panic() {
        // n=0 is the caller's bug; must not divide-by-zero panic.
        assert!(!should_run_full_verify_at_interval(1, 100, Some(0)));
    }

    #[test]
    fn is_final_chunk_true_when_last_block_reached() {
        assert!(is_final_chunk(1_000_000, 1_000_000, false));
        assert!(is_final_chunk(1_000_005, 1_000_000, false));
    }

    #[test]
    fn is_final_chunk_true_when_max_chunks_hit() {
        // max_chunks hit before last_block — still final.
        assert!(is_final_chunk(50_000, 1_000_000, true));
    }

    #[test]
    fn is_final_chunk_false_mid_run() {
        assert!(!is_final_chunk(10_000, 1_000_000, false));
        assert!(!is_final_chunk(990_000, 1_000_000, false));
    }

    // ── market-wide full verify: empty chain is GREEN (no RPC touched) ────

    fn full_verify_ctx<'a>(
        provider: &'a AlloyProvider,
        rt: &'a tokio::runtime::Runtime,
    ) -> FullVerifyCtx<'a> {
        FullVerifyCtx {
            provider,
            rt,
            block_number: 100,
            chain_id: 1,
            pool_manager_chain: 1,
        }
    }

    #[test]
    fn verify_all_pools_committed_on_conn_empty_chain_is_green() {
        // An empty chain has no in-scope V3 or V4 pools → the full verify is
        // trivially GREEN + must not touch the provider (no RPC at all).
        use degenbot_db::DegenbotDb;
        use std::path::Path;
        let (db, _state) = DegenbotDb::open_for_writes(Path::new(":memory:")).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let provider = rt
            .block_on(AlloyProvider::new("http://127.0.0.1:9/", 5))
            .unwrap();
        let ctx = full_verify_ctx(&provider, &rt);
        let conn = db.lock();
        assert!(verify_all_pools_committed_on_conn(&conn, &ctx).is_ok());
    }

    /// Reproduces the production crash: a V4 pool with a committed row but NO
    /// liquidity event in the current chunk (so the chunk-local
    /// `v4_manager_addresses` map does NOT contain it). The `PoolManager` address
    /// must be resolved from the DB via the `manager_id` FK, NOT from the
    /// chunk map. The pool has committed tick positions, so the verify reaches
    /// the on-chain call — proving the address was resolved. The dummy
    /// provider (127.0.0.1:9) will fail with a connection error, which is the
    /// GREEN assertion: the error is an RPC failure, NOT the
    /// "no `PoolManager` address" crash that masked the real bug.
    #[test]
    fn verify_v4_pool_manager_address_resolved_from_db_not_chunk_map() {
        use degenbot_db::DegenbotDb;
        use std::path::Path;

        let (db, _state) = DegenbotDb::open_for_writes(Path::new(":memory:")).unwrap();
        let conn = db.lock();
        // FK enforcement is ON for test connections; the minimal V4 graph below
        // intentionally omits the `erc20_tokens` / `pools` parent rows the FKs
        // reference (production runs with FK off). Lift it for the fixture.
        conn.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        // Insert a minimal V4 pool graph. `address` is the PoolManager. The
        // chunk-local map (empty below) deliberately omits this pool_hash —
        // the verify must read the address from `pool_managers.address` via
        // the `managed_pools.manager_id` FK instead.
        conn.execute(
            "INSERT INTO exchanges (id, chain_id, name, active, factory) \
             VALUES (1, 1, 'test-v4', 1, '0x0000000000000000000000000000000000000001')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO pool_managers (id, address, chain, kind, state_view, exchange_id) \
             VALUES (1, '0x498581fF718922c3f8e6A244956aF099B2652b2b', 1, 'uniswap_v4', NULL, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO managed_pools (id, kind, manager_id) VALUES (1, 'uniswap_v4', 1)",
            [],
        )
        .unwrap();
        // tick_spacing=1, committed markers set so `state` is `Some`.
        conn.execute(
            "INSERT INTO uniswap_v4_pools \
             (managed_pool_id, pool_hash, hooks, currency0_id, currency1_id, \
              fee_currency0, fee_currency1, fee_denominator, tick_spacing, \
              liquidity_update_block, liquidity_update_log_index) \
             VALUES (1, '0x96d4b53a38337a5733179751781178a2613306063c511b78cd02684739288c0a', \
                     '0x0000000000000000000000000000000000000000', 0, 0, 0, 0, 0, 1, 100, 0)",
            [],
        )
        .unwrap();
        // A committed V4 tick position → the verify reaches the on-chain call
        // (non-empty map), proving the PoolManager address was resolved + used.
        // V4 uses its own `managed_pool_*` tables (not the V3 ones).
        conn.execute(
            "INSERT INTO managed_pool_liquidity_positions \
             (id, managed_pool_id, tick, liquidity_net, liquidity_gross) \
             VALUES (1, 1, 0, '0', '1')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO managed_pool_initialization_maps \
             (id, managed_pool_id, word, bitmap) VALUES (1, 1, 0, '1')",
            [],
        )
        .unwrap();
        drop(conn);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let provider = rt
            .block_on(AlloyProvider::new("http://127.0.0.1:9/", 5))
            .unwrap();
        // The bug condition: the pool had NO liquidity event in the current
        // chunk, so the chunk-local map would be empty. The verify must resolve
        // the PoolManager address from the DB via the `manager_id` FK instead.
        let ctx = full_verify_ctx(&provider, &rt);
        let conn = db.lock();
        let result = verify_all_pools_committed_on_conn(&conn, &ctx);
        // GREEN: the address is resolved from the DB → the verify proceeds to
        // the (dummy, connection-refused) RPC call. The error must be an RPC
        // failure, NOT the "no PoolManager address" crash.
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            !msg.contains("no PoolManager address"),
            "PoolManager address must be resolved from the DB via manager_id FK, \
             not the chunk-local map. Got: {msg}"
        );
    }
}
