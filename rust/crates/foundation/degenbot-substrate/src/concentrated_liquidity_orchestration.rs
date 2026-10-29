//! Concentrated-liquidity (concentrated liquidity) structural family — orchestration capability (V3 + V4).
//!
//! This module owns the concentrated liquidity family's state surfaces — V3/V4 registration +
//! apply, the concentrated-liquidity-common dual liquidity buffer, the snapshot seeds, and the
//! coverage/quarantine/lifecycle state accessors — on [`ConcentratedLiquidityOrchestration`],
//! the capability struct `BotState` composes as its `concentrated_liquidity` field. The family
//! state types live in `degenbot-pools` (I/O-free, ADR-001), and the
//! `ConcentratedLiquidityPool(Mut)` trait is the concentrated liquidity family's unified seam.
//! The routing verbs the orchestration shares with the event pump live in
//! `concentrated_liquidity_route.rs`.
//!
//! The capability's own methods never touch `BotState`'s private fields: they reach the
//! family-agnostic pool table through the `PoolTable` parameter each caller
//! passes — a split borrow that lets one call mutate capability state and the
//! pool table together. `BotState`'s public concentrated-liquidity
//! surface lives here as two `impl BotState` blocks: the one-line delegation
//! wrappers over the capability (the composition root at the bottom of this
//! module), and the id-keyed apply doors — swap, genesis seed, liquidity update,
//! backfill replay — which resolve a `pool_id` to its family through
//! [`PoolLookup`] and so are the family dispatch itself rather than a
//! delegation.

use degenbot_core::diag;
use degenbot_core::op_warn;
use hashbrown::HashMap;

use ::degenbot_pools::families::concentrated_liquidity;
use ::degenbot_pools::{FamilyApply, Observe, Observed};

use alloy::primitives::{Address, U256};

use crate::concentrated_liquidity_route::{
    pin_provenance_verdict, route_action, ApplyOutcome, BufferKind, EventKind, Phase,
    PinProvenance, PoolPresence, RouteAction,
};
use crate::log_dispatcher;
use crate::pool_lookup::{entry_family, PoolLookup};
use degenbot_pools::{BufferedPoolEvent, PoolLiquidityUpdate};
use degenbot_pools::{BufferedV3LiquidityUpdate, BufferedV3SwapEvent, V3StateSync};
use degenbot_pools::{
    BufferedV4LiquidityUpdate, BufferedV4SwapEvent, V4PoolKey, V4StateSync, V4_DYNAMIC_FEE_FLAG,
};
use degenbot_pools::{
    PoolRefusal, PoolStateUpdate, TickWordMergeTarget, TickWordReader, WordFetchOutcome,
    WordFetchPolicy,
};
use degenbot_pools::{ScalarPriors, TickBefore, V3BlockDelta};

use super::apply_telemetry::{
    drain_dbg_log_buf, trace_apply_route_v3, trace_apply_route_v4, trace_apply_swap_v3,
    trace_apply_swap_v4,
};
use super::{
    BotState, BufferedV3PoolEvent, BufferedV4PoolEvent, ConcentratedLiquidityPoolMut, PoolEntry,
    PoolTickCoverage, RegisterV3PoolError, RegisterV3PoolParams, RegisterV4PoolError,
    RegisterV4PoolParams, RegisteredPoolFamily, RegistrationLifecycle, TickInfo, V3PoolState,
    V4PoolIdentity, V4PoolState, V4SwapUpdate,
};

use super::pool_table::{PoolClaim, PoolTable};

/// Why a concentrated-liquidity state write refused a pool.
///
/// Genesis seeding and liquidity updates only exist for concentrated liquidity families: a
/// registered non-concentrated-liquidity pool has no concentrated liquidity state to mutate. The typed refusal keeps
/// the family dispatch from silently routing such a pool into the V3 path.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConcentratedLiquidityApplyError {
    /// The pool id is not registered.
    #[error("pool {pool_id} is not registered")]
    NotRegistered {
        /// The requested pool id.
        pool_id: u64,
    },
    /// The pool's family has no concentrated-liquidity state.
    #[error("pool {pool_id} family {family:?} has no concentrated-liquidity {op}")]
    UnsupportedFamily {
        /// The requested pool id.
        pool_id: u64,
        /// The registered family tag.
        family: &'static str,
        /// The refused operation, named for the operator.
        op: &'static str,
    },
}

/// The concentrated-liquidity (V3 + V4) orchestration capability - the ONE
/// struct owning every concentrated-liquidity-local state surface the state owner carries: the
/// dual liquidity buffers, the `(pool_manager, pool_id)` V4 registry, the
/// state-view registry, the keyed registration gate, the snapshot seed
/// block, the pump delivery cutoff, and the per-pool event horizons. The
/// capability's method set (this module) reaches the registry core through
/// the `PoolTable` its callers pass; `BotState` remains the
/// composition root and delegates its public concentrated liquidity surface here.
pub struct ConcentratedLiquidityOrchestration {
    /// Dual-buffer for V3 liquidity (Mint/Burn) events awaiting pool
    /// registration (ADR-003: the accurate-state buffer lives on the state
    /// owner, not the dissolved `V3BlockEngine`).
    pub(crate) v3_buffer: ::degenbot_pools::LiquidityEventBuffer<Address, BufferedV3PoolEvent>,
    /// Dual-buffer for V4 `ModifyLiquidity` events awaiting pool registration.
    /// Keyed by `(pool_manager, pool_id)`.
    pub(crate) v4_buffer: ::degenbot_pools::LiquidityEventBuffer<
        (Address, degenbot_decoders::v4_swap_decoder::V4PoolId),
        BufferedV4PoolEvent,
    >,
    /// Rust-owned V4 pool-manager -> `StateView` registry (ADR-005 / Option 2).
    /// The canonical V4 scalar state is read via the `StateView`'s
    /// `getSlot0`/`getLiquidity`, not `getPool(poolManager)` (which reverts on
    /// the canonical deployment). Keyed by `pool_manager`; each `V4PoolState`
    /// under a manager shares its manager's `StateView`. Seeded once per manager
    /// via `register_v4_state_view` (the driver reads it from the
    /// `pool_managers` DB row); the solver-state verifier reads it via
    /// [`ConcentratedLiquidityOrchestration::state_view_for`].
    pub(crate) v4_state_views: HashMap<Address, Address>,
    /// PRG-2: the keyed registration-gate - immutable V4
    /// admission verdicts (dynamic fee / fee-exceeds-encoder-limit) recorded
    /// by [`BotState::register_v4_pool`] refusals and consulted pre-RPC by the
    /// `PyO3` build path. Bounded by refused pools, not candidates.
    pub(crate) registration_gate: crate::registration_gate::RegistrationGate,
    /// The snapshot seed block `S = min(fetch_newest_update_block(V3), V4)`.
    /// Set by `Bot::load_snapshot_from_db` (or `load_snapshot_from_py`) when a
    /// snapshot is loaded; consumed by the auto-backfill that
    /// closes the `S+1..W-1` gap before resume. `None` when no snapshot was
    /// loaded (cold-start path - the pump anchors on `first_observed_block`).
    pub(crate) snapshot_seed_block: Option<u64>,
    /// The highest FULLY-DELIVERED block - the delivery cutoff (last complete
    /// block). The registration drain reads this as the
    /// `drain_pump_completed` cutoff instead of a buffer-local shadow marker;
    /// `0` means no block has been tombstoned -> nothing drains. Owned here as
    /// a plain monotone value that outlives pump runs: the pump
    /// driver advances it on the tombstone verdict, and a resume never resets
    /// it.
    pub(crate) pump_complete_cutoff: u64,
    /// Per-pool event-witnessed horizon: the
    /// highest block of any V3/V4 event ROUTED for this pool (applied
    /// directly OR staged into a buffer). Advanced ONLY by routed events -
    /// never by imported DB-row stamps - so it corroborates (or refutes) a
    /// pin's freshness claim independently of the seed. Keyed like the
    /// family buffers: address for V3.
    pub(crate) v3_event_horizons: HashMap<Address, u64>,
    /// V4 twin of `v3_event_horizons`, keyed `(pool_manager, pool_id)`.
    pub(crate) v4_event_horizons:
        HashMap<(Address, degenbot_decoders::v4_swap_decoder::V4PoolId), u64>,
    /// The strict sparse-word apply gate stance (`pump.strict_sparse_apply`),
    /// minted from the installed config at construction. ON makes the routing
    /// table's direct-apply arm REFUSE a tick mutation whose boundary word the
    /// pool has not checked ([`ApplyOutcome::MissingTickWord`]) instead of
    /// applying over an unknown region; OFF keeps the lazy production posture
    /// (apply now, backfill the word at solve time).
    ///
    /// A per-instance field rather than a live config read: the config snapshot
    /// is process-wide while a state owner is not, and the stance must be
    /// pinned for the owner's lifetime (the same shape as
    /// `LogDispatcher::strict_decode_fault`).
    pub(crate) strict_sparse_apply: bool,
}

/// The family-specific half of an orchestration verb.
///
/// A concentrated-liquidity pool is addressed two ways and stores two different
/// state types, so a verb's *skeleton* (resolve the id, drain the retained
/// tail, apply each event, set the lifecycle) is shared while four steps are
/// not. Those four live here; the skeleton above them is written once.
///
/// This is the capability-level twin of `PoolLookup`: the seam decides *which*
/// family, the body stays single.
/// Why a drain stopped, for the drain-boundary diagnostic.
#[derive(Clone, Copy, Debug)]
pub(crate) enum DrainPhase {
    /// The pool is not registered (the buffered events have no home yet).
    NotRegistered,
    /// The family buffer had nothing for this key or phase.
    Empty,
    /// The pump has no tombstone cutoff yet, so no block is complete.
    NoCompleted,
    /// A drain happened; `drained` is how many events it yielded.
    Count,
}

pub(crate) trait LiquidityFamily {
    /// The pool's pre-registration key: its address, or its V4 `(manager, id)`.
    type Key: Copy + Eq + std::hash::Hash;
    /// The event this family's buffer holds.
    type Event: ::degenbot_pools::LiquidityEvent;
    /// This family's registered pool state.
    type State: RegistrationState;

    /// The identity this family's key claims — what the pool table resolves a
    /// registration and retires one by.
    fn lookup(key: &Self::Key) -> PoolLookup;

    /// Drain this family's retained pump tail for `key`.
    fn drain_pump(
        orch: &mut ConcentratedLiquidityOrchestration,
        key: &Self::Key,
    ) -> Option<Vec<Self::Event>>;

    /// Drain this family's backfill buffer for `key`.
    fn drain_backfill(
        orch: &mut ConcentratedLiquidityOrchestration,
        key: &Self::Key,
    ) -> Option<Vec<Self::Event>>;

    /// Drain the pump events whose block is complete (`<= cutoff`).
    fn drain_pump_completed(
        orch: &mut ConcentratedLiquidityOrchestration,
        key: &Self::Key,
        cutoff: u64,
    ) -> Option<Vec<Self::Event>>;

