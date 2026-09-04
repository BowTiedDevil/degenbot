//! Solver-state tripwire — the ADR-021 D3 state-accuracy gate, one module.
//!
//! The solver's stored per-hop pool state is diffed against the canonical
//! on-chain state (the ADR-005 Option-A state-accuracy assertion).
//!
//! The mutation-nonce staleness gate (`candidate_is_stale`) verifies that a
//! pool's local state was *updated* — it proves the nonce changed, NOT that
//! the per-hop state handed to the solver/encoder *equals* the chain. If the
//! solver runs on a desynced snapshot (a missed log, an unhandled reorg, or a
//! directly-storage-mutated pool), it emits "impossible" `hop_outputs` and all
//! post-hoc (sim / balance) analysis is moot. This module closes that gap:
//! for each hop, `eth_call`s the canonical on-chain scalar state at the solve
//! block and diffs it against the `BotState` snapshot `resolve_path` consumed.
//!
//! Verified fields are the MUTABLE scalar state the solver predicts from:
//! V2 `(reserve0, reserve1)`, V3/V4 `(sqrtPriceX96, liquidity, tick)` — the
//! same fields the existing `liquidity_verifier` deliberately does NOT touch
//! (it only checks the immutable-init tick *map*).
//!
//! The module owns the whole pipeline behind [`judge`]: the dev-only
//! divergence dry-run scanner, the aggregated lagging-hop reporter, the
//! observational solve-anchor / staged-clock probes, and the strict per-hop
//! gate, and it discriminates the ADR-021 D2 defect classes
//! ([`TripwireClass`]). It reads NO environment: the pump packs the env
//! stances into [`TripwireConfig`] at construction (the Z4KQXF per-pump
//! opt-out pattern) and passes it per judgement. The caller keeps only the
//! trip and the exit: on [`GateVerdict::Divergent`] it prints the verdict
//! breadcrumb and aborts the process (ADR-021 D1: detect, classify, stop
//! loudly — never heal; the AV42C7 Option-A reaction).
//!
//! The anchor diff (each hop at its OWN `update_block`) catches reorgs /
//! same-block sub-tick corruption but tolerates normal latency — a pool honest
//! at an OLD anchor can still be a frozen snapshot whose on-chain price moved
//! past it (missed swap events). The staleness re-check closes that gap: for a
//! CL hop whose `update_block` trails the solve block by more than
//! [`MAX_CL_STALENESS_BLOCKS`], a FRESH solve-block read that still diverges is
//! reported as a stale/desynced hop (the PancakeSwap-V3 non-canonical-Swap-
//! topic0 root cause — see `docs/exploration-no-profit-crash.md`).

use hashbrown::HashMap;

use alloy::primitives::{Address, I256, U256};
use degenbot_rpc::abi::{fetch_v2_reserves, fetch_v3_slot0_liquidity, fetch_v4_slot0_liquidity};
use degenbot_rpc::provider::AlloyProvider;
use degenbot_solvers::mixed::{HopType, MixedPoolRef};

use super::liquidity_verifier::{verify_v3_pool, verify_v4_pool, LiquidityVerifyError};
use super::BotState;

/// The env-derived stances, packed by the pump at construction — the module
/// reads NO env itself (the Z4KQXF per-pump opt-out pattern: tests construct
/// the config directly, immune to the global env).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the four ADR-021 D3 stance flags (the gate + three default-off diagnostics) are the packed per-pump config (Z4KQXF) — one bool per env-named stance is the deliberate shape"
)]
pub struct TripwireConfig {
    /// Whether the gate runs at all (ex `DEGENBOT_ASSERT_SOLVER_STATE`,
    /// conservative default ON in production).
    pub enabled: bool,
    /// Dev-only, NON-ABORTING divergence dry-run scanner that short-circuits
    /// the strict gate (ex `DEGENBOT_SOLVER_DIVERGENCE_SCAN`, default off).
    /// Must never be enabled to silence the production abort.
    pub divergence_scan: bool,
    /// Observational solve-anchor consistency probe for lagging-outlier hops
    /// (ex `DEGENBOT_TRACE_SOLVE_ANCHOR`, default off).
    pub anchor_probe: bool,
    /// Observational staged-clock scalar probe for in-progress hops
    /// (ex `DEGENBOT_TRACE_STAGED_CLOCK`, default off).
    pub staged_clock_probe: bool,
    /// ADR-021 D2 Part B (settled policy): the delivery-lag trip threshold.
    /// `None` (default) = lag stays report-only (today's byte-identical trip
    /// set); `Some(N)` = a Tracked CL hop whose `stale_by` exceeds `N` at the
    /// solve block trips the gate as `DeliveryLag{blocks}` even though the
    /// state is honest at its own anchor. The strict gate still wins when it
    /// fires. Set from `DEGENBOT_DELIVERY_LAG_TRIP_BLOCKS` (unset = off).
    pub delivery_lag_trip_blocks: Option<u64>,
}

impl TripwireConfig {
    /// Gate ON, every observability probe OFF (the red/green unit-test shape).
    #[must_use]
    pub const fn enabled_only() -> Self {
        Self {
            enabled: true,
            divergence_scan: false,
            anchor_probe: false,
            staged_clock_probe: false,
            delivery_lag_trip_blocks: None,
        }
    }

    /// Every stance off (the test-constructor pump default, Z4KQXF).
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            divergence_scan: false,
            anchor_probe: false,
            staged_clock_probe: false,
            delivery_lag_trip_blocks: None,
        }
    }
}

/// The ADR-021 D2 defect class a divergent verdict names — a trip should
/// name the class, not just the scalar diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TripwireClass {
    /// A swap/liquidity event moved on-chain but was never applied before the
    /// solver consumed the pool (the strict gate's STALE flag: honest at the
    /// hop's own anchor, on-chain moved past the staleness threshold).
    MissedLog,
    /// A reorg was not rolled back before solve. Emitted by the reorg
    /// refinement (ADR-021 D2 Part A): a strict-gate own-anchor mismatch
    /// (coarse `StorageMutated`) is re-labelled to this class when a
    /// recorded [`TripReorgWindow`] rollback crossed the hop's anchor — a
    /// replaced historical block is the only mechanism that can move it.
    UnhandledReorg,
    /// On-chain storage diverges from the solver's stored state even AT the
    /// hop's own anchor — direct storage mutation, a decode bug, and an
    /// unhandled reorg are indistinguishable from the scalar reads (coarse
    /// by design).
    StorageMutated,
    /// State is honest at the hop's own anchor but trails the solve block by
    /// `blocks` (WS slow-but-connected). The aggregate reporter LOGS this
    /// class (WARN); by default nothing trips on it (the trip set is
    /// byte-for-byte today's). The config-gated `delivery_lag_trip_blocks`
    /// stance (ADR-021 D2 Part B) trips when the worst lag exceeds the
    /// operator threshold.
    DeliveryLag { blocks: u64 },
    /// A hard stop the coarse on-chain evidence cannot classify (the FUTURE
    /// guard, an `eth_call` read failure, a tick-map fidelity divergence).
    /// The divergence's message carries the detail.
    Unclassified,
}

impl TripwireClass {
    /// ADR-040 reason sub-key for the `failure_policy` matrix (the
    /// `solver_state_desync.<reason>` bucket identity).
    #[must_use]
    pub const fn reason_key(&self) -> &'static str {
        match self {
            Self::MissedLog => crate::telemetry::error_reason::MISSED_LOG,
            Self::UnhandledReorg => crate::telemetry::error_reason::UNHANDLED_REORG,
            Self::StorageMutated => crate::telemetry::error_reason::STORAGE_MUTATED,
            Self::DeliveryLag { .. } => crate::telemetry::error_reason::DELIVERY_LAG,
            Self::Unclassified => crate::telemetry::error_reason::UNCLASSIFIED,
        }
    }
}

/// One reorg window as observed by the pump (FSM
/// `EnterReorg`/`ContinueReorg`/`CloseReorg`) — the ADR-021 D2 Part A
/// evidence record the pump passes to [`judge`]. A rollback to
/// `deepest_removed_block` replaced every block strictly above it, so any
/// anchor `B > deepest_removed_block` sat on a now-discarded fork.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TripReorgWindow {
    /// The deepest (lowest) removed block seen in this window — the rollback
    /// replaced blocks `(deepest_removed_block, ...]`.
    pub deepest_removed_block: u64,
    /// The block number of the opening (first removed) log.
    pub opened_at: u64,
    /// The new head at which the window closed (first forward log); `None`
    /// while the window is still open.
    pub closed_at: Option<u64>,
}

/// ADR-040 / 52I5SV: the reproduction payload captured at trip time - the
/// judged anchor, the verdict identity, and the per-hop state diagnostic
/// so a deterministic follow-up test can be written from the artifact alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesyncStateSnapshot {
    /// The judged anchor block (the judged solve block).
    pub anchor_block: u64,
    /// Class reason sub-key (the `telemetry::error_reason` spelling).
    pub reason: &'static str,
    /// The failing path index.
    pub path_idx: usize,
    /// The failing hop index (within the path).
    pub hop_idx: usize,
    /// The full grep-able breadcrumb (embeds the stored-vs-chain scalar
    /// diff text the strict gate produced).
    pub breadcrumb: String,
    /// Per-hop state diagnostic as JSON; `Value::Null` when unavailable
    /// (the delivery-lag trip arm carries the aggregate instead).
    pub hops: serde_json::Value,
}

impl DesyncStateSnapshot {
    /// Serialize to the offline JSON artifact (pretty).
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&serde_json::json!({
            "schema": "degenbot-desync-dump/1",
            "anchor_block": self.anchor_block,
            "reason": self.reason,
            "path_idx": self.path_idx,
            "hop_idx": self.hop_idx,
            "breadcrumb": self.breadcrumb,
            "hops": self.hops,
        }))
        .unwrap_or_else(|_| String::new())
    }

    /// Write the artifact under `dir` (created on demand); a dump must never
    /// block the reaction, so I/O failure degrades to the caller.
    ///
    /// # Errors
    /// Propagates the underlying `std::io` error (dir create / file write).
    pub fn write(&self, dir: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
        std::fs::create_dir_all(dir)?;
        let file = dir.join(format!(
            "desync-{}-p{}h{}-{}.json",
            self.anchor_block, self.path_idx, self.hop_idx, self.reason
        ));
        std::fs::write(&file, self.to_json())?;
        Ok(file)
    }
}

/// Cap on the reorg windows retained as trip evidence (bounded memory under
/// reorg traffic; the oldest window evicts past this).
pub const TRIP_REORG_WINDOW_CAP: usize = 16;

/// One hard stop: which class, where, and the full grep-able diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TripwireDivergence {
    pub class: TripwireClass,
    pub path_idx: usize,
    pub hop_idx: usize,
    /// The full fatal diagnostic — the grep-able `[SOLVER-STATE] ABORT:`
    /// prefix kept byte-identical to the pre-extraction message, with the
    /// class appended. The caller prints it, then aborts.
    pub breadcrumb: String,
    /// ADR-040 / 52I5SV: reproduction payload written by the reacting seam.
    pub snapshot: DesyncStateSnapshot,
}

/// The verdict of one published-block judgement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateVerdict {
    /// No divergence at any hop (or the dev-only dry-run scanner
    /// short-circuited — it never trips).
    Ok,
    /// A verified desync — the caller must print `breadcrumb` and
    /// `abort()` the process (ADR-021 D1).
    Divergent(TripwireDivergence),
}

/// Judge one published block's change set against the chain — the ADR-021 D3
/// single-call interface. Owns the whole stage pipeline (scanner → reporter
/// → probes → strict gate → delivery-lag trip); never reads env; never aborts
/// (the caller does).
///
/// `path_hop_states` is the per-path scalar state extracted under the caller's
/// short read guard (the pump drops the guard BEFORE this awaits); `anchor` is
/// the pump-resolved [`crate::bot_core::solve_anchor::SolveAnchor`];
/// `reorg_evidence` is the pump's recorded reorg windows (ADR-021 D2 Part A)
/// — an own-anchor mismatch is re-labelled `UnhandledReorg` when a recorded
/// rollback crossed the hop's anchor (tests pass `&[]`).
pub async fn judge(
    provider: &AlloyProvider,
    config: &TripwireConfig,
    path_hop_states: &[Vec<SolverHopScalarState>],
    anchor: crate::bot_core::solve_anchor::SolveAnchor,
    reorg_evidence: &[TripReorgWindow],
) -> GateVerdict {
    let block = anchor.block();
    if path_hop_states.is_empty() {
        return GateVerdict::Ok;
    }
    // The anchor is resolved by the CALLER at the pool-state head (the rule
    // owner + full re-anchor history live in `crate::bot_core::solve_anchor`):
    // during a backfill/drain desync the pools sit AHEAD of the lagging
    // publish block, and a hop at the head is LIVE state, not a future price.
    if config.divergence_scan {
        // Stage 1 (dev-only) short-circuits the strict gate (today's bypass,
        // preserved as-is) — see `divergence_scan_stage`'s docs.
        divergence_scan_stage(provider, path_hop_states, block).await;
        return GateVerdict::Ok;
    }
    // Stage 2: the aggregated lagging-hop reporter (ADR-021 supplement;
    // report-only by default) — see `lagging_hop_report_stage`'s docs. Its
    // worst laggard feeds Stage 5 (the config-gated delivery-lag trip).
    let lag = lagging_hop_report_stage(block, path_hop_states);

    // Stage 3: the solve-anchor consistency probe (observational, config-
    // gated default off) — see `solve_anchor_probe_stage`'s docs.
    if config.anchor_probe {
        solve_anchor_probe_stage(provider, path_hop_states, block).await;
    }
    // Stage 4: the strict per-hop scalar gate — the primary trip (the reorg
    // evidence re-labels its own-anchor divergences; see
    // `reorg_refine_failure`).
    for (path_idx, hop_states) in path_hop_states.iter().enumerate() {
        if let Err(f) =
            verify_solver_hop_states(provider, config, hop_states, anchor, reorg_evidence).await
        {
            return GateVerdict::Divergent(breadcrumb_divergence(path_idx, f, hop_states, block));
        }
    }
    // Stage 5 (ADR-021 D2 Part B, settled 2026-08-20; config-gated, default
    // off): the strict gate PASSED — every hop honest at its own anchor — but
    // delivery itself trails the solve block past the operator threshold.
    // Below the threshold trailing is the designed mode (report-only);
    // beyond it the solver would price on a blind snapshot, so the verdict
    // is Divergent. The strict gate still wins above when it fired.
    if let (Some(limit), Some(l)) = (config.delivery_lag_trip_blocks, lag) {
        if l.stale_by > limit {
            return GateVerdict::Divergent(TripwireDivergence {
                class: TripwireClass::DeliveryLag {
                    blocks: l.stale_by,
                },
                path_idx: 0,
                hop_idx: 0,
                breadcrumb: format!(
                    "[SOLVER-STATE] ABORT: solver used desynced pool state at solve block {block} (path_idx=0, class=DeliveryLag {{ blocks: {} }}, hops: delivery-lag aggregate) Tracked Live CL hop pool {} is honest at its own anchor but trails the solve anchor by {} blocks (update_block={}) — past the operator-configured delivery-lag trip threshold {} (DEGENBOT_DELIVERY_LAG_TRIP_BLOCKS). This pool is DeliveryLag{{blocks}} at the solve block — delivery is slow-but-connected; solving would price on a stale snapshot. Do NOT reuse desynced state or silence this (UO3JM4).",
                    l.stale_by,
                    l.pool,
                    l.stale_by,
                    l.update_block,
                    limit
                ),
                snapshot: DesyncStateSnapshot {
                    anchor_block: block,
                    reason: TripwireClass::DeliveryLag { blocks: l.stale_by }.reason_key(),
                    path_idx: 0,
                    hop_idx: 0,
                    breadcrumb: String::new(),
                    hops: serde_json::Value::Null,
                },
            });
        }
    }
    GateVerdict::Ok
}

