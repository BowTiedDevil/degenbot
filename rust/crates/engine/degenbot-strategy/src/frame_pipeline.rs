//! The frame pipeline : pending-tx frame → frame-replay seam →
//! journal-pool extraction → workspace admission → discovery fan → solve →
//! compose → `eth_callMany` gate → bid decision.
//!
//! # Cache ownership split (the task's hook, resolved here)
//!
//! Two lifetimes meet per frame and must NOT be conflated:
//!
//! - **The strategy runtime** ([`MarketContext`]) survives frames: the
//!   DB-backed connector index, the token id/address joins, the cross-block
//!   warm-code cache owner, and the per-block replay handle live here.
//!   These caches are process-scoped, exactly like the frame stream they
//!   serve — a per-frame refill of the token joins would re-pay a DB query
//!   per pool per frame and forfeit the index's memoized depth rankings.
//! - **The planning workspace scope** dies per frame: [`BackrunSolver`]
//!   wraps a fresh [`degenbot_bot::bot_core::planning::Workspace`] per
//!   frame; replayed pool state, declared paths, and workspace pool ids
//!   never outlive the frame that staged them. Nothing the scope mutates
//!   is visible to the next frame — the next frame re-admits from its own
//!   replay (stale post-target state is structurally impossible to reuse).
//!
//! # Calldata-free frames
//!
//! The touched set + journalled words decide everything; call bytes are never
//! decoded. The downstream `decide()` gate keeps its `TargetClass` parameter
//! shape, so the pipeline feeds the actionable sentinel directly (a frame
//! evaluated end-to-end here is actionable by construction) and carries
//! TRUTHFUL observe reasons via
//! [`FrameArtifacts::decision`]: `replay_failed`, `reverted`,
//! `v4_unsupported`, and `no_candidate` are distinct and mutually exclusive.
//!
//! # Connector admission state source
//!
//! Connector pools are read through the SAME layered chain view the frames
//! replay over ([`ScratchEvm`] — its read-caches answer; cold misses count
//! in the shared RPC counter); a V2 view miss falls back to raw RPC
//! (`getReserves`), and a V3 cold hop reads `slot0`/`liquidity` over RPC with
//! its tick map staged by the ingress (`Db → Chain`). Nothing is fabricated:
//! a zero/failed read skips or falls back, never guesses.
//!
//! # Envelope floor and the bid ladder (single bribe site)
//!
//! Profit `P` (wei) is the exact solver delta over the frame's REPLAYED
//! post-states, WETH-closed for every quote — never an analytic post-target
//! estimate. Two economic gates hang off `P` and they must not double-count
//! each other's term:
//!
//! 1. **Envelope floor** `F = gas_units × b_next`: the gas a landed bundle
//!    costs at the projected next-block base fee `b_next`. The per-block
//!    replay handle ([`build_block_handle`]) projects `b_next` as the
//!    EIP-1559 100%-full-block worst case, `parent_basefee × 12 / 10`; the
//!    operator's gas budget is denominated at that projected fee and rides
//!    [`PipelineConfig::gas_floor_wei`]. [`BackrunSolver::evaluate`] gates
//!    on `P`'s rigorous upper bound (the solver's profit-envelope gate)
//!    BEFORE the bribe: a candidate must clear its gas cost from true
//!    profit. The residual we keep after the bribe (`P − B`, the 2% of the
//!    98% ladder) funds nothing further — the executor's non-zero
//!    check-mode seatbelts the landed tx's worst case to gas.
//! 2. **Wallet economics + bid ladder** — the wallet funds ONLY the gas:
//!    the on-chain bribe is drawn from flash proceeds (the executor's
//!    config word takes `bribe_bips` of the TRUE profit delta to
//!    `block.coinbase`) and the residue parks in executor custody.
//!    [`net_bid`] sizes the bribe from the surplus over the gas burn
//!    (`gross − gas×1.05`, capped by the `bribe_bips` ceiling and the
//!    per-bundle cap) and rejects candidates whose gross cannot cover the
//!    gas; the recomposed config word speaks the SAME bips (one share
//!    decision), and [`decide`] only caps the bid against the
//!    budget/bundle ceilings.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::anchored_dfs::NON_WETH_CYCLE;
use crate::backrun::{gate_mined_target, BackrunConfig, Decision};
use crate::backrun_engine::BackrunSolver;
use crate::backrun_engine::PathReject;
use crate::backrun_strategy::SolveStats;
use alloy::primitives::{Address, Bytes, B256, U256};
use degenbot_bot::bot_core::SimAnchorOracle;
use degenbot_bot::connector_index::V2ConnectorIndex;
use degenbot_pools::v3_state::ClSlotLayout;
use degenbot_rpc::backrun_feed::BackrunFeedEvent;
use degenbot_rpc::provider::{AlloyProvider, DEFAULT_MAX_RETRIES};
use degenbot_simulation::sim::evm::frame_replay::{
    ReplayFrameError, ReplayStatus, ReplayableTx, SequenceReplayError,
};
use degenbot_simulation::sim::evm::journal_pools::{
    extract_pool_post_states, PoolFamily, PoolPostKind, PoolPostState, TypedPoolPost,
    V4PoolDescriptor, V4PoolSet,
};
use degenbot_simulation::sim::evm::BlockSimHandle;
use degenbot_simulation::{SimulationOverrideParams, WarmCodeCacheInner};
use degenbot_submission::dispatcher::Dispatcher;
use degenbot_submission::monitor::ReceiptProbe;
use degenbot_submission::signer::TxSigner;
use degenbot_submission::submission_ledger::NonceLane;
use degenbot_submission::submit::{
    dispatch_and_submit, BundleTarget, SubmissionTarget, SubmitCandidate,
};
use hashbrown::HashMap as HbMap;
use parking_lot::RwLock;

use crate::execution_context::ExecutionContext;
use crate::pending_tx::{FrameContext, GateInput, PendingTxReaction};

pub use crate::market_context::MarketContext;

// ─────────────────────────────────────────────────────────────────────────
// Offline-review capture (moved verbatim from the bin: capture failures
// never disturb the hot loop).
// ─────────────────────────────────────────────────────────────────────────

/// Append one JSON line to the offline-review capture
/// (`logging.trace_jsonl`, best-effort).
pub fn trace_jsonl(kind: &str, mut v: serde_json::Value) {
    use std::io::Write;
    // The typed `logging.trace_jsonl` key wins; absent, the capture defaults
    // to the session's `trace.jsonl` installed at boot (see degenbot-runs).
    let Some(path) = degenbot_runs::configured_trace_jsonl_path() else {
        return;
    };
    let mut line = serde_json::json!({
        "ts_unix_ms": u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0_u128, |d| d.as_millis()),
        )
        .unwrap_or_default(),
        "kind": kind,
    });
    if let (Some(dst), Some(src)) = (line.as_object_mut(), v.as_object_mut()) {
        for (k, val) in std::mem::take(src) {
            dst.insert(k, val);
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{line}");
    }
}

fn now_unix_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0_u128, |d| d.as_millis()),
    )
    .unwrap_or_default()
}

// ─────────────────────────────────────────────────────────────────────────
// Driver-owned stage instrumentation
// ─────────────────────────────────────────────────────────────────────────