    /// The drain-boundary diagnostic (a lean family emits none).
    fn diag_drain(key: &Self::Key, verb: &'static str, phase: DrainPhase, drained: usize);

    /// The per-event drain diagnostic (a lean family emits none).
    fn diag_event(key: &Self::Key, verb: &'static str, event: &Self::Event);

    /// An `update_block` rewind after an apply (an apply must never move it back).
    fn diag_rewind(key: &Self::Key, before: u64, after: u64);

    /// This family's registered state for `pool_id`.
    fn state_mut(reg: &mut PoolTable, pool_id: u64) -> Option<&mut Self::State>;

    /// Apply one buffered event to a registered pool.
    fn apply(state: &mut Self::State, event: Self::Event);

    /// Discard any buffered events for a pool that is leaving the registry.
    fn discard_buffered(orch: &mut ConcentratedLiquidityOrchestration, key: &Self::Key);

    /// The wholesale state-sync payload this family accepts.
    type Sync;

    /// Overwrite the registered state from one source snapshot.
    fn apply_sync(state: &mut Self::State, update: Self::Sync);

    /// The engine-witnessed event horizon for `key` (0 when nothing witnessed).
    fn event_horizon(orch: &ConcentratedLiquidityOrchestration, key: &Self::Key) -> u64;

    /// How many buffered pump events sit at or below `block`.
    fn pump_count_at_or_below(
        orch: &ConcentratedLiquidityOrchestration,
        key: &Self::Key,
        block: u64,
    ) -> usize;

    /// The post-drain pin diagnostic and its stamp-provenance classification.
    fn diag_pin(
        key: &Self::Key,
        clock: u64,
        ticks: usize,
        undrained: usize,
        cutoff: u64,
        verdict: PinProvenance,
    );

    /// Emit the drain diagnostic (the field names are family-specific).
    fn diag(key: &Self::Key, verb: &str, blocks: &[u64], distinct_blocks: &[u64]);
}

/// The registration lifecycle both concentrated-liquidity state types carry.
///
/// Lets a shared verb body set the transition without knowing the family.
pub(crate) trait RegistrationState {
    /// The pool's registration lifecycle.
    fn lifecycle_mut(&mut self) -> &mut RegistrationLifecycle;
    /// The pool's tick coverage (a `Sparse` pool is never quarantined).
    fn coverage(&self) -> PoolTickCoverage;
    /// The block the pool's tick data reflects.
    fn update_block(&self) -> u64;
    /// The pinned snapshot seed (consumed once by step-1 verify).
    fn snapshot_seed_mut(&mut self) -> &mut Option<HashMap<i32, TickInfo>>;
    /// The pinned post-drain `(tick_data, block)` pair (consumed once by step-2 verify).
    fn post_drain_snapshot_mut(&mut self) -> &mut Option<(HashMap<i32, TickInfo>, u64)>;
    /// The block the pool's LIQUIDITY clock (its tick map) reflects.
    fn liquidity_clock(&self) -> u64;
    /// Freeze the post-drain `(tick_data, block)` pair; returns the tick count.
    fn pin_post_drain(&mut self, pinned_block: u64) -> usize;
}

impl RegistrationState for V3PoolState {
    fn lifecycle_mut(&mut self) -> &mut RegistrationLifecycle {
        &mut self.registration_lifecycle
    }

    fn coverage(&self) -> PoolTickCoverage {
        self.coverage
    }

    fn update_block(&self) -> u64 {
        self.update_block
    }

    fn snapshot_seed_mut(&mut self) -> &mut Option<HashMap<i32, TickInfo>> {
        &mut self.snapshot_seed
    }

    fn post_drain_snapshot_mut(&mut self) -> &mut Option<(HashMap<i32, TickInfo>, u64)> {
        &mut self.post_drain_snapshot
    }

    fn liquidity_clock(&self) -> u64 {
        self.tick_data_block
    }

    fn pin_post_drain(&mut self, pinned_block: u64) -> usize {
        self.post_drain_snapshot = Some((self.tick_data.clone(), pinned_block));
        self.tick_data.len()
    }
}

impl RegistrationState for V4PoolState {
    fn lifecycle_mut(&mut self) -> &mut RegistrationLifecycle {
        &mut self.registration_lifecycle
    }

    fn coverage(&self) -> PoolTickCoverage {
        self.coverage
    }

    fn update_block(&self) -> u64 {
        self.update_block
    }

    fn snapshot_seed_mut(&mut self) -> &mut Option<HashMap<i32, TickInfo>> {
        &mut self.snapshot_seed
    }

    fn post_drain_snapshot_mut(&mut self) -> &mut Option<(HashMap<i32, TickInfo>, u64)> {
        &mut self.post_drain_snapshot
    }

    fn liquidity_clock(&self) -> u64 {
        self.tick_data_block
    }

    fn pin_post_drain(&mut self, pinned_block: u64) -> usize {
        self.post_drain_snapshot = Some((self.tick_data.clone(), pinned_block));
        self.tick_data.len()
    }
}

/// The address-keyed (V3) family.
pub(crate) struct V3Family;

impl LiquidityFamily for V3Family {
    type Key = Address;
    type Event = BufferedV3PoolEvent;
    type State = V3PoolState;

    fn lookup(key: &Self::Key) -> PoolLookup {
        PoolLookup::from_address(*key)
    }

    fn drain_pump(
        orch: &mut ConcentratedLiquidityOrchestration,
        key: &Self::Key,
    ) -> Option<Vec<Self::Event>> {
        orch.v3_buffer.drain_pump(key)
    }

    fn state_mut(reg: &mut PoolTable, pool_id: u64) -> Option<&mut Self::State> {
        reg.get_mut(pool_id)
            .and_then(PoolEntry::v3_mut)
            .map(|(_, state)| state)
    }

    fn apply(state: &mut Self::State, event: Self::Event) {
        ConcentratedLiquidityOrchestration::apply_buffered_v3_event(state, event);
    }

    fn discard_buffered(orch: &mut ConcentratedLiquidityOrchestration, key: &Self::Key) {
        orch.v3_buffer.discard_for(key);
    }

    type Sync = V3StateSync;

    fn apply_sync(state: &mut Self::State, update: Self::Sync) {
        state.sqrt_price_x96 = update.sqrt_price_x96;
        state.liquidity = update.liquidity;
        state.tick = update.tick;
        state.tick_data = update.tick_data;
        // A wholesale sync is a sanctioned reset: both clocks move to the
        // snapshot's block together.
        state.update_block = update.update_block;
        state.tick_data_block = update.update_block;
        state.invalidate_tick_range_cache();
    }

    fn drain_backfill(
        orch: &mut ConcentratedLiquidityOrchestration,
        key: &Self::Key,
    ) -> Option<Vec<Self::Event>> {
        orch.v3_buffer.drain_backfill(key)
    }

    fn drain_pump_completed(
        orch: &mut ConcentratedLiquidityOrchestration,
        key: &Self::Key,
        cutoff: u64,
    ) -> Option<Vec<Self::Event>> {
        orch.v3_buffer.drain_pump_completed(key, cutoff)
    }

    fn diag_drain(key: &Self::Key, verb: &'static str, phase: DrainPhase, drained: usize) {
        match phase {
            DrainPhase::NotRegistered => {
                diag!(domain = pump, pool_addr = %format!("{key:x}"), "NOT REGISTERED");
            }
            DrainPhase::Empty => {
                diag!(domain = pump, pool_addr = %format!("{key:x}"), verb = %verb, "EMPTY");
            }
            DrainPhase::NoCompleted => {
                diag!(domain = pump, pool_addr = %format!("{key:x}"),
                    verb = %verb,
                    "NO-COMPLETE (no tombstone yet)"
                );
            }
            DrainPhase::Count => {
                diag!(domain = pump, pool_addr = %format!("{key:x}"), count = drained, verb = %verb);
            }
        }
    }

    fn diag_event(key: &Self::Key, verb: &'static str, event: &Self::Event) {
        let label = if verb == "backfill" {
            "backfill apply"
        } else {
            "pump apply"
        };
        match event {
            BufferedV3PoolEvent::Liquidity(u) => {
                diag!(domain = pump, pool_addr = %format!("{key:x}"),
                    kind = %format!("{label} liq"),
                    tick_lower = u.tick_lower,
                    tick_upper = u.tick_upper,
                    delta = u.liquidity_delta,
                    block = u.block_number
                );
            }
            BufferedV3PoolEvent::Swap(s) => {
                diag!(domain = pump, pool_addr = %format!("{key:x}"),
                    kind = %format!("{label} swap"),
                    liquidity = s.liquidity,
                    tick = s.tick,
                    block = s.block_number
                );
            }
        }
    }

    fn diag_rewind(key: &Self::Key, before: u64, after: u64) {
        op_warn!(domain = state, pool_addr = %format!("{key:x}"),
            ub_before = before,
            ub_after = after,
            "update_block REWIND"
        );
    }

    fn event_horizon(orch: &ConcentratedLiquidityOrchestration, key: &Self::Key) -> u64 {
        orch.event_horizon(key)
    }

    fn pump_count_at_or_below(
        orch: &ConcentratedLiquidityOrchestration,
        key: &Self::Key,
        block: u64,
    ) -> usize {
        orch.v3_buffer.pump_count_at_or_below(key, block)
    }

    fn diag_pin(
        key: &Self::Key,
        clock: u64,
        ticks: usize,
        undrained: usize,
        cutoff: u64,
        verdict: PinProvenance,
    ) {
        diag!(domain = verify, pool_addr = %format!("{key:x}"),
            tick_data_block = clock,
            tick_count = ticks,
            pump_count = undrained,
            last_complete_block = cutoff,
            "V3 pin"
        );
        match verdict {
            PinProvenance::SeedTrustOnly { witnessed_horizon } if witnessed_horizon > 0 => {
                op_warn!(domain = state, pool_addr = %format!("{key:x}"),
                    seed_block = clock,
                    cutoff,
                    witnessed_horizon,
                    "V3 re-seed-after-activity: a fresher seed stamp arrived after the engine had already witnessed events for this pool (the mis-stamped lie shape)"
                );
            }
            PinProvenance::CorroboratedByDelivery
            | PinProvenance::WitnessedBeyondCutoff
            | PinProvenance::SeedTrustOnly { .. } => {
                diag!(domain = state, pool_addr = %format!("{key:x}"),
                    seed_block = clock,
                    cutoff,
                    verdict = ?verdict,
                    "V3 pin stamp classified"
                );
            }
        }
    }

    fn diag(key: &Self::Key, verb: &str, blocks: &[u64], distinct_blocks: &[u64]) {
        diag!(domain = verify, pool_addr = %format!("{key:x}"),
            verb = %verb,
            blocks = ?blocks,
            distinct_blocks = ?distinct_blocks,
            "V3 buffered drain"
        );
    }
}

/// The V4 `(pool_manager, pool_id)`-keyed family.
pub(crate) struct V4Family;

impl LiquidityFamily for V4Family {
    type Key = (Address, degenbot_decoders::v4_swap_decoder::V4PoolId);
    type Event = BufferedV4PoolEvent;
    type State = V4PoolState;

    fn lookup(key: &Self::Key) -> PoolLookup {
        PoolLookup::V4 {
            pool_manager: key.0,
            pool_id: key.1,
        }
    }

    fn drain_pump(
        orch: &mut ConcentratedLiquidityOrchestration,
        key: &Self::Key,
    ) -> Option<Vec<Self::Event>> {
        orch.v4_buffer.drain_pump(key)
    }

    fn state_mut(reg: &mut PoolTable, pool_id: u64) -> Option<&mut Self::State> {
        reg.get_mut(pool_id)
            .and_then(PoolEntry::v4_mut)
            .map(|(_, state)| state)
    }

    fn apply(state: &mut Self::State, event: Self::Event) {
        ConcentratedLiquidityOrchestration::apply_buffered_v4_event(state, event);
    }

    fn discard_buffered(orch: &mut ConcentratedLiquidityOrchestration, key: &Self::Key) {
        orch.v4_buffer.discard_for(key);
    }

    type Sync = V4StateSync;

    fn apply_sync(state: &mut Self::State, update: Self::Sync) {
        state.sqrt_price_x96 = update.sqrt_price_x96;
        state.liquidity = update.liquidity;
        state.tick = update.tick;
        state.tick_data = update.tick_data;
        // A wholesale sync is a sanctioned reset: both clocks move to the
        // snapshot's block together.
        state.update_block = update.update_block;
        state.tick_data_block = update.update_block;
        state.invalidate_tick_range_cache();
    }

    fn drain_backfill(
        orch: &mut ConcentratedLiquidityOrchestration,
        key: &Self::Key,
    ) -> Option<Vec<Self::Event>> {
        orch.v4_buffer.drain_backfill(key)
    }

    fn drain_pump_completed(
        orch: &mut ConcentratedLiquidityOrchestration,
        key: &Self::Key,
        cutoff: u64,
    ) -> Option<Vec<Self::Event>> {
        orch.v4_buffer.drain_pump_completed(key, cutoff)
    }

    fn diag_drain(_key: &Self::Key, _verb: &'static str, _phase: DrainPhase, _drained: usize) {}

    fn diag_event(_key: &Self::Key, _verb: &'static str, _event: &Self::Event) {}

    fn diag_rewind(_key: &Self::Key, _before: u64, _after: u64) {}

    fn event_horizon(orch: &ConcentratedLiquidityOrchestration, key: &Self::Key) -> u64 {
        orch.event_horizon_by_key(key)
    }

    fn pump_count_at_or_below(
        orch: &ConcentratedLiquidityOrchestration,
        key: &Self::Key,
        block: u64,
    ) -> usize {
        orch.v4_buffer.pump_count_at_or_below(key, block)
    }

    fn diag_pin(
        key: &Self::Key,
        clock: u64,
        ticks: usize,
        undrained: usize,
        cutoff: u64,
        verdict: PinProvenance,
    ) {
        diag!(domain = verify, pool_manager = %format!("{:x}", key.0),
            pool_id = %alloy::hex::encode_prefixed(key.1),
            tick_data_block = clock,
            tick_count = ticks,
            pump_count = undrained,
            last_complete_block = cutoff,
            "V4 pin"
        );
        match verdict {
            PinProvenance::SeedTrustOnly { witnessed_horizon } if witnessed_horizon > 0 => {
                op_warn!(domain = state, pool_manager = %format!("{:x}", key.0),
                    pool_id = %alloy::hex::encode_prefixed(key.1),
                    seed_block = clock,
                    cutoff,
                    witnessed_horizon,
                    "V4 re-seed-after-activity: a fresher seed stamp arrived after the engine had already witnessed events for this pool (the mis-stamped lie shape)"
                );
            }
            PinProvenance::CorroboratedByDelivery
            | PinProvenance::WitnessedBeyondCutoff
            | PinProvenance::SeedTrustOnly { .. } => {
                diag!(domain = state, pool_manager = %format!("{:x}", key.0),
                    pool_id = %alloy::hex::encode_prefixed(key.1),
                    seed_block = clock,
                    cutoff,
                    verdict = ?verdict,
                    "V4 pin stamp classified"
                );
            }
        }
    }

    fn diag(key: &Self::Key, verb: &str, blocks: &[u64], distinct_blocks: &[u64]) {
        diag!(domain = verify, pool_manager = %format!("{:x}", key.0),
            pool_id = %alloy::hex::encode_prefixed(key.1),
            verb = %verb,
            blocks = ?blocks,
            distinct_blocks = ?distinct_blocks,
            "V4 buffered drain"
        );
    }
}

impl ConcentratedLiquidityOrchestration {
    /// An empty capability - every buffer, view, gate, and horizon starts
    /// unseeded; `BotState::with_journal_depth` composes it beside the pool
    /// table.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            v3_buffer: ::degenbot_pools::LiquidityEventBuffer::new(),
            v4_buffer: ::degenbot_pools::LiquidityEventBuffer::new(),
            v4_state_views: HashMap::new(),
            registration_gate: crate::registration_gate::RegistrationGate::default(),
            snapshot_seed_block: None,
            pump_complete_cutoff: 0,
            v3_event_horizons: HashMap::new(),
            v4_event_horizons: HashMap::new(),
            strict_sparse_apply: crate::stance::config().pump.strict_sparse_apply,
        }
    }

    /// Arm or disarm the strict sparse-word apply gate for this capability
    /// (`pump.strict_sparse_apply`; the construction default comes from the
    /// installed config).
    pub(crate) fn set_strict_sparse_apply(&mut self, on: bool) {
        self.strict_sparse_apply = on;
    }
}

/// The staged fetch plan a caller holds across the lock-free window: the pool it
/// backfills, plus the tick-word capability's staged plan (the read handle, the
/// word, the fetch block, and the pool's tick fingerprint the install
/// re-validates). The pool id stays substrate vocabulary — the capability owns
/// no pool identity — while the choreography itself is
/// [`degenbot_pools::stage_word_fetch`].
#[derive(Debug)]
pub struct StagedWordFetch {
    pool_id: u64,
    staged: ::degenbot_pools::StagedWordFetch,
}

impl StagedWordFetch {
    /// The pool this fetch backfills.
    #[must_use]
    pub fn pool_id(&self) -> u64 {
        self.pool_id
    }

    /// The word being fetched.
    #[must_use]
    pub fn word(&self) -> i32 {
        self.staged.word()
    }

    /// The block this fetch reads at.
    #[must_use]
    pub fn block(&self) -> u64 {
        self.staged.block()
    }

    /// Run the fetch WITHOUT any state lock held. The stored fetcher re-enters
    /// Python (`Python::attach` + the companion's web3 RPC), so this call is the
    /// multi-second window the fetch-under-write defect parked the whole pump
    /// inside.
    ///
    /// # Errors
    /// Propagates the stored source's [`degenbot_pools::TickWordFetchError`]
    /// (RPC/transport or tick-word decode); the caller owns retry semantics,
    /// nothing panics.
    pub fn fetch(
        &self,
    ) -> Result<::degenbot_pools::CheckedWord, ::degenbot_pools::TickWordFetchError> {
        self.staged.fetch()
    }
}

/// The pool's price clock (`update_block`), or `None` when the id names no
/// concentrated-liquidity pool — the retried fetch context re-derives from it.
fn pool_clock(reg: &PoolTable, pool_id: u64) -> Option<u64> {
    let entry = reg.get(pool_id)?;
    entry
        .v3()
        .map(|(_, state)| state.update_block)
        .or_else(|| entry.v4().map(|(_, state)| state.update_block))
}

#[expect(
    clippy::unused_self,
    reason = "capability methods operate on a caller-owned PoolTable threaded as an explicit parameter; converting them to associated functions churns the whole composition seam"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "event-application signatures mirror decoder payload fields; a params-struct refactor is out of scope"
)]
impl ConcentratedLiquidityOrchestration {
    /// Register a V3 pool by contract address.
    ///
    /// Returns the auto-assigned pool ID.
    ///
    /// # Errors
    ///
    /// Returns [`RegisterV3PoolError::AlreadyRegistered`] if a pool at this
    /// address is already registered (replaces the prior `assert!` panic).
    ///
    /// Returns [`RegisterV3PoolError::SpecViolation`] when `sqrt_price_x96`,
    /// `tick`, `fee`, or `tick_spacing` violates its Solidity-bounded on-chain
    /// invariant (see [`spec_bounds`]). These checks fire *before* the
    /// registration-time tick-data seeding (the Db arm of `assemble_*_tick_map`
    /// supplies `tick_data`/`coverage` via the held snapshot tx) + never touch
    /// the immutable config / current state scalars under validation here.
    pub(crate) fn register_v3_pool(
        &mut self,
        reg: &mut PoolTable,
        params: &RegisterV3PoolParams,
    ) -> Result<u64, RegisterV3PoolError> {
        use ::degenbot_pools::state::spec_bounds as sb;
        sb::validate_sqrt_price(params.sqrt_price_x96)
            .map_err(RegisterV3PoolError::SpecViolation)?;
        sb::validate_tick(params.tick).map_err(RegisterV3PoolError::SpecViolation)?;
        sb::validate_v3_fee(params.fee).map_err(RegisterV3PoolError::SpecViolation)?;
        sb::validate_tick_spacing(params.tick_spacing)
            .map_err(RegisterV3PoolError::SpecViolation)?;

        // [diag] registration-seed probe: log every V3 pool's seed scalar state
        // (update_block + sqrtPriceX96 + tick) so a solver-state mismatch can be
        // traced to its seed. Always-on DEBUG on the `state` domain (the
        // the register-seed probe gate is retired). A pool seeded with an
        // `update_block` well behind the head + an old sqrt is the stale-seed
        // hypothesis; a head-fresh seed points the finger at a post-registration
        // rewind instead.
        diag!(domain = path, pool_addr = %format!("{:x}", params.address),
            family = "V3",
            seed_update_block = params.update_block,
            seed_sqrt = %params.sqrt_price_x96,
            seed_tick = params.tick,
            coverage = ?params.coverage,
            "register-v3-seed"
        );

        let address = params.address;

        // the `seed_from_store` path is retired — the DB
        // seeding is handled by the Db arm of `assemble_v3_tick_map` (held
        // snapshot tx). Just clone + flow the params through.
        let params = params.clone();
        let (identity, state) = V3PoolState::from_params(params, reg.journal_depth());
        reg.register(
            PoolClaim::new(PoolLookup::Address(address), RegisteredPoolFamily::V3),
            PoolEntry::V3(Box::new((identity, state))),
        )
        .pool_id()
        .ok_or(RegisterV3PoolError::AlreadyRegistered { address })
    }
    /// Update a V3 pool's state from a Swap event.
    ///
    /// Looks up the pool by contract address. No-op if the pool is not registered.
    /// Stashes scalar "before" values (and any provided per-tick priors) in the
    /// reorg journal before updating. Kept as the `PyBot` entry; the live
    /// pump path uses [`BotState::apply_swap`] (which returns the
    /// affected `pool_id` and overlays `tick_priors` into `tick_data`).
    pub(crate) fn update_v3_pool(
        &mut self,
        reg: &mut PoolTable,
        pool_address: Address,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: Vec<(i32, TickBefore)>,
    ) {
        let Some(pool_id) = reg.pool_id_by_address(&pool_address) else {
            return;
        };

        let Some(state) = reg
            .get_mut(pool_id)
            .and_then(PoolEntry::v3_mut)
            .map(|(_, s)| s)
        else {
            return;
        };

        // A Swap rewrites the slot0 head AND crosses ticks: it advances BOTH
        // clocks, so record both pre-event clock values for reorg restore, then
        // advance both monotonic (two-stamp pool state). A backward
        // stamp outside a reorg panics via the advance helpers.
        let update_block_before = state.update_block;
        let tick_data_block_before = state.tick_data_block;
        // Stash "before" values in the reorg journal before updating
        state.journal.push_delta(V3BlockDelta {
            block: block_number,
            scalar_priors: Some(ScalarPriors {
                sqrt_price_x96_before: state.sqrt_price_x96,
                liquidity_before: state.liquidity,
                tick_before: state.tick,
            }),
            update_block_before: Some(update_block_before),
            tick_data_block_before: Some(tick_data_block_before),
            tick_priors,
        });

        state.sqrt_price_x96 = sqrt_price_x96;
        state.liquidity = liquidity;
        state.tick = tick;
        state.advance_update_block(block_number);
        state.advance_tick_data_block(block_number);
        state.invalidate_tick_range_cache();
    }
    /// Event-witnessed horizon for an address-registered (V3) pool:
    /// highest block of any routed event for this pool. `0` = none witnessed.
    #[must_use]
    pub(crate) fn event_horizon(&self, pool_address: &Address) -> u64 {
        self.v3_event_horizons
            .get(pool_address)
            .copied()
            .unwrap_or(0)
    }
    fn note_v3_event_block(&mut self, pool_address: Address, block_number: u64) {
        let e = self.v3_event_horizons.entry(pool_address).or_insert(0);
        *e = (*e).max(block_number);
    }
    /// Event-witnessed horizon for a key-registered (V4) pool.
    #[must_use]
    pub(crate) fn event_horizon_by_key(
        &self,
        key: &(Address, degenbot_decoders::v4_swap_decoder::V4PoolId),
    ) -> u64 {
        self.v4_event_horizons.get(key).copied().unwrap_or(0)
    }
    fn note_v4_event_block(
        &mut self,
        key: (Address, degenbot_decoders::v4_swap_decoder::V4PoolId),
        block_number: u64,
    ) {
        let e = self.v4_event_horizons.entry(key).or_insert(0);
        *e = (*e).max(block_number);
    }
    /// Resolve a V3 pool's routing presence (`concentrated_liquidity_route` axis),
    /// keyed by registry pool address.
    fn presence_by_address(&self, reg: &PoolTable, pool_address: &Address) -> PoolPresence {
        let lifecycle = reg
            .pool_id_by_address(pool_address)
            .and_then(|id| match reg.get(id) {
                Some(PoolEntry::V3(p)) => Some(p.1.registration_lifecycle),
                _ => None,
            });
        PoolPresence::from_lifecycle(lifecycle)
    }
    /// The strict sparse-word gate's predicate for one tick mutation: the first
    /// boundary word this pool has not checked, or `None` when the gate is
    /// disarmed (`pump.strict_sparse_apply` off), every boundary word is known,
    /// or the pool is `Tracked`/not concentrated liquidity.
    ///
    /// Refusing beats applying: the event's boundary ticks would journal priors
    /// for a region whose other initialized ticks are absent, so a reorg
    /// rollback would restore a state the chain never had.
    fn sparse_gate_word(
        &self,
        reg: &PoolTable,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
    ) -> Option<i32> {
        if !self.strict_sparse_apply {
            return None;
        }
        match reg.get(pool_id)? {
            PoolEntry::V3(p) => {
                p.1.unknown_boundary_word(tick_lower, tick_upper, p.0.tick_spacing)
            }
            PoolEntry::V4(p) => {
                p.1.unknown_boundary_word(tick_lower, tick_upper, p.0.pool_key.tick_spacing)
            }
            PoolEntry::AerodromeV2(..)
            | PoolEntry::Curve(..)
            | PoolEntry::BalancerWeighted(..)
            | PoolEntry::BalancerStable(..)
            | PoolEntry::V2(..) => None,
        }
    }

