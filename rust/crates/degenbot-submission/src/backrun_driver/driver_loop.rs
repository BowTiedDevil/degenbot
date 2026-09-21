//! The backrun driver loop: the head frame feed and every runtime surface the
//! loop touches.
//!
//! Invariant surface: the loop is the only writer of the quarantine FSM and
//! the per-frame funnel's spent budget. It consumes the host-minted hub,
//! registry, and node join without owning them, and `LoopPhase` is the
//! loop's own protocol run state, driven through `DriverHandle`. The frame feed and the
//! quarantine rescue re-enter through the same `run_frame` argument list, so
//! a signature drift on either call site is a compile error.

#![expect(
    clippy::expect_used,
    reason = "driver start and loop wiring: fatal config, lock, and feed-registration failures exit the process loudly"
)]

use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::primitives::{Address, Bytes, B256, U256};
use degenbot_bot::bot_core::RouteRegistry;
use degenbot_bot::backrun::{gate_mined_target, Decision, BackrunConfig};
use degenbot_eventhub::{HeadSubscription, Hub};
use degenbot_rpc::backrun_feed::{BackrunFeed, BackrunFeedConfig};
use degenbot_rpc::head_watch::{HeadWatch, HeadWatchConfig};
use degenbot_rpc::provider::{AlloyProvider, DEFAULT_MAX_RETRIES};
use degenbot_simulation::sim::evm::frame_replay::ReplayableTx;
use degenbot_simulation::BlockSimHandle;
use parking_lot::Mutex as ParkingMutex;

use crate::backrun_strategy::BackrunStrategy;
use crate::dispatcher::Dispatcher;
use crate::frame_pipeline::{
    build_block_handle, load_fixture_frames, process_frame_with_prefix, trace_jsonl, MarketContext,
    PipelineConfig,
};
use crate::gap_quarantine::{NonceConsumed, ParkedFrame, Quarantine, QuarantineDecision};
use crate::gap_quarantine_journal::{
    self, ArchivedResolution, ParkRecord, QuarantineJournal, Resolution, ResolutionArchiveRecord,
};
use crate::monitor::ReceiptProbe;
use crate::signer::TxSigner;
use crate::submission_ledger::NonceLane;
use crate::submit::{dispatch_and_submit, SubmitCandidate};

use super::driver_boot::BackrunContext;
use super::driver_policy::{
    bid_submission_target, build_broadcast_relays, initial_wallet_gas_cost, priority_fee_wei,
    wallet_gas_cost_at, GAS_FLOOR_WEI,
};

/// How long the live loop waits on the head watch before servicing the frame
/// feed. The watch resolves the instant a header arrives (~12s apart), so a
/// healthy watch leaves this to time out most iterations; the bound is loop
/// latency, not head latency.
const HEAD_WATCH_WAIT: Duration = Duration::from_secs(2);

/// A watch silent this long is treated as dead: the loop polls for that
/// iteration while the driver task's watchdog reconnects in the background.
/// Must exceed the chain's block interval - mainnet blocks arrive ~12s apart,
/// so a threshold at or below that would poll on every block and defeat the
/// point of the subscription. Kept below the driver's 48s watchdog so the
/// fallback poll covers the reconnect window.
const HEAD_WATCH_STALE: Duration = Duration::from_secs(30);

/// The fallback head-poll cadence (the pre-subscription loop's tick).
const HEAD_POLL_TICK: Duration = Duration::from_millis(200);

struct BackrunProbe {
    provider: Arc<AlloyProvider>,
}

impl ReceiptProbe for BackrunProbe {
    fn receipt_found(
        &self,
        tx_hash: alloy::primitives::B256,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = crate::SubmissionResult<bool>> + Send + '_>,
    > {
        let provider = Arc::clone(&self.provider);
        Box::pin(async move {
            let rec = provider
                .get_transaction_receipt(&tx_hash.to_string())
                .await
                .map_err(|e| crate::SubmissionError::MonitorProbe(format!("{e}")))?;
            Ok(rec.is_some())
        })
    }
}

/// The terminal class of one funnel pass, as the rescue router consumes it.
///
/// A rescued frame must survive the boot fold after a transient replay
/// failure, so the two replay-seam failures are their own arms: they leave
/// the frame parked for the next frontier pass. Everything else is terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FrameOutcome {
    /// A bid was decided; terminal whether or not dispatch landed.
    Bid,
    /// The pass re-parked the frame (the interim pool-pred path can repeat a
    /// same-boundary re-park; the FSM guard and journal dedup bound it).
    GapPending,
    /// No replay handle served this head (transient).
    ReplayUnavailable,
    /// The replay died in an RPC hydrate (transient).
    ReplayFailed,
    /// A sequence predecessor failed structurally (unrunnable as fetched):
    /// the structural pool-pred lane owns it — record the guard WITH content,
    /// keep the frame parked, never resolve it.
    PredecessorMalformed,
    /// Any other terminal observe/drop, carrying its trace reason.
    Terminal(&'static str),
}

/// Collapse a detailed pass outcome to the rescue router's three classes.
pub(super) fn reentry_outcome(outcome: FrameOutcome) -> ReentryOutcome {
    match outcome {
        FrameOutcome::Bid | FrameOutcome::Terminal(_) => ReentryOutcome::Terminal,
        FrameOutcome::GapPending => ReentryOutcome::GapPending,
        // `PredecessorMalformed` is routed to the structural lane before this
        // router; when reached, its fallback is the same transient re-park.
        FrameOutcome::ReplayUnavailable
        | FrameOutcome::ReplayFailed
        | FrameOutcome::PredecessorMalformed => ReentryOutcome::Transient,
    }
}

/// Map an `Observe` reason to its outcome class; the replay-seam and
/// gap-pending reasons are named by [`crate::frame_pipeline`].
pub(super) fn outcome_for(reason: &'static str) -> FrameOutcome {
    match reason {
        "gap_pending" => FrameOutcome::GapPending,
        "replay_unavailable" => FrameOutcome::ReplayUnavailable,
        // `mispriced_transaction` (base fee rejected even after the disabled
        // retry) and a predecessor's retryable failure are transient: never
        // terminal and never guarded.
        "replay_failed" | "mispriced_transaction" | "predecessor_replay_failed" => {
            FrameOutcome::ReplayFailed
        }
        "predecessor_malformed" => FrameOutcome::PredecessorMalformed,
        other => FrameOutcome::Terminal(other),
    }
}

/// How a rescued frame's re-entry ended, for journal routing.
///
/// A resolve is written only for a terminal pass; a transient replay failure
/// keeps the original park record alive and re-parks the frame so the next
/// frontier pass retries, and a fresh gap-pending park is its own truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReentryOutcome {
    /// The frame's quarantine life is over (a bid, or a terminal observe/drop).
    Terminal,
    /// The pass re-parked the frame. In the interim pool-pred path a re-park
    /// can repeat the same gap; the FSM guard and the journal dedup bound those
    /// repeats, so this is not proof of a new boundary.
    GapPending,
    /// A transient replay failure; retry on the next frontier pass.
    Transient,
}

/// Apply one rescue re-entry's outcome to the journal and quarantine.
///
/// Terminal resolves after the pass (a dispatched bid or a terminal observe);
/// a gap-pending pass wrote its own fresh park record, which is the truth the
/// fold reads; a transient replay failure re-parks the ORIGINAL frame, whose
/// park record still stands. Terminal and gap-pending passes are disjoint, so
/// no resolve ever races a fresh park for the same hash.
pub(super) fn journal_reentry_outcome(
    frame: &ParkedFrame,
    outcome: ReentryOutcome,
    quarantine: &mut Quarantine,
    journal: Option<&mut QuarantineJournal>,
) {
    match outcome {
        ReentryOutcome::Transient => {
            quarantine.push(frame.clone());
        }
        ReentryOutcome::GapPending => {}
        ReentryOutcome::Terminal => {
            if let Some(journal) = journal {
                let resolved_unix_ms = now_unix_ms();
                if let Err(error) =
                    journal.record_resolve(frame.hash, Resolution::RescueConsumed, resolved_unix_ms)
                {
                    tracing::warn!(%error, "quarantine resolve not journaled");
                }
                let archived = ResolutionArchiveRecord::new(
                    frame,
                    ArchivedResolution::RescueConsumed,
                    resolved_unix_ms,
                );
                if let Err(error) = journal.record_resolution(&archived) {
                    tracing::warn!(%error, "quarantine resolution not archived");
                }
            }
        }
    }
}

