//! The standalone backrun sidecar binary (epic 6ZOGIT, task OQQCQO; frame
//! pipeline rewired by WKPZQK).
//!
//! Loop: `MEVBlocker` feed → `frame_pipeline::process_frame` (frame-replay
//! seam → journal-pool extraction → workspace admission → discovery fan →
//! solve → compose → `eth_callMany` gate → [`degenbot_bot::sidecar::decide`])
//! → bid through the submission leaf ([`dispatch_and_submit`], bid mode
//! exclusively via the private-RPC extra broadcast). Observe-only default;
//! all state is local; zero touches to the live engine block pump (FORK-1).
//!
//! Frames are shaped by the replay (touched set + journalled words), never
//! by calldata decoding.
//!
//! Run (observe-only): set `DEGENBOT_RPC_HTTP_CHAINID_1=$RPC` and boot the
//! binary; every knob comes from the typed `strategy.backrun` facet (TOML
//! `[strategy.backrun]` or `DEGENBOT_STRATEGY_BACKRUN_*`). Bid mode adds
//! `bid_mode = true`, a non-zero `budget_wei`, and `key_file = <hex path>`.
//! Bids are MEVBlocker-specific per
//! docs.mevblocker.io/how-to/searchers/bid: `eth_sendBundle` on the
//! searcher WS with `txs = [targetHash, signed backrun]`, block-pinned.
//! The signed backrun NEVER touches the public mempool or another relay.
//! Dry-run (offline-review over a captured frame JSONL):
//! `logging.dry_run_jsonl = /tmp/mb_trace.jsonl` replaces the live feed with
//! the capture's frames, processed once, in order.
//! Kill switch: the `strategy.backrun.stop_file` facet path (default
//! `/tmp/degenbot-sidecar-STOP`).

#![expect(
    clippy::expect_used,
    reason = "bin: fatal config failures exit the process loudly"
)]
#![expect(
    clippy::print_stderr,
    reason = "bin: boot/config failures must reach the operator before tracing is installed"
)]

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::primitives::{Address, B256, U256};
use degenbot_bot::bot_core::SimAnchorState;
use degenbot_bot::sidecar::{gate_mined_target, Decision, SidecarConfig};
use degenbot_rpc::backrun_feed::{BackrunFeed, BackrunFeedConfig};
use degenbot_rpc::head_watch::{HeadWatch, HeadWatchConfig};
use degenbot_rpc::provider::{AlloyProvider, DEFAULT_MAX_RETRIES};
use degenbot_simulation::BlockSimHandle;
use degenbot_submission::backrun_strategy::BackrunStrategy;
use degenbot_submission::bundle::MEVBLOCKER_STREAM_URL;
use degenbot_submission::dispatcher::Dispatcher;
use degenbot_submission::frame_pipeline::{
    build_block_handle, load_fixture_frames, process_frame, trace_jsonl, MarketContext,
    PipelineConfig,
};
use degenbot_submission::gap_quarantine::{
    NonceConsumed, ParkedFrame, Quarantine, QuarantineDecision,
};
use degenbot_submission::gap_quarantine_journal::{
    self, ParkRecord, QuarantineJournal, Resolution,
};
use degenbot_submission::monitor::ReceiptProbe;
use degenbot_submission::signer::TxSigner;
use degenbot_submission::submit::{
    dispatch_and_submit, BundleTarget, SubmissionTarget, SubmitCandidate,
};

/// The gas floor the envelope gate evaluates at (wei) — the composed lane's
/// standing economics (the env override did not exist upstream either).
const GAS_FLOOR_WEI: u64 = 50_000_000_000_000;

/// The chain the sidecar operates on (the connector index and the per-chain
/// `DEGENBOT_RPC_HTTP_CHAINID_<id>` / `DEGENBOT_RPC_WS_CHAINID_<id>`
/// resolver suffixes both read it).
const CHAIN_ID: u64 = 1;

/// How long the live loop waits on the head watch before servicing the frame
/// feed. The watch resolves the instant a header arrives (~12s apart), so a
/// healthy watch leaves this to time out most iterations; the bound is loop
/// latency, not head latency.
const HEAD_WATCH_WAIT: Duration = Duration::from_secs(2);

/// A watch silent this long is treated as dead: the loop polls for that
/// iteration while the driver task's watchdog reconnects in the background.
/// Must exceed the chain's block interval — mainnet blocks arrive ~12s apart,
/// so a threshold at or below that would poll on every block and defeat the
/// point of the subscription. Kept below the driver's 48s watchdog so the
/// fallback poll covers the reconnect window.
const HEAD_WATCH_STALE: Duration = Duration::from_secs(30);