/// One typed instrumentation event a strategy stage reported. Stages append
/// domain facts to the frame's [`StageTrace`]; this module renders them to
/// the offline-review capture, so no strategy code touches the capture.
#[derive(Debug)]
pub enum StageEvent {
    /// An admission skip: the pool and the sub-step that refused it.
    ExtractSkip { pool: Address, stage: &'static str },
    /// A V3 anchor entering the staged window (before its ingress admission).
    AnchorStage { pool: Address },
    /// A V3 anchor's staged tick evidence (after its ingress admission).
    AnchorTicks {
        pool: Address,
        layout: ClSlotLayout,
        spacing: i32,
        tick: i32,
        liquidity: u128,
        crossed: usize,
    },
    /// A discovery hop reusing a pool this frame already admitted.
    HopReuse {
        pool: Address,
        workspace_pool_id: u64,
    },
    /// A discovery hop's admission step refused: the pool, the sub-step, and
    /// the detail.
    HopFail {
        pool: Address,
        stage: String,
        detail: String,
    },
    /// A discovery hop whose pool family the arm cannot type.
    UnsupportedHop { pool_id: u64, kind: String },
    /// The discovery counters behind the frame's `discover` event.
    Discover(DiscoverTrace),
    /// The evaluate stage's solve evidence.
    Solve(SolveTrace),
    /// The composer declined to encode (the decline's stable label).
    ComposeDecline { reason: &'static str },
    /// The wallet gate refused the net-of-gas bid.
    NetGateRefusal {
        gross_profit_wei: u128,
        wallet_gas_cost_wei: u128,
    },
}

/// The discovery counters the `discover` event publishes.
#[derive(Debug)]
pub struct DiscoverTrace {
    pub connectors: usize,
    pub dfs_cycles: usize,
    pub dfs_chains: usize,
    pub unsupported_hop: usize,
    pub affected: usize,
    pub non_weth_cycles: usize,
    pub non_base_quote_dropped: bool,
    pub cycle_max_hops: usize,
    pub touched_pools: usize,
    pub cycles_with_multi_touched: usize,
}

/// The evaluate stage's solve evidence: the aggregate stats plus the
/// per-chain touched-leg counts, aligned with the stats' chains.
#[derive(Debug)]
pub struct SolveTrace {
    pub stats: SolveStats,
    pub touched_legs: Vec<usize>,
}

/// The stage-evidence buffer one frame's stages append to. The frame module
/// renders (and drains) it after each stage, so the capture carries the
/// events in stage order with no strategy-owned emission.
#[derive(Debug, Default)]
pub struct StageTrace {
    events: Vec<StageEvent>,
}

impl StageTrace {
    /// Append one typed stage event.
    pub fn push(&mut self, event: StageEvent) {
        self.events.push(event);
    }

    /// Render the buffered events to the offline-review capture, stamping
    /// each with the frame's trace identity, then drain the buffer.
    pub fn render(&mut self, trace_tx: &str) {
        for event in self.events.drain(..) {
            match event {
                StageEvent::ExtractSkip { pool, stage } => trace_jsonl(
                    "extract_skip",
                    serde_json::json!({
                        "tx": trace_tx,
                        "pool": format!("0x{}", alloy::hex::encode(pool)),
                        "stage": stage,
                    }),
                ),
                StageEvent::AnchorStage { pool } => trace_jsonl(
                    "anchor_stage",
                    serde_json::json!({
                        "tx": trace_tx,
                        "pool": format!("0x{}", alloy::hex::encode(pool)),
                        "stage": "window-enter",
                    }),
                ),
                StageEvent::AnchorTicks {
                    pool,
                    layout,
                    spacing,
                    tick,
                    liquidity,
                    crossed,
                } => trace_jsonl(
                    "anchor_ticks",
                    serde_json::json!({
                        "tx": trace_tx,
                        "pool": format!("0x{}", alloy::hex::encode(pool)),
                        "layout": format!("{layout:?}"),
                        "spacing": spacing,
                        "tick": tick,
                        "liquidity": liquidity.to_string(),
                        "crossed": crossed,
                        "source": "ingress",
                    }),
                ),
                StageEvent::HopReuse {
                    pool,
                    workspace_pool_id,
                } => trace_jsonl(
                    "hop_reuse",
                    serde_json::json!({
                        "tx": trace_tx,
                        "pool": format!("0x{}", alloy::hex::encode(pool)),
                        "workspace_pool_id": workspace_pool_id,
                    }),
                ),
                StageEvent::HopFail {
                    pool,
                    stage,
                    detail,
                } => trace_jsonl(
                    "admit_hop_fail",
                    serde_json::json!({
                        "tx": trace_tx,
                        "pool": format!("0x{}", alloy::hex::encode(pool)),
                        "stage": stage,
                        "detail": detail,
                    }),
                ),
                StageEvent::UnsupportedHop { pool_id, kind } => trace_jsonl(
                    "unsupported_hop",
                    serde_json::json!({
                        "tx": trace_tx,
                        "pool_id": pool_id,
                        "kind": kind,
                    }),
                ),
                StageEvent::Discover(trace) => {
                    trace_jsonl("discover", discover_trace_payload(trace_tx, &trace));
                }
                StageEvent::Solve(trace) => {
                    trace_jsonl("solve", solve_trace_payload(trace_tx, &trace));
                }
                StageEvent::ComposeDecline { reason } => trace_jsonl(
                    "composed",
                    serde_json::json!({
                        "tx": trace_tx,
                        "composed": false,
                        "reason": reason,
                    }),
                ),
                StageEvent::NetGateRefusal {
                    gross_profit_wei,
                    wallet_gas_cost_wei,
                } => trace_jsonl(
                    "composed",
                    serde_json::json!({
                        "tx": trace_tx,
                        "composed": false,
                        "reason": "net_after_gas_unprofitable",
                        "gross_profit_wei": gross_profit_wei,
                        "wallet_gas_cost_wei": wallet_gas_cost_wei,
                    }),
                ),
            }
        }
    }
}

/// The `discover` JSONL payload built from the frame's discovery counters.
/// The offline harness asserts this boundary because the stage itself needs
/// the production scratch stack.
#[must_use]
pub fn discover_trace_payload(trace_tx: &str, t: &DiscoverTrace) -> serde_json::Value {
    serde_json::json!({
        "tx": trace_tx,
        "connectors": t.connectors,
        "cycles_proposed": t.dfs_cycles,
        "dfs_cycles": t.dfs_cycles,
        "dfs_chains": t.dfs_chains,
        "unsupported_hop": t.unsupported_hop,
        "affected": t.affected,
        "non_weth_cycles": t.non_weth_cycles,
        "cycle_reject": NON_WETH_CYCLE,
        "non_base_quote_dropped": t.non_base_quote_dropped,
        "cycle_max_hops": t.cycle_max_hops,
        "touched_pools": t.touched_pools,
        "cycles_with_multi_touched": t.cycles_with_multi_touched,
    })
}

/// The `solve` JSONL payload: the per-chain outcomes behind the aggregate
/// stats, with the touched-leg counts the discovery stage reported.
fn solve_trace_payload(trace_tx: &str, t: &SolveTrace) -> serde_json::Value {
    serde_json::json!({
        "tx": trace_tx,
        "chains": t
            .stats
            .chains
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let deficit_reasons = match &c.reject {
                    Some(PathReject::UnusablePoolState { reasons, .. }) => {
                        (!reasons.is_empty()).then(|| reasons.clone())
                    }
                    _ => None,
                };
                serde_json::json!({
                "pools": c
                    .pools
                    .iter()
                    .map(|a| format!("0x{}", alloy::hex::encode(a)))
                    .collect::<Vec<_>>(),
                "touched_legs": t.touched_legs.get(i).copied().unwrap_or(0),
                "evaluated": c.evaluated,
                "profit_wei": c.profit_wei.map(|p| p.to_string()),
                "reject": c.reject.as_ref().map(PathReject::label),
                "reject_deficits": match &c.reject {
                    Some(PathReject::UnusablePoolState { deficits, .. }) => Some(deficits),
                    _ => None,
                },
                "reject_deficit_reasons": deficit_reasons,
                "reject_deficit_pools": match &c.reject {
                    Some(PathReject::UnusablePoolState { pools, .. }) => {
                        (!pools.is_empty()).then(|| pools.clone())
                    }
                    _ => None,
                },
                })
            })
            .collect::<Vec<_>>(),
        "best": t.stats.best.is_some(),
        "best_profit_wei": t.stats.best.as_ref().map(|b| b.profit.to_string()),
        "dfs_declared": t.stats.dfs_declared,
        "dfs_evaluated": t.stats.dfs_evaluated,
        "non_base_quote_dropped": t.stats.non_base_quote_dropped,
    })
}

// ─────────────────────────────────────────────────────────────────────────

/// The per-frame strategy configuration the bin owns (session deployment,
/// owner identity, and economics).
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// The boot-owned execution deployment shared with the strategy adapter
    /// and per-block frame simulation.
    pub execution: ExecutionContext,
    pub owner: Address,
    /// The builder's bribe share of TRUE profit (bips of `10_000`) — the
    /// COMPETITIVENESS CEILING; the wallet gate can compose lower bips.
    pub bribe_bips: u16,
    /// The wallet's gas burn for one composed bundle (`gas_estimate x
    /// effective_gas_price`, wei) — what the wallet actually pays per bid
    /// (the on-chain bribe is drawn from flash proceeds). The bin refreshes
    /// it on head advances; the compose gate reads it atomically. Stable
    /// atomics top out at u64: saturating far above any real gas burn
    /// (`u64::MAX` wei = 18.4 ETH of gas for one bundle).
    pub wallet_gas_cost_wei: Arc<std::sync::atomic::AtomicU64>,
    /// The gas floor the envelope gate evaluates at (wei).
    pub gas_floor_wei: U256,
    /// Offline fixture mode: the run replays captured frames against a
    /// pinned historical head, so the live `eth_callMany` sim gate (which
    /// evaluates at `latest`) would diverge. Set on the dry-run path when a
    /// fixture head is pinned; the sim stage then records
    /// `sim_skipped_fixture_mode` and never composes a bid-able artifact.
    pub fixture_mode: bool,
}

