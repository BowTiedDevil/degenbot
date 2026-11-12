//! The settlement-arbitrage strategy's sim value types + the production
//! simulation entry (`simulate_path_on_evm`).
//!
//! Owns the strategy-value surface BOTH consumers of the Rust core reach:
//! the type `SimResult`, `FailBuckets`, `compute_priority_fee`, `fits_int128`,
//! `SimulatePath`, `BlockPriorityFees` (re-exported from `degenbot_rpc::fees` —
//! the fee struct is market data, owned by the RPC crate per ADR-019 D5) + the
//! priority-fee constants, + the per-candidate production seam driven per-block
//! through the settle verb an `Evm2SimSession` opens
//! (`Evm2SimSession::run_sequence`).
//!
//! ADR-019 D4/D7 (decision R — Rust-canonical): this is the settlement-arbitrage
//! bundle + gross/net + priority-fee sizing — one strategy over the
//! `degenbot-simulation` engine. ADR-098 D2/D3 splits the seam: the injected
//! [`crate::SimulationDeployment`] owns Encode / Probe / Assess, the engine owns
//! the mechanics (the journaled sequence, gas, return data, trace, swap capture,
//! access list), and this module owns the categorical policy around them (the
//! int128 guard, the bucket taxonomy, the fee sizing). The built-in
//! `cmd_executor` deployment — the command stream, the receipt assessment, the
//! packed cmd config, the warmup math, and the runtime artifact — lives in
//! [`crate::cmd_executor`]. The Python driver is a thin cockpit over a PyO3
//! wrapper around `dispatch_profitable_results` — it does NOT re-derive the
//! settlement's profit measurement (AGENTS.md: "driver shell, not a
//! co-implementation").
//!
//! # In-process sim
//!
//! One opportunity is one transaction: the deployment's declared probes (none
//! for the built-in) bracket it inside one `Evm2SimSession::run_sequence` of
//! journaled steps (the journaled state accumulates across them), and the seam
//! returns the `SimResult` shape the dispatch leaf consumes. Measured latency
//! profile (cold 8374 µs / 9 RPCs; warm 442 µs / 0 RPC; 18.9× speedup vs cold;
//! standing record: ADR-019) + the
//! `ResultAndState.state` access-list emission API. (The detailing spike doc was
//! removed in the stale-docs cleanup `71ec78b2`.)
//!
//! The built-in deployment declares no probe reads: its contract reports
//! `ExecutionReceipt { gross_profit, bribe_paid }` from `execute(bytes,uint256)`,
//! and the adapter assesses that return data (ADR-098 D7). A foreign adapter may
//! still declare pre/post reads; the engine runs them, reports the readings, and
//! the sequence positions they occupy are what a reported `fail_index` names.

// Solidity/EVM identifiers (execute(bytes,uint256), int128, V4 BalanceDelta,
// WETH9, PoolManager, ERC6909, etc.) are ubiquitous here — match the
// degenbot-simulation convention.
#![expect(clippy::doc_markdown)]
use degenbot_core::{diag, diag_trace, op_info};

use std::collections::BTreeMap;

use alloy::eips::eip7825::MAX_TX_GAS_LIMIT_OSAKA;
use alloy::primitives::{Address, I256, U256};
use alloy::rpc::types::AccessList;
use degenbot_core::errors::{ProviderError, ProviderResult};
use degenbot_core::fee_percentiles::{P10_INDEX, P50_INDEX, PRIORITY_FEE_PERCENTILES};
use degenbot_execution::{
    CandidateId, ComposeError, ComposeOptions, ComposerInputs, ExecutionObservation,
    ExecutionResult, FeePolicy, HopInfo, PathInfo, ProbeSpec, TransactionRequest, V2HopInfo,
    V4HopInfo,
};
use degenbot_rpc::fees::BlockPriorityFees;
// The engine types the strategy drives: the production leaf (the
// adapter-driven simulation seam) + the capture vocabulary (ADR-098 D2/D3).
use degenbot_simulation::sim::production::{simulate_transaction, SimulationCapture};
use degenbot_simulation::{CapturedSwap, ProductionSimContext, ProductionSimError};

use crate::candidate::SolveStep;
use crate::deployment::{FeeSizing, SimulateContext, SimulationDeployment};
use crate::selection::PreparedEncoding;

// ─────────────────────────────────────────────────────────────────────────
// Fee-sizing + gas constants
// ─────────────────────────────────────────────────────────────────────────

/// The target profit ratio — the priority fee is sized so the gross profit /
/// (gas_fee + priority_fee*gas) ≈ 1.25.
pub const TARGET_PROFIT_RATIO: f64 = 1.25;

/// The age-decay constant — older results are worth exponentially less:
/// `priority_fee *= 1/(1 + 0.25*age)`.
pub const AGE_DECAY_CONSTANT: f64 = 0.25;

/// The p10 element of the shared percentile pair
/// ([`degenbot_core::fee_percentiles::PRIORITY_FEE_PERCENTILES`]) — the
/// priority fee's floor is `p10 + 1`.
pub const MIN_PRIORITY_FEE_PERCENTILE: u64 = PRIORITY_FEE_PERCENTILES[P10_INDEX];

/// The p50 element of the shared percentile pair
/// ([`degenbot_core::fee_percentiles::PRIORITY_FEE_PERCENTILES`]) — the
/// priority fee's ceiling is `p50 + 1`.
pub const MAX_PRIORITY_FEE_PERCENTILE: u64 = PRIORITY_FEE_PERCENTILES[P50_INDEX];

/// The 1.5× gas safety margin — the simulate's `gasUsed` for the `execute()`
/// call is inflated before being assigned to the tx's gas limit.
pub const GAS_SAFETY_MARGIN: f64 = 1.5;

/// The default gas cap for the `execute()` tx in the in-process sim —
/// pinned to EIP-7825's per-transaction gas-limit cap
/// ([`MAX_TX_GAS_LIMIT_OSAKA`] = 2^24 = 16_777_216, the figure evm2's
/// `Version::tx_gas_limit_cap` installs at Osaka), the
/// realistic per-tx ceiling, replacing the defunct legacy 5M hard cap that
/// false-OOG'd gas-hungry V3/V4 tick-crossing / USDT swaps which succeed
/// on-chain. After the simulate returns the real `gasUsed`, `tx_params["gas"]`
/// is overwritten with `gasUsed * 1.5` ([`GAS_SAFETY_MARGIN`]).
pub const INITIAL_EXECUTE_GAS: u64 = MAX_TX_GAS_LIMIT_OSAKA;

/// The env-var name overriding [`INITIAL_EXECUTE_GAS`]. The default is now the
/// realistic EIP-7825 per-tx gas-limit cap (`16_777_216`), not the legacy 5M
/// artificial ceiling that false-OOG'd gas-hungry swaps in sim while they
/// succeed on-chain. The engine's block gas limit is unbounded by default
/// (block-level validation never binds); at the Osaka spec its EIP-7825
/// `Version::tx_gas_limit_cap` is also `16_777_216`, so this value is both the
/// default AND the effective ceiling — the override can only *lower* it (e.g.
/// to a tighter submission budget) unless the cap is also lifted via
/// `degenbot_simulation::oracle::set_tx_gas_limit_cap(u64::MAX)` (as the
/// investigation oracle does), which removes the EIP-7825 validation stop and
/// lets it be raised toward the ~30M mainnet block ceiling.
pub const EXECUTE_GAS_ENV: &str = "DEGENBOT_SIM_EXECUTE_GAS";

/// Read the effective `execute()` gas limit: `DEGENBOT_SIM_EXECUTE_GAS` when
/// set (parsed as a decimal u64; an unparseable/garbage value logs a warning
/// and falls back to [`INITIAL_EXECUTE_GAS`]), else [`INITIAL_EXECUTE_GAS`].
#[must_use]
pub fn execute_gas_limit() -> u64 {
    // typed schema key `simulation.sim_execute_gas`
    // (`DEGENBOT_SIM_EXECUTE_GAS`); the loader owns the env read.
    ::degenbot_config::holder::config()
        .simulation
        .sim_execute_gas
        .unwrap_or(INITIAL_EXECUTE_GAS)
}

// Production builds `config` through the axis-aware `config_for_options`
// (physical mode by default, ERC6909 mode under `erc6909_profit`).

/// The int128 range bounds (ports `INT128_MIN`/`INT128_MAX` from
/// `degenbot.arbitrage.encoding`, L19–L20).
pub const INT128_MIN: i128 = i128::MIN;
pub const INT128_MAX: i128 = i128::MAX;

// ─────────────────────────────────────────────────────────────────────────
// The int128 guard (C3)
// ─────────────────────────────────────────────────────────────────────────

/// Return `true` if `value` fits in a signed 128-bit integer.
///
/// Ports `fits_int128` (`INT128_MIN <= value <= INT128_MAX`). V4 `BalanceDelta`
/// uses int128 per component, so `amountSpecified` and the swap output delta
/// must both fit — the encoder rejects int128 overflow instead of encoding a
/// call the pool would revert on.
#[must_use]
#[inline]
pub fn fits_int128(value: u128) -> bool {
    // `value` is non-negative by construction (a `u128` solver output).
    // `fits_int128(value)` ⟺ `value <= INT128_MAX` (the lower bound is
    // trivially satisfied since `u128` has no sign bit's payload).
    i128::try_from(value).is_ok() && value <= INT128_MAX as u128
}

// ─────────────────────────────────────────────────────────────────────────
// The result + failure types
// ─────────────────────────────────────────────────────────────────────────

/// The profitable simulation result — `(path_id, gross, net, gas, tx_params,
/// path_info)`.
///
/// Every gross-profitable path is returned, gas-profitable or not: the split
/// between "profitable after gas" and "onchain-valid but unprofitable" belongs
/// to the caller, so a path that clears the chain but not the gas floor stays
/// visible instead of disappearing at the sim seam.
///
/// The returned figures are the DEPLOYMENT's: with
/// [`FeeSizing::AdapterOwned`] they are the adapter's own `ExecutionResult`,
/// unchanged, and `passed` is its verdict.
///
/// A returned result is not thereby submittable: [`Self::is_submittable`]
/// combines the deployment's verdict with the dispatch gas floor, so a failed
/// assessment stays a visible diagnostic row and never reaches submission.
#[derive(Debug, Clone)]
pub struct SimResult {
    /// The candidate this result belongs to — the solver path bound to the
    /// deployment that produced it, so two deployments simulating the same path
    /// stay distinct rows.
    pub candidate_id: CandidateId,
    /// The deployment's assessed gross profit (wei) before gas pricing — for the
    /// built-in executor the contract's own retained figure
    /// (`gross_profit − bribe_paid`, after the bribe and before gas), for a
    /// foreign adapter whatever its Assess reports.
    pub gross_profit: U256,
    /// Net profit = `gross_profit - gas_used * (base_fee_next + priority_fee)`.
    pub net_profit: U256,
    /// The simulate's `gasUsed` for the `execute()` call (call `[3]`),
    /// UN-inflated. The 1.5× safety-margin `tx_params.gas` is exposed via
    /// [`SimResult::inflated_gas`].
    pub gas_used: u64,
    /// The priority fee (wei/gas) this result was priced with: the adapter's
    /// own `FeePolicy` fee for an adapter-owned deployment, else the
    /// market-aware `_compute_priority_fee` output.
    pub priority_fee: u128,
    /// The base fee of the next block (`base_fee_next`).
    pub base_fee_next: u128,
    /// The deployment's FINAL assessment verdict (its own threshold, applied
    /// over its own net). Carried verbatim — dispatch never re-derives it —
    /// so a failed assessment cannot be returned or submitted as profitable
    /// merely because its net clears the dispatch gas floor.
    pub passed: bool,
    /// The `execute()` calldata (selector + ABI-wrapped `(bytes, uint256)`).
    pub execute_calldata: alloy::primitives::Bytes,
    /// The native value (wei) the deployment's Encode part attached to the
    /// transaction: zero for a contract that funds itself from its own balance,
    /// the prepayment for one that does not. The encoded transaction is ONE
    /// fact — `to` from the deployment's target
    /// ([`crate::SimulationDeployment::target`]), `data` and `value` from here —
    /// so the post-target join carries all three to submission.
    pub value: U256,
    /// The EIP-2930 access list computed for the `execute()` call, if any.
    pub access_list: Option<AccessList>,
    /// The swap events (`Sync`/`Swap`) the composite `Evm2SimInspector`'s
    /// capture handle collected during `execute()` call [3] — the V2/V3/V4
    /// pools' own emitted swap events, decoded. The ground-truth "what each
    /// hop actually produced, as simulated" — replaces the onchain-recompute
    /// pipeline. Empty if `execute()` reverted before any swap emitted.
    pub captured_swaps: Vec<CapturedSwap>,
    /// The number of hops in the path (for the caller's `path_info` reshape).
    pub hop_count: usize,
}

impl SimResult {
    /// Whether this result may be submitted: the deployment's assessment passed
    /// AND the net profit clears the dispatch gas floor
    /// ([`crate::dispatch::MIN_PROFIT_NET`]).
    ///
    /// The two decisions stay separate — the adapter owns its verdict, dispatch
    /// owns the floor — and this is the one place they are joined.
    #[must_use]
    pub fn is_submittable(&self) -> bool {
        self.passed && crate::dispatch::is_gas_profitable(self.net_profit)
    }

    /// The 1.5× safety-margin gas assigned to the tx's gas limit.
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    pub fn inflated_gas(&self) -> u64 {
        // `int(gas_used * 1.5)` — the truncate semantics:
        // `gas_used` is `u64`; `* 1.5` is f64; `int(...)` truncates toward zero.
        // For any `u64`, `gas_used as f64 * 1.5` is exact for `gas_used < 2^53`,
        // and `u64` gas values are far below that ceiling.
        let g = (self.gas_used as f64) * GAS_SAFETY_MARGIN;
        g as u64
    }
}

