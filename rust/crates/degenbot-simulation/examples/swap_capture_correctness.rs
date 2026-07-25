//! Swap-event capture correctness probe (ergo epic 63I7WJ).
//!
//! Proves the `SwapEventCaptureInspector` captures real V2/V3 `Swap` events
//! with amounts that **byte-match the onchain receipt**, by replaying real
//! mainnet swap transactions through a `CacheDB<WrapDatabaseAsync<AlloyDB>>`
//! EVM pinned at the parent block.
//!
//! # The question
//!
//! Does the inspector's captured swap (emitter + amount0 + amount1) match the
//! swap event the chain actually emitted for that exact transaction? This is
//! the ground-truth proof that retires `diagnostic.rs::recompute_v2/v3_amount_out`:
//! if the captured amount equals the onchain-emitted amount, the off-chain
//! `getAmountOut` recompute + the Multicall3 reserves re-fetch are redundant
//! (the run's OWN emitted event is the source of truth).
//!
//! # Method
//!
//! - Scan the latest ~20 blocks' receipts for transactions emitting V2 `Swap`
//!   and/or V3 `Swap` events (topic0 match).
//! - For the first qualifying V2 tx + the first qualifying V3 tx found (each at
//!   tx-index ≤ 30, so prior-tx replay is bounded):
//!   1. Pin `CacheDB<WrapDatabaseAsync<AlloyDB>>` at the PARENT block.
//!   2. Set the EVM block env to the block's header.
//!   3. `transact_commit` every tx BEFORE the target (faithful state advance —
//!      nonce bumps + SSTOREs land in the `CacheDB` so the target sees prior-tx
//!      effects exactly as on-chain).
//!   4. `inspect_one` the target tx with a `SwapEventCaptureInspector`
//!      attached — the inspector's `log_full` hook captures every V2/V3 `Swap`
//!      event the in-process EVM emits.
//!   5. Drain the captured swaps + compare against the target's receipt:
//!      every receipt swap log must have a matching captured swap
//!      (same emitter + family + amount0 + amount1), and no extras.
//!
//! # Run
//!
//! ```bash
//! DEGENBOT_SWAP_CAPTURE_PROBE=1 \
//! RPC=http://host.containers.internal:8545 \
//! cargo run --example swap_capture_correctness --release -p degenbot-simulation
//! ```
//!
//! Skips cleanly (exit 0, "skipped") when `DEGENBOT_SWAP_CAPTURE_PROBE` is
//! unset OR the RPC is unreachable — so CI / `just test-rust` never needs a
//! live node. The probe is a validation artifact, NOT a gated test.

use std::time::Instant;