/// Stage 1 — the dev-only, non-aborting divergence scanner (dry-run low-MTBF
/// failure hunting). Logs every lagging Tracked CL hop's stored-vs-on-chain
/// fidelity (HONEST vs the rare REAL DIVERGENT desync) WITHOUT aborting, so
/// one long dry-run accumulates all genuine desyncs instead of dying on the
/// first. The UO3JM4 abort is bypassed ONLY while this opt-in is set — the
/// stage must never be enabled to silence the production abort (judge
/// short-circuits with `Ok` right after this stage).
async fn divergence_scan_stage(
    provider: &AlloyProvider,
    path_hop_states: &[Vec<SolverHopScalarState>],
    block: u64,
) {
    let mut seen = std::collections::HashSet::new();
    let mut uniq: Vec<&SolverHopScalarState> = Vec::new();
    for hop_states in path_hop_states {
        for hop in hop_states {
            // Dedupe by generalized identity (one on-chain read per unique
            // pool).
            let key = match hop.hop_type {
                HopType::V3 => hop.v3.as_ref().map(|(a, ..)| format!("v3:{a}")),
                HopType::V4 => hop.v4.as_ref().map(|(_, id, ..)| {
                    let mut h = String::with_capacity(8);
                    for b in &id[..4] {
                        let _ = std::fmt::Write::write_fmt(&mut h, format_args!("{b:02x}"));
                    }
                    format!("v4:{h}")
                }),
                _ => None,
            };
            if let Some(k) = key {
                if seen.insert(k) {
                    uniq.push(hop);
                }
            }
        }
    }
    let mut n_scanned = 0usize;
    let mut n_honest = 0usize;
    let mut n_divergent = 0usize;
    for r in scan_lagging_hops_for_divergence(provider, &uniq, block).await {
        n_scanned += 1;
        if let Some(p) = crate::instruments::pipeline() {
            p.count_solver_state_check();
        }
        match r.verdict {
            DivergenceVerdict::Honest => {
                n_honest += 1;
                tracing::debug!(
                    hop_idx = r.hop_index,
                    %r.pool,
                    update_block = r.update_block,
                    tick_data_block = r.tick_data_block,
                    stale_by = r.stale_by,
                    "solver-state divergence scan: HONEST (quiet-but-correct)"
                );
            }
            DivergenceVerdict::Divergent => {
                n_divergent += 1;
                if let Some(p) = crate::instruments::pipeline() {
                    p.count_error(crate::telemetry::error_kind::SOLVER_STATE_DESYNC);
                }
                tracing::error!(
                    hop_idx = r.hop_index,
                    %r.pool,
                    block,
                    update_block = r.update_block,
                    tick_data_block = r.tick_data_block,
                    stale_by = r.stale_by,
                    solver_sqrt = ?r.solver_sqrt,
                    chain_sqrt = ?r.chain_sqrt,
                    "solver-state divergence scan: REAL-DESYNC — pool moved on-chain \
                     but its stored state was not advanced (missed/delayed swap)."
                );
            }
        }
    }
    if n_scanned > 0 {
        tracing::info!(
            block,
            n_scanned,
            n_honest,
            n_divergent,
            "solver-state divergence scan: summary"
        );
    }
}

/// Stage 2 — the generalized lagging-hop reporter (ADR-021 supplement),
/// AGGREGATED. BEFORE the strict gate, surfaces ANY Tracked Live CL hop whose
/// `update_block` trails the promoted solve anchor past
/// `MAX_CL_STALENESS_BLOCKS` — the ADR-021 D2 `DeliveryLag{blocks}`
/// population. Pool-agnostic; observational only — does NOT weaken the
/// UO3JM4 strict gate.
///
/// The natural WS settle lag (~4 blocks) sits just past the threshold, so
/// a naive per-hop WARN re-reported the SAME benign pools ~100× per block
/// across paths (≈143 WARNs/block, 0 desyncs in a 5k-WARN run). We
/// therefore aggregate across all paths into ONE per-block summary (dedupe
/// per pool, keep max `stale_by`) and WARN individually only for genuine
/// outliers (`stale_by >= SOLVER_STATE_ABNORMAL_STALE_BLOCKS`, well above
/// the baseline); the quiet-but-benign bulk stays at `DEBUG`.
fn lagging_hop_report_stage(
    block: u64,
    path_hop_states: &[Vec<SolverHopScalarState>],
) -> Option<LaggingHop> {
    let lags_by_pool = aggregate_lagging_hops(block, path_hop_states);
    let worst = lags_by_pool.values().max_by_key(|l| l.stale_by).cloned();
    if !lags_by_pool.is_empty() {
        let n_pools = lags_by_pool.len();
        let max_stale_by = lags_by_pool.values().map(|l| l.stale_by).max().unwrap_or(0);
        tracing::warn!(
            block,
            n_pools,
            max_stale_by,
            "[solver-state] Tracked Live CL hop pools trail the solve anchor past the staleness \
             threshold (summarized — genuine-outlier pools logged below, benign settle-lag \
             baseline at debug)"
        );
        for lag in lags_by_pool.values() {
            let update_block = lag.update_block;
            let tick_data_block = lag.tick_data_block;
            let stale_by = lag.stale_by;
            let pool = &lag.pool;
            if lag.stale_by >= SOLVER_STATE_ABNORMAL_STALE_BLOCKS {
                tracing::warn!(
                    block,
                    hop_type = ?lag.hop_type,
                    coverage = %lag.coverage,
                    lifecycle = %lag.lifecycle,
                    update_block,
                    tick_data_block,
                    stale_by,
                    %pool,
                    "[solver-state] DeliveryLag{{blocks}} outlier: Tracked Live CL hop pool \
                     is a genuine staleness outlier (well past the natural settle lag) — the \
                     strict gate may escalate this to an abort"
                );
            } else {
                tracing::debug!(
                    block,
                    hop_type = ?lag.hop_type,
                    coverage = %lag.coverage,
                    lifecycle = %lag.lifecycle,
                    update_block,
                    tick_data_block,
                    stale_by,
                    %pool,
                    "[solver-state] Tracked Live CL hop pool trails the solve anchor \
                     (benign settle-lag baseline, folded into the summary above)"
                );
            }
        }
    }
    worst
}

/// Stage 3 — the solve-anchor consistency probe (observational; config-gated
/// default off): discriminates the lagging-outlier population into QUIET
/// (benign-inactive — correct-but-old) vs MOVED-IN-BLOCK-NOT-APPLIED (on-chain
/// moved AT the solve anchor but the pool's in-block Swap was not applied
/// before the solver consumed it — the improper header-promote-ahead-of-
/// apply transition behind the 0x99ac8c abort). NON-aborting and does NOT
/// bypass the production abort — it accumulates observability so the ordering
/// race isn't only visible as a process kill. Only abnormal outliers are
/// read, one read per unique pool per anchor, so cost is bounded.
async fn solve_anchor_probe_stage(
    provider: &AlloyProvider,
    path_hop_states: &[Vec<SolverHopScalarState>],
    block: u64,
) {
    // D2 classification pre-scan (pure metadata, no reads): how many hops
    // are Laggards vs Consensus relative to the solve anchor — the probe
    // below reads only the laggards.
    let (mut laggards, mut consensus) = (0u64, 0u64);
    for hs in path_hop_states {
        for hop in hs {
            if hop.update_block == 0 || hop.update_block >= block {
                continue;
            }
            match solve_anchor_advancement(
                hop.update_block,
                block,
                SOLVER_STATE_ABNORMAL_STALE_BLOCKS,
            ) {
                SolveAnchorAdvancement::Laggard { .. } => laggards += 1,
                SolveAnchorAdvancement::Consensus => consensus += 1,
            }
        }
    }
    tracing::debug!(
        block,
        laggards,
        consensus,
        cutoff = SOLVER_STATE_ABNORMAL_STALE_BLOCKS,
        "solve-anchor advance classification (pre-probe)"
    );
    let probe = probe_solve_anchor_consistency(
        provider,
        path_hop_states,
        block,
        SOLVER_STATE_ABNORMAL_STALE_BLOCKS,
    )
    .await;
    for r in probe {
        let verdict = match r.verdict {
            LaggardProbeVerdict::Quiet => "QUIET (benign-inactive, correct-but-old)",
            LaggardProbeVerdict::MovedInBlockNotApplied => {
                "MOVED-IN-BLOCK-NOT-APPLIED (on-chain moved at the solve anchor but the \
                 in-block Swap was not applied before solve — header-promote-ahead-of-apply)"
            }
            LaggardProbeVerdict::ReadFailed => "READ-FAILED (unclassified)",
        };
        tracing::warn!(
            block,
            hop_type = ?r.hop_type,
            %r.pool,
            update_block = r.update_block,
            tick_data_block = r.tick_data_block,
            stale_by = r.stale_by,
            %verdict,
            "[solve-anchor-probe] lagging Tracked Live CL hop classified at solve anchor"
        );
    }
}

/// Format the full fatal breadcrumb (ADR-021 D1: stop loudly with enough
/// breadcrumb to name the defect class). The literal prefix
/// `[SOLVER-STATE] ABORT: solver used desynced pool state at solve block
/// {block} (path_idx={path_idx}` is byte-identical to the pre-extraction
/// message; the D2 class is appended after it. The OB7UNY two-stamp
/// diagnostic surfaces BOTH per-hop clocks so a staged-clock desync (fresh
/// price, stale tick map — the `0x5653` class the scalar-only diff cannot
/// see) stays visible in the fatal message.
#[expect(
    clippy::needless_pass_by_value,
    reason = "Copy usize used twice (struct field + breadcrumb format); a &usize triggers the inverse trivially_copy_pass_by_ref"
)]
fn breadcrumb_divergence(
    path_idx: usize,
    f: GateFailure,
    hop_states: &[SolverHopScalarState],
    block: u64,
) -> TripwireDivergence {
    let hops_diag: Vec<String> = hop_states
        .iter()
        .filter(|h| h.update_block != 0)
        .map(|h| {
            let meta = h
                .cl_meta
                .as_ref()
                .map(|(c, l)| format!(" cov={c}, lifecycle={l}"))
                .unwrap_or_default();
            format!(
                "hop {:?} update_block={} tick_data_block={} stale_by={}{}",
                h.hop_type,
                h.update_block,
                h.tick_data_block,
                block.saturating_sub(h.update_block),
                meta
            )
        })
        .collect();
    let breadcrumb = format!(
        "[SOLVER-STATE] ABORT: solver used desynced pool state at solve block {block} \
             (path_idx={path_idx}, class={:?}, hops: {}) {}. Do NOT reuse desynced state or \
             silence this (UO3JM4).",
        f.class,
        hops_diag.join(", "),
        f.message,
    );
    TripwireDivergence {
        class: f.class,
        path_idx,
        hop_idx: f.hop_idx,
        breadcrumb: breadcrumb.clone(),
        snapshot: DesyncStateSnapshot {
            anchor_block: block,
            reason: f.class.reason_key(),
            path_idx,
            hop_idx: f.hop_idx,
            breadcrumb: breadcrumb.clone(),
            hops: serde_json::Value::Array(
                hop_states
                    .iter()
                    .map(|h| {
                        serde_json::json!({
                            "hop_type": format!("{:?}", h.hop_type),
                            "update_block": h.update_block,
                            "tick_data_block": h.tick_data_block,
                            "v2": h.v2.as_ref().map(|(a, r0, r1)| {
                                serde_json::json!({"pair": format!("{a}"), "reserve0": r0.to_string(), "reserve1": r1.to_string()})
                            }),
                            "v3": h.v3.as_ref().map(|(a, sq, liq, tick)| {
                                serde_json::json!({"pool": format!("{a}"), "sqrt_price_x96": sq.to_string(), "liquidity": liq, "tick": tick})
                            }),
                            "v4": h.v4.as_ref().map(|(pm, pid, sv, sq, liq, tick)| {
                                serde_json::json!({"pool_manager": format!("{pm}"), "pool_id": alloy::primitives::hex::encode(pid), "state_view": format!("{sv}"), "sqrt_price_x96": sq.to_string(), "liquidity": liq, "tick": tick})
                            }),
                            "cl_meta": h.cl_meta.as_ref().map(|(c, l)| serde_json::json!({"coverage": c, "lifecycle": l})),
                        })
                    })
                    .collect(),
            ),
        },
    }
}

/// The solver's stored per-hop scalar state, pre-extracted from `BotState`
/// so the raw (non-`Clone`, non-`Send`) state can be dropped BEFORE the async
/// on-chain reads begin — the pump must not hold a `parking_lot` read guard
/// across an `.await`.
#[derive(Debug, Clone)]
pub struct SolverHopScalarState {
    pub hop_type: HopType,
    /// The solver's stored `update_block` for this hop's pool — the block its
    /// scalar state was last advanced to. Diffing this against the solve block
    /// discriminates the two mismatch classes: pure WS latency
    /// (`update_block < block`) vs. a same-block sub-tick corruption
    /// (`update_block == block` yet sqrtPrice/liquidity/tick diverges).
    pub update_block: u64,
    /// The **liquidity** clock (`tick_data_block`, two-stamp OB7UNY) — the
    /// block this hop's tick map reflects. A CL hop whose `tick_data_block`
    /// lags its `update_block` is the staged-clock desync class (`0x5653`: a
    /// fresh price but a stale liquidity map) that the scalar-only ADR-021
    /// anchor diff cannot see — this surface makes it observable. Falls back
    /// to `update_block` for families with no tick-data clock and to `0` for
    /// an unregistered hop.
    pub tick_data_block: u64,
    /// CL hop registration metadata (CL pools only; `None` for V2):
    /// `(coverage, lifecycle)` as Debug strings — `Tracked`/`Sparse` and
    /// `Live`/`Quarantined`. A pool frozen far behind the solve block that is
    /// `Sparse`/`Quarantined` is a path-referenced pool the live pump never
    /// applies — the state is pinned at its seed, not a live-swap drop.
    pub cl_meta: Option<(String, String)>,
    /// V2: `(pair, reserve0, reserve1)`.
    pub v2: Option<(Address, U256, U256)>,
    /// V3: `(pool, sqrt_price_x96, liquidity, tick)`.
    pub v3: Option<(Address, U256, u128, i32)>,
    /// V4: `(pool_manager, pool_id, state_view, sqrt_price_x96, liquidity, tick)`.
    pub v4: Option<(Address, [u8; 32], Address, U256, u128, i32)>,
    /// The solver's **live CL tick map**, cloned out of `BotState` under the
    /// short read-guard so the solve-time tick-map fidelity probe
    /// ([`probe_cl_tick_map_fidelity`] → `verify_v3_pool` / `verify_v4_pool`)
    /// can run after the guard is dropped. `None` for V2/non-CL hops.
    ///
    /// The scalar gate ([`verify_solver_hop_states`] scalar diff) is blind to
    /// a snapshot whose tick map is missing positions entirely OFF the active
    /// range: active liquidity (`Σ net ≤ current tick`) is unchanged by a
    /// missing off-range position, yet a solve that CROSSES into the missing
    /// region under-counts liquidity and over-predicts output (the UO3JM4
    /// thin-tick-map class — ADR-021 D3). This map is the surface that makes
    /// that class observable at solve time.
    pub cl_tick_map: Option<ClTickMapSnapshot>,
}

/// A CL hop's solver-stored tick map, cloned under the short read-guard (see
/// [`SolverHopScalarState::cl_tick_map`]). The probe verifies it against
/// on-chain at the hop's **tick-data anchor** (`tick_data_block` — NOT the
/// price clock `update_block`, OB7UNY), because the map reflects the
/// liquidity clock. Verified against the tick-data anchor the map claims to
/// reflect, a 1-2 block price lag or a never-advanced liquidity clock does
/// not false-trip; only a genuinely desynced map (missed Mint/Burn, ghost
/// tick, decode bug) diverges.
#[derive(Clone, Debug)]
pub struct ClTickMapSnapshot {
    /// Tick spacing (immutable — lets the probe compress ticks into bitmap
    /// words).
    pub tick_spacing: i32,
    /// The solver's current active tick — seeds the ±2-word bitmap scan.
    pub active_tick: i32,
    /// The tick bookkeeping map: tick index → `(liquidity_gross, liquidity_net)`.
    pub tick_data: HashMap<i32, degenbot_pools::TickInfo>,
}

