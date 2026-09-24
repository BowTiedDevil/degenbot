//! Bot adapters for the shared contract-facing liquidity-map verifier.
//!
//! Pool lifecycle policy and bot error routing remain here. Contract reads,
//! canonical V4 storage slots, and typed map comparison are owned by
//! `degenbot-rpc::liquidity_verifier`.

use std::collections::HashSet;
use std::fmt::Write as _;

use alloy::primitives::{Address, U256};
use degenbot_core::{diag, op_info};
use degenbot_rpc::liquidity_verifier::{
    verify_liquidity_map, LiquidityMap, LiquidityMapDivergence, LiquidityMapTarget,
    LiquidityMapVerifyError,
};
use degenbot_rpc::provider::AlloyProvider;
use hashbrown::HashMap;

use crate::bot_core::{TickMap, V3PoolIdentity, V3PoolState, V4PoolIdentity, V4PoolState};

/// A single bot verification mismatch.
#[derive(Debug)]
pub struct VerificationMismatch {
    /// Human-readable mismatch description retained for bot error policy.
    pub message: String,
}

impl std::fmt::Display for VerificationMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for VerificationMismatch {}

/// Bot-facing classification between map evidence and inability to read it.
#[derive(Debug)]
pub enum LiquidityVerifyError {
    /// A typed map divergence, fatal to bot operation.
    Mismatch(VerificationMismatch),
    /// An RPC/read/decode failure, retained as transient bot verification
    /// policy input. The shared verifier keeps read versus decode distinct.
    Rpc { message: String },
}
impl std::fmt::Display for LiquidityVerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mismatch(error) => write!(f, "{error}"),
            Self::Rpc { message } => write!(f, "{message}"),
        }
    }
}
impl std::error::Error for LiquidityVerifyError {}

fn map_error(error: &LiquidityMapVerifyError) -> LiquidityVerifyError {
    LiquidityVerifyError::Rpc {
        message: error.to_string(),
    }
}

fn mismatch_message(
    target: &str,
    phase: &str,
    block: Option<u64>,
    fact: LiquidityMapDivergence,
) -> String {
    let block = block.map_or_else(|| "pending".to_string(), |value| value.to_string());
    match fact {
        LiquidityMapDivergence::TickPresence { tick, stored, observed } => format!(
            "{target} at {phase} block {block}: tick {tick} presence mismatch — snapshot: {stored}, on-chain: {observed}"
        ),
        LiquidityMapDivergence::TickGross { tick, expected, actual } => format!(
            "{target} at {phase} block {block}: tick {tick} liquidityGross mismatch — snapshot: {expected}, on-chain: {actual}"
        ),
        LiquidityMapDivergence::TickNet { tick, expected, actual } => format!(
            "{target} at {phase} block {block}: tick {tick} liquidityNet mismatch — snapshot: {expected}, on-chain: {actual}"
        ),
        LiquidityMapDivergence::BitmapWord { word, expected, actual } => format!(
            "{target} at {phase} block {block}: bitmap word {word} mismatch — snapshot: {expected}, on-chain: {actual}"
        ),
    }
}

fn first_mismatch(
    facts: &[LiquidityMapDivergence],
    target: &str,
    phase: &str,
    block: Option<u64>,
) -> Result<(), LiquidityVerifyError> {
    facts
        .first()
        .map(|fact| {
            LiquidityVerifyError::Mismatch(VerificationMismatch {
                message: mismatch_message(target, phase, block, *fact),
            })
        })
        .map_or(Ok(()), Err)
}

fn report_snapshot_facts(
    facts: &[LiquidityMapDivergence],
    target: &str,
    phase: &str,
    block: Option<u64>,
) {
    if facts.is_empty() {
        return;
    }
    let rows: Vec<String> = facts
        .iter()
        .map(|fact| mismatch_message(target, phase, block, *fact))
        .collect();
    diag!(
        domain = verify,
        %target,
        phase,
        block,
        divergence_count = facts.len(),
        facts = ?facts,
        rows = ?rows,
        "liquidity-map divergence set"
    );
}

