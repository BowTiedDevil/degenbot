//! `BotState` — the single owner of all runtime state.
//!
//! All pool data, token metadata, calculation methods, and swap encoding
//! live here. Python objects are thin `PyO3` handles carrying keys into
//! `BotState`'s `HashMaps`.

use hashbrown::HashMap;

use alloy::primitives::{Address, U256};

use ::degenbot_pools::state_history::{JournalError, ReorgPoolState};
use degenbot_uniswap::v2_encoding::{encode_v2_swap, EncodedCall};

pub mod apply_telemetry;
pub mod backrun_resolver;
pub mod balance_vector_orchestration;
pub mod balancer_stable_state;
pub mod balancer_weighted_state;
pub mod block_pump;
pub mod bot;
pub mod cl_orchestration;
pub mod cl_route;
pub mod construction_io;
pub mod curve_data_provider_impl;
pub mod curve_state;
pub mod divergence_probe;
pub mod epoch;
pub mod epoch_delta;
pub mod liquidity_verifier;
pub mod log_dispatcher;
pub mod planning;
pub mod pool_builder;
pub mod pump_control;
pub mod pump_telemetry;
/// PRG-2: the keyed registration-gate table for immutable V4
/// admission verdicts (see [registration_gate] docs).
pub mod registration_gate;
pub mod registration_lifecycle;
pub mod reorg_coordinator;
pub mod reserve_pair_orchestration;
pub(crate) mod resolve;
pub mod sim_anchor;
pub mod snapshot_verify;
pub(crate) mod solve_anchor;
pub mod stage_handlers;
pub mod stage_machine;
pub mod stage_telemetry;
/// the process-wide typed BotConfig holder. The degenbot-config
/// loader (the ONLY environment-reading site in the workspace) produces the
/// value once at startup; every formerly env-reading call site below reads
/// its typed section from here. Purely a VALUE holder — no env access.
pub mod stance;
pub mod state_lock;
pub mod swap_simulation;
pub mod tick_assembly;

// Re-export the merged V3/V4/Curve state types (ADR-003: BotState owns
// pool state; Curve is the ADR-003 "third family").
pub use ::degenbot_pools::aerodrome_v2_state::{
    AerodromeV2PoolIdentity, AerodromeV2PoolState, RegisterAerodromeV2PoolParams,
};
pub use ::degenbot_pools::curve_data_provider::{CurveDataProvider, CurveDataProviderError};
pub use ::degenbot_pools::curve_dy_io::{resolve_dy_inputs, CurveInputsError};
pub use ::degenbot_pools::rate_provider::{
    BalancerRateProvider, RateProviderError, StaticRateProvider,
};
pub use ::degenbot_pools::spec_bounds::{SpecValue, SpecViolation, UINT112_MAX};
pub use ::degenbot_pools::state_history::BalancesBlockDelta;
pub use ::degenbot_pools::v3_state::{
    v3_simulate_swap, BufferedV3LiquidityUpdate, BufferedV3PoolEvent, BufferedV3SwapEvent,
    ClSlotLayout, PoolTickCoverage, RegisterV3PoolError, RegisterV3PoolParams,
    RegistrationLifecycle, SimulateSwapError, V3PoolIdentity, V3PoolState, V3SwapOutcome,
    V3SwapUpdate,
};
pub use balancer_stable_state::{
    BalancerStablePoolIdentity, BalancerStablePoolState, RegisterBalancerStablePoolParams,
};
pub use balancer_weighted_state::{
    BalancerWeightedPoolIdentity, BalancerWeightedPoolState, RegisterBalancerWeightedPoolParams,
};
// the block-clock channel type is a shared-kernel fact type
// (degenbot-core), not bot_runtime knowledge — the runtime’s engine merely
// relays header ticks through it and the PyO3 layer subscribes at the edge.
pub use degenbot_core::block_clock_pipe::{BlockClockPipe, BlockNotification};
// (5WTYYQ) The subscription topic filter is transport knowledge now; the
// dispatcher’s defensive re-check + the FSM’s relevance gate consume the same
// list the ingestion crate filters with.
pub use cl_orchestration::{InstallWordOutcome, RegisteredV4, StagedWordFetch};
pub use curve_state::{CurvePoolIdentity, CurvePoolState, RegisterCurvePoolParams};
pub use degenbot_ingestion::RELEVANT_TOPICS;
use degenbot_math::curve::{CurveBasePoolPort, CurveSwapError};
pub use divergence_probe::{TrackedSlotKind, TrackedSlotProbe};
pub use epoch::{BlockContext, Epoch, StaleEpoch};
pub use epoch_delta::EpochDelta;
pub use pump_control::PumpControl;
pub use registration_lifecycle::{
    run_cl_v3_lifecycle, run_cl_v4_lifecycle, run_v3_registration_lifecycle,
    run_v4_registration_lifecycle, RegistrationLifecycleError,
};
pub use sim_anchor::SimAnchorState;
pub use stage_handlers::{
    AffectedPaths, CandidateId, Finalize, FinalizeOutcome, Gate, GateOutcome, Publish,
    PublishOutcome, QuiesceOutcome, QuiesceVerdict, Resolve, Rewind, RewindOutcome, Simulate,
    SimulateOutcome, Solve, SolveOutcome, Stage, StageError, StageHandlers,
};

pub use ::degenbot_pools::v4_state::{
    v4_simulate_swap, BufferedV4LiquidityUpdate, BufferedV4PoolEvent, BufferedV4SwapEvent,
    RegisterV4PoolError, RegisterV4PoolParams, V4PoolIdentity, V4PoolKey, V4PoolState, V4StateSync,
    V4SwapUpdate, AMOUNT_MODIFYING_HOOK_MASK, V4_DYNAMIC_FEE_FLAG,
};

// Re-export the ADR-004 typed TickMap boundary trait (V3 + V4 impls both live
// in `tick_map.rs`). State structs stay flat; only verifier/apply views are
// typed-narrowed.
pub use ::degenbot_pools::tick_map::{TickMap, TickMapMut};

// Re-export the unified block stage machine : the
// per-block state map + decision producer + watchdogs + gates in ONE pure
// machine; the pump drives it (see `bot_core/stage_machine.rs`). The
// retired `BlockClock`/`PumpFSM` types are gone (hard cutover, Q6) —
// their sub-state lives inside `StageMachine`.
pub use stage_machine::{
    BlockState, CompletenessDecision, HeaderDecision, LogDecision, StageDecision, StageMachine,
    WatchdogPhase,
};