/// Extract the solver's stored per-hop scalar state from `BotState` (the
/// states `resolve_path` consumed). Run this INSIDE the core read-guard scope;
/// return its result, drop the guard, then pass to
/// [`verify_solver_hop_states`]. Hops whose family is not a CL/V2 scalar diff
/// (Solidly / Balancer / Curve) or whose state is missing are skipped.
#[must_use]
pub(crate) fn extract_solver_hop_states(
    core: &BotState,
    pools: &[MixedPoolRef],
) -> Vec<SolverHopScalarState> {
    pools
        .iter()
        .map(|pool_ref| {
            let cl_meta = match pool_ref.hop_type {
                HopType::V3 => core.get_v3_pool(pool_ref.pool_key).map(|s| {
                    (
                        format!("{:?}", s.coverage),
                        format!("{:?}", s.registration_lifecycle),
                    )
                }),
                HopType::V4 => core.get_v4_pool(pool_ref.pool_key).map(|s| {
                    (
                        format!("{:?}", s.coverage),
                        format!("{:?}", s.registration_lifecycle),
                    )
                }),
                _ => None,
            };
            SolverHopScalarState {
                hop_type: pool_ref.hop_type,
                update_block: core.pool_update_block(pool_ref.pool_key),
                tick_data_block: core.pool_tick_data_block(pool_ref.pool_key),
                cl_meta,
                v2: match pool_ref.hop_type {
                    HopType::V2 => core
                        .get_v2_pool_state(pool_ref.pool_key)
                        .zip(core.get_v2_identity(pool_ref.pool_key))
                        .map(|(state, identity)| {
                            (
                                identity.address,
                                U256::from(state.reserve0),
                                U256::from(state.reserve1),
                            )
                        }),
                    _ => None,
                },
                v3: match pool_ref.hop_type {
                    HopType::V3 => core
                        .get_v3_pool(pool_ref.pool_key)
                        .zip(core.get_v3_identity(pool_ref.pool_key))
                        .map(|(state, identity)| {
                            (
                                identity.address,
                                state.sqrt_price_x96,
                                state.liquidity,
                                state.tick,
                            )
                        }),
                    _ => None,
                },
                v4: match pool_ref.hop_type {
                    HopType::V4 => core
                        .get_v4_pool(pool_ref.pool_key)
                        .zip(core.get_v4_identity(pool_ref.pool_key))
                        .and_then(|(state, identity)| {
                            core.state_view_for(identity.pool_manager)
                                .map(|state_view| {
                                    (
                                        identity.pool_manager,
                                        identity.pool_id,
                                        state_view,
                                        state.sqrt_price_x96,
                                        state.liquidity,
                                        state.tick,
                                    )
                                })
                        }),
                    _ => None,
                },
                cl_tick_map: match pool_ref.hop_type {
                    HopType::V3 => core
                        .get_v3_pool(pool_ref.pool_key)
                        .zip(core.get_v3_identity(pool_ref.pool_key))
                        .map(|(state, identity)| ClTickMapSnapshot {
                            tick_spacing: identity.tick_spacing,
                            active_tick: state.tick,
                            tick_data: state.tick_data.clone(),
                        }),
                    HopType::V4 => core
                        .get_v4_pool(pool_ref.pool_key)
                        .zip(core.get_v4_identity(pool_ref.pool_key))
                        .map(|(state, identity)| ClTickMapSnapshot {
                            tick_spacing: identity.pool_key.tick_spacing,
                            active_tick: state.tick,
                            tick_data: state.tick_data.clone(),
                        }),
                    _ => None,
                },
            }
        })
        .collect()
}

/// Whether a V2 pair's solver-stored reserves match the on-chain reserves.
/// `reserve0`/`reserve1` are uint112 on-chain; compared exactly.
#[must_use]
pub fn v2_state_matches(
    solver_reserve0: U256,
    solver_reserve1: U256,
    chain_reserve0: U256,
    chain_reserve1: U256,
) -> bool {
    solver_reserve0 == chain_reserve0 && solver_reserve1 == chain_reserve1
}

/// Whether a V3/V4 pool's solver-stored CL scalar state matches the on-chain
/// `slot0`+`liquidity` state. The solver's predictions are a deterministic
/// function of `(sqrtPriceX96, liquidity, tick)`, so an exact diff of all
/// three is the correct accuracy oracle.
#[must_use]
pub fn cl_state_matches(
    solver_sqrt_price_x96: U256,
    solver_liquidity: u128,
    solver_tick: i32,
    chain_sqrt_price_x96: U256,
    chain_liquidity: U256,
    chain_tick: I256,
) -> bool {
    solver_sqrt_price_x96 == chain_sqrt_price_x96
        && U256::from(solver_liquidity) == chain_liquidity
        && solver_tick == chain_tick.as_i32()
}

/// A **Tracked** concentrated-liquidity (V3/V4) hop whose stored `update_block`
/// trails the promoted solve anchor past `MAX_CL_STALENESS_BLOCKS`. This is the
/// exact pre-condition the strict gate (`verify_solver_hop_states`) escalates to
/// a fatal `SOLVER-STATE` ABORT once the fresh on-chain read confirms a move.
/// The reporter surfaces it NON-fatally and GENERALIZED — the pool identity is
/// read from the hop itself (never a pool literal), so lag is observable on ANY
/// participating pool, not just the one that happens to abort.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LaggingHop {
    /// Index of the hop within its path.
    pub hop_index: usize,
    /// V3 or V4.
    pub hop_type: HopType,
    /// Registration coverage (`Tracked`) — always `Tracked` by construction.
    pub coverage: String,
    /// Registration lifecycle (`Live` / `Quarantined`).
    pub lifecycle: String,
    /// The pool's stored price-clock block.
    pub update_block: u64,
    /// The pool's stored tick-data (liquidity) clock (two-stamp OB7UNY).
    pub tick_data_block: u64,
    /// `anchor - update_block` (how far the pool trails the solve anchor).
    pub stale_by: u64,
    /// Generalized pool identity: `v3:<addr>` or `v4:0x<leading-hex>…`.
    pub pool: String,
}

/// Whether a Tracked Live CL hop trails the solve anchor past the staleness
/// threshold — the shared predicate behind [`lagging_tracked_hops`] (the
/// non-aborting reporter) and the divergence scanner. Excludes non-CL hops,
/// Sparse/Quarantined pools, never-updated hops, and at-or-within-threshold lag.
#[must_use]
fn is_lagging_tracked(hop: &SolverHopScalarState, anchor: u64) -> bool {
    let Some((coverage, lifecycle)) = hop.cl_meta.as_ref() else {
        return false; // non-CL hop (e.g. V2)
    };
    if coverage != "Tracked" || lifecycle != "Live" {
        // Sparse pools and quarantined pools are seeded / taken out of the
        // active solving set by design — expected to lag, never solved against
        // by intent. Only Live-Tracked pools drive real solves.
        return false;
    }
    if hop.update_block == 0 {
        return false; // never updated — the gate verifies it at the solve block
    }
    anchor.saturating_sub(hop.update_block) > cl_staleness_threshold_blocks()
}

/// Report every **Tracked** CL hop whose `update_block` trails `anchor` past
/// [`MAX_CL_STALENESS_BLOCKS`] — the same threshold the strict gate treats as
/// suspicious. Generalized: the pool identity is derived from each hop's own
/// `v3`/`v4` fields, never a hardcoded pool. Observational only — it does NOT
/// weaken the UO3JM4 abort, and it deliberately skips:
/// - non-CL hops (`cl_meta == None`, e.g. V2) — reserve/AMM scalar diff out of scope;
/// - `Sparse` / `Quarantined` hops — seeded/never-updated by design, expected to lag;
/// - never-updated hops (`update_block == 0`) — verified at the solve block instead;
/// - hops at/within the threshold (`stale_by <= MAX_CL_STALENESS_BLOCKS`) — normal latency.
///
/// # Panics
///
/// Panics if a hop that passes [`is_lagging_tracked`] carries `cl_meta == None`
/// (the invariant guarantees every lagging CL hop has CL metadata; the `expect`
/// below is the canary for that invariant).
#[must_use]
fn lagging_tracked_hops(anchor: u64, hops: &[SolverHopScalarState]) -> Vec<LaggingHop> {
    hops.iter()
        .enumerate()
        .filter_map(|(hop_index, h)| {
            if !is_lagging_tracked(h, anchor) {
                return None;
            }
            #[expect(clippy::expect_used)]
            // invariant: is_lagging_tracked guarantees cl_meta is Some
            let (coverage, lifecycle) = h.cl_meta.as_ref().expect("filtered above");
            let pool = match h.hop_type {
                HopType::V3 => h.v3.as_ref().map(|(addr, ..)| format!("v3:{addr}")),
                HopType::V4 => h.v4.as_ref().map(|(_, id, ..)| {
                    let mut head = String::with_capacity(8);
                    for b in &id[..4] {
                        let _ = std::fmt::Write::write_fmt(&mut head, format_args!("{b:02x}"));
                    }
                    format!("v4:0x{head}…")
                }),
                // Only CL hops have a staleness-relevant scalar clock; other hop
                // families (Solidly/Balancer/Curve) have no `cl_meta` and are
                // already filtered above, but this arm keeps the match total.
                _ => return None,
            }?;
            Some(LaggingHop {
                hop_index,
                hop_type: h.hop_type,
                coverage: coverage.clone(),
                lifecycle: lifecycle.clone(),
                update_block: h.update_block,
                tick_data_block: h.tick_data_block,
                stale_by: anchor.saturating_sub(h.update_block),
                pool,
            })
        })
        .collect()
}

/// Aggregate [`lagging_tracked_hops`] across every path into one per-pool map,
/// keeping each pool's max `stale_by`.
///
/// A single pool typically appears in many paths and re-lags every block it
/// stays behind the anchor, so reporting each [`LaggingHop`] verbatim floods the
/// log with the same benign WS settle-lag (~4 blocks) over and over (~143
/// WARNs/block across paths in a real run). This dedupe collapses that to one
/// entry per pool (max staleness), which the pump turns into a single per-block
/// summary plus outlier WARNs.
#[must_use]
fn aggregate_lagging_hops(
    anchor: u64,
    path_hop_states: &[Vec<SolverHopScalarState>],
) -> HashMap<String, LaggingHop> {
    let mut by_pool: HashMap<String, LaggingHop> = HashMap::new();
    for lag in path_hop_states
        .iter()
        .flat_map(|hs| lagging_tracked_hops(anchor, hs))
    {
        by_pool
            .entry(lag.pool.clone())
            .and_modify(|existing| {
                if lag.stale_by > existing.stale_by {
                    *existing = lag.clone();
                }
            })
            .or_insert_with(|| lag.clone());
    }
    by_pool
}

// The dev-only divergence-scan stance now lives on
// [`TripwireConfig::divergence_scan`] — the module reads no env (the pump
// packs it at construction, the Z4KQXF per-pump pattern).

/// Verdict of a single divergence scan on one lagging Tracked CL hop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DivergenceVerdict {
    /// Stored state matches on-chain at the solve block (quiet-but-correct).
    Honest,
    /// Stored state differs from on-chain at the solve block — a REAL desync.
    Divergent,
}

/// Result of scanning one lagging Tracked CL hop.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DivergenceScanResult {
    pub hop_index: usize,
    /// Generalized pool identity (`v3:<addr>` / `v4:0x<leading-hex>…`).
    pub pool: String,
    pub update_block: u64,
    pub tick_data_block: u64,
    pub stale_by: u64,
    pub verdict: DivergenceVerdict,
    /// The solver's stored sqrt (V3/V4) when available.
    pub solver_sqrt: Option<U256>,
    /// On-chain sqrt at the solve block when the read succeeded.
    pub chain_sqrt: Option<U256>,
}

/// Non-aborting divergence scanner: for each lagging Tracked Live CL hop (shared
/// [`is_lagging_tracked`] predicate), read on-chain at the solve `block` and
/// compare against the stored scalar. Returns one [`DivergenceScanResult`] per
/// hop (honest / divergent) without ever aborting — the dry-run low-MTBF window
/// onto the rare real desync. An RPC read that fails is skipped (best-effort; a
/// transport hiccup must not mis-flag a pool), per the liquidity-verifier `Rpc`
/// contract.
async fn scan_lagging_hops_for_divergence(
    provider: &AlloyProvider,
    hops: &[&SolverHopScalarState],
    block: u64,
) -> Vec<DivergenceScanResult> {
    let mut out = Vec::new();
    for (hop_index, hop) in hops.iter().enumerate() {
        if !is_lagging_tracked(hop, block) {
            continue;
        }
        let stale_by = block.saturating_sub(hop.update_block);
        match hop.hop_type {
            HopType::V3 => {
                let Some((pool, s_sqrt, s_liq, s_tick)) = hop.v3 else {
                    continue;
                };
                let result =
                    |verdict: DivergenceVerdict, chain_sqrt: Option<U256>| DivergenceScanResult {
                        hop_index,
                        pool: format!("v3:{pool}"),
                        update_block: hop.update_block,
                        tick_data_block: hop.tick_data_block,
                        stale_by,
                        verdict,
                        solver_sqrt: Some(s_sqrt),
                        chain_sqrt,
                    };
                if let Ok((c_sqrt, c_tick, c_liq)) =
                    fetch_v3_slot0_liquidity(provider, &pool, Some(block)).await
                {
                    let honest = cl_state_matches(s_sqrt, s_liq, s_tick, c_sqrt, c_liq, c_tick);
                    out.push(result(
                        if honest {
                            DivergenceVerdict::Honest
                        } else {
                            DivergenceVerdict::Divergent
                        },
                        Some(c_sqrt),
                    ));
                }
            }
            HopType::V4 => {
                let Some((_pm, pool_id, state_view, s_sqrt, s_liq, s_tick)) = hop.v4 else {
                    continue;
                };
                let mut head = String::with_capacity(8);
                for b in &pool_id[..4] {
                    let _ = std::fmt::Write::write_fmt(&mut head, format_args!("{b:02x}"));
                }
                let result =
                    |verdict: DivergenceVerdict, chain_sqrt: Option<U256>| DivergenceScanResult {
                        hop_index,
                        pool: format!("v4:0x{head}…"),
                        update_block: hop.update_block,
                        tick_data_block: hop.tick_data_block,
                        stale_by,
                        verdict,
                        solver_sqrt: Some(s_sqrt),
                        chain_sqrt,
                    };
                if let Ok((c_sqrt, c_tick, _pf, _lp, c_liq)) =
                    fetch_v4_slot0_liquidity(provider, &state_view, &pool_id, Some(block)).await
                {
                    let honest = cl_state_matches(s_sqrt, s_liq, s_tick, c_sqrt, c_liq, c_tick);
                    out.push(result(
                        if honest {
                            DivergenceVerdict::Honest
                        } else {
                            DivergenceVerdict::Divergent
                        },
                        Some(c_sqrt),
                    ));
                }
            }
            _ => {}
        }
    }
    out
}

/// The solver's anchor block to diff a hop against: its own `update_block`
/// when the pool has been updated (`> 0`), else the solve block (a never-
/// updated pool is verified at the solve block for want of a better anchor).
#[must_use]
fn solver_anchor_block(update_block: u64, block: u64) -> u64 {
    if update_block > 0 {
        update_block
    } else {
        block
    }
}

/// FSM-seed classifier: is this CL pool's scalar state in-consensus with the
/// solve `anchor` (i.e. may the pump legitimately SOLVE through it at `anchor`)?
///
/// This is the pure, unit-testable seed of the solve-anchor FSM the tracing
/// suite feeds. A hop whose `update_block` is `> 0` and trails `anchor` by
/// more than `tol` is a [`SolveAnchorAdvancement::Laggard`] — solving at
/// `anchor` consumes a state older than the block being solved, which is the
/// header-promote-ahead-of-apply transition (the `0x99ac8c` abort: a quiet pool
/// whose in-block Swap was not applied before the solver consumed the pool). A
/// hop at/within `tol`, or never-updated (`0`, verified at the anchor instead),
/// is [`SolveAnchorAdvancement::Consensus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SolveAnchorAdvancement {
    /// The pool has advanced to/at the anchor or within the normal settle
    /// tolerance — solving through it at `anchor` is a legal transition.
    Consensus,
    /// The pool trails the anchor past the tolerance — a SOLVE through it at
    /// `anchor` would consume pre-`anchor` state. `stale_by` is how far behind.
    Laggard { stale_by: u64 },
}

/// Classify a hop's scalar advancement relative to the solve `anchor`. Pure
/// (no on-chain read, no env) so it is directly unit-testable. Mirrors
/// [`is_lagging_tracked`]'s tolerance notion but is hop-metadata-free: it takes
/// the raw `update_block` + `anchor` + `tol`, so a test can drive it directly.
#[must_use]
fn solve_anchor_advancement(update_block: u64, anchor: u64, tol: u64) -> SolveAnchorAdvancement {
    if update_block > 0 && anchor.saturating_sub(update_block) > tol {
        SolveAnchorAdvancement::Laggard {
            stale_by: anchor.saturating_sub(update_block),
        }
    } else {
        SolveAnchorAdvancement::Consensus
    }
}

/// A lagging hop's solve-anchor probe verdict — the discrimination the plain
/// lagging-hop reporter cannot make ([`lagging_tracked_hops`] only says *how* a
/// Tracked hop trails the anchor, not *whether on-chain moved at the anchor*).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaggardProbeVerdict {
    /// On-chain scalar state at the anchor MATCHES the solver's stored state —
    /// a benign-inactive pool (correct-but-old). NOT the defect.
    Quiet,
    /// On-chain scalar state at the anchor DIVERGES from the solver's stored
    /// state — on-chain moved AT the solve block but the in-block Swap was not
    /// applied before the solver consumed the pool. The improper
    /// header-promote-ahead-of-apply transition (`0x99ac8c`).
    MovedInBlockNotApplied,
    /// The on-chain read failed (transport hiccup) — classify as unknown so a
    /// transient read error is never mis-flagged as Quiet or Moved.
    ReadFailed,
}

/// One lagging hop's solve-anchor probe result.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SolveAnchorProbeResult {
    /// Generalized pool identity (`v3:<addr>` / `v4:0x<leading-hex>…`).
    pub pool: String,
    pub hop_type: HopType,
    pub update_block: u64,
    pub tick_data_block: u64,
    pub stale_by: u64,
    pub verdict: LaggardProbeVerdict,
}

