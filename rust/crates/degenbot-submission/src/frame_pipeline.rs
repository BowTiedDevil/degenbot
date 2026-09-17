//! The frame pipeline : pending-tx frame → frame-replay seam →
//! journal-pool extraction → workspace admission → discovery fan → solve →
//! compose → `eth_callMany` gate → bid decision.
//!
//! # Cache ownership split (the task's hook, resolved here)
//!
//! Two lifetimes meet per frame and must NOT be conflated:
//!
//! - **The strategy runtime** ([`StrategyRuntime`]) survives frames: the
//!   DB-backed connector index, the token id/address joins, the cross-block
//!   warm-code cache owner, and the per-block replay handle live here.
//!   These caches are process-scoped, exactly like the frame stream they
//!   serve — a per-frame refill of the token joins would re-pay a DB query
//!   per pool per frame and forfeit the index's memoized depth rankings.
//! - **The planning workspace scope** dies per frame: [`SidecarSolver`]
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
//! in the shared RPC counter), and raw RPC (`getReserves`, the V3 slot0/tick
//! ladder) is only the fallback where the view read fails or returns no
//! usable state. Nothing is fabricated: a zero/failed read skips or falls
//! back, never guesses.
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
//!    [`PipelineConfig::gas_floor_wei`]. [`SidecarSolver::evaluate`] gates
//!    on `P`'s rigorous upper bound (the solver's profit-envelope gate)
//!    BEFORE the bribe: a candidate must clear its gas cost from true
//!    profit. The residual we keep after the bribe (`P − B`, the 2% of the
//!    98% ladder) funds nothing further — the executor's non-zero
//!    check-mode seatbelts the landed tx's worst case to gas.
//! 2. **Bid ladder** `B = floor(P × bribe_bips / 10_000)` (98% default).
//!    The builder's share enters the decision AT MOST ONCE, at
//!    [`bid_from_profit`] — the only site that computes the bid from
//!    profit. [`crate::bundle`]'s composed executor config echoes the SAME
//!    `bribe_bips` for the on-chain coinbase payout (one share decision,
//!    not a second gate), and [`decide`] only caps `B` against the
//!    budget/bundle ceilings.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use alloy::primitives::{address, Address, Bytes, U256};
use degenbot_bot::bot_core::SimAnchorState;
use degenbot_bot::sidecar::{decide, Decision, SidecarConfig};
use degenbot_bot::sidecar_engine::{
    build_candidate_calldata, LaneCandidate, LaneFamily, SidecarHopRef, SidecarSolver,
    SidecarV2Pool,
};
use degenbot_bot::sidecar_paths::V2ConnectorIndex;
// The anchored touched-set discovery walker (startup graph + per-frame
// cycles) and the pathfinding leaf it runs on.
use crate::anchored_dfs::{
    resolve_hop, AnchorPool, AnchoredGraph, DfsCycle, DiscoveryBudget, ResolvedHop,
};
use degenbot_db::connection::DegenbotDb;
use degenbot_decoders::target_class::TargetClass;
use degenbot_pathfinding::PoolKind;
use degenbot_pools::v3_state::ClSlotLayout;
use degenbot_pools::{slot_layout, v3_storage_slots, TickInfo};
use degenbot_rpc::backrun_feed::BackrunFeedEvent;
use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm::frame_replay::{ReplayStatus, ReplayableTx, ScratchEvm};
use degenbot_simulation::sim::evm::journal_pools::{
    extract_pool_post_states, PoolFamily, PoolPostKind, PoolPostState, TypedPoolPost,
};
use degenbot_simulation::sim::evm::{read_view_word, BlockSimHandle, ScratchDb};
use degenbot_simulation::{SimulationOverrideParams, WarmCodeCacheInner};
use hashbrown::HashMap as HbMap;
use parking_lot::RwLock;

/// The Uniswap V4 `PoolManager` singleton — the descriptor key for the
/// explicitly-unsupported V4 family (frames touching ONLY this observe
/// `v4_unsupported` instead of guessing a decode).
const V4_POOL_MANAGER: Address = address!("000000000004444c5dc75cb358380d2e3de08a90");

/// The canonical mainnet WETH address — the base (settlement) quote.
pub const WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
/// The canonical mainnet USDC address — a supported per-frame quote.
pub const USDC: Address = address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
/// The canonical mainnet USDT address — a supported per-frame quote.
pub const USDT: Address = address!("dac17f958d2ee523a2206206994597c13d831ec7");
/// The per-frame quote set: every affected pool may settle its drift cycle
/// in any of these, selected by the touched token's edge degree. Outside
/// the set, a pool keeps its truthful no-quote skip.
pub const SUPPORTED_QUOTES: [Address; 3] = [WETH, USDC, USDT];

/// The per-frame discovery slice: how long the anchored walker may grind on
/// one frame before its cancel flag fires. The slice bounds only the walker
/// (its churn is checked between yields) — never the solve/sim stages.
const FRAME_DISCOVERY_SLICE: Duration = Duration::from_millis(2);

// ─────────────────────────────────────────────────────────────────────────
// Offline-review capture (moved verbatim from the bin: capture failures
// never disturb the hot loop).
// ─────────────────────────────────────────────────────────────────────────