    /// One direct-apply tick mutation onto a registered pool: the strict
    /// sparse-word gate first (see [`Self::sparse_gate_word`]), then the
    /// family's `apply_liquidity_update`. `Some(refusal)` when the gate holds —
    /// the caller backfills the word and re-routes; `None` when it applied.
    fn apply_tick_mutation(
        &mut self,
        reg: &mut PoolTable,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) -> Option<ApplyOutcome> {
        if let Some(word) = self.sparse_gate_word(reg, pool_id, tick_lower, tick_upper) {
            return Some(ApplyOutcome::MissingTickWord { pool_id, word });
        }
        match reg.get_mut(pool_id) {
            Some(PoolEntry::V3(p)) => {
                p.1.apply_liquidity_update(tick_lower, tick_upper, liquidity_delta, block_number);
            }
            Some(PoolEntry::V4(p)) => {
                p.1.apply_liquidity_update(tick_lower, tick_upper, liquidity_delta, block_number);
            }
            _ => {}
        }
        None
    }

    /// THE single decision point for a decoded V3 event (`concentrated_liquidity_route` table).
    ///
    /// Every production entry point — live dispatch, snapshot-gap backfill,
    /// pyo3 staged appliers — must ask this function instead of embedding its
    /// own copy of the policy (three divergent routers each knew a
    /// subset; the funnel's "unregistered implies drop" inference silently lost
    /// live Mints). Callers execute the returned outcome; they do not pre-judge.
    ///
    /// `swap_priors` overlays tick priors onto DIRECT swap application only
    /// (pump/backfill paths pass an empty slice; buffered events never carry
    /// priors).
    pub(crate) fn route_event(
        &mut self,
        reg: &mut PoolTable,
        phase: Phase,
        pool_address: Address,
        event: BufferedV3PoolEvent,
        swap_priors: &[(i32, TickInfo)],
    ) -> ApplyOutcome {
        let kind = match &event {
            BufferedV3PoolEvent::Swap(_) => EventKind::ScalarRefresh,
            BufferedV3PoolEvent::Liquidity(_) => EventKind::TickMutation,
        };
        let presence = self.presence_by_address(reg, &pool_address);
        let event_block = match &event {
            BufferedV3PoolEvent::Swap(s) => s.block_number,
            BufferedV3PoolEvent::Liquidity(l) => l.block_number,
        };
        let action = route_action(phase, presence, kind);
        match action {
            RouteAction::ApplyDirect => {
                // Table invariant: ApplyDirect implies registered. Defensive
                // fallback (never expected) degrades to a named no-op rather
                // than panicking inside the hot dispatch path.
                let Some(pool_id) = reg.pool_id_by_address(&pool_address) else {
                    debug_assert!(
                        false,
                        "ApplyDirect for an unregistered pool — routing table invariant violated"
                    );
                    return ApplyOutcome::Buffered(BufferKind::Pump);
                };
                match event {
                    BufferedV3PoolEvent::Swap(BufferedV3SwapEvent {
                        sqrt_price_x96,
                        liquidity,
                        tick,
                        block_number,
                    }) => {
                        if let Some(PoolEntry::V3(p)) = reg.get_mut(pool_id) {
                            p.1.apply_swap(
                                sqrt_price_x96,
                                liquidity,
                                tick,
                                block_number,
                                swap_priors,
                            );
                        }
                    }
                    BufferedV3PoolEvent::Liquidity(BufferedV3LiquidityUpdate {
                        tick_lower,
                        tick_upper,
                        liquidity_delta,
                        block_number,
                    }) => {
                        if let Some(refusal) = self.apply_tick_mutation(
                            reg,
                            pool_id,
                            tick_lower,
                            tick_upper,
                            liquidity_delta,
                            block_number,
                        ) {
                            return refusal;
                        }
                    }
                }
                self.note_v3_event_block(pool_address, event_block);
                ApplyOutcome::Applied(pool_id)
            }
            RouteAction::Buffer(kind) => {
                // Preserve historical live-path route traces (grep compat).
                if phase == Phase::Live && presence != PoolPresence::Live {
                    if let BufferedV3PoolEvent::Liquidity(u) = &event {
                        let label = match presence {
                            PoolPresence::Unregistered => "none",
                            _ => "Quarantined",
                        };
                        let route = match presence {
                            PoolPresence::Unregistered => "buffer-pump",
                            _ => "buffer-pump-quarantined",
                        };
                        trace_apply_route_v3(
                            pool_address,
                            u.tick_lower,
                            u.tick_upper,
                            u.liquidity_delta,
                            u.block_number,
                            label,
                            route,
                        );
                        drain_dbg_log_buf(
                            pool_address,
                            if presence == PoolPresence::Unregistered {
                                'L'
                            } else {
                                'Q'
                            },
                            u.tick_lower,
                            u.tick_upper,
                            u.liquidity_delta,
                            u.block_number,
                        );
                    }
                }
                match kind {
                    BufferKind::Backfill => self.v3_buffer.buffer_backfill(pool_address, event),
                    BufferKind::Pump => self.v3_buffer.buffer_pump(pool_address, event),
                }
                // Stamp provenance: a buffered event is still
                // engine-witnessed activity for this pool — advance the
                // event horizon at arrival time so the pin's stamp-provenance
                // verdict sees the true witnessed span (parity with V4).
                self.note_v3_event_block(pool_address, event_block);
                ApplyOutcome::Buffered(kind)
            }
            RouteAction::Drop(reason) => ApplyOutcome::NoOp(reason),
        }
    }
    /// Full-sync a V3/V4 pool's `tick_data` from an external source (Python
    /// sparse-map backfill). Replaces the entire `tick_data` map; keeps the
    /// scalars (`sqrt_price_x96`/`liquidity`/`tick`) unchanged; advances
    /// `update_block` if `update_block` is newer (monotonic — no rewind).
    /// No journal delta (a wholesale replace has undefined rollback semantics;
    /// the pump is the authority for event-derived ticks — mirrors
    /// `sync_v3_pool_state`). Returns `false` for V2 / unregistered (mirrors
    /// the apply dispatchers' silent no-op contract).
    ///
    /// The pool_id-keyed twin of `sync_v3_pool_state` (address-keyed): the
    /// `PyLiquidityPool` handle holds the canonical `pool_id`, so this is the
    /// one-lock, one-lookup path. Family-agnostic (V3 + V4) — both store an
    /// identical `tick_data: HashMap<i32, TickInfo>`.
    #[must_use]
    pub(crate) fn sync_tick_data_by_pool_id(
        &mut self,
        reg: &mut PoolTable,
        pool_id: u64,
        tick_data: HashMap<i32, TickInfo>,
        update_block: u64,
    ) -> bool {
        let Some(entry) = reg.get_mut(pool_id) else {
            return false;
        };
        match entry {
            // concentrated-liquidity-family collapse (ADR-014 D2b): the 4-line replace body lives
            // once in `ConcentratedLiquidityPoolMut::replace_tick_data`; each
            // arm only reads its identity's `tick_spacing` (V3 carries it
            // directly, V4 nests it in `pool_key`) and delegates. The
            // `_ => false` arm is the single non-concentrated-liquidity / unregistered no-op.
            PoolEntry::V3(p) => {
                p.1.replace_tick_data(tick_data, update_block, p.0.tick_spacing)
            }
            PoolEntry::V4(p) => {
                p.1.replace_tick_data(tick_data, update_block, p.0.pool_key.tick_spacing)
            }
            PoolEntry::V2(..)
            | PoolEntry::Curve(..)
            | PoolEntry::BalancerWeighted(..)
            | PoolEntry::BalancerStable(..)
            | PoolEntry::AerodromeV2(..) => false,
        }
    }
    /// Record `words` as known on the concentrated liquidity pool behind `pool_id` — the
    /// write-path twin of [`Self::sync_tick_data_by_pool_id`] (arch-review
    /// cand 4, T1). Sparse pools only: the trait writer no-ops for Tracked
    /// (their bitmap is complete). The FFI `update_tick_data` seam calls
    /// both under one write guard: tick rows replaced first, then the
    /// caller's checked words recorded (the contract that retires the
    /// companion's `_bitmap_override` shadow).
    ///
    /// Returns `false` for V2 / non-concentrated-liquidity / unregistered (the family-dispatch
    /// silent no-op contract).
    #[must_use]
    pub(crate) fn mark_bitmap_words_known_by_pool_id(
        &mut self,
        reg: &mut PoolTable,
        pool_id: u64,
        words: &[i32],
    ) -> bool {
        let Some(entry) = reg.get_mut(pool_id) else {
            return false;
        };
        match entry {
            PoolEntry::V3(p) => {
                p.1.mark_bitmap_words_known(words);
                true
            }
            PoolEntry::V4(p) => {
                p.1.mark_bitmap_words_known(words);
                true
            }
            PoolEntry::V2(..)
            | PoolEntry::Curve(..)
            | PoolEntry::BalancerWeighted(..)
            | PoolEntry::BalancerStable(..)
            | PoolEntry::AerodromeV2(..) => false,
        }
    }
    /// Buffer a V3 liquidity update from the backfill phase. During backfill no
    /// pools are registered yet, so this always buffers (routes to the
    /// never-expired backfill buffer). If the pool happens to be registered
    /// already (defensive), applies directly.
    pub(crate) fn buffer_backfill_v3_liquidity_update(
        &mut self,
        reg: &mut PoolTable,
        pool_address: Address,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) {
        if let Some(key) = reg.pool_id_by_address(&pool_address) {
            if let Some(state) = reg.get_mut(key).and_then(PoolEntry::v3_mut).map(|(_, s)| s) {
                // A `Quarantined` pool defers ALL live/backfill events
                // to the buffer so the pin's `update_block` cannot outrun
                // `last_complete_block`. `Live` pools apply directly (the
                // steady-state contract). Backfill completes before
                // `build_paths`/quarantine in the normal flow, but a late
                // backfill chunk interleaving with a re-register must respect
                // the lifecycle for the invariant to hold.
                if state.registration_lifecycle == RegistrationLifecycle::Live {
                    // Unify the backfill->Live ModifyLiquidity apply through the
                    // shared, in-range-aware path (Bug-A fix). The prior inline
                    // `apply_liquidity_to_tick_range` + manual clock advance
                    // applied the tick map but NEVER adjusted the in-range
                    // `liquidity()` scalar, so a post-seed in-range event (a late
                    // backfill chunk on a Live pool) left the active-liquidity
                    // scalar stale while the tick map advanced — the staged-clock
                    // desync (fresh tick map, stale in-range liquidity) behind
                    // path-142603. The shared `apply_liquidity_update` carries
                    // the historical-replay guard (block <= seed -> tick map only),.
                    // the in-range scalar adjust, the two-stamp clock advance, and
                    // reorg journaling in one place.
                    state.apply_liquidity_update(
                        tick_lower,
                        tick_upper,
                        liquidity_delta,
                        block_number,
                    );
                    return;
                }
            }
        }
        self.v3_buffer.buffer_backfill(
            pool_address,
            BufferedV3PoolEvent::Liquidity(BufferedV3LiquidityUpdate {
                tick_lower,
                tick_upper,
                liquidity_delta,
                block_number,
            }),
        );
    }
    /// Drain and apply a pool's buffered backfill events, for either key shape.
    ///
    /// The drain order is arrival order, so a same-block Mint+Burn pair
    /// netting to zero lands both halves.
    pub(crate) fn apply_backfill_buffer<F: LiquidityFamily>(
        &mut self,
        reg: &mut PoolTable,
        key: &F::Key,
    ) {
        let Some(pool_id) = reg.pool_id_for(&F::lookup(key)) else {
            F::diag_drain(key, "backfill", DrainPhase::NotRegistered, 0);
            return;
        };
        let Some(buffered) = F::drain_backfill(self, key) else {
            F::diag_drain(key, "backfill", DrainPhase::Empty, 0);
            return;
        };
        F::diag_drain(key, "backfill", DrainPhase::Count, buffered.len());
        self.apply_buffered_events::<F>(reg, pool_id, key, "backfill", buffered);
    }

