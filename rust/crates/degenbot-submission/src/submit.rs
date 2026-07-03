//! The `dispatch_and_submit` orchestration + `eth_sendRawTransaction`
//! broadcast + `eth_feeHistory` percentile fetch (the N6 + I1/I2 rows of the
//! `SHT6GE` submission epic).
//!
//! Ports `examples/eth_backrun_v2_v3_v4_rust.py::dispatch_profitable_results`
//! submit tail (L2608–L2660 — the `dry_run` guard L2608, `INJECT_EXECUTOR_CODE`
//! guard L2666, mutual-exclusivity guard L2622, claim nonce L2636, finalize
//! fees L2637–L2639, re-compute access list L2641–L2645, sign+broadcast
//! L2647–L2660, reserve pools L2662, spawn monitor L2664–L2672) + the
//! `_apply_block_if_ready` `eth_feeHistory` fetch (L2907–L2923).
//!
//! This is the orchestration that owns the GIL release across the async
//! sign+broadcast slice (ADR-005 §3 — "Rust is the engine, Python is the
//! cockpit"). Owning the sign+broadcast in Rust releases the GIL across the
//! per-tx RPCs (the §2.1 "GIL?" win).
//!
//! # Dispositions (per the `P7AMWR` scope rubric)
//!
//! - **N6 `port-now`** — the submit orchestration (this leaf). Sorts by net
//!   profit descending; for each: mutual-exclusivity guard → `dry_run`/
//!   `inject_code` guard (typed Skip) → claim nonce → finalize fees →
//!   re-compute access list → sign → broadcast → reserve pools → spawn
//!   monitor.
//! - **I1 `done`-reference** — [`AlloyProvider::eth_send_raw_transaction`] (the
//!   ZUZANP typed `bytes → B256` surface — committed `d26b8248`). CONSUMED,
//!   no `make_request` escape hatch (the interim is over).
//! - **I2 `done`-reference** — [`AlloyProvider::eth_fee_history`] (the ZUZANP
//!   typed surface) + [`AlloyProvider::eth_create_access_list`]
//!   (consumed by the N6 access-list re-computation).
//! - **S3 `stays-python`** — the `dry_run`/`INJECT_EXECUTOR_CODE` POLICY (the
//!   live-submission skip as a safety policy) stays Python; this leaf takes
//!   them as `bool` args (config, not policy).

// Solidity/EVM + Rust-ecosystem identifiers (eth_sendRawTransaction,
// eth_feeHistory, EIP-1559, maxFeePerGas, etc.) are ubiquitous here.
#![allow(clippy::doc_markdown)]

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use alloy::primitives::{Address, Bytes, B256, U256};
use alloy::rpc::types::TransactionRequest;
use degenbot_rpc::provider::AlloyProvider;

use crate::dispatcher::{Dispatcher, PoolKey};
use crate::fee::finalize_fees;
use crate::monitor::{monitor_pending_transaction_default, ReceiptProbe, SubmittedTx};
use crate::params::TxParams;
use crate::signer::TxSigner;

// ─────────────────────────────────────────────────────────────────────────
// The submit candidate (decoupled from the Simulation `SimResult`)
// ─────────────────────────────────────────────────────────────────────────

/// A pre-submit candidate — the profitable sim result + the resolved path
/// pools (the keys for mutual-exclusivity).
///
/// Decoupled from `degenbot_simulation::SimResult` to avoid a
/// `degenbot-submission → degenbot-simulation` dependency (which would close a
/// `degenbot-simulation → degenbot-submission → degenbot-simulation` cycle —
/// the simulation crate already depends on submission for `PathSuppression`).
/// The submission crate owns its OWN input shape; the umbrella `Bot` / Python
/// driver maps `SimResult` → `SubmitCandidate` at the seam (resolving the
/// `path_pools` from `path_info.hops` — `pool_id_hex` for V4, `pool_address`
/// for V2/V3 — ports L2623).
///
/// Ports the Python `gas_profitable` tuple `(path_id, gross, net, gas,
/// tx_params, path_info)` (L2428 — the dispatch fan-out output the submit loop
/// iterates).
#[derive(Debug, Clone)]
pub struct SubmitCandidate {
    /// `path_id` — the unique arb path identifier.
    pub path_id: u64,
    /// Gross on-chain profit (wei) — `(weth+eth+erc6909)_after - _before`.
    pub gross_profit: U256,
    /// Net profit = `gross - gas*(base_fee_next + priority_fee)` (wei).
    pub net_profit: U256,
    /// The simulate's `gasUsed` for the `execute()` call (UN-inflated).
    pub gas_used: u64,
    /// The market-aware priority fee (the `_compute_priority_fee` output).
    pub priority_fee: u128,
    /// The base fee of the next block (`base_fee_next`).
    pub base_fee_next: u128,
    /// The `execute()` calldata (selector + ABI-wrapped `(bytes, uint256)`).
    pub execute_calldata: Bytes,
    /// The `to` address (the executor contract).
    pub executor_address: Address,
    /// The pre-sim access list (the V2/V3 slot reads warmup). Re-computed by
    /// the submit orchestration with the updated nonce/fees for accuracy —
    /// if that re-computation fails, this list is kept (ports the
    /// `except Exception: pass` guard, L2644).
    pub access_list: Option<alloy::rpc::types::AccessList>,
    /// The pools this path touches (for mutual-exclusivity — ports the Python
    /// `{h.pool_id_hex if V4 else h.pool_address for h in path_info.hops}`).
    pub path_pools: HashSet<PoolKey>,
}

