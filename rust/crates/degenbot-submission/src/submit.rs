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

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use alloy::eips::{BlockId, BlockNumberOrTag};
use alloy::primitives::{Address, Bytes, B256, U256};
use alloy::rpc::types::TransactionRequest;
use serde_json::json;

use degenbot_rpc::provider::AlloyProvider;

use crate::dispatcher::{Dispatcher, PoolKey};
use crate::fee::finalize_fees;
use crate::monitor::{monitor_pending_transaction_default, ReceiptProbe, SubmittedTx};
use crate::params::TxParams;
use crate::signer::TxSigner;
use crate::submission_ledger::{NonceLane, TargetId};

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

/// Per-candidate `MEVBlocker` bundle context (the `GSUF22` bid wiring).
///
/// docs.mevblocker.io/how-to/searchers/bid: the backrun leaves ONLY via
/// `eth_sendBundle` on `wss://searchers.mevblocker.io`, carrying the pending
/// target's hash as `txs[0]` and the signed backrun as `txs[1]`, pinned to
/// the target's block. When this context is present the public mempool and
/// any other relay are BYPASSED — single-destination auction entry.
#[derive(Debug, Clone)]
pub struct BundleTarget {
    /// The searcher WS endpoint (the same socket the feed subscribes on).
    pub stream_url: String,
    /// The pending target transaction's hash (the feed frame's `hash`).
    pub target_tx_hash: B256,
    /// The block the bundle is valid for (dispatch head + 1).
    pub block_number: u64,
}

/// Where one signed transaction is sent.
#[derive(Debug, Clone)]
pub enum SubmissionTarget {
    /// The `MEVBlocker` bundle channel: `eth_sendBundle` on the searcher WS
    /// carrying the pending target's hash as `txs[0]` and the signed backrun
    /// as `txs[1]`, pinned to the target's block. The public mempool and any
    /// other relay are BYPASSED.
    Bundle(BundleTarget),
    /// The public mempool broadcast: the signed bytes fan out to every listed
    /// relay (the read provider when none is listed).
    Public,
}

/// The bundle relay round-trip budget. One-shot per bid; a dropped bid is a
/// no-cost miss under the revert shield, never a hang.
const BUNDLE_RELAY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(750);

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
#[derive(Debug, Clone, PartialEq)]
pub enum SubmitRecord {
    /// The tx was broadcast — the resulting `tx_hash` + claimed `nonce`.
    Submitted {
        path_id: u64,
        tx_hash: B256,
        nonce: u64,
    },
    /// The candidate was skipped — the typed reason.
    Skipped { path_id: u64, reason: SkipReason },
}

/// The dispatch+submit outcome — the per-candidate records (ports the loop's
/// accumulated `submitted`/`skipped` tallies the Python logs as the
/// `[dispatch]` summary).
///
/// `records` is the single result channel: [`Self::submitted_count`] and
/// [`Self::skipped_count`] derive from it. The `instruments::pipeline()`
/// counters written alongside in `dispatch_and_submit` are a telemetry
/// projection of the same events, not a second result store.
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