// The solve-anchor probe stance now lives on [`TripwireConfig::anchor_probe`]
// — the module reads no env.

/// The `stale_by` cutoff that distinguishes a *genuine* lagging outlier (worth a
/// solve-anchor read to classify) from the benign WS settle-lag baseline — the
/// single home shared by the aggregated reporter and the solve-anchor probe
/// (they must agree on what is "abnormal").
const SOLVER_STATE_ABNORMAL_STALE_BLOCKS: u64 = 10;

/// The solve-anchor consistency probe. For every genuinely-abnormal lagging
/// Tracked Live CL hop (`stale_by >= abnormal_tol`), read on-chain at the solve
/// `anchor` and classify it ([`LaggardProbeVerdict`]). This surfaces the one
/// transition the existing reporter cannot observe — *did on-chain move AT the
/// solve block while the pool's in-block Swap was not yet applied?* — WITHOUT
/// aborting (unlike the strict gate) and WITHOUT bypassing the abort (unlike the
/// dev-only `DIVERGENCE_SCAN`). In-progress (`update_block >= anchor`) and
/// never-updated hops are skipped; a failed read is `ReadFailed` (never
/// mis-classified). Observational only — the production abort runs regardless.
async fn probe_solve_anchor_consistency(
    provider: &AlloyProvider,
    path_hop_states: &[Vec<SolverHopScalarState>],
    anchor: u64,
    abnormal_tol: u64,
) -> Vec<SolveAnchorProbeResult> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for hop_states in path_hop_states {
        for hop in hop_states {
            if hop.update_block >= anchor || hop.update_block == 0 {
                continue; // in-progress or never-updated
            }
            if !is_lagging_tracked(hop, anchor) {
                continue;
            }
            let stale_by = anchor.saturating_sub(hop.update_block);
            if stale_by < abnormal_tol {
                continue; // benign settle-lag baseline, not worth a read
            }
            let pool = match hop.hop_type {
                HopType::V3 => hop.v3.as_ref().map(|(a, ..)| format!("v3:{a}")),
                HopType::V4 => hop.v4.as_ref().map(|(_, id, ..)| {
                    let mut head = String::with_capacity(8);
                    for b in &id[..4] {
                        let _ = std::fmt::Write::write_fmt(&mut head, format_args!("{b:02x}"));
                    }
                    format!("v4:0x{head}…")
                }),
                _ => None,
            };
            let Some(pool) = pool else {
                continue;
            };
            if !seen.insert(pool.clone()) {
                continue; // dedupe: one on-chain read per unique pool per anchor
            }
            let verdict = match hop.hop_type {
                HopType::V3 => {
                    let Some((addr, s_sqrt, s_liq, s_tick)) = hop.v3 else {
                        continue;
                    };
                    match fetch_v3_slot0_liquidity(provider, &addr, Some(anchor)).await {
                        Ok((c_sqrt, c_tick, c_liq)) => {
                            if cl_state_matches(s_sqrt, s_liq, s_tick, c_sqrt, c_liq, c_tick) {
                                LaggardProbeVerdict::Quiet
                            } else {
                                LaggardProbeVerdict::MovedInBlockNotApplied
                            }
                        }
                        Err(_) => LaggardProbeVerdict::ReadFailed,
                    }
                }
                HopType::V4 => {
                    let Some((_pm, pool_id, state_view, s_sqrt, s_liq, s_tick)) = hop.v4 else {
                        continue;
                    };
                    match fetch_v4_slot0_liquidity(provider, &state_view, &pool_id, Some(anchor))
                        .await
                    {
                        Ok((c_sqrt, c_tick, _pf, _lp, c_liq)) => {
                            if cl_state_matches(s_sqrt, s_liq, s_tick, c_sqrt, c_liq, c_tick) {
                                LaggardProbeVerdict::Quiet
                            } else {
                                LaggardProbeVerdict::MovedInBlockNotApplied
                            }
                        }
                        Err(_) => LaggardProbeVerdict::ReadFailed,
                    }
                }
                _ => continue,
            };
            out.push(SolveAnchorProbeResult {
                pool,
                hop_type: hop.hop_type,
                update_block: hop.update_block,
                tick_data_block: hop.tick_data_block,
                stale_by,
                verdict,
            });
        }
    }
    out
}

// -----------------------------------------------------------------------------
// D3 staged-clock probe (Bug-A hardening) — fresh price clock, stale in-range
// `liquidity()` scalar.
// -----------------------------------------------------------------------------

// The staged-clock probe stance now lives on
// [`TripwireConfig::staged_clock_probe`] — the module reads no env.

/// The two-stamp clock gap (`update_block - tick_data_block`) above which a
/// skipped CL hop is worth a staged-clock probe read. A healthy pool advances
/// both clocks together (every `apply_swap` advances both), so a pronounced
/// price-ahead-of-map gap is genuinely anomalous — the map clock stopped
/// advancing while the price clock kept moving.
const STAGED_CLOCK_GAP_BLOCKS: u64 = 3;

/// Pure classifier: is a CL hop a **staged-clock candidate** — SKIPPED by the
/// gate (`update_block >= block`, its price clock reached the solve block) with
/// a COMPLETED, non-zero map anchor (`tick_data_block < block`) and a pronounced
/// price-ahead-of-map clock gap? `skip_in_progress_hop` short-circuits
/// verification for exactly this hop, so its in-range `liquidity` scalar is
/// never diffed — this surfaces that gap. Pure (no env / provider) so it is
/// unit-testable.
#[must_use]
fn staged_clock_candidate(
    update_block: u64,
    tick_data_block: u64,
    solve_block: u64,
    gap_tol: u64,
) -> bool {
    update_block >= solve_block
        && tick_data_block > 0
        && tick_data_block < solve_block
        && update_block.saturating_sub(tick_data_block) > gap_tol
}

/// D3 probe body: for a staged-clock candidate, compare the solver's in-range
/// `liquidity` scalar against on-chain `liquidity()` at the hop's **tick-data
/// anchor** (`tick_data_block` — a completed block, so reproducible, unlike a
/// mid-block capture). A mismatch means a fresh price but a stale in-range
/// liquidity scalar — the Bug-A staged-clock desync. Observational: logs a WARN
/// on divergence; a transport failure is logged and skipped (never a false flag).
async fn probe_staged_clock_scalar(
    provider: &AlloyProvider,
    hop: &SolverHopScalarState,
    solve_block: u64,
    i: usize,
) {
    if !staged_clock_candidate(
        hop.update_block,
        hop.tick_data_block,
        solve_block,
        STAGED_CLOCK_GAP_BLOCKS,
    ) {
        return;
    }
    let anchor = hop.tick_data_block;
    let (pool_name, solver_liq, chain_liq) = match hop.hop_type {
        HopType::V3 => {
            let Some((addr, _s, liq, _t)) = hop.v3 else {
                return;
            };
            let Ok((_, _, cl)) = fetch_v3_slot0_liquidity(provider, &addr, Some(anchor)).await
            else {
                tracing::warn!(
                    pool = %format!("v3:{addr}"),
                    anchor,
                    hop_index = i,
                    "[staged-clock] read failed (transient, not flagged)"
                );
                return;
            };
            let Ok(cl) = u128::try_from(cl) else {
                return;
            };
            (format!("v3:{addr}"), liq, cl)
        }
        HopType::V4 => {
            let Some((_pm, pool_id, sv, _s, liq, _t)) = hop.v4 else {
                return;
            };
            let mut head = String::with_capacity(8);
            for b in &pool_id[..4] {
                let _ = std::fmt::Write::write_fmt(&mut head, format_args!("{b:02x}"));
            }
            let Ok((_, _, _pf, _lp, cl)) =
                fetch_v4_slot0_liquidity(provider, &sv, &pool_id, Some(anchor)).await
            else {
                tracing::warn!(
                    pool = %format!("v4:0x{head}…"),
                    anchor,
                    hop_index = i,
                    "[staged-clock] read failed (transient, not flagged)"
                );
                return;
            };
            let Ok(cl) = u128::try_from(cl) else {
                return;
            };
            (format!("v4:0x{head}…"), liq, cl)
        }
        _ => return,
    };
    if solver_liq != chain_liq {
        tracing::warn!(
            pool = %pool_name,
            hop_index = i,
            block = solve_block,
            update_block = hop.update_block,
            tick_data_block = anchor,
            solver_in_range_liquidity = %solver_liq,
            onchain_liquidity = %chain_liq,
            "[staged-clock] solver in-range liquidity scalar != on-chain liquidity() at the \
             tick-data anchor (fresh price, stale in-range liquidity — Bug-A class)"
        );
    }
}

/// Maximum acceptable lag (in blocks) of a CL pool's stored `update_block`
/// behind the solve block before it is considered suspicious. The anchor-match
/// (exact diff at the hop's OWN `update_block`) tolerates 1-2 blocks of normal
/// pump latency, so this threshold is only reached by a genuinely stale hop
/// (missed swap events / a non-canonical Swap topic0 that the pump never
/// decodes, e.g. the PancakeSwap-V3 family — see the no-profit exploration
/// doc). `is_cl_pool_stale` at the solve block (a fresh read) then confirms
/// whether on-chain actually MOVED past the solver's snapshot; a quiet-but-
/// correct pool (chain unchanged) is not flagged.
///
/// **Configurable for dry-run MTBF iteration.** [`cl_staleness_threshold_blocks`]
/// reads the `DEGENBOT_SOLVER_STALENESS_BLOCKS` env override, defaulting to this
/// 3-block value. A dev dry-run can set it to `1` or `0` to tighten detection and
/// surface lag patterns with low mean-time-between-failures (failures = data);
/// the committed production default stays 3. The abort (UO3JM4) is never
/// weakened by lowering this — it only fires MORE often.
const MAX_CL_STALENESS_BLOCKS: u64 = 3;

/// Parse the `DEGENBOT_SOLVER_STALENESS_BLOCKS` override, falling back to
/// [`MAX_CL_STALENESS_BLOCKS`] on an unparseable value. Pure (no env access) so
/// it is unit-testable without touching process-global env.
#[must_use]
fn parse_cl_staleness_blocks(raw: &str) -> u64 {
    raw.trim().parse::<u64>().unwrap_or(MAX_CL_STALENESS_BLOCKS)
}

/// The effective staleness threshold in blocks: `DEGENBOT_SOLVER_STALENESS_BLOCKS`
/// if set (dry-run dev knob to lower MTBF), else [`MAX_CL_STALENESS_BLOCKS`].
/// Read fresh per call (cheap getenv; once per hop per block) so a test can
/// override without cross-test `OnceLock` contamination.
#[must_use]
fn cl_staleness_threshold_blocks() -> u64 {
    match std::env::var("DEGENBOT_SOLVER_STALENESS_BLOCKS") {
        Ok(v) => parse_cl_staleness_blocks(&v),
        Err(_) => MAX_CL_STALENESS_BLOCKS,
    }
}

/// Whether a CL hop's stored `update_block` is old enough to warrant a
/// fresh solve-block re-check. Skips never-updated pools (`update_block == 0`,
/// verified at the solve block via the anchor path) and pools at/after the
/// solve block (skipped by `skip_in_progress_hop`).
#[must_use]
fn is_cl_pool_stale(update_block: u64, block: u64) -> bool {
    update_block > 0 && block.saturating_sub(update_block) > cl_staleness_threshold_blocks()
}

/// Whether a hop at `update_block` must be SKIPPED when verifying at solve
/// `block`: skip hops touched IN the in-progress block (`update_block >= block`)
/// — their scalar reflects a mid-block capture (an early swap of the block,
/// e.g. 0xE0554a @ 25658682 captured swap #1) that a historical `slot0` read
/// (block-final) can never reproduce. Only hops whose state reflects a
/// COMPLETED block (`update_block < block`) are diffed, against the chain at
/// that completed anchor.
#[must_use]
fn skip_in_progress_hop(update_block: u64, block: u64) -> bool {
    update_block >= block
}