// ─────────────────────────────────────────────────────────────────────────
// The submit outcome (N6)
// ─────────────────────────────────────────────────────────────────────────

/// The typed Skip reason for a not-submitted candidate (ports the
/// `dry_run`/`inject_code`/mutual-exclusivity skip branches L2608/L2666/L2626).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// Pools already claimed by an in-flight or committed tx this batch (the
    /// `is_path_blocked` guard, L2626).
    PoolsClaimed,
    /// `dry_run` is active — the live submission is skipped (the `dry_run`
    /// guard, L2608). Pools are still committed to (so the dry-run respects
    /// mutual exclusivity).
    DryRun,
    /// `inject_code` is active — the injected contract doesn't exist on-chain,
    /// so live submission is unsafe (the `INJECT_EXECUTOR_CODE` guard, L2666).
    InjectCode,
    /// The broadcast RPC failed (the `Web3Exception` skip, L2654 — corresponds
    /// to [`AlloyProvider::eth_send_raw_transaction`] returning `Err`).
    BroadcastFailed(String),
}

/// The per-candidate submit record (ports the loop's submit + skip outcomes).
#[derive(Debug, Clone)]
pub enum SubmitRecord {
    /// The tx was broadcast — the resulting `tx_hash` + claimed `nonce`.
    Submitted {
        path_id: u64,
        tx_hash: B256,
        nonce: u64,
    },
    /// The candidate was skipped — the typed reason.
    Skipped {
        path_id: u64,
        reason: SkipReason,
    },
}

/// The dispatch+submit outcome — the per-candidate records (ports the loop's
/// accumulated `submitted`/`skipped` tallies the Python logs as the
/// `[dispatch]` summary).
#[derive(Debug, Default)]
pub struct SubmitOutcome {
    /// The per-candidate records, in submit order (profit-descending).
    pub records: Vec<SubmitRecord>,
}

impl SubmitOutcome {
    /// Count of candidates actually broadcast (ports `len(submitted)`).
    #[must_use]
    pub fn submitted_count(&self) -> usize {
        self.records
            .iter()
            .filter(|r| matches!(r, SubmitRecord::Submitted { .. }))
            .count()
    }

