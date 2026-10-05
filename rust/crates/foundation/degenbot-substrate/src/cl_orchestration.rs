//! Concentrated-liquidity (CL) structural family — orchestration capability (V3 + V4).
//!
//! This module owns the CL family's state surfaces — V3/V4 registration +
//! apply, the CL-common dual liquidity buffer, the snapshot seeds, and the
//! coverage/quarantine/lifecycle state accessors — on [`ClOrchestration`],
//! the capability struct `BotState` composes as its `cl` field. The family
//! state types live in `degenbot-pools` (I/O-free, ADR-001), and the
//! `ConcentratedLiquidityPool(Mut)` trait is the CL family's unified seam.
//! The routing verbs the orchestration shares with the event pump live in
//! `cl_route.rs`.
//!
//! CL methods never touch `BotState`'s private fields: they reach the
//! family-agnostic pool tables through the [`RegistryCore`] view parameter
//! each caller passes — a split borrow that lets one call mutate capability
//! state and registry core together. `BotState`'s public CL surface stays on
//! `BotState` as one-line delegation wrappers (the delegating
//! composition-root impl at the bottom of this module).

use degenbot_core::diag;
use degenbot_core::op_warn;
use hashbrown::HashMap;

use alloy::primitives::{Address, U256};

use crate::cl_route::{
    pin_provenance_verdict, route_action, ApplyOutcome, BufferKind, EventKind, Phase,
    PinProvenance, PoolPresence, RouteAction,
};
use degenbot_pools::state_history::{ScalarPriors, TickBefore, V3BlockDelta};
use degenbot_pools::v3_state::{BufferedV3LiquidityUpdate, BufferedV3SwapEvent};
use degenbot_pools::v4_state::{
    BufferedV4LiquidityUpdate, BufferedV4SwapEvent, V4PoolKey, V4StateSync, V4_DYNAMIC_FEE_FLAG,
};

use super::apply_telemetry::{
    drain_dbg_log_buf, trace_apply_route_v3, trace_apply_route_v4, trace_apply_swap_v3,
    trace_apply_swap_v4,
};
use super::{
    BotState, BufferedV3PoolEvent, BufferedV4PoolEvent, ConcentratedLiquidityPoolMut, PoolEntry,
    PoolTickCoverage, RegisterV3PoolError, RegisterV3PoolParams, RegisterV4PoolError,
    RegisterV4PoolParams, RegistrationLifecycle, TickInfo, V3PoolIdentity, V3PoolState,
    V4PoolIdentity, V4PoolState, V4SwapUpdate,
};

use super::RegistryCore;

/// The concentrated-liquidity (V3 + V4) orchestration capability - the ONE
/// struct owning every CL-local state surface the state owner carries: the
/// dual liquidity buffers, the `(pool_manager, pool_id)` V4 registry, the
/// state-view registry, the keyed registration gate, the snapshot seed
/// block, the pump delivery cutoff, and the per-pool event horizons. The
/// capability's method set (this module) reaches the registry core through
/// the [`RegistryCore`] view its callers pass; `BotState` remains the
/// composition root and delegates its public CL surface here.
pub struct ClOrchestration {
    /// Dual-buffer for V3 liquidity (Mint/Burn) events awaiting pool
    /// registration (ADR-003: the accurate-state buffer lives on the state
    /// owner, not the dissolved `V3BlockEngine`).
    pub(crate) v3_buffer: ::degenbot_pools::liquidity_event_buffer::LiquidityEventBuffer<
        Address,
        BufferedV3PoolEvent,
    >,
    /// Dual-buffer for V4 `ModifyLiquidity` events awaiting pool registration.
    /// Keyed by `(pool_manager, pool_id)`.
    pub(crate) v4_buffer: ::degenbot_pools::liquidity_event_buffer::LiquidityEventBuffer<
        (Address, degenbot_decoders::v4_swap_decoder::V4PoolId),
        BufferedV4PoolEvent,
    >,
    /// V4 pool registry: `(pool_manager, pool_id)` -> `pool_id` (single entry
    /// per pool - ADR-003 Option I: orientation derived at solve from
    /// `zero_for_one`, not stored as separate forward/reverse entries).
    pub(crate) v4_pool_ids: HashMap<(Address, degenbot_decoders::v4_swap_decoder::V4PoolId), u64>,
    /// Rust-owned V4 pool-manager -> `StateView` registry (ADR-005 / Option 2).
    /// The canonical V4 scalar state is read via the `StateView`'s
    /// `getSlot0`/`getLiquidity`, not `getPool(poolManager)` (which reverts on
    /// the canonical deployment). Keyed by `pool_manager`; each `V4PoolState`
    /// under a manager shares its manager's `StateView`. Seeded once per manager
    /// via `register_v4_state_view` (the driver reads it from the
    /// `pool_managers` DB row); the solver-state verifier reads it via
    /// [`ClOrchestration::state_view_for`].
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
}

impl ClOrchestration {
    /// An empty capability - every buffer, registry, and horizon starts
    /// unseeded; `BotState::with_journal_depth` composes it with a
    /// [`RegistryCore`].
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            v3_buffer: ::degenbot_pools::liquidity_event_buffer::LiquidityEventBuffer::new(),
            v4_buffer: ::degenbot_pools::liquidity_event_buffer::LiquidityEventBuffer::new(),
            v4_pool_ids: HashMap::new(),
            v4_state_views: HashMap::new(),
            registration_gate: crate::registration_gate::RegistrationGate::default(),
            snapshot_seed_block: None,
            pump_complete_cutoff: 0,
            v3_event_horizons: HashMap::new(),
            v4_event_horizons: HashMap::new(),
        }
    }
}

/// the staged fetch plan captured under a SHORT write — pool, word,
/// fetch context, the stored fetcher Arc, and the pool's tick-mutation
/// fingerprint the install re-validates.
#[derive(Debug)]
pub struct StagedWordFetch {
    pub pool_id: u64,
    pub word: i32,
    pub block: u64,
    fetcher: std::sync::Arc<dyn degenbot_pools::tick_fetch::TickWordFetcher>,
    fingerprint: (u64, usize, u64),
}

impl StagedWordFetch {
    /// Run the fetch WITHOUT any state lock held. The stored fetcher
    /// re-enters Python (`Python::attach` + the companion's web3 RPC), so
    /// this call is the multi-second window the fetch-under-write defect
    /// parked the whole pump inside.
    ///
    /// # Errors
    /// Propagates the stored fetcher's [`FetchTickWordError`] (RPC/transport
    /// or tick-word decode); the caller owns retry semantics, nothing panics.
    pub fn fetch(
        &self,
    ) -> Result<
        ::degenbot_pools::tick_fetch::FetchedTickWord,
        ::degenbot_pools::tick_fetch::FetchTickWordError,
    > {
        self.fetcher
            .fetch_missing_tick_word(self.pool_id, self.word, self.block)
    }
}

/// Install outcome: see [`BotState::install_word_fetch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallWordOutcome {
    /// The word merged (checked-empty included) — the fetch window saw no
    /// concurrent pool write.
    Merged,
    /// The pool was mutated during the fetch — the caller retries
    /// stage+fetch (bounded) rather than applying the overlay clobber.
    Raced,
    /// Pool gone or merge refused — the failure contract (the
    /// companion gate raises).
    Failed,
}