/// The fallback head-poll cadence (the pre-subscription loop's tick).
const HEAD_POLL_TICK: Duration = Duration::from_millis(200);

// Session telemetry: the fmt subscriber appends to the session's
// `stdout.log` (file-only by default) so a run's console output survives the
// process, while `logging.log_stderr` duplicates it to stderr for
// interactive runs. The session's `trace.jsonl` becomes the trace helpers'
// default capture path; an explicit `logging.trace_jsonl` still wins. A run
// directory that cannot be created degrades to stderr — capturing logs must
// never abort the bot. Structured (OTel) export stays the operator's
// layering choice via the bot crate.
fn init_tracing(mirror_stderr: bool) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    match degenbot_runs::RunDirectory::create("backrun-sidecar") {
        Ok(run) => {
            let _ = degenbot_runs::set_trace_jsonl_default(run.trace_jsonl_path().to_path_buf());
            let writer = if mirror_stderr {
                run.stdout_writer_tee()
            } else {
                run.stdout_writer()
            };
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(mirror_stderr)
                .with_writer(writer)
                .try_init();
            tracing::info!(
                session_dir = %run.session_dir().display(),
                stdout = %run.stdout_path().display(),
                trace_jsonl = %run.trace_jsonl_path().display(),
                "sidecar run artifacts"
            );
        }
        Err(error) => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .try_init();
            tracing::warn!(error = %error, "run directory unavailable - logging to stderr");
        }
    }
}

/// Provider-backed receipt probe (the sidecar's own node join; the `PyReceiptProbe`
/// twin is the python driver's -- this keeps the sidecar standalone).
struct SidecarProbe {
    provider: Arc<AlloyProvider>,
}