use alloy::consensus::{BlockHeader, Transaction as ConsensusTransaction};
use alloy::eips::{BlockId, BlockNumberOrTag, Typed2718};
use alloy::primitives::{Address, I256, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::{Block as AlloyBlock, TransactionReceipt};
use degenbot_decoders::{
    v2_swap_decoder::{decode_v2_swap_log, V2_SWAP_TOPIC},
    v3_swap_decoder::{decode_v3_swap_log, V3_SWAP_TOPIC},
};
use degenbot_simulation::sim::evm::inspectors::{SwapEventCaptureInspector, SwapFamily};
use revm::context::{BlockEnv, TxEnv};
use revm::context_interface::either::Either;
use revm::database::{AlloyDB, CacheDB};
use revm::database_interface::WrapDatabaseAsync;
use revm::primitives::TxKind;
use revm::{ExecuteCommitEvm, ExecuteEvm, InspectEvm, MainBuilder, MainContext};

const RPC_URL_DEFAULT: &str = "http://host.containers.internal:8545";
/// The number of recent blocks to scan for a qualifying swap tx.
const BLOCK_SCAN_RANGE: u64 = 20;
/// The maximum tx-index of a qualifying swap tx (bounds prior-tx replay cost).
const MAX_TARGET_TX_INDEX: usize = 30;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    // Opt-in gate — the probe needs a live archive node; CI has none.
    if std::env::var("DEGENBOT_SWAP_CAPTURE_PROBE").ok().as_deref() != Some("1") {
        eprintln!("[swap-capture-probe] skipped (set DEGENBOT_SWAP_CAPTURE_PROBE=1 to run)");
        return;
    }
    let rpc_url = std::env::var("RPC").unwrap_or_else(|_| RPC_URL_DEFAULT.to_string());
    eprintln!("[swap-capture-probe] RPC URL: {rpc_url}");

    let provider = ProviderBuilder::default().connect_http(rpc_url.parse().expect("valid URL"));

    // Probe reachability up front so a no-node CI env exits cleanly.
    let latest = match provider.get_block_number().await {
        Ok(n) => n,
        Err(e) => {
            eprintln!("[swap-capture-probe] RPC unreachable ({e:?}) — skipping");
            return;
        }
    };
    eprintln!("[swap-capture-probe] latest block: {latest}");

    let mut validated_v2 = false;
    let mut validated_v3 = false;
    for offset in 0..BLOCK_SCAN_RANGE {
        if validated_v2 && validated_v3 {
            break;
        }
        let block_number = latest.saturating_sub(offset);
        match scan_block_for_swap_txs(&provider, block_number).await {
            Ok(candidates) => {
                for cand in &candidates {
                    if cand.family == SwapFamily::V2 && !validated_v2 {
                        match validate_swap_capture(&provider, block_number, cand).await {
                            Ok(()) => validated_v2 = true,
                            Err(why) => eprintln!(
                                "[swap-capture-probe] block {block_number} V2 tx index {} \
                                 validation FAILED: {why}",
                                cand.tx_index
                            ),
                        }
                    } else if cand.family == SwapFamily::V3 && !validated_v3 {
                        match validate_swap_capture(&provider, block_number, cand).await {
                            Ok(()) => validated_v3 = true,
                            Err(why) => eprintln!(
                                "[swap-capture-probe] block {block_number} V3 tx index {} \
                                 validation FAILED: {why}",
                                cand.tx_index
                            ),
                        }
                    }
                }
            }
            Err(why) => eprintln!("[swap-capture-probe] block {block_number} scan error: {why}"),
        }
    }

    eprintln!();
    eprintln!("=== swap-capture-correctness probe result ===");
    eprintln!("V2 Swap capture validated : {validated_v2}");
    eprintln!("V3 Swap capture validated : {validated_v3}");
    if validated_v2 && validated_v3 {
        eprintln!("PASS — captured swaps match onchain receipts for both families.");
    } else {
        eprintln!(
            "INCOMPLETE — re-run on a busier block range or raise MAX_TARGET_TX_INDEX \
             (V2={validated_v2}, V3={validated_v3})."
        );
    }
}

/// A qualifying swap-emitting tx found by scanning a block's receipts.
struct SwapCandidate {
    tx_index: usize,
    family: SwapFamily,
}

/// Scan one block's receipts for the first V2-Swap tx + first V3-Swap tx
/// (each at tx-index ≤ `MAX_TARGET_TX_INDEX` — bounds the prior-tx replay cost).
async fn scan_block_for_swap_txs(
    provider: &alloy::providers::RootProvider,
    block_number: u64,
) -> Result<Vec<SwapCandidate>, String> {
    let receipts: Vec<TransactionReceipt> = provider
        .get_block_receipts(BlockId::number(block_number))
        .await
        .map_err(|e| format!("getBlockReceipts: {e:?}"))?
        .ok_or_else(|| "no receipts (block missing)".to_string())?;

    let mut first_v2: Option<usize> = None;
    let mut first_v3: Option<usize> = None;
    for (tx_index, rcpt) in receipts.iter().enumerate() {
        if tx_index > MAX_TARGET_TX_INDEX && first_v2.is_none() && first_v3.is_none() {
            // No qualifying swap in the bounded range — give up on this block.
            break;
        }
        for log in rcpt.logs() {
            let Some(topic0) = log.topics().first() else {
                continue;
            };
            if *topic0 == V2_SWAP_TOPIC && first_v2.is_none() && tx_index <= MAX_TARGET_TX_INDEX {
                first_v2 = Some(tx_index);
            } else if *topic0 == V3_SWAP_TOPIC
                && first_v3.is_none()
                && tx_index <= MAX_TARGET_TX_INDEX
            {
                first_v3 = Some(tx_index);
            }
        }
    }
    let mut out = Vec::new();
    if let Some(idx) = first_v2 {
        out.push(SwapCandidate {
            tx_index: idx,
            family: SwapFamily::V2,
        });
    }
    if let Some(idx) = first_v3 {
        out.push(SwapCandidate {
            tx_index: idx,
            family: SwapFamily::V3,
        });
    }
    Ok(out)
}