/// Append one JSON line to the offline-review capture (`SIDECAR_TRACE_JSONL`,
/// best-effort).
pub fn trace_jsonl(kind: &str, mut v: serde_json::Value) {
    use std::io::Write;
    let Ok(path) = std::env::var("SIDECAR_TRACE_JSONL") else {
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
// Strategy runtime (frame-surviving caches)
// ─────────────────────────────────────────────────────────────────────────

/// The frame-surviving half of the ownership split (see the module doc).
pub struct StrategyRuntime {
    /// The chain the connector index + DB id joins are keyed on.
    pub chain_id: i64,
    /// DB-backed connector index (V2 + V3 edges, depth-ranked). `None`
    /// keeps the discovery fan shut (frames observe; connectors are never
    /// guessed).
    pub index: Option<V2ConnectorIndex>,
    /// The DB handle the index was loaded from (token id/address joins).
    pub db: Option<DegenbotDb>,
    /// The discovery fan-out cap (`SIDECAR_CONNECTORS`).
    pub connector_cap: usize,
    /// Cross-block warm bytecode/account cache owner, shared into every
    /// per-block replay handle.
    pub warm_cache: Arc<RwLock<WarmCodeCacheInner>>,
    /// The startup-built discovery graph over the connector index's edge
    /// set (V2 + V3; `None` exactly when the index is `None` — the walker
    /// lane stays shut with it).
    pub dfs: Option<AnchoredGraph>,
    token_ids: Mutex<HashMap<Address, u64>>,
    token_addrs: Mutex<HashMap<u64, Address>>,
}

impl StrategyRuntime {
    #[must_use]
    pub fn new(
        chain_id: i64,
        index: Option<V2ConnectorIndex>,
        db: Option<DegenbotDb>,
        connector_cap: usize,
    ) -> Self {
        Self {
            chain_id,
            // Built from the index BEFORE it moves: one startup graph pass.
            dfs: index.as_ref().map(AnchoredGraph::from_connector_index),
            index,
            db,
            connector_cap,
            warm_cache: WarmCodeCacheInner::shared_default(),
            token_ids: Mutex::new(HashMap::new()),
            token_addrs: Mutex::new(HashMap::new()),
        }
    }

    /// DB id for a token address (memoized across frames). `None` when the
    /// token is absent from the workspace DB or no DB is open.
    #[must_use]
    pub fn token_id(&self, addr: Address) -> Option<u64> {
        let mut ids = self
            .token_ids
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(hit) = ids.get(&addr).copied() {
            return Some(hit);
        }
        let db = self.db.as_ref()?;
        let found = db
            .fetch_token_ids_by_address(self.chain_id, &[addr])
            .ok()?
            .into_iter()
            .next()
            .map(|(_, id)| id)?;
        ids.insert(addr, found);
        Some(found)
    }

    /// Address for a token DB id (memoized across frames).
    #[must_use]
    pub fn token_addr(&self, id: u64) -> Option<Address> {
        let mut addrs = self
            .token_addrs
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(hit) = addrs.get(&id).copied() {
            return Some(hit);
        }
        let id64 = i64::try_from(id).ok()?;
        let row = self.db.as_ref()?.fetch_token_by_id(id64).ok()??;
        let addr = row.address;
        addrs.insert(id, addr);
        Some(addr)
    }

    /// The WETH token id (the discovery quote side). `None` without a DB.
    #[must_use]
    pub fn weth_id(&self) -> Option<u64> {
        self.token_id(WETH)
    }
}

/// The per-frame strategy configuration the bin owns (executor/owner
/// identity + economics).
#[derive(Debug, Clone, Copy)]
pub struct PipelineConfig {
    pub exec: Address,
    pub owner: Address,
    /// The builder's bribe share of TRUE profit (bips of `10_000`).
    pub bribe_bips: u16,
    /// The gas floor the envelope gate evaluates at (wei).
    pub gas_floor_wei: U256,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameArtifacts {
    pub decision: Decision,
    pub requested_bid: U256,
    pub submit_calldata: Option<Bytes>,
    pub stages: StageTimings,
}

impl FrameArtifacts {
    fn observe(reason: &'static str, stages: StageTimings) -> Self {
        // The frame never composed a bid-able artifact: observe it with the
        // reason the STAGE produced, never a generic sim-gate label.
        Self {
            decision: Decision::Observe { reason },
            requested_bid: U256::ZERO,
            submit_calldata: None,
            stages,
        }
    }
}

/// The bid ladder, at the ONE site that turns profit into a bid: the
/// builder's truncated share of the exact-solve profit. The envelope gate
/// above consumed TRUE profit against the gas floor BEFORE this; the
/// composed executor config echoes the same `bribe_bips` for the on-chain
/// payout (one share decision — no second site scales profit again).
#[must_use]
pub fn bid_from_profit(profit: u128, bribe_bips: u16) -> U256 {
    U256::from(profit).saturating_mul(U256::from(u64::from(bribe_bips))) / U256::from(10_000u16)
}

// ─────────────────────────────────────────────────────────────────────────
// Per-block replay handle
// ─────────────────────────────────────────────────────────────────────────

/// Build the per-block frame-replay handle pinning `head`: the scratch
/// stack reads `BlockId::Number(head)` and its frames run in the NEXT
/// block's env. State overrides are the ZERO set — a foreign frame must
/// execute against chain state, not the strategy's simulated funding.
/// `anchor` is typically a leaked empty [`SimAnchorState`] (the sidecar
/// tracks no canonical-registry pools); the shared `warm_cache` carries the
/// cross-block bytecode/account caches across handle rebuilds.
/// `None` when the head block fetch or the handle build fails (no ambient
/// multi-threaded runtime for `WrapDatabaseAsync` included).
pub async fn build_block_handle(
    provider: &AlloyProvider,
    head: u64,
    warm_cache: &Arc<RwLock<WarmCodeCacheInner>>,
    anchor: &'static SimAnchorState,
) -> Option<BlockSimHandle<'static>> {
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
        weth_address: WETH,
        pool_manager_address: V4_POOL_MANAGER,
    };
    BlockSimHandle::build(
        provider,
        base_fee.saturating_mul(12) / 10,
        head,
        timestamp,
        &override_params,
        anchor,
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

/// Project the frame's touched set into the pool-descriptor map the journal
/// extractor consumes: EVERY descriptor comes from the connector index (the
/// tracked-pool registry) — V2 pair / V3 pool / the V4 `PoolManager` singleton
/// (descriptors exist so `v4_unsupported` is OBSERVABLE, never guessable).
/// A touched address without a tracked descriptor contributes nothing.
/// Also returns whether the `PoolManager` named a touched account.
#[must_use]
pub fn build_descriptors(
    index: Option<&V2ConnectorIndex>,
    touched: &[(Address, Vec<U256>)],
) -> (HbMap<Address, PoolFamily>, bool) {
    let mut map = HbMap::new();
    let mut hit_v4 = false;
    for (addr, _) in touched {
        if *addr == V4_POOL_MANAGER {
            hit_v4 = true;
            map.insert(*addr, PoolFamily::V4PoolManager);
            continue;
        }
        let Some(idx) = index else { continue };
        if idx.edge_by_address(*addr).is_some() {
            map.insert(*addr, PoolFamily::V2Pair);
            continue;
        }
        if let Some(e3) = idx.v3_edge_by_address(*addr) {
            map.insert(
                *addr,
                PoolFamily::V3 {
                    layout: ClSlotLayout::UniswapV3,
                    tick_spacing: e3.tick_spacing,
                    // The pre-tx hint is unknown to the pipeline; the
                    // journal's own slot0 carries the only anchor.
                    current_tick_hint: None,
                },
            );
        }
    }
    (map, hit_v4)
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
    }
    let h = keccak256(&buf);
    format!("0x{}", alloy::hex::encode(&h.as_slice()[..8]))
}

/// The JSONL-family label of a descriptor.
#[must_use]
pub const fn family_label(family: PoolFamily) -> &'static str {
    match family {
        PoolFamily::V2Pair => "v2",
        PoolFamily::V3 { .. } => "v3",
        PoolFamily::V4PoolManager => "v4_manager",
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Workspace admission (frame-scoped scope; replayed state verbatim)
// ─────────────────────────────────────────────────────────────────────────

/// One settlement quote an admitted pool supports: the quote's identity +
/// DB ids for the fan's `(token, quote)` connector lookups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AffectedQuote {
    pub quote: Address,
    pub quote_id: u64,
    /// The pool's OTHER token — the discovery fan's token side.
    pub tok_id: u64,
}

/// An affected pool admitted into THIS frame's scope with its replayed
/// post-target state.
#[derive(Debug, Clone)]
pub struct AffectedPool {
    pub address: Address,
    /// The scope-local workspace id (hop references; dies with the frame).
    pub workspace_pool_id: u64,
    /// The connector-index pool id (discovery exclusion).
    pub index_pool_id: u64,
    pub token0: Address,
    pub token1: Address,
    /// The supported quotes the pool trades (a subset of
    /// [`SUPPORTED_QUOTES`]; empty keeps the truthful no-quote skip). The
    /// discovery fan picks per frame by edge degree.
    pub quotes: Vec<AffectedQuote>,
    pub family: LaneFamily,
}

/// A discovery-fan connector admitted into THIS frame's scope.
#[derive(Debug, Clone)]
pub struct DiscoveredConnector {
    pub address: Address,
    pub workspace_pool_id: u64,
    pub family: LaneFamily,
}

/// The supported-quote orientations of an admitted pool: one entry per
/// [`SUPPORTED_QUOTES`] member the pool trades, with the other side's DB id
/// for the fan. Unjoined ids drop that orientation (no id, no fan).
fn quote_orientations(
    rt: &StrategyRuntime,
    token0: Address,
    token1: Address,
) -> Vec<AffectedQuote> {
    let mut out = Vec::new();
    for (quote, other) in [(token0, token1), (token1, token0)] {
        if !SUPPORTED_QUOTES.contains(&quote) {
            continue;
        }
        let Some(quote_id) = rt.token_id(quote) else {
            continue;
        };
        let Some(tok_id) = rt.token_id(other) else {
            continue;
        };
        out.push(AffectedQuote {
            quote,
            quote_id,
            tok_id,
        });
    }
    out
}

/// Admit every typed extracted post-state into the frame's fresh workspace
/// scope. Returns the affected pools that BOTH admitted AND trade WETH.
#[expect(
    clippy::too_many_lines,
    reason = "the V2/V3 admission arms read top-to-bottom per family"
)]
#[must_use]
pub fn admit_extracted(
    rt: &StrategyRuntime,
    solver: &mut SidecarSolver,
    states: &[PoolPostState],
    seed_block: u64,
    trace_tx: &str,
) -> Vec<AffectedPool> {
    let mut out = Vec::new();
    let Some(idx) = rt.index.as_ref() else {
        return out;
    };
    let skip = |pool: Address, stage: &'static str| {
        trace_jsonl(
            "extract_skip",
            serde_json::json!({
                "tx": trace_tx,
                "pool": format!("0x{}", alloy::hex::encode(pool)),
                "stage": stage,
            }),
        );
    };
    for st in states {
        let PoolPostKind::Typed(tp) = &st.kind else {
            continue;
        };
        match &st.family {
            PoolFamily::V2Pair => {
                let TypedPoolPost::V2 { reserves } = tp else {
                    continue;
                };
                let Some(edge) = idx.edge_by_address(st.address) else {
                    continue;
                };
                let (Some(token0), Some(token1)) =
                    (rt.token_addr(edge.token0_id), rt.token_addr(edge.token1_id))
                else {
                    skip(st.address, "token-join");
                    continue;
                };
                let (r0, r1) = (
                    u112_to_u128(reserves.reserve0),
                    u112_to_u128(reserves.reserve1),
                );
                let Ok(p_id) = solver.admit_v2(&SidecarV2Pool {
                    address: st.address,
                    token0,
                    token1,
                    reserve0: r0,
                    reserve1: r1,
                }) else {
                    skip(st.address, "v2-admit");
                    continue;
                };
                let quotes = quote_orientations(rt, token0, token1);
                if quotes.is_empty() {
                    skip(st.address, "no-supported-quote");
                } else {
                    out.push(AffectedPool {
                        address: st.address,
                        workspace_pool_id: p_id,
                        index_pool_id: edge.pool_id,
                        token0,
                        token1,
                        quotes,
                        family: LaneFamily::V2,
                    });
                }
            }
            PoolFamily::V3 { tick_spacing, .. } => {
                let TypedPoolPost::V3 {
                    sqrt_price_x96,
                    tick,
                    liquidity,
                    touched_ticks,
                } = tp
                else {
                    continue;
                };
                let spacing = *tick_spacing;
                let Some(edge) = idx.v3_edge_by_address(st.address) else {
                    skip(st.address, "v3-edge");
                    continue;
                };
                let (Some(token0), Some(token1)) =
                    (rt.token_addr(edge.token0_id), rt.token_addr(edge.token1_id))
                else {
                    skip(st.address, "token-join");
                    continue;
                };
                // Nothing is fabricated: an incomplete slot0/liquidity set
                // cannot stage (the scope never fills in missing words).
                let (Some(sqrt), Some(tk), Some(liq)) = (*sqrt_price_x96, *tick, *liquidity) else {
                    skip(st.address, "incomplete-slot0-liquidity");
                    continue;
                };
                let mut tick_data = HbMap::default();
                for t in touched_ticks {
                    tick_data.insert(
                        t.tick,
                        TickInfo {
                            liquidity_gross: alloy::primitives::aliases::U128::from(
                                t.liquidity_gross,
                            ),
                            liquidity_net: t.liquidity_net,
                            block: seed_block,
                        },
                    );
                }
                let Some(p_id) = solver.admit_v3_explicit(
                    st.address, token0, token1, edge.fee, spacing, sqrt, liq, tk, tick_data,
                    seed_block,
                ) else {
                    skip(st.address, "v3-admit");
                    continue;
                };
                let quotes = quote_orientations(rt, token0, token1);
                if quotes.is_empty() {
                    skip(st.address, "no-supported-quote");
                } else {
                    out.push(AffectedPool {
                        address: st.address,
                        workspace_pool_id: p_id,
                        index_pool_id: edge.pool_id,
                        token0,
                        token1,
                        quotes,
                        family: LaneFamily::V3 { fee: edge.fee },
                    });
                }
            }
            // Descriptors guarantee `Unsupported` was filtered upstream.
            PoolFamily::V4PoolManager => {}
        }
    }
    out
}