/// One-slot memo of the last `GapPending` park written through the park path,
/// keyed `(hash, boundary)`. The fold replaces parks by hash, so a consecutive
/// identical re-park is invisible and is skipped; a non-consecutive duplicate
/// still appends.
pub(super) type GapParkMemo = Option<(B256, u64)>;

/// The feed path's predecessor prefix: always empty. The quarantine rescue arm
/// is the only caller that supplies a hydrated prefix; a feed frame replays
/// against the head state.
pub(super) const FEED_PREFIX: &[ReplayableTx] = &[];

/// Append a `GapPending` park to the journal unless it repeats the immediately
/// preceding `(hash, boundary)` park. Returns whether the line was written.
pub(super) fn record_gap_park(
    journal: Option<&mut QuarantineJournal>,
    record: &ParkRecord,
    hash: B256,
    boundary: u64,
    memo: &mut GapParkMemo,
) -> bool {
    if *memo == Some((hash, boundary)) {
        return false;
    }
    if let Some(journal) = journal {
        if let Err(error) = journal.record_park(record) {
            tracing::warn!(tx = %hash, %error, "quarantine park not journaled");
        }
    }
    *memo = Some((hash, boundary));
    true
}

#[expect(
    clippy::too_many_arguments,
    reason = "the frame handler takes the runtime surfaces it needs"
)]
#[expect(
    clippy::too_many_lines,
    reason = "capture -> pipeline -> dispatch reads top-to-bottom"
)]
pub(super) async fn run_frame(
    ev: &degenbot_rpc::backrun_feed::BackrunFeedEvent,
    prefix: &[ReplayableTx],
    rt: &mut MarketContext,
    strategy: &mut BackrunStrategy,
    provider: &Arc<AlloyProvider>,
    sim_client: &alloy::rpc::client::RpcClient,
    cfg: &BackrunConfig,
    pl: &PipelineConfig,
    handle: &mut Option<BlockSimHandle<'_>>,
    head: u64,
    spent: &mut U256,
    dispatcher: &Arc<Mutex<Dispatcher>>,
    nonce_lane: &Arc<NonceLane>,
    signer: Option<&TxSigner>,
    gap_probe: &crate::gap_probe::GapProbe,
    quarantine: &mut Quarantine,
    journal: Option<&mut QuarantineJournal>,
    gap_park_memo: &mut GapParkMemo,
) -> FrameOutcome {
    // Decode-stage reject: a frame whose gas field reads zero can never
    // pass the EVM's pre-checks (`CallGasCostMoreThanGasLimit` fires
    // structurally) — dropping here keeps the pipeline histogram
    // truthful about WHICH class the frame died in.
    if ev.gas == 0 {
        tracing::info!(
            "observe tx=0x{:x} reason=\"malformed_transaction\" (zero-gas)",
            ev.hash
        );
        trace_jsonl(
            "malformed_transaction",
            serde_json::json!({"tx": format!("0x{:x}", ev.hash), "zero_gas": true}),
        );
        return FrameOutcome::Terminal("malformed_transaction");
    }

    // Offline-review capture: the feed wire shape, verbatim in all its
    // fields (doc how-to/searchers/listen) keyed by frame hash.
    trace_jsonl(
        "frame",
        serde_json::json!({
            "hash": ev.hash.to_string(),
            "chain_id": ev.chain_id,
            "from": format!("0x{}", alloy::hex::encode(ev.from)),
            "to": ev.to.map(|a| format!("0x{}", alloy::hex::encode(a))),
            "value": format!("0x{:x}", ev.value),
            "data": format!("0x{}", alloy::hex::encode(&ev.data)),
            "gas": ev.gas,
            "max_fee_per_gas": ev.max_fee_per_gas,
            "max_priority_fee_per_gas": ev.max_priority_fee_per_gas,
            "nonce": ev.nonce,
            "tx_type": ev.tx_type,
            "received_unix_ms": ev.received_unix_ms,
        }),
    );
    let artifacts = process_frame_with_prefix(
        strategy, rt, provider, sim_client, cfg, pl, handle, ev, prefix, head, *spent,
    )
    .await;
    trace_jsonl(
        "stages",
        serde_json::json!({
            "tx": ev.hash.to_string(),
            "stages": artifacts.stages.to_json(),
            "head": head,
        }),
    );
    // The decision + its inputs, offline-review readable: the tracing line
    // alone leaves the funnel's last stage out of the JSONL.
    let (decision_kind, decision_reason, decision_bid) = match &artifacts.decision {
        Decision::Bid { bid_wei } => ("bid", None, Some(bid_wei.to_string())),
        Decision::Observe { reason } => ("observe", Some((*reason).to_string()), None),
        Decision::Drop { reason } => ("drop", Some((*reason).to_string()), None),
    };
    trace_jsonl(
        "decide",
        serde_json::json!({
            "tx": ev.hash.to_string(),
            "decision": decision_kind,
            "reason": decision_reason,
            "bid_wei": decision_bid,
            "composed_any": artifacts.submit_calldata.is_some(),
            "requested_bid": artifacts.requested_bid.to_string(),
            "spent": spent.to_string(),
            "stop_file": cfg.stop_file.exists(),
        }),
    );
    if let Some(degenbot_simulation::sim::evm::frame_replay::ReplayFrameError::GapPending {
        claimed,
        expected,
    }) = &artifacts.replay_frame_error
    {
        let parked = ParkedFrame {
            hash: ev.hash,
            from: ev.from,
            to: ev.to,
            value: ev.value,
            data: ev.data.clone(),
            gas: ev.gas,
            max_fee_per_gas: ev.max_fee_per_gas,
            max_priority_fee_per_gas: ev.max_priority_fee_per_gas,
            claimed_nonce: ev.nonce,
            expected_at_capture: *expected,
            chain_id: ev.chain_id,
            tx_type: ev.tx_type,
            access_list: ev.access_list.clone(),
            received_unix_ms: ev.received_unix_ms,
        };
        let parked_count = quarantine.push(parked);
        let record = ParkRecord::new(ev, *expected, now_unix_ms());
        if !record_gap_park(journal, &record, ev.hash, *expected, gap_park_memo) {
            trace_jsonl(
                "quarantine",
                serde_json::json!({
                    "action": "park_deduped",
                    "tx": ev.hash.to_string(),
                    "expected_nonce": expected,
                }),
            );
        }
        trace_jsonl(
            "quarantine",
            serde_json::json!({
                "action": "park",
                "tx": ev.hash.to_string(),
                "claimed_nonce": claimed,
                "expected_nonce": expected,
                "parked": parked_count,
                "gap": claimed - expected,
            }),
        );
    }

    if let Decision::Observe {
        reason: "gap_pending",
    } = &artifacts.decision
    {
        let probe_out = gap_probe.probe_gap(ev.from, ev.nonce).await;
        trace_jsonl(
            "gap_probe",
            serde_json::json!({
                "tx": ev.hash.to_string(),
                "sender": format!("0x{:x}", ev.from),
                "claimed_nonce": ev.nonce,
                "probe": probe_out,
            }),
        );
    }
    // Bid-path liveness: probe the target's receipt before any dispatch. A
    // mined target can never be backrun -- observe `already_settled`. A probe
    // failure carries no positive evidence, so it never kills the bid; the
    // MEVBlocker bundle's block anchoring is the final backstop.
    let target_mined = if matches!(artifacts.decision, Decision::Bid { .. }) {
        BackrunProbe {
            provider: Arc::clone(provider),
        }
        .receipt_found(ev.hash)
        .await
        .unwrap_or(false)
    } else {
        false
    };
    if target_mined {
        trace_jsonl(
            "bid_gate",
            serde_json::json!({
                "tx": ev.hash.to_string(),
                "target_mined": true,
                "reason": "already_settled",
            }),
        );
    }
    match gate_mined_target(artifacts.decision, target_mined) {
        Decision::Bid { bid_wei } => {
            let Some(s) = signer else {
                tracing::warn!("bid decided without a signer loaded - skipping");
                return FrameOutcome::Bid;
            };
            // Defense in depth: decide() already refuses zero bids; this
            // refusal keeps a bare-sweep bid out of the auction even if a
            // future refactor reintroduces a fallback.
            let Some(cd) = artifacts.submit_calldata.clone() else {
                tracing::warn!(
                    tx = %ev.hash,
                    "bid decided without a composed candidate - refusing"
                );
                return FrameOutcome::Bid;
            };
            let base_fee_next = provider
                .get_block(head)
                .await
                .ok()
                .flatten()
                .and_then(|b| b.header.base_fee_per_gas)
                .map_or(30_000_000_000u128, |x| u128::from(x) * 12 / 10);
            let priority_fee: u128 = priority_fee_wei(cfg);
            // Honest economics per `SubmitCandidate`'s own contract: gross
            // is the solved profit, net subtracts the wallet's gas burn,
            // gas_used is the estimate (never a placeholder).
            let gross_profit = U256::from(
                artifacts
                    .economics
                    .as_ref()
                    .map_or(0u128, |e| e.gross_profit_wei),
            );
            let net_profit = U256::from(artifacts.economics.as_ref().map_or(0u128, |e| {
                e.gross_profit_wei.saturating_sub(e.wallet_gas_cost_wei)
            }));
            let candidate = SubmitCandidate {
                path_id: u64::from_be_bytes(ev.hash.0[0..8].try_into().expect("8 bytes")),
                gross_profit,
                net_profit,
                gas_used: cfg.bundle_gas_est,
                priority_fee,
                base_fee_next,
                execute_calldata: cd,
                executor_address: pl.exec,
                access_list: None,
                path_pools: HashSet::new(),
            };
            // The private-broadcast arm: with `strategy.backrun.mevblocker_url`
            // set, the signed backrun goes raw to that private endpoint first
            // and to the chain node as the public fallback relay (the order
            // `build_broadcast_relays` preserves). The URL augments the
            // broadcast fan-out only; `bid_submission_target` never reads it,
            // so the target stays the bundle-only arm regardless.
            let broadcast_relays = build_broadcast_relays(cfg, provider).await;
            let target = bid_submission_target(cfg, ev.hash, head + 1);
            match dispatch_and_submit(
                vec![candidate],
                dispatcher,
                provider,
                s,
                Arc::new(BackrunProbe {
                    provider: Arc::clone(provider),
                }),
                nonce_lane,
                head,
                cfg.dry_run,
                false,
                &broadcast_relays,
                target,
            )
            .await
            {
                Ok(outcome) => {
                    if outcome.submitted_count() > 0 {
                        // The wallet's true outflow per dispatched bid is
                        // the GAS (the bribe comes from flash proceeds); the
                        // budget tracks what the bank can actually lose.
                        *spent += U256::from(
                            artifacts
                                .economics
                                .as_ref()
                                .map_or(0u128, |e| e.wallet_gas_cost_wei),
                        );
                    }
                    tracing::info!(
                        tx = %ev.hash,
                        target = %ev.hash,
                        bid_wei = %bid_wei,
                        submitted = outcome.submitted_count(),
                        skipped = outcome.skipped_count(),
                        "bid dispatched"
                    );
                }
                Err(e) => tracing::warn!(tx = %ev.hash, error = %e, "bid dispatch failed"),
            }
            FrameOutcome::Bid
        }
        Decision::Observe { reason } => {
            tracing::info!(tx = %ev.hash, reason, "observe");
            outcome_for(reason)
        }
        Decision::Drop { reason } => {
            tracing::debug!(tx = %ev.hash, reason, "drop");
            FrameOutcome::Terminal(reason)
        }
    }
}