    /// Drain and apply the pump events of every COMPLETED block, for either key
    /// shape.
    ///
    /// The cutoff is the pump `StageMachine` tombstone cutoff: a block is
    /// complete when the first log of N+1 closes N, and draining mid-block
    /// would pin `update_block = N` while missing a later same-block log — so
    /// the in-progress block's events stay buffered.
    pub(crate) fn apply_pump_buffer<F: LiquidityFamily>(
        &mut self,
        reg: &mut PoolTable,
        key: &F::Key,
    ) {
        let Some(pool_id) = reg.pool_id_for(&F::lookup(key)) else {
            F::diag_drain(key, "pump", DrainPhase::NotRegistered, 0);
            return;
        };
        let cutoff = self.pump_complete_cutoff;
        if cutoff == 0 {
            F::diag_drain(key, "pump", DrainPhase::NoCompleted, 0);
            return;
        }
        let Some(buffered) = F::drain_pump_completed(self, key, cutoff) else {
            F::diag_drain(key, "pump", DrainPhase::Empty, 0);
            return;
        };
        F::diag_drain(key, "pump", DrainPhase::Count, buffered.len());
        self.apply_buffered_events::<F>(reg, pool_id, key, "pump", buffered);
    }

    /// Apply drained events in arrival order, warning if `update_block` moves
    /// backwards (an apply must never rewind the pool's freshness).
    fn apply_buffered_events<F: LiquidityFamily>(
        &mut self,
        reg: &mut PoolTable,
        pool_id: u64,
        key: &F::Key,
        verb: &'static str,
        buffered: Vec<F::Event>,
    ) {
        for event in buffered {
            F::diag_event(key, verb, &event);
            let Some(state) = F::state_mut(reg, pool_id) else {
                continue;
            };
            let before = state.update_block();
            F::apply(state, event);
            let after = state.update_block();
            if after < before {
                F::diag_rewind(key, before, after);
            }
        }
    }

    /// Number of buffered V3 liquidity events for a pool address (backfill + pump).
    #[must_use]
    pub fn buffered_v3_event_count(&self, address: &Address) -> usize {
        self.v3_buffer.event_count(address)
    }
    /// Discard all buffered V3 liquidity events for all pools.
    pub fn flush_v3_buffer(&mut self) {
        self.v3_buffer.flush();
    }
    /// Expire V3 pump-buffer events older than `current_block - max_age`.
    /// No-op if `max_age` is `None`. Backfill buffer is never expired.
    pub fn expire_v3_buffered(&mut self, current_block: u64) {
        self.v3_buffer.expire(current_block);
    }
    /// Apply one buffered V3 pool event (`Liquidity` or `Swap`) to a
    /// registered pool's state. The V3 drain loops
    /// ([`apply_backfill_buffer_v3`] / [`apply_pump_buffer_v3`]) dispatch
    /// through here so cross-type arrival order within a block is preserved
    /// (a `Swap` at logIdx 1433 lands after a `Mint` at logIdx 120 if it
    /// arrived after). The slice mutation is the family read module's
    /// apply door (the [`FamilyApply`] contract, V3 arm) — the capability
    /// owns the routing, the door owns the family behavior.
    fn apply_buffered_v3_event(state: &mut V3PoolState, event: BufferedV3PoolEvent) {
        state.apply_event(event);
    }
    /// Mark `block` as fully processed by the pump (every V3 log for `block`
    /// Read a registered V3 pool's state by `pool_id`.
    ///
    /// The solve engine reads state by reference through this accessor
    /// (ADR-003: "Pool's authority over its own math") and calls
    /// Look up a V3 pool's immutable registration identity (address, tokens,
    /// fee, `tick_spacing`, factory). Returns `None` if the pool is not
    /// Snapshot all V3 pool state for verification (clones every V3 entry).
    ///
    /// Used by `verify_liquidity_maps` so the engine+core locks can be
    /// Snapshot seed block `S` setter — the single source of truth for `S`.
    ///
    /// Production paths set `S` here in three ways:
    /// - DB path: `Bot::load_snapshot_from_db` sets `S = min(newest_update_block_v3, v4)`.
    /// - Non-DB path: the `PyArbitrageEngine::set_snapshot_seed_block` setter
    ///   (called by `engine_registry.start()` after `load_*_from_py`) records
    ///   `S = min(newest_block)` from the file/memory snapshot.
    /// - Tests: inject `S` directly to drive the `S≥W` / `S=0` no-op branches
    ///   of `BlockPump::backfill_from_snapshot` without a DB.
    ///
    /// `None` clears the seed (cold-start resume — `BlockPump::resume_from_subscribe`
    /// skips the auto-backfill).
    pub fn set_snapshot_seed_block(&mut self, s: Option<u64>) {
        self.snapshot_seed_block = s;
    }
    /// Consume the pinned snapshot seed for either key shape (exactly once, so
    /// the memory is bounded). `None` when the pool is unregistered or the seed
    /// was already taken.
    pub(crate) fn take_snapshot_seed<F: LiquidityFamily>(
        &mut self,
        reg: &mut PoolTable,
        key: &F::Key,
    ) -> Option<HashMap<i32, TickInfo>> {
        let pool_id = reg.pool_id_for(&F::lookup(key))?;
        F::state_mut(reg, pool_id)?.snapshot_seed_mut().take()
    }

    /// Consume the pinned post-drain `(tick_data, block)` pair for either key
    /// shape (exactly once).
    pub(crate) fn take_post_drain_snapshot<F: LiquidityFamily>(
        &mut self,
        reg: &mut PoolTable,
        key: &F::Key,
    ) -> Option<(HashMap<i32, TickInfo>, u64)> {
        let pool_id = reg.pool_id_for(&F::lookup(key))?;
        F::state_mut(reg, pool_id)?.post_drain_snapshot_mut().take()
    }
    /// Freeze the post-drain `(tick_data, block)` pair atomically with the
    /// drain, for either key shape.
    ///
    /// The pin pairs the tick map with its own LIQUIDITY clock (two-stamp rule:
    /// the price clock can move under the map), clamped down to the tombstone
    /// cutoff when undrained pump events at or below the clock mean the map is
    /// not confirmed complete yet (fabricated-mismatch clamp). The stamp's
    /// provenance is classified here, so a fresher stamp that arrived after the
    /// engine already witnessed events for this pool warns loudly — the
    /// mis-stamped lie shape.
    pub(crate) fn pin_post_drain_snapshot<F: LiquidityFamily>(
        &mut self,
        reg: &mut PoolTable,
        key: &F::Key,
    ) {
        let cutoff = self.pump_complete_cutoff;
        let witnessed = F::event_horizon(self, key);
        let Some(pool_id) = reg.pool_id_for(&F::lookup(key)) else {
            return;
        };
        let Some(state) = F::state_mut(reg, pool_id) else {
            return;
        };
        if state.coverage() != PoolTickCoverage::Tracked {
            return;
        }
        let clock = state.liquidity_clock();
        let undrained = F::pump_count_at_or_below(self, key, clock);
        let pinned_block = if undrained > 0 && cutoff > 0 {
            clock.min(cutoff)
        } else {
            clock
        };
        let ticks = state.pin_post_drain(pinned_block);
        let verdict = pin_provenance_verdict(clock, cutoff, witnessed);
        F::diag_pin(key, clock, ticks, undrained, cutoff, verdict);
    }
    /// Overwrite a registered pool's state from one source snapshot, for either
    /// key shape.
    ///
    /// Returns whether the pool was registered; a wholesale sync replaces the
    /// scalars AND the tick map from one snapshot, so both clocks move to the
    /// snapshot's block together — a sanctioned reset, not the incremental
    /// backward stamp the two-stamp rule forbids.
    pub(crate) fn sync_pool_state<F: LiquidityFamily>(
        &mut self,
        reg: &mut PoolTable,
        key: &F::Key,
        update: F::Sync,
    ) -> bool {
        let Some(pool_id) = reg.pool_id_for(&F::lookup(key)) else {
            return false;
        };
        let Some(state) = F::state_mut(reg, pool_id) else {
            return false;
        };
        F::apply_sync(state, update);
        true
    }