/// U112 → u128 (lossless; the workspace re-validates the uint112 class).
fn u112_to_u128(v: alloy::primitives::aliases::U112) -> u128 {
    u128::try_from(U256::from(v)).unwrap_or(0)
}

// ─────────────────────────────────────────────────────────────────────────
// Same-chain-view reads (connector state; raw-RPC fallback only)
// ─────────────────────────────────────────────────────────────────────────

/// V2 reserves read through the frame-replay chain view (slot 8). Zero
/// reserves read through as `None` (a pool with zero reserves is not usable
/// state — the caller falls back or skips).
fn view_v2_reserves(
    scratch: &mut ScratchEvm<ScratchDb<'_>>,
    pool: Address,
) -> Option<(u128, u128)> {
    let word = read_view_word(
        scratch.ext(),
        pool,
        U256::from(slot_layout::V2_RESERVES_SLOT),
    )?;
    let parts = slot_layout::decode_v2_reserves_word(word);
    (parts.reserve0 != alloy::primitives::aliases::U112::ZERO
        || parts.reserve1 != alloy::primitives::aliases::U112::ZERO)
        .then(|| {
            (
                u128::try_from(U256::from(parts.reserve0)).unwrap_or(0),
                u128::try_from(U256::from(parts.reserve1)).unwrap_or(0),
            )
        })
}