/// T4: a candidate's net profit as `f64` wei (saturating at `u128::MAX`;
/// dashboards chart magnitudes, not exact wei).
#[expect(clippy::cast_precision_loss)]
fn candidate_net_wei(candidate: &SubmitCandidate) -> f64 {
    u128::try_from(candidate.net_profit).unwrap_or(u128::MAX) as f64
}

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
///    d. **Stamp the nonce** — [`NonceLane::stamp`] leases the lowest-free
///       nonce from the process-wide authority, repackaging past the strategy's
///       own stale reservation (L2636).
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
/// `is_path_blocked`/`reserve_pools`/`track_task` calls — NEVER across the
/// `.await` RPCs (sign/broadcast/access-list) so the monitor is never blocked.
/// Nonce issuance is a separate, lock-light authority call, and the sole
/// nonce source in every runtime shape.
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
///
/// # Panics
///
/// Panics if the `dispatcher` mutex is poisoned (a coordinated task
/// panicked while holding it — unrecoverable; matches the Python assumption
/// that the dispatcher state is always readable).
#[expect(clippy::doc_overindented_list_items)] // the a.-g. sub-steps use a deeper indent
#[expect(clippy::too_many_arguments)] // the pipeline stages each need a param
#[expect(clippy::too_many_lines)] // the 10-step pipeline is inherent
pub async fn dispatch_and_submit(
    mut candidates: Vec<SubmitCandidate>,
    dispatcher: &Arc<Mutex<Dispatcher>>,
    provider: &AlloyProvider,
    signer: &TxSigner,
    probe: Arc<dyn ReceiptProbe + Send + Sync>,
    nonce_lane: &Arc<NonceLane>,
    current_block: u64,
    dry_run: bool,
    inject_code: bool,
    extra_broadcast: &[std::sync::Arc<AlloyProvider>],
    // The submission channel: a bundle target routes the bid EXCLUSIVELY
    // through `eth_sendBundle` on the searcher WS (no public broadcast, no
    // relays); `Public` fans the SAME signed bytes across the relays.
    target: SubmissionTarget,
) -> Result<SubmitOutcome, crate::SubmissionError> {
    // RMHQAR  + ZHVXW2: one Jaeger node per dispatch batch
    // (degenbot.bundle.dispatch).
    // - NO span for an EMPTY batch: the observed failure shape was 20
    //   consecutive single-span root traces, candidates=0, pure noise.
    // - The span re-attaches the published block's span context as a REMOTE
    //   parent (the f701ccd3 bridge family): the Python-driven submit task
    //   has no ambient block span, so without this every batch exported as
    //   its own disconnected trace family keyed only by the block tag.
    //   The publisher is the Rust settle-side compute_diff_and_send
    //   (telemetry::publish_block_context, keyed by results_block); the
    //   nearest-previous fallback matches the one-ahead block semantics the
    //   Python seam's simulate_dispatch_span already uses.
    // - Field unified to block.number (the pump/solve span vocabulary).
    let span = (!candidates.is_empty()).then(|| {
        let span = tracing::info_span!(
            "degenbot.bundle.dispatch",
            candidates = candidates.len(),
            dry_run,
            block.number = current_block,
            dispatch.submitted = tracing::field::Empty,
            dispatch.skipped = tracing::field::Empty,
        );
        degenbot_bot::telemetry::attach_published_parent(&span, current_block);
        span
    });
    let _guard = span.as_ref().map(tracing::Span::enter);
    // 1. Sort by net profit descending (the dispatch fan-out's output ordering
    //    — re-asserted so a caller handing un-sorted candidates submits
    //    best-first). Ports L2561's `gas_profitable.sort(key=net, reverse=...)`.
    candidates.sort_by_key(|c| std::cmp::Reverse(c.net_profit));

    let mut outcome = SubmitOutcome::default();
    // The loop-local committed-pools set (ports `committed_pools` — the pools
    // claimed by submitted + dry-run/inject-skipped candidates this batch).
    let mut committed_pools: HashSet<PoolKey> = HashSet::new();

    // T4: per-batch latency anchor — candidate loop start → each broadcast.
    let batch_start = std::time::Instant::now();
    for candidate in candidates {
        // T4: per-candidate economics (histograms; f64 wei for dashboards —
        // they chart magnitudes, not exact wei; clamped at u128::MAX).
        if let Some(p) = degenbot_bot::instruments::pipeline() {
            #[expect(clippy::cast_precision_loss)]
            let gross = u128::try_from(candidate.gross_profit).unwrap_or(u128::MAX) as f64;
            #[expect(clippy::cast_precision_loss)]
            let net = u128::try_from(candidate.net_profit).unwrap_or(u128::MAX) as f64;
            p.observe_dispatch_profits(gross, net);
            p.observe_dispatch_gas(candidate.gas_used);
        }
        let path_pools = candidate.path_pools.clone();

        // 2a. Mutual-exclusivity guard (L2626). Lock briefly — no .await.
        let blocked = {
            #[expect(clippy::expect_used)] // poisoned sync-guard = process bug; panic loudly
            let d = dispatcher.lock().expect("dispatcher mutex poisoned");
            d.is_path_blocked(&path_pools, &committed_pools)
        };
        if blocked {
            if let Some(p) = degenbot_bot::instruments::pipeline() {
                p.count_submit_outcome("skipped_pools_claimed");
                p.add_profit_missed(candidate_net_wei(&candidate));
            }
            outcome.records.push(SubmitRecord::Skipped {
                path_id: candidate.path_id,
                reason: SkipReason::PoolsClaimed,
            });
            continue;
        }

        // 2b. dry_run guard (L2608). Commit pools (dry-run respects mutual
        //     exclusivity) + skip.
        if dry_run {
            // Dry-run is a MODE, not missed money — counted, not profit-summed.
            if let Some(p) = degenbot_bot::instruments::pipeline() {
                p.count_submit_outcome("skipped_dry_run");
            }
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
            if let Some(p) = degenbot_bot::instruments::pipeline() {
                p.count_submit_outcome("skipped_inject_code");
                p.add_profit_missed(candidate_net_wei(&candidate));
            }
            committed_pools.extend(path_pools.clone());
            outcome.records.push(SubmitRecord::Skipped {
                path_id: candidate.path_id,
                reason: SkipReason::InjectCode,
            });
            continue;
        }

        // 2d. Obtain the sign-time nonce from the process-wide authority.
        //     The lane's default policy repackages past the strategy's own
        //     stale reservation; a non-stale decline stops the batch rather
        //     than sign against a nonce the authority refused.
        let Ok(lease) = nonce_lane.stamp() else {
            break;
        };
        let nonce = lease.nonce();

        // 2e. Build the TxParams + finalize fees (L2637–L2639).
        //     `gas_limit = int(gas_used * 1.5)` — the 1.5× safety margin
        //     (L2419). The truncate semantics: `gas_used as f64 * 1.5 as u64`
        //     matches Python `int(gas_used * 1.5)` (toward-zero) for any
        //     realistic gas value.
        #[expect(
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
        match provider
            .eth_create_access_list(&request, BlockId::Number(BlockNumberOrTag::Latest))
            .await
        {
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

        // 2g-bis. Record the freshly signed submission in the per-head ledger
        //     against the authority-granted nonce, so the head feed can
        //     reconcile exactly what was built.
        {
            let signed_hash = alloy::primitives::keccak256(&raw_signed);
            let target_id = match &target {
                SubmissionTarget::Bundle(bt) => TargetId::new(bt.target_tx_hash),
                SubmissionTarget::Public => TargetId::new(signed_hash),
            };
            nonce_lane
                .record_signed(&lease, target_id, signed_hash, current_block)
                .map_err(|e| crate::SubmissionError::Nonce(e.to_string()))?;
        }

        // 2h. Broadcast (L2648–L2654).
        //
        // MEVBlocker bid channel (the sidecar's bid mode): `eth_sendBundle`
        // over the searcher WS, `txs = [targetHash, signed backrun]`, pinned
        // to the target's block with a deterministic replacementUuid. The
        // signed bytes NEVER touch the public mempool or another relay on
        // this path — the auction entry is single-destination (doc
        // how-to/searchers/bid). The executor's coinbase bribe (packed
        // config, recipient 0) is the fee_recipient payment the docs require.
        if let SubmissionTarget::Bundle(bt) = &target {
            let bid = crate::bundle::BundleBid {
                target_tx_hash: bt.target_tx_hash,
                backrun_raw: raw_signed.clone(),
                block_number: bt.block_number,
                replacement_uuid: crate::bundle::replacement_uuid_for(
                    bt.target_tx_hash,
                    bt.block_number,
                ),
            };
            match crate::bundle::send_request(
                &bt.stream_url,
                crate::bundle::eth_send_bundle_request(&bid),
                BUNDLE_RELAY_TIMEOUT,
            )
            .await
            {
                Ok(resp) => {
                    let raw = resp
                        .get("result")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    let hash = alloy::hex::FromHex::from_hex(raw)
                        .unwrap_or_else(|_| alloy::primitives::keccak256(&raw_signed));
                    if let Some(p) = degenbot_bot::instruments::pipeline() {
                        p.count_submit_outcome("bundle_accepted");
                    }
                    nonce_lane
                        .authority()
                        .record_broadcast(&lease)
                        .map_err(|e| crate::SubmissionError::Nonce(e.to_string()))?;
                    nonce_lane
                        .ledger()
                        .record_broadcast(nonce_lane.strategy(), lease.nonce())
                        .map_err(|e| crate::SubmissionError::Nonce(e.to_string()))?;
                    outcome.records.push(SubmitRecord::Submitted {
                        path_id: candidate.path_id,
                        tx_hash: hash,
                        nonce,
                    });
                    continue;
                }
                Err(e) => {
                    degenbot_bot::telemetry::record_exception(
                        degenbot_bot::telemetry::error_kind::SUBMIT_FAILURE,
                        format_args!("path {} mevblocker bundle rejected: {e}", candidate.path_id),
                    );
                    if let Some(p) = degenbot_bot::instruments::pipeline() {
                        p.count_submit_outcome("skipped_broadcast_failed");
                        p.add_profit_missed(candidate_net_wei(&candidate));
                    }
                    crate::bundle::trace_wire_jsonl(
                        "bundle_wire_error",
                        &json!({
                            "path_id": candidate.path_id,
                            "target": format!("0x{}", bt.target_tx_hash),
                            "block": bt.block_number,
                            "error": format!("{e}"),
                        }),
                    );
                    // The bundle never reached the auction: free the lease so
                    // the next candidate can stamp.
                    let _ = nonce_lane.release(&lease);
                    outcome.records.push(SubmitRecord::Skipped {
                        path_id: candidate.path_id,
                        reason: SkipReason::BroadcastFailed(format!(
                            "mevblocker bundle rejected: {e}"
                        )),
                    });
                    continue;
                }
            }
        }
        // Legacy raw fan-out (all other callers): the SAME signed bytes go
        // to every relay in `extra_broadcast` (the read provider is NOT
        // broadcast to unless the list is empty — the legacy single-endpoint
        // behavior). First acceptance defines the tracked hash; total
        // failure = the typed skip.
        let mut accepted_hash: Option<B256> = None;
        let broadcast_targets: Vec<&AlloyProvider> = if extra_broadcast.is_empty() {
            vec![provider]
        } else {
            extra_broadcast.iter().map(std::sync::Arc::as_ref).collect()
        };
        for relay_provider in &broadcast_targets {
            match relay_provider.eth_send_raw_transaction(&raw_signed).await {
                Ok(hash) => {
                    if accepted_hash.is_none() {
                        accepted_hash = Some(hash);
                    }
                }
                Err(e) => {
                    degenbot_bot::telemetry::record_exception(
                        degenbot_bot::telemetry::error_kind::SUBMIT_FAILURE,
                        format_args!("path {} relay broadcast failed: {e}", candidate.path_id),
                    );
                }
            }
        }
        let tx_hash = if let Some(hash) = accepted_hash {
            if let Some(p) = degenbot_bot::instruments::pipeline() {
                p.count_submit_outcome("relay_accepted");
            }
            nonce_lane
                .authority()
                .record_broadcast(&lease)
                .map_err(|e| crate::SubmissionError::Nonce(e.to_string()))?;
            nonce_lane
                .ledger()
                .record_broadcast(nonce_lane.strategy(), lease.nonce())
                .map_err(|e| crate::SubmissionError::Nonce(e.to_string()))?;
            hash
        } else {
            // The broadcast failed on EVERY relay — skip with the typed
            // reason. The claimed nonce + pools are NOT released here
            // (ports the `continue` on Web3Exception — the nonce is
            // leaked until a manual cleanup or the dispatcher's reap).
            // The monitor is NOT spawned (no tx to track).
            if let Some(p) = degenbot_bot::instruments::pipeline() {
                p.count_submit_outcome("skipped_broadcast_failed");
                p.add_profit_missed(candidate_net_wei(&candidate));
            }
            // Every relay rejected the raw transaction: free the lease so the
            // next candidate can stamp.
            let _ = nonce_lane.release(&lease);
            outcome.records.push(SubmitRecord::Skipped {
                path_id: candidate.path_id,
                reason: SkipReason::BroadcastFailed(
                    "all relays rejected the raw transaction".to_string(),
                ),
            });
            continue;
        };

        // 2i. Reserve pools (L2662) + commit to the local set.
        {
            #[expect(clippy::expect_used)] // poisoned sync-guard = process bug; panic loudly
            let mut d = dispatcher.lock().expect("dispatcher mutex poisoned");
            d.reserve_pools(path_pools.clone());
        }
        committed_pools.extend(path_pools.clone());

        if let Some(p) = degenbot_bot::instruments::pipeline() {
            p.count_submit_outcome("submitted");
            p.observe_submit_latency(batch_start.elapsed().as_secs_f64());
        }
        outcome.records.push(SubmitRecord::Submitted {
            path_id: candidate.path_id,
            tx_hash,
            nonce,
        });

        // 2j. Spawn the monitor (L2664–L2672). The dispatcher handle is cloned
        //     into the spawned task (the `Arc<Mutex<Dispatcher>>` sharing
        //     pattern). The monitor releases the pools on confirm/expire; the
        //     authority owns the nonce lifecycle.
        let dispatcher_clone = Arc::clone(dispatcher);
        let probe_clone = Arc::clone(&probe);
        let lane_clone = Arc::clone(nonce_lane);
        let submitted_tx = SubmittedTx::new(tx_hash, nonce, path_pools, current_block);
        // T4: realized profit — this candidate's net profit lands on the
        // realized counter iff its tx confirms. Cloned into the task (the
        // loop moves `candidate` on).
        let candidate_net = candidate_net_wei(&candidate);
        #[expect(clippy::expect_used)] // poisoned sync-guard = process bug; panic loudly
        {
            dispatcher
                .lock()
                .expect("dispatcher mutex poisoned")
                .track_task(async move {
                    let outcome = monitor_pending_transaction_default(
                        submitted_tx,
                        &*probe_clone,
                        &dispatcher_clone,
                    )
                    .await;
                    // An expired broadcast is declared dead: release the
                    // authority slot so a dropped transaction cannot wedge the
                    // account's nonce prefix. A confirmed broadcast is landed
                    // by the per-head reconcile instead (its transaction is on
                    // the chain).
                    if matches!(outcome, Ok(ref o) if o.is_expired()) {
                        let _ = lane_clone.release_broadcast(nonce);
                    }
                    if let Some(p) = degenbot_bot::instruments::pipeline() {
                        match &outcome {
                            Ok(o) if o.is_confirmed() => {
                                p.count_monitor_outcome("confirmed");
                                p.add_profit_realized(candidate_net);
                            }
                            Ok(_) => p.count_monitor_outcome("expired"),
                            Err(ref e) => {
                                p.count_monitor_outcome("error");
                                degenbot_bot::telemetry::record_exception(
                                    degenbot_bot::telemetry::error_kind::MONITOR_FAILURE,
                                    format_args!("tx monitor error: {e}"),
                                );
                            }
                        }
                    }
                });
        }
    }

    // with the empty-batch gate the span is optional - the outcome
    // counts land only when a batch (span) actually exists.
    if let Some(span) = span.as_ref() {
        span.record("dispatch.submitted", outcome.submitted_count());
        span.record("dispatch.skipped", outcome.skipped_count());
    }
    Ok(outcome)
}

// ─────────────────────────────────────────────────────────────────────────
// The fee-history fetch (I2)
// ─────────────────────────────────────────────────────────────────────────

/// Convert an `eth_feeHistory` reward percentile (`f64`, per the JSON-RPC
/// spec) to the `u64` key the `block_priority_fees` ring stores, returning
/// `None` for anything that is not a finite whole-number value in the valid
/// percentile range `[0, 100]`.
///
/// A bare `p as u64` masks two real hazards: fractional percentiles (e.g.
/// `10.5`) silently truncate to a key that never matches the `.get(&10)` /
/// `.get(&50)` lookups the consumer performs, and negative / `NaN` /
/// out-of-range values sign-mangle into garbage `u64` keys. Rejecting them
/// here keeps malformed input from polluting the ring; the caller drops the
/// pairing via `filter_map` (matching the function's tolerate-failure
/// semantics — a bad percentile is advisory, not fatal).
fn percentile_key(p: f64) -> Option<u64> {
    if !p.is_finite() || !(0.0..=100.0).contains(&p) || p.fract() != 0.0 {
        return None;
    }
    // Guarded cast: `p` is finite, whole, and in `[0, 100]`, so it fits `u64`
    // exactly — no truncation, no sign loss.
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let key = p as u64;
    Some(key)
}

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
///
/// # Panics
///
/// Panics if the `dispatcher` mutex is poisoned (a coordinated task
/// panicked while holding it — unrecoverable).
pub async fn fetch_fee_history(
    provider: &AlloyProvider,
    dispatcher: &Arc<Mutex<Dispatcher>>,
    block_count: u64,
    last_block: u64,
    reward_percentiles: &[f64],
) -> bool {
    let Ok(history) = provider
        .eth_fee_history(
            block_count,
            alloy::rpc::types::BlockNumberOrTag::Number(last_block),
            reward_percentiles,
        )
        .await
    else {
        return false;
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

    // Zip the percentile KEYS with the reward values, validating each
    // percentile through [`percentile_key`] before it becomes the `u64` key
    // the `block_priority_fees` consumer reads via `.get(&10)` / `.get(&50)`.
    // A bare `*p as u64` masks two real hazards: fractional percentiles
    // (e.g. `10.5`) truncate to a key that never matches a lookup, and
    // negative / `NaN` / out-of-range values sign-mangle into garbage keys.
    // Ports `dict(zip(FEE_PERCENTILES, reward_ints))` (the Python source
    // passed integer percentiles, so the keys were always whole numbers).
    let fees: BTreeMap<u64, u128> = reward_percentiles
        .iter()
        .zip(last_block_rewards.iter().copied())
        .filter_map(|(p, reward)| percentile_key(*p).map(|k| (k, reward)))
        .collect();

    let recorded_block = history.oldest_block + block_count.saturating_sub(1);
    #[expect(clippy::expect_used)] // poisoned sync-guard = process bug; panic loudly
    {
        dispatcher
            .lock()
            .expect("dispatcher mutex poisoned")
            .record_priority_fees(recorded_block, fees);
    }
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
        transaction_type: Some(2u8), // EIP-1559
        blob_versioned_hashes: None,
        max_fee_per_blob_gas: None,
        sidecar: None,
        authorization_list: None,
    }
}

#[expect(clippy::unwrap_used, clippy::panic)]
#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "tests: fixtures use expect for loud, precise failures"
)]
mod tests {
    use super::*;
    use crate::dispatcher::Dispatcher;
    use crate::signer::TxSigner;
    use alloy::primitives::{address, Address, Bytes, B256, U256};
    use alloy::providers::{Provider, ProviderBuilder};
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

