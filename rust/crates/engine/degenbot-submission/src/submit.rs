//! The `dispatch_and_submit` orchestration + `eth_sendRawTransaction`
//! broadcast + `eth_feeHistory` percentile fetch.
//!
//! Ports `examples/eth_backrun_v2_v3_v4_rust.py::dispatch_profitable_results`
//! submit tail (L2608–L2660 — the `dry_run` guard L2608, `INJECT_EXECUTOR_CODE`
//! guard L2666, mutual-exclusivity guard L2622, claim nonce L2636, finalize
//! fees L2637–L2639, re-compute access list L2641–L2645, sign+broadcast
//! L2647–L2660, reserve pools L2662, spawn monitor L2664–L2672) + the
//! `_apply_block_if_ready` `eth_feeHistory` fetch (L2907–L2923).
//!
//! This is the orchestration that owns the GIL release across the async
//! sign+broadcast slice (ADR-005 — "Python is the cockpit, Rust is the
//! engine"). Owning the sign+broadcast in Rust releases the GIL across the
//! per-tx RPCs (the "GIL?" win).
//!
//! # Dispositions
//!
//! - **N6 `port-now`** — the submit orchestration (this leaf). Sorts by net
//!   profit descending; for each: mutual-exclusivity guard → `dry_run`/
//!   `inject_code` guard (typed Skip) → claim nonce → finalize fees →
//!   re-compute access list → sign → broadcast → reserve pools → spawn
//!   monitor.
//! - **I1 `done`-reference** — [`AlloyProvider::eth_send_raw_transaction`] (the
//!   typed `bytes → B256` surface — committed `d26b8248`). CONSUMED,
//!   no `make_request` escape hatch (the interim is over).
//! - **I2 `done`-reference** — [`AlloyProvider::eth_fee_history`] (the
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

use crate::delivery::{classify_delivery_failure, DeliveryFailureClass, DeliveryVerdict};
use crate::dispatcher::{Dispatcher, PoolKey};
use crate::fee::finalize_fees;
use crate::monitor::{monitor_pending_transaction_default, ReceiptProbe, SubmittedTx};
use crate::params::TxParams;
use crate::signer::TxSigner;
use crate::submission_ledger::SubmissionLedger;
use degenbot_substrate::nonce::{NonceLane, NonceLease};

// ─────────────────────────────────────────────────────────────────────────
// The submit candidate (decoupled from the Simulation `SimResult`)
// ─────────────────────────────────────────────────────────────────────────

/// A pre-submit candidate — the profitable sim result + the resolved path
/// pools (the keys for mutual-exclusivity).
///
/// Decoupled from the simulation crate's `SimResult` on purpose, and it stays
/// decoupled: submission consumes a candidate's economics (profit, gas, fees),
/// its ready-to-sign calldata, and the pools it touches — not the simulation
/// crate's state and journal payloads — so reaching for sim types here would
/// widen this crate's dependency surface to the revm stack and freeze the
/// submit seam against a shape only one caller produces. The umbrella `Bot` /
/// Python driver maps `SimResult` → `SubmitCandidate` at the seam instead
/// (resolving the `path_pools` from `path_info.hops` — `pool_id_hex` for V4,
/// `pool_address` for V2/V3 — ports L2623).
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

/// Per-candidate `MEVBlocker` bundle context (the bid wiring).
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
    /// The Flashbots-compatible builder-relay bundle: the TARGET's verbatim
    /// signed bytes + the signed backrun as one `eth_sendBundle` HTTPS POST
    /// to every relay (no auction host in the path, no public raw fan-out).
    BuilderRelay {
        /// The builder relay URLs, in fan-out order.
        relays: Vec<String>,
        /// The target tx's verbatim signed bytes (the txpool frame's `raw`).
        target_raw: alloy::primitives::Bytes,
        /// The block the bundle is valid for (dispatch head + 1).
        block_number: u64,
    },
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
    ///
    /// The record says the candidate was not submitted; what became of its
    /// nonce is the delivery classifier's verdict, not the record's.
    BroadcastFailed(String),
    /// The batch could not obtain a nonce: the lane's stamp was declined,
    /// because another strategy holds the slot or because a reservation sits
    /// below the confirmed head.
    ///
    /// The candidate in hand and every candidate behind it in the batch are
    /// recorded with this reason. Nonce contention blocks a candidate, it does
    /// not kill it: a blocked candidate is a retry candidate for its strategy,
    /// while a broadcast failure or a pool collision is final.
    NonceContended,
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
/// [`Self::skipped_count`] derive from it. The `instruments::record`
/// census events written alongside in `dispatch_and_submit` are a telemetry
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

// ─────────────────────────────────────────────────────────────────────────
// The submit pipeline stages
// ─────────────────────────────────────────────────────────────────────────

/// What one submit stage decided for the candidate in hand.
///
/// The payload is generic because the stages carry different things forward:
/// the pre-nonce guards carry nothing, [`SubmitBatch::stamp_nonce`] carries the
/// nonce lease, [`SubmitBatch::build_tx_params`] and
/// [`SubmitBatch::recompute_access_list`] carry the [`TxParams`] carrier, and
/// [`SubmitBatch::deliver`] carries the broadcast hash.
///
/// The third arm is why these stages are not a `Result<_, SkipReason>`: a
/// refused nonce stamp stops the WHOLE batch, while a skip is a per-candidate
/// outcome. A single error type would have to spell the batch abort as a skip
/// reason no consumer acts on.
enum StepVerdict<T> {
    /// Hand the payload to the next stage.
    Continue(T),
    /// Do not submit this candidate — the typed reason, recorded as a skip.
    Skip(SkipReason),
    /// Stop the batch where it stands. The reason is per-candidate: the
    /// candidate in hand and every candidate behind it are recorded with it,
    /// so a stopped batch never drops a candidate without an outcome.
    AbortBatch(SkipReason),
}

/// The batch-scoped state the pipeline stages mutate.
///
/// `committed_pools` is this batch's claim set: the pools of every candidate
/// submitted so far, plus the ones a `dry_run` / `inject_code` skip claimed —
/// a dry run still respects mutual exclusivity. It is distinct from the
/// dispatcher's `pending_pools`, which tracks the cross-batch claims of
/// in-flight transactions.
#[derive(Default)]
struct BatchState {
    committed_pools: HashSet<PoolKey>,
}

impl BatchState {
    /// Claim `pools` for the rest of the batch.
    fn commit(&mut self, pools: &HashSet<PoolKey>) {
        self.committed_pools.extend(pools.iter().cloned());
    }
}

