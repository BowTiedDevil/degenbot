//! Contract-facing full liquidity-map verification for concentrated-liquidity pools.
//!
//! The verifier is deliberately family-neutral at its public seam. A caller
//! supplies a complete (`Tracked`) map and either a V3 pool address or a V4
//! `PoolManager` + `PoolId`; family-specific storage encoding is an internal
//! read adapter. Mutable slot0 scalars are outside this seam.

use alloy::dyn_abi::{DynSolType, DynSolValue};
use alloy::primitives::{keccak256, Address, Bytes, B256, U128, U256};
use degenbot_core::errors::ProviderError;
use degenbot_math::cl::tick_math::{MAX_TICK, MIN_TICK};
use degenbot_pools::v3_state::PoolTickCoverage;
use degenbot_pools::TickInfo;
use hashbrown::{HashMap, HashSet};

use crate::abi::{decode_tick_bitmap, decode_tick_data, encode_tick_bitmap, encode_tick_data};
use crate::multicall3::{multicall3_batch, MulticallResult};
use crate::provider::AlloyProvider;

/// The contract identity targeted by a full-map verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiquidityMapTarget {
    /// A V3 pool contract exposing `ticks(int24)` and `tickBitmap(int16)`.
    V3(Address),
    /// A V4 pool's canonical storage owner and its `PoolId`.
    V4 {
        /// The V4 `PoolManager` singleton.
        pool_manager: Address,
        /// The V4 pool identity.
        pool_id: B256,
    },
}

/// A complete concentrated-liquidity map supplied to the verifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiquidityMap {
    /// Coverage provenance. Only [`PoolTickCoverage::Tracked`] is a valid
    /// full-map input; sparse maps remain bootstrap-only.
    pub coverage: PoolTickCoverage,
    /// Initialized tick rows.
    pub ticks: HashMap<i32, TickInfo>,
    /// Bitmap words, keyed by their V3/V4 signed word position.
    pub bitmaps: HashMap<i32, U256>,
    /// Tick spacing used to expand bitmap words into tick keys.
    pub tick_spacing: i32,
}

impl LiquidityMap {
    /// Construct a tracked full map with the pool's tick spacing.
    ///
    /// A spacing-free constructor would fabricate bitmap words when the map is
    /// used for discovery, so callers must supply the pool's real spacing.
    ///
    /// ```compile_fail
    /// use degenbot_rpc::liquidity_verifier::LiquidityMap;
    /// use hashbrown::HashMap;
    ///
    /// let _ = LiquidityMap::tracked_with_spacing(HashMap::new(), HashMap::new());
    /// ```
    #[must_use]
    pub fn tracked_with_spacing(
        ticks: HashMap<i32, TickInfo>,
        bitmaps: HashMap<i32, U256>,
        tick_spacing: i32,
    ) -> Self {
        Self {
            coverage: PoolTickCoverage::Tracked,
            ticks,
            bitmaps,
            tick_spacing,
        }
    }
}

/// One typed fact proving that a supplied map differs from chain state.
///
/// Facts are ordered deterministically by the verifier: tick facts first in
/// ascending tick order (`TickPresence`, then `TickGross`, then `TickNet`),
/// followed by bitmap facts in ascending word order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiquidityMapDivergence {
    /// The initialized-tick presence differs between map and chain.
    TickPresence {
        /// Tick index.
        tick: i32,
        /// Whether the supplied map contains the tick.
        stored: bool,
        /// Whether chain state contains the tick.
        observed: bool,
    },
    /// The tick exists on both sides but its gross liquidity differs.
    TickGross {
        /// Tick index.
        tick: i32,
        /// Supplied gross liquidity.
        expected: U128,
        /// Chain gross liquidity.
        actual: U128,
    },
    /// The tick exists on both sides but its net liquidity differs.
    TickNet {
        /// Tick index.
        tick: i32,
        /// Supplied net liquidity.
        expected: i128,
        /// Chain net liquidity.
        actual: i128,
    },
    /// A supplied bitmap word differs from chain state.
    BitmapWord {
        /// Signed bitmap word position.
        word: i32,
        /// Supplied bitmap.
        expected: U256,
        /// Chain bitmap.
        actual: U256,
    },
}

/// A failure to complete a contract read or decode its response.
///
/// These remain separate from [`LiquidityMapDivergence`]: a mismatch is map
/// state evidence, while `Read` and `Decode` describe inability to establish
/// that evidence.
#[derive(Debug, thiserror::Error)]
pub enum LiquidityMapVerifyError {
    /// A sparse map cannot be used as a full verification input.
    #[error("full liquidity-map verification requires Tracked coverage")]
    SparseInput,
    /// The RPC read failed.
    #[error("liquidity-map RPC read failed: {reason}")]
    Read { reason: String },
    /// The RPC response could not be decoded.
    #[error("liquidity-map response decode failed: {reason}")]
    Decode { reason: String },
}