/// Verify that every hop's solver-stored scalar state (`extract_solver_hop_states`)
/// matches the chain at the SOLVER'S OWN anchor block (`hop.update_block`) — the
/// block its stored state claims to reflect. A pool 1-2 blocks behind the solve
/// block is normal latency; matching the chain at `update_block` proves the state
/// is accurate where it says it is, and only a divergence even AT the anchor is a
/// true desync (missed log / reorg / storage mutation). The pump-resolved solve
/// ANCHOR (`solve_anchor` — `crate::bot_core::solve_anchor`) is retained for
/// staleness reporting + the future-hop guard. Fails fast on the FIRST mismatch
/// or any read error.
///
/// Each mismatch message carries the hop's solver-stored `update_block` and the
/// staleness `block - update_block`, so a panic both proves the divergence and
/// reports its age.
///
/// V2 reads `getReserves`, V3 `slot0`+`liquidity`, and V4 the `StateView`'s
/// `getSlot0`+`getLiquidity` at the hop's Rust-owned `state_view` (ADR-005 /
/// Option 2 — not `getPool`, which reverts on the canonical `PoolManager`). Non-CL hop
/// families (Solidly / Balancer / Curve) are skipped — their solve state is
/// not a simple scalar slot0/getReserves diff.
///
/// # Errors
///
/// Returns a [`GateFailure`] (carrying its ADR-021 D2 [`TripwireClass`]) on
/// any hop whose stored state diverges from the chain at its anchor, or on an
/// `eth_call` transport/decode failure (both class `Unclassified` — a read
/// failure must never look like an honest pass).
#[expect(clippy::too_many_lines)]
async fn verify_solver_hop_states(
    provider: &AlloyProvider,
    config: &TripwireConfig,
    hops: &[SolverHopScalarState],
    solve_anchor: crate::bot_core::solve_anchor::SolveAnchor,
    reorg_evidence: &[TripReorgWindow],
) -> Result<(), GateFailure> {
    // The pump resolved the anchor at publish (see `crate::bot_core::solve_anchor`);
    // the body reads the plain block for staleness math + messages.
    let block = solve_anchor.block();
    for (i, hop) in hops.iter().enumerate() {
        // Compare against the chain at the SOLVER'S OWN anchor block
        // (`update_block`) — the block its stored scalar state claims to
        // reflect. A solver holding state from 1-2 blocks ago is normal
        // latency (you cannot know future swaps); matching the chain at
        // `update_block` proves the state is accurate where it says it is.
        // Only a state that diverges even AT its own anchor is a true
        // desync (a missed log / reorg / storage mutation). `block` (the
        // solve block) remains for message context / staleness reporting.
        let anchor = solver_anchor_block(hop.update_block, block);
        // FUTURE-PRICE guard (belts + suspenders): `solve_anchor` is the
        // PUMP-resolved anchor (BO5FBS / B2 re-anchor history in
        // `crate::bot_core::solve_anchor`), so no hop's `update_block` can
        // exceed it (head is the max). This fires only in a genuinely impossible
        // case — NOT the backfill/dispatch race (a hop at the head is LIVE
        // state, and the anchor already floored at it). The guard stays to
        // catch a mid-solve state advance or a bypassing caller; the verifier's
        // reaction is the terminal `MismatchError` (ADR-021), unlike the
        // solve-stage deferral.
        if solve_anchor.is_future(hop.update_block) {
            return Err(mismatch(
                i,
                TripwireClass::Unclassified,
                &format!(
                    "CL hop FUTURE at solve anchor {} (solver update_block={}, ahead by {} blocks): \
                     the price clock runs past the block being solved — solving with a future price is \
                     never legitimate",
                    solve_anchor.block(),
                    hop.update_block,
                    hop.update_block.saturating_sub(solve_anchor.block()),
                ),
            ));
        }
        // A hop whose `update_block >= block` was touched IN the solve block
        // (`block` is the pump's in-progress header block) is honest-by-
        // construction and unverifiable via historical slot0 — skip it. Only
        // a hop whose state reflects a COMPLETED block is diffed.
        if skip_in_progress_hop(hop.update_block, block) {
            // D3 (Bug-A): a skipped hop's in-range `liquidity` scalar is NOT
            // diffed here (the price clock reaching the solve block short-
            // circuits the scalar gate). Surface the two-stamp staged-clock
            // desync — fresh price, stale in-range liquidity — via an opt-in,
            // non-aborting probe that compares the solver's scalar to on-chain
            // `liquidity()` at the hop's completed tick-data anchor. This turns
            // the silently-skipped stale-liquidity class into an observable
            // `[staged-clock]` WARN without weakening or bypassing the abort.
            if config.staged_clock_probe {
                probe_staged_clock_scalar(provider, hop, block, i).await;
            }
            continue;
        }
        match hop.hop_type {
            HopType::V2 => {
                let Some((pool, s0, s1)) = hop.v2 else {
                    continue;
                };
                let (c0, c1) = fetch_v2_reserves(provider, &pool, Some(anchor))
                    .await
                    .map_err(|e| {
                        mismatch(
                            i,
                            TripwireClass::Unclassified,
                            &format!("V2 eth_call at block {anchor}: {e}"),
                        )
                    })?;
                if !v2_state_matches(s0, s1, c0, c1) {
                    return Err(reorg_refine_failure(
                        mismatch(
                            i,
                            TripwireClass::StorageMutated,
                            &format!(
                                "V2 pool {pool} state mismatch at its anchor block {anchor} \
                                 (solve block {block}, solver update_block={ub}, behind by {}): \
                                 solver reserve0/1 = ({s0},{s1}), on-chain = ({c0},{c1})",
                                block.saturating_sub(hop.update_block),
                                ub = hop.update_block,
                            ),
                        ),
                        anchor,
                        reorg_evidence,
                    ));
                }
            }
            HopType::V3 => {
                let Some((pool, s_sqrt, s_liq, s_tick)) = hop.v3 else {
                    continue;
                };
                let (c_sqrt, c_tick, c_liq) =
                    fetch_v3_slot0_liquidity(provider, &pool, Some(anchor))
                        .await
                        .map_err(|e| {
                            mismatch(
                                i,
                                TripwireClass::Unclassified,
                                &format!("V3 eth_call at block {anchor}: {e}"),
                            )
                        })?;
                if !cl_state_matches(s_sqrt, s_liq, s_tick, c_sqrt, c_liq, c_tick) {
                    return Err(reorg_refine_failure(
                        mismatch(
                            i,
                            TripwireClass::StorageMutated,
                            &format!(
                                "V3 pool {pool} state mismatch at its anchor block {anchor} \
                                 (solve block {block}, solver update_block={ub}, behind by {}): \
                                 solver (sqrt={s_sqrt}, liq={s_liq}, tick={s_tick}), on-chain \
                                 (sqrt={c_sqrt}, liq={c_liq}, tick={c_tick})",
                                block.saturating_sub(hop.update_block),
                                ub = hop.update_block,
                            ),
                        ),
                        anchor,
                        reorg_evidence,
                    ));
                }
                // Staleness re-check: a hop accurate at its OWN (old) anchor can
                // still be a frozen snapshot whose on-chain price moved past it
                // (missed swap events — e.g. a PancakeSwap-V3 pool whose Swap
                // topic0 the pump does not decode). Compare against a FRESH
                // solve-block read; only flag when the solve-block state diverges
                // AND the hop is genuinely old. A quiet-but-correct pool (on-chain
                // unchanged) is not flagged.
                if is_cl_pool_stale(hop.update_block, block) {
                    let (b_sqrt, b_tick, b_liq) =
                        fetch_v3_slot0_liquidity(provider, &pool, Some(block))
                            .await
                            .map_err(|e| {
                                mismatch(
                                    i,
                                    TripwireClass::Unclassified,
                                    &format!("V3 solve-block eth_call at {block}: {e}"),
                                )
                            })?;
                    if !cl_state_matches(s_sqrt, s_liq, s_tick, b_sqrt, b_liq, b_tick) {
                        return Err(mismatch(
                            i,
                            TripwireClass::MissedLog,
                            &format!(
                                "V3 pool {pool} STALE at solve block {block} (solver \
                                 update_block={ub}, behind by {} blocks): solver snapshot \
                                 (sqrt={s_sqrt}, liq={s_liq}, tick={s_tick}) no longer matches \
                                 on-chain at {block} (sqrt={b_sqrt}, liq={b_liq}, tick={b_tick}). \
                                 On-chain price moved past this pool's frozen snapshot: a swap \
                                 was not applied before solve. Most likely a pump drain/state-\
                                 advance stall (pool state did not reach the solve block — e.g. \
                                 after a snapshot backfill); for forked pools, could also be a \
                                 Swap topic0 the pump does not decode (e.g. PancakeSwap-V3).\n",
                                block.saturating_sub(hop.update_block),
                                ub = hop.update_block,
                            ),
                        ));
                    }
                }
                // Solve-time tick-map fidelity probe (ADR-021 D3): verify the
                // solver's LIVE tick map against on-chain at the tick-data
                // anchor. The scalar diff above only catches active-liquidity
                // divergence (Σ net ≤ current tick); a snapshot missing an
                // OFF-range position passes it yet under-counts liquidity once
                // a solve crosses into the missing region — the UO3JM4
                // over-prediction class.
                if let Some(cl) = hop.cl_tick_map.as_ref() {
                    probe_cl_tick_map_fidelity(
                        provider,
                        hop,
                        cl,
                        pool,
                        Address::ZERO,
                        [0u8; 32],
                        i,
                    )
                    .await?;
                }
            }
            HopType::V4 => {
                let Some((pm, pool_id, state_view, s_sqrt, s_liq, s_tick)) = hop.v4 else {
                    continue;
                };
                let (c_sqrt, c_tick, _protocol_fee, _lp_fee, c_liq) =
                    fetch_v4_slot0_liquidity(provider, &state_view, &pool_id, Some(anchor))
                        .await
                        .map_err(|e| {
                            mismatch(
                                i,
                                TripwireClass::Unclassified,
                                &format!("V4 eth_call at block {anchor}: {e}"),
                            )
                        })?;
                if !cl_state_matches(s_sqrt, s_liq, s_tick, c_sqrt, c_liq, c_tick) {
                    return Err(reorg_refine_failure(
                        mismatch(
                            i,
                            TripwireClass::StorageMutated,
                            &format!(
                                "V4 pool {pm} (id {:02x}…) state mismatch at its anchor block {anchor} \
                                 (solve block {block}, solver update_block={ub}, behind by {}): \
                                 solver (sqrt={s_sqrt}, liq={s_liq}, tick={s_tick}), on-chain \
                                 (sqrt={c_sqrt}, liq={c_liq}, tick={c_tick})",
                                pool_id[0],
                                block.saturating_sub(hop.update_block),
                                ub = hop.update_block,
                            ),
                        ),
                        anchor,
                        reorg_evidence,
                    ));
                }
                if is_cl_pool_stale(hop.update_block, block) {
                    let (b_sqrt, b_tick, _b_pf, _b_lp, b_liq) =
                        fetch_v4_slot0_liquidity(provider, &state_view, &pool_id, Some(block))
                            .await
                            .map_err(|e| {
                                mismatch(
                                    i,
                                    TripwireClass::Unclassified,
                                    &format!("V4 solve-block eth_call at {block}: {e}"),
                                )
                            })?;
                    if !cl_state_matches(s_sqrt, s_liq, s_tick, b_sqrt, b_liq, b_tick) {
                        return Err(mismatch(
                            i,
                            TripwireClass::MissedLog,
                            &format!(
                                "V4 pool {pm} (id {:02x}…) STALE at solve block {block} (solver \
                                 update_block={ub}, behind by {} blocks): solver snapshot \
                                 (sqrt={s_sqrt}, liq={s_liq}, tick={s_tick}) no longer matches \
                                 on-chain at {block} (sqrt={b_sqrt}, liq={b_liq}, tick={b_tick})",
                                pool_id[0],
                                block.saturating_sub(hop.update_block),
                                ub = hop.update_block,
                            ),
                        ));
                    }
                }
                // Solve-time tick-map fidelity probe (ADR-021 D3) — see the V3
                // arm above.
                if let Some(cl) = hop.cl_tick_map.as_ref() {
                    probe_cl_tick_map_fidelity(
                        provider,
                        hop,
                        cl,
                        Address::ZERO,
                        state_view,
                        pool_id,
                        i,
                    )
                    .await?;
                }
            }
            // Solidly / Balancer / Curve — no scalar diff in scope; pass-through.
            _ => {}
        }
    }
    Ok(())
}

/// A strict-gate failure carrying its ADR-021 D2 class, wrapped into a
/// [`GateVerdict::Divergent`] by [`judge`].
#[derive(Debug, Clone)]
struct GateFailure {
    hop_idx: usize,
    class: TripwireClass,
    message: String,
}

fn mismatch(index: usize, class: TripwireClass, message: &str) -> GateFailure {
    GateFailure {
        hop_idx: index,
        class,
        message: format!("hop {index}: {message}"),
    }
}

/// ADR-021 D2 Part A — the reorg refinement of a strict-gate failure: an
/// own-anchor mismatch classed coarse `StorageMutated` is re-labelled
/// `UnhandledReorg` when a recorded reorg window's rollback crossed the hop's
/// anchor. At a historical block the chain state is immutable — a replaced
/// block is the only mechanism that can move chain@anchor away from the value
/// an honest apply recorded — so the refinement is conservative: it only
/// re-labels (the trip is unchanged), and a decode/apply bug that wrote a
/// wrong value at derivation keeps the `StorageMutated` label.
fn reorg_refine_failure(
    f: GateFailure,
    hop_anchor: u64,
    reorg_evidence: &[TripReorgWindow],
) -> GateFailure {
    if f.class != TripwireClass::StorageMutated {
        return f;
    }
    let Some(w) = reorg_evidence
        .iter()
        .find(|w| w.deepest_removed_block < hop_anchor)
    else {
        return f;
    };
    GateFailure {
        hop_idx: f.hop_idx,
        class: TripwireClass::UnhandledReorg,
        message: format!(
            "{} [reorg evidence] a reorg window (deepest removed block {} < hop anchor {}, opened at block {}, closed at {}) crossed this hop's anchor before the state was re-derived — class UnhandledReorg (a replaced anchor is the only mechanism that moves historical state).",
            f.message,
            w.deepest_removed_block,
            hop_anchor,
            w.opened_at,
            w.closed_at.map_or("open".to_string(), |b| b.to_string()),
        ),
    }
}

/// ADR-021 D2 Part A — record an `EnterReorg` on the pump's bounded evidence
/// list (the oldest window evicts past [`TRIP_REORG_WINDOW_CAP`]).
pub(crate) fn reorg_window_open(
    windows: &mut std::collections::VecDeque<TripReorgWindow>,
    removed_block: u64,
    seen_at: u64,
) {
    if windows.len() >= TRIP_REORG_WINDOW_CAP {
        windows.pop_front();
    }
    windows.push_back(TripReorgWindow {
        deepest_removed_block: removed_block,
        opened_at: seen_at,
        closed_at: None,
    });
}

/// A `ContinueReorg` log inside the currently open window widens its rollback
/// to the minimum removed block (order-insensitive, like the journal restore).
pub(crate) fn reorg_window_continue(
    windows: &mut std::collections::VecDeque<TripReorgWindow>,
    removed_block: u64,
) {
    if let Some(w) = windows.iter_mut().rev().find(|w| w.closed_at.is_none()) {
        w.deepest_removed_block = w.deepest_removed_block.min(removed_block);
    }
}

/// A `CloseReorg { new_head }` closes the currently open reorg window.
pub(crate) fn reorg_window_close(
    windows: &mut std::collections::VecDeque<TripReorgWindow>,
    new_head: u64,
) {
    if let Some(w) = windows.iter_mut().rev().find(|w| w.closed_at.is_none()) {
        w.closed_at = Some(new_head);
    }
}

/// A lightweight `TickMap` carrier over a CL hop's extracted snapshot, so the
/// solve-time tick-map fidelity probe can reuse `verify_v3_pool` /
/// `verify_v4_pool` (which take `&impl degenbot_pools::tick_map::TickMap`) on
/// the post-guard snapshot without cloning the tick map a second time.
struct SolverTickMapCarrier<'a> {
    address: Address,
    tick_spacing: i32,
    active_tick: i32,
    tick_data: &'a HashMap<i32, degenbot_pools::TickInfo>,
}

impl degenbot_pools::tick_map::TickMap for SolverTickMapCarrier<'_> {
    fn address(&self) -> Address {
        self.address
    }

    fn tick_spacing(&self) -> i32 {
        self.tick_spacing
    }

    fn active_tick(&self) -> i32 {
        self.active_tick
    }

    fn tick_data(&self) -> &HashMap<i32, degenbot_pools::TickInfo> {
        self.tick_data
    }
}

/// The solve-time CL **tick-map fidelity probe** (ADR-021 D3 — one state-
/// accuracy tripwire converging the solver-state scalar diff with the
/// liquidity-verifier tick-map machinery).
///
/// Verifies the solver's extracted live tick map against on-chain at the
/// hop's tick-data anchor (`tick_data_block`) via `verify_v3_pool` /
/// `verify_v4_pool`, which scan the bitmap and fire on "on-chain tick NOT in
/// engine" — the partial-snapshot class the scalar gate (active-liquidity
/// diff only) cannot see. A `Mismatch` is a hard trip (a genuine desync → the
/// gate aborts). An `Rpc` transport failure is NOT a divergence — it is logged
/// and skipped (the probe must not kill the bot on a transient read hiccup; a
/// retry/backoff candidate, per the liquidity-verifier `Rpc` contract).
///
/// The anchor is the hop's tick-data clock (`tick_data_block`), NOT the price
/// clock `update_block`: the map reflects the liquidity clock (OB7UNY), so
/// verifying at the map's own claimed anchor lets a 1-2 block price lag or a
/// never-advanced liquidity clock pass legitimately and only trips on a truly
/// desynced map.
async fn probe_cl_tick_map_fidelity(
    provider: &AlloyProvider,
    hop: &SolverHopScalarState,
    cl: &ClTickMapSnapshot,
    v3_addr: Address,
    v4_state_view: Address,
    v4_pool_id: [u8; 32],
    i: usize,
) -> Result<(), GateFailure> {
    let tick_anchor = if hop.tick_data_block > 0 {
        hop.tick_data_block
    } else {
        hop.update_block
    };
    let carrier = SolverTickMapCarrier {
        address: match hop.hop_type {
            HopType::V3 => v3_addr,
            _ => v4_state_view,
        },
        tick_spacing: cl.tick_spacing,
        active_tick: cl.active_tick,
        tick_data: &cl.tick_data,
    };
    let result = match hop.hop_type {
        HopType::V3 => verify_v3_pool(provider, &carrier, Some(tick_anchor)).await,
        HopType::V4 => {
            verify_v4_pool(
                provider,
                v4_state_view,
                v4_pool_id,
                &carrier,
                Some(tick_anchor),
            )
            .await
        }
        _ => return Ok(()),
    };
    match result {
        Ok(()) => Ok(()),
        Err(LiquidityVerifyError::Mismatch(m)) => Err(mismatch(
            i,
            TripwireClass::Unclassified,
            &format!(
                "tick-map fidelity probe at tick-data anchor {tick_anchor}: {} (the scalar gate \
                 diffs active liquidity only and can miss off-range missing positions — a thin/\
                 partial tick map over-predicts output once a solve crosses into the missing \
                 region; UO3JM4 class, ADR-021)",
                m.message,
            ),
        )),
        Err(LiquidityVerifyError::Rpc { message }) => {
            // A transport failure is NOT a divergence — do not abort the bot on
            // a transient read hiccup; log + continue (the `Mismatch` branch is
            // the hard trip).
            tracing::warn!(
                %message,
                tick_anchor,
                hop_index = i,
                "tick-map fidelity probe RPC skipped (transient, not a divergence)"
            );
            Ok(())
        }
    }
}