/// The current wall clock in unix milliseconds.
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// One canonical block-hash read (the reorg check's truth).
async fn canonical_block_hash(provider: &AlloyProvider, block: u64) -> Option<B256> {
    provider
        .get_block(block)
        .await
        .ok()
        .flatten()
        .map(|b| b.header.hash)
}

/// Nonce-lane evidence gate: only a decoded `u64` counts. A transport error
/// or an oversized value is NO evidence - the caller must skip, never feed
/// a sentinel that would read as `nonce_consumed` for every tracked frame.
pub(super) fn nonce_lane_evidence<E>(read: Result<U256, E>) -> Option<u64> {
    u64::try_from(read.ok()?).ok()
}

/// The pool-known predecessor set for one sender tick. A failed pending-lane
/// read is NO evidence: the gap stays empty, so the tick still runs its
/// latest-lane classification (a mined frontier rescues with an empty prefix
/// regardless of the pool view).
pub(super) fn pool_known_gap_for_tick(
    quarantine: &Quarantine,
    sender: Address,
    head_nonce: u64,
    pending_count: Option<u64>,
) -> Vec<u64> {
    pending_count.map_or_else(Vec::new, |pending| {
        quarantine.pool_known_gap(sender, head_nonce, pending)
    })
}

/// Decode one `eth_getTransactionBySenderAndNonce` result into the replay
/// shape. A null result (no such tx) and a missing or unparseable field are
/// both hydration failures: the frame must never replay against a guessed
/// predecessor.
pub(super) fn decode_predecessor(
    sender: Address,
    nonce: u64,
    value: &serde_json::Value,
) -> Result<ReplayableTx, &'static str> {
    let obj = value
        .as_object()
        .ok_or("predecessor is not a transaction object")?;
    let get_str = |key: &str| obj.get(key).and_then(serde_json::Value::as_str);
    let to = match get_str("to") {
        Some(s) if !s.is_empty() => Some(s.parse::<Address>().map_err(|_| "unparseable to")?),
        _ => None,
    };
    let data_hex = get_str("input")
        .or_else(|| get_str("data"))
        .ok_or("missing data")?;
    let data =
        alloy::hex::decode(data_hex.trim_start_matches("0x")).map_err(|_| "unparseable data")?;
    let value_wei = get_str("value")
        .and_then(|s| U256::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .ok_or("unparseable value")?;
    let gas = u64::from_str_radix(
        get_str("gas")
            .ok_or("missing gas")?
            .trim_start_matches("0x"),
        16,
    )
    .map_err(|_| "unparseable gas")?;
    // A type-0 (legacy) envelope carries a single `gasPrice`; the node may
    // return that shape for the pending lookup, so accept it for both fee
    // fields. Missing both is structural: the replay cannot price from nothing.
    let parse_u128_hex = |raw: &str| u128::from_str_radix(raw.trim_start_matches("0x"), 16).ok();
    let gas_price = get_str("gasPrice").and_then(parse_u128_hex);
    let max_fee_per_gas = get_str("maxFeePerGas")
        .and_then(parse_u128_hex)
        .or(gas_price)
        .ok_or("missing maxFeePerGas and gasPrice")?;
    let max_priority_fee_per_gas = get_str("maxPriorityFeePerGas")
        .and_then(parse_u128_hex)
        .or(gas_price)
        .ok_or("missing maxPriorityFeePerGas and gasPrice")?;
    let tx_nonce = u64::from_str_radix(
        get_str("nonce")
            .ok_or("missing nonce")?
            .trim_start_matches("0x"),
        16,
    )
    .map_err(|_| "unparseable nonce")?;
    if tx_nonce != nonce {
        return Err("predecessor nonce mismatch");
    }
    Ok(ReplayableTx {
        from: sender,
        to,
        value: value_wei,
        data: Bytes::from(data),
        gas_limit: gas,
        max_fee_per_gas,
        max_priority_fee_per_gas,
        nonce: tx_nonce,
    })
}

/// Why a predecessor prefix could not hydrate, split by whether a retry can
/// ever change the answer.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HydrationFailure {
    /// A transport error or a null lookup: endpoint skew, retried.
    Transient(&'static str),
    /// The node returned a shape that cannot decode (or a tx object without a
    /// usable hash): permanent for the pool-pred lane. Carries the tx hashes
    /// lifted from the returned shapes (including the failing one) so the
    /// structural guard can tell a replaced predecessor from the same one.
    Structural {
        reason: &'static str,
        hashes: Vec<B256>,
    },
}