/// One dispatch batch: the invariant per-batch inputs every stage reads, and
/// the ordered pipeline those stages form.
///
/// The order is the Python oracle's `dispatch_profitable_results` submit tail:
///
/// 1. [`Self::sort`] — best-first by net profit.
/// 2. [`Self::record_candidate_economics`] — the per-candidate census prologue.
/// 3. [`Self::guard_mutual_exclusion`] — a claimed pool skips the candidate.
/// 4. [`Self::guard_dry_run`] — a dry run claims the pools and skips.
/// 5. [`Self::guard_inject_code`] — injected code claims the pools and skips.
/// 6. [`Self::stamp_nonce`] — lease the nonce; a refusal stops the batch.
/// 7. [`Self::build_tx_params`] then [`Self::recompute_access_list`] — one
///    [`TxParams`] carrier, mutated across both stages.
/// 8. [`Self::sign_and_record`] — sign, then record the signed stamp.
/// 9. [`Self::deliver`] — the three delivery arms.
/// 10. [`Self::reserve_and_record`] then [`Self::spawn_monitor`] — the
///     commitment.
///
/// The per-batch claim set is NOT a field: [`BatchState`] is threaded through
/// the stages as an explicit argument, so a stage that reaches across candidates
/// says so in its signature. The stages after [`Self::sign_and_record`] cannot
/// skip a candidate — they always continue, and their verdict exists so the
/// loop reads as one pipeline.
struct SubmitBatch<'a> {
    dispatcher: &'a Arc<Mutex<Dispatcher>>,
    provider: &'a AlloyProvider,
    signer: &'a TxSigner,
    probe: Arc<dyn ReceiptProbe + Send + Sync>,
    nonce_lane: &'a Arc<NonceLane<SubmissionLedger>>,
    current_block: u64,
    dry_run: bool,
    inject_code: bool,
    extra_broadcast: &'a [Arc<AlloyProvider>],
    target: &'a SubmissionTarget,
}