fn report_live_facts<T: TickMap + ?Sized>(
    facts: &[LiquidityMapDivergence],
    pool: &T,
    target: &str,
    block: Option<u64>,
) {
    if facts.is_empty() {
        return;
    }
    let mut stored_ticks = String::new();
    let mut ticks: Vec<&i32> = pool.tick_data().keys().collect();
    ticks.sort_unstable();
    for tick in ticks {
        let info = &pool.tick_data()[tick];
        let _ = write!(
            stored_ticks,
            "{tick}:{},{};",
            info.liquidity_gross, info.liquidity_net
        );
    }
    let block_tag = block.map_or_else(|| "pending".to_string(), |value| value.to_string());
    op_info!(
        domain = verify,
        pool = %target,
        block_tag = %block_tag,
        tick_spacing = pool.tick_spacing(),
        active_tick = pool.active_tick(),
        update_block = pool.dbg_update_block(),
        journal_len = pool.dbg_journal_len(),
        total_ticks = pool.tick_data().len(),
        divergence_count = facts.len(),
        facts = ?facts,
        stored_ticks = %stored_ticks,
        "TICK-MAP DESYNC (divergence set)"
    );
    degenbot_core::diag_trace!(
        domain = verify,
        pool = %target,
        block_tag = %block_tag,
        stored_ticks = %stored_ticks,
        observed_facts = ?facts,
        "TICK-MAP DESYNC full divergence facts"
    );
}

fn map_facts(
    facts: &[LiquidityMapDivergence],
    target: &str,
    phase: &str,
    block: Option<u64>,
) -> Result<(), LiquidityVerifyError> {
    report_snapshot_facts(facts, target, phase, block);
    first_mismatch(facts, target, phase, block)
}

fn map_live_facts<T: TickMap + ?Sized>(
    facts: &[LiquidityMapDivergence],
    pool: &T,
    target: &str,
    phase: &str,
    block: Option<u64>,
    record_telemetry: bool,
) -> Result<(), LiquidityVerifyError> {
    report_live_facts(facts, pool, target, block);
    if record_telemetry {
        if let Some(fact) = facts.iter().find(|fact| {
            matches!(
                fact,
                LiquidityMapDivergence::TickGross { .. } | LiquidityMapDivergence::TickNet { .. }
            )
        }) {
            crate::telemetry::record_exception(
                crate::telemetry::error_kind::VERIFY_MISMATCH,
                format_args!("{}", mismatch_message(target, phase, block, *fact)),
            );
        }
    }
    first_mismatch(facts, target, phase, block)
}

fn map_from_ticks<S: std::hash::BuildHasher>(
    tick_data: &HashMap<i32, crate::bot_core::TickInfo, S>,
    tick_spacing: i32,
    active_tick: Option<i32>,
) -> LiquidityMap {
    let mut bitmaps: HashMap<i32, U256> = HashMap::new();
    let mut words = HashSet::new();
    if (1..=32_767).contains(&tick_spacing) {
        for &tick in tick_data.keys() {
            let compressed = tick.div_euclid(tick_spacing);
            let word = compressed >> 8;
            words.insert(word);
            let bit = compressed.rem_euclid(256) as u32;
            let entry = bitmaps.entry(word).or_insert(U256::ZERO);
            *entry |= U256::from(1u8) << bit;
        }
        if let Some(active_tick) = active_tick {
            let active_word = active_tick.div_euclid(tick_spacing) >> 8;
            for offset in -2..=2 {
                words.insert(active_word.saturating_add(offset));
            }
        }
    }
    for word in words {
        bitmaps.entry(word).or_insert(U256::ZERO);
    }
    let ticks = tick_data
        .iter()
        .map(|(&tick, info)| (tick, info.clone()))
        .collect();
    LiquidityMap::tracked_with_spacing(ticks, bitmaps, tick_spacing)
}

async fn verify_snapshot<S: std::hash::BuildHasher>(
    provider: &AlloyProvider,
    target: LiquidityMapTarget,
    ticks: &HashMap<i32, crate::bot_core::TickInfo, S>,
    tick_spacing: i32,
    block: Option<u64>,
    phase: &str,
    label: String,
) -> Result<(), LiquidityVerifyError> {
    // Snapshot lifecycle verifies the supplied tick rows only. It does not own a
    // separate bitmap-row map, so deriving words here would invent coverage and
    // issue reads the old snapshot lifecycle never made.
    let snapshot_ticks = ticks
        .iter()
        .map(|(&tick, info)| (tick, info.clone()))
        .collect();
    let map = LiquidityMap::tracked_with_spacing(snapshot_ticks, HashMap::new(), tick_spacing);
    let facts = verify_liquidity_map(provider, target, &map, block)
        .await
        .map_err(|error| map_error(&error))?;
    map_facts(&facts, &label, phase, block)
}