/// One hydrated predecessor prefix: the replayable txs plus their tx hashes
/// (the guard's content identity). The hash is lifted from the same node JSON
/// independently of the tx decode; a missing or zero hash is a broken shape
/// and fails the hydration `Structural`.
#[derive(Debug)]
struct HydratedPredecessors {
    txs: Vec<ReplayableTx>,
    hashes: Vec<B256>,
}

impl HydratedPredecessors {
    /// The `(nonce, hash)` content identity the re-park guard compares.
    fn content(&self) -> Vec<(u64, B256)> {
        self.txs
            .iter()
            .map(|tx| tx.nonce)
            .zip(self.hashes.iter().copied())
            .collect()
    }
}

/// Lift a predecessor's tx hash from its node JSON. A shape without a usable
/// (non-zero) hash is broken: the guard's content identity would be a lie, and
/// `B256::ZERO` could silently equal a later `B256::ZERO` and never re-arm,
/// so the caller fails the hydration `Structural` rather than zero-filling.
pub(super) fn predecessor_hash(value: &serde_json::Value) -> Option<B256> {
    value
        .get("hash")
        .and_then(serde_json::Value::as_str)
        .and_then(|raw| raw.trim_start_matches("0x").parse::<B256>().ok())
        .filter(|hash| !hash.is_zero())
}

/// Hydrate the pool-pred rescue's predecessor prefix, ascending. ONE
/// `eth_getTransactionBySenderAndNonce` per nonce; any miss or decode failure
/// fails the whole prefix. The prefix exists to give the frame replay a
/// fake-mined queue, so a partially hydrated prefix is worse than none.
async fn hydrate_predecessors(
    client: &alloy::rpc::client::RpcClient,
    sender: Address,
    nonces: &[u64],
) -> Result<HydratedPredecessors, HydrationFailure> {
    let mut prefix = HydratedPredecessors {
        txs: Vec::with_capacity(nonces.len()),
        hashes: Vec::with_capacity(nonces.len()),
    };
    for &nonce in nonces {
        let nonce_hex = format!("0x{nonce:x}");
        let value = client
            .request::<(Address, String), serde_json::Value>(
                std::borrow::Cow::from("eth_getTransactionBySenderAndNonce"),
                (sender, nonce_hex),
            )
            .await
            .map_err(|_| HydrationFailure::Transient("predecessor lookup failed"))?;
        if value.is_null() {
            return Err(HydrationFailure::Transient("predecessor missing"));
        }
        // Lift the content identity first: the node returns the hash even when
        // the tx shape fails to decode, and the structural guard needs it.
        let Some(hash) = predecessor_hash(&value) else {
            return Err(HydrationFailure::Structural {
                reason: "predecessor missing hash",
                hashes: prefix.hashes,
            });
        };
        prefix.hashes.push(hash);
        let decoded = decode_predecessor(sender, nonce, &value).map_err(|reason| {
            HydrationFailure::Structural {
                reason,
                hashes: prefix.hashes.clone(),
            }
        })?;
        prefix.txs.push(decoded);
    }
    Ok(prefix)
}

/// A failed predecessor hydration is the endpoint-skew transient: the frame
/// survives and retries once the pool view and the replay view agree. No
/// resolve is written -- the original park record still stands.
pub(super) fn route_failed_hydration(
    frame: &ParkedFrame,
    quarantine: &mut Quarantine,
    journal: Option<&mut QuarantineJournal>,
) {
    journal_reentry_outcome(frame, ReentryOutcome::Transient, quarantine, journal);
}

/// A structurally undecodable predecessor permanently disqualifies the
/// pool-pred lane for this frame: re-requesting the same list returns the same
/// shape. Keep the frame tracked -- the mined frontier needs no predecessors --
/// and record the guard so the identical pool-pred rescue is suppressed until
/// the predecessor set changes.
pub(super) fn route_unhydratable_hydration(
    frame: &ParkedFrame,
    predecessors: &[u64],
    content: &[B256],
    reason: &'static str,
    quarantine: &mut Quarantine,
    journal: Option<&mut QuarantineJournal>,
) {
    quarantine.record_unhydratable_guard_with_content(frame.hash, predecessors, content);
    journal_reentry_outcome(frame, ReentryOutcome::Transient, quarantine, journal);
    trace_jsonl(
        "quarantine",
        serde_json::json!({
            "action": "rescue_unhydratable",
            "tx": frame.hash.to_string(),
            "predecessor_nonces": predecessors,
            "reason": reason,
            "permanent": true,
        }),
    );
    tracing::warn!(
        tx = %frame.hash,
        reason,
        "pool-pred predecessor structurally unhydratable - pool-pred lane suppressed"
    );
}

/// The node's `finalized` tag (ONE read per head advance). Never block-count
/// arithmetic: the tag tracks the real 2-epoch lag, missed slots and all.
async fn finalized_block_number(provider: &AlloyProvider) -> Option<u64> {
    provider
        .provider_arc()
        .get_block_by_number(alloy::eips::BlockNumberOrTag::Finalized)
        .await
        .ok()
        .flatten()
        .map(|b| b.header.number)
}

/// Classify a consumed nonce with ONE receipt probe on the frame's own hash:
/// a receipt is `MinedAt`; its absence is `SlotTakenAt`, resolved to the
/// same-nonce tx's carrying block when the index can see it. Evidence only:
/// both the carrying block AND its hash must come from the node's records
/// of the chain - never the observed head (the D2 adversarial-review find:
/// a fallback anchored to the live canonical head writes a block/hash the
/// reorg check then verifies against itself, an unrevivable fabricated
/// death). `None` means no block evidence could be established; the frame
/// stays tracked and the next head retries.
async fn classify_consumption(
    provider: &AlloyProvider,
    client: &alloy::rpc::client::RpcClient,
    frame: &ParkedFrame,
) -> Option<NonceConsumed> {
    let receipt = provider
        .get_transaction_receipt(&frame.hash.to_string())
        .await
        .ok()
        .flatten();
    if let Some(receipt) = receipt {
        if let (Some(block), Some(block_hash)) = (receipt.block_number, receipt.block_hash) {
            return Some(NonceConsumed::MinedAt { block, block_hash });
        }
        return None;
    }
    let nonce_hex = format!("0x{:x}", frame.claimed_nonce);
    let other = client
        .request::<(Address, String), serde_json::Value>(
            std::borrow::Cow::from("eth_getTransactionBySenderAndNonce"),
            (frame.from, nonce_hex),
        )
        .await
        .ok();
    let resolved = other.as_ref().and_then(|value| {
        let block = value
            .get("blockNumber")
            .and_then(serde_json::Value::as_str)
            .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())?;
        let block_hash = value
            .get("blockHash")
            .and_then(serde_json::Value::as_str)
            .and_then(|s| s.parse::<B256>().ok())?;
        let by = value
            .get("hash")
            .and_then(serde_json::Value::as_str)
            .and_then(|s| s.parse::<B256>().ok());
        Some((block, block_hash, by))
    });
    let (block, block_hash, by) = resolved?;
    Some(NonceConsumed::SlotTakenAt {
        block,
        block_hash,
        by,
    })
}

/// Reload the durable quarantine journal and re-park the still-pending frames
/// exactly as they left: a tracked park re-enters tracked, a tentative park
/// re-enters tentative with its consumption evidence. Nothing is dropped for
/// age or for a closed nonce -- the head-advance FSM tick classifies closed
/// nonces (one receipt probe) and waits for finality.
///
/// Returns the append handle (None when persistence is unavailable -- a journal
/// problem must never abort the bot). A missing journal, an unreadable one, and
/// corrupt lines are all non-fatal.
/// Resolve the quarantine journal path: the lane namespace when this driver
/// is hosted (another strategy may share the process), the process-global
/// state root for a standalone single-strategy boot.
pub(super) fn quarantine_journal_path(namespace_root: Option<&Path>) -> std::io::Result<PathBuf> {
    match namespace_root {
        Some(root) => Ok(root.join(gap_quarantine_journal::JOURNAL_FILE_NAME)),
        None => degenbot_runs::resolve_state_root()
            .map(|root| root.join(gap_quarantine_journal::JOURNAL_FILE_NAME)),
    }
}