impl ClOrchestration {
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
        reg: &mut RegistryCore,
        params: &RegisterV3PoolParams,
    ) -> Result<u64, RegisterV3PoolError> {
        use ::degenbot_pools::spec_bounds as sb;
        sb::validate_sqrt_price(params.sqrt_price_x96)
            .map_err(RegisterV3PoolError::SpecViolation)?;
        sb::validate_tick(params.tick).map_err(RegisterV3PoolError::SpecViolation)?;
        sb::validate_v3_fee(params.fee).map_err(RegisterV3PoolError::SpecViolation)?;
        sb::validate_tick_spacing(params.tick_spacing)
            .map_err(RegisterV3PoolError::SpecViolation)?;

        if reg.pool_addresses.contains_key(&params.address) {
            return Err(RegisterV3PoolError::AlreadyRegistered {
                address: params.address,
            });
        }

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

        let pool_id = reg.next_pool_id;
        reg.next_pool_id += 1;
        let address = params.address;

        // the `seed_from_store` path is retired — the DB
        // seeding is handled by the Db arm of `assemble_v3_tick_map` (held
        // snapshot tx). Just clone + flow the params through.
        let params = params.clone();
        let (identity, state) = V3PoolState::from_params(params, reg.journal_depth);
        reg.pools
            .insert(pool_id, PoolEntry::V3(Box::new((identity, state))));
        reg.pool_addresses.insert(address, pool_id);

        Ok(pool_id)
    }
    /// Update a V3 pool's state from a Swap event.
    ///
    /// Looks up the pool by contract address. No-op if the pool is not registered.
    /// Stashes scalar "before" values (and any provided per-tick priors) in the
    /// reorg journal before updating. Kept as the `PyBot` entry; the live
    /// pump path uses [`apply_v3_swap`](Self::apply_v3_swap) (which returns the
    /// affected `pool_id` and overlays `tick_priors` into `tick_data`).
    pub(crate) fn update_v3_pool(
        &mut self,
        reg: &mut RegistryCore,
        pool_address: Address,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: Vec<(i32, TickBefore)>,
    ) {
        let Some(&pool_id) = reg.pool_addresses.get(&pool_address) else {
            return;
        };

        let Some(state) = reg
            .pools
            .get_mut(&pool_id)
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
    /// Event-witnessed horizon for a V3 pool:
    /// highest block of any routed event for this pool. `0` = none witnessed.
    #[must_use]
    pub(crate) fn v3_event_horizon(&self, pool_address: &Address) -> u64 {
        self.v3_event_horizons
            .get(pool_address)
            .copied()
            .unwrap_or(0)
    }
    fn note_v3_event_block(&mut self, pool_address: Address, block_number: u64) {
        let e = self.v3_event_horizons.entry(pool_address).or_insert(0);
        *e = (*e).max(block_number);
    }
    /// Event-witnessed horizon for a V4 pool.
    #[must_use]
    pub(crate) fn v4_event_horizon(
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
    /// Resolve a V3 pool's routing presence (`cl_route` axis).
    fn v3_presence(&self, reg: &RegistryCore, pool_address: &Address) -> PoolPresence {
        let lifecycle =
            reg.pool_addresses
                .get(pool_address)
                .and_then(|&id| match reg.pools.get(&id) {
                    Some(PoolEntry::V3(p)) => Some(p.1.registration_lifecycle),
                    _ => None,
                });
        PoolPresence::from_lifecycle(lifecycle)
    }
    /// THE single decision point for a decoded V3 event (`cl_route` table).
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
    pub(crate) fn route_v3_event(
        &mut self,
        reg: &mut RegistryCore,
        phase: Phase,
        pool_address: Address,
        event: BufferedV3PoolEvent,
        swap_priors: &[(i32, TickInfo)],
    ) -> ApplyOutcome {
        let kind = match &event {
            BufferedV3PoolEvent::Swap(_) => EventKind::ScalarRefresh,
            BufferedV3PoolEvent::Liquidity(_) => EventKind::TickMutation,
        };
        let presence = self.v3_presence(reg, &pool_address);
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
                let Some(pool_id) = reg.pool_addresses.get(&pool_address).copied() else {
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
                        self.apply_v3_swap_by_pool_id(
                            reg,
                            pool_id,
                            sqrt_price_x96,
                            liquidity,
                            tick,
                            block_number,
                            swap_priors,
                        );
                    }
                    BufferedV3PoolEvent::Liquidity(BufferedV3LiquidityUpdate {
                        tick_lower,
                        tick_upper,
                        liquidity_delta,
                        block_number,
                    }) => {
                        self.apply_v3_liquidity_update_by_pool_id(
                            reg,
                            pool_id,
                            tick_lower,
                            tick_upper,
                            liquidity_delta,
                            block_number,
                        );
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
    /// Apply a V3 `Swap` event (ADR-003 live path).
    ///
    /// Thin adapter over [`Self::route_v3_event`] at `Phase::Live`: the routing
    /// table decides apply-vs-buffer-vs-drop; this entry flattens the outcome
    /// to the historical `Option<pool_id>` shape (`None` = buffered or
    /// dropped). `tick_priors` overlay only on direct application.
    pub(crate) fn apply_v3_swap(
        &mut self,
        reg: &mut RegistryCore,
        pool_address: Address,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)],
    ) -> Option<u64> {
        trace_apply_swap_v3(pool_address, sqrt_price_x96, liquidity, tick, block_number);
        match self.route_v3_event(
            reg,
            Phase::Live,
            pool_address,
            BufferedV3PoolEvent::Swap(BufferedV3SwapEvent {
                sqrt_price_x96,
                liquidity,
                tick,
                block_number,
            }),
            tick_priors,
        ) {
            ApplyOutcome::Applied(pool_id) => Some(pool_id),
            ApplyOutcome::Buffered(_) | ApplyOutcome::NoOp(_) => None,
        }
    }
    /// Apply a V3 Swap event keyed by the handle's `pool_id` (plan-101 slice 8a).
    ///
    /// Same semantics as [`apply_v3_swap`] but skips address resolution —
    /// the `PyLiquidityPool` handle already holds the canonical `pool_id`, so
    /// this is the one-lock, one-lookup path the handle uses.
    pub(crate) fn apply_v3_swap_by_pool_id(
        &mut self,
        reg: &mut RegistryCore,
        pool_id: u64,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)],
    ) -> Option<u64> {
        let (_identity, state) = reg.pools.get_mut(&pool_id).and_then(PoolEntry::v3_mut)?;
        state.apply_swap(sqrt_price_x96, liquidity, tick, block_number, tick_priors);
        Some(pool_id)
    }
    /// Apply a V3 liquidity update (Mint/Burn) — thin adapter over
    /// [`Self::route_v3_event`] at `Phase::Live`. Unregistered pools
    /// stage into the PUMP buffer here so late registration captures them;
    /// under the old funnel this row was a silent drop.
    pub(crate) fn apply_v3_liquidity_update(
        &mut self,
        reg: &mut RegistryCore,
        pool_address: Address,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) -> Option<u64> {
        match self.route_v3_event(
            reg,
            Phase::Live,
            pool_address,
            BufferedV3PoolEvent::Liquidity(BufferedV3LiquidityUpdate {
                tick_lower,
                tick_upper,
                liquidity_delta,
                block_number,
            }),
            &[],
        ) {
            ApplyOutcome::Applied(pool_id) => Some(pool_id),
            ApplyOutcome::Buffered(_) | ApplyOutcome::NoOp(_) => None,
        }
    }
    /// V3 liquidity update keyed by the handle's `pool_id` (plan-101 slice 8a).
    ///
    /// Skips address resolution — the `PyLiquidityPool` handle holds the
    /// canonical `pool_id`, so this is the one-lock, one-lookup path. Registered
    /// pools only (no buffering — the handle's pool is necessarily registered).
    pub(crate) fn apply_v3_liquidity_update_by_pool_id(
        &mut self,
        reg: &mut RegistryCore,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) -> Option<u64> {
        let (_identity, state) = reg.pools.get_mut(&pool_id).and_then(PoolEntry::v3_mut)?;
        state.apply_liquidity_update(tick_lower, tick_upper, liquidity_delta, block_number);
        Some(pool_id)
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
        reg: &mut RegistryCore,
        pool_id: u64,
        tick_data: HashMap<i32, TickInfo>,
        update_block: u64,
    ) -> bool {
        let Some(entry) = reg.pools.get_mut(&pool_id) else {
            return false;
        };
        match entry {
            // CL-family collapse (ADR-014 D2b): the 4-line replace body lives
            // once in `ConcentratedLiquidityPoolMut::replace_tick_data`; each
            // arm only reads its identity's `tick_spacing` (V3 carries it
            // directly, V4 nests it in `pool_key`) and delegates. The
            // `_ => false` arm is the single non-CL / unregistered no-op.
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
    /// Record `words` as known on the CL pool behind `pool_id` — the
    /// write-path twin of [`Self::sync_tick_data_by_pool_id`] (arch-review
    /// cand 4, T1). Sparse pools only: the trait writer no-ops for Tracked
    /// (their bitmap is complete). The FFI `update_tick_data` seam calls
    /// both under one write guard: tick rows replaced first, then the
    /// caller's checked words recorded (the contract that retires the
    /// companion's `_bitmap_override` shadow).
    ///
    /// Returns `false` for V2 / non-CL / unregistered (the family-dispatch
    /// silent no-op contract).
    #[must_use]
    pub(crate) fn mark_bitmap_words_known_by_pool_id(
        &mut self,
        reg: &mut RegistryCore,
        pool_id: u64,
        words: &[i32],
    ) -> bool {
        let Some(entry) = reg.pools.get_mut(&pool_id) else {
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
        reg: &mut RegistryCore,
        pool_address: Address,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) {
        if let Some(&key) = reg.pool_addresses.get(&pool_address) {
            if let Some(state) = reg
                .pools
                .get_mut(&key)
                .and_then(PoolEntry::v3_mut)
                .map(|(_, s)| s)
            {
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
    /// Apply all buffered **backfill** V3 events for a pool address.
    /// Call this during registration, after `register_v3_pool` and before
    /// [`apply_pump_buffer_v3`](Self::apply_pump_buffer_v3). No-op if there are
    /// none. The post-call state is at the backfill boundary (a deterministic
    /// point suitable for verification cloning).
    ///
    /// Each buffered Mint/Burn pushes a tick-only `V3BlockDelta` (carrying the
    /// boundary-tick priors) and advances `state.update_block` — mirroring the
    /// live-path [`apply_v3_liquidity_update_by_pool_id`]. Pre-fix these
    /// appliers mutated `tick_data` only, so the buffered events were invisible
    /// to `restore_before_block` and `update_block` stayed frozen at the
    /// registration block.
    pub(crate) fn apply_backfill_buffer_v3(&mut self, reg: &mut RegistryCore, address: &Address) {
        // Drain diagnostics: per-event apply at DEBUG on `pump` (the
        // drain-dbg gate is retired). Diagnoses same-block Mint+Burn
        // net-zero races where one half is lost between fetch and drain.
        let Some(&key) = reg.pool_addresses.get(address) else {
            diag!(domain = pump, pool_addr = %format!("{address:x}"), "backfill NOT REGISTERED");
            return;
        };
        let Some(buffered) = self.v3_buffer.drain_backfill(address) else {
            diag!(domain = pump, pool_addr = %format!("{address:x}"), "backfill EMPTY");
            return;
        };
        diag!(domain = pump, pool_addr = %format!("{address:x}"),
            count = buffered.len(),
            "backfill"
        );
        for update in buffered {
            match &update {
                BufferedV3PoolEvent::Liquidity(u) => {
                    diag!(domain = pump, pool_addr = %format!("{address:x}"),
                        tick_lower = u.tick_lower,
                        tick_upper = u.tick_upper,
                        delta = u.liquidity_delta,
                        block = u.block_number,
                        "backfill apply liq"
                    );
                }
                BufferedV3PoolEvent::Swap(s) => {
                    diag!(domain = pump, pool_addr = %format!("{address:x}"),
                        liquidity = s.liquidity,
                        tick = s.tick,
                        block = s.block_number,
                        "backfill apply swap"
                    );
                }
            }
            if let Some(state) = reg
                .pools
                .get_mut(&key)
                .and_then(PoolEntry::v3_mut)
                .map(|(_, s)| s)
            {
                let ub_before = state.update_block;
                Self::apply_buffered_v3_event(state, update);
                if state.update_block < ub_before {
                    op_warn!(domain = state, pool_addr = %format!("{address:x}"),
                        ub_before,
                        ub_after = state.update_block,
                        "update_block REWIND (backfill)"
                    );
                }
            }
        }
    }
    /// Apply all buffered **pump** V3 events for a pool address.
    /// Call this during registration, after [`apply_backfill_buffer_v3`].
    ///
    /// Same journal + `update_block` contract as
    /// [`apply_backfill_buffer_v3`] — see its docs.
    pub(crate) fn apply_pump_buffer_v3(&mut self, reg: &mut RegistryCore, address: &Address) {
        let Some(&key) = reg.pool_addresses.get(address) else {
            diag!(domain = pump, pool_addr = %format!("{address:x}"), "pump NOT REGISTERED");
            return;
        };
        // drain ONLY fully-completed blocks. The cutoff is the pump's
        // `StageMachine` tombstone cutoff — a block is complete when
        // the first log of N+1 closes N; a drain mid-block would pin
        // `update_block=N` missing a later same-block log. Events for the
        // in-progress block stay buffered.
        let cutoff = self.pump_complete_cutoff;
        if cutoff == 0 {
            diag!(domain = pump, pool_addr = %format!("{address:x}"), "pump NO-COMPLETE (no tombstone yet)");
            return;
        }
        let Some(buffered) = self.v3_buffer.drain_pump_completed(address, cutoff) else {
            diag!(domain = pump, pool_addr = %format!("{address:x}"), "pump EMPTY (no completed blocks)");
            return;
        };
        diag!(domain = pump, pool_addr = %format!("{address:x}"), count = buffered.len(), "pump");
        for update in buffered {
            match &update {
                BufferedV3PoolEvent::Liquidity(u) => {
                    diag!(domain = pump, pool_addr = %format!("{address:x}"),
                        tick_lower = u.tick_lower,
                        tick_upper = u.tick_upper,
                        delta = u.liquidity_delta,
                        block = u.block_number,
                        "pump apply liq"
                    );
                }
                BufferedV3PoolEvent::Swap(s) => {
                    diag!(domain = pump, pool_addr = %format!("{address:x}"),
                        liquidity = s.liquidity,
                        tick = s.tick,
                        block = s.block_number,
                        "pump apply swap"
                    );
                }
            }
            if let Some(state) = reg
                .pools
                .get_mut(&key)
                .and_then(PoolEntry::v3_mut)
                .map(|(_, s)| s)
            {
                let ub_before = state.update_block;
                Self::apply_buffered_v3_event(state, update);
                if state.update_block < ub_before {
                    op_warn!(domain = state, pool_addr = %format!("{address:x}"),
                        ub_before,
                        ub_after = state.update_block,
                        "update_block REWIND (pump)"
                    );
                }
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
    /// arrived after). Mirrors the live-path apply methods:
    /// `Liquidity` → `state.apply_liquidity_update`, `Swap` →
    /// `state.apply_swap` (with `tick_priors: &[]` — the pump path never
    /// carries tick priors).
    fn apply_buffered_v3_event(state: &mut V3PoolState, event: BufferedV3PoolEvent) {
        match event {
            BufferedV3PoolEvent::Liquidity(u) => state.apply_liquidity_update(
                u.tick_lower,
                u.tick_upper,
                u.liquidity_delta,
                u.block_number,
            ),
            BufferedV3PoolEvent::Swap(s) => {
                state.apply_swap(s.sqrt_price_x96, s.liquidity, s.tick, s.block_number, &[]);
            }
        }
    }
    /// Mark `block` as fully processed by the pump (every V3 log for `block`
    /// Read a registered V3 pool's state by `pool_id`.
    ///
    /// The solve engine reads state by reference through this accessor
    /// (ADR-003: "Pool's authority over its own math") and calls
    /// `build_int_v3_sequence(zfo)` to build the per-hop state.
    #[must_use]
    pub(crate) fn get_v3_pool<'r>(
        &self,
        reg: &'r RegistryCore,
        pool_id: u64,
    ) -> Option<&'r V3PoolState> {
        reg.pools
            .get(&pool_id)
            .and_then(PoolEntry::v3)
            .map(|(_, state)| state)
    }
    /// Look up a V3 pool's immutable registration identity (address, tokens,
    /// fee, `tick_spacing`, factory). Returns `None` if the pool is not
    /// registered or isn't a V3 pool.
    #[must_use]
    pub(crate) fn get_v3_identity<'r>(
        &self,
        reg: &'r RegistryCore,
        pool_id: u64,
    ) -> Option<&'r V3PoolIdentity> {
        reg.pools
            .get(&pool_id)
            .and_then(PoolEntry::v3)
            .map(|(identity, _)| identity)
    }
    /// Snapshot all V3 pool state for verification (clones every V3 entry).
    ///
    /// Used by `verify_liquidity_maps` so the engine+core locks can be
    /// released before making async RPC calls.
    #[must_use]
    pub(crate) fn v3_pools_snapshot(
        &self,
        reg: &RegistryCore,
    ) -> HashMap<u64, (V3PoolIdentity, V3PoolState)> {
        reg.pools
            .iter()
            .filter_map(|(id, e)| match e {
                PoolEntry::V3(p) => Some((*id, (p.0, p.1.clone()))),
                PoolEntry::V2(..)
                | PoolEntry::V4(..)
                | PoolEntry::Curve(..)
                | PoolEntry::BalancerWeighted(..)
                | PoolEntry::BalancerStable(..)
                | PoolEntry::AerodromeV2(..) => None,
            })
            .collect()
    }
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
    /// Read the pinned snapshot seed for a V3 pool. Returns the
    /// seed if the pool is `Tracked` and the seed has not yet been taken; `None`
    /// for sparse pools or after `take_v3_snapshot_seed`. The seed is the
    /// registration-time `tick_data`, immutable across pump Mint/Burn — step-1
    /// verify compares this against on-chain@snapshot_block (not the
    /// pump-mutated `tick_data` current).
    #[must_use]
    pub(crate) fn v3_snapshot_seed<'r>(
        &self,
        reg: &'r RegistryCore,
        address: Address,
    ) -> Option<&'r HashMap<i32, TickInfo>> {
        let &pool_id = reg.pool_addresses.get(&address)?;
        let (_identity, state) = reg.pools.get(&pool_id).and_then(PoolEntry::v3)?;
        state.snapshot_seed.as_ref()
    }
    /// Take (move out + clear) the pinned snapshot seed for a V3 pool.
    /// Step-1 verify calls this to read+free the seed in one pass — the seed is
    /// verified exactly once (at the snapshot block during `build_paths`), then
    /// released to bound memory across 18k pools. Returns `None` for sparse
    /// pools or if already taken.
    pub(crate) fn take_v3_snapshot_seed(
        &mut self,
        reg: &mut RegistryCore,
        address: Address,
    ) -> Option<HashMap<i32, TickInfo>> {
        let &pool_id = reg.pool_addresses.get(&address)?;
        let (_identity, state) = reg.pools.get_mut(&pool_id).and_then(PoolEntry::v3_mut)?;
        state.snapshot_seed.take()
    }
    /// Pin the **post-drain** `(tick_data, block)` pair for a V3 pool (the step-2
    /// rolling-start race fix). Captures a frozen copy of the current
    /// `tick_data` alongside the `update_block` it was computed at — called
    /// atomically with `apply_buffer_v3`'s final drain (the single
    /// `core.write()` hold running backfill + pump buffers). Step-2 verify then
    /// compares THIS pinned pair (via `take_v3_post_drain_snapshot`) to
    /// on-chain@**the pinned block** — NOT engine-current (which under a
    /// rolling start accumulates pump Mint/Burn journals AFTER the drain) and
    /// NOT a start()-time `verify_backfill_block` constant (which predates the
    /// pump buffer's drain and would fabricate a mismatch on any active pool
    /// the 2026-06-29 crash). `Some` only for `Tracked` pools; `Sparse`
    /// stays `None` (no complete `tick_data` → step-2 is a no-op). Idempotent
    /// if called twice (the second pin overwrites; only step-2 consumes it).
    pub(crate) fn pin_v3_post_drain_snapshot(&mut self, reg: &mut RegistryCore, address: Address) {
        // Hoist the tombstone-confirmed cutoff (`pump_complete_cutoff` takes
        // `&self`) out of the inner scope, where `&mut state` is alive.
        let cutoff = self.pump_complete_cutoff;
        // Stamp provenance: hoist the engine-witnessed horizon for
        // this pool — independent of the imported seed stamp — so the pin can
        // classify the stamp's freshness claim (the load-time tripwire).
        let witnessed = self.v3_event_horizon(&address);
        // Capture the pin scalars in an inner scope so the `&mut state`
        // borrow of `reg.pools` ends before the diagnostic reads
        // `self.v3_buffer` (a second `&self` borrow).
        let diag = {
            let Some(&pool_id) = reg.pool_addresses.get(&address) else {
                return;
            };
            let Some(state) = reg
                .pools
                .get_mut(&pool_id)
                .and_then(PoolEntry::v3_mut)
                .map(|(_, s)| s)
            else {
                return;
            };
            if state.coverage == PoolTickCoverage::Tracked {
                let liquidity_clock = state.tick_data_block;
                // Two-stamp rule: the pin pairs the TICK MAP with its own
                // LIQUIDITY clock (`tick_data_block`), not the price clock —
                // step-2 verify compares `tick_data` against on-chain@the
                // pinned block, so the pinned block must be the liquidity clock.
                //
                // Fabricated-mismatch clamp: the verify block is the
                // block the tick map is CONFIRMED-complete at. If the pump has
                // any UNDRAINED event at/below the pool's liquidity clock
                // (`pump_count_at_or_below > 0` — an in-progress block the
                // drain held back at the cutoff), the map may be incomplete AT
                // that clock block, and verifying there would compare an
                // incomplete map against the full on-chain block -> false
                // mismatch. Clamp down to the tombstone-complete cutoff. The
                // `pump_count == 0` case is the BENIGN seed carrying the live
                // WS head past the cutoff (mod.rs:580) — keep the clock block.
                let undrained = self
                    .v3_buffer
                    .pump_count_at_or_below(&address, liquidity_clock);
                let pinned_block = if undrained > 0 && cutoff > 0 {
                    liquidity_clock.min(cutoff)
                } else {
                    liquidity_clock
                };
                state.post_drain_snapshot = Some((state.tick_data.clone(), pinned_block));
                let verdict = pin_provenance_verdict(liquidity_clock, cutoff, witnessed);
                Some((
                    pinned_block,
                    liquidity_clock,
                    state.tick_data.len(),
                    verdict,
                ))
            } else {
                None
            }
        };
        if let Some((tick_data_block, seed_block, tick_count, verdict)) = diag {
            diag!(domain = verify, pool_addr = %format!("{address:x}"),
                tick_data_block,
                tick_count,
                pump_count = self.v3_buffer.pump_count_at_or_below(&address, tick_data_block),
                last_complete_block = self.pump_complete_cutoff,
                "V3 pin"
            );
            // Stamp provenance — the load-time tripwire. The
            // seed stamp (`seed_block`) is honest only if independent of it,
            // something witnessed state at/beyond it: the tombstone-confirmed
            // delivery horizon (`cutoff`) or engine-witnessed events for THIS
            // pool (`witnessed_horizon`). A re-seed-after-activity (a fresher
            // stamp arrived after the engine already processed events for
            // this pool) is exactly the mis-stamped lie shape and warns loudly;
            // the verify tripwire that follows covers content-correctness, but
            // provenance covers freshness-claim honesty.
            match verdict {
                PinProvenance::SeedTrustOnly { witnessed_horizon } if witnessed_horizon > 0 => {
                    op_warn!(domain = state, pool_addr = %format!("{address:x}"),
                        seed_block,
                        cutoff,
                        witnessed_horizon,
                        "V3 re-seed-after-activity: a fresher seed stamp arrived \n                         after the engine had already witnessed events for this pool                          (the mis-stamped lie shape)"
                    );
                }
                PinProvenance::CorroboratedByDelivery
                | PinProvenance::WitnessedBeyondCutoff
                | PinProvenance::SeedTrustOnly { .. } => {
                    diag!(domain = state, pool_addr = %format!("{address:x}"),
                        seed_block,
                        cutoff,
                        verdict = ?verdict,
                        "V3 pin stamp classified"
                    );
                }
            }
        }
    }
    /// Take (move out + clear) the pinned post-drain `(tick_data, block)` pair
    /// for a V3 pool. Step-2 verify calls this to read+free the pin in one
    /// pass — the pin is verified exactly once (at the pinned block during
    /// `build_paths`), then released to bound memory. The returned block is the
    /// `tick_data_block` (liquidity clock, two-stamp rule) captured
    /// atomically with the drain; the verify compares
    /// `tick_data` against on-chain@THIS block, NOT a caller-supplied
    /// `verify_backfill_block` constant. Returns `None` for sparse pools, pools
    /// with no drain-yet pin, or if already taken (no-op Ok at the seam).
    pub(crate) fn take_v3_post_drain_snapshot(
        &mut self,
        reg: &mut RegistryCore,
        address: Address,
    ) -> Option<(HashMap<i32, TickInfo>, u64)> {
        let &pool_id = reg.pool_addresses.get(&address)?;
        let (_identity, state) = reg.pools.get_mut(&pool_id).and_then(PoolEntry::v3_mut)?;
        state.post_drain_snapshot.take()
    }
    /// Full-sync a V3 pool's `tick_data` from an external source (e.g. Python
    /// backfill). Replaces the entire `tick_data` map (so ticks Burn-removed
    /// on-chain are also removed here) and updates scalar state. No-op if the
    /// pool address is not registered.
    pub(crate) fn sync_v3_pool_state(
        &mut self,
        reg: &mut RegistryCore,
        pool_address: Address,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        tick_data: HashMap<i32, TickInfo>,
        update_block: u64,
    ) {
        let Some(&key) = reg.pool_addresses.get(&pool_address) else {
            return;
        };
        let Some(state) = reg
            .pools
            .get_mut(&key)
            .and_then(PoolEntry::v3_mut)
            .map(|(_, s)| s)
        else {
            return;
        };
        state.sqrt_price_x96 = sqrt_price_x96;
        state.liquidity = liquidity;
        state.tick = tick;
        state.tick_data = tick_data;
        // Two-stamp rule: a wholesale full-state sync replaces BOTH clocks
        // with the same source block (the sync provides scalars AND tick_data
        // from one snapshot). A full replacement is a sanctioned reset (not an
        // incremental backward stamp), so it sets both directly — reorg is not
        // the only permitted rewind here.
        state.update_block = update_block;
        state.tick_data_block = update_block;
        state.invalidate_tick_range_cache();
    }
    /// per-pool tick-mutation fingerprint for the staged word-fetch
    /// install check. Any `tick_data` mutation moves `update_block` and/or
    /// the tick population; the stamp-sum term kills same-block net-zero
    /// churn that len alone would miss. `None` = pool gone.
    pub(crate) fn tick_fingerprint(
        &self,
        reg: &RegistryCore,
        pool_id: u64,
    ) -> Option<(u64, usize, u64)> {
        match reg.pools.get(&pool_id) {
            Some(PoolEntry::V3(p)) => Some((
                p.1.update_block,
                p.1.tick_data.len(),
                p.1.tick_data.values().map(|t| t.block).sum(),
            )),
            Some(PoolEntry::V4(p)) => Some((
                p.1.update_block,
                p.1.tick_data.len(),
                p.1.tick_data.values().map(|t| t.block).sum(),
            )),
            _ => None,
        }
    }
    /// Stage half of the word backfill: clone the stored fetcher +
    /// capture the pool's tick fingerprint UNDER a short write, and release
    /// the caller's guard before the (multi-second, `Python::attach` + web3
    /// RPC) fetch runs. The old single-hold path fetched while the write
    /// guard was alive, parking the pump’s apply/solve pipeline behind the
    /// RPC. Pair with [`Self::install_word_fetch`].
    pub(crate) fn stage_word_fetch_by_pool_id(
        &mut self,
        reg: &RegistryCore,
        pool_id: u64,
        word: i32,
        block: u64,
        retried: bool,
    ) -> Option<StagedWordFetch> {
        let fetcher = match reg.pools.get(&pool_id) {
            Some(PoolEntry::V3(p)) => p.1.fetcher.clone(),
            Some(PoolEntry::V4(p)) => p.1.fetcher.clone(),
            _ => None,
        }?;
        // The fetch context must reflect the CURRENT
        // pool clock on a retry, not the (stale) companion block — a fetch
        // at the original context after an interleaved event snapshots the
        // word pre-event, and even the stamp guard cannot help a tick the
        // event added (the guard preserves resident stamps; the word would
        // otherwise go known-minus-missing-ticks alongside the event's own
        // later application). First attempt honors the companion context.
        let fetch_context = if retried {
            match reg.pools.get(&pool_id).map(|entry| match entry {
                PoolEntry::V3(p) => p.1.update_block,
                PoolEntry::V4(p) => p.1.update_block,
                PoolEntry::AerodromeV2(..)
                | PoolEntry::Curve(..)
                | PoolEntry::BalancerWeighted(..)
                | PoolEntry::BalancerStable(..)
                | PoolEntry::V2(..) => 0,
            }) {
                Some(clock) => clock.saturating_sub(1),
                None => 0,
            }
        } else {
            block
        };
        let fingerprint = self.tick_fingerprint(reg, pool_id)?;
        Some(StagedWordFetch {
            pool_id,
            word,
            block: fetch_context,
            fetcher,
            fingerprint,
        })
    }
    /// Install half: merges the fetched word only if the pool was NOT
    /// mutated while the fetch ran (fingerprint re-check). A mutation means
    /// the pump applied an event for this pool during the fetch window —
    /// the staged overlay would then clobber fresher tick writes, so the
    /// caller RETRIES the stage+fetch (bounded) instead of applying a lost
    /// update. [`InstallWordOutcome::Failed`] keeps the failure contract
    /// (fetch failed / pool gone → the companion gate raises).
    pub(crate) fn install_word_fetch(
        &mut self,
        reg: &mut RegistryCore,
        staged: &StagedWordFetch,
        fetched: &::degenbot_pools::tick_fetch::FetchedTickWord,
    ) -> InstallWordOutcome {
        use InstallWordOutcome::{Failed, Merged, Raced};
        match self.tick_fingerprint(reg, staged.pool_id) {
            None => Failed,
            Some(fp) if fp == staged.fingerprint => {
                if self.merge_tick_word(reg, staged.pool_id, fetched) {
                    Merged
                } else {
                    Failed
                }
            }
            Some(_) => Raced,
        }
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
        reg: &mut RegistryCore,
        pool_id: u64,
        word: i32,
        block: u64,
    ) -> bool {
        // Clone the stored fetcher off the state first: the fetch call must
        // not hold the pool borrow, and `merge_tick_word` re-borrows `self`.
        let fetcher = match reg.pools.get(&pool_id) {
            Some(PoolEntry::V3(p)) => p.1.fetcher.clone(),
            Some(PoolEntry::V4(p)) => p.1.fetcher.clone(),
            _ => None,
        };
        let Some(fetcher) = fetcher else {
            return false;
        };
        let Ok(fetched_word) = fetcher.fetch_missing_tick_word(pool_id, word, block) else {
            return false;
        };
        self.merge_tick_word(reg, pool_id, &fetched_word)
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
        reg: &mut RegistryCore,
        pool_id: u64,
        fetched: &::degenbot_pools::tick_fetch::FetchedTickWord,
    ) -> bool {
        // ADR-017 slice 1: dispatch through `ConcentratedLiquidityPoolMut`
        // (the body lived inlined in V3/V4 arms here; the trait dedups the
        // two). The `bool` wraps the trait's always-`true` return: `false`
        // for non-CL / unregistered pools (the non-CL no-op).
        let Some(entry) = reg.pools.get_mut(&pool_id) else {
            return false;
        };
        match entry.as_cl_mut() {
            Some(cl) => cl.merge_tick_word(fetched),
            None => false,
        }
    }
    /// Number of registered V3 pools.
    #[must_use]
    pub(crate) fn v3_pool_count(&self, reg: &RegistryCore) -> usize {
        reg.pools
            .values()
            .filter(|e| matches!(e, PoolEntry::V3(..)))
            .count()
    }
    // -----------------------------------------------------------------------
    // V4 state (ADR-003: single entry per `(pool_manager, pool_id)`;
    // orientation derived at solve from `zero_for_one`)
    // -----------------------------------------------------------------------
    /// Record the canonical V4 `StateView` contract address for a `pool_manager`
    /// (ADR-005 / Option 2 — Rust owns the mapping). V4 scalar state is read
    /// via the `StateView`'s `getSlot0`/`getLiquidity`, not `getPool` on the
    /// `PoolManager` (which reverts on the canonical deployment); the
    /// solver-state verifier resolves it per-hop via [`ClOrchestration::state_view_for`].
    /// Idempotent: the seed for a manager is supplied once by the driver
    /// (read from the `pool_managers` DB row) before V4 pools solve.
    pub fn register_v4_state_view(&mut self, pool_manager: Address, state_view: Address) {
        self.v4_state_views.insert(pool_manager, state_view);
    }
    /// The canonical V4 `StateView` address for `pool_manager`, if registered.
    /// `None` when unknown — the solver-state verifier skips a V4 hop whose
    /// manager's `StateView` has not been seeded (no false alarm on an
    /// un-verifiable hop). No Rust reader is wired yet; the mapping's write
    /// path is live via `register_v4_state_view` (the `PyO3` registration
    /// path), so the read stays in the capability's method set.
    #[must_use]
    #[expect(dead_code)]
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
    /// an impossible-CL-config rejection surfaces the primitive at fault.
    ///
    /// Returns `Err` if the pool uses a dynamic fee (`fee == 0x100000`),
    /// has a static fee exceeding the executor's `u16` encoding field
    /// (`fee >= degenbot_executor::encoders::V4_FEE_ENCODER_MAX`), or a pool
    /// with the same `(pool_manager, pool_id)` is already registered.
    pub(crate) fn register_v4_pool(
        &mut self,
        reg: &mut RegistryCore,
        params: &RegisterV4PoolParams,
    ) -> Result<u64, RegisterV4PoolError> {
        use ::degenbot_pools::spec_bounds as sb;
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

        let key = (params.pool_manager, params.pool_id);
        if self.v4_pool_ids.contains_key(&key) {
            return Err(RegisterV4PoolError::AlreadyRegistered {
                pool_manager: params.pool_manager,
                pool_id: params.pool_id,
            });
        }

        let pool_id = reg.next_pool_id;
        reg.next_pool_id += 1;

        // the `seed_from_store` path is retired — the DB
        // seeding is handled by the Db arm of `assemble_v4_tick_map` (held
        // snapshot tx). Just clone + flow the params through.
        let params = params.clone();
        let (identity, state) = V4PoolState::from_params(params, reg.journal_depth);
        reg.pools
            .insert(pool_id, PoolEntry::V4(Box::new((identity, state))));
        self.v4_pool_ids.insert(key, pool_id);

        Ok(pool_id)
    }
    /// Resolve a V4 pool's routing presence (`cl_route` axis).
    fn v4_presence(
        &self,
        reg: &RegistryCore,
        key: &(Address, degenbot_decoders::v4_swap_decoder::V4PoolId),
    ) -> PoolPresence {
        let lifecycle = self
            .v4_pool_ids
            .get(key)
            .and_then(|&id| match reg.pools.get(&id) {
                Some(PoolEntry::V4(p)) => Some(p.1.registration_lifecycle),
                _ => None,
            });
        PoolPresence::from_lifecycle(lifecycle)
    }
    /// THE single decision point for a decoded V4 event (`cl_route` table).
    /// V4 twin of [`Self::route_v3_event`], keyed by `(pool_manager, pool_id)`.
    /// `swap_priors` overlays tick priors onto DIRECT swap application only.
    pub(crate) fn route_v4_event(
        &mut self,
        reg: &mut RegistryCore,
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
        let presence = self.v4_presence(reg, &(pool_manager, v4_pool_id));
        let event_block = match &event {
            BufferedV4PoolEvent::Swap(s) => s.block_number,
            BufferedV4PoolEvent::Liquidity(l) => l.block_number,
        };
        match route_action(phase, presence, kind) {
            RouteAction::ApplyDirect => {
                // Table invariant: ApplyDirect implies registered. Defensive
                // fallback (never expected) stages rather than drops.
                let Some(id) = self.v4_pool_ids.get(&(pool_manager, v4_pool_id)).copied() else {
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
                        self.apply_v4_swap_by_pool_id(
                            reg,
                            id,
                            sqrt_price_x96,
                            liquidity,
                            tick,
                            block_number,
                            swap_priors,
                        );
                    }
                    BufferedV4PoolEvent::Liquidity(BufferedV4LiquidityUpdate {
                        tick_lower,
                        tick_upper,
                        liquidity_delta,
                        block_number,
                    }) => {
                        if let Ok(delta_i128) = i128::try_from(liquidity_delta) {
                            self.apply_v4_liquidity_update_by_pool_id(
                                reg,
                                id,
                                tick_lower,
                                tick_upper,
                                delta_i128,
                                block_number,
                            );
                        } else {
                            debug_assert!(
                                false,
                                "V4 liquidity_delta exceeds i128 — unreachable for real events"
                            );
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
    /// Apply a V4 Swap event (ADR-003 live path) — thin adapter over
    /// [`Self::route_v4_event`] at `Phase::Live`; outcome flattened to the
    /// historical `Option<pool_id>` shape. `update.tick_priors` overlay only
    /// on direct application (pump/backfill pass an empty slice).
    pub(crate) fn apply_v4_swap(
        &mut self,
        reg: &mut RegistryCore,
        update: &V4SwapUpdate,
        block_number: u64,
    ) -> Option<u64> {
        let pool_id_hex = alloy::hex::encode_prefixed(update.pool_id);
        trace_apply_swap_v4(
            update.pool_manager,
            &pool_id_hex,
            update.sqrt_price_x96,
            update.liquidity,
            update.tick,
            block_number,
        );
        match self.route_v4_event(
            reg,
            Phase::Live,
            update.pool_manager,
            update.pool_id,
            BufferedV4PoolEvent::Swap(BufferedV4SwapEvent {
                sqrt_price_x96: update.sqrt_price_x96,
                liquidity: update.liquidity,
                tick: update.tick,
                block_number,
            }),
            &update.tick_priors,
        ) {
            ApplyOutcome::Applied(pool_id) => Some(pool_id),
            ApplyOutcome::Buffered(_) | ApplyOutcome::NoOp(_) => None,
        }
    }
    /// Apply a V4 `ModifyLiquidity` event — thin adapter over
    /// [`Self::route_v4_event`] at `Phase::Live`. The same class of safety:
    /// unregistered pools stage into the PUMP buffer here so late registration
    /// captures them; the old inline arms embedded a partial policy copy.
    pub(crate) fn apply_v4_liquidity_update(
        &mut self,
        reg: &mut RegistryCore,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: alloy::primitives::I256,
        block_number: u64,
    ) -> Option<u64> {
        match self.route_v4_event(
            reg,
            Phase::Live,
            pool_manager,
            pool_id,
            BufferedV4PoolEvent::Liquidity(BufferedV4LiquidityUpdate {
                tick_lower,
                tick_upper,
                liquidity_delta,
                block_number,
            }),
            &[],
        ) {
            ApplyOutcome::Applied(pid) => Some(pid),
            ApplyOutcome::Buffered(_) | ApplyOutcome::NoOp(_) => None,
        }
    }
    /// Apply a V4 Swap keyed by the resolved `pool_id` (ADR-014 D1 twin of
    /// the V3 address-keyed wrapper; registered pools only).
    pub(crate) fn apply_v4_swap_by_pool_id(
        &mut self,
        reg: &mut RegistryCore,
        pool_id: u64,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)],
    ) -> Option<u64> {
        let (_identity, state) = reg.pools.get_mut(&pool_id).and_then(PoolEntry::v4_mut)?;
        state.apply_swap(sqrt_price_x96, liquidity, tick, block_number, tick_priors);
        Some(pool_id)
    }
    /// Apply a V4 `ModifyLiquidity` keyed by the resolved `pool_id`: journals
    /// the two tick priors, applies the delta to the tick range
    /// (`liquidity_net` `+=` at lower, `-=` at upper, both `gross +=`),
    /// advances `update_block`, invalidates the tick-range cache. No scalar
    /// change (`scalar_priors: None`) — same ADR-004 tick-only contract as V3.
    ///
    /// Returns `Some(pool_id)` if the pool is V4; `None` otherwise.
    pub(crate) fn apply_v4_liquidity_update_by_pool_id(
        &mut self,
        reg: &mut RegistryCore,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) -> Option<u64> {
        let (_identity, state) = reg.pools.get_mut(&pool_id).and_then(PoolEntry::v4_mut)?;
        state.apply_liquidity_update(tick_lower, tick_upper, liquidity_delta, block_number);
        Some(pool_id)
    }
    /// Buffer a V4 `ModifyLiquidity` event from the backfill phase.
    pub(crate) fn buffer_backfill_v4_liquidity_update(
        &mut self,
        reg: &mut RegistryCore,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: alloy::primitives::I256,
        block_number: u64,
    ) {
        let key = (pool_manager, pool_id);
        if let Some(&id) = self.v4_pool_ids.get(&key) {
            if let Some(state) = reg
                .pools
                .get_mut(&id)
                .and_then(PoolEntry::v4_mut)
                .map(|(_, s)| s)
            {
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
                    if let Ok(delta_i128) = i128::try_from(liquidity_delta) {
                        state.apply_liquidity_update(
                            tick_lower,
                            tick_upper,
                            delta_i128,
                            block_number,
                        );
                        return;
                    }
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
    /// Apply all buffered **backfill** V4 `ModifyLiquidity` events for a pool.
    ///
    /// Same journal + `update_block` contract as the V3 buffer appliers
    /// ([`apply_backfill_buffer_v3`]) — each event pushes a tick-only
    /// `V3BlockDelta` (V4 shares the V3 journal shape) and advances
    /// `state.update_block`. Pre-fix these mutated `tick_data` only.
    pub(crate) fn apply_backfill_buffer_v4(
        &mut self,
        reg: &mut RegistryCore,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) {
        let key = (pool_manager, pool_id);
        let Some(&id) = self.v4_pool_ids.get(&key) else {
            return;
        };
        let Some(buffered) = self.v4_buffer.drain_backfill(&key) else {
            return;
        };
        for update in buffered {
            let Some(state) = reg
                .pools
                .get_mut(&id)
                .and_then(PoolEntry::v4_mut)
                .map(|(_, s)| s)
            else {
                continue;
            };
            Self::apply_buffered_v4_event(state, update);
        }
    }
    /// Apply all buffered **pump** V4 `ModifyLiquidity` events for a pool.
    ///
    /// Same journal + `update_block` contract as
    /// [`apply_backfill_buffer_v4`] — see its docs.
    pub(crate) fn apply_pump_buffer_v4(
        &mut self,
        reg: &mut RegistryCore,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) {
        let key = (pool_manager, pool_id);
        let Some(&id) = self.v4_pool_ids.get(&key) else {
            return;
        };
        // drain ONLY fully-completed blocks. The cutoff is the pump's
        // `StageMachine` tombstone cutoff — a block is complete when
        // the first log of N+1 closes N; a drain mid-block would pin
        // `update_block=N` missing a later same-block log.
        let cutoff = self.pump_complete_cutoff;
        if cutoff == 0 {
            return;
        }
        let Some(buffered) = self.v4_buffer.drain_pump_completed(&key, cutoff) else {
            return;
        };
        for update in buffered {
            let Some(state) = reg
                .pools
                .get_mut(&id)
                .and_then(PoolEntry::v4_mut)
                .map(|(_, s)| s)
            else {
                continue;
            };
            Self::apply_buffered_v4_event(state, update);
        }
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
    /// V4 twin of [`apply_buffered_v3_event`] — the `Liquidity` variant narrows
    /// the int256 delta to i128 at the drain→apply seam (ADR-014 D4, matching
    /// the live `apply_v4_liquidity_update_by_pool_id` path).
    fn apply_buffered_v4_event(state: &mut V4PoolState, event: BufferedV4PoolEvent) {
        match event {
            BufferedV4PoolEvent::Liquidity(u) => {
                if let Ok(delta_i128) = i128::try_from(u.liquidity_delta) {
                    state.apply_liquidity_update(
                        u.tick_lower,
                        u.tick_upper,
                        delta_i128,
                        u.block_number,
                    );
                }
            }
            BufferedV4PoolEvent::Swap(s) => {
                state.apply_swap(s.sqrt_price_x96, s.liquidity, s.tick, s.block_number, &[]);
            }
        }
    }
    /// Set a V3 pool's registration lifecycle to `Quarantined`. The
    /// live pump then defers the pool's `Swap`/`Mint`/`Burn` events to the
    /// pump buffer until [`set_pool_live`] transitions it back. Call at the
    /// start of `register_v3_pool` (before the first RPC await). No-op for
    /// unregistered / non-V3 pools AND for non-`Tracked` pools (a `Sparse`
    /// pool has no pin / step-2 verify to protect, so quarantining it would
    /// only defer events with nothing to gain — it stays `Live`/direct-apply;
    /// coverage-aware carve-out).
    /// Coverage flag for a registered V3 pool (`Tracked` = complete tick data,
    /// `Sparse` = none). Returns `None` for unregistered / non-V3 pools. The
    /// registration-lifecycle module reads this up-front to branch the
    /// verify-lifecycle (Sparse stays `Live`, no RPC).
    #[must_use]
    pub(crate) fn v3_pool_coverage(
        &self,
        reg: &RegistryCore,
        address: Address,
    ) -> Option<PoolTickCoverage> {
        let &pool_id = reg.pool_addresses.get(&address)?;
        match reg.pools.get(&pool_id)? {
            PoolEntry::V3(p) => Some(p.1.coverage),
            _ => None,
        }
    }
    /// Coverage flag for a registered V4 pool (`Tracked` / `Sparse`). Returns
    /// `None` for unregistered / non-V4 pools. V4 twin of
    /// [`v3_pool_coverage`] — read up-front by the registration-lifecycle to
    /// keep Sparse pools out of the verify deferral.
    #[must_use]
    pub(crate) fn v4_pool_coverage(
        &self,
        reg: &RegistryCore,
        pool_manager: Address,
        pool_id: &degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) -> Option<PoolTickCoverage> {
        let pid = self.v4_pool_id_by_key(pool_manager, pool_id)?;
        match reg.pools.get(&pid)? {
            PoolEntry::V4(p) => Some(p.1.coverage),
            _ => None,
        }
    }
    pub(crate) fn set_v3_pool_quarantined(&mut self, reg: &mut RegistryCore, address: Address) {
        if let Some(&id) = reg.pool_addresses.get(&address) {
            if let Some(state) = reg
                .pools
                .get_mut(&id)
                .and_then(PoolEntry::v3_mut)
                .map(|(_, s)| s)
            {
                if state.coverage == PoolTickCoverage::Tracked {
                    state.registration_lifecycle = RegistrationLifecycle::Quarantined;
                }
            }
        }
    }
    /// Set a V4 pool's registration lifecycle to `Quarantined`. V4
    /// twin of [`set_v3_pool_quarantined`]. Call at the start of
    /// `register_v4_pool` (before the first RPC await). No-op for unregistered
    /// V4 pools and for non-`Tracked` pools (Sparse stays `Live`).
    pub(crate) fn set_v4_pool_quarantined(
        &mut self,
        reg: &mut RegistryCore,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) {
        let key = (pool_manager, pool_id);
        if let Some(&id) = self.v4_pool_ids.get(&key) {
            if let Some(state) = reg
                .pools
                .get_mut(&id)
                .and_then(PoolEntry::v4_mut)
                .map(|(_, s)| s)
            {
                if state.coverage == PoolTickCoverage::Tracked {
                    state.registration_lifecycle = RegistrationLifecycle::Quarantined;
                }
            }
        }
    }
    /// Transition a V3 pool from `Quarantined` to `Live`: flush any
    /// remaining buffered pump events for the pool (the in-progress-block tail
    /// retained by `drain_pump_completed`) via the UNGUARDED `drain_pump` in
    /// insertion order, then mark `Live`. Applies under one `core.write()`
    /// hold so no live event interleaves between the flush and the mark. The
    /// flush uses `drain_pump` (not `drain_pump_completed`) because the
    /// retained tail must not be orphaned (no second registration drain
    /// exists) — matches the Live steady-state contract (Live pools receive
    /// direct apply with no per-block gate; ordering preserved). No-op for
    /// unregistered / non-V3 pools or an already-`Live` pool.
    pub(crate) fn set_v3_pool_live(&mut self, reg: &mut RegistryCore, address: Address) {
        let Some(&id) = reg.pool_addresses.get(&address) else {
            return;
        };
        // Flush the retained pump tail first (backfill already fully drained
        // during `apply_backfill_buffer_v3`).
        if let Some(buffered) = self.v3_buffer.drain_pump(&address) {
            {
                use ::degenbot_pools::liquidity_event::LiquidityEvent;
                let blocks: Vec<u64> = buffered.iter().map(LiquidityEvent::block_number).collect();
                let mut sorted = blocks.clone();
                sorted.sort_unstable();
                let distinct: Vec<u64> = sorted
                    .into_iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                diag!(domain = verify, pool_addr = %format!("{address:x}"),
                    drained_tail = buffered.len(),
                    blocks = ?blocks,
                    distinct_blocks = ?distinct,
                    "V3 set_live"
                );
            }
            for event in buffered {
                if let Some(state) = reg
                    .pools
                    .get_mut(&id)
                    .and_then(PoolEntry::v3_mut)
                    .map(|(_, s)| s)
                {
                    Self::apply_buffered_v3_event(state, event);
                }
            }
        }
        if let Some(state) = reg
            .pools
            .get_mut(&id)
            .and_then(PoolEntry::v3_mut)
            .map(|(_, s)| s)
        {
            state.registration_lifecycle = RegistrationLifecycle::Live;
        }
    }
    /// Transition a V4 pool from `Quarantined` to `Live`. V4 twin of
    /// [`set_v3_pool_live`] — flushes the retained pump tail via the
    /// unguarded `drain_pump`, then marks `Live`. No-op for unregistered V4
    /// pools or an already-`Live` pool.
    pub(crate) fn set_v4_pool_live(
        &mut self,
        reg: &mut RegistryCore,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) {
        let key = (pool_manager, pool_id);
        let Some(&id) = self.v4_pool_ids.get(&key) else {
            return;
        };
        if let Some(buffered) = self.v4_buffer.drain_pump(&key) {
            {
                use ::degenbot_pools::liquidity_event::LiquidityEvent;
                let blocks: Vec<u64> = buffered.iter().map(LiquidityEvent::block_number).collect();
                let mut sorted = blocks.clone();
                sorted.sort_unstable();
                let distinct: Vec<u64> = sorted
                    .into_iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                diag!(domain = verify, pool_manager = %format!("{pool_manager:x}"),
                    pool_id = %alloy::hex::encode_prefixed(pool_id),
                    drained_tail = buffered.len(),
                    blocks = ?blocks,
                    distinct_blocks = ?distinct,
                    "V4 set_live"
                );
            }
            for event in buffered {
                if let Some(state) = reg
                    .pools
                    .get_mut(&id)
                    .and_then(PoolEntry::v4_mut)
                    .map(|(_, s)| s)
                {
                    Self::apply_buffered_v4_event(state, event);
                }
            }
        }
        if let Some(state) = reg
            .pools
            .get_mut(&id)
            .and_then(PoolEntry::v4_mut)
            .map(|(_, s)| s)
        {
            state.registration_lifecycle = RegistrationLifecycle::Live;
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
    /// [`set_v3_pool_live`]/[`set_v4_pool_live`]) and mark it `Live`, so no
    /// registered pool is left buffering forever. No-op when nothing is
    /// quarantined.
    pub(crate) fn release_all_v3_v4_quarantined(&mut self, reg: &mut RegistryCore) {
        // Collect the still-Quarantined V3 addresses and V4 (pm, pool_id) keys
        // first (drain buffers are keyed by those, not `pool_id`), then release
        // each via the existing set_live flush+mark. Collect-then-apply avoids
        // holding a `&mut reg.pools` borrow across the drain calls.
        let v3_addrs: Vec<Address> = reg
            .pools
            .iter()
            .filter_map(|(&id, e)| match e {
                PoolEntry::V3(p)
                    if p.1.registration_lifecycle == RegistrationLifecycle::Quarantined =>
                {
                    if let PoolEntry::V3(q) = &reg.pools[&id] {
                        Some(q.0.address)
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .collect();
        let v4_keys: Vec<(Address, degenbot_decoders::v4_swap_decoder::V4PoolId)> = reg
            .pools
            .iter()
            .filter_map(|(&id, e)| match e {
                PoolEntry::V4(p)
                    if p.1.registration_lifecycle == RegistrationLifecycle::Quarantined =>
                {
                    if let PoolEntry::V4(q) = &reg.pools[&id] {
                        Some((q.0.pool_manager, q.0.pool_id))
                    } else {
                        None
                    }
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
            self.set_v3_pool_live(reg, addr);
        }
        for (pm, pid) in v4_keys {
            self.set_v4_pool_live(reg, pm, pid);
        }
    }
    /// Read a registered V4 pool's state by `pool_id`.
    #[must_use]
    pub(crate) fn get_v4_pool<'r>(
        &self,
        reg: &'r RegistryCore,
        pool_id: u64,
    ) -> Option<&'r V4PoolState> {
        reg.pools
            .get(&pool_id)
            .and_then(PoolEntry::v4)
            .map(|(_, state)| state)
    }
    /// Look up a V4 pool's immutable registration identity (`pool_manager`,
    /// `pool_id`, `pool_key`). Returns `None` if the pool is not registered or
    /// isn't a V4 pool.
    #[must_use]
    pub(crate) fn get_v4_identity<'r>(
        &self,
        reg: &'r RegistryCore,
        pool_id: u64,
    ) -> Option<&'r V4PoolIdentity> {
        reg.pools
            .get(&pool_id)
            .and_then(PoolEntry::v4)
            .map(|(identity, _)| identity)
    }
    /// Look up the pool ID for a registered `(pool_manager, pool_id)` pair.
    #[must_use]
    pub fn v4_pool_id_by_key(
        &self,
        pool_manager: Address,
        pool_id: &degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) -> Option<u64> {
        self.v4_pool_ids.get(&(pool_manager, *pool_id)).copied()
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
        reg: &RegistryCore,
        pool_manager: Address,
        pool_id: &degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) -> Option<RegisteredV4> {
        let id = self.v4_pool_id_by_key(pool_manager, pool_id)?;
        let identity = self.get_v4_identity(reg, id)?;
        let state = self.get_v4_pool(reg, id)?;
        Some(RegisteredV4 {
            pool_id: id,
            pool_key: identity.pool_key.clone(),
            protocol_fee: state.protocol_fee,
            coverage: state.coverage,
        })
    }
    /// Read the pinned snapshot seed for a V4 pool (V4 twin of
    /// `v3_snapshot_seed`). Keyed by `(pool_manager, pool_id)`.
    #[must_use]
    pub(crate) fn v4_snapshot_seed<'r>(
        &self,
        reg: &'r RegistryCore,
        pool_manager: Address,
        pool_id: &degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) -> Option<&'r HashMap<i32, TickInfo>> {
        let pid = self.v4_pool_id_by_key(pool_manager, pool_id)?;
        let (_identity, state) = reg.pools.get(&pid).and_then(PoolEntry::v4)?;
        state.snapshot_seed.as_ref()
    }
    /// Take (move out + clear) the pinned snapshot seed for a V4 pool.
    /// V4 twin of `take_v3_snapshot_seed` — step-1 verify consumes the seed once.
    pub(crate) fn take_v4_snapshot_seed(
        &mut self,
        reg: &mut RegistryCore,
        pool_manager: Address,
        pool_id: &degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) -> Option<HashMap<i32, TickInfo>> {
        let pid = self.v4_pool_id_by_key(pool_manager, pool_id)?;
        let (_identity, state) = reg.pools.get_mut(&pid).and_then(PoolEntry::v4_mut)?;
        state.snapshot_seed.take()
    }
    /// Pin the post-drain `(tick_data, block)` pair for a V4 pool (step-2 race
    /// fix, V4 twin of `pin_v3_post_drain_snapshot`). Captures a frozen copy
    /// of the current `tick_data` alongside the `update_block` it was computed
    /// at, atomically with `apply_buffer_v4`'s final drain. Step-2 verify
    /// compares THIS pin (via `take_v4_post_drain_snapshot`) to on-chain@**the
    /// pinned block** — NOT engine-current (which accumulates pump
    /// `ModifyLiquidity` journals after the drain) and NOT a start()-time
    /// `verify_backfill_block` constant (which predates the pump buffer's drain
    /// the 2026-06-29 crash). `Tracked` pools only.
    pub(crate) fn pin_v4_post_drain_snapshot(
        &mut self,
        reg: &mut RegistryCore,
        pool_manager: Address,
        pool_id: &degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) {
        let key = (pool_manager, *pool_id);
        // Hoist the tombstone-confirmed cutoff (`pump_complete_cutoff` takes
        // `&self`) out of the inner scope, where `&mut state` is alive.
        let cutoff = self.pump_complete_cutoff;
        // Stamp provenance (V4 twin): hoist the engine-witnessed
        // horizon so the pin can classify the seed stamp's freshness claim.
        let witnessed = self.v4_event_horizon(&key);
        // Capture the pin scalar in an inner scope so the `&mut state` borrow
        // of `reg.pools` ends before the diagnostic reads `self.v4_buffer`
        // (a second `&self` borrow) — Rust forbids both alive at once.
        let diag = {
            let Some(pid) = self.v4_pool_id_by_key(pool_manager, pool_id) else {
                return;
            };
            let Some(state) = reg
                .pools
                .get_mut(&pid)
                .and_then(PoolEntry::v4_mut)
                .map(|(_, s)| s)
            else {
                return;
            };
            if state.coverage == PoolTickCoverage::Tracked {
                // Two-stamp rule (V4 twin): pin pairs tick_data with the
                // LIQUIDITY clock, not the price clock.
                let liquidity_clock = state.tick_data_block;
                // Fabricated-mismatch clamp (V4 twin): verify only at
                // the block the map is confirmed-complete at. `> 0` undrained
                // pump events at/below the clock + a nonzero cutoff -> clamp
                // down to the cutoff; the `pump_count == 0` benign-seed case
                // (mod.rs:580) and the no-tombstone (`cutoff == 0`) guard keep
                // the clock block.
                let undrained = self.v4_buffer.pump_count_at_or_below(&key, liquidity_clock);
                let pinned_block = if undrained > 0 && cutoff > 0 {
                    liquidity_clock.min(cutoff)
                } else {
                    liquidity_clock
                };
                state.post_drain_snapshot = Some((state.tick_data.clone(), pinned_block));
                let verdict = pin_provenance_verdict(liquidity_clock, cutoff, witnessed);
                Some((pinned_block, liquidity_clock, verdict))
            } else {
                None
            }
        };
        if let Some((tick_data_block, seed_block, verdict)) = diag {
            diag!(domain = verify, pool_manager = %format!("{pool_manager:x}"),
                pool_id = %alloy::hex::encode_prefixed(pool_id),
                tick_data_block,
                pump_count = self.v4_buffer.pump_count_at_or_below(&key, tick_data_block),
                last_complete_block = self.pump_complete_cutoff,
                "V4 pin"
            );
            // Stamp provenance (V4 twin) — see the V3 pin.
            match verdict {
                PinProvenance::SeedTrustOnly { witnessed_horizon } if witnessed_horizon > 0 => {
                    op_warn!(domain = state, pool_manager = %format!("{pool_manager:x}"),
                        pool_id = %alloy::hex::encode_prefixed(pool_id),
                        seed_block,
                        cutoff,
                        witnessed_horizon,
                        "V4 re-seed-after-activity: a fresher seed stamp arrived \n                         after the engine had already witnessed events for this pool                          (the mis-stamped lie shape)"
                    );
                }
                PinProvenance::CorroboratedByDelivery
                | PinProvenance::WitnessedBeyondCutoff
                | PinProvenance::SeedTrustOnly { .. } => {
                    diag!(domain = state, pool_manager = %format!("{pool_manager:x}"),
                        pool_id = %alloy::hex::encode_prefixed(pool_id),
                        seed_block,
                        cutoff,
                        verdict = ?verdict,
                        "V4 pin stamp classified"
                    );
                }
            }
        }
    }
    /// Take (move out + clear) the V4 post-drain `(tick_data, block)` pair.
    /// Step-2 verify consumes it once (at the pinned block). The returned
    /// block is the `tick_data_block` (liquidity clock, two-stamp rule)
    /// captured atomically with the drain; the verify compares `tick_data`
    /// against on-chain@THIS block, NOT a caller-supplied
    /// `verify_backfill_block` constant. `None` for sparse / un-drained /
    /// already-taken pools (no-op Ok at the seam).
    pub(crate) fn take_v4_post_drain_snapshot(
        &mut self,
        reg: &mut RegistryCore,
        pool_manager: Address,
        pool_id: &degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) -> Option<(HashMap<i32, TickInfo>, u64)> {
        let pid = self.v4_pool_id_by_key(pool_manager, pool_id)?;
        let (_identity, state) = reg.pools.get_mut(&pid).and_then(PoolEntry::v4_mut)?;
        state.post_drain_snapshot.take()
    }
    /// Number of registered V4 pools.
    #[must_use]
    pub fn v4_pool_count(&self) -> usize {
        self.v4_pool_ids.len()
    }
    /// Snapshot all V4 pool state for verification.
    #[must_use]
    pub(crate) fn v4_pools_snapshot(
        &self,
        reg: &RegistryCore,
    ) -> HashMap<u64, (V4PoolIdentity, V4PoolState)> {
        reg.pools
            .iter()
            .filter_map(|(id, e)| match e {
                PoolEntry::V4(p) => Some((*id, (p.0.clone(), p.1.clone()))),
                PoolEntry::V2(..)
                | PoolEntry::V3(..)
                | PoolEntry::Curve(..)
                | PoolEntry::BalancerWeighted(..)
                | PoolEntry::BalancerStable(..)
                | PoolEntry::AerodromeV2(..) => None,
            })
            .collect()
    }
    /// Full-sync a V4 pool's `tick_data` from an external source.
    pub(crate) fn sync_v4_pool_state(
        &mut self,
        reg: &mut RegistryCore,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId,
        update: V4StateSync,
    ) {
        let Some(&id) = self.v4_pool_ids.get(&(pool_manager, pool_id)) else {
            return;
        };
        let Some(state) = reg
            .pools
            .get_mut(&id)
            .and_then(PoolEntry::v4_mut)
            .map(|(_, s)| s)
        else {
            return;
        };
        state.sqrt_price_x96 = update.sqrt_price_x96;
        state.liquidity = update.liquidity;
        state.tick = update.tick;
        state.tick_data = update.tick_data;
        // Two-stamp rule (V4 twin of `sync_v3_pool_state`): wholesale sync
        // replaces both clocks with the same source block (sanctioned reset).
        state.update_block = update.update_block;
        state.tick_data_block = update.update_block;
        state.invalidate_tick_range_cache();
    }
    // ------------------------------------------------------------------
    // Capability interface for registry-side consumers
    // ------------------------------------------------------------------

    /// Enumerate the registered V4 pools - `(pool_manager, pool_id)` ->
    /// internal pool id. Registry-side consumers (the sim-anchor projection,
    /// the storage probe) see V4 registration ONLY through this
    /// capability-scoped read.
    pub(crate) fn registered_v4_pools(
        &self,
    ) -> impl Iterator<
        Item = (
            &(Address, degenbot_decoders::v4_swap_decoder::V4PoolId),
            &u64,
        ),
    > {
        self.v4_pool_ids.iter()
    }

    /// Unregistration seam (ADR-007 U3): drop every buffered V3 event for
    /// `address` so a re-register never replays stale Mint/Burn events.
    pub(crate) fn discard_v3_buffered(&mut self, address: &Address) {
        self.v3_buffer.discard_for(address);
    }

    /// Unregistration seam (ADR-007 U3): remove the `(pool_manager, pool_id)`
    /// V4 registration, returning its internal pool id (`None` = never
    /// registered).
    pub(crate) fn remove_v4_registration(
        &mut self,
        key: &(Address, degenbot_decoders::v4_swap_decoder::V4PoolId),
    ) -> Option<u64> {
        self.v4_pool_ids.remove(key)
    }

    /// Unregistration seam (ADR-007 U3): drop every buffered V4 event for `key`.
    pub(crate) fn discard_v4_buffered(
        &mut self,
        key: &(Address, degenbot_decoders::v4_swap_decoder::V4PoolId),
    ) {
        self.v4_buffer.discard_for(key);
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
    /// Read access to the CL orchestration capability — the seam for callers
    /// that need only CL-local state and never the registry core.
    #[must_use]
    pub fn cl(&self) -> &ClOrchestration {
        &self.cl
    }

    /// Mutable access to the CL orchestration capability. Operations that
    /// must also mutate the registry core go through the composition-root
    /// methods, which own the split borrow.
    pub fn cl_mut(&mut self) -> &mut ClOrchestration {
        &mut self.cl
    }
}

/// Delegating composition-root surface: every CL capability method stays
/// reachable on `BotState` (the pub surface external crates consume);
/// each wrapper is a one-line split-borrow delegation.
impl BotState {
    /// Delegates to the CL orchestration capability - see [`ClOrchestration::register_v3_pool`].
    pub fn register_v3_pool(
        &mut self,
        params: &RegisterV3PoolParams,
    ) -> Result<u64, RegisterV3PoolError> {
        self.cl.register_v3_pool(&mut self.registry, params)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::update_v3_pool`].
    pub fn update_v3_pool(
        &mut self,
        pool_address: Address,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: Vec<(i32, TickBefore)>,
    ) {
        self.cl.update_v3_pool(
            &mut self.registry,
            pool_address,
            sqrt_price_x96,
            liquidity,
            tick,
            block_number,
            tick_priors,
        )
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::route_v3_event`].
    pub fn route_v3_event(
        &mut self,
        phase: Phase,
        pool_address: Address,
        event: BufferedV3PoolEvent,
        swap_priors: &[(i32, TickInfo)],
    ) -> ApplyOutcome {
        self.cl
            .route_v3_event(&mut self.registry, phase, pool_address, event, swap_priors)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::apply_v3_swap`].
    pub fn apply_v3_swap(
        &mut self,
        pool_address: Address,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)],
    ) -> Option<u64> {
        self.cl.apply_v3_swap(
            &mut self.registry,
            pool_address,
            sqrt_price_x96,
            liquidity,
            tick,
            block_number,
            tick_priors,
        )
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::apply_v3_swap_by_pool_id`].
    pub fn apply_v3_swap_by_pool_id(
        &mut self,
        pool_id: u64,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)],
    ) -> Option<u64> {
        self.cl.apply_v3_swap_by_pool_id(
            &mut self.registry,
            pool_id,
            sqrt_price_x96,
            liquidity,
            tick,
            block_number,
            tick_priors,
        )
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::apply_v3_liquidity_update`].
    pub fn apply_v3_liquidity_update(
        &mut self,
        pool_address: Address,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) -> Option<u64> {
        self.cl.apply_v3_liquidity_update(
            &mut self.registry,
            pool_address,
            tick_lower,
            tick_upper,
            liquidity_delta,
            block_number,
        )
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::apply_v3_liquidity_update_by_pool_id`].
    pub fn apply_v3_liquidity_update_by_pool_id(
        &mut self,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) -> Option<u64> {
        self.cl.apply_v3_liquidity_update_by_pool_id(
            &mut self.registry,
            pool_id,
            tick_lower,
            tick_upper,
            liquidity_delta,
            block_number,
        )
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::sync_tick_data_by_pool_id`].
    pub fn sync_tick_data_by_pool_id(
        &mut self,
        pool_id: u64,
        tick_data: HashMap<i32, TickInfo>,
        update_block: u64,
    ) -> bool {
        self.cl
            .sync_tick_data_by_pool_id(&mut self.registry, pool_id, tick_data, update_block)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::mark_bitmap_words_known_by_pool_id`].
    pub fn mark_bitmap_words_known_by_pool_id(&mut self, pool_id: u64, words: &[i32]) -> bool {
        self.cl
            .mark_bitmap_words_known_by_pool_id(&mut self.registry, pool_id, words)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::buffer_backfill_v3_liquidity_update`].
    pub fn buffer_backfill_v3_liquidity_update(
        &mut self,
        pool_address: Address,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) {
        self.cl.buffer_backfill_v3_liquidity_update(
            &mut self.registry,
            pool_address,
            tick_lower,
            tick_upper,
            liquidity_delta,
            block_number,
        )
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::apply_backfill_buffer_v3`].
    pub fn apply_backfill_buffer_v3(&mut self, address: &Address) {
        self.cl
            .apply_backfill_buffer_v3(&mut self.registry, address)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::apply_pump_buffer_v3`].
    pub fn apply_pump_buffer_v3(&mut self, address: &Address) {
        self.cl.apply_pump_buffer_v3(&mut self.registry, address)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::buffered_v3_event_count`].
    pub fn buffered_v3_event_count(&self, address: &Address) -> usize {
        self.cl.buffered_v3_event_count(address)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::get_v3_pool`].
    pub fn get_v3_pool(&self, pool_id: u64) -> Option<&V3PoolState> {
        self.cl.get_v3_pool(&self.registry, pool_id)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::get_v3_identity`].
    pub fn get_v3_identity(&self, pool_id: u64) -> Option<&V3PoolIdentity> {
        self.cl.get_v3_identity(&self.registry, pool_id)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::v3_pools_snapshot`].
    pub fn v3_pools_snapshot(&self) -> HashMap<u64, (V3PoolIdentity, V3PoolState)> {
        self.cl.v3_pools_snapshot(&self.registry)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::set_snapshot_seed_block`].
    pub fn set_snapshot_seed_block(&mut self, s: Option<u64>) {
        self.cl.set_snapshot_seed_block(s)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::v3_snapshot_seed`].
    pub fn v3_snapshot_seed(&self, address: Address) -> Option<&HashMap<i32, TickInfo>> {
        self.cl.v3_snapshot_seed(&self.registry, address)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::take_v3_snapshot_seed`].
    pub fn take_v3_snapshot_seed(&mut self, address: Address) -> Option<HashMap<i32, TickInfo>> {
        self.cl.take_v3_snapshot_seed(&mut self.registry, address)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::pin_v3_post_drain_snapshot`].
    pub fn pin_v3_post_drain_snapshot(&mut self, address: Address) {
        self.cl
            .pin_v3_post_drain_snapshot(&mut self.registry, address)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::take_v3_post_drain_snapshot`].
    pub fn take_v3_post_drain_snapshot(
        &mut self,
        address: Address,
    ) -> Option<(HashMap<i32, TickInfo>, u64)> {
        self.cl
            .take_v3_post_drain_snapshot(&mut self.registry, address)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::sync_v3_pool_state`].
    pub fn sync_v3_pool_state(
        &mut self,
        pool_address: Address,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        tick_data: HashMap<i32, TickInfo>,
        update_block: u64,
    ) {
        self.cl.sync_v3_pool_state(
            &mut self.registry,
            pool_address,
            sqrt_price_x96,
            liquidity,
            tick,
            tick_data,
            update_block,
        )
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::tick_fingerprint`].
    pub fn tick_fingerprint(&self, pool_id: u64) -> Option<(u64, usize, u64)> {
        self.cl.tick_fingerprint(&self.registry, pool_id)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::stage_word_fetch_by_pool_id`].
    pub fn stage_word_fetch_by_pool_id(
        &mut self,
        pool_id: u64,
        word: i32,
        block: u64,
        retried: bool,
    ) -> Option<StagedWordFetch> {
        self.cl
            .stage_word_fetch_by_pool_id(&self.registry, pool_id, word, block, retried)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::install_word_fetch`].
    pub fn install_word_fetch(
        &mut self,
        staged: &StagedWordFetch,
        fetched: &::degenbot_pools::tick_fetch::FetchedTickWord,
    ) -> InstallWordOutcome {
        self.cl
            .install_word_fetch(&mut self.registry, staged, fetched)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::ensure_word_known_by_pool_id`].
    pub fn ensure_word_known_by_pool_id(&mut self, pool_id: u64, word: i32, block: u64) -> bool {
        self.cl
            .ensure_word_known_by_pool_id(&mut self.registry, pool_id, word, block)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::merge_tick_word`].
    pub fn merge_tick_word(
        &mut self,
        pool_id: u64,
        fetched: &::degenbot_pools::tick_fetch::FetchedTickWord,
    ) -> bool {
        self.cl
            .merge_tick_word(&mut self.registry, pool_id, fetched)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::v3_pool_count`].
    pub fn v3_pool_count(&self) -> usize {
        self.cl.v3_pool_count(&self.registry)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::register_v4_state_view`].
    pub fn register_v4_state_view(&mut self, pool_manager: Address, state_view: Address) {
        self.cl.register_v4_state_view(pool_manager, state_view)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::register_v4_pool`].
    pub fn register_v4_pool(
        &mut self,
        params: &RegisterV4PoolParams,
    ) -> Result<u64, RegisterV4PoolError> {
        self.cl.register_v4_pool(&mut self.registry, params)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::route_v4_event`].
    pub fn route_v4_event(
        &mut self,
        phase: Phase,
        pool_manager: Address,
        v4_pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId,
        event: BufferedV4PoolEvent,
        swap_priors: &[(i32, TickInfo)],
    ) -> ApplyOutcome {
        self.cl.route_v4_event(
            &mut self.registry,
            phase,
            pool_manager,
            v4_pool_id,
            event,
            swap_priors,
        )
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::apply_v4_swap`].
    pub fn apply_v4_swap(&mut self, update: &V4SwapUpdate, block_number: u64) -> Option<u64> {
        self.cl
            .apply_v4_swap(&mut self.registry, update, block_number)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::apply_v4_liquidity_update`].
    pub fn apply_v4_liquidity_update(
        &mut self,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: alloy::primitives::I256,
        block_number: u64,
    ) -> Option<u64> {
        self.cl.apply_v4_liquidity_update(
            &mut self.registry,
            pool_manager,
            pool_id,
            tick_lower,
            tick_upper,
            liquidity_delta,
            block_number,
        )
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::apply_v4_swap_by_pool_id`].
    pub fn apply_v4_swap_by_pool_id(
        &mut self,
        pool_id: u64,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)],
    ) -> Option<u64> {
        self.cl.apply_v4_swap_by_pool_id(
            &mut self.registry,
            pool_id,
            sqrt_price_x96,
            liquidity,
            tick,
            block_number,
            tick_priors,
        )
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::apply_v4_liquidity_update_by_pool_id`].
    pub fn apply_v4_liquidity_update_by_pool_id(
        &mut self,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) -> Option<u64> {
        self.cl.apply_v4_liquidity_update_by_pool_id(
            &mut self.registry,
            pool_id,
            tick_lower,
            tick_upper,
            liquidity_delta,
            block_number,
        )
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::buffer_backfill_v4_liquidity_update`].
    pub fn buffer_backfill_v4_liquidity_update(
        &mut self,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: alloy::primitives::I256,
        block_number: u64,
    ) {
        self.cl.buffer_backfill_v4_liquidity_update(
            &mut self.registry,
            pool_manager,
            pool_id,
            tick_lower,
            tick_upper,
            liquidity_delta,
            block_number,
        )
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::apply_backfill_buffer_v4`].
    pub fn apply_backfill_buffer_v4(
        &mut self,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) {
        self.cl
            .apply_backfill_buffer_v4(&mut self.registry, pool_manager, pool_id)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::apply_pump_buffer_v4`].
    pub fn apply_pump_buffer_v4(
        &mut self,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) {
        self.cl
            .apply_pump_buffer_v4(&mut self.registry, pool_manager, pool_id)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::v3_pool_coverage`].
    pub fn v3_pool_coverage(&self, address: Address) -> Option<PoolTickCoverage> {
        self.cl.v3_pool_coverage(&self.registry, address)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::v4_pool_coverage`].
    pub fn v4_pool_coverage(
        &self,
        pool_manager: Address,
        pool_id: &degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) -> Option<PoolTickCoverage> {
        self.cl
            .v4_pool_coverage(&self.registry, pool_manager, pool_id)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::set_v3_pool_quarantined`].
    pub fn set_v3_pool_quarantined(&mut self, address: Address) {
        self.cl.set_v3_pool_quarantined(&mut self.registry, address)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::set_v4_pool_quarantined`].
    pub fn set_v4_pool_quarantined(
        &mut self,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) {
        self.cl
            .set_v4_pool_quarantined(&mut self.registry, pool_manager, pool_id)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::set_v3_pool_live`].
    pub fn set_v3_pool_live(&mut self, address: Address) {
        self.cl.set_v3_pool_live(&mut self.registry, address)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::set_v4_pool_live`].
    pub fn set_v4_pool_live(
        &mut self,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) {
        self.cl
            .set_v4_pool_live(&mut self.registry, pool_manager, pool_id)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::release_all_v3_v4_quarantined`].
    pub fn release_all_v3_v4_quarantined(&mut self) {
        self.cl.release_all_v3_v4_quarantined(&mut self.registry)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::get_v4_pool`].
    pub fn get_v4_pool(&self, pool_id: u64) -> Option<&V4PoolState> {
        self.cl.get_v4_pool(&self.registry, pool_id)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::get_v4_identity`].
    pub fn get_v4_identity(&self, pool_id: u64) -> Option<&V4PoolIdentity> {
        self.cl.get_v4_identity(&self.registry, pool_id)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::v4_pool_id_by_key`].
    pub fn v4_pool_id_by_key(
        &self,
        pool_manager: Address,
        pool_id: &degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) -> Option<u64> {
        self.cl.v4_pool_id_by_key(pool_manager, pool_id)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::try_registered_v4`].
    pub fn try_registered_v4(
        &self,
        pool_manager: Address,
        pool_id: &degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) -> Option<RegisteredV4> {
        self.cl
            .try_registered_v4(&self.registry, pool_manager, pool_id)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::v4_snapshot_seed`].
    pub fn v4_snapshot_seed(
        &self,
        pool_manager: Address,
        pool_id: &degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) -> Option<&HashMap<i32, TickInfo>> {
        self.cl
            .v4_snapshot_seed(&self.registry, pool_manager, pool_id)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::take_v4_snapshot_seed`].
    pub fn take_v4_snapshot_seed(
        &mut self,
        pool_manager: Address,
        pool_id: &degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) -> Option<HashMap<i32, TickInfo>> {
        self.cl
            .take_v4_snapshot_seed(&mut self.registry, pool_manager, pool_id)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::pin_v4_post_drain_snapshot`].
    pub fn pin_v4_post_drain_snapshot(
        &mut self,
        pool_manager: Address,
        pool_id: &degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) {
        self.cl
            .pin_v4_post_drain_snapshot(&mut self.registry, pool_manager, pool_id)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::take_v4_post_drain_snapshot`].
    pub fn take_v4_post_drain_snapshot(
        &mut self,
        pool_manager: Address,
        pool_id: &degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) -> Option<(HashMap<i32, TickInfo>, u64)> {
        self.cl
            .take_v4_post_drain_snapshot(&mut self.registry, pool_manager, pool_id)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::v4_pools_snapshot`].
    pub fn v4_pools_snapshot(&self) -> HashMap<u64, (V4PoolIdentity, V4PoolState)> {
        self.cl.v4_pools_snapshot(&self.registry)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::sync_v4_pool_state`].
    pub fn sync_v4_pool_state(
        &mut self,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId,
        update: V4StateSync,
    ) {
        self.cl
            .sync_v4_pool_state(&mut self.registry, pool_manager, pool_id, update)
    }

    /// Delegates to the CL orchestration capability - see [`ClOrchestration::snapshot_seed_block`].
    #[must_use]
    pub const fn snapshot_seed_block(&self) -> Option<u64> {
        self.cl.snapshot_seed_block()
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
    use degenbot_pools::v3_state::PoolTickCoverage;
    use degenbot_pools::v4_state::V4PoolKey;
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
            .try_registered_v4(pm, &pid)
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
            .try_registered_v4(pm, &pid)
            .expect("registered pool must resolve");
        assert_eq!(existing.coverage, PoolTickCoverage::Sparse);
    }

    #[test]
    fn unknown_key_resolves_none() {
        let mut core = BotState::new();
        let pm = Address::from([0x44u8; 20]);
        let _ = register_v4(&mut core, [0xeeu8; 32], PoolTickCoverage::Sparse, 0);

        assert_eq!(
            core.try_registered_v4(pm, &[0xffu8; 32]),
            None,
            "an unregistered pool_id under the same manager must miss"
        );
        assert_eq!(
            core.try_registered_v4(Address::from([0x45u8; 20]), &[0xeeu8; 32]),
            None,
            "an unknown pool_manager must miss"
        );
    }
}

#[cfg(test)]
mod known_word_dispatcher_tests {
    use hashbrown::HashMap;

    use alloy::primitives::{Address, U256};

    use degenbot_pools::v3_state::{PoolTickCoverage, V3PoolState};

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
            "dispatcher must report a CL apply"
        );
        let state = core.get_v3_pool(pool_id).expect("registered");
        assert!(state.known_bitmap_words.contains(&7));
    }

    #[test]
    #[expect(clippy::expect_used)]
    fn mark_bitmap_words_known_by_pool_id_tracked_noop() {
        let (mut core, pool_id) = register_v3(PoolTickCoverage::Tracked);
        let _ = core.mark_bitmap_words_known_by_pool_id(pool_id, &[7]);
        let state = core.get_v3_pool(pool_id).expect("registered");
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
        let state = core.get_v3_pool(pool_id).expect("registered");
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