impl LiquidityMapVerifyError {
    fn provider(error: ProviderError) -> Self {
        match error {
            ProviderError::DecodingError { message }
            | ProviderError::InvalidResponse { message } => Self::Decode { reason: message },
            other => Self::Read {
                reason: other.to_string(),
            },
        }
    }
}

/// Verify one complete map against its contract target at `block_number`.
///
/// V3 reads supplied tick rows and bitmap words through the existing V3 ABI +
/// Multicall3 read behavior, then batch-reads any on-chain-only ticks discovered
/// from those words. V4 derives canonical `Pool.State` slots and uses one
/// `PoolManager.extsload(bytes32[])` call for supplied slots, with a second
/// batched call only when bitmap discovery finds additional ticks.
/// The function does not read mutable slot0 scalars and has no verification
/// policy argument: callers decide whether and when to invoke it.
///
/// # Errors
///
/// Returns [`LiquidityMapVerifyError::SparseInput`] for a sparse map,
/// [`LiquidityMapVerifyError::Read`] when the RPC read cannot complete, and
/// [`LiquidityMapVerifyError::Decode`] when a response cannot be decoded.
pub async fn verify_liquidity_map(
    provider: &AlloyProvider,
    target: LiquidityMapTarget,
    map: &LiquidityMap,
    block_number: Option<u64>,
) -> Result<Vec<LiquidityMapDivergence>, LiquidityMapVerifyError> {
    if map.coverage != PoolTickCoverage::Tracked {
        return Err(LiquidityMapVerifyError::SparseInput);
    }
    validate_map_domains(map)?;
    if map.ticks.is_empty() && map.bitmaps.is_empty() {
        return Ok(Vec::new());
    }

    match target {
        LiquidityMapTarget::V3(pool) => verify_v3(provider, pool, map, block_number).await,
        LiquidityMapTarget::V4 {
            pool_manager,
            pool_id,
        } => verify_v4(provider, pool_manager, pool_id, map, block_number).await,
    }
}

async fn verify_v3(
    provider: &AlloyProvider,
    pool: Address,
    map: &LiquidityMap,
    block_number: Option<u64>,
) -> Result<Vec<LiquidityMapDivergence>, LiquidityMapVerifyError> {
    let mut ticks: Vec<i32> = map.ticks.keys().copied().collect();
    ticks.sort_unstable();
    let mut words: Vec<i32> = map.bitmaps.keys().copied().collect();
    words.sort_unstable();

    // Read supplied rows and words together first. Bitmap discovery can then
    // add any on-chain-only ticks in the checked words and issue one follow-up
    // tick batch only when needed.
    let mut calls = Vec::with_capacity(ticks.len() + words.len());
    for &tick in &ticks {
        calls.push((pool, Bytes::from(encode_tick_data(tick))));
    }
    for &word in &words {
        let word_i16 = i16::try_from(word).map_err(|_| LiquidityMapVerifyError::Decode {
            reason: format!("V3 bitmap word {word} does not fit int16"),
        })?;
        calls.push((pool, Bytes::from(encode_tick_bitmap(word_i16))));
    }

    let results = multicall3_batch(provider, &calls, block_number)
        .await
        .map_err(LiquidityMapVerifyError::provider)?;
    let mut observed_ticks = HashMap::with_capacity(ticks.len());
    for (tick, result) in ticks.iter().zip(&results) {
        let (gross, net) = decode_v3_result(result, *tick)?;
        observed_ticks.insert(*tick, (gross, net));
    }
    let mut observed_bitmaps = HashMap::with_capacity(words.len());
    for (word, result) in words.iter().zip(results[ticks.len()..].iter()) {
        observed_bitmaps.insert(*word, decode_v3_bitmap_result(result, *word)?);
    }

    let mut discovered = HashSet::new();
    for (word, bitmap) in &observed_bitmaps {
        discovered.extend(bitmap_ticks(*word, *bitmap, map.tick_spacing)?);
    }
    let mut missing: Vec<i32> = discovered
        .into_iter()
        .filter(|tick| !observed_ticks.contains_key(tick))
        .collect();
    missing.sort_unstable();
    if !missing.is_empty() {
        let calls: Vec<_> = missing
            .iter()
            .map(|&tick| (pool, Bytes::from(encode_tick_data(tick))))
            .collect();
        let results = multicall3_batch(provider, &calls, block_number)
            .await
            .map_err(LiquidityMapVerifyError::provider)?;
        for (tick, result) in missing.iter().zip(&results) {
            let (gross, net) = decode_v3_result(result, *tick)?;
            observed_ticks.insert(*tick, (gross, net));
        }
    }
    Ok(compare_maps(map, &observed_ticks, &observed_bitmaps))
}

