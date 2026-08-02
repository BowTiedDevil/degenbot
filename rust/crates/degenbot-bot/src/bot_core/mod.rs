//! `BotState` — the single owner of all runtime state.
//!
//! All pool data, token metadata, calculation methods, and swap encoding
//! live here. Python objects are thin `PyO3` handles carrying keys into
//! `BotState`'s `HashMaps`.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use alloy::primitives::{aliases::U112, Address, I256, U256};

use ::degenbot_pools::state_history::{
    JournalError, ReorgPoolState, ScalarPriors, TickBefore, V3BlockDelta,
};
use degenbot_uniswap::v2_encoding::{encode_v2_swap, EncodedCall};

pub mod balancer_stable_state;
pub mod balancer_weighted_state;
pub mod block_clock;
pub mod block_pump;
pub mod bot;
pub mod construction_io;
pub mod curve_state;
pub mod divergence_probe;
pub mod drain_sink;
pub mod engine;
pub mod liquidity_verifier;
pub mod log_dispatcher;
pub mod reorg_coordinator;
pub mod snapshot_verify;
pub mod solve_coordinator;
pub mod solver_state_verifier;
pub mod tick_assembly;
pub mod v3_state;

// Re-export the merged V3/V4/Curve state types (ADR-003: BotState owns
// pool state; Curve is the ADR-003 "third family").
pub use ::degenbot_pools::aerodrome_v2_state::{
    AerodromeV2PoolIdentity, AerodromeV2PoolState, RegisterAerodromeV2PoolParams,
};
pub use ::degenbot_pools::curve_data_provider::{CurveDataProvider, CurveDataProviderError};
pub use ::degenbot_pools::rate_provider::{
    BalancerRateProvider, RateProviderError, StaticRateProvider,
};
pub use ::degenbot_pools::spec_bounds::{SpecValue, SpecViolation, UINT112_MAX};
pub use ::degenbot_pools::state_history::BalancesBlockDelta;
pub use balancer_stable_state::{
    BalancerStablePoolIdentity, BalancerStablePoolState, RegisterBalancerStablePoolParams,
};
pub use balancer_weighted_state::{
    BalancerWeightedPoolIdentity, BalancerWeightedPoolState, RegisterBalancerWeightedPoolParams,
};
pub use curve_state::{CurvePoolIdentity, CurvePoolState, RegisterCurvePoolParams};
pub use divergence_probe::{TrackedSlotKind, TrackedSlotProbe};
pub use v3_state::{
    v3_simulate_swap, BufferedV3LiquidityUpdate, BufferedV3PoolEvent, BufferedV3SwapEvent,
    PoolTickCoverage, RegisterV3PoolError, RegisterV3PoolParams, RegistrationLifecycle,
    SimulateSwapError, V3PoolIdentity, V3PoolState, V3SwapOutcome, V3SwapUpdate,
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

// Re-export the ADR-008 per-block state machine core (pure; the pump drives
// it — see `bot_core/block_clock.rs`).
pub use block_clock::{BlockClock, BlockState, HeaderDecision, LogDecision};

// ---------------------------------------------------------------------------
// Pool state types
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Pool registry sum type + V2 identity/state + token entry + swap-sim dispatch.
// **Relocated** to `degenbot-pools`; re-exported here at the historical
// `bot_core::*` paths so consumers resolve unchanged.
// Transient re-export — repointed at `degenbot_pools::*` natively by USPN7M/P2CKRL.
// ---------------------------------------------------------------------------

pub use ::degenbot_pools::registry::{
    ConcentratedLiquidityPool, ConcentratedLiquidityPoolMut, PoolEntry, TokenEntry,
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
    /// The snapshot seed block `S = min(fetch_newest_update_block(V3), V4)`.
    /// Set by `Bot::load_snapshot_from_db` (or `load_snapshot_from_py`) when a
    /// snapshot is loaded; consumed by the auto-backfill (B1/J3FMDO) that
    /// closes the `S+1..W-1` gap before resume. `None` when no snapshot was
    /// loaded (cold-start path — the pump anchors on `first_observed_block`).
    snapshot_seed_block: Option<u64>,
    /// The highest FULLY-DELIVERED block — the shared handle to the pump's
    /// `BlockClock` tombstone cutoff (3M5PO5). The registration drain reads
    /// this as the `drain_pump_completed` cutoff instead of a buffer-local
    /// shadow marker; `0` (or the buffer as a whole not yet seeded) means no
    /// block has been tombstoned → nothing drains. Seeded by the pump at
    /// startup via [`set_pump_complete_cutoff`](Self::set_pump_complete_cutoff).
    pump_complete_cutoff: Arc<AtomicU64>,
}

/// Diagnostic: log every V3 pump-buffer INSERTION for the pool address named
/// by `DEGENBOT_DRAIN_DBG` (a hex address, with or without `0x`). Companion
/// to the drain-side `[dbg-drain]` logs in `apply_backfill_buffer_v3`/
/// `apply_pump_buffer_v3` — diffing insertion vs drain logs reveals whether a
/// Mint that on-chain shows at block N was (a) buffered-then-missed-by-drain
/// (insertion logged, no matching drain apply) or (b) never buffered at all
/// (no insertion log). `tag` ∈ {'L' (unregistered Live-eligible path),
/// 'Q' (Quarantined deferral)}. Gated on the env var so it is a no-op in
/// production runs that don't opt in.
/// Whether `DEGENBOT_DRAIN_DBG` is set to `address` (hex, with or without
/// `0x`). The single pool-match predicate shared by every per-pool trace probe
/// — keeps the `[trace]` / `[dbg-buf]` / `[dbg-drain]` series gated on ONE
/// env var so a single run surfaces the full event flow (WS delivery →
/// decode → apply-route → buffer → drain → pin → verify) for the failing
/// pool with no behavior change when unset.
pub(crate) fn drain_dbg_pool_match(address: Address) -> bool {
    std::env::var("DEGENBOT_DRAIN_DBG")
        .is_ok_and(|v| format!("{address:x}").eq_ignore_ascii_case(v.trim_start_matches("0x")))
}

/// Whether the global liquidity-events trace is on (env
/// `DEGENBOT_TRACE_LIQUIDITY=1`). When set, the `[trace] apply-route` and
/// `[trace] ws-log` probes fire for EVERY V3 Mint/Burn + V4 `ModifyLiquidity`
/// event across ALL pools (not just the `DEGENBOT_DRAIN_DBG` one). Liquidity
/// events are rare vs Swaps, so the volume is bounded; the value is that a
/// non-deterministic failure that HOPS pools (the add-applied/remove-buffered
/// split of a same-block `ModifyLiquidity` pair) is captured for whichever
/// pool it lands on. Pairs with `DEGENBOT_DRAIN_DBG` (per-pool) — either gate
/// fires the probe.
pub(crate) fn trace_liquidity_global() -> bool {
    std::env::var("DEGENBOT_TRACE_LIQUIDITY")
        .is_ok_and(|v| v.trim() == "1" || v.trim().eq_ignore_ascii_case("true"))
}

/// Optional watch tick for the per-pool trace (env `DEGENBOT_TRACE_TICK`, a
/// signed decimal). When set, the pin summary + drain-apply probes log the
/// value of THAT tick after each mutation, so a single known-divergent tick
/// (e.g. the ghost-value upper tick of a same-block Mint+Burn) can be tracked
/// across the rolling-start lifecycle. Unset = no per-tick watch.
pub(crate) fn trace_watch_tick() -> Option<i32> {
    std::env::var("DEGENBOT_TRACE_TICK")
        .ok()
        .and_then(|v| v.trim().parse::<i32>().ok())
}

fn drain_dbg_log_buf(
    address: Address,
    tag: char,
    tick_lower: i32,
    tick_upper: i32,
    liquidity_delta: i128,
    block_number: u64,
) {
    if !drain_dbg_pool_match(address) {
        return;
    }
    tracing::info!(
        %tag,
        pool_addr = %format!("{address:x}"),
        tick_lower,
        tick_upper,
        liquidity_delta,
        block_number,
        "[dbg-buf] INSERT"
    );
}

/// Per-pool WS-pump trace: log every relevant-topic log the live pump
/// dispatches for the traced pool. Emits block, log-index, tx-index,
/// topic0, removed flag, and the ADR-008 clock decision — so the
/// delivery order of same-block Mint/Burn/ModifyLiquidity logs is visible
/// (a Burn arriving after the registration drain+pin is the rolling-start
/// race this probe exists to catch). Fires when the pool matches
/// `DEGENBOT_DRAIN_DBG` OR the global liquidity trace is on AND the topic is
/// a liquidity-mutating one (V3 Mint/Burn, V4 `ModifyLiquidity`)..
#[allow(clippy::too_many_arguments)]
pub(crate) fn trace_ws_log_dispatch(
    address: Address,
    block_number: u64,
    log_index: Option<u64>,
    tx_index: Option<u64>,
    topic0: alloy::primitives::B256,
    removed: bool,
    decision: &str,
) {
    use degenbot_decoders::v3_mint_burn_decoder::{V3_BURN_TOPIC, V3_MINT_TOPIC};
    use degenbot_decoders::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC;
    let is_liquidity =
        topic0 == V3_MINT_TOPIC || topic0 == V3_BURN_TOPIC || topic0 == V4_MODIFY_LIQUIDITY_TOPIC;
    let pool_match = drain_dbg_pool_match(address);
    let global_liquidity_hit = trace_liquidity_global() && is_liquidity;
    if !pool_match && !global_liquidity_hit {
        return;
    }
    tracing::info!(
        pool_addr = %format!("{address:x}"),
        block = block_number,
        log_index = ?log_index,
        tx_index = ?tx_index,
        topic0 = %topic0, // full topic — greppable by short prefix
        removed,
        decision = %decision,
        "[trace] ws-log"
    );
}

/// Per-pool apply-route trace: log how a V3 liquidity update was routed —
///`(lifecycle, routed_to)` where `routed_to` ∈ {"buffer-pump",
/// "buffer-pump-quarantined", "direct-live", "no-pool"}. Answers whether the
/// Mint/Burn hit the pump buffer (then drained) or was direct-applied to a
/// Live pool (then captured by the pin or missed it). Fires when the pool
/// matches `DEGENBOT_DRAIN_DBG` OR the global liquidity trace is on.
/// V3 `Swap`-arrival trace: log every swap the live pump dispatches for a
/// `DEGENBOT_DRAIN_DBG`-named pool, with its on-chain `sqrt_price_x96`,
/// `liquidity`, `tick`, and `block`. Answers whether a within-tick swap that
/// should have advanced the pool's sqrtPrice actually ARRIVED (and with what
/// value) — the discriminator between a swap that was never delivered and one
/// that was delivered but not applied. Fires only when the pool matches
/// `DEGENBOT_DRAIN_DBG`; zero cost otherwise.
#[allow(clippy::too_many_arguments)]
pub(crate) fn trace_apply_swap_v3(
    pool_address: Address,
    sqrt_price_x96: U256,
    liquidity: u128,
    tick: i32,
    block_number: u64,
) {
    if !drain_dbg_pool_match(pool_address) {
        return;
    }
    tracing::info!(
        pool_addr = %format!("{pool_address:x}"),
        family = "V3",
        sqrt_price_x96 = %sqrt_price_x96,
        liquidity,
        tick,
        block = block_number,
        "[trace] swap-apply"
    );
}

/// V4 twin of [`trace_apply_swap_v3`] — logs a V4 `Swap` dispatch keyed by
/// `pool_id_hex` (the V4 analog of the pool address for `DEGENBOT_DRAIN_DBG`
/// matching). Fires when `DEGENBOT_DRAIN_DBG` names this `pool_id_hex`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn trace_apply_swap_v4(
    pool_manager: Address,
    pool_id_hex: &str,
    sqrt_price_x96: U256,
    liquidity: u128,
    tick: i32,
    block_number: u64,
) {
    if !std::env::var("DEGENBOT_DRAIN_DBG")
        .is_ok_and(|v| v.trim_start_matches("0x").eq_ignore_ascii_case(pool_id_hex))
    {
        return;
    }
    tracing::info!(
        pool_manager = %format!("{pool_manager:x}"),
        pool_id = %pool_id_hex,
        family = "V4",
        sqrt_price_x96 = %sqrt_price_x96,
        liquidity,
        tick,
        block = block_number,
        "[trace] swap-apply"
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn trace_apply_route_v3(
    address: Address,
    tick_lower: i32,
    tick_upper: i32,
    liquidity_delta: i128,
    block_number: u64,
    lifecycle: &str,
    routed_to: &str,
) {
    if !drain_dbg_pool_match(address) && !trace_liquidity_global() {
        return;
    }
    tracing::info!(
        pool_addr = %format!("{address:x}"),
        family = "V3",
        tick_lower,
        tick_upper,
        liquidity_delta,
        block = block_number,
        lifecycle = %lifecycle,
        routed_to = %routed_to,
        "[trace] apply-route"
    );
}

/// V4 twin of [`trace_apply_route_v3`] — logs how a V4 `ModifyLiquidity`
/// update was routed (`buffer-pump` / `buffer-pump-quarantined` /
/// `direct-live` / `no-pool`). Keyed by `(pool_manager, pool_id_hex)` so the
/// failing V4 pool's add/remove split is visible across the registration
/// lifecycle transition. Fires on the global liquidity trace OR when
/// `DEGENBOT_DRAIN_DBG` names this pool's `pool_id_hex`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn trace_apply_route_v4(
    pool_manager: Address,
    pool_id_hex: &str,
    tick_lower: i32,
    tick_upper: i32,
    liquidity_delta: alloy::primitives::I256,
    block_number: u64,
    lifecycle: &str,
    routed_to: &str,
) {
    if !trace_liquidity_global()
        && !std::env::var("DEGENBOT_DRAIN_DBG")
            .is_ok_and(|v| v.trim_start_matches("0x").eq_ignore_ascii_case(pool_id_hex))
    {
        return;
    }
    tracing::info!(
        pool_manager = %format!("{pool_manager:x}"),
        pool_id = %pool_id_hex,
        family = "V4",
        tick_lower,
        tick_upper,
        liquidity_delta = %liquidity_delta,
        block = block_number,
        lifecycle = %lifecycle,
        routed_to = %routed_to,
        "[trace] apply-route"
    );
}

/// Whether the verify-diagnostics probes are enabled.
///
/// Set `DEGENBOT_VERIFY_DBG` (any value) to opt the bot into the structural
/// visibility probes that diagnose intermittent liquidity-map
/// verification misses at startup (the pump / drain / verifier concurrency
/// window). The probes are pure `log::info!` emission gated on this flag —
/// zero behavior change, zero runtime cost when unset (a single env-var
/// `is_ok` check per call site, same posture as `DEGENBOT_DRAIN_DBG`).
///
/// Probes gated here:
/// - `mark_v3/v4_pump_block_complete` logs the count of pump events at or
///   below the marked block (a `mark_complete(W)` with zero pump events for
///   an active pool proves the pump never delivered block W's logs — the
///   subscribe→resume drop).
/// - `pin_v3/v4_post_drain_snapshot` logs the pinned `(update_block,
///   tick_data.len(), pump_count_at_or_below, last_complete_block)` so a
///   step-2 mismatch can be correlated to the drain that produced the pin.
///   NOTE: `update_block` may legitimately exceed `last_complete_block` when
///   the registration seed carries the live WS head while the pump buffer has
///   not yet tombstoned it (a benign `pump_count_at_or_below == 0` case). It
///   is NOT by itself a bug signal — the real failure symptom is a divergent
///   `tick_data` entry (ghost gross/net) against on-chain at the pinned block
///   (the `[verify-dbg] divergence set`). Do not read `update_block >
///   last_complete_block` alone as evidence of a leaked in-progress event.
/// - `set_v3/v4_pool_live` logs the count + block numbers of the retained
///   in-progress-block tail flushed via the unguarded `drain_pump`.
fn verify_dbg_enabled() -> bool {
    std::env::var("DEGENBOT_VERIFY_DBG").is_ok()
}

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
    /// `Swap` / Mint-Burn event. `0` for an unregistered pool (the freshness
    /// gate treats 0 as stale: a missing pool defers its path until registered).
    ///
    /// Used by the per-path state-freshness gate (ergo AV42C7): before solving
    /// a path at `solve_block`, every hop's `update_block` must be `>=
    /// solve_block`, else the path is deferred — its backrun would otherwise
    /// land at a block where one pool still holds the prior block's state, and
    /// the solver's prediction diverges from on-chain reality by the mid-block
    /// move (the constant, amount-independent +1 V3 / V4 drift class that turned
    /// the V3-V3-V3 IIA reverts on the USDC/WETH anchor).
    #[must_use]
    pub fn pool_update_block(&self, pool_id: u64) -> u64 {
        self.pools.get(&pool_id).map_or(0, PoolEntry::update_block)
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
            snapshot_seed_block: None,
            pump_complete_cutoff: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Share the pump's `BlockClock` tombstone cutoff with `BotState` so the
    /// registration drain reads the same "highest fully-delivered block" the
    /// pump's own clock tracks — the single source of truth (3M5PO5). Called
    /// once by the pump at startup; a no-op elsewhere.
    pub fn set_pump_complete_cutoff(&mut self, handle: Arc<AtomicU64>) {
        self.pump_complete_cutoff = handle;
    }

    /// The current shared pump-completeness cutoff (`0` until the first
    /// tombstone). Test/diagnostic read of the value the registration drain
    /// gates on.
    #[must_use]
    pub fn pump_complete_cutoff(&self) -> u64 {
        self.pump_complete_cutoff.load(Ordering::Relaxed)
    }

    /// Monotonically advance the shared pump-completeness cutoff. The live
    /// pump mutates the SAME value via `BlockClock::tombstone`; this direct
    /// advance is for tests that drive the registration drain without a pump.
    pub fn advance_pump_complete_cutoff(&mut self, block: u64) {
        let cur = self.pump_complete_cutoff.load(Ordering::Relaxed);
        if block > cur {
            self.pump_complete_cutoff.store(block, Ordering::Relaxed);
        }
    }

    /// Register a V2 pool by contract address.
    ///
    /// Returns the auto-assigned pool ID.
    ///
    /// # Errors
    ///
    /// Returns [`RegisterV2PoolError::AlreadyRegistered`] if a pool at this
    /// address is already registered (replaces the prior `assert!` panic).
    /// Returns [`RegisterV2PoolError::SpecViolation`] when `reserve0` or
    /// `reserve1` exceed `uint112::MAX` — the on-chain `uint112` storage width
    /// v2-core asserts at `UniswapV2Pair._update`. Living pool state from
    /// `Sync(uint112,uint112)` events is structurally spec-bound, so spec
    /// checks fire only on synthetic / corrupt registration.
    pub fn register_v2_pool(
        &mut self,
        params: &RegisterV2PoolParams,
    ) -> Result<u64, RegisterV2PoolError> {
        // Spec-bound admission (epic WOYYS2 / MSTAT2): reject up-front rather
        // than propagating overlarge reserves into `V2PoolState` (where the
        // downstream swap-math U512→U256 narrowing would silently degrade to
        // `U256::MAX` under the prior sat-cap, or panic — see the helper's
        // `# Panics` section committed in `19218a2c`).
        ::degenbot_pools::spec_bounds::validate_v2_reserve(params.reserve0, "reserve0")?;
        ::degenbot_pools::spec_bounds::validate_v2_reserve(params.reserve1, "reserve1")?;
        if self.pool_addresses.contains_key(&params.address) {
            return Err(RegisterV2PoolError::AlreadyRegistered {
                address: params.address,
            });
        }

        let pool_id = self.next_pool_id;
        self.next_pool_id += 1;

        // Construct (identity, state) + genesis journal delta on the state
        // struct (ADR-014 D6/Q7 — V2 joins its 6 siblings; the construction
        // + genesis-delta push moved out of `register_v2_pool` into
        // `V2PoolState::from_params`).
        let (identity, state) = V2PoolState::from_params(params, self.journal_depth);

        self.pools.insert(pool_id, PoolEntry::V2(identity, state));
        self.pool_addresses.insert(params.address, pool_id);

        Ok(pool_id)
    }

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
    pub fn register_v3_pool(
        &mut self,
        params: &RegisterV3PoolParams,
    ) -> Result<u64, RegisterV3PoolError> {
        use ::degenbot_pools::spec_bounds as sb;
        sb::validate_sqrt_price(params.sqrt_price_x96)
            .map_err(RegisterV3PoolError::SpecViolation)?;
        sb::validate_tick(params.tick).map_err(RegisterV3PoolError::SpecViolation)?;
        sb::validate_v3_fee(params.fee).map_err(RegisterV3PoolError::SpecViolation)?;
        sb::validate_tick_spacing(params.tick_spacing)
            .map_err(RegisterV3PoolError::SpecViolation)?;

        if self.pool_addresses.contains_key(&params.address) {
            return Err(RegisterV3PoolError::AlreadyRegistered {
                address: params.address,
            });
        }

        // [diag] registration-seed probe: log every V3 pool's seed scalar state
        // (update_block + sqrtPriceX96 + tick) so a solver-state mismatch can be
        // traced to its seed. Gated on `DEGENBOT_TRACE_REGISTER_SEED=1` (off by
        // default; run_bot.sh sets it for diagnosis). A pool seeded with an
        // `update_block` well behind the head + an old sqrt is the stale-seed
        // hypothesis; a head-fresh seed points the finger at a post-registration
        // rewind instead.
        if std::env::var("DEGENBOT_TRACE_REGISTER_SEED").is_ok() {
            tracing::info!(
                pool_addr = %format!("{:x}", params.address),
                family = "V3",
                seed_update_block = params.update_block,
                seed_sqrt = %params.sqrt_price_x96,
                seed_tick = params.tick,
                coverage = ?params.coverage,
                "[diag] register-v3-seed"
            );
        }

        let pool_id = self.next_pool_id;
        self.next_pool_id += 1;
        let address = params.address;

        // RUQ637/XEANMB: the `seed_from_store` path is retired — the DB
        // seeding is handled by the Db arm of `assemble_v3_tick_map` (held
        // snapshot tx). Just clone + flow the params through.
        let params = params.clone();
        let (identity, state) = V3PoolState::from_params(params, self.journal_depth);
        self.pools.insert(pool_id, PoolEntry::V3(identity, state));
        self.pool_addresses.insert(address, pool_id);

        Ok(pool_id)
    }

    /// Register a Curve `StableSwap` pool by contract address.
    ///
    /// ADR-005 slice 11a (state port) — the third `PoolEntry` family. Carries
    /// immutable config (tokens, A, fee, variant strategy enums, base-pool
    /// reference) + the registration-time mutable state (`balances`,
    /// `update_block`). Seeds the reorg journal with a genesis anchor (mirror
    /// of V2's discipline) so the balance-vector trait dispatcher
    /// (`restore_balance_vector_before_block`, ADR-016) can land on the
    /// registration state.
    ///
    /// Returns the auto-assigned pool ID.
    ///
    /// # Panics
    ///
    /// Panics if the pool address is already registered. The caller pre-checks
    /// hook / dynamic-fee rejection at the Python seam for V4; Curve has no
    /// analogous admission floor in this sub-slice (the stableswap math stays
    /// Python-side at calc time, so there's no Rust correctness floor to
    /// enforce yet).
    pub fn register_curve_pool(&mut self, params: &RegisterCurvePoolParams) -> u64 {
        assert!(
            !self.pool_addresses.contains_key(&params.address),
            "pool already registered: {}",
            params.address
        );

        assert!(
            params.balances.len() == params.tokens.len()
                && params.balances.len() == params.rate_multipliers.len(),
            "Curve params mismatch: tokens={}, balances={}, rate_multipliers={} (must all be N)",
            params.tokens.len(),
            params.balances.len(),
            params.rate_multipliers.len(),
        );

        let pool_id = self.next_pool_id;
        self.next_pool_id += 1;

        let (identity, state) = CurvePoolState::from_params(params.clone(), self.journal_depth);
        self.pools
            .insert(pool_id, PoolEntry::Curve(identity, state));
        self.pool_addresses.insert(params.address, pool_id);

        pool_id
    }

    /// Apply a Curve `external_update` (new balances from an `Exchange` event)
    /// by `pool_id` — the `PyLiquidityPool.apply_curve_balance_update` backing.
    ///
    /// Journals the prior balances (genesis-anchor V2-style discipline), then
    /// lands the new balances + `update_block`. Returns the affected `pool_id`,
    /// or `None` if not registered / not a Curve pool (silent no-op — don't
    /// corrupt a V2/V3/V4 pool).
    ///
    /// # Panics
    ///
    /// Panics if `balances.len()` doesn't match the registered pool's coin
    /// count — a wiring/programming error (the builder always passes an
    /// `Exchange`-decoded balance tuple of the right arity).
    #[must_use]
    /// Apply a balance-vector update (a Curve `Exchange` event or a Balancer
    /// Vault `PoolBalanceChanged` event) keyed by the handle's `pool_id`,
    /// dispatching through `BalanceVectorPoolState::apply_balance_update`
    /// (ADR-017 D1 — replaces the three per-family
    /// `apply_curve_balance_update_by_pool_id` /
    /// `apply_balancer_weighted_balance_update_by_pool_id` /
    /// `apply_balancer_stable_balance_update_by_pool_id` methods, whose bodies
    /// were byte-identical modulo the arity `assert!` message).
    ///
    /// Returns `Some(pool_id)` if the pool is a balance-vector family
    /// (Curve / `BalancerWeighted` / `BalancerStable`); `None` otherwise (silent
    /// no-op — mirrors the per-family silent-no-op contract on a non-matching
    /// family, e.g. a V2 `pool_id`).
    pub fn apply_balance_update_by_pool_id(
        &mut self,
        pool_id: u64,
        balances: Vec<U256>,
        block_number: u64,
    ) -> Option<u64> {
        let entry = self.pools.get_mut(&pool_id)?;
        entry
            .as_balance_vector_mut()?
            .apply_balance_update(balances, block_number);
        Some(pool_id)
    }

    /// Read a registered Curve pool's state by `pool_id`.
    ///
    /// The Python companion (slice 11b) reads `balances` / `update_block`
    /// through this accessor via `PyLiquidityPool.balances` getter. Returns
    /// `None` for non-Curve pools (silent no-op).
    #[must_use]
    pub fn get_curve_pool(&self, pool_id: u64) -> Option<&CurvePoolState> {
        self.pools
            .get(&pool_id)
            .and_then(PoolEntry::curve)
            .map(|(_, state)| state)
    }

    /// Look up a Curve pool's immutable registration identity (address,
    /// tokens, fee, `admin_fee`, `rate_multipliers`, variant enums, `base_pool`).
    /// Returns `None` if the pool is not registered or isn't a Curve pool.
    #[must_use]
    pub fn get_curve_identity(&self, pool_id: u64) -> Option<&CurvePoolIdentity> {
        self.pools
            .get(&pool_id)
            .and_then(PoolEntry::curve)
            .map(|(identity, _)| identity)
    }

    // --- ADR-005 slice 12a: Balancer V2 weighted state port -------------

    /// Register a Balancer V2 weighted pool. The pool's immutable config
    /// (`pool_id`, vault, tokens, weights, `scaling_factors`, `swap_fee`,
    /// `pow_version`) + the registration `balances`/`update_block` are stored
    /// in a `BalancerWeightedPoolState` and seeded with a genesis reorg
    /// journal delta. The Python `BalancerV2Pool` companion (slice 12b) will
    /// be built over a `PyLiquidityPool` handle that reads back through
    /// [`Self::get_balancer_weighted_pool`].
    ///
    /// # Panics
    ///
    /// Panics if the pool's address is already registered, or if
    /// `balances.len()` doesn't match `tokens.len()` / `weights.len()` /
    /// `scaling_factors.len()` (a builder wiring error — the builder always
    /// passes N-token tuples of consistent arity).
    pub fn register_balancer_weighted_pool(
        &mut self,
        params: &RegisterBalancerWeightedPoolParams,
    ) -> u64 {
        assert!(
            !self.pool_addresses.contains_key(&params.address),
            "pool already registered: {}",
            params.address
        );

        assert!(
            params.balances.len() == params.tokens.len()
                && params.balances.len() == params.weights.len()
                && params.balances.len() == params.scaling_factors.len(),
            "Balancer weighted params mismatch: tokens={}, balances={}, weights={}, scaling_factors={} (must all be N)",
            params.tokens.len(),
            params.balances.len(),
            params.weights.len(),
            params.scaling_factors.len(),
        );

        let pool_id = self.next_pool_id;
        self.next_pool_id += 1;

        let (identity, state) =
            BalancerWeightedPoolState::from_params(params.clone(), self.journal_depth);
        self.pools
            .insert(pool_id, PoolEntry::BalancerWeighted(identity, state));
        self.pool_addresses.insert(params.address, pool_id);

        pool_id
    }

    /// Read a registered Balancer weighted pool's state by `pool_id`.
    ///
    /// The Python companion (slice 12b) reads `balances` / `update_block`
    /// through this accessor via `PyLiquidityPool` getters. Returns `None`
    /// for non-Balancer-weighted pools (silent no-op).
    #[must_use]
    pub fn get_balancer_weighted_pool(&self, pool_id: u64) -> Option<&BalancerWeightedPoolState> {
        self.pools
            .get(&pool_id)
            .and_then(PoolEntry::balancer_weighted)
            .map(|(_, state)| state)
    }

    /// Look up a Balancer weighted pool's immutable registration identity
    /// (address, vault, `pool_id`, tokens, weights, `scaling_factors`, `swap_fee`,
    /// `pow_version`). Returns `None` if not registered or not a weighted pool.
    #[must_use]
    pub fn get_balancer_weighted_identity(
        &self,
        pool_id: u64,
    ) -> Option<&BalancerWeightedPoolIdentity> {
        self.pools
            .get(&pool_id)
            .and_then(PoolEntry::balancer_weighted)
            .map(|(identity, _)| identity)
    }

    // --- ADR-005 slice 12c: Balancer V2 stable state port --------------

    /// Register a Balancer V2 stable pool. The pool's immutable config
    /// (`pool_id`, vault, tokens, amp, `scaling_factors`, `swap_fee`,
    /// `bpt_idx`, `invariant_version`) + the registration `balances`/
    /// `update_block` are stored in a `BalancerStablePoolState` and seeded
    /// with a genesis reorg journal delta. The Python `BalancerV2StablePool`
    /// companion (slice 12d) will be built over a `PyLiquidityPool` handle
    /// that reads back through [`Self::get_balancer_stable_pool`].
    ///
    /// # Panics
    ///
    /// Panics if the pool's address is already registered, if
    /// `balances.len()` doesn't match `tokens.len()` / `scaling_factors.len()`,
    /// or if `bpt_idx` is `Some(i)` with `i >= tokens.len()` (a builder
    /// wiring error — the builder always passes N-token tuples of
    /// consistent arity and resolves `bpt_idx` via `detect_bpt_index`).
    pub fn register_balancer_stable_pool(
        &mut self,
        params: &RegisterBalancerStablePoolParams,
    ) -> u64 {
        assert!(
            !self.pool_addresses.contains_key(&params.address),
            "pool already registered: {}",
            params.address
        );

        assert!(
            params.balances.len() == params.tokens.len()
                && params.balances.len() == params.scaling_factors.len(),
            "Balancer stable params mismatch: tokens={}, balances={}, scaling_factors={} (must all be N)",
            params.tokens.len(),
            params.balances.len(),
            params.scaling_factors.len(),
        );

        if let Some(idx) = params.bpt_idx {
            assert!(
                idx < params.tokens.len(),
                "Balancer stable bpt_idx {} >= tokens.len() {} (BPT must be in-token-list)",
                idx,
                params.tokens.len(),
            );
        }

        let pool_id = self.next_pool_id;
        self.next_pool_id += 1;

        let (identity, state) =
            BalancerStablePoolState::from_params(params.clone(), self.journal_depth);
        self.pools
            .insert(pool_id, PoolEntry::BalancerStable(identity, state));
        self.pool_addresses.insert(params.address, pool_id);

        pool_id
    }

    /// Read a registered Balancer stable pool's state by `pool_id`.
    ///
    /// The Python companion (slice 12d) reads `balances` / `update_block` /
    /// `bpt_idx` / `invariant_version` / `amp` through this accessor via
    /// `PyLiquidityPool` getters. Returns `None` for non-Balancer-stable
    /// pools (silent no-op).
    #[must_use]
    pub fn get_balancer_stable_pool(&self, pool_id: u64) -> Option<&BalancerStablePoolState> {
        self.pools
            .get(&pool_id)
            .and_then(PoolEntry::balancer_stable)
            .map(|(_, state)| state)
    }

    /// Look up a Balancer stable pool's immutable registration identity
    /// (address, vault, `pool_id`, tokens, amp, `scaling_factors`, `swap_fee`,
    /// `bpt_idx`, `invariant_version`). Returns `None` if not registered or not
    /// a stable pool.
    #[must_use]
    pub fn get_balancer_stable_identity(
        &self,
        pool_id: u64,
    ) -> Option<&BalancerStablePoolIdentity> {
        self.pools
            .get(&pool_id)
            .and_then(PoolEntry::balancer_stable)
            .map(|(identity, _)| identity)
    }

    /// Apply a V2 `Sync` event to a registered pool's state.
    ///
    /// This is the live-path mutation method (ADR-003): journals the prior
    /// reserves, then updates `reserve0`/`reserve1`/`update_block` in place.
    /// Returns the affected `pool_id` so the engine can mark the right path set
    /// dirty; returns `None` if the pool is not registered (a no-op).
    ///
    /// # Panics
    ///
    /// Panics if a `pool_id` is found in `pool_addresses` but not in `pools`
    /// (should never happen — they are inserted together).
    #[must_use]
    pub fn apply_v2_sync(
        &mut self,
        pool_address: Address,
        reserve0: U112,
        reserve1: U112,
        block_number: u64,
    ) -> Option<u64> {
        // ADR-014 D1: delegate to the pool_id-keyed dispatcher (the V3
        // address-keyed wrapper pattern). The inline body that previously
        // lived here was byte-identical to `V2PoolState::apply_sync`, which the
        // twin reaches via `as_reserve_pair_mut()?.apply_sync(...)` — the
        // duplication (the bug-hiding class D1 was written to kill) is removed;
        // the address→pool_id resolution is what this wrapper owns.
        let &pool_id = self.pool_addresses.get(&pool_address)?;
        self.apply_sync_by_pool_id(pool_id, reserve0, reserve1, block_number)
    }

    /// Update a V2 pool's reserves from a Sync event.
    ///
    /// Looks up the pool by contract address. No-op if the pool is not registered.
    /// Thin wrapper over [`apply_v2_sync`](Self::apply_v2_sync) that discards
    /// the returned `pool_id` (kept for the `PyBot` surface).
    ///
    /// # Panics
    ///
    /// Panics if a `pool_id` is found in `pool_addresses` but not in `pools`
    /// (should never happen — they are inserted together).
    pub fn update_v2_pool(
        &mut self,
        pool_address: Address,
        reserve0: U112,
        reserve1: U112,
        block_number: u64,
    ) {
        let _ = self.apply_v2_sync(pool_address, reserve0, reserve1, block_number);
    }

    /// Apply a V2 `Sync` by `pool_id` — the `PyLiquidityPool.sync_reserves`
    /// backing. Returns the affected `pool_id`, or `None` if not registered /
    /// not a V2 pool (no-op). Journals the prior reserves then lands the new.
    #[must_use]
    /// Apply a reserve-pair `Sync` event keyed by the handle's `pool_id`,
    /// dispatching through `ReservePairPoolState::apply_sync` (ADR-017 D3 —
    /// replaces the two per-family `apply_v2_sync_by_pool_id` /
    /// `apply_aerodrome_sync_by_pool_id` dispatchers, whose bodies were
    /// byte-identical modulo the variant name). Covers both V2 and Aerodrome
    /// pools (Solidly mirrors v2-core's `Sync(uint112, uint112)`).
    ///
    /// Returns `Some(pool_id)` if the pool is a reserve-pair family
    /// (V2 / `AerodromeV2`); `None` otherwise (silent no-op — a CL / Curve /
    /// Balancer `pool_id` yields `None`).
    pub fn apply_sync_by_pool_id(
        &mut self,
        pool_id: u64,
        reserve0: U112,
        reserve1: U112,
        block_number: u64,
    ) -> Option<u64> {
        let entry = self.pools.get_mut(&pool_id)?;
        entry
            .as_reserve_pair_mut()?
            .apply_sync(reserve0, reserve1, block_number);
        Some(pool_id)
    }

    /// Read a registered V2 pool's state by `pool_id`.
    ///
    /// The solve engine reads state by reference through this accessor
    /// (ADR-003: "Pool's authority over its own math") and builds the
    /// orientation-specific `IntHopState` at resolve time from `zero_for_one`.
    #[must_use]
    pub fn get_v2_pool_state(&self, pool_id: u64) -> Option<&V2PoolState> {
        self.pools
            .get(&pool_id)
            .and_then(PoolEntry::v2)
            .map(|(_, state)| state)
    }

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

    /// Look up a V2 pool's immutable registration identity (address, tokens,
    /// fees, factory, variant, stable-strategy inputs). Returns `None` if the
    /// pool is not registered or isn't a V2 pool.
    #[must_use]
    pub fn get_v2_identity(&self, pool_id: u64) -> Option<&V2PoolIdentity> {
        self.pools
            .get(&pool_id)
            .and_then(PoolEntry::v2)
            .map(|(identity, _)| identity)
    }

    /// Snapshot a V2 pool's current mutable state (reserves + block) under one
    /// read guard (ADR-005 slice 4 step 3). Returns `None` if the pool is not
    /// registered or isn't a V2 pool (no V2 state to read).
    ///
    /// The Python companion's `state` property + `simulate_*` methods build a
    /// `UniswapV2PoolState` from this single snapshot so a Rust-side
    /// `sync_reserves` (pump update) can't interleave between separate reads —
    /// the `StateCache.lock()` atomicity the drop-`StateCache` refactor loses.
    #[must_use]
    pub fn v2_snapshot(&self, pool_id: u64) -> Option<(U256, U256, u64)> {
        let state = self.get_v2_pool_state(pool_id)?;
        Some((
            state.reserve0.to::<U256>(),
            state.reserve1.to::<U256>(),
            state.update_block,
        ))
    }

    /// Update a V3 pool's state from a Swap event.
    ///
    /// Looks up the pool by contract address. No-op if the pool is not registered.
    /// Stashes scalar "before" values (and any provided per-tick priors) in the
    /// reorg journal before updating. Kept as the `PyBot` entry; the live
    /// pump path uses [`apply_v3_swap`](Self::apply_v3_swap) (which returns the
    /// affected `pool_id` and overlays `tick_priors` into `tick_data`).
    pub fn update_v3_pool(
        &mut self,
        pool_address: Address,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: Vec<(i32, TickBefore)>,
    ) {
        let Some(&pool_id) = self.pool_addresses.get(&pool_address) else {
            return;
        };

        let Some(PoolEntry::V3(_, state)) = self.pools.get_mut(&pool_id) else {
            return;
        };

        // Stash "before" values in the reorg journal before updating
        state.journal.push_delta(V3BlockDelta {
            block: block_number,
            scalar_priors: Some(ScalarPriors {
                sqrt_price_x96_before: state.sqrt_price_x96,
                liquidity_before: state.liquidity,
                tick_before: state.tick,
            }),
            tick_priors,
        });

        state.sqrt_price_x96 = sqrt_price_x96;
        state.liquidity = liquidity;
        state.tick = tick;
        state.update_block = block_number;
        state.invalidate_tick_range_cache();
    }

    /// Apply a V3 `Swap` event to a registered pool's state (ADR-003 live path).
    ///
    /// Mirrors the dissolved `V3BlockEngine::apply_swap`: overlays `tick_priors`
    /// into `tick_data` (the live pump path passes `&[]` — swaps don't modify
    /// `tick_data`), sets the scalar fields, invalidates the tick-range cache,
    /// journals the prior scalars (and any provided per-tick priors) for reorg
    /// rollback, and returns the affected `pool_id`. Returns `None` if the pool
    /// is not registered (a no-op). I/O-free; the engine calls this under the
    /// core lock inside the engine lock (engine-then-core ordering).
    pub fn apply_v3_swap(
        &mut self,
        pool_address: Address,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)],
    ) -> Option<u64> {
        let &pool_id = self.pool_addresses.get(&pool_address)?;
        trace_apply_swap_v3(pool_address, sqrt_price_x96, liquidity, tick, block_number);
        // 6N7XVR: a `Quarantined` pool defers the live `Swap` to the pump
        // buffer. A `Swap` does NOT touch `tick_data` (the pump path passes
        // `tick_priors: &[]`), but it DOES set `update_block = block_number` —
        // so without deferral a live `Swap` at an in-progress block N+1 would
        // advance the pin's `update_block` to N+1 while a buffered same-block
        // `Mint`/`Burn` stays retained → the same mismatch the liquidity-only
        // deferral was meant to prevent (the 25647112 reproduction). `Live`
        // applies directly (the steady-state contract).
        if let Some(PoolEntry::V3(_, state)) = self.pools.get(&pool_id) {
            if state.registration_lifecycle == RegistrationLifecycle::Quarantined {
                self.v3_buffer.buffer_pump(
                    pool_address,
                    BufferedV3PoolEvent::Swap(BufferedV3SwapEvent {
                        sqrt_price_x96,
                        liquidity,
                        tick,
                        block_number,
                    }),
                );
                return None;
            }
        }
        self.apply_v3_swap_by_pool_id(
            pool_id,
            sqrt_price_x96,
            liquidity,
            tick,
            block_number,
            tick_priors,
        )
    }

    /// Apply a V3 Swap event keyed by the handle's `pool_id` (plan-101 slice 8a).
    ///
    /// Same semantics as [`apply_v3_swap`] but skips address resolution —
    /// the `PyLiquidityPool` handle already holds the canonical `pool_id`, so
    /// this is the one-lock, one-lookup path the handle uses.
    pub fn apply_v3_swap_by_pool_id(
        &mut self,
        pool_id: u64,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)],
    ) -> Option<u64> {
        let Some(PoolEntry::V3(_, state)) = self.pools.get_mut(&pool_id) else {
            return None;
        };
        state.apply_swap(sqrt_price_x96, liquidity, tick, block_number, tick_priors);
        Some(pool_id)
    }

    /// Apply a V3 liquidity update (Mint/Burn) to a registered pool's
    /// `tick_data`, or buffer it for an unregistered pool (ADR-003 live path).
    ///
    /// Registered pool: applies via `apply_liquidity_to_tick_range` (matching
    /// Solidity `Tick.update` — both lower and upper get `liquidity_gross +=
    /// delta`; `liquidity_net` `+=` at lower, `-=` at upper), invalidates the
    /// tick-range cache, returns the affected `pool_id`.
    ///
    /// Unregistered pool: buffers into the pump buffer for staged application
    /// at registration; returns `None`.
    pub fn apply_v3_liquidity_update(
        &mut self,
        pool_address: Address,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) -> Option<u64> {
        let Some(&pool_id) = self.pool_addresses.get(&pool_address) else {
            trace_apply_route_v3(
                pool_address,
                tick_lower,
                tick_upper,
                liquidity_delta,
                block_number,
                "none",
                "buffer-pump",
            );
            drain_dbg_log_buf(
                pool_address,
                'L',
                tick_lower,
                tick_upper,
                liquidity_delta,
                block_number,
            );
            self.v3_buffer.buffer_pump(
                pool_address,
                BufferedV3PoolEvent::Liquidity(BufferedV3LiquidityUpdate {
                    tick_lower,
                    tick_upper,
                    liquidity_delta,
                    block_number,
                }),
            );
            return None;
        };
        // 6N7XVR: a `Quarantined` registered pool defers the live event to the
        // pump buffer (via the same unregistered-buffering path) so the pin's
        // `update_block` cannot outrun `last_complete_block`. `Live` applies
        // directly. The deferral preserves cross-type arrival order within a
        // block (a same-block `Swap` and `Mint` both land in the one buffer).
        if let Some(PoolEntry::V3(_, state)) = self.pools.get(&pool_id) {
            if state.registration_lifecycle == RegistrationLifecycle::Quarantined {
                trace_apply_route_v3(
                    pool_address,
                    tick_lower,
                    tick_upper,
                    liquidity_delta,
                    block_number,
                    "Quarantined",
                    "buffer-pump-quarantined",
                );
                drain_dbg_log_buf(
                    pool_address,
                    'Q',
                    tick_lower,
                    tick_upper,
                    liquidity_delta,
                    block_number,
                );
                self.v3_buffer.buffer_pump(
                    pool_address,
                    BufferedV3PoolEvent::Liquidity(BufferedV3LiquidityUpdate {
                        tick_lower,
                        tick_upper,
                        liquidity_delta,
                        block_number,
                    }),
                );
                return None;
            }
        }
        trace_apply_route_v3(
            pool_address,
            tick_lower,
            tick_upper,
            liquidity_delta,
            block_number,
            "Live",
            "direct-live",
        );
        self.apply_v3_liquidity_update_by_pool_id(
            pool_id,
            tick_lower,
            tick_upper,
            liquidity_delta,
            block_number,
        )
    }

    /// V3 liquidity update keyed by the handle's `pool_id` (plan-101 slice 8a).
    ///
    /// Skips address resolution — the `PyLiquidityPool` handle holds the
    /// canonical `pool_id`, so this is the one-lock, one-lookup path. Registered
    /// pools only (no buffering — the handle's pool is necessarily registered).
    pub fn apply_v3_liquidity_update_by_pool_id(
        &mut self,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) -> Option<u64> {
        let Some(PoolEntry::V3(_, state)) = self.pools.get_mut(&pool_id) else {
            return None;
        };
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
    /// identical `tick_data: HashMap<i32, TickInfo>` (J63J3N).
    #[must_use]
    pub fn sync_tick_data_by_pool_id(
        &mut self,
        pool_id: u64,
        tick_data: HashMap<i32, TickInfo>,
        update_block: u64,
    ) -> bool {
        let Some(entry) = self.pools.get_mut(&pool_id) else {
            return false;
        };
        match entry {
            // CL-family collapse (ADR-014 D2b): the 4-line replace body lives
            // once in `ConcentratedLiquidityPoolMut::replace_tick_data`; each
            // arm only reads its identity's `tick_spacing` (V3 carries it
            // directly, V4 nests it in `pool_key`) and delegates. The
            // `_ => false` arm is the single non-CL / unregistered no-op.
            PoolEntry::V3(identity, state) => {
                state.replace_tick_data(tick_data, update_block, identity.tick_spacing)
            }
            PoolEntry::V4(identity, state) => {
                state.replace_tick_data(tick_data, update_block, identity.pool_key.tick_spacing)
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
    pub fn buffer_backfill_v3_liquidity_update(
        &mut self,
        pool_address: Address,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) {
        if let Some(&key) = self.pool_addresses.get(&pool_address) {
            if let Some(PoolEntry::V3(_, state)) = self.pools.get_mut(&key) {
                // 6N7XVR: a `Quarantined` pool defers ALL live/backfill events
                // to the buffer so the pin's `update_block` cannot outrun
                // `last_complete_block`. `Live` pools apply directly (the
                // steady-state contract). Backfill completes before
                // `build_paths`/quarantine in the normal flow, but a late
                // backfill chunk interleaving with a re-register must respect
                // the lifecycle for the invariant to hold.
                if state.registration_lifecycle == RegistrationLifecycle::Live {
                    ::degenbot_pools::tick_bitmap::apply_liquidity_to_tick_range(
                        &mut state.tick_data,
                        tick_lower,
                        tick_upper,
                        liquidity_delta,
                        block_number,
                    );
                    state.update_block = block_number;
                    state.invalidate_tick_range_cache();
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
    pub fn apply_backfill_buffer_v3(&mut self, address: &Address) {
        // Debug-drain gate: log per-event apply when `DEGENBOT_DRAIN_DBG` is set
        // to this pool's address. Diagnoses same-block Mint+Bun net-zero races
        // where one half is lost between fetch and drain.
        let dbg = std::env::var("DEGENBOT_DRAIN_DBG")
            .is_ok_and(|v| format!("{address:x}").eq_ignore_ascii_case(v.trim_start_matches("0x")));
        let Some(&key) = self.pool_addresses.get(address) else {
            if dbg {
                tracing::info!(pool_addr = %format!("{address:x}"), "[dbg-drain] backfill NOT REGISTERED");
            }
            return;
        };
        let Some(buffered) = self.v3_buffer.drain_backfill(address) else {
            if dbg {
                tracing::info!(pool_addr = %format!("{address:x}"), "[dbg-drain] backfill EMPTY");
            }
            return;
        };
        if dbg {
            tracing::info!(
                pool_addr = %format!("{address:x}"),
                count = buffered.len(),
                "[dbg-drain] backfill"
            );
        }
        for update in buffered {
            if dbg {
                match &update {
                    BufferedV3PoolEvent::Liquidity(u) => {
                        tracing::info!(
                            pool_addr = %format!("{address:x}"),
                            tick_lower = u.tick_lower,
                            tick_upper = u.tick_upper,
                            delta = u.liquidity_delta,
                            block = u.block_number,
                            "[dbg-drain] backfill apply liq"
                        );
                    }
                    BufferedV3PoolEvent::Swap(s) => {
                        tracing::info!(
                            pool_addr = %format!("{address:x}"),
                            liquidity = s.liquidity,
                            tick = s.tick,
                            block = s.block_number,
                            "[dbg-drain] backfill apply swap"
                        );
                    }
                }
            }
            if let Some(PoolEntry::V3(_, state)) = self.pools.get_mut(&key) {
                let ub_before = state.update_block;
                Self::apply_buffered_v3_event(state, update);
                if dbg && state.update_block < ub_before {
                    tracing::warn!(
                        pool_addr = %format!("{address:x}"),
                        ub_before,
                        ub_after = state.update_block,
                        "[dbg-drain] update_block REWIND (backfill)"
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
    pub fn apply_pump_buffer_v3(&mut self, address: &Address) {
        let dbg = std::env::var("DEGENBOT_DRAIN_DBG")
            .is_ok_and(|v| format!("{address:x}").eq_ignore_ascii_case(v.trim_start_matches("0x")));
        let Some(&key) = self.pool_addresses.get(address) else {
            if dbg {
                tracing::info!(pool_addr = %format!("{address:x}"), "[dbg-drain] pump NOT REGISTERED");
            }
            return;
        };
        // YLYJM2: drain ONLY fully-completed blocks. The cutoff is the pump's
        // `BlockClock` tombstone cutoff (3M5PO5) — a block is complete when
        // the first log of N+1 closes N; a drain mid-block would pin
        // `update_block=N` missing a later same-block log. Events for the
        // in-progress block stay buffered.
        let cutoff = self.pump_complete_cutoff.load(Ordering::Relaxed);
        if cutoff == 0 {
            if dbg {
                tracing::info!(pool_addr = %format!("{address:x}"), "[dbg-drain] pump NO-COMPLETE (no tombstone yet)");
            }
            return;
        }
        let Some(buffered) = self.v3_buffer.drain_pump_completed(address, cutoff) else {
            if dbg {
                tracing::info!(pool_addr = %format!("{address:x}"), "[dbg-drain] pump EMPTY (no completed blocks)");
            }
            return;
        };
        if dbg {
            tracing::info!(pool_addr = %format!("{address:x}"), count = buffered.len(), "[dbg-drain] pump");
        }
        for update in buffered {
            if dbg {
                match &update {
                    BufferedV3PoolEvent::Liquidity(u) => tracing::info!(
                        pool_addr = %format!("{address:x}"),
                        tick_lower = u.tick_lower,
                        tick_upper = u.tick_upper,
                        delta = u.liquidity_delta,
                        block = u.block_number,
                        "[dbg-drain] pump apply liq"
                    ),
                    BufferedV3PoolEvent::Swap(s) => tracing::info!(
                        pool_addr = %format!("{address:x}"),
                        liquidity = s.liquidity,
                        tick = s.tick,
                        block = s.block_number,
                        "[dbg-drain] pump apply swap"
                    ),
                }
            }
            if let Some(PoolEntry::V3(_, state)) = self.pools.get_mut(&key) {
                let ub_before = state.update_block;
                Self::apply_buffered_v3_event(state, update);
                if dbg && state.update_block < ub_before {
                    tracing::warn!(
                        pool_addr = %format!("{address:x}"),
                        ub_before,
                        ub_after = state.update_block,
                        "[dbg-drain] update_block REWIND (pump)"
                    );
                }
            }
        }
    }

    /// Set the maximum age (in blocks) for buffered V3 pump events.
    /// `None` means unbounded. Takes effect on the next `expire_v3_buffered`.
    pub const fn set_v3_buffer_max_age(&mut self, max_age: Option<u64>) {
        self.v3_buffer.set_max_age(max_age);
    }

    /// Number of buffered V3 liquidity events for a pool address (backfill + pump).
    #[must_use]
    pub fn buffered_v3_event_count(&self, address: &Address) -> usize {
        self.v3_buffer.event_count(address)
    }

    /// Number of buffered V4 pool events for a `(pool_manager, pool_id)` key
    /// (backfill + pump). 6N7XVR test/diagnostic seam.
    #[must_use]
    pub fn buffered_v4_event_count(
        &self,
        key: &(Address, degenbot_decoders::v4_swap_decoder::V4PoolId),
    ) -> usize {
        self.v4_buffer.event_count(key)
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
    /// registered pool's state. 6N7XVR: the V3 drain loops
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
    /// `build_int_v3_sequence(zfo, 10)` to build the per-hop state.
    #[must_use]
    pub fn get_v3_pool(&self, pool_id: u64) -> Option<&V3PoolState> {
        self.pools
            .get(&pool_id)
            .and_then(PoolEntry::v3)
            .map(|(_, state)| state)
    }

    /// Look up a V3 pool's immutable registration identity (address, tokens,
    /// fee, `tick_spacing`, factory). Returns `None` if the pool is not
    /// registered or isn't a V3 pool.
    #[must_use]
    pub fn get_v3_identity(&self, pool_id: u64) -> Option<&V3PoolIdentity> {
        self.pools
            .get(&pool_id)
            .and_then(PoolEntry::v3)
            .map(|(identity, _)| identity)
    }

    /// Snapshot all V3 pool state for verification (clones every V3 entry).
    ///
    /// Used by `verify_liquidity_maps` so the engine+core locks can be
    /// released before making async RPC calls.
    #[must_use]
    pub fn v3_pools_snapshot(&self) -> HashMap<u64, (V3PoolIdentity, V3PoolState)> {
        self.pools
            .iter()
            .filter_map(|(id, e)| match e {
                PoolEntry::V3(identity, state) => Some((*id, (*identity, state.clone()))),
                PoolEntry::V2(..)
                | PoolEntry::V4(..)
                | PoolEntry::Curve(..)
                | PoolEntry::BalancerWeighted(..)
                | PoolEntry::BalancerStable(..)
                | PoolEntry::AerodromeV2(..) => None,
            })
            .collect()
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

    /// Snapshot seed block `S` setter — the single source of truth for `S`.
    ///
    /// Production paths set `S` here in three ways:
    /// - DB path: `Bot::load_snapshot_from_db` sets `S = min(newest_update_block_v3, v4)`.
    /// - Non-DB path: the `PyArbitrageEngine::set_snapshot_seed_block` setter
    ///   (called by `engine_registry.start()` after `load_*_from_py`) records
    ///   `S = min(newest_block)` from the file/memory snapshot (2SM4Y7).
    /// - Tests: inject `S` directly to drive the `S≥W` / `S=0` no-op branches
    ///   of `BlockPump::backfill_from_snapshot` without a DB (FD7NFG).
    ///
    /// `None` clears the seed (cold-start resume — `BlockPump::resume_from_subscribe`
    /// skips the auto-backfill).
    pub fn set_snapshot_seed_block(&mut self, s: Option<u64>) {
        self.snapshot_seed_block = s;
    }

    /// Read the pinned snapshot seed for a V3 pool (CBCH6H). Returns the
    /// seed if the pool is `Tracked` and the seed has not yet been taken; `None`
    /// for sparse pools or after `take_v3_snapshot_seed`. The seed is the
    /// registration-time `tick_data`, immutable across pump Mint/Burn — step-1
    /// verify compares this against on-chain@snapshot_block (not the
    /// pump-mutated `tick_data` current).
    #[must_use]
    pub fn v3_snapshot_seed(&self, address: Address) -> Option<&HashMap<i32, TickInfo>> {
        let &pool_id = self.pool_addresses.get(&address)?;
        let Some(PoolEntry::V3(_, state)) = self.pools.get(&pool_id) else {
            return None;
        };
        state.snapshot_seed.as_ref()
    }

    /// Take (move out + clear) the pinned snapshot seed for a V3 pool (CBCH6H).
    /// Step-1 verify calls this to read+free the seed in one pass — the seed is
    /// verified exactly once (at the snapshot block during `build_paths`), then
    /// released to bound memory across 18k pools. Returns `None` for sparse
    /// pools or if already taken.
    pub fn take_v3_snapshot_seed(&mut self, address: Address) -> Option<HashMap<i32, TickInfo>> {
        let &pool_id = self.pool_addresses.get(&address)?;
        let Some(PoolEntry::V3(_, state)) = self.pools.get_mut(&pool_id) else {
            return None;
        };
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
    /// — the 2026-06-29 crash). `Some` only for `Tracked` pools; `Sparse`
    /// stays `None` (no complete `tick_data` → step-2 is a no-op). Idempotent
    /// if called twice (the second pin overwrites; only step-2 consumes it).
    pub fn pin_v3_post_drain_snapshot(&mut self, address: Address) {
        // Capture the pin scalars + an optional watch-tick snapshot in an
        // inner scope so the `&mut state` borrow of `self.pools` ends before
        // the diagnostic reads `self.v3_buffer` (a second `&self` borrow).
        let diag = {
            let Some(&pool_id) = self.pool_addresses.get(&address) else {
                return;
            };
            let Some(PoolEntry::V3(_, state)) = self.pools.get_mut(&pool_id) else {
                return;
            };
            if state.coverage == PoolTickCoverage::Tracked {
                let watch = trace_watch_tick()
                    .and_then(|t| state.tick_data.get(&t))
                    .map(|info| (info.liquidity_gross, info.liquidity_net));
                state.post_drain_snapshot = Some((state.tick_data.clone(), state.update_block));
                Some((state.update_block, state.tick_data.len(), watch))
            } else {
                None
            }
        };
        if let Some((update_block, tick_count, watch)) = diag {
            let pool_match = drain_dbg_pool_match(address);
            if verify_dbg_enabled() {
                tracing::info!(
                    pool_addr = %format!("{address:x}"),
                    update_block,
                    tick_count,
                    pump_count = self.v3_buffer.pump_count_at_or_below(&address, update_block),
                    last_complete_block = self.pump_complete_cutoff(),
                    "[verify-dbg] V3 pin"
                );
            }
            // Per-pool watch-tick probe: log (gross, net) at `DEGENBOT_TRACE_TICK`
            // right at the pin, so a ghost-value tick (e.g. an un-burned Mint
            // upper tick) is visible at the moment step-2 verify compares it.
            if pool_match {
                if let Some((g, n)) = watch {
                    tracing::info!(
                        pool_addr = %format!("{address:x}"),
                        update_block,
                        watch_tick = ?trace_watch_tick(),
                        gross = %g,
                        net = %n,
                        "[trace] pin watch-tick"
                    );
                } else {
                    tracing::info!(
                        pool_addr = %format!("{address:x}"),
                        update_block,
                        watch_tick = ?trace_watch_tick(),
                        "[trace] pin watch-tick absent"
                    );
                }
            }
        }
    }

    /// Take (move out + clear) the pinned post-drain `(tick_data, block)` pair
    /// for a V3 pool. Step-2 verify calls this to read+free the pin in one
    /// pass — the pin is verified exactly once (at the pinned block during
    /// `build_paths`), then released to bound memory. The returned block is the
    /// `update_block` captured atomically with the drain; the verify compares
    /// `tick_data` against on-chain@THIS block, NOT a caller-supplied
    /// `verify_backfill_block` constant. Returns `None` for sparse pools, pools
    /// with no drain-yet pin, or if already taken (no-op Ok at the seam).
    pub fn take_v3_post_drain_snapshot(
        &mut self,
        address: Address,
    ) -> Option<(HashMap<i32, TickInfo>, u64)> {
        let &pool_id = self.pool_addresses.get(&address)?;
        let Some(PoolEntry::V3(_, state)) = self.pools.get_mut(&pool_id) else {
            return None;
        };
        state.post_drain_snapshot.take()
    }

    /// Family-dispatching reader for the V3/V4 concentrated-liquidity
    /// families (J63J3N). Returns a trait view over the shared read-only
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
            PoolEntry::V3(_, state) => Some(state),
            PoolEntry::V4(_, state) => Some(state),
            PoolEntry::V2(..)
            | PoolEntry::Curve(..)
            | PoolEntry::BalancerWeighted(..)
            | PoolEntry::BalancerStable(..)
            | PoolEntry::AerodromeV2(..) => None,
        }
    }

    /// Full-sync a V3 pool's `tick_data` from an external source (e.g. Python
    /// backfill). Replaces the entire `tick_data` map (so ticks Burn-removed
    /// on-chain are also removed here) and updates scalar state. No-op if the
    /// pool address is not registered.
    pub fn sync_v3_pool_state(
        &mut self,
        pool_address: Address,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        tick_data: HashMap<i32, TickInfo>,
        update_block: u64,
    ) {
        let Some(&key) = self.pool_addresses.get(&pool_address) else {
            return;
        };
        let Some(PoolEntry::V3(_, state)) = self.pools.get_mut(&key) else {
            return;
        };
        state.sqrt_price_x96 = sqrt_price_x96;
        state.liquidity = liquidity;
        state.tick = tick;
        state.tick_data = tick_data;
        state.update_block = update_block;
        state.invalidate_tick_range_cache();
    }

    /// Calculate the output token amount for a given input amount.
    ///
    /// Uses the constant-product / concentrated-liquidity invariant with
    /// EVM-exact integer arithmetic. Returns `Ok(U256::ZERO)` for the
    /// non-fetchable zeros (pool not found, zero amount, V2/Curve/Balancer
    /// sentinels). A V3/V4 sparse tick-word miss is surfaced as
    /// [`SimulateSwapError::MissingTickWord`] so the caller can fetch + retry;
    /// a `uint256` arithmetic overflow (the on-chain `getAmountOut` `SafeMath`
    /// revert, cdbc03bb) is surfaced as [`SimulateSwapError::NotComputable`].
    ///
    /// Callers MUST NOT swallow `NotComputable` to `U256::ZERO` — that
    /// concealment was the cdbc03bb bug (an on-chain `getAmountOut` revert
    /// surfaced as a silent `0`). Handle `MissingTickWord` by fetching +
    /// retrying (see [`Self::calculate_tokens_out_with_fetch`]) or by mapping
    /// ONLY that variant to zero with an explicit `match`; propagate
    /// `NotComputable` as a panic/error so a real overflow is visible.
    ///
    /// # Errors
    ///
    /// Returns [`SimulateSwapError::MissingTickWord(word)`] when a V3/V4 sparse
    /// pool's walk enters an unfetched tick-bitmap word (the caller fetches +
    /// retries), or [`SimulateSwapError::NotComputable`] on arithmetic
    /// overflow / invariant violation / non-positive amount for a V3/V4 pool.
    pub fn calculate_tokens_out_miss_aware(
        &self,
        pool_id: u64,
        zero_for_one: bool,
        amount_in: U256,
    ) -> Result<U256, SimulateSwapError> {
        let Some(entry) = self.pools.get(&pool_id) else {
            return Ok(U256::ZERO);
        };
        simulate_swap(entry, zero_for_one, amount_in)
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
    /// ADR-005 sparse-map feature parity (slice 2).
    pub fn merge_tick_word(
        &mut self,
        pool_id: u64,
        fetched: &::degenbot_pools::tick_fetch::FetchedTickWord,
    ) -> bool {
        // ADR-017 slice 1: dispatch through `ConcentratedLiquidityPoolMut`
        // (the body lived inlined in V3/V4 arms here; the trait dedups the
        // two). The `bool` wraps the trait's always-`true` return: `false`
        // for non-CL / unregistered pools (the non-CL no-op).
        let Some(entry) = self.pools.get_mut(&pool_id) else {
            return false;
        };
        match entry.as_cl_mut() {
            Some(cl) => cl.merge_tick_word(fetched),
            None => false,
        }
    }

    /// Fetch+retry exact-input swap for sparse V3/V4 pools (ADR-005 slice 2).
    ///
    /// Loops: compute via [`Self::calculate_tokens_out_miss_aware`]; on
    /// [`SimulateSwapError::MissingTickWord(word)`] call the **stored**
    /// fetcher (registered at `register_v3/v4_pool`) for `word`,
    /// [`merge_tick_word`] the result, and retry. Dedup prevents an infinite
    /// refetch of the same word (a repeated miss returns `U256::ZERO`).
    /// [`SimulateSwapError::NotComputable`] and a fetcher error (or no stored
    /// fetcher) return `U256::ZERO`.
    ///
    /// The fetcher returns the word's data (it must NOT write back into
    /// `BotState` itself — re-entrancy is safe because the calc holds no lock
    /// across `fetch_missing_tick_word`; the caller merged result is applied
    /// here). `block` is forwarded to the fetcher as the fetch context.
    #[must_use]
    pub fn calculate_tokens_out_with_fetch(
        &mut self,
        pool_id: u64,
        zero_for_one: bool,
        amount_in: U256,
        block: u64,
    ) -> U256 {
        // Clone the stored `Arc<dyn TickWordFetcher>` off the V3/V4 state
        // before the loop (avoids a self-referential borrow: the loop both
        // calls the fetcher and mutates `self.pools` via `merge_tick_word`).
        let fetcher: Option<Arc<dyn ::degenbot_pools::tick_fetch::TickWordFetcher>> =
            match self.pools.get(&pool_id) {
                Some(PoolEntry::V3(_, state)) => state.fetcher.clone(),
                Some(PoolEntry::V4(_, state)) => state.fetcher.clone(),
                _ => None,
            };
        let mut attempted: HashSet<i32> = HashSet::new();
        loop {
            match self.calculate_tokens_out_miss_aware(pool_id, zero_for_one, amount_in) {
                Ok(out) => return out,
                Err(SimulateSwapError::NotComputable) => return U256::ZERO,
                Err(SimulateSwapError::MissingTickWord(word)) => {
                    // Dedup: a repeated miss on an already-fetched word means
                    // the merge didn't satisfy the simulator (defensive) —
                    // give up rather than loop forever.
                    if !attempted.insert(word) {
                        return U256::ZERO;
                    }
                    let Some(ref fetcher) = fetcher else {
                        return U256::ZERO;
                    };
                    match fetcher.fetch_missing_tick_word(pool_id, word, block) {
                        Ok(data) => {
                            self.merge_tick_word(pool_id, &data);
                        }
                        Err(_) => return U256::ZERO,
                    }
                }
            }
        }
    }

    /// Miss-aware exact-input swap returning the FULL V3/V4 outcome
    /// (amounts + final `sqrt_price_x96`/`liquidity`/`tick`).
    ///
    /// Like [`Self::calculate_tokens_out_miss_aware`] but returns the
    /// [`V3SwapOutcome`] so the caller (the companion's
    /// `simulate_exact_input_swap`) can build `final_state`. Returns
    /// [`SimulateSwapError::NotComputable`] for non-V3/V4 pools (the Rust core
    /// path doesn't simulate Curve/Balancer) or a zero `amount_in`.
    ///
    /// # Errors
    ///
    /// [`SimulateSwapError::MissingTickWord(word)`] when a V3/V4 sparse pool's
    /// walk enters an unfetched tick-bitmap word; [`SimulateSwapError::NotComputable`]
    /// on overflow / invariant violation / non-V3V4 pool / zero amount.
    pub fn simulate_exact_input_swap_miss_aware(
        &self,
        pool_id: u64,
        zero_for_one: bool,
        amount_in: U256,
        sqrt_price_limit: U256,
    ) -> Result<V3SwapOutcome, SimulateSwapError> {
        let Some(entry) = self.pools.get(&pool_id) else {
            return Err(SimulateSwapError::NotComputable);
        };
        if amount_in.is_zero() {
            return Err(SimulateSwapError::NotComputable);
        }
        let Some(spec) = I256::try_from(amount_in).ok() else {
            return Err(SimulateSwapError::NotComputable);
        };
        match entry {
            PoolEntry::V3(identity, state) => v3_simulate_swap(
                state,
                identity.fee,
                identity.tick_spacing,
                zero_for_one,
                spec,
                sqrt_price_limit,
            ),
            // V4 sign convention: exact-input is `amountSpecified < 0`.
            PoolEntry::V4(identity, state) => v4_simulate_swap(
                state,
                identity.pool_key.fee,
                identity.pool_key.tick_spacing,
                zero_for_one,
                -spec,
                sqrt_price_limit,
            ),
            PoolEntry::V2(..)
            | PoolEntry::Curve(..)
            | PoolEntry::BalancerWeighted(..)
            | PoolEntry::BalancerStable(..)
            | PoolEntry::AerodromeV2(..) => Err(SimulateSwapError::NotComputable),
        }
    }

    /// Fetch+retry full-outcome exact-input swap for sparse V3/V4 pools
    /// (ADR-005 slice 3b). Returns the [`V3SwapOutcome`] (amounts + final
    /// state) or `None` if the pool is not V3/V4, the amount is zero, the fetch
    /// failed, or the swap is not computable. Mirrors
    /// [`Self::calculate_tokens_out_with_fetch`] but returns the full outcome.
    ///
    /// Returns `None` (not `Result`) for every failure mode by design —
    /// callers cannot distinguish a fetch miss from `NotComputable` here; use
    /// [`Self::simulate_exact_input_swap_miss_aware`] for miss-aware control.
    pub fn simulate_exact_input_swap_with_fetch(
        &mut self,
        pool_id: u64,
        zero_for_one: bool,
        amount_in: U256,
        sqrt_price_limit: U256,
        block: u64,
    ) -> Option<V3SwapOutcome> {
        let fetcher: Option<Arc<dyn ::degenbot_pools::tick_fetch::TickWordFetcher>> =
            match self.pools.get(&pool_id) {
                Some(PoolEntry::V3(_, state)) => state.fetcher.clone(),
                Some(PoolEntry::V4(_, state)) => state.fetcher.clone(),
                _ => None,
            };
        let mut attempted: HashSet<i32> = HashSet::new();
        loop {
            match self.simulate_exact_input_swap_miss_aware(
                pool_id,
                zero_for_one,
                amount_in,
                sqrt_price_limit,
            ) {
                Ok(outcome) => return Some(outcome),
                Err(SimulateSwapError::NotComputable) => return None,
                Err(SimulateSwapError::MissingTickWord(word)) => {
                    if !attempted.insert(word) {
                        return None;
                    }
                    let fetcher = fetcher.as_ref()?;
                    match fetcher.fetch_missing_tick_word(pool_id, word, block) {
                        Ok(data) => {
                            self.merge_tick_word(pool_id, &data);
                        }
                        Err(_) => return None,
                    }
                }
            }
        }
    }

    /// Miss-aware exact-OUTPUT swap (full outcome). Mirror of
    /// [`Self::simulate_exact_input_swap_miss_aware`] but the caller passes the
    /// desired `amount_out` + the sim derives the required input. V3 sign:
    /// exact-output is `amountSpecified < 0` (V3 negates). V4 sign:
    /// exact-output is `amountSpecified > 0` (V4 doesn't negate). Both are
    /// already handled by `v3_simulate_swap` / `v4_simulate_swap`'s `exact_in`
    /// flag.
    ///
    /// # Errors
    ///
    /// [`SimulateSwapError::MissingTickWord(word)`] when a sparse V3/V4 pool's
    /// walk enters an unfetched tick-bitmap word;
    /// [`SimulateSwapError::NotComputable`] on overflow / invariant violation /
    /// non-V3V4 pool / zero amount.
    pub fn simulate_exact_output_swap_miss_aware(
        &self,
        pool_id: u64,
        zero_for_one: bool,
        amount_out: U256,
        sqrt_price_limit: U256,
    ) -> Result<V3SwapOutcome, SimulateSwapError> {
        let Some(entry) = self.pools.get(&pool_id) else {
            return Err(SimulateSwapError::NotComputable);
        };
        if amount_out.is_zero() {
            return Err(SimulateSwapError::NotComputable);
        }
        let Some(spec) = I256::try_from(amount_out).ok() else {
            return Err(SimulateSwapError::NotComputable);
        };
        match entry {
            // V3 sign convention: exact-output is `amountSpecified < 0`.
            PoolEntry::V3(identity, state) => v3_simulate_swap(
                state,
                identity.fee,
                identity.tick_spacing,
                zero_for_one,
                -spec,
                sqrt_price_limit,
            ),
            // V4 sign convention: exact-output is `amountSpecified > 0`.
            PoolEntry::V4(identity, state) => v4_simulate_swap(
                state,
                identity.pool_key.fee,
                identity.pool_key.tick_spacing,
                zero_for_one,
                spec,
                sqrt_price_limit,
            ),
            PoolEntry::V2(..)
            | PoolEntry::Curve(..)
            | PoolEntry::BalancerWeighted(..)
            | PoolEntry::BalancerStable(..)
            | PoolEntry::AerodromeV2(..) => Err(SimulateSwapError::NotComputable),
        }
    }

    /// Fetch+retry full-outcome exact-OUTPUT swap for sparse V3/V4 pools.
    /// Mirror of [`Self::simulate_exact_input_swap_with_fetch`] but the caller
    /// passes the desired `amount_out` + the sim derives the required input.
    /// Returns `None` on any non-computable / fetch-failure mode (same
    /// discipline as the exact-input variant).
    pub fn simulate_exact_output_swap_with_fetch(
        &mut self,
        pool_id: u64,
        zero_for_one: bool,
        amount_out: U256,
        sqrt_price_limit: U256,
        block: u64,
    ) -> Option<V3SwapOutcome> {
        let fetcher: Option<Arc<dyn ::degenbot_pools::tick_fetch::TickWordFetcher>> =
            match self.pools.get(&pool_id) {
                Some(PoolEntry::V3(_, state)) => state.fetcher.clone(),
                Some(PoolEntry::V4(_, state)) => state.fetcher.clone(),
                _ => None,
            };
        let mut attempted: HashSet<i32> = HashSet::new();
        loop {
            match self.simulate_exact_output_swap_miss_aware(
                pool_id,
                zero_for_one,
                amount_out,
                sqrt_price_limit,
            ) {
                Ok(outcome) => return Some(outcome),
                Err(SimulateSwapError::NotComputable) => return None,
                Err(SimulateSwapError::MissingTickWord(word)) => {
                    if !attempted.insert(word) {
                        return None;
                    }
                    let fetcher = fetcher.as_ref()?;
                    match fetcher.fetch_missing_tick_word(pool_id, word, block) {
                        Ok(data) => {
                            self.merge_tick_word(pool_id, &data);
                        }
                        Err(_) => return None,
                    }
                }
            }
        }
    }

    /// Simulate an exact-input swap over a HYPOTHETICAL (override) pool state.
    ///
    /// Builds a transient `V3PoolState`/`V4PoolState` from the override
    /// scalars (`sqrt_price_x96`, `liquidity`, `tick`) + override `tick_data`,
    /// reusing the registered pool's immutable params (`fee`, `tick_spacing`,
    /// `pool_key` / `factory`). The sim runs over the transient state with the
    /// given `sqrt_price_limit` — NO fetch+retry loop, NO mutation of the
    /// registered `BotState` (the override is a frozen hypothetical; a missing
    /// tick word surfaces as `None`, mirroring the Python frozen-snapshot
    /// override's `MissingLiquidityData`). This is the arbitrage-hypothetical
    /// seam ("what if the pool were at state X?").
    ///
    /// Returns `None` if the pool is not V3/V4, the amount is zero, the sim
    /// is not computable, or the override's tick data is missing a required
    /// word.
    /// Simulate a swap over a HYPOTHETICAL (override) pool state, with
    /// fetch+retry for sparse misses.
    ///
    /// Builds a transient `V3PoolState`/`V4PoolState` from the override
    /// scalars (`sqrt_price_x96`, `liquidity`, `tick`) + override `tick_data`,
    /// reusing the registered pool's immutable params (`fee`, `tick_spacing`,
    /// `pool_key` / `factory`). The sim runs over the transient state with the
    /// given `sqrt_price_limit`. On a `MissingTickWord(word)` miss, the fetcher
    /// is called + the word's ticks are merged into the TRANSIENT state's
    /// `tick_data` + `known_bitmap_words` (NOT registered `BotState` — the
    /// override is a hypothetical that cannot pollute real state), and the sim
    /// retries. Mirrors the Python override path's fetch+retry loop (V3
    /// `_calculate_swap` line 296-345).
    ///
    /// `exact_output = false` -> exact-input (caller passes the input amount);
    /// `exact_output = true` -> exact-output (caller passes the desired output
    /// amount). The V3/V4 sign convention is handled here.
    ///
    /// Returns `None` if the pool is not V3/V4, the amount is zero, the sim
    /// is not computable, the fetcher fails, or the override's tick data is
    /// missing a required word the fetcher cannot resolve.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_lines)]
    pub fn simulate_swap_with_override(
        &self,
        pool_id: u64,
        zero_for_one: bool,
        amount: U256,
        exact_output: bool,
        sqrt_price_limit: U256,
        override_sqrt_price_x96: U256,
        override_liquidity: u128,
        override_tick: i32,
        override_tick_data: HashMap<i32, TickInfo>,
        block: u64,
    ) -> Option<V3SwapOutcome> {
        if amount.is_zero() {
            return None;
        }
        let entry = self.pools.get(&pool_id)?;
        let spec = I256::try_from(amount).ok()?;
        // Clone the stored fetcher off the registered state (the override
        // state is a transient copy — the fetcher itself is shared via `Arc`).
        let fetcher: Option<Arc<dyn ::degenbot_pools::tick_fetch::TickWordFetcher>> = match entry {
            PoolEntry::V3(_, state) => state.fetcher.clone(),
            PoolEntry::V4(_, state) => state.fetcher.clone(),
            _ => None,
        };
        match entry {
            // V3: exact-input is `amountSpecified > 0`; exact-output is `< 0`.
            PoolEntry::V3(identity, state) => {
                let params = RegisterV3PoolParams {
                    address: identity.address,
                    token0: identity.token0,
                    token1: identity.token1,
                    fee: identity.fee,
                    tick_spacing: identity.tick_spacing,
                    factory: identity.factory,
                    deployer: identity.deployer,
                    init_hash: identity.init_hash,
                    sqrt_price_x96: override_sqrt_price_x96,
                    liquidity: override_liquidity,
                    tick: override_tick,
                    tick_data: override_tick_data,
                    update_block: state.update_block,
                    coverage: PoolTickCoverage::Sparse,
                    fetcher: None,
                };
                let (override_identity, mut override_state) =
                    V3PoolState::from_params(params, self.journal_depth);
                let signed = if exact_output { -spec } else { spec };
                let mut attempted: HashSet<i32> = HashSet::new();
                loop {
                    match v3_simulate_swap(
                        &override_state,
                        override_identity.fee,
                        override_identity.tick_spacing,
                        zero_for_one,
                        signed,
                        sqrt_price_limit,
                    ) {
                        Ok(o) => return Some(o),
                        Err(SimulateSwapError::NotComputable) => return None,
                        Err(SimulateSwapError::MissingTickWord(word)) => {
                            if !attempted.insert(word) {
                                return None;
                            }
                            let fetcher = fetcher.as_ref()?;
                            match fetcher.fetch_missing_tick_word(pool_id, word, block) {
                                Ok(data) => {
                                    override_state.merge_tick_word(&data);
                                }
                                Err(_) => return None,
                            }
                        }
                    }
                }
            }
            // V4: exact-input is `amountSpecified < 0`; exact-output is `> 0`.
            PoolEntry::V4(identity, state) => {
                let params = RegisterV4PoolParams {
                    pool_manager: identity.pool_manager,
                    pool_id: identity.pool_id,
                    pool_key: identity.pool_key.clone(),
                    // Registered pool already passed the hook/dynamic-fee gate;
                    // the override only borrows `pool_key` (fee/tick_spacing).
                    hook_flags: 0,
                    protocol_fee: 0,
                    sqrt_price_x96: override_sqrt_price_x96,
                    liquidity: override_liquidity,
                    tick: override_tick,
                    tick_data: override_tick_data,
                    update_block: state.update_block,
                    coverage: PoolTickCoverage::Sparse,
                    fetcher: None,
                };
                let (override_identity, mut override_state) =
                    V4PoolState::from_params(params, self.journal_depth);
                let signed = if exact_output { spec } else { -spec };
                let mut attempted: HashSet<i32> = HashSet::new();
                loop {
                    match v4_simulate_swap(
                        &override_state,
                        override_identity.pool_key.fee,
                        override_identity.pool_key.tick_spacing,
                        zero_for_one,
                        signed,
                        sqrt_price_limit,
                    ) {
                        Ok(o) => return Some(o),
                        Err(SimulateSwapError::NotComputable) => return None,
                        Err(SimulateSwapError::MissingTickWord(word)) => {
                            if !attempted.insert(word) {
                                return None;
                            }
                            let fetcher = fetcher.as_ref()?;
                            match fetcher.fetch_missing_tick_word(pool_id, word, block) {
                                Ok(data) => {
                                    override_state.merge_tick_word(&data);
                                }
                                Err(_) => return None,
                            }
                        }
                    }
                }
            }
            PoolEntry::V2(..)
            | PoolEntry::Curve(..)
            | PoolEntry::BalancerWeighted(..)
            | PoolEntry::BalancerStable(..)
            | PoolEntry::AerodromeV2(..) => None,
        }
    }

    /// Calculate the input token amount required for a given output amount.
    ///
    /// Uses the constant-product invariant with EVM-exact integer arithmetic.
    ///
    /// Returns 0 if the pool is not found, the amount is 0,
    /// or the output exceeds available reserves.
    #[must_use]
    pub fn calculate_tokens_in(&self, pool_id: u64, zero_for_one: bool, amount_out: U256) -> U256 {
        let Some(entry) = self.pools.get(&pool_id) else {
            return U256::ZERO;
        };

        match entry {
            PoolEntry::V2(identity, state) => {
                if amount_out.is_zero() {
                    return U256::ZERO;
                }

                let (reserve_in, reserve_out, gamma_numer, fee_denom) = if zero_for_one {
                    (
                        state.reserve0.to::<U256>(),
                        state.reserve1.to::<U256>(),
                        identity.fee_token0.0,
                        identity.fee_token0.1,
                    )
                } else {
                    (
                        state.reserve1.to::<U256>(),
                        state.reserve0.to::<U256>(),
                        identity.fee_token1.0,
                        identity.fee_token1.1,
                    )
                };

                if amount_out >= reserve_out {
                    return U256::ZERO;
                }

                // constant_product_calc_exact_out:
                // amount_in = 1 + (reserve_in * amount_out * fee_denom) //
                //   ((reserve_out - amount_out) * gamma_numer)
                let numerator = U256::from(reserve_in)
                    .saturating_mul(amount_out)
                    .saturating_mul(U256::from(fee_denom));
                let denominator = (reserve_out.saturating_sub(amount_out))
                    .saturating_mul(U256::from(gamma_numer));

                if denominator.is_zero() {
                    return U256::ZERO;
                }

                U256::from(1) + numerator / denominator
            }
            // V3 concentrated-liquidity math. Exact-output swap: amount_specified
            // < 0 (V3 convention; magnitude = desired output). Input required is
            // token0 for zfo, token1 for ofz (the callback receives the input).
            PoolEntry::V3(identity, state) => {
                if amount_out.is_zero() {
                    return U256::ZERO;
                }
                let Some(spec) = I256::try_from(amount_out).ok() else {
                    return U256::ZERO;
                };
                let Ok(outcome) = v3_simulate_swap(
                    state,
                    identity.fee,
                    identity.tick_spacing,
                    zero_for_one,
                    -spec,
                    V3PoolState::default_sqrt_price_limit(zero_for_one),
                ) else {
                    return U256::ZERO;
                };
                if zero_for_one {
                    outcome.amount0
                } else {
                    outcome.amount1
                }
            }
            // V4: exact-output. V4 sign convention is opposite to V3: V4
            // exact-output uses `amountSpecified > 0` (positive). So the
            // magnitude passed to the V4 simulator is already positive (no
            // negation, unlike V3's `-spec`).
            PoolEntry::V4(identity, state) => {
                if amount_out.is_zero() {
                    return U256::ZERO;
                }
                let Some(spec) = I256::try_from(amount_out).ok() else {
                    return U256::ZERO;
                };
                let Ok(outcome) = v4_simulate_swap(
                    state,
                    identity.pool_key.fee,
                    identity.pool_key.tick_spacing,
                    zero_for_one,
                    spec,
                    V3PoolState::default_sqrt_price_limit(zero_for_one),
                ) else {
                    return U256::ZERO;
                };
                if zero_for_one {
                    outcome.amount0
                } else {
                    outcome.amount1
                }
            }
            // Curve (11a) + Balancer weighted (12a) + Balancer stable (12c):
            // math not ported in their state-port sub-slices; see
            // `calculate_tokens_out`'s combined arm. Returns 0 (the Python
            // companion handles the calc).
            PoolEntry::Curve(..)
            | PoolEntry::BalancerWeighted(..)
            | PoolEntry::BalancerStable(..)
            | PoolEntry::AerodromeV2(..) => U256::ZERO,
        }
    }

    /// Get the pool ID for a given contract address.
    #[must_use]
    pub fn pool_id_by_address(&self, address: &Address) -> Option<u64> {
        self.pool_addresses.get(address).copied()
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

    /// Number of registered V3 pools.
    #[must_use]
    pub fn v3_pool_count(&self) -> usize {
        self.pools
            .values()
            .filter(|e| matches!(e, PoolEntry::V3(..)))
            .count()
    }

    /// Number of registered V2 pools.
    #[must_use]
    pub fn v2_pool_count(&self) -> usize {
        self.pools
            .values()
            .filter(|e| matches!(e, PoolEntry::V2(..)))
            .count()
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
    /// — a reorg touches only a subset of pools). Returns the count of pools
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

    /// Register an Aerodrome V2 pool by contract address (ADR-005 Aerodrome
    /// state port).
    ///
    /// Stores immutable identity (`address`, `token0`, `token1`, `factory`,
    /// `variant`, `stable`, unidirectional `fee`) + the registration reserves
    /// + a genesis reorg-journal anchor (mirror of V2's discipline). Returns
    ///   the auto-assigned pool ID.
    ///
    /// # Panics
    ///
    /// Panics if the pool address is already registered.
    pub fn register_aerodrome_pool(&mut self, params: &RegisterAerodromeV2PoolParams) -> u64 {
        assert!(
            !self.pool_addresses.contains_key(&params.address),
            "pool already registered: {}",
            params.address
        );
        let pool_id = self.next_pool_id;
        self.next_pool_id += 1;
        let (identity, state) = AerodromeV2PoolState::from_params(params, self.journal_depth);
        self.pools
            .insert(pool_id, PoolEntry::AerodromeV2(identity, state));
        self.pool_addresses.insert(params.address, pool_id);
        pool_id
    }

    /// Look up an Aerodrome V2 pool's immutable registration identity. Returns
    /// `None` if not registered or not an Aerodrome pool.
    #[must_use]
    pub fn get_aerodrome_identity(&self, pool_id: u64) -> Option<&AerodromeV2PoolIdentity> {
        self.pools
            .get(&pool_id)
            .and_then(PoolEntry::aerodrome_v2)
            .map(|(identity, _)| identity)
    }

    /// Read a registered Aerodrome V2 pool's state by `pool_id` (reserves +
    /// `update_block` + the reorg journal).
    #[must_use]
    pub fn get_aerodrome_pool(&self, pool_id: u64) -> Option<&AerodromeV2PoolState> {
        self.pools
            .get(&pool_id)
            .and_then(PoolEntry::aerodrome_v2)
            .map(|(_, state)| state)
    }

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
    #[must_use]
    pub fn has_state_prior_to(&self, pool_id: u64, block: u64) -> bool {
        let Some(entry) = self.pools.get(&pool_id) else {
            // Pool not registered → no journal → the reorg can't restore it.
            // Treat as "has state" (no-op) so the caller proceeds to the normal
            // pool-not-found no-op path rather than a fail-stop.
            return true;
        };
        match entry {
            PoolEntry::V2(_, state) => state
                .journal
                .earliest_block()
                .is_some_and(|earliest| earliest < block),
            // No genesis anchor — empty journal is the only too-deep case.
            PoolEntry::V3(_, state) => !state.journal.is_empty(),
            PoolEntry::V4(_, state) => !state.journal.is_empty(),
            // Curve carries a genesis delta (mirror of V2) — a target at/before
            // the genesis block is too-deep: `earliest < block`.
            PoolEntry::Curve(_, state) => state
                .journal
                .earliest_block()
                .is_some_and(|earliest| earliest < block),
            // Balancer weighted carries a genesis delta (mirror of V2/Curve) —
            // ADR-005 slice 12a. Same predicate: `earliest < block`.
            PoolEntry::BalancerWeighted(_, state) => state
                .journal
                .earliest_block()
                .is_some_and(|earliest| earliest < block),
            // Balancer stable carries a genesis delta (mirror of
            // V2/Curve/BalancerWeighted) — ADR-005 slice 12c. Same predicate:
            // `earliest < block`.
            PoolEntry::BalancerStable(_, state) => state
                .journal
                .earliest_block()
                .is_some_and(|earliest| earliest < block),
            // Aerodrome carries a genesis delta (mirror of V2/Curve/Balancer)
            // — ADR-005 Aerodrome slice. Same predicate: `earliest < block`.
            PoolEntry::AerodromeV2(_, state) => state
                .journal
                .earliest_block()
                .is_some_and(|earliest| earliest < block),
        }
    }

    /// Encode a V2 swap call for the given pool.
    ///
    /// Produces pre-encoded calldata for `swap(uint256,uint256,address,bytes)`
    /// that is ready for on-chain submission.
    ///
    /// Returns `None` if the pool ID is not registered.
    #[must_use]
    pub fn encode_swap(
        &self,
        pool_id: u64,
        zero_for_one: bool,
        amount_out: U256,
        recipient: Address,
    ) -> Option<EncodedCall> {
        let entry = self.pools.get(&pool_id)?;
        match entry {
            PoolEntry::V2(identity, _) => {
                let call =
                    encode_v2_swap(identity.address, zero_for_one, amount_out, recipient).ok()?;
                Some(call)
            }
            // V3 encoding is not yet implemented
            PoolEntry::V3(..)
            | PoolEntry::V4(..)
            | PoolEntry::Curve(..)
            | PoolEntry::BalancerWeighted(..)
            | PoolEntry::BalancerStable(..)
            | PoolEntry::AerodromeV2(..) => None,
        }
    }

    // -----------------------------------------------------------------------
    // V4 state (ADR-003: single entry per `(pool_manager, pool_id)`;
    // orientation derived at solve from `zero_for_one`)
    // -----------------------------------------------------------------------

    /// Record the canonical V4 `StateView` contract address for a `pool_manager`
    /// (ADR-005 / Option 2 — Rust owns the mapping). V4 scalar state is read
    /// via the `StateView`'s `getSlot0`/`getLiquidity`, not `getPool` on the
    /// `PoolManager` (which reverts on the canonical deployment); the
    /// solver-state verifier resolves it per-hop via [`BotState::state_view_for`].
    /// Idempotent: the seed for a manager is supplied once by the driver
    /// (read from the `pool_managers` DB row) before V4 pools solve.
    pub fn register_v4_state_view(&mut self, pool_manager: Address, state_view: Address) {
        self.v4_state_views.insert(pool_manager, state_view);
    }

    /// The canonical V4 `StateView` address for `pool_manager`, if registered.
    /// `None` when unknown — the solver-state verifier skips a V4 hop whose
    /// manager's `StateView` has not been seeded (no false alarm on an
    /// un-verifiable hop).
    #[must_use]
    pub fn state_view_for(&self, pool_manager: Address) -> Option<Address> {
        self.v4_state_views.get(&pool_manager).copied()
    }

    /// Register a V4 pool by `(pool_manager, pool_id)`.
    ///
    /// ADR-003 hook filter inline: pools with amount-modifying hooks, dynamic
    /// fees, or static fees exceeding the `cmd_executor`'s 2-byte encoding
    /// limit are rejected. Returns `Err(RegisterV4PoolError)` on rejection.
    ///
    /// # Errors
    ///
    /// Returns [`RegisterV4PoolError::SpecViolation`] when `sqrt_price_x96`,
    /// `tick`, V4 `fee`, or `tick_spacing` violates its Solidity-bounded
    /// on-chain invariant (see [`spec_bounds`]). These checks fire *first* —
    /// before the hooked / dynamic-fee / high-fee / already-registered
    /// rejections — so an impossible-CL-config rejection surfaces the
    /// primitive at fault.
    ///
    /// Returns `Err` if the pool has amount-modifying hooks
    /// (`hook_flags & 0xCC != 0`), uses a dynamic fee (`fee == 0x100000`),
    /// has a static fee exceeding the executor's `u16` encoding field
    /// (`fee >= degenbot_executor::encoders::V4_FEE_ENCODER_MAX`, ergo
    /// DPODAZ), or a pool with the same `(pool_manager, pool_id)` is
    /// already registered.
    pub fn register_v4_pool(
        &mut self,
        params: &RegisterV4PoolParams,
    ) -> Result<u64, RegisterV4PoolError> {
        use ::degenbot_pools::spec_bounds as sb;
        sb::validate_sqrt_price(params.sqrt_price_x96)
            .map_err(RegisterV4PoolError::SpecViolation)?;
        sb::validate_tick(params.tick).map_err(RegisterV4PoolError::SpecViolation)?;
        sb::validate_v4_fee(params.pool_key.fee).map_err(RegisterV4PoolError::SpecViolation)?;
        sb::validate_tick_spacing(params.pool_key.tick_spacing)
            .map_err(RegisterV4PoolError::SpecViolation)?;

        if (params.hook_flags & AMOUNT_MODIFYING_HOOK_MASK) != 0 {
            return Err(RegisterV4PoolError::HookedPool {
                hook_flags: params.hook_flags,
            });
        }
        if params.pool_key.fee == V4_DYNAMIC_FEE_FLAG {
            return Err(RegisterV4PoolError::DynamicFee {
                fee: params.pool_key.fee,
            });
        }
        // DPODAZ: the cmd_executor encodes V4 `fee` as a 2-byte field in both
        // swap commands; a static fee > 65535 is protocol-valid but
        // un-encodable. Reject at admission (mirroring the dynamic-fee floor)
        // so these pools never reach the composer's `u16::try_from` guard and
        // waste a solve cycle.
        if params.pool_key.fee >= degenbot_executor::encoders::V4_FEE_ENCODER_MAX {
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

        let pool_id = self.next_pool_id;
        self.next_pool_id += 1;

        // RUQ637/XEANMB: the `seed_from_store` path is retired — the DB
        // seeding is handled by the Db arm of `assemble_v4_tick_map` (held
        // snapshot tx). Just clone + flow the params through.
        let params = params.clone();
        let (identity, state) = V4PoolState::from_params(params, self.journal_depth);
        self.pools.insert(pool_id, PoolEntry::V4(identity, state));
        self.v4_pool_ids.insert(key, pool_id);

        Ok(pool_id)
    }

    /// Apply a V4 Swap event to a registered pool (ADR-003 live path).
    pub fn apply_v4_swap(&mut self, update: &V4SwapUpdate, block_number: u64) -> Option<u64> {
        // ADR-014 D1: delegate to the pool_id-keyed dispatcher (the V3
        // address-keyed wrapper pattern). The inline body that previously
        // lived here was byte-identical to `impl ConcentratedLiquidityPoolMut
        // for V4PoolState::apply_swap`, which the twin reaches via
        // `state.apply_swap(...)` — the duplication (the bug-hiding class D1
        // was written to kill) is removed; the (pool_manager, pool_id)→pool_id
        // resolution is what this wrapper owns.
        let key = (update.pool_manager, update.pool_id);
        let pool_id_hex = degenbot_core::hex_utils::encode_hex(&update.pool_id);
        trace_apply_swap_v4(
            update.pool_manager,
            &pool_id_hex,
            update.sqrt_price_x96,
            update.liquidity,
            update.tick,
            block_number,
        );
        let &pool_id = self.v4_pool_ids.get(&key)?;
        // 6N7XVR: a `Quarantined` pool defers the live `Swap` to the pump
        // buffer. A `Swap` does NOT touch `tick_data` (the pump path passes
        // `tick_priors: &[]`), but it DOES set `update_block = block_number`.
        // So without deferral a live `Swap` at an in-progress block N+1 would
        // advance the pin's `update_block` to N+1 while a buffered same-block
        // `ModifyLiquidity` Burn stays retained → the 25647112 mismatch by
        // exactly the Burn's delta (the live direct-apply gap YLYJM2's
        // `drain_pump_completed` buffer gate does NOT cover). `Live` applies
        // directly (the steady-state contract).
        if let Some(PoolEntry::V4(_, state)) = self.pools.get(&pool_id) {
            if state.registration_lifecycle == RegistrationLifecycle::Quarantined {
                self.v4_buffer.buffer_pump(
                    key,
                    BufferedV4PoolEvent::Swap(BufferedV4SwapEvent {
                        sqrt_price_x96: update.sqrt_price_x96,
                        liquidity: update.liquidity,
                        tick: update.tick,
                        block_number,
                    }),
                );
                return None;
            }
        }
        self.apply_v4_swap_by_pool_id(
            pool_id,
            update.sqrt_price_x96,
            update.liquidity,
            update.tick,
            block_number,
            &update.tick_priors,
        )
    }

    /// Apply a V4 `ModifyLiquidity` event to a registered pool, or buffer it
    /// for an unregistered pool (ADR-003 live path).
    pub fn apply_v4_liquidity_update(
        &mut self,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: alloy::primitives::I256,
        block_number: u64,
    ) -> Option<u64> {
        let key = (pool_manager, pool_id);
        let pool_id_hex = degenbot_core::hex_utils::encode_hex(&pool_id);
        let Some(&pool_id) = self.v4_pool_ids.get(&key) else {
            trace_apply_route_v4(
                pool_manager,
                &pool_id_hex,
                tick_lower,
                tick_upper,
                liquidity_delta,
                block_number,
                "none",
                "buffer-pump",
            );
            self.v4_buffer.buffer_pump(
                key,
                BufferedV4PoolEvent::Liquidity(BufferedV4LiquidityUpdate {
                    tick_lower,
                    tick_upper,
                    liquidity_delta,
                    block_number,
                }),
            );
            return None;
        };
        // 6N7XVR: a `Quarantined` registered pool defers the live event to the
        // pump buffer (via the same unregistered-buffering path) so the pin's
        // `update_block` cannot outrun `last_complete_block`. `Live` applies
        // directly (the steady-state contract). The deferral preserves
        // cross-type arrival order within a block (a same-block `Swap` and
        // `ModifyLiquidity` both land in the one buffer).
        if let Some(PoolEntry::V4(_, state)) = self.pools.get(&pool_id) {
            if state.registration_lifecycle == RegistrationLifecycle::Quarantined {
                trace_apply_route_v4(
                    pool_manager,
                    &pool_id_hex,
                    tick_lower,
                    tick_upper,
                    liquidity_delta,
                    block_number,
                    "Quarantined",
                    "buffer-pump-quarantined",
                );
                self.v4_buffer.buffer_pump(
                    key,
                    BufferedV4PoolEvent::Liquidity(BufferedV4LiquidityUpdate {
                        tick_lower,
                        tick_upper,
                        liquidity_delta,
                        block_number,
                    }),
                );
                return None;
            }
        }
        trace_apply_route_v4(
            pool_manager,
            &pool_id_hex,
            tick_lower,
            tick_upper,
            liquidity_delta,
            block_number,
            "Live",
            "direct-live",
        );
        // ADR-014 D1: delegate to the pool_id-keyed dispatcher (the V3
        // address-keyed wrapper pattern). The inline body that previously
        // lived here was byte-identical to `impl ConcentratedLiquidityPoolMut
        // for V4PoolState::apply_liquidity_update`, which the twin reaches via
        // `state.apply_liquidity_update(...)` — the duplication (the bug-hiding
        // class D1 was written to kill) is removed.
        //
        // ADR-014 D4 seam: the int256→i128 narrowing lives at this drain→apply
        // call site (matches the contract's own
        // `params.liquidityDelta.toInt128()` at `PoolManager.sol:666`); the
        // state-struct apply body operates on int128 (matches `Tick.Info`'s
        // int128). An int256 that doesn't fit int128 is dropped here, not
        // buried in the apply body. The buffer branch above (unregistered pool
        // → `v4_buffer`) stays — a registry concern ADR-014 D1 says lives on
        // the holder, not the state struct.
        let delta_i128: i128 = i128::try_from(liquidity_delta).ok()?;
        self.apply_v4_liquidity_update_by_pool_id(
            pool_id,
            tick_lower,
            tick_upper,
            delta_i128,
            block_number,
        )
    }

    /// Apply a V4 Swap event to a registered pool by its inner handle
    /// `pool_id` (the per-handle Python API path, an alternative entry to the
    /// `(pool_manager, pool_id)`-keyed `apply_v4_swap`).
    ///
    /// Mirrors `apply_v3_swap_by_pool_id` for the V4 entry: journals the
    /// scalar priors (and any passed `tick_priors`, empty from the handle path
    /// — same "scalars only" contract the V3 handle method documents), mutates
    /// `slot0`, advances `update_block`, invalidates the tick-range cache.
    ///
    /// Returns `Some(pool_id)` if the pool is V4; `None` for V2/V3 or
    /// unregistered (silent no-op, matching the V3 sibling's contract).
    pub fn apply_v4_swap_by_pool_id(
        &mut self,
        pool_id: u64,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)],
    ) -> Option<u64> {
        let Some(PoolEntry::V4(_, state)) = self.pools.get_mut(&pool_id) else {
            return None;
        };
        state.apply_swap(sqrt_price_x96, liquidity, tick, block_number, tick_priors);
        Some(pool_id)
    }

    /// Apply a V4 `ModifyLiquidity` event to a registered pool by its inner
    /// handle `pool_id` (the per-handle Python API path, an alternative entry
    /// to the `(pool_manager, pool_id)`-keyed `apply_v4_liquidity_update`).
    ///
    /// Mirrors `apply_v3_liquidity_update_by_pool_id` for the V4 entry:
    /// journals the two tick priors, applies the delta to the tick range
    /// (`liquidity_net` `+=` at lower, `-=` at upper, both `gross +=`),
    /// advances `update_block`, invalidates the tick-range cache. No scalar
    /// change (`scalar_priors: None`) — same ADR-004 tick-only contract as V3.
    ///
    /// Returns `Some(pool_id)` if the pool is V4; `None` otherwise.
    pub fn apply_v4_liquidity_update_by_pool_id(
        &mut self,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) -> Option<u64> {
        let Some(PoolEntry::V4(_, state)) = self.pools.get_mut(&pool_id) else {
            return None;
        };
        state.apply_liquidity_update(tick_lower, tick_upper, liquidity_delta, block_number);
        Some(pool_id)
    }

    /// Family-dispatching Swap apply (RAJ3PP). The single entry point
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

    /// Family-dispatching liquidity update (RAJ3PP). The single entry point
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

    /// Buffer a V4 `ModifyLiquidity` event from the backfill phase.
    pub fn buffer_backfill_v4_liquidity_update(
        &mut self,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: alloy::primitives::I256,
        block_number: u64,
    ) {
        let key = (pool_manager, pool_id);
        if let Some(&id) = self.v4_pool_ids.get(&key) {
            if let Some(PoolEntry::V4(_, state)) = self.pools.get_mut(&id) {
                // 6N7XVR: a `Quarantined` pool defers ALL live/backfill events
                // to the buffer so the pin's `update_block` cannot outrun
                // `last_complete_block`. `Live` pools apply directly (the
                // steady-state contract). Backfill completes before
                // `build_paths`/quarantine in the normal flow, but a late
                // backfill chunk interleaving with a re-register must respect
                // the lifecycle for the invariant to hold.
                if state.registration_lifecycle == RegistrationLifecycle::Live {
                    if let Ok(delta_i128) = i128::try_from(liquidity_delta) {
                        ::degenbot_pools::tick_bitmap::apply_liquidity_to_tick_range(
                            &mut state.tick_data,
                            tick_lower,
                            tick_upper,
                            delta_i128,
                            block_number,
                        );
                        state.update_block = block_number;
                        state.invalidate_tick_range_cache();
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
    pub fn apply_backfill_buffer_v4(
        &mut self,
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
            let Some(PoolEntry::V4(_, state)) = self.pools.get_mut(&id) else {
                continue;
            };
            Self::apply_buffered_v4_event(state, update);
        }
    }

    /// Apply all buffered **pump** V4 `ModifyLiquidity` events for a pool.
    ///
    /// Same journal + `update_block` contract as
    /// [`apply_backfill_buffer_v4`] — see its docs.
    pub fn apply_pump_buffer_v4(
        &mut self,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) {
        let key = (pool_manager, pool_id);
        let Some(&id) = self.v4_pool_ids.get(&key) else {
            return;
        };
        // YLYJM2: drain ONLY fully-completed blocks. The cutoff is the pump's
        // `BlockClock` tombstone cutoff (3M5PO5) — a block is complete when
        // the first log of N+1 closes N; a drain mid-block would pin
        // `update_block=N` missing a later same-block log.
        let cutoff = self.pump_complete_cutoff.load(Ordering::Relaxed);
        if cutoff == 0 {
            return;
        }
        let Some(buffered) = self.v4_buffer.drain_pump_completed(&key, cutoff) else {
            return;
        };
        for update in buffered {
            let Some(PoolEntry::V4(_, state)) = self.pools.get_mut(&id) else {
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
    /// registered pool's state. 6N7XVR: the V4 drain loops
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

    /// Set a V3 pool's registration lifecycle to `Quarantined` (6N7XVR). The
    /// live pump then defers the pool's `Swap`/`Mint`/`Burn` events to the
    /// pump buffer until [`set_pool_live`] transitions it back. Call at the
    /// start of `register_v3_pool` (before the first RPC await). No-op for
    /// unregistered / non-V3 pools AND for non-`Tracked` pools (a `Sparse`
    /// pool has no pin / step-2 verify to protect, so quarantining it would
    /// only defer events with nothing to gain — it stays `Live`/direct-apply;
    /// DFQYM5 coverage-aware carve-out).
    pub fn set_v3_pool_quarantined(&mut self, address: Address) {
        if let Some(&id) = self.pool_addresses.get(&address) {
            if let Some(PoolEntry::V3(_, state)) = self.pools.get_mut(&id) {
                if state.coverage == PoolTickCoverage::Tracked {
                    state.registration_lifecycle = RegistrationLifecycle::Quarantined;
                }
            }
        }
    }

    /// Set a V4 pool's registration lifecycle to `Quarantined` (6N7XVR). V4
    /// twin of [`set_v3_pool_quarantined`]. Call at the start of
    /// `register_v4_pool` (before the first RPC await). No-op for unregistered
    /// V4 pools and for non-`Tracked` pools (Sparse stays `Live`; DFQYM5).
    pub fn set_v4_pool_quarantined(
        &mut self,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) {
        let key = (pool_manager, pool_id);
        if let Some(&id) = self.v4_pool_ids.get(&key) {
            if let Some(PoolEntry::V4(_, state)) = self.pools.get_mut(&id) {
                if state.coverage == PoolTickCoverage::Tracked {
                    state.registration_lifecycle = RegistrationLifecycle::Quarantined;
                }
            }
        }
    }

    /// Transition a V3 pool from `Quarantined` to `Live` (6N7XVR): flush any
    /// remaining buffered pump events for the pool (the in-progress-block tail
    /// retained by `drain_pump_completed`) via the UNGUARDED `drain_pump` in
    /// insertion order, then mark `Live`. Applies under one `core.write()`
    /// hold so no live event interleaves between the flush and the mark. The
    /// flush uses `drain_pump` (not `drain_pump_completed`) because the
    /// retained tail must not be orphaned (no second registration drain
    /// exists) — matches the Live steady-state contract (Live pools receive
    /// direct apply with no per-block gate; ordering preserved). No-op for
    /// unregistered / non-V3 pools or an already-`Live` pool.
    pub fn set_v3_pool_live(&mut self, address: Address) {
        let Some(&id) = self.pool_addresses.get(&address) else {
            return;
        };
        // Flush the retained pump tail first (backfill already fully drained
        // during `apply_backfill_buffer_v3`).
        if let Some(buffered) = self.v3_buffer.drain_pump(&address) {
            if verify_dbg_enabled() {
                use ::degenbot_pools::liquidity_event::LiquidityEvent;
                let blocks: Vec<u64> = buffered.iter().map(LiquidityEvent::block_number).collect();
                let mut sorted = blocks.clone();
                sorted.sort_unstable();
                let distinct: Vec<u64> = sorted
                    .into_iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                tracing::info!(
                    pool_addr = %format!("{address:x}"),
                    drained_tail = buffered.len(),
                    blocks = ?blocks,
                    distinct_blocks = ?distinct,
                    "[verify-dbg] V3 set_live"
                );
            }
            for event in buffered {
                if let Some(PoolEntry::V3(_, state)) = self.pools.get_mut(&id) {
                    Self::apply_buffered_v3_event(state, event);
                }
            }
        }
        if let Some(PoolEntry::V3(_, state)) = self.pools.get_mut(&id) {
            state.registration_lifecycle = RegistrationLifecycle::Live;
        }
    }

    /// Transition a V4 pool from `Quarantined` to `Live` (6N7XVR). V4 twin of
    /// [`set_v3_pool_live`] — flushes the retained pump tail via the
    /// unguarded `drain_pump`, then marks `Live`. No-op for unregistered V4
    /// pools or an already-`Live` pool.
    pub fn set_v4_pool_live(
        &mut self,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) {
        let key = (pool_manager, pool_id);
        let Some(&id) = self.v4_pool_ids.get(&key) else {
            return;
        };
        if let Some(buffered) = self.v4_buffer.drain_pump(&key) {
            if verify_dbg_enabled() {
                use ::degenbot_pools::liquidity_event::LiquidityEvent;
                let blocks: Vec<u64> = buffered.iter().map(LiquidityEvent::block_number).collect();
                let mut sorted = blocks.clone();
                sorted.sort_unstable();
                let distinct: Vec<u64> = sorted
                    .into_iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                tracing::info!(
                    pool_manager = %format!("{pool_manager:x}"),
                    pool_id = %degenbot_core::hex_utils::encode_hex(&pool_id),
                    drained_tail = buffered.len(),
                    blocks = ?blocks,
                    distinct_blocks = ?distinct,
                    "[verify-dbg] V4 set_live"
                );
            }
            for event in buffered {
                if let Some(PoolEntry::V4(_, state)) = self.pools.get_mut(&id) {
                    Self::apply_buffered_v4_event(state, event);
                }
            }
        }
        if let Some(PoolEntry::V4(_, state)) = self.pools.get_mut(&id) {
            state.registration_lifecycle = RegistrationLifecycle::Live;
        }
    }

    /// Batch-release every pool still `Quarantined` (DFQYM5 orphan sweep).
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
    pub fn release_all_v3_v4_quarantined(&mut self) {
        // Collect the still-Quarantined V3 addresses and V4 (pm, pool_id) keys
        // first (drain buffers are keyed by those, not `pool_id`), then release
        // each via the existing set_live flush+mark. Collect-then-apply avoids
        // holding a `&mut self.pools` borrow across the drain calls.
        let v3_addrs: Vec<Address> = self
            .pools
            .iter()
            .filter_map(|(&id, e)| match e {
                PoolEntry::V3(_, s)
                    if s.registration_lifecycle == RegistrationLifecycle::Quarantined =>
                {
                    if let PoolEntry::V3(i, _) = &self.pools[&id] {
                        Some(i.address)
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .collect();
        let v4_keys: Vec<(Address, degenbot_decoders::v4_swap_decoder::V4PoolId)> = self
            .pools
            .iter()
            .filter_map(|(&id, e)| match e {
                PoolEntry::V4(_, s)
                    if s.registration_lifecycle == RegistrationLifecycle::Quarantined =>
                {
                    if let PoolEntry::V4(i, _) = &self.pools[&id] {
                        Some((i.pool_manager, i.pool_id))
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
        if verify_dbg_enabled() {
            tracing::info!(
                v3 = v3_addrs.len(),
                v4 = v4_keys.len(),
                "[verify-dbg] release-all quarantined"
            );
        }
        for addr in v3_addrs {
            self.set_v3_pool_live(addr);
        }
        for (pm, pid) in v4_keys {
            self.set_v4_pool_live(pm, pid);
        }
    }

    /// solving (B3 move, FD7NFG). Decodes V3 swap/mint/burn + V4 swap/modify-
    /// liquidity logs and applies each via the same `apply_v3_swap` /
    /// `buffer_backfill_*_liquidity_update` / `apply_v4_swap` path the live
    /// loop uses; after the chunk, `expire_v3/v4_buffered(chunk_end)` advances
    /// the liquidity buffers. No `dispatch` / `solve_dirty` — the `Backfilled`
    /// phase invariant is "state advanced, no batches emitted".
    ///
    /// This is the BotState-level relocation of what was
    /// `ArbitrageEngine::process_backfill_logs` (`solvers/arb_engine/
    /// event_routing.rs`); the engine method is now a thin delegator +
    /// `last_processed_block` stamp. `BotState` owns the state (ADR-003);
    /// `BlockPump::backfill_from_snapshot` (core) reaches it via `self.bot`.
    pub fn process_backfill_logs(&mut self, logs: &[alloy::rpc::types::Log], chunk_end: u64) {
        use degenbot_decoders::v3_mint_burn_decoder::{decode_v3_burn_log, decode_v3_mint_log};
        use degenbot_decoders::v3_swap_decoder::decode_v3_swap_log;
        use degenbot_decoders::v4_modify_liquidity_decoder::decode_v4_modify_liquidity_log;
        use degenbot_decoders::v4_swap_decoder::decode_v4_swap_log;
        let mut v3_touched = false;
        let mut v4_touched = false;
        for log in logs {
            // Stamp this log with its own block number. A backfill log should
            // always carry `block_number`; fall back to `chunk_end` only for a
            // malformed log so apply never sees block 0.
            let log_block = log.block_number.unwrap_or(chunk_end);
            let Some(topic0) = log.topic0() else { continue };
            if *topic0 == degenbot_decoders::v3_swap_decoder::V3_SWAP_TOPIC {
                if let Some(event) = decode_v3_swap_log(log) {
                    self.apply_v3_swap(
                        event.pool_address,
                        event.sqrt_price_x96,
                        event.liquidity.to::<u128>(),
                        event.tick,
                        log_block,
                        &[],
                    );
                    v3_touched = true;
                }
            } else if *topic0 == degenbot_decoders::v3_mint_burn_decoder::V3_MINT_TOPIC {
                if let Some(event) = decode_v3_mint_log(log) {
                    self.buffer_backfill_v3_liquidity_update(
                        event.pool_address,
                        event.tick_lower,
                        event.tick_upper,
                        event.amount.cast_signed(),
                        log_block,
                    );
                    v3_touched = true;
                }
            } else if *topic0 == degenbot_decoders::v3_mint_burn_decoder::V3_BURN_TOPIC {
                if let Some(event) = decode_v3_burn_log(log) {
                    self.buffer_backfill_v3_liquidity_update(
                        event.pool_address,
                        event.tick_lower,
                        event.tick_upper,
                        -(event.amount.cast_signed()),
                        log_block,
                    );
                    v3_touched = true;
                }
            } else if *topic0 == degenbot_decoders::v4_swap_decoder::V4_SWAP_TOPIC {
                if let Some(event) = decode_v4_swap_log(log) {
                    self.apply_v4_swap(
                        &V4SwapUpdate {
                            pool_manager: log.address(),
                            pool_id: event.pool_id,
                            sqrt_price_x96: event.sqrt_price_x96,
                            liquidity: event.liquidity.to::<u128>(),
                            tick: event.tick,
                            tick_priors: vec![],
                        },
                        log_block,
                    );
                    v4_touched = true;
                }
            } else if *topic0
                == degenbot_decoders::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC
            {
                if let Some(event) = decode_v4_modify_liquidity_log(log) {
                    self.buffer_backfill_v4_liquidity_update(
                        log.address(),
                        event.pool_id,
                        event.tick_lower,
                        event.tick_upper,
                        event.liquidity_delta,
                        log_block,
                    );
                    v4_touched = true;
                }
            }
        }
        if v3_touched {
            self.expire_v3_buffered(chunk_end);
        }
        if v4_touched {
            self.expire_v4_buffered(chunk_end);
        }
    }

    /// Read a registered V4 pool's state by `pool_id`.
    #[must_use]
    pub fn get_v4_pool(&self, pool_id: u64) -> Option<&V4PoolState> {
        self.pools
            .get(&pool_id)
            .and_then(PoolEntry::v4)
            .map(|(_, state)| state)
    }

    /// Look up a V4 pool's immutable registration identity (`pool_manager`,
    /// `pool_id`, `pool_key`). Returns `None` if the pool is not registered or
    /// isn't a V4 pool.
    #[must_use]
    pub fn get_v4_identity(&self, pool_id: u64) -> Option<&V4PoolIdentity> {
        self.pools
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

    /// Read the pinned snapshot seed for a V4 pool (CBCH6H — V4 twin of
    /// `v3_snapshot_seed`). Keyed by `(pool_manager, pool_id)`.
    #[must_use]
    pub fn v4_snapshot_seed(
        &self,
        pool_manager: Address,
        pool_id: &degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) -> Option<&HashMap<i32, TickInfo>> {
        let pid = self.v4_pool_id_by_key(pool_manager, pool_id)?;
        let Some(PoolEntry::V4(_, state)) = self.pools.get(&pid) else {
            return None;
        };
        state.snapshot_seed.as_ref()
    }

    /// Take (move out + clear) the pinned snapshot seed for a V4 pool (CBCH6H).
    /// V4 twin of `take_v3_snapshot_seed` — step-1 verify consumes the seed once.
    pub fn take_v4_snapshot_seed(
        &mut self,
        pool_manager: Address,
        pool_id: &degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) -> Option<HashMap<i32, TickInfo>> {
        let pid = self.v4_pool_id_by_key(pool_manager, pool_id)?;
        let Some(PoolEntry::V4(_, state)) = self.pools.get_mut(&pid) else {
            return None;
        };
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
    /// — the 2026-06-29 crash). `Tracked` pools only.
    pub fn pin_v4_post_drain_snapshot(
        &mut self,
        pool_manager: Address,
        pool_id: &degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) {
        let key = (pool_manager, *pool_id);
        // Capture the pin scalar in an inner scope so the `&mut state` borrow
        // of `self.pools` ends before the diagnostic reads `self.v4_buffer`
        // (a second `&self` borrow) — Rust forbids both alive at once.
        let diag = {
            let Some(pid) = self.v4_pool_id_by_key(pool_manager, pool_id) else {
                return;
            };
            let Some(PoolEntry::V4(_, state)) = self.pools.get_mut(&pid) else {
                return;
            };
            if state.coverage == PoolTickCoverage::Tracked {
                state.post_drain_snapshot = Some((state.tick_data.clone(), state.update_block));
                Some(state.update_block)
            } else {
                None
            }
        };
        if let Some(update_block) = diag {
            if verify_dbg_enabled() {
                tracing::info!(
                    pool_manager = %format!("{pool_manager:x}"),
                    pool_id = %degenbot_core::hex_utils::encode_hex(pool_id),
                    update_block,
                    pump_count = self.v4_buffer.pump_count_at_or_below(&key, update_block),
                    last_complete_block = self.pump_complete_cutoff(),
                    "[verify-dbg] V4 pin"
                );
            }
        }
    }

    /// Take (move out + clear) the V4 post-drain `(tick_data, block)` pair.
    /// Step-2 verify consumes it once (at the pinned block). The returned
    /// block is the `update_block` captured atomically with the drain; the
    /// verify compares `tick_data` against on-chain@THIS block, NOT a
    /// caller-supplied `verify_backfill_block` constant. `None` for sparse /
    /// un-drained / already-taken pools (no-op Ok at the seam).
    pub fn take_v4_post_drain_snapshot(
        &mut self,
        pool_manager: Address,
        pool_id: &degenbot_decoders::v4_swap_decoder::V4PoolId,
    ) -> Option<(HashMap<i32, TickInfo>, u64)> {
        let pid = self.v4_pool_id_by_key(pool_manager, pool_id)?;
        let Some(PoolEntry::V4(_, state)) = self.pools.get_mut(&pid) else {
            return None;
        };
        state.post_drain_snapshot.take()
    }

    /// Number of registered V4 pools.
    #[must_use]
    pub fn v4_pool_count(&self) -> usize {
        self.v4_pool_ids.len()
    }

    /// Return the set of V4 `PoolManager` addresses with registered pools.
    #[must_use]
    pub fn v4_registered_pool_managers(&self) -> Vec<Address> {
        self.v4_pool_ids
            .keys()
            .map(|(pm, _)| *pm)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    }

    /// Snapshot all V4 pool state for verification.
    #[must_use]
    pub fn v4_pools_snapshot(&self) -> HashMap<u64, (V4PoolIdentity, V4PoolState)> {
        self.pools
            .iter()
            .filter_map(|(id, e)| match e {
                PoolEntry::V4(identity, state) => Some((*id, (identity.clone(), state.clone()))),
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
    pub fn sync_v4_pool_state(
        &mut self,
        pool_manager: Address,
        pool_id: degenbot_decoders::v4_swap_decoder::V4PoolId,
        update: V4StateSync,
    ) {
        let Some(&id) = self.v4_pool_ids.get(&(pool_manager, pool_id)) else {
            return;
        };
        let Some(PoolEntry::V4(_, state)) = self.pools.get_mut(&id) else {
            return;
        };
        state.sqrt_price_x96 = update.sqrt_price_x96;
        state.liquidity = update.liquidity;
        state.tick = update.tick;
        state.tick_data = update.tick_data;
        state.update_block = update.update_block;
        state.invalidate_tick_range_cache();
    }

    // --- V4 journal methods ---

    /// Register a token.
    ///
    /// # Panics
    ///
    /// Panics if the token address is already registered.
    pub fn register_token(
        &mut self,
        address: Address,
        name: String,
        symbol: String,
        decimals: u8,
        chain_id: u64,
    ) {
        assert!(
            !self.tokens.contains_key(&address),
            "token already registered: {address}"
        );

        self.tokens.insert(
            address,
            TokenEntry {
                address,
                name,
                symbol,
                decimals,
                chain_id,
            },
        );
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
// — the 4 reachers (`block_pump`, `degenbot-python/bot/mod.rs`,
// `degenbot-python/bot/pump.rs` ×2) are byte-identical.
// ---------------------------------------------------------------------------
pub use bot::Bot;

/// Block metadata included in each `ResultBatch`.
///
/// Passed from the pump's WS block header into the drain tick, then forwarded
/// to Python via the result batch channel. Lives in `bot_core` (general block
/// data) so the `BlockPump` + `DrainSink` seams stay in `bot_core` without a
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::uint;

    const FEE_03: (u64, u64) = (997, 1000);

    fn make_pool_addr() -> Address {
        Address::from([0xaa; 20])
    }
    fn make_token0() -> Address {
        Address::from([0xbb; 20])
    }
    fn make_token1() -> Address {
        Address::from([0xcc; 20])
    }
    fn make_factory() -> Address {
        Address::from([0xdd; 20])
    }

    fn make_params(r0: U112, r1: U112) -> RegisterV2PoolParams {
        RegisterV2PoolParams {
            address: make_pool_addr(),
            token0: make_token0(),
            token1: make_token1(),
            reserve0: r0,
            reserve1: r1,
            fee_token0: FEE_03,
            fee_token1: FEE_03,
            factory: make_factory(),
            update_block: 0,
            variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
            stable_swap: false,
            fee_denominator: None,
            ..Default::default()
        }
    }

    #[test]
    fn pool_update_block_tracks_forward_sync_and_returns_zero_for_unknown() {
        // AV42C7 accessor: `pool_update_block` is the per-pool freshness
        // signal the block-boundary FSM (ergo 3M5PO5/ZU7RAF) will use to
        // re-solve at block completion. Registers a V2 pool at `update_block=0`,
        // applies a forward Sync, and asserts the accessor advances + returns 0
        // for an unregistered id (the FSM treats 0 as stale: a missing pool
        // defers its path until registered).
        let mut core = BotState::new();
        let pool_id = core
            .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
            .expect("test setup: V2 registration");
        assert_eq!(
            core.pool_update_block(pool_id),
            0,
            "freshly-registered V2 pool is at update_block 0"
        );
        assert_eq!(
            core.pool_update_block(999_999),
            0,
            "an unregistered pool_id reports update_block 0 (stale sentinel)"
        );
        core.apply_v2_sync(make_pool_addr(), U112::from(900), U112::from(2222), 7)
            .expect("forward Sync at block 7 applies");
        assert_eq!(
            core.pool_update_block(pool_id),
            7,
            "forward Sync advances the pool's update_block to the event block"
        );
    }

    #[test]
    fn register_v2_pool_and_calculate_tokens_out() {
        let mut core = BotState::new();
        let pool_id = core
            .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
            .expect("test setup: V2 registration");

        // Python reference: constant_product_calc_exact_in(100, 1000, 2000, 3/1000) = 181
        let amount_out = core
            .calculate_tokens_out_miss_aware(pool_id, true, U256::from(100))
            .expect("small non-overflowing V2 amount; calc must not miss or overflow");
        assert_eq!(amount_out, U256::from(181));
    }

    #[test]
    fn v2_identity_round_trip() {
        // The identity (address/tokens/fees/factory/variant/stable_swap/
        // fee_denominator) round-trips through register_v2_pool ->
        // get_v2_identity. Identity is pure immutable registration data
        // (mirrors TokenEntry), distinct from the mutable V2PoolState.
        let mut core = BotState::new();
        let pool_id = core
            .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
            .expect("test setup: V2 registration");
        let id = core
            .get_v2_identity(pool_id)
            .expect("registered V2 pool has an identity");
        assert_eq!(id.address, make_pool_addr());
        assert_eq!(id.token0, make_token0());
        assert_eq!(id.token1, make_token1());
        assert_eq!(id.factory, make_factory());
        assert_eq!(id.fee_token0, FEE_03);
        assert_eq!(id.fee_token1, FEE_03);
        assert_eq!(
            id.variant,
            degenbot_uniswap::dex_identity::DexVariant::UniswapV2
        );
        assert!(!id.stable_swap);
        assert_eq!(id.fee_denominator, None);
    }

    #[test]
    fn pool_family_dispatches_v2_and_unknown() {
        // `pool_family(pool_id)` returns a kebab-case family tag by matching
        // on the `PoolEntry` variant. This is the uniform family-guard
        // primitive every `_from_py_pool` seam asserts against (replacing the
        // V2-only `variant` getter). Tracer bullet: V2 + unregistered.
        let mut core = BotState::new();
        let pool_id = core
            .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
            .expect("test setup: V2 registration");
        assert_eq!(core.pool_family(pool_id), "v2");
        assert_eq!(core.pool_family(999_999), "");
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn pool_family_dispatches_every_registered_family() {
        // Each non-V2 `PoolEntry` variant resolves to its own family tag.
        // Registers one pool of each family with minimal params and asserts
        // the tag — this is the precondition for every non-V2 `_from_py_pool`
        // seam's variant-family guard.
        use crate::bot_core::{
            RegisterAerodromeV2PoolParams, RegisterBalancerStablePoolParams,
            RegisterBalancerWeightedPoolParams, RegisterCurvePoolParams, RegisterV4PoolParams,
            TickInfo, V4PoolKey,
        };
        use alloy::primitives::U128;

        let mut core = BotState::new();

        // V3
        let v3_id = register_v3(&mut core, 0);
        assert_eq!(core.pool_family(v3_id), "v3");

        // V4
        let pool_manager = Address::from([0x44u8; 20]);
        let pool_id_bytes: degenbot_decoders::v4_swap_decoder::V4PoolId = [0xeeu8; 32];
        let v4_id = core
            .register_v4_pool(&RegisterV4PoolParams {
                pool_manager,
                pool_id: pool_id_bytes,
                pool_key: V4PoolKey {
                    currency0: Address::ZERO,
                    currency1: Address::from([0x01u8; 20]),
                    fee: 500,
                    tick_spacing: 10,
                    hooks: Address::ZERO,
                },
                hook_flags: 0,
                protocol_fee: 0,
                sqrt_price_x96: U256::from(1u64) << 96,
                liquidity: 0,
                tick: 0,
                tick_data: HashMap::new(),
                update_block: 0,
                coverage: PoolTickCoverage::Sparse,
                fetcher: None,
            })
            .expect("V4 registration");
        assert_eq!(core.pool_family(v4_id), "v4");

        // Curve (2-token plain pool)
        let curve_id = core.register_curve_pool(&RegisterCurvePoolParams {
            address: Address::from([0xc0u8; 20]),
            tokens: vec![Address::ZERO, Address::from([0x01u8; 20])],
            a_coefficient: 100,
            a_precision: 100,
            fee: 4_000_000,
            admin_fee: 5_000_000_000,
            rate_multipliers: vec![U256::from(1u64), U256::from(1u64)],
            balances: vec![U256::from(1_000_000u64), U256::from(1_000_000u64)],
            update_block: 0,
            swap_style: 0,
            lending_rate_style: 0,
            d_variant: 1,
            y_variant: 1,
            yd_variant: 0,
            base_pool: None,
            initial_a_coefficient: None,
            future_a_coefficient: None,
            initial_a_coefficient_time: None,
            future_a_coefficient_time: None,
            create_timestamp: None,
            fee_gamma: None,
            mid_fee: None,
            offpeg_fee_multiplier: None,
            out_fee: None,
            gamma: None,
            lp_token: None,
            use_lending: vec![false, false],
            precision_multipliers: vec![U256::from(1u64), U256::from(1u64)],
            tokens_underlying: None,
            metapool_rate_style: 1,
            metapool_underlying_style: 1,
            data_provider: None,
        });
        assert_eq!(core.pool_family(curve_id), "curve");

        // Balancer weighted (2-token)
        let bal_weighted_id =
            core.register_balancer_weighted_pool(&RegisterBalancerWeightedPoolParams {
                address: Address::from([0xb1u8; 20]),
                vault: Address::from([0xa0u8; 20]),
                pool_id: [0x11u8; 32],
                tokens: vec![Address::ZERO, Address::from([0x01u8; 20])],
                weights: vec![
                    U256::from(5_000_000_000_000_000_000u128),
                    U256::from(5_000_000_000_000_000_000u128),
                ],
                scaling_factors: vec![U256::from(1u64), U256::from(1u64)],
                swap_fee: 1_000_000_000_000_000,
                pow_version: 2,
                balances: vec![U256::from(1_000_000u64), U256::from(1_000_000u64)],
                update_block: 0,
            });
        assert_eq!(core.pool_family(bal_weighted_id), "balancer-weighted");

        // Balancer stable (2-token, MetaStable — bpt_idx=None)
        let bal_stable_id = core.register_balancer_stable_pool(&RegisterBalancerStablePoolParams {
            address: Address::from([0xb2u8; 20]),
            vault: Address::from([0xb0u8; 20]),
            pool_id: [0x22u8; 32],
            tokens: vec![Address::ZERO, Address::from([0x01u8; 20])],
            amp: 100,
            scaling_factors: vec![U256::from(1u64), U256::from(1u64)],
            swap_fee: 1_000_000_000_000_000,
            bpt_idx: None,
            invariant_version: 2,
            balances: vec![U256::from(1_000_000u64), U256::from(1_000_000u64)],
            update_block: 0,
            rate_provider: None,
        });
        assert_eq!(core.pool_family(bal_stable_id), "balancer-stable");

        // Suppress unused-import warning for TickInfo/U128 when the V4 tick_data
        // map is empty — kept for parity with sibling V4 tests.
        let _ = TickInfo {
            liquidity_gross: U128::ZERO,
            liquidity_net: alloy::primitives::I256::ZERO,
            block: 0,
        };

        // Aerodrome V2 (volatile mode; stable=false)
        let aero_id = core.register_aerodrome_pool(&RegisterAerodromeV2PoolParams {
            address: Address::from([0xaeu8; 20]),
            token0: Address::ZERO,
            token1: Address::from([0x01u8; 20]),
            factory: Address::from([0xafu8; 20]),
            variant: degenbot_uniswap::dex_identity::DexVariant::AerodromeV2Volatile,
            stable: false,
            fee: (3, 1000),
            reserve0: U112::from(1_000_000u64),
            reserve1: U112::from(2_000_000u64),
            update_block: 0,
        });
        assert_eq!(core.pool_family(aero_id), "aerodrome-v2");
    }

    #[test]
    fn aerodrome_reserve_mutation_and_reorg_rollback() {
        // Aerodrome V2 reserves + reorg journal live in Rust (ADR-005
        // Aerodrome state port): `apply_sync_by_pool_id` journals
        // the prior reserves then lands the new; `aerodrome_restore_before_block`
        // pops back to the landed-at state at the target block.
        use crate::bot_core::RegisterAerodromeV2PoolParams;

        let mut core = BotState::new();
        let pool_id = core.register_aerodrome_pool(&RegisterAerodromeV2PoolParams {
            address: Address::from([0xaeu8; 20]),
            token0: Address::ZERO,
            token1: Address::from([0x01u8; 20]),
            factory: Address::from([0xafu8; 20]),
            variant: degenbot_uniswap::dex_identity::DexVariant::AerodromeV2Volatile,
            stable: false,
            fee: (3, 1000),
            reserve0: U112::from(1_000u64),
            reserve1: U112::from(2_000u64),
            update_block: 10,
        });

        // Identity survives.
        let identity = core
            .get_aerodrome_identity(pool_id)
            .expect("aerodrome identity");
        assert_eq!(identity.fee, (3, 1000));
        assert!(!identity.stable);

        // Initial registration state (genesis anchor at block 10).
        let state = core.get_aerodrome_pool(pool_id).expect("aerodrome state");
        assert_eq!(state.reserve0, U112::from(1_000u64));
        assert_eq!(state.reserve1, U112::from(2_000u64));
        assert_eq!(state.update_block, 10);
        assert_eq!(state.journal.len(), 1);

        // Apply a Sync at block 20 (journals prior reserves, lands new).
        let applied =
            core.apply_sync_by_pool_id(pool_id, U112::from(1_500u64), U112::from(2_500u64), 20);
        assert_eq!(applied, Some(pool_id));
        let state = core.get_aerodrome_pool(pool_id).expect("aerodrome state");
        assert_eq!(state.reserve0, U112::from(1_500u64));
        assert_eq!(state.reserve1, U112::from(2_500u64));
        assert_eq!(state.update_block, 20);
        assert_eq!(state.journal.len(), 2);

        // Reorg to before block 20 → restores registration state (genesis at 10).
        core.restore_pool_before_block(pool_id, 20)
            .expect("restore returns Some")
            .expect("restore succeeds");
        let state = core.get_aerodrome_pool(pool_id).expect("aerodrome state");
        assert_eq!(state.reserve0, U112::from(1_000u64));
        assert_eq!(state.reserve1, U112::from(2_000u64));
        assert_eq!(state.update_block, 10);

        // With ADR-017 slice 5 the Aerodrome + V2 `apply_sync` paths are
        // one dispatcher (`apply_sync_by_pool_id` across both reserve-pair
        // families), so a V2 pool_id now lands too — the cross-family isolation
        // that the old per-family method provided is gone by design.
        let v2_id = core
            .register_v2_pool(&make_params(U112::from(100), U112::from(200)))
            .expect("test setup: V2 registration");
        assert_eq!(
            core.apply_sync_by_pool_id(v2_id, U112::ZERO, U112::ZERO, 99),
            Some(v2_id)
        );
        // The no-mutate-on-wrong-family guard for restore moved to the PyO3
        // wrapper layer (ADR-016); BotState's unified restore dispatches
        // across all families, so a V2 pool_id is restored as V2.
    }

    #[test]
    fn calculate_tokens_out_reverse_direction() {
        let mut core = BotState::new();
        let pool_id = core
            .register_v2_pool(&make_params(U112::from(2000), U112::from(1000)))
            .expect("test setup: V2 registration");

        // Python reference: constant_product_calc_exact_in(100, 1000, 2000, 3/1000) = 181
        let amount_out = core
            .calculate_tokens_out_miss_aware(pool_id, false, U256::from(100))
            .expect("small non-overflowing V2 amount; calc must not miss or overflow");
        assert_eq!(amount_out, U256::from(181));
    }

    #[test]
    fn update_v2_pool_changes_calculation_result() {
        let mut core = BotState::new();
        let pool_id = core
            .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
            .expect("test setup: V2 registration");

        // Before update: swap 100 token0 → 181 token1
        let before = core
            .calculate_tokens_out_miss_aware(pool_id, true, U256::from(100))
            .expect("small non-overflowing V2 amount; calc must not miss or overflow");
        assert_eq!(before, U256::from(181));

        // Update reserves: now reserve0=2000, reserve1=1000
        core.update_v2_pool(make_pool_addr(), U112::from(2000), U112::from(1000), 42);

        // After update: Python: constant_product_calc_exact_in(100, 2000, 1000, 3/1000) = 47
        let after = core
            .calculate_tokens_out_miss_aware(pool_id, true, U256::from(100))
            .expect("small non-overflowing V2 amount; calc must not miss or overflow");
        assert_eq!(after, U256::from(47));
    }

    #[test]
    fn calculate_tokens_in_for_v2_pool() {
        let mut core = BotState::new();
        let pool_id = core
            .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
            .expect("test setup: V2 registration");

        // Python: constant_product_calc_exact_out(50, 1000, 2000, 3/1000) = 26
        let amount_in = core.calculate_tokens_in(pool_id, true, U256::from(50));
        assert_eq!(amount_in, U256::from(26));

        // Reverse: Python: constant_product_calc_exact_out(10, 2000, 1000, 3/1000) = 21
        let amount_in_rev = core.calculate_tokens_in(pool_id, false, U256::from(10));
        assert_eq!(amount_in_rev, U256::from(21));
    }

    #[test]
    fn calculate_tokens_out_realistic_amounts() {
        let mut core = BotState::new();

        // Realistic: 1.5M USDC / 800 WETH, 0.3% fee
        let reserve0 = U112::from(1_500_000_000_000u64); // 1.5M USDC (6dp)
        let reserve1 = U112::from(800u128) * U112::from(10u64).pow(U112::from(18)); // 800 WETH

        let params = RegisterV2PoolParams {
            address: make_pool_addr(),
            token0: make_token0(),
            token1: make_token1(),
            reserve0,
            reserve1,
            fee_token0: FEE_03,
            fee_token1: FEE_03,
            factory: make_factory(),
            update_block: 0,
            variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
            stable_swap: false,
            fee_denominator: None,
            ..Default::default()
        };
        let pool_id = core
            .register_v2_pool(&params)
            .expect("test setup: V2 registration");

        // Swap 1000 USDC for WETH
        // Python reference: 531380142665175213
        let amount_in = U256::from(1_000_000_000u64); // 1000 USDC (6dp)
        let amount_out = core
            .calculate_tokens_out_miss_aware(pool_id, true, amount_in)
            .expect("small non-overflowing V2 amount; calc must not miss or overflow");
        assert_eq!(amount_out, U256::from(531_380_142_665_175_213_u64));
    }

    // --- V3 restore no-op (reorg on an unrelated pool) ---

    fn register_v3(core: &mut BotState, update_block: u64) -> u64 {
        core.register_v3_pool(&RegisterV3PoolParams {
            address: make_pool_addr(),
            token0: make_token0(),
            token1: make_token1(),
            fee: 3_000,
            tick_spacing: 60,
            factory: make_factory(),
            sqrt_price_x96: U256::from(1u64) << 96,
            liquidity: 0,
            tick: 0,
            tick_data: HashMap::new(),
            update_block,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
            ..Default::default()
        })
        .expect("test setup: V3 registration")
    }

    /// Regression (WZWKKU): `v3_restore_before_block(B)` with `B` strictly past
    /// the journal's newest delta must leave the pool's current state
    /// UNTOUCHED. This is the per-pool path `dispatch_reorg_log` hits for a
    /// `removed: true` log on an unrelated pool — totally normal reorg traffic.
    ///
    /// Pre-fix the journal returned the newest delta's own `scalar_priors` and
    /// `tick_priors` (the PRE-newest state) and `v3_restore_before_block`
    /// reverse-applied them, silently rolling back the newest delta's swap and
    /// deleting its freshly-initialized ticks. The engine then re-solved off
    /// the corrupted scalars + `tick_data`.
    #[test]
    fn v3_restore_before_block_past_newest_leaves_state_untouched() {
        let mut core = BotState::new();
        let pool_id = register_v3(&mut core, 5);

        // Forward Swap at block 10 moves scalars + initializes tick 100.
        let new_sqrt = U256::from(2u64) << 96;
        core.apply_v3_swap_by_pool_id(
            pool_id,
            new_sqrt,
            1_000,
            100,
            10,
            &[(
                100,
                TickInfo {
                    liquidity_gross: alloy::primitives::U128::from(500),
                    liquidity_net: alloy::primitives::I256::try_from(500i64).unwrap(),
                    block: 0,
                },
            )],
        );

        // Snapshot the landed-at (post-block-10) state.
        let landed_sqrt;
        let landed_liq;
        let landed_tick;
        let landed_update_block;
        let tick_present;
        {
            let Some(PoolEntry::V3(_, state)) = core.pools.get(&pool_id) else {
                panic!("pool missing");
            };
            landed_sqrt = state.sqrt_price_x96;
            landed_liq = state.liquidity;
            landed_tick = state.tick;
            landed_update_block = state.update_block;
            tick_present = state.tick_data.contains_key(&100);
        }
        assert_eq!(landed_sqrt, new_sqrt);
        assert_eq!(landed_liq, 1_000);
        assert_eq!(landed_tick, 100);
        assert_eq!(landed_update_block, 10);
        assert!(tick_present, "block-10 swap initialized tick 100");

        // Reorg: a removed log arrives for an unrelated pool whose newest
        // journal delta (block 10) is BELOW the reorg target (block 12).
        // `has_state_prior_to` returns true (V3 journal non-empty), so the
        // coordinator proceeds to `restore_pool_before_block` →
        // `v3_restore_before_block`.
        assert!(core.has_state_prior_to(pool_id, 12));
        let result = core.restore_pool_before_block(pool_id, 12);
        assert!(
            result.is_some(),
            "restore returns Some even on the no-op path"
        );

        // The landed-at state must survive unchanged.
        let Some(PoolEntry::V3(_, state)) = core.pools.get(&pool_id) else {
            panic!("pool missing post-restore");
        };
        assert_eq!(
            state.sqrt_price_x96, landed_sqrt,
            "scalars must not roll back"
        );
        assert_eq!(state.liquidity, landed_liq);
        assert_eq!(state.tick, landed_tick);
        assert_eq!(state.update_block, landed_update_block);
        assert!(
            state.tick_data.contains_key(&100),
            "tick 100 must survive — pre-fix it was deleted via the newest delta's tick_priors"
        );
        assert_eq!(
            state.journal.len(),
            1,
            "no deltas popped on the no-op path (only the block-10 swap delta; V3 registration pushes no genesis)"
        );
    }

    // --- HO3GWT: buffer appliers push journal deltas + advance update_block ---

    /// Register a V3 pool with tick 60 pre-initialized (gross/net 100) and
    /// tick 120 absent, so a buffered Mint at [60,120] bumps 60 → 600 and
    /// newly initializes 120. Helper does NOT create the `BotState` — the
    /// caller must buffer events on the SAME core before calling this.
    fn register_v3_on_core(core: &mut BotState, pool_addr: Address, update_block: u64) -> u64 {
        use crate::bot_core::{RegisterV3PoolParams, TickInfo};
        use crate::solvers::arb_engine::PoolTickCoverage;
        use alloy::primitives::{I256, U128};
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: U128::from(100),
                liquidity_net: I256::try_from(100i128).unwrap(),
                block: 0,
            },
        );
        core.register_v3_pool(&RegisterV3PoolParams {
            address: pool_addr,
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
            update_block,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
            ..Default::default()
        })
        .expect("test setup: V3 registration")
    }

    /// Regression (HO3GWT, V3 backfill buffer): a Mint buffered during the
    /// backfill phase, then applied at registration via
    /// `apply_backfill_buffer_v3`, must (1) push a tick-only journal delta,
    /// (2) advance `state.update_block` to the event's block, and (3) be
    /// reversible by `restore_before_block` (roll the tick state back to the
    /// pre-buffer registration snapshot).
    ///
    /// Pre-fix `apply_backfill_buffer_v3` called `apply_liquidity_to_tick_range`
    /// and `invalidate_tick_range_cache()` and stopped — no journal, no
    /// `update_block` bump. A reorg landing inside the buffered range
    /// couldn't reverse the buffered events, and `v3_snapshot`/diagnostics
    /// reported a stale last-update block.
    #[test]
    fn apply_backfill_buffer_v3_journals_and_advances_update_block() {
        let pool_addr = Address::from([0x88u8; 20]);
        let block_b = 5u64;

        // 1. Pre-registration: buffer a backfill Mint at [60, 120], block B=5.
        let mut core = BotState::new();
        core.buffer_backfill_v3_liquidity_update(pool_addr, 60, 120, 500_i128, block_b);

        // 2. Register on the SAME core (tick 60 gross=100, tick 120 absent).
        let pool_id = register_v3_on_core(&mut core, pool_addr, 0);

        // 3. Apply the backfill buffer (the registration-staged application).
        core.apply_backfill_buffer_v3(&pool_addr);

        {
            let s = core.get_v3_pool(pool_id).expect("registered");
            assert_eq!(
                s.update_block, block_b,
                "update_block must advance to the buffered event's block (pre-fix: stays at registration block 0)"
            );
            assert_eq!(
                s.journal.len(),
                1,
                "buffered Mint must push one tick-only journal delta (pre-fix: 0)"
            );
            let t60 = s.tick_data.get(&60).expect("tick 60 present");
            assert_eq!(t60.liquidity_gross, alloy::primitives::U128::from(600));
            assert_eq!(
                t60.liquidity_net,
                alloy::primitives::I256::try_from(600i128).unwrap()
            );
            let t120 = s.tick_data.get(&120).expect("tick 120 newly initialized");
            assert_eq!(t120.liquidity_gross, alloy::primitives::U128::from(500));
            assert_eq!(
                t120.liquidity_net,
                alloy::primitives::I256::try_from(-500i128).unwrap(),
                "upper tick net -= delta (V3/`apply_liquidity_to_tick_range` convention)"
            );
        }

        // 4. Restore before block B → rolls back the buffered Mint to the
        //    registration snapshot.
        core.restore_pool_before_block(pool_id, block_b);
        let s = core.get_v3_pool(pool_id).expect("registered");
        let t60 = s.tick_data.get(&60).expect("tick 60 still present");
        assert_eq!(
            t60.liquidity_gross,
            alloy::primitives::U128::from(100),
            "tick 60 reverts to registration snapshot (gross 100) on rollback"
        );
        assert_eq!(
            t60.liquidity_net,
            alloy::primitives::I256::try_from(100i128).unwrap()
        );
        assert!(
            !s.tick_data.contains_key(&120),
            "newly-initialized tick 120 removed on rollback"
        );
    }

    /// Regression (HO3GWT, V3 pump buffer): same invariants as the backfill
    /// path, but the event is buffered via the WS-pump path (`apply_v3_
    /// liquidity_update` while unregistered routes to the pump buffer) and
    /// applied via `apply_pump_buffer_v3`.
    #[test]
    fn apply_pump_buffer_v3_journals_and_advances_update_block() {
        let pool_addr = Address::from([0x99u8; 20]);
        let block_b = 7u64;

        // 1. Pre-registration: pump the Mint (unregistered → pump buffer).
        let mut core = BotState::new();
        core.apply_v3_liquidity_update(pool_addr, 60, 120, 500_i128, block_b);

        // 2. Register on the SAME core + 3. apply pump buffer.
        let pool_id = register_v3_on_core(&mut core, pool_addr, 0);
        // YLYJM2: the gated drain only yields fully-completed blocks. The
        // live pump marks `block_b` complete at its ADR-008 D1 tombstone (the
        // first log of block_b+1); mirror that here so the drain takes the
        // buffered Mint instead of leaving it pinned behind the gate.
        core.advance_pump_complete_cutoff(block_b);
        core.apply_pump_buffer_v3(&pool_addr);

        {
            let s = core.get_v3_pool(pool_id).expect("registered");
            assert_eq!(
                s.update_block, block_b,
                "update_block advances to pump-buffer event block"
            );
            assert_eq!(
                s.journal.len(),
                1,
                "pump-buffer Mint pushes one journal delta"
            );
            assert_eq!(
                s.tick_data.get(&60).expect("t60").liquidity_gross,
                alloy::primitives::U128::from(600)
            );
            assert!(s.tick_data.contains_key(&120));
        }

        core.restore_pool_before_block(pool_id, block_b);
        let s = core.get_v3_pool(pool_id).expect("registered");
        assert_eq!(
            s.tick_data.get(&60).expect("t60").liquidity_gross,
            alloy::primitives::U128::from(100),
            "pump-buffer Mint rolls back to registration snapshot"
        );
        assert!(
            !s.tick_data.contains_key(&120),
            "newly-initialized tick 120 removed on pump-buffer rollback"
        );
    }

    /// Regression (HO3GWT, V4 backfill buffer): mirror of the V3 backfill
    /// test for `apply_backfill_buffer_v4` — a `ModifyLiquidity` buffered
    /// during backfill must journal + advance `update_block` + be reversible
    /// via `v4_restore_before_block`.
    #[test]
    fn apply_backfill_buffer_v4_journals_and_advances_update_block() {
        use crate::bot_core::{RegisterV4PoolParams, TickInfo, V4PoolKey};
        use crate::solvers::arb_engine::PoolTickCoverage;
        use alloy::primitives::{I256, U128};

        let pool_manager = Address::from([0x44u8; 20]);
        let pool_id_bytes: degenbot_decoders::v4_swap_decoder::V4PoolId = [0xeeu8; 32];
        let block_b = 9u64;

        // 1. Pre-registration: buffer a backfill ModifyLiquidity at [60,120].
        let mut core = BotState::new();
        core.buffer_backfill_v4_liquidity_update(
            pool_manager,
            pool_id_bytes,
            60,
            120,
            I256::try_from(500i128).unwrap(),
            block_b,
        );

        // 2. Register (tick 60 gross=100, tick 120 absent, update_block=0).
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: U128::from(100),
                liquidity_net: I256::try_from(100i128).unwrap(),
                block: 0,
            },
        );
        let pool_id = core
            .register_v4_pool(&RegisterV4PoolParams {
                pool_manager,
                pool_id: pool_id_bytes,
                pool_key: V4PoolKey {
                    currency0: Address::ZERO,
                    currency1: Address::from([1u8; 20]),
                    fee: 10_000,
                    tick_spacing: 60,
                    hooks: Address::ZERO,
                },
                hook_flags: 0,
                protocol_fee: 0,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data,
                update_block: 0,
                coverage: PoolTickCoverage::Sparse,
                fetcher: None,
            })
            .expect("V4 pool registers");

        // 3. Apply the backfill buffer.
        core.apply_backfill_buffer_v4(pool_manager, pool_id_bytes);

        {
            let s = core.get_v4_pool(pool_id).expect("registered");
            assert_eq!(
                s.update_block, block_b,
                "V4 update_block advances to buffered event block"
            );
            assert_eq!(
                s.journal.len(),
                1,
                "V4 buffered ModifyLiquidity pushes one journal delta"
            );
            assert_eq!(
                s.tick_data.get(&60).expect("t60").liquidity_gross,
                U128::from(600)
            );
            assert!(s.tick_data.contains_key(&120));
        }

        // 4. Restore before block B → rolls back the ModifyLiquidity.
        core.restore_pool_before_block(pool_id, block_b);
        let s = core.get_v4_pool(pool_id).expect("registered");
        assert_eq!(
            s.tick_data.get(&60).expect("t60").liquidity_gross,
            U128::from(100),
            "V4 tick 60 reverts to registration snapshot on rollback"
        );
        assert!(
            !s.tick_data.contains_key(&120),
            "V4 newly-initialized tick 120 removed on rollback"
        );
    }

    // ── 6N7XVR: pool-registration lifecycle FSM (Quarantined→Live) ────────
    //
    // The rolling-start race the YLYJM2 `drain_pump_completed` buffer gate
    // does NOT cover: a registered pool's LIVE direct-apply path advances
    // `update_block` past `last_complete_block` during the drain+pin+verify
    // window, so the pin captures `(tick_data_without_burn, block_N)` while a
    // same-block Burn stays retained in the pump buffer → mismatch by exactly
    // the Burn's delta (block 25647112 reproduction). The Quarantined lifecycle
    // defers ALL live events (swap + liquidity) to the buffer until
    // drain+pin+verify completes; `set_pool_live` flushes the retained tail.
    //
    // These tests cover the CORE lifecycle + deferral invariants; the
    // positional 25647112 reproduction + concurrent-registration stress live
    // in the wiring/seam task (6XG2NC) and the robust-suite task (BWUHVX).

    /// Register a V4 pool on `core` with a single tick at 60 (gross/net 100)
    /// and `update_block`, returning its `pool_id`. Test helper. `pool_id`
    /// distinguishes concurrent registrations (default `[0xee;32]`).
    fn register_v4_on_core(core: &mut BotState, update_block: u64) -> u64 {
        register_v4_on_core_with_pid(core, [0xeeu8; 32], update_block)
    }

    /// `register_v4_on_core` with an explicit V4 `pool_id` (concurrent-
    /// registration tests need distinct keys).
    fn register_v4_on_core_with_pid(
        core: &mut BotState,
        pool_id_bytes: [u8; 32],
        update_block: u64,
    ) -> u64 {
        use crate::bot_core::{RegisterV4PoolParams, TickInfo, V4PoolKey};
        use crate::solvers::arb_engine::PoolTickCoverage;
        use alloy::primitives::U128;
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: U128::from(100),
                liquidity_net: I256::try_from(100i128).unwrap(),
                block: 0,
            },
        );
        let pool_manager = Address::from([0x44u8; 20]);
        core.register_v4_pool(&RegisterV4PoolParams {
            pool_manager,
            pool_id: pool_id_bytes,
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee: 500,
                tick_spacing: 10,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
            update_block,
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
        })
        .expect("test setup: V4 registration")
    }

    /// A freshly-registered CL pool's lifecycle is COVERAGE-AWARE (DFQYM5):
    /// `Tracked` (complete liquidity map, pins + step-2 verifies) defaults to
    /// `Quarantined` so no live event direct-applies before the two-step
    /// verify; `Sparse` (no complete map → no pin / step-2 verify) stays
    /// `Live`/direct-apply. True for both V3 and V4.
    #[test]
    fn fresh_pool_lifecycle_is_coverage_aware() {
        use crate::bot_core::{RegisterV3PoolParams, RegisterV4PoolParams, TickInfo, V4PoolKey};
        use crate::solvers::arb_engine::PoolTickCoverage;
        use alloy::primitives::{I256, U128};
        let mut core = BotState::new();

        // Tracked V4 → Quarantined (register_v4_on_core uses Tracked).
        let tracked_v4 = register_v4_on_core(&mut core, 0);
        assert_eq!(
            core.get_v4_pool(tracked_v4).unwrap().registration_lifecycle,
            RegistrationLifecycle::Quarantined
        );

        // Sparse V3 → Live (register_v3_on_core uses Sparse).
        let sparse_v3 = register_v3_on_core(&mut core, Address::from([0x88u8; 20]), 0);
        assert_eq!(
            core.get_v3_pool(sparse_v3).unwrap().registration_lifecycle,
            RegistrationLifecycle::Live
        );

        // Sparse V4 → Live: trim the Tracked helper's params to Sparse, then
        // release the Tracked pool to free its key space isn't needed — use a
        // fresh pool_id.
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: U128::from(100),
                liquidity_net: I256::try_from(100i128).unwrap(),
                block: 0,
            },
        );
        let pm = Address::from([0x44u8; 20]);
        let pid = [0xabu8; 32];
        let sparse_v4 = core
            .register_v4_pool(&RegisterV4PoolParams {
                pool_manager: pm,
                pool_id: pid,
                pool_key: V4PoolKey {
                    currency0: Address::ZERO,
                    currency1: Address::from([1u8; 20]),
                    fee: 500,
                    tick_spacing: 10,
                    hooks: Address::ZERO,
                },
                hook_flags: 0,
                protocol_fee: 0,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data,
                update_block: 0,
                coverage: PoolTickCoverage::Sparse,
                fetcher: None,
            })
            .expect("sparse V4 registration");
        assert_eq!(
            core.get_v4_pool(sparse_v4).unwrap().registration_lifecycle,
            RegistrationLifecycle::Live
        );

        // Tracked V3 → Quarantined: reuse the V3 Sparse helper's shape but
        // override coverage to Tracked.
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: U128::from(100),
                liquidity_net: I256::try_from(100i128).unwrap(),
                block: 0,
            },
        );
        let tracked_v3 = core
            .register_v3_pool(&RegisterV3PoolParams {
                address: Address::from([0x99u8; 20]),
                token0: Address::ZERO,
                token1: Address::from([1u8; 20]),
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data,
                update_block: 0,
                coverage: PoolTickCoverage::Tracked,
                fetcher: None,
                ..Default::default()
            })
            .expect("tracked V3 registration");
        assert_eq!(
            core.get_v3_pool(tracked_v3).unwrap().registration_lifecycle,
            RegistrationLifecycle::Quarantined
        );
    }

    /// `set_v3/v4_pool_quarantined` is a no-op for non-`Tracked` pools: a
    /// `Sparse` pool has no pin / step-2 verify to protect, so it must stay
    /// `Live`/direct-apply (DFQYM5 carve-out). The driver calls `set_*_quarantined`
    /// for every registered pool, so this guard is what keeps Sparse out of the
    /// `quarantine→buffer→set_live` round trip.
    #[test]
    fn sparse_pool_ignores_set_quarantined() {
        let mut core = BotState::new();
        let pool_addr = Address::from([0x77u8; 20]);
        // register_v3_on_core uses Sparse coverage.
        register_v3_on_core(&mut core, pool_addr, 0);
        core.set_v3_pool_quarantined(pool_addr);
        assert_eq!(
            core.get_v3_pool(core.pool_id_by_address(&pool_addr).unwrap())
                .unwrap()
                .registration_lifecycle,
            RegistrationLifecycle::Live,
            "Sparse pool must ignore set_quarantined and stay Live"
        );
    }

    /// `release_all_v3_v4_quarantined` flushes + marks `Live` every pool still
    /// `Quarantined` — the orphan sweep that stops a Tracked pool registered
    /// but never reaching `set_live` (path skipped before registration) from
    /// deferring events to its buffer indefinitely. Already-`Live` (Sparse) and
    /// non-CL pools are untouched.
    #[test]
    fn release_all_quarantined_flushes_and_marks_live() {
        use alloy::primitives::{I256, U128};
        let mut core = BotState::new();
        // Two Tracked pools — both register Quarantined under DFQYM5.
        let tracked_v3 = register_v3_on_core(&mut core, Address::from([0x55u8; 20]), 0);
        // register_v3_on_core is Sparse — build a Tracked V3 explicitly.
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: U128::from(100),
                liquidity_net: I256::try_from(100i128).unwrap(),
                block: 0,
            },
        );
        let tracked_v3b = core
            .register_v3_pool(&crate::bot_core::RegisterV3PoolParams {
                address: Address::from([0x66u8; 20]),
                token0: Address::ZERO,
                token1: Address::from([1u8; 20]),
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data,
                update_block: 0,
                coverage: PoolTickCoverage::Tracked,
                fetcher: None,
                ..Default::default()
            })
            .expect("tracked V3");
        let tracked_v4 = register_v4_on_core(&mut core, 0);
        let _ = tracked_v3; // Sparse → Live from the start, not in the sweep.
        assert_eq!(
            core.get_v3_pool(tracked_v3b)
                .unwrap()
                .registration_lifecycle,
            RegistrationLifecycle::Quarantined
        );
        assert_eq!(
            core.get_v4_pool(tracked_v4).unwrap().registration_lifecycle,
            RegistrationLifecycle::Quarantined
        );

        core.release_all_v3_v4_quarantined();

        assert_eq!(
            core.get_v3_pool(tracked_v3b)
                .unwrap()
                .registration_lifecycle,
            RegistrationLifecycle::Live
        );
        assert_eq!(
            core.get_v4_pool(tracked_v4).unwrap().registration_lifecycle,
            RegistrationLifecycle::Live
        );
        // The Sparse V3 (register_v3_on_core) was already Live; release
        // (which would be idempotent) is safe here too.
        assert_eq!(
            core.get_v3_pool(tracked_v3).unwrap().registration_lifecycle,
            RegistrationLifecycle::Live
        );
    }

    /// A `Quarantined` V4 pool's live `Swap` lands in the pump buffer (NOT
    /// applied directly): `tick_data` and scalars are unchanged, but the
    /// buffered event count increases and the buffer carries a `Swap` variant.
    #[test]
    fn quarantined_v4_pool_defers_live_swap_to_pump_buffer() {
        use alloy::primitives::U128;
        let _ = U128::from(0);
        let pool_manager = Address::from([0x44u8; 20]);
        let pool_id_bytes: [u8; 32] = [0xeeu8; 32];
        let mut core = BotState::new();
        let pool_id = register_v4_on_core(&mut core, 10);
        // Quarantine the pool.
        core.set_v4_pool_quarantined(pool_manager, pool_id_bytes);
        assert_eq!(
            core.get_v4_pool(pool_id).unwrap().registration_lifecycle,
            RegistrationLifecycle::Quarantined
        );
        // Snapshot the pre-swap state.
        let pre = core.get_v4_pool(pool_id).unwrap().clone();
        let pre_count = core.buffered_v4_event_count(&(pool_manager, pool_id_bytes));
        // Deliver a live Swap at block 11.
        core.apply_v4_swap(
            &V4SwapUpdate {
                pool_manager,
                pool_id: pool_id_bytes,
                sqrt_price_x96: U256::from(2u128) << 96,
                liquidity: 2_000_000,
                tick: 1,
                tick_priors: vec![],
            },
            11,
        );
        // The swap was deferred: scalars + update_block unchanged.
        let s = core.get_v4_pool(pool_id).unwrap();
        assert_eq!(
            s.update_block, pre.update_block,
            "update_block must NOT advance — swap deferred"
        );
        assert_eq!(
            s.sqrt_price_x96, pre.sqrt_price_x96,
            "sqrt_price_x96 unchanged — swap deferred"
        );
        assert_eq!(
            s.liquidity, pre.liquidity,
            "liquidity unchanged — swap deferred"
        );
        assert_eq!(s.tick, pre.tick, "tick unchanged — swap deferred");
        // The pump buffer gained one event.
        assert_eq!(
            core.buffered_v4_event_count(&(pool_manager, pool_id_bytes)),
            pre_count + 1,
            "live swap buffered, not applied"
        );
    }

    /// A `Quarantined` V4 pool's live `ModifyLiquidity` (Burn) lands in the
    /// pump buffer: `tick_data` is unchanged (the Burn is NOT applied).
    #[test]
    fn quarantined_v4_pool_defers_live_modify_liquidity_to_pump_buffer() {
        use alloy::primitives::U128;
        let _ = U128::from(0);
        let pool_manager = Address::from([0x44u8; 20]);
        let pool_id_bytes: [u8; 32] = [0xeeu8; 32];
        let mut core = BotState::new();
        let pool_id = register_v4_on_core(&mut core, 10);
        core.set_v4_pool_quarantined(pool_manager, pool_id_bytes);
        let pre_t60 = core
            .get_v4_pool(pool_id)
            .unwrap()
            .tick_data
            .get(&60)
            .unwrap()
            .clone();
        let pre_update_block = core.get_v4_pool(pool_id).unwrap().update_block;
        let pre_count = core.buffered_v4_event_count(&(pool_manager, pool_id_bytes));
        // Deliver a live Burn (negative ModifyLiquidity) at block 11.
        core.apply_v4_liquidity_update(
            pool_manager,
            pool_id_bytes,
            60,
            120,
            I256::try_from(-500i128).unwrap(),
            11,
        );
        let s = core.get_v4_pool(pool_id).unwrap();
        assert_eq!(
            *s.tick_data.get(&60).unwrap(),
            pre_t60,
            "tick_data unchanged — Burn deferred"
        );
        assert_eq!(
            s.update_block, pre_update_block,
            "update_block must NOT advance — Burn deferred"
        );
        assert_eq!(
            core.buffered_v4_event_count(&(pool_manager, pool_id_bytes)),
            pre_count + 1,
            "live Burn buffered, not applied"
        );
    }

    /// The 6N7XVR invariant: while `Quarantined`, the pin's source
    /// `update_block` CANNOT outrun `last_complete_block`. A live Swap at
    /// block N+1 (in-progress, `last_complete_block == N`) is deferred, so
    /// `update_block` stays at N — the gated drain then yields only complete-
    /// block events, and the pin captures a self-consistent pair.
    ///
    /// Pre-fix (RED): the live Swap applied directly, advancing `update_block`
    /// to N+1 while a same-block buffered Burn stayed retained; `pin_v4_post_
    /// drain_snapshot` captured `(tick_data_without_burn, N+1)` → mismatch.
    #[test]
    fn quarantined_pool_update_block_cannot_outrun_last_complete_block() {
        use alloy::primitives::U128;
        let pool_manager = Address::from([0x44u8; 20]);
        let pool_id_bytes: [u8; 32] = [0xeeu8; 32];
        let mut core = BotState::new();
        // Snapshot seed at S=10; register the pool.
        let _pool_id = register_v4_on_core(&mut core, 10);
        // Quarantine BEFORE any live event lands (the registration seam's job).
        core.set_v4_pool_quarantined(pool_manager, pool_id_bytes);
        // The pump has fully delivered block 10 (tombstone at 11).
        core.advance_pump_complete_cutoff(10);
        // A live Swap lands at block 11 (in-progress; `last_complete_block` is
        // still 10 — no tombstone for 11 yet).
        core.apply_v4_swap(
            &V4SwapUpdate {
                pool_manager,
                pool_id: pool_id_bytes,
                sqrt_price_x96: U256::from(2u128) << 96,
                liquidity: 2_000_000,
                tick: 1,
                tick_priors: vec![],
            },
            11,
        );
        // Drain the complete-block tail (block 10 has no events; block 11 is
        // retained by the gate).
        core.apply_pump_buffer_v4(pool_manager, pool_id_bytes);
        // Pin the post-drain pair.
        core.pin_v4_post_drain_snapshot(pool_manager, &pool_id_bytes);
        let (tick_data, pinned_block) = core
            .take_v4_post_drain_snapshot(pool_manager, &pool_id_bytes)
            .expect("a Tracked pool pins a post-drain pair");
        // The pin's `update_block` is 10 (the registration block) — the live
        // Swap at 11 was deferred and the gate retained it. `update_block` did
        // NOT advance to 11 (the in-progress block). This is the invariant
        // YLYJM2's buffer gate alone could NOT guarantee (the live path was
        // ungated).
        assert_eq!(
            pinned_block, 10,
            "pin's update_block cannot outrun last_complete_block"
        );
        // The pinned tick_data matches the registration seed (no live event was
        // applied). tick 60 unchanged.
        let t60 = tick_data.get(&60).expect("tick 60 present");
        assert_eq!(t60.liquidity_gross, U128::from(100));
    }

    /// `set_v4_pool_live` flushes the retained in-progress-block pump tail
    /// (via the unguarded `drain_pump`) in insertion order, then marks `Live`.
    /// After the transition, subsequent live events apply directly (the
    /// steady-state contract). The flushed events land in arrival order (swap
    /// after a buffered Burn if it arrived after).
    #[test]
    fn set_v4_pool_live_flushes_retained_tail_and_marks_live() {
        use alloy::primitives::U128;
        let pool_manager = Address::from([0x44u8; 20]);
        let pool_id_bytes: [u8; 32] = [0xeeu8; 32];
        let mut core = BotState::new();
        let pool_id = register_v4_on_core(&mut core, 10);
        core.set_v4_pool_quarantined(pool_manager, pool_id_bytes);
        core.advance_pump_complete_cutoff(10);
        // Buffer a Burn (block 11, in-progress) + a Swap (block 11) — both
        // retained by the gate during quarantine.
        core.apply_v4_liquidity_update(
            pool_manager,
            pool_id_bytes,
            60,
            120,
            I256::try_from(-50i128).unwrap(),
            11,
        );
        core.apply_v4_swap(
            &V4SwapUpdate {
                pool_manager,
                pool_id: pool_id_bytes,
                sqrt_price_x96: U256::from(2u128) << 96,
                liquidity: 2_000_000,
                tick: 1,
                tick_priors: vec![],
            },
            11,
        );
        assert_eq!(
            core.buffered_v4_event_count(&(pool_manager, pool_id_bytes)),
            2,
            "both events retained"
        );
        // Transition to Live: flush the retained tail.
        core.set_v4_pool_live(pool_manager, pool_id_bytes);
        let s = core.get_v4_pool(pool_id).unwrap();
        assert_eq!(s.registration_lifecycle, RegistrationLifecycle::Live);
        assert_eq!(s.update_block, 11, "flush applied both events at block 11");
        // tick 60: 100 (seed) - 50 (Burn) = 50.
        assert_eq!(
            s.tick_data.get(&60).unwrap().liquidity_gross,
            U128::from(50)
        );
        // scalars reflect the flushed swap.
        assert_eq!(s.sqrt_price_x96, U256::from(2u128) << 96);
        // The buffer is drained.
        assert_eq!(
            core.buffered_v4_event_count(&(pool_manager, pool_id_bytes)),
            0
        );
        // A subsequent live event applies directly (no buffering).
        core.apply_v4_liquidity_update(
            pool_manager,
            pool_id_bytes,
            60,
            120,
            I256::try_from(10i128).unwrap(),
            12,
        );
        assert_eq!(
            core.buffered_v4_event_count(&(pool_manager, pool_id_bytes)),
            0,
            "Live pool applies directly — no buffering"
        );
        let s = core.get_v4_pool(pool_id).unwrap();
        assert_eq!(
            s.tick_data.get(&60).unwrap().liquidity_gross,
            U128::from(60)
        );
    }

    /// A `Live` (un-quarantined) registered pool applies events directly — the
    /// 6N7XVR change does NOT regress the steady-state live-apply path.
    #[test]
    fn live_pool_applies_modify_liquidity_directly() {
        use alloy::primitives::U128;
        let pool_manager = Address::from([0x44u8; 20]);
        let pool_id_bytes: [u8; 32] = [0xeeu8; 32];
        let mut core = BotState::new();
        let pool_id = register_v4_on_core(&mut core, 10);
        // A Tracked pool registers `Quarantined` under DFQYM5 — transition it
        // to `Live` (the driver's `set_v4_pool_live` is the sole path to the
        // steady-state direct-apply contract).
        core.set_v4_pool_live(pool_manager, pool_id_bytes);
        // Now Live — applies directly, never buffers.
        core.apply_v4_liquidity_update(
            pool_manager,
            pool_id_bytes,
            60,
            120,
            I256::try_from(-50i128).unwrap(),
            11,
        );
        assert_eq!(
            core.buffered_v4_event_count(&(pool_manager, pool_id_bytes)),
            0,
            "Live pool never buffers"
        );
        let s = core.get_v4_pool(pool_id).unwrap();
        assert_eq!(s.update_block, 11, "applied directly");
        assert_eq!(
            s.tick_data.get(&60).unwrap().liquidity_gross,
            U128::from(50)
        );
    }

    // ── 6N7XVR robust suite (BWUHVX) ──────────────────────────────────────
    //
    // The lifecycle invariant under concurrency, dual-buffer drains, the
    // backfill-boundary regression, and the reorg-during-quarantine edge.

    /// Dual-buffer drain correctness: a quarantined pool with events in BOTH
    /// the backfill buffer (snapshot gap) and the pump buffer (live) drains
    /// both in order during `apply_buffer_*`; the pin reflects backfill +
    /// complete-block pump events; `set_pool_live` flushes only the retained
    /// in-progress pump tail (backfill is always fully drained).
    #[test]
    fn quarantined_pool_dual_buffer_drain_correctness() {
        use alloy::primitives::U128;
        let pool_manager = Address::from([0x44u8; 20]);
        let pool_id_bytes: [u8; 32] = [0xeeu8; 32];
        let mut core = BotState::new();
        // Backfill-range event (block 8, in the snapshot gap S+1..W-1) for
        // an UNregistered pool → backfill buffer.
        core.buffer_backfill_v4_liquidity_update(
            pool_manager,
            pool_id_bytes,
            60,
            120,
            I256::try_from(30i128).unwrap(),
            8,
        );
        // Register the pool (snapshot seed at block 10, tick 60 gross 100).
        let pool_id = register_v4_on_core(&mut core, 10);
        core.set_v4_pool_quarantined(pool_manager, pool_id_bytes);
        // The pump has tombstoned block 10 (live events at 10 are complete).
        core.advance_pump_complete_cutoff(10);
        // A complete-block pump event (block 10) + an in-progress-block pump
        // event (block 11) — both deferred to the pump buffer.
        core.apply_v4_liquidity_update(
            pool_manager,
            pool_id_bytes,
            60,
            120,
            I256::try_from(20i128).unwrap(),
            10,
        );
        core.apply_v4_liquidity_update(
            pool_manager,
            pool_id_bytes,
            60,
            120,
            I256::try_from(-15i128).unwrap(),
            11,
        );
        // Drain both buffers (the registration `apply_buffer_v4` sequence).
        core.apply_backfill_buffer_v4(pool_manager, pool_id_bytes);
        core.apply_pump_buffer_v4(pool_manager, pool_id_bytes);
        // Pin the post-drain pair.
        core.pin_v4_post_drain_snapshot(pool_manager, &pool_id_bytes);
        let (tick_data, pinned_block) = core
            .take_v4_post_drain_snapshot(pool_manager, &pool_id_bytes)
            .expect("Tracked pool pins");
        // The pin reflects backfill (8) + complete-block pump (10): tick 60
        // gross = 100 (seed) + 30 (backfill) + 20 (pump@10) = 150. The pin's
        // `update_block` is 10 (the highest complete-block event). The
        // in-progress block-11 Burn (-15) was RETAINED by the gate.
        assert_eq!(
            pinned_block, 10,
            "pin reflects backfill + complete pump only"
        );
        assert_eq!(
            tick_data.get(&60).unwrap().liquidity_gross,
            U128::from(150),
            "pin = seed + backfill + complete-pump (block-11 Burn retained)"
        );
        // set_live flushes the retained tail (block-11 Burn).
        core.set_v4_pool_live(pool_manager, pool_id_bytes);
        let s = core.get_v4_pool(pool_id).unwrap();
        assert_eq!(s.update_block, 11, "flush advanced to the retained tail");
        assert_eq!(
            s.tick_data.get(&60).unwrap().liquidity_gross,
            U128::from(135),
            "flush applied the block-11 Burn (150 - 15)"
        );
    }

    /// Backfill-boundary regression: the FSM does NOT accidentally gate the
    /// backfill buffer (it is ungated `drain_backfill`). A quarantined pool
    /// whose ONLY events are in the backfill gap still drains them fully at
    /// `apply_backfill_buffer_*` — the two-step verify passes because the pin
    /// reflects the complete backfill.
    #[test]
    fn quarantined_pool_backfill_always_fully_drained() {
        use alloy::primitives::U128;
        let pool_manager = Address::from([0x44u8; 20]);
        let pool_id_bytes: [u8; 32] = [0xeeu8; 32];
        let mut core = BotState::new();
        // A backfill event at block 9 (gap S+1..W-1). No pump buffer events.
        core.buffer_backfill_v4_liquidity_update(
            pool_manager,
            pool_id_bytes,
            60,
            120,
            I256::try_from(40i128).unwrap(),
            9,
        );
        let pool_id = register_v4_on_core(&mut core, 10);
        core.set_v4_pool_quarantined(pool_manager, pool_id_bytes);
        // NO cutoff set (no tombstone yet) — the gated pump drain would yield
        // nothing, but the backfill drain is UNGATED and must still apply.
        core.apply_backfill_buffer_v4(pool_manager, pool_id_bytes);
        core.apply_pump_buffer_v4(pool_manager, pool_id_bytes);
        core.pin_v4_post_drain_snapshot(pool_manager, &pool_id_bytes);
        let (tick_data, pinned_block) = core
            .take_v4_post_drain_snapshot(pool_manager, &pool_id_bytes)
            .expect("Tracked pool pins");
        // The backfill event applied (ungated) → the tick (seed 100 + 40) is
        // present. `update_block` is MONOTONIC (no rewind): the pool registered
        // at block 10 and the backfill event is at the older block 9, so the
        // seed block 10 is retained — applying an older event must not rewind
        // the metadata to look stale (AV42C7: the backfill drain rewinding a
        // head-fresh pool's `update_block` to the backfill boundary produced
        // the solver-state false positives).
        assert_eq!(
            pinned_block, 10,
            "update_block must not rewind to an older backfill block"
        );
        assert_eq!(
            tick_data.get(&60).unwrap().liquidity_gross,
            U128::from(140),
            "backfill fully drained (100 seed + 40)"
        );
        let _ = pool_id;
    }

    /// Concurrent-registration invariant: many pools registered concurrently
    /// with a live pump delivering interleaved ModifyLiquidity/Swap across
    /// blocks — every pool's pin's `update_block` ≤ `last_complete_block`
    /// while Quarantined (the family-level closure of the single-pool fix).
    #[test]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn concurrent_registration_lifecycle_invariant() {
        use alloy::primitives::U128;
        let pool_manager = Address::from([0x44u8; 20]);
        let mut core = BotState::new();
        // Register three V4 pools (distinct pool_ids), quarantine each.
        let pool_ids: Vec<([u8; 32], u64)> = (0..3)
            .map(|i| {
                let pid_byte = 0xeeu8 + i as u8;
                let pid_bytes = [pid_byte; 32];
                let pool_id = register_v4_on_core_with_pid(&mut core, pid_bytes, 10);
                core.set_v4_pool_quarantined(pool_manager, pid_bytes);
                (pid_bytes, pool_id)
            })
            .collect();
        // The pump has fully delivered block 10 (tombstone).
        core.advance_pump_complete_cutoff(10);
        // Interleaved live events: a ModifyLiquidity on pool 0, a Swap on
        // pool 1, a ModifyLiquidity on pool 2 — all at the in-progress block
        // 11. All deferred to their respective pump buffers.
        core.apply_v4_liquidity_update(
            pool_manager,
            pool_ids[0].0,
            60,
            120,
            I256::try_from(-10i128).unwrap(),
            11,
        );
        core.apply_v4_swap(
            &V4SwapUpdate {
                pool_manager,
                pool_id: pool_ids[1].0,
                sqrt_price_x96: U256::from(2u128) << 96,
                liquidity: 2_000_000,
                tick: 1,
                tick_priors: vec![],
            },
            11,
        );
        core.apply_v4_liquidity_update(
            pool_manager,
            pool_ids[2].0,
            60,
            120,
            I256::try_from(5i128).unwrap(),
            11,
        );
        // Drain + pin each pool. For every pool, the pin's `update_block`
        // MUST be ≤ `last_complete_block` (10) — the in-progress block 11
        // events were retained by the gate, NOT applied.
        for (pid_bytes, _) in &pool_ids {
            core.apply_pump_buffer_v4(pool_manager, *pid_bytes);
            core.pin_v4_post_drain_snapshot(pool_manager, pid_bytes);
            let (_, pinned_block) = core
                .take_v4_post_drain_snapshot(pool_manager, pid_bytes)
                .expect("each Tracked pool pins");
            assert!(
                pinned_block <= 10,
                "pool {pid_bytes:?}: pin update_block {pinned_block} must be ≤ last_complete_block 10"
            );
        }
        // Pool 0's tick 60 unchanged (block-11 Burn retained). Pool 2's
        // tick 60 unchanged (block-11 Mint retained). Pool 1's scalars
        // unchanged (block-11 Swap retained).
        let s0 = core.get_v4_pool(pool_ids[0].1).unwrap();
        assert_eq!(
            s0.tick_data.get(&60).unwrap().liquidity_gross,
            U128::from(100)
        );
        let s2 = core.get_v4_pool(pool_ids[2].1).unwrap();
        assert_eq!(
            s2.tick_data.get(&60).unwrap().liquidity_gross,
            U128::from(100)
        );
        let s1 = core.get_v4_pool(pool_ids[1].1).unwrap();
        assert_eq!(s1.sqrt_price_x96, U256::from(1u128) << 96, "swap deferred");
    }

    /// Lifecycle invariant property: for any interleaving of (live pump
    /// events, drain, pin) over a Quarantined pool, the pin's `update_block`
    // ≤ `last_complete_block` AND the pinned `tick_data` excludes any
    // in-progress-block liquidity event. Enumerated interleaving ( Swap arrives
    // before the Mint, both same-block in-progress — both retained).
    #[test]
    fn lifecycle_invariant_swap_before_mint_same_inprogress_block() {
        use alloy::primitives::U128;
        let pool_manager = Address::from([0x44u8; 20]);
        let pool_id_bytes: [u8; 32] = [0xeeu8; 32];
        let mut core = BotState::new();
        let _pool_id = register_v4_on_core(&mut core, 10);
        core.set_v4_pool_quarantined(pool_manager, pool_id_bytes);
        core.advance_pump_complete_cutoff(10);
        // Swap arrives FIRST (logIdx 120), then Mint (logIdx 1433) — both at
        // the in-progress block 11. Cross-type arrival order is preserved in
        // the buffer (Swap before Mint in the Vec).
        core.apply_v4_swap(
            &V4SwapUpdate {
                pool_manager,
                pool_id: pool_id_bytes,
                sqrt_price_x96: U256::from(3u128) << 96,
                liquidity: 3_000_000,
                tick: 2,
                tick_priors: vec![],
            },
            11,
        );
        core.apply_v4_liquidity_update(
            pool_manager,
            pool_id_bytes,
            60,
            120,
            I256::try_from(25i128).unwrap(),
            11,
        );
        core.apply_pump_buffer_v4(pool_manager, pool_id_bytes);
        core.pin_v4_post_drain_snapshot(pool_manager, &pool_id_bytes);
        let (tick_data, pinned_block) = core
            .take_v4_post_drain_snapshot(pool_manager, &pool_id_bytes)
            .expect("Tracked pool pins");
        assert_eq!(pinned_block, 10, "in-progress block 11 events retained");
        assert_eq!(
            tick_data.get(&60).unwrap().liquidity_gross,
            U128::from(100),
            "Mint at 11 NOT applied — retained"
        );
        // Flush at Live applies BOTH in arrival order (Swap, then Mint).
        core.set_v4_pool_live(pool_manager, pool_id_bytes);
    }

    /// Reorg-during-quarantine edge (documented): a reorg that changes
    /// on-chain@`pinned_block` surfaces as a step-2 `VerificationMismatchError`
    /// (fail-fast) — the pinned pair reflects pre-reorg state, on-chain
    /// reflects post-reorg. This test documents that the pin (the verified
    /// pair) is a SEPARATE clone, so a reorg's `restore_before_block` on the
    /// live pool state does NOT corrupt the already-consumed pin; the
    /// reorg's effect on the retained tail is a known gap (the flush-at-Live
    /// would re-apply reorged-block events) that is mitigation-gated to the
    /// reorg coordinator (out of scope: 6N7XVR does not rewrite the reorg
    /// path). Here we assert the pin-independence property.
    #[test]
    fn reorg_during_quarantine_pin_is_independent_of_live_rollback() {
        use alloy::primitives::U128;
        let pool_manager = Address::from([0x44u8; 20]);
        let pool_id_bytes: [u8; 32] = [0xeeu8; 32];
        let mut core = BotState::new();
        let pool_id = register_v4_on_core(&mut core, 10);
        core.set_v4_pool_quarantined(pool_manager, pool_id_bytes);
        core.advance_pump_complete_cutoff(10);
        // A complete-block Mint at 10 (applied at drain), then pin.
        core.apply_v4_liquidity_update(
            pool_manager,
            pool_id_bytes,
            60,
            120,
            I256::try_from(50i128).unwrap(),
            10,
        );
        core.apply_pump_buffer_v4(pool_manager, pool_id_bytes);
        core.pin_v4_post_drain_snapshot(pool_manager, &pool_id_bytes);
        let (tick_data, pinned_block) = core
            .take_v4_post_drain_snapshot(pool_manager, &pool_id_bytes)
            .expect("Tracked pool pins");
        assert_eq!(pinned_block, 10);
        assert_eq!(tick_data.get(&60).unwrap().liquidity_gross, U128::from(150));
        // Now a reorg rolls back block 10 on the LIVE pool state. The
        // already-consumed pin (a clone) is UNAFFECTED — verify @ pinned_block
        // 10 would compare this frozen pair against post-reorg on-chain@10
        // (which lacks the Mint) → mismatch → fail-fast (the reorg surfaces).
        core.restore_pool_before_block(pool_id, 10);
        let s = core.get_v4_pool(pool_id).unwrap();
        assert_eq!(
            s.tick_data.get(&60).unwrap().liquidity_gross,
            U128::from(100),
            "live state rolled back the Mint"
        );
        // The pin (consumed above) is independent — it still holds (150, 10),
        // a frozen snapshot the verify compares against post-reorg on-chain.
        // (No re-assertion of the pin value: it was moved out by take_*.)
    }

    /// Plan 102, slice 2: `BotState::register_v4_pool` returns a typed
    /// `RegisterV4PoolError` (not a flat `String`) for each admission
    /// category, so the `PyO3` seam can surface distinct Python exception
    /// types. Pins the three variants the seam maps to
    /// `HookedPoolRejectedError` / `DynamicFeePoolRejectedError` / plain
    /// `PyValueError` respectively.
    #[test]
    fn register_v4_pool_rejects_amount_modifying_hook_with_typed_error() {
        use crate::bot_core::{RegisterV4PoolError, RegisterV4PoolParams, V4PoolKey};
        use crate::solvers::arb_engine::PoolTickCoverage;
        use std::collections::HashMap;

        let mut core = BotState::new();
        let err = core
            .register_v4_pool(&RegisterV4PoolParams {
                pool_manager: Address::from([0x44u8; 20]),
                pool_id: [0xeeu8; 32],
                pool_key: V4PoolKey {
                    currency0: Address::ZERO,
                    currency1: Address::from([1u8; 20]),
                    fee: 500,
                    tick_spacing: 10,
                    hooks: Address::ZERO,
                },
                // BEFORE_SWAP (0x80) — amount-modifying.
                hook_flags: 0x80,
                protocol_fee: 0,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data: HashMap::new(),
                update_block: 0,
                coverage: PoolTickCoverage::Sparse,
                fetcher: None,
            })
            .expect_err("hooked pool must be rejected");
        assert_eq!(
            err,
            RegisterV4PoolError::HookedPool { hook_flags: 0x80 },
            "amount-modifying-hook refusal returns the typed HookedPool variant"
        );
    }

    #[test]
    fn register_v4_pool_rejects_dynamic_fee_with_typed_error() {
        use crate::bot_core::{RegisterV4PoolError, RegisterV4PoolParams, V4PoolKey};
        use crate::solvers::arb_engine::PoolTickCoverage;
        use std::collections::HashMap;

        let mut core = BotState::new();
        let err = core
            .register_v4_pool(&RegisterV4PoolParams {
                pool_manager: Address::from([0x44u8; 20]),
                pool_id: [0xeeu8; 32],
                pool_key: V4PoolKey {
                    currency0: Address::ZERO,
                    currency1: Address::from([1u8; 20]),
                    fee: crate::bot_core::V4_DYNAMIC_FEE_FLAG,
                    tick_spacing: 10,
                    hooks: Address::ZERO,
                },
                hook_flags: 0,
                protocol_fee: 0,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data: HashMap::new(),
                update_block: 0,
                coverage: PoolTickCoverage::Sparse,
                fetcher: None,
            })
            .expect_err("dynamic-fee pool must be rejected");
        assert_eq!(
            err,
            RegisterV4PoolError::DynamicFee {
                fee: crate::bot_core::V4_DYNAMIC_FEE_FLAG,
            },
            "dynamic-fee refusal returns the typed DynamicFee variant"
        );
    }

    #[test]
    fn register_v4_pool_rejects_duplicate_with_already_registered_variant() {
        use crate::bot_core::{RegisterV4PoolError, RegisterV4PoolParams, V4PoolKey};
        use crate::solvers::arb_engine::PoolTickCoverage;
        use std::collections::HashMap;

        let pool_manager = Address::from([0x44u8; 20]);
        let pool_id_bytes: degenbot_decoders::v4_swap_decoder::V4PoolId = [0xeeu8; 32];
        let mut core = BotState::new();
        let params = RegisterV4PoolParams {
            pool_manager,
            pool_id: pool_id_bytes,
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee: 500,
                tick_spacing: 10,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
        };
        core.register_v4_pool(&params)
            .expect("first registration ok");
        let err = core
            .register_v4_pool(&params)
            .expect_err("duplicate registration must be rejected");
        assert_eq!(
            err,
            RegisterV4PoolError::AlreadyRegistered {
                pool_manager,
                pool_id: pool_id_bytes,
            },
            "duplicate-registration refusal returns the typed AlreadyRegistered variant"
        );
    }

    // -----------------------------------------------------------------------
    // Spec-bound admission (epic WOYYS2 / task K3IICB).
    // Mirrors the V2/V3 spec-bound tests: `register_v4_pool` now rejects
    // out-of-solidity-bounds `sqrt_price_x96` / `tick` / V4 `fee` /
    // `tick_spacing` with a typed `RegisterV4PoolError::SpecViolation`, ahead
    // of the existing `HookedPool` / `DynamicFee` / `AlreadyRegistered`
    // rejections. The four V4 spec validators (`validate_sqrt_price` /
    // `validate_tick` / `validate_v4_fee` / `validate_tick_spacing`) are the
    // same family-agnostic CL validators V3 uses (V4 shares TickMath); only
    // `validate_v4_fee` is V4-specific (the `0x800000` high bit flags a
    // dynamic-fee pool, which `DynamicFee` rejects upstream as a more specific
    // typed variant).
    // -----------------------------------------------------------------------

    /// Baseline in-spec V4 params at tick 0, srqt=1<<96, fee=500,
    /// `tick_spacing=10`. Each spec-violation test below derives a
    /// broken-on-one-field copy.
    fn make_v4_params_in_spec() -> crate::bot_core::RegisterV4PoolParams {
        use crate::bot_core::{RegisterV4PoolParams, V4PoolKey};
        use crate::solvers::arb_engine::PoolTickCoverage;
        use std::collections::HashMap;
        RegisterV4PoolParams {
            pool_manager: Address::from([0x44u8; 20]),
            pool_id: [0xeeu8; 32],
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee: 500,
                tick_spacing: 10,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
        }
    }

    #[test]
    fn register_v4_pool_rejects_sqrt_price_at_max_as_spec_violation() {
        use crate::bot_core::RegisterV4PoolError;
        let mut core = BotState::new();
        let mut params = make_v4_params_in_spec();
        // Distinct pool_id so the duplicate-registered guard never fires if the
        // previous test's params linger in core (defensive; core is fresh here).
        params.pool_id = [0xe1u8; 32];
        params.sqrt_price_x96 = U256::from(degenbot_cl_math::cl_lib::tick_math::MAX_SQRT_RATIO);
        assert!(
            matches! {
                core.register_v4_pool(&params),
                Err(RegisterV4PoolError::SpecViolation(v)) if v.field == "sqrtPriceX96",
            },
            "sqrtPriceX96 == MAX_SQRT_RATIO surfaces a V4 typed SpecViolation"
        );
    }

    #[test]
    fn register_v4_pool_rejects_sqrt_price_below_min_as_spec_violation() {
        use crate::bot_core::RegisterV4PoolError;
        let mut core = BotState::new();
        let mut params = make_v4_params_in_spec();
        params.pool_id = [0xe2u8; 32];
        params.sqrt_price_x96 =
            U256::from(degenbot_cl_math::cl_lib::tick_math::MIN_SQRT_RATIO) - uint!(1_U256);
        assert!(
            matches! {
                core.register_v4_pool(&params),
                Err(RegisterV4PoolError::SpecViolation(v)) if v.field == "sqrtPriceX96",
            },
            "sqrtPriceX96 < MIN_SQRT_RATIO surfaces a V4 typed SpecViolation"
        );
    }

    #[test]
    fn register_v4_pool_rejects_tick_below_min_as_spec_violation() {
        use crate::bot_core::RegisterV4PoolError;
        let mut core = BotState::new();
        let mut params = make_v4_params_in_spec();
        params.pool_id = [0xe3u8; 32];
        params.tick = degenbot_cl_math::cl_lib::tick_math::MIN_TICK - 1;
        assert!(
            matches! {
                core.register_v4_pool(&params),
                Err(RegisterV4PoolError::SpecViolation(v)) if v.field == "tick",
            },
            "tick < MIN_TICK surfaces a V4 typed SpecViolation"
        );
    }

    #[test]
    fn register_v4_pool_rejects_fee_at_v4_max_as_spec_violation() {
        // The V4 fee bound is `< 1 << 24` (uint24 width; the `0x800000` high
        // bit is the dynamic-fee flag, separately rejected as `DynamicFee`).
        // A fee of `1 << 24` itself is out-of-spec for V4 — distinct from a
        // dynamic-fee flag, and surfaces as a `SpecViolation`, not `DynamicFee`.
        use crate::bot_core::RegisterV4PoolError;
        let mut core = BotState::new();
        let mut params = make_v4_params_in_spec();
        params.pool_id = [0xe4u8; 32];
        params.pool_key.fee = ::degenbot_pools::spec_bounds::V4_FEE_MAX;
        assert!(
            matches! {
                core.register_v4_pool(&params),
                Err(RegisterV4PoolError::SpecViolation(v)) if v.field == "fee",
            },
            "V4 fee >= 1 << 24 surfaces a V4 typed SpecViolation (not DynamicFee)"
        );
    }

    #[test]
    fn register_v4_pool_rejects_fee_exceeding_encoder_limit() {
        // The cmd_executor encodes V4 `fee` as a 2-byte field in both
        // V4_SWAP_COMPACT and V4_SWAP_DYNAMIC (the contract masks `& 65535`).
        // A static fee > 65535 is protocol-valid (`< 1 << 24`, not the
        // dynamic-fee flag) but un-encodable — and unprofitable (32%+ per
        // swap). Reject at admission (ergo DPODAZ), mirroring the dynamic-fee
        // refusal, so these pools never enter the path graph.
        use crate::bot_core::RegisterV4PoolError;
        let mut core = BotState::new();
        let mut params = make_v4_params_in_spec();
        params.pool_id = [0xe6u8; 32];
        // fee=320000 — a real mainnet V4 pool (32% fee) seen in the bot run.
        params.pool_key.fee = 320_000;
        assert!(
            matches! {
                core.register_v4_pool(&params),
                Err(RegisterV4PoolError::FeeExceedsEncoderLimit { fee }) if fee == 320_000,
            },
            "V4 fee > u16::MAX (65535) must surface the typed FeeExceedsEncoderLimit variant at admission"
        );
    }

    #[test]
    fn register_v4_pool_admits_fee_at_encoder_limit_boundary() {
        // fee = 65535 (u16::MAX) is the largest encodable static fee; it must
        // be ADMITTED (the executor's 2-byte field holds it). fee = 65536 is
        // the first un-encodable value; it must be rejected.
        let mut core = BotState::new();
        let mut params = make_v4_params_in_spec();
        params.pool_id = [0xe7u8; 32];
        params.pool_key.fee = 65_535;
        assert!(
            core.register_v4_pool(&params).is_ok(),
            "fee = u16::MAX (65535) is encodable and must be admitted"
        );

        let mut core = BotState::new();
        let mut params = make_v4_params_in_spec();
        params.pool_id = [0xe8u8; 32];
        params.pool_key.fee = 65_536;
        assert!(
            matches! {
                core.register_v4_pool(&params),
                Err(crate::bot_core::RegisterV4PoolError::FeeExceedsEncoderLimit { fee }) if fee == 65_536,
            },
            "fee = 65536 (first value > u16::MAX) must be rejected as FeeExceedsEncoderLimit"
        );
    }

    #[test]
    fn register_v4_pool_rejects_tick_spacing_out_of_range_as_spec_violation() {
        use crate::bot_core::RegisterV4PoolError;
        let mut core = BotState::new();
        let mut params = make_v4_params_in_spec();
        params.pool_id = [0xe5u8; 32];
        params.pool_key.tick_spacing = ::degenbot_pools::spec_bounds::MAX_TICK_SPACING + 1;
        assert!(
            matches! {
                core.register_v4_pool(&params),
                Err(RegisterV4PoolError::SpecViolation(v)) if v.field == "tickSpacing",
            },
            "tickSpacing > MAX_TICK_SPACING surfaces a V4 typed SpecViolation"
        );
    }

    #[test]
    fn register_v4_pool_accepts_in_spec_params() {
        // Green companion for the V4 reject tests above: baseline
        // in-spec V4 params must register OK (and reach the
        // `AlreadyRegistered` guard cleanly past the spec validators).
        let mut core = BotState::new();
        let params = make_v4_params_in_spec();
        let pool_id = core
            .register_v4_pool(&params)
            .expect("in-spec V4 params must register");
        assert!(pool_id > 0, "registration returns a non-zero pool_id");
    }

    /// Regression (RAJ3PP): `PyLiquidityPool.apply_swap` routed V4 pools into
    /// `apply_v3_swap_by_pool_id`, which matches `PoolEntry::V3` only and
    /// silently no-op'd on `PoolEntry::V4` — a Python-side V4 update path
    /// (snapshots, regression tests, manual `external_update`) dropped every
    /// update. The fix is a family-dispatching `apply_swap_by_pool_id` on
    /// `BotState` that `PyLiquidityPool.apply_swap` calls (the preferred
    /// "existing methods do family dispatch internally" option). This test
    /// pins both halves of the AC: V4 scalars actually change (not a no-op),
    /// and they match a direct `apply_v4_swap` on the same scalar inputs.
    #[test]
    fn apply_swap_by_pool_id_routes_to_v4_and_matches_apply_v4_swap() {
        use crate::bot_core::{RegisterV4PoolParams, V4PoolKey, V4SwapUpdate};
        use crate::solvers::arb_engine::PoolTickCoverage;

        let pool_manager = Address::from([0x44u8; 20]);
        let pool_id_bytes: degenbot_decoders::v4_swap_decoder::V4PoolId = [0x66u8; 32];
        let block_b = 7u64;

        let make_params = || RegisterV4PoolParams {
            pool_manager,
            pool_id: pool_id_bytes,
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee: 10_000,
                tick_spacing: 60,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
        };

        // Twin pools: A updated via the family dispatcher, B via the
        // dedicated V4 path (`apply_v4_swap`). Both start identical.
        let mut core_a = BotState::new();
        let id_a = core_a
            .register_v4_pool(&make_params())
            .expect("V4 pool A registers");
        let mut core_b = BotState::new();
        let id_b = core_b
            .register_v4_pool(&make_params())
            .expect("V4 pool B registers");

        // Before the fix, this call was a silent no-op on A (routed to the
        // V3-only method). Assert it now applies.
        let _ = core_a.apply_swap_by_pool_id(
            id_a,
            U256::from(2u128) << 96,
            2_000_000,
            -100,
            block_b,
            &[],
        );
        let s_a = core_a.get_v4_pool(id_a).expect("V4 pool A registered");
        assert_eq!(s_a.sqrt_price_x96, U256::from(2u128) << 96);
        assert_eq!(s_a.liquidity, 2_000_000);
        assert_eq!(s_a.tick, -100);
        assert_eq!(s_a.update_block, block_b);
        assert_eq!(
            s_a.journal.len(),
            1,
            "the dispatcher must journal a scalar delta like apply_v4_swap"
        );

        // AC parity: same scalar inputs via `apply_v4_swap` produce identical
        // post-state. The dispatcher's V4 branch mirrors `apply_v4_swap`'s
        // body (same tick_priors=[], same journal shape).
        let _ = core_b.apply_v4_swap(
            &V4SwapUpdate {
                pool_manager,
                pool_id: pool_id_bytes,
                sqrt_price_x96: U256::from(2u128) << 96,
                liquidity: 2_000_000,
                tick: -100,
                tick_priors: Vec::new(),
            },
            block_b,
        );
        let s_b = core_b.get_v4_pool(id_b).expect("V4 pool B registered");
        assert_eq!(s_b.sqrt_price_x96, s_a.sqrt_price_x96);
        assert_eq!(s_b.liquidity, s_a.liquidity);
        assert_eq!(s_b.tick, s_a.tick);
        assert_eq!(s_b.update_block, s_a.update_block);
        assert_eq!(s_b.journal.len(), s_a.journal.len());
    }

    /// Regression (RAJ3PP, V4 `apply_liquidity_update` half): the liquidity
    /// update previously routed V4 pools into `apply_v3_liquidity_update_by_pool
    /// _id` (V3-only, no-op on V4). The family dispatcher must apply a V4
    /// `ModifyLiquidity` to the tick range and journal it, matching
    /// `apply_v4_liquidity_update` on the same inputs.
    #[test]
    fn apply_liquidity_update_by_pool_id_routes_to_v4_and_applies_ticks() {
        use crate::bot_core::{RegisterV4PoolParams, TickInfo, V4PoolKey};
        use crate::solvers::arb_engine::PoolTickCoverage;
        use alloy::primitives::{I256, U128};

        let pool_manager = Address::from([0x55u8; 20]);
        let pool_id_bytes: degenbot_decoders::v4_swap_decoder::V4PoolId = [0x77u8; 32];
        let block_b = 5u64;

        // Pre-seed tick 60 (gross=100, net=+100); tick 120 absent.
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: U128::from(100),
                liquidity_net: I256::try_from(100i128).unwrap(),
                block: 0,
            },
        );
        let params = RegisterV4PoolParams {
            pool_manager,
            pool_id: pool_id_bytes,
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee: 10_000,
                tick_spacing: 60,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
            update_block: 0,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
        };

        // Twin pools: A via the dispatcher, B via apply_v4_liquidity_update.
        let mut core_a = BotState::new();
        let id_a = core_a.register_v4_pool(&params).expect("V4 A registers");
        let mut core_b = BotState::new();
        let id_b = core_b.register_v4_pool(&params).expect("V4 B registers");

        // Before the fix this no-op'd on A. Assert it now applies.
        assert_eq!(
            core_a.apply_liquidity_update_by_pool_id(id_a, 60, 120, 500, block_b),
            Some(id_a),
            "dispatcher must report an applied V4 liquidity update"
        );
        let s_a = core_a.get_v4_pool(id_a).expect("registered A");
        assert_eq!(s_a.update_block, block_b);
        assert_eq!(s_a.journal.len(), 1);
        assert_eq!(
            s_a.tick_data.get(&60).expect("t60").liquidity_gross,
            U128::from(600),
            "tick 60 gross += delta (ModifyLiquidity) via the dispatcher"
        );
        assert!(s_a.tick_data.contains_key(&120), "tick 120 initialized");
        // slot0 scalars unchanged (tick-only event per ADR-004).
        assert_eq!(s_a.sqrt_price_x96, U256::from(1u128) << 96);

        // Parity: direct apply_v4_liquidity_update on B produces the same state.
        assert_eq!(
            core_b.apply_v4_liquidity_update(
                pool_manager,
                pool_id_bytes,
                60,
                120,
                I256::try_from(500i128).unwrap(),
                block_b
            ),
            Some(id_b)
        );
        let s_b = core_b.get_v4_pool(id_b).expect("registered B");
        assert_eq!(
            s_b.tick_data.get(&60).expect("t60").liquidity_gross,
            s_a.tick_data.get(&60).expect("t60").liquidity_gross
        );
        assert_eq!(s_b.journal.len(), s_a.journal.len());
        assert_eq!(s_b.update_block, s_a.update_block);
    }

    /// Regression (J63J3N, scalar read half): `PyLiquidityPool.snapshot_v3`
    /// and the per-field scalar getters all routed through `get_v3_pool`,
    /// which returns `None` for `PoolEntry::V4` — silently dropping V4 reads
    /// (the read-side twin of the RAJ3PP write-side bug). The fix is the
    /// family-dispatching `BotState::get_v3_or_v4_pool` accessor returning a
    /// `&dyn ConcentratedLiquidityPool`. This pins both halves of the AC: a V4 pool
    /// returns a non-`None` scalar view, and the scalars match a direct
    /// `apply_v4_swap` on the same inputs.
    #[test]
    fn get_v3_or_v4_pool_reads_v4_scalars_matching_apply_v4_swap() {
        use crate::bot_core::{RegisterV4PoolParams, V4PoolKey, V4SwapUpdate};
        use crate::solvers::arb_engine::PoolTickCoverage;

        let pool_manager = Address::from([0x88u8; 20]);
        let pool_id_bytes: degenbot_decoders::v4_swap_decoder::V4PoolId = [0x99u8; 32];
        let block_b = 11u64;

        let make_params = || RegisterV4PoolParams {
            pool_manager,
            pool_id: pool_id_bytes,
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee: 500,
                tick_spacing: 10,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
        };

        // Twin V4 pools: A read via the family accessor after a dispatcher
        // apply; B updated via the dedicated `apply_v4_swap`. Both start
        // identical.
        let mut core_a = BotState::new();
        let id_a = core_a
            .register_v4_pool(&make_params())
            .expect("V4 pool A registers");
        let mut core_b = BotState::new();
        let id_b = core_b
            .register_v4_pool(&make_params())
            .expect("V4 pool B registers");

        // Before the fix, `get_v3_or_v4_pool` did not exist and the Python
        // reader used `get_v3_pool` (None for V4). Assert the accessor now
        // returns a non-None view of the post-apply state.
        let _ = core_a.apply_swap_by_pool_id(
            id_a,
            U256::from(3u128) << 96,
            9_000_000,
            -240,
            block_b,
            &[],
        );
        let view_a = core_a
            .get_v3_or_v4_pool(id_a)
            .expect("V4 pool must surface a non-None reader view");
        assert_eq!(view_a.sqrt_price_x96(), U256::from(3u128) << 96);
        assert_eq!(view_a.liquidity(), 9_000_000);
        assert_eq!(view_a.tick(), -240);
        assert_eq!(view_a.update_block(), block_b);
        // Immutable V4 key fields surface from `pool_key` (the ConcentratedLiquidityPool
        // reader trait was slimmed to mutable-only scalars in the V3/V4
        // identity/state split; identity reads go through the family-specific
        // getter rather than the dyn-dispatch view).
        let v4_id_a = core_a
            .get_v4_identity(id_a)
            .expect("registered V4 pool surfaces an identity via get_v4_identity");
        assert_eq!(v4_id_a.pool_key.fee, 500);
        assert_eq!(v4_id_a.pool_key.tick_spacing, 10);

        // AC parity: identical scalar inputs via `apply_v4_swap` produce the
        // same values read through the accessor.
        let _ = core_b.apply_v4_swap(
            &V4SwapUpdate {
                pool_manager,
                pool_id: pool_id_bytes,
                sqrt_price_x96: U256::from(3u128) << 96,
                liquidity: 9_000_000,
                tick: -240,
                tick_priors: Vec::new(),
            },
            block_b,
        );
        let view_b = core_b
            .get_v3_or_v4_pool(id_b)
            .expect("V4 pool B surfaces a reader view");
        assert_eq!(view_b.sqrt_price_x96(), view_a.sqrt_price_x96());
        assert_eq!(view_b.liquidity(), view_a.liquidity());
        assert_eq!(view_b.tick(), view_a.tick());
        assert_eq!(view_b.update_block(), view_a.update_block());
    }

    /// Regression (J63J3N, tick-data read half): `tick_data_snapshot` and
    /// `tick_bitmap_snapshot` routed through `get_v3_pool`, returning an
    /// empty dict for V4 pools. The family `get_v3_or_v4_pool` accessor's
    /// `tick_data()` must surface a V4 pool's tick map (post-Mint/Burn) — and
    /// it must match `apply_v4_liquidity_update` on the same inputs.
    #[test]
    fn get_v3_or_v4_pool_reads_v4_tick_data_matching_apply_v4_liquidity_update() {
        use crate::bot_core::{RegisterV4PoolParams, TickInfo, V4PoolKey};
        use crate::solvers::arb_engine::PoolTickCoverage;
        use alloy::primitives::{I256, U128};

        let pool_manager = Address::from([0xaau8; 20]);
        let pool_id_bytes: degenbot_decoders::v4_swap_decoder::V4PoolId = [0xbbu8; 32];
        let block_b = 13u64;

        // Pre-seed tick 60 (gross=100, net=+100); tick 120 absent.
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: U128::from(100),
                liquidity_net: I256::try_from(100i128).unwrap(),
                block: 0,
            },
        );
        let params = RegisterV4PoolParams {
            pool_manager,
            pool_id: pool_id_bytes,
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee: 3_000,
                tick_spacing: 60,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
            update_block: 0,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
        };

        // Twin pools: A via the dispatcher, B via apply_v4_liquidity_update.
        let mut core_a = BotState::new();
        let id_a = core_a.register_v4_pool(&params).expect("V4 A registers");
        let mut core_b = BotState::new();
        let id_b = core_b.register_v4_pool(&params).expect("V4 B registers");

        let _ = core_a.apply_liquidity_update_by_pool_id(id_a, 60, 120, 700, block_b);
        let _ = core_b.apply_v4_liquidity_update(
            pool_manager,
            pool_id_bytes,
            60,
            120,
            I256::try_from(700i128).unwrap(),
            block_b,
        );

        // Before the fix, the V4 view came back None → empty dict. Assert the
        // accessor now yields the V4 tick map (non-empty, mutated) and the
        // view matches the dedicated-path twin.
        let view_a = core_a
            .get_v3_or_v4_pool(id_a)
            .expect("V4 A reader view non-None");
        let view_b = core_b
            .get_v3_or_v4_pool(id_b)
            .expect("V4 B reader view non-None");
        assert!(!view_a.tick_data().is_empty(), "V4 tick map surfaced");
        assert_eq!(
            view_a.tick_data().get(&60).expect("t60").liquidity_gross,
            U128::from(800),
            "tick 60 gross reflects the +700 ModifyLiquidity"
        );
        assert_eq!(
            view_a.tick_data().get(&60).expect("t60").liquidity_gross,
            view_b.tick_data().get(&60).expect("t60").liquidity_gross,
            "dispatcher and dedicated path produce identical V4 tick maps"
        );
        assert_eq!(view_a.update_block(), view_b.update_block());
    }

    /// Regression (F7HX73): same-block multi-Swap reorg rollback.
    ///
    /// `push_delta` collapsed same-block deltas ("same-block replacement"):
    /// the second Swap at block B replaced the first, so the recorded
    /// `scalar_priors` became post-first-Swap, not pre-block. On
    /// `restore_before_block(B)` the popped delta then returned post-first-Swap
    /// scalars, landing the pool on post-first-Swap instead of the true pre-B
    /// state. (Two same-block swaps on mainnet V3/V4 are common — multi-hop
    /// arb bots, MEV activity.)
    ///
    /// This test pins the AC: register a V3 pool, push two Swap deltas at
    /// block B with different scalars, then `restore_before_block(B)` and
    /// assert the pool scalars match the pre-B (registration) state, not
    /// post-first-Swap.
    ///
    /// Note: the AC text says `restore_before_block(B+1)`, but that is the
    /// no-op case (newest at B < B+1 returns current state = post-both-swaps,
    /// correct for "before B+1"). The bug manifests at `restore_before_block(B)`,
    /// which pops the block-B delta — the trigger exercised here.
    #[test]
    fn v3_restore_before_block_after_same_block_multi_swap_lands_on_pre_block() {
        use crate::bot_core::RegisterV3PoolParams;
        use crate::solvers::arb_engine::PoolTickCoverage;

        let mut core = BotState::new();
        let pool_id = core
            .register_v3_pool(&RegisterV3PoolParams {
                address: Address::from([0xf7u8; 20]),
                token0: Address::ZERO,
                token1: Address::from([1u8; 20]),
                fee: 500,
                tick_spacing: 10,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data: HashMap::new(),
                update_block: 0,
                coverage: PoolTickCoverage::Sparse,
                fetcher: None,
                ..Default::default()
            })
            .expect("test setup: V3 registration");

        let block_b = 9u64;
        // Two same-block Swaps with distinct scalars.
        let _ = core.apply_v3_swap_by_pool_id(
            pool_id,
            U256::from(2u128) << 96,
            2_000_000,
            -10,
            block_b,
            &[],
        );
        let _ = core.apply_v3_swap_by_pool_id(
            pool_id,
            U256::from(3u128) << 96,
            3_000_000,
            -20,
            block_b,
            &[],
        );

        // Sanity: current state reflects the second swap.
        {
            let s = core.get_v3_pool(pool_id).expect("registered");
            assert_eq!(s.sqrt_price_x96, U256::from(3u128) << 96);
        }

        // Roll back block B. Pre-fix this returned post-first-Swap scalars
        // (2<<96, 2_000_000, -10); the fix must land on the pre-B (registration)
        // state (1<<96, 1_000_000, 0).
        let _ = core.restore_pool_before_block(pool_id, block_b);
        let s = core.get_v3_pool(pool_id).expect("registered after restore");
        assert_eq!(
            s.sqrt_price_x96,
            U256::from(1u128) << 96,
            "same-block multi-swap restore lands on pre-B sqrt_price, not post-first-Swap"
        );
        assert_eq!(s.liquidity, 1_000_000);
        assert_eq!(s.tick, 0);
    }

    // -----------------------------------------------------------------------
    // ADR-007: BotState::unregister_pool (V2/V3 address-keyed, V4 tuple-keyed).
    // -----------------------------------------------------------------------

    #[test]
    fn unregister_v2_pool_returns_true_then_re_register_allocates_fresh_id() {
        let mut core = BotState::new();
        let params = make_params(U112::from(1000), U112::from(2000));
        let first_id = core
            .register_v2_pool(&params)
            .expect("test setup: V2 registration");
        assert_eq!(core.pool_count(), 1);

        // Unregister the V2 pool.
        let removed = core.unregister_pool(make_pool_addr(), None);
        assert!(removed, "unregister of a registered V2 pool returns true");
        assert_eq!(core.pool_count(), 0, "unregister must drop the PoolEntry");
        assert_eq!(
            core.pool_id_by_address(&make_pool_addr()),
            None,
            "unregister must clear pool_addresses"
        );

        // Re-register: must succeed (no panic) and allocate a fresh id
        // (retired ids are NOT reused — ADR-007 U3).
        let second_id = core
            .register_v2_pool(&params)
            .expect("test setup: V2 registration");
        assert_ne!(
            second_id, first_id,
            "re-register must allocate a fresh id (retired, not reused)",
        );
        assert_eq!(core.pool_count(), 1);
    }

    #[test]
    fn unregister_pool_on_unknown_address_returns_false_silently() {
        let mut core = BotState::new();
        // Register one pool at make_pool_addr().
        let _ = core
            .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
            .expect("test setup: V2 registration");
        let unknown = Address::from([0x99u8; 20]);

        let removed = core.unregister_pool(unknown, None);
        assert!(!removed, "unregister on an unknown address returns false");
        // No mutation occurred.
        assert_eq!(core.pool_count(), 1);
    }

    // -----------------------------------------------------------------------
    // Spec-bound admission (epic WOYYS2 / task MSTAT2).
    // `register_v2_pool` is a typed `Result` that rejects (a) duplicate
    // address and (b) out-of-spec `uint112` reserves, rather than panicking
    // on (a) and silently degrading to `U256::MAX` on (b).
    // -----------------------------------------------------------------------

    #[test]
    fn register_v2_pool_rejects_duplicate_address_as_already_registered() {
        let mut core = BotState::new();
        let params = make_params(U112::from(1000), U112::from(2000));
        let _ok = core.register_v2_pool(&params).expect("first registration");
        // Second registration at the same address: prior impl `assert!`-panicked;
        // now returns `Err(AlreadyRegistered { address })`.
        assert!(
            matches! {
                core.register_v2_pool(&params),
                Err(RegisterV2PoolError::AlreadyRegistered { address }) if address == params.address,
            },
            "duplicate-address registration surfaces a typed Err, not a panic"
        );
    }

    // Note: the overlarge-reserve rejection that lived here pre-ZPHT6X has
    // moved to the `narrow_v2_reserve` ingestion seam (PyO3 `sync_reserves` /
    // `register_*_pool` paths + the V2 Sync decoder) — see
    // `degenbot_pools::spec_bounds::narrow_v2_reserve` and its tests. With
    // `V2PoolState`/`RegisterV2PoolParams.reserve0/1` typed `U112`, an
    // overlarge value cannot be constructed at the `register_v2_pool` layer
    // (the type system enforces the `uint112` bound), so there is nothing to
    // test here.

    // -----------------------------------------------------------------------
    // Spec-bound admission (epic WOYYS2 / task 24KNGF).
    // `register_v3_pool` is a typed `Result` that rejects (a) duplicate
    // address and (b) out-of-spec `sqrtPriceX96` / `tick` / `fee` /
    // `tickSpacing`, rather than `assert!`-panicking on (a) and silently
    // accepting impossible CL config on (b). Mirrors the V2 tests above.
    // -----------------------------------------------------------------------

    /// Baseline in-spec V3 params at tick 0, `sqrt=1<<96`, `fee=3_000`,
    /// `tick_spacing=60`, undirectional tokens. Each spec-violation test below
    /// derives a fresh broken-on-one-field copy.
    fn make_v3_params_in_spec() -> RegisterV3PoolParams {
        RegisterV3PoolParams {
            address: make_pool_addr(),
            token0: make_token0(),
            token1: make_token1(),
            fee: 3_000,
            tick_spacing: 60,
            factory: make_factory(),
            sqrt_price_x96: U256::from(1u64) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data: std::collections::HashMap::new(),
            update_block: 0,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
            ..Default::default()
        }
    }

    #[test]
    fn register_v3_pool_rejects_duplicate_address_as_already_registered() {
        let mut core = BotState::new();
        let params = make_v3_params_in_spec();
        let _ok = core.register_v3_pool(&params).expect("first registration");
        assert!(
            matches! {
                core.register_v3_pool(&params),
                Err(RegisterV3PoolError::AlreadyRegistered { address }) if address == params.address,
            },
            "duplicate-address registration surfaces a typed Err, not an assert! panic"
        );
    }

    #[test]
    fn register_v3_pool_rejects_sqrt_price_at_max_as_spec_violation() {
        let mut core = BotState::new();
        let mut params = make_v3_params_in_spec();
        params.sqrt_price_x96 = U256::from(degenbot_cl_math::cl_lib::tick_math::MAX_SQRT_RATIO);
        assert!(
            matches! {
                core.register_v3_pool(&params),
                Err(RegisterV3PoolError::SpecViolation(v)) if v.field == "sqrtPriceX96",
            },
            "sqrtPriceX96 == MAX_SQRT_RATIO surfaces a typed SpecViolation"
        );
    }

    #[test]
    fn register_v3_pool_rejects_sqrt_price_below_min_as_spec_violation() {
        let mut core = BotState::new();
        let mut params = make_v3_params_in_spec();
        params.sqrt_price_x96 =
            U256::from(degenbot_cl_math::cl_lib::tick_math::MIN_SQRT_RATIO) - uint!(1_U256);
        assert!(
            matches! {
                core.register_v3_pool(&params),
                Err(RegisterV3PoolError::SpecViolation(v)) if v.field == "sqrtPriceX96",
            },
            "sqrtPriceX96 < MIN_SQRT_RATIO surfaces a typed SpecViolation"
        );
    }

    #[test]
    fn register_v3_pool_rejects_tick_below_min_as_spec_violation() {
        let mut core = BotState::new();
        let mut params = make_v3_params_in_spec();
        params.tick = degenbot_cl_math::cl_lib::tick_math::MIN_TICK - 1;
        assert!(
            matches! {
                core.register_v3_pool(&params),
                Err(RegisterV3PoolError::SpecViolation(v)) if v.field == "tick",
            },
            "tick < MIN_TICK surfaces a typed SpecViolation"
        );
    }

    #[test]
    fn register_v3_pool_rejects_fee_at_max_as_spec_violation() {
        let mut core = BotState::new();
        let mut params = make_v3_params_in_spec();
        params.fee = ::degenbot_pools::spec_bounds::V3_FEE_MAX;
        assert!(
            matches! {
                core.register_v3_pool(&params),
                Err(RegisterV3PoolError::SpecViolation(v)) if v.field == "fee",
            },
            "fee >= 1_000_000 surfaces a typed SpecViolation"
        );
    }

    #[test]
    fn register_v3_pool_rejects_tick_spacing_out_of_range_as_spec_violation() {
        let mut core = BotState::new();
        let mut params = make_v3_params_in_spec();
        params.tick_spacing = ::degenbot_pools::spec_bounds::MAX_TICK_SPACING + 1;
        assert!(
            matches! {
                core.register_v3_pool(&params),
                Err(RegisterV3PoolError::SpecViolation(v)) if v.field == "tickSpacing",
            },
            "tickSpacing > MAX_TICK_SPACING surfaces a typed SpecViolation"
        );
    }

    #[test]
    fn register_v3_pool_accepts_in_spec_params() {
        // Green companion for the reject tests above: each validator's accept
        // boundary (sqrtPriceX96 in [MIN_SQRT_RATIO, MAX_SQRT_RATIO), tick in
        // [MIN_TICK, MAX_TICK], fee < 1_000_000, tickSpacing in [1, 32_767])
        // composes — the baseline `make_v3_params_in_spec()` must register OK.
        let mut core = BotState::new();
        let params = make_v3_params_in_spec();
        let pool_id = core
            .register_v3_pool(&params)
            .expect("in-spec V3 params must register");
        assert!(pool_id > 0, "registration returns a non-zero pool_id");
    }

    #[test]
    fn unregister_v3_pool_discards_buffered_liquidity_events() {
        let mut core = BotState::new();
        let v3_addr = make_pool_addr();

        // Pre-registration: buffer a backfill ModifyLiquidity for `v3_addr`.
        // `buffer_backfill_v3_liquidity_update` on an UNregistered address
        // buffers (the registered-address early-return path doesn't fire).
        core.buffer_backfill_v3_liquidity_update(v3_addr, -100, 100, 500_i128, 42_u64);
        assert_eq!(
            core.buffered_v3_event_count(&v3_addr),
            1,
            "precondition: the event is buffered for the unregistered address"
        );

        // Register the V3 pool. Registration does NOT auto-drain the buffer
        // (drain is caller-driven via `apply_backfill_buffer_v3`); the buffer
        // entry persists. This mirrors the live pump path where events can
        // arrive pre-registration and stay buffered.
        let _ = core
            .register_v3_pool(&RegisterV3PoolParams {
                address: v3_addr,
                token0: make_token0(),
                token1: make_token1(),
                fee: 3_000,
                tick_spacing: 60,
                factory: make_factory(),
                sqrt_price_x96: U256::from(1u64) << 96,
                liquidity: 0,
                tick: 0,
                tick_data: HashMap::new(),
                update_block: 0,
                coverage: PoolTickCoverage::Sparse,
                fetcher: None,
                ..Default::default()
            })
            .expect("test setup: V3 registration");
        assert_eq!(
            core.buffered_v3_event_count(&v3_addr),
            1,
            "registration must not auto-drain (drain is caller-driven), so the buffer entry survives to here"
        );

        // Unregister the V3 pool. ADR-007 U3: drain the V3 buffer for the
        // removed address so a re-register does not replay stale Mint/Burn.
        let removed = core.unregister_pool(v3_addr, None);
        assert!(removed);
        assert_eq!(
            core.buffered_v3_event_count(&v3_addr),
            0,
            "unregister must discard buffered V3 events for the removed address"
        );
    }

    /// CBCH6H: the snapshot seed must be retained separately from the live
    /// `tick_data` so step-1 verify can compare the *seed* against
    /// on-chain@snapshot_block, NOT the pump-mutated current. During a rolling
    /// start (`resume()` precedes `build_paths`) the pump applies Mint/Burn to
    /// `tick_data`; without a pinned seed, step-1 reads engine-current (seed +
    /// journal) vs on-chain@snapshot (pre-journal) → false mismatch on every
    /// active pool (logs/perm-V2-V3-V2.log). The seed is pinned at registration
    /// and never mutated by `apply_v3_liquidity_update`.
    #[test]
    fn v3_snapshot_seed_survives_pump_liquidity_update() {
        use alloy::primitives::{I256, U128};
        let mut core = BotState::new();
        let v3_addr = make_pool_addr();

        // Seed tick_data with one initialized tick (gross=L, net=+L).
        let liq: u128 = 1_000_000;
        let liq_u128 = U256::from(liq).to::<U128>();
        let mut seed = HashMap::new();
        seed.insert(
            -60,
            TickInfo {
                liquidity_gross: liq_u128,
                liquidity_net: I256::try_from(i128::try_from(liq).unwrap()).unwrap(),
                block: 0,
            },
        );
        let seed_clone = seed.clone();

        core.register_v3_pool(&RegisterV3PoolParams {
            address: v3_addr,
            token0: make_token0(),
            token1: make_token1(),
            fee: 3_000,
            tick_spacing: 60,
            factory: make_factory(),
            sqrt_price_x96: U256::from(1u64) << 96,
            liquidity: 0,
            tick: 0,
            tick_data: seed,
            update_block: 0,
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
            ..Default::default()
        })
        .expect("test setup: V3 registration");
        // DFQYM5: Tracked pools register `Quarantined`; transition to `Live`
        // (the driver's post-verify `set_live`) so the pump update below
        // direct-applies as this test models.
        core.set_v3_pool_live(v3_addr);

        // The seed is pinned at registration for Tracked (snapshot) pools.
        assert_eq!(
            core.v3_snapshot_seed(v3_addr).cloned(),
            Some(seed_clone.clone()),
            "Tracked pool must pin its snapshot seed at registration"
        );

        // Pump applies a Mint at block 1, mutating tick -60's gross.
        core.apply_v3_liquidity_update(v3_addr, -60, 60, 500_i128, 1);

        // Live tick_data CHANGED (journal applied) ...
        let live_gross = core
            .get_v3_pool(core.pool_id_by_address(&v3_addr).unwrap())
            .and_then(|s| s.tick_data.get(&-60))
            .map(|t| t.liquidity_gross.to::<u128>());
        assert_ne!(
            live_gross,
            Some(liq),
            "pump Mint must mutate the live tick_data (precondition)"
        );

        // ... but the pinned seed is UNCHANGED — step-1 can still verify the
        // snapshot block against the seed, not the pump-corrupted current.
        assert_eq!(
            core.v3_snapshot_seed(v3_addr).cloned(),
            Some(seed_clone.clone()),
            "snapshot seed must be immutable across pump Mint/Burn (the rolling-start race fix)"
        );

        // `take_v3_snapshot_seed` returns the seed once and clears the slot
        // (memory is freed after step-1 verify; the seed is never needed again).
        let taken = core.take_v3_snapshot_seed(v3_addr);
        assert_eq!(taken, Some(seed_clone), "take returns the pinned seed");
        assert_eq!(
            core.v3_snapshot_seed(v3_addr),
            None,
            "take clears the slot so the seed is verified exactly once"
        );
    }

    /// Step-2 (post-drain) twin of `v3_snapshot_seed_survives_pump_liquidity_update`.
    /// The post-drain pin is captured atomically with `apply_buffer_v3`'s final
    /// drain and must be IMMUTABLE across a subsequent pump Mint/Burn — otherwise
    /// step-2 (verify post-drain state vs on-chain@backfill) reads engine-current
    /// (drain + pump journal) vs on-chain@backfill (pre-journal) → a false
    /// mismatch on every active pool during a rolling start
    /// (logs/verify-race-hotloop.log: tick 59940, `update_block=25396803 >
    /// block=25396790`). The pin is taken once (step-2 verify) then freed.
    #[test]
    fn v3_post_drain_snapshot_survives_pump_liquidity_update() {
        use alloy::primitives::{I256, U128};
        let mut core = BotState::new();
        let v3_addr = make_pool_addr();

        let liq: u128 = 1_000_000;
        let liq_u128 = U256::from(liq).to::<U128>();
        let mut seed = HashMap::new();
        seed.insert(
            -60,
            TickInfo {
                liquidity_gross: liq_u128,
                liquidity_net: I256::try_from(i128::try_from(liq).unwrap()).unwrap(),
                block: 0,
            },
        );
        let drain_clone = seed.clone();

        core.register_v3_pool(&RegisterV3PoolParams {
            address: v3_addr,
            token0: make_token0(),
            token1: make_token1(),
            fee: 3_000,
            tick_spacing: 60,
            factory: make_factory(),
            sqrt_price_x96: U256::from(1u64) << 96,
            liquidity: 0,
            tick: 0,
            tick_data: seed,
            update_block: 0,
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
            ..Default::default()
        })
        .expect("test setup: V3 registration");
        // DFQYM5: Tracked pools register `Quarantined`; transition to `Live` so
        // the pump Mint below direct-applies (this test's model).
        core.set_v3_pool_live(v3_addr);

        // Pin the post-drain state atomically with the drain (no buffer here →
        // pin == current tick_data == the seed at registration). This is what
        // `apply_buffer_v3` does inside its single core.write() hold.
        core.pin_v3_post_drain_snapshot(v3_addr);

        // A pump Mint lands AFTER the drain — mutating the live tick_data.
        core.apply_v3_liquidity_update(v3_addr, -60, 60, 500_i128, 1);

        // Live tick_data CHANGED (journal applied) ...
        let live_gross = core
            .get_v3_pool(core.pool_id_by_address(&v3_addr).unwrap())
            .and_then(|s| s.tick_data.get(&-60))
            .map(|t| t.liquidity_gross.to::<u128>());
        assert_ne!(
            live_gross,
            Some(liq),
            "pump Mint must mutate the live tick_data (precondition)"
        );

        // ... but the pinned post-drain snapshot is UNCHANGED — step-2 verifies
        // the drain-time state, not the pump-corrupted current.
        let taken = core.take_v3_post_drain_snapshot(v3_addr);
        let (tick_data, pinned_block) = taken.expect("Tracked pool pins post-drain");
        assert_eq!(
            tick_data, drain_clone,
            "post-drain pin must be frozen at drain time, not pump-mutated current (step-2 race fix)"
        );
        assert_eq!(
            pinned_block, 0,
            "no buffer events drained → pin's block is the registration update_block (0)"
        );
        assert_eq!(
            core.take_v3_post_drain_snapshot(v3_addr),
            None,
            "take clears the post-drain slot (verified exactly once)"
        );
    }

    /// `Sparse` pools have no complete `tick_data` to pin — the post-drain pin
    /// stays `None` (step-2 verify is a no-op, same as the seed for sparse).
    #[test]
    fn v3_post_drain_snapshot_is_none_for_sparse_pools() {
        let mut core = BotState::new();
        let v3_addr = make_pool_addr();
        core.register_v3_pool(&RegisterV3PoolParams {
            address: v3_addr,
            token0: make_token0(),
            token1: make_token1(),
            fee: 3_000,
            tick_spacing: 60,
            factory: make_factory(),
            sqrt_price_x96: U256::from(1u64) << 96,
            liquidity: 0,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
            ..Default::default()
        })
        .expect("test setup: V3 registration");
        core.pin_v3_post_drain_snapshot(v3_addr);
        assert_eq!(
            core.take_v3_post_drain_snapshot(v3_addr),
            None,
            "sparse pools must not pin post-drain (no complete tick_data)"
        );
    }

    /// Regression (post-drain pin block race, 2026-06-29): the post-drain pin
    /// must carry the block its `tick_data` was computed at — namely the
    /// `update_block` at pin time, which reflects the last drained backfill OR
    /// pump event's block. Pre-fix the pin stored `tick_data` only and the
    /// step-2 verify compared it against on-chain@`verify_backfill_block`
    /// (a start()-time constant). For an active pool on a slow `build_paths`,
    /// the pump buffer accumulated Mint/Burn events at blocks PAST the
    /// backfill boundary; draining them advanced `tick_data` to a state that
    /// matched on-chain at a LATER block, so the verify fabricated a mismatch
    /// and crashed the bot.
    ///
    /// This test reproduces the exact shape: a seed at block S, a backfill
    /// event at S+1 (within the backfill window), and a pump event at block B
    /// (past `verify_backfill_block`). After draining both buffers + pinning,
    /// the pin's block must be B (the pump event's block) — the block on-chain
    /// tick data actually matches at — NOT `verify_backfill_block`.
    #[test]
    fn v3_post_drain_snapshot_carries_drained_block_not_backfill_block() {
        use alloy::primitives::{I256, U128};
        let mut core = BotState::new();
        let v3_addr = make_pool_addr();

        // Snapshot block S (the registration `update_block`).
        let snapshot_block: u64 = 100;
        // Backfill boundary (start()'s `verify_backfill_block`).
        let backfill_block: u64 = 150;
        // Pump event lands at block B, PAST the backfill boundary — the
        // rolling-start window the bot hit in production (a Mint that fired
        // between `subscribe()` and this pool's `register_v3_pool`).
        let pump_block: u64 = backfill_block + 29;

        let seed_liq: u128 = 1_000_000;
        let liq_u128 = U256::from(seed_liq).to::<U128>();
        let mut seed = HashMap::new();
        seed.insert(
            -60,
            TickInfo {
                liquidity_gross: liq_u128,
                liquidity_net: I256::try_from(i128::try_from(seed_liq).unwrap()).unwrap(),
                block: 0,
            },
        );

        // Pre-registration: buffer a backfill Mint at S+1 (within the
        // snapshot→backfill window) and a pump Mint at `pump_block` (past the
        // backfill boundary — the pump was already running during `build_paths`,
        // so the pool's Mint at `pump_block` landed in the unregistered-pool
        // pump buffer via `apply_v3_liquidity_update`).
        core.buffer_backfill_v3_liquidity_update(v3_addr, -60, 60, 500_i128, snapshot_block + 1);
        core.apply_v3_liquidity_update(v3_addr, -60, 60, 750_i128, pump_block);

        core.register_v3_pool(&RegisterV3PoolParams {
            address: v3_addr,
            token0: make_token0(),
            token1: make_token1(),
            fee: 3_000,
            tick_spacing: 60,
            factory: make_factory(),
            sqrt_price_x96: U256::from(1u64) << 96,
            liquidity: 0,
            tick: 0,
            tick_data: seed,
            update_block: snapshot_block,
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
            ..Default::default()
        })
        .expect("test setup: V3 registration");

        // Drain both buffers + pin — exactly what `apply_buffer_v3` does
        // inside its single `core.write()` hold.
        core.apply_backfill_buffer_v3(&v3_addr);
        // YLYJM2: the gated pump drain only yields fully-completed blocks.
        // Mirror the live pump's ADR-008 D1 tombstone (first log of
        // `pump_block`+1 closes `pump_block`) so the drain takes the pump
        // Mint at `pump_block` rather than leaving it behind the gate.
        core.advance_pump_complete_cutoff(pump_block);
        core.apply_pump_buffer_v3(&v3_addr);
        core.pin_v3_post_drain_snapshot(v3_addr);

        // The pin must carry the pump event's block — the block the drained
        // `tick_data` actually matches on-chain at. Pre-fix the pin carried no
        // block at all and the verify used `backfill_block` → false mismatch.
        let taken = core.take_v3_post_drain_snapshot(v3_addr);
        let (tick_data, pinned_block) = taken.expect("Tracked pool pins post-drain");
        assert_eq!(
            pinned_block, pump_block,
            "pin's block must be the last drained event's block (the pump Mint at {pump_block}), \
             not the backfill boundary ({backfill_block}) — pre-fix the verify used the wrong \
             block and fabricated a mismatch on every active pool during a slow build_paths"
        );

        // Sanity: the drained tick_data reflects seed + backfill Mint + pump Mint.
        let t = tick_data.get(&-60).expect("tick -60 present");
        assert_eq!(
            t.liquidity_gross,
            U128::from(seed_liq + 500 + 750),
            "drained tick_data = seed + backfill Mint + pump Mint"
        );

        // Idempotent take: second call returns None (verified exactly once).
        assert_eq!(
            core.take_v3_post_drain_snapshot(v3_addr),
            None,
            "take clears the pin slot (verified exactly once)"
        );
    }

    /// OVVLGO: the V4 twin of the rolling-start race regression. The V4 seed
    /// must survive a pump `ModifyLiquidity` event so step-1 (seed-verify at
    /// the snapshot block) is race-free under a rolling start. Mirrors
    /// `v3_snapshot_seed_survives_pump_liquidity_update` for the V4
    /// `(pool_manager, pool_id)` keying.
    #[test]
    fn v4_snapshot_seed_survives_pump_modify_liquidity() {
        use crate::bot_core::{RegisterV4PoolParams, V4PoolKey};
        use alloy::primitives::{I256, U128};
        use degenbot_cl_math as _;
        let pool_manager = Address::from([0x44u8; 20]);
        let pool_id_bytes: degenbot_decoders::v4_swap_decoder::V4PoolId = [0xeeu8; 32];
        let mut core = BotState::new();

        let gross: u128 = 1_000_000;
        let liq_u128 = U256::from(gross).to::<U128>();
        let mut seed = HashMap::new();
        seed.insert(
            -60,
            TickInfo {
                liquidity_gross: liq_u128,
                liquidity_net: I256::try_from(i128::try_from(gross).unwrap()).unwrap(),
                block: 0,
            },
        );
        let seed_clone = seed.clone();

        core.register_v4_pool(&RegisterV4PoolParams {
            pool_manager,
            pool_id: pool_id_bytes,
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee: 10_000,
                tick_spacing: 60,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 0,
            tick: 0,
            tick_data: seed,
            update_block: 0,
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
        })
        .expect("V4 pool registers");
        // DFQYM5: Tracked pools register `Quarantined`; transition to `Live`
        // (the driver's post-verify `set_live`) so the pump update below
        // direct-applies as this test models.
        core.set_v4_pool_live(pool_manager, pool_id_bytes);

        assert_eq!(
            core.v4_snapshot_seed(pool_manager, &pool_id_bytes).cloned(),
            Some(seed_clone.clone()),
            "Tracked V4 pool must pin its snapshot seed at registration"
        );

        // Pump applies a ModifyLiquidity at block 1, mutating tick -60's gross.
        core.apply_v4_liquidity_update(
            pool_manager,
            pool_id_bytes,
            -60,
            60,
            I256::try_from(500_i128).unwrap(),
            1,
        );

        // Live tick_data CHANGED (journal applied) ...
        let live_gross = {
            let pid = core
                .v4_pool_id_by_key(pool_manager, &pool_id_bytes)
                .expect("registered");
            core.get_v4_pool(pid)
                .and_then(|s| s.tick_data.get(&-60))
                .map(|t| t.liquidity_gross.to::<u128>())
        };
        assert_ne!(
            live_gross,
            Some(gross),
            "pump ModifyLiquidity must mutate the live tick_data (precondition)"
        );

        // ... but the pinned seed is UNCHANGED — step-1 verifies seed, not current.
        assert_eq!(
            core.v4_snapshot_seed(pool_manager, &pool_id_bytes).cloned(),
            Some(seed_clone.clone()),
            "V4 snapshot seed must be immutable across pump ModifyLiquidity (rolling-start race fix)"
        );

        // take: returns the seed exactly once then clears.
        let taken = core.take_v4_snapshot_seed(pool_manager, &pool_id_bytes);
        assert_eq!(taken, Some(seed_clone), "take returns the pinned seed");
        assert_eq!(
            core.v4_snapshot_seed(pool_manager, &pool_id_bytes),
            None,
            "take clears the V4 seed slot (verified exactly once)"
        );
    }

    /// Step-2 (post-drain) V4 twin of
    /// `v4_snapshot_seed_survives_pump_modify_liquidity`. The V4 post-drain pin
    /// is captured atomically with `apply_buffer_v4`'s final drain and must be
    /// IMMUTABLE across a subsequent pump `ModifyLiquidity` — otherwise step-2
    /// reads engine-current (drain + pump journal) vs on-chain@backfill
    /// (pre-journal) → a false mismatch on every active V4 pool during a
    /// rolling start. Pinned for Tracked pools only (Sparse → None, no-op).
    #[test]
    fn v4_post_drain_snapshot_survives_pump_modify_liquidity() {
        use crate::bot_core::{RegisterV4PoolParams, V4PoolKey};
        use alloy::primitives::{I256, U128};
        use degenbot_cl_math as _;
        let pool_manager = Address::from([0x44u8; 20]);
        let pool_id_bytes: degenbot_decoders::v4_swap_decoder::V4PoolId = [0xeeu8; 32];
        let mut core = BotState::new();

        let gross: u128 = 1_000_000;
        let liq_u128 = U256::from(gross).to::<U128>();
        let mut seed = HashMap::new();
        seed.insert(
            -60,
            TickInfo {
                liquidity_gross: liq_u128,
                liquidity_net: I256::try_from(i128::try_from(gross).unwrap()).unwrap(),
                block: 0,
            },
        );
        let drain_clone = seed.clone();

        core.register_v4_pool(&RegisterV4PoolParams {
            pool_manager,
            pool_id: pool_id_bytes,
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee: 10_000,
                tick_spacing: 60,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 0,
            tick: 0,
            tick_data: seed,
            update_block: 0,
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
        })
        .expect("V4 pool registers");
        // DFQYM5: Tracked pools register `Quarantined`; transition to `Live` so
        // the pump ModifyLiquidity below direct-applies (this test's model).
        core.set_v4_pool_live(pool_manager, pool_id_bytes);

        // Pin post-drain state atomically with the drain (what apply_buffer_v4
        // does inside its single core.write() hold).
        core.pin_v4_post_drain_snapshot(pool_manager, &pool_id_bytes);

        // Pump ModifyLiquidity lands AFTER the drain.
        core.apply_v4_liquidity_update(
            pool_manager,
            pool_id_bytes,
            -60,
            60,
            I256::try_from(500_i128).unwrap(),
            1,
        );

        // Live tick_data CHANGED ...
        let live_gross = {
            let pid = core
                .v4_pool_id_by_key(pool_manager, &pool_id_bytes)
                .expect("registered");
            core.get_v4_pool(pid)
                .and_then(|s| s.tick_data.get(&-60))
                .map(|t| t.liquidity_gross.to::<u128>())
        };
        assert_ne!(
            live_gross,
            Some(gross),
            "pump ModifyLiquidity must mutate the live tick_data (precondition)"
        );

        // ... but the pinned post-drain snapshot is UNCHANGED — step-2 verifies
        // drain-time state, not pump-corrupted current.
        let taken = core.take_v4_post_drain_snapshot(pool_manager, &pool_id_bytes);
        let (tick_data, pinned_block) = taken.expect("Tracked V4 pool pins post-drain");
        assert_eq!(
            tick_data, drain_clone,
            "V4 post-drain pin must be frozen at drain time (step-2 race fix)"
        );
        assert_eq!(
            pinned_block, 0,
            "no buffer events drained → pin's block is the registration update_block (0)"
        );
        assert_eq!(
            core.take_v4_post_drain_snapshot(pool_manager, &pool_id_bytes),
            None,
            "take clears the V4 post-drain slot (verified exactly once)"
        );
    }

    /// `Sparse` V4 pools have no complete `tick_data` to pin — post-drain pin
    /// stays `None` (step-2 verify is a no-op, same as the V4 seed for sparse).
    #[test]
    fn v4_post_drain_snapshot_is_none_for_sparse_pools() {
        use crate::bot_core::{RegisterV4PoolParams, V4PoolKey};
        let pool_manager = Address::from([0x44u8; 20]);
        let pool_id_bytes: degenbot_decoders::v4_swap_decoder::V4PoolId = [0xeeu8; 32];
        let mut core = BotState::new();
        core.register_v4_pool(&RegisterV4PoolParams {
            pool_manager,
            pool_id: pool_id_bytes,
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee: 10_000,
                tick_spacing: 60,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 0,
            tick: 0,
            tick_data: HashMap::new(),
            update_block: 0,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
        })
        .expect("V4 sparse pool registers");
        core.pin_v4_post_drain_snapshot(pool_manager, &pool_id_bytes);
        assert_eq!(
            core.take_v4_post_drain_snapshot(pool_manager, &pool_id_bytes),
            None,
            "sparse V4 pools must not pin post-drain"
        );
    }

    #[test]
    fn unregister_v4_pool_by_tuple_key_discards_buffered_modify_liquidity() {
        use crate::bot_core::{RegisterV4PoolParams, V4PoolKey};
        use crate::solvers::arb_engine::PoolTickCoverage;

        let pool_manager = Address::from([0x44u8; 20]);
        let pool_id_bytes: degenbot_decoders::v4_swap_decoder::V4PoolId = [0xeeu8; 32];
        let mut core = BotState::new();

        let pool_id_u64 = core
            .register_v4_pool(&RegisterV4PoolParams {
                pool_manager,
                pool_id: pool_id_bytes,
                pool_key: V4PoolKey {
                    currency0: Address::ZERO,
                    currency1: Address::from([1u8; 20]),
                    fee: 10_000,
                    tick_spacing: 60,
                    hooks: Address::ZERO,
                },
                hook_flags: 0,
                protocol_fee: 0,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data: HashMap::new(),
                update_block: 0,
                coverage: PoolTickCoverage::Sparse,
                fetcher: None,
            })
            .expect("V4 pool registers");
        assert_eq!(core.pool_count(), 1);

        // Unregister by the V4 tuple key (address = pool_manager).
        let removed = core.unregister_pool(pool_manager, Some(pool_id_bytes));
        assert!(removed, "unregister of a registered V4 pool returns true");
        assert_eq!(core.pool_count(), 0);

        // Re-register must succeed (no Err) and allocate a fresh id.
        let second_id = core
            .register_v4_pool(&RegisterV4PoolParams {
                pool_manager,
                pool_id: pool_id_bytes,
                pool_key: V4PoolKey {
                    currency0: Address::ZERO,
                    currency1: Address::from([1u8; 20]),
                    fee: 10_000,
                    tick_spacing: 60,
                    hooks: Address::ZERO,
                },
                hook_flags: 0,
                protocol_fee: 0,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data: HashMap::new(),
                update_block: 0,
                coverage: PoolTickCoverage::Sparse,
                fetcher: None,
            })
            .expect("V4 re-register after unregister must succeed");
        assert_ne!(
            second_id, pool_id_u64,
            "re-register must allocate a fresh id (retired, not reused)",
        );
    }

    // --- ADR-005 sparse-map parity, slice 2: fetch-callback seam ---

    #[test]
    fn calculate_tokens_out_with_fetch_fills_missing_word_and_retries() {
        // A sparse V3 pool (empty tick_data, start tick 0, word 0 unknown)
        // misses on the starting word. The fetch seam fills the missing word
        // (via a fake fetcher) and retries; the result must match the direct
        // no-miss path (word 0 now known after the fetch merge) and must be
        // non-zero (not the miss sentinel).
        use ::degenbot_pools::tick_fetch::{FetchTickWordError, FetchedTickWord, TickWordFetcher};

        #[derive(Debug)]
        struct FakeFetcher;
        impl TickWordFetcher for FakeFetcher {
            fn fetch_missing_tick_word(
                &self,
                _pool_id: u64,
                word: i32,
                _block: u64,
            ) -> Result<FetchedTickWord, FetchTickWordError> {
                // Mark the word known with no initialized ticks (empty word).
                Ok(FetchedTickWord {
                    word,
                    ticks: HashMap::new(),
                })
            }
        }

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
                tick_data: HashMap::new(), // fully sparse — word 0 unknown
                update_block: 0,
                coverage: PoolTickCoverage::Sparse,
                fetcher: Some(std::sync::Arc::new(FakeFetcher)),
                ..Default::default()
            })
            .expect("test setup: V3 registration");

        // Without the fetch+retry loop: the starting word (0) is unknown →
        // miss → ZERO. Only `MissingTickWord` maps to ZERO here; a
        // `NotComputable` (overflow) must panic rather than be swallowed to
        // ZERO (that swallow was the cdbc03bb bug).
        assert_eq!(
            match core.calculate_tokens_out_miss_aware(pool_id, true, U256::from(1000u64)) {
                Ok(v) => v,
                Err(SimulateSwapError::MissingTickWord(_)) => U256::ZERO,
                Err(SimulateSwapError::NotComputable) => panic!(
                    "sparse V3 swap must miss (MissingTickWord), not overflow (NotComputable)"
                ),
            },
            U256::ZERO,
            "sparse pool with unknown starting word must miss → ZERO without the fetch+retry loop"
        );
        // Miss-aware surfaces the fetchable miss.
        assert_eq!(
            core.calculate_tokens_out_miss_aware(pool_id, true, U256::from(1000u64)),
            Err(SimulateSwapError::MissingTickWord(0)),
            "miss-aware calc must surface MissingTickWord(0), not map it to ZERO"
        );

        // With fetch+retry (stored fetcher): fills word 0 (empty/known) +
        // retries → computes.
        let fetched = core.calculate_tokens_out_with_fetch(pool_id, true, U256::from(1000u64), 0);

        // The fetch+retry result must match the direct no-miss path (word 0
        // now known after the fetch merge — no further miss).
        let direct = core
            .calculate_tokens_out_miss_aware(pool_id, true, U256::from(1000u64))
            .expect(
                "word 0 is known after the fetch merge; the direct path must not miss or overflow",
            );
        assert_eq!(
            fetched, direct,
            "with_fetch result must match the no-miss direct path after the word is filled"
        );
        assert_ne!(
            fetched,
            U256::ZERO,
            "the fetched sparse swap must produce a non-zero amount, not stay at the miss sentinel"
        );
        // The fetch merged word 0 into known_bitmap_words (no further miss).
        let state = core.get_v3_pool(pool_id).expect("pool registered");
        assert!(
            state.known_bitmap_words.contains(&0),
            "fetched word 0 must be marked known"
        );
    }

    #[test]
    fn calculate_tokens_out_with_fetch_fetcher_error_returns_zero() {
        // If the fetcher cannot satisfy the missing word (RPC error / out of
        // range), the calc must give up with `U256::ZERO` rather than panic or
        // spin. Covers the `Err(_)` arm of the fetch+retry loop.
        use ::degenbot_pools::tick_fetch::{FetchTickWordError, FetchedTickWord, TickWordFetcher};

        #[derive(Debug)]
        struct FailingFetcher;
        impl TickWordFetcher for FailingFetcher {
            fn fetch_missing_tick_word(
                &self,
                _pool_id: u64,
                _word: i32,
                _block: u64,
            ) -> Result<FetchedTickWord, FetchTickWordError> {
                Err(FetchTickWordError::FetchFailed)
            }
        }

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
                coverage: PoolTickCoverage::Sparse,
                fetcher: Some(std::sync::Arc::new(FailingFetcher)),
                ..Default::default()
            })
            .expect("test setup: V3 registration");

        assert_eq!(
            core.calculate_tokens_out_with_fetch(pool_id, true, U256::from(1000u64), 0,),
            U256::ZERO,
            "a failing fetcher must give up with ZERO, not panic or spin"
        );
    }

    #[test]
    fn calculate_tokens_out_with_fetch_empty_word_not_refetched() {
        // A fetcher that returns an empty word (checked-but-empty) marks the
        // word known in `known_bitmap_words`. A second solve must NOT re-invoke
        // the fetcher — the empty word survived in the bitmap (ADR-006/005
        // stored-tick-fetcher task MLJT4V). This is the bitmap empty-word fix
        // that lets the companion delete `_bitmap_override`.
        use ::degenbot_pools::tick_fetch::{FetchTickWordError, FetchedTickWord, TickWordFetcher};
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        #[derive(Debug)]
        struct CountingFetcher {
            calls: AtomicU32,
        }
        impl TickWordFetcher for CountingFetcher {
            fn fetch_missing_tick_word(
                &self,
                _pool_id: u64,
                word: i32,
                _block: u64,
            ) -> Result<FetchedTickWord, FetchTickWordError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(FetchedTickWord {
                    word,
                    ticks: HashMap::new(),
                })
            }
        }

        let counter = Arc::new(CountingFetcher {
            calls: AtomicU32::new(0),
        });
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
                coverage: PoolTickCoverage::Sparse,
                fetcher: Some(counter.clone() as Arc<dyn TickWordFetcher>),
                ..Default::default()
            })
            .expect("test setup: V3 registration");

        // First solve: misses word 0, fetches (empty), retries → computes.
        let first = core.calculate_tokens_out_with_fetch(pool_id, true, U256::from(1000u64), 0);
        let calls_after_first = counter.calls.load(Ordering::SeqCst);
        assert!(calls_after_first >= 1, "first solve must fetch word 0");
        assert_eq!(
            first,
            core.calculate_tokens_out_miss_aware(pool_id, true, U256::from(1000u64))
                .expect("word 0 is known after the fetch+retry; the direct path must not miss or overflow"),
            "first fetched result must match the no-miss direct path"
        );

        // Second solve: word 0 is now known → NO fetch should happen.
        let _second = core.calculate_tokens_out_with_fetch(pool_id, true, U256::from(1000u64), 0);
        assert_eq!(
            counter.calls.load(Ordering::SeqCst),
            calls_after_first,
            "second solve must NOT re-invoke the fetcher — the empty word survived in known_bitmap_words"
        );
    }
    /// B3OROH / epic `XEANMB`: `Bot::load_snapshot_from_db` against the parity
    /// fixture DB (`crates/degenbot-db/tests/fixtures/parity.db`) — opens a
    /// `SnapshotDb` (held read tx) + records `S = min(newest_update_block(V3),
    /// V4)` read INSIDE the held tx. The `SnapshotStore` is NOT populated
    /// (the Store is retired by epic `XEANMB`; the held tx replaces it).
    #[test]
    fn load_snapshot_from_db_populates_store_and_seed_block() {
        use degenbot_db::snapshot::TickMapDb;
        use std::path::PathBuf;
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../degenbot-db/tests/fixtures/parity.db");
        if !fixture.exists() {
            // The fixture lives in the sibling crate; skip if absent.
            eprintln!("skipping: parity fixture not at {}", fixture.display());
            return;
        }
        let (snap, _state) = degenbot_db::snapshot_db::SnapshotDb::open(&fixture).unwrap();
        let bot = Bot::new(8453);
        bot.load_snapshot_from_db(&snap, 8453).unwrap();

        let state = bot.state_arc();
        let core = state.read();
        // S = min(newest V3, newest V4). The parity fixture records both; the
        // exact S is whatever the fixture DB carries (we assert it's Some and
        // matches the per-family min computed independently inside the SAME
        // held tx).
        let v3 = snap
            .fetch_newest_update_block(8453, degenbot_db::read::ExchangeFamily::V3)
            .unwrap();
        let v4 = snap
            .fetch_newest_update_block(8453, degenbot_db::read::ExchangeFamily::V4)
            .unwrap();
        let expected_s = match (v3, v4) {
            (Some(a), Some(b)) => Some(u64::try_from(a.min(b)).expect("block number non-negative")),
            (Some(a), None) => Some(u64::try_from(a).expect("block number non-negative")),
            (None, Some(b)) => Some(u64::try_from(b).expect("block number non-negative")),
            (None, None) => None,
        };
        assert_eq!(
            core.snapshot_seed_block(),
            expected_s,
            "snapshot_seed_block must be min(newest_update_block(V3), V4)"
        );
    }
    /// B3OROH: `load_snapshot_from_db` on an empty chain → no snapshot loaded,
    /// S = None (cold-start path: the pump will anchor on `first_observed_block`).
    #[test]
    fn load_snapshot_from_db_empty_chain_is_cold_start() {
        let (snap, _state) = degenbot_db::snapshot_db::SnapshotDb::open_in_memory().unwrap();
        let bot = Bot::new(1);
        bot.load_snapshot_from_db(&snap, 1).unwrap();
        let state = bot.state_arc();
        let core = state.read();
        // No pools → no seed block (cold-start path: the pump anchors on
        // `first_observed_block`).
        assert_eq!(
            core.snapshot_seed_block(),
            None,
            "empty chain → no seed block (cold start)"
        );
    }

    // ── verify-dbg visibility probes (DEGENBOT_VERIFY_DBG) ───────────────────
    //
    // Asserts the WIRING the probes rely on: a tracked V3 pool's pump
    // Mint/Burn is counted by `v3_buffer.pump_count_at_or_below` through the
    // `BotState` field, `advance_pump_complete_cutoff` advances the shared
    // pump-completeness cutoff (the BlockClock tombstone, 3M5PO5), and
    // `pin_v3_post_drain_snapshot` +
    // `set_v3_pool_live` remain behavior-preserving under the buffered
    // tail (the apply path executes regardless of the gate — the
    // `verify_dbg_enabled()` branch is pure logging).
    #[test]
    fn verify_dbg_mark_complete_and_pin_are_behavior_preserving() {
        use crate::bot_core::RegisterV3PoolParams;
        use crate::solvers::arb_engine::PoolTickCoverage;
        let mut core = BotState::new();
        let pool_addr = Address::from([0xf7u8; 20]);
        let _id = core
            .register_v3_pool(&RegisterV3PoolParams {
                address: pool_addr,
                token0: Address::ZERO,
                token1: Address::from([1u8; 20]),
                fee: 500,
                tick_spacing: 10,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data: HashMap::new(),
                update_block: 0,
                coverage: PoolTickCoverage::Tracked,
                fetcher: None,
                ..Default::default()
            })
            .expect("test setup: V3 registration");
        // Quarantine so live Mint/Burn route to the pump buffer (the
        // registration-seam posture during drain+pin+verify).
        core.set_v3_pool_quarantined(pool_addr);
        // Pump-buffer a Mint + a same-block Burn for block 100.
        core.apply_v3_liquidity_update(pool_addr, -10, 10, 500_i128, 100);
        core.apply_v3_liquidity_update(pool_addr, -10, 10, -500_i128, 100);
        // Pre-mark: two pump events, no complete block yet.
        assert_eq!(core.v3_buffer.pump_count_at_or_below(&pool_addr, 100), 2);
        assert_eq!(core.pump_complete_cutoff(), 0);
        assert_eq!(core.v3_buffer.pump_total_at_or_below(100), 2);
        // Mark block 100 complete (what the pump does at N+1's tombstone).
        core.advance_pump_complete_cutoff(100);
        assert_eq!(core.pump_complete_cutoff(), 100);
        // Drain + pin: the gated drain yields both events, then the pin
        // captures the post-drain pair. (apply_pump_buffer_v3 + pin are the
        // exact sequence the registration seam runs.)
        core.apply_pump_buffer_v3(&pool_addr);
        core.pin_v3_post_drain_snapshot(pool_addr);
        let (pinned_ticks, pinned_block) = core
            .take_v3_post_drain_snapshot(pool_addr)
            .expect("pin was captured for a Tracked pool");
        assert_eq!(pinned_block, 100, "pin captured the drained update_block");
        // The net-zero Mint+Burn leaves gross=0 on the boundary ticks and
        // creates NO initialized tick (a zero gross is pruned) → the pin carries
        // an empty map. The point: the pin pair is self-consistent with the
        // drain the probe correlates.
        assert_eq!(pinned_ticks.len(), 0, "net-zero Mint+Burn yields no tick");
        // set_live under an empty retained tail is a no-op flush (the probe
        // would log drained_retained_tail=0).
        core.set_v3_pool_live(pool_addr);
    }
}