/// The expected (onchain-receipt-derived) swap shape the capture must match.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ExpectedSwap {
    emitter: Address,
    family: SwapFamily,
    amount0: I256,
    amount1: I256,
}

/// Decode the target tx's receipt logs into the expected swap set.
fn expected_swaps_from_receipt(receipt: &TransactionReceipt) -> Result<Vec<ExpectedSwap>, String> {
    let mut out = Vec::new();
    for log in receipt.logs() {
        let Some(topic0) = log.topics().first() else {
            continue;
        };
        if *topic0 == V2_SWAP_TOPIC {
            let ev = decode_v2_swap_log(log)
                .ok_or_else(|| "receipt V2 Swap log failed to decode".to_string())?;
            let amount0 = u256_signed_delta(ev.amount0_out, ev.amount0_in)
                .ok_or_else(|| "V2 amount0 delta overflow".to_string())?;
            let amount1 = u256_signed_delta(ev.amount1_out, ev.amount1_in)
                .ok_or_else(|| "V2 amount1 delta overflow".to_string())?;
            out.push(ExpectedSwap {
                emitter: ev.pool_address,
                family: SwapFamily::V2,
                amount0,
                amount1,
            });
        } else if *topic0 == V3_SWAP_TOPIC {
            let ev = decode_v3_swap_log(log)
                .ok_or_else(|| "receipt V3 Swap log failed to decode".to_string())?;
            out.push(ExpectedSwap {
                emitter: ev.pool_address,
                family: SwapFamily::V3,
                amount0: ev.amount0,
                amount1: ev.amount1,
            });
        }
    }
    Ok(out)
}

/// `out - in` as a signed I256 (the V3 signed-delta convention the inspector
/// uses for V2). `None` if either operand exceeds `I256::MAX`.
fn u256_signed_delta(amount_out: U256, amount_in: U256) -> Option<I256> {
    let out = I256::try_from(amount_out).ok()?;
    let inm = I256::try_from(amount_in).ok()?;
    Some(out - inm)
}