impl SubmitBatch<'_> {
    /// Stage 1 — sort by net profit descending, best-first.
    ///
    /// The dispatch fan-out already sorts (`gas_profitable.sort(key=net,
    /// reverse=True)`); re-asserted here so a caller handing un-sorted
    /// candidates still submits best-first. `sort_by_key` is stable, so equally
    /// profitable candidates keep their input order.
    fn sort(candidates: &mut [SubmitCandidate]) {
        candidates.sort_by_key(|c| std::cmp::Reverse(c.net_profit));
    }

    /// Stage 2 — the per-candidate economics + gas census sample.
    ///
    /// `f64` wei: dashboards chart magnitudes, not exact wei (clamped at
    /// `u128::MAX`).
    fn record_candidate_economics(candidate: &SubmitCandidate) {
        #[expect(clippy::cast_precision_loss)]
        let gross = u128::try_from(candidate.gross_profit).unwrap_or(u128::MAX) as f64;
        #[expect(clippy::cast_precision_loss)]
        let net = u128::try_from(candidate.net_profit).unwrap_or(u128::MAX) as f64;
        degenbot_bot::instruments::record(
            degenbot_bot::instruments::CensusEvent::CandidateEconomics {
                gross_wei: gross,
                net_wei: net,
            },
        );
        degenbot_bot::instruments::record(degenbot_bot::instruments::CensusEvent::CandidateGas {
            gas: candidate.gas_used,
        });
    }

    /// Stage 3 — mutual-exclusivity guard.
    ///
    /// A resolved pool already claimed by an in-flight transaction
    /// ([`Dispatcher::is_path_blocked`]) or by an earlier candidate of this
    /// batch skips the candidate. The lock is held only for that synchronous
    /// call — never across an `.await`.
    fn guard_mutual_exclusion(
        &self,
        candidate: &SubmitCandidate,
        path_pools: &HashSet<PoolKey>,
        state: &BatchState,
    ) -> StepVerdict<()> {
        #[expect(clippy::expect_used)] // poisoned sync-guard = process bug; panic loudly
        let blocked = self
            .dispatcher
            .lock()
            .expect("dispatcher mutex poisoned")
            .is_path_blocked(path_pools, &state.committed_pools);
        if !blocked {
            return StepVerdict::Continue(());
        }
        degenbot_bot::instruments::record(degenbot_bot::instruments::CensusEvent::SubmitOutcome {
            outcome: "skipped_pools_claimed",
        });
        degenbot_bot::instruments::record(degenbot_bot::instruments::CensusEvent::ProfitMissed {
            wei: candidate_net_wei(candidate),
        });
        StepVerdict::Skip(SkipReason::PoolsClaimed)
    }

    /// Stage 4 — the `dry_run` guard: skip the live submission.
    ///
    /// The pools are still claimed, so a dry run respects mutual exclusivity
    /// exactly as a live batch does. No profit-missed sample: a dry run is a
    /// mode, not missed money.
    fn guard_dry_run(
        &self,
        path_pools: &HashSet<PoolKey>,
        state: &mut BatchState,
    ) -> StepVerdict<()> {
        if !self.dry_run {
            return StepVerdict::Continue(());
        }
        degenbot_bot::instruments::record(degenbot_bot::instruments::CensusEvent::SubmitOutcome {
            outcome: "skipped_dry_run",
        });
        state.commit(path_pools);
        StepVerdict::Skip(SkipReason::DryRun)
    }

    /// Stage 5 — the `inject_code` guard: the injected contract does not exist
    /// on-chain, so a live submission would target an absent executor.
    ///
    /// The pools are claimed as a live batch would claim them.
    fn guard_inject_code(
        &self,
        candidate: &SubmitCandidate,
        path_pools: &HashSet<PoolKey>,
        state: &mut BatchState,
    ) -> StepVerdict<()> {
        if !self.inject_code {
            return StepVerdict::Continue(());
        }
        degenbot_bot::instruments::record(degenbot_bot::instruments::CensusEvent::SubmitOutcome {
            outcome: "skipped_inject_code",
        });
        degenbot_bot::instruments::record(degenbot_bot::instruments::CensusEvent::ProfitMissed {
            wei: candidate_net_wei(candidate),
        });
        state.commit(path_pools);
        StepVerdict::Skip(SkipReason::InjectCode)
    }

    /// Stage 6 — stamp the sign-time nonce from the process-wide authority.
    ///
    /// The lane's default policy repackages past the strategy's own stale
    /// reservation. A non-stale decline aborts the batch: the alternative is
    /// signing against a nonce the authority refused. The abort carries the
    /// nonce-contention reason, so every candidate it stops is recorded as
    /// blocked rather than dropped.
    fn stamp_nonce(&self) -> StepVerdict<NonceLease> {
        match self.nonce_lane.stamp() {
            Ok(lease) => StepVerdict::Continue(lease),
            Err(_decline) => StepVerdict::AbortBatch(SkipReason::NonceContended),
        }
    }

    /// Stage 7a — build the [`TxParams`] carrier and finalize its fees.
    ///
    /// `gas_limit = int(gas_used * 1.5)` (the 1.5× safety margin); the
    /// truncating cast matches the Python `int()` for any realistic gas value.
    /// The candidate's pre-sim access list rides along until
    /// [`Self::recompute_access_list`] replaces it.
    ///
    /// # Errors
    ///
    /// [`crate::SubmissionError::FeeOverflow`] when the fee finalization
    /// exceeds the EIP-1559 field widths.
    fn build_tx_params(
        candidate: &SubmitCandidate,
        nonce: u64,
    ) -> Result<StepVerdict<TxParams>, crate::SubmissionError> {
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
        if let Some(access_list) = &candidate.access_list {
            tx_params.access_list = access_list.clone();
        }
        finalize_fees(
            &mut tx_params,
            candidate.base_fee_next,
            candidate.priority_fee,
        )?;
        Ok(StepVerdict::Continue(tx_params))
    }

    /// Stage 7b — re-compute the access list against the finalized nonce and
    /// fees, handing back the same carrier it was given.
    ///
    /// A failed `eth_createAccessList` is tolerated: the carrier keeps the
    /// candidate's pre-sim list. Lock-free — the RPC is a pure provider call.
    async fn recompute_access_list(&self, mut tx_params: TxParams) -> StepVerdict<TxParams> {
        let request = build_transaction_request(&tx_params);
        match self
            .provider
            .eth_create_access_list(&request, BlockId::Number(BlockNumberOrTag::Latest))
            .await
        {
            Ok(access_list_result) => {
                tx_params.access_list = access_list_result.access_list;
            }
            Err(_e) => {
                // Tolerated — keep the pre-sim access list.
            }
        }
        StepVerdict::Continue(tx_params)
    }

    /// Stage 8 — sign the transaction, record the signed stamp in the lane's
    /// ledger, and hand the raw bytes to delivery.
    ///
    /// The signed hash is the identity head reconciliation and the monitor read
    /// back. The bundle arm keys its ledger entry by the TARGET's hash instead:
    /// that bid is identified by the transaction it backruns.
    ///
    /// # Errors
    ///
    /// [`crate::SubmissionError::Sign`] for a corrupt signing key, and
    /// [`crate::SubmissionError::Nonce`] when the lane's ledger refuses the
    /// signed-stamp record.
    fn sign_and_record(
        &self,
        tx_params: &TxParams,
        lease: &NonceLease,
    ) -> Result<StepVerdict<Bytes>, crate::SubmissionError> {
        // Synchronous ECDSA — no `.await`, no lock.
        let raw_signed = self.signer.sign_eip1559(tx_params)?;
        // Record the freshly signed submission in the per-head ledger against
        // the authority-granted nonce, so the head feed can reconcile exactly
        // what was built.
        let signed_hash = alloy::primitives::keccak256(&raw_signed);
        let target_hash = match self.target {
            SubmissionTarget::Bundle(bundle_target) => bundle_target.target_tx_hash,
            // Both non-auction arms identify the submission by the freshly
            // signed backrun's hash.
            SubmissionTarget::BuilderRelay { .. } | SubmissionTarget::Public => signed_hash,
        };
        self.nonce_lane
            .record_signed(lease, target_hash, signed_hash, self.current_block)
            .map_err(|e| crate::SubmissionError::Nonce(e.to_string()))?;
        Ok(StepVerdict::Continue(raw_signed))
    }

    /// Stage 9 — send the signed bytes down the submission target's channel.
    ///
    /// The three arms are deliberately NOT one code path behind a delivery
    /// strategy: they differ in which destination sees the bytes, in what an
    /// acceptance commits to, and in the evidence a failed attempt carries.
    /// Their failure POLICY is not theirs to choose — every arm hands its
    /// evidence to [`crate::delivery::classify_delivery_failure`] and acts on
    /// the verdict through [`Self::conclude_failed_delivery`], so the arms
    /// cannot drift into different nonce policies.
    ///
    /// # Errors
    ///
    /// [`crate::SubmissionError::Nonce`] when an accepted (or promoted)
    /// transmission cannot be recorded on both nonce books, or when the ledger
    /// refuses a delivery-unknown hold.
    async fn deliver(
        &self,
        candidate: &SubmitCandidate,
        raw_signed: &Bytes,
        lease: &NonceLease,
    ) -> Result<StepVerdict<B256>, crate::SubmissionError> {
        match self.target {
            SubmissionTarget::Bundle(bundle_target) => {
                self.deliver_bundle(candidate, bundle_target, raw_signed, lease)
                    .await
            }
            SubmissionTarget::BuilderRelay {
                relays,
                target_raw,
                block_number,
            } => {
                self.deliver_builder_relay(
                    candidate,
                    relays,
                    target_raw,
                    *block_number,
                    raw_signed,
                    lease,
                )
                .await
            }
            SubmissionTarget::Public => self.deliver_public(candidate, raw_signed, lease).await,
        }
    }

    /// Stage 9, arm 1 — the `MEVBlocker` bid channel.
    ///
    /// `eth_sendBundle` over the searcher WS with `txs = [target hash, signed
    /// backrun]`, pinned to the target's block with a deterministic
    /// `replacementUuid`. The signed bytes never touch the public mempool or
    /// another relay on this path: the auction entry is single-destination (the
    /// searcher docs' how-to/searchers/bid). The executor's coinbase bribe
    /// (packed config, recipient 0) is the `fee_recipient` payment those docs
    /// require.
    ///
    /// Failure policy: the bid is pinned to exactly one block and the auction
    /// books a bid only by acknowledging it, so a round trip that ends without
    /// an acknowledgement leaves no bid for that block — and no later block can
    /// include a bid pinned to a passed one. The pin is the bid's whole
    /// liveness window, which is what makes this arm's evidence definitive
    /// (`DeliveryFailureClass::NotAdmitted`) and the verdict `Release`.
    ///
    /// # Errors
    ///
    /// [`crate::SubmissionError::Nonce`] when an accepted bid cannot be
    /// promoted to a broadcast.
    async fn deliver_bundle(
        &self,
        candidate: &SubmitCandidate,
        bundle_target: &BundleTarget,
        raw_signed: &Bytes,
        lease: &NonceLease,
    ) -> Result<StepVerdict<B256>, crate::SubmissionError> {
        let bid = crate::bundle::BundleBid {
            target_tx_hash: bundle_target.target_tx_hash,
            backrun_raw: raw_signed.clone(),
            block_number: bundle_target.block_number,
            replacement_uuid: crate::bundle::replacement_uuid_for(
                bundle_target.target_tx_hash,
                bundle_target.block_number,
            ),
        };
        match crate::bundle::send_request(
            &bundle_target.stream_url,
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
                    .unwrap_or_else(|_| alloy::primitives::keccak256(raw_signed));
                degenbot_bot::instruments::record(
                    degenbot_bot::instruments::CensusEvent::SubmitOutcome {
                        outcome: "bundle_accepted",
                    },
                );
                self.promote_broadcast(lease)?;
                Ok(StepVerdict::Continue(hash))
            }
            Err(e) => {
                degenbot_bot::telemetry::record_exception(
                    degenbot_bot::telemetry::error_kind::SUBMIT_FAILURE,
                    format_args!("path {} mevblocker bundle rejected: {e}", candidate.path_id),
                );
                crate::bundle::trace_wire_jsonl(
                    "bundle_wire_error",
                    &json!({
                        "path_id": candidate.path_id,
                        "target": format!("0x{}", bundle_target.target_tx_hash),
                        "block": bundle_target.block_number,
                        "error": format!("{e}"),
                    }),
                );
                // No acknowledgement for a bid pinned to one block: definitive
                // non-liveness for that slot, so the classifier releases the
                // lease.
                self.conclude_failed_delivery(
                    candidate,
                    &[DeliveryFailureClass::NotAdmitted],
                    lease,
                    raw_signed,
                    format!("mevblocker bundle rejected: {e}"),
                )
            }
        }
    }

    /// Stage 9, arm 2 — the Flashbots-compatible builder-relay bundle.
    ///
    /// The TARGET's verbatim signed bytes plus the signed backrun as one
    /// `eth_sendBundle` HTTPS POST to every relay; any single relay acceptance
    /// submits the bundle, so an all-rejected round is a skip. The
    /// `X-Flashbots-Signature` header is derived per request from the exact JSON
    /// body, which is why a signing failure fails this arm before any relay sees
    /// a bundle.
    ///
    /// Failure policy: no relay holds the bundle, and the bundle is pinned to
    /// one block — the same liveness bound the auction arm reads, so the
    /// evidence is definitive (`DeliveryFailureClass::NotAdmitted`) and the
    /// verdict is `Release`.
    ///
    /// # Errors
    ///
    /// [`crate::SubmissionError::Nonce`] when an accepted bundle cannot be
    /// promoted to a broadcast.
    async fn deliver_builder_relay(
        &self,
        candidate: &SubmitCandidate,
        relays: &[String],
        target_raw: &Bytes,
        block_number: u64,
        raw_signed: &Bytes,
        lease: &NonceLease,
    ) -> Result<StepVerdict<B256>, crate::SubmissionError> {
        let bundle = crate::relay::BuilderRelayBundle {
            target_raw: target_raw.clone(),
            backrun_raw: raw_signed.clone(),
            block_number,
        };
        let body = crate::relay::builder_bundle_request(&bundle);
        let header = match crate::relay::flashbots_signature_header(self.signer, &body.to_string())
        {
            Ok(header) => header,
            Err(e) => {
                degenbot_bot::telemetry::record_exception(
                    degenbot_bot::telemetry::error_kind::SUBMIT_FAILURE,
                    format_args!("path {} relay signature failed: {e}", candidate.path_id),
                );
                // No relay ever saw a bundle, so nothing was admitted for the
                // pinned block: definitive non-liveness.
                return self.conclude_failed_delivery(
                    candidate,
                    &[DeliveryFailureClass::NotAdmitted],
                    lease,
                    raw_signed,
                    format!("relay sign failed: {e}"),
                );
            }
        };
        let outcomes = crate::relay::send_bundle_to_relays(relays, &body, &header).await;
        if crate::relay::accepted_count(&outcomes) > 0 {
            degenbot_bot::instruments::record(
                degenbot_bot::instruments::CensusEvent::SubmitOutcome {
                    outcome: "builder_relay_accepted",
                },
            );
            self.promote_broadcast(lease)?;
            return Ok(StepVerdict::Continue(alloy::primitives::keccak256(
                raw_signed,
            )));
        }
        degenbot_bot::telemetry::record_exception(
            degenbot_bot::telemetry::error_kind::SUBMIT_FAILURE,
            format_args!(
                "path {} every builder relay rejected the bundle",
                candidate.path_id
            ),
        );
        // Every relay refused to admit the bundle for its pinned block: the
        // same definitive non-liveness the auction arm reads.
        self.conclude_failed_delivery(
            candidate,
            &[DeliveryFailureClass::NotAdmitted],
            lease,
            raw_signed,
            "every builder relay rejected the bundle".to_string(),
        )
    }

    /// Stage 9, arm 3 — the raw fan-out.
    ///
    /// The SAME signed bytes go to every relay in `extra_broadcast`; an empty
    /// list means the read provider alone (the legacy single-endpoint
    /// behavior). First acceptance defines the tracked hash.
    ///
    /// Failure policy: [`crate::delivery::classify_delivery_failure`] decides
    /// from every attempt's class. A unanimously definitive refusal releases the
    /// lease; a node that already holds the bytes promotes the submission to a
    /// tracked broadcast; any ambiguous transport failure holds the lease and
    /// parks the ledger record in `DeliveryUnknown`. A failed delivery never
    /// reserves the pools, and no monitor is spawned unless the classifier
    /// promoted a live hash.
    ///
    /// # Errors
    ///
    /// [`crate::SubmissionError::Nonce`] when an accepted broadcast cannot be
    /// promoted on both nonce books.
    async fn deliver_public(
        &self,
        candidate: &SubmitCandidate,
        raw_signed: &Bytes,
        lease: &NonceLease,
    ) -> Result<StepVerdict<B256>, crate::SubmissionError> {
        let broadcast_targets: Vec<&AlloyProvider> = if self.extra_broadcast.is_empty() {
            vec![self.provider]
        } else {
            self.extra_broadcast
                .iter()
                .map(std::sync::Arc::as_ref)
                .collect()
        };
        let mut accepted_hash: Option<B256> = None;
        let mut failures: Vec<DeliveryFailureClass> = Vec::new();
        for relay_provider in &broadcast_targets {
            match relay_provider.eth_send_raw_transaction(raw_signed).await {
                Ok(hash) => {
                    accepted_hash.get_or_insert(hash);
                }
                Err(e) => {
                    degenbot_bot::telemetry::record_exception(
                        degenbot_bot::telemetry::error_kind::SUBMIT_FAILURE,
                        format_args!("path {} relay broadcast failed: {e}", candidate.path_id),
                    );
                    // Every attempt's class is evidence: one relay answering
                    // "already known" or one transport failure decides the
                    // verdict even beside a relay that refused outright.
                    failures.push(DeliveryFailureClass::from_provider_error(&e));
                }
            }
        }
        let Some(tx_hash) = accepted_hash else {
            return self.conclude_failed_delivery(
                candidate,
                &failures,
                lease,
                raw_signed,
                "all relays rejected the raw transaction".to_string(),
            );
        };
        degenbot_bot::instruments::record(degenbot_bot::instruments::CensusEvent::SubmitOutcome {
            outcome: "relay_accepted",
        });
        self.promote_broadcast(lease)?;
        Ok(StepVerdict::Continue(tx_hash))
    }

    /// Promote the lease on both books once a channel accepted the signed bytes.
    ///
    /// Two books, two calls — neither subsumes the other: the authority promotes
    /// the account lease to a broadcast (the nonce stays outstanding until the
    /// chain confirms it), while the lane's ledger promotes its own signed stamp
    /// for this strategy. Head reconciliation and the monitor read both, so
    /// collapsing this pair into one call leaves one book stale.
    ///
    /// # Errors
    ///
    /// [`crate::SubmissionError::Nonce`] when either book declines the
    /// promotion.
    fn promote_broadcast(&self, lease: &NonceLease) -> Result<(), crate::SubmissionError> {
        self.nonce_lane
            .authority()
            .record_broadcast(lease)
            .map_err(|e| crate::SubmissionError::Nonce(e.to_string()))?;
        self.nonce_lane
            .record_broadcast(lease)
            .map_err(|e| crate::SubmissionError::Nonce(e.to_string()))?;
        Ok(())
    }

    /// Apply the classifier's verdict to a failed delivery attempt: the one
    /// place a failure becomes a lease and ledger decision.
    ///
    /// - `Promote` — a channel already holds these exact bytes. The submission
    ///   is live, so it is promoted on both books and handed on as a tracked
    ///   submission under the signed bytes' hash (the same hash the provider
    ///   computes locally before broadcasting), which is what makes the caller
    ///   reserve the pools, record it, and spawn its monitor.
    /// - `Hold` — the attempt's fate is unknown. The ledger record parks in
    ///   `DeliveryUnknown` and the lease stays held: releasing it would let a
    ///   different transaction be signed over a possibly-live one.
    /// - `Release` — definitive non-liveness. The lease goes back so the next
    ///   candidate may stamp the nonce.
    ///
    /// # Errors
    ///
    /// [`crate::SubmissionError::Nonce`] when a promoted delivery cannot be
    /// recorded on both nonce books or the ledger refuses the hold.
    fn conclude_failed_delivery(
        &self,
        candidate: &SubmitCandidate,
        failures: &[DeliveryFailureClass],
        lease: &NonceLease,
        raw_signed: &Bytes,
        detail: String,
    ) -> Result<StepVerdict<B256>, crate::SubmissionError> {
        match classify_delivery_failure(failures) {
            DeliveryVerdict::Promote => {
                self.promote_broadcast(lease)?;
                Ok(StepVerdict::Continue(alloy::primitives::keccak256(
                    raw_signed,
                )))
            }
            DeliveryVerdict::Hold => {
                self.hold_delivery(lease)?;
                Self::record_broadcast_failure(candidate);
                Ok(StepVerdict::Skip(SkipReason::BroadcastFailed(detail)))
            }
            DeliveryVerdict::Release => {
                let _ = self.nonce_lane.release(lease);
                Self::record_broadcast_failure(candidate);
                Ok(StepVerdict::Skip(SkipReason::BroadcastFailed(detail)))
            }
        }
    }

    /// Park the delivery-unknown hold: the lane's ledger record moves to
    /// `DeliveryUnknown` and the lease stays reserved, because the bytes may be
    /// live at the nonce it holds.
    ///
    /// The write goes to the lane's ledger directly rather than through the
    /// `NonceLedger` seam: that seam carries the two writes a sign path makes
    /// through the lane, and this crate owns both the ledger type and the hold.
    ///
    /// # Errors
    ///
    /// [`crate::SubmissionError::Nonce`] when the ledger refuses the transition.
    /// The signing path always records a `Signed` record before delivery, so a
    /// refusal means the two books disagree about the submission.
    fn hold_delivery(&self, lease: &NonceLease) -> Result<(), crate::SubmissionError> {
        self.nonce_lane
            .ledger()
            .record_delivery_unknown(self.nonce_lane.strategy(), lease.nonce())
            .map(|_state| ())
            .map_err(|e| crate::SubmissionError::Nonce(e.to_string()))
    }

    /// The census pair every failed delivery shares: the skipped-broadcast
    /// outcome plus the profit the batch left on the table.
    ///
    /// The lease and ledger decision is deliberately NOT here — it belongs to
    /// the classifier's verdict ([`Self::conclude_failed_delivery`]).
    fn record_broadcast_failure(candidate: &SubmitCandidate) {
        degenbot_bot::instruments::record(degenbot_bot::instruments::CensusEvent::SubmitOutcome {
            outcome: "skipped_broadcast_failed",
        });
        degenbot_bot::instruments::record(degenbot_bot::instruments::CensusEvent::ProfitMissed {
            wei: candidate_net_wei(candidate),
        });
    }

    /// Stage 10a — reserve the path pools on the dispatcher and claim them for
    /// the rest of the batch.
    fn reserve_and_record(
        &self,
        path_pools: &HashSet<PoolKey>,
        state: &mut BatchState,
    ) -> StepVerdict<()> {
        #[expect(clippy::expect_used)] // poisoned sync-guard = process bug; panic loudly
        self.dispatcher
            .lock()
            .expect("dispatcher mutex poisoned")
            .reserve_pools(path_pools.clone());
        state.commit(path_pools);
        StepVerdict::Continue(())
    }

    /// Stage 10b — spawn the receipt monitor for the broadcast transaction.
    ///
    /// The dispatcher handle, the probe, and the lane are cloned into the
    /// spawned task (the `Arc<Mutex<Dispatcher>>` sharing pattern). The monitor
    /// releases the pools on confirm/expire; the authority owns the nonce
    /// lifecycle. An EXPIRED broadcast is declared dead and its authority slot
    /// released, so a dropped transaction cannot wedge the account's nonce
    /// prefix — a CONFIRMED broadcast is landed by the per-head reconcile
    /// instead, because its transaction is on the chain.
    fn spawn_monitor(
        &self,
        tx_hash: B256,
        nonce: u64,
        path_pools: HashSet<PoolKey>,
        candidate_net: f64,
    ) -> StepVerdict<()> {
        let dispatcher_clone = Arc::clone(self.dispatcher);
        let probe_clone = Arc::clone(&self.probe);
        let lane_clone = Arc::clone(self.nonce_lane);
        let submitted_tx = SubmittedTx::new(tx_hash, nonce, path_pools, self.current_block);
        #[expect(clippy::expect_used)] // poisoned sync-guard = process bug; panic loudly
        {
            self.dispatcher
                .lock()
                .expect("dispatcher mutex poisoned")
                .track_task(async move {
                    let outcome = monitor_pending_transaction_default(
                        submitted_tx,
                        &*probe_clone,
                        &dispatcher_clone,
                    )
                    .await;
                    if matches!(outcome, Ok(ref o) if o.is_expired()) {
                        let _ = lane_clone.release_broadcast(nonce);
                    }
                    match &outcome {
                        Ok(o) if o.is_confirmed() => {
                            degenbot_bot::instruments::record(
                                degenbot_bot::instruments::CensusEvent::MonitorOutcome {
                                    outcome: "confirmed",
                                },
                            );
                            degenbot_bot::instruments::record(
                                degenbot_bot::instruments::CensusEvent::ProfitRealized {
                                    wei: candidate_net,
                                },
                            );
                        }
                        Ok(_) => {
                            degenbot_bot::instruments::record(
                                degenbot_bot::instruments::CensusEvent::MonitorOutcome {
                                    outcome: "expired",
                                },
                            );
                        }
                        Err(ref e) => {
                            degenbot_bot::instruments::record(
                                degenbot_bot::instruments::CensusEvent::MonitorOutcome {
                                    outcome: "error",
                                },
                            );
                            degenbot_bot::telemetry::record_exception(
                                degenbot_bot::telemetry::error_kind::MONITOR_FAILURE,
                                format_args!("tx monitor error: {e}"),
                            );
                        }
                    }
                });
        }
        StepVerdict::Continue(())
    }
}