impl PipelineConfig {
    /// The wallet's current gas burn for one composed bundle (wei).
    #[must_use]
    pub fn wallet_gas_cost(&self) -> u128 {
        u128::from(
            self.wallet_gas_cost_wei
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }
}

/// A bid's wallet economics, carried to the bin's submit path so
/// `SubmitCandidate` reports gross/net/gas HONESTLY (the struct's own doc
/// contract; the arm used to fill both profit fields with the bid and
/// a hardcoded 300k gas).
#[derive(Debug, Clone)]
pub struct BidEconomics {
    /// The solved gross on-chain profit (wei).
    pub gross_profit_wei: u128,
    /// The wallet's gas estimate for this bundle (wei).
    pub wallet_gas_cost_wei: u128,
    /// The bips the recomposed config word speaks.
    pub bribe_bips: u16,
    /// The on-chain bribe (wei), `bribe_bips` of gross.
    pub bid_wei: U256,
}

/// Per-stage wall times (µs) of one processed frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StageTimings {
    pub replay_us: u64,
    pub extract_us: u64,
    pub admit_us: u64,
    pub discover_us: u64,
    pub solve_us: u64,
    pub compose_us: u64,
    pub sim_us: u64,
}

impl StageTimings {
    /// The JSONL `stages` payload.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "replay_us": self.replay_us,
            "extract_us": self.extract_us,
            "admit_us": self.admit_us,
            "discover_us": self.discover_us,
            "solve_us": self.solve_us,
            "compose_us": self.compose_us,
            "sim_us": self.sim_us,
        })
    }
}

/// Everything the bin needs after one frame: the gate decision (with a
/// TRUTHFUL observe reason), the bid request, the composed artifact, and
/// the stage timings for the JSONL trace.
#[derive(Debug, Clone)]
pub struct FrameArtifacts {
    pub decision: Decision,
    pub requested_bid: U256,
    pub submit_calldata: Option<Bytes>,
    /// The bid's wallet economics, present only when a sim-passed candidate
    /// cleared the net-of-gas gate and composed a bid-able artifact.
    pub economics: Option<BidEconomics>,
    pub stages: StageTimings,
    /// The typed replay failure when the frame died at the replay seam
    /// (`Decision::Observe { reason }` mirrors `replay_observe_reason`).
    pub replay_frame_error: Option<ReplayFrameError>,
}

impl FrameArtifacts {
    fn observe(reason: &'static str, stages: StageTimings) -> Self {
        Self::observe_with_replay_error(reason, stages, None)
    }

    fn observe_with_replay_error(
        reason: &'static str,
        stages: StageTimings,
        replay_frame_error: Option<ReplayFrameError>,
    ) -> Self {
        // The frame never composed a bid-able artifact: observe it with the
        // reason the STAGE produced, never a generic sim-gate label.
        Self {
            decision: Decision::Observe { reason },
            requested_bid: U256::ZERO,
            submit_calldata: None,
            economics: None,
            stages,
            replay_frame_error,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Per-block replay handle
// ─────────────────────────────────────────────────────────────────────────

/// Build the per-block frame-replay handle pinning `head` under the boot-owned
/// execution deployment: the scratch
/// stack reads `BlockId::Number(head)` and its frames run in the NEXT
/// block's env. State overrides are the ZERO set — a foreign frame must
/// execute against chain state, not the strategy's simulated funding.
/// `oracle` is the sim DB's membership/observation view — the boot
/// [`RouteRegistry`](degenbot_bot::bot_core::RouteRegistry) (the driver
/// carries no engine state, so the divergence observer is inert) or a
/// state-less [`NoSimAnchor`](degenbot_bot::bot_core::NoSimAnchor) when the
/// registry load failed; the shared `warm_cache` carries the cross-block
/// bytecode/account caches across handle rebuilds.
/// `None` when the head block fetch or the handle build fails (no ambient
/// multi-threaded runtime for `WrapDatabaseAsync` included).
pub async fn build_block_handle<'a>(
    provider: &AlloyProvider,
    head: u64,
    execution: &ExecutionContext,
    warm_cache: &Arc<RwLock<WarmCodeCacheInner>>,
    oracle: &'a dyn SimAnchorOracle,
) -> Option<BlockSimHandle<'a>> {
    let block = provider.get_block(head).await.ok().flatten()?;
    let timestamp = block.header.timestamp;
    let base_fee = u128::from(block.header.base_fee_per_gas.unwrap_or(0));
    let override_params = SimulationOverrideParams {
        owner: Address::ZERO,
        inject_code: false,
        injected_address: None,
        runtime_bytecode: Bytes::new(),
        warmup: degenbot_executor::WarmupSlots {
            weth_balance: U256::ZERO,
            erc6909_weth: U256::ZERO,
            erc6909_native: U256::ZERO,
        },
        weth_address: execution.weth(),
        pool_manager_address: execution.pool_manager(),
    };
    BlockSimHandle::build(
        provider,
        base_fee.saturating_mul(12) / 10,
        head,
        timestamp,
        &override_params,
        oracle,
        warm_cache,
        None,
        false,
    )
}

// ─────────────────────────────────────────────────────────────────────────
// Dry-run fixture loader (captured-frame JSONL)
// ─────────────────────────────────────────────────────────────────────────

/// Load dry-run fixture frames from a JSONL capture (the `/tmp/mb_trace.jsonl`
/// conventions: either the trace wrapper object carrying the verbatim feed
/// event fields, or the bare event). Malformed lines are SKIPPED — the
/// capture is best-effort by contract and a dry-run must process the rest.
#[must_use]
pub fn load_fixture_frames(path: &std::path::Path) -> Vec<BackrunFeedEvent> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let now = now_unix_ms();
    text.lines()
        .filter_map(|l| parse_frame_line(l, now))
        .collect()
}