#[expect(clippy::expect_used, clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{I256, U256};
    use alloy::transports::mock::Asserter;
    use std::sync::Arc;

    // -----------------------------------------------------------------
    // `judge` — the ADR-021 D3 single-call interface (red phase).
    // These pin the stage pipeline through the public interface only.
    // -----------------------------------------------------------------

    /// A V2 hop for the judge tests (no tick clock, no CL metadata).
    fn v2_hop(s0: u64, s1: u64, update_block: u64) -> SolverHopScalarState {
        SolverHopScalarState {
            hop_type: HopType::V2,
            update_block,
            tick_data_block: 0,
            cl_meta: None,
            v2: Some((Address::from([0x44u8; 20]), U256::from(s0), U256::from(s1))),
            v3: None,
            v4: None,
            cl_tick_map: None,
        }
    }

    /// One `getReserves` `eth_call` result: (reserve0, reserve1, block32).
    fn v2_reserves_response(s0: u64, s1: u64) -> String {
        let mut buf = Vec::with_capacity(96);
        for v in [U256::from(s0), U256::from(s1), U256::ZERO] {
            let w: [u8; 32] = v.to_be_bytes();
            buf.extend_from_slice(&w);
        }
        format!("0x{}", alloy::primitives::hex::encode(buf))
    }

    /// One Uniswap V3 `slot0` `eth_call` result: 7 words (sqrt, tick, tail).
    fn v3_slot0_response(sqrt: U256, tick: i32) -> String {
        let mut buf = vec![0u8; 7 * 32];
        let sqrt_w: [u8; 32] = sqrt.to_be_bytes();
        buf[0..32].copy_from_slice(&sqrt_w);
        let tick_w: [u8; 4] = tick.to_be_bytes();
        buf[44..48].copy_from_slice(&tick_w);
        format!("0x{}", alloy::primitives::hex::encode(buf))
    }

    /// One `liquidity()` `eth_call` result: uint128 (upper 16 bytes).
    fn liq_response(liq: u128) -> String {
        let mut buf = [0u8; 32];
        let w: [u8; 16] = liq.to_be_bytes();
        buf[16..32].copy_from_slice(&w);
        format!("0x{}", alloy::primitives::hex::encode(buf))
    }

    /// (i) A V2 hop honest at its own anchor is `Ok` — one strict-gate read,
    /// nothing else.
    /// ADR-040 / 52I5SV: the divergence snapshot embeds the scalar diff
    /// (breadcrumb) AND the structured per-hop state; the JSON artifact is
    /// parseable and round-trips through `write` into a file whose content
    /// is sufficient to reproduce the divergence (the mocked on-chain
    /// reserve 777 vs stored 1000 appears in the dump).
    #[tokio::test]
    async fn divergence_snapshot_round_trips_to_a_json_dump() {
        let hop = v2_hop(1_000, 2_000, 190);
        let provider = mock_provider(vec![v2_reserves_response(777, 888)]);
        let v = judge(
            &provider,
            &TripwireConfig::enabled_only(),
            &[vec![hop]],
            crate::bot_core::solve_anchor::SolveAnchor::for_head(200, 0),
            &[],
        )
        .await;
        let GateVerdict::Divergent(d) = v else {
            panic!("mismatch must trip");
        };
        let json = d.snapshot.to_json();
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("dump must be valid JSON");
        assert_eq!(parsed["schema"], "degenbot-desync-dump/1");
        assert_eq!(parsed["anchor_block"], 200);
        assert!(
            json.contains("777"),
            "the chain scalar must appear in the dump: {json}"
        );
        let dir = std::env::temp_dir().join(format!("degenbot-dump-test-{}", std::process::id()));
        let path = d.snapshot.write(&dir).expect("dump write");
        let body = std::fs::read_to_string(&path).expect("dump readable");
        assert!(body.contains("777"), "written dump carries the diff");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn judge_honest_v2_at_anchor_is_ok() {
        let hop = v2_hop(1000, 2000, 190);
        let provider = mock_provider(vec![v2_reserves_response(1000, 2000)]);
        let v = judge(
            &provider,
            &TripwireConfig::enabled_only(),
            &[vec![hop]],
            crate::bot_core::solve_anchor::SolveAnchor::for_head(200, 0),
            &[],
        )
        .await;
        assert!(
            matches!(v, GateVerdict::Ok),
            "honest at own anchor must pass"
        );
    }

    /// (ii) A scalar mismatch AT the hop's own anchor trips as
    /// `StorageMutated` with the grep-able literal prefix + class token.
    #[tokio::test]
    async fn judge_own_anchor_mismatch_triages_storage_mutated_with_literal() {
        let hop = v2_hop(1000, 2000, 190);
        let provider = mock_provider(vec![v2_reserves_response(777, 888)]);
        let v = judge(
            &provider,
            &TripwireConfig::enabled_only(),
            &[vec![hop]],
            crate::bot_core::solve_anchor::SolveAnchor::for_head(200, 0),
            &[],
        )
        .await;
        let GateVerdict::Divergent(d) = v else {
            panic!("gate must trip on an own-anchor mismatch");
        };
        assert_eq!(d.class, TripwireClass::StorageMutated);
        assert_eq!(d.path_idx, 0);
        assert_eq!(d.hop_idx, 0);
        assert!(
            d.breadcrumb.starts_with(
                "[SOLVER-STATE] ABORT: solver used desynced pool state at solve block 200 (path_idx=0"
            ),
            "literal prefix must stay byte-identical; got: {}",
            d.breadcrumb,
        );
        assert!(
            d.breadcrumb.contains("StorageMutated"),
            "the class must be named in the breadcrumb; got: {}",
            d.breadcrumb,
        );
    }

    /// (iii) Own-anchor honest + CL STALE flag (on-chain moved past the
    /// staleness threshold) trips as `MissedLog`.
    #[tokio::test]
    async fn judge_own_anchor_honest_stale_cl_moves_to_missed_log() {
        let sqrt = U256::from(1u128) << 96;
        let liq = 1_000_000u128;
        let hop = SolverHopScalarState {
            hop_type: HopType::V3,
            update_block: 190,
            tick_data_block: 190,
            cl_meta: None,
            v2: None,
            v3: Some((Address::from([0xaau8; 20]), sqrt, liq, 0)),
            v4: None,
            cl_tick_map: None,
        };
        let moved = U256::from(2u128) << 96;
        let provider = mock_provider(vec![
            v3_slot0_response(sqrt, 0),
            liq_response(liq),
            v3_slot0_response(moved, 0),
            liq_response(liq),
        ]);
        let v = judge(
            &provider,
            &TripwireConfig::enabled_only(),
            &[vec![hop]],
            crate::bot_core::solve_anchor::SolveAnchor::for_head(200, 0),
            &[],
        )
        .await;
        let GateVerdict::Divergent(d) = v else {
            panic!("the STALE flag must trip");
        };
        assert_eq!(d.class, TripwireClass::MissedLog);
    }

    /// ADR-021 D2 Part A — an own-anchor mismatch (coarse `StorageMutated`)
    /// with a recorded reorg window whose rollback crossed the hop's anchor is
    /// re-labelled `UnhandledReorg`, keeping the trip + literal prefix +
    /// quantified evidence in the breadcrumb.
    #[tokio::test]
    async fn judge_own_anchor_mismatch_with_crossing_reorg_window_is_unhandled_reorg() {
        let hop = v2_hop(1000, 2000, 190);
        let provider = mock_provider(vec![v2_reserves_response(777, 888)]);
        let window = TripReorgWindow {
            deepest_removed_block: 150,
            opened_at: 210,
            closed_at: Some(205),
        };
        let v = judge(
            &provider,
            &TripwireConfig::enabled_only(),
            &[vec![hop]],
            crate::bot_core::solve_anchor::SolveAnchor::for_head(200, 0),
            &[window],
        )
        .await;
        let GateVerdict::Divergent(d) = v else {
            panic!("gate must trip on an own-anchor mismatch");
        };
        assert_eq!(d.class, TripwireClass::UnhandledReorg);
        assert!(
            d.breadcrumb.starts_with(
                "[SOLVER-STATE] ABORT: solver used desynced pool state at solve block 200 (path_idx=0"
            ),
            "literal prefix must stay byte-identical; got: {}",
            d.breadcrumb
        );
        assert!(
            d.breadcrumb.contains("UnhandledReorg"),
            "the class must be named; got: {}",
            d.breadcrumb
        );
        assert!(
            d.breadcrumb.contains("[reorg evidence]"),
            "the crossing window must be named; got: {}",
            d.breadcrumb
        );
        assert!(
            d.breadcrumb.contains("deepest removed block 150"),
            "the crossing must be quantified; got: {}",
            d.breadcrumb
        );
    }

    /// A reorg window that did NOT cross the hop's anchor (the rollback
    /// stopped above it) must not re-label: the mismatch stays coarse
    /// `StorageMutated`, no evidence line.
    #[tokio::test]
    async fn judge_own_anchor_mismatch_with_noncrossing_reorg_window_stays_storage_mutated() {
        let hop = v2_hop(1000, 2000, 190);
        let provider = mock_provider(vec![v2_reserves_response(777, 888)]);
        let window = TripReorgWindow {
            deepest_removed_block: 195, // > anchor 190 — the rollback stopped above the anchor
            opened_at: 210,
            closed_at: Some(205),
        };
        let v = judge(
            &provider,
            &TripwireConfig::enabled_only(),
            &[vec![hop]],
            crate::bot_core::solve_anchor::SolveAnchor::for_head(200, 0),
            &[window],
        )
        .await;
        let GateVerdict::Divergent(d) = v else {
            panic!("gate must trip on an own-anchor mismatch");
        };
        assert_eq!(d.class, TripwireClass::StorageMutated);
        assert!(
            !d.breadcrumb.contains("[reorg evidence]"),
            "a non-crossing window must not re-label; got: {}",
            d.breadcrumb
        );
    }

    /// Reorg evidence NEVER re-labels a `MissedLog` verdict (honest at the
    /// anchor, on-chain moved later — the refinement applies to own-anchor
    /// divergence only).
    #[tokio::test]
    async fn judge_missed_log_is_not_relabelled_by_reorg_evidence() {
        let sqrt = U256::from(1u128) << 96;
        let liq = 1_000_000u128;
        let hop = SolverHopScalarState {
            hop_type: HopType::V3,
            update_block: 190,
            tick_data_block: 190,
            cl_meta: None,
            v2: None,
            v3: Some((Address::from([0xaau8; 20]), sqrt, liq, 0)),
            v4: None,
            cl_tick_map: None,
        };
        let moved = U256::from(2u128) << 96;
        let provider = mock_provider(vec![
            v3_slot0_response(sqrt, 0),
            liq_response(liq),
            v3_slot0_response(moved, 0),
            liq_response(liq),
        ]);
        let window = TripReorgWindow {
            deepest_removed_block: 150,
            opened_at: 210,
            closed_at: Some(205),
        };
        let v = judge(
            &provider,
            &TripwireConfig::enabled_only(),
            &[vec![hop]],
            crate::bot_core::solve_anchor::SolveAnchor::for_head(200, 0),
            &[window],
        )
        .await;
        let GateVerdict::Divergent(d) = v else {
            panic!("the STALE flag must trip");
        };
        assert_eq!(d.class, TripwireClass::MissedLog);
        assert!(
            !d.breadcrumb.contains("[reorg evidence]"),
            "MissedLog must never carry the reorg label; got: {}",
            d.breadcrumb
        );
    }

    /// ADR-021 D2 Part B (settled policy) — the strict gate PASSES (honest at
    /// the anchor) but the aggregate worst delivery lag exceeds the operator
    /// threshold: the verdict is Divergent `DeliveryLag{blocks}`.
    #[tokio::test]
    async fn judge_honest_lag_beyond_threshold_trips_delivery_lag() {
        let sqrt = U256::from(1u128) << 96;
        let liq = 1_000_000u128;
        let hop = SolverHopScalarState {
            hop_type: HopType::V3,
            update_block: 180, // 20 blocks behind the solve block 200
            tick_data_block: 180,
            cl_meta: Some(("Tracked".to_string(), "Live".to_string())),
            v2: None,
            v3: Some((Address::from([0xaau8; 20]), sqrt, liq, 0)),
            v4: None,
            cl_tick_map: None,
        };
        let provider = mock_provider(vec![
            v3_slot0_response(sqrt, 0), // strict gate: anchor read (honest)
            liq_response(liq),
            v3_slot0_response(sqrt, 0), // staleness re-read at the solve block (honest)
            liq_response(liq),
        ]);
        let cfg = TripwireConfig {
            delivery_lag_trip_blocks: Some(10),
            ..TripwireConfig::enabled_only()
        };
        let v = judge(
            &provider,
            &cfg,
            &[vec![hop]],
            crate::bot_core::solve_anchor::SolveAnchor::for_head(200, 0),
            &[],
        )
        .await;
        let GateVerdict::Divergent(d) = v else {
            panic!("lag past the operator threshold must trip");
        };
        assert_eq!(d.class, TripwireClass::DeliveryLag { blocks: 20 });
        assert!(
            d.breadcrumb.starts_with(
                "[SOLVER-STATE] ABORT: solver used desynced pool state at solve block 200 (path_idx=0"
            ),
            "literal prefix must stay byte-identical; got: {}",
            d.breadcrumb
        );
        assert!(
            d.breadcrumb
                .contains("trails the solve anchor by 20 blocks"),
            "the lag must be quantified; got: {}",
            d.breadcrumb
        );
        assert!(
            d.breadcrumb.contains("at the solve block"),
            "the ADR-021 trip phrasing must survive; got: {}",
            d.breadcrumb
        );
    }

    /// The same honest lag WITHIN the threshold must NOT trip (parity with
    /// today's trip set below the operator's bar).
    #[tokio::test]
    async fn judge_honest_lag_within_threshold_does_not_trip() {
        let sqrt = U256::from(1u128) << 96;
        let liq = 1_000_000u128;
        let hop = SolverHopScalarState {
            hop_type: HopType::V3,
            update_block: 180,
            tick_data_block: 180,
            cl_meta: Some(("Tracked".to_string(), "Live".to_string())),
            v2: None,
            v3: Some((Address::from([0xaau8; 20]), sqrt, liq, 0)),
            v4: None,
            cl_tick_map: None,
        };
        let provider = mock_provider(vec![
            v3_slot0_response(sqrt, 0),
            liq_response(liq),
            v3_slot0_response(sqrt, 0),
            liq_response(liq),
        ]);
        let cfg = TripwireConfig {
            delivery_lag_trip_blocks: Some(100),
            ..TripwireConfig::enabled_only()
        };
        let v = judge(
            &provider,
            &cfg,
            &[vec![hop]],
            crate::bot_core::solve_anchor::SolveAnchor::for_head(200, 0),
            &[],
        )
        .await;
        assert!(
            matches!(v, GateVerdict::Ok),
            "a lag within the operator threshold must not trip"
        );
    }

    /// The DEFAULT (threshold unset) keeps today's byte-identical trip set:
    /// a 20-block honest lag is report-only observability, not a trip.
    #[tokio::test]
    async fn judge_honest_lag_default_off_is_parity() {
        let sqrt = U256::from(1u128) << 96;
        let liq = 1_000_000u128;
        let hop = SolverHopScalarState {
            hop_type: HopType::V3,
            update_block: 180,
            tick_data_block: 180,
            cl_meta: Some(("Tracked".to_string(), "Live".to_string())),
            v2: None,
            v3: Some((Address::from([0xaau8; 20]), sqrt, liq, 0)),
            v4: None,
            cl_tick_map: None,
        };
        let provider = mock_provider(vec![
            v3_slot0_response(sqrt, 0),
            liq_response(liq),
            v3_slot0_response(sqrt, 0),
            liq_response(liq),
        ]);
        let v = judge(
            &provider,
            &TripwireConfig::enabled_only(),
            &[vec![hop]],
            crate::bot_core::solve_anchor::SolveAnchor::for_head(200, 0),
            &[],
        )
        .await;
        assert!(
            matches!(v, GateVerdict::Ok),
            "default parity: delivery lag alone must not trip today's trip set"
        );
    }

    /// The pump-side window record helpers: open / continue (min-widening) /
    /// close / bounded cap (bounded memory under reorg traffic).
    #[test]
    fn reorg_window_record_open_continue_close_and_cap() {
        let mut w: std::collections::VecDeque<TripReorgWindow> = std::collections::VecDeque::new();
        reorg_window_open(&mut w, 100, 210);
        reorg_window_continue(&mut w, 95);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].deepest_removed_block, 95, "continue widens to the min");
        assert_eq!(w[0].opened_at, 210);
        assert_eq!(w[0].closed_at, None);
        reorg_window_continue(&mut w, 101); // shallower — must not shrink the window
        assert_eq!(w[0].deepest_removed_block, 95);
        reorg_window_close(&mut w, 215);
        assert_eq!(w[0].closed_at, Some(215));
        // A continue after close must not resurrect the closed window.
        reorg_window_continue(&mut w, 50);
        assert_eq!(w[0].deepest_removed_block, 95);
        assert_eq!(w[0].closed_at, Some(215));
        // Cap: a 17th open evicts the oldest window.
        let mut full: std::collections::VecDeque<TripReorgWindow> =
            std::collections::VecDeque::new();
        for i in 0..TRIP_REORG_WINDOW_CAP {
            reorg_window_open(&mut full, 1000 + i as u64, 300 + i as u64);
        }
        reorg_window_open(&mut full, 2000, 400);
        assert_eq!(full.len(), TRIP_REORG_WINDOW_CAP, "bounded to the cap");
        assert_eq!(
            full[0].deepest_removed_block, 1001,
            "the oldest window evict"
        );
    }

    /// (iv) The dev-only divergence scanner short-circuits the strict gate
    /// (today's bypass, preserved as-is): a V2 hop (invisible to the CL-only
    /// scanner) yields ZERO reads and `Ok`.
    #[tokio::test]
    async fn judge_divergence_scan_short_circuits_strict_gate() {
        let hop = v2_hop(1000, 2000, 200);
        // No responses queued: ANY eth_call (strict gate or scan) would fail
        // the mock transport's asserter.
        let provider = mock_provider(vec![]);
        let cfg = TripwireConfig {
            divergence_scan: true,
            ..TripwireConfig::enabled_only()
        };
        let v = judge(
            &provider,
            &cfg,
            &[vec![hop]],
            crate::bot_core::solve_anchor::SolveAnchor::for_head(200, 0),
            &[],
        )
        .await;
        assert!(matches!(v, GateVerdict::Ok), "dry-run scan must never trip");
    }

    /// (v) With every probe default-off, a fresh CL hop issues exactly the
    /// strict-gate reads (slot0 + liquidity at its own anchor) — no extras.
    #[tokio::test]
    async fn judge_default_config_issues_only_strict_gate_reads() {
        let sqrt = U256::from(1u128) << 96;
        let hop = SolverHopScalarState {
            hop_type: HopType::V3,
            update_block: 199,
            tick_data_block: 199,
            cl_meta: Some(("Tracked".to_string(), "Live".to_string())),
            v2: None,
            v3: Some((Address::from([0xaau8; 20]), sqrt, 1_000_000, 0)),
            v4: None,
            cl_tick_map: None,
        };
        let provider = mock_provider(vec![v3_slot0_response(sqrt, 0), liq_response(1_000_000)]);
        let v = judge(
            &provider,
            &TripwireConfig::enabled_only(),
            &[vec![hop]],
            crate::bot_core::solve_anchor::SolveAnchor::for_head(200, 0),
            &[],
        )
        .await;
        assert!(matches!(v, GateVerdict::Ok));
    }

    #[test]
    fn anchor_block_uses_update_block_when_set() {
        // A pool with a real update_block is diffed at ITS anchor.
        assert_eq!(solver_anchor_block(100, 102), 100);
        // A never-updated pool (update_block == 0) falls back to the solve block.
        assert_eq!(solver_anchor_block(0, 102), 102);
    }

    #[test]
    fn in_progress_hop_is_skipped_completed_hop_is_not() {
        // A hop touched IN the solve block (mid-block capture) is skipped.
        assert!(skip_in_progress_hop(25_658_682, 25_658_682));
        // A hop equal to or ahead of the solve block is skipped too.
        assert!(skip_in_progress_hop(25_658_683, 25_658_682));
        // A hop one block behind (normal latency) reflects a COMPLETED block
        // and is diffed, NOT skipped.
        assert!(!skip_in_progress_hop(25_658_682, 25_658_683));
        // A never-updated pool (0) is diffed at the solve block, not skipped.
        assert!(!skip_in_progress_hop(0, 100));
        // A genuinely frozen pool (far behind) is NOT skipped — the gate
        // verifies it and would catch the desync.
        assert!(!skip_in_progress_hop(25_658_442, 25_658_682));
    }

    #[test]
    fn fresh_pool_is_not_stale() {
        // A hop 1-2 blocks behind (normal pump latency) is NOT stale.
        assert!(!is_cl_pool_stale(100, 101));
        assert!(!is_cl_pool_stale(100, 102));
        // A hop at/beyond the solve block is not stale (handled by skip).
        assert!(!is_cl_pool_stale(102, 102));
        assert!(!is_cl_pool_stale(102, 101));
        // A never-updated pool (update_block == 0) is not flagged here; the
        // anchor path verifies it at the solve block instead.
        assert!(!is_cl_pool_stale(0, 100));
    }

    #[test]
    fn stale_pool_is_flagged_after_threshold() {
        // Exactly at threshold is not stale.
        assert!(!is_cl_pool_stale(100, 103));
        // One past the threshold -> stale.
        assert!(is_cl_pool_stale(100, 104));
        // The observed failure: a pool ~100 blocks behind IS stale.
        assert!(is_cl_pool_stale(25_664_550, 25_664_704));
    }

    // ---------------------------------------------------------------
    // Solve-anchor FSM seed (`solve_anchor_advancement`)
    //
    // The pure classifier behind the possible-legal-transition gate: may a SOLVE
    // at `anchor` consume this pool? A Tracked Live hop whose `update_block`
    // trails the anchor past `tol` (Consensus Laggard) is the header-promote-
    // ahead-of-apply transition — the 0x99ac8c abort (a quiet pool whose in-
    // block Swap was not applied before solve).
    // ---------------------------------------------------------------
    #[test]
    fn advancement_consensus_within_tolerance() {
        // At the anchor.
        assert_eq!(
            solve_anchor_advancement(100, 100, 3),
            SolveAnchorAdvancement::Consensus
        );
        // Within tolerance (1-2 blocks of normal settle lag).
        assert_eq!(
            solve_anchor_advancement(98, 100, 3),
            SolveAnchorAdvancement::Consensus
        );
        // Exactly at the tolerance boundary.
        assert_eq!(
            solve_anchor_advancement(97, 100, 3),
            SolveAnchorAdvancement::Consensus
        );
        // Ahead of the anchor (mid-block capture / future) is consensus for the
        // gate (handled by the in-progress skip), not a Laggard.
        assert_eq!(
            solve_anchor_advancement(101, 100, 3),
            SolveAnchorAdvancement::Consensus
        );
    }

    #[test]
    fn advancement_never_updated_is_consensus() {
        // update_block == 0 (never advanced) is verified at the anchor instead;
        // never a Laggard.
        assert_eq!(
            solve_anchor_advancement(0, 100, 3),
            SolveAnchorAdvancement::Consensus
        );
    }

    #[test]
    fn advancement_laggard_past_tolerance_reports_stale_by() {
        // One past the tolerance boundary -> Laggard with the exact distance.
        assert_eq!(
            solve_anchor_advancement(96, 100, 3),
            SolveAnchorAdvancement::Laggard { stale_by: 4 }
        );
        // The observed failure: a quiet pool 15 blocks behind the solve anchor.
        assert_eq!(
            solve_anchor_advancement(25_714_794, 25_714_809, 3),
            SolveAnchorAdvancement::Laggard { stale_by: 15 }
        );
        // A zero tolerance flags any strictly-behind hop.
        assert_eq!(
            solve_anchor_advancement(99, 100, 0),
            SolveAnchorAdvancement::Laggard { stale_by: 1 }
        );
        // A large behind distance stays a saturating u64 Laggard (no wrap).
        // (update_block=0 is never-updated -> Consensus, so start from a non-zero
        // behind clock.)
        assert_eq!(
            solve_anchor_advancement(1, u64::MAX, 3),
            SolveAnchorAdvancement::Laggard {
                stale_by: u64::MAX - 1
            }
        );
    }

    #[test]
    fn probe_verdicts_distinct() {
        // The three probe verdicts are distinct discriminators.
        use LaggardProbeVerdict as V;
        assert_ne!(V::Quiet, V::MovedInBlockNotApplied);
        assert_ne!(V::Quiet, V::ReadFailed);
        assert_ne!(V::MovedInBlockNotApplied, V::ReadFailed);
        // Debug round-trips the discriminant.
        assert_eq!(format!("{:?}", V::Quiet), "Quiet");
        assert_eq!(
            format!("{:?}", V::MovedInBlockNotApplied),
            "MovedInBlockNotApplied"
        );
    }

    #[test]
    fn staged_clock_candidate_detects_skipped_price_ahead_of_map() {
        // Bug-A signature: price clock reached the solve block, map clock trails
        // by far (pool B: update 25723658, tick_data 25722568 vs solve).
        assert!(staged_clock_candidate(
            25_723_658, 25_722_568, 25_723_658, 3
        ));
        assert!(staged_clock_candidate(
            25_723_660, 25_723_000, 25_723_658, 3
        ));
    }

    #[test]
    fn staged_clock_candidate_false_for_benign_and_ambiguous() {
        let tol = 3;
        // Not skipped (price clock one block behind a completed solve) → gate
        // diffs it normally; not a skip-path staged-clock case.
        assert!(!staged_clock_candidate(
            25_723_657, 25_723_650, 25_723_658, tol
        ));
        // Map anchor at the solve block (mid-block capture) → ambiguous, never
        // comparable.
        assert!(!staged_clock_candidate(
            25_723_658, 25_723_658, 25_723_658, tol
        ));
        // Never-updated map clock (0) → no completed anchor.
        assert!(!staged_clock_candidate(25_723_658, 0, 25_723_658, tol));
        // Small/benign gap (at or under tolerance) → not worth a read.
        assert!(!staged_clock_candidate(
            25_723_658, 25_723_656, 25_723_658, tol
        ));
        assert!(!staged_clock_candidate(
            25_723_658, 25_723_655, 25_723_658, tol
        ));
        // Price behind map (out-of-range liquidity event) → not a candidate.
        assert!(!staged_clock_candidate(
            25_723_650, 25_723_658, 25_723_658, tol
        ));
    }

    #[test]
    fn v2_match_and_mismatch() {
        let (s0, s1) = (U256::from(1000u64), U256::from(2000u64));
        // Matching chain state → true.
        assert!(v2_state_matches(s0, s1, s0, s1));
        // Off-by-one on either reserve → false.
        assert!(!v2_state_matches(s0, s1, s0 + U256::from(1), s1));
        assert!(!v2_state_matches(s0, s1, s0, s1 - U256::from(1)));
        // Both wrong → false.
        assert!(!v2_state_matches(s0, s1, U256::from(9), U256::from(9)));
    }

    #[test]
    fn cl_match_and_mismatch() {
        let sqrt = U256::from(2u128.pow(100));
        let liq = 1_000_000u128;
        let tick = -12345i32;
        // Matching → true.
        assert!(cl_state_matches(
            sqrt,
            liq,
            tick,
            sqrt,
            U256::from(liq),
            I256::unchecked_from(tick)
        ));
        // sqrt off → false.
        assert!(!cl_state_matches(
            sqrt,
            liq,
            tick,
            sqrt + U256::from(1),
            U256::from(liq),
            I256::unchecked_from(tick)
        ));
        // liquidity off → false.
        assert!(!cl_state_matches(
            sqrt,
            liq,
            tick,
            sqrt,
            U256::from(liq) - U256::from(1),
            I256::unchecked_from(tick)
        ));
        // tick off → false.
        assert!(!cl_state_matches(
            sqrt,
            liq,
            tick,
            sqrt,
            U256::from(liq),
            I256::unchecked_from(tick + 1)
        ));
    }

    // -----------------------------------------------------------------
    // Solve-time tick-map fidelity probe (ADR-021 D3)
    //
    // RED: a CL hop whose in-memory tick map is MISSING an on-chain tick
    // must trip the gate EVEN WHEN the scalar diff passes. The scalar gate
    // compares (sqrt, liquidity=ACTIVE γ, tick); a missing OFF-range position
    // leaves γ unchanged so the scalar passes, yet a solve that crosses into
    // the missing region under-counts liquidity → over-predicts output (the
    // UO3JM4 thin-tick-map class).
    // -----------------------------------------------------------------

    /// Build a mock `AlloyProvider` whose `Asserter` queue is preloaded with
    /// `responses` (one JSON `result` per expected `eth_call`, FIFO). Mirrors
    /// the `liquidity_verifier` mock-transport harness.
    fn mock_provider(responses: Vec<String>) -> AlloyProvider {
        use alloy::network::Ethereum as NetEth;
        use alloy::providers::{Provider, ProviderBuilder};
        use alloy::rpc::client::ClientBuilder;
        use alloy::transports::mock::MockTransport;
        let asserter = Asserter::new();
        for resp in responses {
            asserter.push_success(&resp);
        }
        let client = ClientBuilder::default().transport(MockTransport::new(asserter.clone()), true);
        let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
        AlloyProvider::from_provider(
            Arc::new(dyn_provider) as Arc<dyn alloy::providers::Provider<NetEth>>
        )
    }

    /// Encode a Multicall3 `aggregate3` return payload — `(bool,bytes)[]` —
    /// as a `0x`-prefixed hex string (one `(success, return_data)` tuple per
    /// sub-call, in batch order).
    fn encode_mc3_return(results: &[(bool, Vec<u8>)]) -> String {
        let arr = alloy::dyn_abi::DynSolValue::Array(
            results
                .iter()
                .map(|(ok, data)| {
                    alloy::dyn_abi::DynSolValue::Tuple(vec![
                        alloy::dyn_abi::DynSolValue::Bool(*ok),
                        alloy::dyn_abi::DynSolValue::Bytes(data.clone()),
                    ])
                })
                .collect(),
        );
        format!("0x{}", alloy::primitives::hex::encode(arr.abi_encode()))
    }

    /// 32-byte ABI word for a u256 bitmap value.
    fn u256_word(val: u64) -> Vec<u8> {
        U256::from(val).to_be_bytes::<32>().to_vec()
    }

    /// 64-byte `ticks()` return: `(uint128 liquidityGross, int128 liquidityNet)`.
    fn ticks_return(gross: u128, net: i128) -> Vec<u8> {
        let mut buf = vec![0u8; 64];
        buf[16..32].copy_from_slice(&gross.to_be_bytes());
        let sign = if net < 0 { 0xff } else { 0x00 };
        for b in &mut buf[32..48] {
            *b = sign;
        }
        buf[48..64].copy_from_slice(&net.to_be_bytes());
        buf
    }

    fn tick_info(gross: u128, net: i128) -> degenbot_pools::TickInfo {
        degenbot_pools::TickInfo {
            liquidity_gross: alloy::primitives::U128::from(gross),
            liquidity_net: net,
            block: 0,
        }
    }

    /// The RED case: a V3 hop whose in-memory tick map (tick 0 only) is
    /// missing on-chain tick 60. The scalar reads (slot0 + active liquidity)
    /// MATCH so the scalar gate would pass; only the tick-map fidelity probe
    /// catches the missing position → `verify_solver_hop_states` must return
    /// `Err` with the missing-tick class.
    #[tokio::test]
    async fn tickmap_fidelity_probe_catches_missing_onchain_tick() {
        let sqrt: U256 = U256::from(1u128) << 96;
        let liq = 1_000_000u128;
        let tick = 0i32;
        let mut tick_data = HashMap::new();
        tick_data.insert(0, tick_info(100, 50)); // engine holds ONLY tick 0

        // Scalar + probe responses (FIFO):
        // 1. slot0 (7 words: sqrt, tick=0, five zero tail fields)
        let mut slot0 = vec![0u8; 7 * 32];
        slot0[0..32].copy_from_slice(&sqrt.to_be_bytes::<32>());
        // word 1 = tick 0, words 2-6 = zero tail
        // 2. liquidity (1 word = 1_000_000) — matches solver γ
        let mut liq_word = vec![0u8; 32];
        liq_word[16..32].copy_from_slice(&liq.to_be_bytes());
        // 3. bitmap batch over sorted words [-2,-1,0,1,2]; word 0 = bits {0,1}
        //    (on-chain ticks 0 AND 60 @ spacing 60) → value 3.
        let bitmap = encode_mc3_return(&[
            (true, u256_word(0)), // word -2
            (true, u256_word(0)), // word -1
            (true, u256_word(3)), // word  0 → ticks 0, 60
            (true, u256_word(0)), // word  1
            (true, u256_word(0)), // word  2
        ]);
        // 4. ticks batch over sorted [0, 60]: (100,50) for 0, (200,-30) for 60.
        let ticks = encode_mc3_return(&[
            (true, ticks_return(100, 50)),
            (true, ticks_return(200, -30)),
        ]);
        let provider = mock_provider(vec![
            format!("0x{}", alloy::primitives::hex::encode(&slot0)),
            format!("0x{}", alloy::primitives::hex::encode(&liq_word)),
            bitmap,
            ticks,
        ]);

        let hop = SolverHopScalarState {
            hop_type: HopType::V3,
            update_block: 100,
            tick_data_block: 100,
            cl_meta: None,
            v2: None,
            v3: Some((Address::from([0xaau8; 20]), sqrt, liq, tick)),
            v4: None,
            cl_tick_map: Some(ClTickMapSnapshot {
                tick_spacing: 60,
                active_tick: 0,
                tick_data,
            }),
        };

        // Scalar reads match on-chain (sqrt/γ/tick all equal at anchor 100),
        // so a scalar-only gate returns Ok; the tick-map probe must be what
        // turns this into an Err (the RED assertion).
        let err = verify_solver_hop_states(
            &provider,
            &TripwireConfig::enabled_only(),
            &[hop],
            crate::bot_core::solve_anchor::SolveAnchor::for_head(102, 0),
            &[],
        )
        .await
        .expect_err("gate must trip: on-chain tick 60 is missing from the solver's map");
        assert!(
            err.message.contains("tick-map fidelity probe")
                && err.message.contains("NOT in engine"),
            "expected the missing-tick class in the trip message, got: {}",
            err.message
        );
    }

    /// V4 twin of `tickmap_fidelity_probe_catches_missing_onchain_tick`: a V4
    /// hop whose in-memory tick map is missing an on-chain tick must trip the
    /// gate even though its scalar reads (getSlot0 sqrt/tick + getLiquidity γ)
    /// match on-chain. V4 RPC goes through `StateView.getTickBitmap` /
    /// `getTickLiquidity` (the `state_view` param), but the mock transport
    /// returns queued responses FIFO regardless of target/calldata — so the
    /// bitmap/ticks batch encoding is identical to the V3 case.
    #[tokio::test]
    async fn v4_tickmap_fidelity_probe_catches_missing_onchain_tick() {
        let sqrt: U256 = U256::from(1u128) << 96;
        let liq = 1_000_000u128;
        let tick = 0i32;
        let mut tick_data = HashMap::new();
        tick_data.insert(0, tick_info(100, 50)); // engine holds ONLY tick 0

        let pool_manager = Address::from([0xbbu8; 20]);
        let state_view = Address::from([0xccu8; 20]);
        let pool_id = [0x11u8; 32];

        // 1. getSlot0 (4 words: sqrt, tick=0, protocolFee=0, lpFee=0)
        let mut slot0 = vec![0u8; 4 * 32];
        slot0[0..32].copy_from_slice(&sqrt.to_be_bytes::<32>());
        // word 1 = tick 0, words 2-3 = fees 0
        // 2. getLiquidity (1 word = 1_000_000) — matches solver γ
        let mut liq_word = vec![0u8; 32];
        liq_word[16..32].copy_from_slice(&liq.to_be_bytes());
        // 3. bitmap batch over sorted words [-2,-1,0,1,2]; word 0 = bits {0,1}
        //    (on-chain ticks 0 AND 60 @ spacing 60) → value 3.
        let bitmap = encode_mc3_return(&[
            (true, u256_word(0)), // word -2
            (true, u256_word(0)), // word -1
            (true, u256_word(3)), // word  0 → ticks 0, 60
            (true, u256_word(0)), // word  1
            (true, u256_word(0)), // word  2
        ]);
        // 4. ticks batch over sorted [0, 60]: (100,50) for 0, (200,-30) for 60.
        let ticks = encode_mc3_return(&[
            (true, ticks_return(100, 50)),
            (true, ticks_return(200, -30)),
        ]);
        let provider = mock_provider(vec![
            format!("0x{}", alloy::primitives::hex::encode(&slot0)),
            format!("0x{}", alloy::primitives::hex::encode(&liq_word)),
            bitmap,
            ticks,
        ]);

        let hop = SolverHopScalarState {
            hop_type: HopType::V4,
            update_block: 100,
            tick_data_block: 100,
            cl_meta: None,
            v2: None,
            v3: None,
            v4: Some((pool_manager, pool_id, state_view, sqrt, liq, tick)),
            cl_tick_map: Some(ClTickMapSnapshot {
                tick_spacing: 60,
                active_tick: 0,
                tick_data,
            }),
        };

        let err = verify_solver_hop_states(
            &provider,
            &TripwireConfig::enabled_only(),
            &[hop],
            crate::bot_core::solve_anchor::SolveAnchor::for_head(102, 0),
            &[],
        )
        .await
        .expect_err("gate must trip: V4 on-chain tick 60 missing from the solver's map");
        assert!(
            err.message.contains("tick-map fidelity probe")
                && err.message.contains("NOT in engine"),
            "expected the missing-tick class in the trip message, got: {}",
            err.message
        );
    }

    // -----------------------------------------------------------------
    // Generalized lagging-Tracked-hop reporter (LaggingHop / lagging_tracked_hops)
    //
    // RED: a Tracked CL (V3/V4) hop whose `update_block` trails the promoted
    // solve anchor past MAX_CL_STALENESS_BLOCKS must be reported, GENERALIZED
    // (identity read from the hop itself, never a pool literal); Sparse /
    // Quarantined, never-updated, non-CL, and at-or-within-threshold hops must
    // NOT fire. The reporter is observational only — it deliberately shares
    // the threshold with (and does NOT weaken) the strict UO3JM4 abort gate.
    // -----------------------------------------------------------------

    /// Build a V3/V4 CL hop for the reporter tests. `tick_data_block` mirrors
    /// `update_block`; `cl_meta` is always present (as it is for CL hops in
    /// `extract_solver_hop_states`).
    fn cl_hop(
        hop_type: HopType,
        update_block: u64,
        cov: &str,
        lifecycle: &str,
        v3: Option<(Address, U256, u128, i32)>,
        v4: Option<(Address, [u8; 32], Address, U256, u128, i32)>,
    ) -> SolverHopScalarState {
        SolverHopScalarState {
            hop_type,
            update_block,
            tick_data_block: update_block,
            cl_meta: Some((cov.to_string(), lifecycle.to_string())),
            v2: None,
            v3,
            v4,
            cl_tick_map: None,
        }
    }

    #[test]
    fn parse_cl_staleness_blocks_defaults_on_garbage_or_empty() {
        // Unparseable / empty input falls back to the committed default of 3.
        assert_eq!(parse_cl_staleness_blocks(""), MAX_CL_STALENESS_BLOCKS);
        assert_eq!(parse_cl_staleness_blocks("  "), MAX_CL_STALENESS_BLOCKS);
        assert_eq!(parse_cl_staleness_blocks("abc"), MAX_CL_STALENESS_BLOCKS);
        assert_eq!(parse_cl_staleness_blocks("-1"), MAX_CL_STALENESS_BLOCKS);
    }

    #[test]
    fn parse_cl_staleness_blocks_honors_numeric_override() {
        // Dev dry-run knob: tighten (0/1) or loosen (5) the threshold.
        assert_eq!(parse_cl_staleness_blocks("0"), 0);
        assert_eq!(parse_cl_staleness_blocks("1"), 1);
        assert_eq!(parse_cl_staleness_blocks("2"), 2);
        assert_eq!(parse_cl_staleness_blocks("5"), 5);
        // Whitespace-tolerant.
        assert_eq!(parse_cl_staleness_blocks(" 2 "), 2);
    }

    #[test]
    fn reporter_fires_on_lagging_tracked_v3() {
        let hop = cl_hop(
            HopType::V3,
            100, // update_block
            "Tracked",
            "Live",
            Some((Address::from([0xaau8; 20]), U256::ZERO, 0, 0)),
            None,
        );
        // anchor 105, stale_by = 5 > 3 -> must fire.
        let lag = lagging_tracked_hops(105, std::slice::from_ref(&hop));
        assert_eq!(lag.len(), 1);
        assert_eq!(lag[0].hop_type, HopType::V3);
        assert_eq!(lag[0].stale_by, 5);
        assert_eq!(lag[0].update_block, 100);
        assert!(lag[0].pool.starts_with("v3:"));
    }

    #[test]
    fn reporter_fires_on_lagging_tracked_v4() {
        let hop = cl_hop(
            HopType::V4,
            100,
            "Tracked",
            "Live",
            None,
            Some((Address::ZERO, [0x11u8; 32], Address::ZERO, U256::ZERO, 0, 0)),
        );
        let lag = lagging_tracked_hops(104, std::slice::from_ref(&hop)); // stale_by 4 > 3
        assert_eq!(lag.len(), 1);
        assert_eq!(lag[0].hop_type, HopType::V4);
        assert!(lag[0].pool.starts_with("v4:"));
        assert!(lag[0].pool.contains("11")); // pool_id leading bytes surfaced
    }

    #[test]
    fn reporter_ignores_sparse_and_quarantined() {
        for (cov, lifecycle) in [("Sparse", "Live"), ("Tracked", "Quarantined")] {
            let hop = cl_hop(
                HopType::V3,
                100,
                cov,
                lifecycle,
                Some((Address::ZERO, U256::ZERO, 0, 0)),
                None,
            );
            assert!(
                lagging_tracked_hops(200, std::slice::from_ref(&hop)).is_empty(),
                "cov={cov} lifecycle={lifecycle} must not fire"
            );
        }
    }

    #[test]
    fn reporter_ignores_never_updated_and_at_or_within_threshold() {
        // update_block == 0 (never updated) must not fire even if the anchor is far ahead.
        let never = cl_hop(
            HopType::V3,
            0,
            "Tracked",
            "Live",
            Some((Address::ZERO, U256::ZERO, 0, 0)),
            None,
        );
        assert!(lagging_tracked_hops(1000, std::slice::from_ref(&never)).is_empty());
        // Within threshold (<= 3) must not fire.
        for stale in [0u64, 1, 2, 3] {
            let hop = cl_hop(
                HopType::V3,
                100,
                "Tracked",
                "Live",
                Some((Address::ZERO, U256::ZERO, 0, 0)),
                None,
            );
            assert!(
                lagging_tracked_hops(100 + stale, std::slice::from_ref(&hop)).is_empty(),
                "stale_by={stale} must not fire"
            );
        }
        // At/after the anchor (update_block >= anchor) must not fire.
        let ahead = cl_hop(
            HopType::V3,
            200,
            "Tracked",
            "Live",
            Some((Address::ZERO, U256::ZERO, 0, 0)),
            None,
        );
        assert!(lagging_tracked_hops(100, std::slice::from_ref(&ahead)).is_empty());
    }

    #[test]
    fn reporter_skips_non_cl_and_identity_is_generalized() {
        // V2 hop (cl_meta None) must not fire.
        let v2 = SolverHopScalarState {
            hop_type: HopType::V2,
            update_block: 100,
            tick_data_block: 100,
            cl_meta: None,
            v2: Some((Address::ZERO, U256::ZERO, U256::ZERO)),
            v3: None,
            v4: None,
            cl_tick_map: None,
        };
        assert!(lagging_tracked_hops(200, std::slice::from_ref(&v2)).is_empty());
        // A Tracked V3 hop is flagged and its identity is used verbatim (not a
        // fixed literal), proving the reporter generalizes across pools.
        let addr = Address::from([0xbeu8; 20]);
        let hop = cl_hop(
            HopType::V3,
            100,
            "Tracked",
            "Live",
            Some((addr, U256::ZERO, 0, 0)),
            None,
        );
        let lag = lagging_tracked_hops(105, std::slice::from_ref(&hop));
        assert_eq!(lag.len(), 1);
        assert_eq!(lag[0].pool, format!("v3:{addr}"));
    }

    #[test]
    fn aggregate_lagging_hops_dedupes_by_pool_keeps_max_stale() {
        // The same V3 pool appears in two paths, re-lagging at stale_by 5 and
        // then 9. Across paths/``update_block``s it must collapse to ONE entry
        // carrying the max stale_by (9), not two noisy entries.
        let addr = Address::from([0xcu8; 20]);
        let path_a = vec![cl_hop(
            HopType::V3,
            100, // anchor 105 -> stale_by 5
            "Tracked",
            "Live",
            Some((addr, U256::ZERO, 0, 0)),
            None,
        )];
        let path_b = vec![cl_hop(
            HopType::V3,
            96, // anchor 105 -> stale_by 9
            "Tracked",
            "Live",
            Some((addr, U256::ZERO, 0, 0)),
            None,
        )];
        let map = aggregate_lagging_hops(105, &[path_a, path_b]);
        assert_eq!(
            map.len(),
            1,
            "same pool in N paths must dedupe to a single entry"
        );
        let hop = map.get(&format!("v3:{addr}")).expect("pool present");
        assert_eq!(hop.stale_by, 9, "the max staleness across paths wins");
    }

    #[test]
    fn aggregate_lagging_hops_keys_by_pool_and_filters_non_lagging() {
        // Two distinct pools + one at-or-within-threshold pool: only the two
        // genuinely-lagging ones survive, each keyed by its own identity.
        let a = Address::from([0xaau8; 20]);
        let safe = Address::from([0xccu8; 20]);
        let path = vec![
            cl_hop(
                HopType::V3,
                100, // stale_by 5 > 3 -> lagging
                "Tracked",
                "Live",
                Some((a, U256::ZERO, 0, 0)),
                None,
            ),
            cl_hop(
                HopType::V4,
                100,
                "Tracked",
                "Live",
                None,
                Some((Address::ZERO, [0x11u8; 32], Address::ZERO, U256::ZERO, 0, 0)),
            ),
            cl_hop(
                HopType::V3,
                103, // stale_by 2 <= 3 -> not lagging
                "Tracked",
                "Live",
                Some((safe, U256::ZERO, 0, 0)),
                None,
            ),
        ];
        let map = aggregate_lagging_hops(105, &[path]);
        assert_eq!(map.len(), 2, "only the two genuinely-lagging pools survive");
        assert!(map.contains_key(&format!("v3:{a}")));
        assert!(map
            .values()
            .any(|l| l.hop_type == HopType::V4 && l.pool.starts_with("v4:")));
        assert!(!map.contains_key(&format!("v3:{safe}")));
    }

    // -----------------------------------------------------------------
    // Non-aborting divergence scanner (DivergenceVerdict / scan_lagging_hops_for_divergence)
    //
    // RED: a lagging Tracked Live CL hop whose stored state MATCHES on-chain at
    // the solve block must be classified HONEST; one whose state DIFFERS must be
    // classified DIVERGENT (the real missed-swap desync). The scanner must NOT
    // abort — it returns verdicts. `is_lagging_tracked` must agree with the
    // reporter's filter.
    // -----------------------------------------------------------------

    #[test]
    fn lagging_predicate_agrees_with_reporter_filter() {
        // Tracked Live, stale_by 5 (>3) -> lagging.
        let hop = cl_hop(
            HopType::V3,
            100,
            "Tracked",
            "Live",
            Some((Address::ZERO, U256::ZERO, 0, 0)),
            None,
        );
        assert!(is_lagging_tracked(&hop, 105));
        assert!(!is_lagging_tracked(&hop, 102)); // within threshold
                                                 // Sparse / Quarantined / never-updated / non-CL never lag.
        let sparse = cl_hop(
            HopType::V3,
            100,
            "Sparse",
            "Live",
            Some((Address::ZERO, U256::ZERO, 0, 0)),
            None,
        );
        assert!(!is_lagging_tracked(&sparse, 200));
        let quar = cl_hop(
            HopType::V3,
            100,
            "Tracked",
            "Quarantined",
            Some((Address::ZERO, U256::ZERO, 0, 0)),
            None,
        );
        assert!(!is_lagging_tracked(&quar, 200));
        let never = cl_hop(
            HopType::V3,
            0,
            "Tracked",
            "Live",
            Some((Address::ZERO, U256::ZERO, 0, 0)),
            None,
        );
        assert!(!is_lagging_tracked(&never, 200));
        let v2 = SolverHopScalarState {
            hop_type: HopType::V2,
            update_block: 100,
            tick_data_block: 100,
            cl_meta: None,
            v2: Some((Address::ZERO, U256::ZERO, U256::ZERO)),
            v3: None,
            v4: None,
            cl_tick_map: None,
        };
        assert!(!is_lagging_tracked(&v2, 200));
    }

    #[tokio::test]
    async fn divergence_scan_classifies_honest_and_divergent_v3() {
        let sqrt: U256 = U256::from(1u128) << 96;
        let liq = 1_000_000u128;
        let tick = 0i32;
        let hop = cl_hop(
            HopType::V3,
            100,
            "Tracked",
            "Live",
            Some((Address::from([0xaau8; 20]), sqrt, liq, tick)),
            None,
        );
        // HONEST: on-chain slot0+liq match the solver's stored state.
        let slot0 = {
            let mut v = vec![0u8; 7 * 32];
            v[0..32].copy_from_slice(&sqrt.to_be_bytes::<32>());
            v
        };
        let liqw = {
            let mut v = vec![0u8; 32];
            v[16..32].copy_from_slice(&liq.to_be_bytes());
            v
        };
        let p = mock_provider(vec![
            format!("0x{}", alloy::primitives::hex::encode(&slot0)),
            format!("0x{}", alloy::primitives::hex::encode(&liqw)),
        ]);
        let res = scan_lagging_hops_for_divergence(&p, &[{ &hop }], 105).await;
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].verdict, DivergenceVerdict::Honest);
        assert_eq!(res[0].stale_by, 5);
        assert!(res[0].pool.starts_with("v3:"));

        // DIVERGENT: on-chain sqrt differs at the solve block (real missed-swap).
        let moved = sqrt + U256::from(1u128);
        let slot0d = {
            let mut v = vec![0u8; 7 * 32];
            v[0..32].copy_from_slice(&moved.to_be_bytes::<32>());
            v
        };
        let pd = mock_provider(vec![
            format!("0x{}", alloy::primitives::hex::encode(&slot0d)),
            format!("0x{}", alloy::primitives::hex::encode(&liqw)),
        ]);
        let res = scan_lagging_hops_for_divergence(&pd, &[{ &hop }], 105).await;
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].verdict, DivergenceVerdict::Divergent);
    }

    #[tokio::test]
    async fn divergence_scan_skips_non_lagging_and_v2() {
        // Within threshold -> not scanned (empty result).
        let hop = cl_hop(
            HopType::V3,
            100,
            "Tracked",
            "Live",
            Some((Address::ZERO, U256::ZERO, 0, 0)),
            None,
        );
        let empty =
            scan_lagging_hops_for_divergence(&mock_provider(vec![]), &[{ &hop }], 101).await;
        assert!(empty.is_empty());
        // V2 hop -> skipped (no CL scalar diff).
        let v2 = SolverHopScalarState {
            hop_type: HopType::V2,
            update_block: 100,
            tick_data_block: 100,
            cl_meta: None,
            v2: Some((Address::ZERO, U256::ZERO, U256::ZERO)),
            v3: None,
            v4: None,
            cl_tick_map: None,
        };
        let empty = scan_lagging_hops_for_divergence(&mock_provider(vec![]), &[{ &v2 }], 200).await;
        assert!(empty.is_empty());
    }
}