// ---------------------------------------------------------------------------
// Pool state types
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Pool registry sum type + V2 identity/state + token entry + swap-sim dispatch.
// **Relocated** to `degenbot-pools`; re-exported here at the historical
// `bot_core::*` paths so consumers resolve unchanged.
// Transient re-export — repointed at `degenbot_pools::*` natively.
// ---------------------------------------------------------------------------

pub use ::degenbot_pools::registry::{
    ConcentratedLiquidityPool, ConcentratedLiquidityPoolMut, PoolEntry, RegisteredPoolFamily,
    TokenEntry,
};
pub use ::degenbot_pools::simulate_swap::simulate_swap;
pub use ::degenbot_pools::v2_state::{
    RegisterV2PoolError, RegisterV2PoolParams, V2PoolIdentity, V2PoolState,
};
pub use ::degenbot_pools::TickInfo;

// ---------------------------------------------------------------------------
// BotState
// ---------------------------------------------------------------------------

/// The single owner of all runtime state.
///
/// All pool data, token metadata, engines, and encoded results live here.
/// Python holds `PyBot` — an `Arc` pointing here.
///
/// ADR-006 D4: `BotState` is the pure-data submodule (no I/O) behind the
/// thin `Bot` orchestrator facade. `pub` — callers cross the
/// orchestrator seam; `BotState` is a private deep module with its own test
/// seam.
pub struct BotState {
    /// Pool registry: `pool_id` → `PoolEntry`.
    pools: HashMap<u64, PoolEntry>,
    /// Pool contract address → `pool_id`.
    pool_addresses: HashMap<Address, u64>,
    /// Token registry: address → `TokenEntry`.
    tokens: HashMap<Address, TokenEntry>,
    /// Auto-incrementing pool ID.
    next_pool_id: u64,
    /// Reorg journal depth (in blocks) for every pool — one mainnet epoch
    /// by default (ADR-003). Applied uniformly to V2/V3/V4.
    journal_depth: usize,
    /// Dual-buffer for V3 liquidity (Mint/Burn) events awaiting pool
    /// registration (ADR-003: the accurate-state buffer lives on `BotState`, not
    /// the dissolved `V3BlockEngine`).
    v3_buffer: ::degenbot_pools::liquidity_event_buffer::LiquidityEventBuffer<
        Address,
        BufferedV3PoolEvent,
    >,
    /// Dual-buffer for V4 `ModifyLiquidity` events awaiting pool registration.
    /// Keyed by `(pool_manager, pool_id)`.
    v4_buffer: ::degenbot_pools::liquidity_event_buffer::LiquidityEventBuffer<
        (Address, degenbot_decoders::v4_swap_decoder::V4PoolId),
        BufferedV4PoolEvent,
    >,
    /// V4 pool registry: `(pool_manager, pool_id)` → `pool_id` (single entry
    /// per pool — ADR-003 Option I: orientation derived at solve from
    /// `zero_for_one`, not stored as separate forward/reverse entries).
    v4_pool_ids: HashMap<(Address, degenbot_decoders::v4_swap_decoder::V4PoolId), u64>,
    /// Rust-owned V4 pool-manager → `StateView` registry (ADR-005 / Option 2).
    /// The canonical V4 scalar state is read via the `StateView`'s
    /// `getSlot0`/`getLiquidity`, not `getPool(poolManager)` (which reverts on
    /// the canonical deployment). Keyed by `pool_manager`; each `V4PoolState`
    /// under a manager shares its manager's `StateView`. Seeded once per manager
    /// via `register_v4_state_view` (the driver reads it from the
    /// `pool_managers` DB row); the solver-state verifier reads it via
    /// [`BotState::state_view_for`].
    v4_state_views: HashMap<Address, Address>,
    /// PRG-2: the keyed registration-gate — immutable V4
    /// admission verdicts (dynamic fee / fee-exceeds-encoder-limit) recorded
    /// by [`Self::register_v4_pool`] refusals and consulted pre-RPC by the
    /// `PyO3` build path. Bounded by refused pools, not candidates.
    registration_gate: registration_gate::RegistrationGate,
    /// The snapshot seed block `S = min(fetch_newest_update_block(V3), V4)`.
    /// Set by `Bot::load_snapshot_from_db` (or `load_snapshot_from_py`) when a
    /// snapshot is loaded; consumed by the auto-backfill (B1/J3FMDO) that
    /// closes the `S+1..W-1` gap before resume. `None` when no snapshot was
    /// loaded (cold-start path — the pump anchors on `first_observed_block`).
    snapshot_seed_block: Option<u64>,
    /// The highest FULLY-DELIVERED block — the delivery cutoff (last complete
    /// block, 3M5PO5). The registration drain reads this as the
    /// `drain_pump_completed` cutoff instead of a buffer-local shadow marker;
    /// `0` means no block has been tombstoned → nothing drains. Owned here as
    /// a plain monotone value that outlives pump runs: the pump
    /// driver advances it on the tombstone verdict, and a resume never resets
    /// it.
    pump_complete_cutoff: u64,
    /// Per-pool event-witnessed horizon: the
    /// highest block of any V3/V4 event ROUTED for this pool (applied
    /// directly OR staged into a buffer). Advanced ONLY by routed events —
    /// never by imported DB-row stamps — so it corroborates (or refutes) a
    /// pin's freshness claim independently of the seed. Keyed like the
    /// family buffers: address for V3, `(pool_manager, pool_id)` for V4.
    /// ADR-040 quarantine set: pool ids currently EXCLUDED from solve
    /// resolution (tainted-by-desync surfaces). Family-agnostic — spans
    /// V2/V3/V4 and every other family because the gate sits in the
    /// projection dispatcher, not per state type. Each transition bumps the
    /// pool's `state_nonce` (dirties it) so cached hop projections and
    /// in-flight candidates invalidate.
    quarantined_pools: hashbrown::HashSet<u64>,
    v3_event_horizons: HashMap<Address, u64>,
    v4_event_horizons: HashMap<(Address, degenbot_decoders::v4_swap_decoder::V4PoolId), u64>,
}

