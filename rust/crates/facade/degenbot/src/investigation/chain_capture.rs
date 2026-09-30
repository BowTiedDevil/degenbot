//! Chain-sourced state capture at a fixed block — the tick snapshot a pool
//! actually carries at `target_block`, without consulting the tracker database.
//!
//! Two retrieval vehicles for initialized ticks, both word-oriented and batched
//! through [`degenbot_rpc::multicall3`] `aggregate3`:
//!
//! - **TickLens** ([`ITickLens::getPopulatedTicksInWord`]): one call per bitmap
//!   word returns every initialized tick in the word with its
//!   (`liquidity_net`, `liquidity_gross`) — no waterfall.
//! - **Direct pool reads** (`tickBitmap(word)` + `ticks(tick)`), the lens-free
//!   path over the pool itself (and the natural cross-check of a lens).
//!
//! V4 pools have no periphery lens: their word scan rides the pool's StateView
//! (`getTickBitmap` / `getTickLiquidity`) — the V4-native twin of the lens.
//!
//! Scan math mirrors `TickLens.sol` exactly:
//! `initialized tick = ((word << 8) + bit) * tick_spacing`, with words covering
//! the usable range [`tick_math::min_usable_tick`..=`max_usable_tick`].

use std::collections::BTreeMap;

use alloy::primitives::{Address, Bytes, U256};
use alloy::sol_types::SolCall;
use degenbot_math::cl::tick_math::{max_usable_tick, min_usable_tick};
use degenbot_rpc::multicall3::{
    decode_aggregate3_results, encode_aggregate3, encode_try_aggregate, MulticallResult,
    MULTICALL3_ADDRESS, MULTICALL3_BATCH_SIZE,
};
use degenbot_rpc::provider::AlloyProvider;

use crate::investigation::capture::FetchedState;

alloy::sol! {
    /// The canonical Uniswap v3-periphery tick lens (interface verbatim from
    /// `Uniswap/v3-periphery` `contracts/interfaces/ITickLens.sol`).
    interface ITickLens {
        struct PopulatedTick {
            int24 tick;
            int128 liquidityNet;
            uint128 liquidityGross;
        }

        function getPopulatedTicksInWord(address pool, int16 tickBitmapIndex)
            external
            view
            returns (PopulatedTick[] memory populatedTicks);
    }
}

/// How initialized ticks are read for a V3-family pool at the capture block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickScan {
    /// `ITickLens.getPopulatedTicksInWord` against this lens deployment.
    Lens(Address),
    /// `tickBitmap(word)` + `ticks(tick)` against the pool itself. Costs one
    /// call per initialized tick but needs no external contract.
    DirectPool,
}

/// Safe width conversion for ticks: every usable tick fits `int24` by
/// construction (|tick| <= 887272 < 2^23).
fn i24(v: i32) -> Result<alloy::primitives::aliases::I24, String> {
    v.to_string()
        .parse()
        .map_err(|e| format!("tick {v} does not fit int24: {e}"))
}

/// Floor division by 256 on a signed tick index — bitmap words round DOWN for
/// negative indices (truncating Rust division does not).
const fn word_of_index(index: i32) -> i16 {
    ((index - index.rem_euclid(256)) / 256) as i16
}

/// The bitmap word and bit that index an initialized tick — the inverse of
/// `TickLens`'s `((word << 8) + bit) * spacing`.
pub fn word_and_bit(tick: i32, tick_spacing: i32) -> (i16, u8) {
    let index = tick / tick_spacing;
    let word = word_of_index(index);
    let bit = (index - i32::from(word) * 256) as u8;
    (word, bit)
}

/// Every bitmap word that can hold initialized ticks for this spacing.
pub fn word_positions(tick_spacing: i32) -> std::ops::RangeInclusive<i16> {
    let min_index = min_usable_tick(tick_spacing) / tick_spacing;
    let max_index = max_usable_tick(tick_spacing) / tick_spacing;
    word_of_index(min_index)..=word_of_index(max_index)
}

/// The initialized ticks a bitmap word holds (mirrors `TickLens`'s inner loop).
pub fn ticks_from_bitmap_word(word: i16, bitmap: U256, tick_spacing: i32) -> Vec<i32> {
    let mut ticks = Vec::new();
    for bit in 0..256usize {
        if bitmap.bit(bit) {
            ticks.push((i32::from(word) * 256 + bit as i32) * tick_spacing);
        }
    }
    ticks
}