/// A per-path simulation failure record captured alongside the [`FailBuckets`]
/// tally when a path fails simulation, surfaced as `DispatchOutcome::failures`
/// so the Python driver can render a per-candidate `[sim-fail]` line
/// (path_id + bucket label + the failing step's index in the executed sequence +
/// the raw revert data bytes + the inspector-captured reverting-frame
/// attribution when available).
///
/// `fail_index` is `Some(idx)` for failures attributable to a specific
/// simulated step — the revert branch AND a declared probe read whose figure was
/// absent. It is `None` for orchestration-only buckets (`int128-overflow`,
/// `encode-failed`, `rpc-failed`, `balance-decode`, `no-profit`) where no single
/// call failed.
///
/// `reverting_frame` is `Some` only for `execute()` reverts where the
/// `CallTraceInspector` (attached at the opportunity's own step) captured the
/// failing frame — it carries the DEEP attribution (depth, target, selector,
/// revert data, `classify_revert` label) of the frame that actually reverted,
/// rather than the top-level bubble. `None` for orchestration-only buckets + the
/// balance-decode branch (no inspection ran on the failing call).
#[derive(Debug, Clone)]
pub struct SimFailure {
    /// The candidate that failed (mirror of `SimulatePath::candidate_id`).
    pub candidate_id: CandidateId,
    /// The bucket label — the `classify_revert` output for reverts, else the
    /// orchestration-only bucket string.
    pub bucket: String,
    /// The index of the failing step in the executed sequence (the adapter's
    /// declared pre-reads, then the opportunity's transaction, then its
    /// post-reads), if any.
    pub fail_index: Option<usize>,
    /// The raw revert data bytes (the 4-byte selector + ABI-encoded args) —
    /// the TOP-LEVEL revert bubble's data. Empty for orchestration-only
    /// buckets + the balance-decode branch. For `execute()` reverts this is
    /// the same bytes as [`RevertingFrame::revert_data`] (the reverting
    /// frame's depth + target are the new attribution; this field is kept for
    /// the Python `[sim-fail]` column-stability contract).
    pub revert_data: alloy::primitives::Bytes,
    /// The inspector-captured reverting-frame attribution — `Some` only for
    /// `execute()` reverts where the opportunity's own step ran under the
    /// composite inspector's call trace. `None` for orchestration-only buckets +
    /// the balance-decode branch.
    pub reverting_frame: Option<RevertingFrame>,
    /// The swap events captured BEFORE the revert (the swaps `execute()` did
    /// before reverting) — diagnostic for WHY it reverted (e.g. a partial fill
    /// that blew the price limit). Empty for orchestration-only buckets + the
    /// balance-decode branch (no inspection ran).
    pub captured_swaps: Vec<CapturedSwap>,
    /// Total `log_full` hook firings during `execute()` (ALL logs, incl.
    /// non-swap + reverted-frame). Diagnostic: if it exceeds
    /// `captured_swaps.len() + reverted_swaps.len()`, some swap-side LOGs were
    /// dropped from BOTH buffers (the outermost-committed-swap capture gap).
    pub log_full_count: usize,
    /// Swaps emitted inside REVERTED frames — preserved for the divergence
    /// diagnostic (the EVM pops reverted-fame logs, so they don't reach
    /// [`captured_swaps`](Self::captured_swaps)). For a `no-profit`, if the
    /// outermost V3 hop's Swap appears here and not in `captured_swaps`, the
    /// frame was misclassified as reverted (a frame-stack pairing bug) despite
    /// the executor's WETH balance showing it committed.
    pub reverted_swaps: Vec<CapturedSwap>,
    /// The solver's optimal input for this path (`path.optimal_input`). Carried
    /// so the `[sim-diag]` classifier can emit
    /// the expected-vs-actual comparison without re-deriving it — the
    /// *actual* amount comes from `captured_swaps`.
    pub optimal_input: u128,
    /// The solver's per-hop outputs (`path.hop_outputs`). The EXPECTED amount
    /// the solver said each hop would produce; the ACTUAL amount lives on each
    /// `captured_swaps` entry's `amount0`/`amount1` — the gap between them is
    /// the new `SolverCalc` classification basis (replaces the deleted
    /// `recompute.matches_solver`). Empty for orchestration-only buckets where
    /// no hop ran (the int128-overflow guard, the encode-failed guard).
    pub hop_outputs: Vec<u128>,
    /// Compact per-frame summary of the full EVM call trace captured during
    /// `execute()` (`depth,target,selector,outcome_kind,gas_used`). Populated
    /// for inspectable calls ([3]) to show hop-entry/completion — e.g. a
    /// `no-profit` where hop0 + mid hops emit but the outermost V3 swap does
    /// not. Empty when no trace was captured.
    pub call_trace: Vec<String>,
}

/// The inspector-captured attribution of a failing `execute()` frame — the
/// depth, target, selector, revert data, + `classify_revert` label of the
/// deepest non-`Success` frame the composite inspector's call trace saw
/// during call [3]. Replaces the top-level-only `fail_index` + revert-bubble
/// data with the frame that actually reverted (or halted).
///
/// For a `Halt` (e.g. `0xfe` INVALID, OOG), `revert_data` is empty + `label`
/// is `classify_revert` on empty bytes (the `"empty"` bucket) — parity with
/// the pre-inspector behavior, now attributed to the halting frame's target.
#[derive(Debug, Clone)]
pub struct RevertingFrame {
    /// The call-stack depth (1 = top-level `execute()`, 2 = first sub-call, …).
    pub depth: usize,
    /// The reverting/halting contract's address.
    pub target: alloy::primitives::Address,
    /// The first 4 bytes of the call's calldata (the Solidity selector).
    pub selector: [u8; 4],
    /// The reverting frame's revert data (`0x` for a `Halt`).
    pub revert_data: alloy::primitives::Bytes,
    /// The `classify_revert` label run on `revert_data`.
    pub label: String,
    /// Whether the frame is a clean `REVERT` or a hard `HALT` (OOG, `0xfe`
    /// invalid opcode, call-too-deep …). A `Halt` carries no revert data, so it
    /// lands in the `"empty"` bucket — this discriminant + `gas_used` let the
    /// operator tell an OOG/invalid from a clean `require` failure.
    pub outcome_kind: &'static str,
    /// Gas spent by the failing frame (useful to confirm an OOG: compares
    /// against the frame's `gas_limit`).
    pub gas_used: u64,
}

/// The shapes a zero-gross-profit simulation outcome can take, as a labeled
/// discriminant rather than a `no-profit` catch-all.
///
/// One bucket used to carry two different verdicts: an ECONOMIC one (the path
/// traded and its balance legs netted ≤ 0) and an ORCHESTRATION one
/// (`execute()` emitted no swap at all — nothing ran). An empty
/// `captured_swaps` list was documented as "`execute()` did nothing", so a
/// record could not say which it was, and every legitimately empty list read as
/// the other class. The captured swap set is the discriminator, so the label
/// derives from the same record it labels and cannot disagree with the evidence
/// beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoProfitShape {
    /// Committed swaps were captured: the executor traded and the balance delta
    /// still netted to zero — an economic verdict (`SolverCalc`/`Encoding`
    /// suspect), not a no-op.
    SwapsEmitted,
    /// No committed swap, but swaps were captured inside REVERTED frames: the
    /// frame classification disagrees with the balance delta (the pairing bug
    /// `reverted_swaps` exists to expose).
    SwapsReverted,
    /// No swap captured anywhere: `execute()` did nothing (an empty encode, a
    /// no-op executor) — the only case where that reading is honest.
    ExecuteDidNothing,
}

impl NoProfitShape {
    /// Classify a zero-gross outcome from its two captured swap sets.
    #[must_use]
    pub fn of(captured_swaps: &[CapturedSwap], reverted_swaps: &[CapturedSwap]) -> Self {
        if !captured_swaps.is_empty() {
            Self::SwapsEmitted
        } else if !reverted_swaps.is_empty() {
            Self::SwapsReverted
        } else {
            Self::ExecuteDidNothing
        }
    }

    /// The `[sim] by reason` bucket label this shape tallies under.
    #[must_use]
    pub const fn bucket(self) -> &'static str {
        match self {
            Self::SwapsEmitted => "no-profit",
            Self::SwapsReverted => "no-profit-reverted",
            Self::ExecuteDidNothing => "execute-no-op",
        }
    }
}

/// A revert-bucket tally accumulator: one increment per bucket hit, so the
/// driver can render the reason breakdown from typed counts.
/// The bucket strings are the `classify_revert` labels + a handful of
/// orchestration-only buckets (`int128-overflow`, `rpc-failed`,
/// `balance-decode`, `no-profit`, `encode-failed`, `blocked-path`).
///
/// In addition to the per-bucket count, this struct carries the per-path
/// [`SimFailure`] records (one per `tally`/`record` site) so the dispatch
/// outcome can surface per-candidate failure detail across the FFI boundary —
/// the count alone is insufficient for the operator to identify WHICH path
/// reverted against WHICH pools.
///
/// The rendering of these maps (the `[sim] summary` + the `[sim-fail]` lines)
/// stays in the Python driver — sibling D4 `stays-python`. This leaf only
/// ACCUMULATES.
#[derive(Debug, Default, Clone)]
pub struct FailBuckets {
    buckets: BTreeMap<String, u64>,
    failures: Vec<SimFailure>,
}

impl FailBuckets {
    /// Construct an empty tally.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Increment the bucket count (ports `_tally_fail`).
    pub fn tally(&mut self, bucket: &str) {
        *self.buckets.entry(bucket.to_string()).or_insert(0) += 1;
    }

    /// Record a per-path failure: increment the bucket count AND push the
    /// [`SimFailure`] detail so the dispatch outcome can surface per-candidate
    /// attribution (`path_id` + `fail_index` + `revert_data`).
    ///
    /// `tally` sites that should also be surfaced as a `[sim-fail]` detail call
    /// this instead of `tally` directly (most do — the exception is the
    /// hop-output length mismatch guard, which is a pre-sim skip, not a
    /// sim-failed bucket, and stays `tally`-less).
    pub fn record(
        &mut self,
        candidate_id: CandidateId,
        bucket: &str,
        fail_index: Option<usize>,
        revert_data: alloy::primitives::Bytes,
        optimal_input: u128,
        hop_outputs: Vec<u128>,
    ) {
        self.tally(bucket);
        // every classified sim failure surfaces through OTel (span
        // status + exception event + degenbot.errors counter). Callers hold
        // the `degenbot.bundle.simulate` span, so path context rides along.
        degenbot_bot::telemetry::record_exception(
            degenbot_bot::telemetry::error_kind::SIM_FAILURE,
            format_args!("path {candidate_id} bucket {bucket} fail_index {fail_index:?}"),
        );
        self.failures.push(SimFailure {
            candidate_id,
            bucket: bucket.to_string(),
            fail_index,
            revert_data,
            reverting_frame: None,
            captured_swaps: Vec::new(),
            log_full_count: 0,
            reverted_swaps: Vec::new(),
            optimal_input,
            hop_outputs,
            call_trace: Vec::new(),
        });
    }

    /// Record a per-path success-path failure WITH the captured swap events
    /// but WITHOUT a reverting frame (no call reverted). The companion to
    /// [`record`](Self::record) for the success-path-no-profit case: the
    /// inspector-captured `execute()` swap events ARE in scope at the
    /// recording site (they are drained before the profit check), but
    /// [`record`](Self::record) hardcodes `captured_swaps: Vec::new()`.
    ///
    /// The bucket is DERIVED from the swap sets ([`NoProfitShape::of`]), never
    /// passed: a caller-supplied label let an empty `captured_swaps` list
    /// record as a plain `no-profit`, which downstream reads as "the path
    /// traded and earned nothing" when it means "`execute()` emitted no swap
    /// at all". Callers cannot mislabel a shape they do not name.
    #[expect(clippy::too_many_arguments)] // path attribution + swap/trace detail
    pub fn record_no_profit(
        &mut self,
        candidate_id: CandidateId,
        captured_swaps: Vec<CapturedSwap>,
        reverted_swaps: Vec<CapturedSwap>,
        log_full_count: usize,
        optimal_input: u128,
        hop_outputs: Vec<u128>,
        call_trace: Vec<String>,
    ) {
        let bucket = NoProfitShape::of(&captured_swaps, &reverted_swaps).bucket();
        self.tally(bucket);
        self.failures.push(SimFailure {
            candidate_id,
            bucket: bucket.to_string(),
            fail_index: None,
            revert_data: alloy::primitives::Bytes::new(),
            reverting_frame: None,
            captured_swaps,
            log_full_count,
            reverted_swaps,
            optimal_input,
            hop_outputs,
            call_trace,
        });
    }

    /// Record a per-path `execute()` revert WITH the inspector-captured
    /// reverting-frame attribution. Like [`record`](Self::record) but populates
    /// [`SimFailure::reverting_frame`] — the deep (depth/target/selector/
    /// revert-data/label) attribution of the frame that actually reverted.
    #[expect(clippy::too_many_arguments)]
    pub fn record_revert(
        &mut self,
        candidate_id: CandidateId,
        bucket: &str,
        fail_index: Option<usize>,
        reverting_frame: RevertingFrame,
        captured_swaps: Vec<CapturedSwap>,
        optimal_input: u128,
        hop_outputs: Vec<u128>,
    ) {
        self.tally(bucket);
        self.failures.push(SimFailure {
            candidate_id,
            bucket: bucket.to_string(),
            fail_index,
            revert_data: reverting_frame.revert_data.clone(),
            reverting_frame: Some(reverting_frame),
            captured_swaps,
            log_full_count: 0,
            reverted_swaps: Vec::new(),
            optimal_input,
            hop_outputs,
            call_trace: Vec::new(),
        });
    }

    /// Read a bucket's count (0 if absent).
    #[must_use]
    pub fn get(&self, bucket: &str) -> u64 {
        self.buckets.get(bucket).copied().unwrap_or(0)
    }

    /// The underlying bucket→count map (for the Python driver's `[sim] summary`).
    #[must_use]
    pub fn buckets(&self) -> &BTreeMap<String, u64> {
        &self.buckets
    }

    /// The underlying bucket→count map, mutable (for the dispatch fan-out to
    /// merge per-path buckets into the outcome tally).
    pub fn buckets_mut(&mut self) -> &mut BTreeMap<String, u64> {
        &mut self.buckets
    }

    /// The per-path [`SimFailure`] records accumulated via [`record`] —
    /// surfaced as `DispatchOutcome::failures` for the Python driver's
    /// `[sim-fail]` line.
    #[must_use]
    pub fn failures(&self) -> &[SimFailure] {
        &self.failures
    }