/// Why [`BotState::encode_swap`] refused a call.
#[derive(Debug, thiserror::Error)]
pub enum EncodeSwapError {
    /// The pool id is not registered.
    #[error("pool {pool_id} is not registered")]
    NotRegistered {
        /// The requested pool id.
        pool_id: u64,
    },
    /// The pool's family has no swap-call encoder.
    #[error("pool {pool_id} family {family:?} has no swap encoder")]
    UnsupportedFamily {
        /// The requested pool id.
        pool_id: u64,
        /// The registered family tag.
        family: &'static str,
    },
    /// The V2 encoder rejected the call.
    #[error("pool {pool_id} swap call failed to encode")]
    Encode {
        /// The requested pool id.
        pool_id: u64,
        /// The ABI-level failure.
        #[source]
        cause: degenbot_core::errors::AbiDecodeError,
    },
}

/// Diagnostic: log every V3 pump-buffer INSERTION. Companion to the
/// drain-side `[dbg-drain]` logs in `apply_backfill_buffer_v3`/
/// `apply_pump_buffer_v3` — diffing insertion vs drain logs reveals whether a
/// Mint that on-chain shows at block N was (a) buffered-then-missed-by-drain
/// (insertion logged, no matching drain apply) or (b) never buffered at all
/// (no insertion log). `tag` ∈ {'L' (unregistered Live-eligible path),
/// 'Q' (Quarantined deferral)}. Always-on DEBUG on `pump`.
/// The base-pool delegation port for metapool `get_dy_underlying`. Implements [`CurveBasePoolPort`] by delegating each op to a
/// registered base `CurvePool` in the same `BotState` — the Rust twin of the
/// Python `_LazyBasePool`/`CurveStableswapPool` base-pool delegate. All
/// methods are immutable reads on `&BotState`, so the port borrows the state
/// freely without a re-entrant lock.
/// Convert an orchestration [`CurveInputsError`] into a [`CurveSwapError`]
/// for the `CurveBasePoolPort` contract (which can only carry `CurveSwapError`).
fn curve_error_into_swap(e: CurveInputsError) -> CurveSwapError {
    match e {
        CurveInputsError::Swap(x) => x,
        CurveInputsError::UnknownPool(_) => CurveSwapError::MissingValue("unknown base pool"),
        CurveInputsError::NotMetapool => CurveSwapError::NotMetapool,
        CurveInputsError::LengthMismatch(_) => {
            CurveSwapError::MissingValue("base-pool length mismatch")
        }
        CurveInputsError::NoProvider(_) => {
            CurveSwapError::MissingValue("base-pool provider missing")
        }
        CurveInputsError::Provider(_) => {
            CurveSwapError::MissingValue("base-pool provider fetch failed")
        }
    }
}

pub(crate) struct BotCurveBasePoolPort<'a> {
    state: &'a BotState,
    base_id: u64,
}

impl CurveBasePoolPort for BotCurveBasePoolPort<'_> {
    fn token_count(&self) -> usize {
        self.state
            .get_curve_identity(self.base_id)
            .map_or(0, degenbot_pools::curve_state::CurvePoolIdentity::n_coins)
    }

    fn fee(&self) -> U256 {
        self.state
            .get_curve_identity(self.base_id)
            .map_or(U256::ZERO, |id| U256::from(id.fee))
    }

    fn calc_token_amount(&self, amounts: &[U256], block: u64) -> Result<U256, CurveSwapError> {
        self.state
            .curve_calc_token_amount(self.base_id, amounts, true, block)
            .map_err(curve_error_into_swap)
    }

    fn get_dy(&self, i: usize, j: usize, dx: U256, block: u64) -> Result<U256, CurveSwapError> {
        self.state
            .curve_get_dy(self.base_id, i, j, dx, block, None)
            .map_err(curve_error_into_swap)
    }

    fn calc_withdraw_one_coin(
        &self,
        token_amount: U256,
        i: usize,
        block: u64,
    ) -> Result<U256, CurveSwapError> {
        self.state
            .curve_calc_withdraw_one_coin(self.base_id, token_amount, i, block)
            .map_err(curve_error_into_swap)
    }
}

/// Whether the verify-diagnostics probes are enabled.
///
/// Conservative default ON (`verify_dbg`, via [`bot_env_flag_default_on`]);
/// set `=0` to disable the structural visibility probes that diagnose
/// intermittent liquidity-map verification misses at startup (the pump /
/// drain / verifier concurrency window). The probes are pure `log::info!`
/// emission — zero behavior change (a single env-var check per call site).
///
/// Probes gated here:
/// - `mark_v3/v4_pump_block_complete` logs the count of pump events at or
///   below the marked block (a `mark_complete(W)` with zero pump events for
///   an active pool proves the pump never delivered block W's logs — the
///   subscribe→resume drop).
/// - `pin_v3/v4_post_drain_snapshot` logs the pinned `(tick_data_block,
///   tick_data.len(), pump_count_at_or_below, last_complete_block)` so a
///   step-2 mismatch can be correlated to the drain that produced the pin.
///   NOTE: `tick_data_block` may legitimately exceed `last_complete_block`
///   when the registration seed carries the live WS head while the pump
///   buffer has not yet tombstoned it (a benign
///   `pump_count_at_or_below == 0` case). It
///   is NOT by itself a bug signal — the real failure symptom is a divergent
///   `tick_data` entry (ghost gross/net) against on-chain at the pinned block
///   (the `[verify-dbg] divergence set`). Do not read `update_block >
///   last_complete_block` alone as evidence of a leaked in-progress event.
/// - `set_v3/v4_pool_live` logs the count + block numbers of the retained
///   in-progress-block tail flushed via the unguarded `drain_pump`.
impl BotState {
    /// Create a new, empty `BotState` with the default 32-block reorg journal.
    #[must_use]
    pub fn new() -> Self {
        Self::with_journal_depth(32)
    }

    /// Borrow a registered pool entry by ID. Used by the structural `Pool`
    /// handle prototype (V2 slice) to present a family-agnostic interface.
    #[must_use]
    pub fn pool_entry(&self, pool_id: u64) -> Option<&PoolEntry> {
        self.pools.get(&pool_id)
    }

    /// The pool's per-mutation state nonce (AV42C7 staleness gate). Returns
    /// `0` for an unregistered pool (the dispatch seam treats an unknown
    /// pool as fresh — it will fail the path-validity check elsewhere).
    /// Used by the dispatch fan-out to detect a stale solve result: the
    /// solver snapshots each hop's nonce at resolve time; the sim seam
    /// re-reads it and skips candidates whose pool state advanced since.
    #[must_use]
    pub fn pool_state_nonce(&self, pool_id: u64) -> u64 {
        self.pools.get(&pool_id).map_or(0, PoolEntry::state_nonce)
    }