    /// Count of candidates skipped (ports the `continue` branches).
    #[must_use]
    pub fn skipped_count(&self) -> usize {
        self.records
            .iter()
            .filter(|r| matches!(r, SubmitRecord::Skipped { .. }))
            .count()
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The submit orchestration (N6)
// ─────────────────────────────────────────────────────────────────────────

/// The 1.5× gas safety margin (`tx_params["gas"] = int(gas_used * 1.5)`,
/// `examples/eth_backrun_v2_v3_v4_rust.py` L2419). Applied to the simulate's
/// `gasUsed` when building the `TxParams`.
const GAS_SAFETY_MARGIN: f64 = 1.5;

/// Sort + submit the gas-profitable candidates with mutual exclusivity.
///
/// Pipeline (ports L2608–L2672):
/// 1. **Sort** by net profit descending (the dispatch fan-out's
///    `gas_profitable.sort(key=net, reverse=True)` output ordering — L2561;
///    re-asserted here so a caller handing un-sorted candidates still
///    submits best-first).
/// 2. For each candidate:
///    a. **Mutual-exclusivity guard** — [`Dispatcher::is_path_blocked`] against
///       `pending_pools` + the loop-local `committed_pools`. Skip with
///       [`SkipReason::PoolsClaimed`] if blocked (L2626).
///    b. **`dry_run` guard** — if `dry_run`, commit the pools to the local set
///       (so the dry-run respects mutual exclusivity) + skip with
///       [`SkipReason::DryRun`] (L2608/L2664).
///    c. **`inject_code` guard** — if `inject_code`, commit the pools + skip
///       with [`SkipReason::InjectCode`] (L2666/L2671 — the injected contract
///       doesn't exist on-chain, so live submission is unsafe).
///    d. **Claim nonce** — [`Dispatcher::claim_nonce`] scanning from
///       `operator_nonce` (L2636).
///    e. **Finalize fees** — [`finalize_fees`] sets `maxPriorityFeePerGas` +
///       `maxFeePerGas` (= `int(1.5*base_fee_next) + priority_fee`, L2637–
///       L2639).
///    f. **Re-compute access list** — [`AlloyProvider::eth_create_access_list`]
///       with the updated nonce/fees for accuracy (L2641–L2645; failure is
///       tolerated — the pre-sim access list from the candidate is kept).
///    g. **Sign** — [`TxSigner::sign_eip1559`] produces the raw `Typed2718`
///       bytes (L2647 — RFC 6979 deterministic ECDSA, `eth_account`-parity).
///    h. **Broadcast** — [`AlloyProvider::eth_send_raw_transaction`] returns
///       the `tx_hash` (L2648–L2654; failure → [`SkipReason::BroadcastFailed`],
///       the nonce + pools are NOT released — they'll be released by the
///       monitor's expiry path or a manual cleanup).
///    i. **Reserve pools** — [`Dispatcher::reserve_pools`] + commit to the
///       local `committed_pools` (L2662/L2671).
///    j. **Spawn monitor** — [`monitor_pending_transaction_default`] tracked
///       by [`Dispatcher::track_task`] (L2664–L2672; the monitor releases the
///       nonce + pools on confirm/expire).
///
/// `dispatcher` is shared via `Arc<Mutex<Dispatcher>>` (the standard sharing
/// pattern — the monitor reads `current_block` + releases the tx across the
/// consumer/monitor boundary). The lock is held ONLY for the synchronous
/// `is_path_blocked`/`claim_nonce`/`reserve_pools`/`track_task` calls — NEVER
/// across the `.await` RPCs (sign/broadcast/access-list) so the monitor is
/// never blocked.
///
/// `probe` is the [`ReceiptProbe`] the spawned monitor tasks poll for tx
/// confirmation. Passed as `Arc<dyn ReceiptProbe + Send + Sync>` so the
/// `'static` spawned tasks can clone the handle into the monitor (the umbrella
/// `Bot` builds a probe wrapping `AlloyProvider::get_transaction_receipt`).
///
/// # §4.2 parity
///
/// The submit order (net-desc), the mutual-exclusivity skip, the
/// `dry_run`/`inject_code` skip (with pools still committed), the
/// nonce→fee→access-list→sign→broadcast sequence, + the monitor spawn match
/// the Python oracle's `dispatch_profitable_results` submit tail.
///
/// # Errors
///
/// Returns `Err` only on an unrecoverable signer failure (the ECDSA
/// `sign_eip1559` `?` — a corrupt key; never for a validly-constructed
/// signer). Per-candidate RPC failures are tolerated as
/// [`SkipReason::BroadcastFailed`] records (ports the `continue` on
/// `Web3Exception`).
pub async fn dispatch_and_submit(
    mut candidates: Vec<SubmitCandidate>,
    dispatcher: &Arc<Mutex<Dispatcher>>,
    provider: &AlloyProvider,
    signer: &TxSigner,
    probe: Arc<dyn ReceiptProbe + Send + Sync>,
    operator_nonce: u64,
    current_block: u64,
    dry_run: bool,
    inject_code: bool,
) -> Result<SubmitOutcome, crate::SubmissionError> {
    // 1. Sort by net profit descending (the dispatch fan-out's output ordering
    //    — re-asserted so a caller handing un-sorted candidates submits
    //    best-first). Ports L2561's `gas_profitable.sort(key=net, reverse=...)`.
    candidates.sort_by_key(|c| std::cmp::Reverse(c.net_profit));

    let mut outcome = SubmitOutcome::default();
    // The loop-local committed-pools set (ports `committed_pools` — the pools
    // claimed by submitted + dry-run/inject-skipped candidates this batch).
    let mut committed_pools: HashSet<PoolKey> = HashSet::new();

    for candidate in candidates {
        let path_pools = candidate.path_pools.clone();

        // 2a. Mutual-exclusivity guard (L2626). Lock briefly — no .await.
        let blocked = {
            let d = dispatcher
                .lock()
                .expect("dispatcher mutex poisoned");
            d.is_path_blocked(&path_pools, &committed_pools)
        };
        if blocked {
            outcome.records.push(SubmitRecord::Skipped {
                path_id: candidate.path_id,
                reason: SkipReason::PoolsClaimed,
            });
            continue;
        }

        // 2b. dry_run guard (L2608). Commit pools (dry-run respects mutual
        //     exclusivity) + skip.
        if dry_run {
            committed_pools.extend(path_pools.clone());
            outcome.records.push(SubmitRecord::Skipped {
                path_id: candidate.path_id,
                reason: SkipReason::DryRun,
            });
            continue;
        }

        // 2c. inject_code guard (L2666). The injected contract doesn't exist
        //     on-chain, so live submission is unsafe. Commit pools + skip.
        if inject_code {
            committed_pools.extend(path_pools.clone());
            outcome.records.push(SubmitRecord::Skipped {
                path_id: candidate.path_id,
                reason: SkipReason::InjectCode,
            });
            continue;
        }

        // 2d. Claim nonce (L2636). Lock briefly — no .await.
        let nonce = {
            let mut d = dispatcher
                .lock()
                .expect("dispatcher mutex poisoned");
            d.claim_nonce(operator_nonce)
        };

        // 2e. Build the TxParams + finalize fees (L2637–L2639).
        //     `gas_limit = int(gas_used * 1.5)` — the 1.5× safety margin
        //     (L2419). The truncate semantics: `gas_used as f64 * 1.5 as u64`
        //     matches Python `int(gas_used * 1.5)` (toward-zero) for any
        //     realistic gas value.
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let gas_limit = (candidate.gas_used as f64 * GAS_SAFETY_MARGIN) as u64;
        let mut tx_params = TxParams::new(
            candidate.executor_address,
            candidate.execute_calldata.clone(),
            gas_limit,
            nonce,
        );
        // Carry the pre-sim access list (re-computed below for accuracy).
        if let Some(al) = &candidate.access_list {
            tx_params.access_list = al.clone();
        }
        finalize_fees(
            &mut tx_params,
            candidate.base_fee_next,
            candidate.priority_fee,
        )?;

        // 2f. Re-compute access list with updated nonce/fees (L2641–L2645).
        //     Failure is tolerated — the pre-sim access list is kept. Builds a
        //     TransactionRequest from the finalized TxParams. Lock-free — the
        //     RPC is a pure provider call.
        let request = build_transaction_request(&tx_params);
        match provider.eth_create_access_list(&request, None).await {
            Ok(al_result) => {
                tx_params.access_list = al_result.access_list;
            }
            Err(_e) => {
                // Tolerated — keep the pre-sim access list (ports the
                // `except Exception as al_exc: ... pass` guard, L2644).
            }
        }

        // 2g. Sign (L2647). Synchronous ECDSA — no .await, no lock.
        let raw_signed = signer.sign_eip1559(&tx_params)?;

        // 2h. Broadcast (L2648–L2654). Lock-free — pure provider call.
        let tx_hash = match provider.eth_send_raw_transaction(&raw_signed).await {
            Ok(hash) => hash,
            Err(e) => {
                // The broadcast failed — skip with the typed reason. The
                // claimed nonce + pools are NOT released here (ports the
                // `continue` on Web3Exception — the nonce is leaked until a
                // manual cleanup or the dispatcher's reap). The monitor is
                // NOT spawned (no tx to track).
                outcome.records.push(SubmitRecord::Skipped {
                    path_id: candidate.path_id,
                    reason: SkipReason::BroadcastFailed(format!("{e}")),
                });
                continue;
            }
        };

        // 2i. Reserve pools (L2662) + commit to the local set.
        {
            let mut d = dispatcher
                .lock()
                .expect("dispatcher mutex poisoned");
            d.reserve_pools(path_pools.clone());
        }
        committed_pools.extend(path_pools.clone());

        outcome.records.push(SubmitRecord::Submitted {
            path_id: candidate.path_id,
            tx_hash,
            nonce,
        });

        // 2j. Spawn the monitor (L2664–L2672). The dispatcher handle is cloned
        //     into the spawned task (the `Arc<Mutex<Dispatcher>>` sharing
        //     pattern). The monitor releases the nonce + pools on
        //     confirm/expire.
        let dispatcher_clone = Arc::clone(dispatcher);
        let probe_clone = Arc::clone(&probe);
        let submitted_tx = SubmittedTx::new(tx_hash, nonce, path_pools, current_block);
        dispatcher
            .lock()
            .expect("dispatcher mutex poisoned")
            .track_task(async move {
                let _ = monitor_pending_transaction_default(
                    submitted_tx,
                    &*probe_clone,
                    &dispatcher_clone,
                )
                .await;
            });
    }

    Ok(outcome)
}

// ─────────────────────────────────────────────────────────────────────────
// The fee-history fetch (I2)
// ─────────────────────────────────────────────────────────────────────────

/// Fetch the per-block priority-fee percentiles via `eth_feeHistory` (typed
/// ZUZANP surface) + record them into the dispatcher's `block_priority_fees`
/// ring (ports L2907–L2923).
///
/// Requests `block_count` blocks ending at `last_block` with the given
/// `reward_percentiles` (e.g. `[10.0, 50.0]` for the p10/p50 the
/// `_compute_priority_fee` bounds use). Extracts the LAST block's reward
/// vector (alloy returns `reward: Option<Vec<Vec<u128>>>` where `reward[i]`
/// is block `i`'s percentile rewards) zipped with the percentile keys into a
/// `BTreeMap<percentile, fee>` + feeds `Dispatcher::record_priority_fees`.
///
/// On any RPC failure the call is a no-op (ports the `except Web3Exception:
/// pass` guard, L2921-2922) — the dispatcher keeps its previous fee samples.
///
/// # Errors
///
/// Never returns `Err` — failures are tolerated (the fee history is
/// advisory; the previous samples remain valid). Returns `true` if recorded,
/// `false` if skipped (RPC failure or empty rewards).
pub async fn fetch_fee_history(
    provider: &AlloyProvider,
    dispatcher: &Arc<Mutex<Dispatcher>>,
    block_count: u64,
    last_block: u64,
    reward_percentiles: &[f64],
) -> bool {
    let history = match provider
        .eth_fee_history(
            block_count,
            alloy::rpc::types::BlockNumberOrTag::Number(last_block),
            reward_percentiles,
        )
        .await
    {
        Ok(h) => h,
        Err(_) => return false,
    };

    // alloy returns `reward: Option<Vec<Vec<u128>>>` where `reward[i]` is the
    // i-th block's per-percentile rewards. The Python takes `reward[-1]` (the
    // last/newest block) + zips with FEE_PERCENTILES → record_priority_fees.
    let Some(rewards) = history.reward else {
        return false;
    };
    let Some(last_block_rewards) = rewards.last() else {
        return false;
    };

    // Zip the percentile KEYS (the `reward_percentiles` cast to u64 —
    // `FEE_PERCENTILES = [10, 50]` → `[10u64, 50u64]`) with the reward values.
    // Ports `dict(zip(FEE_PERCENTILES, reward_ints))`.
    #[allow(clippy::cast_possible_truncation)]
    let fees: BTreeMap<u64, u128> = reward_percentiles
        .iter()
        .map(|p| *p as u64)
        .zip(last_block_rewards.iter().copied())
        .collect();

    let recorded_block = history.oldest_block + block_count.saturating_sub(1);
    dispatcher
        .lock()
        .expect("dispatcher mutex poisoned")
        .record_priority_fees(recorded_block, fees);
    true
}

// ─────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────

/// Build an alloy `TransactionRequest` from the finalized `TxParams` (for the
/// `eth_createAccessList` re-computation, L2641).
fn build_transaction_request(params: &TxParams) -> TransactionRequest {
    use alloy::rpc::types::TransactionInput;
    TransactionRequest {
        from: None, // filled by the provider from the signer's address
        to: Some(alloy::primitives::TxKind::Call(params.to)),
        gas_price: None,
        max_fee_per_gas: Some(params.max_fee_per_gas),
        max_priority_fee_per_gas: Some(params.max_priority_fee_per_gas),
        gas: Some(params.gas_limit),
        value: Some(params.value),
        input: TransactionInput::both(params.data.clone()),
        nonce: Some(params.nonce),
        chain_id: Some(1), // mainnet — access-list result is chain-id-independent
        access_list: Some(params.access_list.clone()),
        transaction_type: Some(2u8),   // EIP-1559
        blob_versioned_hashes: None,
        max_fee_per_blob_gas: None,
        sidecar: None,
        authorization_list: None,
        other: alloy::rpc::types::OtherFields::default(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::too_many_lines)]

    use super::*;
    use crate::dispatcher::Dispatcher;
    use crate::signer::TxSigner;
    use alloy::primitives::{address, Address, Bytes, B256, U256};
    use alloy::providers::ProviderBuilder;
    use alloy::rpc::client::ClientBuilder;
    use alloy::transports::mock::{Asserter, MockTransport};
    use std::sync::Arc;

    const EXECUTOR: Address = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    const POOL_A: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const POOL_B: &str = "0xcccccccccccccccccccccccccccccccccccccccc";

    fn signer() -> TxSigner {
        // A deterministic test key (the alloy test default — never use on
        // mainnet).
        TxSigner::from_key_hex(
            "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
            1,
        )
        .unwrap()
    }

    fn mock_provider(asserter: &Asserter) -> AlloyProvider {
        let client = ClientBuilder::default()
            .transport(MockTransport::new(asserter.clone()), true);
        let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
        AlloyProvider::from_provider(
            Arc::new(dyn_provider) as Arc<dyn alloy::providers::Provider<alloy::network::Ethereum>>,
        )
    }

    fn candidate(path_id: u64, net_profit: u128, pools: &[&str]) -> SubmitCandidate {
        SubmitCandidate {
            path_id,
            gross_profit: U256::from(net_profit + 1_000_000_000u128),
            net_profit: U256::from(net_profit),
            gas_used: 200_000,
            priority_fee: 1_000_000_000u128,
            base_fee_next: 1_000_000_000u128,
            execute_calldata: Bytes::from_static(&[0xde, 0xad, 0xbe, 0xef]),
            executor_address: EXECUTOR,
            access_list: None,
            path_pools: pools.iter().map(|s| PoolKey::new(*s)).collect(),
        }
    }

    fn empty_access_list_response() -> serde_json::Value {
        serde_json::json!({"accessList": [], "gasUsed": "0x0"})
    }

    fn tx_hash_response(hash: &str) -> serde_json::Value {
        serde_json::json!(hash)
    }

    // ── N6: the submit order (profit-desc) ────────────────────────────────

    #[tokio::test]
    async fn submit_sorts_candidates_by_net_profit_descending() {
        // Two candidates, un-sorted (B has higher net). After dispatch they
        // should be submitted net-desc (B first). dry_run so no broadcast
        // (just checks the order via the Skipped records).
        let asserter = Asserter::new();
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::new()));
        let s = signer();
        let probe: Arc<dyn ReceiptProbe + Send + Sync> = Arc::new(NoopProbe);

        let cands = vec![
            candidate(1, 1_000_000_000u128, &[POOL_A]),
            candidate(2, 5_000_000_000u128, &[POOL_B]),
        ];
        let outcome = dispatch_and_submit(
            cands,
            &dispatcher,
            &provider,
            &s,
            probe,
            0,
            100,
            true, // dry_run
            false,
        )
        .await
        .unwrap();

        // Both skipped (dry_run), but in net-desc order: candidate 2 first.
        assert_eq!(outcome.records.len(), 2);
        assert_eq!(
            outcome.records[0],
            SubmitRecord::Skipped { path_id: 2, reason: SkipReason::DryRun }
        );
        assert_eq!(
            outcome.records[1],
            SubmitRecord::Skipped { path_id: 1, reason: SkipReason::DryRun }
        );
    }