    /// A sign-time lane over a fresh authority seeded at `operator_nonce` and
    /// an empty ledger — the sole nonce source every dispatch path now takes.
    fn lane(operator_nonce: u64) -> Arc<NonceLane> {
        Arc::new(NonceLane::new(
            Arc::new(degenbot_bot::nonce_authority::NonceAuthority::new(
                operator_nonce,
            )),
            Arc::new(crate::submission_ledger::SubmissionLedger::new()),
            "test-strategy",
        ))
    }

    fn mock_provider(asserter: &Asserter) -> AlloyProvider {
        let client = ClientBuilder::default().transport(MockTransport::new(asserter.clone()), true);
        let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
        AlloyProvider::from_provider(
            Arc::new(dyn_provider) as Arc<dyn alloy::providers::Provider<alloy::network::Ethereum>>
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
        let dispatcher = Arc::new(Mutex::new(Dispatcher::default()));
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
            &lane(0),
            100,
            true, // dry_run
            false,
            &[],
            SubmissionTarget::Public,
        )
        .await
        .unwrap();

        // Both skipped (dry_run), but in net-desc order: candidate 2 first.
        assert_eq!(outcome.records.len(), 2);
        assert_eq!(
            outcome.records[0],
            SubmitRecord::Skipped {
                path_id: 2,
                reason: SkipReason::DryRun
            }
        );
        assert_eq!(
            outcome.records[1],
            SubmitRecord::Skipped {
                path_id: 1,
                reason: SkipReason::DryRun
            }
        );
    }