    /// Take ownership of the per-path failures (used by the dispatch fan-out
    /// to move the per-path record set into the outcome tally without cloning).
    #[must_use]
    pub fn into_failures(self) -> Vec<SimFailure> {
        self.failures
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The market-aware age-decay priority fee (C4)
// ─────────────────────────────────────────────────────────────────────────

/// Compute the market-aware priority fee with age decay.
///
/// The fee is:
/// 1. **Target fee** — the priority fee that achieves `TARGET_PROFIT_RATIO`
///    (1.25): `int((gross_profit / 1.25 - gas_used * base_fee_next) / gas_used)`,
///    floored at 1.
/// 2. **Age decay** — older results are worth exponentially less:
///    `priority_fee = int(target_priority_fee * (1 / (1 + 0.25 * age)))`,
///    where `age = max(0, current_block - solve_block)`.
/// 3. **Market bounds** — clamped to `[min_priority_fee, max_priority_fee]`,
///    where `min = max(p10 + 1, 1)` and `max = max(p50 + 1, min)`. If no
///    feeHistory percentiles are available, `min = 1` and `max = target`.
///
/// # Float→int truncate semantics
///
/// Both `target_priority_fee` and `priority_fee` land via `as u128` casts on
/// `f64`, which truncate toward zero — a priority fee is never rounded up past
/// what the margin supports. The only values reachable here are non-negative
/// (non-negative inputs, and the floor at 1 keeps the target positive), so the
/// cast never saturates.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
pub fn compute_priority_fee(
    gross_profit: U256,
    gas_used: u64,
    base_fee_next: u128,
    solve_block: u64,
    current_block: u64,
    block_priority_fees: Option<&BlockPriorityFees>,
) -> u128 {
    // Target fee from profit ratio.
    if gas_used == 0 {
        return 1;
    }
    let gross_f64 = u256_to_f64_lossy(gross_profit);
    let gas_f64 = gas_used as f64;
    let base_f64 = base_fee_next as f64;
    // `target_priority_fee = max(1, int((gross / 1.25 - gas * base) / gas))`.
    let target_raw = (gross_f64 / TARGET_PROFIT_RATIO - gas_f64 * base_f64) / gas_f64;
    let target_priority_fee = target_raw.max(1.0) as u128;

    // Age decay: factor = 1 / (1 + 0.25 * age). `age = max(0, current - solve)`.
    let age = current_block.saturating_sub(solve_block);
    let age_factor = 1.0 / (1.0 + AGE_DECAY_CONSTANT * age as f64);
    let priority_fee = (target_priority_fee as f64 * age_factor) as u128;

    // Market bounds from feeHistory.
    let mut min_priority_fee: u128 = 1;
    let mut max_priority_fee: u128 = target_priority_fee;
    if let Some(fees) = block_priority_fees {
        let p10 = fees.p10.to::<u128>();
        let p50 = fees.p50.to::<u128>();
        min_priority_fee = p10.saturating_add(1).max(1);
        max_priority_fee = p50.saturating_add(1).max(min_priority_fee);
    }

    // Clamp: `max(min, min(priority_fee, max))`.
    priority_fee.clamp(min_priority_fee, max_priority_fee)
}

/// Lossy `U256` → `f64` — the promotion the fee sizing needs to mix an integer
/// wei gross with an `f64` ratio. The fee's contract is the truncation of the
/// FINAL float, not the precision of the gross: gross profit is wei-denominated
/// and realistically < 2^53, where f64 is exact.
#[expect(clippy::cast_precision_loss)]
fn u256_to_f64_lossy(v: U256) -> f64 {
    // `U256` → `f64` via the little-endian u64 limbs (high limb first).
    //
    // What matters is the truncate of the FINAL float (the priority fee), not
    // the f64 precision of the gross. Gross profits are wei-denominated and
    // realistically < 2^53, where the f64 is exact.
    let limbs: &[u64; 4] = v.as_limbs();
    let mut acc: f64 = 0.0;
    for &limb in limbs.iter().rev() {
        acc = acc * ((1u128 << 64) as f64) + limb as f64;
    }
    acc
}

/// The V2 hop's input token address (`zfo=true` → token0).
fn v2_input_token(hop: &V2HopInfo) -> Address {
    if hop.zfo {
        hop.token0_address
    } else {
        hop.token1_address
    }
}

/// The V2 hop's output token address (`zfo=true` → token1).
fn v2_output_token(hop: &V2HopInfo) -> Address {
    if hop.zfo {
        hop.token1_address
    } else {
        hop.token0_address
    }
}

/// The V4 hop's input currency (`zfo=true` → currency0).
fn v4_input_currency(hop: &V4HopInfo) -> Address {
    if hop.zfo {
        hop.currency0_address
    } else {
        hop.currency1_address
    }
}

/// The V4 hop's output currency (`zfo=true` → currency1).
fn v4_output_currency(hop: &V4HopInfo) -> Address {
    if hop.zfo {
        hop.currency1_address
    } else {
        hop.currency0_address
    }
}

/// Scan `hops` for a V4↔V2 adjacency that needs a representation bridge
/// (native on the V4 side, WETH on the V2 side) — the one path shape the
/// 3-hop composers do NOT encode. Returns a human-readable description of
/// the gap when one exists; `None` otherwise.
///
/// Native-ETH and WETH are economically the same token but V4 tracks them
/// as distinct delta currencies (`NATIVE_ADDRESS` vs the WETH ERC-20), while
/// V2 pools hold the WETH ERC-20 token directly. A path whose adjacency is
/// V4(native) → V2(WETH) or V2(WETH) → V4(native) would need an explicit
/// `WETH_DEPOSIT` / `WETH_WITHDRAW` opcode bridging the representation gap —
/// which the 2-hop `encode_cmd_v4_v2` emits but the 3-hop V4-V2 / V2-V4
/// composers do not (the V2 callback's received-token accounting would need
/// per-composer sync/settle restructuring to port the 2-hop bridge).
///
/// Empirical finding: across ~148 mainnet blocks and
/// ~1325 simulated V4-containing candidates, zero such paths materialized —
/// the pathfinder's token-graph adjacency never paired a native-currency V4
/// pool against a WETH-input V2 pool at an interior boundary. This probe
/// stays in-tree (gated by `DEGENBOT_BRIDGE_PROBE`) so a future materialization
/// is observed rather than silently dropping as `encode-failed`.
#[must_use]
fn scan_for_v4_v2_boundary_bridge(hops: &[HopInfo], weth_address: Address) -> Option<String> {
    const NATIVE: Address = Address::ZERO;
    for i in 0..hops.len().saturating_sub(1) {
        let (a, b) = (&hops[i], &hops[i + 1]);
        // V4 → V2: V4's output is native, V2's input is WETH.
        if let (HopInfo::V4(va), HopInfo::V2(vb)) = (a, b) {
            if v4_output_currency(va) == NATIVE && v2_input_token(vb) == weth_address {
                return Some(format!(
                    "boundary {}→{}: V4 native-out → V2 WETH-in (needs Wrap)",
                    i,
                    i + 1
                ));
            }
        }
        // V2 → V4: V2's output is WETH, V4's input is native.
        if let (HopInfo::V2(vb), HopInfo::V4(va)) = (a, b) {
            if v2_output_token(vb) == weth_address && v4_input_currency(va) == NATIVE {
                return Some(format!(
                    "boundary {}→{}: V2 WETH-out → V4 native-in (needs Unwrap)",
                    i,
                    i + 1
                ));
            }
        }
    }
    None
}

// ─────────────────────────────────────────────────────────────────────────
// The orchestration inputs
// ─────────────────────────────────────────────────────────────────────────

/// Per-path inputs to [`simulate_path_on_evm`].
///
/// One candidate's path plus the solver's per-hop rows and the block the solver
/// produced it on. The Encode policy is the DEPLOYMENT's (the injected adapter
/// owns it), so a candidate never carries a second copy of it.
#[derive(Debug, Clone)]
pub struct SimulatePath {
    /// The candidate identity: the solver path bound to the deployment that
    /// executes it.
    pub candidate_id: CandidateId,
    /// The arb's optimal input (`optimal_input`).
    pub optimal_input: u128,
    /// Per-hop rows (output / executable input / solve-time nonce) — one
    /// allocation instead of the former three parallel `Vec`s.
    pub steps: Box<[SolveStep]>,
    /// The path info (the ordered hops the deployment's Encode part encodes).
    pub path_info: PathInfo,
    /// The block the solver produced the result on.
    pub solve_block: u64,
}

impl SimulatePath {
    /// The number of hops in the path (convenience for `path_info.hops.len()`).
    #[must_use]
    pub fn hop_count(&self) -> usize {
        self.path_info.hops.len()
    }

    /// The solver's per-hop expected outputs as a flat `Vec` for the
    /// failure-telemetry intake (`FailBuckets::record` takes `Vec<u128>`); a
    /// failure-path-only view of the merged rows.
    #[must_use]
    pub fn expected_outputs_vec(&self) -> Vec<u128> {
        self.steps.iter().map(|s| s.output).collect()
    }

    /// The solver's per-hop executable inputs (the concentrated-liquidity clamp
    /// landed here) as a flat `Vec` — the second half of the Encode intake.
    #[must_use]
    pub fn consumed_inputs_vec(&self) -> Vec<u128> {
        self.steps.iter().map(|s| s.consumed_input).collect()
    }

    /// Whether any hop is a V4 hop (the int128 guard keys on this).
    #[must_use]
    pub fn has_v4_hop(&self) -> bool {
        self.path_info
            .hops
            .iter()
            .any(|h| matches!(h, HopInfo::V4(_)))
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The in-process entry point
// ─────────────────────────────────────────────────────────────────────────

// The engine `Evm2BlockSimHandle` (the per-block shared EVM handle) + the
// `Evm2ProductionBlockDb`/`Evm2ScratchDb` type aliases live in
// `degenbot-simulation` (the engine). This strategy builds the handle via
// `Evm2BlockSimHandle::build` and hands the session the 7 calls as one
// sequence: the bundle is the consumer whose journal must accumulate ACROSS
// calls, which is what `Evm2SimSession::run_sequence` is for.

// ── The evm2 twin (ADR-089, hard-swap wiring) ────────────────────────────
//
// `simulate_path_on_evm` is the production seam: the SAME
// policy (encoders, bucket taxonomy, profit/priority-fee math) driven
// through `Evm2SimSession`'s journaled sequence. Engine substitutions
// only: envelopes for `TxEnv`s, the ambient composite inspector for the
// per-step tuple + handles, and the sequence-returned capture for the
// handle drains. This twin retires the revm seam at the hard cutover —
// no genericization layer.

/// On close, records the `simulate.*` span fields when the
/// per-candidate sim span closes. Default verdict is `not_profitable`
/// (every `Ok(None)` branch falls through to it); the success + error sites
/// override before returning. `span.record` ignores undeclared field names
/// (as_field), so the fields are declared as `tracing::field::Empty`
/// placeholders at span creation.
/// The closed verdict vocabulary stamped on the per-candidate sim span
/// and exported as the `simulate.verdict` metric label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SimVerdict {
    Profitable,
    NotProfitable,
    Error,
}

impl SimVerdict {
    fn as_str(self) -> &'static str {
        match self {
            Self::Profitable => "profitable",
            Self::NotProfitable => "not_profitable",
            Self::Error => "error",
        }
    }
}

struct SimSpanVerdict {
    span: tracing::Span,
    verdict: SimVerdict,
    /// ADR-040: the closed `telemetry::error_reason` classification for an
    /// error-verdict sim (`None` until an error seam stamps it). Recorded on
    /// the span and tallied via `degenbot.sim.error_reason{reason}`.
    error_reason: Option<&'static str>,
    profit: Option<i64>,
    /// T3: wall-clock anchor for the per-path simulate duration histogram.
    started: std::time::Instant,
}

impl Drop for SimSpanVerdict {
    fn drop(&mut self) {
        self.span.record("simulate.verdict", self.verdict.as_str());
        if let Some(profit) = self.profit {
            self.span.record("simulate.expected_profit", profit);
        }
        // T3: verdict counter (labeled, small closed set) + duration histogram.
        degenbot_bot::instruments::record(
            degenbot_bot::instruments::CensusEvent::SimulateVerdict {
                verdict: self.verdict.as_str(),
            },
        );
        degenbot_bot::instruments::record(degenbot_bot::instruments::CensusEvent::PathSimulated {
            secs: self.started.elapsed().as_secs_f64(),
        });
        // ADR-040: error-outcome sims tally by closed reason (the bucket
        // table keys the reaction; the label set stays closed).
        if self.verdict == SimVerdict::Error {
            if let Some(reason) = self.error_reason {
                degenbot_bot::instruments::record(
                    degenbot_bot::instruments::CensusEvent::SimErrorReason { reason },
                );
            }
        }
        if self.verdict == SimVerdict::Error {
            if let Some(reason) = self.error_reason {
                self.span.record("simulate.error_reason", reason);
            }
        }
    }
}

/// The evm2 in-process entry point (mirrors `simulate_in_process_with_db`):
/// overrides already applied by the caller over the evm2 `CacheDB`; the
/// EVM built with the engine's per-block policy; the production seam
/// in [`simulate_path_on_evm`].
///
/// # Errors
///
/// Same contract as the revm entry (`Ok(None)` for non-profitable /
/// reverted outcomes; `Err` only on an unrecoverable engine refusal).
pub fn simulate_in_process_with_db<Db>(
    ctx: &SimulateContext<'_>,
    cache_db: ::evm2::evm::CacheDB<Db>,
    path: &SimulatePath,
    fail_buckets: &mut FailBuckets,
) -> ProviderResult<Option<SimResult>>
where
    Db: ::evm2::evm::DynDatabase,
{
    let mut evm = degenbot_simulation::sim::evm2::build_block_evm2(
        ::evm2::evm::Db::new(cache_db),
        ctx.block.base_fee_next,
        ctx.block.current_block,
        ctx.block.block_timestamp,
    );
    let block_env = degenbot_simulation::sim::evm2::handle_block_env(
        ctx.block.base_fee_next,
        ctx.block.current_block,
        ctx.block.block_timestamp,
    );
    let mut session = degenbot_simulation::sim::evm2::session::Evm2SimSession::with_block_env(
        &mut evm, block_env,
    );
    simulate_path_on_evm(&mut session, ctx, path, fail_buckets)
}

/// The evm2 variant of [`simulate_path_on_evm`] (see the twin note above).
/// The policy is verbatim the revm seam's; only the engine surface differs.
///
/// # Errors
///
/// Same contract as the revm seam.
pub fn simulate_path_on_evm(
    session: &mut degenbot_simulation::sim::evm2::session::Evm2SimSession<'_, '_>,
    ctx: &SimulateContext<'_>,
    path: &SimulatePath,
    fail_buckets: &mut FailBuckets,
) -> ProviderResult<Option<SimResult>> {
    let span = tracing::info_span!(
        "degenbot.bundle.simulate",
        path_id = path.candidate_id.path_id(),
        simulate.verdict = tracing::field::Empty,
        simulate.expected_profit = tracing::field::Empty,
        simulate.error_reason = tracing::field::Empty,
    );
    simulate_path_on_evm_in_span(session, ctx, path, fail_buckets, &span)
}

/// Simulate a candidate whose deployment already took its Encode decision.
///
/// `prepared` is the selection walk's answer for THIS candidate (see
/// [`crate::selection`]): an accepted transaction is simulated exactly as
/// composed — the adapter's `encode` callback does not run again — and a
/// recorded decline or failure is categorised without calling it at all.
/// `None` composes here, the explicit all-target fan-out's shape.
///
/// `fanout` is the dispatch fan-out this candidate belongs to, echoed on its
/// [`fanout_trace`](crate::fanout_trace) row so the row joins the fan-out's
/// dispatch-level telemetry. A caller outside any fan-out — the inline hook
/// re-quote arm — passes `None` and its row stays unattributed.
///
/// # Errors
///
/// Same contract as the revm seam.
pub fn simulate_path_on_evm_prepared(
    session: &mut degenbot_simulation::sim::evm2::session::Evm2SimSession<'_, '_>,
    ctx: &SimulateContext<'_>,
    path: &SimulatePath,
    prepared: Option<&PreparedEncoding>,
    fail_buckets: &mut FailBuckets,
    fanout: Option<u64>,
) -> ProviderResult<Option<SimResult>> {
    let span = tracing::info_span!(
        "degenbot.bundle.simulate",
        path_id = path.candidate_id.path_id(),
        simulate.verdict = tracing::field::Empty,
        simulate.expected_profit = tracing::field::Empty,
        simulate.error_reason = tracing::field::Empty,
    );
    simulate_path_on_evm_in_span_prepared(session, ctx, path, prepared, fail_buckets, &span, fanout)
}

/// The caller-held-span form (mirrors `simulate_path_on_evm_in_span`).
///
/// # Errors
///
/// Same contract as the revm seam.
pub fn simulate_path_on_evm_in_span(
    session: &mut degenbot_simulation::sim::evm2::session::Evm2SimSession<'_, '_>,
    ctx: &SimulateContext<'_>,
    path: &SimulatePath,
    fail_buckets: &mut FailBuckets,
    span: &tracing::Span,
) -> ProviderResult<Option<SimResult>> {
    simulate_path_on_evm_in_span_prepared(session, ctx, path, None, fail_buckets, span, None)
}

/// The span-taking body both public entries share.
fn simulate_path_on_evm_in_span_prepared(
    session: &mut degenbot_simulation::sim::evm2::session::Evm2SimSession<'_, '_>,
    ctx: &SimulateContext<'_>,
    path: &SimulatePath,
    prepared: Option<&PreparedEncoding>,
    fail_buckets: &mut FailBuckets,
    span: &tracing::Span,
    fanout: Option<u64>,
) -> ProviderResult<Option<SimResult>> {
    // The candidate's PRODUCTION relevance set: what its path and the
    // deployment's settlement declaration make relevant, derived HERE — before
    // the frame runs, from facts the path already carries. The trace records it
    // beside the post-read walk so a snapshot replay can scope its snapshot by
    // something the live run actually knew; deriving it from the execution's own
    // access list would hand the replay an oracle no live run ever had.
    let relevance = degenbot_simulation::sim::prefetch::path_relevance_addresses(
        std::iter::once(&path.path_info),
        &degenbot_simulation::sim::prefetch::PrefetchFacts {
            settlement: ctx.deployment.settlement(),
        },
    );
    let mut trace_reads = None;
    let (result, sim_dur) = degenbot_simulation::sim::sim_metrics::timed(|| {
        simulate_path_on_evm_seam(
            session,
            ctx,
            path,
            prepared,
            fail_buckets,
            span,
            &mut trace_reads,
        )
    });
    degenbot_simulation::sim::sim_metrics::record_sim(sim_dur);
    // One TRACE-level fan-out-trace row per candidate (the recorded-fan-out
    // replay's corpus). Gated by the `degenbot=trace` sink, never the console.
    let (verdict, gas) = match &result {
        Ok(Some(sim)) => ("result", sim.gas_used),
        Ok(None) => ("none", 0),
        Err(_) => ("error", 0),
    };
    let empty = AccessList::default();
    crate::fanout_trace::record(
        ctx.block.current_block,
        &path.candidate_id.to_string(),
        u64::try_from(sim_dur.as_nanos()).unwrap_or(u64::MAX),
        verdict,
        gas,
        trace_reads.as_ref().unwrap_or(&empty),
        &relevance,
        fanout,
    );
    result
}

/// The deployment's Encode decision for one path: ONE complete transaction
/// (ADR-098 D2), built from the same neutral [`ComposerInputs`] whoever asks.
///
/// The target-selection walk ([`crate::selection`]) and this seam both call it,
/// so an adapter's `encode` callback can never see two spellings of one solve
/// result — and a caller holding an accepted request never needs to ask twice.
///
/// # Errors
///
/// The adapter's own [`ComposeError`]: [`ComposeError::Unsupported`] when the
/// path is outside the contract's capability, [`ComposeError::Encode`] when a
/// primitive encoding step failed, [`ComposeError::Validation`] when the produced
/// transaction violates the interface its adapter declared (ADR-100 D5).
pub fn compose_transaction(
    deployment: &SimulationDeployment,
    path: &SimulatePath,
) -> Result<TransactionRequest, ComposeError> {
    let hop_outputs = path.expected_outputs_vec();
    let consumed_inputs = path.consumed_inputs_vec();
    let inputs = ComposerInputs {
        optimal_input: path.optimal_input,
        hop_outputs: &hop_outputs,
        consumed_inputs: &consumed_inputs,
        opts: ComposeOptions,
    };
    deployment.adapter().encode(&path.path_info, &inputs)
}

/// The production simulation seam: the deployment's Encode, the engine's
/// mechanics, then the settlement strategy's categorization and fee sizing
/// (ADR-098 D2/D3).
///
/// The injected adapter owns Encode, Probe, and Assess. The engine owns the
/// journaled sequence, the measured gas, the return data, the trace, the swap
/// capture, and the access list. This strategy owns the categorical policy
/// around them: the int128 guard, the bucket taxonomy, the gas floor, and the
/// market-aware priority fee (ADR-098 D7 puts gas profitability and operator
/// margins off-chain, above the seam).
#[expect(clippy::too_many_lines)] // the production seam's body
fn simulate_path_on_evm_seam(
    session: &mut degenbot_simulation::sim::evm2::session::Evm2SimSession<'_, '_>,
    ctx: &SimulateContext<'_>,
    path: &SimulatePath,
    prepared: Option<&crate::selection::PreparedEncoding>,
    fail_buckets: &mut FailBuckets,
    span: &tracing::Span,
    trace_reads: &mut Option<AccessList>,
) -> ProviderResult<Option<SimResult>> {
    let _enter = span.enter();
    let mut guard = SimSpanVerdict {
        span: span.clone(),
        error_reason: None,
        verdict: SimVerdict::NotProfitable,
        started: std::time::Instant::now(),
        profit: None,
    };
    // C3 — int128 check (verbatim the revm seam's guard).
    if path.steps.len() != path.path_info.hops.len() {
        return Ok(None);
    }
    for (i, hop) in path.path_info.hops.iter().enumerate() {
        if let HopInfo::V4(_) = hop {
            let amount_specified = if i == 0 {
                path.optimal_input
            } else {
                path.steps[i].consumed_input
            };
            let output_amount = path.steps[i].output;
            if !fits_int128(amount_specified) || !fits_int128(output_amount) {
                fail_buckets.record(
                    path.candidate_id,
                    "int128-overflow",
                    None,
                    alloy::primitives::Bytes::new(),
                    path.optimal_input,
                    path.expected_outputs_vec(),
                );
                return Ok(None);
            }
        }
    }

    // The boundary-bridge observation probe: a V4↔V2 native/WETH adjacency the
    // 3-hop composers do not encode. A deployment that declares no WETH identity
    // (a foreign contract) has no such adjacency to report.
    if ::degenbot_config::holder::config().aave.bridge_probe {
        if let Some(settlement) = ctx.deployment.settlement() {
            if let Some(desc) =
                scan_for_v4_v2_boundary_bridge(&path.path_info.hops, settlement.weth_address)
            {
                op_info!(domain = sim, path_id = path.candidate_id.path_id(),
                    desc = %desc,
                    "V4 native to V2 WETH boundary; 3-hop composer does not encode this"
                );
            }
        }
    }

    // The deployment's Encode part: ONE complete transaction for this
    // opportunity (ADR-098 D2). A target the selection walk already composed
    // re-enters its recorded decision, so the adapter's callback runs once per
    // examined target, never once for selection and again here.
    let encoding = match prepared {
        Some(PreparedEncoding::Accepted(transaction)) => Ok(transaction.clone()),
        Some(PreparedEncoding::Declined(detail)) => Err(ComposeError::Unsupported(detail.clone())),
        Some(PreparedEncoding::Failed(detail)) => Err(ComposeError::Encode(detail.clone())),
        None => compose_transaction(ctx.deployment, path),
    };
    let transaction = match encoding {
        Ok(transaction) => transaction,
        Err(ComposeError::Unsupported(_decline)) => {
            // A routine encode decline is a bucket, never an exception.
            fail_buckets.record(
                path.candidate_id,
                "encode-failed",
                None,
                alloy::primitives::Bytes::new(),
                path.optimal_input,
                path.expected_outputs_vec(),
            );
            return Ok(None);
        }
        Err(ComposeError::Encode(detail)) => {
            // A primitive encode failure (e.g. an out-of-range config axis) is
            // unrecoverable for this path: counted as an exception, never folded
            // into a routine skip.
            guard.verdict = SimVerdict::Error;
            guard.error_reason = Some(degenbot_bot::telemetry::error_reason::SIM_PRE_ENCODE);
            return Err(ProviderError::RpcError {
                code: -32603,
                message: detail,
            });
        }
        Err(ComposeError::Validation(violation)) => {
            // The adapter's own calldata contradicts the interface it declared
            // (ADR-100 D5): broken adapter code, so the same hard pre-encode
            // error as an unrecoverable encode failure — never the routine
            // `encode-failed` skip, and never a submission row.
            guard.verdict = SimVerdict::Error;
            guard.error_reason = Some(degenbot_bot::telemetry::error_reason::SIM_PRE_ENCODE);
            return Err(ProviderError::RpcError {
                code: -32603,
                message: violation.to_string(),
            });
        }
    };

    // The deployment's declared target is the address its adapter must encode
    // to. The engine executes the transaction's OWN `to`, so an adapter that
    // encodes elsewhere would leave the submit join announcing a target whose
    // code never ran — a per-candidate refusal, not a silent re-target.
    if transaction.to != ctx.deployment.target() {
        fail_buckets.record(
            path.candidate_id,
            "target-mismatch",
            None,
            alloy::primitives::Bytes::new(),
            path.optimal_input,
            path.expected_outputs_vec(),
        );
        return Ok(None);
    }

    // The native value rides the encoded transaction to submission.
    let value = transaction.value;

    // The engine's mechanics: the adapter's declared probes bracket exactly that
    // transaction inside one journaled sequence, and the adapter assesses the
    // observed evidence.
    let sim_ctx = ProductionSimContext {
        caller: ctx.deployment.owner(),
        gas_limit: ctx.deployment.gas_limit(),
    };
    let (observation, capture, assessment) =
        match simulate_transaction(session, ctx.deployment.adapter(), transaction, &sim_ctx) {
            Ok(simulation) => (
                simulation.observation,
                simulation.capture,
                Some(simulation.result),
            ),
            // An assessment refused for want of evidence still hands the engine's
            // observation back: the failure is categorised against the same facts.
            Err(ProductionSimError::Assess {
                observation,
                capture,
                ..
            }) => (*observation, *capture, None),
            Err(ProductionSimError::Engine(err)) => {
                fail_buckets.record(
                    path.candidate_id,
                    "rpc-failed",
                    err.step_index(),
                    alloy::primitives::Bytes::new(),
                    path.optimal_input,
                    path.expected_outputs_vec(),
                );
                return Ok(None);
            }
            Err(err) => {
                guard.verdict = SimVerdict::Error;
                return Err(ProviderError::RpcError {
                    code: -32603,
                    message: err.to_string(),
                });
            }
        };

    // The forensic fan-out trace's read set: captured here, before any
    // outcome branch, so reverts and refusals carry their read walk too.
    *trace_reads = Some(capture.access_list.clone());

    // The step taxonomy the failure record reports on: the adapter's declared
    // reads in declared order, the opportunity's transaction at the probe count,
    // and the first failing step carrying the revert/no-op attribution.
    let probes = ctx.deployment.adapter().probe_spec();
    if let Some(fail_idx) = capture.failing_step() {
        record_failed_step(path, fail_buckets, probes, &observation, &capture, fail_idx);
        return Ok(None);
    }

    // An assessment that needed evidence the transaction never produced —
    // a declared probe read with no figure, or an `execute` receipt the call
    // did not return (ADR-098 D7). The record names a failing read when the
    // adapter declared one; otherwise the bucket alone carries the verdict.
    let Some(assessment) = assessment else {
        fail_buckets.record(
            path.candidate_id,
            "balance-decode",
            absent_probe_reading(&observation),
            alloy::primitives::Bytes::new(),
            path.optimal_input,
            path.expected_outputs_vec(),
        );
        return Ok(None);
    };

    let gross_profit = assessment.gross_profit;
    if gross_profit.is_zero() {
        record_no_profit(path, fail_buckets, &capture);
        return Ok(None);
    }

    // The deployment's fee sizing, applied ONCE: an adapter-owned deployment
    // returns its own Assess result verbatim (Fee belongs to the adapter); one
    // that opted into market-aware sizing is re-priced with the operator's
    // market-aware priority fee and the dispatch gas floor.
    let priced = match ctx.deployment.fee_sizing() {
        FeeSizing::AdapterOwned => assessment,
        FeeSizing::MarketAware => price_gross(
            gross_profit,
            &assessment.deltas,
            &observation,
            ctx,
            path.solve_block,
        ),
    };

    guard.verdict = SimVerdict::Profitable;
    let profit_i128 = i128::try_from(priced.net_profit).unwrap_or(i128::from(i64::MAX));
    guard.profit = Some(i64::try_from(profit_i128).unwrap_or(i64::MAX));
    Ok(Some(SimResult {
        candidate_id: path.candidate_id,
        gross_profit: priced.gross_profit,
        net_profit: priced.net_profit,
        gas_used: priced.gas_used,
        priority_fee: priced.priority_fee,
        base_fee_next: priced.base_fee_next,
        passed: priced.passed,
        execute_calldata: observation.transaction.data.clone(),
        value,
        access_list: Some(capture.access_list),
        captured_swaps: capture.swaps,
        hop_count: path.hop_count(),
    }))
}

/// Price a gross settlement figure with the operator's fee sizing: the
/// market-aware priority fee from the observed gross and gas, then the shared
/// [`ExecutionResult::price`] pricing half with the dispatch gas floor.
///
/// Only a deployment that declared [`FeeSizing::MarketAware`] reaches here — the
/// built-in adapter's Assess reports the gross evidence and the off-chain
/// gas/priority policy lands once, above the seam (ADR-098 D7). An
/// [`FeeSizing::AdapterOwned`] deployment is returned from its own
/// `ExecutionResult` and never re-priced: Fee is part of the adapter's Assess.
fn price_gross(
    gross_profit: U256,
    deltas: &[i128],
    observation: &ExecutionObservation,
    ctx: &SimulateContext<'_>,
    solve_block: u64,
) -> ExecutionResult {
    let priority_fee = compute_priority_fee(
        gross_profit,
        observation.gas_used,
        ctx.block.base_fee_next,
        solve_block,
        ctx.block.current_block,
        ctx.block.block_priority_fees.as_ref(),
    );
    let fee_policy = FeePolicy {
        market_percentile: false,
        override_priority_fee: priority_fee,
    };
    ExecutionResult::price(
        gross_profit,
        deltas.to_vec(),
        observation,
        U256::from(crate::dispatch::MIN_PROFIT_NET),
        &fee_policy,
    )
}

/// The first declared probe whose figure is absent, as a sequence position: the
/// pre-edge reads first (`0..n`), then the post-edge reads (`n + 1..`), with the
/// opportunity's own transaction at `n`.
fn absent_probe_reading(observation: &ExecutionObservation) -> Option<usize> {
    let declared = observation.probe_reads.len();
    observation
        .probe_reads
        .iter()
        .position(|reading| reading.before.is_none())
        .or_else(|| {
            observation
                .probe_reads
                .iter()
                .position(|reading| reading.after.is_none())
                .map(|index| declared + 1 + index)
        })
}

/// The `(target, selector)` of the step at `index` — the identity a synthesized
/// revert frame carries when the capture collected no call trace.
fn step_identity(
    probes: &[ProbeSpec],
    observation: &ExecutionObservation,
    index: usize,
) -> (Address, [u8; 4]) {
    let declared = |spec: &ProbeSpec| -> (Address, [u8; 4]) {
        match spec {
            ProbeSpec::Call { to, data, .. } => (*to, selector_of(data)),
            // A native-balance read runs no code and cannot revert, so it never
            // reaches this attribution.
            ProbeSpec::NativeBalance { account, .. } => (*account, [0u8; 4]),
        }
    };
    if index < probes.len() {
        return declared(&probes[index]);
    }
    if index == probes.len() {
        return (
            observation.transaction.to,
            selector_of(&observation.transaction.data),
        );
    }
    probes
        .get(index - probes.len() - 1)
        .map_or((observation.transaction.to, [0u8; 4]), declared)
}

/// The first four calldata bytes, zero-padded when the call carries fewer.
fn selector_of(data: &alloy::primitives::Bytes) -> [u8; 4] {
    let mut selector = [0u8; 4];
    let len = data.len().min(4);
    selector[..len].copy_from_slice(&data[..len]);
    selector
}

/// Record the first failing step of the sequence — the bucket/revert taxonomy,
/// with the deepest captured frame when the capture collected a trace and the
/// failing step's own identity otherwise.
fn record_failed_step(
    path: &SimulatePath,
    fail_buckets: &mut FailBuckets,
    probes: &[ProbeSpec],
    observation: &ExecutionObservation,
    capture: &SimulationCapture,
    fail_idx: usize,
) {
    let Some(step) = capture.step(fail_idx) else {
        return;
    };
    let revert_data = step.output.clone();
    let bucket = degenbot_decoders::revert::classify_revert(&revert_data);

    log_reverted_swaps_vs_hop_outputs(
        path.candidate_id.path_id(),
        &capture.reverted_swaps,
        &path.path_info.hops,
        path.expected_outputs_vec().as_slice(),
    );

    if let Some(frame) = capture.trace.failing_frame() {
        diag_trace!(domain = sim, path_id = path.candidate_id.path_id(), first_fail_call = fail_idx,
            full_call_trace = %capture.trace.render_debug(),
            "[sim-trace]"
        );
        let frame_revert_data = match &frame.outcome {
            Some(degenbot_simulation::FrameOutcome::Revert { data, .. }) => data.clone(),
            _ => alloy::primitives::Bytes::new(),
        };
        let frame_label = degenbot_decoders::revert::classify_revert(&frame_revert_data);
        fail_buckets.record_revert(
            path.candidate_id,
            &frame_label.clone(),
            Some(fail_idx),
            RevertingFrame {
                depth: frame.depth,
                target: frame.target,
                selector: frame.selector,
                revert_data: frame_revert_data,
                label: frame_label,
                outcome_kind: match &frame.outcome {
                    Some(degenbot_simulation::FrameOutcome::Revert { .. }) => "revert",
                    Some(degenbot_simulation::FrameOutcome::Success { .. }) => "success-unexpected",
                    _ => "halt",
                },
                gas_used: frame.outcome.as_ref().map_or(0, |outcome| {
                    use degenbot_simulation::FrameOutcome;
                    match outcome {
                        FrameOutcome::Revert { gas_used, .. }
                        | FrameOutcome::Halt { gas_used, .. }
                        | FrameOutcome::Success { gas_used, .. } => *gas_used,
                    }
                }),
            },
            capture.swaps.clone(),
            path.optimal_input,
            path.expected_outputs_vec(),
        );
        return;
    }

    // Seam-mode attribution (no call trace): synthesize the frame from the failing
    // STEP's own identity — a declared read or the opportunity's transaction.
    let (target, selector) = step_identity(probes, observation, fail_idx);
    fail_buckets.record_revert(
        path.candidate_id,
        &bucket.clone(),
        Some(fail_idx),
        RevertingFrame {
            depth: 1,
            target,
            selector,
            revert_data: revert_data.clone(),
            label: bucket,
            outcome_kind: match step.verdict {
                degenbot_simulation::sim::RunVerdict::Reverted => "revert",
                degenbot_simulation::sim::RunVerdict::Success => "success-unexpected",
                degenbot_simulation::sim::RunVerdict::Halted => "halt",
            },
            gas_used: step.gas_used,
        },
        capture.swaps.clone(),
        path.optimal_input,
        path.expected_outputs_vec(),
    );
}

/// Record a zero-gross success: the shape named by what the capture saw (a path
/// that traded, a frame that reverted its swaps, or a no-op encode).
fn record_no_profit(
    path: &SimulatePath,
    fail_buckets: &mut FailBuckets,
    capture: &SimulationCapture,
) {
    use degenbot_simulation::FrameOutcome as FO;

    let call_trace: Vec<String> = capture
        .trace
        .frames
        .iter()
        .map(|frame| {
            let kind = match &frame.outcome {
                Some(FO::Revert { .. }) => "revert",
                Some(FO::Success { .. }) => "ok",
                _ => "halt",
            };
            let gas = frame.outcome.as_ref().map_or(0, |outcome| match outcome {
                FO::Revert { gas_used, .. }
                | FO::Halt { gas_used, .. }
                | FO::Success { gas_used, .. } => *gas_used,
            });
            format!(
                "d{}:{:#x}:0x{}:{}=g{}",
                frame.depth,
                frame.target,
                alloy::primitives::hex::encode(frame.selector),
                kind,
                gas
            )
        })
        .collect();

    fail_buckets.record_no_profit(
        path.candidate_id,
        capture.swaps.clone(),
        capture.reverted_swaps.clone(),
        capture.log_count,
        path.optimal_input,
        path.expected_outputs_vec(),
        call_trace,
    );
}

// ── Revert-tolerant swap diagnostic ───────────────────
//
// The committed `captured_swaps` is empty for a reverting V4 `unlock`
// (the EVM pops the inner swaps from the journal), but the inspector preserves
// them in `reverted_swaps`. Comparing each reverted swap's ACTUAL output to
// `hop_outputs[i]` pinpoints which hop diverged — the decisive
// state-divergence-vs-composer-bug test for `CurrencyNotSettled`. Conservative
// default ON (a single atomic load per failed path); set `=0` to disable.

/// The `[sim-revert-swap]` log prefix — verbatim so log greps return here.
const SIM_REVERT_SWAP_LOG_PREFIX: &str = "[sim-revert-swap]";

/// The positive (output) side of a captured swap's signed amounts — the
/// amount the swapper RECEIVED (the hop's output). `None` if neither side
/// carries a non-zero magnitude (a degenerate zero-zero swap, shouldn't occur).
///
/// # Sign conventions differ per family
///
/// The V2/V3/V4 `Swap` event `amount0`/`amount1` signed deltas use DIFFERENT
/// perspectives, so the output is on a different sign side per family:
/// - **V2**: the [`from_log`](CapturedSwap::from_log) decoder maps to the
///   `out - in` convention (positive = received by swapper) → output is the
///   POSITIVE side.
/// - **V4**: PoolManager amounts are the caller's delta (positive = received
///   by swapper) → output is the POSITIVE side (verified: the V4 hop's
///   positive side matches `hop_outputs[0]` on mainnet).
/// - **V3**: the UniswapV3 `Swap` event amounts are the POOL's balance delta
///   (positive = pool received = swapper paid INPUT; negative = pool paid =
///   swapper received OUTPUT) → output is the NEGATIVE side. This was the
///   subtle bug: picking the positive side for V3 returned the INPUT, not the
///   output (the V3 hop's positive side = `hop_outputs[i-1]`, the prior hop's
///   output = this hop's input).
fn captured_swap_output_amount(swap: &CapturedSwap) -> Option<U256> {
    use degenbot_simulation::SwapFamily;
    // For V3, the output is the NEGATIVE side (pool paid out); for V2/V4, the
    // POSITIVE side (swapper received). Pick the side whose sign matches the
    // family's "received by swapper" convention.
    match swap.family {
        SwapFamily::V3 => negative_side_magnitude(swap.amount0, swap.amount1),
        SwapFamily::V2 | SwapFamily::V4 => positive_side_magnitude(swap.amount0, swap.amount1),
    }
}

/// The magnitude of the POSITIVE side (`out - in` / swapper-received).
#[must_use]
fn positive_side_magnitude(
    a0: alloy::primitives::I256,
    a1: alloy::primitives::I256,
) -> Option<U256> {
    if a0.is_positive() {
        Some(U256::try_from(a0.abs()).unwrap_or(U256::ZERO))
    } else if a1.is_positive() {
        Some(U256::try_from(a1.abs()).unwrap_or(U256::ZERO))
    } else {
        None
    }
}

/// The magnitude of the NEGATIVE side (pool-paid-out = swapper-received for
/// V3's pool-perspective deltas).
#[must_use]
fn negative_side_magnitude(
    a0: alloy::primitives::I256,
    a1: alloy::primitives::I256,
) -> Option<U256> {
    if a0.is_negative() {
        Some(U256::try_from(a0.abs()).unwrap_or(U256::ZERO))
    } else if a1.is_negative() {
        Some(U256::try_from(a1.abs()).unwrap_or(U256::ZERO))
    } else {
        None
    }
}

/// For a reverted path, log each reverted-frame swap's ACTUAL output vs the
/// solver's predicted `hop_outputs[i]` (by emission order). A mismatch on
/// hop 0 (the V4 swap) means the V4 swap diverged (solver calc or engine
/// state); a match on hop 0 but mismatch on hop 1 means V3 hop B diverged;
/// all-match-but-still-`CurrencyNotSettled` points at the composer/encoding.
/// Conservative default ON (`the reverted-swap diagnostic`); `=0` to disable.
/// One captured reverted swap attributed to its path hop. Built by
/// [`match_reverted_swaps_to_hops`] so the log layer formats instead of
/// re-deriving (and so the attribution is unit-tested independently of the
/// env-gated log path).
#[derive(Clone, Debug, PartialEq, Eq)]
struct RevertedSwapMatch {
    /// Path hop index this captured swap was attributed to. `None` when the
    /// swap's emitter matches no hop on the path (a stray emit from a pool the
    /// solver did not register — should not occur, handled defensively).
    hop_index: Option<usize>,
    /// Emission order of the captured swap (0-based), preserved for the log so
    /// a reader can reconstruct the EVM's execution order independently of the
    /// hop attribution.
    emit_index: usize,
    family: degenbot_simulation::SwapFamily,
    emitter: Address,
    /// V4 PoolManager has no address-level identity (its emitter is shared by
    /// every V4 hop), so carry the matched hop's pool_id — or the V2/V3 pool
    /// address — letting a log reader resolve the exact pool. `None` when the
    /// swap matched no hop on the path.
    pool_id: Option<String>,
    /// Post-swap active-liquidity scalars captured at SIM time (the moment
    /// `actual_out` was produced), as the shared slot0 head-scalar fact type
    /// (`degenbot_pools::Slot0HeadScalars` — the same family
    /// the solve-time tripwire's `SolverHopScalarState` pins; ADR-021 D3).
    /// It pins the exact pool state the oracle (`v4_simulate_swap`) must be
    /// driven at to reproduce a recurrence, closing the solve-vs-sim state gap
    /// the log's solve-time values cannot close.
    sim_scalars: degenbot_pools::Slot0HeadScalars,
    /// The exact-in amount the swap consumed (the negative-side delta
    /// magnitude) — the `amountSpecified` that produced `actual_out`.
    actual_in: Option<U256>,
    actual_out: Option<U256>,
    /// The FULL signed captured-swap deltas (from the pool's perspective): for
    /// V3, `amount0` is the positive input side and `amount1` the negative
    /// output side (or vice-versa by zfo). The V3 `actual_in`/`actual_out`
    /// collapse BOTH to the negative-side magnitude (both are
    /// `negative_side_magnitude`), so the input amount is otherwise
    /// unrecoverable from the log — logging these removes the ambiguity for
    /// reconstruction (the solve-vs-sim state gap).
    captured_amount0: I256,
    captured_amount1: I256,
    predicted: Option<u128>,
    matched: bool,
}

/// Attribute each captured reverted swap to its path hop by the swap's
/// EMITTER, not by capture index. Capture (emission) order is the EVM's
/// execution order: the deepest nested swap emits first, the outermost
/// frame emits last. For a `V3-V4-V3` whose V3c (outer) frame reverts, the
/// inspector captures `[V4_emit, V3a_emit]` — the V4 swap (hop 1) emits before the V3a
/// swap (hop 0). Index-based attribution crosses them, so the V4 swap is
/// compared to `hop_outputs[0]` (the V3a prediction) and vice versa, making
/// the actual-vs-predicted diagnostic lie about WHICH hop diverged.
///
/// Emitter attribution fixes this for the common case: each hop emits from a
/// distinct pool address (V2/V3) or the single PoolManager (V4). A path with
/// TWO V4 hops shares the PoolManager emitter for both — those are
/// disambiguated by stable order within the V4 subset (first-emitted V4 swap
/// → first V4 hop), documented as a residual imprecision.
fn match_reverted_swaps_to_hops(
    reverted_swaps: &[CapturedSwap],
    hops: &[HopInfo],
    hop_outputs: &[u128],
) -> Vec<RevertedSwapMatch> {
    // Claimed hop indices (a hop matches at most one captured swap).
    let mut claimed: Vec<bool> = vec![false; hops.len()];
    let mut matches = Vec::with_capacity(reverted_swaps.len());
    for (emit_index, swap) in reverted_swaps.iter().enumerate() {
        // Lowest-index unclaimed hop whose emitter matches this swap's.
        let mut hop_index = None;
        for (i, h) in hops.iter().enumerate() {
            if !claimed[i] && hop_emitter_match(h, swap) {
                claimed[i] = true;
                hop_index = Some(i);
                break;
            }
        }
        let actual = captured_swap_output_amount(swap);
        let predicted = hop_index.and_then(|i| hop_outputs.get(i).copied());
        let is_match = match (actual, predicted) {
            (Some(a), Some(p)) => a == U256::from(p),
            _ => false,
        };
        matches.push(RevertedSwapMatch {
            hop_index,
            emit_index,
            family: swap.family,
            emitter: swap.emitter,
            pool_id: hop_index.and_then(|i| hop_pool_id(hops, i)),
            sim_scalars: degenbot_pools::Slot0HeadScalars {
                sqrt_price_x96: swap.sqrt_price_x96,
                tick: swap.tick,
                liquidity: swap.liquidity,
            },
            actual_in: negative_side_magnitude(swap.amount0, swap.amount1),
            actual_out: actual,
            captured_amount0: swap.amount0,
            captured_amount1: swap.amount1,
            predicted,
            matched: is_match,
        });
    }
    matches
}

/// `true` iff `hop`'s emitter address equals the captured swap's emitter.
/// Separate from [`hop_emitter`] so the matcher allocates no `Address` copies
/// in the hot inner loop (the comparison is by value on `hop_*_address`).
fn hop_emitter_match(hop: &HopInfo, swap: &CapturedSwap) -> bool {
    match hop {
        HopInfo::V2(h) => h.pool_address == swap.emitter,
        HopInfo::V3(h) => h.pool_address == swap.emitter,
        HopInfo::V4(h) => h.pool_manager_address == swap.emitter,
    }
}

/// A hop's resolvable pool identifier for the log: the V4 `pool_id_hex`
/// (identity is the pool hash, since every V4 hop shares the PoolManager
/// emitter) or the V2/V3 pool address. `None` if `i` is out of range.
fn hop_pool_id(hops: &[HopInfo], i: usize) -> Option<String> {
    match hops.get(i)? {
        HopInfo::V2(h) => Some(h.pool_address.to_string()),
        HopInfo::V3(h) => Some(h.pool_address.to_string()),
        HopInfo::V4(h) => Some(h.pool_id_hex.clone()),
    }
}

fn log_reverted_swaps_vs_hop_outputs(
    path_id: u64,
    reverted_swaps: &[CapturedSwap],
    hops: &[HopInfo],
    hop_outputs: &[u128],
) {
    if reverted_swaps.is_empty() {
        return;
    }
    let matches = match_reverted_swaps_to_hops(reverted_swaps, hops, hop_outputs);
    op_info!(
        domain = sim,
        path_id,
        reverted_swaps = reverted_swaps.len(),
        hop_outputs = hop_outputs.len(),
        "{SIM_REVERT_SWAP_LOG_PREFIX}"
    );
    for m in &matches {
        let hop_str = m
            .hop_index
            .map_or_else(|| "unmatched".into(), |i| i.to_string());
        diag!(domain = sim, path_id,
            hop = %hop_str,
            emit = m.emit_index,
            family = ?m.family,
            emitter = ?m.emitter,
            pool_id = %m.pool_id.as_deref().unwrap_or("unmatched"),
            actual_in = %m.actual_in.map_or_else(|| "none".into(), |v| v.to_string()),
            actual_out = %m.actual_out.map_or_else(|| "none".into(), |v| v.to_string()),
            captured_amount0 = %m.captured_amount0,
            captured_amount1 = %m.captured_amount1,
            predicted = %m.predicted.map_or_else(|| "none".into(), |v| v.to_string()),
            sim_sqrt_price_x96 = %m.sim_scalars.sqrt_price_x96,
            sim_tick = m.sim_scalars.tick,
            sim_liquidity = %m.sim_scalars.liquidity,
            matched = m.matched,
            "{SIM_REVERT_SWAP_LOG_PREFIX}"
        );
    }
}

#[expect(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {

    // Test-only span capture (same process-global seam
    // as the degenbot-submission span tests - one global per test binary;
    // the crate has no other global subscriber installs).
    mod span_capture {
        use std::collections::{BTreeMap, HashMap};
        use std::sync::{Arc, Mutex, OnceLock};
        use tracing::field::Visit;
        use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

        type SpanEntry = (String, BTreeMap<String, String>);
        #[derive(Default)]

        pub struct SpanCapture {
            spans: Mutex<HashMap<tracing::span::Id, SpanEntry>>,
        }

        impl SpanCapture {
            /// All captured spans as (name, recorded fields).
            pub fn snapshot(&self) -> Vec<(String, BTreeMap<String, String>)> {
                let Ok(g) = self.spans.lock() else {
                    return Vec::new();
                };
                g.values()
                    .map(|entry| (entry.0.clone(), entry.1.clone()))
                    .collect()
            }
        }

        pub struct CaptureLayer(pub Arc<SpanCapture>);

        struct FieldPusher<'a>(&'a mut BTreeMap<String, String>);

        impl Visit for FieldPusher<'_> {
            fn record_debug(&mut self, key: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0.insert(key.name().to_string(), format!("{value:?}"));
            }
            fn record_str(&mut self, key: &tracing::field::Field, value: &str) {
                self.0.insert(key.name().to_string(), value.to_string());
            }
            fn record_i64(&mut self, key: &tracing::field::Field, value: i64) {
                self.0.insert(key.name().to_string(), value.to_string());
            }
            fn record_u64(&mut self, key: &tracing::field::Field, value: u64) {
                self.0.insert(key.name().to_string(), value.to_string());
            }
            fn record_bool(&mut self, key: &tracing::field::Field, value: bool) {
                self.0.insert(key.name().to_string(), value.to_string());
            }
        }