/// Sort + submit the gas-profitable candidates with mutual exclusivity.
///
/// The batch is one ordered pass over the candidates: every step is a named
/// stage on [`SubmitBatch`], and the loop acts on all their [`StepVerdict`]s the
/// same way (a skip becomes a [`SubmitRecord::Skipped`], an abort ends the
/// batch). The stages share no signature beyond that verdict on purpose — they
/// carry different payloads, one of them mutates the batch claim set, and the
/// three delivery arms have different failure policies (see
/// [`SubmitBatch::deliver`]).
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
/// A stopped batch still answers for every candidate it stopped: a refused
/// nonce stamp records the candidate in hand and the unprocessed tail as
/// [`SkipReason::NonceContended`], so no candidate leaves the pipeline without
/// an outcome.
///
/// # Parity gate (`docs/architecture/parity-gate.md`)
///
/// The submit order (net-desc), the mutual-exclusivity skip, the
/// `dry_run`/`inject_code` skip (with pools still committed), the
/// nonce→fee→access-list→sign→broadcast sequence, + the monitor spawn match
/// the Python oracle's `dispatch_profitable_results` submit tail.
///
/// # Errors
///
/// Returns `Err` only on an unrecoverable signer failure (the ECDSA
/// `sign_eip1559` error — a corrupt key; never for a validly-constructed
/// signer) or a refused nonce-bookkeeping promotion. Per-candidate RPC failures
/// are tolerated as [`SkipReason::BroadcastFailed`] records (the `continue` on
/// `Web3Exception`), and a refused nonce stamp ends the batch with the records
/// gathered so far instead of erroring.
///
/// # Panics
///
/// Panics if the `dispatcher` mutex is poisoned (a coordinated task
/// panicked while holding it — unrecoverable; matches the Python assumption
/// that the dispatcher state is always readable).
#[expect(clippy::too_many_arguments)] // the pipeline's inputs are the batch's inputs
pub async fn dispatch_and_submit(
    mut candidates: Vec<SubmitCandidate>,
    dispatcher: &Arc<Mutex<Dispatcher>>,
    provider: &AlloyProvider,
    signer: &TxSigner,
    probe: Arc<dyn ReceiptProbe + Send + Sync>,
    nonce_lane: &Arc<NonceLane<SubmissionLedger>>,
    current_block: u64,
    dry_run: bool,
    inject_code: bool,
    extra_broadcast: &[std::sync::Arc<AlloyProvider>],
    // The submission channel: a bundle target routes the bid EXCLUSIVELY
    // through `eth_sendBundle` on the searcher WS (no public broadcast, no
    // relays); `Public` fans the SAME signed bytes across the relays.
    target: SubmissionTarget,
) -> Result<SubmitOutcome, crate::SubmissionError> {
    // One Jaeger node per dispatch batch
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

    SubmitBatch::sort(&mut candidates);
    let batch = SubmitBatch {
        dispatcher,
        provider,
        signer,
        probe,
        nonce_lane,
        current_block,
        dry_run,
        inject_code,
        extra_broadcast,
        target: &target,
    };

    let mut outcome = SubmitOutcome::default();
    let mut state = BatchState::default();

    // T4: per-batch latency anchor — candidate loop start → each broadcast.
    let batch_start = std::time::Instant::now();
    for (index, candidate) in candidates.iter().enumerate() {
        SubmitBatch::record_candidate_economics(candidate);
        let path_pools = candidate.path_pools.clone();

        // The bookkeeping around the stages is uniform even though the stages
        // are not: a `Skip` records the typed reason and moves to the next
        // candidate, an `AbortBatch` records the reason against every candidate
        // it stops and ends the batch. What each stage decides stays inside its
        // own method.
        macro_rules! stage {
            ($verdict:expr) => {
                match $verdict {
                    StepVerdict::Continue(payload) => payload,
                    StepVerdict::Skip(reason) => {
                        outcome.records.push(SubmitRecord::Skipped {
                            path_id: candidate.path_id,
                            reason,
                        });
                        continue;
                    }
                    StepVerdict::AbortBatch(reason) => {
                        // The candidate in hand plus the whole unprocessed tail:
                        // a stopped batch reports each one, in submit order.
                        for blocked in &candidates[index..] {
                            outcome.records.push(SubmitRecord::Skipped {
                                path_id: blocked.path_id,
                                reason: reason.clone(),
                            });
                        }
                        break;
                    }
                }
            };
        }

        let () = stage!(batch.guard_mutual_exclusion(candidate, &path_pools, &state));
        let () = stage!(batch.guard_dry_run(&path_pools, &mut state));
        let () = stage!(batch.guard_inject_code(candidate, &path_pools, &mut state));
        let lease = stage!(batch.stamp_nonce());
        let nonce = lease.nonce();
        let tx_params = stage!(SubmitBatch::build_tx_params(candidate, nonce)?);
        let tx_params = stage!(batch.recompute_access_list(tx_params).await);
        let raw_signed = stage!(batch.sign_and_record(&tx_params, &lease)?);
        let tx_hash = stage!(batch.deliver(candidate, &raw_signed, &lease).await?);
        let () = stage!(batch.reserve_and_record(&path_pools, &mut state));

        degenbot_bot::instruments::record(degenbot_bot::instruments::CensusEvent::SubmitOutcome {
            outcome: "submitted",
        });
        degenbot_bot::instruments::record(degenbot_bot::instruments::CensusEvent::SubmitLatency {
            secs: batch_start.elapsed().as_secs_f64(),
        });
        outcome.records.push(SubmitRecord::Submitted {
            path_id: candidate.path_id,
            tx_hash,
            nonce,
        });

        // T4: realized profit — this candidate's net profit lands on the
        // realized counter iff its tx confirms.
        let () =
            stage!(batch.spawn_monitor(tx_hash, nonce, path_pools, candidate_net_wei(candidate)));
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
/// surface) + record them into the dispatcher's `block_priority_fees`
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
    use crate::submission_ledger::SubmissionState;
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
    fn lane(operator_nonce: u64) -> Arc<NonceLane<SubmissionLedger>> {
        books(operator_nonce).2
    }

    /// A lane plus the two nonce books it writes — the authority and the
    /// ledger — so a test can assert both sides of a delivery decision.
    fn books(
        operator_nonce: u64,
    ) -> (
        Arc<degenbot_substrate::nonce::NonceAuthority>,
        Arc<SubmissionLedger>,
        Arc<NonceLane<SubmissionLedger>>,
        degenbot_substrate::nonce::StrategyId,
    ) {
        use degenbot_substrate::nonce::{NonceAuthority, StrategyId};
        let authority = Arc::new(NonceAuthority::new(operator_nonce));
        let ledger = Arc::new(SubmissionLedger::new());
        let strategy = StrategyId::new("test-strategy");
        let lane = Arc::new(authority.mint_lane(strategy.clone(), Arc::clone(&ledger)));
        (authority, ledger, lane, strategy)
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
        // The node refused the signed bytes for good, so the classifier reads a
        // unanimously definitive set: the lease goes back and the next candidate
        // may stamp the same nonce. Nothing is on the wire, so no monitor is
        // spawned and the pool is never reserved; the ledger keeps the signed
        // record for head reconciliation to close out.
        let asserter = Asserter::new();
        asserter.push_success(&empty_access_list_response()); // access-list ok
        asserter.push_failure_msg("nonce too low: next nonce 8, tx nonce 7"); // definitive refusal
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::default()));
        let s = signer();
        let probe: Arc<dyn ReceiptProbe + Send + Sync> = Arc::new(NoopProbe);
        let (authority, ledger, lane, strategy) = books(7);

        let outcome = dispatch_and_submit(
            vec![candidate(1, 5_000_000_000u128, &[POOL_A])],
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
        // Definitive non-liveness: the lease is back on both books' terms.
        assert_eq!(
            authority.lease_of(&strategy),
            None,
            "a definitive refusal frees the reservation"
        );
        assert_eq!(authority.outstanding_nonces(), Vec::<u64>::new());
        assert_eq!(
            ledger.state_of(&strategy, 7),
            Some(SubmissionState::Signed),
            "the signed record stays outstanding for reconciliation"
        );
        // The freed nonce is re-issuable.
        assert_eq!(lane.stamp().expect("re-stamp the freed nonce").nonce(), 7);
    }

    #[tokio::test]
    async fn an_already_known_broadcast_promotes_to_a_tracked_submission() {
        // A node answering "already known" holds these exact signed bytes: the
        // submission is live, not failed. It is recorded as submitted under the
        // signed bytes' hash, promoted on both nonce books, and monitored — the
        // same shape an acknowledged broadcast takes.
        let asserter = Asserter::new();
        asserter.push_success(&empty_access_list_response());
        asserter.push_failure_msg("already known");
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::default()));
        let s = signer();
        let probe: Arc<dyn ReceiptProbe + Send + Sync> = Arc::new(NoopProbe);
        let (authority, ledger, lane, strategy) = books(7);

        let outcome = dispatch_and_submit(
            vec![candidate(1, 5_000_000_000u128, &[POOL_A])],
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

        assert_eq!(outcome.submitted_count(), 1);
        assert_eq!(outcome.skipped_count(), 0);
        let SubmitRecord::Submitted {
            tx_hash,
            nonce,
            path_id,
        } = &outcome.records[0]
        else {
            panic!("expected Submitted, got {:?}", outcome.records[0]);
        };
        assert_eq!((*path_id, *nonce), (1, 7));
        let record = ledger.record_of(&strategy, 7).expect("ledger record");
        assert_eq!(
            *tx_hash,
            record.bundle_hash(),
            "the tracked hash is keccak256 of the signed bytes"
        );
        assert_eq!(
            ledger.state_of(&strategy, 7),
            Some(SubmissionState::Broadcast)
        );
        assert_eq!(
            authority.outstanding_nonces(),
            vec![7],
            "the promotion keeps the nonce outstanding on the authority book"
        );
        assert!(dispatcher
            .lock()
            .unwrap()
            .is_pool_pending(&PoolKey::new(POOL_A)));
        assert_eq!(
            dispatcher.lock().unwrap().active_task_count(),
            1,
            "a promoted submission is monitored"
        );
        dispatcher.lock().unwrap().abort_all_tasks();
    }

    #[tokio::test]
    async fn an_unreadable_relay_answer_holds_the_lease_and_the_ledger_record() {
        // The relay's answer cannot be read back as a transaction hash. The
        // bytes were sent, so whether they are live is unknown: the classifier
        // holds — the lease stays reserved and the ledger record parks in
        // DeliveryUnknown — and no monitor is spawned, because there is no hash
        // to track.
        let asserter = Asserter::new();
        asserter.push_success(&empty_access_list_response());
        asserter.push_success(&serde_json::json!("0xnot-a-transaction-hash"));
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::default()));
        let s = signer();
        let probe: Arc<dyn ReceiptProbe + Send + Sync> = Arc::new(NoopProbe);
        let (authority, ledger, lane, strategy) = books(7);

        let outcome = dispatch_and_submit(
            vec![candidate(1, 5_000_000_000u128, &[POOL_A])],
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

        assert_eq!(outcome.skipped_count(), 1);
        assert_eq!(outcome.submitted_count(), 0);
        assert_eq!(
            authority.lease_of(&strategy).expect("held lease").nonce(),
            7,
            "ambiguity keeps the reservation: the bytes may be live"
        );
        assert_eq!(authority.outstanding_nonces(), vec![7]);
        assert_eq!(
            ledger.state_of(&strategy, 7),
            Some(SubmissionState::DeliveryUnknown)
        );
        assert_eq!(dispatcher.lock().unwrap().active_task_count(), 0);
        assert!(!dispatcher
            .lock()
            .unwrap()
            .is_pool_pending(&PoolKey::new(POOL_A)));
    }

    #[tokio::test]
    async fn a_declined_stamp_records_every_candidate_the_abort_stops() {
        // Another strategy's reservation sits below the confirmed head, so this
        // lane's stamp is declined before anything is signed. The batch stops —
        // and every candidate it stops leaves a blocked record instead of
        // vanishing into an unprocessed tail.
        let asserter = Asserter::new();
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::default()));
        let s = signer();
        let probe: Arc<dyn ReceiptProbe + Send + Sync> = Arc::new(NoopProbe);
        let (authority, ledger, lane, strategy) = books(5);
        let other = authority.mint_lane(
            degenbot_substrate::nonce::StrategyId::new("other"),
            Arc::new(SubmissionLedger::new()),
        );
        assert_eq!(
            other.stamp().expect("the other lane holds nonce 5").nonce(),
            5
        );
        authority.set_confirmed(6);

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

        assert_eq!(outcome.submitted_count(), 0);
        assert_eq!(
            outcome.records,
            vec![
                SubmitRecord::Skipped {
                    path_id: 1,
                    reason: SkipReason::NonceContended,
                },
                SubmitRecord::Skipped {
                    path_id: 2,
                    reason: SkipReason::NonceContended,
                },
            ],
            "the candidate in hand and the unprocessed tail are both recorded"
        );
        assert_eq!(outcome.skipped_count(), 2);
        assert!(
            asserter.read_q().is_empty(),
            "the batch stopped at the stamp: nothing was signed or broadcast"
        );
        assert_eq!(
            ledger.records(),
            Vec::new(),
            "no submission record exists for a candidate that never signed"
        );
        assert_eq!(authority.lease_of(&strategy), None);
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
        use crate::submission_ledger::{SubmissionLedger, SubmissionState};
        use degenbot_substrate::nonce::StrategyId;

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
        let authority = Arc::new(degenbot_substrate::nonce::NonceAuthority::new(42));
        let ledger = Arc::new(SubmissionLedger::new());
        let lane = Arc::new(authority.mint_lane(strategy.clone(), Arc::clone(&ledger)));

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
    /// The `dispatch_and_submit` span records the
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

    /// Traces of block 25913390: the 20 most-recent Jaeger traces
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

    // ── MEVBlocker bundle channel (wiring, doc how-to/searchers/bid) ──

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
        //
        // The arm's classified policy is Release: the bid is valid for exactly
        // the block it was pinned to and the auction books a bid only by
        // acknowledging it, so the pin bounds the failure's liveness window and
        // a round trip that ends without an acknowledgement leaves no bid for
        // that block. The lease therefore goes back, and the ledger keeps the
        // signed record for reconciliation.
        let asserter = Asserter::new();
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::default()));
        let s = signer();
        let probe: Arc<dyn ReceiptProbe + Send + Sync> = Arc::new(NoopProbe);
        let (authority, ledger, lane, strategy) = books(0);

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
            &lane,
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
        assert_eq!(
            authority.lease_of(&strategy),
            None,
            "the pinned bid's failure is definitive non-liveness for its slot"
        );
        assert_eq!(ledger.state_of(&strategy, 0), Some(SubmissionState::Signed));
        assert_eq!(dispatcher.lock().unwrap().active_task_count(), 0);
    }

    #[tokio::test]
    async fn builder_relay_arm_releases_the_lease_when_every_relay_rejects() {
        // No relay is listening, so every POST is a rejection and no relay
        // holds the bundle. As with the auction arm, the bid is pinned to one
        // block: the pin bounds the failure's liveness window, so the arm's
        // classified policy is Release and the lease goes back.
        let asserter = Asserter::new();
        asserter.push_success(&empty_access_list_response());
        let provider = mock_provider(&asserter);
        let dispatcher = Arc::new(Mutex::new(Dispatcher::default()));
        let s = signer();
        let probe: Arc<dyn ReceiptProbe + Send + Sync> = Arc::new(NoopProbe);
        let (authority, ledger, lane, strategy) = books(3);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);

        let target = SubmissionTarget::BuilderRelay {
            relays: vec![format!("http://{addr}")],
            target_raw: Bytes::from_static(&[0x02, 0xf8, 0x01]),
            block_number: 42,
        };
        let outcome = dispatch_and_submit(
            vec![candidate(4, 1_000_000_000u128, &[POOL_A])],
            &dispatcher,
            &provider,
            &s,
            probe,
            &lane,
            41,
            false,
            false,
            &[],
            target,
        )
        .await
        .expect("dispatch completes");

        assert_eq!(
            outcome.records,
            vec![SubmitRecord::Skipped {
                path_id: 4,
                reason: SkipReason::BroadcastFailed(
                    "every builder relay rejected the bundle".to_string()
                ),
            }]
        );
        assert_eq!(authority.lease_of(&strategy), None);
        assert_eq!(ledger.state_of(&strategy, 3), Some(SubmissionState::Signed));
        assert_eq!(dispatcher.lock().unwrap().active_task_count(), 0);
    }
}
