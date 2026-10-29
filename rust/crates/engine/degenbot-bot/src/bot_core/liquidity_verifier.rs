//! Bot adapters for the shared contract-facing liquidity-map verifier.
//!
//! Pool lifecycle policy and bot error routing remain here. Contract reads,
//! canonical V4 storage slots, and typed map comparison are owned by
//! `degenbot-rpc::liquidity_verifier`.

use std::collections::HashSet;
use std::fmt::Write as _;

use alloy::primitives::U256;
use degenbot_core::{diag, op_info};
use degenbot_rpc::liquidity_verifier::{
    verify_liquidity_map, LiquidityMap, LiquidityMapDivergence, LiquidityMapTarget,
    LiquidityMapVerifyError,
};
use degenbot_rpc::provider::AlloyProvider;
use hashbrown::HashMap;

use crate::bot_core::registration_ledger::{RefusalKind, RegistrationPhase, VerifyOrigin};
use degenbot_pools::TickMap;
use degenbot_substrate::PoolLookup;

/// The bot-facing classification between map evidence and inability to read
/// it is the one [`RefusalKind`] vocabulary's verify arm: a typed map
/// divergence is the fatal tripwire ([`VerifyOrigin::Mismatch`]); an
/// RPC/read/decode failure is a transient transport refusal
/// ([`VerifyOrigin::Transport`]).
fn map_error(error: &LiquidityMapVerifyError) -> RefusalKind {
    RefusalKind::verify_failed(
        RegistrationPhase::Verify,
        false,
        VerifyOrigin::Transport,
        error.to_string(),
    )
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
) -> Result<(), RefusalKind> {
    facts
        .first()
        .map(|fact| {
            RefusalKind::verify_failed(
                RegistrationPhase::Verify,
                true,
                VerifyOrigin::Mismatch,
                mismatch_message(target, phase, block, *fact),
            )
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
) -> Result<(), RefusalKind> {
    report_snapshot_facts(facts, target, phase, block);
    first_mismatch(facts, target, phase, block)
}

fn map_live_facts<T: TickMap + ?Sized>(
    facts: &[LiquidityMapDivergence],
    pool: &T,
    target: &str,
    phase: &str,
    block: Option<u64>,
) -> Result<(), RefusalKind> {
    report_live_facts(facts, pool, target, block);
    // A gross/net divergence is the verify exception whichever family the key
    // names, so the metric is recorded here once, not per design.
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
    first_mismatch(facts, target, phase, block)
}

fn map_from_ticks<S: std::hash::BuildHasher>(
    tick_data: &HashMap<i32, degenbot_pools::TickInfo, S>,
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
    ticks: &HashMap<i32, degenbot_pools::TickInfo, S>,
    tick_spacing: i32,
    block: Option<u64>,
    phase: &str,
    label: String,
) -> Result<(), RefusalKind> {
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

/// Verify a pool's supplied tick rows — a snapshot seed or a post-drain pin —
/// against on-chain state at `block`, labelled with `phase`.
///
/// The key names the pool once (ADR-082): the contract target and the mismatch
/// label both come off it, so this body holds no design branch.
///
/// # Errors
///
/// Returns a bot mismatch or RPC error according to the shared verifier result.
pub async fn verify_snapshot_map<S: std::hash::BuildHasher>(
    provider: &AlloyProvider,
    key: PoolLookup,
    tick_data: &HashMap<i32, degenbot_pools::TickInfo, S>,
    tick_spacing: i32,
    block: u64,
    phase: &str,
) -> Result<(), RefusalKind> {
    verify_snapshot(
        provider,
        key.liquidity_map_target(),
        tick_data,
        tick_spacing,
        Some(block),
        phase,
        pool_label(key),
    )
    .await
}

/// Verify a pool's full live map against on-chain state: its tick rows, the
/// bitmap words those rows imply, and the ±2 word window around the active
/// tick (the read set a live pool's map needs; a snapshot verify reads the
/// supplied rows only).
///
/// The key names the pool once (ADR-082); the contract target comes off it.
///
/// # Errors
///
/// Returns a bot mismatch or RPC error according to the shared verifier result.
pub async fn verify_live_map<T: TickMap + ?Sized>(
    provider: &AlloyProvider,
    key: PoolLookup,
    pool: &T,
    block_number: Option<u64>,
) -> Result<(), RefusalKind> {
    let facts = verify_liquidity_map(
        provider,
        key.liquidity_map_target(),
        &map_from_ticks(
            pool.tick_data(),
            pool.tick_spacing(),
            Some(pool.active_tick()),
        ),
        block_number,
    )
    .await
    .map_err(|error| map_error(&error))?;
    map_live_facts(&facts, pool, &pool_label(key), "pool", block_number)
}

/// The label a mismatch message carries: the family word plus the key text (a
/// V3 address, or a V4 `pool_manager:pool_id` pair).
fn pool_label(key: PoolLookup) -> String {
    format!("{} pool {key}", key.family().tag().to_uppercase())
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use alloy::dyn_abi::DynSolValue;
    use alloy::primitives::{address, keccak256, Address, Bytes, B256};
    use degenbot_pools::TickInfo;
    use degenbot_rpc::multicall3::{encode_aggregate3, MULTICALL3_ADDRESS};
    use degenbot_rpc::offline::OfflineProvider;
    use degenbot_rpc::provider::AlloyProvider;
    use std::sync::Arc;

    /// A pool whose map is empty and whose active tick sits in bitmap word 0,
    /// so the live verify reads the ±2 word window around it and nothing else.
    struct ActiveWindowPool {
        ticks: HashMap<i32, TickInfo>,
    }

    impl ActiveWindowPool {
        const ADDRESS: Address = address!("0000000000000000000000000000000000000001");
    }

    impl TickMap for ActiveWindowPool {
        fn address(&self) -> Address {
            Self::ADDRESS
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

    /// A provider replaying the recorded call map for one block.
    fn offline_provider(calls: &serde_json::Value) -> AlloyProvider {
        let recorded = serde_json::json!({
            "chain_id": 1,
            "block_number": 42,
            "timestamp": 0,
            "calls": calls,
            "code": {}
        });
        OfflineProvider::from_json_str(&recorded.to_string())
            .unwrap()
            .as_alloy_provider()
    }

    fn call_key(to: Address, calldata: &[u8]) -> String {
        format!(
            "{}:0x{}",
            to.to_checksum(None).to_lowercase(),
            alloy::hex::encode(calldata)
        )
    }

    /// A Multicall3 aggregate return carrying one `(success, data)` tuple per
    /// call.
    fn aggregate_return(values: &[(bool, Vec<u8>)]) -> String {
        let tuples = values
            .iter()
            .map(|(success, data)| {
                DynSolValue::Tuple(vec![
                    DynSolValue::Bool(*success),
                    DynSolValue::Bytes(data.clone()),
                ])
            })
            .collect();
        alloy::hex::encode(DynSolValue::Array(tuples).abi_encode())
    }

    /// The `ticks(int24)` return: `liquidityGross`, then `liquidityNet`.
    fn tick_return(gross: u128, net: i128) -> Vec<u8> {
        let mut data = vec![0u8; 64];
        data[16..32].copy_from_slice(&gross.to_be_bytes());
        data[48..64].copy_from_slice(&net.to_be_bytes());
        data
    }

    /// On-chain truth for the empty active-window map: no tick row stored, and
    /// bitmap word 1 carrying bit 0 — an on-chain-only tick at 256.
    fn v3_active_window_truth() -> serde_json::Value {
        let pool = ActiveWindowPool::ADDRESS;
        let words: Vec<i16> = (-2..=2).collect();
        let bitmap_calls: Vec<_> = words
            .iter()
            .map(|word| {
                (
                    pool,
                    Bytes::from(degenbot_rpc::abi::encode_tick_bitmap(*word)),
                )
            })
            .collect();
        let bitmap_batch = encode_aggregate3(&bitmap_calls).unwrap();
        let bitmap_values: Vec<(bool, Vec<u8>)> = words
            .iter()
            .map(|word| {
                let value = if *word == 1 {
                    U256::from(1)
                } else {
                    U256::ZERO
                };
                (true, value.to_be_bytes::<32>().to_vec())
            })
            .collect();
        let tick_batch =
            encode_aggregate3(&[(pool, Bytes::from(degenbot_rpc::abi::encode_tick_data(256)))])
                .unwrap();
        serde_json::json!({
            call_key(MULTICALL3_ADDRESS, &bitmap_batch): aggregate_return(&bitmap_values),
            call_key(MULTICALL3_ADDRESS, &tick_batch): aggregate_return(&[(true, tick_return(9, 4))]),
        })
    }

    /// `Pool.State` lives at a `keccak256(pool_id, 6)` base slot, with the tick
    /// and bitmap mappings `+4` and `+5` from it — the same derivation the
    /// shared verifier issues its `extsload` reads from.
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

    fn extsload_calldata(slots: &[B256]) -> Bytes {
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

    fn extsload_return(values: &[B256]) -> String {
        let words = values
            .iter()
            .map(|value| DynSolValue::FixedBytes(*value, 32))
            .collect();
        alloy::hex::encode(DynSolValue::Array(words).abi_encode())
    }

    /// The V4 twin of [`v3_active_window_truth`]: bitmap word 1 carries bit 0
    /// in `Pool.State`, so the verifier discovers tick 256 and reads its slot.
    fn v4_active_window_truth(pool_manager: Address, pool_id: B256) -> serde_json::Value {
        let state = v4_state_base_slot(pool_id);
        let ticks_base = state + U256::from(4);
        let bitmap_base = state + U256::from(5);
        let words: Vec<i32> = (-2..=2).collect();
        let bitmap_slots: Vec<B256> = words
            .iter()
            .map(|word| v4_nested_slot(*word, bitmap_base))
            .collect();
        let bitmap_values: Vec<B256> = words
            .iter()
            .map(|word| {
                if *word == 1 {
                    B256::with_last_byte(1)
                } else {
                    B256::ZERO
                }
            })
            .collect();
        let tick_slots = [v4_nested_slot(256, ticks_base)];
        let mut packed = [0u8; 32];
        packed[..16].copy_from_slice(&4i128.to_be_bytes());
        packed[16..].copy_from_slice(&9u128.to_be_bytes());
        serde_json::json!({
            call_key(pool_manager, &extsload_calldata(&bitmap_slots)): extsload_return(&bitmap_values),
            call_key(pool_manager, &extsload_calldata(&tick_slots)): extsload_return(&[B256::from(packed)]),
        })
    }

    /// One table over the key shapes: the map stores no tick at 256 while the
    /// chain carries one, and both keys must classify that as the same fatal
    /// `Mismatch` — the verifier body holds no design branch.
    #[tokio::test]
    async fn live_map_mismatch_is_classified_identically_for_both_pool_keys() {
        let pool_manager = address!("0000000000000000000000000000000000000002");
        let pool_id = [0xabu8; 32];
        let cases = [
            (
                PoolLookup::from_address(ActiveWindowPool::ADDRESS),
                v3_active_window_truth(),
            ),
            (
                PoolLookup::V4 {
                    pool_manager,
                    pool_id,
                },
                v4_active_window_truth(pool_manager, B256::from(pool_id)),
            ),
        ];
        for (key, calls) in cases {
            let provider = offline_provider(&calls);
            let pool = ActiveWindowPool {
                ticks: HashMap::new(),
            };
            let result = verify_live_map(&provider, key, &pool, Some(42)).await;
            assert!(
                matches!(
                    result,
                    Err(RefusalKind::VerifyFailed {
                        fatal: true,
                        origin: VerifyOrigin::Mismatch,
                        ..
                    })
                ),
                "pool {key} must classify its divergence as the fatal mismatch"
            );
        }
    }

    /// A snapshot verify reads the supplied tick rows only — never a derived
    /// bitmap word — so a non-unit-spacing map compares row for row.
    #[tokio::test]
    async fn snapshot_verification_with_nonunit_spacing_checks_tick_rows_only() {
        let pool = ActiveWindowPool::ADDRESS;
        let tick_call = degenbot_rpc::abi::encode_tick_data(120);
        let batch = encode_aggregate3(&[(pool, Bytes::from(tick_call))]).unwrap();
        let response = aggregate_return(&[(true, tick_return(7, 3))]);
        let json = serde_json::json!({ call_key(MULTICALL3_ADDRESS, &batch): response });
        let provider = offline_provider(&json);
        let ticks = HashMap::from([(
            120,
            TickInfo {
                liquidity_gross: alloy::primitives::U128::from(7),
                liquidity_net: 3,
                block: 0,
            },
        )]);

        verify_snapshot_map(
            &provider,
            PoolLookup::from_address(pool),
            &ticks,
            10,
            42,
            "seed",
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

    /// A live mismatch logs every typed fact the divergence set carries — one
    /// diagnostic shape for every key, because the key only names the target.
    #[tokio::test]
    async fn live_map_mismatch_logs_every_typed_fact() {
        let mut ticks = HashMap::new();
        for tick in 0..=1 {
            ticks.insert(
                tick,
                TickInfo {
                    liquidity_gross: alloy::primitives::U128::from(1),
                    liquidity_net: 0,
                    block: 0,
                },
            );
        }
        let pool = ActiveWindowPool { ticks };
        let address = ActiveWindowPool::ADDRESS;
        let mut calls = vec![
            (address, Bytes::from(degenbot_rpc::abi::encode_tick_data(0))),
            (address, Bytes::from(degenbot_rpc::abi::encode_tick_data(1))),
        ];
        calls.extend((-2..=2).map(|word| {
            (
                address,
                Bytes::from(degenbot_rpc::abi::encode_tick_bitmap(word)),
            )
        }));
        let batch = encode_aggregate3(&calls).unwrap();
        let response = aggregate_return(&[
            (true, tick_return(2, 0)),
            (true, tick_return(3, 0)),
            (true, U256::ZERO.to_be_bytes::<32>().to_vec()),
            (true, U256::ZERO.to_be_bytes::<32>().to_vec()),
            (true, U256::from(3).to_be_bytes::<32>().to_vec()),
            (true, U256::ZERO.to_be_bytes::<32>().to_vec()),
            (true, U256::ZERO.to_be_bytes::<32>().to_vec()),
        ]);
        let json = serde_json::json!({ call_key(MULTICALL3_ADDRESS, &batch): response });
        let provider = offline_provider(&json);

        let capture = LogCapture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(capture.clone())
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let result = verify_live_map(
            &provider,
            PoolLookup::from_address(address),
            &pool,
            Some(42),
        )
        .await;
        assert!(matches!(
            result,
            Err(RefusalKind::VerifyFailed {
                fatal: true,
                origin: VerifyOrigin::Mismatch,
                ..
            })
        ));

        let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("TICK-MAP DESYNC"));
        assert!(logs.contains("divergence_count=2"));
        assert!(logs.contains("TickGross"));
    }
}