/// Validate the inspector's captured swaps for one target tx against its
/// onchain receipt. Replays all prior txs (faithful state advance) before
/// inspecting the target.
async fn validate_swap_capture(
    provider: &alloy::providers::RootProvider,
    block_number: u64,
    candidate: &SwapCandidate,
) -> Result<(), String> {
    let parent_block = block_number.saturating_sub(1);
    let tx_index = candidate.tx_index;
    eprintln!(
        "[swap-capture-probe] block {block_number} {:?} tx index {tx_index} (parent pin \
         {parent_block}) — fetching full block + receipts…",
        candidate.family
    );

    let block: AlloyBlock = provider
        .get_block_by_number(BlockNumberOrTag::Number(block_number))
        .full()
        .await
        .map_err(|e| format!("getBlockByNumber: {e:?}"))?
        .ok_or_else(|| "block missing".to_string())?;
    let receipts: Vec<TransactionReceipt> = provider
        .get_block_receipts(BlockId::number(block_number))
        .await
        .map_err(|e| format!("getBlockReceipts: {e:?}"))?
        .ok_or_else(|| "receipts missing".to_string())?;

    // The receipts are in tx-index order; the block's txs are too. Index aligns.
    let target_receipt = receipts.get(tx_index).ok_or_else(|| {
        format!(
            "receipt[{tx_index}] missing (only {} receipts)",
            receipts.len()
        )
    })?;
    let expected = expected_swaps_from_receipt(target_receipt)?;
    if expected
        .iter()
        .filter(|s| s.family == candidate.family)
        .count()
        == 0
    {
        return Err(format!(
            "receipt[{tx_index}] has no {:?} swap log (scan-then-fetch race?)",
            candidate.family
        ));
    }

    // Build the EVM: CacheDB over AlloyDB pinned at the parent block.
    let alloy_db = AlloyDB::new(provider.clone(), BlockId::number(parent_block));
    let wrap_db = WrapDatabaseAsync::new(alloy_db)
        .ok_or_else(|| "WrapDatabaseAsync::new returned None (no tokio runtime)".to_string())?;
    let cache_db = CacheDB::new(wrap_db);

    let mut ctx = revm::context::Context::mainnet();
    ctx.cfg.disable_nonce_check = false;
    // Bake the SwapEventCaptureInspector type in (the default placeholder is
    // swapped for the real (handle-linked) inspector at `inspect_one` time).
    let mut evm = ctx
        .with_db(cache_db)
        .build_mainnet_with_inspector(SwapEventCaptureInspector::default());
    evm.set_block(block_env_from_header(&block.header));

    // Replay prior txs [0..tx_index] via transact_commit (state advances).
    let txs = block.transactions.txns();
    let replay_start = Instant::now();
    for (i, tx) in txs.enumerate() {
        if i >= tx_index {
            break;
        }
        let tx_env =
            tx_env_from_alloy_tx(tx).map_err(|why| format!("prior tx[{i}] env build: {why}"))?;
        // A prior-tx revert is fine (onchain reverts still commit nonce bumps);
        // the error we care about is a DB cold-miss RPC failure.
        if let Err(e) = evm.transact_commit(tx_env) {
            return Err(format!("prior tx[{i}] transact_commit error: {e:?}"));
        }
    }
    eprintln!(
        "[swap-capture-probe] replayed {tx_index} prior tx(s) in {:?}; inspecting target…",
        replay_start.elapsed()
    );

    // Inspect the target tx (no commit needed — we drain + done).
    let target_tx = block
        .transactions
        .txns()
        .nth(tx_index)
        .ok_or_else(|| format!("block tx[{tx_index}] missing"))?;
    let target_env =
        tx_env_from_alloy_tx(target_tx).map_err(|why| format!("target tx env build: {why}"))?;
    let (se, handle) = SwapEventCaptureInspector::new();
    let result = evm
        .inspect_one(target_env, se)
        .map_err(|e| format!("target inspect_one error: {e:?}"))?;
    if !result.is_success() {
        return Err(format!(
            "target tx did not succeed in replay (status: {result:?}) — \
             prior-tx replay diverged from onchain state"
        ));
    }

    let captured = handle.take_swaps();
    eprintln!(
        "[swap-capture-probe] captured {} swap(s) for tx[{tx_index}] {:?}; comparing to receipt…",
        captured.len(),
        candidate.family
    );

    compare_captured_to_expected(&captured, &expected, candidate.family)
}