/// Verify a V3 snapshot map through the shared contract-facing verifier.
///
/// # Errors
///
/// Returns a bot mismatch or RPC error according to the shared verifier result.
pub async fn verify_v3_liquidity_map<S: std::hash::BuildHasher>(
    provider: &AlloyProvider,
    pool_address: Address,
    tick_data: &HashMap<i32, crate::bot_core::TickInfo, S>,
    tick_spacing: i32,
    block_number: u64,
    phase: &str,
) -> Result<(), LiquidityVerifyError> {
    verify_snapshot(
        provider,
        LiquidityMapTarget::V3(pool_address),
        tick_data,
        tick_spacing,
        Some(block_number),
        phase,
        format!("V3 pool {pool_address}"),
    )
    .await
}

/// Verify a V4 snapshot map through the shared contract-facing verifier. The
/// target is the `PoolManager` + `PoolId`; `StateView` is not a full-map target.
///
/// # Errors
///
/// Returns a bot mismatch or RPC error according to the shared verifier result.
pub async fn verify_v4_liquidity_map<S: std::hash::BuildHasher>(
    provider: &AlloyProvider,
    pool_manager: Address,
    pool_id: [u8; 32],
    tick_data: &HashMap<i32, crate::bot_core::TickInfo, S>,
    tick_spacing: i32,
    block_number: u64,
    phase: &str,
) -> Result<(), LiquidityVerifyError> {
    verify_snapshot(
        provider,
        LiquidityMapTarget::V4 {
            pool_manager,
            pool_id: pool_id.into(),
        },
        tick_data,
        tick_spacing,
        Some(block_number),
        phase,
        format!("V4 pool 0x{}", alloy::hex::encode_prefixed(pool_id)),
    )
    .await
}

/// Verify all V3 pool maps through the shared verifier.
///
/// # Errors
///
/// Returns the first pool mismatch or RPC error.
pub async fn verify_v3_pools<S: std::hash::BuildHasher>(
    provider: &AlloyProvider,
    _pool_address: Address,
    pools: &HashMap<u64, (V3PoolIdentity, V3PoolState), S>,
    block_number: Option<u64>,
) -> Result<(), LiquidityVerifyError> {
    for pool in pools.values() {
        verify_v3_pool(provider, pool, block_number).await?;
    }
    Ok(())
}

/// Verify a V3 pool map through the shared verifier.
///
/// # Errors
///
/// Returns the first map mismatch or RPC error.
pub async fn verify_v3_pool<T: TickMap + ?Sized>(
    provider: &AlloyProvider,
    pool: &T,
    block_number: Option<u64>,
) -> Result<(), LiquidityVerifyError> {
    let facts = verify_liquidity_map(
        provider,
        LiquidityMapTarget::V3(pool.address()),
        &map_from_ticks(
            pool.tick_data(),
            pool.tick_spacing(),
            Some(pool.active_tick()),
        ),
        block_number,
    )
    .await
    .map_err(|error| map_error(&error))?;
    map_live_facts(
        &facts,
        pool,
        &format!("V3 pool {}", pool.address()),
        "pool",
        block_number,
        true,
    )
}

/// Verify all V4 pool maps through the shared verifier.
///
/// # Errors
///
/// Returns the first pool mismatch or RPC error.
pub async fn verify_v4_pools<S: std::hash::BuildHasher>(
    provider: &AlloyProvider,
    pool_manager: Address,
    pools: &HashMap<u64, (V4PoolIdentity, V4PoolState), S>,
    block_number: Option<u64>,
) -> Result<(), LiquidityVerifyError> {
    let mut seen = HashSet::new();
    for pool in pools.values() {
        if seen.insert(pool.0.pool_id) {
            verify_v4_pool(provider, pool_manager, pool.0.pool_id, pool, block_number).await?;
        }
    }
    Ok(())
}