    /// Stage half of the word backfill: clone the pool's stored read handle and
    /// capture its tick fingerprint UNDER a short write, then release the
    /// caller's guard before the (multi-second, `Python::attach` + web3 RPC)
    /// fetch runs. The choreography is the tick-word capability's
    /// ([`degenbot_pools::stage_word_fetch`]); the pool id and the retried fetch
    /// context are the substrate's half. Pair with
    /// [`Self::install_word_fetch`].
    pub(crate) fn stage_word_fetch_by_pool_id(
        &self,
        reg: &PoolTable,
        pool_id: u64,
        word: i32,
        block: u64,
        retried: bool,
    ) -> Option<StagedWordFetch> {
        let entry = reg.get(pool_id)?;
        let reader: TickWordReader = entry.tick_word_reader()?;
        // The fetch context must reflect the CURRENT
        // pool clock on a retry, not the (stale) companion block — a fetch
        // at the original context after an interleaved event snapshots the
        // word pre-event, and even the stamp guard cannot help a tick the
        // event added (the guard preserves resident stamps; the word would
        // otherwise go known-minus-missing-ticks alongside the event's own
        // later application). First attempt honors the companion context.
        let fetch_context = if retried {
            pool_clock(reg, pool_id).map_or(0, |clock| clock.saturating_sub(1))
        } else {
            block
        };
        Some(StagedWordFetch {
            pool_id,
            staged: degenbot_pools::stage_word_fetch(
                entry,
                word,
                fetch_context,
                WordFetchPolicy::Armed(&reader),
            )?,
        })
    }
    /// Install half: merge the fetched word only if the pool was NOT mutated
    /// while the fetch ran — the capability's fingerprint re-check. A mutation
    /// means the pump applied an event for this pool during the fetch window,
    /// so the staged overlay would clobber fresher tick writes and the caller
    /// RETRIES the stage+fetch (bounded) instead of applying a lost update.
    /// [`WordFetchOutcome::NoSource`] covers the pool gone / no longer a merge
    /// target case, which keeps the failure contract (the companion gate
    /// raises).
    pub(crate) fn install_word_fetch(
        &self,
        reg: &mut PoolTable,
        staged: &StagedWordFetch,
        fetched: &::degenbot_pools::CheckedWord,
    ) -> WordFetchOutcome {
        let Some(entry) = reg.get_mut(staged.pool_id) else {
            return WordFetchOutcome::NoSource;
        };
        degenbot_pools::install_staged_word(entry, &staged.staged, fetched)
    }
    /// Backfill an unknown tick-bitmap word for a registered V3/V4 pool
    /// (the write-path twin of the fetch+retry calc seam).
    ///
    /// Invokes the state's stored fetcher for `word` at `block` (the
    /// companion passes `state_block - 1` as the fetch context), and on
    /// success merges the word through [`Self::merge_tick_word`] — the SAME
    /// core merge the sim loop uses (ticks overlay, word marked known,
    /// cache invalidated; a checked-empty fetch marks the word known with no
    /// ticks).
    ///
    /// Returns `false` when the pool has no stored fetcher OR the fetch
    /// failed — the caller (the Python companion gate) RAISES on `false`
    /// rather than applying the event over an unknown word. `true` only on a
    /// successful merge (checked-empty included).
    pub(crate) fn ensure_word_known_by_pool_id(
        &mut self,
        reg: &mut PoolTable,
        pool_id: u64,
        word: i32,
        block: u64,
    ) -> bool {
        let Some(entry) = reg.get_mut(pool_id) else {
            return false;
        };
        let Some(reader) = entry.tick_word_reader() else {
            return false;
        };
        degenbot_pools::fetch_and_merge(entry, word, block, WordFetchPolicy::Armed(&reader)).known()
    }
    /// Merge a fetched tick-bitmap word into a V3/V4 pool's state.
    ///
    /// Adds the word's initialized ticks to `tick_data` (overlaying any
    /// existing entries at the same tick) and records the `word` as known in
    /// `known_bitmap_words` (so the next simulate does not re-fetch it). A
    /// fetched-but-empty word is recorded as known with no ticks added —
    /// mirrors the Python bitmap-store rule (a region is unknown unless its
    /// word key is in the lazy-loaded map, regardless of the bitmap value).
    ///
    /// Returns `true` if the merge applied to a registered V3/V4 pool,
    /// `false` otherwise (silent no-op — mirrors `sync_tick_data_by_pool_id`).
    /// ADR-005 sparse-map feature parity.
    pub(crate) fn merge_tick_word(
        &mut self,
        reg: &mut PoolTable,
        pool_id: u64,
        fetched: &::degenbot_pools::FetchedTickWord,
    ) -> bool {
        // One dispatch through the merge-target seam: V3 and V4 share the state
        // verb behind it, and an entry that is not a target answers `None` (a
        // non-concentrated-liquidity / unregistered pool is a silent no-op).
        let Some(entry) = reg.get_mut(pool_id) else {
            return false;
        };
        if entry.tick_word_fingerprint().is_none() {
            return false;
        }
        entry.apply_fetched_word(fetched);
        true
    }
    // -----------------------------------------------------------------------
    // V4 state (ADR-003: single entry per `(pool_manager, pool_id)`;
    // orientation derived at solve from `zero_for_one`)
    // -----------------------------------------------------------------------
    /// Record the canonical V4 `StateView` contract address for a `pool_manager`
    /// (ADR-005 / Option 2 — Rust owns the mapping). V4 scalar state is read
    /// via the `StateView`'s `getSlot0`/`getLiquidity`, not `getPool` on the
    /// `PoolManager` (which reverts on the canonical deployment); the
    /// solver-state verifier resolves it per-hop via [`ConcentratedLiquidityOrchestration::state_view_for`].
    /// Idempotent: the seed for a manager is supplied once by the driver
    /// (read from the `pool_managers` DB row) before V4 pools solve.
    pub fn register_v4_state_view(&mut self, pool_manager: Address, state_view: Address) {
        self.v4_state_views.insert(pool_manager, state_view);
    }
    /// The canonical V4 `StateView` address for `pool_manager`, if registered.
    /// `None` when unknown — a V4 hop whose manager's `StateView` has not been
    /// seeded is skipped rather than false-alarmed, and a refresh of such a
    /// pool reports its missing identity instead of guessing a read target.
    /// The mapping's write path is live via `register_v4_state_view` (the
    /// `PyO3` registration path); `BotState::v4_state_view_for_pool` is the
    /// pool-keyed reader the refresh entry uses.
    #[must_use]
    pub(crate) fn state_view_for(&self, pool_manager: Address) -> Option<Address> {
        self.v4_state_views.get(&pool_manager).copied()
    }
    /// The immutable admission verdict recorded for a pool, if any (PRG-2).
    /// The `PyO3` `build_v4_pool` pre-check consults this BEFORE any RPC
    /// work on the registration path.
    #[must_use]
    pub fn admission_verdict(
        &self,
        pool_manager: Address,
        pool_id: &degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) -> Option<crate::registration_gate::AdmissionVerdict> {
        self.registration_gate.verdict(pool_manager, *pool_id)
    }
    /// Register a V4 pool by `(pool_manager, pool_id)`.
    ///
    /// ADR-037: pools with amount-modifying hooks are ADMITTED (their
    /// simulations carry `Caveats::HOOKED_POOL` and hop projection excludes
    /// them from solving). Dynamic fees and static fees exceeding the
    /// `cmd_executor`'s 2-byte encoding limit are still rejected. Returns
    /// `Err(RegisterV4PoolError)` on rejection.
    ///
    /// # Errors
    ///
    /// Returns [`RegisterV4PoolError::SpecViolation`] when `sqrt_price_x96`,
    /// `tick`, V4 `fee`, or `tick_spacing` violates its Solidity-bounded
    /// on-chain invariant (see [`spec_bounds`]). These checks fire *first* —
    /// before the dynamic-fee / high-fee / already-registered rejections — so
    /// an impossible-concentrated-liquidity-config rejection surfaces the primitive at fault.
    ///
    /// Returns `Err` if the pool uses a dynamic fee (`fee == 0x100000`),
    /// has a static fee exceeding the executor's `u16` encoding field
    /// (`fee >= degenbot_executor::encoders::V4_FEE_ENCODER_MAX`), or a pool
    /// with the same `(pool_manager, pool_id)` is already registered.
    pub(crate) fn register_v4_pool(
        &mut self,
        reg: &mut PoolTable,
        params: &RegisterV4PoolParams,
    ) -> Result<u64, RegisterV4PoolError> {
        use ::degenbot_pools::state::spec_bounds as sb;
        sb::validate_sqrt_price(params.sqrt_price_x96)
            .map_err(RegisterV4PoolError::SpecViolation)?;
        sb::validate_tick(params.tick).map_err(RegisterV4PoolError::SpecViolation)?;
        sb::validate_v4_fee(params.pool_key.fee).map_err(RegisterV4PoolError::SpecViolation)?;
        sb::validate_tick_spacing(params.pool_key.tick_spacing)
            .map_err(RegisterV4PoolError::SpecViolation)?;

        if params.pool_key.fee == V4_DYNAMIC_FEE_FLAG {
            self.registration_gate.record(
                params.pool_manager,
                params.pool_id,
                crate::registration_gate::AdmissionVerdict::DynamicFee {
                    fee: params.pool_key.fee,
                },
            );
            return Err(RegisterV4PoolError::DynamicFee {
                fee: params.pool_key.fee,
            });
        }
        // the cmd_executor encodes V4 `fee` as a 2-byte field in both
        // swap commands; a static fee > 65535 is protocol-valid but
        // un-encodable. Reject at admission (mirroring the dynamic-fee floor)
        // so these pools never reach the composer's `u16::try_from` guard and
        // waste a solve cycle.
        if params.pool_key.fee >= degenbot_executor::encoders::V4_FEE_ENCODER_MAX {
            self.registration_gate.record(
                params.pool_manager,
                params.pool_id,
                crate::registration_gate::AdmissionVerdict::FeeExceedsEncoderLimit {
                    fee: params.pool_key.fee,
                },
            );
            return Err(RegisterV4PoolError::FeeExceedsEncoderLimit {
                fee: params.pool_key.fee,
            });
        }

        let (pool_manager, v4_pool_id) = (params.pool_manager, params.pool_id);

        // the `seed_from_store` path is retired — the DB
        // seeding is handled by the Db arm of `assemble_v4_tick_map` (held
        // snapshot tx). Just clone + flow the params through.
        let params = params.clone();
        let (identity, state) = V4PoolState::from_params(params, reg.journal_depth());
        reg.register(
            PoolClaim::new(
                PoolLookup::V4 {
                    pool_manager,
                    pool_id: v4_pool_id,
                },
                RegisteredPoolFamily::V4,
            ),
            PoolEntry::V4(Box::new((identity, state))),
        )
        .pool_id()
        .ok_or(RegisterV4PoolError::AlreadyRegistered {
            pool_manager,
            pool_id: v4_pool_id,
        })
    }
    /// Resolve a V4 pool's routing presence (`concentrated_liquidity_route` axis),
    /// keyed by the (`pool_manager`, pool id) singleton key.
    fn presence_by_pool_id(
        &self,
        reg: &PoolTable,
        key: &(Address, degenbot_decoders::v4_swap_decoder::V4PoolId),
    ) -> PoolPresence {
        let lifecycle = reg
            .pool_id_by_key(key.0, &key.1)
            .and_then(|id| match reg.get(id) {
                Some(PoolEntry::V4(p)) => Some(p.1.registration_lifecycle),
                _ => None,
            });
        PoolPresence::from_lifecycle(lifecycle)
    }
    /// THE single decision point for a decoded V4 event (`concentrated_liquidity_route` table).
    /// Key-registered twin of [`Self::route_event`], keyed by `(pool_manager, pool_id)`.
    /// `swap_priors` overlays tick priors onto DIRECT swap application only.
    pub(crate) fn route_event_by_key(
        &mut self,
        reg: &mut PoolTable,
        phase: Phase,
        pool_manager: Address,
        v4_pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId,
        event: BufferedV4PoolEvent,
        swap_priors: &[(i32, TickInfo)],
    ) -> ApplyOutcome {
        let kind = match &event {
            BufferedV4PoolEvent::Swap(_) => EventKind::ScalarRefresh,
            BufferedV4PoolEvent::Liquidity(_) => EventKind::TickMutation,
        };
        let presence = self.presence_by_pool_id(reg, &(pool_manager, v4_pool_id));
        let event_block = match &event {
            BufferedV4PoolEvent::Swap(s) => s.block_number,
            BufferedV4PoolEvent::Liquidity(l) => l.block_number,
        };
        match route_action(phase, presence, kind) {
            RouteAction::ApplyDirect => {
                // Table invariant: ApplyDirect implies registered. Defensive
                // fallback (never expected) stages rather than drops.
                let Some(id) = reg.pool_id_by_key(pool_manager, &v4_pool_id) else {
                    debug_assert!(
                        false,
                        "ApplyDirect for an unregistered V4 pool — routing table invariant violated"
                    );
                    return ApplyOutcome::Buffered(BufferKind::Pump);
                };
                match event {
                    BufferedV4PoolEvent::Swap(BufferedV4SwapEvent {
                        sqrt_price_x96,
                        liquidity,
                        tick,
                        block_number,
                    }) => {
                        if let Some(PoolEntry::V4(p)) = reg.get_mut(id) {
                            p.1.apply_swap(
                                sqrt_price_x96,
                                liquidity,
                                tick,
                                block_number,
                                swap_priors,
                            );
                        }
                    }
                    BufferedV4PoolEvent::Liquidity(BufferedV4LiquidityUpdate {
                        tick_lower,
                        tick_upper,
                        liquidity_delta,
                        block_number,
                    }) => {
                        if let Some(refusal) = self.apply_tick_mutation(
                            reg,
                            id,
                            tick_lower,
                            tick_upper,
                            liquidity_delta,
                            block_number,
                        ) {
                            return refusal;
                        }
                    }
                }
                self.note_v4_event_block((pool_manager, v4_pool_id), event_block);
                ApplyOutcome::Applied(id)
            }
            RouteAction::Buffer(kind) => {
                // Preserve historical live-path route traces (grep compat).
                if phase == Phase::Live && presence != PoolPresence::Live {
                    if let BufferedV4PoolEvent::Liquidity(u) = &event {
                        let label = if presence == PoolPresence::Unregistered {
                            "none"
                        } else {
                            "Quarantined"
                        };
                        let route = if presence == PoolPresence::Unregistered {
                            "buffer-pump"
                        } else {
                            "buffer-pump-quarantined"
                        };
                        trace_apply_route_v4(
                            pool_manager,
                            &alloy::hex::encode_prefixed(v4_pool_id),
                            u.tick_lower,
                            u.tick_upper,
                            u.liquidity_delta,
                            u.block_number,
                            label,
                            route,
                        );
                    }
                }
                match kind {
                    BufferKind::Backfill => self
                        .v4_buffer
                        .buffer_backfill((pool_manager, v4_pool_id), event),
                    BufferKind::Pump => self
                        .v4_buffer
                        .buffer_pump((pool_manager, v4_pool_id), event),
                }
                self.note_v4_event_block((pool_manager, v4_pool_id), event_block);
                ApplyOutcome::Buffered(kind)
            }
            RouteAction::Drop(reason) => ApplyOutcome::NoOp(reason),
        }
    }
    /// Buffer a V4 `ModifyLiquidity` event from the backfill phase.
    pub(crate) fn buffer_backfill_v4_liquidity_update(
        &mut self,
        reg: &mut PoolTable,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) {
        let key = (pool_manager, pool_id);
        if let Some(id) = reg.pool_id_by_key(key.0, &key.1) {
            if let Some(state) = reg.get_mut(id).and_then(PoolEntry::v4_mut).map(|(_, s)| s) {
                // A `Quarantined` pool defers ALL live/backfill events
                // to the buffer so the pin's `update_block` cannot outrun
                // `last_complete_block`. `Live` pools apply directly (the
                // steady-state contract). Backfill completes before
                // `build_paths`/quarantine in the normal flow, but a late
                // backfill chunk interleaving with a re-register must respect
                // the lifecycle for the invariant to hold.
                if state.registration_lifecycle == RegistrationLifecycle::Live {
                    // V4 twin of the V3 backfill->Live unification (Bug-A fix):
                    // the shared `apply_liquidity_update` adjusts the in-range
                    // `liquidity()` scalar for a post-seed event (and carries the
                    // historical-replay guard + two-stamp clocks + journal), where
                    // the prior inline `apply_liquidity_to_tick_range` did not.
                    state.apply_liquidity_update(
                        tick_lower,
                        tick_upper,
                        liquidity_delta,
                        block_number,
                    );
                    return;
                }
            }
        }
        self.v4_buffer.buffer_backfill(
            key,
            BufferedV4PoolEvent::Liquidity(BufferedV4LiquidityUpdate {
                tick_lower,
                tick_upper,
                liquidity_delta,
                block_number,
            }),
        );
    }
    /// Set the maximum age for buffered V4 pump events. `None` = unbounded.
    pub fn set_v4_buffer_max_age(&mut self, max_age: Option<u64>) {
        self.v4_buffer.set_max_age(max_age);
    }
    pub fn flush_v4_buffer(&mut self) {
        self.v4_buffer.flush();
    }
    pub fn expire_v4_buffered(&mut self, current_block: u64) {
        self.v4_buffer.expire(current_block);
    }
    /// Apply one buffered V4 pool event (`Liquidity` or `Swap`) to a
    /// registered pool's state. The V4 drain loops
    /// ([`apply_backfill_buffer_v4`] / [`apply_pump_buffer_v4`]) dispatch
    /// through here so cross-type arrival order within a block is preserved.
    /// V4 twin of [`apply_buffered_v3_event`] — the `Liquidity` payload already
    /// carries the contracts' `int128` bound, so the drain narrows nothing
    /// (ADR-082 D3).
    fn apply_buffered_v4_event(state: &mut V4PoolState, event: BufferedV4PoolEvent) {
        state.apply_event(event);
    }
    /// Set a `Tracked` pool's registration lifecycle to `Quarantined`, for
    /// either key shape.
    ///
    /// The live pump then defers the pool's `Swap`/`Mint`/`Burn` events to the
    /// pump buffer until [`Self::mark_registration_live`] transitions it back. Call at the
    /// start of registration, before the first RPC await. No-op for an
    /// unregistered pool and for a `Sparse` pool (it has no pin / step-2 verify
    /// to protect, so deferring its events buys nothing — coverage-aware
    /// carve-out).
    pub(crate) fn begin_registration_quarantine<F: LiquidityFamily>(
        &mut self,
        reg: &mut PoolTable,
        key: F::Key,
    ) {
        let Some(pool_id) = reg.pool_id_for(&F::lookup(&key)) else {
            return;
        };
        let Some(state) = F::state_mut(reg, pool_id) else {
            return;
        };
        if state.coverage() == PoolTickCoverage::Tracked {
            *state.lifecycle_mut() = RegistrationLifecycle::Quarantined;
        }
    }