    // ── N6: mutual-exclusivity skip ────────────────────────────────────────

    #[tokio::test]
    async fn submit_skips_when_path_pools_already_claimed() {
        // Candidate A submitted (dry_run commits POOL_A). Candidate B shares
        // POOL_A → skipped with PoolsClaimed.
        let asserter = Asserter::new();
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::new()));
        let s = signer();
        let probe: Arc<dyn ReceiptProbe + Send + Sync> = Arc::new(NoopProbe);

        let cands = vec![
            candidate(1, 5_000_000_000u128, &[POOL_A]),
            candidate(2, 4_000_000_000u128, &[POOL_A]), // shares POOL_A
        ];
        let outcome = dispatch_and_submit(
            cands,
            &dispatcher,
            &provider,
            &s,
            probe,
            0,
            100,
            true, // dry_run — A commits POOL_A, B is blocked
            false,
        )
        .await
        .unwrap();

        // A skipped (dry_run), B skipped (pools claimed).
        assert_eq!(outcome.records.len(), 2);
        assert_eq!(
            outcome.records[1],
            SubmitRecord::Skipped { path_id: 2, reason: SkipReason::PoolsClaimed }
        );
    }

    #[tokio::test]
    async fn submit_skips_when_pools_pending_in_dispatcher() {
        // Pre-reserve POOL_A in the dispatcher → candidate A is blocked.
        let asserter = Asserter::new();
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::new()));
        dispatcher
            .lock()
            .unwrap()
            .reserve_pools(vec![PoolKey::new(POOL_A)]);
        let s = signer();
        let probe: Arc<dyn ReceiptProbe + Send + Sync> = Arc::new(NoopProbe);

        let outcome = dispatch_and_submit(
            vec![candidate(1, 5_000_000_000u128, &[POOL_A])],
            &dispatcher,
            &provider,
            &s,
            probe,
            0,
            100,
            false,
            false,
        )
        .await
        .unwrap();

        assert_eq!(outcome.records.len(), 1);
        assert_eq!(
            outcome.records[0],
            SubmitRecord::Skipped { path_id: 1, reason: SkipReason::PoolsClaimed }
        );
    }

    // ── N6: the dry_run / inject_code skip ──────────────────────────────────

    #[tokio::test]
    async fn submit_skips_on_dry_run_and_commits_pools() {
        let asserter = Asserter::new();
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::new()));
        let s = signer();
        let probe: Arc<dyn ReceiptProbe + Send + Sync> = Arc::new(NoopProbe);

        let outcome = dispatch_and_submit(
            vec![candidate(1, 1_000_000_000u128, &[POOL_A])],
            &dispatcher,
            &provider,
            &s,
            probe,
            0,
            100,
            true,  // dry_run
            false,
        )
        .await
        .unwrap();

        assert_eq!(outcome.skipped_count(), 1);
        assert_eq!(outcome.submitted_count(), 0);
    }

    #[tokio::test]
    async fn submit_skips_on_inject_code() {
        let asserter = Asserter::new();
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::new()));
        let s = signer();
        let probe: Arc<dyn ReceiptProbe + Send + Sync> = Arc::new(NoopProbe);

        let outcome = dispatch_and_submit(
            vec![candidate(1, 1_000_000_000u128, &[POOL_A])],
            &dispatcher,
            &provider,
            &s,
            probe,
            0,
            100,
            false,
            true, // inject_code
        )
        .await
        .unwrap();

        assert_eq!(outcome.records.len(), 1);
        assert_eq!(
            outcome.records[0],
            SubmitRecord::Skipped { path_id: 1, reason: SkipReason::InjectCode }
        );
    }

    // ── N6: the nonce→fee→access-list→sign→broadcast sequence ──────────────

    #[tokio::test]
    async fn submit_claims_nonce_finalizes_fees_broadcasts_and_spawns_monitor() {
        // One candidate, not dry_run, not inject. Push the access-list + the
        // tx-hash responses. Assert: Submitted{tx_hash, nonce=42}, the
        // dispatcher holds the nonce (pending) + the pool (reserved) + a
        // tracked task.
        let asserter = Asserter::new();
        // eth_createAccessList response.
        asserter.push_success(&empty_access_list_response());
        // eth_sendRawTransaction response — a fake tx hash.
        let fake_hash =
            B256::from(alloy::primitives::address!("dddddddddddddddddddddddddddddddddddddddd"));
        asserter.push_success(&tx_hash_response(&format!("{fake_hash:?}")));
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::new()));
        let s = signer();
        let probe: Arc<dyn ReceiptProbe + Send + Sync> = Arc::new(NoopProbe);

        let outcome = dispatch_and_submit(
            vec![candidate(1, 5_000_000_000u128, &[POOL_A])],
            &dispatcher,
            &provider,
            &s,
            probe,
            42, // operator_nonce
            100,
            false,
            false,
        )
        .await
        .unwrap();

        assert_eq!(outcome.submitted_count(), 1);
        assert_eq!(outcome.skipped_count(), 0);
        let SubmitRecord::Submitted { path_id, tx_hash, nonce } = &outcome.records[0] else {
            panic!("expected Submitted, got {:?}", outcome.records[0]);
        };
        assert_eq!(*path_id, 1);
        assert_eq!(*nonce, 42);
        assert_eq!(*tx_hash, fake_hash);

        // The dispatcher holds the nonce (pending) + the pool (reserved) + a
        // tracked monitor task.
        let d = dispatcher.lock().unwrap();
        assert!(d.is_pool_pending(&PoolKey::new(POOL_A)));
        assert_eq!(d.active_task_count(), 1);
        // Stop the spawned monitor so the test runtime can shut down cleanly.
        drop(d);
        dispatcher.lock().unwrap().abort_all_tasks();
    }

    #[tokio::test]
    async fn submit_skips_on_broadcast_failure() {
        // The broadcast RPC fails → skip with BroadcastFailed. The nonce is
        // claimed but NOT released (leaked until manual cleanup — ports the
        // `continue` on Web3Exception).
        let asserter = Asserter::new();
        asserter.push_success(&empty_access_list_response()); // access-list ok
        asserter.push_failure_msg("eth_sendRawTransaction failed"); // broadcast fails
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::new()));
        let s = signer();
        let probe: Arc<dyn ReceiptProbe + Send + Sync> = Arc::new(NoopProbe);

        let outcome = dispatch_and_submit(
            vec![candidate(1, 5_000_000_000u128, &[POOL_A])],
            &dispatcher,
            &provider,
            &s,
            probe,
            7,
            100,
            false,
            false,
        )
        .await
        .unwrap();

        assert_eq!(outcome.skipped_count(), 1);
        assert_eq!(outcome.submitted_count(), 0);
        assert!(matches!(
            &outcome.records[0],
            SubmitRecord::Skipped { reason: SkipReason::BroadcastFailed(_), .. }
        ));
        // The pool was NOT reserved (broadcast failed before reserve_pools).
        assert!(!dispatcher.lock().unwrap().is_pool_pending(&PoolKey::new(POOL_A)));
        // No monitor task spawned.
        assert_eq!(dispatcher.lock().unwrap().active_task_count(), 0);
    }

    #[tokio::test]
    async fn submit_claims_unique_nonces_for_sequential_candidates() {
        // Two candidates, distinct pools. Both submit. Nonces 42 + 43.
        let asserter = Asserter::new();
        asserter.push_success(&empty_access_list_response());
        let hash_a =
            B256::from(alloy::primitives::address!("1111111111111111111111111111111111111111"));
        asserter.push_success(&tx_hash_response(&format!("{hash_a:?}")));
        asserter.push_success(&empty_access_list_response());
        let hash_b =
            B256::from(alloy::primitives::address!("2222222222222222222222222222222222222222"));
        asserter.push_success(&tx_hash_response(&format!("{hash_b:?}")));
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::new()));
        let s = signer();
        let probe: Arc<dyn ReceiptProbe + Send + Sync> = Arc::new(NoopProbe);

        let outcome = dispatch_and_submit(
            vec![
                candidate(1, 5_000_000_000u128, &[POOL_A]),
                candidate(2, 4_000_000_000u128, &[POOL_B]),
            ],
            &dispatcher,
            &provider,
            &s,
            probe,
            42,
            100,
            false,
            false,
        )
        .await
        .unwrap();

        assert_eq!(outcome.submitted_count(), 2);
        let nonces: Vec<u64> = outcome
            .records
            .iter()
            .filter_map(|r| match r {
                SubmitRecord::Submitted { nonce, .. } => Some(*nonce),
                _ => None,
            })
            .collect();
        assert_eq!(nonces, vec![42, 43]);
        dispatcher.lock().unwrap().abort_all_tasks();
    }

    // ── I2: fetch_fee_history ──────────────────────────────────────────────

    #[tokio::test]
    async fn fetch_fee_history_records_percentiles_into_dispatcher() {
        // eth_feeHistory returns reward=[[1e9, 2e9]] (p10=1gwei, p50=2gwei)
        // for block 100. record_priority_fees should store {10: 1e9, 50: 2e9}
        // keyed by block 100.
        let asserter = Asserter::new();
        asserter.push_success(&serde_json::json!({
            "oldestBlock": "0x64",
            "baseFeePerGas": ["0x3b9aca00"],
            "gasUsedRatio": [0.5],
            "reward": [["0x3b9aca00", "0x77359400"]],
        }));
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::new()));

        let recorded =
            fetch_fee_history(&provider, &dispatcher, 1, 100, &[10.0, 50.0]).await;

        assert!(recorded);
        let fees = dispatcher.lock().unwrap().latest_priority_fees();
        assert_eq!(fees.get(&10), Some(&1_000_000_000u128));
        assert_eq!(fees.get(&50), Some(&2_000_000_000u128));
    }

    #[tokio::test]
    async fn fetch_fee_history_tolerates_rpc_failure() {
        // eth_feeHistory fails → no-op, returns false, dispatcher keeps prior
        // state (empty here).
        let asserter = Asserter::new();
        asserter.push_failure_msg("eth_feeHistory failed");
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::new()));

        let recorded =
            fetch_fee_history(&provider, &dispatcher, 1, 100, &[10.0, 50.0]).await;

        assert!(!recorded);
    }

    // ── test helpers ──────────────────────────────────────────────────────

    /// A `ReceiptProbe` that never confirms (the monitor would poll forever —
    /// but the spawned tasks are aborted at the end of each test via
    /// `abort_all_tasks`).
    struct NoopProbe;

    impl ReceiptProbe for NoopProbe {
        fn receipt_found(
            &self,
            _tx_hash: B256,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = crate::SubmissionResult<bool>> + Send + '_>>
        {
            Box::pin(async { Ok(false) })
        }
    }
}