/// Verify a V4 pool map through the shared `PoolManager` + `PoolId` verifier.
///
/// # Errors
///
/// Returns the first map mismatch or RPC error.
pub async fn verify_v4_pool<T: TickMap + ?Sized>(
    provider: &AlloyProvider,
    pool_manager: Address,
    pool_id: [u8; 32],
    pool: &T,
    block_number: Option<u64>,
) -> Result<(), LiquidityVerifyError> {
    let facts = verify_liquidity_map(
        provider,
        LiquidityMapTarget::V4 {
            pool_manager,
            pool_id: pool_id.into(),
        },
        &map_from_ticks(
            pool.tick_data(),
            pool.tick_spacing(),
            Some(pool.active_tick()),
        ),
        block_number,
    )
    .await
    .map_err(|error| map_error(&error))?;
    map_live_facts(
        &facts,
        pool,
        &format!("V4 pool 0x{}", alloy::hex::encode_prefixed(pool_id)),
        "pool",
        block_number,
        false,
    )
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::bot_core::TickInfo;
    use alloy::dyn_abi::DynSolValue;
    use alloy::primitives::address;
    use degenbot_rpc::multicall3::{encode_aggregate3, MULTICALL3_ADDRESS};
    use degenbot_rpc::offline::OfflineProvider;
    use std::sync::Arc;

    struct ActiveWindowPool {
        ticks: HashMap<i32, TickInfo>,
    }

    impl TickMap for ActiveWindowPool {
        fn address(&self) -> Address {
            address!("0000000000000000000000000000000000000001")
        }

        fn tick_spacing(&self) -> i32 {
            1
        }

        fn active_tick(&self) -> i32 {
            0
        }

        fn tick_data(&self) -> &HashMap<i32, TickInfo> {
            &self.ticks
        }
    }

    #[tokio::test]
    async fn live_v3_verifier_checks_onchain_only_active_window_word() {
        let pool = ActiveWindowPool {
            ticks: HashMap::new(),
        };
        let address = pool.address();
        let words: Vec<i16> = (-2..=2).collect();
        let bitmap_calls: Vec<_> = words
            .iter()
            .map(|word| {
                (
                    address,
                    alloy::primitives::Bytes::from(degenbot_rpc::abi::encode_tick_bitmap(*word)),
                )
            })
            .collect();
        let bitmap_batch = encode_aggregate3(&bitmap_calls).unwrap();
        let bitmap_values = words
            .iter()
            .map(|word| {
                let value = if *word == 1 {
                    U256::from(1)
                } else {
                    U256::ZERO
                };
                (true, value.to_be_bytes::<32>().to_vec())
            })
            .collect::<Vec<_>>();
        let bitmap_return = DynSolValue::Array(
            bitmap_values
                .into_iter()
                .map(|(success, data)| {
                    DynSolValue::Tuple(vec![DynSolValue::Bool(success), DynSolValue::Bytes(data)])
                })
                .collect(),
        )
        .abi_encode();
        let tick_batch = encode_aggregate3(&[(
            address,
            alloy::primitives::Bytes::from(degenbot_rpc::abi::encode_tick_data(256)),
        )])
        .unwrap();
        let mut tick_data = vec![0u8; 64];
        tick_data[16..32].copy_from_slice(&9u128.to_be_bytes());
        tick_data[48..64].copy_from_slice(&4i128.to_be_bytes());
        let tick_return = DynSolValue::Array(vec![DynSolValue::Tuple(vec![
            DynSolValue::Bool(true),
            DynSolValue::Bytes(tick_data),
        ])])
        .abi_encode();
        let call_key = |calldata: &[u8]| {
            format!(
                "{}:0x{}",
                MULTICALL3_ADDRESS.to_checksum(None).to_lowercase(),
                alloy::hex::encode(calldata)
            )
        };
        let json = serde_json::json!({
            "chain_id": 1,
            "block_number": 42,
            "timestamp": 0,
            "calls": {
                call_key(&bitmap_batch): alloy::hex::encode(bitmap_return),
                call_key(&tick_batch): alloy::hex::encode(tick_return)
            },
            "code": {}
        });
        let provider = OfflineProvider::from_json_str(&json.to_string())
            .unwrap()
            .as_alloy_provider();
        let result = verify_v3_pool(&provider, &pool, Some(42)).await;
        assert!(matches!(result, Err(LiquidityVerifyError::Mismatch(_))));
    }

    #[tokio::test]
    async fn snapshot_verification_with_nonunit_spacing_checks_tick_rows_only() {
        let pool = address!("0000000000000000000000000000000000000001");
        let tick_call = degenbot_rpc::abi::encode_tick_data(120);
        let batch =
            encode_aggregate3(&[(pool, alloy::primitives::Bytes::from(tick_call))]).unwrap();
        let mut tick_return = vec![0u8; 64];
        tick_return[16..32].copy_from_slice(&7u128.to_be_bytes());
        tick_return[48..64].copy_from_slice(&3i128.to_be_bytes());
        let response = DynSolValue::Array(vec![DynSolValue::Tuple(vec![
            DynSolValue::Bool(true),
            DynSolValue::Bytes(tick_return),
        ])])
        .abi_encode();
        let call_key = format!(
            "{}:0x{}",
            MULTICALL3_ADDRESS.to_checksum(None).to_lowercase(),
            alloy::hex::encode(&batch)
        );
        let json = serde_json::json!({
            "chain_id": 1,
            "block_number": 42,
            "timestamp": 0,
            "calls": {call_key: alloy::hex::encode(response)},
            "code": {}
        });
        let provider = OfflineProvider::from_json_str(&json.to_string())
            .unwrap()
            .as_alloy_provider();
        let ticks = HashMap::from([(
            120,
            TickInfo {
                liquidity_gross: alloy::primitives::U128::from(7),
                liquidity_net: 3,
                block: 0,
            },
        )]);

        verify_snapshot(
            &provider,
            LiquidityMapTarget::V3(pool),
            &ticks,
            10,
            Some(42),
            "seed",
            format!("V3 pool {pool}"),
        )
        .await
        .unwrap();
    }

    #[derive(Clone, Default)]
    struct LogCapture(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for LogCapture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for LogCapture {
        type Writer = Self;

        fn make_writer(&'writer self) -> Self::Writer {
            self.clone()
        }
    }

    #[tokio::test]
    async fn live_v3_mismatch_logs_every_typed_fact() {
        let mut ticks = HashMap::new();
        ticks.insert(
            0,
            TickInfo {
                liquidity_gross: alloy::primitives::U128::from(1),
                liquidity_net: 0,
                block: 0,
            },
        );
        ticks.insert(
            1,
            TickInfo {
                liquidity_gross: alloy::primitives::U128::from(1),
                liquidity_net: 0,
                block: 0,
            },
        );
        let pool = ActiveWindowPool { ticks };
        let address = pool.address();
        let mut calls = vec![
            (
                address,
                alloy::primitives::Bytes::from(degenbot_rpc::abi::encode_tick_data(0)),
            ),
            (
                address,
                alloy::primitives::Bytes::from(degenbot_rpc::abi::encode_tick_data(1)),
            ),
        ];
        calls.extend((-2..=2).map(|word| {
            (
                address,
                alloy::primitives::Bytes::from(degenbot_rpc::abi::encode_tick_bitmap(word)),
            )
        }));
        let batch = encode_aggregate3(&calls).unwrap();
        let tick_return = |gross: u128| {
            let mut data = vec![0u8; 64];
            data[16..32].copy_from_slice(&gross.to_be_bytes());
            data
        };
        let response = DynSolValue::Array(vec![
            DynSolValue::Tuple(vec![
                DynSolValue::Bool(true),
                DynSolValue::Bytes(tick_return(2)),
            ]),
            DynSolValue::Tuple(vec![
                DynSolValue::Bool(true),
                DynSolValue::Bytes(tick_return(3)),
            ]),
            DynSolValue::Tuple(vec![
                DynSolValue::Bool(true),
                DynSolValue::Bytes(U256::ZERO.to_be_bytes::<32>().to_vec()),
            ]),
            DynSolValue::Tuple(vec![
                DynSolValue::Bool(true),
                DynSolValue::Bytes(U256::ZERO.to_be_bytes::<32>().to_vec()),
            ]),
            DynSolValue::Tuple(vec![
                DynSolValue::Bool(true),
                DynSolValue::Bytes(U256::from(3).to_be_bytes::<32>().to_vec()),
            ]),
            DynSolValue::Tuple(vec![
                DynSolValue::Bool(true),
                DynSolValue::Bytes(U256::ZERO.to_be_bytes::<32>().to_vec()),
            ]),
            DynSolValue::Tuple(vec![
                DynSolValue::Bool(true),
                DynSolValue::Bytes(U256::ZERO.to_be_bytes::<32>().to_vec()),
            ]),
        ])
        .abi_encode();
        let call_key = format!(
            "{}:0x{}",
            MULTICALL3_ADDRESS.to_checksum(None).to_lowercase(),
            alloy::hex::encode(&batch)
        );
        let json = serde_json::json!({
            "chain_id": 1,
            "block_number": 42,
            "timestamp": 0,
            "calls": {call_key: alloy::hex::encode(response)},
            "code": {}
        });
        let provider = OfflineProvider::from_json_str(&json.to_string())
            .unwrap()
            .as_alloy_provider();
        let capture = LogCapture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(capture.clone())
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let result = verify_v3_pool(&provider, &pool, Some(42)).await;
        assert!(matches!(result, Err(LiquidityVerifyError::Mismatch(_))));
        let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("TICK-MAP DESYNC"));
        assert!(logs.contains("divergence_count=2"));
        assert!(logs.contains("TickGross"));
    }
}