async fn verify_v4(
    provider: &AlloyProvider,
    pool_manager: Address,
    pool_id: B256,
    map: &LiquidityMap,
    block_number: Option<u64>,
) -> Result<Vec<LiquidityMapDivergence>, LiquidityMapVerifyError> {
    let mut ticks: Vec<i32> = map.ticks.keys().copied().collect();
    ticks.sort_unstable();
    let mut words: Vec<i32> = map.bitmaps.keys().copied().collect();
    words.sort_unstable();

    let state_base = v4_state_base_slot(pool_id);
    let ticks_base = state_base + U256::from(4);
    let bitmap_base = state_base + U256::from(5);
    let mut slots = Vec::with_capacity(ticks.len() + words.len());
    for &tick in &ticks {
        slots.push(v4_nested_slot(tick, ticks_base));
    }
    for &word in &words {
        slots.push(v4_nested_slot(word, bitmap_base));
    }
    let values = read_v4_slots(provider, pool_manager, &slots, block_number).await?;
    let mut observed_ticks = HashMap::with_capacity(ticks.len());
    for (index, &tick) in ticks.iter().enumerate() {
        observed_ticks.insert(tick, decode_v4_tick_slot(values[index]));
    }
    let mut observed_bitmaps = HashMap::with_capacity(words.len());
    for (index, &word) in words.iter().enumerate() {
        observed_bitmaps.insert(
            word,
            U256::from_be_bytes::<32>(values[ticks.len() + index].0),
        );
    }

    let mut discovered = HashSet::new();
    for (word, bitmap) in &observed_bitmaps {
        discovered.extend(bitmap_ticks(*word, *bitmap, map.tick_spacing)?);
    }
    let mut missing: Vec<i32> = discovered
        .into_iter()
        .filter(|tick| !observed_ticks.contains_key(tick))
        .collect();
    missing.sort_unstable();
    if !missing.is_empty() {
        let slots: Vec<_> = missing
            .iter()
            .map(|&tick| v4_nested_slot(tick, ticks_base))
            .collect();
        let values = read_v4_slots(provider, pool_manager, &slots, block_number).await?;
        for (tick, value) in missing.iter().zip(&values) {
            observed_ticks.insert(*tick, decode_v4_tick_slot(*value));
        }
    }
    Ok(compare_maps(map, &observed_ticks, &observed_bitmaps))
}

async fn read_v4_slots(
    provider: &AlloyProvider,
    pool_manager: Address,
    slots: &[B256],
    block_number: Option<u64>,
) -> Result<Vec<B256>, LiquidityMapVerifyError> {
    if slots.is_empty() {
        return Ok(Vec::new());
    }
    let return_data = provider
        .eth_call(&pool_manager, encode_extsload(slots), block_number)
        .await
        .map_err(LiquidityMapVerifyError::provider)?;
    let values = decode_extsload(&return_data, slots.len())?;
    if values.len() != slots.len() {
        return Err(LiquidityMapVerifyError::Decode {
            reason: format!(
                "extsload returned {} values for {} slots",
                values.len(),
                slots.len()
            ),
        });
    }
    Ok(values)
}

fn compare_maps(
    map: &LiquidityMap,
    observed_ticks: &HashMap<i32, (U128, i128)>,
    observed_bitmaps: &HashMap<i32, U256>,
) -> Vec<LiquidityMapDivergence> {
    let mut ticks: Vec<i32> = map
        .ticks
        .keys()
        .chain(observed_ticks.keys())
        .copied()
        .collect();
    ticks.sort_unstable();
    ticks.dedup();
    let mut out = Vec::new();
    for tick in ticks {
        let stored = map.ticks.get(&tick);
        let observed = observed_ticks.get(&tick);
        match (stored, observed) {
            (None, None) => {}
            (Some(_), None) | (None, Some(_)) => out.push(LiquidityMapDivergence::TickPresence {
                tick,
                stored: stored.is_some(),
                observed: observed.is_some(),
            }),
            (Some(stored), Some(observed)) => {
                if stored.liquidity_gross != observed.0 {
                    out.push(LiquidityMapDivergence::TickGross {
                        tick,
                        expected: stored.liquidity_gross,
                        actual: observed.0,
                    });
                }
                if stored.liquidity_net != observed.1 {
                    out.push(LiquidityMapDivergence::TickNet {
                        tick,
                        expected: stored.liquidity_net,
                        actual: observed.1,
                    });
                }
            }
        }
    }
    let mut words: Vec<i32> = map.bitmaps.keys().copied().collect();
    words.sort_unstable();
    for word in words {
        if let Some(actual) = observed_bitmaps.get(&word) {
            let expected = map.bitmaps[&word];
            if expected != *actual {
                out.push(LiquidityMapDivergence::BitmapWord {
                    word,
                    expected,
                    actual: *actual,
                });
            }
        }
    }
    out
}