fn reload_quarantine(
    quarantine: &mut Quarantine,
    namespace_root: Option<&Path>,
) -> Option<QuarantineJournal> {
    let path = match quarantine_journal_path(namespace_root) {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!(%error, "quarantine journal root unavailable - persistence off");
            return None;
        }
    };
    let read = match gap_quarantine_journal::read_pending(&path) {
        Ok(read) => read,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "quarantine journal unreadable - starting empty");
            return QuarantineJournal::open(&path).ok();
        }
    };
    if read.skipped > 0 {
        tracing::warn!(
            skipped = read.skipped,
            path = %path.display(),
            "quarantine journal corrupt lines skipped"
        );
    }
    let mut reparked: Vec<ParkRecord> = Vec::new();
    let mut tentative = 0usize;
    let mut dropped = 0usize;
    for record in &read.pending {
        match record.to_parked_frame() {
            Ok(frame) => {
                let hash = frame.hash;
                quarantine.push(frame);
                if let Some(consumed) = record.tentative {
                    quarantine.enter_tentative(hash, consumed.to_consumed());
                    tentative += 1;
                }
                reparked.push(record.clone());
            }
            Err(error) => {
                dropped += 1;
                tracing::warn!(
                    tx = %record.frame.hash,
                    %error,
                    "quarantine reload record undecodable - dropped"
                );
            }
        }
    }
    if let Err(error) = gap_quarantine_journal::compact(&path, &reparked) {
        tracing::warn!(%error, path = %path.display(), "quarantine journal compaction failed");
    }
    tracing::info!(
        journal = %path.display(),
        loaded = read.pending.len(),
        reparked = reparked.len(),
        tentative,
        dropped,
        skipped = read.skipped,
        "quarantine journal reloaded"
    );
    QuarantineJournal::open(&path).ok()
}

/// The protocol run phase a running driver loop walks. `Stopped` is
/// terminal; the operator-facing pose lives on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopPhase {
    /// Boot is in flight; the loop has not been polled.
    Starting,
    /// The loop is polling.
    Running,
    /// A stop was requested; the loop is draining.
    Stopping,
    /// Terminal: the loop has returned.
    Stopped,
}

impl LoopPhase {
    /// Whether the driver has returned and can never run again.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Stopped)
    }

    /// Boot finished: `Starting -> Running`.
    ///
    /// # Errors
    ///
    /// [`LoopDecline::StartRequiresStarting`] from any other state.
    pub const fn on_running(self) -> Result<Self, LoopDecline> {
        if matches!(self, Self::Starting) {
            Ok(Self::Running)
        } else {
            Err(LoopDecline::StartRequiresStarting)
        }
    }

    /// A stop request: a live driver -> `Stopping`.
    ///
    /// # Errors
    ///
    /// [`LoopDecline::StopRequiresLive`] from a stopping or stopped driver.
    pub const fn on_stop(self) -> Result<Self, LoopDecline> {
        if matches!(self, Self::Starting | Self::Running) {
            Ok(Self::Stopping)
        } else {
            Err(LoopDecline::StopRequiresLive)
        }
    }

    /// The loop returned: any non-terminal state -> `Stopped`.
    ///
    /// # Errors
    ///
    /// [`LoopDecline::StoppedRequiresLive`] from a stopped driver.
    pub const fn on_stopped(self) -> Result<Self, LoopDecline> {
        if self.is_terminal() {
            Err(LoopDecline::StoppedRequiresLive)
        } else {
            Ok(Self::Stopped)
        }
    }
}

/// Why a lifecycle move was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LoopDecline {
    /// `running` is only legal from [`LoopPhase::Starting`].
    #[error("running requires the Starting state")]
    StartRequiresStarting,
    /// `stop` is only legal from a live state.
    #[error("stop requires a live (Starting or Running) state")]
    StopRequiresLive,
    /// `stopped` refuses the terminal state.
    #[error("stopped refuses an already-terminal driver")]
    StoppedRequiresLive,
}

#[derive(Debug)]
pub(super) struct LoopShared {
    pub(super) state: ParkingMutex<LoopPhase>,
    pub(super) stop: AtomicBool,
}

impl LoopShared {
    pub(super) fn new() -> Self {
        Self {
            state: ParkingMutex::new(LoopPhase::Starting),
            stop: AtomicBool::new(false),
        }
    }

    /// Record that the loop is live. A stop requested before the first poll
    /// keeps the `Stopping` state so the loop can drain instead of resurrecting.
    pub(super) fn begin_running(&self) {
        let mut state = self.state.lock();
        if let Ok(next) = state.on_running() {
            *state = next;
        }
    }

    pub(super) fn request_stop(&self) -> Result<LoopPhase, LoopDecline> {
        let mut state = self.state.lock();
        let next = state.on_stop()?;
        *state = next;
        self.stop.store(true, Ordering::Relaxed);
        Ok(next)
    }

    pub(super) fn mark_stopped(&self) {
        let mut state = self.state.lock();
        if let Ok(next) = state.on_stopped() {
            *state = next;
        }
    }

    pub(super) fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

/// The loop future the handle drives. Boxed so the handle owns it without the
/// bin naming the driver's generic stack. Deliberately NOT `Send`: the loop's
/// replay stack holds `Rc`-backed buffers, so a host drives it on a dedicated
/// single-thread runtime (a standalone boot polls it inline).
type RunFuture = Pin<Box<dyn Future<Output = ()> + 'static>>;

/// A started driver loop. [`wait`](Self::wait) drives it to completion;
/// [`stop`](Self::stop) requests an early exit.
pub struct DriverHandle {
    pub(super) shared: Arc<LoopShared>,
    pub(super) run: Option<RunFuture>,
}

impl DriverHandle {
    /// The driver's current lifecycle state.
    #[must_use]
    pub fn state(&self) -> LoopPhase {
        *self.shared.state.lock()
    }

    /// Request the loop stop. The flag stays armed across repeated calls,
    /// but only the first live request succeeds.
    ///
    /// # Errors
    ///
    /// [`LoopDecline::StopRequiresLive`] once the driver is already
    /// stopping or stopped.
    pub fn stop(&self) -> Result<LoopPhase, LoopDecline> {
        self.shared.request_stop()
    }