    /// Transition a pool from `Quarantined` to `Live`, flushing the retained
    /// pump tail first, for either key shape.
    ///
    /// The flush uses `drain_pump` (not `drain_pump_completed`) because the
    /// retained tail must not be orphaned — there is no second registration
    /// drain — matching the Live steady-state contract (Live pools receive
    /// direct apply with no per-block gate; ordering preserved). No-op for an
    /// unregistered pool.
    pub(crate) fn mark_registration_live<F: LiquidityFamily>(
        &mut self,
        reg: &mut PoolTable,
        key: F::Key,
    ) {
        let Some(pool_id) = reg.pool_id_for(&F::lookup(&key)) else {
            return;
        };
        if let Some(buffered) = F::drain_pump(self, &key) {
            let blocks: Vec<u64> = buffered
                .iter()
                .map(::degenbot_pools::LiquidityEvent::block_number)
                .collect();
            let mut distinct_blocks = blocks.clone();
            distinct_blocks.sort_unstable();
            distinct_blocks.dedup();
            F::diag(&key, "set_live", &blocks, &distinct_blocks);
            for event in buffered {
                if let Some(state) = F::state_mut(reg, pool_id) {
                    F::apply(state, event);
                }
            }
        }
        if let Some(state) = F::state_mut(reg, pool_id) {
            *state.lifecycle_mut() = RegistrationLifecycle::Live;
        }
    }

    /// Batch-release every pool still `Quarantined` (orphan sweep).
    ///
    /// With Tracked pools now registering `Quarantined` by default, a Tracked
    /// pool built via `build_pool`/`build_managed_pool` but never reached by
    /// the driver's `register_v3/v4_pool` (e.g. its path was skipped before
    /// registration) would otherwise defer events to its buffer indefinitely.
    /// Call once after `build_paths` finishes: flush each still-`Quarantined`
    /// pool's retained pump tail (same unguarded `drain_pump` as
    /// [`Self::mark_registration_live`]) and mark it `Live`, so no
    /// registered pool is left buffering forever. No-op when nothing is
    /// quarantined.
    pub(crate) fn release_all_v3_v4_quarantined(&mut self, reg: &mut PoolTable) {
        // Collect the still-Quarantined V3 addresses and V4 (pm, pool_id) keys
        // first (drain buffers are keyed by those, not `pool_id`), then release
        // each via the existing set_live flush+mark. Collect-then-apply avoids
        // holding a `&mut reg` borrow across the drain calls.
        let v3_addrs: Vec<Address> = reg
            .entries()
            .filter_map(|(_, e)| match e {
                PoolEntry::V3(p)
                    if p.1.registration_lifecycle == RegistrationLifecycle::Quarantined =>
                {
                    Some(p.0.address)
                }
                _ => None,
            })
            .collect();
        let v4_keys: Vec<(Address, degenbot_decoders::v4_swap_decoder::V4PoolId)> = reg
            .entries()
            .filter_map(|(_, e)| match e {
                PoolEntry::V4(p)
                    if p.1.registration_lifecycle == RegistrationLifecycle::Quarantined =>
                {
                    Some((p.0.pool_manager, p.0.pool_id))
                }
                _ => None,
            })
            .collect();
        let total = v3_addrs.len() + v4_keys.len();
        if total == 0 {
            return;
        }
        diag!(
            domain = verify,
            v3 = v3_addrs.len(),
            v4 = v4_keys.len(),
            "release-all quarantined"
        );
        for addr in v3_addrs {
            self.mark_registration_live::<V3Family>(reg, addr);
        }
        for (pm, pid) in v4_keys {
            self.mark_registration_live::<V4Family>(reg, (pm, pid));
        }
    }
    /// One-read fast-path lookup for the `PyO3` `build_v4_pool` re-build guard
    /// (missed-WS-pong incident 2026-08-28).
    ///
    /// The driver re-attempts `build_managed_pool` for V4 hops whose pools
    /// are ALREADY registered in this shared core (DB-snapshot seeds). The
    /// historical behavior re-ran the full builder (fresh slot0/liquidity
    /// RPC reads + tick-map assembly) and only failed at the terminal
    /// `register_v4_pool` with `AlreadyRegistered` — ~13k wasted builds per
    /// block, whose RPC load + GIL/log flooding starved the WS keepalive. This
    /// accessor resolves the existing registration under ONE read guard so the
    /// `PyO3` layer can synthesize the builder's return surface (identity +
    /// coverage + fees) with core-tracked values and NO RPC.
    ///
    /// Dynamic-fee / hooked / fee-exceeds-encoder pools are admission-rejected
    /// and never registered, so they miss here and keep their existing typed
    /// rejections.
    #[must_use]
    pub(crate) fn try_registered_v4(
        &self,
        reg: &PoolTable,
        pool_manager: Address,
        pool_id: &degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) -> Option<RegisteredV4> {
        let id = reg.pool_id_by_key(pool_manager, pool_id)?;
        let (identity, state) = reg.get(id).and_then(PoolEntry::v4)?;
        Some(RegisteredV4 {
            pool_id: id,
            pool_key: identity.pool_key.clone(),
            protocol_fee: state.protocol_fee,
            coverage: state.coverage,
        })
    }
    /// Remove a pool's registration and everything buffered for it, for either
    /// key shape. Returns the pool id that left the registry.
    ///
    /// The address arm covers every address-keyed family (V2/V3/Aerodrome/
    /// Curve/Balancer as well as concentrated liquidity); discarding the
    /// concentrated-liquidity buffers for a non-CL address is a no-op.
    pub(crate) fn unregister_pool<F: LiquidityFamily>(
        &mut self,
        reg: &mut PoolTable,
        key: &F::Key,
    ) -> Option<u64> {
        let pool_id = reg.unregister(&F::lookup(key))?;
        F::discard_buffered(self, key);
        Some(pool_id)
    }

    // ------------------------------------------------------------------
    // Delivery-cutoff / seed-block accessors (relocated from lib.rs)
    // ------------------------------------------------------------------

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

    /// Set the maximum age (in blocks) for buffered V3 pump events.
    /// `None` means unbounded. Takes effect on the next `expire_v3_buffered`.
    pub const fn set_v3_buffer_max_age(&mut self, max_age: Option<u64>) {
        self.v3_buffer.set_max_age(max_age);
    }

    /// The snapshot seed block `S` - `min(fetch_newest_update_block(V3), V4)`
    /// across the loaded snapshots. `None` when no snapshot was loaded (the
    /// cold-start path pumps directly from `first_observed_block`). Set by
    /// `Bot::load_snapshot_from_db` / `load_snapshot_from_py`; consumed by the
    /// auto-backfill (`resume_from_subscribe`) that closes `S+1..W-1`.
    #[must_use]
    pub const fn snapshot_seed_block(&self) -> Option<u64> {
        self.snapshot_seed_block
    }
}

impl BotState {
    /// Read access to the concentrated liquidity orchestration capability — the seam for callers
    /// that need only concentrated-liquidity-local state and never the registry core.
    #[must_use]
    pub fn concentrated_liquidity(&self) -> &ConcentratedLiquidityOrchestration {
        &self.concentrated_liquidity
    }

    /// Mutable access to the concentrated liquidity orchestration capability. Operations that
    /// must also mutate the registry core go through the composition-root
    /// methods, which own the split borrow.
    pub fn concentrated_liquidity_mut(&mut self) -> &mut ConcentratedLiquidityOrchestration {
        &mut self.concentrated_liquidity
    }

    /// The pinned snapshot seed a pool is registered with, if it has one.
    ///
    /// Read before the seed is consumed by step-1 verify; one door for either
    /// key shape (the seed itself is the same tick-row map in both families).
    #[must_use]
    pub fn snapshot_seed(&self, key: PoolLookup) -> Option<&HashMap<i32, TickInfo>> {
        match key {
            PoolLookup::Address(address) => {
                let pool_id = self.pool_id_by_address(&address)?;
                let (_identity, state) = self.pools.get(pool_id).and_then(PoolEntry::v3)?;
                state.snapshot_seed.as_ref()
            }
            PoolLookup::V4 {
                pool_manager,
                pool_id,
            } => {
                let pool_id = self.pools.pool_id_by_key(pool_manager, &pool_id)?;
                let (_identity, state) = self.pools.get(pool_id).and_then(PoolEntry::v4)?;
                state.snapshot_seed.as_ref()
            }
        }
    }
}

/// The id-keyed concentrated-liquidity apply doors: every verb a caller
/// holding a `pool_id` uses, each resolved to its family through [`PoolLookup`]
/// so a registered pool of another family is a typed refusal rather than a
/// silent no-op.
impl BotState {
    /// Family-dispatching Swap apply. The single entry point
    /// `PyLiquidityPool.apply_swap` calls — the registered pool's entry
    /// selects the design (ADR-073 D3: the surface name is family-neutral).
    /// V2/unregistered → `None` (no-op, matching the liquidity-update
    /// sibling). This preserves the single Python `apply_swap` API while
    /// routing V4 updates correctly (the prior unconditional V3 routing
    /// silently dropped every V4 update).
    pub fn apply_swap_by_pool_id(
        &mut self,
        pool_id: u64,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)],
    ) -> Option<u64> {
        let state = self.concentrated_state_mut(pool_id, "swap").ok()?;
        state.apply_swap(sqrt_price_x96, liquidity, tick, block_number, tick_priors);
        Some(pool_id)
    }

    /// Registration/seed genesis anchor for a registered V3/V4 pool
    /// (two-stamp rule): pushes a `before == after` journal delta at `block`
    /// so the reorg journal is non-empty from registration, WITHOUT advancing
    /// either clock. The split-seed replacement for the builder's old
    /// `apply_swap` genesis, which would backward-panic `update_block` (price
    /// seeded at HEAD past the DB map block) and falsely advance
    /// `tick_data_block`. A non-concentrated-liquidity family has no concentrated liquidity journal to seed and is
    /// refused with [`ConcentratedLiquidityApplyError::UnsupportedFamily`].
    ///
    /// # Errors
    ///
    /// [`ConcentratedLiquidityApplyError::NotRegistered`] for an unknown pool id, or
    /// [`ConcentratedLiquidityApplyError::UnsupportedFamily`] for a registered non-concentrated-liquidity family.
    pub fn seed_genesis_by_pool_id(
        &mut self,
        pool_id: u64,
        block: u64,
    ) -> Result<u64, ConcentratedLiquidityApplyError> {
        self.concentrated_state_mut(pool_id, "genesis seed")?
            .seed_genesis(block);
        Ok(pool_id)
    }

    /// The ONE family dispatch behind the three `*_by_pool_id` apply doors:
    /// resolve the id to the [`PoolLookup`] key its family addresses the pool
    /// by, then hand back the family-neutral state that key names.
    ///
    /// [`PoolLookup::from_pool_id`] answers `Some` only for the two
    /// concentrated-liquidity families, so a miss is the refusal itself: an
    /// unregistered id is [`ConcentratedLiquidityApplyError::NotRegistered`] and
    /// a registered family without concentrated-liquidity state is
    /// [`ConcentratedLiquidityApplyError::UnsupportedFamily`], with `op` naming
    /// the refused operation for the operator.
    fn concentrated_state_mut(
        &mut self,
        pool_id: u64,
        op: &'static str,
    ) -> Result<&mut dyn ConcentratedLiquidityPoolMut, ConcentratedLiquidityApplyError> {
        let Some(key) = PoolLookup::from_pool_id(self, pool_id) else {
            return Err(match self.pools.get(pool_id) {
                Some(entry) => ConcentratedLiquidityApplyError::UnsupportedFamily {
                    pool_id,
                    family: entry_family(entry),
                    op,
                },
                None => ConcentratedLiquidityApplyError::NotRegistered { pool_id },
            });
        };
        key.state_mut(self)
            .ok_or(ConcentratedLiquidityApplyError::NotRegistered { pool_id })
    }

    /// Family-dispatching liquidity update: apply a liquidity-position change
    /// to a registered pool **now** — the single entry point
    /// `PyLiquidityPool.apply_liquidity_update` calls, with the design
    /// selected by the registered entry (ADR-073 D3: the surface name is
    /// family-neutral) and a registered non-concentrated-liquidity family
    /// refused with [`ConcentratedLiquidityApplyError::UnsupportedFamily`].
    ///
    /// This is the caller-owns-the-event entry, not the driver's: the companion
    /// (a Python tracker applying a decoded Mint/Burn it already gated) and the
    /// memoization seam assert that a registered pool's state moves immediately,
    /// so this door skips the phase gate that [`Self::apply_liquidity_update`]
    /// applies. Routing it through the phase table would stage the delta into
    /// the pump buffer for a pool that is registered but not yet live, which
    /// those callers do not drain — `pool_id` names a pool this caller already
    /// holds, and the driver keeps its own buffered path for the same event.
    ///
    /// # Errors
    ///
    /// [`ConcentratedLiquidityApplyError::NotRegistered`] for an unknown pool id, or
    /// [`ConcentratedLiquidityApplyError::UnsupportedFamily`] for a registered non-concentrated-liquidity family.
    pub fn apply_liquidity_update_by_pool_id(
        &mut self,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) -> Result<u64, ConcentratedLiquidityApplyError> {
        self.concentrated_state_mut(pool_id, "liquidity update")?
            .apply_liquidity_update(tick_lower, tick_upper, liquidity_delta, block_number);
        Ok(pool_id)
    }

    /// Replay a backfill chunk's decoded events into pool state without
    /// emitting a batch.
    ///
    /// Applies each decoded event via the same
    /// `apply_swap` / `buffer_backfill_*_liquidity_update` / `apply_swap_update`
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
            // at `chunk_end`, never block 0. V2 Sync stays out
            // of backfill scope (concentrated-liquidity-only: scalar state arrives via snapshot).
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
            // The strict sparse-word gate can refuse a tick mutation here too
            // (`pump.strict_sparse_apply`), but this loop holds the chunk's
            // write guard, so no word fetch is reachable: surface the refusal
            // loudly rather than letting it pass as an unapplied event.
            if let Some(crate::concentrated_liquidity_route::ApplyOutcome::MissingTickWord {
                pool_id,
                word,
            }) = event.apply_backfill(self)
            {
                tracing::warn!(
                    pool_id,
                    word,
                    chunk_end,
                    "strict sparse apply gate: backfill-phase boundary word \
                     unknown and unfetchable under the chunk guard; event NOT applied"
                );
            }
        }
        if v3_touched {
            self.concentrated_liquidity.expire_v3_buffered(chunk_end);
        }
        if v4_touched {
            self.concentrated_liquidity.expire_v4_buffered(chunk_end);
        }
    }

    /// Remove a pool from the registry, discarding anything buffered for it.
    ///
    /// One effect, either key shape: the address names any address-keyed family,
    /// the `(pool_manager, pool_id)` pair names a V4 pool. Returns whether a
    /// registration was removed.
    pub fn unregister_pool(&mut self, key: PoolLookup) -> bool {
        match key {
            PoolLookup::Address(address) => self
                .concentrated_liquidity
                .unregister_pool::<V3Family>(&mut self.pools, &address)
                .is_some(),
            PoolLookup::V4 {
                pool_manager,
                pool_id,
            } => self
                .concentrated_liquidity
                .unregister_pool::<V4Family>(&mut self.pools, &(pool_manager, pool_id))
                .is_some(),
        }
    }

    /// Seed tick rows into a registered V3 pool's tick map.
    ///
    /// Registration is the only production writer of a V3 pool's tick map, so a
    /// harness that must start from a hand-chosen tick layout has no other
    /// entry. Compiled for in-crate tests and behind the `test-fixtures`
    /// feature a consumer's dev-dependency enables; never in a production
    /// build.
    ///
    /// Returns `false` for an unregistered id and for a non-V3 pool.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn seed_v3_pool_ticks_for_test(
        &mut self,
        pool_id: u64,
        ticks: impl IntoIterator<Item = (i32, TickInfo)>,
    ) -> bool {
        let Some(PoolEntry::V3(pool)) = self.pools.get_mut(pool_id) else {
            return false;
        };
        pool.1.tick_data.extend(ticks);
        true
    }
}