    /// The `update_block` of the pool at `pool_id` — the block its reserves /
    /// `sqrt_price` / `tick` / liquidity were last mutated by a forward `Sync` /
    /// `Swap` / Mint-Burn event. `0` for an unregistered pool (never advanced).
    ///
    /// `update_block` is a last-activity clock, NOT a staleness signal: a pool
    /// that last mutated N blocks ago is quiet (its stored state is byte-
    /// identical to on-chain), not stale. The former solve-time staleness
    /// gate mis-used it to defer quiet paths and
    /// was REMOVED. The ADR-021 chain-vs-solver tripwire retiree:
    /// in-process chain-vs-solver-state verification is retired
    /// with the stage-separated data plane — desync the plane excludes is
    /// unrepresentable — so `update_block` stays a pure bookkeeping clock
    /// (used by the Q1a merge staleness oracle and the epoch delta). Upstream
    /// verification remains at the Published edge (`CompletenessDecision::Verify`
    /// → `assert_ws_block_complete`).
    #[must_use]
    pub fn pool_update_block(&self, pool_id: u64) -> u64 {
        self.pools.get(&pool_id).map_or(0, PoolEntry::update_block)
    }

    /// ADR-040 quarantine seam: exclude the pool from solve resolution
    /// (tainted surface containment). Idempotent; first transition bumps the
    /// pool's `state_nonce` so every cached projection + in-flight solver
    /// snapshot invalidates. Returns `true` when this call CHANGED the state
    /// (a `false` return is a no-op or an unknown pool id — callers log).
    /// Maintains the `degenbot.engine.quarantined_pools` scrape gauge.
    pub fn quarantine_pool(&mut self, pool_id: u64) -> bool {
        if !self.pools.contains_key(&pool_id) {
            return false;
        }
        if !self.quarantined_pools.insert(pool_id) {
            return false;
        }
        if let Some(entry) = self.pools.get_mut(&pool_id) {
            entry.bump_state_nonce();
        }
        if let Some(p) = crate::instruments::pipeline() {
            p.set_quarantined_pools(self.quarantined_pools.len());
        }
        true
    }

    /// ADR-040 quarantine release: re-admit the pool to solve resolution and
    /// dirty its nonce so stale `Invalid(Quarantined)` cache entries cannot
    /// stick. Returns `true` when this call CHANGED the state.
    pub fn release_pool(&mut self, pool_id: u64) -> bool {
        if !self.quarantined_pools.remove(&pool_id) {
            return false;
        }
        if let Some(entry) = self.pools.get_mut(&pool_id) {
            entry.bump_state_nonce();
        }
        if let Some(p) = crate::instruments::pipeline() {
            p.set_quarantined_pools(self.quarantined_pools.len());
        }
        true
    }

    /// ADR-040: is the pool currently quarantined (excluded from solve)?
    #[must_use]
    pub fn is_pool_quarantined(&self, pool_id: u64) -> bool {
        self.quarantined_pools.contains(&pool_id)
    }

    /// ADR-040: current quarantine depth (the scrape gauge's backing count).
    #[must_use]
    pub fn quarantined_pool_count(&self) -> usize {
        self.quarantined_pools.len()
    }

    /// The pool-state **price clock head**: the maximum `update_block` across
    /// every registered pool (V2/V3/V4), `0` when none are registered.
    ///
    /// This is the block the live pool state actually reflects. During a
    /// backfill/drain desync the pools are advanced ahead of the pump's
    /// header clock, so `pool_state_head()` can exceed the drain `block_number`
    /// the correct solve/verify/sim anchor is this head, NOT the lagging
    /// clock. Because a pool is unchanged from its `update_block` onward, a
    /// single head anchor reproduces each path's solver state exactly
    /// (unchanged pools have byte-identical EVM state at `update_block` and
    /// head), so one shared sim cache serves every path.
    #[must_use]
    pub fn pool_state_head(&self) -> u64 {
        self.pools
            .values()
            .map(PoolEntry::update_block)
            .max()
            .unwrap_or(0)
    }

    /// The pool's **liquidity** clock (`tick_data_block`, two-stamp rule) —
    /// the block its tick map reflects. See [`PoolEntry::tick_data_block`]. A
    /// CL pool with `pool_tick_data_block` well behind `pool_update_block` is
    /// the staged-clock desync class (`0x5653`): fresh price, stale tick map.
    /// Returns `0` for an unregistered id (the freshness gate treats 0 as
    /// stale, mirroring [`Self::pool_update_block`]).
    #[must_use]
    pub fn pool_tick_data_block(&self, pool_id: u64) -> u64 {
        self.pools
            .get(&pool_id)
            .map_or(0, PoolEntry::tick_data_block)
    }

    /// Create a new, empty `BotState` with a custom reorg journal depth.
    #[must_use]
    pub fn with_journal_depth(journal_depth: usize) -> Self {
        Self {
            pools: HashMap::new(),
            pool_addresses: HashMap::new(),
            tokens: HashMap::new(),
            next_pool_id: 1,
            journal_depth,
            v3_buffer: ::degenbot_pools::liquidity_event_buffer::LiquidityEventBuffer::new(),
            v4_buffer: ::degenbot_pools::liquidity_event_buffer::LiquidityEventBuffer::new(),
            v4_pool_ids: HashMap::new(),
            v4_state_views: HashMap::new(),
            registration_gate: registration_gate::RegistrationGate::default(),
            snapshot_seed_block: None,
            pump_complete_cutoff: 0,
            v3_event_horizons: HashMap::new(),
            quarantined_pools: hashbrown::HashSet::new(),
            v4_event_horizons: HashMap::new(),
        }
    }

    /// The current delivery cutoff (`0` until the first tombstone). Read of
    /// the value the registration drain gates on.
    #[must_use]
    pub fn pump_complete_cutoff(&self) -> u64 {
        self.pump_complete_cutoff
    }

    /// Monotonically advance the delivery cutoff (last complete block). The
    /// live pump drives this when executing the `TombstonePrevious` verdict
    ///; tests that drive the registration drain without a pump use
    /// the same entry point.
    pub fn advance_pump_complete_cutoff(&mut self, block: u64) {
        if block > self.pump_complete_cutoff {
            self.pump_complete_cutoff = block;
        }
    }

    // --- ADR-005 slice 12a: Balancer V2 weighted state port -------------

    // --- ADR-005 slice 12c: Balancer V2 stable state port --------------