fn validate_map_domains(map: &LiquidityMap) -> Result<(), LiquidityMapVerifyError> {
    if !(1..=32_767).contains(&map.tick_spacing) {
        return Err(LiquidityMapVerifyError::Decode {
            reason: format!("tick spacing {} is outside [1, 32767]", map.tick_spacing),
        });
    }
    for &tick in map.ticks.keys() {
        validate_tick(tick, "V3/V4")?;
    }
    for &word in map.bitmaps.keys() {
        validate_word(word, "V3/V4")?;
    }
    Ok(())
}

fn validate_tick(tick: i32, family: &str) -> Result<(), LiquidityMapVerifyError> {
    if (MIN_TICK..=MAX_TICK).contains(&tick) {
        Ok(())
    } else {
        Err(LiquidityMapVerifyError::Decode {
            reason: format!("{family} tick {tick} is outside [{MIN_TICK}, {MAX_TICK}]"),
        })
    }
}

fn validate_word(word: i32, family: &str) -> Result<(), LiquidityMapVerifyError> {
    if (i32::from(i16::MIN)..=i32::from(i16::MAX)).contains(&word) {
        Ok(())
    } else {
        Err(LiquidityMapVerifyError::Decode {
            reason: format!("{family} bitmap word {word} does not fit int16"),
        })
    }
}

fn bitmap_ticks(
    word: i32,
    bitmap: U256,
    tick_spacing: i32,
) -> Result<Vec<i32>, LiquidityMapVerifyError> {
    validate_word(word, "V3/V4")?;
    let mut ticks = Vec::new();
    for bit in 0u8..=255 {
        if bitmap.bit(usize::from(bit)) {
            let compressed = word
                .checked_mul(256)
                .and_then(|value| value.checked_add(i32::from(bit)))
                .and_then(|value| value.checked_mul(tick_spacing))
                .ok_or_else(|| LiquidityMapVerifyError::Decode {
                    reason: format!("bitmap word {word} bit {bit} produces an invalid tick"),
                })?;
            validate_tick(compressed, "V3/V4")?;
            ticks.push(compressed);
        }
    }
    Ok(ticks)
}

fn decode_v3_result(
    result: &MulticallResult,
    tick: i32,
) -> Result<(U128, i128), LiquidityMapVerifyError> {
    if !result.success {
        return Err(LiquidityMapVerifyError::Read {
            reason: format!("V3 ticks({tick}) reverted"),
        });
    }
    decode_tick_data(&result.return_data).map_err(|e| LiquidityMapVerifyError::Decode {
        reason: format!("V3 ticks({tick}): {e}"),
    })
}

fn decode_v3_bitmap_result(
    result: &MulticallResult,
    word: i32,
) -> Result<U256, LiquidityMapVerifyError> {
    if !result.success {
        return Err(LiquidityMapVerifyError::Read {
            reason: format!("V3 tickBitmap({word}) reverted"),
        });
    }
    decode_tick_bitmap(&result.return_data).map_err(|e| LiquidityMapVerifyError::Decode {
        reason: format!("V3 tickBitmap({word}): {e}"),
    })
}

const V4_POOLS_BASE_SLOT: u8 = 6;
const EXTSLOAD_SELECTOR: [u8; 4] = [0xdb, 0xd0, 0x35, 0xff];

fn v4_state_base_slot(pool_id: B256) -> U256 {
    let mut encoded = [0u8; 64];
    encoded[..32].copy_from_slice(pool_id.as_slice());
    encoded[63] = V4_POOLS_BASE_SLOT;
    U256::from_be_bytes(keccak256(encoded).0)
}

fn v4_nested_slot(key: i32, base: U256) -> B256 {
    let mut encoded = [0u8; 64];
    if key < 0 {
        encoded[..28].fill(0xff);
    }
    encoded[28..32].copy_from_slice(&key.to_be_bytes());
    encoded[32..].copy_from_slice(&base.to_be_bytes::<32>());
    keccak256(encoded)
}

fn encode_extsload(slots: &[B256]) -> Bytes {
    let values: Vec<DynSolValue> = slots
        .iter()
        .map(|slot| DynSolValue::FixedBytes(*slot, 32))
        .collect();
    let params = DynSolValue::Tuple(vec![DynSolValue::Array(values)]);
    let mut calldata = Vec::with_capacity(4 + 64 + 32 * slots.len());
    calldata.extend_from_slice(&EXTSLOAD_SELECTOR);
    calldata.extend_from_slice(&params.abi_encode_params());
    Bytes::from(calldata)
}