/// Delegating composition-root surface: every concentrated liquidity capability method stays
/// reachable on `BotState` (the pub surface external crates consume);
/// each wrapper is a one-line split-borrow delegation.
impl BotState {
    /// Delegates to the concentrated liquidity orchestration capability - see [`ConcentratedLiquidityOrchestration::register_v3_pool`].
    ///
    /// # Errors
    ///
    /// Returns [`RegisterV3PoolError`] when the pool address is already
    /// registered or a registration spec bound is violated.
    pub fn register_v3_pool(
        &mut self,
        params: &RegisterV3PoolParams,
    ) -> Result<u64, RegisterV3PoolError> {
        self.concentrated_liquidity
            .register_v3_pool(&mut self.pools, params)
    }

    /// Delegates to the concentrated liquidity orchestration capability - see [`ConcentratedLiquidityOrchestration::update_v3_pool`].
    pub fn update_pool(
        &mut self,
        pool_address: Address,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: Vec<(i32, TickBefore)>,
    ) {
        self.concentrated_liquidity.update_v3_pool(
            &mut self.pools,
            pool_address,
            sqrt_price_x96,
            liquidity,
            tick,
            block_number,
            tick_priors,
        );
    }

    /// Delegates to the concentrated liquidity orchestration capability - see [`ConcentratedLiquidityOrchestration::sync_tick_data_by_pool_id`].
    pub fn sync_tick_data_by_pool_id(
        &mut self,
        pool_id: u64,
        tick_data: HashMap<i32, TickInfo>,
        update_block: u64,
    ) -> bool {
        self.concentrated_liquidity.sync_tick_data_by_pool_id(
            &mut self.pools,
            pool_id,
            tick_data,
            update_block,
        )
    }

    /// Delegates to the concentrated liquidity orchestration capability - see [`ConcentratedLiquidityOrchestration::mark_bitmap_words_known_by_pool_id`].
    pub fn mark_bitmap_words_known_by_pool_id(&mut self, pool_id: u64, words: &[i32]) -> bool {
        self.concentrated_liquidity
            .mark_bitmap_words_known_by_pool_id(&mut self.pools, pool_id, words)
    }
    /// Apply a liquidity-position change to a pool, for either key shape.
    ///
    /// Routes through [`Self::route_event`], so the phase gate, the
    /// family-mismatch refusal and the not-yet-live buffer all apply to both
    /// families; the family-typed payload is built here, once.
    ///
    /// The router's verdict comes back whole: `Applied(pool_id)` on direct
    /// application, `Buffered(kind)` when the delta was staged (`Pump` for a
    /// quarantined or unregistered pool at the live phase — the staging that
    /// keeps a tick mutation alive across registration), `NoOp(reason)` for a
    /// sanctioned drop. Flattening here would make staging
    /// indistinguishable from a drop.
    ///
    /// # Errors
    ///
    /// [`PoolRefusal::FamilyMismatch`] when the payload's family differs from
    /// the key's.
    pub fn apply_liquidity_update(
        &mut self,
        key: PoolLookup,
        update: PoolLiquidityUpdate,
    ) -> Result<ApplyOutcome, PoolRefusal> {
        let event = match key {
            PoolLookup::Address(_) => {
                BufferedPoolEvent::V3(BufferedV3PoolEvent::Liquidity(BufferedV3LiquidityUpdate {
                    tick_lower: update.tick_lower,
                    tick_upper: update.tick_upper,
                    liquidity_delta: update.liquidity_delta,
                    block_number: update.block_number,
                }))
            }
            PoolLookup::V4 { .. } => {
                BufferedPoolEvent::V4(BufferedV4PoolEvent::Liquidity(BufferedV4LiquidityUpdate {
                    tick_lower: update.tick_lower,
                    tick_upper: update.tick_upper,
                    liquidity_delta: update.liquidity_delta,
                    block_number: update.block_number,
                }))
            }
        };
        self.route_event(Phase::Live, key, event, &[])
    }

    /// Stage a backfill liquidity-position change into the pool's backfill
    /// buffer, for either key shape.
    pub fn buffer_backfill_liquidity_update(
        &mut self,
        key: PoolLookup,
        update: PoolLiquidityUpdate,
    ) {
        match key {
            PoolLookup::Address(pool_address) => {
                self.concentrated_liquidity
                    .buffer_backfill_v3_liquidity_update(
                        &mut self.pools,
                        pool_address,
                        update.tick_lower,
                        update.tick_upper,
                        update.liquidity_delta,
                        update.block_number,
                    );
            }
            PoolLookup::V4 {
                pool_manager,
                pool_id,
            } => {
                self.concentrated_liquidity
                    .buffer_backfill_v4_liquidity_update(
                        &mut self.pools,
                        pool_manager,
                        pool_id,
                        update.tick_lower,
                        update.tick_upper,
                        update.liquidity_delta,
                        update.block_number,
                    );
            }
        }
    }

    /// Delegates to the concentrated liquidity orchestration capability - see [`ConcentratedLiquidityOrchestration::set_snapshot_seed_block`].
    pub fn set_snapshot_seed_block(&mut self, s: Option<u64>) {
        self.concentrated_liquidity.set_snapshot_seed_block(s);
    }
    /// Overwrite a registered pool's state from one source snapshot, for either
    /// key shape.
    ///
    /// The payload names the family and the key names the pool; a disagreement
    /// is a caller bug, reported rather than silently ignored (an unregistered
    /// pool is the one expected refusal). A wholesale sync replaces the scalars
    /// AND the tick map from one snapshot, so both clocks move to the snapshot's
    /// block together — a sanctioned reset, not the incremental backward stamp
    /// the two-stamp rule forbids.
    ///
    /// # Errors
    ///
    /// [`PoolRefusal::FamilyMismatch`] when the payload's family differs
    /// from the key's, [`PoolRefusal::Unregistered`] when the key names no
    /// registered pool.
    pub fn sync_pool_state(
        &mut self,
        key: PoolLookup,
        update: PoolStateUpdate,
    ) -> Result<(), PoolRefusal> {
        match (key, update) {
            (PoolLookup::Address(address), PoolStateUpdate::V3(update)) => self
                .concentrated_liquidity
                .sync_pool_state::<V3Family>(&mut self.pools, &address, update)
                .then_some(())
                .ok_or(PoolRefusal::Unregistered),
            (
                PoolLookup::V4 {
                    pool_manager,
                    pool_id,
                },
                PoolStateUpdate::V4(update),
            ) => self
                .concentrated_liquidity
                .sync_pool_state::<V4Family>(&mut self.pools, &(pool_manager, pool_id), update)
                .then_some(())
                .ok_or(PoolRefusal::Unregistered),
            (key, update) => Err(PoolRefusal::FamilyMismatch {
                key: key.family(),
                value: update.family(),
            }),
        }
    }

    /// Delegates to the concentrated liquidity orchestration capability - see [`ConcentratedLiquidityOrchestration::stage_word_fetch_by_pool_id`].
    #[must_use]
    pub fn stage_word_fetch_by_pool_id(
        &self,
        pool_id: u64,
        word: i32,
        block: u64,
        retried: bool,
    ) -> Option<StagedWordFetch> {
        self.concentrated_liquidity.stage_word_fetch_by_pool_id(
            &self.pools,
            pool_id,
            word,
            block,
            retried,
        )
    }

    /// Delegates to the concentrated liquidity orchestration capability - see [`ConcentratedLiquidityOrchestration::install_word_fetch`].
    pub fn install_word_fetch(
        &mut self,
        staged: &StagedWordFetch,
        fetched: &::degenbot_pools::CheckedWord,
    ) -> ::degenbot_pools::WordFetchOutcome {
        self.concentrated_liquidity
            .install_word_fetch(&mut self.pools, staged, fetched)
    }

    /// Arm or disarm the strict sparse-word apply gate for this state owner
    /// (`pump.strict_sparse_apply`). The construction default comes from the
    /// installed config; this is the per-instance override for a consumer that
    /// fronts one process config with several state owners (and for tests,
    /// which must not re-govern the process-wide snapshot).
    pub fn set_strict_sparse_apply(&mut self, on: bool) {
        self.concentrated_liquidity.set_strict_sparse_apply(on);
    }

    /// Whether the strict sparse-word apply gate is armed for this state owner.
    #[must_use]
    pub fn strict_sparse_apply(&self) -> bool {
        self.concentrated_liquidity.strict_sparse_apply
    }

    /// Delegates to the concentrated liquidity orchestration capability - see [`ConcentratedLiquidityOrchestration::ensure_word_known_by_pool_id`].
    pub fn ensure_word_known_by_pool_id(&mut self, pool_id: u64, word: i32, block: u64) -> bool {
        self.concentrated_liquidity.ensure_word_known_by_pool_id(
            &mut self.pools,
            pool_id,
            word,
            block,
        )
    }

    /// Delegates to the concentrated liquidity orchestration capability - see [`ConcentratedLiquidityOrchestration::merge_tick_word`].
    pub fn merge_tick_word(
        &mut self,
        pool_id: u64,
        fetched: &::degenbot_pools::FetchedTickWord,
    ) -> bool {
        self.concentrated_liquidity
            .merge_tick_word(&mut self.pools, pool_id, fetched)
    }

    /// Delegates to the concentrated liquidity orchestration capability - see [`ConcentratedLiquidityOrchestration::register_v4_state_view`].
    pub fn register_state_view(&mut self, pool_manager: Address, state_view: Address) {
        self.concentrated_liquidity
            .register_v4_state_view(pool_manager, state_view);
    }

    /// The V4 `StateView` contract registered for the manager of the V4 pool
    /// `pool_id` — the live-scalar read target a refresh fetches from. `None`
    /// when the id is not a V4 pool or no state view was registered for its
    /// manager.
    ///
    /// The pool-keyed twin of [`ConcentratedLiquidityOrchestration::state_view_for`] (which is
    /// manager-keyed and crate-internal): a refresh holds a `pool_id` and needs
    /// the state view the pool's own manager registered.
    #[must_use]
    pub fn state_view_for_pool(&self, pool_id: u64) -> Option<Address> {
        let manager = self
            .pool_slice::<degenbot_pools::V4PoolIdentity>(pool_id)?
            .pool_manager;
        self.concentrated_liquidity.state_view_for(manager)
    }

    /// Delegates to the concentrated liquidity orchestration capability - see [`ConcentratedLiquidityOrchestration::register_v4_pool`].
    ///
    /// # Errors
    ///
    /// Returns [`RegisterV4PoolError`] on a spec-bound violation, a dynamic
    /// or un-encodable static fee, or an already-registered
    /// `(pool_manager, pool_id)` key.
    pub fn register_v4_pool(
        &mut self,
        params: &RegisterV4PoolParams,
    ) -> Result<u64, RegisterV4PoolError> {
        self.concentrated_liquidity
            .register_v4_pool(&mut self.pools, params)
    }

    /// Coverage flag for the address-registered (V3) pool behind `pool_id`
    /// (`Tracked` = complete tick data, `Sparse` = none), read through the
    /// family read module's observe door. `None` for an unregistered / non-V3
    /// id. The registration-lifecycle reads this up-front to branch the
    /// verify-lifecycle (Sparse stays `Live`, no RPC); the address and V4 key
    /// shapes resolve into this one interior.
    #[must_use]
    pub(crate) fn coverage_by_pool_id(&self, pool_id: u64) -> Option<PoolTickCoverage> {
        match self.pools.get(pool_id) {
            Some(PoolEntry::V3(p)) => {
                let design = concentrated_liquidity::ConcentratedLiquidityState::V3 {
                    state: &p.1,
                    scalars: concentrated_liquidity::DesignScalars::new(p.0.fee, p.0.tick_spacing),
                };
                match concentrated_liquidity::observe(design, Observe::Coverage) {
                    Observed::Coverage(coverage) => Some(coverage),
                    _ => unreachable!("the coverage probe answers"),
                }
            }
            _ => None,
        }
    }
    /// The pool's tick coverage (`Tracked` = complete tick data, `Sparse` = none),
    /// or `None` when the key names no registered concentrated-liquidity pool.
    ///
    /// The registration lifecycle reads this up-front to branch the verify
    /// path: a Sparse pool stays `Live` and never needs an RPC.
    #[must_use]
    pub fn pool_coverage(&self, key: PoolLookup) -> Option<PoolTickCoverage> {
        match key {
            PoolLookup::Address(address) => self
                .pool_id_by_address(&address)
                .and_then(|pool_id| self.coverage_by_pool_id(pool_id)),
            PoolLookup::V4 {
                pool_manager,
                pool_id,
            } => {
                let pid = self.pools.pool_id_by_key(pool_manager, &pool_id)?;
                match self.pools.get(pid) {
                    Some(PoolEntry::V4(p)) => {
                        let design = concentrated_liquidity::ConcentratedLiquidityState::V4 {
                            state: &p.1,
                            scalars: concentrated_liquidity::DesignScalars::new(
                                p.0.pool_key.fee,
                                p.0.pool_key.tick_spacing,
                            ),
                            hooks: p.0.pool_key.hooks,
                        };
                        match concentrated_liquidity::observe(design, Observe::Coverage) {
                            Observed::Coverage(coverage) => Some(coverage),
                            _ => unreachable!("the coverage probe answers"),
                        }
                    }
                    _ => None,
                }
            }
        }
    }