    /// Drive the driver loop to completion. Polled inline by the bin, so a driver
    /// panic unwinds the caller rather than being swallowed by a task boundary.
    pub async fn wait(mut self) {
        if let Some(run) = self.run.take() {
            run.await;
        }
    }
}

/// The owned boot products the loop needs; grouping them keeps the loop
/// signature readable and keeps the boot/loop split explicit.
struct LoopBoot {
    provider: Arc<AlloyProvider>,
    runtime: MarketContext,
    strategy: BackrunStrategy,
    pl: PipelineConfig,
    dispatcher: Arc<Mutex<Dispatcher>>,
    nonce_lane: Arc<NonceLane>,
    signer: Option<TxSigner>,
    gap_probe: crate::gap_probe::GapProbe,
    sim_client: alloy::rpc::client::RpcClient,
    fixture_frames: Option<Vec<degenbot_rpc::backrun_feed::BackrunFeedEvent>>,
    head_ws_url: Option<String>,
    namespace_root: Option<PathBuf>,
}

/// The backrun driver entry point.
pub struct BackrunDriver;

impl BackrunDriver {
    /// Boot the driver and return a handle whose [`DriverHandle::wait`] runs it.
    ///
    /// The hub, registry, and node join are host-minted and only borrowed for
    /// the driver's lifetime; boot failures panic exactly as the single-driver bin
    /// did, so the caller's panic behavior is unchanged. Only one driver may
    /// attach to a given hub: the feed registration panics on a second `start`
    /// sharing the same hub.
    ///
    /// # Panics
    ///
    /// The driver boot panics on a malformed config (an unparseable sim URL, an
    /// unreadable or malformed key file, an unparseable executor/owner
    /// address, or a failed head fetch), preserving the single-driver bin's
    /// loud-failure behavior.
    pub async fn start(
        cfg: BackrunConfig,
        hub: Arc<Hub>,
        route_registry: Option<Arc<RouteRegistry>>,
        ctx: BackrunContext,
    ) -> DriverHandle {
        let BackrunContext {
            connector_db,
            head_ws_url,
            provider,
            namespace_root,
            nonce_lane,
        } = ctx;
        // The bundle-sim client (`strategy.backrun.sim_url`, default: the chain
        // node the frames replay against). READ/SIM ONLY -- `eth_callMany` never
        // broadcasts, and this client is passed nothing else. The sim MUST run
        // on an endpoint that actually serves `eth_callMany`; MEVBlocker's
        // /fast http tier answers method-missing for it, so the node is the
        // fallback and /fast stays available as an explicit override.
        let sim_client = alloy::rpc::client::ClientBuilder::default().http(
            cfg.sim_url
                .clone()
                .unwrap_or_else(|| cfg.rpc_url.clone())
                .parse()
                .expect("strategy.backrun.sim_url parses as an http url"),
        );
        // The gap-boundary probe samples the chain node's pending-pool lanes
        // whenever a frame claims a nonce ahead of the parent state.
        let gap_probe = crate::gap_probe::GapProbe::new(sim_client.clone());

        // Bid mode legality was already decided in `decide`; the signer only
        // loads when the key material exists so observe-only runs need none.
        let signer: Option<TxSigner> = cfg.key_file.as_ref().map(|p| {
            let hex = std::fs::read_to_string(p)
                .expect("strategy.backrun.key_file readable")
                .trim()
                .to_string();
            TxSigner::from_key_hex(&hex, 1)
                .expect("strategy.backrun.key_file parses as a secp256k1 key")
        });

        // Dry-run (offline-review): a captured frame JSONL replaces the live
        // feed — the capture's frames are processed once, in order.
        let fixture_frames = cfg.dry_run_jsonl.as_ref().map(|p| {
            let frames = load_fixture_frames(p);
            tracing::info!(frames = frames.len(), path = %p.display(), "dry-run fixture loaded");
            frames
        });

        // The offline fixture's pinned head: with the `fixture_head` facet set
        // the dry-run replays captured frames against the chain view they were
        // pending in (the capture's `stages` records carry it) instead of the
        // live tip. `None` falls back to the fetched head.
        let fixture_head = cfg.fixture_head;
        if fixture_head.is_some() {
            tracing::info!(fixture_head = ?fixture_head, "fixture head pinned");
        }

        // The strategy runtime OWNS the frame-surviving caches (index, token
        // joins, warm-code cache); each frame gets a fresh planning Workspace
        // scope (see frame_pipeline's module doc for the split).
        let runtime = MarketContext::new(1, route_registry, connector_db, cfg.connectors);
        let strategy = BackrunStrategy::new();

        let exec: Address = cfg
            .executor
            .parse()
            .expect("strategy.backrun.executor is a valid address");
        // The sim oracle's caller identity: the executor is OWNER-gated
        // (`execute()` asserts msg.sender == OWNER_ADDR), so the simulated
        // call must come from the OPERATOR address -- never the target tx's
        // original sender. The bid tx itself is signed by the operator key,
        // so sim-from == tx-from.
        let owner: Address = cfg
            .operator
            .clone()
            .or_else(|| std::env::var("EXECUTOR_OWNER_ADDRESS").ok())
            .unwrap_or_else(|| String::from("0x5c603b8a137A40426E0dDFA981EC10c245AF080e"))
            .parse()
            .expect("executor owner address parses");
        let wallet_gas_cost_wei = Arc::new(std::sync::atomic::AtomicU64::new(
            u64::try_from(initial_wallet_gas_cost(&provider, &cfg).await).unwrap_or(u64::MAX),
        ));
        let pl = PipelineConfig {
            exec,
            owner,
            bribe_bips: cfg.bribe_bips,
            wallet_gas_cost_wei,
            gas_floor_wei: U256::from(GAS_FLOOR_WEI),
            // Historical mode only when the dry-run actually pinned a head: the
            // live sim gate evaluates at `latest` and would diverge otherwise.
            fixture_mode: fixture_frames.is_some() && fixture_head.is_some(),
        };

        let fetched_head = provider.get_block_number().await.expect("head block fetch");
        // Fixture mode pins the dispatcher (and, below, the replay handle) to the
        // capture head so the scratch's reads answer the historical chain view.
        let replay_head = if fixture_frames.is_some() {
            fixture_head.unwrap_or(fetched_head)
        } else {
            fetched_head
        };
        let dispatcher = Arc::new(Mutex::new(Dispatcher::for_block(replay_head)));
        // Seed the authority from the chain's next nonce for the operator
        // account: the driver's first stamp must never re-issue a nonce the
        // chain has already consumed.
        let operator_nonce = provider
            .get_transaction_count(
                &signer.as_ref().map(TxSigner::address).unwrap_or_default(),
                None,
            )
            .await
            .unwrap_or_default();
        nonce_lane.observe_chain_nonce(operator_nonce);

        tracing::info!(
            bid_mode = cfg.bid_mode_legal(),
            budget = %cfg.budget_wei,
            stop_file = %cfg.stop_file.display(),
            "backrun driver starting"
        );

        let boot = LoopBoot {
            provider,
            runtime,
            strategy,
            pl,
            dispatcher,
            nonce_lane,
            signer,
            gap_probe,
            sim_client,
            fixture_frames,
            head_ws_url,
            namespace_root,
        };
        let shared = Arc::new(LoopShared::new());
        let run = Box::pin(drive(cfg, hub, boot, Arc::clone(&shared)));
        DriverHandle {
            shared,
            run: Some(run),
        }
    }
}

/// The driver loop. Everything here is loop-local: the replay handle borrows
/// only the loop's own runtime, never a host handle.
#[expect(clippy::too_many_lines, reason = "the driver loop reads top-to-bottom")]
async fn drive(cfg: BackrunConfig, hub: Arc<Hub>, boot: LoopBoot, shared: Arc<LoopShared>) {
    let LoopBoot {
        provider,
        mut runtime,
        mut strategy,
        pl,
        dispatcher,
        nonce_lane,
        signer,
        gap_probe,
        sim_client,
        fixture_frames,
        head_ws_url,
        namespace_root,
    } = boot;
    shared.begin_running();
    // The per-block replay handle: rebuilt whenever the observed head
    // advances (the scratch stack pins `BlockId::Number(head)` and frames
    // run in the NEXT block's env). The sim DB's membership view is the boot
    // registry (or a state-less no-op when the discovery lane is shut); the
    // driver carries no engine state, so the divergence observer is inert.
    // The shared warm cache carries the cross-block bytecode/account caches
    // across rebuilds.
    let oracle_holder = runtime.registry.clone();
    let oracle: &dyn degenbot_bot::bot_core::SimAnchorOracle = match oracle_holder.as_deref() {
        Some(registry) => registry,
        None => &degenbot_bot::bot_core::NO_SIM_ANCHOR,
    };
    let mut current_block = dispatcher
        .lock()
        .expect("dispatcher mutex poisoned")
        .current_block();
    let mut handle: Option<BlockSimHandle<'_>> =
        build_block_handle(&provider, current_block, &runtime.warm_cache, oracle).await;
    if handle.is_none() {
        tracing::warn!(
            "replay handle build failed - frames observe replay_unavailable until it recovers"
        );
    }

    let mut spent = U256::ZERO;

    let mut quarantine = Quarantine::new();

    let mut gap_park_memo: GapParkMemo = None;

    if let Some(frames) = fixture_frames {
        // Dry-run over the capture: every frame processed once, in order.
        for ev in &frames {
            if cfg.stop_file.exists() || shared.stop_requested() {
                tracing::info!("kill switch present - halting dry-run");
                break;
            }
            // No age gate exists: a captured frame (by definition old) runs
            // the full funnel exactly like a fresh one.
            run_frame(
                ev,
                FEED_PREFIX,
                &mut runtime,
                &mut strategy,
                &provider,
                &sim_client,
                &cfg,
                &pl,
                &mut handle,
                current_block,
                &mut spent,
                &dispatcher,
                &nonce_lane,
                signer.as_ref(),
                &gap_probe,
                &mut quarantine,
                None,
                &mut gap_park_memo,
            )
            .await;
        }
        tracing::info!("dry-run fixture complete - halting");
        shared.mark_stopped();
        return;
    }

    // Reload the durable quarantine before servicing any frame, so a restart
    // resumes the parked set.
    let mut quarantine_journal = reload_quarantine(&mut quarantine, namespace_root.as_deref());

    // Live mode: MEVBlocker feed. The hub owns the process-lifetime event
    // channels; the feed registers its PendingTx drop-oldest ring on it and
    // drains through the hub's typed receiver.
    let event_hub = Arc::clone(&hub);
    let feed = BackrunFeed::spawn_on_hub(
        &event_hub,
        BackrunFeedConfig {
            url: if cfg.stream_url.is_empty() {
                BackrunFeedConfig::for_mainnet().url
            } else {
                cfg.stream_url.clone()
            },
            ..BackrunFeedConfig::for_mainnet()
        },
    )
    .expect("fresh hub registers the pending-tx feed");

    // Head source for the live loop: a `newHeads` subscription over a dedicated
    // WS endpoint (the MEVBlocker frame feed and the chain node are different
    // hosts, so the head WS is its own URL). The hub owns the latest head and
    // its staleness; `HeadWatch` only performs the subscribe + reconnect and
    // publishes into the hub. The 200ms `eth_blockNumber` poll is the FALLBACK,
    // not the primary source: it costs a round-trip per tick and cannot fire the
    // instant a head lands. Without a WS URL, or when the subscribe fails, no
    // head source is registered and the loop polls.
    let mut head_source: Option<HeadSubscription> = if let Some(url) = head_ws_url {
        match AlloyProvider::new(&url, DEFAULT_MAX_RETRIES).await {
            Ok(ws_provider) => {
                match HeadWatch::subscribe(
                    &event_hub,
                    ws_provider.provider_arc(),
                    HeadWatchConfig::default(),
                )
                .await
                {
                    Ok(_transport) => match event_hub.subscribe_head() {
                        Ok(head) => Some(head),
                        Err(e) => {
                            tracing::error!(
                                error = %e,
                                "head source subscribe failed - falling back to 200ms head poll"
                            );
                            None
                        }
                    },
                    Err(e) => {
                        tracing::error!(
                            error = %e,
                            "head watch subscribe failed - falling back to 200ms head poll"
                        );
                        None
                    }
                }
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "head watch WS connect failed - falling back to 200ms head poll"
                );
                None
            }
        }
    } else {
        tracing::warn!("no DEGENBOT_RPC_WS_CHAINID_1 set - using 200ms head poll");
        None
    };