fn decode_extsload(data: &[u8], expected: usize) -> Result<Vec<B256>, LiquidityMapVerifyError> {
    if expected == 0 {
        return Ok(Vec::new());
    }
    let ty: DynSolType = "bytes32[]"
        .parse()
        .map_err(|e| LiquidityMapVerifyError::Decode {
            reason: format!("bytes32[] type: {e}"),
        })?;
    let decoded = ty
        .abi_decode(data)
        .map_err(|e| LiquidityMapVerifyError::Decode {
            reason: format!("extsload return: {e}"),
        })?;
    let DynSolValue::Array(values) = decoded else {
        return Err(LiquidityMapVerifyError::Decode {
            reason: "extsload return was not an array".into(),
        });
    };
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        let DynSolValue::FixedBytes(bytes, 32) = value else {
            return Err(LiquidityMapVerifyError::Decode {
                reason: "extsload return contained a non-bytes32 value".into(),
            });
        };
        out.push(bytes);
    }
    Ok(out)
}

fn decode_v4_tick_slot(word: B256) -> (U128, i128) {
    let bytes = word.0;
    let mut gross = [0u8; 16];
    gross.copy_from_slice(&bytes[16..]);
    let mut net = [0u8; 16];
    net.copy_from_slice(&bytes[..16]);
    (U128::from_be_bytes(gross), i128::from_be_bytes(net))
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use alloy::primitives::{address, hex_literal::hex};

    fn tick(gross: u128, net: i128) -> TickInfo {
        TickInfo {
            liquidity_gross: U128::from(gross),
            liquidity_net: net,
            block: 0,
        }
    }

    #[test]
    fn tracked_empty_map_is_valid_without_rpc() {
        let map = LiquidityMap::tracked_with_spacing(HashMap::new(), HashMap::new(), 1);
        let runtime = degenbot_core::runtime::get_runtime();
        let provider = runtime
            .block_on(AlloyProvider::new("http://127.0.0.1:9/", 1))
            .unwrap();
        let result = runtime.block_on(verify_liquidity_map(
            &provider,
            LiquidityMapTarget::V3(address!("0000000000000000000000000000000000000001")),
            &map,
            Some(1),
        ));
        assert_eq!(result.unwrap(), Vec::<LiquidityMapDivergence>::new());
    }

    #[test]
    fn sparse_map_is_rejected() {
        let mut map = LiquidityMap::tracked_with_spacing(HashMap::new(), HashMap::new(), 1);
        map.coverage = PoolTickCoverage::Sparse;
        let runtime = degenbot_core::runtime::get_runtime();
        let provider = runtime
            .block_on(AlloyProvider::new("http://127.0.0.1:9/", 1))
            .unwrap();
        let result = runtime.block_on(verify_liquidity_map(
            &provider,
            LiquidityMapTarget::V3(address!("0000000000000000000000000000000000000001")),
            &map,
            Some(1),
        ));
        assert!(matches!(result, Err(LiquidityMapVerifyError::SparseInput)));
    }

    fn call_key(to: Address, calldata: &[u8]) -> String {
        format!(
            "{}:0x{}",
            to.to_checksum(None).to_lowercase(),
            alloy::hex::encode(calldata)
        )
    }

    fn aggregate_return(results: &[(bool, Vec<u8>)]) -> String {
        let values = results
            .iter()
            .map(|(success, data)| {
                DynSolValue::Tuple(vec![
                    DynSolValue::Bool(*success),
                    DynSolValue::Bytes(data.clone()),
                ])
            })
            .collect();
        alloy::hex::encode(DynSolValue::Array(values).abi_encode())
    }

    fn v3_map() -> LiquidityMap {
        LiquidityMap::tracked_with_spacing(
            HashMap::from([(120, tick(500, -100))]),
            HashMap::from([(0, U256::from(1u8) << 12)]),
            10,
        )
    }
    #[tokio::test]
    async fn v3_discovers_onchain_only_tick_from_supplied_bitmap_word() {
        let pool = address!("0000000000000000000000000000000000000001");
        let bitmap_calls = vec![(pool, Bytes::from(encode_tick_bitmap(0)))];
        let bitmap_batch = crate::multicall3::encode_aggregate3(&bitmap_calls).unwrap();
        let tick_calls = vec![(pool, Bytes::from(encode_tick_data(0)))];
        let tick_batch = crate::multicall3::encode_aggregate3(&tick_calls).unwrap();
        let ticks_return = {
            let mut data = vec![0u8; 64];
            data[16..32].copy_from_slice(&7u128.to_be_bytes());
            data[48..64].copy_from_slice(&3i128.to_be_bytes());
            data
        };
        let json = serde_json::json!({
            "chain_id": 1,
            "block_number": 42,
            "timestamp": 0,
            "calls": {
                call_key(crate::multicall3::MULTICALL3_ADDRESS, &bitmap_batch): aggregate_return(&[(true, U256::from(1).to_be_bytes::<32>().to_vec())]),
                call_key(crate::multicall3::MULTICALL3_ADDRESS, &tick_batch): aggregate_return(&[(true, ticks_return)])
            },
            "code": {}
        });
        let provider = crate::offline::OfflineProvider::from_json_str(&json.to_string())
            .unwrap()
            .as_alloy_provider();
        let map =
            LiquidityMap::tracked_with_spacing(HashMap::new(), HashMap::from([(0, U256::ZERO)]), 1);
        let facts = verify_liquidity_map(&provider, LiquidityMapTarget::V3(pool), &map, Some(42))
            .await
            .unwrap();
        assert!(facts.contains(&LiquidityMapDivergence::TickPresence {
            tick: 0,
            stored: false,
            observed: true,
        }));
    }

    #[tokio::test]
    async fn v4_discovers_onchain_only_tick_from_supplied_bitmap_word() {
        let manager = address!("0000000000000000000000000000000000000002");
        let pool_id = B256::with_last_byte(1);
        let state = v4_state_base_slot(pool_id);
        let bitmap_calldata = encode_extsload(&[v4_nested_slot(0, state + U256::from(5))]);
        let tick_calldata = encode_extsload(&[v4_nested_slot(0, state + U256::from(4))]);
        let mut packed = [0u8; 32];
        packed[16..32].copy_from_slice(&7u128.to_be_bytes());
        packed[31] = 3;
        let json = serde_json::json!({
            "chain_id": 1,
            "block_number": 42,
            "timestamp": 0,
            "calls": {
                call_key(manager, &bitmap_calldata): alloy::hex::encode(DynSolValue::Array(vec![DynSolValue::FixedBytes(B256::with_last_byte(1), 32)]).abi_encode()),
                call_key(manager, &tick_calldata): alloy::hex::encode(DynSolValue::Array(vec![DynSolValue::FixedBytes(B256::from(packed), 32)]).abi_encode())
            },
            "code": {}
        });
        let provider = crate::offline::OfflineProvider::from_json_str(&json.to_string())
            .unwrap()
            .as_alloy_provider();
        let map =
            LiquidityMap::tracked_with_spacing(HashMap::new(), HashMap::from([(0, U256::ZERO)]), 1);
        let facts = verify_liquidity_map(
            &provider,
            LiquidityMapTarget::V4 {
                pool_manager: manager,
                pool_id,
            },
            &map,
            Some(42),
        )
        .await
        .unwrap();
        assert!(facts.contains(&LiquidityMapDivergence::TickPresence {
            tick: 0,
            stored: false,
            observed: true,
        }));
    }

    #[tokio::test]
    async fn invalid_v3_v4_and_v4_bitmap_domains_are_rejected_before_reads() {
        let pool = address!("0000000000000000000000000000000000000001");
        let provider = crate::offline::OfflineProvider::from_json_str(
            &serde_json::json!({"chain_id": 1, "block_number": 42, "timestamp": 0, "calls": {}, "code": {}}).to_string(),
        )
        .unwrap()
        .as_alloy_provider();
        let v3 = LiquidityMap::tracked_with_spacing(
            HashMap::from([(887_273, tick(1, 0))]),
            HashMap::new(),
            1,
        );
        let error = verify_liquidity_map(&provider, LiquidityMapTarget::V3(pool), &v3, Some(42))
            .await
            .unwrap_err();
        assert!(
            matches!(error, LiquidityMapVerifyError::Decode { reason } if reason.contains("887273"))
        );

        let v3_word = LiquidityMap::tracked_with_spacing(
            HashMap::new(),
            HashMap::from([(32_768, U256::ZERO)]),
            1,
        );
        let error =
            verify_liquidity_map(&provider, LiquidityMapTarget::V3(pool), &v3_word, Some(42))
                .await
                .unwrap_err();
        assert!(
            matches!(error, LiquidityMapVerifyError::Decode { reason } if reason.contains("int16"))
        );

        let v4 = LiquidityMap::tracked_with_spacing(
            HashMap::from([(-887_273, tick(1, 0))]),
            HashMap::new(),
            1,
        );
        let manager = address!("0000000000000000000000000000000000000002");
        let error = verify_liquidity_map(
            &provider,
            LiquidityMapTarget::V4 {
                pool_manager: manager,
                pool_id: B256::with_last_byte(1),
            },
            &v4,
            Some(42),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, LiquidityMapVerifyError::Decode { reason } if reason.contains("-887273"))
        );

        let word = LiquidityMap::tracked_with_spacing(
            HashMap::new(),
            HashMap::from([(32_768, U256::ZERO)]),
            1,
        );
        let error = verify_liquidity_map(
            &provider,
            LiquidityMapTarget::V4 {
                pool_manager: manager,
                pool_id: B256::with_last_byte(1),
            },
            &word,
            Some(42),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, LiquidityMapVerifyError::Decode { reason } if reason.contains("int16"))
        );

        let negative_word = LiquidityMap::tracked_with_spacing(
            HashMap::new(),
            HashMap::from([(-32_769, U256::ZERO)]),
            1,
        );
        let error = verify_liquidity_map(
            &provider,
            LiquidityMapTarget::V4 {
                pool_manager: manager,
                pool_id: B256::with_last_byte(1),
            },
            &negative_word,
            Some(42),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, LiquidityMapVerifyError::Decode { reason } if reason.contains("int16"))
        );
    }

    #[tokio::test]
    async fn valid_negative_tick_boundaries_remain_accepted() {
        let provider = crate::offline::OfflineProvider::from_json_str(
            &serde_json::json!({"chain_id": 1, "block_number": 42, "timestamp": 0, "calls": {}, "code": {}}).to_string(),
        )
        .unwrap()
        .as_alloy_provider();
        let map = LiquidityMap::tracked_with_spacing(
            HashMap::from([(-887_272, tick(0, 0))]),
            HashMap::new(),
            1,
        );
        let error = verify_liquidity_map(
            &provider,
            LiquidityMapTarget::V3(address!("0000000000000000000000000000000000000001")),
            &map,
            Some(42),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, LiquidityMapVerifyError::Read { .. }));
    }

    #[tokio::test]
    async fn v3_reads_tick_rows_and_bitmap_through_existing_batch_behavior() {
        let pool = address!("0000000000000000000000000000000000000001");
        let calls = vec![
            (pool, Bytes::from(encode_tick_data(120))),
            (pool, Bytes::from(encode_tick_bitmap(0))),
        ];
        let batch = crate::multicall3::encode_aggregate3(&calls).unwrap();
        let ticks_return = {
            let mut data = vec![0u8; 64];
            data[16..32].copy_from_slice(&500u128.to_be_bytes());
            data[32..48].fill(0xff);
            data[48..64].copy_from_slice(&(-100i128).to_be_bytes());
            data
        };
        let bitmap_return = (U256::from(1u16) << 12usize).to_be_bytes::<32>().to_vec();
        let json = serde_json::json!({
            "chain_id": 1,
            "block_number": 42,
            "timestamp": 0,
            "calls": {call_key(crate::multicall3::MULTICALL3_ADDRESS, &batch): aggregate_return(&[(true, ticks_return), (true, bitmap_return)])},
            "code": {}
        });
        let provider = crate::offline::OfflineProvider::from_json_str(&json.to_string())
            .unwrap()
            .as_alloy_provider();
        let result =
            verify_liquidity_map(&provider, LiquidityMapTarget::V3(pool), &v3_map(), Some(42))
                .await;
        assert_eq!(result.unwrap(), Vec::<LiquidityMapDivergence>::new());
    }

    #[tokio::test]
    async fn v3_reverted_subcall_is_read_not_divergence() {
        let pool = address!("0000000000000000000000000000000000000001");
        let calls = vec![(pool, Bytes::from(encode_tick_data(0)))];
        let batch = crate::multicall3::encode_aggregate3(&calls).unwrap();
        let json = serde_json::json!({
            "chain_id": 1,
            "block_number": 42,
            "timestamp": 0,
            "calls": {call_key(crate::multicall3::MULTICALL3_ADDRESS, &batch): aggregate_return(&[(false, vec![])])},
            "code": {}
        });
        let provider = crate::offline::OfflineProvider::from_json_str(&json.to_string())
            .unwrap()
            .as_alloy_provider();
        let map =
            LiquidityMap::tracked_with_spacing(HashMap::from([(0, tick(1, 0))]), HashMap::new(), 1);
        let error = verify_liquidity_map(&provider, LiquidityMapTarget::V3(pool), &map, Some(42))
            .await
            .unwrap_err();
        assert!(matches!(error, LiquidityMapVerifyError::Read { .. }));
    }

    #[tokio::test]
    async fn v4_targets_pool_manager_and_uses_one_batched_extsload() {
        let manager = address!("0000000000000000000000000000000000000002");
        let pool_id = B256::with_last_byte(1);
        let state = v4_state_base_slot(pool_id);
        let slots = [
            v4_nested_slot(120, state + U256::from(4)),
            v4_nested_slot(0, state + U256::from(5)),
        ];
        let mut packed = [0u8; 32];
        packed[..16].fill(0xff);
        packed[15] = 0x9c;
        packed[30] = 1;
        packed[31] = 0xf4;
        let values = [
            B256::from(packed),
            B256::from((U256::from(1u16) << 12usize).to_be_bytes::<32>()),
        ];
        let return_data = DynSolValue::Array(
            values
                .iter()
                .map(|value| DynSolValue::FixedBytes(*value, 32))
                .collect(),
        )
        .abi_encode();
        let calldata = encode_extsload(&slots);
        let json = serde_json::json!({
            "chain_id": 1,
            "block_number": 42,
            "timestamp": 0,
            "calls": {call_key(manager, &calldata): alloy::hex::encode(return_data)},
            "code": {}
        });
        let provider = crate::offline::OfflineProvider::from_json_str(&json.to_string())
            .unwrap()
            .as_alloy_provider();
        let result = verify_liquidity_map(
            &provider,
            LiquidityMapTarget::V4 {
                pool_manager: manager,
                pool_id,
            },
            &v3_map(),
            Some(42),
        )
        .await;
        assert_eq!(result.unwrap(), Vec::<LiquidityMapDivergence>::new());
    }

    #[tokio::test]
    async fn v4_malformed_extsload_is_decode_not_divergence() {
        let manager = address!("0000000000000000000000000000000000000002");
        let pool_id = B256::with_last_byte(1);
        let state = v4_state_base_slot(pool_id);
        let calldata = encode_extsload(&[v4_nested_slot(0, state + U256::from(4))]);
        let json = serde_json::json!({
            "chain_id": 1,
            "block_number": 42,
            "timestamp": 0,
            "calls": {call_key(manager, &calldata): "00"},
            "code": {}
        });
        let provider = crate::offline::OfflineProvider::from_json_str(&json.to_string())
            .unwrap()
            .as_alloy_provider();
        let map =
            LiquidityMap::tracked_with_spacing(HashMap::from([(0, tick(1, 0))]), HashMap::new(), 1);
        let error = verify_liquidity_map(
            &provider,
            LiquidityMapTarget::V4 {
                pool_manager: manager,
                pool_id,
            },
            &map,
            Some(42),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, LiquidityMapVerifyError::Decode { .. }));
    }

    #[test]
    fn comparison_orders_presence_gross_net_then_bitmap() {
        let mut ticks = HashMap::new();
        ticks.insert(-1, tick(1, 2));
        ticks.insert(4, tick(3, 4));
        let mut bitmaps = HashMap::new();
        bitmaps.insert(2, U256::from(1));
        let map = LiquidityMap::tracked_with_spacing(ticks, bitmaps, 1);
        let mut observed = HashMap::new();
        observed.insert(-1, (U128::from(9), 2));
        observed.insert(8, (U128::from(8), 8));
        let observed_bitmaps = HashMap::from([(2, U256::from(2))]);
        assert_eq!(
            compare_maps(&map, &observed, &observed_bitmaps),
            vec![
                LiquidityMapDivergence::TickGross {
                    tick: -1,
                    expected: U128::from(1),
                    actual: U128::from(9)
                },
                LiquidityMapDivergence::TickPresence {
                    tick: 4,
                    stored: true,
                    observed: false
                },
                LiquidityMapDivergence::TickPresence {
                    tick: 8,
                    stored: false,
                    observed: true
                },
                LiquidityMapDivergence::BitmapWord {
                    word: 2,
                    expected: U256::from(1),
                    actual: U256::from(2)
                },
            ]
        );
    }

    #[test]
    fn v4_slots_match_independent_oracle_vectors() {
        let pool_id = B256::with_last_byte(1);
        let state = v4_state_base_slot(pool_id);
        assert_eq!(
            B256::from(state.to_be_bytes::<32>()),
            B256::from(hex!(
                "3e5fec24aa4dc4e5aee2e025e51e1392c72a2500577559fae9665c6d52bd6a31"
            ))
        );
        assert_eq!(
            v4_nested_slot(0, state + U256::from(4)),
            B256::from(hex!(
                "071baefdc11a1a736f835256c2bc394c68d5ec5a0621b837dd5a1614ef57dbb2"
            ))
        );
        assert_eq!(
            v4_nested_slot(-100, state + U256::from(4)),
            B256::from(hex!(
                "0f335c0565e65bd65f7ae80502d13da8188bd2e85f79878450decf77501242af"
            ))
        );
        assert_eq!(
            v4_nested_slot(0, state + U256::from(5)),
            B256::from(hex!(
                "6395bb2e70acf609fb69050da9fdfeb703963b9ae70f663b6a5d53a413ed6684"
            ))
        );
    }

    #[test]
    fn v4_packed_gross_net_and_bitmap_decode() {
        let mut bytes = [0u8; 32];
        bytes[..16].fill(0xff);
        bytes[15] = 0x9c;
        bytes[30] = 1;
        bytes[31] = 0xf4;
        assert_eq!(
            decode_v4_tick_slot(B256::from(bytes)),
            (U128::from(500), -100)
        );
        assert_eq!(
            U256::from_be_bytes::<32>(B256::with_last_byte(0xef).0),
            U256::from(0xef)
        );
    }
}