/// V3 CL state read through the frame-replay chain view: slot0, liquidity,
/// and the in-range tick words (current word ± 1 — the same window as the
/// RPC bootstrap ladder).
///
/// `None` on a failed read or a zero in-range liquidity (unsolvable CL state
/// — the raw-RPC ladder decides).
fn read_v3_view(
    scratch: &mut ScratchEvm<ScratchDb<'_>>,
    pool: Address,
    layout: ClSlotLayout,
    tick_spacing: i32,
    head: u64,
) -> Option<(U256, i32, u128, HbMap<i32, TickInfo>)> {
    let slot0 = read_view_word(scratch.ext(), pool, U256::ZERO)?;
    let parts = v3_storage_slots::decode_v3_slot0(slot0);
    let liq_word = read_view_word(scratch.ext(), pool, U256::from(layout.liquidity_slot()))?;
    let liquidity = (liq_word & U256::from(u128::MAX)).to::<u128>();
    if liquidity == 0 {
        return None;
    }
    let spacing = i64::from(tick_spacing.max(1));
    let (word_pos, _) = floor_word_pos(parts.tick, spacing);
    let w0 = i64::from(word_pos).saturating_sub(1);
    let w1 = i64::from(word_pos).saturating_add(1);
    let mut tick_data = HbMap::default();
    for w in w0..=w1 {
        let Ok(word_pos_i16) = i16::try_from(w) else {
            continue;
        };
        let slot = slot_layout::cl_tick_bitmap_word_slot(ClSlotLayout::UniswapV3, word_pos_i16);
        let Some(bitmap) = read_view_word(scratch.ext(), pool, slot) else {
            continue;
        };
        for bit in 0..256i64 {
            if (bitmap >> U256::try_from(bit).ok()?) & U256::ONE != U256::ONE {
                continue;
            }
            let tick = i64::from(word_pos_i16) * 256 + bit;
            let tick_i32 = i32::try_from(tick.checked_mul(spacing)?).ok()?;
            let Some(word) = read_view_word(
                scratch.ext(),
                pool,
                slot_layout::cl_tick_mapping_slot(ClSlotLayout::UniswapV3, tick_i32),
            ) else {
                continue;
            };
            let (gross, net) = slot_layout::decode_tick_word(word);
            tick_data.insert(
                tick_i32,
                TickInfo {
                    liquidity_gross: alloy::primitives::aliases::U128::from(gross),
                    liquidity_net: net,
                    block: head,
                },
            );
        }
    }
    Some((parts.sqrt_price_x96, parts.tick, liquidity, tick_data))
}

/// `(word position, bit position)` per the CL bitmap layout —
/// `compressed = floor_div(tick / spacing)`; `word = compressed >> 8`,
/// `bit = compressed & 0xFF`. Local floor-division twin of
/// `degenbot_math::cl::liquidity_mapping` (kept inline so this crate needs
/// no extra edge).
fn floor_word_pos(tick: i32, spacing: i64) -> (i16, u16) {
    let compressed = floor_div_i64(i64::from(tick), spacing);
    (
        i16::try_from(compressed >> 8).unwrap_or(0),
        u16::try_from(compressed & 0xFF).unwrap_or(0),
    )
}

fn floor_div_i64(a: i64, b: i64) -> i64 {
    let q = a / b;
    if a % b != 0 && ((a < 0) != (b < 0)) {
        q - 1
    } else {
        q
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Discovery + solve
// ─────────────────────────────────────────────────────────────────────────

/// The two drift cycles for a (staged affected pool, connector) pairing in
/// `quote`: enter on the quote side, exit on the token side (and reverse).
/// Both cycle pools trade the SAME token pair, so pair-level canonical
/// order is shared; the QUOTE level is only closed in WETH (wei-honest
/// 2-hops) — for every other quote the caller must wrap these with the
/// priced [`Normalization`] legs before declaring.
#[must_use]
pub fn two_hop_cycles(
    p: &AffectedPool,
    c: &DiscoveredConnector,
    token0: Address,
    token1: Address,
    tok: Address,
    quote: Address,
) -> Vec<Vec<SidecarHopRef>> {
    let hop = |pool_id: u64, pool: Address, zfo: bool, family: LaneFamily| SidecarHopRef {
        pool_id,
        pool,
        token0,
        token1,
        zfo,
        family,
    };
    let buy_on_c = hop(c.workspace_pool_id, c.address, quote == token0, c.family);
    let sell_into_p = hop(p.workspace_pool_id, p.address, tok == token0, p.family);
    let buy_on_p = hop(p.workspace_pool_id, p.address, quote == token0, p.family);
    let sell_into_c = hop(c.workspace_pool_id, c.address, tok == token0, c.family);
    vec![vec![buy_on_c, sell_into_p], vec![buy_on_p, sell_into_c]]
}

/// The priced WETH ↔ quote normalization lane for a non-WETH settlement
/// quote: two DISTINCT deep quote pools from the depth-ranked index, one
/// WETH→quote and one quote→WETH. Both legs ride the candidate as hops, so
/// the cycle closes in WETH and the solver's profit is wei; a reused pool
/// would double-count its reserves in the chain solve, hence the pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Normalization {
    /// WETH → quote (input = WETH).
    pub inbound: SidecarHopRef,
    /// Quote → WETH (input = quote, the inverse direction).
    pub outbound: SidecarHopRef,
}

/// One quote's discovery fan for an affected pool: the quote's connectors
/// (already admitted) and the priced normalization legs when the quote is
/// not WETH.
#[derive(Debug, Clone)]
pub struct QuoteFan {
    pub quote: Address,
    pub quote_id: u64,
    pub connectors: Vec<DiscoveredConnector>,
    /// `None` for WETH (already the base quote — 2-hop cycles are
    /// wei-honest) or when the normalization could not be priced on the
    /// spot (the fan's candidates drop with the frame's `non_base_quote`
    /// observation).
    pub normalization: Option<Normalization>,
}

/// Solve stats for the JSONL trace + the composed candidate.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SolveStats {
    pub connectors: usize,
    pub cycles_declared: usize,
    pub cycles_evaluated: usize,
    /// Rounded-up to the same gate as the fans: the anchored walker's
    /// depth-3 chains declared / envelope-evaluated.
    pub dfs_declared: usize,
    pub dfs_evaluated: usize,
    pub best: Option<LaneCandidate>,
    /// A non-WETH quote fan had connectors but no priceable WETH
    /// normalization lane, so its candidates were refused (the frame's
    /// truthful `non_base_quote` observation when nothing else composed).
    pub non_base_quote_dropped: bool,
}

/// Wrap the quote-land 2-hop drift cycle with the priced normalization
/// legs: `WETH →quote → tok → quote → WETH`, one connected WETH-closed
/// path whose solver profit is wei. Both hops carry their own pool's
/// canonical pair order and zfo direction.
fn normalized_cycle(base: Vec<SidecarHopRef>, norm: &Normalization) -> Vec<SidecarHopRef> {
    let mut path = Vec::with_capacity(base.len() + 2);
    path.push(norm.inbound);
    path.extend(base);
    path.push(norm.outbound);
    path
}