    loop {
        if cfg.stop_file.exists() || shared.stop_requested() {
            tracing::info!("kill switch present - halting");
            feed.stop();
            break;
        }
        // The head source resolves on a header's arrival; the 2s bound keeps
        // the frame feed serviced while the head is quiet. On timeout a stale
        // source falls back to the poll for this iteration; a receiver with no
        // sender left is treated the same way (and paced) rather than
        // busy-spinning on a closed channel.
        let next_head: Option<u64> = if let Some(head) = head_source.as_mut() {
            match tokio::time::timeout(HEAD_WATCH_WAIT, head.changed()).await {
                Ok(Ok(())) => head.borrow_and_update(),
                Ok(Err(_)) => {
                    tracing::warn!("head source closed - polling");
                    tokio::time::sleep(HEAD_POLL_TICK).await;
                    provider.get_block_number().await.ok()
                }
                Err(_) => {
                    if head.stale(HEAD_WATCH_STALE) {
                        tracing::warn!("head source stale - polling");
                        tokio::time::sleep(HEAD_POLL_TICK).await;
                        provider.get_block_number().await.ok()
                    } else {
                        None
                    }
                }
            }
        } else {
            tokio::time::sleep(HEAD_POLL_TICK).await;
            provider.get_block_number().await.ok()
        };

        if let Some(head) = next_head {
            if head > current_block {
                dispatcher
                    .lock()
                    .expect("dispatcher mutex poisoned")
                    .advance_block(head);
                current_block = head;
                // Refresh the shared nonce authority from the chain's
                // operator-account nonce: a confirmed broadcast leaves the
                // outstanding set, and a rewind restores a broadcast the old
                // head had confirmed. Guarded on outstanding work so an idle
                // driver pays no per-head chain read.
                if nonce_lane.authority().has_outstanding() {
                    if let Some(operator) = signer.as_ref().map(TxSigner::address) {
                        match provider.get_transaction_count(&operator, None).await {
                            Ok(confirmed) => {
                                let _ = nonce_lane.authority().set_confirmed_reorg(confirmed);
                                let outstanding = nonce_lane.authority().outstanding_nonces();
                                let _ = nonce_lane.ledger().reconcile(confirmed, &outstanding);
                            }
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    "head nonce read failed - authority reconcile deferred"
                                );
                            }
                        }
                    }
                }
                pl.wallet_gas_cost_wei.store(
                    u64::try_from(wallet_gas_cost_at(&provider, head, &cfg).await)
                        .unwrap_or(u64::MAX),
                    std::sync::atomic::Ordering::Relaxed,
                );
                match build_block_handle(&provider, head, &runtime.warm_cache, oracle).await {
                    Some(h) => handle = Some(h),
                    None => {
                        tracing::warn!(
                            "replay handle rebuild failed - frames observe replay_unavailable"
                        );
                    }
                }

                // ── frame-liveness FSM tick ─────────────────────────────
                // 1. Classify every tracked frame whose sender nonce reached
                //    the claim: ONE receipt probe on the frame's own hash
                //    decides MinedAt vs SlotTakenAt. No clock is consulted; a
                //    classification failure leaves the frame tracked for the
                //    next head.
                for sender in quarantine.senders() {
                    // Evidence-only nonce lane: u64::MAX as a FAILURE default
                    // would fabricate consumption for every tracked frame of
                    // the sender (the D1 adversarial-review find) - a failed
                    // read skips the tick, holding every frame instead.
                    let Some(head_nonce) = nonce_lane_evidence(
                        sim_client
                            .request::<(Address, &str), U256>(
                                std::borrow::Cow::from("eth_getTransactionCount"),
                                (sender, "latest"),
                            )
                            .await,
                    ) else {
                        tracing::warn!(sender = ?sender, "head-nonce read failed - sender tick skipped");
                        continue;
                    };
                    // ONE pending-lane read; a failure is NO evidence and
                    // degrades this tick to the latest-lane behavior rather
                    // than skipping the sender's frames entirely.
                    let pending_count = nonce_lane_evidence(
                        sim_client
                            .request::<(Address, &str), U256>(
                                std::borrow::Cow::from("eth_getTransactionCount"),
                                (sender, "pending"),
                            )
                            .await,
                    );
                    let pool_known_gap =
                        pool_known_gap_for_tick(&quarantine, sender, head_nonce, pending_count);
                    // With a guard recorded for this sender, fetch the
                    // pool-known predecessors' content identity so a same-nonce
                    // replacement re-arms instead of suppressing. A failed
                    // prefetch is no content evidence: the guard falls back to
                    // the nonce + boundary comparison.
                    let predecessor_content =
                        if !pool_known_gap.is_empty() && quarantine.has_guard_for_sender(sender) {
                            hydrate_predecessors(&sim_client, sender, &pool_known_gap)
                                .await
                                .map_or_else(|_| Vec::new(), |prefix| prefix.content())
                        } else {
                            Vec::new()
                        };
                    for (frame, decision) in quarantine.poll_with_content(
                        sender,
                        head_nonce,
                        &pool_known_gap,
                        &predecessor_content,
                    ) {
                        match decision {
                            QuarantineDecision::NonceConsumed => {
                                let Some(consumed) =
                                    classify_consumption(&provider, &sim_client, &frame).await
                                else {
                                    tracing::warn!(
                                        tx = %frame.hash,
                                        "nonce-consumption classification failed - frame stays tracked"
                                    );
                                    continue;
                                };
                                if !quarantine.enter_tentative(frame.hash, consumed) {
                                    continue;
                                }
                                if let Some(journal) = quarantine_journal.as_mut() {
                                    if let Err(error) = journal.record_tentative(
                                        frame.hash,
                                        consumed,
                                        now_unix_ms(),
                                    ) {
                                        tracing::warn!(%error, "tentative record not journaled");
                                    }
                                }
                                trace_jsonl(
                                    "nonce_consumed",
                                    serde_json::json!({
                                        "tx": frame.hash.to_string(),
                                        "sender": format!("0x{:x}", frame.from),
                                        "mined": consumed.mined(),
                                        "block": consumed.block(),
                                        "block_hash": consumed.block_hash().to_string(),
                                    }),
                                );
                                if consumed.mined() {
                                    tracing::info!(
                                        tx = %frame.hash,
                                        reason = "already_settled",
                                        block = consumed.block(),
                                        "observe"
                                    );
                                }
                            }
                            QuarantineDecision::Rescue { predecessors } => {
                                trace_jsonl(
                                    "quarantine",
                                    serde_json::json!({
                                        "action": "rescue_reentered",
                                        "tx": frame.hash.to_string(),
                                        "predecessors": predecessors.len(),
                                        "predecessor_nonces": predecessors.clone(),
                                    }),
                                );
                                // A non-empty prefix is the pool-pred case: the
                                // gap's txs must all hydrate before the frame
                                // can advance. A transport error or a null
                                // lookup is transient; a returned shape that
                                // cannot decode is permanent for this lane.
                                let mut prefix_txs: Vec<ReplayableTx> = Vec::new();
                                let mut prefix_hashes: Vec<B256> = Vec::new();
                                if !predecessors.is_empty() {
                                    match hydrate_predecessors(
                                        &sim_client,
                                        frame.from,
                                        &predecessors,
                                    )
                                    .await
                                    {
                                        Ok(hydrated) => {
                                            trace_jsonl(
                                                "quarantine",
                                                serde_json::json!({
                                                    "action": "rescue_hydrated",
                                                    "tx": frame.hash.to_string(),
                                                    "predecessors": hydrated.txs.len(),
                                                }),
                                            );
                                            prefix_txs = hydrated.txs;
                                            prefix_hashes = hydrated.hashes;
                                        }
                                        Err(HydrationFailure::Transient(reason)) => {
                                            trace_jsonl(
                                                "quarantine",
                                                serde_json::json!({
                                                    "action": "rescue_hydration_failed",
                                                    "tx": frame.hash.to_string(),
                                                    "predecessor_nonces": predecessors.clone(),
                                                    "reason": reason,
                                                    "permanent": false,
                                                }),
                                            );
                                            tracing::warn!(
                                                tx = %frame.hash,
                                                reason = reason,
                                                "pool-pred predecessor hydration failed - transient"
                                            );
                                            route_failed_hydration(
                                                &frame,
                                                &mut quarantine,
                                                quarantine_journal.as_mut(),
                                            );
                                            continue;
                                        }
                                        Err(HydrationFailure::Structural { reason, hashes }) => {
                                            route_unhydratable_hydration(
                                                &frame,
                                                &predecessors,
                                                &hashes,
                                                reason,
                                                &mut quarantine,
                                                quarantine_journal.as_mut(),
                                            );
                                            continue;
                                        }
                                    }
                                }
                                // Re-enter the funnel exactly as the feed loop
                                // does, with the same context. `run_frame`
                                // owns its own spent/consumption accounting, so
                                // nothing is double-counted here.
                                let ev = frame.to_event();
                                let outcome = run_frame(
                                    &ev,
                                    &prefix_txs,
                                    &mut runtime,
                                    &mut strategy,
                                    &provider,
                                    &sim_client,
                                    &cfg,
                                    &pl,
                                    &mut handle,
                                    current_block,
                                    &mut spent,
                                    &dispatcher,
                                    &nonce_lane,
                                    signer.as_ref(),
                                    &gap_probe,
                                    &mut quarantine,
                                    quarantine_journal.as_mut(),
                                    &mut gap_park_memo,
                                )
                                .await;
                                // The caller observed the re-park, so it can key
                                // the suppression guard on the boundary the
                                // re-park actually carried.
                                if outcome == FrameOutcome::GapPending {
                                    if let Some(boundary) = quarantine.expected_boundary(frame.hash)
                                    {
                                        quarantine.record_repark_guard_with_content(
                                            frame.hash,
                                            &predecessors,
                                            &prefix_hashes,
                                            boundary,
                                        );
                                    }
                                }
                                if outcome == FrameOutcome::PredecessorMalformed {
                                    // A predecessor that cannot run as fetched
                                    // blocks the pool-pred lane, not the frame:
                                    // record the structural guard WITH content so
                                    // a replaced predecessor re-arms, and keep
                                    // the frame parked (Transient). The frame's
                                    // quarantine life is never resolved.
                                    route_unhydratable_hydration(
                                        &frame,
                                        &predecessors,
                                        &prefix_hashes,
                                        "malformed_predecessor",
                                        &mut quarantine,
                                        quarantine_journal.as_mut(),
                                    );
                                } else {
                                    journal_reentry_outcome(
                                        &frame,
                                        reentry_outcome(outcome),
                                        &mut quarantine,
                                        quarantine_journal.as_mut(),
                                    );
                                }
                            }
                            QuarantineDecision::StillWaiting { unknown } => {
                                trace_jsonl(
                                    "quarantine",
                                    serde_json::json!({"action": "still_waiting",
                                        "tx": frame.hash.to_string(),
                                        "unknown_nonces": unknown,
                                    }),
                                );
                            }
                        }
                    }
                }

                // 2. Reorg check: one canonical-hash read per DISTINCT
                //    tentative block; a mismatch (or a vanished block)
                //    revives its frames back to Tracked.
                for (block, recorded_hash) in quarantine.tentative_blocks() {
                    let canonical = canonical_block_hash(&provider, block).await;
                    for frame in quarantine.check_reorg(block, canonical) {
                        trace_jsonl(
                            "reorg_revived",
                            serde_json::json!({
                                "tx": frame.hash.to_string(),
                                "block": block,
                                "recorded_hash": recorded_hash.to_string(),
                                "canonical_hash": canonical.map(|h| h.to_string()),
                            }),
                        );
                        tracing::info!(
                            tx = %frame.hash,
                            block,
                            "reorg revived - frame back to tracked"
                        );
                    }
                }

                // 3. Finality: ONE `finalized` tag read per head advance; a
                //    tentative block at or below the tag is dead for good.
                if let Some(finalized) = finalized_block_number(&provider).await {
                    for (frame, consumed) in quarantine.check_finalized(finalized) {
                        let (resolution, reason) = if consumed.mined() {
                            (Resolution::MinedFinalized, "mined_finalized")
                        } else {
                            (Resolution::SlotTakenFinalized, "slot_taken_finalized")
                        };
                        if let Some(journal) = quarantine_journal.as_mut() {
                            let resolved_unix_ms = now_unix_ms();
                            if let Err(error) =
                                journal.record_resolve(frame.hash, resolution, resolved_unix_ms)
                            {
                                tracing::warn!(%error, "finalized tombstone not journaled");
                            }
                            let archived = ResolutionArchiveRecord::new(
                                &frame,
                                ArchivedResolution::from_journal(resolution),
                                resolved_unix_ms,
                            )
                            .with_consumption(consumed)
                            .with_finalized_block(finalized);
                            if let Err(error) = journal.record_resolution(&archived) {
                                tracing::warn!(%error, "finalized resolution not archived");
                            }
                        }
                        trace_jsonl(
                            "finalized",
                            serde_json::json!({
                                "tx": frame.hash.to_string(),
                                "reason": reason,
                                "mined": consumed.mined(),
                                "block": consumed.block(),
                                "finalized": finalized,
                            }),
                        );
                        tracing::info!(
                            tx = %frame.hash,
                            mined = consumed.mined(),
                            block = consumed.block(),
                            finalized,
                            "frame finalized"
                        );
                    }
                }
            }
        }

        for ev in feed.drain() {
            if cfg.stop_file.exists() || shared.stop_requested() {
                tracing::info!("kill switch present - halting");
                break;
            }
            run_frame(
                &ev,
                FEED_PREFIX,
                &mut runtime,
                &mut strategy,
                &provider,
                &sim_client,
                &cfg,
                &pl,
                &mut handle,
                current_block,
                &mut spent,
                &dispatcher,
                &nonce_lane,
                signer.as_ref(),
                &gap_probe,
                &mut quarantine,
                quarantine_journal.as_mut(),
                &mut gap_park_memo,
            )
            .await;
        }
    }
    shared.mark_stopped();
}