impl ReceiptProbe for SidecarProbe {
    fn receipt_found(
        &self,
        tx_hash: alloy::primitives::B256,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = degenbot_submission::SubmissionResult<bool>>
                + Send
                + '_,
        >,
    > {
        let provider = Arc::clone(&self.provider);
        Box::pin(async move {
            let rec = provider
                .get_transaction_receipt(&tx_hash.to_string())
                .await
                .map_err(|e| degenbot_submission::SubmissionError::MonitorProbe(format!("{e}")))?;
            Ok(rec.is_some())
        })
    }
}

/// The operator's priority fee converted from the facet's gwei to wei.
fn priority_fee_wei(cfg: &SidecarConfig) -> u128 {
    u128::from(cfg.priority_fee_gwei).saturating_mul(1_000_000_000u128)
}

/// The wallet's gas burn for one composed bundle at `head`:
/// estimate x (next base fee x 1.2 + priority). Read on head advances so
/// the compose gate always prices at a fresh base fee.
async fn wallet_gas_cost_at(provider: &AlloyProvider, head: u64, cfg: &SidecarConfig) -> u128 {
    let base_fee_next = provider
        .get_block(head)
        .await
        .ok()
        .flatten()
        .and_then(|b| b.header.base_fee_per_gas)
        .map_or(30_000_000_000u128, |x| u128::from(x) * 12 / 10);
    u128::from(cfg.bundle_gas_est)
        .saturating_mul(base_fee_next.saturating_add(priority_fee_wei(cfg)))
}

async fn initial_wallet_gas_cost(provider: &AlloyProvider, cfg: &SidecarConfig) -> u128 {
    let head = provider.get_block_number().await.unwrap_or(0);
    wallet_gas_cost_at(provider, head, cfg).await
}

/// The raw-broadcast relay list for the private-broadcast arm, private-first.
///
/// Empty when `strategy.backrun.mevblocker_url` is unset: the submit leaf then
/// broadcasts to the read provider alone. When set, the configured private
/// endpoint leads and the read provider follows, so the private path is tried
/// first and the public provider is the fallback relay. An endpoint that cannot
/// be constructed degrades to the read-provider-only list.
async fn build_broadcast_relays(
    cfg: &SidecarConfig,
    provider: &Arc<AlloyProvider>,
) -> Vec<Arc<AlloyProvider>> {
    let Some(url) = cfg.mevblocker_url.as_deref() else {
        return Vec::new();
    };
    match AlloyProvider::new(url, DEFAULT_MAX_RETRIES).await {
        Ok(private) => vec![Arc::new(private), Arc::clone(provider)],
        Err(error) => {
            tracing::warn!(
                %error,
                url,
                "strategy.backrun.mevblocker_url provider build failed - read provider only"
            );
            Vec::new()
        }
    }
}

/// The bid's submission target: this frame's target hash pinned to the next
/// block, `MEVBlocker` searcher WS only.
///
/// Deliberately independent of `strategy.backrun.mevblocker_url`: that key
/// augments the raw broadcast fan-out (see [`build_broadcast_relays`]) and
/// never replaces the bundle target. The bundle (auction) arm keeps its own
/// economics; the private endpoint engages only when the target fans out
/// under [`SubmissionTarget::Public`].
fn bid_submission_target(
    cfg: &SidecarConfig,
    target_tx_hash: B256,
    block_number: u64,
) -> SubmissionTarget {
    SubmissionTarget::Bundle(BundleTarget {
        stream_url: if cfg.stream_url.is_empty() {
            String::from(MEVBLOCKER_STREAM_URL)
        } else {
            cfg.stream_url.clone()
        },
        target_tx_hash,
        block_number,
    })
}

/// The terminal class of one funnel pass, as the rescue router consumes it.
///
/// A rescued frame must survive the boot fold after a transient replay
/// failure, so the two replay-seam failures are their own arms: they leave
/// the frame parked for the next frontier pass. Everything else is terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameOutcome {
    /// A bid was decided; terminal whether or not dispatch landed.
    Bid,
    /// The pass re-parked the frame on a fresh nonce gap.
    GapPending,
    /// No replay handle served this head (transient).
    ReplayUnavailable,
    /// The replay died in an RPC hydrate (transient).
    ReplayFailed,
    /// Any other terminal observe/drop, carrying its trace reason.
    Terminal(&'static str),
}

/// Collapse a detailed pass outcome to the rescue router's three classes.
fn reentry_outcome(outcome: FrameOutcome) -> ReentryOutcome {
    match outcome {
        FrameOutcome::Bid | FrameOutcome::Terminal(_) => ReentryOutcome::Terminal,
        FrameOutcome::GapPending => ReentryOutcome::GapPending,
        FrameOutcome::ReplayUnavailable | FrameOutcome::ReplayFailed => ReentryOutcome::Transient,
    }
}

/// Map an `Observe` reason to its outcome class; the replay-seam and
/// gap-pending reasons are named by [`degenbot_submission::frame_pipeline`].
fn outcome_for(reason: &'static str) -> FrameOutcome {
    match reason {
        "gap_pending" => FrameOutcome::GapPending,
        "replay_unavailable" => FrameOutcome::ReplayUnavailable,
        "replay_failed" => FrameOutcome::ReplayFailed,
        other => FrameOutcome::Terminal(other),
    }
}

/// How a rescued frame's re-entry ended, for journal routing.
///
/// A resolve is written only for a terminal pass; a transient replay failure
/// keeps the original park record alive and re-parks the frame so the next
/// frontier pass retries, and a fresh gap-pending park is its own truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReentryOutcome {
    /// The frame's quarantine life is over (a bid, or a terminal observe/drop).
    Terminal,
    /// The pass re-parked the frame on a genuinely new gap.
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
fn journal_reentry_outcome(
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
                if let Err(error) =
                    journal.record_resolve(frame.hash, Resolution::RescueConsumed, now_unix_ms())
                {
                    tracing::warn!(%error, "quarantine resolve not journaled");
                }
            }
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the frame handler takes the runtime surfaces it needs"
)]
#[expect(
    clippy::too_many_lines,
    reason = "capture -> pipeline -> dispatch reads top-to-bottom"
)]
async fn run_frame(
    ev: &degenbot_rpc::backrun_feed::BackrunFeedEvent,
    rt: &mut MarketContext,
    strategy: &mut BackrunStrategy,
    provider: &Arc<AlloyProvider>,
    sim_client: &alloy::rpc::client::RpcClient,
    cfg: &SidecarConfig,
    pl: &PipelineConfig,
    handle: &mut Option<BlockSimHandle<'static>>,
    head: u64,
    spent: &mut U256,
    dispatcher: &Arc<Mutex<Dispatcher>>,
    operator_nonce: u64,
    signer: Option<&TxSigner>,
    gap_probe: &degenbot_submission::gap_probe::GapProbe,
    quarantine: &mut Quarantine,
    journal: Option<&mut QuarantineJournal>,
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
    let artifacts = process_frame(
        strategy, rt, provider, sim_client, cfg, pl, handle, ev, head, *spent,
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
        if let Some(journal) = journal {
            let record = ParkRecord::new(ev, *expected, now_unix_ms());
            if let Err(error) = journal.record_park(&record) {
                tracing::warn!(tx = %ev.hash, %error, "quarantine park not journaled");
            }
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
        SidecarProbe {
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
                Arc::new(SidecarProbe {
                    provider: Arc::clone(provider),
                }),
                operator_nonce,
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
fn nonce_lane_evidence<E>(read: Result<U256, E>) -> Option<u64> {
    u64::try_from(read.ok()?).ok()
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
fn reload_quarantine(quarantine: &mut Quarantine) -> Option<QuarantineJournal> {
    let path = match degenbot_runs::resolve_state_root() {
        Ok(root) => root.join(gap_quarantine_journal::JOURNAL_FILE_NAME),
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

/// The strategy-arm boot gate: this binary is the pending-transaction
/// backrun arm, so `strategy.name` must be `backrun` or unset. An explicit
/// `settlement` selection names the wrong invocation and is refused.
fn strategy_arm_refusal(arm: Option<degenbot_config::StrategyName>) -> Option<String> {
    match arm {
        None | Some(degenbot_config::StrategyName::Backrun) => None,
        Some(degenbot_config::StrategyName::Settlement) => Some(String::from(
            "strategy.name=settlement: this binary is the pending-transaction backrun sidecar; \
             boot `backrun_sidecar` itself with a typed config carrying `[strategy] name = \
             \"backrun\"` (or DEGENBOT_STRATEGY_NAME=backrun), or run the settled-block arm instead",
        )),
    }
}

#[tokio::main]
#[expect(
    clippy::too_many_lines,
    reason = "bin orchestration loop reads top-to-bottom"
)]
async fn main() {
    let loaded = degenbot_config::BotConfigLoader::new()
        .with_standard_file_paths()
        .load()
        .unwrap_or_else(|error| {
            eprintln!("backrun sidecar config load failed: {error}");
            std::process::exit(2);
        });
    if let Some(refusal) = strategy_arm_refusal(loaded.config.strategy.name) {
        eprintln!("{refusal}");
        std::process::exit(2);
    }
    let config = std::sync::Arc::new(loaded.config.clone());
    let _ = degenbot_config::holder::install(std::sync::Arc::clone(&config));
    init_tracing(config.logging.log_stderr);

    let env = degenbot_config::ProcessEnv;
    let rpc_url = degenbot_config::resolve_node_http_uri(&env, CHAIN_ID, None)
        .unwrap_or_else(|error| {
            eprintln!("{error}");
            std::process::exit(2);
        })
        .value;
    let cfg = SidecarConfig::from_config(&config, rpc_url);

    let client = alloy::rpc::client::ClientBuilder::default().http(
        cfg.rpc_url
            .parse()
            .expect("DEGENBOT_RPC_HTTP_CHAINID_1 is a valid http url"),
    );
    let provider = Arc::new(AlloyProvider::from_provider(Arc::new(
        alloy::providers::ProviderBuilder::default().connect_client(client),
    )));

    // The bundle-sim client (`strategy.backrun.sim_url`, default: the chain
    // node the frames replay against). READ/SIM ONLY -- `eth_callMany` never
    // broadcasts, and this client is passed nothing else. The sim MUST run
    // on an endpoint that actually serves `eth_callMany`; MEVBlocker's
    // /fast http tier answers method-missing for it, so the node is the
    // fallback and /fast stays available as an explicit override.
    let sim_client = alloy::rpc::client::ClientBuilder::default().http(
        config
            .strategy
            .backrun
            .sim_url
            .clone()
            .unwrap_or_else(|| cfg.rpc_url.clone())
            .parse()
            .expect("strategy.backrun.sim_url parses as an http url"),
    );
    // The gap-boundary probe samples the chain node's pending-pool lanes
    // whenever a frame claims a nonce ahead of the parent state.
    let gap_probe = degenbot_submission::gap_probe::GapProbe::new(sim_client.clone());

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
    let fixture_frames = config.logging.dry_run_jsonl.as_ref().map(|p| {
        let frames = load_fixture_frames(p);
        tracing::info!(frames = frames.len(), path = %p.display(), "dry-run fixture loaded");
        frames
    });

    // The offline fixture's pinned head: with the `fixture_head` facet set
    // the dry-run replays captured frames against the chain view they were
    // pending in (the capture's `stages` records carry it) instead of the
    // live tip. `None` falls back to the fetched head.
    let fixture_head = config.strategy.backrun.fixture_head;
    if fixture_head.is_some() {
        tracing::info!(fixture_head = ?fixture_head, "fixture head pinned");
    }

    // DFYDYI B3: the DB-backed connector index -- ONE startup scan, never a
    // per-frame query. The path comes from the shared `DEGENBOT_DB_PATH`
    // resolver; a missing file leaves the discovery fan shut and frames
    // observe (connectors are never guessed).
    let db_path = degenbot_config::resolve_database_path(&env, None).value;
    let (connector_index, connector_db): (
        Option<degenbot_bot::sidecar_paths::V2ConnectorIndex>,
        Option<degenbot_db::connection::DegenbotDb>,
    ) = if db_path.is_file() {
        match degenbot_db::connection::DegenbotDb::open(&db_path) {
            Ok((db, _)) => match degenbot_bot::sidecar_paths::V2ConnectorIndex::load(&db, 1)
                .and_then(|mut ix| ix.load_v3(&db, 1).map(|()| ix))
            {
                Ok(mut ix) => {
                    ix.set_ranker(Arc::new(
                        degenbot_bot::sidecar_paths::OnChainLiquidityRanker::new(Arc::clone(
                            &provider,
                        )),
                    ));
                    tracing::info!(edges = ix.len(), "connector index loaded");
                    // Evidence mode (`strategy.backrun.rank_evidence`): a LIVE
                    // sanity probe before any frame trusts the depth
                    // truncation -- the canonical deep USDC/WETH pair must
                    // top the ranking.
                    if config.strategy.backrun.rank_evidence {
                        match degenbot_bot::sidecar_paths::deep_pair_ranking_evidence(&ix, &db)
                            .await
                        {
                            Ok(()) => {
                                tracing::info!(
                                    "rank evidence: deep USDC/WETH pair tops the ranking"
                                );
                            }
                            Err(e) => tracing::warn!(evidence = %e, "rank evidence FAILED"),
                        }
                    }
                    (Some(ix), Some(db))
                }
                Err(e) => {
                    tracing::warn!(error = %e, "connector index load failed - lane disabled");
                    (None, None)
                }
            },
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %db_path.display(),
                    "DEGENBOT_DB_PATH unopenable - lane disabled"
                );
                (None, None)
            }
        }
    } else {
        tracing::debug!(path = %db_path.display(), "connector DB absent - lane disabled");
        (None, None)
    };
    let connector_cap: usize = config.strategy.backrun.connectors;

    // The strategy runtime OWNS the frame-surviving caches (index, token
    // joins, warm-code cache); each frame gets a fresh planning Workspace
    // scope (see frame_pipeline's module doc for the split).
    let mut runtime = MarketContext::new(1, connector_index, connector_db, connector_cap);
    let mut strategy = BackrunStrategy::new();

    let exec: Address = config
        .strategy
        .backrun
        .executor
        .parse()
        .expect("strategy.backrun.executor is a valid address");
    // The sim oracle's caller identity: the executor is OWNER-gated
    // (`execute()` asserts msg.sender == OWNER_ADDR), so the simulated
    // call must come from the OPERATOR address -- never the target tx's
    // original sender. The bid tx itself is signed by the operator key,
    // so sim-from == tx-from.
    let owner: Address = config
        .strategy
        .backrun
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
    let operator_nonce = provider
        .get_transaction_count(
            &signer.as_ref().map(TxSigner::address).unwrap_or_default(),
            None,
        )
        .await
        .unwrap_or_default();

    tracing::info!(
        bid_mode = cfg.bid_mode_legal(),
        budget = %cfg.budget_wei,
        stop_file = %cfg.stop_file.display(),
        "sidecar starting"
    );

    // The per-block replay handle: rebuilt whenever the observed head
    // advances (the scratch stack pins `BlockId::Number(head)` and frames
    // run in the NEXT block's env). The anchor is an EMPTY leaked snapshot
    // — the sidecar tracks no canonical-registry pools; the shared warm
    // cache carries the cross-block bytecode/account caches across rebuilds.
    let anchor: &'static SimAnchorState = Box::leak(Box::new(SimAnchorState::default()));
    let mut current_block = dispatcher
        .lock()
        .expect("dispatcher mutex poisoned")
        .current_block();
    let mut handle: Option<BlockSimHandle<'static>> =
        build_block_handle(&provider, current_block, &runtime.warm_cache, anchor).await;
    if handle.is_none() {
        tracing::warn!(
            "replay handle build failed - frames observe replay_unavailable until it recovers"
        );
    }

    let mut spent = U256::ZERO;

    let mut quarantine = Quarantine::new();

    if let Some(frames) = fixture_frames {
        // Dry-run over the capture: every frame processed once, in order.
        for ev in &frames {
            if cfg.stop_file.exists() {
                tracing::info!("kill switch present - halting dry-run");
                break;
            }
            // No age gate exists: a captured frame (by definition old) runs
            // the full funnel exactly like a fresh one.
            run_frame(
                ev,
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
                operator_nonce,
                signer.as_ref(),
                &gap_probe,
                &mut quarantine,
                None,
            )
            .await;
        }
        tracing::info!("dry-run fixture complete - halting");
        return;
    }

    // Reload the durable quarantine before servicing any frame, so a restart
    // resumes the parked set.
    let mut quarantine_journal = reload_quarantine(&mut quarantine);

    // Live mode: MEVBlocker feed.
    let feed = BackrunFeed::spawn(BackrunFeedConfig {
        url: if cfg.stream_url.is_empty() {
            BackrunFeedConfig::for_mainnet().url
        } else {
            cfg.stream_url.clone()
        },
        ..BackrunFeedConfig::for_mainnet()
    });

    // Head source for the live loop: a `newHeads` subscription over a dedicated
    // WS endpoint (the MEVBlocker frame feed and the chain node are different
    // hosts, so the head WS is its own URL). The 200ms `eth_blockNumber` poll
    // is the FALLBACK, not the primary source: it costs a round-trip per tick
    // and cannot fire the instant a head lands. Without a WS URL, or when the
    // subscribe fails, the watch stays absent and the loop polls.
    let head_ws_url = degenbot_config::resolve_node_ws_uri(&env, CHAIN_ID, None)
        .ok()
        .map(|resolved| resolved.value);
    let head_watch: Option<HeadWatch> = if let Some(url) = head_ws_url {
        match AlloyProvider::new(&url, DEFAULT_MAX_RETRIES).await {
            Ok(ws_provider) => {
                match HeadWatch::subscribe(ws_provider.provider_arc(), HeadWatchConfig::default())
                    .await
                {
                    Ok(watch) => Some(watch),
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
    let mut head_rx = head_watch.as_ref().map(HeadWatch::head_rx);

    loop {
        if cfg.stop_file.exists() {
            tracing::info!("kill switch present - halting");
            feed.stop();
            break;
        }
        // The watch resolves on a header's arrival; the 2s bound keeps the
        // frame feed serviced while the head is quiet. On timeout a stale
        // watch falls back to the poll for this iteration; a receiver with no
        // sender left is treated the same way (and paced) rather than
        // busy-spinning on a closed channel.
        let next_head: Option<u64> =
            if let (Some(watch), Some(rx)) = (head_watch.as_ref(), head_rx.as_mut()) {
                match tokio::time::timeout(HEAD_WATCH_WAIT, rx.changed()).await {
                    Ok(Ok(())) => Some(*rx.borrow_and_update()),
                    Ok(Err(_)) => {
                        tracing::warn!("head watch channel closed - polling");
                        tokio::time::sleep(HEAD_POLL_TICK).await;
                        provider.get_block_number().await.ok()
                    }
                    Err(_) => {
                        if watch.stale(HEAD_WATCH_STALE) {
                            tracing::warn!("head watch stale - polling");
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
                pl.wallet_gas_cost_wei.store(
                    u64::try_from(wallet_gas_cost_at(&provider, head, &cfg).await)
                        .unwrap_or(u64::MAX),
                    std::sync::atomic::Ordering::Relaxed,
                );
                match build_block_handle(&provider, head, &runtime.warm_cache, anchor).await {
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
                    for (frame, decision) in quarantine.poll(sender, head_nonce, &[]) {
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
                                    }),
                                );
                                // Re-enter the funnel exactly as the feed loop
                                // does, with the same context. `run_frame`
                                // owns its own spent/consumption accounting, so
                                // nothing is double-counted here.
                                let ev = frame.to_event();
                                let outcome = run_frame(
                                    &ev,
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
                                    operator_nonce,
                                    signer.as_ref(),
                                    &gap_probe,
                                    &mut quarantine,
                                    quarantine_journal.as_mut(),
                                )
                                .await;
                                journal_reentry_outcome(
                                    &frame,
                                    reentry_outcome(outcome),
                                    &mut quarantine,
                                    quarantine_journal.as_mut(),
                                );
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
                            if let Err(error) =
                                journal.record_resolve(frame.hash, resolution, now_unix_ms())
                            {
                                tracing::warn!(%error, "finalized tombstone not journaled");
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
            if cfg.stop_file.exists() {
                tracing::info!("kill switch present - halting");
                break;
            }
            run_frame(
                &ev,
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
                operator_nonce,
                signer.as_ref(),
                &gap_probe,
                &mut quarantine,
                quarantine_journal.as_mut(),
            )
            .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        bid_submission_target, build_broadcast_relays, journal_reentry_outcome,
        nonce_lane_evidence, outcome_for, reentry_outcome, strategy_arm_refusal, FrameOutcome,
        ParkedFrame, Quarantine, QuarantineJournal, ReentryOutcome,
    };
    use alloy::primitives::U256;
    use degenbot_bot::sidecar::SidecarConfig;
    use degenbot_rpc::provider::{AlloyProvider, DEFAULT_MAX_RETRIES};
    use std::sync::Arc;

    #[test]
    fn backrun_arm_is_own_binary_and_settlement_refused() {
        use degenbot_config::StrategyName;

        assert!(strategy_arm_refusal(None).is_none());
        assert!(strategy_arm_refusal(Some(StrategyName::Backrun)).is_none());
        let refusal = strategy_arm_refusal(Some(StrategyName::Settlement)).expect("refused");
        assert!(refusal.contains("backrun"), "{refusal}");
        assert!(refusal.contains("strategy.name"), "{refusal}");
    }

    #[tokio::test]
    async fn broadcast_relays_are_private_first_with_read_provider_fallback() {
        let provider = Arc::new(
            AlloyProvider::new("http://node.local:8545", DEFAULT_MAX_RETRIES)
                .await
                .expect("lazy http provider"),
        );
        let mut cfg =
            SidecarConfig::from_config(&degenbot_config::BotConfig::default(), String::new());
        assert!(
            build_broadcast_relays(&cfg, &provider).await.is_empty(),
            "an unset mevblocker_url must leave the read-provider-only list"
        );
        cfg.mevblocker_url = Some("http://private.local:8545".to_string());
        let relays = build_broadcast_relays(&cfg, &provider).await;
        assert_eq!(relays.len(), 2, "private endpoint + read provider");
        assert_eq!(relays[0].rpc_url(), "http://private.local:8545");
        assert!(
            Arc::ptr_eq(&relays[1], &provider),
            "the read provider is the fallback relay"
        );
        assert!(
            !Arc::ptr_eq(&relays[0], &provider),
            "the private endpoint leads, so it is not the read provider"
        );
    }

    #[test]
    fn mevblocker_url_does_not_alter_the_target() {
        // Adjudicated policy: augment, never replace. The key only changes the
        // `extra_broadcast` list content; the target-selection region computes
        // the same target with and without it set.
        use degenbot_submission::submit::SubmissionTarget;

        let mut cfg =
            SidecarConfig::from_config(&degenbot_config::BotConfig::default(), String::new());
        let hash = alloy::primitives::B256::repeat_byte(0x11);
        let without = bid_submission_target(&cfg, hash, 21_000_001);

        cfg.mevblocker_url = Some("http://private.local:8545".to_string());
        let with = bid_submission_target(&cfg, hash, 21_000_001);

        let to_bundle = |target: SubmissionTarget| match target {
            SubmissionTarget::Bundle(b) => Some(b),
            SubmissionTarget::Public => None,
        };
        let a =
            to_bundle(without).expect("target must be the Bundle arm regardless of mevblocker_url");
        let b =
            to_bundle(with).expect("target must be the Bundle arm regardless of mevblocker_url");
        assert_eq!(a.stream_url, b.stream_url);
        assert_eq!(a.target_tx_hash, b.target_tx_hash);
        assert_eq!(a.block_number, b.block_number);
    }

    #[test]
    fn rescue_outcome_mapping_routes_transient_gap_and_terminal() {
        assert_eq!(
            reentry_outcome(FrameOutcome::ReplayUnavailable),
            ReentryOutcome::Transient
        );
        assert_eq!(
            reentry_outcome(FrameOutcome::ReplayFailed),
            ReentryOutcome::Transient
        );
        assert_eq!(
            reentry_outcome(FrameOutcome::GapPending),
            ReentryOutcome::GapPending
        );
        assert_eq!(reentry_outcome(FrameOutcome::Bid), ReentryOutcome::Terminal);
        assert_eq!(
            reentry_outcome(FrameOutcome::Terminal("already_settled")),
            ReentryOutcome::Terminal
        );
        assert_eq!(
            outcome_for("replay_unavailable"),
            FrameOutcome::ReplayUnavailable
        );
        assert_eq!(outcome_for("replay_failed"), FrameOutcome::ReplayFailed);
        assert_eq!(outcome_for("gap_pending"), FrameOutcome::GapPending);
        assert_eq!(
            outcome_for("already_settled"),
            FrameOutcome::Terminal("already_settled")
        );
    }

    #[test]
    fn nonce_lane_failure_is_no_evidence_never_max() {
        // D1 pin: a failed read must not fabricate u64::MAX consumption
        // for every tracked frame of the sender - it is no evidence; the
        // caller skips the tick and every frame holds.
        assert!(nonce_lane_evidence::<()>(Err(())).is_none());
        assert!(nonce_lane_evidence::<()>(Ok(U256::MAX)).is_none());
        assert_eq!(
            nonce_lane_evidence::<()>(Ok(U256::from(29_764u64))),
            Some(29_764)
        );
    }

    fn temp_journal(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "degenbot-sidecar-reentry-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(degenbot_submission::gap_quarantine_journal::JOURNAL_FILE_NAME);
        (dir, path)
    }

    fn resolve_count(path: &std::path::Path) -> usize {
        std::fs::read_to_string(path).map_or(0, |raw| {
            raw.lines()
                .filter(|line| line.contains("\"kind\":\"resolve\""))
                .count()
        })
    }

    fn parked(hash_byte: u8) -> ParkedFrame {
        use alloy::primitives::{Address, Bytes, B256, U256};
        let mut raw = [0u8; 32];
        raw[0] = hash_byte;
        ParkedFrame {
            hash: B256::from(raw),
            chain_id: 1,
            from: Address::with_last_byte(7),
            to: None,
            value: U256::ZERO,
            data: Bytes::new(),
            gas: 300_000,
            max_fee_per_gas: 514_684_409,
            max_priority_fee_per_gas: 1_000_000_000,
            claimed_nonce: 12,
            expected_at_capture: 10,
            tx_type: 2,
            access_list: serde_json::json!([]),
            received_unix_ms: 1_700_000_000_000,
        }
    }

    /// P1: a transient re-entry failure re-parks the ORIGINAL frame and writes
    /// no resolve, so the boot fold reloads it alive.
    #[test]
    fn transient_reentry_failure_reparks_and_leaves_no_resolve() {
        let (dir, path) = temp_journal("transient");
        let mut journal = QuarantineJournal::open(&path).expect("open");
        let mut q = Quarantine::new();
        let f = parked(61);
        journal_reentry_outcome(&f, ReentryOutcome::Transient, &mut q, Some(&mut journal));

        assert_eq!(
            q.state(f.hash),
            Some(degenbot_submission::gap_quarantine::FrameState::Tracked),
            "the original frame is back in quarantine"
        );
        assert_eq!(
            resolve_count(&path),
            0,
            "a transient failure writes no resolve"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P2: a gap-pending re-entry already re-parked the frame and wrote the
    /// fresh park; the router adds no resolve and no duplicate.
    #[test]
    fn gap_pending_reentry_does_not_resolve_or_duplicate() {
        let (dir, path) = temp_journal("gap-pending");
        let mut journal = QuarantineJournal::open(&path).expect("open");
        let mut q = Quarantine::new();
        let f = parked(62);
        journal_reentry_outcome(&f, ReentryOutcome::GapPending, &mut q, Some(&mut journal));

        assert_eq!(q.len(), 0, "run_frame already re-parked; no duplicate");
        assert_eq!(resolve_count(&path), 0, "the fresh park is its own truth");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P3: a terminal re-entry writes exactly one resolve and never re-parks.
    #[test]
    fn terminal_reentry_writes_exactly_one_resolve() {
        let (dir, path) = temp_journal("terminal");
        let mut journal = QuarantineJournal::open(&path).expect("open");
        let mut q = Quarantine::new();
        let f = parked(63);
        journal_reentry_outcome(&f, ReentryOutcome::Terminal, &mut q, Some(&mut journal));

        assert_eq!(q.len(), 0, "a terminal outcome never re-parks");
        assert_eq!(resolve_count(&path), 1, "exactly one resolve");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