/// Solve every quote fan of one affected pool: declare both drift cycles
/// per (quote, connector) pairing — wrapped in the priced normalization
/// lane for non-WETH quotes — evaluate envelope-gated (wei floor vs wei
/// profit for every quote), and keep the best. Pure workspace work (no
/// I/O) — the code-only fixture tests drive this directly; ranking across
/// quotes compares profits in wei, never in quote units.
pub fn solve_fans(
    solver: &mut SidecarSolver,
    affected: &AffectedPool,
    fans: &[QuoteFan],
    gas_floor_wei: U256,
) -> SolveStats {
    let mut stats = SolveStats {
        connectors: fans.iter().map(|f| f.connectors.len()).sum(),
        ..SolveStats::default()
    };
    for fan in fans {
        if fan.quote != WETH && fan.normalization.is_none() {
            // Quote-land candidates exist but cannot close in WETH honestly:
            // refuse them (never "convert for ranking only").
            stats.non_base_quote_dropped = !fan.connectors.is_empty();
            continue;
        }
        let tok = if affected.token0 == fan.quote {
            affected.token1
        } else {
            affected.token0
        };
        for c in &fan.connectors {
            for base in two_hop_cycles(
                affected,
                c,
                affected.token0,
                affected.token1,
                tok,
                fan.quote,
            ) {
                let cycle = match fan.normalization {
                    Some(norm) => normalized_cycle(base, &norm),
                    None => base,
                };
                let idx = solver.declare_hops(&cycle);
                stats.cycles_declared += 1;
                let Some(res) = solver.evaluate(idx, gas_floor_wei) else {
                    continue;
                };
                stats.cycles_evaluated += 1;
                let profit = res.profit.to::<u128>();
                if stats.best.as_ref().is_some_and(|b| b.profit >= profit) {
                    continue;
                }
                stats.best = Some(LaneCandidate {
                    hops: cycle,
                    optimal_input: res.optimal_input.to::<u128>(),
                    hop_outputs: res.hop_outputs.iter().map(|v| v.to::<u128>()).collect(),
                    consumed_inputs: res.consumed_inputs.iter().map(|v| v.to::<u128>()).collect(),
                    profit,
                });
            }
        }
    }
    stats
}

/// Evaluate the anchored walker's depth-3 chains through the SAME
/// declare/evaluate/envelope gate as the fans (wei floor vs wei profit),
/// keeping the best. The chains arrive anchor-first in traversal order;
/// selection across fans, quotes, and walker chains still compares wei
/// downstream — this function only bundles the walker surface's counts.
pub fn solve_dfs_chains(
    solver: &mut SidecarSolver,
    chains: &[Vec<SidecarHopRef>],
    gas_floor_wei: U256,
) -> SolveStats {
    let mut stats = SolveStats::default();
    for chain in chains {
        let idx = solver.declare_hops(chain);
        stats.dfs_declared += 1;
        let Some(res) = solver.evaluate(idx, gas_floor_wei) else {
            continue;
        };
        stats.dfs_evaluated += 1;
        let profit = res.profit.to::<u128>();
        if stats.best.as_ref().is_some_and(|b| b.profit >= profit) {
            continue;
        }
        stats.best = Some(LaneCandidate {
            hops: chain.clone(),
            optimal_input: res.optimal_input.to::<u128>(),
            hop_outputs: res.hop_outputs.iter().map(|v| v.to::<u128>()).collect(),
            consumed_inputs: res.consumed_inputs.iter().map(|v| v.to::<u128>()).collect(),
            profit,
        });
    }
    stats
}

/// The hop refs of one WETH-entry 3-hop walker cycle:
/// `WETH →(anchor) tok →(h1) mid →(h2) WETH`. `None` unless the resolved
/// bridges straddle exactly that token path — the cycle came from the
/// walker, and this re-derives its traversal from index identity instead
/// of trusting orientation.
#[expect(
    clippy::too_many_arguments,
    reason = "the identity fields name the traversal: derived ids, addresses, admissions"
)]
#[must_use]
pub fn three_hop_refs(
    a: &AffectedPool,
    weth_id: u64,
    tok_id: u64,
    tok_addr: Address,
    mid_id: u64,
    mid_addr: Address,
    h1: &ResolvedHop,
    h2: &ResolvedHop,
    ws1: u64,
    ws2: u64,
) -> Option<Vec<SidecarHopRef>> {
    let straddles = |r: &ResolvedHop, x: u64, y: u64| {
        (r.token0_id() == x && r.token1_id() == y) || (r.token1_id() == x && r.token0_id() == y)
    };
    if !straddles(h1, tok_id, mid_id) || !straddles(h2, mid_id, weth_id) {
        return None;
    }
    let family = |r: &ResolvedHop| match r {
        ResolvedHop::V2(_) => LaneFamily::V2,
        ResolvedHop::V3(e) => LaneFamily::V3 { fee: e.fee },
    };
    let anchor = SidecarHopRef {
        pool_id: a.workspace_pool_id,
        pool: a.address,
        token0: a.token0,
        token1: a.token1,
        // The anchor hop consumes WETH: `zfo` ⇔ the input is the hop's token0.
        zfo: a.token0 == WETH,
        family: a.family,
    };
    let hop1 = SidecarHopRef {
        pool_id: ws1,
        pool: *h1.address(),
        token0: if h1.token0_id() == tok_id {
            tok_addr
        } else {
            mid_addr
        },
        token1: if h1.token0_id() == tok_id {
            mid_addr
        } else {
            tok_addr
        },
        zfo: h1.token0_id() == tok_id,
        family: family(h1),
    };
    let hop2 = SidecarHopRef {
        pool_id: ws2,
        pool: *h2.address(),
        token0: if h2.token0_id() == weth_id {
            WETH
        } else {
            mid_addr
        },
        token1: if h2.token1_id() == weth_id {
            WETH
        } else {
            mid_addr
        },
        zfo: h2.token0_id() == mid_id,
        family: family(h2),
    };
    Some(vec![anchor, hop1, hop2])
}

/// Admit one walker hop's pool at the frame's chain view (raw-RPC fallback)
/// and return its workspace id. A pool whose state reads as unusable
/// (zero reserves, incomplete slot0/liquidity) is skipped, never guessed.
async fn admit_hop_pool(
    rt: &StrategyRuntime,
    solver: &mut SidecarSolver,
    scratch: &mut ScratchEvm<ScratchDb<'_>>,
    provider: &AlloyProvider,
    hop: &ResolvedHop,
    head: u64,
) -> Option<u64> {
    match hop {
        ResolvedHop::V2(e) => {
            let reserves = match view_v2_reserves(scratch, e.address) {
                Some(r) => r,
                None => {
                    match degenbot_rpc::abi::fetch_v2_reserves(provider, &e.address, None).await {
                        Ok((r0, r1)) => Some((u128::try_from(r0).ok()?, u128::try_from(r1).ok()?)),
                        Err(_) => None,
                    }?
                }
            };
            let token0 = rt.token_addr(e.token0_id)?;
            let token1 = rt.token_addr(e.token1_id)?;
            solver
                .admit_v2(&SidecarV2Pool {
                    address: e.address,
                    token0,
                    token1,
                    reserve0: reserves.0,
                    reserve1: reserves.1,
                })
                .ok()
        }
        ResolvedHop::V3(e) => {
            let token0 = rt.token_addr(e.token0_id)?;
            let token1 = rt.token_addr(e.token1_id)?;
            if let Some((sqrt, tk, liq, tick_data)) = read_v3_view(
                scratch,
                e.address,
                ClSlotLayout::UniswapV3,
                e.tick_spacing,
                head,
            ) {
                solver.admit_v3_explicit(
                    e.address,
                    token0,
                    token1,
                    e.fee,
                    e.tick_spacing,
                    sqrt,
                    liq,
                    tk,
                    tick_data,
                    head,
                )
            } else {
                solver
                    .admit_v3_full(
                        provider,
                        e.address,
                        token0,
                        token1,
                        e.fee,
                        e.tick_spacing,
                        None,
                        head,
                    )
                    .await
            }
        }
    }
}