    /// Return the pool-family tag for `pool_id` as a kebab-case string
    /// (`"v2"`, `"v3"`, `"v4"`, `"curve"`, `"balancer-weighted"`,
    /// `"balancer-stable"`). Returns `""` for an unregistered `pool_id`.
    ///
    /// This is the uniform family-guard primitive every `_from_py_pool`
    /// seam asserts against — dispatches on the `PoolEntry` variant directly,
    /// so it is correct for every registered family (unlike the V2-only
    /// `variant` getter on `PyLiquidityPool`, which returns `""` for non-V2).
    #[must_use]
    pub fn pool_family(&self, pool_id: u64) -> &'static str {
        match self.pools.get(&pool_id) {
            Some(PoolEntry::V2(..)) => "v2",
            Some(PoolEntry::V3(..)) => "v3",
            Some(PoolEntry::V4(..)) => "v4",
            Some(PoolEntry::Curve(..)) => "curve",
            Some(PoolEntry::BalancerWeighted(..)) => "balancer-weighted",
            Some(PoolEntry::BalancerStable(..)) => "balancer-stable",
            Some(PoolEntry::AerodromeV2(..)) => "aerodrome-v2",
            None => "",
        }
    }

    /// Set the maximum age (in blocks) for buffered V3 pump events.
    /// `None` means unbounded. Takes effect on the next `expire_v3_buffered`.
    pub const fn set_v3_buffer_max_age(&mut self, max_age: Option<u64>) {
        self.v3_buffer.set_max_age(max_age);
    }

    /// The snapshot seed block `S` — `min(fetch_newest_update_block(V3), V4)`
    /// across the loaded snapshots. `None` when no snapshot was loaded (the
    /// cold-start path pumps directly from `first_observed_block`). Set by
    /// `Bot::load_snapshot_from_db` / `load_snapshot_from_py`; consumed by the
    /// auto-backfill (`resume_from_subscribe`) that closes `S+1..W-1`.
    #[must_use]
    pub const fn snapshot_seed_block(&self) -> Option<u64> {
        self.snapshot_seed_block
    }

    /// Family-dispatching reader for the V3/V4 concentrated-liquidity
    /// families. Returns a trait view over the shared read-only
    /// surface — the mutable scalars (`sqrt_price_x96`/`liquidity`/`tick`/
    /// `update_block`), the immutable fee/tick-spacing, and `tick_data`.
    ///
    /// This is the reader twin of the RAJ3PP apply dispatchers: the prior
    /// per-handle Python readers (`PyLiquidityPool.snapshot_v3`,
    /// `tick_data_snapshot`, the scalar getters, the restore/discard guards)
    /// went through `get_v3_pool`, which matches `PoolEntry::V3` only and
    /// returns `None` for `PoolEntry::V4` — silently yielding `None`/empty/0
    /// for every V4 read. Routing them through this accessor makes the
    /// docstrings' "V3/V4 pool" wording honest.
    ///
    /// Returns `None` for V2 or unregistered (the V3-only contract) — V2 has
    /// a different state shape and is read via the dedicated V2 getters.
    #[must_use]
    pub fn get_v3_or_v4_pool(&self, pool_id: u64) -> Option<&dyn ConcentratedLiquidityPool> {
        match self.pools.get(&pool_id)? {
            PoolEntry::V3(p) => Some(&p.1),
            PoolEntry::V4(p) => Some(&p.1),
            PoolEntry::V2(..)
            | PoolEntry::Curve(..)
            | PoolEntry::BalancerWeighted(..)
            | PoolEntry::BalancerStable(..)
            | PoolEntry::AerodromeV2(..) => None,
        }
    }

    /// Get the pool ID for a given contract address.
    #[must_use]
    pub fn pool_id_by_address(&self, address: &Address) -> Option<u64> {
        self.pool_addresses.get(address).copied()
    }

    /// The address-keyed registration of record, family-tagged (PRG-1 /
    /// registry unification). `BotState` is the sole pool registry,
    /// so the `PyO3` build adapters (`build_v2_pool` / `build_v3_pool` /
    /// `build_aerodrome_v2_pool` / `build_balancer_*_pool`) consult this
    /// pre-check INSTEAD of a Python-mirror registry: an address this core
    /// already registered is answered by this reader — identity straight off
    /// the registered entry — with no duplicate-handed builder replay and no
    /// terminal `AlreadyRegistered` refusal.
    ///
    /// V4 pools are NOT address-keyed (one `PoolManager` hosts many pool ids,
    /// keying is `(pool_manager, pool_id)`) and return `None` here; their
    /// fast path is the existing `try_registered_v4`.
    #[must_use]
    pub fn registered_pool_by_address(
        &self,
        address: &Address,
    ) -> Option<(u64, RegisteredPoolFamily)> {
        let pool_id = self.pool_id_by_address(address)?;
        let family = match self.pools.get(&pool_id)? {
            PoolEntry::V2(..) => RegisteredPoolFamily::V2,
            PoolEntry::V3(..) => RegisteredPoolFamily::V3,
            PoolEntry::Curve(..) => RegisteredPoolFamily::Curve,
            PoolEntry::BalancerWeighted(..) => RegisteredPoolFamily::BalancerWeighted,
            PoolEntry::BalancerStable(..) => RegisteredPoolFamily::BalancerStable,
            PoolEntry::AerodromeV2(..) => RegisteredPoolFamily::AerodromeV2,
            // V4 is (PoolManager, pool_id)-keyed, never address-keyed.
            PoolEntry::V4(..) => return None,
        };
        Some((pool_id, family))
    }

    /// Unregister a pool. ADR-007 U3.
    ///
    /// Drops the `PoolEntry` (and its reorg journal with it — restore for a
    /// removed pool is a no-op target) plus its index entries, and discards
    /// any buffered liquidity events for the pool so a re-register does not
    /// replay stale Mint/Burn/ModifyLiquidity onto the fresh pool.
    ///
    /// # Keying
    ///
    /// - **V2/V3 path** (`pool_id` = `None`): keyed by contract `address`
    ///   (`pool_addresses`).
    /// - **V4 path** (`pool_id` = `Some`): keyed by `(address, pool_id)` where
    ///   `address` is the **`PoolManager`** contract address (one `PoolManager`
    ///   hosts many pool ids — address alone is ambiguous, hence the tuple).
    ///
    /// `next_pool_id` is **not** reused — removed ids are retired so a stale
    /// `PyLiquidityPool` handle retained by a Python caller cannot alias onto
    /// a different pool that happens to receive the recycled id.
    ///
    /// # Returns
    ///
    /// `true` if an entry was found and removed; `false` if the address/tuple
    /// was never registered (silent no-op, mirroring Python `PoolRegistry.remove`
    /// silent-on-miss). Register stays refusal-on-`panic!`/`Err` (ADR-007 U2);
    /// the asymmetry reflects the asymmetry in the operations' invariants.
    pub fn unregister_pool(
        &mut self,
        address: Address,
        pool_id: Option<degenbot_decoders::v4_swap_decoder::V4PoolId>,
    ) -> bool {
        match pool_id {
            None => {
                // V2/V3 path: address-keyed.
                let Some(id) = self.pool_addresses.remove(&address) else {
                    return false;
                };
                self.pools.remove(&id);
                self.v3_buffer.discard_for(&address);
                true
            }
            Some(pid) => {
                // V4 path: (pool_manager, pool_id)-keyed.
                let key = (address, pid);
                let Some(id) = self.v4_pool_ids.remove(&key) else {
                    return false;
                };
                self.pools.remove(&id);
                self.v4_buffer.discard_for(&key);
                true
            }
        }
    }

    /// Number of registered pools.
    #[must_use]
    pub fn pool_count(&self) -> usize {
        self.pools.len()
    }

    /// Check if a pool ID is registered.
    #[must_use]
    pub fn has_pool(&self, pool_id: u64) -> bool {
        self.pools.contains_key(&pool_id)
    }

    /// Check if a token address is registered.
    #[must_use]
    pub fn has_token(&self, address: &Address) -> bool {
        self.tokens.contains_key(address)
    }

    /// Look up a registered token's metadata entry (address, name, symbol,
    /// decimals, `chain_id`) by contract address. Used by `PyErc20Token`'s getters
    /// (ADR-003 T3: Rust owns token identity metadata).
    #[must_use]
    pub fn token_entry(&self, address: &Address) -> Option<&TokenEntry> {
        self.tokens.get(address)
    }

    /// Get the number of deltas in the reorg journal for a V2 pool.
    ///
    /// Returns 0 if the pool ID is not registered.
    /// Restore **every** registered V2 pool's state to just before `target`.
    ///
    /// Bulk restore helper. ADR-006 slice 7 replaced the engine-level
    /// `handle_reorg` (which called this) with per-event
    /// `ReorgCoordinator::dispatch_reorg_log` (per-pool `restore_before_block`).
    /// This bulk helper survives as a `BotState` API (used by tests + available
    /// for ad-hoc bulk rollback); the engine no longer calls it on the hot path.
    ///
    /// Pools with no journal delta at/after `target` are left as-is (idempotent
    /// a reorg touches only a subset of pools). Returns the count of pools
    /// that were rolled back.
    pub fn restore_all_pools_before_block(&mut self, target: u64) -> usize {
        let pool_ids: Vec<u64> = self.pools.keys().copied().collect();
        let mut restored = 0usize;
        for pool_id in pool_ids {
            // Peek the per-pool newest delta block without a mutable borrow
            // (ADR-016). Only pools with a delta at/after the reorg target
            // need rollback; untouched pools keep their current state
            // (idempotent restore). The peek also guards the CL family's
            // panic-on-empty journal: an empty journal reports `None` → skip.
            let needs_restore = self
                .pools
                .get(&pool_id)
                .and_then(PoolEntry::as_reorg_state)
                .and_then(ReorgPoolState::newest_block)
                .is_some_and(|b| b >= target);
            if !needs_restore {
                continue;
            }

            // Dispatch through the unified trait path. On `Ok`, the trait
            // impl wrote the landed-at state into the struct's own fields; on
            // `Err` (target at/before registration), skip the pool
            // (idempotent — a reorg doesn't touch pools that didn't exist
            // before the fork target).
            let did_restore = self
                .restore_pool_before_block(pool_id, target)
                .is_some_and(|r| r.is_ok());
            if did_restore {
                restored += 1;
            }
        }
        restored
    }

    // --- Unified reorg dispatch (ADR-016 ReorgPoolState) ---
    // One trait-dispatching method per op over all 7 `PoolEntry` variants,
    // via `PoolEntry::as_reorg_state(_mut)`. The trait impls on each state
    // struct absorb the field-write; restore returns `()` so `V3RestoreResult`
    // and the per-family restore-return types stay internal to the impls and
    // never escape. These three methods replace the per-family `v2_*` /
    // `aerodrome_*` / `v3_*` / `v4_*` / `curve_*` / `balancer_weighted_*` /
    // `balancer_stable_*` reorg dispatchers.

    /// Restore `pool_id`'s state to the landed-at state strictly before
    /// `block`, dispatching through `ReorgPoolState::restore_before_block`.
    /// Returns `None` if the pool is not registered.
    ///
    /// A caller needing the restored values (the `PyO3` wrapper, which marshals
    /// a tuple to Python) reads the struct's current fields after restore —
    /// the post-restore fields ARE the landed-at (before) values the
    /// per-family return types previously carried.
    ///
    /// # Errors
    ///
    /// `NoStatePriorToBlock` if the target is at/before the registration
    /// (genesis) delta. The CL family's journal panics on empty instead —
    /// callers must pre-check [`has_state_prior_to`](Self::has_state_prior_to).
    pub fn restore_pool_before_block(
        &mut self,
        pool_id: u64,
        block: u64,
    ) -> Option<Result<(), JournalError>> {
        Some(
            self.pools
                .get_mut(&pool_id)?
                .as_reorg_state_mut()?
                .restore_before_block(block),
        )
    }

    /// Discard reorg journal deltas earlier than `block`, dispatching through
    /// `ReorgPoolState::discard_before_block`. Returns `None` if the pool is
    /// not registered. Does NOT mutate the live state fields (only trims old
    /// history).
    ///
    /// # Errors
    ///
    /// `NoStateAtOrAfterBlock` if the target is past the newest delta.
    pub fn discard_pool_before_block(
        &mut self,
        pool_id: u64,
        block: u64,
    ) -> Option<Result<(), JournalError>> {
        Some(
            self.pools
                .get_mut(&pool_id)?
                .as_reorg_state_mut()?
                .discard_before_block(block),
        )
    }

    /// Number of deltas in the reorg journal, dispatching through
    /// `ReorgPoolState::journal_len`. Returns `None` if the pool is not
    /// registered.
    #[must_use]
    pub fn pool_journal_len(&self, pool_id: u64) -> Option<usize> {
        Some(self.pools.get(&pool_id)?.as_reorg_state()?.journal_len())
    }

    // --- Aerodrome V2 journal + registration methods ---

    // --- V3 journal methods ---

    /// Get the number of deltas in the reorg journal for a V3 pool.
    ///
    /// Returns 0 if the pool ID is not registered or is not a V3 pool.
    /// Does `pool_id`'s journal have state at or before `block`? (ADR-006
    /// slice 7.) `false` → a too-deep reorg; `ReorgCoordinator` returns
    /// `Err(NoStatePriorToBlock)` and the pump shuts down gracefully.
    ///
    /// The predicate is **family-dependent** because the journals differ in
    /// whether they carry a genesis anchor:
    ///
    /// - **V2** carries a genesis delta (pushed at registration, `before ==
    ///   after`). There is genuinely no state *prior to* the genesis block, so
    ///   a target at or before the earliest (genesis) delta is too-deep:
    ///   `earliest < block`.
    /// - **V3/V4** push **no** genesis delta at registration. The "before"
    ///   values of the first forward event ARE the registration state, so
    ///   `restore_before_block(B)` handles a single delta at `B` (and any
    ///   target below the earliest delta) by popping down to registration
    ///   state. The ONLY unrecoverable case is an empty journal
    ///   (`restore_before_block` panics on empty) → `!is_empty()`.
    ///
    /// A pool whose newest delta is below `block` (idempotent no-op restore)
    /// returns `true` under both predicates.
    /// Peek the newest reorg-journal delta block for `pool_id` without
    /// mutating anything. `None` when unregistered or the journal is empty.
    /// Used by `ReorgCoordinator` to label idempotent no-op restores
    /// (newest delta strictly below the reorg target) in its
    /// `degenbot.reorg.restore` spans (WAJEQP T-R1).
    #[must_use]
    pub fn newest_journal_block(&self, pool_id: u64) -> Option<u64> {
        self.pools
            .get(&pool_id)
            .and_then(PoolEntry::as_reorg_state)
            .and_then(ReorgPoolState::newest_block)
    }

    #[must_use]
    pub fn has_state_prior_to(&self, pool_id: u64, block: u64) -> bool {
        let Some(entry) = self.pools.get(&pool_id) else {
            // Pool not registered → no journal → the reorg can't restore it.
            // Treat as "has state" (no-op) so the caller proceeds to the normal
            // pool-not-found no-op path rather than a fail-stop.
            return true;
        };
        match entry {
            PoolEntry::V2(p) => {
                p.1.journal
                    .earliest_block()
                    .is_some_and(|earliest| earliest < block)
            }
            // No genesis anchor — empty journal is the only too-deep case.
            PoolEntry::V3(p) => !p.1.journal.is_empty(),
            PoolEntry::V4(p) => !p.1.journal.is_empty(),
            // Curve carries a genesis delta (mirror of V2) — a target at/before
            // the genesis block is too-deep: `earliest < block`.
            PoolEntry::Curve(p) => {
                p.1.journal
                    .earliest_block()
                    .is_some_and(|earliest| earliest < block)
            }
            // Balancer weighted carries a genesis delta (mirror of V2/Curve) —
            // ADR-005 slice 12a. Same predicate: `earliest < block`.
            PoolEntry::BalancerWeighted(p) => {
                p.1.journal
                    .earliest_block()
                    .is_some_and(|earliest| earliest < block)
            }
            // Balancer stable carries a genesis delta (mirror of
            // V2/Curve/BalancerWeighted) — ADR-005 slice 12c. Same predicate:
            // `earliest < block`.
            PoolEntry::BalancerStable(p) => {
                p.1.journal
                    .earliest_block()
                    .is_some_and(|earliest| earliest < block)
            }
            // Aerodrome carries a genesis delta (mirror of V2/Curve/Balancer)
            // ADR-005 Aerodrome slice. Same predicate: `earliest < block`.
            PoolEntry::AerodromeV2(p) => {
                p.1.journal
                    .earliest_block()
                    .is_some_and(|earliest| earliest < block)
            }
        }
    }

    /// Encode a V2 swap call for the given pool.
    ///
    /// Produces pre-encoded calldata for `swap(uint256,uint256,address,bytes)`
    /// that is ready for on-chain submission.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeSwapError::NotRegistered`] for an unknown pool id,
    /// [`EncodeSwapError::UnsupportedFamily`] for a registered pool whose
    /// family has no swap encoder, and [`EncodeSwapError::Encode`] when the V2
    /// encoder rejects the call.
    pub fn encode_swap(
        &self,
        pool_id: u64,
        zero_for_one: bool,
        amount_out: U256,
        recipient: Address,
    ) -> Result<EncodedCall, EncodeSwapError> {
        let entry = self
            .pools
            .get(&pool_id)
            .ok_or(EncodeSwapError::NotRegistered { pool_id })?;
        match entry {
            PoolEntry::V2(p) => encode_v2_swap(p.0.address, zero_for_one, amount_out, recipient)
                .map_err(|cause| EncodeSwapError::Encode { pool_id, cause }),
            PoolEntry::V3(..)
            | PoolEntry::V4(..)
            | PoolEntry::Curve(..)
            | PoolEntry::BalancerWeighted(..)
            | PoolEntry::BalancerStable(..)
            | PoolEntry::AerodromeV2(..) => Err(EncodeSwapError::UnsupportedFamily {
                pool_id,
                family: self.pool_family(pool_id),
            }),
        }
    }

    // -----------------------------------------------------------------------
    // V4 state (ADR-003: single entry per `(pool_manager, pool_id)`;
    // orientation derived at solve from `zero_for_one`)
    // -----------------------------------------------------------------------

    /// Family-dispatching Swap apply. The single entry point
    /// `PyLiquidityPool.apply_swap` calls — routes V3 pools to
    /// `apply_v3_swap_by_pool_id` and V4 pools to
    /// `apply_v4_swap_by_pool_id`. V2/unregistered → `None` (no-op, matching
    /// the V3 sibling). This preserves the single Python `apply_swap` API
    /// while correcting the prior unconditional V3 routing that silently
    /// dropped every V4 update.
    ///
    /// The family probe is a `matches!` (Copy discriminant) so the immutable
    /// borrow of `self.pools` ends before the `&mut self` apply call — one
    /// held write guard throughout, two O(1) `HashMap` lookups (probe + apply).
    pub fn apply_swap_by_pool_id(
        &mut self,
        pool_id: u64,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)],
    ) -> Option<u64> {
        if matches!(self.pools.get(&pool_id), Some(PoolEntry::V4(..))) {
            self.apply_v4_swap_by_pool_id(
                pool_id,
                sqrt_price_x96,
                liquidity,
                tick,
                block_number,
                tick_priors,
            )
        } else {
            self.apply_v3_swap_by_pool_id(
                pool_id,
                sqrt_price_x96,
                liquidity,
                tick,
                block_number,
                tick_priors,
            )
        }
    }

    /// Registration/seed genesis anchor for a registered V3/V4 pool
    /// (two-stamp rule): pushes a `before == after` journal delta at `block`
    /// so the reorg journal is non-empty from registration, WITHOUT advancing
    /// either clock. The split-seed replacement for the builder's old
    /// `apply_swap` genesis, which would backward-panic `update_block` (price
    /// seeded at HEAD past the DB map block) and falsely advance
    /// `tick_data_block`. V2/unregistered → `None`.
    pub fn seed_genesis_by_pool_id(&mut self, pool_id: u64, block: u64) -> Option<u64> {
        match self.pools.get_mut(&pool_id) {
            Some(PoolEntry::V3(p)) => {
                p.1.seed_genesis(block);
                Some(pool_id)
            }
            Some(PoolEntry::V4(p)) => {
                p.1.seed_genesis(block);
                Some(pool_id)
            }
            _ => None,
        }
    }

    /// Family-dispatching liquidity update. The single entry point
    /// `PyLiquidityPool.apply_liquidity_update` calls — routes V3 to
    /// `apply_v3_liquidity_update_by_pool_id` and V4 to
    /// `apply_v4_liquidity_update_by_pool_id`. V2/unregistered → `None`.
    pub fn apply_liquidity_update_by_pool_id(
        &mut self,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) -> Option<u64> {
        if matches!(self.pools.get(&pool_id), Some(PoolEntry::V4(..))) {
            self.apply_v4_liquidity_update_by_pool_id(
                pool_id,
                tick_lower,
                tick_upper,
                liquidity_delta,
                block_number,
            )
        } else {
            self.apply_v3_liquidity_update_by_pool_id(
                pool_id,
                tick_lower,
                tick_upper,
                liquidity_delta,
                block_number,
            )
        }
    }

    /// solving (B3 move, FD7NFG). Applies each decoded event via the same
    /// `apply_v3_swap` / `buffer_backfill_*_liquidity_update` / `apply_v4_swap`
    /// path the live loop uses; decode selection lives in the dispatcher
    /// registry, never here. After the chunk,
    /// `expire_v3/v4_buffered(chunk_end)` advances the liquidity buffers. No
    /// `dispatch` / no solve cycle — the `Backfilled` phase invariant is
    /// "state advanced, no batches emitted".
    ///
    /// The engine-level entry is a thin delegator + `last_processed_block`
    /// stamp. `BotState` owns the state (ADR-003);
    /// `BlockPump::backfill_from_snapshot` (core) reaches it via `self.bot`.
    ///
    /// Decode selection is the dispatcher's registry
    /// (`log_dispatcher`, the same decoders the forward path routes through);
    /// `BotState` keeps apply/route only — the decoders are never imported
    /// here.
    pub fn process_backfill_logs(
        &mut self,
        dispatcher: &log_dispatcher::LogDispatcher,
        logs: &[alloy::rpc::types::Log],
        chunk_end: u64,
    ) {
        let mut v3_touched = false;
        let mut v4_touched = false;
        for log in logs {
            // The chunk-end fallback stamps a malformed log (no `block_number`)
            // at `chunk_end`, never block 0 (3ECKWX family). V2 Sync stays out
            // of backfill scope (CL-only: scalar state arrives via snapshot).
            let Some(event) = dispatcher.try_decode_log_with_block(log, chunk_end) else {
                continue;
            };
            v3_touched |= matches!(
                event,
                log_dispatcher::DecodedPoolEvent::V3Swap { .. }
                    | log_dispatcher::DecodedPoolEvent::V3Liquidity { .. }
            );
            v4_touched |= matches!(
                event,
                log_dispatcher::DecodedPoolEvent::V4Swap { .. }
                    | log_dispatcher::DecodedPoolEvent::V4Liquidity { .. }
            );
            let _ = event.apply_backfill(self);
        }
        if v3_touched {
            self.expire_v3_buffered(chunk_end);
        }
        if v4_touched {
            self.expire_v4_buffered(chunk_end);
        }
    }

    // --- V4 journal methods ---

    /// Register a token.
    ///
    /// Idempotent (35NMBX Guard 1 / concurrent registration workers): if the
    /// token address is already registered, the existing entry is canonical and
    /// this is a no-op (no panic). A sibling registration worker may insert the
    /// same token concurrently; racing inserts must not take the process down.
    pub fn register_token(
        &mut self,
        address: Address,
        name: String,
        symbol: String,
        decimals: u8,
        chain_id: u64,
    ) {
        self.tokens.entry(address).or_insert_with(|| TokenEntry {
            address,
            name,
            symbol,
            decimals,
            chain_id,
        });
    }
}