fn parse_frame_line(line: &str, now: u64) -> Option<BackrunFeedEvent> {
    let obj = serde_json::from_str::<serde_json::Value>(line)
        .ok()?
        .take_object()?;
    let sget = |k: &str| {
        obj.get(k)
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
    };
    let uget = |k: &str| obj.get(k).and_then(serde_json::Value::as_u64);
    let addr = |k: &str| sget(k).and_then(|s| s.parse().ok());
    Some(BackrunFeedEvent {
        chain_id: uget("chain_id").unwrap_or(1),
        from: addr("from")?,
        to: addr("to"),
        value: sget("value")
            .and_then(|s| U256::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            .unwrap_or_default(),
        data: sget("data")
            .and_then(|s| alloy::hex::decode(s).ok())
            .map(Bytes::from)
            .unwrap_or_default(),
        gas: uget("gas").unwrap_or(120_000),
        max_fee_per_gas: u128::from(uget("max_fee_per_gas").unwrap_or(0)),
        max_priority_fee_per_gas: u128::from(uget("max_priority_fee_per_gas").unwrap_or(0)),
        nonce: uget("nonce").unwrap_or(0),
        // `trim_start_matches` strips ALL leading repeats — the captured
        // feed carries `hash` as `0x0x…` (kept verbatim in the fixture; the
        // loader is the tolerant side of the contract).
        hash: sget("hash")
            .and_then(|s| s.trim_start_matches("0x").parse().ok())
            .unwrap_or_default(),
        access_list: serde_json::Value::Null,
        tx_type: u8::try_from(uget("tx_type").unwrap_or(2)).unwrap_or(2),
        received_unix_ms: uget("received_unix_ms").unwrap_or(now),
        raw_signed_tx: None,
    })
}

/// Take the object out of a JSON value (a one-line serde helper the loader
/// uses to accept both the trace-wrapper shape and the bare event shape).
trait TakeObject {
    fn take_object(self) -> Option<serde_json::Map<String, serde_json::Value>>;
}

impl TakeObject for serde_json::Value {
    fn take_object(self) -> Option<serde_json::Map<String, serde_json::Value>> {
        match self {
            serde_json::Value::Object(m) => Some(m),
            _ => None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Descriptors + extraction trace
// ─────────────────────────────────────────────────────────────────────────

/// The descriptor projection of one frame's touched set: the supported-family
/// map the journal extractor consumes, whether the `PoolManager` was touched,
/// and the touched addresses whose DB family this arm cannot type.
#[derive(Debug, Default)]
pub struct FrameDescriptors {
    /// Supported-family descriptors, from the connector index only.
    pub by_address: HbMap<Address, PoolFamily>,
    /// Whether the authoritative or a roster-known V4 `PoolManager` named a
    /// touched account.
    pub hit_v4: bool,
    /// Touched addresses present in the DB under an unsupported family, with
    /// the `kind` discriminator. Excluded from `by_address` on purpose —
    /// extraction must never guess a decode — but carried so the frame
    /// observes `family-unsupported` instead of an unexplained no-candidate.
    pub unsupported: Vec<(Address, String)>,
}

/// Project the frame's touched set into the pool-descriptor map the journal
/// extractor consumes: EVERY descriptor comes from the connector index (the
/// tracked-pool registry) — V2 pair / V3 pool / the session context's
/// authoritative V4 `PoolManager` and every manager with roster edges
/// (descriptors exist so `v4_unsupported` is OBSERVABLE, never guessable).
/// A touched address whose DB family the arm cannot type yields no descriptor
/// but is returned in [`FrameDescriptors::unsupported`] so it observes loudly.
#[must_use]
pub fn build_descriptors(
    index: Option<&V2ConnectorIndex>,
    touched: &[(Address, Vec<U256>)],
    execution: &ExecutionContext,
) -> FrameDescriptors {
    let mut out = FrameDescriptors::default();
    for (addr, _) in touched {
        let manager_edges = index.map(|idx| idx.v4_edges_for_manager(*addr));
        let is_manager = *addr == execution.pool_manager()
            || manager_edges
                .as_ref()
                .is_some_and(|edges| !edges.is_empty());
        if is_manager {
            out.hit_v4 = true;
            // The index's live V4 roster populates the per-manager descriptor
            // set; a manager with no roster edges keeps the empty set, so
            // extraction stays explicitly Unsupported (see `V4PoolSet`).
            let pools = manager_edges
                .unwrap_or_default()
                .into_iter()
                .map(|edge| V4PoolDescriptor {
                    pool_id: edge.pool_hash,
                    tick_spacing: edge.tick_spacing,
                })
                .collect();
            out.by_address.insert(
                *addr,
                PoolFamily::V4PoolManager {
                    pools: V4PoolSet::new(pools),
                },
            );
            continue;
        }
        let Some(idx) = index else { continue };
        if idx.edge_by_address(*addr).is_some() {
            out.by_address.insert(*addr, PoolFamily::V2Pair);
            continue;
        }
        if let Some(e3) = idx.v3_edge_by_address(*addr) {
            out.by_address.insert(
                *addr,
                PoolFamily::V3 {
                    // The edge carries the fork layout (W32CAU): a Pancake
                    // pool replayed with Uniswap slots stages a garbage map
                    // and every anchored chain dies sequence_unavailable.
                    layout: e3.layout,
                    tick_spacing: e3.tick_spacing,
                    // The pre-tx hint is unknown to the pipeline; the
                    // journal's own slot0 carries the only anchor.
                    current_tick_hint: None,
                },
            );
            continue;
        }
        if let Some(kind) = idx.unsupported_kind(*addr) {
            out.unsupported.push((*addr, kind.to_string()));
        }
    }
    out
}

/// A stable short digest of one extracted post-state (the JSONL `extract`
/// event's per-family evidence).
#[must_use]
pub fn state_digest(state: &PoolPostState) -> String {
    use alloy::primitives::keccak256;
    let mut buf = Vec::with_capacity(64 + 4 * 32);
    buf.extend_from_slice(state.address.as_slice());
    match &state.kind {
        PoolPostKind::Unsupported => buf.extend_from_slice(&[0xFF]),
        PoolPostKind::Typed(TypedPoolPost::V2 { reserves }) => {
            buf.extend_from_slice(&[0x02]);
            buf.extend_from_slice(&U256::from(reserves.reserve0).to_be_bytes::<32>());
            buf.extend_from_slice(&U256::from(reserves.reserve1).to_be_bytes::<32>());
        }
        PoolPostKind::Typed(TypedPoolPost::V3 {
            sqrt_price_x96,
            tick,
            liquidity,
            touched_ticks,
        }) => {
            buf.extend_from_slice(&[0x03]);
            buf.extend_from_slice(&sqrt_price_x96.unwrap_or_default().to_be_bytes::<32>());
            buf.extend_from_slice(&i64::from(tick.unwrap_or(0)).to_be_bytes());
            buf.extend_from_slice(&liquidity.unwrap_or(0).to_be_bytes());
            buf.extend_from_slice(
                &u64::try_from(touched_ticks.len())
                    .unwrap_or(u64::MAX)
                    .to_be_bytes(),
            );
            for t in touched_ticks {
                buf.extend_from_slice(&i64::from(t.tick).to_be_bytes());
                buf.extend_from_slice(&t.liquidity_net.to_be_bytes());
            }
        }
        PoolPostKind::Typed(TypedPoolPost::V4 {
            pool_id,
            sqrt_price_x96,
            tick,
            liquidity,
            touched_ticks,
        }) => {
            buf.extend_from_slice(&[0x04]);
            buf.extend_from_slice(pool_id.as_slice());
            buf.extend_from_slice(&sqrt_price_x96.unwrap_or_default().to_be_bytes::<32>());
            buf.extend_from_slice(&i64::from(tick.unwrap_or(0)).to_be_bytes());
            buf.extend_from_slice(&liquidity.unwrap_or(0).to_be_bytes());
            buf.extend_from_slice(
                &u64::try_from(touched_ticks.len())
                    .unwrap_or(u64::MAX)
                    .to_be_bytes(),
            );
            for t in touched_ticks {
                buf.extend_from_slice(&i64::from(t.tick).to_be_bytes());
                buf.extend_from_slice(&t.liquidity_net.to_be_bytes());
            }
        }
    }
    let h = keccak256(&buf);
    format!("0x{}", alloy::hex::encode(&h.as_slice()[..8]))
}

/// The JSONL-family label of a descriptor.
#[must_use]
pub fn family_label(family: &PoolFamily) -> &'static str {
    match family {
        PoolFamily::V2Pair => "v2",
        PoolFamily::V3 { .. } => "v3",
        PoolFamily::V4PoolManager { .. } => "v4",
    }
}

/// The terminal observe reason for a frame that surfaced no tracked pool
/// state. An unsupported DB family outranks the V4 manager (naming the
/// untracked family is the more specific cause), which outranks a genuine
/// no-candidate. The kind string rides the extract JSONL detail, never the
/// closed Prometheus reason label.
#[must_use]
pub fn empty_frame_observe_reason(descriptors: &FrameDescriptors) -> &'static str {
    if !descriptors.unsupported.is_empty() {
        "family-unsupported"
    } else if descriptors.hit_v4 {
        "v4_unsupported"
    } else {
        "no_candidate"
    }
}

/// The truthful observe label for an un-composed frame: a non-base-quote
/// fan whose normalization hop could not be priced dropped a real
/// quote-land candidate — distinct from "no work on this frame" — unless a
/// WETH-closing candidate actually solved (then the existing label, which
/// already explains the composed/sim verdict, stays).
#[must_use]
pub fn honest_observe(
    reason: &'static str,
    non_base_quote_dropped: bool,
    solved_any: bool,
) -> &'static str {
    match (reason, non_base_quote_dropped, solved_any) {
        ("no_candidate", true, false) => "non_base_quote",
        _ => reason,
    }
}

/// Does `[target, backrun]` survive as one bundle? (read/sim only.)
///
/// Endpoints disagree on the bundle envelope: the `MEVBlocker`
/// searcher-doc shape carries `params[0] = { transactions: [...] }`, while
/// mev-geth lineage nodes (reth included) parse `params[0]` as a LIST of
/// blocks — `params[0] = [{ transactions: [...] }]`. Both shapes are
/// attempted in that order; a parameter-shape or method-missing error on
/// one endpoint must never read as "the bundle reverted".
pub async fn simulate_candidate(
    sim_provider: &AlloyProvider,
    ev: &BackrunFeedEvent,
    exec: Address,
    owner: Address,
    cd: &Bytes,
) -> bool {
    let target_call = serde_json::json!({
        "from": format!("0x{}", alloy::hex::encode(ev.from)),
        "to": format!("0x{}", alloy::hex::encode(ev.to.unwrap_or_default())),
        "data": format!("0x{}", alloy::hex::encode(&ev.data)),
        "value": format!("0x{:x}", ev.value),
        "gas": format!("0x{:x}", ev.gas.max(120_000)),
        "gasPrice": format!("0x{:x}", ev.max_fee_per_gas.max(1)),
    });
    let backrun_call =
        degenbot_submission::bundle::backrun_sim_call(owner, exec, cd, 900_000, 30_000_000_000);
    let params = degenbot_submission::bundle::eth_call_many_bundle_sim_params(
        &[target_call, backrun_call],
        "latest",
    );
    // params[0] = the bundle object (doc shape); the mev-geth shape wraps
    // it in the blocks list.
    let mut blocks_list = params.clone();
    if let Some(arr) = blocks_list.as_array_mut() {
        if let Some(first) = arr.first().cloned() {
            arr[0] = serde_json::json!([first]);
        }
    }
    for attempt in [params, blocks_list] {
        if let Ok(resp) = sim_provider.make_request("eth_callMany", attempt).await {
            let txt = serde_json::to_string(&resp).unwrap_or_default();
            if !txt.contains("error") {
                return true;
            }
        }
    }
    false
}

// ─────────────────────────────────────────────────────────────────────────
// The frame's submit ordering (dispatch consumption owned by the frame)
// ─────────────────────────────────────────────────────────────────────────

/// The frame's receipt probe: one receipt read over the provider the driver
/// signs against. The submit ordering's liveness evidence, and the monitor's
/// landed check.
struct FrameProbe {
    provider: Arc<AlloyProvider>,
}

impl ReceiptProbe for FrameProbe {
    fn receipt_found(
        &self,
        tx_hash: B256,
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

/// The submit surfaces one decided frame's dispatch consumes: the node join
/// every probe and relay rides, the typed channel pair, and the signer when
/// bid mode loaded one. Grouped so the frame module owns the whole submit
/// ordering.
pub struct FrameSubmit<'a> {
    /// The node the driver signs against (the probe's provider and the raw
    /// fan-out's public fallback relay).
    pub provider: &'a Arc<AlloyProvider>,
    pub dispatcher: &'a Arc<Mutex<Dispatcher>>,
    pub nonce_lane: &'a Arc<NonceLane>,
    pub signer: Option<&'a TxSigner>,
}

/// The frame's submit ordering: liveness probe on the target → the
/// mined-target gate → the candidate build → channel dispatch → spent
/// accounting. A mined target can never be backrun — observe
/// `already_settled`. A probe failure carries no positive evidence, so it
/// never kills the bid; the `MEVBlocker` bundle's block anchoring is the
/// final backstop. Returns the post-gate decision the driver routes on.
///
/// # Panics
///
/// The candidate's path id takes the first 8 bytes of the frame hash — an
/// infallible split (`expect` pins the invariant).
#[expect(
    clippy::too_many_lines,
    reason = "the submit ordering reads top-to-bottom"
)]
pub async fn dispatch_frame(
    cfg: &BackrunConfig,
    pl: &PipelineConfig,
    ev: &BackrunFeedEvent,
    head: u64,
    artifacts: FrameArtifacts,
    submit: FrameSubmit<'_>,
    spent: &mut U256,
) -> Decision {
    let provider = submit.provider;
    let target_mined = if matches!(artifacts.decision, Decision::Bid { .. }) {
        FrameProbe {
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
            let Some(s) = submit.signer else {
                tracing::warn!("bid decided without a signer loaded - skipping");
                return Decision::Bid { bid_wei };
            };
            // Defense in depth: decide() already refuses zero bids; this
            // refusal keeps a bare-sweep bid out of the auction even if a
            // future refactor reintroduces a fallback.
            let Some(cd) = artifacts.submit_calldata.clone() else {
                tracing::warn!(
                    tx = %ev.hash,
                    "bid decided without a composed candidate - refusing"
                );
                return Decision::Bid { bid_wei };
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
                #[expect(
                    clippy::expect_used,
                    reason = "a frame hash is 32 bytes; the first 8 always split into u64"
                )]
                path_id: u64::from_be_bytes(ev.hash.0[0..8].try_into().expect("8 bytes")),
                gross_profit,
                net_profit,
                gas_used: cfg.bundle_gas_est,
                priority_fee,
                base_fee_next,
                execute_calldata: cd,
                executor_address: pl.execution.executor(),
                access_list: None,
                path_pools: HashSet::new(),
            };
            // The submission slot decides both the target and the raw
            // fan-out: the MEVBlocker arm anchors a bundle and leads the
            // fan-out with its private endpoint, the builder-relay arm
            // bundles the target's verbatim signed bytes, and the legacy
            // public arm fans the signed bytes out. The reaction machinery
            // above is identical for every arm.
            let broadcast_relays = build_broadcast_relays(cfg, provider).await;
            let Some(target) =
                bid_submission_target(cfg, ev.hash, head + 1, ev.raw_signed_tx.as_ref())
            else {
                tracing::warn!(
                    tx = %ev.hash,
                    "builder-relay slot without the target raw signed bytes - refusing"
                );
                return Decision::Bid { bid_wei };
            };
            match dispatch_and_submit(
                vec![candidate],
                submit.dispatcher,
                provider,
                s,
                Arc::new(FrameProbe {
                    provider: Arc::clone(provider),
                }),
                submit.nonce_lane,
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
            Decision::Bid { bid_wei }
        }
        Decision::Observe { reason } => {
            tracing::info!(tx = %ev.hash, reason, "observe");
            Decision::Observe { reason }
        }
        Decision::Drop { reason } => {
            tracing::debug!(tx = %ev.hash, reason, "drop");
            Decision::Drop { reason }
        }
    }
}