/// The anchored walker's depth-3 lane: per WETH-entry cycle, admit the two
/// bridge pools at the frame's chain view and build the anchor-first hop
/// chain the shared gate evaluates. 2-hop walker cycles never reach here —
/// the depth-ranked fan owns that surface (parity is proven at the engine
/// level) — so the walker's NEW candidates are exactly these chains.
#[expect(
    clippy::too_many_arguments,
    reason = "the lane takes the runtime surfaces it needs"
)]
async fn dfs_three_hop_chains(
    rt: &StrategyRuntime,
    idx: &V2ConnectorIndex,
    a: &AffectedPool,
    wq: &AffectedQuote,
    cycles: &[DfsCycle],
    scratch: &mut ScratchEvm<ScratchDb<'_>>,
    solver: &mut SidecarSolver,
    provider: &AlloyProvider,
    head: u64,
) -> Vec<Vec<SidecarHopRef>> {
    let mut chains = Vec::new();
    let Some(tok_addr) = rt.token_addr(wq.tok_id) else {
        return chains;
    };
    for cycle in cycles
        .iter()
        .filter(|c| c.pools.len() == 3 && c.entry_token_id == wq.quote_id)
    {
        let Some(h1) = resolve_hop(idx, cycle.pools[1]) else {
            continue;
        };
        let Some(h2) = resolve_hop(idx, cycle.pools[2]) else {
            continue;
        };
        let mid_id = if h1.token0_id() == wq.tok_id {
            h1.token1_id()
        } else {
            h1.token0_id()
        };
        let Some(mid_addr) = rt.token_addr(mid_id) else {
            continue;
        };
        let Some(ws1) = admit_hop_pool(rt, solver, scratch, provider, &h1, head).await else {
            continue;
        };
        let Some(ws2) = admit_hop_pool(rt, solver, scratch, provider, &h2, head).await else {
            continue;
        };
        if let Some(chain) = three_hop_refs(
            a,
            wq.quote_id,
            wq.tok_id,
            tok_addr,
            mid_id,
            mid_addr,
            &h1,
            &h2,
            ws1,
            ws2,
        ) {
            chains.push(chain);
        }
    }
    chains
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

/// Price the WETH ↔ `quote` normalization lane on the spot for a non-WETH
/// settlement quote: walk the quote's depth-ranked WETH connector ranking,
/// admit the first TWO distinct pools priced through the frame's chain view
/// (raw-RPC fallback), and build the two hops. `None` when fewer than two
/// pools price — the caller drops the quote's candidates with the frame's
/// `non_base_quote` observation. `exclude_pool` keeps the affected pool
/// itself out of the lane (it already runs as the arbitrage hop).
async fn price_normalization(
    rt: &StrategyRuntime,
    idx: &V2ConnectorIndex,
    quote: &AffectedQuote,
    exclude_pool: u64,
    solver: &mut SidecarSolver,
    scratch: &mut ScratchEvm<ScratchDb<'_>>,
    provider: &AlloyProvider,
) -> Option<Normalization> {
    let weth_id = rt.weth_id()?;
    let quote_addr = rt.token_addr(quote.quote_id)?;
    // Deepest first (the ranking memo); only V2 legs — the cheapest to
    // price honestly per frame.
    let cands = idx
        .connectors(quote.quote_id, weth_id, exclude_pool, rt.connector_cap)
        .await;
    let mut legs: Vec<SidecarHopRef> = Vec::with_capacity(2);
    for (edge, quote_is_token0) in cands {
        let reserves: Option<(u128, u128)> = match view_v2_reserves(scratch, edge.address) {
            Some(r) => Some(r),
            None => match degenbot_rpc::abi::fetch_v2_reserves(provider, &edge.address, None).await
            {
                Ok((r0, r1)) => Some((u128::try_from(r0).ok()?, u128::try_from(r1).ok()?)),
                Err(_) => None,
            },
        };
        let Some((r0, r1)) = reserves else {
            continue;
        };
        let (token0, token1) = if quote_is_token0 {
            (quote_addr, WETH)
        } else {
            (WETH, quote_addr)
        };
        let Ok(ws_id) = solver.admit_v2(&SidecarV2Pool {
            address: edge.address,
            token0,
            token1,
            reserve0: r0,
            reserve1: r1,
        }) else {
            continue;
        };
        legs.push(SidecarHopRef {
            pool_id: ws_id,
            pool: edge.address,
            token0,
            token1,
            // The inbound leg inputs WETH; zfo is true iff WETH is token0.
            zfo: WETH == token0,
            family: LaneFamily::V2,
        });
        if legs.len() == 2 {
            break;
        }
    }
    let inbound = legs.first().copied()?;
    let outbound = SidecarHopRef {
        zfo: !legs[1].zfo, // the outbound leg inputs quote
        ..legs[1]
    };
    Some(Normalization { inbound, outbound })
}

// ─────────────────────────────────────────────────────────────────────────
// The `eth_callMany` bundle-sim gate: READ/SIM ONLY — never a broadcast.
// ─────────────────────────────────────────────────────────────────────────

/// Does `[target, backrun]` survive as one bundle? (read/sim only.)
///
/// Endpoints disagree on the bundle envelope: the `MEVBlocker`
/// searcher-doc shape carries `params[0] = { transactions: [...] }`, while
/// mev-geth lineage nodes (reth included) parse `params[0]` as a LIST of
/// blocks — `params[0] = [{ transactions: [...] }]`. Both shapes are
/// attempted in that order; a parameter-shape or method-missing error on
/// one endpoint must never read as "the bundle reverted".
pub async fn simulate_candidate(
    sim_client: &alloy::rpc::client::RpcClient,
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
    let backrun_call = crate::bundle::backrun_sim_call(owner, exec, cd, 900_000, 30_000_000_000);
    let params =
        crate::bundle::eth_call_many_bundle_sim_params(&[target_call, backrun_call], "latest");
    // params[0] = the bundle object (doc shape); the mev-geth shape wraps
    // it in the blocks list.
    let mut blocks_list = params.clone();
    if let Some(arr) = blocks_list.as_array_mut() {
        if let Some(first) = arr.first().cloned() {
            arr[0] = serde_json::json!([first]);
        }
    }
    for attempt in [params, blocks_list] {
        if let Ok(resp) = sim_client
            .request::<serde_json::Value, serde_json::Value>("eth_callMany", attempt)
            .await
        {
            let txt = serde_json::to_string(&resp).unwrap_or_default();
            if !txt.contains("error") {
                return true;
            }
        }
    }
    false
}

// ─────────────────────────────────────────────────────────────────────────
// The frame pipeline (one feed frame, end to end)
// ─────────────────────────────────────────────────────────────────────────

/// Process ONE feed frame: replay → extract → admit → discover → solve →
/// compose → sim gate → decision. The bin drives this per drained event and
/// owns dispatch/submit; the returned [`FrameArtifacts::decision`] already
/// carries the truthful observe reason when nothing composed.
///
/// `handle` may be `None` when the per-block replay stack failed to build
/// (no ambient multi-threaded runtime, RPC outage) — the frame observes
/// `replay_unavailable` rather than being classified or silently dropped.
#[expect(clippy::too_many_lines, reason = "one frame reads top-to-bottom")]
#[expect(
    clippy::too_many_arguments,
    reason = "the frame takes the runtime surfaces it needs"
)]
pub async fn process_frame(
    rt: &mut StrategyRuntime,
    provider: &AlloyProvider,
    sim_client: &alloy::rpc::client::RpcClient,
    sidecar: &SidecarConfig,
    pl: &PipelineConfig,
    handle: &mut Option<BlockSimHandle<'static>>,
    ev: &BackrunFeedEvent,
    head: u64,
    age_ms: u64,
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
    let outcome = match scratch.replay(&replayable) {
        Ok(o) => o,
        Err(e) => {
            stages.replay_us = u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX);
            trace_jsonl(
                "replay",
                serde_json::json!({"tx": tx_hex, "error": e.to_string()}),
            );
            return FrameArtifacts::observe("replay_failed", stages);
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
    let (descriptors, hit_v4) = build_descriptors(rt.index.as_ref(), &outcome.touched);
    let extracted = extract_pool_post_states(&outcome, &descriptors);
    stages.extract_us = u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX);
    let all_unsupported = !extracted.is_empty()
        && extracted
            .iter()
            .all(|s| matches!(s.kind, PoolPostKind::Unsupported));
    trace_jsonl(
        "extract",
        serde_json::json!({
            "tx": tx_hex,
            "families": extracted.iter().map(|s| family_label(s.family)).collect::<Vec<_>>(),
            "digests": extracted.iter().map(state_digest).collect::<Vec<_>>(),
        }),
    );
    if all_unsupported || extracted.is_empty() {
        // V4-only frames are observably unsupported; everything else that
        // surfaced no tracked pool state is (so far) a no-candidate frame.
        return FrameArtifacts::observe(
            if hit_v4 {
                "v4_unsupported"
            } else {
                "no_candidate"
            },
            stages,
        );
    }

    // ── stage: workspace admission (fresh scope; replayed state verbatim) ─
    let t = Instant::now();
    let mut solver = SidecarSolver::new();
    let affected = admit_extracted(rt, &mut solver, &extracted, head, &tx_hex);
    stages.admit_us = u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX);
    if affected.is_empty() {
        return FrameArtifacts::observe("no_candidate", stages);
    }

    // ── stage: discovery fan (per-frame quote set; depth-ranked) ─────────
    let t = Instant::now();
    let Some(idx) = rt.index.as_ref() else {
        return FrameArtifacts::observe("no_candidate", stages);
    };
    if rt.weth_id().is_none() {
        return FrameArtifacts::observe("no_candidate", stages);
    }
    // Quote selection by touched-token edge degree (V2 + V3 index): quotes
    // that actually connect rank first (WETH on ties), quotes that cannot
    // connect drop out, and the connector budget splits across the rest.
    let mut selected: Vec<Vec<AffectedQuote>> = Vec::with_capacity(affected.len());
    for a in &affected {
        let mut ranked: Vec<(AffectedQuote, usize)> = a
            .quotes
            .iter()
            .map(|q| {
                (
                    *q,
                    idx.edge_degree(q.tok_id, q.quote_id)
                        + idx.v3_edge_degree(q.tok_id, q.quote_id),
                )
            })
            .collect();
        ranked.retain(|(_, deg)| *deg > 0);
        ranked
            .sort_by_key(|(q, deg)| (std::cmp::Reverse(*deg), std::cmp::Reverse(q.quote == WETH)));
        selected.push(ranked.into_iter().map(|(q, _)| q).collect());
    }
    let mut fans: Vec<Vec<QuoteFan>> = Vec::with_capacity(affected.len());
    // Normalization lanes memoize per (quote, excluded affected pool) so a
    // second pool settling in the same quote reuses the priced hops.
    let mut normalizations: HashMap<(u64, u64), Option<Normalization>> = HashMap::new();
    let mut cycles_proposed = 0usize;
    let mut connectors_seen = 0usize;
    let mut non_base_quote_dropped = false;
    for (ai, a) in affected.iter().enumerate() {
        let quotes = &selected[ai];
        let mut pool_fans = Vec::with_capacity(quotes.len());
        let per_quote_cap = if rt.connector_cap == 0 {
            0
        } else {
            (rt.connector_cap / quotes.len()).max(1)
        };
        for q in quotes {
            let normalization = if q.quote == WETH {
                None
            } else {
                let key = (q.quote_id, a.index_pool_id);
                if let std::collections::hash_map::Entry::Vacant(e) = normalizations.entry(key) {
                    let priced = price_normalization(
                        rt,
                        idx,
                        q,
                        a.index_pool_id,
                        &mut solver,
                        scratch,
                        provider,
                    )
                    .await;
                    trace_jsonl(
                        "normalize",
                        serde_json::json!({
                            "tx": tx_hex,
                            "quote": format!("0x{}", alloy::hex::encode(q.quote)),
                            "priced": priced.is_some(),
                        }),
                    );
                    e.insert(priced);
                }
                normalizations.get(&key).copied().flatten()
            };
            if q.quote != WETH && normalization.is_none() {
                // Quote-land candidates exist in the index but cannot close
                // in WETH honestly: refuse instead of converting for
                // ranking only.
                trace_jsonl(
                    "discover_skip",
                    serde_json::json!({
                        "tx": tx_hex,
                        "reason": "non_base_quote",
                        "quote": format!("0x{}", alloy::hex::encode(q.quote)),
                    }),
                );
                non_base_quote_dropped = true;
                continue;
            }
            let cands = idx
                .connectors(q.tok_id, q.quote_id, a.index_pool_id, per_quote_cap)
                .await;
            let v3cands = idx
                .v3_connectors(q.tok_id, q.quote_id, a.index_pool_id, per_quote_cap)
                .await;
            connectors_seen += cands.len() + v3cands.len();
            cycles_proposed += 2 * (cands.len() + v3cands.len());
            let mut admitted = Vec::new();
            for (edge, _) in cands {
                let reserves = match view_v2_reserves(scratch, edge.address) {
                    Some(r) => r,
                    None => {
                        match degenbot_rpc::abi::fetch_v2_reserves(provider, &edge.address, None)
                            .await
                        {
                            Ok((r0, r1)) => match (u128::try_from(r0), u128::try_from(r1)) {
                                (Ok(r0), Ok(r1)) => (r0, r1),
                                _ => continue,
                            },
                            Err(_) => continue,
                        }
                    }
                };
                if let Ok(c_id) = solver.admit_v2(&SidecarV2Pool {
                    address: edge.address,
                    token0: a.token0,
                    token1: a.token1,
                    reserve0: reserves.0,
                    reserve1: reserves.1,
                }) {
                    admitted.push(DiscoveredConnector {
                        address: edge.address,
                        workspace_pool_id: c_id,
                        family: LaneFamily::V2,
                    });
                }
            }
            for (v3e, _) in v3cands {
                // Same-chain-view first; the raw-RPC ladder only as fallback.
                let admitted_id = if let Some((sqrt, tk, liq, tick_data)) = read_v3_view(
                    scratch,
                    v3e.address,
                    ClSlotLayout::UniswapV3,
                    v3e.tick_spacing,
                    head,
                ) {
                    solver.admit_v3_explicit(
                        v3e.address,
                        a.token0,
                        a.token1,
                        v3e.fee,
                        v3e.tick_spacing,
                        sqrt,
                        liq,
                        tk,
                        tick_data,
                        head,
                    )
                } else {
                    solver
                        .admit_v3_full(
                            provider,
                            v3e.address,
                            a.token0,
                            a.token1,
                            v3e.fee,
                            v3e.tick_spacing,
                            None,
                            head,
                        )
                        .await
                };
                if let Some(c_id) = admitted_id {
                    admitted.push(DiscoveredConnector {
                        address: v3e.address,
                        workspace_pool_id: c_id,
                        family: LaneFamily::V3 { fee: v3e.fee },
                    });
                }
            }
            pool_fans.push(QuoteFan {
                quote: q.quote,
                quote_id: q.quote_id,
                connectors: admitted,
                normalization,
            });
        }
        fans.push(pool_fans);
    }

    // ── anchored walker: the depth-3 lane ────────────────────────────────
    // Anchors are this frame's touched pools; every cycle includes its
    // anchor by construction; the walker's cancel budget is the frame's
    // discovery slice. The depth-ranked fan owns the 2-hop surface until
    // cutover — the walker's NEW candidates here are exactly the 3-hop
    // chains (an anchor pool bridging two connectors through an
    // intermediate token), admitted at the frame's chain view. WETH-transit
    // cycles only: the cycle's transit currency must be wei for the shared
    // envelope gate; non-WETH quotes keep their priced-normalization lane.
    let mut dfs_chains: Vec<Vec<SidecarHopRef>> = Vec::new();
    let mut dfs_cycles = 0usize;
    if let Some(graph) = rt.dfs.as_ref() {
        let budget = DiscoveryBudget::after(FRAME_DISCOVERY_SLICE);
        for a in &affected {
            let Some(wq) = a.quotes.iter().find(|q| q.quote == WETH) else {
                continue;
            };
            let anchor = AnchorPool {
                pool_id: a.index_pool_id,
                pool_kind: match a.family {
                    LaneFamily::V2 => PoolKind::V2,
                    LaneFamily::V3 { .. } => PoolKind::V3,
                },
                token_a_id: wq.quote_id,
                token_b_id: wq.tok_id,
            };
            let cycles = graph.cycles_through_pool(anchor, &budget, rt.connector_cap.max(1));
            dfs_cycles += cycles.len();
            let chains = dfs_three_hop_chains(
                rt,
                idx,
                a,
                wq,
                &cycles,
                scratch,
                &mut solver,
                provider,
                head,
            )
            .await;
            dfs_chains.extend(chains);
            if budget.expired() {
                break;
            }
        }
    }
    stages.discover_us = u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX);
    trace_jsonl(
        "discover",
        serde_json::json!({
            "tx": tx_hex,
            "connectors": connectors_seen,
            "cycles_proposed": cycles_proposed,
            "dfs_cycles": dfs_cycles,
            "dfs_chains": dfs_chains.len(),
            "affected": affected.len(),
            "non_base_quote_dropped": non_base_quote_dropped,
        }),
    );

    // ── stage: solve (envelope-gated fans; wei across every quote) ───────
    let t = Instant::now();
    let mut aggregate = SolveStats::default();
    for (a, pool_fans) in affected.iter().zip(&fans) {
        let stats = solve_fans(&mut solver, a, pool_fans, pl.gas_floor_wei);
        aggregate.connectors += stats.connectors;
        aggregate.cycles_declared += stats.cycles_declared;
        aggregate.cycles_evaluated += stats.cycles_evaluated;
        aggregate.non_base_quote_dropped |= stats.non_base_quote_dropped;
        if stats.best.as_ref().is_some_and(|b| {
            aggregate
                .best
                .as_ref()
                .is_none_or(|best| b.profit > best.profit)
        }) {
            aggregate.best = stats.best;
        }
    }
    // The walker's depth-3 chains meet the same envelope gate and wei
    // ranking as the fans' best candidates.
    if !dfs_chains.is_empty() {
        let dfs_stats = solve_dfs_chains(&mut solver, &dfs_chains, pl.gas_floor_wei);
        aggregate.dfs_declared += dfs_stats.dfs_declared;
        aggregate.dfs_evaluated += dfs_stats.dfs_evaluated;
        if dfs_stats.best.as_ref().is_some_and(|b| {
            aggregate
                .best
                .as_ref()
                .is_none_or(|best| b.profit > best.profit)
        }) {
            aggregate.best = dfs_stats.best;
        }
    }
    aggregate.non_base_quote_dropped |= non_base_quote_dropped;
    stages.solve_us = u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX);

    // ── stage: compose + `eth_callMany` gate ──────────────────────────────
    let mut requested_bid = U256::ZERO;
    let mut submit_calldata = None;
    if let Some(best) = aggregate.best.clone() {
        let t = Instant::now();
        let composed = build_candidate_calldata(&best, pl.exec, WETH, pl.bribe_bips);
        stages.compose_us = u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX);
        match composed {
            Some(cd) => {
                let t = Instant::now();
                let sim_ok = simulate_candidate(sim_client, ev, pl.exec, pl.owner, &cd).await;
                stages.sim_us = u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX);
                trace_jsonl(
                    "composed",
                    serde_json::json!({
                        "tx": tx_hex,
                        "profit": best.profit,
                        "input": best.optimal_input,
                        "calldata": format!("0x{}", alloy::hex::encode(&cd)),
                        "sim": if sim_ok { "passed" } else { "failed" },
                    }),
                );
                if sim_ok {
                    // Floor to 1: a sim-passed candidate with a sub-wei
                    // share must reach decide() as a bid, not as zero_bid.
                    requested_bid = bid_from_profit(best.profit, pl.bribe_bips).max(U256::from(1));
                    submit_calldata = Some(cd);
                }
            }
            None => {
                trace_jsonl(
                    "composed",
                    serde_json::json!({"tx": tx_hex, "composed": false}),
                );
            }
        }
    }

    // ── stage: decide ─────────────────────────────────────────────────────
    let composed_any = submit_calldata.is_some();
    // The classifier is off the hot path (the wire task): a frame that reached the
    // decision stage IS actionable — the sentinel routes decide() through
    // its actionable arm without consulting degenbot_decoders.
    let class = TargetClass::Swap(Vec::new());
    let decision = decide(
        sidecar,
        sidecar.stop_file.exists(),
        &class,
        composed_any,
        requested_bid,
        age_ms,
        spent,
    );
    // The observe label tells the truth: a frame that never composed a
    // candidate must not read as "the sim rejected our work".
    let decision = if composed_any {
        decision
    } else {
        match decision {
            Decision::Observe {
                reason: "sim_gate_failed",
            } => Decision::Observe {
                reason: honest_observe(
                    "no_candidate",
                    aggregate.non_base_quote_dropped,
                    aggregate.best.is_some(),
                ),
            },
            Decision::Observe { reason } => Decision::Observe {
                reason: honest_observe(
                    reason,
                    aggregate.non_base_quote_dropped,
                    aggregate.best.is_some(),
                ),
            },
            d => d,
        }
    };
    FrameArtifacts {
        decision,
        requested_bid,
        submit_calldata,
        stages,
    }
}