/// How sub-calls reach the node once batching is underway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BatchMode {
    /// Multicall3-compatible `aggregate3` (allowFailure per item).
    Aggregate3,
    /// Multicall2 `tryAggregate` (requireSuccess = false) — same return shape.
    TryAggregate,
    /// No batch contract usable: one `eth_call` per sub-call.
    Sequential,
}

/// Run `calls` in batches at `block`, preserving order. The batch vehicle is a
/// capability ladder — `aggregate3`, then Multicall2 `tryAggregate` (some nodes
/// carry only a Multicall2 deployment at the batch address), then one
/// `eth_call` per sub-call — chosen once per sequence and announced. A sub-call
/// failure is reported per item (the lens returns empty words rather than
/// reverting, so a failure is a real fault — surfaced, never elided).
async fn aggregate3(
    provider: &AlloyProvider,
    calls: &[(Address, Bytes)],
    block: u64,
) -> Result<Vec<MulticallResult>, String> {
    let mut out = Vec::with_capacity(calls.len());
    let mut mode: Option<BatchMode> = None;
    for chunk in calls.chunks(MULTICALL3_BATCH_SIZE) {
        let mut attempt = mode.unwrap_or(BatchMode::Aggregate3);
        let results = loop {
            match attempt {
                BatchMode::Sequential => {
                    let mut results = Vec::with_capacity(chunk.len());
                    for (to, data) in chunk {
                        match provider.eth_call(to, data.clone(), Some(block)).await {
                            Ok(return_data) => results.push(MulticallResult {
                                success: true,
                                return_data,
                            }),
                            Err(_) => results.push(MulticallResult {
                                success: false,
                                return_data: Bytes::new(),
                            }),
                        }
                    }
                    break results;
                }
                batched => {
                    let (label, encoder): (_, fn(&[(Address, Bytes)]) -> _) = match batched {
                        BatchMode::Aggregate3 => ("aggregate3", encode_aggregate3),
                        _ => ("tryAggregate", encode_try_aggregate),
                    };
                    let call = encoder(chunk).map_err(|e| format!("{label} encode: {e}"))?;
                    match provider
                        .eth_call(&MULTICALL3_ADDRESS, call, Some(block))
                        .await
                    {
                        Ok(raw) => {
                            break decode_aggregate3_results(&raw, chunk.len())
                                .map_err(|e| format!("{label} decode: {e}"))?;
                        }
                        Err(_) if mode.is_none() && batched == BatchMode::Aggregate3 => {
                            // No `aggregate3` at the batch address (a Multicall2
                            // deployment): degrade to its twin and say so.
                            eprintln!(
                                "note: aggregate3 unusable at block {block}; batching via Multicall2 tryAggregate"
                            );
                            attempt = BatchMode::TryAggregate;
                        }
                        Err(_) if mode.is_none() => {
                            eprintln!(
                                "note: no batch contract usable at block {block}; one eth_call per sub-call"
                            );
                            attempt = BatchMode::Sequential;
                        }
                        Err(e) => return Err(format!("{label} call: {e}")),
                    }
                }
            }
        };
        mode = Some(attempt);
        out.extend(results);
    }
    Ok(out)
}

/// `getReserves()` at `block`. The V2 `block_number` field of the corpus is the
/// pair's `blockTimestampLast` (a timestamp — the key name the corpus records
/// it under, misleading but load-bearing).
pub async fn scrape_v2_state(
    provider: &AlloyProvider,
    pair: Address,
    block: u64,
) -> Result<FetchedState, String> {
    use degenbot_rpc::abi;
    let bytes = provider
        .eth_call(&pair, Bytes::from(abi::encode_get_reserves()), Some(block))
        .await
        .map_err(|e| format!("getReserves: {e}"))?;
    let r = abi::IUniswapV2Pair::getReservesCall::abi_decode_returns(&bytes)
        .map_err(|e| format!("getReserves decode: {e}"))?;
    Ok(FetchedState::V2 {
        reserve0: U256::from(r.reserve0),
        reserve1: U256::from(r.reserve1),
        block_number: r
            .blockTimestampLast
            .to_string()
            .parse()
            .map_err(|e| format!("blockTimestampLast: {e}"))?,
    })
}

/// A V3-family pool's state at `block`: scalars plus the full initialized-tick
/// set fetched word-wise. The snapshot is exact at `block` by construction.
pub async fn scrape_v3_state(
    provider: &AlloyProvider,
    pool: Address,
    tick_spacing: i32,
    block: u64,
    scan: TickScan,
) -> Result<FetchedState, String> {
    use degenbot_rpc::abi;
    let (sqrt_price_x96, tick, liquidity) =
        abi::fetch_v3_slot0_liquidity(provider, &pool, Some(block))
            .await
            .map_err(|e| format!("v3 scalars: {e}"))?;
    let tick_data = match scan {
        TickScan::Lens(lens) => scan_v3_via_lens(provider, lens, pool, tick_spacing, block).await?,
        TickScan::DirectPool => scan_v3_direct(provider, pool, tick_spacing, block).await?,
    };
    Ok(FetchedState::V3 {
        liquidity_update_block: block,
        tick_data,
        sqrt_price_x96,
        tick: tick.to_string().parse().map_err(|e| format!("tick: {e}"))?,
        liquidity,
    })
}