/// The operator's priority fee converted from the facet's gwei to wei.
#[must_use]
pub fn priority_fee_wei(cfg: &BackrunConfig) -> u128 {
    u128::from(cfg.priority_fee_gwei).saturating_mul(1_000_000_000u128)
}

/// The raw-broadcast relay list for the private-broadcast arm, private-first.
///
/// Empty when the `MEVBlocker` slot's private endpoint is unset: the submit leaf then
/// broadcasts to the read provider alone. When set, the configured private
/// endpoint leads and the read provider follows, so the private path is tried
/// first and the public provider is the fallback relay. An endpoint that cannot
/// be constructed degrades to the read-provider-only list.
pub async fn build_broadcast_relays(
    cfg: &BackrunConfig,
    provider: &Arc<AlloyProvider>,
) -> Vec<Arc<AlloyProvider>> {
    let urls = cfg.submission.raw_relay_urls();
    if urls.is_empty() {
        return Vec::new();
    }
    let mut relays = Vec::new();
    for url in &urls {
        match AlloyProvider::new(url, DEFAULT_MAX_RETRIES).await {
            Ok(relay) => relays.push(Arc::new(relay)),
            Err(error) => {
                tracing::warn!(
                    %error,
                    url,
                    "raw broadcast relay build failed - skipped"
                );
            }
        }
    }
    // The read provider is the public fallback relay on every raw fan-out.
    relays.push(Arc::clone(provider));
    relays
}

/// The bid's submission target, bound by the composition's slot: the
/// `MEVBlocker` arm anchors a bundle on its searcher WebSocket, the
/// builder-relay arm bundles the target's verbatim signed bytes with the
/// signed backrun, and the legacy public arm fans the signed bytes out.
///
/// `Some(None)` semantics are folded into the `Option` return: a
/// builder-relay slot whose frame lacks the target's raw signed bytes has no
/// bundle to send (the caller logs a quiet skip — it is never broadcast raw,
/// which would leak the backrun without its target).
#[must_use]
pub fn bid_submission_target(
    cfg: &BackrunConfig,
    target_tx_hash: B256,
    block_number: u64,
    target_raw: Option<&alloy::primitives::Bytes>,
) -> Option<SubmissionTarget> {
    match &cfg.submission {
        crate::backrun::SubmissionSlot::Mevblocker { bundle_url, .. } => {
            Some(SubmissionTarget::Bundle(BundleTarget {
                stream_url: bundle_url.clone(),
                target_tx_hash,
                block_number,
            }))
        }
        crate::backrun::SubmissionSlot::BuilderRelay { relays } => {
            Some(SubmissionTarget::BuilderRelay {
                relays: relays.clone(),
                target_raw: target_raw?.clone(),
                block_number,
            })
        }
        crate::backrun::SubmissionSlot::PublicFanOut { .. } => Some(SubmissionTarget::Public),
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The frame pipeline (one feed frame, end to end)
// ─────────────────────────────────────────────────────────────────────────

/// Map a typed replay failure to the truthful observe reason + the JSONL
/// evidence the trace carries. Single source for the histogram split.
#[must_use]
pub fn replay_observe_reason(e: &ReplayFrameError) -> (&'static str, serde_json::Value) {
    match e {
        ReplayFrameError::GapPending { claimed, expected } => (
            "gap_pending",
            serde_json::json!({"claimed_nonce": claimed, "expected_nonce": expected}),
        ),
        ReplayFrameError::AlreadySettled { frame, parent } => (
            "already_settled",
            serde_json::json!({"frame_nonce": frame, "parent_nonce": parent}),
        ),
        ReplayFrameError::MalformedTransaction { raw } => {
            ("malformed_transaction", serde_json::json!({"detail": raw}))
        }
        ReplayFrameError::Mispriced { raw } => {
            ("mispriced_transaction", serde_json::json!({"detail": raw}))
        }
        ReplayFrameError::Other { .. } => ("replay_failed", serde_json::json!({})),
    }
}

/// Map a predecessor-sourced sequence failure to the rescue router's observe
/// reason. A structurally unrunnable predecessor (`MalformedTransaction` as
/// fetched at this shape) goes to the structural pool-pred lane; every
/// retryable class — RPC/validation (`Other`), mispricing, gap, stale nonce —
/// is transient (retried next head, never a permanent guard).
#[must_use]
pub fn predecessor_observe_reason(e: &ReplayFrameError) -> &'static str {
    match e {
        ReplayFrameError::MalformedTransaction { .. } => "predecessor_malformed",
        ReplayFrameError::GapPending { .. }
        | ReplayFrameError::AlreadySettled { .. }
        | ReplayFrameError::Mispriced { .. }
        | ReplayFrameError::Other { .. } => "predecessor_replay_failed",
    }
}

/// Process ONE feed frame: replay → extract → admit → discover → evaluate →
/// compose → sim gate → decision. The bin drives this per drained event and
/// owns dispatch/submit; the returned [`FrameArtifacts::decision`] already
/// carries the truthful observe reason when nothing composed.
///
/// The feed path replays against the head state with an empty predecessor
/// prefix — [`process_frame_with_prefix`] is the rescue path that executes a
/// hydrated unmined queue first.
///
/// `handle` may be `None` when the per-block replay stack failed to build
/// (no ambient multi-threaded runtime, RPC outage) — the frame observes
/// `replay_unavailable` rather than being classified or silently dropped.
#[expect(
    clippy::too_many_arguments,
    reason = "the frame takes the runtime surfaces it needs"
)]
pub async fn process_frame<S: PendingTxReaction>(
    strategy: &mut S,
    ctx: &mut MarketContext,
    provider: &AlloyProvider,
    sim_provider: &AlloyProvider,
    knobs: &BackrunConfig,
    pl: &PipelineConfig,
    handle: &mut Option<BlockSimHandle<'_>>,
    ev: &BackrunFeedEvent,
    head: u64,
    spent: U256,
) -> FrameArtifacts {
    process_frame_with_prefix(
        strategy,
        ctx,
        provider,
        sim_provider,
        knobs,
        pl,
        handle,
        ev,
        &[],
        head,
        spent,
    )
    .await
}