    /// The per-candidate records are the single result channel: the summary
    /// counts are derived from them, so a caller reads one store.
    #[test]
    fn submit_outcome_records_are_the_single_result_channel() {
        let outcome = SubmitOutcome {
            records: vec![
                SubmitRecord::Submitted {
                    path_id: 1,
                    tx_hash: B256::ZERO,
                    nonce: 7,
                },
                SubmitRecord::Skipped {
                    path_id: 2,
                    reason: SkipReason::DryRun,
                },
            ],
        };

        assert_eq!(outcome.submitted_count(), 1);
        assert_eq!(outcome.skipped_count(), 1);
        assert_eq!(
            outcome.submitted_count() + outcome.skipped_count(),
            outcome.records.len(),
            "the counts partition the records; there is no second store to diverge"
        );
    }

    // ── N6: mutual-exclusivity skip ────────────────────────────────────────

    #[tokio::test]
    async fn submit_skips_when_path_pools_already_claimed() {
        // Candidate A submitted (dry_run commits POOL_A). Candidate B shares
        // POOL_A → skipped with PoolsClaimed.
        let asserter = Asserter::new();
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::default()));
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
            &lane(0),
            100,
            true, // dry_run — A commits POOL_A, B is blocked
            false,
            &[],
            SubmissionTarget::Public,
        )
        .await
        .unwrap();

        // A skipped (dry_run), B skipped (pools claimed).
        assert_eq!(outcome.records.len(), 2);
        assert_eq!(
            outcome.records[1],
            SubmitRecord::Skipped {
                path_id: 2,
                reason: SkipReason::PoolsClaimed
            }
        );
    }

    #[tokio::test]
    async fn submit_skips_when_pools_pending_in_dispatcher() {
        // Pre-reserve POOL_A in the dispatcher → candidate A is blocked.
        let asserter = Asserter::new();
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::default()));
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
            &lane(0),
            100,
            false,
            false,
            &[],
            SubmissionTarget::Public,
        )
        .await
        .unwrap();

        assert_eq!(outcome.records.len(), 1);
        assert_eq!(
            outcome.records[0],
            SubmitRecord::Skipped {
                path_id: 1,
                reason: SkipReason::PoolsClaimed
            }
        );
    }

    // ── N6: the dry_run / inject_code skip ──────────────────────────────────

    #[tokio::test]
    async fn submit_skips_on_dry_run_and_commits_pools() {
        let asserter = Asserter::new();
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::default()));
        let s = signer();
        let probe: Arc<dyn ReceiptProbe + Send + Sync> = Arc::new(NoopProbe);

        let outcome = dispatch_and_submit(
            vec![candidate(1, 1_000_000_000u128, &[POOL_A])],
            &dispatcher,
            &provider,
            &s,
            probe,
            &lane(0),
            100,
            true, // dry_run
            false,
            &[],
            SubmissionTarget::Public,
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
        let dispatcher = Arc::new(Mutex::new(Dispatcher::default()));
        let s = signer();
        let probe: Arc<dyn ReceiptProbe + Send + Sync> = Arc::new(NoopProbe);

        let outcome = dispatch_and_submit(
            vec![candidate(1, 1_000_000_000u128, &[POOL_A])],
            &dispatcher,
            &provider,
            &s,
            probe,
            &lane(0),
            100,
            false,
            true, // inject_code
            &[],
            SubmissionTarget::Public,
        )
        .await
        .unwrap();

        assert_eq!(outcome.records.len(), 1);
        assert_eq!(
            outcome.records[0],
            SubmitRecord::Skipped {
                path_id: 1,
                reason: SkipReason::InjectCode
            }
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
        let fake_hash = B256::repeat_byte(0xdd);
        asserter.push_success(&tx_hash_response(&format!("{fake_hash:?}")));
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::default()));
        let s = signer();
        let probe: Arc<dyn ReceiptProbe + Send + Sync> = Arc::new(NoopProbe);

        let outcome = dispatch_and_submit(
            vec![candidate(1, 5_000_000_000u128, &[POOL_A])],
            &dispatcher,
            &provider,
            &s,
            probe,
            &lane(42), // operator_nonce
            100,
            false,
            false,
            &[],
            SubmissionTarget::Public,
        )
        .await
        .unwrap();

        assert_eq!(outcome.submitted_count(), 1);
        assert_eq!(outcome.skipped_count(), 0);
        let SubmitRecord::Submitted {
            path_id,
            tx_hash,
            nonce,
        } = &outcome.records[0]
        else {
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
        let dispatcher = Arc::new(Mutex::new(Dispatcher::default()));
        let s = signer();
        let probe: Arc<dyn ReceiptProbe + Send + Sync> = Arc::new(NoopProbe);

        let outcome = dispatch_and_submit(
            vec![candidate(1, 5_000_000_000u128, &[POOL_A])],
            &dispatcher,
            &provider,
            &s,
            probe,
            &lane(7),
            100,
            false,
            false,
            &[],
            SubmissionTarget::Public,
        )
        .await
        .unwrap();

        assert_eq!(outcome.skipped_count(), 1);
        assert_eq!(outcome.submitted_count(), 0);
        assert!(matches!(
            &outcome.records[0],
            SubmitRecord::Skipped {
                reason: SkipReason::BroadcastFailed(_),
                ..
            }
        ));
        // The pool was NOT reserved (broadcast failed before reserve_pools).
        assert!(!dispatcher
            .lock()
            .unwrap()
            .is_pool_pending(&PoolKey::new(POOL_A)));
        // No monitor task spawned.
        assert_eq!(dispatcher.lock().unwrap().active_task_count(), 0);
    }

    #[tokio::test]
    async fn submit_claims_unique_nonces_for_sequential_candidates() {
        // Two candidates, distinct pools. Both submit. Nonces 42 + 43.
        let asserter = Asserter::new();
        asserter.push_success(&empty_access_list_response());
        let hash_a = B256::repeat_byte(0x11);
        asserter.push_success(&tx_hash_response(&format!("{hash_a:?}")));
        asserter.push_success(&empty_access_list_response());
        let hash_b = B256::repeat_byte(0x22);
        asserter.push_success(&tx_hash_response(&format!("{hash_b:?}")));
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::default()));
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
            &lane(42),
            100,
            false,
            false,
            &[],
            SubmissionTarget::Public,
        )
        .await
        .unwrap();

        assert_eq!(outcome.submitted_count(), 2);
        let nonces: Vec<u64> = outcome
            .records
            .iter()
            .filter_map(|r| match r {
                SubmitRecord::Submitted { nonce, .. } => Some(*nonce),
                SubmitRecord::Skipped { .. } => None,
            })
            .collect();
        assert_eq!(nonces, vec![42, 43]);
        dispatcher.lock().unwrap().abort_all_tasks();
    }

    #[tokio::test]
    async fn a_hosted_submission_stamps_through_the_authority_and_records_the_ledger() {
        // Nonces come from the process-wide authority and each signed
        // submission is recorded in the per-head ledger.
        use crate::submission_ledger::{NonceLane, SubmissionLedger, SubmissionState};
        use degenbot_bot::nonce_authority::StrategyId;

        let asserter = Asserter::new();
        asserter.push_success(&empty_access_list_response());
        let hash_a = B256::repeat_byte(0xa1);
        asserter.push_success(&tx_hash_response(&format!("{hash_a:?}")));
        asserter.push_success(&empty_access_list_response());
        let hash_b = B256::repeat_byte(0xb2);
        asserter.push_success(&tx_hash_response(&format!("{hash_b:?}")));
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::default()));
        let s = signer();
        let probe: Arc<dyn ReceiptProbe + Send + Sync> = Arc::new(NoopProbe);

        let strategy = StrategyId::new("settlement");
        let authority = Arc::new(degenbot_bot::nonce_authority::NonceAuthority::new(42));
        let ledger = Arc::new(SubmissionLedger::new());
        let lane = Arc::new(NonceLane::new(
            Arc::clone(&authority),
            Arc::clone(&ledger),
            strategy.clone(),
        ));

        let outcome = dispatch_and_submit(
            vec![
                candidate(1, 5_000_000_000u128, &[POOL_A]),
                candidate(2, 4_000_000_000u128, &[POOL_B]),
            ],
            &dispatcher,
            &provider,
            &s,
            probe,
            &lane,
            100,
            false,
            false,
            &[],
            SubmissionTarget::Public,
        )
        .await
        .unwrap();

        assert_eq!(outcome.submitted_count(), 2);
        assert_eq!(
            authority.outstanding_nonces(),
            vec![42, 43],
            "the authority owns both outstanding broadcast nonces"
        );
        assert_eq!(
            ledger.state_of(&strategy, 42),
            Some(SubmissionState::Broadcast)
        );
        assert_eq!(
            ledger.state_of(&strategy, 43),
            Some(SubmissionState::Broadcast)
        );
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
        let dispatcher = Arc::new(Mutex::new(Dispatcher::default()));

        let recorded = fetch_fee_history(&provider, &dispatcher, 1, 100, &[10.0, 50.0]).await;

        assert!(recorded);
        let d = dispatcher.lock().unwrap();
        let fees = d.latest_priority_fees();
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
        let dispatcher = Arc::new(Mutex::new(Dispatcher::default()));

        let recorded = fetch_fee_history(&provider, &dispatcher, 1, 100, &[10.0, 50.0]).await;

        assert!(!recorded);
    }

    #[tokio::test]
    async fn fetch_fee_history_drops_malformed_percentiles() {
        // The `eth_feeHistory` percentile keys are `f64`; a bare `as u64` cast
        // would silently truncate fractional parts and sign-mangle negatives
        // / NaN into garbage keys. `percentile_key` rejects them, so the ring
        // only ever holds validated whole-number percentile keys.
        let asserter = Asserter::new();
        asserter.push_success(&serde_json::json!({
            "oldestBlock": "0x64",
            "baseFeePerGas": ["0x3b9aca00"],
            "gasUsedRatio": [0.5],
            // Four reward slots for the four requested percentiles.
            "reward": [["0x3b9aca00", "0x77359400", "0x0", "0x0"]],
        }));
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::default()));

        // p10 valid; 50.5 fractional (rejected); -1.0 negative (rejected);
        // NaN (rejected). Only p10 lands in the ring.
        let recorded = fetch_fee_history(
            &provider,
            &dispatcher,
            1,
            100,
            &[10.0, 50.5, -1.0, f64::NAN],
        )
        .await;

        assert!(recorded);
        let d = dispatcher.lock().unwrap();
        let fees = d.latest_priority_fees();
        assert_eq!(fees.get(&10), Some(&1_000_000_000u128));
        assert!(fees.get(&50).is_none());
        assert!(fees.get(&505).is_none());
        // No garbage key from the negative / NaN casts (would have been
        // `u64::MAX`-ish under a bare `as u64` cast).
        assert_eq!(fees.len(), 1);
    }

    #[tokio::test]
    async fn public_fan_out_reaches_explicit_relay_and_read_provider() {
        // The private-broadcast arm's call site passes an extra list that
        // EXPLICITLY contains the read provider (private-first, public
        // fallback). Both transports must receive the raw signed bytes; the
        // first acceptance defines the tracked hash.
        let provider_asserter = Asserter::new();
        let relay_asserter = Asserter::new();
        // The read provider serves the access-list RPC and its own raw send.
        provider_asserter.push_success(&empty_access_list_response());
        let provider_hash = B256::repeat_byte(0xaa);
        provider_asserter.push_success(&tx_hash_response(&format!("{provider_hash:?}")));
        // The relay only sees the raw send.
        let relay_hash = B256::repeat_byte(0xbb);
        relay_asserter.push_success(&tx_hash_response(&format!("{relay_hash:?}")));

        let provider = Arc::new(mock_provider(&provider_asserter));
        let relay = Arc::new(mock_provider(&relay_asserter));
        let dispatcher = Arc::new(Mutex::new(Dispatcher::default()));
        let s = signer();
        let probe: Arc<dyn ReceiptProbe + Send + Sync> = Arc::new(NoopProbe);
        let extra: Vec<Arc<AlloyProvider>> = vec![Arc::clone(&relay), Arc::clone(&provider)];

        let outcome = dispatch_and_submit(
            vec![candidate(1, 5_000_000_000u128, &[POOL_A])],
            &dispatcher,
            &provider,
            &s,
            probe,
            &lane(42),
            100,
            false,
            false,
            &extra,
            SubmissionTarget::Public,
        )
        .await
        .unwrap();

        assert_eq!(outcome.submitted_count(), 1);
        assert!(
            relay_asserter.read_q().is_empty(),
            "the explicit relay must receive the raw broadcast"
        );
        assert!(
            provider_asserter.read_q().is_empty(),
            "the read provider must receive the raw broadcast too"
        );
        let SubmitRecord::Submitted { tx_hash, .. } = &outcome.records[0] else {
            panic!("expected Submitted, got {:?}", outcome.records[0]);
        };
        assert_eq!(
            *tx_hash, relay_hash,
            "private-first: the relay's acceptance wins the tracked hash"
        );
        dispatcher.lock().unwrap().abort_all_tasks();
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
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = crate::SubmissionResult<bool>> + Send + '_>,
        > {
            Box::pin(async { Ok(false) })
        }
    }
    /// RMHQAR : the `dispatch_and_submit` span records the
    /// candidate count and outcome counts (`dry_run` marker path: 1 candidate
    /// -> 0 submitted, 1 skipped).
    /// The unique `block.number` creation field filters this test's span from the
    /// shared global capture.
    #[tokio::test]
    async fn dispatch_span_records_candidate_and_outcome_counts() {
        const MY_BLOCK: u64 = 999_999_999;
        let cap = crate::span_capture::global();
        let asserter = Asserter::new();
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::default()));
        let s = signer();
        let probe: Arc<dyn ReceiptProbe + Send + Sync> = Arc::new(NoopProbe);

        let outcome = dispatch_and_submit(
            vec![candidate(7, 1_000_000_000u128, &[POOL_A])],
            &dispatcher,
            &provider,
            &s,
            probe,
            &lane(0),
            MY_BLOCK,
            true, // dry_run
            false,
            &[],
            SubmissionTarget::Public,
        )
        .await
        .unwrap();
        assert_eq!(outcome.submitted_count(), 0);
        assert_eq!(outcome.skipped_count(), 1);

        let mut mine = 0;
        for (name, fields) in cap.snapshot() {
            if name != "degenbot.bundle.dispatch" {
                continue;
            }
            if fields.get("block.number").map(String::as_str) != Some(MY_BLOCK.to_string().as_str())
            {
                continue;
            }
            mine += 1;
            assert_eq!(fields.get("dry_run").map(String::as_str), Some("true"));
            assert_eq!(fields.get("candidates").map(String::as_str), Some("1"));
            assert_eq!(
                fields.get("dispatch.submitted").map(String::as_str),
                Some("0")
            );
            assert_eq!(
                fields.get("dispatch.skipped").map(String::as_str),
                Some("1"),
                "all fields: {fields:?}"
            );
        }
        assert_eq!(
            mine, 1,
            "one dry-run dispatch span for block 999_999_999 captured"
        );
    }

    /// ZHVXW2 (traces of block 25913390): the 20 most-recent Jaeger traces
    /// were ALL single-span `degenbot.bundle.dispatch` roots with
    /// candidates=0 and every counter 0 - empty batches export a span AND
    /// the Python-driven task has no ambient block context, so it also
    /// exported as its own disconnected trace family. Both fixed at the
    /// span seam: an empty batch must export NO dispatch span (the call
    /// itself is unchanged - outcome/loop bookkeeping is a no-op with zero
    /// candidates, but the counters the loop tails write are unchanged).
    #[tokio::test]
    async fn empty_batch_exports_no_dispatch_span() {
        const MY_BLOCK: u64 = 987_654_321;
        let cap = crate::span_capture::global();
        let asserter = Asserter::new();
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::default()));
        let s = signer();
        let probe: Arc<dyn ReceiptProbe + Send + Sync> = Arc::new(NoopProbe);

        let outcome = dispatch_and_submit(
            Vec::new(),
            &dispatcher,
            &provider,
            &s,
            probe,
            &lane(0),
            MY_BLOCK,
            true,
            false,
            &[],
            SubmissionTarget::Public,
        )
        .await
        .unwrap();
        assert_eq!(outcome.submitted_count(), 0, "empty batch submits nothing");

        let spans_for_block = cap
            .snapshot()
            .into_iter()
            .filter(|(name, fields)| {
                name == "degenbot.bundle.dispatch"
                    && fields.get("block.number").map(String::as_str)
                        == Some(MY_BLOCK.to_string().as_str())
            })
            .count();
        assert_eq!(
            spans_for_block, 0,
            "an empty-candidate batch must not export a dispatch span"
        );
    }

    // ── MEVBlocker bundle channel (GSUF22 wiring, doc how-to/searchers/bid) ──

    /// A one-shot in-process WS relay: accepts one connection, captures the
    /// first frame, answers with a bundle id, closes.
    async fn relay_once() -> (String, tokio::task::JoinHandle<serde_json::Value>) {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("addr");
        let handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("relay connection");
            let mut ws = tokio_tungstenite::accept_async(stream)
                .await
                .expect("ws handshake");
            let frame = ws.next().await.expect("relay frame").expect("frame ok");
            let v: serde_json::Value =
                serde_json::from_str(frame.to_text().expect("text frame")).expect("json frame");
            ws.send(Message::text(
                "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":\"0x164d7d41f24b7d41f24b7d41f24b7d41f24b7d41f24b7d41f24b7d41f24b7d41\"}",
            ))
            .await
            .expect("relay reply");
            let _ = ws.close(None).await;
            v
        });
        (format!("ws://{addr}"), handle)
    }

    #[tokio::test]
    async fn bundle_channel_sends_only_to_mevblocker_ws_with_target_hash_pinned() {
        // The bid leaves EXCLUSIVELY as eth_sendBundle on the searcher WS:
        // txs[0] = the feed frame's target hash, txs[1] = the signed
        // backrun, blockNumber pinned to the target's block. The provider
        // (public mempool) is never asked to broadcast — the ONLY Submitted
        // record's hash is the RELAY's bundle id.
        let (url, relay) = relay_once().await;
        let asserter = Asserter::new();
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::default()));
        let s = signer();
        let probe: Arc<dyn ReceiptProbe + Send + Sync> = Arc::new(NoopProbe);

        let target = alloy::primitives::B256::repeat_byte(0x42);
        let bundle = BundleTarget {
            stream_url: url,
            target_tx_hash: target,
            block_number: 4242,
        };
        let outcome = dispatch_and_submit(
            vec![candidate(7, 1_000_000_000u128, &[POOL_A])],
            &dispatcher,
            &provider,
            &s,
            probe,
            &lane(0),
            4241,
            false,
            false,
            &[],
            SubmissionTarget::Bundle(bundle),
        )
        .await
        .expect("bundle dispatch");

        let req = relay.await.expect("relay task");
        assert_eq!(req["method"], "eth_sendBundle");
        let p = &req["params"][0];
        assert_eq!(
            p["txs"][0],
            "0x4242424242424242424242424242424242424242424242424242424242424242"
        );
        let raw = p["txs"][1].as_str().expect("raw tx string");
        assert!(raw.starts_with("0x02"), "signed EIP-1559 bytes: {raw}");
        assert_eq!(p["blockNumber"], "0x1092"); // 4242 pinned
        let uuid = p["replacementUuid"].as_str().expect("uuid");
        assert!(uuid.starts_with("degenbot-"), "deterministic uuid: {uuid}");

        assert_eq!(
            outcome.records,
            vec![SubmitRecord::Submitted {
                path_id: 7,
                tx_hash: alloy::hex::FromHex::from_hex(
                    "0x164d7d41f24b7d41f24b7d41f24b7d41f24b7d41f24b7d41f24b7d41f24b7d41"
                )
                .unwrap(),
                nonce: 0,
            }],
            "the recorded hash is the RELAY's bundle id — the public provider saw nothing"
        );
    }

    #[tokio::test]
    async fn bundle_channel_skips_typed_when_relay_unreachable() {
        // No relay on the port → a typed BroadcastFailed skip with the
        // mevblocker attribution, NOT a panic, NOT a public-mempool fallback.
        let asserter = Asserter::new();
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::default()));
        let s = signer();
        let probe: Arc<dyn ReceiptProbe + Send + Sync> = Arc::new(NoopProbe);

        // A bound-then-dropped listener guarantees connection-refused.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);

        let bundle = BundleTarget {
            stream_url: format!("ws://{addr}"),
            target_tx_hash: alloy::primitives::B256::repeat_byte(0x43),
            block_number: 7,
        };
        let outcome = dispatch_and_submit(
            vec![candidate(9, 999_000_000u128, &[POOL_B])],
            &dispatcher,
            &provider,
            &s,
            probe,
            &lane(0),
            6,
            false,
            false,
            &[],
            SubmissionTarget::Bundle(bundle),
        )
        .await
        .expect("dispatch completes");

        match outcome.records.as_slice() {
            [SubmitRecord::Skipped {
                path_id: 9,
                reason: SkipReason::BroadcastFailed(msg),
            }] => {
                assert!(msg.contains("mevblocker bundle"), "attribution: {msg}");
            }
            other => panic!("expected typed broadcast-failure skip, got {other:?}"),
        }
    }
}
