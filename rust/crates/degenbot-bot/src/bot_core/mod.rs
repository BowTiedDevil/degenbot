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
pub mod balance_vector_orchestration;
pub mod balancer_stable_state;
pub mod balancer_weighted_state;
pub mod block_clock;
pub mod block_clock_pipe;
pub mod block_pump;
pub mod bot;
pub mod cl_orchestration;
pub mod cl_route;
pub mod construction_io;
pub mod cpu_budget;
pub mod curve_data_provider_impl;
pub mod curve_state;
pub mod divergence_probe;
pub mod drain_sink;
pub mod engine;
pub mod event_dispatch;
pub mod liquidity_verifier;
pub mod log_dispatcher;
pub mod pool_builder;
pub mod pump_fsm;
pub mod pump_telemetry;
pub mod registration_lifecycle;
pub mod reorg_coordinator;
pub mod reserve_pair_orchestration;
pub(crate) mod resolve;
pub mod sim_anchor;
pub mod snapshot_verify;
pub(crate) mod solve_anchor;
pub mod solve_coordinator;
pub mod solver_state_tripwire;
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
    PoolTickCoverage, RegisterV3PoolError, RegisterV3PoolParams, RegistrationLifecycle,
    SimulateSwapError, V3PoolIdentity, V3PoolState, V3SwapOutcome, V3SwapUpdate,
};
pub use balancer_stable_state::{
    BalancerStablePoolIdentity, BalancerStablePoolState, RegisterBalancerStablePoolParams,
};
pub use balancer_weighted_state::{
    BalancerWeightedPoolIdentity, BalancerWeightedPoolState, RegisterBalancerWeightedPoolParams,
};
pub use block_clock_pipe::BlockNotification;
pub use cl_orchestration::{InstallWordOutcome, RegisteredV4, StagedWordFetch};
pub use curve_state::{CurvePoolIdentity, CurvePoolState, RegisterCurvePoolParams};
use degenbot_math::curve::{CurveBasePoolPort, CurveSwapError};
pub use divergence_probe::{TrackedSlotKind, TrackedSlotProbe};
pub use registration_lifecycle::{
    run_cl_v3_lifecycle, run_cl_v4_lifecycle, run_v3_registration_lifecycle,
    run_v4_registration_lifecycle, RegistrationLifecycleError,
};
pub use sim_anchor::SimAnchorState;

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
    /// The highest FULLY-DELIVERED block — the delivery cutoff (last complete
    /// block, 3M5PO5). The registration drain reads this as the
    /// `drain_pump_completed` cutoff instead of a buffer-local shadow marker;
    /// `0` means no block has been tombstoned → nothing drains. Owned here as
    /// a plain monotone value that outlives pump runs (BGEDB6): the pump
    /// driver advances it on the tombstone verdict, and a resume never resets
    /// it.
    pump_complete_cutoff: u64,
    /// Per-pool event-witnessed horizon (FUWYUR clock-provenance): the
    /// highest block of any V3/V4 event ROUTED for this pool (applied
    /// directly OR staged into a buffer). Advanced ONLY by routed events —
    /// never by imported DB-row stamps — so it corroborates (or refutes) a
    /// pin's freshness claim independently of the seed. Keyed like the
    /// family buffers: address for V3, `(pool_manager, pool_id)` for V4.
    v3_event_horizons: HashMap<Address, u64>,
    v4_event_horizons: HashMap<(Address, degenbot_decoders::v4_swap_decoder::V4PoolId), u64>,
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
/// `42FL35`: V4-aware `DRAIN_DBG` match. For V4, `log.address()` is the shared
/// `PoolManager` contract - every V4 pool carries it, so an address-shape match
/// cannot attribute a Swap to a specific pool. The `PoolId` lives in the event's
/// indexed topics (`topics[1]` for V4 Swap/ModifyLiquidity). This matcher
/// accepts EITHER shape: the env value matches the address, or it matches any
/// indexed topic (`PoolId` hex). Zero cost when the env is unset.
fn drain_dbg_match_v4(address: Address, topics: &[alloy::primitives::B256]) -> bool {
    let Ok(env) = std::env::var("DEGENBOT_DRAIN_DBG") else {
        return false;
    };
    let want = env.trim_start_matches("0x");
    if format!("{address:x}").eq_ignore_ascii_case(want) {
        return true;
    }
    topics
        .iter()
        .skip(1)
        .any(|t| format!("{t:x}").eq_ignore_ascii_case(want))
}

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

/// Whether the global WS-log pipeline trace is on (env
/// `DEGENBOT_WS_TRACE=1`). When set, `[trace] ws-log` fires for EVERY
/// relevant-topic log the live pump dispatches — the catch-all companion to
/// the per-pool `DEGENBOT_DRAIN_DBG` and the liquidity-only
/// `DEGENBOT_TRACE_LIQUIDITY` gates. High volume by design (one line per
/// relevant WS log); opt-in for desync investigations that cannot be
/// pinned to a single pool in advance.
pub(crate) fn trace_ws_global() -> bool {
    std::env::var("DEGENBOT_WS_TRACE")
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

/// The base-pool delegation port for metapool `get_dy_underlying` (task
/// `V5X2YP`). Implements [`CurveBasePoolPort`] by delegating each op to a
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
/// a liquidity-mutating one (V3 Mint/Burn, V4 `ModifyLiquidity`), or the
/// global `DEGENBOT_WS_TRACE` catch-all is on (every relevant-topic log).
pub(crate) fn trace_ws_log_dispatch(
    address: Address,
    topics: &[alloy::primitives::B256],
    block_number: u64,
    log_index: Option<u64>,
    tx_index: Option<u64>,
    removed: bool,
    decision: &str,
) {
    use degenbot_decoders::v3_mint_burn_decoder::{V3_BURN_TOPIC, V3_MINT_TOPIC};
    use degenbot_decoders::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC;
    let first_topic = topics
        .first()
        .copied()
        .unwrap_or(alloy::primitives::B256::ZERO);
    let is_liquidity = first_topic == V3_MINT_TOPIC
        || first_topic == V3_BURN_TOPIC
        || first_topic == V4_MODIFY_LIQUIDITY_TOPIC;
    // 42FL35: V4-aware match - for V4 events the address is the shared
    // PoolManager, so attribution requires the indexed PoolId in topics[1].
    let pool_match = drain_dbg_match_v4(address, topics);
    let global_liquidity_hit = trace_liquidity_global() && is_liquidity;
    let global_ws_hit = trace_ws_global();
    if !pool_match && !global_liquidity_hit && !global_ws_hit {
        return;
    }
    tracing::info!(
        pool_addr = %format!("{address:x}"),
        block = block_number,
        log_index = ?log_index,
        tx_index = ?tx_index,
        topic0 = %first_topic, // full topic — greppable by short prefix
        topic1 = ?topics.get(1), // 42FL35: V4 PoolId lives here - greppable
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
#[expect(clippy::too_many_arguments)]
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

/// Parse a flag's env value against the conservative default: `false` only for
/// an explicit falsey value (`""`, `0`, `false`, `off`, `no`); `true`
/// otherwise. Pure so it is unit-testable without process-global env mutation.
pub(crate) fn parse_bot_flag_value(v: &str) -> bool {
    !matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "" | "0" | "false" | "off" | "no" | "n"
    )
}

/// Conservative-default environment flag (Z4KQXF): `true` unless `name` is set
/// to an explicit falsey value (`""`, `0`, `false`, `off`, `no`). Default-on so
/// a hand-run or harness never silently drops failure visibility — the HARD/
/// LOUD posture is the default; disable explicitly with, e.g. `X=0`.
pub(crate) fn bot_env_flag_default_on(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => parse_bot_flag_value(&v),
        Err(_) => true,
    }
}

/// default-off environment flag (the inverse of the default-on variant above):
/// true only when the named var is set to an explicit truthy value
/// (per `parse_bot_flag_value`). For opt-in diagnostics that a hand-run or
/// harness must never silently activate — unset/false env means the
/// diagnostic is absent, at zero cost.
pub(crate) fn bot_env_flag_default_off(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => parse_bot_flag_value(&v),
        Err(_) => false,
    }
}