    /// Delegates to the concentrated liquidity orchestration capability - see [`ConcentratedLiquidityOrchestration::release_all_v3_v4_quarantined`].
    pub fn release_all_quarantined(&mut self) {
        self.concentrated_liquidity
            .release_all_v3_v4_quarantined(&mut self.pools);
    }

    /// The pool registered under this `(pool_manager, pool_id)` pair — the V4
    /// key's one resolution, answered by the pool table.
    #[must_use]
    pub fn pool_id_by_key(
        &self,
        pool_manager: Address,
        pool_id: &degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) -> Option<u64> {
        self.pools.pool_id_by_key(pool_manager, pool_id)
    }

    /// The number of pools registered under a `(pool_manager, pool_id)` key.
    #[must_use]
    pub fn registered_v4_pool_count(&self) -> usize {
        self.pools.v4_count()
    }

    /// Delegates to the concentrated liquidity orchestration capability - see [`ConcentratedLiquidityOrchestration::try_registered_v4`].
    #[must_use]
    pub fn try_registered(
        &self,
        pool_manager: Address,
        pool_id: &degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) -> Option<RegisteredV4> {
        self.concentrated_liquidity
            .try_registered_v4(&self.pools, pool_manager, pool_id)
    }

    /// The address-keyed swap apply (the payload shape is the V3 scalar form —
    /// V4 swaps carry [`V4SwapUpdate`] through [`Self::apply_swap_update`]).
    /// Routes through [`Self::route_event`] at `Phase::Live`: the routing table
    /// decides apply-vs-buffer-vs-drop, and its verdict is returned unflattened
    /// (`Applied` / `Buffered` / `NoOp`). `tick_priors` overlay only on direct
    /// application.
    ///
    /// # Errors
    ///
    /// Never: an address key with a V3 payload cannot disagree on family, so
    /// the row is infallible by construction — the `Result` shape matches the
    /// key-shaped doors it sits beside.
    pub fn apply_swap(
        &mut self,
        pool_address: Address,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)],
    ) -> Result<ApplyOutcome, PoolRefusal> {
        trace_apply_swap_v3(pool_address, sqrt_price_x96, liquidity, tick, block_number);
        self.route_event(
            Phase::Live,
            PoolLookup::from_address(pool_address),
            BufferedPoolEvent::V3(BufferedV3PoolEvent::Swap(BufferedV3SwapEvent {
                sqrt_price_x96,
                liquidity,
                tick,
                block_number,
            })),
            tick_priors,
        )
    }

    /// The update-payload swap apply (the update type carries the design).
    /// Routes through [`Self::route_event`] at `Phase::Live`; the verdict is
    /// returned unflattened (`Applied` / `Buffered` / `NoOp`).
    /// `update.tick_priors` overlay only on direct application (pump/backfill
    /// pass an empty slice).
    ///
    /// # Errors
    ///
    /// Never: a `(pool_manager, pool_id)` key with a V4 payload cannot
    /// disagree on family, so the row is infallible by construction — the
    /// `Result` shape matches the key-shaped doors it sits beside.
    pub fn apply_swap_update(
        &mut self,
        update: &V4SwapUpdate,
        block_number: u64,
    ) -> Result<ApplyOutcome, PoolRefusal> {
        let pool_id_hex = alloy::hex::encode_prefixed(update.pool_id);
        trace_apply_swap_v4(
            update.pool_manager,
            &pool_id_hex,
            update.sqrt_price_x96,
            update.liquidity,
            update.tick,
            block_number,
        );
        self.route_event(
            Phase::Live,
            PoolLookup::V4 {
                pool_manager: update.pool_manager,
                pool_id: update.pool_id,
            },
            BufferedPoolEvent::V4(BufferedV4PoolEvent::Swap(BufferedV4SwapEvent {
                sqrt_price_x96: update.sqrt_price_x96,
                liquidity: update.liquidity,
                tick: update.tick,
                block_number,
            })),
            &update.tick_priors,
        )
    }

    /// Route one decoded concentrated-liquidity event through the apply path,
    /// for either key shape.
    ///
    /// The event carries its family and the key names the pool; a disagreement
    /// is a caller bug, refused rather than dropped (a drop would look like one
    /// of the sanctioned `NoOp` trust decisions).
    ///
    /// # Errors
    ///
    /// [`PoolRefusal::FamilyMismatch`] when the event's family differs from the
    /// key's.
    pub fn route_event(
        &mut self,
        phase: Phase,
        key: PoolLookup,
        event: BufferedPoolEvent,
        swap_priors: &[(i32, TickInfo)],
    ) -> Result<ApplyOutcome, PoolRefusal> {
        match (key, event) {
            (PoolLookup::Address(pool_address), BufferedPoolEvent::V3(event)) => Ok(self
                .concentrated_liquidity
                .route_event(&mut self.pools, phase, pool_address, event, swap_priors)),
            (
                PoolLookup::V4 {
                    pool_manager,
                    pool_id,
                },
                BufferedPoolEvent::V4(event),
            ) => Ok(self.concentrated_liquidity.route_event_by_key(
                &mut self.pools,
                phase,
                pool_manager,
                pool_id,
                event,
                swap_priors,
            )),
            (key, event) => Err(PoolRefusal::FamilyMismatch {
                key: key.family(),
                value: event.family(),
            }),
        }
    }

    /// The buffered-event depth probe for the address-keyed buffer.
    #[must_use]
    pub fn buffered_event_count(&self, address: &Address) -> usize {
        self.concentrated_liquidity.buffered_v3_event_count(address)
    }

    /// Snapshot all key-registered (V4) pool state for verification.
    #[must_use]
    pub fn registered_pools_snapshot(&self) -> HashMap<u64, (V4PoolIdentity, V4PoolState)> {
        self.pools
            .entries()
            .filter_map(|(id, e)| match e {
                PoolEntry::V4(p) => Some((id, (p.0.clone(), p.1.clone()))),
                PoolEntry::V2(..)
                | PoolEntry::V3(..)
                | PoolEntry::Curve(..)
                | PoolEntry::BalancerWeighted(..)
                | PoolEntry::BalancerStable(..)
                | PoolEntry::AerodromeV2(..) => None,
            })
            .collect()
    }

    /// Delegates to the concentrated liquidity orchestration capability - see [`ConcentratedLiquidityOrchestration::snapshot_seed_block`].
    #[must_use]
    pub const fn snapshot_seed_block(&self) -> Option<u64> {
        self.concentrated_liquidity.snapshot_seed_block()
    }
}

/// One-read snapshot of an ALREADY-registered V4 pool's return-relevant
/// state — the numeric pool id, immutable registration key, core-tracked
/// protocol fee, and registration coverage. Produced by
/// [`BotState::try_registered_v4`] for the `PyO3` `build_v4_pool` fast path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisteredV4 {
    /// The auto-assigned numeric pool id in the shared core.
    pub pool_id: u64,
    /// The immutable V4 pool key (`currency0/currency1/fee/tick_spacing/hooks`).
    pub pool_key: V4PoolKey,
    /// The packed `slot0.protocolFee` tracked by the core's state machine.
    pub protocol_fee: u32,
    /// `Sparse` or `Tracked` coverage recorded at registration.
    pub coverage: PoolTickCoverage,
}

#[cfg(test)]
mod registered_v4_fast_path_tests {
    use alloy::primitives::{Address, U256};
    use degenbot_decoders::v4_swap_decoder::V4PoolId;
    use degenbot_pools::PoolTickCoverage;
    use degenbot_pools::V4PoolKey;
    use hashbrown::HashMap;

    use super::{BotState, RegisterV4PoolParams};

    #[expect(clippy::expect_used)]
    fn register_v4(
        core: &mut BotState,
        pid: [u8; 32],
        coverage: PoolTickCoverage,
        protocol_fee: u32,
    ) -> u64 {
        core.register_v4_pool(&RegisterV4PoolParams {
            pool_manager: Address::from([0x44u8; 20]),
            pool_id: pid,
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee: 500,
                tick_spacing: 10,
                hooks: Address::from([0x22u8; 20]),
            },
            state_view: Address::ZERO,
            hook_flags: 0,
            protocol_fee,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            tick_data_block: None,
            coverage,
            fetcher: None,
            known_bitmap_words: Vec::new(),
        })
        .expect("test setup: V4 registration")
    }

    /// The 2026-08-28 missed-WS-pong fix: an already-registered V4 pool must
    /// re-resolve from the core alone (identity + fees + coverage) so the
    /// `PyO3` builder can skip the RPC build pipeline entirely.
    #[test]
    #[expect(clippy::expect_used)]
    fn existing_registration_returns_identity_fees_and_coverage_tracked() {
        let mut core = BotState::new();
        let pm = Address::from([0x44u8; 20]);
        let pid: V4PoolId = [0xeeu8; 32];
        let registered = register_v4(&mut core, pid, PoolTickCoverage::Tracked, 0x0102);

        let existing = core
            .try_registered(pm, &pid)
            .expect("registered pool must resolve by (pool_manager, pool_id)");
        assert_eq!(existing.pool_id, registered, "must be the same numeric id");
        let key = &existing.pool_key;
        assert_eq!(key.currency0, Address::ZERO);
        assert_eq!(key.currency1, Address::from([1u8; 20]));
        assert_eq!(key.fee, 500);
        assert_eq!(key.tick_spacing, 10);
        assert_eq!(key.hooks, Address::from([0x22u8; 20]));
        assert_eq!(existing.protocol_fee, 0x0102);
        assert_eq!(existing.coverage, PoolTickCoverage::Tracked);
    }

    #[test]
    #[expect(clippy::expect_used)]
    fn existing_registration_reports_sparse_coverage() {
        let mut core = BotState::new();
        let pm = Address::from([0x44u8; 20]);
        let pid: V4PoolId = [0xeeu8; 32];
        let _ = register_v4(&mut core, pid, PoolTickCoverage::Sparse, 0);

        let existing = core
            .try_registered(pm, &pid)
            .expect("registered pool must resolve");
        assert_eq!(existing.coverage, PoolTickCoverage::Sparse);
    }

    #[test]
    fn unknown_key_resolves_none() {
        let mut core = BotState::new();
        let pm = Address::from([0x44u8; 20]);
        let _ = register_v4(&mut core, [0xeeu8; 32], PoolTickCoverage::Sparse, 0);

        assert_eq!(
            core.try_registered(pm, &[0xffu8; 32]),
            None,
            "an unregistered pool_id under the same manager must miss"
        );
        assert_eq!(
            core.try_registered(Address::from([0x45u8; 20]), &[0xeeu8; 32]),
            None,
            "an unknown pool_manager must miss"
        );
    }
}

#[cfg(test)]
mod known_word_dispatcher_tests {
    use hashbrown::HashMap;

    use alloy::primitives::{Address, U256};

    use degenbot_pools::{PoolTickCoverage, V3PoolState};

    use super::{BotState, RegisterV3PoolParams};

    #[expect(clippy::expect_used)]
    fn register_v3(coverage: PoolTickCoverage) -> (BotState, u64) {
        let mut core = BotState::new();
        let pool_id = core
            .register_v3_pool(&RegisterV3PoolParams {
                address: Address::ZERO,
                token0: Address::ZERO,
                token1: Address::from([1u8; 20]),
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 10_000_000_000_000u128,
                tick: 0,
                tick_data: HashMap::new(),
                update_block: 0,
                tick_data_block: None,
                coverage,
                fetcher: None,
                ..Default::default()
            })
            .expect("test setup: V3 registration");
        (core, pool_id)
    }

    #[test]
    #[expect(clippy::expect_used)]
    fn mark_bitmap_words_known_by_pool_id_sparse_marks() {
        let (mut core, pool_id) = register_v3(PoolTickCoverage::Sparse);
        assert!(
            core.mark_bitmap_words_known_by_pool_id(pool_id, &[7]),
            "dispatcher must report a concentrated liquidity apply"
        );
        let state = core
            .pool_slice::<degenbot_pools::V3PoolState>(pool_id)
            .expect("registered");
        assert!(state.known_bitmap_words.contains(&7));
    }

    #[test]
    #[expect(clippy::expect_used)]
    fn mark_bitmap_words_known_by_pool_id_tracked_noop() {
        let (mut core, pool_id) = register_v3(PoolTickCoverage::Tracked);
        let _ = core.mark_bitmap_words_known_by_pool_id(pool_id, &[7]);
        let state = core
            .pool_slice::<degenbot_pools::V3PoolState>(pool_id)
            .expect("registered");
        assert!(
            !state.known_bitmap_words.contains(&7),
            "Tracked pools never record checked words"
        );
    }

    #[test]
    fn mark_bitmap_words_known_by_pool_id_unregistered_noop() {
        let (mut core, _pool_id) = register_v3(PoolTickCoverage::Sparse);
        assert!(!core.mark_bitmap_words_known_by_pool_id(99, &[7]));
    }

    #[test]
    #[expect(clippy::expect_used)]
    fn apply_liquidity_update_does_not_mark_words_known() {
        // Discipline (grilling decision): a word becomes known only when its
        // WHOLE tick set is established (fetch-merge / full sync / passed
        // checked words) — an apply event must never mark the touched word.
        let (mut core, pool_id) = register_v3(PoolTickCoverage::Sparse);
        // A Mint initializing a tick in word 1 (compressed 256 -> tick 15360).
        assert!(core
            .apply_liquidity_update_by_pool_id(pool_id, 15360, 15420, 1, 5)
            .is_ok());
        let state = core
            .pool_slice::<degenbot_pools::V3PoolState>(pool_id)
            .expect("registered");
        assert!(
            state.tick_data.contains_key(&15360),
            "the Mint must initialize the boundary tick row"
        );
        let word = V3PoolState::word_of(15360, 60);
        assert!(
            !state.known_bitmap_words.contains(&word),
            "an apply event must not mark the touched word known"
        );
    }
}