async fn scan_v3_via_lens(
    provider: &AlloyProvider,
    lens: Address,
    pool: Address,
    tick_spacing: i32,
    block: u64,
) -> Result<BTreeMap<i32, (i128, u128)>, String> {
    let words: Vec<i16> = word_positions(tick_spacing).collect();
    let calls: Vec<(Address, Bytes)> = words
        .iter()
        .map(|word| {
            (
                lens,
                Bytes::from(
                    ITickLens::getPopulatedTicksInWordCall {
                        pool,
                        tickBitmapIndex: *word,
                    }
                    .abi_encode(),
                ),
            )
        })
        .collect();
    let results = aggregate3(provider, &calls, block).await?;
    let mut out = BTreeMap::new();
    for (word, result) in words.iter().zip(results) {
        if !result.success {
            return Err(format!(
                "TickLens.getPopulatedTicksInWord(pool={pool}, word={word}) returned failure"
            ));
        }
        let decoded =
            ITickLens::getPopulatedTicksInWordCall::abi_decode_returns(&result.return_data)
                .map_err(|e| format!("populated-ticks decode (word {word}): {e}"))?;
        for entry in decoded {
            let tick: i32 = entry
                .tick
                .to_string()
                .parse()
                .map_err(|e| format!("word {word} tick: {e}"))?;
            out.insert(tick, (entry.liquidityNet, entry.liquidityGross));
        }
    }
    Ok(out)
}

async fn scan_v3_direct(
    provider: &AlloyProvider,
    pool: Address,
    tick_spacing: i32,
    block: u64,
) -> Result<BTreeMap<i32, (i128, u128)>, String> {
    use degenbot_rpc::abi::IUniswapV3Pool;
    let words: Vec<i16> = word_positions(tick_spacing).collect();
    let bitmap_calls: Vec<(Address, Bytes)> = words
        .iter()
        .map(|word| {
            (
                pool,
                Bytes::from(
                    IUniswapV3Pool::tickBitmapCall {
                        wordPosition: *word,
                    }
                    .abi_encode(),
                ),
            )
        })
        .collect();
    let bitmap_results = aggregate3(provider, &bitmap_calls, block).await?;
    let mut bitmaps: Vec<(i16, U256)> = Vec::with_capacity(words.len());
    for (word, result) in words.iter().zip(bitmap_results) {
        if !result.success {
            return Err(format!("tickBitmap(word={word}) returned failure"));
        }
        // Single return: the decode is the bare word.
        let bitmap = IUniswapV3Pool::tickBitmapCall::abi_decode_returns(&result.return_data)
            .map_err(|e| format!("bitmap decode (word {word}): {e}"))?;
        bitmaps.push((*word, bitmap));
    }
    let mut ticks: Vec<i32> = Vec::new();
    for (word, bitmap) in &bitmaps {
        ticks.extend(ticks_from_bitmap_word(*word, *bitmap, tick_spacing));
    }
    let mut tick_calls: Vec<(Address, Bytes)> = Vec::with_capacity(ticks.len());
    for tick in &ticks {
        tick_calls.push((
            pool,
            Bytes::from(IUniswapV3Pool::ticksCall { tick: i24(*tick)? }.abi_encode()),
        ));
    }
    let tick_results = aggregate3(provider, &tick_calls, block).await?;
    let mut out = BTreeMap::new();
    for (tick, result) in ticks.into_iter().zip(tick_results) {
        if !result.success {
            return Err(format!("ticks(tick={tick}) returned failure"));
        }
        let decoded = IUniswapV3Pool::ticksCall::abi_decode_returns(&result.return_data)
            .map_err(|e| format!("tick decode ({tick}): {e}"))?;
        out.insert(tick, (decoded.liquidityNet, decoded.liquidityGross));
    }
    Ok(out)
}