/// As [`process_frame`], with a hydrated unmined predecessor prefix. A
/// non-empty prefix runs each predecessor through the frame-replay pipeline
/// first (committing its settled state into a local overlay), so the frame
/// replays over the end state of its fake-mined ancestors; an empty prefix is
/// exactly [`process_frame`]'s feed path.
#[expect(clippy::too_many_lines, reason = "one frame reads top-to-bottom")]
#[expect(
    clippy::too_many_arguments,
    reason = "the frame takes the runtime surfaces it needs"
)]
pub async fn process_frame_with_prefix<S: PendingTxReaction>(
    strategy: &mut S,
    ctx: &mut MarketContext,
    provider: &AlloyProvider,
    sim_provider: &AlloyProvider,
    knobs: &BackrunConfig,
    pl: &PipelineConfig,
    handle: &mut Option<BlockSimHandle<'_>>,
    ev: &BackrunFeedEvent,
    prefix: &[ReplayableTx],
    head: u64,
    spent: U256,
) -> FrameArtifacts {
    let tx_hex = format!("0x{}", alloy::hex::encode(ev.hash));
    let mut stages = StageTimings::default();

    // ── stage: replay (the classifier never participates) ────────────────
    let Some(handle_ref) = handle.as_mut() else {
        return FrameArtifacts::observe("replay_unavailable", stages);
    };
    let Some(scratch) = handle_ref.scratch_evm() else {
        return FrameArtifacts::observe("replay_unavailable", stages);
    };
    let t = Instant::now();
    let replayable = ReplayableTx {
        from: ev.from,
        to: ev.to,
        value: ev.value,
        data: ev.data.clone(),
        gas_limit: ev.gas,
        max_fee_per_gas: ev.max_fee_per_gas,
        max_priority_fee_per_gas: ev.max_priority_fee_per_gas,
        nonce: ev.nonce,
    };
    let replayed = if prefix.is_empty() {
        scratch
            .replay(&replayable)
            .map(|outcome| (outcome, Vec::new()))
            .map_err(SequenceReplayError::Frame)
    } else {
        scratch
            .replay_sequence(prefix, &replayable)
            .map(|sequence| (sequence.frame, sequence.predecessors))
    };
    let (outcome, predecessor_statuses) = match replayed {
        Ok(replayed) => replayed,
        Err(e) => {
            stages.replay_us = u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX);
            // A predecessor's failure is never the frame's: the frame never
            // ran, so its quarantine life cannot be resolved on this evidence
            // (no `replay_frame_error`, hence no inline park). The observe
            // reason carries the class the rescue router owns.
            let (reason, mut evidence, frame_error) = match &e {
                SequenceReplayError::Frame(frame_error) => {
                    let (reason, extra) = replay_observe_reason(frame_error);
                    (reason, extra, Some(frame_error.clone()))
                }
                SequenceReplayError::Predecessor { nonce, source } => (
                    predecessor_observe_reason(source),
                    serde_json::json!({
                        "predecessor_nonce": nonce,
                        "detail": source.to_string(),
                    }),
                    None,
                ),
            };
            evidence["error"] = serde_json::Value::String(e.to_string());
            let mut payload = serde_json::json!({"tx": tx_hex, "prefix_len": prefix.len()});
            if let (Some(dst), Some(src)) = (payload.as_object_mut(), evidence.as_object_mut()) {
                dst.append(src);
            }
            payload["observe_reason"] = serde_json::Value::String(reason.to_string());
            trace_jsonl("replay", payload);
            return FrameArtifacts::observe_with_replay_error(reason, stages, frame_error);
        }
    };
    stages.replay_us = u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX);
    trace_jsonl(
        "replay",
        serde_json::json!({
            "tx": tx_hex,
            "wall_us": u64::try_from(outcome.wall.as_micros()).unwrap_or(u64::MAX),
            "rpc_reads": outcome.rpc_reads,
            "touched": outcome.touched.len(),
            "prefix_len": prefix.len(),
            "predecessor_statuses": predecessor_statuses
                .iter()
                .map(|status| status.label())
                .collect::<Vec<_>>(),
            "base_fee": match outcome.base_fee_source {
                degenbot_simulation::sim::evm::frame_replay::BaseFeeSource::Projected => "projected",
                degenbot_simulation::sim::evm::frame_replay::BaseFeeSource::DisabledFallback => "disabled_fallback",
            },
            "status": match outcome.status {
                ReplayStatus::Success => "success",
                ReplayStatus::Reverted => "reverted",
                ReplayStatus::Halted => "halted",
            },
        }),
    );
    if !matches!(outcome.status, ReplayStatus::Success) {
        return FrameArtifacts::observe("reverted", stages);
    }

    // ── stage: extract (journal post-states; descriptors from the index) ──
    let t = Instant::now();
    let descriptors = build_descriptors(ctx.index(), &outcome.touched, &pl.execution);
    if !descriptors.unsupported.is_empty() {
        // Loud, and legible alongside the closed `family-unsupported` reason:
        // the kind string never becomes a metric label, so it is named here
        // and in the extract JSONL detail.
        tracing::info!(
            tx = %tx_hex,
            kinds = ?descriptors
                .unsupported
                .iter()
                .map(|(_, kind)| kind.as_str())
                .collect::<Vec<_>>(),
            "frame touched an unsupported pool family"
        );
    }
    let extracted = extract_pool_post_states(&outcome, &descriptors.by_address);
    stages.extract_us = u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX);
    let all_unsupported = !extracted.is_empty()
        && extracted
            .iter()
            .all(|s| matches!(s.kind, PoolPostKind::Unsupported));
    // A mixed frame carries typed state AND an unadmitted V4 half: count it in
    // the frame evidence so the drop is legible alongside the per-state
    // `extract_skip` witness `admit_extracted` emits.
    let v4_half_unobserved = if all_unsupported {
        0
    } else {
        extracted
            .iter()
            .filter(|s| matches!(s.kind, PoolPostKind::Unsupported))
            .count()
    };
    trace_jsonl(
        "extract",
        serde_json::json!({
            "tx": tx_hex,
            "families": extracted
                .iter()
                .map(|s| family_label(&s.family))
                .collect::<Vec<_>>(),
            "digests": extracted.iter().map(state_digest).collect::<Vec<_>>(),
            "v4_half_unobserved": v4_half_unobserved,
            // The kind string rides the JSONL detail: the Prometheus reason
            // label is the closed `family-unsupported` bucket.
            "unsupported_families": descriptors
                .unsupported
                .iter()
                .map(|(addr, kind)| serde_json::json!({
                    "address": format!("0x{}", alloy::hex::encode(addr)),
                    "kind": kind,
                }))
                .collect::<Vec<_>>(),
        }),
    );
    if all_unsupported || extracted.is_empty() {
        return FrameArtifacts::observe(empty_frame_observe_reason(&descriptors), stages);
    }

    // The frame's stage context: the per-driver config and per-frame identity
    // every stage reads, plus the evidence buffer the stages append to and
    // this module renders after each stage — no strategy code touches the
    // offline-review capture.
    let mut stage_trace = StageTrace::default();
    let mut cx = FrameContext {
        pl,
        knobs,
        head,
        trace_tx: &tx_hex,
        trace: &mut stage_trace,
    };

    // ── stage: admission (fresh scope; replayed state verbatim) ──────────
    let t = Instant::now();
    let mut solver = BackrunSolver::new();
    let affected = strategy
        .admit(&mut cx, &*ctx, &mut solver, &extracted)
        .await;
    stages.admit_us = u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX);
    cx.trace.render(&tx_hex);
    if S::affected_is_empty(&affected) {
        return FrameArtifacts::observe("no_candidate", stages);
    }

    // ── stage: discovery (per-frame; strategy-owned) ─────────────────────
    let t = Instant::now();
    let intents = strategy
        .discover(&mut cx, &*ctx, &mut solver, scratch, provider, &affected)
        .await;
    stages.discover_us = u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX);
    cx.trace.render(&tx_hex);

    // ── stage: evaluate (envelope-gated; strategy-owned) ─────────────────
    let t = Instant::now();
    let evaluated = strategy.evaluate(&mut cx, &mut solver, intents);
    stages.solve_us = u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX);
    cx.trace.render(&tx_hex);

    // ── stage: compose (strategy-owned; the driver sims the result) ──────
    let t = Instant::now();
    let composed = strategy.compose(&mut cx, &evaluated);
    stages.compose_us = u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX);
    cx.trace.render(&tx_hex);

    // ── stage: the driver-owned bundle sim gate ──────────────────────────
    let mut sim_ok = false;
    if let Some(c) = composed.as_ref() {
        if pl.fixture_mode {
            stages.sim_us = 0;
            trace_jsonl(
                "sim",
                serde_json::json!({
                    "tx": tx_hex,
                    "status": "skipped",
                    "reason": "sim_skipped_fixture_mode",
                }),
            );
        } else {
            let t = Instant::now();
            sim_ok = simulate_candidate(
                sim_provider,
                ev,
                pl.execution.executor(),
                pl.owner,
                &c.sim_calldata,
            )
            .await;
            stages.sim_us = u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX);
            trace_jsonl(
                "composed",
                serde_json::json!({
                    "tx": tx_hex,
                    // u128 wei values exceed serde_json's numeric range -
                    // string them (same as the solve event's profit field).
                    "profit": c.profit_wei.to_string(),
                    "input": c.optimal_input_wei.to_string(),
                    "calldata": format!("0x{}", alloy::hex::encode(&c.sim_calldata)),
                    "sim": if sim_ok { "passed" } else { "failed" },
                }),
            );
        }
    }

    // ── stage: decide (strategy-owned; carries the truthful observe) ─────
    let gate = GateInput {
        evaluated: &evaluated,
        composed: composed.as_ref(),
        sim_ok,
        spent,
    };
    let decided = strategy.decide(&mut cx, &gate);
    cx.trace.render(&tx_hex);
    FrameArtifacts {
        decision: decided.decision,
        requested_bid: decided.requested_bid,
        submit_calldata: decided.submit_calldata,
        economics: decided.economics,
        stages,
        replay_frame_error: None,
    }
}