/// Whether the verify-diagnostics probes are enabled.
///
/// Conservative default ON (`DEGENBOT_VERIFY_DBG`, via [`bot_env_flag_default_on`]);
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
fn verify_dbg_enabled() -> bool {
    bot_env_flag_default_on("DEGENBOT_VERIFY_DBG")
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
    /// `Swap` / Mint-Burn event. `0` for an unregistered pool (never advanced).
    ///
    /// `update_block` is a last-activity clock, NOT a staleness signal: a pool
    /// that last mutated N blocks ago is quiet (its stored state is byte-
    /// identical to on-chain), not stale. The former TQ43TU solve-time staleness
    /// gate (ergo YXHHKR, resolved QNFYR5) mis-used it to defer quiet paths and
    /// was REMOVED. The tripwire (`solver_state_tripwire::judge`) diffs each
    /// hop against the chain at its OWN `update_block` anchor and fatal-aborts on
    /// a real desync; a never-updated pool (`update_block == 0`) is diffed at the
    /// solve block instead.
    #[must_use]
    pub fn pool_update_block(&self, pool_id: u64) -> u64 {
        self.pools.get(&pool_id).map_or(0, PoolEntry::update_block)
    }

    /// The pool-state **price clock head**: the maximum `update_block` across
    /// every registered pool (V2/V3/V4), `0` when none are registered.
    ///
    /// This is the block the live pool state actually reflects. During a
    /// backfill/drain desync the pools are advanced ahead of the pump's
    /// header clock, so `pool_state_head()` can exceed the drain `block_number`
    /// — the correct solve/verify/sim anchor is this head, NOT the lagging
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

    /// The pool's **liquidity** clock (`tick_data_block`, two-stamp OB7UNY) —
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
            snapshot_seed_block: None,
            pump_complete_cutoff: 0,
            v3_event_horizons: HashMap::new(),
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
    /// (BGEDB6); tests that drive the registration drain without a pump use
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

    /// Registration/seed genesis anchor for a registered V3/V4 pool
    /// (two-stamp OB7UNY): pushes a `before == after` journal delta at `block`
    /// so the reorg journal is non-empty from registration, WITHOUT advancing
    /// either clock. The split-seed replacement for the builder's old
    /// `apply_swap` genesis, which would backward-panic `update_block` (price
    /// seeded at HEAD past the DB map block) and falsely advance
    /// `tick_data_block`. V2/unregistered → `None`.
    pub fn seed_genesis_by_pool_id(&mut self, pool_id: u64, block: u64) -> Option<u64> {
        match self.pools.get_mut(&pool_id) {
            Some(PoolEntry::V3(_, state)) => {
                state.seed_genesis(block);
                Some(pool_id)
            }
            Some(PoolEntry::V4(_, state)) => {
                state.seed_genesis(block);
                Some(pool_id)
            }
            _ => None,
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
    #[expect(clippy::too_many_lines)]
    pub fn process_backfill_logs(&mut self, logs: &[alloy::rpc::types::Log], chunk_end: u64) {
        use degenbot_decoders::v3_mint_burn_decoder::{decode_v3_burn_log, decode_v3_mint_log};
        use degenbot_decoders::v3_pancakeswap_swap_decoder::decode_v3_pancakeswap_swap_log;
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
            // V3 events route through the SINGLE routing table (cl_route)
            // at Phase::Backfill — no per-site policy copies. The table's rows
            // reproduce the historical behavior exactly (unregistered scalar
            // refresh drops rely on the row re-seed; liquidity always stages).
            if *topic0 == degenbot_decoders::v3_swap_decoder::V3_SWAP_TOPIC {
                if let Some(event) = decode_v3_swap_log(log) {
                    self.route_v3_event(
                        crate::bot_core::cl_route::Phase::Backfill,
                        event.pool_address,
                        BufferedV3PoolEvent::Swap(BufferedV3SwapEvent {
                            sqrt_price_x96: event.sqrt_price_x96,
                            liquidity: event.liquidity.to::<u128>(),
                            tick: event.tick,
                            block_number: log_block,
                        }),
                        &[],
                    );
                    v3_touched = true;
                }
            } else if *topic0
                == degenbot_decoders::v3_pancakeswap_swap_decoder::V3_PANCAKESWAP_SWAP_TOPIC
            {
                // PancakeSwap V3 swaps carry a non-canonical topic0 (the fork
                // added two trailing data fields to the Swap event) — decode
                // them via the dedicated decoder so these pools stay live. See
                // docs/exploration-no-profit-crash.md (stale-state root cause).
                if let Some(event) = decode_v3_pancakeswap_swap_log(log) {
                    self.route_v3_event(
                        crate::bot_core::cl_route::Phase::Backfill,
                        event.pool_address,
                        BufferedV3PoolEvent::Swap(BufferedV3SwapEvent {
                            sqrt_price_x96: event.sqrt_price_x96,
                            liquidity: event.liquidity.to::<u128>(),
                            tick: event.tick,
                            block_number: log_block,
                        }),
                        &[],
                    );
                    v3_touched = true;
                }
            } else if *topic0 == degenbot_decoders::v3_mint_burn_decoder::V3_MINT_TOPIC {
                if let Some(event) = decode_v3_mint_log(log) {
                    self.route_v3_event(
                        crate::bot_core::cl_route::Phase::Backfill,
                        event.pool_address,
                        BufferedV3PoolEvent::Liquidity(BufferedV3LiquidityUpdate {
                            tick_lower: event.tick_lower,
                            tick_upper: event.tick_upper,
                            liquidity_delta: event.amount.cast_signed(),
                            block_number: log_block,
                        }),
                        &[],
                    );
                    v3_touched = true;
                }
            } else if *topic0 == degenbot_decoders::v3_mint_burn_decoder::V3_BURN_TOPIC {
                if let Some(event) = decode_v3_burn_log(log) {
                    self.route_v3_event(
                        crate::bot_core::cl_route::Phase::Backfill,
                        event.pool_address,
                        BufferedV3PoolEvent::Liquidity(BufferedV3LiquidityUpdate {
                            tick_lower: event.tick_lower,
                            tick_upper: event.tick_upper,
                            liquidity_delta: -(event.amount.cast_signed()),
                            block_number: log_block,
                        }),
                        &[],
                    );
                    v3_touched = true;
                }
            } else if *topic0 == degenbot_decoders::v4_swap_decoder::V4_SWAP_TOPIC {
                if let Some(event) = decode_v4_swap_log(log) {
                    self.route_v4_event(
                        crate::bot_core::cl_route::Phase::Backfill,
                        log.address(),
                        event.pool_id,
                        BufferedV4PoolEvent::Swap(BufferedV4SwapEvent {
                            sqrt_price_x96: event.sqrt_price_x96,
                            liquidity: event.liquidity.to::<u128>(),
                            tick: event.tick,
                            block_number: log_block,
                        }),
                        &[],
                    );
                    v4_touched = true;
                }
            } else if *topic0
                == degenbot_decoders::v4_modify_liquidity_decoder::V4_MODIFY_LIQUIDITY_TOPIC
            {
                if let Some(event) = decode_v4_modify_liquidity_log(log) {
                    self.route_v4_event(
                        crate::bot_core::cl_route::Phase::Backfill,
                        log.address(),
                        event.pool_id,
                        BufferedV4PoolEvent::Liquidity(BufferedV4LiquidityUpdate {
                            tick_lower: event.tick_lower,
                            tick_upper: event.tick_upper,
                            liquidity_delta: event.liquidity_delta,
                            block_number: log_block,
                        }),
                        &[],
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

#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot_core::swap_simulation::{Caveats, SwapOutcome, SwapRead, SwapRequest};
    use alloy::primitives::aliases::U112;
    use alloy::primitives::uint;
    use alloy::primitives::I256;

    /// Exact-input read through the swap-simulation gate (ADR-037) — the
    /// replacement for the deleted `calculate_tokens_out_miss_aware` seam.
    fn tokens_out(core: &mut BotState, pool_id: u64, zero_for_one: bool, amount_in: U256) -> U256 {
        match core.swap_simulation(
            0,
            pool_id,
            SwapRequest {
                zero_for_one,
                amount_specified: -I256::try_from(amount_in).unwrap(),
                sqrt_price_limit: None,
            },
        ) {
            SwapRead::Computed(outcome) => outcome.delivered_unsigned(),
            f => panic!("small non-overflowing V2 amount; calc must not miss or overflow: {f:?}"),
        }
    }

    const FEE_03: (u64, u64) = (997, 1000);

    #[test]
    fn conservative_bot_flag_default_on() {
        // Conservative default (Z4KQXF): unset ⇒ enabled (HARD/LOUD). This var
        // is not set by any test, so the default-on path is deterministic.
        assert!(bot_env_flag_default_on("DEGENBOT_UNUSED_TEST_FLAG"));
        // Explicit falsey values opt OUT.
        assert!(!parse_bot_flag_value("0"));
        assert!(!parse_bot_flag_value("false"));
        assert!(!parse_bot_flag_value("off"));
        assert!(!parse_bot_flag_value("no"));
        assert!(!parse_bot_flag_value("n"));
        assert!(!parse_bot_flag_value(""));
        // Everything else stays enabled.
        assert!(parse_bot_flag_value("1"));
        assert!(parse_bot_flag_value("true"));
        assert!(parse_bot_flag_value("on"));
        assert!(parse_bot_flag_value("yes"));
    }

    fn make_pool_addr() -> Address {
        Address::from([0xaa; 20])
    }

    /// FUWYUR router contract (`cl_route)`: `route_v3_event` is THE decision
    /// point — a live-phase tick mutation for an unregistered pool must
    /// stage into the pump buffer (not drop), and the buffered event lands
    /// in `v3_buffer` keyed by address for the registration drain+pin seam.
    #[test]
    fn fuwyur_router_stages_unregistered_live_liquidity_into_pump_buffer() {
        use crate::bot_core::cl_route::{ApplyOutcome, BufferKind};
        let mut core = BotState::new();
        let addr = Address::from([0x66; 20]);
        let outcome = core.route_v3_event(
            crate::bot_core::cl_route::Phase::Live,
            addr,
            BufferedV3PoolEvent::Liquidity(BufferedV3LiquidityUpdate {
                tick_lower: -100,
                tick_upper: 7,
                liquidity_delta: 118_748_558_607_688,
                block_number: 10,
            }),
            &[],
        );
        assert_eq!(outcome, ApplyOutcome::Buffered(BufferKind::Pump));
        assert_eq!(core.buffered_v3_event_count(&addr), 1);
        // 7HUYWM: a buffered event is engine-witnessed activity — the
        // event horizon advances at ARRIVAL time (parity with V4) so the
        // pin's stamp-provenance verdict sees the true witnessed span after
        // the staged drain, not just ApplyDirect-routed events.
        assert_eq!(core.v3_event_horizon(&addr), 10);
    }

    /// 7HUYWM: the event horizon tracks the MAX block across multiple buffered
    /// events for the same (still-unregistered) pool. The pin's
    /// `SeedTrustOnly{witnessed_horizon>0}` classification (the re-seed-after-
    /// activity tripwire) depends on this being the true high-water mark.
    #[test]
    fn v3_event_horizon_tracks_max_block_across_buffered_events() {
        let mut core = BotState::new();
        let addr = Address::from([0x67; 20]);
        let mk = |block, delta| {
            BufferedV3PoolEvent::Liquidity(BufferedV3LiquidityUpdate {
                tick_lower: -100,
                tick_upper: 7,
                liquidity_delta: delta,
                block_number: block,
            })
        };
        core.route_v3_event(crate::bot_core::cl_route::Phase::Live, addr, mk(10, 1), &[]);
        core.route_v3_event(crate::bot_core::cl_route::Phase::Live, addr, mk(50, 2), &[]);
        core.route_v3_event(crate::bot_core::cl_route::Phase::Live, addr, mk(30, 3), &[]);
        assert_eq!(core.buffered_v3_event_count(&addr), 3);
        assert_eq!(core.v3_event_horizon(&addr), 50);
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
    fn pool_state_head_is_max_update_block_across_all_pools() {
        // The solve/verify/sim anchor. During a backfill/drain desync the
        // pools are advanced ahead of the pump's header clock, so
        // `pool_state_head()` (max `update_block` across every pool) can
        // exceed the drain `block_number` — that head is the block the live
        // state reflects, and the correct anchor. Unchanged pools have
        // byte-identical EVM state from `update_block` to head, so one head
        // anchor reproduces each path's solver state (B2 collapse).
        let mut core = BotState::new();
        assert_eq!(core.pool_state_head(), 0, "empty state head is 0");

        // Register + advance two pools at different blocks; the head is the max.
        let a = core
            .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
            .expect("setup: pool A");
        core.apply_v2_sync(make_pool_addr(), U112::from(900), U112::from(2222), 5)
            .expect("forward Sync at block 5 applies");
        // Pool B on a distinct address (shared tokens): its update_block 12
        // overtakes pool A's 5, so the head tracks B.
        let b_addr = Address::from([0x12u8; 20]);
        let b = core
            .register_v2_pool(&RegisterV2PoolParams {
                address: b_addr,
                ..make_params(U112::from(3000), U112::from(4000))
            })
            .expect("setup: pool B");
        let _ = b;
        core.apply_v2_sync(b_addr, U112::from(3300), U112::from(4400), 12)
            .expect("forward Sync at block 12 applies");
        assert_eq!(core.pool_update_block(a), 5);
        assert_eq!(core.pool_state_head(), 12, "head = max update_block");

        // De-registering the leading pool (B, at 12) drops the head back to A's 5.
        let _ = a;
        core.unregister_pool(b_addr, None);
        assert_eq!(
            core.pool_state_head(),
            5,
            "head falls back to the next-highest update_block"
        );
    }

    #[test]
    fn pool_tick_data_block_exposes_staged_liquidity_clock() {
        // OB7UNY two-stamp / the `0x5653` staged-clock class: a CL pool whose
        // PRICE clock (`update_block`) is fresh but whose LIQUIDITY clock
        // (`tick_data_block`) lags. The scalar-only ADR-021 diff keys on
        // `update_block` and therefore cannot see this stagger; the new
        // `pool_tick_data_block` accessor makes it observable so a tick-map
        // consumer can key on the right clock.
        let mut core = BotState::new();
        let pool_addr = Address::from([0xabu8; 20]);
        let pool_id = register_v3_on_core(&mut core, pool_addr, 100);
        assert_eq!(
            core.pool_update_block(pool_id),
            100,
            "registered pool: price clock at seed block"
        );
        assert_eq!(
            core.pool_tick_data_block(pool_id),
            100,
            "registered pool: liquidity clock at seed block"
        );
        // Simulate a buggy scalar-only advance that moves the price clock
        // without touching the tick map (direct poke — the two-stamp mutators
        // keep them in lockstep, which is exactly why this class only arises
        // from a bug / non-CL-advancing path).
        if let Some(crate::bot_core::PoolEntry::V3(_, state)) = core.pools.get_mut(&pool_id) {
            state.update_block = 200;
        }
        assert_eq!(core.pool_update_block(pool_id), 200, "price clock advanced");
        assert_eq!(
            core.pool_tick_data_block(pool_id),
            100,
            "liquidity clock still lags → the stagger is observable"
        );
        // Non-CL families fall back to `update_block` for the total accessor.
        let v2_id = core
            .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
            .expect("test setup: V2 registration");
        assert_eq!(core.pool_tick_data_block(v2_id), 0);
        assert_eq!(
            core.pool_tick_data_block(999_999),
            0,
            "unknown id → stale sentinel"
        );
    }

    #[test]
    fn v3_split_clock_seed_prices_at_head_ticks_at_db_block() {
        // OB7UNY two-stamp / the fresh-read builder: seed the PRICE clock at
        // HEAD (`update_block` — a cheap slot0 read) while the LIQUIDITY
        // clock stays at the DB liquidity snapshot block (`tick_data_block`).
        // The historical-replay guard must key on the PRICE seed block
        // (`initial_state_block == update_block`): the head-seeded slot0
        // `liquidity` scalar already reflects every in-range event up to head,
        // so a backfilled in-range replay below head must not adjust it.
        use crate::bot_core::{RegisterV3PoolParams, TickInfo};
        use crate::solvers::arb_engine::PoolTickCoverage;
        let mut core = BotState::new();
        let pool_addr = Address::from([0xccu8; 20]);
        let head = 1_000u64;
        let db_block = 950u64;
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: alloy::primitives::U128::from(100),
                liquidity_net: I256::try_from(100i128).unwrap(),
                block: 0,
            },
        );
        let pool_id = core
            .register_v3_pool(&RegisterV3PoolParams {
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
                update_block: head,
                tick_data_block: Some(db_block),
                coverage: PoolTickCoverage::Sparse,
                fetcher: None,
                ..Default::default()
            })
            .expect("test setup: V3 split-clock registration");
        let s = core.get_v3_pool(pool_id).expect("registered");
        assert_eq!(
            s.update_block, head,
            "price clock stamped at HEAD (fresh slot0 read)"
        );
        assert_eq!(
            s.tick_data_block, db_block,
            "liquidity clock anchored at the DB snapshot block"
        );
        assert_eq!(
            s.initial_state_block, head,
            "replay guard keys on the PRICE seed block (head-seeded scalar)"
        );
        assert_eq!(core.pool_update_block(pool_id), head);
        assert_eq!(core.pool_tick_data_block(pool_id), db_block);
    }

    #[test]
    fn seed_genesis_anchors_journal_without_advancing_clocks() {
        // The split-seed builder (price at HEAD, tick map at the DB block)
        // replaces its old `apply_swap` genesis with `seed_genesis`: a
        // `before == after` journal delta that makes the journal non-empty
        // (so a mid-window reorg restores instead of a graceful
        // `NoStatePriorToBlock` shutdown) WITHOUT advancing either clock
        // (two-stamp OB7UNY).
        use crate::bot_core::{RegisterV3PoolParams, TickInfo};
        use crate::solvers::arb_engine::PoolTickCoverage;
        let mut core = BotState::new();
        let pool_addr = Address::from([0xe1u8; 20]);
        let head = 1_000u64;
        let db_block = 950u64;
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: alloy::primitives::U128::from(100),
                liquidity_net: I256::try_from(100i128).unwrap(),
                block: 0,
            },
        );
        let pool_id = core
            .register_v3_pool(&RegisterV3PoolParams {
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
                update_block: head,
                tick_data_block: Some(db_block),
                coverage: PoolTickCoverage::Sparse,
                fetcher: None,
                ..Default::default()
            })
            .expect("test setup: V3 split-clock registration");
        // Empty journal → not restorable (would be a graceful too-deep shutdown).
        assert!(!core.has_state_prior_to(pool_id, 100));
        // Seed genesis at the DB floor anchor.
        assert_eq!(
            core.seed_genesis_by_pool_id(pool_id, db_block),
            Some(pool_id)
        );
        assert!(core.has_state_prior_to(pool_id, 100), "non-empty journal");
        // The anchor advances NO clock.
        assert_eq!(core.pool_update_block(pool_id), head);
        assert_eq!(core.pool_tick_data_block(pool_id), db_block);
        // Restore below the anchor pops the before==after delta → seeded state.
        let restored = core.restore_pool_before_block(pool_id, 100);
        assert!(restored.unwrap().is_ok());
        assert_eq!(core.pool_update_block(pool_id), head);
        assert_eq!(core.pool_tick_data_block(pool_id), db_block);
        assert_eq!(core.seed_genesis_by_pool_id(999_999, 1), None);
    }

    #[test]
    fn register_v2_pool_and_calculate_tokens_out() {
        let mut core = BotState::new();
        let pool_id = core
            .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
            .expect("test setup: V2 registration");

        // Python reference: constant_product_calc_exact_in(100, 1000, 2000, 3/1000) = 181
        let amount_out = tokens_out(&mut core, pool_id, true, U256::from(100));
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
    fn curve_get_dy_runs_the_rust_owned_swap_path() {
        // Task `45QBUG`: the Rust-owned `get_dy` entry replays the shared
        // `standard_plain` fixture and reproduces the recorded dy — proving
        // the whole swap path (orchestration + calc) runs with no Python
        // provider / cache / calculator.
        use crate::bot_core::RegisterCurvePoolParams;

        const E18: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);
        const E21: U256 = U256::from_limbs([11_627_460_059_052_638_208, 162, 0, 0]); // 3e21
        const TWO_E21: U256 = U256::from_limbs([4_808_176_044_395_724_800, 325, 0, 0]); // 6e21

        let mut core = BotState::new();
        let pool_id = core.register_curve_pool(&RegisterCurvePoolParams {
            address: Address::from([0xccu8; 20]),
            tokens: vec![Address::ZERO, Address::from([0x01u8; 20])],
            a_coefficient: 100,
            a_precision: 100,
            fee: 500_000,
            admin_fee: 0,
            rate_multipliers: vec![E18, E18],
            balances: vec![E21, TWO_E21],
            update_block: 0,
            swap_style: 1,         // STANDARD
            lending_rate_style: 1, // NONE
            d_variant: 1,
            y_variant: 1,
            yd_variant: 1,
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
            use_lending: Vec::new(),
            precision_multipliers: vec![E18, E18],
            tokens_underlying: None,
            metapool_rate_style: 1,
            metapool_underlying_style: 1,
            data_provider: None,
        });

        let dy = core
            .curve_get_dy(pool_id, 0, 1, E18, 0, None)
            .expect("standard curve get_dy");
        assert_eq!(dy, U256::from(1_008_296_947_143_911_861u64));

        // Unknown pool id -> UnknownPool error.
        assert!(matches!(
            core.curve_get_dy(999_999, 0, 1, E18, 0, None),
            Err(CurveInputsError::UnknownPool(999_999))
        ));
    }

    #[test]
    #[expect(clippy::too_many_lines)]
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
                tick_data_block: None,
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
            token0_decimals: 18,
            token1_decimals: 18,
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
            token0_decimals: 18,
            token1_decimals: 18,
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
        let amount_out = tokens_out(&mut core, pool_id, false, U256::from(100));
        assert_eq!(amount_out, U256::from(181));
    }

    #[test]
    fn update_v2_pool_changes_calculation_result() {
        let mut core = BotState::new();
        let pool_id = core
            .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
            .expect("test setup: V2 registration");

        // Before update: swap 100 token0 → 181 token1
        let before = tokens_out(&mut core, pool_id, true, U256::from(100));
        assert_eq!(before, U256::from(181));

        // Update reserves: now reserve0=2000, reserve1=1000
        core.update_v2_pool(make_pool_addr(), U112::from(2000), U112::from(1000), 42);

        // After update: Python: constant_product_calc_exact_in(100, 2000, 1000, 3/1000) = 47
        let after = tokens_out(&mut core, pool_id, true, U256::from(100));
        assert_eq!(after, U256::from(47));
    }

    /// Required-input read through the swap-simulation gate (ADR-037) — the
    /// replacement for the deleted `calculate_tokens_in` seam. Exact-output
    /// request (positive user-perspective); the required input is the
    /// magnitude of the consumed delta.
    fn tokens_in(core: &mut BotState, pool_id: u64, zero_for_one: bool, amount_out: U256) -> U256 {
        match core.swap_simulation(
            0,
            pool_id,
            SwapRequest {
                zero_for_one,
                amount_specified: I256::try_from(amount_out).unwrap(),
                sqrt_price_limit: None,
            },
        ) {
            SwapRead::Computed(outcome) => (-match &outcome {
                SwapOutcome::V2(o) => o.consumed,
                SwapOutcome::V3(o) | SwapOutcome::V4(o) => o.consumed,
            })
            .into_raw(),
            f => panic!("exact-out calc must not fail on this fixture: {f:?}"),
        }
    }

    #[test]
    fn calculate_tokens_in_for_v2_pool() {
        let mut core = BotState::new();
        let pool_id = core
            .register_v2_pool(&make_params(U112::from(1000), U112::from(2000)))
            .expect("test setup: V2 registration");

        // Python: constant_product_calc_exact_out(50, 1000, 2000, 3/1000) = 26
        let amount_in = tokens_in(&mut core, pool_id, true, U256::from(50));
        assert_eq!(amount_in, U256::from(26));

        // Reverse: Python: constant_product_calc_exact_out(10, 2000, 1000, 3/1000) = 21
        let amount_in_rev = tokens_in(&mut core, pool_id, false, U256::from(10));
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
        let amount_out = tokens_out(&mut core, pool_id, true, amount_in);
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
            tick_data_block: None,
            coverage: PoolTickCoverage::Sparse,
            fetcher: None,
            ..Default::default()
        })
        .expect("test setup: V3 registration")
    }

    /// Bug-A regression (path-142603): a **backfill→Live** in-range
    /// `ModifyLiquidity` with block > seed must adjust the in-range `liquidity()`
    /// scalar. Pre-fix the backfill→Live branch applied the tick map via the
    /// low-level `apply_liquidity_to_tick_range` and NEVER adjusted the scalar,
    /// producing the staged-clock desync (fresh tick map, stale in-range
    /// liquidity). Now it routes through the shared, in-range-aware
    /// `apply_liquidity_update`. (Sparse → Live registration per DFQYM5.)
    #[test]
    fn backfill_live_in_range_post_seed_adjusts_in_range_liquidity() {
        let mut core = BotState::new();
        let pool_id = register_v3(&mut core, 5); // seed block 5, liq 0, tick 0 (Sparse -> Live)
                                                 // Post-seed in-range Mint (tick 0 in [-60,60), block 6 > seed 5):
        core.buffer_backfill_v3_liquidity_update(make_pool_addr(), -60, 60, 123_456_i128, 6);
        let Some(PoolEntry::V3(_, state)) = core.pools.get(&pool_id) else {
            panic!("pool missing")
        };
        assert_eq!(
            state.liquidity, 123_456,
            "in-range post-seed Mint must adjust the active scalar"
        );
        assert_eq!(state.tick_data_block, 6, "liquidity clock advanced");
        assert_eq!(
            state.update_block, 6,
            "price clock advanced (in-range post-seed)"
        );
        assert!(
            state.tick_data.contains_key(&-60) && state.tick_data.contains_key(&60),
            "both boundary ticks mutated"
        );
    }

    /// Bug-A companion: a backfill→Live in-range event at/before the seed block
    /// is a historical replay — the seed's `liquidity` already reflects every
    /// on-chain event <= seed, so the tick map mutates but the scalar must NOT
    /// change (the guard in the shared `apply_liquidity_update` survives the
    /// unification).
    #[test]
    fn backfill_live_in_range_pre_seed_does_not_adjust_scalar() {
        let mut core = BotState::new();
        let pool_id = register_v3(&mut core, 5); // seed block 5
                                                 // Pre-seed in-range Mint (block 4 <= seed 5):
        core.buffer_backfill_v3_liquidity_update(make_pool_addr(), -60, 60, 250_000_i128, 4);
        let Some(PoolEntry::V3(_, state)) = core.pools.get(&pool_id) else {
            panic!("pool missing")
        };
        assert_eq!(
            state.liquidity, 0,
            "pre-seed replay must NOT adjust the scalar"
        );
        assert!(
            state.tick_data.contains_key(&-60),
            "pre-seed replay still mutates the tick map"
        );
        assert_eq!(
            state.tick_data_block, 5,
            "pre-seed replay is a monotonic no-op: clock stays at the seed block (5 > 4)"
        );
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
            tick_data_block: None,
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
            // OB7UNY two-stamp: a Mint mutates the TICK MAP (liquidity clock
            // advances) but, being out of range, leaves the slot0 head
            // untouched (price clock stays at registration block 0).
            assert_eq!(
                s.tick_data_block, block_b,
                "the liquidity clock advances to the buffered event's block"
            );
            assert_eq!(
                s.update_block, 0,
                "the price clock is untouched (out-of-range mint)"
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
            // OB7UNY two-stamp: tick-map-only mint → liquidity clock advances,
            // price clock untouched.
            assert_eq!(
                s.tick_data_block, block_b,
                "the liquidity clock advances to pump-buffer event block"
            );
            assert_eq!(
                s.update_block, 0,
                "the price clock is untouched (out-of-range mint)"
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
                tick_data_block: None,
                coverage: PoolTickCoverage::Sparse,
                fetcher: None,
            })
            .expect("V4 pool registers");

        // 3. Apply the backfill buffer.
        core.apply_backfill_buffer_v4(pool_manager, pool_id_bytes);

        {
            let s = core.get_v4_pool(pool_id).expect("registered");
            // OB7UNY two-stamp: tick-map-only ModifyLiquidity → liquidity clock
            // advances, price clock untouched.
            assert_eq!(
                s.tick_data_block, block_b,
                "V4 liquidity clock advances to buffered event block"
            );
            assert_eq!(
                s.update_block, 0,
                "V4 price clock untouched (out-of-range mint)"
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
            tick_data_block: None,
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
                tick_data_block: None,
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
                tick_data_block: None,
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
                tick_data_block: None,
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
        // OB7UNY two-stamp: tick-map-only ModifyLiquidity → liquidity clock
        // advances; the price clock stays at the seed block 10.
        assert_eq!(s.tick_data_block, 11, "applied directly (liquidity clock)");
        assert_eq!(
            s.update_block, 10,
            "price clock untouched (out-of-range mint)"
        );
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
        // OB7UNY two-stamp: the retained-tail Burn (block 11) is tick-map-only,
        // so it advances the LIQUIDITY clock; the price clock stays at 10.
        assert_eq!(
            s.tick_data_block, 11,
            "flush advanced the retained tail (liquidity clock)"
        );
        assert_eq!(
            s.update_block, 10,
            "price clock untouched by the out-of-range Burn"
        );
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
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
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
    fn register_v4_pool_admits_amount_modifying_hook_with_caveat() {
        // ADR-037/X4EU3J: hooked pools are ADMITTED (the hard rejection is
        // gone) — their sims carry Caveats::HOOKED_POOL and paths through
        // them are excluded from solving at projection time.
        use crate::bot_core::swap_simulation::{Caveats, SwapOutcome, SwapRead, SwapRequest};
        use crate::bot_core::{RegisterV4PoolParams, V4PoolKey};
        use crate::solvers::arb_engine::PoolTickCoverage;
        use hashbrown::HashMap;

        let mut core = BotState::new();
        let pid = core
            .register_v4_pool(&RegisterV4PoolParams {
                pool_manager: Address::from([0x44u8; 20]),
                pool_id: [0xeeu8; 32],
                pool_key: V4PoolKey {
                    currency0: Address::ZERO,
                    currency1: Address::from([1u8; 20]),
                    fee: 500,
                    tick_spacing: 10,
                    // BEFORE_SWAP (0x80) — amount-modifying. Note the flags
                    // are derived from the hook address's low 16 bits by
                    // pool_builder, not trusted from this field.
                    hooks: Address::from([
                        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x00, 0x80,
                    ]),
                },
                hook_flags: 0x80,
                protocol_fee: 0,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                // Seed tick 0 + Tracked so an in-tick swap computes without
                // a fetch (isolates the caveat assertion from fetch policy).
                tick_data: HashMap::from([(
                    0_i32,
                    TickInfo {
                        liquidity_gross: alloy::primitives::U128::from(1_000_000u64),
                        liquidity_net: I256::ZERO,
                        block: 0,
                    },
                )]),
                update_block: 0,
                tick_data_block: None,
                coverage: PoolTickCoverage::Tracked,
                fetcher: None,
            })
            .expect("hooked pool must now be admitted");

        // The sim computes but is caveated as potentially inaccurate.
        let read = core.swap_simulation(
            0,
            pid,
            SwapRequest {
                zero_for_one: true,
                amount_specified: -I256::try_from(1_000u64).unwrap(),
                sqrt_price_limit: None,
            },
        );
        match read {
            SwapRead::Computed(SwapOutcome::V4(payload)) => {
                assert!(
                    payload.caveats.contains(Caveats::HOOKED_POOL),
                    "hooked-pool sims must carry the HOOKED_POOL caveat"
                );
            }
            other => panic!("hooked V4 sim must compute with caveat, got {other:?}"),
        }
    }

    #[test]
    fn register_v4_pool_rejects_dynamic_fee_with_typed_error() {
        use crate::bot_core::{RegisterV4PoolError, RegisterV4PoolParams, V4PoolKey};
        use crate::solvers::arb_engine::PoolTickCoverage;
        use hashbrown::HashMap;

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
                tick_data_block: None,
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
        use hashbrown::HashMap;

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
            tick_data_block: None,
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
        use hashbrown::HashMap;
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
            tick_data_block: None,
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
        params.sqrt_price_x96 = U256::from(degenbot_math::cl::tick_math::MAX_SQRT_RATIO);
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
            U256::from(degenbot_math::cl::tick_math::MIN_SQRT_RATIO) - uint!(1_U256);
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
        params.tick = degenbot_math::cl::tick_math::MIN_TICK - 1;
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
            tick_data_block: None,
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
            tick_data_block: None,
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
        // OB7UNY two-stamp: out-of-range mint → liquidity clock advances, price
        // clock untouched.
        assert_eq!(s_a.tick_data_block, block_b);
        assert_eq!(s_a.update_block, 0);
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
            tick_data_block: None,
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
            tick_data_block: None,
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
                tick_data_block: None,
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
            tick_data: HashMap::new(),
            update_block: 0,
            tick_data_block: None,
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
        params.sqrt_price_x96 = U256::from(degenbot_math::cl::tick_math::MAX_SQRT_RATIO);
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
            U256::from(degenbot_math::cl::tick_math::MIN_SQRT_RATIO) - uint!(1_U256);
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
        params.tick = degenbot_math::cl::tick_math::MIN_TICK - 1;
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
                tick_data_block: None,
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
            tick_data_block: None,
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
            tick_data_block: None,
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
            tick_data_block: None,
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
            tick_data_block: None,
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
            tick_data_block: None,
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
            tick_data_block: None,
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
            tick_data_block: None,
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
                tick_data_block: None,
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
                tick_data_block: None,
                coverage: PoolTickCoverage::Sparse,
                fetcher: None,
            })
            .expect("V4 re-register after unregister must succeed");
        assert_ne!(
            second_id, pool_id_u64,
            "re-register must allocate a fresh id (retired, not reused)",
        );
    }

    // --- T2 (FBJTUM): write-path sparse backfill — ensure_word_known ---

    #[test]
    #[expect(clippy::expect_used, clippy::indexing_slicing)]
    fn staged_word_fetch_install_races_on_interleaved_pool_write() {
        use crate::bot_core::InstallWordOutcome;
        use ::degenbot_pools::tick_fetch::{FetchedTickWord, TickWordFetcher};

        // Scripted fetcher (RATR5A Finding-1 red shape): attempt 1 -> empty
        // word (checked-empty, RACED); attempt 2 -> the stale tick-60 value
        // (block 99) the retried context returns.
        #[derive(Debug)]
        struct ScriptedWordFetcher {
            script: std::sync::Mutex<std::collections::VecDeque<FetchedTickWord>>,
        }
        impl ScriptedWordFetcher {
            fn new_scripted() -> Self {
                Self {
                    script: std::sync::Mutex::new(std::collections::VecDeque::from([
                        FetchedTickWord {
                            word: 0,
                            ticks: HashMap::new(),
                        },
                        FetchedTickWord {
                            word: 0,
                            ticks: HashMap::from_iter([(
                                60,
                                TickInfo {
                                    liquidity_gross: alloy::primitives::U128::from(100u128),
                                    liquidity_net: ::alloy::primitives::I256::try_from(100i128)
                                        .unwrap(),
                                    block: 99,
                                },
                            )]),
                        },
                    ])),
                }
            }
        }
        impl TickWordFetcher for ScriptedWordFetcher {
            fn fetch_missing_tick_word(
                &self,
                _pool_id: u64,
                word: i32,
                _block: u64,
            ) -> Result<FetchedTickWord, ::degenbot_pools::tick_fetch::FetchTickWordError>
            {
                self.script
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .pop_front()
                    .map(|mut w| {
                        w.word = word;
                        w
                    })
                    .ok_or(::degenbot_pools::tick_fetch::FetchTickWordError::FetchFailed)
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
                tick_data_block: None,
                coverage: PoolTickCoverage::Sparse,
                fetcher: Some(std::sync::Arc::new(ScriptedWordFetcher::new_scripted())),
                ..Default::default()
            })
            .expect("test setup: V3 registration");

        // Stage under the (simulated) short write, then a pump event for the
        // SAME pool lands during the fetch window (the RATR5A race shape).
        let staged = core
            .stage_word_fetch_by_pool_id(pool_id, 0, 99, false)
            .expect("sparse pool stores a fetcher");
        core.apply_v3_liquidity_update_by_pool_id(pool_id, -60, 60, 1_000, 100);
        let fetched = staged.fetch().expect("empty-word fetch");
        assert_eq!(
            core.install_word_fetch(&staged, &fetched),
            InstallWordOutcome::Raced,
            "an interleaved pool write must force a retry, never a clobbering overlay"
        );

        // Retry shape (RATR5A Finding 1): the stage re-derives the fetch
        // context from the pool clock - the companion block passed above is
        // deliberately bogus (9_999) so a failed re-derivation is loud - and
        // the scripted second fetch returns the stale tick-60 value (block
        // 99). The stamp guard (Finding-1(a)) must keep the event values
        // (gross 1_000 @ block 100).
        let staged = core
            .stage_word_fetch_by_pool_id(pool_id, 0, 9_999, true)
            .expect("sparse pool stores a fetcher");
        assert_eq!(
            staged.block, 99,
            "retry fetch context re-derives from the pool clock (update_block - 1)"
        );
        let fetched = staged.fetch().expect("scripted stale fetch");
        let outcome = core.install_word_fetch(&staged, &fetched);
        assert_eq!(
            outcome,
            InstallWordOutcome::Merged,
            "quiet retry merges (fingerprint unchanged since restage)"
        );
        match core.pools.get(&pool_id) {
            Some(PoolEntry::V3(_, state)) => {
                let tick = state
                    .tick_data
                    .get(&60)
                    .expect("merged word carries tick 60 after the retry");
                assert_eq!(
                    (tick.liquidity_gross.to::<u128>(), tick.block),
                    (u128::from(1_000u64), 100),
                    "the event-applied tick must NOT be regressed by the stale overlay"
                );
            }
            _ => panic!("test setup: V3 pool missing"),
        }
        let known: Vec<i32> = match core.pools.get(&pool_id) {
            Some(PoolEntry::V3(_, state)) => state.known_bitmap_words().iter().copied().collect(),
            _ => panic!("test setup: V3 pool missing"),
        };
        assert!(
            known.contains(&0),
            "the retry's install marks the word known (T2 FBJTUM parity)"
        );
    }

    #[test]
    fn ensure_word_known_no_fetcher_returns_false() {
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
                coverage: PoolTickCoverage::Sparse,
                fetcher: None,
                ..Default::default()
            })
            .expect("test setup: V3 registration");

        assert!(
            !core.ensure_word_known_by_pool_id(pool_id, 0, 99),
            "no stored fetcher → False (the Python gate raises)"
        );
    }

    #[test]
    fn ensure_word_known_merges_ticks_and_marks_word_known() {
        use ::degenbot_pools::tick_fetch::{FetchTickWordError, FetchedTickWord, TickWordFetcher};
        use ::degenbot_pools::TickInfo;
        use alloy::primitives::{I256, U128};

        #[derive(Debug)]
        struct WordFetcher;
        impl TickWordFetcher for WordFetcher {
            fn fetch_missing_tick_word(
                &self,
                _pool_id: u64,
                word: i32,
                _block: u64,
            ) -> Result<FetchedTickWord, FetchTickWordError> {
                Ok(FetchedTickWord {
                    word,
                    ticks: HashMap::from_iter([(
                        60,
                        TickInfo {
                            liquidity_gross: U128::from(100u128),
                            liquidity_net: I256::try_from(100i128).unwrap(),
                            block: 99,
                        },
                    )]),
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
                tick_data: HashMap::new(),
                update_block: 0,
                tick_data_block: None,
                coverage: PoolTickCoverage::Sparse,
                fetcher: Some(std::sync::Arc::new(WordFetcher)),
                ..Default::default()
            })
            .expect("test setup: V3 registration");

        let ok = core.ensure_word_known_by_pool_id(pool_id, 0, 99);
        assert!(ok, "a successful fetch must return True");

        let tick_data: HashMap<i32, TickInfo> = match core.pools.get(&pool_id) {
            Some(PoolEntry::V3(_, state)) => state.tick_data().clone(),
            _ => panic!("test setup: V3 pool missing"),
        };
        assert!(
            tick_data.contains_key(&60),
            "the fetched word's ticks must land in tick_data (core merge reused)"
        );
        let known: Vec<i32> = match core.pools.get(&pool_id) {
            Some(PoolEntry::V3(_, state)) => state.known_bitmap_words().iter().copied().collect(),
            _ => panic!("test setup: V3 pool missing"),
        };
        assert!(known.contains(&0), "the fetched word must be marked known");
    }

    #[test]
    fn ensure_word_known_fetch_error_returns_false() {
        use ::degenbot_pools::tick_fetch::{FetchTickWordError, FetchedTickWord, TickWordFetcher};

        #[derive(Debug)]
        struct FailingWordFetcher;
        impl TickWordFetcher for FailingWordFetcher {
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
                tick_data_block: None,
                coverage: PoolTickCoverage::Sparse,
                fetcher: Some(std::sync::Arc::new(FailingWordFetcher)),
                ..Default::default()
            })
            .expect("test setup: V3 registration");

        assert!(
            !core.ensure_word_known_by_pool_id(pool_id, 0, 99),
            "a fetch failure must return False (the Python gate RAISES, never applies)"
        );
        let (len, known) = match core.pools.get(&pool_id) {
            Some(PoolEntry::V3(_, state)) => {
                (state.tick_data().len(), state.known_bitmap_words().len())
            }
            _ => panic!("test setup: V3 pool missing"),
        };
        assert_eq!((len, known), (0, 0), "no state mutation on a failed fetch");
    }

    #[test]
    fn ensure_word_known_checked_empty_marks_word_known() {
        use ::degenbot_pools::tick_fetch::{FetchTickWordError, FetchedTickWord, TickWordFetcher};

        #[derive(Debug)]
        struct EmptyWordFetcher;
        impl TickWordFetcher for EmptyWordFetcher {
            fn fetch_missing_tick_word(
                &self,
                _pool_id: u64,
                word: i32,
                _block: u64,
            ) -> Result<FetchedTickWord, FetchTickWordError> {
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
                tick_data: HashMap::new(),
                update_block: 0,
                tick_data_block: None,
                coverage: PoolTickCoverage::Sparse,
                fetcher: Some(std::sync::Arc::new(EmptyWordFetcher)),
                ..Default::default()
            })
            .expect("test setup: V3 registration");

        assert!(
            core.ensure_word_known_by_pool_id(pool_id, 3, 99),
            "a checked-empty fetch is a success: the word is known (T1 semantics)"
        );
        let known: Vec<i32> = match core.pools.get(&pool_id) {
            Some(PoolEntry::V3(_, state)) => state.known_bitmap_words().iter().copied().collect(),
            _ => panic!("test setup: V3 pool missing"),
        };
        assert!(known.contains(&3), "checked-empty word 3 must be known");
    }

    // --- ADR-005 sparse-map parity, slice 2: fetch-callback seam ---

    #[test]
    fn swap_simulation_fills_missing_word_and_retries() {
        // A sparse V3 pool (empty tick_data, start tick 0, word 0 unknown)
        // misses on the starting word. The gate's fetch seam fills the missing
        // word (via a fake fetcher), merges, and retries; the result must be
        // non-zero, record the fetch, and carry the sparse caveat.
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
                tick_data_block: None,
                coverage: PoolTickCoverage::Sparse,
                fetcher: Some(std::sync::Arc::new(FakeFetcher)),
                ..Default::default()
            })
            .expect("test setup: V3 registration");

        // Through the gate: the miss is recovered automatically — computed,
        // non-zero, the fetched word recorded, sparse coverage caveated.
        let read = core.swap_simulation(
            0,
            pool_id,
            SwapRequest {
                zero_for_one: true,
                amount_specified: -I256::try_from(U256::from(1000u64)).unwrap(),
                sqrt_price_limit: None,
            },
        );
        match read {
            SwapRead::Computed(SwapOutcome::V3(payload)) => {
                assert_ne!(
                    payload.delivered,
                    I256::ZERO,
                    "the fetched sparse swap must produce a non-zero amount"
                );
                // The walk may legitimately miss both word 0 and the adjacent
                // word (-1) before finding liquidity; both fetches are recorded.
                assert!(
                    payload.fetched_words.contains(&0),
                    "fetch of word 0 recorded, got {:?}",
                    payload.fetched_words
                );
                assert!(
                    payload.caveats.contains(Caveats::SPARSE_COVERAGE),
                    "sparse coverage must be caveated"
                );
            }
            other => panic!("gate must recover via fetch+retry, got {other:?}"),
        }
        // The fetch merged word 0 into known_bitmap_words (no further miss).
        let state = core.get_v3_pool(pool_id).expect("pool registered");
        assert!(
            state.known_bitmap_words.contains(&0),
            "fetched word 0 must be marked known"
        );
    }

    #[test]
    fn swap_simulation_fetcher_error_surfaces_fetch_failed() {
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
                tick_data_block: None,
                coverage: PoolTickCoverage::Sparse,
                fetcher: Some(std::sync::Arc::new(FailingFetcher)),
                ..Default::default()
            })
            .expect("test setup: V3 registration");

        // Through the gate: a failing fetcher is OBSERVABLE as FetchFailed
        // (word 0) — the legacy seam collapsed this into silent ZERO.
        let read = core.swap_simulation(
            0,
            pool_id,
            SwapRequest {
                zero_for_one: true,
                amount_specified: -I256::try_from(U256::from(1000u64)).unwrap(),
                sqrt_price_limit: None,
            },
        );
        assert_eq!(
            read,
            SwapRead::FetchFailed { word: 0 },
            "a failing fetcher must surface FetchFailed, not panic, spin, or silently zero"
        );
    }

    #[test]
    fn swap_simulation_empty_word_not_refetched() {
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
                tick_data_block: None,
                coverage: PoolTickCoverage::Sparse,
                fetcher: Some(counter.clone() as Arc<dyn TickWordFetcher>),
                ..Default::default()
            })
            .expect("test setup: V3 registration");

        // First solve: misses word 0, fetches (empty), retries → computes.
        // The fetched word rides on the outcome; the empty (checked) word
        // survives in known_bitmap_words.
        let read = core.swap_simulation(
            0,
            pool_id,
            SwapRequest {
                zero_for_one: true,
                amount_specified: -I256::try_from(U256::from(1000u64)).unwrap(),
                sqrt_price_limit: None,
            },
        );
        let first = match &read {
            SwapRead::Computed(SwapOutcome::V3(p)) => p.delivered.into_raw(),
            other => panic!("first solve must compute via fetch+retry, got {other:?}"),
        };
        let calls_after_first = counter.calls.load(Ordering::SeqCst);
        assert!(calls_after_first >= 1, "first solve must fetch word 0");
        match read {
            SwapRead::Computed(SwapOutcome::V3(p)) => {
                assert!(
                    p.fetched_words.contains(&0),
                    "word 0 fetched once, got {:?}",
                    p.fetched_words
                );
            }
            _ => unreachable!(),
        }

        // Second solve: word 0 is now known → NO fetch should happen and the
        // outcome records no fetched words.
        let second = core.swap_simulation(
            0,
            pool_id,
            SwapRequest {
                zero_for_one: true,
                amount_specified: -I256::try_from(U256::from(1000u64)).unwrap(),
                sqrt_price_limit: None,
            },
        );
        assert_eq!(
            counter.calls.load(Ordering::SeqCst),
            calls_after_first,
            "second solve must NOT re-invoke the fetcher — the empty word survived in known_bitmap_words"
        );
        match second {
            SwapRead::Computed(SwapOutcome::V3(p)) => {
                assert_eq!(
                    p.delivered.into_raw(),
                    first,
                    "second solve must match the first"
                );
                assert!(p.fetched_words.is_empty());
            }
            other => panic!("second solve must compute without fetching, got {other:?}"),
        }
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
                tick_data_block: None,
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

    // ── pin clamp regression (DFQYM5 fabricated-mismatch fix) ──────────────
    //
    // The pin stores (tick_map, liquidity_clock) and step-2 verify compares
    // the map against on-chain @ that block. If the pump has any UNDRAINED
    // event at/below the pool's liquidity clock (an in-progress block the
    // drain held back at the tombstone cutoff), the map is NOT complete AT
    // that clock block — verifying there would compare an incomplete map
    // against the full on-chain block and fabricate a mismatch. The pin must
    // clamp the verify block down to `pump_complete_cutoff`. Both branches are
    // asserted below: the clamp (undrained > 0) and the benign preserve
    // (undrained == 0, mod.rs:580 seed).
    #[test]
    fn pin_clamps_verify_block_to_complete_cutoff_when_pump_undrained() {
        use crate::bot_core::RegisterV3PoolParams;
        use crate::solvers::arb_engine::PoolTickCoverage;
        let mut core = BotState::new();
        let pool_addr = Address::from([0xf8u8; 20]);
        core.register_v3_pool(&RegisterV3PoolParams {
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
            // DB-seeded seed: the liquidity clock is exact at block 100.
            update_block: 100,
            tick_data_block: Some(100),
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
            ..Default::default()
        })
        .expect("test setup: V3 registration");
        // Quarantine so live Mint/Burn route to the pump buffer.
        core.set_v3_pool_quarantined(pool_addr);
        // Pump-buffer an UNDRAINED Mint at block 100, but only tombstone the
        // pump through block 99 → the drain (apply_pump_buffer_v3) holds the
        // block-100 event back, so the map is NOT complete at its 100 clock.
        core.apply_v3_liquidity_update(pool_addr, -10, 10, 500_i128, 100);
        core.advance_pump_complete_cutoff(99);
        assert_eq!(core.pump_complete_cutoff(), 99);
        assert_eq!(core.v3_buffer.pump_count_at_or_below(&pool_addr, 100), 1);
        core.apply_pump_buffer_v3(&pool_addr);
        core.pin_v3_post_drain_snapshot(pool_addr);
        let (_ticks, pinned_block) = core
            .take_v3_post_drain_snapshot(pool_addr)
            .expect("pin captured for a Tracked pool");
        assert_eq!(
            pinned_block, 99,
            "fabricated mismatch: pin must clamp the verify block down to the \
             complete cutoff (99), not the seed liquidity clock (100), when the \
             pump has undrained events at/below the clock"
        );
    }

    #[test]
    fn pin_preserves_clock_block_when_no_undrained_events() {
        use crate::bot_core::RegisterV3PoolParams;
        use crate::solvers::arb_engine::PoolTickCoverage;
        let mut core = BotState::new();
        let pool_addr = Address::from([0xf9u8; 20]);
        core.register_v3_pool(&RegisterV3PoolParams {
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
            update_block: 100,
            tick_data_block: Some(100),
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
            ..Default::default()
        })
        .expect("test setup: V3 registration");
        core.set_v3_pool_quarantined(pool_addr);
        // Cutoff is BEHIND the seed clock (99 < 100) and the pump has NO event
        // for this pool at/below the clock → pump_count == 0. This is the
        // mod.rs:580 BENIGN seed case: the DB seed carries the live WS head
        // past the cutoff, so no event could be missing — the clock block is
        // preserved (NOT clamped), and verifying at 100 is correct.
        core.advance_pump_complete_cutoff(99);
        assert_eq!(core.v3_buffer.pump_count_at_or_below(&pool_addr, 100), 0);
        core.apply_pump_buffer_v3(&pool_addr);
        core.pin_v3_post_drain_snapshot(pool_addr);
        let (_ticks, pinned_block) = core
            .take_v3_post_drain_snapshot(pool_addr)
            .expect("pin captured for a Tracked pool");
        assert_eq!(
            pinned_block, 100,
            "benign seed (pump_count==0) must keep the clock block, not clamp"
        );
    }
}