/// A V4 pool's state at `block` via its StateView: scalars in one helper pair,
/// ticks word-wise (`getTickBitmap`) then per initialized tick
/// (`getTickLiquidity`). The snapshot is exact at `block` by construction.
pub async fn scrape_v4_state(
    provider: &AlloyProvider,
    state_view: Address,
    pool_id: alloy::primitives::B256,
    tick_spacing: i32,
    block: u64,
) -> Result<FetchedState, String> {
    use degenbot_rpc::abi::{self, IUniswapV4StateView};
    let (sqrt_price_x96, tick, protocol_fee, lp_fee, liquidity) =
        abi::fetch_v4_slot0_liquidity(provider, &state_view, &pool_id.0, Some(block))
            .await
            .map_err(|e| format!("v4 scalars: {e}"))?;
    let words: Vec<i16> = word_positions(tick_spacing).collect();
    let bitmap_calls: Vec<(Address, Bytes)> = words
        .iter()
        .map(|word| {
            (
                state_view,
                Bytes::from(
                    IUniswapV4StateView::getTickBitmapCall {
                        poolId: pool_id,
                        wordPosition: *word,
                    }
                    .abi_encode(),
                ),
            )
        })
        .collect();
    let bitmap_results = aggregate3(provider, &bitmap_calls, block).await?;
    let mut ticks: Vec<i32> = Vec::new();
    for (word, result) in words.iter().zip(bitmap_results) {
        if !result.success {
            return Err(format!(
                "StateView.getTickBitmap(word={word}) returned failure"
            ));
        }
        let bitmap =
            IUniswapV4StateView::getTickBitmapCall::abi_decode_returns(&result.return_data)
                .map_err(|e| format!("bitmap decode (word {word}): {e}"))?;
        ticks.extend(ticks_from_bitmap_word(*word, bitmap, tick_spacing));
    }
    let mut tick_calls: Vec<(Address, Bytes)> = Vec::with_capacity(ticks.len());
    for tick in &ticks {
        tick_calls.push((
            state_view,
            Bytes::from(
                IUniswapV4StateView::getTickLiquidityCall {
                    poolId: pool_id,
                    tick: i24(*tick)?,
                }
                .abi_encode(),
            ),
        ));
    }
    let tick_results = aggregate3(provider, &tick_calls, block).await?;
    let mut tick_data = BTreeMap::new();
    for (tick, result) in ticks.into_iter().zip(tick_results) {
        if !result.success {
            return Err(format!(
                "StateView.getTickLiquidity(tick={tick}) returned failure"
            ));
        }
        let decoded =
            IUniswapV4StateView::getTickLiquidityCall::abi_decode_returns(&result.return_data)
                .map_err(|e| format!("tick decode ({tick}): {e}"))?;
        tick_data.insert(tick, (decoded.net, decoded.gross));
    }
    Ok(FetchedState::V4 {
        liquidity_update_block: block,
        tick_data,
        sqrt_price_x96,
        tick: tick.to_string().parse().map_err(|e| format!("tick: {e}"))?,
        liquidity,
        protocol_fee: protocol_fee
            .to_string()
            .parse()
            .map_err(|e| format!("protocol_fee: {e}"))?,
        lp_fee: lp_fee
            .to_string()
            .parse()
            .map_err(|e| format!("lp_fee: {e}"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::I256;
    use degenbot_rpc::abi::{IUniswapV3Pool, IUniswapV4StateView};

    const FIXTURE_73385: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../../tests/fixtures/path73385_v4_block25706469.json"
    );
    const FIXTURE_5000: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../../tests/fixtures/path5000_v2v4v3_block25704509.json"
    );

    #[test]
    fn word_and_bit_round_trips_lens_formula() {
        // `TickLens` inverts as `((word << 8) + bit) * spacing`.
        for (tick, spacing) in [
            (-887_272, 1),
            (-887_270, 10),
            (0, 1),
            (1, 1),
            (255, 1),
            (256, 1),
            (887_270, 10),
            (887_040, 1),
        ] {
            let (word, bit) = word_and_bit(tick, spacing);
            let back = (i32::from(word) * 256 + i32::from(bit)) * spacing;
            assert_eq!(back, tick, "tick={tick} spacing={spacing}");
        }
    }

    #[test]
    fn word_range_covers_the_usable_bounds() {
        let range = word_positions(1);
        assert_eq!(*range.start(), -3466, "-887272 floors to word -3466");
        assert_eq!(*range.end(), 3465, "887272 floors to word 3465");
        assert!(range.contains(&word_and_bit(-887_272, 1).0));
        assert!(range.contains(&word_and_bit(887_040, 1).0));
        let range10 = word_positions(10);
        assert_eq!((*range10.start(), *range10.end()), (-347, 346));
    }

    #[test]
    fn bitmap_words_enumerate_both_signs() {
        let mut bitmap = U256::ZERO;
        bitmap.set_bit(24, true);
        bitmap.set_bit(232, true);
        let ticks = ticks_from_bitmap_word(-3466, bitmap, 1);
        assert_eq!(ticks, vec![-887_272, -887_064]);
        let ticks = ticks_from_bitmap_word(0, bitmap, 10);
        assert_eq!(ticks, vec![240, 2320]);
    }

    // ---- pseudo-golden harness: the committed captures as recorded chain ----

    fn load(path: &str) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture readable"))
            .expect("fixture parses")
    }

    fn word_be_u(v: U256) -> [u8; 32] {
        v.to_be_bytes::<32>()
    }

    fn word_be_i(v: i128) -> [u8; 32] {
        I256::from_dec_str(&v.to_string())
            .expect("i128 fits i256")
            .into_raw()
            .to_be_bytes::<32>()
    }

    fn concat_words(words: &[[u8; 32]]) -> Vec<u8> {
        words.iter().flatten().copied().collect()
    }

    fn hex_str(data: &[u8]) -> String {
        alloy::hex::encode(data)
    }

    fn call_key(to: Address, calldata: &[u8]) -> String {
        format!(
            "{}:0x{}",
            to.to_checksum(None).to_lowercase(),
            hex_str(calldata)
        )
    }

    fn aggregate_return(results: &[(bool, Vec<u8>)]) -> serde_json::Value {
        let values = results
            .iter()
            .map(|(success, data)| {
                alloy::dyn_abi::DynSolValue::Tuple(vec![
                    alloy::dyn_abi::DynSolValue::Bool(*success),
                    alloy::dyn_abi::DynSolValue::Bytes(data.clone()),
                ])
            })
            .collect();
        serde_json::Value::String(hex_str(
            &alloy::dyn_abi::DynSolValue::Array(values).abi_encode(),
        ))
    }

    /// Synthesize the recorded chain: scalars at `target_block` plus every word
    /// answer the harness will batch — chunked exactly like [`aggregate3`]
    /// (MULTICALL3_BATCH_SIZE sub-calls per `aggregate3` key).
    fn recorded_chain(
        doc: &serde_json::Value,
        scan: TickScan,
        state_view: Address,
    ) -> serde_json::Value {
        let target = doc["target_block"].as_u64().expect("target_block");
        let mut calls = serde_json::Map::new();
        // Mirrors `aggregate3`'s chunking: one recorded answer per encoded batch.
        let record_batches = |chunk: &[(Address, Bytes)], results: Vec<(bool, Vec<u8>)>| {
            let mut by_batch: Vec<(String, serde_json::Value)> = Vec::new();
            for (batch, items) in chunk
                .chunks(MULTICALL3_BATCH_SIZE)
                .zip(results.chunks(MULTICALL3_BATCH_SIZE))
            {
                by_batch.push((
                    call_key(
                        MULTICALL3_ADDRESS,
                        &encode_aggregate3(batch).expect("encode"),
                    ),
                    aggregate_return(items),
                ));
            }
            by_batch
        };
        for (key, entry) in doc["pools"].as_object().expect("pools") {
            let family = entry["family"].as_str().expect("family");
            let ticks: BTreeMap<i32, (i128, u128)> = match entry
                .get("tick_data")
                .and_then(serde_json::Value::as_object)
            {
                Some(map) => map
                    .iter()
                    .map(|(t, v)| {
                        (
                            t.parse().expect("tick"),
                            (
                                v["liquidity_net"]
                                    .as_str()
                                    .expect("net")
                                    .parse()
                                    .expect("net"),
                                v["liquidity_gross"]
                                    .as_str()
                                    .expect("gross")
                                    .parse()
                                    .expect("gross"),
                            ),
                        )
                    })
                    .collect(),
                None => BTreeMap::new(),
            };
            let spacing = entry["tick_spacing"].as_i64().unwrap_or(1) as i32;
            if family.ends_with("_v4") {
                let pid: alloy::primitives::B256 = entry["pool_id"]
                    .as_str()
                    .expect("pool_id")
                    .parse()
                    .expect("pid");
                let slot0 = concat_words(&[
                    word_be_u(
                        entry["sqrt_price_x96"]
                            .as_str()
                            .expect("sqrt")
                            .parse()
                            .expect("sqrt"),
                    ),
                    word_be_i(entry["tick"].as_i64().expect("tick") as i128),
                    word_be_u(U256::from(entry["protocol_fee"].as_u64().expect("pf"))),
                    word_be_u(U256::from(entry["lp_fee"].as_u64().expect("lf"))),
                ]);
                let liq = word_be_u(
                    entry["liquidity"]
                        .as_str()
                        .expect("liq")
                        .parse()
                        .expect("liq"),
                );
                calls.insert(
                    call_key(
                        state_view,
                        &IUniswapV4StateView::getSlot0Call { poolId: pid }.abi_encode(),
                    ),
                    serde_json::Value::String(hex_str(&slot0)),
                );
                calls.insert(
                    call_key(
                        state_view,
                        &IUniswapV4StateView::getLiquidityCall { poolId: pid }.abi_encode(),
                    ),
                    serde_json::Value::String(hex_str(&liq)),
                );
                let words: Vec<i16> = word_positions(spacing).collect();
                let bitmap_calls: Vec<(Address, Bytes)> = words
                    .iter()
                    .map(|word| {
                        let mut bitmap = U256::ZERO;
                        for t in ticks.keys() {
                            if word_and_bit(*t, spacing).0 == *word {
                                bitmap.set_bit(word_and_bit(*t, spacing).1 as usize, true);
                            }
                        }
                        (
                            state_view,
                            Bytes::from(
                                IUniswapV4StateView::getTickBitmapCall {
                                    poolId: pid,
                                    wordPosition: *word,
                                }
                                .abi_encode(),
                            ),
                        )
                    })
                    .collect();
                let bitmap_results: Vec<(bool, Vec<u8>)> = bitmap_calls
                    .iter()
                    .map(|(_, calldata)| {
                        let decoded = IUniswapV4StateView::getTickBitmapCall::abi_decode(calldata)
                            .expect("call decodes");
                        let word = decoded.wordPosition;
                        let mut bitmap = U256::ZERO;
                        for t in ticks.keys() {
                            if word_and_bit(*t, spacing).0 == word {
                                bitmap.set_bit(word_and_bit(*t, spacing).1 as usize, true);
                            }
                        }
                        (true, word_be_u(bitmap).to_vec())
                    })
                    .collect();
                for (k, v) in record_batches(&bitmap_calls, bitmap_results) {
                    calls.insert(k, v);
                }
                let all_ticks: Vec<i32> = ticks.keys().copied().collect();
                let tick_calls: Vec<(Address, Bytes)> = all_ticks
                    .iter()
                    .map(|t| {
                        (
                            state_view,
                            Bytes::from(
                                IUniswapV4StateView::getTickLiquidityCall {
                                    poolId: pid,
                                    tick: i24(*t).expect("tick"),
                                }
                                .abi_encode(),
                            ),
                        )
                    })
                    .collect();
                let tick_results: Vec<(bool, Vec<u8>)> = all_ticks
                    .iter()
                    .map(|t| {
                        let (net, gross) = ticks[t];
                        (
                            true,
                            concat_words(&[word_be_u(U256::from(gross)), word_be_i(net)]),
                        )
                    })
                    .collect();
                for (k, v) in record_batches(&tick_calls, tick_results) {
                    calls.insert(k, v);
                }
                let _ = &key;
            } else if family.ends_with("_v3") {
                let pool: Address = entry["address"]
                    .as_str()
                    .expect("address")
                    .parse()
                    .expect("addr");
                let slot0 = concat_words(&[
                    word_be_u(
                        entry["sqrt_price_x96"]
                            .as_str()
                            .expect("sqrt")
                            .parse()
                            .expect("sqrt"),
                    ),
                    word_be_i(entry["tick"].as_i64().expect("tick") as i128),
                    word_be_u(U256::ZERO),
                    word_be_u(U256::ZERO),
                    word_be_u(U256::ZERO),
                    word_be_u(U256::ZERO),
                    word_be_u(U256::from(1)),
                ]);
                let liq = word_be_u(
                    entry["liquidity"]
                        .as_str()
                        .expect("liq")
                        .parse()
                        .expect("liq"),
                );
                calls.insert(
                    call_key(pool, &degenbot_rpc::abi::encode_slot0()),
                    serde_json::Value::String(hex_str(&slot0)),
                );
                calls.insert(
                    call_key(pool, &degenbot_rpc::abi::encode_liquidity()),
                    serde_json::Value::String(hex_str(&liq)),
                );
                let words: Vec<i16> = word_positions(spacing).collect();
                let per_word = |word: i16| {
                    let mut entries = Vec::new();
                    for t in ticks.keys() {
                        if word_and_bit(*t, spacing).0 == word {
                            let (net, gross) = ticks[t];
                            entries.push((*t, net, gross));
                        }
                    }
                    entries
                };
                match scan {
                    TickScan::Lens(lens) => {
                        let lens_calls: Vec<(Address, Bytes)> = words
                            .iter()
                            .map(|word| {
                                (
                                    lens,
                                    Bytes::from(
                                        ITickLens::getPopulatedTicksInWordCall {
                                            pool,
                                            tickBitmapIndex: *word,
                                        }
                                        .abi_encode(),
                                    ),
                                )
                            })
                            .collect();
                        let lens_results: Vec<(bool, Vec<u8>)> = words
                            .iter()
                            .map(|word| {
                                let entries: Vec<ITickLens::PopulatedTick> = per_word(*word)
                                    .into_iter()
                                    .map(|(t, net, gross)| ITickLens::PopulatedTick {
                                        tick: i24(t).expect("tick"),
                                        liquidityNet: net,
                                        liquidityGross: gross,
                                    })
                                    .collect();
                                (
                                    true,
                                    ITickLens::getPopulatedTicksInWordCall::abi_encode_returns(
                                        &entries,
                                    ),
                                )
                            })
                            .collect();
                        for (k, v) in record_batches(&lens_calls, lens_results) {
                            calls.insert(k, v);
                        }
                    }
                    TickScan::DirectPool => {
                        let bitmap_calls: Vec<(Address, Bytes)> = words
                            .iter()
                            .map(|word| {
                                (
                                    pool,
                                    Bytes::from(
                                        IUniswapV3Pool::tickBitmapCall {
                                            wordPosition: *word,
                                        }
                                        .abi_encode(),
                                    ),
                                )
                            })
                            .collect();
                        let bitmap_results: Vec<(bool, Vec<u8>)> = words
                            .iter()
                            .map(|word| {
                                let mut bitmap = U256::ZERO;
                                for (t, _, _) in per_word(*word) {
                                    bitmap.set_bit(word_and_bit(t, spacing).1 as usize, true);
                                }
                                (true, word_be_u(bitmap).to_vec())
                            })
                            .collect();
                        for (k, v) in record_batches(&bitmap_calls, bitmap_results) {
                            calls.insert(k, v);
                        }
                        let all_ticks: Vec<i32> = ticks.keys().copied().collect();
                        let tick_calls: Vec<(Address, Bytes)> = all_ticks
                            .iter()
                            .map(|t| {
                                (
                                    pool,
                                    Bytes::from(
                                        IUniswapV3Pool::ticksCall {
                                            tick: i24(*t).expect("tick"),
                                        }
                                        .abi_encode(),
                                    ),
                                )
                            })
                            .collect();
                        let tick_results: Vec<(bool, Vec<u8>)> = all_ticks
                            .iter()
                            .map(|t| {
                                let (net, gross) = ticks[t];
                                let mut data =
                                    concat_words(&[word_be_u(U256::from(gross)), word_be_i(net)]);
                                data.extend(std::iter::repeat_n(0u8, 6 * 32));
                                (true, data)
                            })
                            .collect();
                        for (k, v) in record_batches(&tick_calls, tick_results) {
                            calls.insert(k, v);
                        }
                    }
                }
            } else {
                let pair: Address = entry["address"]
                    .as_str()
                    .expect("address")
                    .parse()
                    .expect("addr");
                let reserves = concat_words(&[
                    word_be_u(entry["reserve0"].as_str().expect("r0").parse().expect("r0")),
                    word_be_u(entry["reserve1"].as_str().expect("r1").parse().expect("r1")),
                    word_be_u(U256::from(entry["block_number"].as_u64().expect("ts"))),
                ]);
                calls.insert(
                    call_key(pair, &degenbot_rpc::abi::encode_get_reserves()),
                    serde_json::Value::String(hex_str(&reserves)),
                );
            }
        }
        serde_json::json!({
            "chain_id": 1,
            "block_number": target,
            "timestamp": 0,
            "calls": calls,
            "code": {},
        })
    }

    fn provider_for(recorded: &serde_json::Value) -> AlloyProvider {
        degenbot_rpc::offline::OfflineProvider::from_json_str(&recorded.to_string())
            .expect("recorded chain parses")
            .as_alloy_provider()
    }

    async fn scrape_all(
        provider: &AlloyProvider,
        doc: &serde_json::Value,
        state_view: Address,
        scan: TickScan,
    ) -> BTreeMap<String, FetchedState> {
        use crate::investigation::capture::pool_spec;
        let target = doc["target_block"].as_u64().expect("target_block");
        let mut out = BTreeMap::new();
        for (key, entry) in doc["pools"].as_object().expect("pools") {
            let spec = pool_spec(entry).expect("spec");
            let spacing = entry["tick_spacing"].as_i64().unwrap_or(1) as i32;
            let state = if spec.family.ends_with("_v4") {
                let pid = spec.pool_id.expect("pid");
                scrape_v4_state(provider, state_view, pid, spacing, target)
                    .await
                    .expect("v4 scrape")
            } else if spec.family.ends_with("_v3") {
                let pool = spec.address.expect("addr");
                scrape_v3_state(provider, pool, spacing, target, scan)
                    .await
                    .expect("v3 scrape")
            } else {
                let pair = spec.address.expect("addr");
                scrape_v2_state(provider, pair, target)
                    .await
                    .expect("v2 scrape")
            };
            out.insert(key.clone(), state);
        }
        out
    }

    /// The recorded fixture's own chain answers, re-scraped through the real
    /// fetch path, must reproduce the document EXCEPT that a chain snapshot
    /// stamps its capture block as the liquidity-update marker where the corpus
    /// recorded the tracker's older marker (an intentional, documented delta).
    async fn pseudo_golden(path: &str, scan: TickScan) {
        use crate::investigation::capture::{diff_state, refresh_fixture};
        let doc = load(path);
        let state_view = Address::repeat_byte(0x5f);
        let recorded = recorded_chain(&doc, scan, state_view);
        let provider = provider_for(&recorded);
        let fetched = scrape_all(&provider, &doc, state_view, scan).await;
        let refreshed = refresh_fixture(&doc, &fetched).expect("refresh");
        assert_eq!(
            diff_state(&doc, &refreshed),
            vec!["pools.v4.liquidity_update_block".to_string()],
            "the only delta vs the corpus must be the v4 snapshot marker ({path})"
        );
    }

    #[tokio::test]
    async fn pseudo_golden_path73385_via_ticklens() {
        pseudo_golden(FIXTURE_73385, TickScan::Lens(Address::repeat_byte(0x1e))).await;
    }

    #[tokio::test]
    async fn pseudo_golden_path5000_via_ticklens() {
        pseudo_golden(FIXTURE_5000, TickScan::Lens(Address::repeat_byte(0x1e))).await;
    }

    #[tokio::test]
    async fn pseudo_golden_path73385_direct_pool_scan() {
        pseudo_golden(FIXTURE_73385, TickScan::DirectPool).await;
    }

    /// Drift-gate discipline: a missing batch answer must fail the scrape
    /// loudly (OfflineProvider's unrecorded-key contract), never silently
    /// under-capture.
    #[tokio::test]
    async fn missing_word_answer_fails_the_scrape() {
        let doc = load(FIXTURE_5000);
        let state_view = Address::repeat_byte(0x5f);
        let lens = Address::repeat_byte(0x1e);
        let mut recorded = recorded_chain(&doc, TickScan::Lens(lens), state_view);
        // Punch the hole where the v3_2 lens scan will look: its FIRST encoded
        // aggregate3 batch (MULTICALL3_BATCH_SIZE word sub-calls).
        let spacing0 = doc["pools"]["v3_2"]["tick_spacing"]
            .as_i64()
            .expect("spacing") as i32;
        let words: Vec<i16> = word_positions(spacing0).collect();
        let pool0 = {
            let spec =
                crate::investigation::capture::pool_spec(&doc["pools"]["v3_2"]).expect("spec");
            spec.address.expect("addr")
        };
        let lens_calls: Vec<(Address, Bytes)> = words
            .iter()
            .map(|word| {
                (
                    lens,
                    Bytes::from(
                        ITickLens::getPopulatedTicksInWordCall {
                            pool: pool0,
                            tickBitmapIndex: *word,
                        }
                        .abi_encode(),
                    ),
                )
            })
            .collect();
        let first_batch = &lens_calls[..MULTICALL3_BATCH_SIZE.min(lens_calls.len())];
        let hole_key = call_key(
            MULTICALL3_ADDRESS,
            &encode_aggregate3(first_batch).expect("encode"),
        );
        let calls = recorded["calls"].as_object_mut().expect("calls");
        calls.remove(&hole_key);
        let provider = provider_for(&recorded);
        let pool = {
            let spec =
                crate::investigation::capture::pool_spec(&doc["pools"]["v3_2"]).expect("spec");
            spec.address.expect("addr")
        };
        let spacing = doc["pools"]["v3_2"]["tick_spacing"]
            .as_i64()
            .expect("spacing") as i32;
        let result = scrape_v3_state(
            &provider,
            pool,
            spacing,
            doc["target_block"].as_u64().expect("target"),
            TickScan::Lens(Address::repeat_byte(0x1e)),
        )
        .await;
        assert!(
            result.is_err(),
            "a missing word answer must fail the scrape"
        );
    }
}