impl Default for BotState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bot — thin orchestrator facade (ADR-006 D4). Extracted to `bot.rs` (the
// lone ADR-006 D4 helper row not previously file-extracted; siblings
// `log_dispatcher`/`block_pump`/`solve_coordinator`/`reorg_coordinator`/...).
// Reachability path `degenbot_bot::bot_core::Bot` preserved by the re-export
// the 4 reachers (`block_pump`, `degenbot-python/bot/mod.rs`,
// `degenbot-python/bot/pump.rs` ×2) are byte-identical.
// ---------------------------------------------------------------------------
pub use bot::Bot;

/// Block metadata included in each `ResultBatch`.
///
/// Passed from the pump's WS block header into the drain tick, then forwarded
/// to Python via the result batch channel. Lives in `bot_core` (general block
/// data) so the `BlockPump` + `StageHandlers` seams stay in `bot_core` without a
/// reverse dependency on `solvers` (ADR-006 D4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockMetadata {
    /// Block timestamp
    pub timestamp: u64,
    /// Base fee per gas (None for pre-EIP-1559 blocks)
    pub base_fee_per_gas: Option<u64>,
    /// Gas used in this block
    pub gas_used: u64,
    /// Gas limit of this block
    pub gas_limit: u64,
}

#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]
#[cfg(test)]
mod tests;