#[cfg(test)]
mod tests {
    use crate::backrun_strategy::net_bid;

    /// The live defect, pinned: the first two landed bids tendered a ~110
    /// gwei gross while the receipts show the wallet burning ~527 gwei of
    /// gas (tx 0xd41a1c35 / 0x8603039d). Gross below gas must never bid.
    #[test]
    fn net_bid_rejects_gross_below_wallet_gas() {
        let receipt_gas_cost = 248_213u128 * 2_125_376_207u128;
        assert!(net_bid(110_065_920_704, receipt_gas_cost, 9_800, u128::MAX).is_none());
        // Even a generous gross of exactly the keep threshold does not bid.
        assert!(net_bid(receipt_gas_cost, receipt_gas_cost, 9_800, u128::MAX).is_none());
    }

    #[test]
    #[expect(clippy::expect_used, reason = "literals are the spec")]
    fn net_bid_sizes_bribe_from_wallet_surplus() {
        let gas_cost = 600_000_000_000u128; // 600 gwei of wallet gas
                                            // Gross 10x the gas burn: the bribe takes everything above the
                                            // keep (gas + 5%), so the wallet recovers custody worth ~5% of
                                            // nothing and the builder gets the rest.
        let nb = net_bid(6_000_000_000_000, gas_cost, 9_800, u128::MAX).expect("viable");
        // keep = 630 gwei (gas + 5%), bid takes the remaining 5_370 gwei.
        assert_eq!(nb.bribe_bips, 8_950);
        assert_eq!(
            nb.bid_wei,
            alloy::primitives::U256::from(5_370_000_000_000u128)
        );
        assert_eq!(
            nb.keep_wei,
            alloy::primitives::U256::from(630_000_000_000u128)
        );
    }

    #[test]
    #[expect(clippy::expect_used, reason = "literals are the spec")]
    fn net_bid_caps_bips_at_competitiveness_ceiling() {
        // Gross far above gas: without the 9800 bips ceiling the bribe
        // would consume everything; the ceiling keeps 2% in custody.
        let nb =
            net_bid(1_000_000_000_000_000_000, 1_000_000_000, 9_800, u128::MAX).expect("viable");
        assert_eq!(nb.bribe_bips, 9_800);
        assert_eq!(
            nb.keep_wei,
            alloy::primitives::U256::from(20_000_000_000_000_000u128)
        );
    }

    #[test]
    #[expect(clippy::expect_used, reason = "literals are the spec")]
    fn net_bid_caps_bid_at_bundle_cap() {
        let nb = net_bid(
            1_000_000_000_000_000_000,
            1_000_000_000,
            10_000,
            500_000_000_000_000,
        )
        .expect("viable at 10000 bips");
        // Floor to U256 arithmetic: bid_wei must never exceed the cap.
        assert!(nb.bid_wei <= alloy::primitives::U256::from(500_000_000_000_000u128));
    }

    /// The extract JSONL `families` array speaks the bare family name, so the
    /// V4 manager arm reads `v4` alongside `v2`/`v3` — not a manager-specific
    /// tag that would fork the offline-review vocabulary.
    #[test]
    fn family_label_names_the_v4_family_as_v4() {
        use super::{family_label, PoolFamily, V4PoolSet};

        let family = PoolFamily::V4PoolManager {
            pools: V4PoolSet::new(Vec::new()),
        };
        assert_eq!(family_label(&family), "v4");
    }

    // ── The `eth_callMany` bundle gate ──────────────────────────────────
    //
    // `simulate_candidate` is the only place backrun candidates pass or
    // fail, and its failure mode is silent: an endpoint that implements
    // neither envelope drops every candidate with no log. These tests drive
    // the function against a scripted JSON-RPC node on a real unix socket --
    // the same transport approach as `backrun_driver::tests::ipc_node` --
    // and assert on the request shapes the provider actually sent.

    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    use alloy::primitives::{Address, Bytes, B256, U256};
    use degenbot_rpc::backrun_feed::BackrunFeedEvent;
    use degenbot_rpc::provider::{AlloyProvider, DEFAULT_MAX_RETRIES};

    use super::simulate_candidate;

    /// The `params` of every request the scripted node received.
    type RequestLog = Arc<Mutex<Vec<serde_json::Value>>>;