        impl<S> Layer<S> for CaptureLayer
        where
            S: tracing::Subscriber,
        {
            fn on_new_span(
                &self,
                attrs: &tracing::span::Attributes<'_>,
                id: &tracing::span::Id,
                _ctx: Context<'_, S>,
            ) {
                let name = attrs.metadata().name().to_string();
                let Ok(mut g) = self.0.spans.lock() else {
                    return;
                };
                let entry = g
                    .entry(id.clone())
                    .or_insert_with(|| (name, BTreeMap::new()));
                attrs.values().record(&mut FieldPusher(&mut entry.1));
            }

            fn on_record(
                &self,
                id: &tracing::span::Id,
                values: &tracing::span::Record<'_>,
                _ctx: Context<'_, S>,
            ) {
                let Ok(mut g) = self.0.spans.lock() else {
                    return;
                };
                if let Some(entry) = g.get_mut(id) {
                    values.record(&mut FieldPusher(&mut entry.1));
                }
            }
        }

        /// The shared capture, installed as the process-global subscriber on
        /// first use (once per test binary; a second install is a no-op and
        /// the shared `Arc` keeps capturing regardless).
        #[must_use]
        pub fn global() -> Arc<SpanCapture> {
            static CAP: OnceLock<Arc<SpanCapture>> = OnceLock::new();
            let cap = CAP.get_or_init(|| Arc::new(SpanCapture::default())).clone();
            let _ = tracing::subscriber::set_global_default(
                tracing_subscriber::registry().with(CaptureLayer(cap.clone())),
            );
            cap
        }
    }
    use super::*;
    use alloy::primitives::U256;
    use degenbot_execution::V3HopInfo;

    // ── C0: execute_gas_limit config override (artificial-ceiling
    // investigation; the string contract lives in degenbot-config's
    // loader tests — here the value is a typed Option<u64>) ──

    #[test]
    fn execute_gas_limit_falls_back_to_eip7825_default() {
        assert_eq!(super::execute_gas_limit(), INITIAL_EXECUTE_GAS);
        assert_eq!(INITIAL_EXECUTE_GAS, 16_777_216);
    }

    // ── C3: fits_int128 ──────────────────────────────────────────────────

    #[test]
    fn fits_int128_matches_python_bounds() {
        assert!(fits_int128(0));
        assert!(fits_int128(1));
        // INT128_MAX = (1<<127) - 1 fits; (1<<127) doesn't.
        assert!(fits_int128(INT128_MAX as u128));
        assert!(!fits_int128((INT128_MAX as u128) + 1));
        assert!(!fits_int128(u128::MAX));
    }

    // ── C4: compute_priority_fee — parity gate vs _compute_priority_fee ──

    #[test]
    fn priority_fee_zero_gas_returns_one() {
        let fees = BlockPriorityFees {
            block: 100,
            p10: U256::ZERO,
            p50: U256::ZERO,
        };
        assert_eq!(
            compute_priority_fee(U256::from(1_000u128), 0, 1, 100, 100, Some(&fees)),
            1
        );
    }

    #[test]
    fn priority_fee_target_truncates_toward_zero() {
        // gross=1000 wei, gas=100, base=0, age=0.
        // target = max(1, int((1000/1.25 - 0)/100)) = int(800/100) = int(8.0) = 8.
        // age_factor = 1/(1+0) = 1.0. priority_fee = int(8*1.0) = 8.
        // bounds: p10=p50=0 → min=max(1,1)=1, max=max(1,1)=1.
        // clamp(8, 1, 1) = 1.
        let fees = BlockPriorityFees {
            block: 100,
            p10: U256::ZERO,
            p50: U256::ZERO,
        };
        assert_eq!(
            compute_priority_fee(U256::from(1_000u128), 100, 0, 100, 100, Some(&fees)),
            1
        );
    }

    #[test]
    fn priority_fee_uses_p10_plus_one_floor() {
        // gross=1e6, gas=100, base=0, age=0.
        // target = int(1e6/1.25/100) = int(8000.0) = 8000.
        // priority_fee = 8000 (age 0). bounds: p10=99, p50=199.
        //   min = max(99+1, 1) = 100. max = max(199+1, 100) = 200.
        // clamp(8000, 100, 200) = 200.
        let fees = BlockPriorityFees {
            block: 100,
            p10: U256::from(99u128),
            p50: U256::from(199u128),
        };
        assert_eq!(
            compute_priority_fee(U256::from(1_000_000u128), 100, 0, 100, 100, Some(&fees)),
            200
        );
    }

    #[test]
    fn priority_fee_applies_age_decay() {
        // gross=1e9, gas=1000, base=0, solve=100, current=104 → age=4.
        // target = int(1e9/1.25/1000) = int(800_000.0) = 800_000.
        // age_factor = 1/(1+0.25*4) = 1/2 = 0.5. priority_fee = int(400_000.0) = 400_000.
        // bounds: p10=0, p50=0 → min=1, max=max(1,1)=1. clamp(400_000, 1, 1) = 1.
        let fees = BlockPriorityFees {
            block: 100,
            p10: U256::ZERO,
            p50: U256::ZERO,
        };
        assert_eq!(
            compute_priority_fee(
                U256::from(1_000_000_000u128),
                1000,
                0,
                100,
                104,
                Some(&fees)
            ),
            1
        );
    }

    #[test]
    fn priority_fee_age_decay_without_market_clamp() {
        // Same as above but with a generous p50 so the age-decayed fee survives.
        // target=800_000, age_factor=0.5 → priority=400_000.
        // p10=0, p50=1_000_000 → min=1, max=1_000_001 ≥ 400_000.
        let fees = BlockPriorityFees {
            block: 100,
            p10: U256::ZERO,
            p50: U256::from(1_000_000u128),
        };
        assert_eq!(
            compute_priority_fee(
                U256::from(1_000_000_000u128),
                1000,
                0,
                100,
                104,
                Some(&fees)
            ),
            400_000
        );
    }

    #[test]
    fn priority_fee_no_market_uses_target_as_ceiling() {
        // No feeHistory → min=1, max=target. age=0 → priority=target.
        // gross=1e6, gas=100, base=0 → target=8000, priority=8000.
        // clamp(8000, 1, 8000) = 8000.
        assert_eq!(
            compute_priority_fee(U256::from(1_000_000u128), 100, 0, 100, 100, None),
            8000
        );
    }

    // ── simulate_in_process_with_db: the in-process smoke test ───────────

    use alloy::primitives::{address, Bytes};
    use alloy::providers::{Provider, ProviderBuilder};
    use alloy::rpc::client::ClientBuilder;
    use alloy::transports::mock::{Asserter, MockTransport};
    use degenbot_execution::{ExecutionAdapter, HopInfo, PathInfo, V2HopInfo};
    use degenbot_executor::composers::EncodeOptions;
    use degenbot_rpc::provider::AlloyProvider;
    use std::sync::Arc;

    use crate::cmd_executor::CmdExecutorBinding;
    use crate::deployment::{SimulationBlock, SimulationDeployment};
    use degenbot_execution::DeploymentId;

    /// The smoke fixtures' one deployment identity.
    const SMOKE_DEPLOYMENT_ID: u64 = 1;

    fn smoke_deployment_id() -> DeploymentId {
        DeploymentId::new(SMOKE_DEPLOYMENT_ID)
    }

    const SMOKE_OWNER: Address = address!("9c56a29c7231974c269e24f9fb3c29203039089e");
    const SMOKE_EXECUTOR: Address = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    const SMOKE_WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    const SMOKE_PM: Address = address!("000000000004444c5dc75cb358380d2e3de08a90");

    fn smoke_provider(asserter: &Asserter) -> AlloyProvider {
        let client = ClientBuilder::default().transport(MockTransport::new(asserter.clone()), true);
        let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
        AlloyProvider::from_provider(
            Arc::new(dyn_provider) as Arc<dyn alloy::providers::Provider<alloy::network::Ethereum>>
        )
    }

    /// The built-in `cmd_executor` binding the smoke fixtures inject.
    fn smoke_binding() -> CmdExecutorBinding {
        CmdExecutorBinding::new(
            SMOKE_OWNER,
            SMOKE_EXECUTOR,
            SMOKE_WETH,
            SMOKE_PM,
            EncodeOptions {
                erc6909_profit: false,
                use_v4_batch: false,
                ..EncodeOptions::default()
            },
        )
    }

    /// An executor stub that returns `(gross_profit, bribe_paid)` as the two
    /// ABI words `execute` returns — the receipt the built-in adapter assesses.
    fn receipt_stub(gross: U256, bribe: U256) -> Bytes {
        let mut code = Vec::with_capacity(76);
        code.push(0x7f); // PUSH32 gross_profit
        code.extend_from_slice(&gross.to_be_bytes::<32>());
        code.extend_from_slice(&[0x60, 0x00, 0x52]); // PUSH1 0x00 MSTORE
        code.push(0x7f); // PUSH32 bribe_paid
        code.extend_from_slice(&bribe.to_be_bytes::<32>());
        code.extend_from_slice(&[0x60, 0x20, 0x52]); // PUSH1 0x20 MSTORE
        code.extend_from_slice(&[0x60, 0x40, 0x60, 0x00, 0xf3]); // PUSH1 0x40 PUSH1 0x00 RETURN
        Bytes::from(code)
    }

    /// The built-in deployment with `runtime` installed at the executor address
    /// (the smoke fixtures' declared code override).
    #[expect(clippy::expect_used)] // the declared override names the executor
    fn smoke_deployment(runtime: Bytes) -> SimulationDeployment {
        smoke_binding()
            .injected(smoke_deployment_id(), SMOKE_EXECUTOR, runtime)
            .expect("the declared override names the executor")
    }

    fn smoke_ctx<'a>(
        provider: &'a AlloyProvider,
        deployment: &'a SimulationDeployment,
    ) -> SimulateContext<'a> {
        SimulateContext {
            block: SimulationBlock {
                provider,
                base_fee_next: 1_000_000_000u128,
                current_block: 100,
                block_timestamp: 0,
                block_priority_fees: Some(BlockPriorityFees {
                    block: 100,
                    p10: U256::from(500_000_000u64),
                    p50: U256::from(2_000_000_000u64),
                }),
            },
            deployment,
        }
    }

    fn smoke_v2_path(path_id: u64) -> SimulatePath {
        SimulatePath {
            candidate_id: CandidateId::new(path_id, smoke_deployment_id()),
            optimal_input: 1_000_000_000_000_000_000u128,
            steps: Box::new([
                SolveStep {
                    output: 1_100_000_000_000_000_000u128,
                    consumed_input: 1_100_000_000_000_000_000u128,
                    state_nonce: 0,
                },
                SolveStep {
                    output: 1_210_000_000_000_000_000u128,
                    consumed_input: 1_210_000_000_000_000_000u128,
                    state_nonce: 0,
                },
            ]),
            path_info: PathInfo::new(vec![
                HopInfo::V2(V2HopInfo {
                    pool_address: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
                    token0_address: SMOKE_WETH,
                    token1_address: address!("1111111111111111111111111111111111111111"),
                    fee: 30,
                    zfo: true,
                }),
                HopInfo::V2(V2HopInfo {
                    pool_address: address!("cccccccccccccccccccccccccccccccccccccccc"),
                    token0_address: address!("1111111111111111111111111111111111111111"),
                    token1_address: SMOKE_WETH,
                    fee: 30,
                    zfo: true,
                }),
            ]),
            solve_block: 100,
        }
    }

    /// The seam characterization gate: the smoke fixtures through the
    /// engine's production sim seam — fixed outcomes, bucket tallies, and
    /// deep revert attribution (ADR-089 cutover: the cross-engine parity
    /// form retired with the revm seam).
    #[test]
    fn simulate_in_process_revert_fixtures_characterize_the_seam() {
        for (path_id, bytecode, bucket, label, revert_data) in [
            // The INVALID-executor fixture: Halt, no data, "empty" bucket.
            (
                7u64,
                Bytes::from_static(&[0xfe]),
                "empty",
                "empty",
                &b""[..],
            ),
            // The REVERT-with-data fixture: the 4-byte selector.
            (
                42u64,
                Bytes::from_static(&[
                    0x63, 0xca, 0xfe, 0xba, 0xbe, 0x60, 0x00, 0x52, 0x60, 0x04, 0x60, 0x1c, 0xfd,
                ]),
                "unknown:0xcafebabe",
                "unknown:0xcafebabe",
                &[0xca, 0xfe, 0xba, 0xbe][..],
            ),
        ] {
            let asserter = Asserter::new();
            let provider = smoke_provider(&asserter);
            let deployment = smoke_deployment(bytecode.clone());
            let ctx = smoke_ctx(&provider, &deployment);
            let mut cache_db = ::evm2::evm::InMemoryDB::default();
            degenbot_simulation::sim::evm2::state_override::apply_simulation_overrides(
                &mut cache_db,
                &ctx.deployment.override_params(),
            );
            let mut buckets = FailBuckets::new();
            let result =
                simulate_in_process_with_db(&ctx, cache_db, &smoke_v2_path(path_id), &mut buckets)
                    .unwrap();

            assert!(result.is_none(), "reverting execute");
            assert_eq!(buckets.get(bucket), 1, "bucket tallies");

            let failures = buckets.failures();
            assert_eq!(failures.len(), 1);
            let failure = &failures[0];
            assert_eq!(failure.candidate_id.path_id(), path_id);
            assert_eq!(
                failure.fail_index,
                Some(0),
                "the execute() call is the sequence's only step"
            );
            let frame = failure.reverting_frame.as_ref().expect("deep attribution");
            assert_eq!(frame.depth, 1, "the top-level execute() frame");
            assert_eq!(frame.target, SMOKE_EXECUTOR, "the executor reverted");
            assert_eq!(frame.label, label);
            assert_eq!(frame.revert_data.as_ref(), revert_data);
        }
    }

    /// The fixture `CacheDB` for a declared-code build: the state overrides and
    /// nothing else (the receipt needs no auxiliary contract code).
    fn smoke_cache_db(ctx: &SimulateContext<'_>) -> ::evm2::evm::InMemoryDB {
        let mut db = ::evm2::evm::InMemoryDB::default();
        degenbot_simulation::sim::evm2::state_override::apply_simulation_overrides(
            &mut db,
            &ctx.deployment.override_params(),
        );
        db
    }

    /// The no-profit smoke: the executor returns a `(0, 0)` receipt → the
    /// assessment prices zero gross → the `execute-no-op` shape. Proves the
    /// success-path orchestration (the transaction, the assessment, the profit
    /// arithmetic, the tally) runs end-to-end, and that the no-op shape is
    /// named for what it is instead of posing as an economic `no-profit`.
    #[test]
    fn simulate_in_process_with_db_no_profit_path_smoke() {
        let asserter = Asserter::new();
        let provider = smoke_provider(&asserter);
        let deployment = smoke_deployment(receipt_stub(U256::ZERO, U256::ZERO));
        let ctx = smoke_ctx(&provider, &deployment);
        let cache_db = smoke_cache_db(&ctx);
        let mut buckets = FailBuckets::new();

        let path = smoke_v2_path(11);
        let result = simulate_in_process_with_db(&ctx, cache_db, &path, &mut buckets).unwrap();
        assert!(result.is_none(), "no-profit returns None");
        assert_eq!(
            buckets.get("execute-no-op"),
            1,
            "the empty swap set names the no-op shape"
        );
        assert_eq!(buckets.get("no-profit"), 0, "not an economic no-profit");
        let failures = buckets.failures();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].candidate_id.path_id(), 11);
        assert_eq!(failures[0].bucket, "execute-no-op");
        assert!(failures[0].fail_index.is_none(), "no call reverted");
        // Empty-bytecode executor emits no Swap events; record_no_profit
        // surfaces the (empty) captured swaps instead of hardcoding Vec::new()
        // like the plain record() — proves the field is plumbed end-to-end.
        assert!(
            failures[0].captured_swaps.is_empty(),
            "no swap events from the receipt stub"
        );
    }

    /// An `execute` that returns no receipt is unmeasured, never a
    /// zero-profit arb: the assessment refuses, and the seam records the
    /// evidence refusal instead of the economic no-profit shapes.
    #[test]
    fn an_unmeasured_execute_receipt_is_not_a_zero_profit() {
        let asserter = Asserter::new();
        let provider = smoke_provider(&asserter);
        let deployment = smoke_deployment(Bytes::new()); // returns no data at all
        let ctx = smoke_ctx(&provider, &deployment);
        let cache_db = smoke_cache_db(&ctx);
        let mut buckets = FailBuckets::new();

        let result =
            simulate_in_process_with_db(&ctx, cache_db, &smoke_v2_path(12), &mut buckets).unwrap();
        assert!(result.is_none(), "no receipt, no verdict");
        assert_eq!(buckets.get("balance-decode"), 1, "the refusal is recorded");
        assert_eq!(buckets.get("no-profit"), 0, "not an economic no-profit");
        assert_eq!(buckets.get("execute-no-op"), 0, "not a measured zero");
        let failures = buckets.failures();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].candidate_id.path_id(), 12);
        assert!(failures[0].fail_index.is_none(), "no read or call to name");
    }

    /// The shape discriminator: a zero-gross outcome is named by what the
    /// capture saw, so a no-profit that really swapped never reads as
    /// "execute() did nothing" and a no-op never poses as an economic verdict.
    #[test]
    fn record_no_profit_names_the_shape_from_the_captured_swap_set() {
        use degenbot_simulation::SwapFamily;

        fn swap() -> CapturedSwap {
            CapturedSwap {
                emitter: Address::repeat_byte(0x42),
                family: SwapFamily::V3,
                amount0: alloy::primitives::I256::try_from(-1_000_i128).unwrap(),
                amount1: alloy::primitives::I256::try_from(990_i128).unwrap(),
                sqrt_price_x96: U256::ZERO,
                liquidity: U256::ZERO,
                tick: 0,
            }
        }

        // (captured, reverted) → (bucket, shape)
        for (captured, reverted, bucket, shape) in [
            (
                vec![swap()],
                Vec::new(),
                "no-profit",
                super::NoProfitShape::SwapsEmitted,
            ),
            (
                Vec::new(),
                vec![swap()],
                "no-profit-reverted",
                super::NoProfitShape::SwapsReverted,
            ),
            (
                Vec::new(),
                Vec::new(),
                "execute-no-op",
                super::NoProfitShape::ExecuteDidNothing,
            ),
        ] {
            let mut buckets = FailBuckets::new();
            buckets.record_no_profit(
                CandidateId::new(3, smoke_deployment_id()),
                captured,
                reverted,
                0,
                1_000,
                vec![990],
                Vec::new(),
            );
            assert_eq!(buckets.get(bucket), 1, "tallied under its own shape");
            let failures = buckets.failures();
            assert_eq!(failures[0].bucket, bucket);
            assert_eq!(shape.bucket(), bucket);
            // The request's figures and the captured set ride the record —
            // the dump is evidence, not an artifact.
            assert_eq!(failures[0].optimal_input, 1_000);
            assert_eq!(failures[0].hop_outputs, vec![990]);
        }
    }

    /// The swap set is the discriminator, whichever way the sets come in: a
    /// committed swap wins over a reverted one (a path that traded is an
    /// economic verdict even if a later frame reverted).
    #[test]
    fn no_profit_shape_prefers_committed_swaps() {
        use degenbot_simulation::SwapFamily;

        let one = CapturedSwap {
            emitter: Address::repeat_byte(0x42),
            family: SwapFamily::V2,
            amount0: alloy::primitives::I256::try_from(-5_i128).unwrap(),
            amount1: alloy::primitives::I256::try_from(4_i128).unwrap(),
            sqrt_price_x96: U256::ZERO,
            liquidity: U256::ZERO,
            tick: 0,
        };
        assert_eq!(
            super::NoProfitShape::of(std::slice::from_ref(&one), std::slice::from_ref(&one)),
            super::NoProfitShape::SwapsEmitted
        );
    }

    // ── scan_for_v4_v2_boundary_bridge ─────────────────────────
    #[test]
    fn scan_detects_v4_native_to_v2_weth_boundary() {
        use degenbot_execution::{HopInfo, V2HopInfo, V4HopInfo};
        let native = Address::ZERO;
        let weth = Address::from([0xc0u8; 20]);
        let usdc = Address::from([0xa0u8; 20]);
        // V4a: USDC→native (zfo=false → in=c1=USDC, out=c0=native)
        let a = V4HopInfo {
            pool_manager_address: Address::ZERO,
            pool_id_hex: String::new(),
            currency0_address: native,
            currency1_address: usdc,
            fee: 0,
            tick_spacing: 0,
            hook_address: Address::ZERO,
            zfo: false,
        };
        // V2b: WETH→USDC (zfo=true → in=token0=WETH)
        let b = V2HopInfo {
            pool_address: Address::ZERO,
            token0_address: weth,
            token1_address: usdc,
            fee: 0,
            zfo: true,
        };
        let hops = vec![HopInfo::V4(a), HopInfo::V2(b)];
        let got = scan_for_v4_v2_boundary_bridge(&hops, weth);
        assert!(
            got.is_some(),
            "V4 native-out → V2 WETH-in must scan as a boundary bridge"
        );
        assert!(got.unwrap().contains("Wrap"), "Wrap direction signalled");
    }

    #[test]
    fn scan_detects_v2_weth_to_v4_native_boundary() {
        use degenbot_execution::{HopInfo, V2HopInfo, V4HopInfo};
        let native = Address::ZERO;
        let weth = Address::from([0xc0u8; 20]);
        let usdc = Address::from([0xa0u8; 20]);
        // V2a: USDC→WETH (zfo=false → in=token1=USDC, out=token0=WETH)
        let a = V2HopInfo {
            pool_address: Address::ZERO,
            token0_address: weth,
            token1_address: usdc,
            fee: 0,
            zfo: false,
        };
        // V4b: native→USDC (zfo=true → in=token0=native)
        let b = V4HopInfo {
            pool_manager_address: Address::ZERO,
            pool_id_hex: String::new(),
            currency0_address: native,
            currency1_address: usdc,
            fee: 0,
            tick_spacing: 0,
            hook_address: Address::ZERO,
            zfo: true,
        };
        let hops = vec![HopInfo::V2(a), HopInfo::V4(b)];
        let got = scan_for_v4_v2_boundary_bridge(&hops, weth);
        assert!(
            got.is_some(),
            "V2 WETH-out → V4 native-in must scan as a boundary bridge"
        );
        assert!(
            got.unwrap().contains("Unwrap"),
            "Unwrap direction signalled"
        );
    }

    #[test]
    fn scan_returns_none_for_native_path_ends_v4_v2_v4() {
        // V4a native-in, V4c native-out — native at PATH ENDS, boundary token
        // is WETH (ERC20) on the V2 ↔ V4 sides. No bridge needed.
        use degenbot_execution::{HopInfo, V2HopInfo, V4HopInfo};
        let native = Address::ZERO;
        let weth = Address::from([0xc0u8; 20]);
        let usdc = Address::from([0xa0u8; 20]);
        let wbtc = Address::from([0xbbu8; 20]);
        // V4a: native→USDC (zfo=true → in=c0=native, out=c1=USDC)
        let a = V4HopInfo {
            pool_manager_address: Address::ZERO,
            pool_id_hex: String::new(),
            currency0_address: native,
            currency1_address: usdc,
            fee: 0,
            tick_spacing: 0,
            hook_address: Address::ZERO,
            zfo: true,
        };
        // V2b: USDC→WBTC (zfo=true, boundary token USDC is ERC20)
        let b = V2HopInfo {
            pool_address: Address::ZERO,
            token0_address: usdc,
            token1_address: wbtc,
            fee: 0,
            zfo: true,
        };
        // V4c: WBTC→native (zfo=true → in=c0=WBTC, out=c1=native)
        let c = V4HopInfo {
            pool_manager_address: Address::ZERO,
            pool_id_hex: String::new(),
            currency0_address: wbtc,
            currency1_address: native,
            fee: 0,
            tick_spacing: 0,
            hook_address: Address::ZERO,
            zfo: true,
        };
        let hops = vec![HopInfo::V4(a), HopInfo::V2(b), HopInfo::V4(c)];
        assert!(
            scan_for_v4_v2_boundary_bridge(&hops, weth).is_none(),
            "native-path-ends with ERC20 boundary must NOT scan as boundary bridge"
        );
    }

    // ── captured_swap_output_amount sign conventions ───────
    //
    // The decisive divergence localizer: pick the swapper-RECEIVED side of
    // a captured swap's signed amounts so `actual_out` can be compared to
    // the solver's `hop_outputs[i]`. The sign side DIFFERS per family — V2/V4
    // are swapper-perspective (output = POSITIVE side), V3 is pool-perspective
    // (output = NEGATIVE side). Picking the wrong side for V3 returned the
    // INPUT, masking the V4-doesn't-match-V3-does signal. These tests pin the
    // convention per family so a future refactor can't silently flip it.

    /// Build a minimal `CapturedSwap` with given signed amounts (the only
    /// fields `captured_swap_output_amount` reads).
    fn swap_with_amounts(
        family: degenbot_simulation::SwapFamily,
        amount0: alloy::primitives::I256,
        amount1: alloy::primitives::I256,
    ) -> CapturedSwap {
        CapturedSwap {
            emitter: Address::ZERO,
            family,
            amount0,
            amount1,
            sqrt_price_x96: U256::ZERO,
            liquidity: U256::ZERO,
            tick: 0,
        }
    }

    #[test]
    fn v2_captured_output_is_the_positive_side_swapper_received() {
        // V2 `out - in` convention: amount0=-1e18 (paid in), amount1=+3000e6
        // (received). Output = the positive side (+3000e6).
        let swap = swap_with_amounts(
            degenbot_simulation::SwapFamily::V2,
            alloy::primitives::I256::try_from(-1_000_000_000_000_000_000_i128).unwrap(),
            alloy::primitives::I256::try_from(3_000_000_000_i128).unwrap(),
        );
        assert_eq!(
            captured_swap_output_amount(&swap),
            Some(U256::from(3_000_000_000_u64)),
            "V2 output = the positive (received) side"
        );
    }

    #[test]
    fn v4_captured_output_is_the_positive_side_swapper_received() {
        // V4 PoolManager amounts are the caller's delta (positive = received).
        // amount0=+997 (received token0), amount1=-1000 (paid token1).
        // Output = the positive side (+997) — this is the field that diverges
        // by 1-8 units vs hop_outputs[0] on mainnet V4-V3-V3 paths.
        let swap = swap_with_amounts(
            degenbot_simulation::SwapFamily::V4,
            alloy::primitives::I256::try_from(997_i128).unwrap(),
            alloy::primitives::I256::try_from(-1000_i128).unwrap(),
        );
        assert_eq!(
            captured_swap_output_amount(&swap),
            Some(U256::from(997_u64)),
            "V4 output = the positive (received) side"
        );
    }

    #[test]
    fn v3_captured_output_is_the_negative_side_pool_paid_out() {
        // V3 Swap amounts are the POOL's balance delta: positive = pool
        // received (swapper paid IN), negative = pool paid (swapper received
        // OUT). amount0=+1e18 (pool got token0 = input), amount1=-3000e6
        // (pool paid token1 = output). Output = the NEGATIVE side (-3000e6).
        // Picking the positive side here was the bug that masked the V4
        // divergence — it returned the V3 INPUT (= hop_outputs[i-1]).
        let swap = swap_with_amounts(
            degenbot_simulation::SwapFamily::V3,
            alloy::primitives::I256::try_from(1_000_000_000_000_000_000_i128).unwrap(),
            alloy::primitives::I256::try_from(-3_000_000_000_i128).unwrap(),
        );
        assert_eq!(
            captured_swap_output_amount(&swap),
            Some(U256::from(3_000_000_000_u64)),
            "V3 output = the negative (pool-paid) side magnitude, NOT the input"
        );
    }

    #[test]
    fn v3_reverse_direction_output_is_still_the_negative_side() {
        // Reverse V3 direction: amount0=-2e6 (pool paid token0 = output),
        // amount1=+500 (pool got token1 = input). Output still = negative side.
        let swap = swap_with_amounts(
            degenbot_simulation::SwapFamily::V3,
            alloy::primitives::I256::try_from(-2_000_000_i128).unwrap(),
            alloy::primitives::I256::try_from(500_i128).unwrap(),
        );
        assert_eq!(
            captured_swap_output_amount(&swap),
            Some(U256::from(2_000_000_u64)),
            "V3 reverse: output still the negative side"
        );
    }

    #[test]
    fn zero_amounts_return_none_for_all_families() {
        // A degenerate zero-zero swap: neither side carries an output → None.
        for family in [
            degenbot_simulation::SwapFamily::V2,
            degenbot_simulation::SwapFamily::V3,
            degenbot_simulation::SwapFamily::V4,
        ] {
            let swap = swap_with_amounts(
                family,
                alloy::primitives::I256::ZERO,
                alloy::primitives::I256::ZERO,
            );
            assert_eq!(
                captured_swap_output_amount(&swap),
                None,
                "zero-zero swap → None for {family:?}"
            );
        }
    }

    // ── match_reverted_swaps_to_hops emitter attribution (the IIA log lie) ─
    //
    // The bug this pins: a V3-V4-V3 whose V3c (outer) frame reverts captures
    // swaps in the EVM's EMISSION order [V4_emit, V3a_emit], because the V4 swap
    // (nested deepest) emits before the V3a swap (outer) completes. The OLD
    // index-based log compared captured[0]→hop_outputs[0], crossing V4↔V3a,
    // so the log lied about which hop diverged. Emitter attribution fixes it.

    /// Build a `CapturedSwap` from a (family, emitter, signed-amount pair),
    /// letting the matcher test pin emitters without decoding a real log.
    fn captured(
        family: degenbot_simulation::SwapFamily,
        emitter: Address,
        out_amount: u128,
    ) -> CapturedSwap {
        // V2/V4 output = positive side (swapper received); V3 output = negative
        // side (pool paid). Encode `out_amount` on the correct side per family.
        let (a0, a1) = match family {
            degenbot_simulation::SwapFamily::V3 => (
                -alloy::primitives::I256::try_from(out_amount).unwrap(),
                alloy::primitives::I256::ZERO,
            ),
            degenbot_simulation::SwapFamily::V2 | degenbot_simulation::SwapFamily::V4 => (
                alloy::primitives::I256::try_from(out_amount).unwrap(),
                alloy::primitives::I256::ZERO,
            ),
        };
        CapturedSwap {
            emitter,
            family,
            amount0: a0,
            amount1: a1,
            sqrt_price_x96: U256::ZERO,
            liquidity: U256::ZERO,
            tick: 0,
        }
    }

    /// A V3-V4-V3 path whose V3c outer frame reverts: the inspector captures the
    /// nested V4 swap (hop 1) and the V3a swap (hop 0) in emission order
    /// [V4, V3a]. Index-based attribution crossed them; emitter attribution
    /// must map each to its true hop.
    #[test]
    fn match_v3_v4_v3_captures_by_emitter_not_emit_index() {
        use degenbot_execution::{HopInfo, V3HopInfo, V4HopInfo};
        let v3a = address!("1111111111111111111111111111111111111111");
        let v3c = address!("2222222222222222222222222222222222222222");
        let pm = SMOKE_PM; // the V4 PoolManager (same emitter as every V4 hop)
        let hops = vec![
            HopInfo::V3(V3HopInfo {
                pool_address: v3a,
                token0_address: Address::ZERO,
                token1_address: Address::ZERO,
                fee: 3000,
                zfo: false,
            }),
            HopInfo::V4(V4HopInfo {
                pool_manager_address: pm,
                pool_id_hex: String::new(),
                currency0_address: Address::ZERO,
                currency1_address: Address::ZERO,
                fee: 100,
                tick_spacing: 1,
                hook_address: Address::ZERO,
                zfo: true,
            }),
            HopInfo::V3(V3HopInfo {
                pool_address: v3c,
                token0_address: Address::ZERO,
                token1_address: Address::ZERO,
                fee: 500,
                zfo: true,
            }),
        ];
        // Emission order: V4 (hop 1) emits first, then V3a (hop 0). The V4
        // actual (124_645) DIVERGES from its prediction (122_707) — the real
        // mainnet-fixture shape at block 25642187. V3a matches its prediction.
        let reverted_swaps = vec![
            captured(degenbot_simulation::SwapFamily::V4, pm, 124_645),
            captured(degenbot_simulation::SwapFamily::V3, v3a, 191),
        ];
        let hop_outputs = vec![191u128, 122_707u128, 64_688_975_647_480u128];
        let matches = match_reverted_swaps_to_hops(&reverted_swaps, &hops, &hop_outputs);
        // emit order preserved for the log...
        assert_eq!(matches[0].emit_index, 0);
        assert_eq!(matches[1].emit_index, 1);
        // ...but hop attribution is by emitter, NOT by emit index:
        assert_eq!(
            matches[0].hop_index,
            Some(1),
            "V4 emit must map to hop 1, not hop 0"
        );
        assert_eq!(
            matches[1].hop_index,
            Some(0),
            "V3a emit must map to hop 0, not hop 1"
        );
        // The V4 divergence is now correctly flagged (was masked by the cross):
        assert!(
            !matches[0].matched,
            "V4 actual 124645 ≠ predicted 122707 — divergence"
        );
        assert!(matches[1].matched, "V3a actual 191 == predicted 191");
    }

    /// A captured swap whose emitter matches no hop on the path is
    /// `hop_index = None` (defensive — should not occur, but must not panic).
    #[test]
    fn unmatched_emitter_attributes_to_none_without_panicking() {
        use degenbot_execution::{HopInfo, V2HopInfo};
        let stranger = address!("9999999999999999999999999999999999999999");
        let hops = vec![HopInfo::V2(V2HopInfo {
            pool_address: address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            token0_address: Address::ZERO,
            token1_address: Address::ZERO,
            fee: 30,
            zfo: true,
        })];
        let reverted_swaps = vec![captured(degenbot_simulation::SwapFamily::V2, stranger, 7)];
        let matches = match_reverted_swaps_to_hops(&reverted_swaps, &hops, &[10u128]);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].hop_index, None);
        assert_eq!(matches[0].actual_out, Some(U256::from(7u128)));
        assert_eq!(matches[0].predicted, None);
        assert!(!matches[0].matched);
    }

    /// The fix-enabler fields: the `[sim-revert-swap]` match must carry the
    /// V4 pool_id (the PoolManager emitter is shared, so identity = pool hash),
    /// the exact-in amount, and the post-swap sim scalars — so a recurrence can
    /// be driven through `v4_simulate_swap` at the EXACT state+input that
    /// produced `actual_out` (closing the solve-vs-sim gap). Shape mirrors the
    /// real UNI pool 0x9a5c1d2f recurrence (block 25673381, path 57150).
    #[test]
    fn reverted_swap_carries_pool_id_input_and_sim_scalars() {
        use degenbot_execution::{HopInfo, V4HopInfo};
        let pm = SMOKE_PM;
        let hops = vec![HopInfo::V4(V4HopInfo {
            pool_manager_address: pm,
            pool_id_hex: "0x9a5c1d2f4a7a7962a63259de6fcc1afb1d0aa1abdf5d19c23d22fd78953c5167"
                .to_string(),
            currency0_address: Address::ZERO,
            currency1_address: Address::ZERO,
            fee: 100,
            tick_spacing: 1,
            hook_address: Address::ZERO,
            zfo: true,
        })];
        // Exact-in: currency0 in (negative), currency1 out (positive).
        let swap = CapturedSwap {
            emitter: pm,
            family: degenbot_simulation::SwapFamily::V4,
            amount0: -alloy::primitives::I256::try_from(3_135u128).unwrap(),
            amount1: alloy::primitives::I256::try_from(772_076_574_181_336u128).unwrap(),
            sqrt_price_x96: U256::from(159_369_389_255_773_083_394_993u128),
            liquidity: U256::from(1_432_650_976_603_835_442u128),
            tick: -262_346,
        };
        let matches = match_reverted_swaps_to_hops(&[swap], &hops, &[772_076_574_181_336u128]);
        assert_eq!(matches.len(), 1);
        let m = &matches[0];
        assert_eq!(m.hop_index, Some(0));
        assert_eq!(
            m.pool_id.as_deref(),
            Some("0x9a5c1d2f4a7a7962a63259de6fcc1afb1d0aa1abdf5d19c23d22fd78953c5167")
        );
        assert_eq!(m.actual_in, Some(U256::from(3_135u128)));
        assert_eq!(m.actual_out, Some(U256::from(772_076_574_181_336u128)));
        // The pinned fix-enabler scalars ride the shared slot0 head-scalar
        // fact type (degenbot_pools::Slot0HeadScalars) so
        // the [sim-revert-swap] log and the solve-time tripwire speak one
        // type language.
        assert_eq!(
            m.sim_scalars,
            degenbot_pools::Slot0HeadScalars {
                sqrt_price_x96: U256::from(159_369_389_255_773_083_394_993u128),
                tick: -262_346,
                liquidity: U256::from(1_432_650_976_603_835_442u128),
            }
        );
        assert!(m.matched);
    }

    #[test]
    fn conservative_flag_default_on_and_parse() {
        // The falsey/parse contract lives in degenbot_config::parse_bool_flag.
        assert!(!::degenbot_config::parse_bool_flag("0").unwrap());
        assert!(::degenbot_config::parse_bool_flag("1").unwrap());
        assert!(::degenbot_config::parse_bool_flag("yes").unwrap());
        assert!(::degenbot_config::parse_bool_flag("").is_err());
    }

    /// The end-to-end clamp attestation: the concentrated-liquidity-hop clamp
    /// (`consumed_inputs`) reaches EXECUTABLE BYTES through the built-in
    /// deployment's Encode part — the only producer of the executor command
    /// stream in production, and the exact adapter the production seam injects.
    ///
    /// Path-5000 shape (`three_hop_v2_v4_v3`): the V4 mid hop is over-fed —
    /// `hop_outputs[0]` (the V2 output it forwards) is one more than the
    /// solver's clamped executable swap-in `consumed_inputs[1]`. The emitted
    /// `V4_SWAP_COMPACT` amount must embed `consumed_inputs[1]` (0x…773593FF =
    /// 1_999_999_999), NOT `hop_outputs[0]` (0x…77359400 = 2_000_000_000).
    /// This proves the clamp is not diagnostic-only — it is what actually
    /// gets encoded for the on-chain fill.
    #[test]
    fn clamped_consumed_inputs_reach_encoded_v4_swap_in() {
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let usdc = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
        let pm = address!("000000000004444c5dc75cB358380D2e3dE08A90");
        let executor = address!("DeAd0000000000000000000000000000000000Be");

        let path = SimulatePath {
            candidate_id: degenbot_execution::CandidateId::new(5000, DeploymentId::new(1)),
            optimal_input: 1_000_000_000_000_000_000u128,
            // V2 out = 2e9, V4 out = 2_001e15, V3 out = 2_001e6.
            // The concentrated liquidity clamp: V4 (idx 1) swap-in is ONE BELOW its forward.
            steps: Box::new([
                SolveStep {
                    output: 2_000_000_000u128,
                    consumed_input: 1_000_000_000_000_000_000u128,
                    state_nonce: 0,
                },
                SolveStep {
                    output: 2_001_000_000_000_000_000u128,
                    consumed_input: 1_999_999_999u128,
                    state_nonce: 0,
                },
                SolveStep {
                    output: 2_001_000_000u128,
                    consumed_input: 2_001_000_000u128,
                    state_nonce: 0,
                },
            ]),
            path_info: PathInfo::new(vec![
                HopInfo::V2(V2HopInfo {
                    pool_address: address!("1111111111111111111111111111111111111111"),
                    token0_address: weth,
                    token1_address: usdc,
                    fee: 30,
                    zfo: true,
                }),
                HopInfo::V4(V4HopInfo {
                    pool_manager_address: pm,
                    pool_id_hex: "0x1111".to_string(),
                    // b's input = a's output (USDC), output WETH (the coherent
                    // V2V4V3 chain — b must consume a's forward).
                    currency0_address: usdc,
                    currency1_address: weth,
                    fee: 3000,
                    tick_spacing: 60,
                    hook_address: Address::ZERO,
                    zfo: true,
                }),
                HopInfo::V3(V3HopInfo {
                    pool_address: address!("6666666666666666666666666666666666666666"),
                    token0_address: weth,
                    token1_address: usdc,
                    fee: 500,
                    zfo: true,
                }),
            ]),
            solve_block: 22_562_100,
        };

        // The clamp reaches EXECUTABLE BYTES through the built-in deployment's
        // Encode part — the only producer of the command stream in production.
        let adapter = CmdExecutorBinding::new(
            SMOKE_OWNER,
            executor,
            weth,
            pm,
            EncodeOptions {
                erc6909_profit: false,
                use_v4_batch: false,
                ..EncodeOptions::default()
            },
        )
        .adapter();
        let hop_outputs = path.expected_outputs_vec();
        let consumed_inputs = path.consumed_inputs_vec();
        let transaction = adapter
            .encode(
                &path.path_info,
                &degenbot_execution::ComposerInputs {
                    optimal_input: path.optimal_input,
                    hop_outputs: &hop_outputs,
                    consumed_inputs: &consumed_inputs,
                    opts: degenbot_execution::ComposeOptions,
                },
            )
            .expect("expression should encode");
        let cmd = transaction.data;

        // The V4 swap-in amount is a big-endian u96. 1_999_999_999 encodes
        // ending in 0x…773593FF; the un-clamped forward 2_000_000_000 ends in
        // 0x…77359400. Assert the clamped bytes are present and the forward
        // bytes are absent.
        let clamped = [0u8, 0, 0, 0, 0, 0, 0, 0, 0x77, 0x35, 0x93, 0xFF];
        let forward = [0u8, 0, 0, 0, 0, 0, 0, 0, 0x77, 0x35, 0x94, 0x00];
        assert!(
            cmd.windows(clamped.len()).any(|w| w == clamped),
            "V4 swap-in must embed consumed_inputs[1]=1_999_999_999"
        );
        assert!(
            !cmd.windows(forward.len()).any(|w| w == forward),
            "V4 swap-in must NOT embed the un-clamped hop_outputs[0]=2_000_000_000"
        );
    }

    /// The `degenbot.bundle.simulate` span is recorded
    /// per candidate at the shared sim seam, with the terminal verdict.
    /// Reuses the reverting-path smoke fixture (exec() call [3] reverts ->
    /// `Ok(None)` -> `not_profitable`).
    #[test]
    fn simulate_span_records_verdict_for_reverting_path() {
        const UNIQUE_PATH_ID: u64 = 991_234;
        let cap = span_capture::global();
        let asserter = Asserter::new();
        let provider = smoke_provider(&asserter);
        let deployment = smoke_deployment(Bytes::from_static(&[0xfe])); // INVALID
        let ctx = smoke_ctx(&provider, &deployment);
        let mut cache_db = ::evm2::evm::InMemoryDB::default();
        degenbot_simulation::sim::evm2::state_override::apply_simulation_overrides(
            &mut cache_db,
            &ctx.deployment.override_params(),
        );
        let mut buckets = FailBuckets::new();

        let path = smoke_v2_path(UNIQUE_PATH_ID);
        let result = simulate_in_process_with_db(&ctx, cache_db, &path, &mut buckets).unwrap();
        assert!(result.is_none(), "reverting execute returns None");

        let mine: Vec<_> = cap
            .snapshot()
            .into_iter()
            .filter(|(name, fields)| {
                name == "degenbot.bundle.simulate"
                    && fields.get("path_id").map(String::as_str)
                        == Some(UNIQUE_PATH_ID.to_string().as_str())
            })
            .collect();
        assert_eq!(
            mine.len(),
            1,
            "one simulate span for the candidate; all: {:?}",
            cap.snapshot()
        );
        assert_eq!(
            mine[0].1.get("simulate.verdict").map(String::as_str),
            Some("not_profitable"),
            "reverted path is not_profitable; fields: {:?}",
            mine[0].1
        );
    }

    /// SIMSPANDUP (trace 73604143 cleanup): the worker-inline arm hands the
    /// caller-held `degenbot.bundle.simulate` span to
    /// [`simulate_path_on_evm_in_span`] — the seam must REUSE it (records the
    /// verdict on it) rather than open a duplicate same-named span nested
    /// under `degenbot.simulate.inline`.
    #[test]
    fn caller_held_sim_span_is_reused_not_duplicated() {
        const UNIQUE_PATH_ID: u64 = 991_235;
        let cap = span_capture::global();
        let asserter = Asserter::new();
        let provider = smoke_provider(&asserter);
        let deployment = smoke_deployment(Bytes::from_static(&[0xfe])); // INVALID
        let ctx = smoke_ctx(&provider, &deployment);
        let mut cache_db = ::evm2::evm::InMemoryDB::default();
        degenbot_simulation::sim::evm2::state_override::apply_simulation_overrides(
            &mut cache_db,
            &ctx.deployment.override_params(),
        );
        let mut buckets = FailBuckets::new();

        let path = smoke_v2_path(UNIQUE_PATH_ID);
        // The caller (worker arm analog) holds a span of the seam name with
        // the declared `simulate.*` fields, exactly as solver_dispatch's
        // worker arms open before the runtime hop.
        let caller = tracing::info_span!(
            "degenbot.bundle.simulate",
            path_id = UNIQUE_PATH_ID,
            sim.path = "worker_inline",
            simulate.verdict = tracing::field::Empty,
            simulate.expected_profit = tracing::field::Empty,
            simulate.error_reason = tracing::field::Empty,
        );
        let _enter = caller.enter();

        // Build the EVM exactly as `simulate_in_process_with_db` does so the
        // seam is reached directly under the caller-held span.
        let mut evm = degenbot_simulation::sim::evm2::build_block_evm2(
            ::evm2::evm::Db::new(cache_db),
            ctx.block.base_fee_next,
            ctx.block.current_block,
            ctx.block.block_timestamp,
        );
        let mut session = degenbot_simulation::sim::evm2::session::Evm2SimSession::with_block_env(
            &mut evm,
            degenbot_simulation::sim::evm2::handle_block_env(
                ctx.block.base_fee_next,
                ctx.block.current_block,
                ctx.block.block_timestamp,
            ),
        );
        let result =
            simulate_path_on_evm_in_span(&mut session, &ctx, &path, &mut buckets, &caller).unwrap();
        assert!(result.is_none(), "reverting execute returns None");

        let mine: Vec<_> = cap
            .snapshot()
            .into_iter()
            .filter(|(name, fields)| {
                name == "degenbot.bundle.simulate"
                    && fields.get("path_id").map(String::as_str)
                        == Some(UNIQUE_PATH_ID.to_string().as_str())
            })
            .collect();
        assert_eq!(
            mine.len(),
            1,
            "caller-held sim span is reused, not duplicated; all: {:?}",
            cap.snapshot()
        );
        assert_eq!(
            mine[0].1.get("simulate.verdict").map(String::as_str),
            Some("not_profitable"),
            "verdict lands on the caller-held span; fields: {:?}",
            mine[0].1
        );
    }
}