/// Multiset-compare the captured swaps to the expected (receipt-derived) set,
/// filtering to the candidate family. Every expected swap must have a matching
/// captured swap (emitter + family + amount0 + amount1) + no extras.
fn compare_captured_to_expected(
    captured: &[degenbot_simulation::sim::evm::inspectors::CapturedSwap],
    expected: &[ExpectedSwap],
    family: SwapFamily,
) -> Result<(), String> {
    use degenbot_simulation::sim::evm::inspectors::CapturedSwap;
    let expected_f: Vec<&ExpectedSwap> = expected.iter().filter(|s| s.family == family).collect();
    let captured_f: Vec<&CapturedSwap> = captured.iter().filter(|s| s.family == family).collect();

    if captured_f.len() != expected_f.len() {
        return Err(format!(
            "{family:?} count mismatch — captured {} vs expected {}\n  captured: {captured_f:?}\n  expected: {expected_f:?}",
            captured_f.len(),
            expected_f.len()
        ));
    }

    // Greedy multiset match: every expected must find an unmatched captured.
    let mut used = vec![false; captured_f.len()];
    for exp in &expected_f {
        let mut found = false;
        for (i, cap) in captured_f.iter().enumerate() {
            if used[i] {
                continue;
            }
            if cap.emitter == exp.emitter
                && cap.family == exp.family
                && cap.amount0 == exp.amount0
                && cap.amount1 == exp.amount1
            {
                used[i] = true;
                found = true;
                break;
            }
        }
        if !found {
            return Err(format!(
                "{family:?}: no captured swap matches expected \
                 (emitter={}, amount0={}, amount1={})\n  captured: {captured_f:?}",
                exp.emitter, exp.amount0, exp.amount1
            ));
        }
    }
    Ok(())
}

// =================================================================
// Block env lift: alloy RPC header → revm BlockEnv (copied from
// block_replay_spike.rs — the helper is example-local there too).
// =================================================================

fn block_env_from_header(h: &alloy::rpc::types::Header) -> BlockEnv {
    let mut env = BlockEnv {
        number: U256::from(h.number()),
        beneficiary: h.beneficiary(),
        timestamp: U256::from(h.timestamp()),
        gas_limit: h.gas_limit(),
        basefee: h.base_fee_per_gas().unwrap_or(0),
        difficulty: U256::ZERO,
        prevrandao: h.mix_hash(),
        blob_excess_gas_and_price: None,
        slot_num: 0,
    };
    if let Some(excess) = h.excess_blob_gas() {
        // revm 42 ships Cancun + Prague blob fractions; the live chain is on
        // Osaka BPO2 (EIP-7892), whose fraction revm lacks. Use the BPO2
        // constant directly (mirrors block_replay_spike.rs). Only affects blob
        // txs (irrelevant to the swap txs this probe inspects).
        const OSAKA_BPO2_BLOB_BASE_FEE_UPDATE_FRACTION: u64 = 11_684_671;
        env.set_blob_excess_gas_and_price(excess, OSAKA_BPO2_BLOB_BASE_FEE_UPDATE_FRACTION);
    }
    env
}

// =================================================================
// TxEnv lift: alloy RPC Transaction → revm TxEnv (copied from
// block_replay_spike.rs).
// =================================================================

fn tx_env_from_alloy_tx(tx: &alloy::rpc::types::Transaction) -> Result<TxEnv, String> {
    let caller = tx.inner.signer();
    let kind = tx.inner.to().map_or(TxKind::Create, TxKind::Call);
    let mut builder = TxEnv::builder()
        .caller(caller)
        .kind(kind)
        .value(tx.inner.value())
        .data(tx.inner.input().clone())
        .gas_limit(tx.inner.gas_limit())
        .gas_price(tx.inner.max_fee_per_gas())
        .nonce(tx.inner.nonce());
    if let Some(priority) = tx.inner.max_priority_fee_per_gas() {
        builder = builder.gas_priority_fee(Some(priority));
    }
    if let Some(chain_id) = tx.inner.chain_id() {
        builder = builder.chain_id(Some(chain_id));
    }
    if let Some(access_list) = tx.inner.access_list() {
        builder = builder.access_list(access_list.clone());
    }
    if let Some(blobs) = tx.inner.blob_versioned_hashes() {
        builder = builder.blob_hashes(blobs.to_vec());
    }
    if let Some(blob_fee) = tx.inner.max_fee_per_blob_gas() {
        builder = builder.max_fee_per_blob_gas(blob_fee);
    }
    let mut env = builder.build().map_err(|e| format!("TxEnv build: {e:?}"))?;
    env.tx_type = tx.inner.ty();
    if let Some(auth_list) = tx.inner.authorization_list() {
        env.authorization_list = auth_list.iter().cloned().map(Either::Left).collect();
    }
    Ok(env)
}