    /// A JSON-RPC endpoint on a real unix socket. It records the `params` of
    /// every request and answers `eth_callMany` through `reply`. `Ok` is the
    /// JSON-RPC `result`, `Err` is `(code, message)`.
    ///
    /// Follows `backrun_driver::tests::ipc_node`: the IPC client frames bare
    /// JSON values with no delimiter, so each buffer is parsed as soon as it
    /// holds one complete value. A mock transport would prove nothing here
    /// because the emitted request shape is the subject, so the requests are
    /// read off a socket.
    #[expect(clippy::expect_used, reason = "test fixtures fail loudly")]
    fn recording_ipc_node(
        dir: &std::path::Path,
        reply: impl Fn(&serde_json::Value) -> Result<serde_json::Value, (i64, String)>
            + Send
            + Sync
            + 'static,
    ) -> (std::path::PathBuf, RequestLog) {
        let path = dir.join("node.ipc");
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind the IPC socket");
        let log: RequestLog = Arc::new(Mutex::new(Vec::new()));
        let reply = Arc::new(reply);
        let thread_log = Arc::clone(&log);
        std::thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let log = Arc::clone(&thread_log);
                let reply = Arc::clone(&reply);
                std::thread::spawn(move || {
                    let mut pending = String::new();
                    let mut chunk = [0_u8; 4096];
                    loop {
                        let Ok(read) = stream.read(&mut chunk) else {
                            return;
                        };
                        if read == 0 {
                            return;
                        }
                        pending.push_str(&String::from_utf8_lossy(&chunk[..read]));
                        loop {
                            let Ok(request) =
                                serde_json::from_str::<serde_json::Value>(pending.trim())
                            else {
                                break;
                            };
                            pending.clear();
                            let id = request
                                .get("id")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null);
                            let params = request
                                .get("params")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null);
                            log.lock()
                                .expect("the recording lock is not poisoned")
                                .push(params.clone());
                            let response = match reply(&params) {
                                Ok(result) => {
                                    serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result})
                                }
                                Err((code, message)) => serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": id,
                                    "error": {"code": code, "message": message},
                                }),
                            };
                            if write!(stream, "{response}").is_err() || stream.flush().is_err() {
                                return;
                            }
                        }
                    }
                });
            }
        });
        (path, log)
    }

    /// The searcher-doc shape: `params[0]` is the bundle object.
    fn is_doc_shape(params: &serde_json::Value) -> bool {
        params
            .get(0)
            .and_then(|p0| p0.get("transactions"))
            .is_some_and(serde_json::Value::is_array)
    }

    /// The mev-geth shape reth parses: `params[0]` is a LIST whose first
    /// element is the bundle object.
    fn is_mev_geth_shape(params: &serde_json::Value) -> bool {
        params
            .get(0)
            .and_then(serde_json::Value::as_array)
            .and_then(|blocks| blocks.first())
            .and_then(|block| block.get("transactions"))
            .is_some_and(serde_json::Value::is_array)
    }

    /// A minimal frame: the gate reads only `from`/`to`/`data`/`value`/`gas`/
    /// `max_fee_per_gas`.
    fn sim_frame() -> BackrunFeedEvent {
        BackrunFeedEvent {
            chain_id: 1,
            from: Address::repeat_byte(0x11),
            to: Some(Address::repeat_byte(0x22)),
            value: U256::from(1_u64),
            data: Bytes::from(vec![0xaa, 0xbb]),
            gas: 200_000,
            max_fee_per_gas: 1_000_000_000,
            max_priority_fee_per_gas: 1_000_000_000,
            nonce: 7,
            hash: B256::repeat_byte(0x33),
            access_list: serde_json::json!([]),
            tx_type: 2,
            received_unix_ms: 0,
            raw_signed_tx: None,
        }
    }

    /// Drive the gate against one scripted endpoint and return whether it
    /// admitted the bundle plus the exact request params it sent.
    #[expect(clippy::expect_used, reason = "test fixtures fail loudly")]
    async fn run_gate(
        dir: &std::path::Path,
        reply: impl Fn(&serde_json::Value) -> Result<serde_json::Value, (i64, String)>
            + Send
            + Sync
            + 'static,
    ) -> (bool, Vec<serde_json::Value>) {
        let (path, log) = recording_ipc_node(dir, reply);
        let provider =
            AlloyProvider::new(&format!("ipc://{}", path.display()), DEFAULT_MAX_RETRIES)
                .await
                .expect("a scripted socket builds a provider");
        let admitted = simulate_candidate(
            &provider,
            &sim_frame(),
            Address::repeat_byte(0x44),
            Address::repeat_byte(0x55),
            &Bytes::from(vec![0x01, 0x02]),
        )
        .await;
        let requests = log
            .lock()
            .expect("the recording lock is not poisoned")
            .clone();
        (admitted, requests)
    }

    /// The fallback is what makes reth work: an endpoint that rejects the
    /// searcher-doc shape with -32602 and accepts the mev-geth block-list
    /// shape admits the bundle. Both requests must have been sent, and the
    /// second must be the mev-geth shape.
    #[tokio::test]
    #[expect(clippy::expect_used, reason = "test assertions fail loudly")]
    async fn the_mev_geth_fallback_admits_where_the_doc_shape_is_rejected() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (admitted, requests) = run_gate(dir.path(), |params| {
            if is_doc_shape(params) {
                Err((-32602, "invalid type: map, expected a sequence".to_string()))
            } else {
                Ok(serde_json::json!([{"success": true, "returnData": "0x"}]))
            }
        })
        .await;

        assert!(
            admitted,
            "reth admits the bundle through the mev-geth fallback: {requests:?}"
        );
        assert_eq!(
            requests.len(),
            2,
            "the doc shape was tried first, then the fallback: {requests:?}"
        );
        assert!(
            is_doc_shape(&requests[0]),
            "the first attempt is the searcher-doc shape: {:?}",
            requests[0]
        );
        assert!(
            is_mev_geth_shape(&requests[1]),
            "the second attempt wraps the bundle in the mev-geth blocks list: {:?}",
            requests[1]
        );
    }

    /// Fail closed, loudly in the assertion: an endpoint that implements
    /// neither envelope (`-32601 Method not found`, as anvil v1.7.1 answers)
    /// drops the candidate. The gate returns `false` rather than assuming the
    /// bundle reverted; both shapes must have been attempted.
    #[tokio::test]
    #[expect(clippy::expect_used, reason = "test assertions fail loudly")]
    async fn a_node_without_the_sim_method_fails_closed_after_both_shapes() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (admitted, requests) = run_gate(dir.path(), |_params| {
            Err((-32601, "Method not found".to_string()))
        })
        .await;

        assert!(
            !admitted,
            "DELIBERATE, not a bug: an endpoint that implements neither \
             eth_callMany shape must drop the candidate -- the gate fails \
             closed instead of misreading a method-missing error as a revert \
             (this is the anvil v1.7.1 case)"
        );
        assert_eq!(
            requests.len(),
            2,
            "both shapes were attempted before the gate gave up: {requests:?}"
        );
        assert!(
            is_doc_shape(&requests[0]) && is_mev_geth_shape(&requests[1]),
            "the two attempts are the doc shape then the mev-geth shape: {requests:?}"
        );
    }

    /// Order is pinned: a doc-shape endpoint that accepts on the first
    /// attempt must receive EXACTLY ONE request, so a future "always try
    /// both" change cannot double sim traffic.
    #[tokio::test]
    #[expect(clippy::expect_used, reason = "test assertions fail loudly")]
    async fn a_doc_shape_endpoint_is_asked_exactly_once() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (admitted, requests) = run_gate(dir.path(), |_params| {
            Ok(serde_json::json!([{"success": true, "returnData": "0x"}]))
        })
        .await;

        assert!(admitted, "the doc shape admits the bundle: {requests:?}");
        assert_eq!(
            requests.len(),
            1,
            "a first-attempt success must stop the loop, not re-send the bundle: {requests:?}"
        );
        assert!(
            is_doc_shape(&requests[0]),
            "the single request is the doc shape: {:?}",
            requests[0]
        );
    }

    /// A shape error is not a revert, and a revert is not success. The doc
    /// shape is rejected with -32602 (a shape error, as reth answers) and the
    /// mev-geth shape returns a well-formed REVERT result: the gate must
    /// return `false`. This is a distinct branch from the method-missing
    /// case -- attempt two returns `Ok`, so a change that treated any parsed
    /// response as a pass would ship green there but fail here.
    #[tokio::test]
    #[expect(clippy::expect_used, reason = "test assertions fail loudly")]
    async fn a_shape_error_is_not_a_revert_and_a_revert_is_not_success() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (admitted, requests) = run_gate(dir.path(), |params| {
            if is_doc_shape(params) {
                Err((-32602, "invalid type: map, expected a sequence".to_string()))
            } else {
                Ok(serde_json::json!([
                    {"success": false, "error": "execution reverted", "returnData": "0x"}
                ]))
            }
        })
        .await;

        assert!(
            !admitted,
            "a reverted bundle never passes the gate: {requests:?}"
        );
        assert_eq!(
            requests.len(),
            2,
            "the doc shape was rejected, then the fallback served the revert: {requests:?}"
        );
        assert!(
            is_mev_geth_shape(&requests[1]),
            "the revert came back on the mev-geth shape, distinct from the \
             method-missing case: {:?}",
            requests[1]
        );
    }
}
