//! V3 concentrated-liquidity pool state — the single BotState-owned home for
//! V3 pool data (ADR-003). Supersedes the engine-side `V3PoolState` that lived
//! in `solvers/v3_block_engine.rs`; that engine is dissolved and V3 state
//! is owned by [`crate::BotState`], peer to `ArbitrageEngine`.
//!
//! This struct carries both the authoritative mutable state (`sqrt_price_x96`,
//! `liquidity`, `tick`, `tick_data`), the snapshot-coverage flag, the lazy
//! tick-range derivation cache (`cached_tick_ranges`, shared infra consumed
//! by `build_int_v3_sequence`), and the per-pool reorg journal
//! ([`ReorgJournal`] of [`V3BlockDelta`]).

use hashbrown::{HashMap, HashSet};
use std::sync::Arc;

use alloy::primitives::{Address, B256, I256, U160, U256};

use crate::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};
use crate::state_history::{
    JournalError, ReorgJournal, ReorgPoolState, ScalarPriors, V3BlockDelta,
};
use crate::tick_bitmap::{compute_tick_ranges, gen_ticks_iter, V3TickRangeForSolver};
use crate::tick_fetch::TickWordFetcher;
use crate::TickInfo;
use degenbot_math::cl::functions::tick_position;
use degenbot_math::cl::swap_math::compute_swap_step_v3;
use degenbot_math::cl::tick_math::{
    get_sqrt_ratio_at_tick_internal, get_tick_at_sqrt_ratio_internal, MAX_SQRT_RATIO,
    MIN_SQRT_RATIO,
};

// ---------------------------------------------------------------------------
// Coverage flag
// ---------------------------------------------------------------------------

/// Describes the completeness of tick data for a registered V3 pool.
///
/// `Tracked` means the snapshot provided complete tick data (may be empty =
/// genuinely illiquid). `Sparse` means no snapshot data exists for this pool
/// — solver results may contain errors or phantom profits.
///
/// Moved from `solvers/arb_engine/mod.rs` to live with V3 state under
/// ADR-003; re-exported from `arb_engine` for back-compat with callers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PoolTickCoverage {
    /// Snapshot provided complete tick data. Solver results are trustworthy.
    #[default]
    Tracked,
    /// No snapshot data exists. Solver results may be inaccurate.
    Sparse,
}

// ---------------------------------------------------------------------------
// Pool registration lifecycle (Quarantined / Live)
// ---------------------------------------------------------------------------

/// The per-pool registration lifecycle for V3/V4 CL pools. Controls whether
/// the live pump applies events directly (`Live`) or defers them to the pump
/// buffer (`Quarantined`).
///
/// `Quarantined` is set at the start of `register_v3/v4_pool` (before the
/// first RPC await in the two-step verify) so a live `Swap`/`Mint`/`Burn`
/// landing during the drain+pin+verify window cannot advance `update_block`
/// past the pump's `last_complete_block` — which would desync the pinned
/// `(tick_data, update_block)` pair (the 6N7XVR race; the live direct-apply
/// gap YLYJM2's `drain_pump_completed` buffer gate does NOT cover).
///
/// Defaults to `Live`: pools registered outside the two-step verify path
/// (test/standalone construction) keep the existing direct-apply behavior.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RegistrationLifecycle {
    /// Live events apply directly to pool state (the steady-state contract).
    #[default]
    Live,
    /// Live events are deferred to the pump buffer until `set_pool_live`
    /// flushes them. Used during `register_v3/v4_pool`'s drain+pin+verify.
    Quarantined,
}

// ---------------------------------------------------------------------------
// Buffered liquidity update for unregistered V3 pools
// ---------------------------------------------------------------------------

/// A buffered liquidity update (Mint or Burn) for an unregistered V3 pool
/// awaiting registration. Stores raw event data (not collapsed) so future
/// reorg handling can reverse-apply.
#[derive(Clone, Debug)]
pub struct BufferedV3LiquidityUpdate {
    /// The tick lower boundary of the position.
    pub tick_lower: i32,
    /// The tick upper boundary of the position.
    pub tick_upper: i32,
    /// The signed liquidity delta: positive for Mint, negative for Burn.
    pub liquidity_delta: i128,
    /// The block number of this event.
    pub block_number: u64,
}

impl crate::liquidity_event::LiquidityEvent for BufferedV3LiquidityUpdate {
    fn block_number(&self) -> u64 {
        self.block_number
    }
}

/// A buffered V3 `Swap` event awaiting application — either because the pool
/// is unregistered (the live drop path, retained for symmetry with liquidity)
/// or because the pool is `Quarantined` (6N7XVR deferral). Carries the scalar
/// fields `apply_swap` mutates; no `tick_priors` (the pump path passes `&[]`).
#[derive(Clone, Debug)]
pub struct BufferedV3SwapEvent {
    /// The post-swap `sqrtPriceX96`.
    pub sqrt_price_x96: U256,
    /// The post-swap active liquidity.
    pub liquidity: u128,
    /// The post-swap active tick.
    pub tick: i32,
    /// The block number of this event.
    pub block_number: u64,
}

impl crate::liquidity_event::LiquidityEvent for BufferedV3SwapEvent {
    fn block_number(&self) -> u64 {
        self.block_number
    }
}

/// A buffered V3 pool event — a `Swap` or a liquidity update (`Mint`/`Burn`),
/// unified in one enum so the `LiquidityEventBuffer` preserves cross-type
/// arrival order within a block (a `Swap` at logIdx 1433 must apply after a
/// `Mint` at logIdx 120). 6N7XVR: the quarantine deferral routes BOTH variants
/// through the same gated drain, so the pin's `update_block` cannot outrun
/// `last_complete_block` regardless of event type.
#[derive(Clone, Debug)]
pub enum BufferedV3PoolEvent {
    /// A `Mint`/`Burn` liquidity update.
    Liquidity(BufferedV3LiquidityUpdate),
    /// A `Swap` (scalar update — does not touch `tick_data`).
    Swap(BufferedV3SwapEvent),
}

impl crate::liquidity_event::LiquidityEvent for BufferedV3PoolEvent {
    fn block_number(&self) -> u64 {
        match self {
            Self::Liquidity(u) => u.block_number,
            Self::Swap(s) => s.block_number,
        }
    }
}

// ---------------------------------------------------------------------------
// Registration params
// ---------------------------------------------------------------------------

/// Parameters for registering a V3 pool with `BotState`.
///
/// Bundles all fields to satisfy `clippy::too_many_arguments`.
#[derive(Clone, Debug, Default)]
pub struct RegisterV3PoolParams {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub fee: u32,
    pub tick_spacing: i32,
    pub factory: Address,
    pub sqrt_price_x96: U256,
    pub liquidity: u128,
    pub tick: i32,
    pub tick_data: HashMap<i32, TickInfo>,
    pub update_block: u64,
    /// The **liquidity** clock seed (two-stamp OB7UNY) — the block the
    /// supplied `tick_data` map is exact at. `None` (the default) falls back
    /// to [`Self::update_block`], keeping a single-clock seed for existing
    /// callers. A fresh-read builder sets `update_block` to a HEAD slot0 read
    /// while `tick_data_block` stays at the DB liquidity snapshot block, so
    /// the price clock can be freshly stamped without pretending the tick
    /// map reaches head.
    pub tick_data_block: Option<u64>,
    /// Whether tick data came from the snapshot (`Tracked`) or has no
    /// snapshot coverage (`Sparse`). The buffer is always applied — the
    /// snapshot is always stale data from the DB.
    pub coverage: PoolTickCoverage,
    /// Sparse-tick backfill fetcher (stored on `V3PoolState` at
    /// registration; `None` for `Tracked` pools or when no Python fetcher
    /// was supplied). ADR-006 I/O trait object — pyo3-free trait, the
    /// `PyTickWordFetcher` adapter in `degenbot-python` wraps a Python
    /// callable.
    pub fetcher: Option<Arc<dyn TickWordFetcher>>,
    /// The CREATE2 deployer the Rust builder verified this pool's address
    /// against (Fork A, P62DKO). Equals the JSON row's `deployer`, or `factory`
    /// when the row had `null` (the `None -> factory` convention). For non-JSON
    /// pools, the factory (no lookup). Stored on the identity so a Python
    /// companion reads the verified deployer off the handle (no `chain_id`
    /// plumbing needed).
    pub deployer: Address,
    /// The CREATE2 init code hash the Rust builder verified this pool's
    /// address against (Fork A, P62DKO). The JSON row's `init_hash` when the
    /// `(chain, factory)` shipped, else the [`degenbot_uniswap::deployments`]
    /// `UNISWAP_V3_MAINNET_INIT_HASH` fallback (the retired Python `ClassVar`'s
    /// documented default for non-JSON V3 pools).
    pub init_hash: B256,
}

/// Typed rejection from [`crate::BotState::register_v3_pool`] (the
/// spec-bound + duplicate-address admission contract — see
/// [`crate::spec_bounds`]). Mirrors `RegisterV4PoolError` (and the
/// V2 twin `RegisterV2PoolError`):
/// `#[derive(Clone, Debug, PartialEq, Eq)]`, no `Display`/`Error` impl (the
/// `PyO3` mapper pattern-matches the variants directly).
///
/// Variants:
/// - [`AlreadyRegistered`](Self::AlreadyRegistered) — replaces the prior
///   `assert!` duplicate-check panic on V3.
/// - [`SpecViolation`](Self::SpecViolation) — wraps a
///   `spec_bounds::SpecViolation` from the validator helpers
///   (`validate_sqrt_price` / `validate_tick` / `validate_v3_fee` /
///   `validate_tick_spacing`); the four V3 spec checks fire together.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegisterV3PoolError {
    /// A pool at this contract address is already registered.
    AlreadyRegistered { address: Address },
    /// An out-of-spec field (e.g. `sqrt_price_x96 >= MAX_SQRT_RATIO`).
    SpecViolation(crate::spec_bounds::SpecViolation),
}

/// A pre-decoded V3 Swap update for testing without log decoding.
#[derive(Clone, Debug)]
pub struct V3SwapUpdate {
    pub pool_address: Address,
    pub sqrt_price_x96: U256,
    pub liquidity: u128,
    pub tick: i32,
    /// Stored as `Box<[(i32, TickInfo)]>` (read-only once applied): `apply_swap`
    /// takes `&[(i32, TickInfo)]`, so the boxed slice drops the unused `Vec`
    /// capacity field.
    pub tick_priors: Box<[(i32, TickInfo)]>,
}

// ---------------------------------------------------------------------------
// V3 pool state
// ---------------------------------------------------------------------------

/// Cached state of a tick-range computation for one direction.
///
/// Three states: `StillEmpty` (not computed since last invalidation),
/// `Hit` (`compute_tick_ranges` returned `Some`), `Miss` (returned `None`).
/// Caching `Miss` prevents re-walking pools whose state hasn't changed
/// since the last failed walk — the dominant cost in live solve cycles
/// (1300-9600 `SequenceUnavailable` rejections per cycle, each re-walking
/// O(tick-walk) from scratch). `invalidate_tick_range_cache` resets both
/// slots to `StillEmpty` on every Swap/Mint/Burn (ergo 2SGSE3).
#[derive(Clone, Debug, Default)]
pub enum CachedTickRanges {
    /// Not computed since the last invalidation — the next call must walk.
    #[default]
    StillEmpty,
    /// `compute_tick_ranges` returned `Some` — cached for reuse.
    Hit(Arc<[V3TickRangeForSolver]>),
    /// `compute_tick_ranges` returned `None` — cached to avoid re-walking.
    Miss,
}

/// Cached tick ranges for a single pool, keyed by direction.
#[derive(Clone, Debug, Default)]
pub struct TickRangeCache {
    zfo: CachedTickRanges,
    ofz: CachedTickRanges,
}

/// Immutable V3 registration identity (ADR-005 identity slice).
///
/// Pure registration data — permanent pool identity set once at
/// `register_v3_pool` and never mutated. Mirrors `TokenEntry`/`V2PoolIdentity`.
/// Distinct from [`V3PoolState`], which carries only mutable runtime data
/// (`sqrt_price/liquidity/tick/tick_data/journal` + the pinned verify seeds).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct V3PoolIdentity {
    /// Pool contract address.
    pub address: Address,
    /// Token0 contract address.
    pub token0: Address,
    /// Token1 contract address.
    pub token1: Address,
    /// Pool fee tier (immutable config). `PyLiquidityPool.fee()` reads this.
    pub fee: u32,
    /// Tick spacing (immutable config).
    pub tick_spacing: i32,
    /// Pool factory address.
    pub factory: Address,
    /// The CREATE2 deployer the pool's address was verified against (Fork A,
    /// P62DKO). The JSON row's `deployer` (or `factory` for null), or the
    /// factory itself for non-JSON pools. Stored on the identity so the
    /// companion reads it off the handle.
    pub deployer: Address,
    /// The CREATE2 init code hash (Fork A, P62DKO). The JSON row's `init_hash`
    /// when shipped, else the Uniswap V3 mainnet fallback const. Off the
    /// handle, not the retired Python `ClassVar`.
    pub init_hash: B256,
}

/// V3 concentrated-liquidity pool state owned by [`crate::BotState`].
///
/// Carries authoritative mutable state plus a per-pool reorg journal. Swap
/// calculations read current mutable fields directly (never touch the journal);
/// `apply_swap`/`apply_liquidity_update` push reverse-apply deltas. Immutable
/// identity lives on [`V3PoolIdentity`]; look it up via
/// [`crate::BotState::get_v3_identity`].
#[derive(Debug)]
pub struct V3PoolState {
    // --- Mutable state (authoritative) ---
    pub sqrt_price_x96: U256,
    pub liquidity: u128,
    pub tick: i32,
    /// The **price** clock — the block the SLOT0 scalars
    /// (`sqrt_price_x96`/`liquidity`/`tick`) reflect. Advanced only by an
    /// apply that changes the scalars: a Swap, an in-range post-seed liquidity
    /// event, a fresh slot0 read, or registration. NEVER advanced by a tick-
    /// map-only change (`replace_tick_data`/`merge_tick_word`/backfill replay).
    /// Monotonic non-decreasing — a backward stamp outside a reorg panics
    /// (two-stamp pool state, OB7UNY).
    pub update_block: u64,
    /// The **liquidity** clock — the block the `tick_data` map reflects.
    /// Advanced by any event that mutates the tick map: a Swap (crossings),
    /// a liquidity event, `replace_tick_data`, or registration. Monotonic
    /// non-decreasing — a backward stamp outside a reorg panics. Distinct
    /// from [`Self::update_block`]: a pool can have a fresh price but a
    /// liquidity map that lags the chain (the two-stamp distinction).
    pub tick_data_block: u64,

    /// The frozen block at which this pool's state was seeded/synchronized
    /// from on-chain (the registration/seed block, equal to the `update_block`
    /// supplied at construction — Python twin of
    /// `PyLiquidityPool._initial_state_block`).
    ///
    /// Historical replay guard: the seed's `liquidity` scalar ALREADY
    /// reflects every in-range Mint/Burn at or before this block, so a
    /// liquidity event replayed at `block_number <= initial_state_block`
    /// (e.g. a backfilled Burn applied after the pool was registered against
    /// head) must NOT adjust the active-liquidity scalar — it would double-
    /// count a removal/addition the seed already contains (UO3JM4 solver
    /// desync: a pre-seed in-range Burn subtracted its net twice). Frozen —
    /// never advanced by `apply_swap`/`apply_liquidity_update`/`update_block`.
    pub initial_state_block: u64,

    /// The per-pool registration lifecycle (6N7XVR): `Quarantined` during
    /// `register_v3_pool`'s drain+pin+verify (live events deferred to the pump
    /// buffer so the pin's `update_block` cannot outrun `last_complete_block`),
    /// `Live` thereafter (direct apply). Defaults to `Live` (pools registered
    /// outside the two-step verify keep the steady-state direct-apply path).
    pub registration_lifecycle: RegistrationLifecycle,

    /// Per-mutation nonce — bumped on every state change (`apply_swap`,
    /// `apply_liquidity_update`, `replace_tick_data`, `merge_tick_word`,
    /// `restore_before_block`). The solver snapshots this per-hop at resolve
    /// time so the dispatch seam can detect staleness: if a pool's current
    /// nonce has advanced past the snapshot, the solver computed its result
    /// against state that has since been superseded → skip the stale
    /// candidate (AV42C7: the block-N solve used pool@N-1 while on-chain@N
    /// has pool@N after the user swap).
    pub state_nonce: u64,

    /// Initialized ticks: tick index → (`liquidity_gross`, `liquidity_net`).
    pub tick_data: HashMap<i32, TickInfo>,

    /// The pinned snapshot seed (CBCH6H): a copy of the `tick_data` supplied
    /// at registration, NEVER mutated by `apply_v3_liquidity_update` /
    /// `apply_v3_swap`. Retained so step-1 verify can compare the **seed**
    /// against on-chain@snapshot_block — NOT the pump-mutated `tick_data`
    /// current. During a rolling start (`resume()` precedes `build_paths`)
    /// the live pump applies Mint/Burn journals onto `tick_data`; without a
    /// pinned seed, step-1 would read engine-current (seed + journal) vs
    /// on-chain@snapshot (pre-journal) → a false mismatch on every active
    /// pool. `Some` only for `Tracked` (snapshot-seeded) pools; `None` for
    /// `Sparse` pools (no complete seed to verify). Cleared by
    /// `take_v3_snapshot_seed` after step-1 verify (verified exactly once).
    pub snapshot_seed: Option<HashMap<i32, TickInfo>>,

    /// The pinned **post-drain** `(tick_data, block)` pair (the step-2
    /// rolling-start race fix, twin of `snapshot_seed`). Captured atomically
    /// with `apply_buffer_v3`'s final drain (the single `core.write()` hold
    /// that runs the backfill + pump drains) by `pin_v3_post_drain_snapshot`,
    /// and consumed once by `take_v3_post_drain_snapshot` for step-2 verify.
    ///
    /// The pair carries BOTH the frozen `tick_data` AND the `update_block` it
    /// was computed at (the last drained event's block, or the registration
    /// block if no events landed in either buffer). Step-2 verify compares
    /// this pair against on-chain@**the pinned block** — NOT a start()-time
    /// `verify_backfill_block` constant. The pin's block is load-bearing: for
    /// an active pool on a slow `build_paths`, the pump buffer accumulates
    /// Mint/Burn events at blocks PAST the backfill boundary; draining them
    /// advances `tick_data` to a state that matches on-chain at a LATER block,
    /// so verifying against `verify_backfill_block` fabricated a mismatch and
    /// crashed the bot (2026-06-29). Capturing the block alongside the state
    /// — under the same write-lock that finished the drain — makes the
    /// (state, block) pair self-consistent and the verify race-free.
    ///
    /// Verifying THIS frozen pair (not engine-current) is also race-free under
    /// a rolling start (`resume()` precedes `build_paths`): the live pump
    /// applies Mint/Burn onto `tick_data` AFTER the drain; reading
    /// engine-current at step-2 would compare drain+pump-journal vs
    /// on-chain@pinned-block (pre-journal) → a false mismatch on every active
    /// pool (logs/verify-race-hotloop.log). `Some` only for `Tracked` pools
    /// (Sparse has no complete `tick_data` → `None` → step-2 no-op).
    pub post_drain_snapshot: Option<(HashMap<i32, TickInfo>, u64)>,

    /// Whether the snapshot provided complete tick data for this pool.
    pub coverage: PoolTickCoverage,

    /// Tick-bitmap word positions (`tick_position(tick.div_euclid(tick_spacing))`)
    /// whose bitmap has been fetched and is therefore "known". Only consulted
    /// when `coverage == Sparse` (Tracked pools have complete data and bypass
    /// miss-detection). Seeded at registration from the initial `tick_data`
    /// keys' word positions; grown by the fetch-write seam (`update_tick_data`)
    /// as the registered tick-data fetcher fills unknown regions. Mirrors the
    /// Python `LiquidityMapSnapshot.tick_bitmap` key-presence rule: in sparse
    /// mode a region is unknown unless its word key is in this set (a
    /// fetched-but-empty word is recorded here as `known`).
    pub known_bitmap_words: HashSet<i32>,

    /// The sparse-tick backfill fetcher (ADR-006 I/O trait object). Set once
    /// at `register_v3_pool`; the fetch+retry loop in
    /// `calculate_tokens_out_with_fetch` / `simulate_*_with_fetch` clones
    /// this `Arc` out and calls it on a `MissingTickWord` miss. `None` for
    /// `Tracked` pools (complete tick data) or when no Python fetcher was
    /// supplied at registration.
    pub fetcher: Option<Arc<dyn TickWordFetcher>>,

    /// Reorg journal — scalar priors + per-tick priors for rollback.
    pub journal: ReorgJournal<V3BlockDelta>,

    // Cached tick ranges (interior mutability for lazy computation from &self).
    // Invalidated on apply_swap / apply_liquidity_update. Consumed only by
    // `build_int_v3_sequence` (gen-3 integer solver).
    pub cached_tick_ranges: parking_lot::Mutex<TickRangeCache>,
}

impl Clone for V3PoolState {
    fn clone(&self) -> Self {
        Self {
            sqrt_price_x96: self.sqrt_price_x96,
            liquidity: self.liquidity,
            tick: self.tick,
            update_block: self.update_block,
            tick_data_block: self.tick_data_block,
            initial_state_block: self.initial_state_block,
            state_nonce: self.state_nonce,
            registration_lifecycle: self.registration_lifecycle,
            tick_data: self.tick_data.clone(),
            coverage: self.coverage,
            known_bitmap_words: self.known_bitmap_words.clone(),
            fetcher: self.fetcher.clone(),
            // Clones start with no cached ranges — the cache is invalidated on
            // mutation anyway, and a fresh Mutex avoids aliasing the source's.
            journal: self.journal.clone(),
            // Clones (e.g. `v3_pools_snapshot()`) do NOT carry the pinned seed
            // or the pinned post-drain snapshot: both are only needed for
            // step-1/step-2 verify on the live pool, and copying them into
            // every snapshot clone would waste memory across 18k pools.
            snapshot_seed: None,
            post_drain_snapshot: None,
            cached_tick_ranges: parking_lot::Mutex::new(TickRangeCache::default()),
        }
    }
}

impl V3PoolState {
    /// Directional swap viability — can this pool host a swap in the given
    /// direction with its CURRENT state?
    ///
    /// O(1) using the tick-map extremes; ported from the archived Python
    /// `v3_liquidity_pool.py::swap_is_viable`, keeping its directional
    /// precision (Python's coarse empty-map early-out is subsumed by the
    /// directional test, which an empty map also fails).
    ///
    /// Checks, in order:
    /// 1. uninitialized price (`sqrt_price_x96 == 0`) → not viable;
    /// 2. price at the `MIN`/`MAX_SQRT_RATIO` protocol boundary in the swap's
    ///    direction → not viable (the swap would drive price past the limit);
    /// 3. no initialized tick strictly ahead in the walk direction → not
    ///    viable (a zfo walk descends and needs a position below; ofz ascends
    ///    and needs one above).
    ///
    /// NOTE: unlike `build_int_v3_sequence`, this does NOT consult or populate
    /// the tick-range cache — it is safe to call under contention and costs
    /// two hashmap-extreme scans at worst (amortized O(1) via BTreeMap-free
    /// iteration over keys; tick maps are small).
    #[must_use]
    pub fn swap_is_viable(&self, zero_for_one: bool) -> bool {
        if self.sqrt_price_x96.is_zero() {
            return false;
        }
        let sp = self.sqrt_price_x96;
        if zero_for_one {
            if sp <= U256::from(MIN_SQRT_RATIO) + U256::from(1u64) {
                return false;
            }
            // Viable iff some initialized tick sits strictly below the price.
            match self.tick_data.keys().min() {
                Some(&min_tick) => match get_sqrt_ratio_at_tick_internal(min_tick) {
                    Ok(t) => U256::from(t) < sp,
                    Err(_) => false,
                },
                None => false,
            }
        } else {
            if sp >= U256::from(MAX_SQRT_RATIO) - U256::from(1u64) {
                return false;
            }
            match self.tick_data.keys().max() {
                Some(&max_tick) => match get_sqrt_ratio_at_tick_internal(max_tick) {
                    Ok(t) => U256::from(t) > sp,
                    Err(_) => false,
                },
                None => false,
            }
        }
    }

    /// Default V3/V4 `sqrt_price_limit` bounds (widened `U160` → `U256`).
    ///
    /// The swap cannot cross these regardless of amount. Callers without a
    /// custom limit (the mainline exact-input path) pass this; a custom limit
    /// (arbitrage / exact-output cap) overrides it.
    #[must_use]
    pub fn default_sqrt_price_limit(zero_for_one: bool) -> U256 {
        if zero_for_one {
            U256::from(MIN_SQRT_RATIO) + U256::from(1u64) // 4295128740
        } else {
            U256::from(MAX_SQRT_RATIO) - U256::from(1u64)
        }
    }

    /// Word position (`tick_position(tick.div_euclid(tick_spacing))`) holding
    /// the given tick. Shared by V3/V4 sparse miss-detection.
    #[must_use]
    pub fn word_of(tick: i32, tick_spacing: i32) -> i32 {
        i32::from(tick_position(tick.div_euclid(tick_spacing)).0)
    }

    /// Seed `known_bitmap_words` from the current `tick_data` keys' word
    /// positions. Called at registration for Sparse pools (the keys a partial
    /// snapshot carries are known); a no-op for Tracked pools (detection is
    /// bypassed when `coverage != Sparse`).
    pub fn seed_known_bitmap_words(&mut self, tick_spacing: i32) {
        self.known_bitmap_words = self
            .tick_data
            .keys()
            .map(|&t| Self::word_of(t, tick_spacing))
            .collect();
    }

    /// Monotonic advance of the **price** clock (`update_block`).
    ///
    /// Sets it to `block` when `block > cur`; an equal `block` is an
    /// idempotent no-op. A `block < cur` is a BACKWARD STAMP — an invariant
    /// violation outside a reorg — and panics loudly with a stable, grep-able
    /// literal (ADR-021 fail-fast discipline; two-stamp OB7UNY). The only
    /// sanctioned rewind is `ReorgPoolState::restore_before_block`, which sets
    /// both clocks directly from the journal priors and must NOT call this.
    ///
    /// # Panics
    ///
    /// Panics with `PoolState monotonicity violated: update_block attempted
    /// {block} < current {cur} outside a reorg — ABORT` when `block < cur`.
    pub fn advance_update_block(&mut self, block: u64) {
        assert!(
            block >= self.update_block,
            "PoolState monotonicity violated: update_block attempted {block} < current {} outside a reorg — ABORT",
            self.update_block
        );
        if block > self.update_block {
            self.update_block = block;
        }
    }

    /// Monotonic advance of the **liquidity** clock (`tick_data_block`).
    ///
    /// Same contract as [`Self::advance_update_block`] for the tick-map clock.
    ///
    /// # Panics
    ///
    /// Panics with `PoolState monotonicity violated: tick_data_block attempted
    /// {block} < current {cur} outside a reorg — ABORT` when `block < cur`.
    pub fn advance_tick_data_block(&mut self, block: u64) {
        assert!(
            block >= self.tick_data_block,
            "PoolState monotonicity violated: tick_data_block attempted {block} < current {} outside a reorg — ABORT",
            self.tick_data_block
        );
        if block > self.tick_data_block {
            self.tick_data_block = block;
        }
    }

    // `merge_tick_word` lives on the `ConcentratedLiquidityPoolMut` trait
    // (ADR-017 slice 1) — the body was the byte-identical twin of
    // `V4PoolState::merge_tick_word`; the trait dedups the two. See
    // `impl ConcentratedLiquidityPoolMut for V3PoolState` in `registry.rs`.

    /// Registration/seed genesis anchor (two-stamp OB7UNY fresh-read builder).
    ///
    /// Pushes a `before == after` journal delta at `block` so the reorg
    /// journal is non-empty from registration — keeping `has_state_prior_to`
    /// true so a mid-window reorg restores to the seeded state instead of the
    /// graceful `NoStatePriorToBlock` pump shutdown that an empty journal
    /// would trigger. The delta records the CURRENT scalars + both clocks as
    /// its “before” priors, so a restore pops to the exact seeded state.
    ///
    /// It deliberately advances NO clock: it is the split-seed replacement
    /// for the builder's old `apply_swap`-genesis, which (a) would
    /// backward-panic the PRICE clock when registration seeds `update_block`
    /// at HEAD past the DB map block, and (b) would advance `tick_data_block`
    /// and falsely claim the tick map reaches head. Because `before == after`
    /// it is safe; call once at registration time, mirroring the V2/Curve
    /// genesis-anchor pattern.
    pub fn seed_genesis(&mut self, block: u64) {
        self.journal.push_delta(V3BlockDelta {
            block,
            scalar_priors: Some(ScalarPriors {
                sqrt_price_x96_before: self.sqrt_price_x96,
                liquidity_before: self.liquidity,
                tick_before: self.tick,
            }),
            update_block_before: Some(self.update_block),
            tick_data_block_before: Some(self.tick_data_block),
            tick_priors: Vec::new(),
        });
    }

    /// Construct from registration params with a journal of the given depth.
    #[must_use]
    /// Build the (immutable identity, mutable state) pair for `register_v3_pool`
    /// (ADR-005 identity/state split). Identity carries the immutable config
    /// (`address/tokens/fee/tick_spacing/factory`); state carries mutable
    /// runtime + the pinned verify seeds.
    pub fn from_params(
        params: RegisterV3PoolParams,
        journal_depth: usize,
    ) -> (V3PoolIdentity, V3PoolState) {
        let mut state = Self {
            sqrt_price_x96: params.sqrt_price_x96,
            liquidity: params.liquidity,
            tick: params.tick,
            update_block: params.update_block,
            // Two-stamp OB7UNY: the price clock seeds at `update_block`; the
            // liquidity clock seeds at `tick_data_block` when the caller
            // split them (fresh-read builder), else falls back to the same
            // seed block. The historical-replay guard always keys on the
            // PRICE seed block (`update_block` — the block the SLOT0 scalar,
            // including active `liquidity`, reflects).
            tick_data_block: params.tick_data_block.unwrap_or(params.update_block),
            initial_state_block: params.update_block,
            state_nonce: 0,
            // ADR-close of the rolling-start direct-apply gap (DFQYM5): a
            // freshly-registered `Tracked` pool starts `Quarantined` so NO live
            // event can direct-apply before the two-step verify; `set_pool_live`
            // (post-verify) is the sole transition to `Live`. `Sparse` pools
            // (no complete liquidity map → no pin, no step-2 verify) stay
            // `Live`/direct-apply — quarantining them would defer events with
            // nothing to protect and re-raise the retained-tail flush hazard at
            // `set_live`.
            registration_lifecycle: if params.coverage == PoolTickCoverage::Tracked {
                RegistrationLifecycle::Quarantined
            } else {
                RegistrationLifecycle::Live
            },
            tick_data: params.tick_data,
            coverage: params.coverage,
            known_bitmap_words: HashSet::new(),
            fetcher: params.fetcher,
            journal: ReorgJournal::<V3BlockDelta>::new(journal_depth),
            snapshot_seed: None,
            post_drain_snapshot: None,
            cached_tick_ranges: parking_lot::Mutex::new(TickRangeCache::default()),
        };
        let identity = V3PoolIdentity {
            address: params.address,
            token0: params.token0,
            token1: params.token1,
            fee: params.fee,
            tick_spacing: params.tick_spacing,
            factory: params.factory,
            deployer: params.deployer,
            init_hash: params.init_hash,
        };
        // CBCH6H: pin the snapshot seed for Tracked pools so step-1 verify
        // compares the seed (not pump-mutated `tick_data`) against
        // on-chain@snapshot_block. Sparse pools have no complete seed. Computed
        // AFTER the struct literal because `tick_data` is moved into `state` above.
        state.snapshot_seed = if params.coverage == PoolTickCoverage::Tracked {
            Some(state.tick_data.clone())
        } else {
            None
        };
        // A partial snapshot's tick_data keys are known regions.
        if params.coverage == PoolTickCoverage::Sparse {
            state.seed_known_bitmap_words(params.tick_spacing);
        }
        (identity, state)
    }

    // `apply_swap` + `apply_liquidity_update` live on the
    // `ConcentratedLiquidityPoolMut` trait (ADR-017 slice 2) — the bodies
    // were byte-identical twins across V3/V4; the trait dedups the two.
    // See `impl ConcentratedLiquidityPoolMut for V3PoolState` in `registry.rs`.
    /// Invalidate the cached tick ranges (call after any state mutation).
    pub fn invalidate_tick_range_cache(&self) {
        let mut cache = self.cached_tick_ranges.lock();
        cache.zfo = CachedTickRanges::StillEmpty;
        cache.ofz = CachedTickRanges::StillEmpty;
    }

    /// Get cached tick ranges for the given direction, computing and caching
    /// if absent. The walk visits every initialized tick in the swap direction.
    /// Results are cached per pool+direction so the walk
    /// amortizes to zero per cycle.
    fn get_cached_tick_ranges(
        &self,
        tick_spacing: i32,
        zero_for_one: bool,
    ) -> Option<Arc<[V3TickRangeForSolver]>> {
        {
            let cache = self.cached_tick_ranges.lock();
            let slot = if zero_for_one { &cache.zfo } else { &cache.ofz };
            match slot {
                CachedTickRanges::Hit(ranges) => return Some(Arc::clone(ranges)),
                CachedTickRanges::Miss => return None, // cached negative
                CachedTickRanges::StillEmpty => {}     // fall through to compute
            }
        }

        // Not cached — compute and store
        let ranges = compute_tick_ranges(
            &self.tick_data,
            self.tick,
            tick_spacing,
            self.liquidity,
            zero_for_one,
        )
        .map(|(ranges, _)| Arc::<[V3TickRangeForSolver]>::from(ranges));

        // Cache the result regardless of Some/None (2SGSE3: caching None
        // avoids re-walking pools whose state hasn't changed since the last
        // failed walk).
        let mut cache = self.cached_tick_ranges.lock();
        match ranges {
            Some(ref r) => {
                if zero_for_one {
                    cache.zfo = CachedTickRanges::Hit(Arc::clone(r));
                } else {
                    cache.ofz = CachedTickRanges::Hit(Arc::clone(r));
                }
            }
            None => {
                if zero_for_one {
                    cache.zfo = CachedTickRanges::Miss;
                } else {
                    cache.ofz = CachedTickRanges::Miss;
                }
            }
        }

        ranges
    }

    /// Build an integer V3 tick range sequence using original U256 sqrt prices
    /// and i128→u128 liquidity (no f64 conversion).
    ///
    /// Produces an [`IntV3TickRangeSequence`] suitable for the integer-exact
    /// V3-V3 solver, preserving full precision. Returns `None` if insufficient
    /// tick data. The walk visits every initialized tick in the swap direction;
    /// the solver's own bounds (`iteration_cap`, `prune`, `REFINE_GRID_POINTS`)
    /// handle pathological cost post-hoc.
    #[must_use]
    pub fn build_int_v3_sequence(
        &self,
        tick_spacing: i32,
        fee: u32,
        zero_for_one: bool,
    ) -> Option<IntV3TickRangeSequence> {
        let ranges = self.get_cached_tick_ranges(tick_spacing, zero_for_one)?;
        let use_ranges: &[V3TickRangeForSolver] = &ranges;

        let gamma_numer = u64::from(1_000_000 - fee);
        let fee_denom = 1_000_000u64;

        // Net at the current tick (zfo only). For a zfo swap the V3 swap
        // loop's first `nextInitializedTickWithinOneWord(lte=true)` resolves
        // the CURRENT tick when it is initialized, so the swap sweeps the
        // leading segment [current, sqrt(currentTick)] at the PRE-drain stored
        // liquidity; only upon REACHING sqrt(currentTick) does it cross the
        // tick and apply `if (zeroForOne) liquidityNet = -liquidityNet;` →
        // `state.liquidity -= net(currentTick)` (UniswapV3Pool::swap →
        // Tick.cross → LiquidityMath.addDelta).
        //
        // This function models that leading segment as a dedicated hop at the
        // stored liquidity (below) and applies the net at the crossing into the
        // below ranges via `base = stored - net(currentTick)`. Prior to this
        // the net was folded into range 0's liquidity unconditionally — a
        // compression faithful only for DEEP swaps that reach the boundary. For
        // a SHALLOW swap that partial-fills ABOVE sqrt(currentTick) (e.g. path
        // 13827 DAI/USDC: current tick -276324 carries net -4.27e17, inflating
        // the modeled starting liquidity and over-predicting `v3_simulate_swap`
        // by 1 wei → the `V3_TAKE` overdraft / "IIA" revert class), that
        // compression incorrectly governed the output by the post-drain
        // liquidity. ofz uses `gt` (exclusive) and does NOT re-cross the
        // current tick, so no net applies there.
        let current_tick_net: i128 = if zero_for_one {
            // liquidity_net is I256; the low-128-bit int128 projection
            // (the shared `TickInfo::liquidity_net_i128` — the extraction
            // `compute_tick_ranges` uses too; documents the path-13827
            // 1-wei over-prediction trap).
            self.tick_data
                .get(&self.tick)
                .map_or(0, TickInfo::liquidity_net_i128)
        } else {
            0
        };
        let base_liquidity: i128 = self.liquidity.cast_signed() - current_tick_net;

        let has_leading_current_segment = zero_for_one && self.tick_data.contains_key(&self.tick);

        let mut int_ranges = Vec::with_capacity(use_ranges.len() + 1);

        // Contract-faithful leading segment (zfo + initialized current tick
        // only): the swap's first step targets sqrt(currentTick) while
        // `state.liquidity` is still the stored (pre-drain) value, so model
        // [current, sqrt(currentTick)] as a leading hop at stored liquidity.
        // The below computed ranges then apply the net at the crossing.
        if has_leading_current_segment {
            let tick_sqrt = U256::from(
                get_sqrt_ratio_at_tick_internal(self.tick).unwrap_or(alloy::primitives::U160::ZERO),
            );
            int_ranges.push(IntV3TickRangeHop {
                liquidity: self.liquidity,
                sqrt_price_x96: self.sqrt_price_x96,
                sqrt_price_lower_x96: tick_sqrt,
                sqrt_price_upper_x96: tick_sqrt,
                gamma_numer,
                fee_denom,
                zero_for_one,
                word_boundary_prices: Vec::new(),
            });
        }

        for (i, r) in use_ranges.iter().enumerate() {
            let sqrt_price_x96 = int_range_entry_sqrt_price(
                i,
                has_leading_current_segment,
                zero_for_one,
                self.sqrt_price_x96,
                self.tick,
                use_ranges,
            );
            let range_liquidity = int_range_liquidity(i, base_liquidity, zero_for_one, use_ranges);

            int_ranges.push(IntV3TickRangeHop {
                liquidity: range_liquidity,
                sqrt_price_x96,
                sqrt_price_lower_x96: r.sqrt_price_lower,
                sqrt_price_upper_x96: r.sqrt_price_upper,
                gamma_numer,
                fee_denom,
                zero_for_one,
                // Convert the interior word-boundary ticks `compute_tick_ranges`
                // collapsed out of this range into sqrt prices (swap order) so
                // the solver's `compute_crossing` / `int_simulate_v3_swap` can
                // re-walk them per boundary, restoring the per-step
                // `computeSwapStep` flooring (ergo E7ALWT).
                word_boundary_prices: r
                    .interior_boundaries
                    .iter()
                    .map(|&t| {
                        U256::from(
                            degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal(t)
                                .unwrap_or(alloy::primitives::U160::ZERO),
                        )
                    })
                    .collect(),
            });
        }

        IntV3TickRangeSequence::new(int_ranges).ok()
    }
}

/// Entry sqrt price for computed range `i` of an integer V3 walk.
///
/// Range 0 enters at sqrt(currentTick) when a leading current-tick segment
/// was modeled (the leading hop already covers [current, sqrt(currentTick)]);
/// otherwise it enters at the pool's current sqrt price. Later ranges enter
/// at the previous range's swap-direction boundary.
fn int_range_entry_sqrt_price(
    i: usize,
    has_leading_current_segment: bool,
    zero_for_one: bool,
    current_sqrt_price_x96: U256,
    current_tick: i32,
    use_ranges: &[V3TickRangeForSolver],
) -> U256 {
    if i == 0 {
        if has_leading_current_segment {
            // leading hop already starts at current; this first computed
            // range enters at sqrt(currentTick)
            U256::from(
                get_sqrt_ratio_at_tick_internal(current_tick)
                    .unwrap_or(alloy::primitives::U160::ZERO),
            )
        } else {
            current_sqrt_price_x96
        }
    } else if zero_for_one {
        use_ranges[i - 1].sqrt_price_upper
    } else {
        use_ranges[i - 1].sqrt_price_lower
    }
}

/// Stored-liquidity-adjusted liquidity of computed range `i`.
///
/// Range 0 carries the leading-segment-corrected base liquidity; each later
/// range applies every prior range's boundary net in swap order, flooring
/// negative results at zero.
fn int_range_liquidity(
    i: usize,
    base_liquidity: i128,
    zero_for_one: bool,
    use_ranges: &[V3TickRangeForSolver],
) -> u128 {
    if i == 0 {
        if base_liquidity < 0 {
            0
        } else {
            base_liquidity.cast_unsigned()
        }
    } else {
        let mut l = base_liquidity;
        for prev_range in &use_ranges[..i] {
            let net = prev_range.liquidity_net;
            if zero_for_one {
                l -= net;
            } else {
                l += net;
            }
        }
        if l.is_negative() {
            0u128
        } else {
            l.cast_unsigned()
        }
    }
}

// ADR-016 — pool-owned reorg rollback for the CL family. The field-write
// previously duplicated across `BotState::v3_restore_before_block` /
// `v4_restore_before_block` (scalar-priors write-back + tick reverse-apply)
// is absorbed into the state struct; `V3RestoreResult` stays internal to
// this impl and never escapes. The CL journal's `restore_before_block`
// returns `V3RestoreResult` directly (panics on empty — the empty-journal
// case is an invariant violation for a registered pool, not a recoverable
// error), so this impl returns `Ok(())` after applying the landed-at state.
// Byte-identical to the V4 impl modulo the struct name. See ADR-016 D4.
impl ReorgPoolState for V3PoolState {
    fn restore_before_block(&mut self, block: u64) -> Result<(), JournalError> {
        let result = self.journal.restore_before_block(block);

        // Sync scalar fields if the rolled-back range had scalar changes.
        // If scalar_priors is None (tick-only event(s) rolled back), the
        // current slot0 scalars were never changed by the rolled-back events
        // and are already correct — skip the write-back. See ADR-004.
        if let Some(p) = &result.scalar_priors {
            self.sqrt_price_x96 = p.sqrt_price_x96_before;
            self.liquidity = p.liquidity_before;
            self.tick = p.tick_before;
        }
        // Reorg is the sole sanctioned rewind of both clocks: restore each to
        // its exact pre-target value from the rolled-back range's priors
        // (two-stamp OB7UNY). A `None` prior means the rolled-back events did
        // not advance that clock — its current value is already correct.
        if let Some(b) = result.update_block_before {
            self.update_block = b;
        }
        if let Some(b) = result.tick_data_block_before {
            self.tick_data_block = b;
        }
        self.state_nonce = self.state_nonce.wrapping_add(1);
        self.invalidate_tick_range_cache();

        // Reverse-apply tick priors accumulated across all popped deltas.
        for (tick_idx, tick_before) in &result.tick_priors {
            match tick_before.liquidity_gross_before {
                Some(gross_before) => {
                    // Tick existed before — restore its prior values.
                    self.tick_data.insert(
                        *tick_idx,
                        TickInfo {
                            liquidity_gross: gross_before,
                            liquidity_net: tick_before.liquidity_net_before,
                            block: 0,
                        },
                    );
                }
                None => {
                    // Tick was newly initialized in this block — remove it.
                    self.tick_data.remove(tick_idx);
                }
            }
        }

        Ok(())
    }

    fn discard_before_block(&mut self, block: u64) -> Result<(), JournalError> {
        self.journal.discard_before_block(block)
    }

    fn journal_len(&self) -> usize {
        self.journal.len()
    }

    fn newest_block(&self) -> Option<u64> {
        self.journal.newest_block()
    }
}

// ---------------------------------------------------------------------------
// V3 single-pool swap simulation (ADR-003: "Pool's authority over its own math")
// ---------------------------------------------------------------------------

/// Result of a single-pool V3 swap simulation.
///
/// `amount0` is the token0 delta (positive = pool received token0, i.e. the
/// swapper paid token0). `amount1` is the token1 delta. The sign convention
/// matches Uniswap V3's `Swap` callback: for an exact-input swap,
/// `zero_for_one` swaps pay token0 (`amount0 > 0`) and receive token1
/// (`amount1 < 0`); both magnitudes are returned unsigned here.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct V3SwapOutcome {
    /// Absolute token0 amount moved (input for zfo swaps, output for ofz).
    pub amount0: U256,
    /// Absolute token1 amount moved (output for zfo swaps, input for ofz).
    pub amount1: U256,
    /// Final `sqrtPriceX96` after the swap walk (ADR-005 slice 3b: the
    /// companion's `simulate_exact_input_swap` builds `final_state` from this).
    pub sqrt_price_x96: U256,
    /// Final active liquidity after the swap walk.
    pub liquidity: u128,
    /// Final tick after the swap walk.
    pub tick: i32,
    /// The gross amount of the *input* token the swap actually consumed, in
    /// the input token's units.
    ///
    /// For an **exact-input** swap this is the pool's max-convertible input: if
    /// it is `< |amount_specified|` the requested input was NOT fully consumed
    /// (the walk hit the price limit with input left over — the pool could not
    /// convert the whole amount). That leftover is the over-swap signal: a
    /// solver must clamp its CL-hop input to this value (with a rounding margin)
    /// so the exact-in loop terminates on `amountRemaining == 0` at the last
    /// funded tick instead of marching empty bitmap words to the price limit
    /// (the path-5000 20M-gas EMPTY-HALT). For an **exact-output** swap it is
    /// the input required to produce the requested output (≤ the input that an
    /// unbounded exact-in would spend). Identical to Solidity's
    /// `amountSpecified - amountSpecifiedRemaining`.
    pub input_consumed: U256,
}

impl V3SwapOutcome {
    /// For an exact-input swap requested at `gross_input`, return the largest
    /// input the pool can actually convert — i.e. the solver's clamp bound.
    ///
    /// Returns `Some(limit)` when the request exceeds capacity (`input_consumed
    /// < gross_input`), `None` when the input was fully consumed (no clamp
    /// needed — the caller may keep the default min/max price limit and the
    /// exact-in loop exits on `amountRemaining == 0`). Callers should apply a
    /// `margin` below the returned bound to absorb solver-vs-engine rounding at
    /// the capacity boundary (`margin` subtracted here, so a 1-wei over-prediction
    /// cannot re-trigger the EMPTY march). `gross_input` is the absolute value of
    /// the exact-input `amount_specified` (the input token's side).
    #[must_use]
    pub fn exact_input_clamp_bound(&self, gross_input: U256, margin: U256) -> Option<U256> {
        (self.input_consumed < gross_input).then(|| self.input_consumed.saturating_sub(margin))
    }
}

#[cfg(test)]
mod exact_input_clamp_tests {
    //! Unit tests for `V3SwapOutcome::exact_input_clamp_bound` — the solver's
    //! CL-hop input clamp that prevents over-feeding a pool past its capacity
    //! (the path-5000 20M-gas EMPTY-HALT class, AGENTS.md UO3JM4).
    use super::V3SwapOutcome;
    use alloy::primitives::U256;

    fn outcome(input_consumed: u128) -> V3SwapOutcome {
        V3SwapOutcome {
            amount0: U256::ZERO,
            amount1: U256::ZERO,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 0,
            tick: 0,
            input_consumed: U256::from(input_consumed),
        }
    }

    #[test]
    fn returns_none_when_input_fully_consumed() {
        // The pool converted the entire 1000 input — no over-feed, no clamp.
        let o = outcome(1000);
        assert_eq!(
            o.exact_input_clamp_bound(U256::from(1000), U256::from(0)),
            None
        );
    }

    #[test]
    fn caps_at_consumed_minus_margin() {
        // Pool could only convert 900 of the 1000 requested — clamp to ~900.
        let o = outcome(900);
        assert_eq!(
            o.exact_input_clamp_bound(U256::from(1000), U256::from(0)),
            Some(U256::from(900))
        );
        // margin absorbs solver-vs-engine rounding at the capacity boundary.
        assert_eq!(
            o.exact_input_clamp_bound(U256::from(1000), U256::from(500)),
            Some(U256::from(400))
        );
        // margin >= capacity saturates to 0.
        assert_eq!(
            o.exact_input_clamp_bound(U256::from(1000), U256::from(21000)),
            Some(U256::ZERO)
        );
    }

    #[test]
    fn exact_capacity_is_not_clamped() {
        // Request == capacity → fully consumed → the loop exits on
        // amountRemaining == 0 → clamp is a no-op.
        let o = outcome(1000);
        assert_eq!(
            o.exact_input_clamp_bound(U256::from(1000), U256::from(0)),
            None
        );
    }

    #[test]
    fn nothing_convertible_caps_to_zero() {
        let o = outcome(0);
        assert_eq!(
            o.exact_input_clamp_bound(U256::from(500), U256::from(0)),
            Some(U256::ZERO)
        );
    }
}

/// Why a [`v3_simulate_swap`] / [`v4_simulate_swap`] call could not produce a
/// trustworthy outcome. Shared by V3 and V4.
///
/// `NotComputable` covers the non-fetchable failures the previous
/// `Option::None` return encoded (zero amount, arithmetic overflow, invariant
/// violation). `MissingTickWord` is the new fetchable sparse-map miss
/// (ADR-005 sparse-map feature parity): the walk entered a tick-bitmap word
/// whose bitmap has not been fetched, so the caller must fetch it via the
/// registered tick-data fetcher and retry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimulateSwapError {
    /// The swap is not computable (zero amount, arithmetic overflow, invariant).
    NotComputable,
    /// Sparse coverage: the walk entered tick-bitmap word `i32` that has not
    /// been fetched. Mirrors Python's `MissingLiquidityData(word=...)`.
    MissingTickWord(i32),
}

/// Simulate a V3 exact-input or exact-output swap against a pool's state.
///
/// Port of `UniswapV3Pool._calculate_swap` / `degenbot.uniswap.concentrated.
/// v3_simulator.calculate_swap`. Walks initialized ticks via [`gen_ticks`],
/// runs [`compute_swap_step_v3`] per tick range, and crosses liquidity-net at
/// initialized boundaries. Pure (no state mutation — reads `&V3PoolState`);
/// the solve engine + the calc API both read by reference per ADR-003.
///
/// `amount_specified` uses the V3 sign convention: positive = exact input,
/// negative = exact output.
///
/// # Errors
///
/// Returns [`SimulateSwapError::NotComputable`] if the amount is zero, the
/// swap cannot be computed (tick math / swap-step error, liquidity overflow),
/// or an invariant is violated. Returns [`SimulateSwapError::MissingTickWord`]
/// when `coverage == Sparse` and the walk entered a tick-bitmap word whose
/// bitmap has not been fetched — the caller must fetch word `w` via the
/// registered tick-data fetcher and retry (ADR-005 sparse-map parity).
///
/// See: `contract_reference/uniswap/V3/UniswapV3Factory.sol` (`SwapMath`,
/// `TickBitmap`, `TickMath`).
// `tick` tracks the contract's post-step tick; kept faithful to the V3
// `_calculate_swap` loop even though this pure simulator returns only amounts.
#[expect(clippy::too_many_lines)] // faithful port of V3's `_calculate_swap`; splitting would obscure the loop.
pub fn v3_simulate_swap(
    state: &V3PoolState,
    fee: u32,
    tick_spacing: i32,
    zero_for_one: bool,
    amount_specified: I256,
    sqrt_price_limit: U256,
) -> Result<V3SwapOutcome, SimulateSwapError> {
    if amount_specified.is_zero() {
        return Err(SimulateSwapError::NotComputable); // AS: zero amount (V3 reverts)
    }
    let exact_in = amount_specified.is_positive();

    let mut amount_specified_remaining = amount_specified;
    let mut amount_calculated = I256::ZERO;
    let mut sqrt_price_x96 = state.sqrt_price_x96;
    let mut tick = state.tick;
    let mut liquidity =
        i128::try_from(state.liquidity).map_err(|_| SimulateSwapError::NotComputable)?;

    let fee_pips = U256::from(fee);
    // `tick_spacing` is the immutable-config parameter (threaded from identity).

    // Sparse-map miss detection (ADR-005 feature parity). In sparse mode a
    // region is unknown unless its word key is in `known_bitmap_words`. The
    // Python simulator checks the starting word first via
    // `next_initialized_tick_within_one_word`; mirror that: if the word
    // containing the current tick is unknown, signal a fetchable miss before
    // trusting any per-step result derived from `gen_ticks` (which, lacking a
    // bitmap store, would otherwise silently produce a wrong amount here).
    let sparse = state.coverage == PoolTickCoverage::Sparse;
    if sparse
        && !state
            .known_bitmap_words
            .contains(&V3PoolState::word_of(tick, tick_spacing))
    {
        return Err(SimulateSwapError::MissingTickWord(V3PoolState::word_of(
            tick,
            tick_spacing,
        )));
    }

    // Walk initialized + word-boundary ticks in the swap direction. gen_ticks
    // yields ticks in swap order (descending for zfo, ascending for ofz).
    // Cap iterations to bound pathological loops; the V3 contract's loop also
    // terminates at MIN/MAX_TICK.
    let ticks = gen_ticks_iter(
        &state.tick_data,
        tick,
        tick_spacing,
        zero_for_one, // less_than_or_equal matches swap direction
        30_000,
    )
    .map_err(|_| SimulateSwapError::NotComputable)?;

    for tick_along_path in ticks {
        if amount_specified_remaining.is_zero() || sqrt_price_x96 == sqrt_price_limit {
            break;
        }

        let mut tick_next = tick_along_path.tick;
        let initialized = tick_along_path.is_initialized;

        // Clamp to the V3 tick bounds (mirrors `_calculate_swap`).
        tick_next = if zero_for_one {
            tick_next.max(-887_272)
        } else {
            tick_next.min(887_272)
        };

        let sqrt_price_next = U256::from(
            get_sqrt_ratio_at_tick_internal(tick_next)
                .map_err(|_| SimulateSwapError::NotComputable)?,
        );

        // Target price: clamp to the swap price-limit if the next tick would
        // cross it.
        let sqrt_price_target = if (zero_for_one && sqrt_price_next < sqrt_price_limit)
            || (!zero_for_one && sqrt_price_next > sqrt_price_limit)
        {
            sqrt_price_limit
        } else {
            sqrt_price_next
        };

        let sqrt_price_start = sqrt_price_x96;
        let step = compute_swap_step_v3(
            sqrt_price_x96,
            sqrt_price_target,
            liquidity,
            amount_specified_remaining,
            fee_pips,
        )
        .map_err(|_| SimulateSwapError::NotComputable)?;

        sqrt_price_x96 = step.sqrt_price_next;

        if exact_in {
            // Gross input consumed this step = amount_in + fee_amount.
            let consumed = I256::try_from(step.amount_in.saturating_add(step.fee_amount))
                .map_err(|_| SimulateSwapError::NotComputable)?;
            amount_specified_remaining = amount_specified_remaining
                .checked_sub(consumed)
                .ok_or(SimulateSwapError::NotComputable)?;
            amount_calculated = amount_calculated
                .checked_sub(
                    I256::try_from(step.amount_out)
                        .map_err(|_| SimulateSwapError::NotComputable)?,
                )
                .ok_or(SimulateSwapError::NotComputable)?;
        } else {
            amount_specified_remaining = amount_specified_remaining
                .checked_add(
                    I256::try_from(step.amount_out)
                        .map_err(|_| SimulateSwapError::NotComputable)?,
                )
                .ok_or(SimulateSwapError::NotComputable)?;
            let gross_input = I256::try_from(step.amount_in.saturating_add(step.fee_amount))
                .map_err(|_| SimulateSwapError::NotComputable)?;
            amount_calculated = amount_calculated
                .checked_add(gross_input)
                .ok_or(SimulateSwapError::NotComputable)?;
        }

        if sqrt_price_x96 == sqrt_price_next {
            // The walk reached `tick_next` — crossing into its word. In sparse
            // mode a region is unknown unless its word key is in
            // `known_bitmap_words`; an unfetched word means the result past
            // this crossing would be untrustworthy, so signal a fetchable miss
            // (caller fetches + retries) rather than reading potentially-absent
            // tick_data or proceeding on an unknown region. Mirrors Python's
            // per-step `MissingLiquidityData` raise on word entry. The check is
            // gated on an actual crossing so a merely-proposed (unreached)
            // boundary tick in a neighbouring word does not false-trigger.
            if sparse
                && !state
                    .known_bitmap_words
                    .contains(&V3PoolState::word_of(tick_next, tick_spacing))
            {
                return Err(SimulateSwapError::MissingTickWord(V3PoolState::word_of(
                    tick_next,
                    tick_spacing,
                )));
            }
            // Reached the next tick — cross it if initialized.
            if initialized {
                if let Some(info) = state.tick_data.get(&tick_next) {
                    let liquidity_net_i256 = info.liquidity_net;
                    // Crossing direction: zfo crosses from above (subtract net);
                    // ofz crosses from below (add net). Matches V3's
                    // `liquidity = liquidity - liquidityNet` (zfo) branch.
                    let liquidity_net: i128 = i128::try_from(liquidity_net_i256)
                        .map_err(|_| SimulateSwapError::NotComputable)?;
                    let net = if zero_for_one {
                        -liquidity_net
                    } else {
                        liquidity_net
                    };
                    liquidity = liquidity
                        .checked_add(net)
                        .ok_or(SimulateSwapError::NotComputable)?;
                    if liquidity < 0 {
                        return Err(SimulateSwapError::NotComputable); // LO: invariant violated
                    }
                }
            }
            tick = if zero_for_one {
                tick_next - 1
            } else {
                tick_next
            };
        } else if sqrt_price_x96 != sqrt_price_start {
            // Price moved but didn't reach tick_next — recompute tick from price.
            tick = get_tick_at_sqrt_ratio_internal(sqrt_price_x96.to::<U160>())
                .map_err(|_| SimulateSwapError::NotComputable)?
                .as_i32();
            // Slice-4 fix (V3 mirror of the V4 ELSE-branch miss check): an
            // amount-capped step can land inside an UNFETCHED word whose
            // initialized ticks `gen_ticks` never proposed (absent from
            // `tick_data`). The CROSS branch above guards its `tick_next`; this
            // branch derived `tick` from the price with no word-knownness check
            // — so the walk could terminate having skipped that word's
            // liquidity-nets, producing a short output. Raise `MissingTickWord`
            // so the fetch+retry loop backfills the word; on re-run `gen_ticks`
            // proposes its ticks as `init=true` and they are crossed + applied
            // like the dense path. See the V4 corpus fixture
            // (`test_rust_v4_sparse_fetch_corpus_diverges_from_dense`).
            if sparse
                && !state
                    .known_bitmap_words
                    .contains(&V3PoolState::word_of(tick, tick_spacing))
            {
                return Err(SimulateSwapError::MissingTickWord(V3PoolState::word_of(
                    tick,
                    tick_spacing,
                )));
            }
        }
    }

    // Derive amount0 / amount1 per the V3 Swap callback convention.
    // input_consumed = amount_specified - amount_remaining (positive for
    // both exact-in and exact-out).
    let input_consumed = amount_specified
        .checked_sub(amount_specified_remaining)
        .ok_or(SimulateSwapError::NotComputable)?;

    let (amount0_signed, amount1_signed) = if zero_for_one == exact_in {
        // zfo + exact_in  → pool receives token0 (input),  sends token1 (output)
        // ofz + exact_out → pool sends token0 (output),  receives token1 (input)
        (input_consumed, -amount_calculated)
    } else {
        (-amount_calculated, input_consumed)
    };

    Ok(V3SwapOutcome {
        amount0: amount0_signed.unsigned_abs(),
        amount1: amount1_signed.unsigned_abs(),
        sqrt_price_x96,
        liquidity: u128::try_from(liquidity.max(0)).unwrap_or(0),
        tick,
        input_consumed: input_consumed.unsigned_abs(),
    })
}

// ===========================================================================
// Tests for the relocated apply methods (ADR-014 D1 — CL half of Q1).
// These exercise `V3PoolState::apply_swap` / `apply_liquidity_update` directly
// against a constructed state, with no `BotState` registry, no buffer, no
// address index — the unit layer for the CL-family apply contract. The
// integration coverage that previously lived on `BotState` (exercise through
// the registry dispatch) stays in `bot_core/v3_state.rs` / `bot_core/mod.rs`
// as the dispatch regression net.
// ===========================================================================
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    unused_imports
)]
#[cfg(test)]
mod apply_inherent_tests {
    use super::*;
    use crate::registry::ConcentratedLiquidityPoolMut;
    use crate::state_history::{ReorgJournal, ScalarPriors, TickBefore, V3BlockDelta};
    use alloy::primitives::{I256, U128, U256};
    use hashbrown::{HashMap, HashSet};

    /// Minimal V3 state at tick 0, 1:1 price, liquidity `liq`, with a
    /// [-60, +60] position (so `tick_data` has ticks -60 and +60 initialized).
    /// The journal is fresh (depth 8); snapshot/pinned fields are `None`.
    fn state_with_position(liq: u128) -> V3PoolState {
        let sp_0 = U256::from(1u128) << 96;
        let liq_u128 = U256::from(liq).to::<U128>();
        let mut tick_data = HashMap::new();
        tick_data.insert(
            -60,
            TickInfo {
                liquidity_gross: liq_u128,
                liquidity_net: I256::try_from(i128::try_from(liq).unwrap()).unwrap(),
                block: 0,
            },
        );
        tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: liq_u128,
                liquidity_net: I256::try_from(-i128::try_from(liq).unwrap()).unwrap(),
                block: 0,
            },
        );
        V3PoolState {
            sqrt_price_x96: sp_0,
            liquidity: liq,
            tick: 0,
            update_block: 0,
            tick_data_block: 0,
            initial_state_block: 0,
            state_nonce: 0,
            registration_lifecycle: RegistrationLifecycle::default(),
            tick_data,
            coverage: PoolTickCoverage::Tracked,
            known_bitmap_words: HashSet::new(),
            fetcher: None,
            journal: ReorgJournal::<V3BlockDelta>::new(8),
            snapshot_seed: None,
            post_drain_snapshot: None,
            cached_tick_ranges: parking_lot::Mutex::new(TickRangeCache::default()),
        }
    }

    /// In-range swap (same tick, empty `tick_priors`) preserves the tick-range
    /// cache: the walk result depends on `tick_data` + `current_tick`, neither of
    /// which changed.
    #[test]
    fn apply_swap_in_range_preserves_cache() {
        let mut state = state_with_position(10_000_000_000_000u128);

        // Populate the cache.
        let _ = state.get_cached_tick_ranges(60, true);
        {
            let cache = state.cached_tick_ranges.lock();
            assert!(
                matches!(cache.zfo, CachedTickRanges::Hit(_)),
                "cache populated"
            );
        }

        // Apply a swap that stays at tick 0 (same tick, no tick_priors).
        state.apply_swap(
            U256::from(2u128) << 95,
            11_000_000_000_000u128,
            0, // SAME tick
            1,
            &[],
        );

        // The cache must be preserved.
        let cache = state.cached_tick_ranges.lock();
        assert!(
            matches!(cache.zfo, CachedTickRanges::Hit(_)),
            "in-range swap must preserve the tick-range cache"
        );
    }

    /// Tick-crossing swap (different tick) invalidates the cache.
    #[test]
    fn apply_swap_cross_tick_invalidates_cache() {
        let mut state = state_with_position(10_000_000_000_000u128);

        // Populate the cache.
        let _ = state.get_cached_tick_ranges(60, true);
        {
            let cache = state.cached_tick_ranges.lock();
            assert!(matches!(cache.zfo, CachedTickRanges::Hit(_)));
        }

        // Apply a swap that crosses into tick 1 (different tick).
        state.apply_swap(
            U256::from(2u128) << 96,
            11_000_000_000_000u128,
            1, // tick CHANGED
            1,
            &[],
        );

        let cache = state.cached_tick_ranges.lock();
        assert!(
            matches!(cache.zfo, CachedTickRanges::StillEmpty),
            "tick-crossing swap must invalidate the cache"
        );
    }

    #[test]
    fn build_int_v3_sequence_is_not_truncated_at_twentyfour_ranges() {
        // Pin 4b05cb17e (max_ranges / WALK_CEILING removal): a pool with 30
        // initialized ticks in one swap direction must yield MORE than 24
        // ranges. Pre-removal the feed was truncated at 24, which is exactly
        // the shape the pre-removal golden captures (369 corpus) froze in.
        let sp_0 = U256::from(1u128) << 96;
        let mut tick_data = HashMap::new();
        for i in 1..=30 {
            let net: i128 = if i % 2 == 0 {
                -100_000_000_000
            } else {
                100_000_000_000
            };
            tick_data.insert(
                60 * i,
                TickInfo {
                    liquidity_gross: U128::from(200_000_000_000u128),
                    liquidity_net: I256::try_from(net).unwrap(),
                    block: 0,
                },
            );
        }
        let state = V3PoolState {
            sqrt_price_x96: sp_0,
            liquidity: 1_000_000_000_000u128,
            tick: 0,
            tick_data,
            ..state_with_position(1_000_000_000_000u128)
        };

        // ofz walks UP from tick 0 through every initialized tick above it.
        let seq = state
            .build_int_v3_sequence(60, 3_000, false)
            .expect("30 initialized ticks above the current tick build a sequence");
        assert!(
            seq.ranges.len() > 24,
            "sequence must not be truncated at 24 ranges; got {}",
            seq.ranges.len()
        );
    }

    #[test]
    fn apply_swap_updates_scalars_advances_block_invalidates_cache_and_journals_priors() {
        // What: apply_swap must (1) overwrite the slot0 scalars with the
        // event values, (2) advance update_block, (3) seed tick_priors into
        // tick_data, (4) push a V3BlockDelta carrying scalar_priors: Some(..)
        // AND the tick priors (with the pre-swap gross/net), (5) invalidate
        // the cached tick ranges.
        // Why: this is the journal-capture-then-mutate contract the reorg
        // rollback reverses; without the scalar priors, restore could not
        // rewind the slot0 head.
        let liq = 1_000_000u128;
        let mut state = state_with_position(liq);

        // New tick info for tick 100 (a tick not present before) — prior must
        // be recorded as "did not exist" (gross_before: None, net_before: 0).
        let new_tick_info = TickInfo {
            liquidity_gross: U256::from(500u64).to::<U128>(),
            liquidity_net: I256::try_from(500i128).unwrap(),
            block: 7,
        };
        let tick_priors = vec![(100, new_tick_info.clone())];

        let new_sqrt = U256::from(2u128) << 96;
        let before_len = state.journal.len();

        state.apply_swap(new_sqrt, liq + 1, 1, 7, &tick_priors);

        // (1) scalars updated.
        assert_eq!(state.sqrt_price_x96, new_sqrt);
        assert_eq!(state.liquidity, liq + 1);
        assert_eq!(state.tick, 1);
        // (2) update_block advanced.
        assert_eq!(state.update_block, 7);
        // (3) tick_priors seeded into tick_data.
        assert_eq!(state.tick_data.get(&100), Some(&new_tick_info));
        // (4) journal gained exactly one delta at block 7.
        assert_eq!(state.journal.len(), before_len + 1);
        assert_eq!(state.journal.newest_block(), Some(7));
        // (5) cache invalidated (both directions cleared).
        {
            let cache = state.cached_tick_ranges.lock();
            assert!(
                matches!(cache.zfo, CachedTickRanges::StillEmpty),
                "zfo cache must be invalidated"
            );
            assert!(
                matches!(cache.ofz, CachedTickRanges::StillEmpty),
                "ofz cache must be invalidated"
            );
        }
    }

    #[test]
    fn apply_swap_advances_both_clocks() {
        // OB7UNY: a Swap rewrites the slot0 head AND crosses ticks, so it
        // advances BOTH the price clock and the liquidity clock.
        let mut state = state_with_position(1_000_000u128);
        assert_eq!(state.update_block, 0);
        assert_eq!(state.tick_data_block, 0);
        state.apply_swap(U256::from(2u128) << 96, 1_000_001, 1, 7, &[]);
        assert_eq!(state.update_block, 7, "price clock advanced by the swap");
        assert_eq!(
            state.tick_data_block, 7,
            "liquidity clock advanced by the swap"
        );
    }

    #[test]
    #[should_panic(expected = "monotonicity violated: update_block")]
    fn apply_swap_backward_block_panics() {
        // OB7UNY monotonicity: applying a Swap at a block BELOW the current
        // price clock is a backward stamp → must fail loudly, never silently
        // regress the clock.
        let mut state = state_with_position(1_000_000u128);
        state.update_block = 10;
        state.apply_swap(U256::from(2u128) << 96, 1_000_001, 1, 5, &[]);
    }

    #[test]
    fn out_of_range_liquidity_advances_only_liquidity_clock() {
        // OB7UNY: an out-of-range Mint/Burn mutates the tick map (liquidity
        // clock advances) but leaves the slot0 head byte-identical (price clock
        // does NOT move).
        let mut state = state_with_position(1_000_000u128);
        state.tick = 500; // out of [-60, 60)
        let price_clock = state.update_block;
        state.apply_liquidity_update(-60, 60, 123_456i128, 9);
        assert_eq!(state.tick_data_block, 9, "liquidity clock advances");
        assert_eq!(
            state.update_block, price_clock,
            "out-of-range event leaves the price clock untouched"
        );
    }

    #[test]
    #[should_panic(expected = "monotonicity violated: tick_data_block")]
    fn replace_tick_data_backward_block_panics() {
        // OB7UNY monotonicity on the liquidity clock: replacing the tick map
        // with data from a block BELOW the current liquidity clock is a
        // backward stamp → panic loudly.
        let mut state = state_with_position(1_000_000u128);
        state.tick_data_block = 10;
        let empty: HashMap<i32, TickInfo> = HashMap::new();
        state.replace_tick_data(empty, 5, 60);
    }

    #[test]
    fn apply_liquidity_update_out_of_range_is_tick_only_journals_no_scalars() {
        // What: an OUT-OF-RANGE Mint/Burn (the applied [lower, upper) region
        // does NOT straddle the current tick) must (1) apply the delta to BOTH
        // boundary ticks per Solidity Tick.update (gross += at both, net += at
        // lower, net -= at upper), (2) advance update_block, (3) push a
        // V3BlockDelta with scalar_priors: None (no active-scalar change), (4)
        // journal the two tick priors captured BEFORE mutation, (5) NOT change
        // sqrt_price / liquidity / tick.
        // Why: only IN-RANGE events adjust the active `liquidity` scalar;
        // out-of-range events are tick-only (ADR-004 — restore skips the
        // scalar write-back for these deltas).
        let liq = 1_000_000u128;
        let mut state = state_with_position(liq);
        // Move the current tick outside [-60, 60) so the applied range is
        // out-of-range and cannot touch the active scalar.
        state.tick = 500;

        let sp_before = state.sqrt_price_x96;
        let liq_before = state.liquidity;
        let tick_before = state.tick;

        // Capture the pre-mutation tick info at -60 / +60 so we can assert the
        // journal captured these "before" values.
        let prior_lower = state.tick_data.get(&-60).cloned().unwrap();
        let prior_upper = state.tick_data.get(&60).cloned().unwrap();

        let delta = 123_456i128;
        let before_len = state.journal.len();

        state.apply_liquidity_update(-60, 60, delta, 9);

        // (1) tick_data mutated per Tick.update.
        let after_lower = state.tick_data.get(&-60).unwrap();
        let after_upper = state.tick_data.get(&60).unwrap();
        // gross += at both boundaries.
        assert_eq!(
            after_lower.liquidity_gross,
            prior_lower.liquidity_gross + U256::from(u128::try_from(delta).unwrap()).to::<U128>()
        );
        assert_eq!(
            after_upper.liquidity_gross,
            prior_upper.liquidity_gross + U256::from(u128::try_from(delta).unwrap()).to::<U128>()
        );
        // net += at lower, net -= at upper.
        assert_eq!(
            after_lower.liquidity_net,
            prior_lower.liquidity_net + I256::try_from(delta).unwrap()
        );
        assert_eq!(
            after_upper.liquidity_net,
            prior_upper.liquidity_net - I256::try_from(delta).unwrap()
        );
        // The mutating helper advances the tick's `block` field too.
        assert_eq!(after_lower.block, 9);
        assert_eq!(after_upper.block, 9);
        // (2) OB7UNY: out-of-range → only the LIQUIDITY clock advances; the
        // price clock does NOT move (slot0 head byte-identical).
        assert_eq!(state.tick_data_block, 9);
        assert_eq!(state.update_block, 0);
        // (5) slot0 scalars UNCHANGED (out-of-range → no active adjust).
        assert_eq!(state.sqrt_price_x96, sp_before);
        assert_eq!(state.liquidity, liq_before);
        assert_eq!(state.tick, tick_before);
        // (3)/(4) journal gained exactly one delta at block 9.
        assert_eq!(state.journal.len(), before_len + 1);
        assert_eq!(state.journal.newest_block(), Some(9));
    }

    #[test]
    fn apply_liquidity_update_in_range_mint_adjusts_active_liquidity_and_restores() {
        // What: an IN-RANGE Mint (tick_lower <= current tick < tick_upper)
        // adds the delta to the ACTIVE `liquidity` scalar on top of the
        // boundary-tick map mutation (parity with the on-chain `liquidity`
        // field and the pure reference
        // `degenbot-concentrated-liquidity-math::apply_liquidity_mapping_update`). The journal
        // delta carries `scalar_priors: Some(..)` so a reorg restore rolls the
        // scalar back to its pre-event value.
        // Why: without the adjust the solver's active-liquidity scalar drifts
        // from on-chain even though sqrt_price/tick stay identical — an
        // in-range Mint leaves it too low, an in-range Burn too high.
        let liq = 1_000_000u128;
        let mut state = state_with_position(liq); // current tick 0 ∈ [-60, 60)
        let pre_liq = state.liquidity;
        let pre_sqrt = state.sqrt_price_x96;
        let delta = 123_456i128;

        state.apply_liquidity_update(-60, 60, delta, 9);

        assert_eq!(
            state.liquidity,
            pre_liq + 123_456,
            "in-range mint adds to active liquidity"
        );
        // A liquidity event does not move the price head.
        assert_eq!(state.sqrt_price_x96, pre_sqrt);
        assert_eq!(state.tick, 0);
        assert_eq!(state.update_block, 9);

        // Reorg restore rolls the in-range scalar adjust back.
        let res: Result<(), JournalError> = state.restore_before_block(9);
        assert!(res.is_ok());
        assert_eq!(
            state.liquidity, pre_liq,
            "restore undoes the in-range scalar adjust"
        );
        assert_eq!(state.sqrt_price_x96, pre_sqrt);
        assert_eq!(state.tick, 0);
    }

    #[test]
    fn apply_liquidity_update_in_range_burn_reduces_active_liquidity() {
        // What: an IN-RANGE Burn (negative delta) removes the magnitude from
        // the ACTIVE `liquidity` scalar — the exact case that historically
        // left the solver's scalar too high on mainnet (desync manifesting as
        // an on-chain-vs-solver liquidity mismatch with byte-identical
        // sqrt_price/tick).
        let liq = 1_000_000u128;
        let mut state = state_with_position(liq); // current tick 0 ∈ [-60, 60)
        let pre_liq = state.liquidity;

        state.apply_liquidity_update(-60, 60, -250_000i128, 9);

        assert_eq!(
            state.liquidity,
            pre_liq - 250_000,
            "in-range burn removes active liquidity"
        );
    }

    #[test]
    fn apply_liquidity_update_replay_at_or_before_seed_block_does_not_adjust_scalar() {
        // UO3JM4 (historical-replay guard, Python `_initial_state_block` twin):
        // a pool seeded against head already reflects in its `liquidity` scalar
        // every on-chain in-range Mint/Burn at or before the seed block.
        // Replaying one such event after seed (e.g. a backfilled Burn applied
        // after registration) must NOT adjust the active-liquidity scalar — it
        // would subtract a removal the seed already contains, leaving the
        // solver exactly one in-range position-net BELOW on-chain with
        // identical sqrt/tick (the observed mainnet desync).
        let liq = 1_000_000u128;
        let mut state = state_with_position(liq); // tick 0 ∈ [-60, 60)
                                                  // Seed against head at block 100: scalar already reflects events <= 100.
        state.initial_state_block = 100;
        let pre_liq = state.liquidity;

        // Historical replay of the in-range Burn at block 50 (<= seed 100).
        state.apply_liquidity_update(-60, 60, -250_000i128, 50);

        assert_eq!(
            state.liquidity, pre_liq,
            "replay at or before the seed block must NOT re-adjust the active liquidity (already in the seed)"
        );
        // Two-stamp OB7UNY: the replay mutates the TICK MAP (liquidity clock
        // advances to 50) but does NOT advance the PRICE clock (update_block
        // stays 0 — the replay leaves the slot0 head byte-identical).
        assert_eq!(
            state.tick_data_block, 50,
            "the tick mutation advances the liquidity clock (tick map is replayed)"
        );
        assert_eq!(
            state.update_block, 0,
            "an at-or-before-seed replay must NOT advance the price clock (slot0 head untouched)"
        );
    }

    #[test]
    fn apply_liquidity_update_post_seed_in_range_adjusts_scalar() {
        // A genuinely post-seed in-range event (block > seed block) DOES adjust
        // the active-liquidity scalar — the historical guard must not swallow
        // real forward liquidity events.
        let liq = 1_000_000u128;
        let mut state = state_with_position(liq);
        state.initial_state_block = 100;
        let pre_liq = state.liquidity;

        state.apply_liquidity_update(-60, 60, -250_000i128, 150);

        assert_eq!(
            state.liquidity,
            pre_liq - 250_000,
            "post-seed (block > initial_state_block) in-range burn adjusts the active liquidity"
        );
    }

    #[test]
    fn replace_tick_data_swaps_map_advances_block_seeds_words_invalidates_cache() {
        // What: replace_tick_data must (1) wholesale-swaps tick_data,
        // (2) advance update_block only if newer (monotonic), (3) re-seed
        // known_bitmap_words from the new keys, (4) invalidate the cached
        // tick ranges. Scalars are NOT touched.
        let liq = 1_000_000u128;
        let mut state = state_with_position(liq);
        let sp_before = state.sqrt_price_x96;

        // Pre-condition: known_bitmap_words is empty, update_block is 0.
        assert!(state.known_bitmap_words.is_empty());
        assert_eq!(state.update_block, 0);

        // New tick_data: a single tick at 120 (word 2 at tick_spacing 60).
        let mut new_data = HashMap::new();
        new_data.insert(
            120,
            TickInfo {
                liquidity_gross: U256::from(7u64).to::<U128>(),
                liquidity_net: I256::try_from(7i128).unwrap(),
                block: 5,
            },
        );

        state.replace_tick_data(new_data.clone(), 5, 60);

        // (1) tick_data swapped (old -60/+60 gone, 120 present).
        assert_eq!(
            state.tick_data.get(&120).map(|t| t.liquidity_gross),
            Some(U256::from(7u64).to::<U128>())
        );
        assert!(!state.tick_data.contains_key(&-60));
        assert!(!state.tick_data.contains_key(&60));
        // (2) OB7UNY: the tick-map replace advances only the LIQUIDITY clock;
        // the price clock is untouched (scalars aren't changed).
        assert_eq!(state.tick_data_block, 5);
        assert_eq!(state.update_block, 0);
        // (3) known_bitmap_words seeded from the new keys (word of tick 120
        // at spacing 60 = 120.div_euclid(60) >> 8 = 2 >> 8 = 0).
        assert!(state
            .known_bitmap_words
            .contains(&V3PoolState::word_of(120, 60)));
        // (4) cache invalidated.
        {
            let cache = state.cached_tick_ranges.lock();
            assert!(matches!(cache.zfo, CachedTickRanges::StillEmpty));
            assert!(matches!(cache.ofz, CachedTickRanges::StillEmpty));
        }
        // Scalars untouched.
        assert_eq!(state.sqrt_price_x96, sp_before);
    }

    #[test]
    fn replace_tick_data_does_not_rewind_block() {
        // OB7UNY: replace_tick_data advances only the LIQUIDITY clock
        // (`tick_data_block`) — scalars untouched, so the PRICE clock
        // (`update_block`) never moves here. Advancing the liquidity clock to a
        // NEWER block is fine; a lower block is a monotonicity panic (guarded
        // elsewhere).
        let liq = 1_000_000u128;
        let mut state = state_with_position(liq);
        state.update_block = 10;
        assert_eq!(state.tick_data_block, 0);
        let empty: HashMap<i32, TickInfo> = HashMap::new();
        state.replace_tick_data(empty, 3, 60);
        assert_eq!(
            state.update_block, 10,
            "the price clock is untouched by a tick-map replace"
        );
        assert_eq!(
            state.tick_data_block, 3,
            "the liquidity clock advances to the replace block"
        );
    }

    // ------------------------------------------------------------------
    // ReorgPoolState trait (ADR-016 D4 — CL family adopts the trait).
    // `restore_before_block` returns `()`; the landed-at state lives in the
    // struct's own fields (read-after-restore). `V3RestoreResult` stays
    // internal to the impl and never escapes.
    // ------------------------------------------------------------------

    #[test]
    fn restore_before_block_writes_landed_at_scalars_and_restore_point_block() {
        // apply_swap captures scalar priors; restore_before_block pops the
        // delta and writes the pre-swap scalars back. Two-stamp OB7UNY:
        // restore rewinds BOTH clocks to their exact pre-swap values (the
        // delta's `update_block_before`/`tick_data_block_before`), NOT to the
        // restore-point block.
        let liq = 1_000_000u128;
        let mut state = state_with_position(liq);
        let pre_sqrt = state.sqrt_price_x96;
        let pre_liq = state.liquidity;
        let pre_tick = state.tick;

        let new_sqrt = U256::from(2u128) << 96;
        state.apply_swap(new_sqrt, liq + 1, 1, 7, &[]);
        assert_eq!(state.update_block, 7);
        assert_eq!(state.tick_data_block, 7);

        let result: Result<(), JournalError> = state.restore_before_block(7);
        assert!(result.is_ok());
        assert_eq!(state.sqrt_price_x96, pre_sqrt);
        assert_eq!(state.liquidity, pre_liq);
        assert_eq!(state.tick, pre_tick);
        assert_eq!(
            state.update_block, 0,
            "the price clock rewinds to its exact pre-swap value (both clocks start at 0)"
        );
        assert_eq!(state.tick_data_block, 0, "the liquidity clock rewinds to 0");
        assert_eq!(state.journal_len(), 0, "swap delta popped");
    }

    #[test]
    fn restore_before_block_no_op_when_newest_delta_before_target() {
        let liq = 1_000_000u128;
        let mut state = state_with_position(liq);
        let new_sqrt = U256::from(2u128) << 96;
        state.apply_swap(new_sqrt, liq + 1, 1, 7, &[]);
        let post_sqrt = state.sqrt_price_x96;

        let result: Result<(), JournalError> = state.restore_before_block(8);
        assert!(result.is_ok());
        assert_eq!(
            state.sqrt_price_x96, post_sqrt,
            "no rollback when newest delta is before the target"
        );
        assert_eq!(state.journal_len(), 1, "delta not popped");
    }

    #[test]
    fn restore_before_block_removes_newly_initialized_tick_on_rollback() {
        // A swap that seeds a brand-new tick (no prior entry) records
        // `liquidity_gross_before: None`; restore must remove that tick.
        let liq = 1_000_000u128;
        let mut state = state_with_position(liq);
        let new_tick = TickInfo {
            liquidity_gross: U256::from(500u64).to::<U128>(),
            liquidity_net: I256::try_from(500i128).unwrap(),
            block: 7,
        };
        let new_sqrt = U256::from(2u128) << 96;
        state.apply_swap(new_sqrt, liq + 1, 1, 7, &[(100, new_tick)]);
        assert!(
            state.tick_data.contains_key(&100),
            "new tick seeded by swap"
        );

        let result: Result<(), JournalError> = state.restore_before_block(7);
        assert!(result.is_ok());
        assert!(
            !state.tick_data.contains_key(&100),
            "newly-initialized tick removed on rollback"
        );
    }

    #[test]
    fn discard_before_block_drops_old_deltas_without_mutating_live_state() {
        let liq = 1_000_000u128;
        let mut state = state_with_position(liq);
        // Genesis anchor (tick-only, block 0) so discard has an old delta to
        // drop without emptying the journal entirely.
        state.journal.push_delta(V3BlockDelta {
            block: 0,
            scalar_priors: None,
            update_block_before: None,
            tick_data_block_before: None,
            tick_priors: Vec::new(),
        });
        let new_sqrt = U256::from(2u128) << 96;
        state.apply_swap(new_sqrt, liq + 1, 1, 7, &[]);
        assert_eq!(state.journal_len(), 2);
        let post_sqrt = state.sqrt_price_x96;

        let result: Result<(), JournalError> = state.discard_before_block(7);
        assert!(result.is_ok());
        assert_eq!(state.journal_len(), 1, "genesis dropped, swap kept");
        assert_eq!(
            state.sqrt_price_x96, post_sqrt,
            "discard trims history only — live state untouched"
        );
    }

    #[test]
    fn journal_len_reports_delta_count() {
        let liq = 1_000_000u128;
        let mut state = state_with_position(liq);
        assert_eq!(state.journal_len(), 0);
        state.apply_swap(U256::from(2u128) << 96, liq + 1, 1, 7, &[]);
        assert_eq!(state.journal_len(), 1);
    }

    /// Regression: the closed-form solver's `build_int_v3_sequence` MUST drain
    /// the current tick's `liquidity_net` at step 1 for `zero_for_one` swaps
    /// when the current tick is initialized. The mainnet DAI/WETH pool
    /// `0xD8dE…7aC19` (fee=100, `tick_spacing=1`) at block 25641224 had its
    /// current tick `-74028` initialized with `liq_net = +5_407_362_545_736_161_987`
    /// — exactly the active liquidity. A zfo swap's first V3 step crosses the
    /// current tick (lte, inclusive) in a zero-amount step, draining liquidity
    /// to ZERO; the price then free-falls through the 10k-tick gap to `-84382`
    /// where liquidity recovers to `6_491_467_503_505_060_4`. Without the drain,
    /// the solver models range 0 as carrying the full `5.407e18` liquidity
    /// through the whole region → no-slippage over-prediction of ~2.7× (the
    /// `no-profit` trap; see `logs/fixtures/v2_v3_v3_solver_divergence_25641093.md`).
    #[test]
    fn build_int_v3_sequence_drains_current_tick_on_zfo_when_current_is_initialized() {
        // 12 initialized ticks copied verbatim from the degenbot snapshot DB
        // for pool 0xD8dE…7aC19 at block 25641224.
        let raw: [(i32, u128, i128); 12] = [
            (-84469, 9_223_372_036_854_775_807, 9_223_372_036_854_775_807),
            (
                -84460,
                9_223_372_036_854_775_807,
                -9_223_372_036_854_775_808,
            ),
            (-84440, 2_319_993_473_851_491_971, 2_319_993_473_851_491_971),
            (-84422, 64_914_675_035_050_604, 64_914_675_035_050_604),
            (
                -84401,
                2_319_993_473_851_491_971,
                -2_319_993_473_851_491_971,
            ),
            (-84382, 64_914_675_035_050_604, -64_914_675_035_050_604),
            (-74028, 5_407_362_545_736_161_987, 5_407_362_545_736_161_987),
            (-74021, 8_246_173_613_278_771_746, 8_246_173_613_278_771_746),
            (-74017, 5_283_388_076_511_134_702, 5_283_388_076_511_134_702),
            (
                -74008,
                5_407_362_545_736_161_987,
                -5_407_362_545_736_161_987,
            ),
            (
                -74001,
                8_246_173_613_278_771_746,
                -8_246_173_613_278_771_746,
            ),
            (
                -73990,
                5_283_388_076_511_134_702,
                -5_283_388_076_511_134_702,
            ),
        ];
        let mut tick_data = HashMap::new();
        for (t, gross, net) in raw {
            tick_data.insert(
                t,
                TickInfo {
                    liquidity_gross: U256::from(gross).to::<U128>(),
                    liquidity_net: I256::try_from(net).unwrap(),
                    block: 0,
                },
            );
        }
        // Pre-swap on-chain state (verified via cast at block 25641093).
        let sqrt_price_x96 = U256::from(1_956_421_190_421_993_762_013_571_523u128);
        let state = V3PoolState {
            sqrt_price_x96,
            liquidity: 5_407_362_545_736_161_987,
            tick: -74028,
            update_block: 0,
            tick_data_block: 0,
            initial_state_block: 0,
            state_nonce: 0,
            registration_lifecycle: RegistrationLifecycle::default(),
            tick_data,
            coverage: PoolTickCoverage::Tracked,
            known_bitmap_words: HashSet::new(),
            fetcher: None,
            journal: ReorgJournal::<V3BlockDelta>::new(8),
            snapshot_seed: None,
            post_drain_snapshot: None,
            cached_tick_ranges: parking_lot::Mutex::new(TickRangeCache::default()),
        };

        // zfo (DAI->WETH). Collapsed ranges (verified in
        // `tick_bitmap::tests::test_diag_compute_tick_ranges_mainnet_dai_weth_pool`):
        //   r0 = [-74240, -74028] liq_net=0   (current tick -74028 = r0 UPPER bound)
        //   r1 = [-84224, -74240] liq_net=0   (the 10k-tick free-fall gap)
        //   r2 = [-84382, -84224] liq_net=-6.49e16
        //   r3 = [-84401, -84382] liq_net=-2.32e18  ← holds captured post-swap tick -84383
        // `build_int_v3_sequence` prepends a contract-faithful LEADING hop = the
        // current segment [current, sqrt(currentTick)] at the stored (pre-drain)
        // liquidity 5.4e18, then applies the net at the crossing:
        //   lead = 5.4e18 (stored; the segment the swap sweeps before crossing
        //                   the current tick -74028, whose net == stored)
        //   r0   = 0   (drained at -74028)
        //   r1   = 0   (free-fall gap, no net until -84382)
        //   r2   = 0
        //   r3   = 64_914_675_035_050_604 (crossing -84382 recovers liq)
        let seq = state
            .build_int_v3_sequence(1, 100, true)
            .expect("should build a sequence");

        let liqs: Vec<u128> = seq.ranges.iter().map(|r| r.liquidity).collect();
        eprintln!("[drain-test] range liquidities: {liqs:?}");

        assert_eq!(
            liqs[0], 5_407_362_545_736_161_987,
            "leading current segment sweeps [current, sqrt(-74028)] at stored liquidity"
        );
        assert_eq!(
            liqs[1], 0,
            "r0 drained after crossing current tick -74028 (net == stored)"
        );
        assert_eq!(liqs[2], 0, "r1 must be 0 (free-fall gap above -84382)");
        assert_eq!(
            liqs[3], 0,
            "r2 must be 0 (still above -84382, no net applied)"
        );
        assert_eq!(
            liqs[4], 64_914_675_035_050_604,
            "r3 must recover to 6.49e16 after crossing -84382"
        );

        // ofz must NOT drain: current tick -74028 is r0's LOWER bound for ofz,
        // and gt is exclusive → no zero-amount crossing at step 1.
        state.invalidate_tick_range_cache();
        let seq_ofz = state
            .build_int_v3_sequence(1, 100, false)
            .expect("should build an ofz sequence");
        let ofz_r0 = seq_ofz.ranges[0].liquidity;
        eprintln!("[drain-test] ofz r0 liquidity: {ofz_r0}");
        assert_eq!(
            ofz_r0, 5_407_362_545_736_161_987,
            "ofz r0 must NOT drain (gt exclusive)"
        );
    }
}

// ---------------------------------------------------------------------------
// Tests relocated from the `bot_core::v3_state` shim (USPN7M/P2CKRL): the shim
// is deleted and these integration-style tests of `v3_simulate_swap` now live
// beside the implementation.
// ---------------------------------------------------------------------------

#[expect(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::U128;

    /// Build a V3 pool at tick 0 (1:1 price, `sqrt_price` = 2^96), liquidity `liq`,
    /// fee 0.3% (3000 pips), `tick_spacing` 60, with a single position spanning
    /// [-60, +60] so the active range bounded by ±60 matches
    /// `make_v3_hop_at_1to1`. The ticks -60 and +60 are initialized with the
    /// position's `liquidity_net` (+L at lower, -L at upper) and matching gross.
    fn pool_1to1_with_position(liq: u128) -> (V3PoolIdentity, V3PoolState) {
        let sp_0 = U256::from(1u128) << 96;
        let mut tick_data = HashMap::new();
        // Position [-60, +60] with liquidity `liq`.
        let liq_u128 = U256::from(liq).to::<U128>();
        tick_data.insert(
            -60,
            TickInfo {
                liquidity_gross: liq_u128,
                liquidity_net: I256::try_from(i128::try_from(liq).unwrap()).unwrap(),
                block: 0,
            },
        );
        tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: liq_u128,
                liquidity_net: I256::try_from(-i128::try_from(liq).unwrap()).unwrap(),
                block: 0,
            },
        );
        (
            V3PoolIdentity {
                address: Address::ZERO,
                token0: Address::ZERO,
                token1: Address::ZERO,
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                deployer: Address::ZERO,
                init_hash: alloy::primitives::B256::ZERO,
            },
            V3PoolState {
                sqrt_price_x96: sp_0,
                liquidity: liq,
                tick: 0,
                update_block: 0,
                tick_data_block: 0,
                initial_state_block: 0,
                tick_data,
                coverage: PoolTickCoverage::Tracked,
                known_bitmap_words: HashSet::new(),
                fetcher: None,
                journal: ReorgJournal::<V3BlockDelta>::new(8),
                cached_tick_ranges: parking_lot::Mutex::new(super::TickRangeCache::default()),
                snapshot_seed: None,
                post_drain_snapshot: None,
                state_nonce: 0,
                registration_lifecycle: RegistrationLifecycle::default(),
            },
        )
    }

    // --- swap_is_viable (directional viability, ported from the archived
    // Python v3_liquidity_pool.py::swap_is_viable) ---

    /// Probe: single position NEAR the price ([-600,+600], spacing 60) —
    /// does range building succeed?
    #[test]
    fn single_straddling_position_builds_one_range() {
        // A single reachable position is enough for range building — the
        // solver does not require multiple positions.
        let (identity, state) = pool_1to1_with_position(10_000_000_000_000u128);
        let seq = state.build_int_v3_sequence(identity.tick_spacing, identity.fee, true);
        assert_eq!(seq.expect("built").ranges.len(), 1);
    }

    /// 2SGSE3: a `None` from `get_cached_tick_ranges` must be cached so the
    /// next call returns `None` without re-walking `compute_tick_ranges`.
    /// Without caching None, every solve cycle re-walks the same failing
    /// pools (1300-9600 `SequenceUnavailable` rejections per cycle in live
    /// traces — each doing an O(tick-walk) that returns None).
    ///
    /// Safety: `invalidate_tick_range_cache()` is already called on every
    /// V3/V4 state mutation (Swap/Mint/Burn), so a pool that was unviable
    /// and later receives liquidity will have its cached None cleared, and
    /// the next solve re-walks fresh.
    #[test]
    fn none_tick_ranges_cached_to_avoid_rewalk() {
        let (_id, mut state) = pool_1to1_with_position(10_000_000_000_000u128);
        // Clear tick_data so compute_tick_ranges returns None (no initialized
        // ticks ahead in either direction).
        state.tick_data.clear();
        state.coverage = PoolTickCoverage::Tracked;

        // First call: walks, returns None.
        let first = state.get_cached_tick_ranges(60, true);
        assert!(first.is_none(), "empty tick data should yield None");

        // The cache slot must now reflect a computed Miss — NOT StillEmpty.
        // Before the fix, the slot is StillEmpty because None was not cached,
        // meaning every subsequent call re-walks from scratch.
        {
            let cache = state.cached_tick_ranges.lock();
            assert!(
                !matches!(cache.zfo, CachedTickRanges::StillEmpty),
                "zfo slot must be cached (Miss) after the first None result,                  not StillEmpty (which causes a re-walk every cycle)"
            );
            assert!(
                matches!(cache.zfo, CachedTickRanges::Miss),
                "zfo slot should be Miss"
            );
            assert!(
                matches!(cache.ofz, CachedTickRanges::StillEmpty),
                "ofz slot should still be StillEmpty (not called)"
            );
        }

        // After invalidation, both slots reset to StillEmpty.
        state.invalidate_tick_range_cache();
        {
            let cache = state.cached_tick_ranges.lock();
            assert!(matches!(cache.zfo, CachedTickRanges::StillEmpty));
            assert!(matches!(cache.ofz, CachedTickRanges::StillEmpty));
        }

        // After adding liquidity + invalidation, should return Some.
        let liq = U256::from(1_000_000u64).to::<U128>();
        state.tick_data.insert(
            -60,
            TickInfo {
                liquidity_gross: liq,
                liquidity_net: I256::try_from(1_000_000i128).unwrap(),
                block: 0,
            },
        );
        state.tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: liq,
                liquidity_net: I256::try_from(-1_000_000i128).unwrap(),
                block: 0,
            },
        );
        let third = state.get_cached_tick_ranges(60, true);
        assert!(
            third.is_some(),
            "after adding liquidity + invalidation, should be Some"
        );
        {
            let cache = state.cached_tick_ranges.lock();
            assert!(
                matches!(cache.zfo, CachedTickRanges::Hit(_)),
                "zfo slot should be Hit after computing Some"
            );
        }
    }

    #[test]
    fn empty_tick_data_is_not_viable_in_either_direction() {
        // What: a pool with NO initialized ticks cannot host a swap in either
        // direction — the solver has no range to walk.
        // Why: the cheapest rejection; mirrors Python `tick_data == {}`.
        let (_identity, mut state) = pool_1to1_with_position(10_000_000_000_000u128);
        state.tick_data.clear();

        assert!(!state.swap_is_viable(true));
        assert!(!state.swap_is_viable(false));
    }

    #[test]
    fn uninitialized_price_is_not_viable_in_either_direction() {
        // What: a pool whose sqrt_price_x96 is zero was never initialized on
        // chain and cannot host a swap.
        // Why: mirrors Python `sqrt_price_x96 == 0` guard.
        let (_identity, mut state) = pool_1to1_with_position(10_000_000_000_000u128);
        state.sqrt_price_x96 = U256::ZERO;

        assert!(!state.swap_is_viable(true));
        assert!(!state.swap_is_viable(false));
    }

    #[test]
    fn liquidity_ahead_of_price_is_viable_in_both_directions() {
        // What: the 1:1 pool with [-60,+60] straddling the current tick has
        // initialized ticks BOTH below (-60) and above (+60) the price —
        // viable in both directions.
        let (_identity, state) = pool_1to1_with_position(10_000_000_000_000u128);

        assert!(state.swap_is_viable(true));
        assert!(state.swap_is_viable(false));
    }

    #[test]
    fn one_sided_liquidity_is_direction_dependent() {
        // What: all initialized ticks ABOVE the current price → a zfo swap
        // (walking DOWN) finds no liquidity ahead and is not viable, while
        // ofz (walking UP) is viable. This is the directional precision the
        // Python check had via min/max tick comparison.
        // Why: this exact population produces the one-sided pools that die as
        // SequenceUnavailable in only one direction.
        let (_identity, mut state) = pool_1to1_with_position(10_000_000_000_000u128);
        // Move the price far BELOW every initialized tick (tick -100000): a
        // zfo swap walks DOWN from here and finds no position ahead.
        state.tick = -100_000;
        state.sqrt_price_x96 = U256::from(get_sqrt_ratio_at_tick_internal(-100_000).unwrap());

        assert!(!state.swap_is_viable(true));
        assert!(state.swap_is_viable(false));
    }

    #[test]
    fn price_at_sqrt_ratio_boundary_is_not_viable_in_the_exiting_direction() {
        // What: a price at/below MIN_SQRT_RATIO+1 cannot go further DOWN
        // (zfo); a price at/above MAX_SQRT_RATIO-1 cannot go further UP (ofz).
        // Why: protocol limit guard from the Python port.
        let (_identity, mut state) = pool_1to1_with_position(10_000_000_000_000u128);
        state.sqrt_price_x96 = U256::from(MIN_SQRT_RATIO) + U256::from(1u64);

        assert!(!state.swap_is_viable(true));
        assert!(state.swap_is_viable(false));

        state.sqrt_price_x96 = U256::from(MAX_SQRT_RATIO) - U256::from(1u64);
        assert!(state.swap_is_viable(true));
        assert!(!state.swap_is_viable(false));
    }

    #[test]
    fn zfo_small_swap_matches_single_compute_swap_step() {
        // What: a small zfo exact-input swap on a 1:1 V3 pool with a [-60,+60]
        // position stays inside the range (no tick crossing), so the outcome
        // must equal a single `compute_swap_step_v3` call with the same bounds.
        // Why: pins the V3 simulator's first-step behavior against the already-
        // tested swap-step primitive as the oracle (zero hand-computed math).
        let liq = 10_000_000_000_000u128;
        let (identity, state) = pool_1to1_with_position(liq);
        let amount_in = U256::from(1000u64);

        let outcome = v3_simulate_swap(
            &state,
            identity.fee,
            identity.tick_spacing,
            true,
            I256::try_from(amount_in).unwrap(),
            V3PoolState::default_sqrt_price_limit(true),
        )
        .expect("small swap should produce an outcome");

        // Oracle: the single-step target is tick -60's sqrt price (the range
        // lower bound), which a small input does not reach. amount_remaining is
        // the full positive input (V3 exact-in convention).
        let sp_lower = U256::from(get_sqrt_ratio_at_tick_internal(-60).unwrap());
        let step = compute_swap_step_v3(
            state.sqrt_price_x96,
            sp_lower,
            i128::try_from(liq).unwrap(),
            I256::try_from(amount_in).unwrap(),
            U256::from(identity.fee),
        )
        .unwrap();

        assert_eq!(
            outcome.amount1, step.amount_out,
            "zfo exact-in: token1 output must equal the single swap-step amount_out"
        );
        assert_eq!(
            outcome.amount0,
            step.amount_in + step.fee_amount,
            "zfo exact-in: token0 input consumed must equal amount_in + fee_amount"
        );
        assert!(
            outcome.amount1 < amount_in,
            "on a 1:1 pool with fees, output must be < input (got {} >= {})",
            outcome.amount1,
            amount_in
        );
    }

    #[test]
    fn ofz_small_swap_matches_single_compute_swap_step() {
        // Mirrors the zfo test for the one_for_zero direction — oracle target
        // is tick +60's sqrt price (the range upper bound).
        let liq = 10_000_000_000_000u128;
        let (identity, state) = pool_1to1_with_position(liq);
        let amount_in = U256::from(1000u64);

        let outcome = v3_simulate_swap(
            &state,
            identity.fee,
            identity.tick_spacing,
            false,
            I256::try_from(amount_in).unwrap(),
            V3PoolState::default_sqrt_price_limit(false),
        )
        .expect("small ofz swap should produce an outcome");

        let sp_upper = U256::from(get_sqrt_ratio_at_tick_internal(60).unwrap());
        let step = compute_swap_step_v3(
            state.sqrt_price_x96,
            sp_upper,
            i128::try_from(liq).unwrap(),
            I256::try_from(amount_in).unwrap(),
            U256::from(identity.fee),
        )
        .unwrap();

        assert_eq!(outcome.amount0, step.amount_out);
        assert_eq!(outcome.amount1, step.amount_in + step.fee_amount);
        assert!(outcome.amount0 < amount_in);
    }

    #[test]
    fn zero_amount_is_not_computable() {
        let (identity, state) = pool_1to1_with_position(1_000_000u128);
        let outcome = v3_simulate_swap(
            &state,
            identity.fee,
            identity.tick_spacing,
            true,
            I256::ZERO,
            V3PoolState::default_sqrt_price_limit(true),
        );
        assert_eq!(
            outcome,
            Err(SimulateSwapError::NotComputable),
            "zero amount_specified should be NotComputable (V3 AS revert)"
        );
    }

    #[test]
    fn output_scales_monotonically_with_input() {
        // Larger exact-input swaps produce larger outputs (within the same
        // tick range, pre-crossing).
        let (identity, state) = pool_1to1_with_position(10_000_000_000_000u128);
        let small = v3_simulate_swap(
            &state,
            identity.fee,
            identity.tick_spacing,
            true,
            I256::try_from(U256::from(100u64)).unwrap(),
            V3PoolState::default_sqrt_price_limit(true),
        )
        .unwrap()
        .amount1;
        let large = v3_simulate_swap(
            &state,
            identity.fee,
            identity.tick_spacing,
            true,
            I256::try_from(U256::from(10_000u64)).unwrap(),
            V3PoolState::default_sqrt_price_limit(true),
        )
        .unwrap()
        .amount1;
        assert!(
            large > small,
            "larger input must produce larger output (small={small}, large={large})"
        );
    }

    #[test]
    fn sparse_unknown_word_signals_fetchable_miss() {
        // ADR-005 sparse-map feature parity. In sparse mode a region is unknown
        // unless its word key is in `known_bitmap_words`. A pool constructed
        // sparse with no known words must therefore signal a fetchable miss on
        // the starting word (mirrors Python's `MissingLiquidityData(word=0)`
        // first-step raise), NOT a silently-wrong computed amount.
        let liq = 10_000_000_000_000u128;
        let (identity, mut state) = pool_1to1_with_position(liq);
        state.coverage = PoolTickCoverage::Sparse;
        state.known_bitmap_words.clear(); // fully sparse: no regions known

        let res = v3_simulate_swap(
            &state,
            identity.fee,
            identity.tick_spacing,
            true,
            I256::try_from(1_000u64).unwrap(),
            V3PoolState::default_sqrt_price_limit(true),
        );
        assert_eq!(
            res,
            Err(SimulateSwapError::MissingTickWord(0)),
            "sparse pool with unknown starting word must signal MissingTickWord, \
             not a computed outcome nor NotComputable"
        );
    }

    #[test]
    fn tracked_pool_bypasses_miss_detection() {
        // ADR-005 sparse-map feature parity. Miss detection is gated on
        // `coverage == Sparse`: a Tracked pool (complete tick data) must
        // compute normally even when `known_bitmap_words` is empty — it never
        // consults the set. Confirms detection is sparse-only.
        let liq = 10_000_000_000_000u128;
        let (identity, mut state) = pool_1to1_with_position(liq);
        // Tracked + empty known set — must NOT miss.
        state.known_bitmap_words.clear();
        assert_eq!(state.coverage, PoolTickCoverage::Tracked);

        let res = v3_simulate_swap(
            &state,
            identity.fee,
            identity.tick_spacing,
            true,
            I256::try_from(1_000u64).unwrap(),
            V3PoolState::default_sqrt_price_limit(true),
        );
        assert!(
            res.is_ok(),
            "Tracked pool must compute regardless of known_bitmap_words, got {res:?}"
        );
    }

    #[test]
    fn sparse_unreached_boundary_does_not_false_miss() {
        // The complement of `sparse_unknown_word_signals_fetchable_miss`.
        // gen_ticks proposes candidate boundary ticks along the path; a swap
        // that does NOT reach a proposed tick in an unknown neighbor word must
        // still compute (no false miss). Mirrors Python's per-word miss: the
        // word is only consulted when the walk actually enters it.
        //
        // Uses an ofz (price-rising) swap from tick 0: the position's lower
        // tick −60 lives in word −1 (unknown here), but ofz walks UPWARD into
        // word 0 (known) toward +60, so word −1 is merely proposed, never
        // entered — no miss. (A zfo swap would move the tick into word −1, the
        // endpoint-in-unknown-word case covered by
        // `sparse_endpoint_in_unknown_word_signals_miss`.)
        let liq = 10_000_000_000_000u128;
        let (identity, mut state) = pool_1to1_with_position(liq);
        state.coverage = PoolTickCoverage::Sparse;
        // Word 0 (tick 0, the start) is known; word −1 (containing the tick −60
        // boundary of the position) is NOT known.
        state.known_bitmap_words.clear();
        state.known_bitmap_words.insert(0);

        let res = v3_simulate_swap(
            &state,
            identity.fee,
            identity.tick_spacing,
            false,
            I256::try_from(100u64).unwrap(),
            V3PoolState::default_sqrt_price_limit(false),
        );
        assert!(
            res.is_ok(),
            "ofz swap staying in the known word 0 should compute, got {res:?}"
        );
    }

    #[test]
    fn sparse_endpoint_in_unknown_word_signals_miss() {
        // Slice-4 fix (V3 mirror of the V4 ELSE-branch miss check). A swap
        // whose price endpoint lands in an UNFETCHED word must signal
        // `MissingTickWord` — the word may contain initialized ticks the walk
        // crossed but `gen_ticks` never proposed (they are absent from
        // `tick_data`). This is the divergence that made V4 `test_cached_
        // calculations` undercount on multi-word swaps: without this check the
        // walk committed a result computed with stale liquidity, having skipped
        // the unknown word's liquidity-nets. Mirrors Python's
        // `next_initialized_tick_within_one_word`, which raises for the current
        // tick's word at every step — so Python fetches the endpoint word; Rust
        // must too.
        let liq = 10_000_000_000_000u128;
        let (identity, mut state) = pool_1to1_with_position(liq);
        state.coverage = PoolTickCoverage::Sparse;
        state.known_bitmap_words.clear();
        state.known_bitmap_words.insert(0); // word 0 known; word −1 unknown

        // zfo drops the price below tick 0 into word −1 (unknown).
        let res = v3_simulate_swap(
            &state,
            identity.fee,
            identity.tick_spacing,
            true,
            I256::try_from(100u64).unwrap(),
            V3PoolState::default_sqrt_price_limit(true),
        );
        assert_eq!(
            res,
            Err(SimulateSwapError::MissingTickWord(-1)),
            "zfo swap whose endpoint enters the unknown word −1 must signal a \
             fetchable miss, got {res:?}"
        );
    }

    #[test]
    fn v3_simulate_swap_outcome_caries_final_state() {
        // ADR-005 slice 3b: the companion's simulate_exact_input_swap builds
        // final_state from the outcome, so v3_simulate_swap must return the
        // post-walk sqrt_price_x96 / liquidity / tick (not just the amounts).
        let (identity, state) = pool_1to1_with_position(10_000_000_000_000u128);
        let amount_in = I256::try_from(1_000u128).unwrap();
        let outcome = v3_simulate_swap(
            &state,
            identity.fee,
            identity.tick_spacing,
            true,
            amount_in,
            V3PoolState::default_sqrt_price_limit(true),
        )
        .expect("computes");
        // zfo small swap below +60 (no crossing): price drops (sqrt_price_x96 <
        // start) but liquidity + tick stay within the active range.
        assert!(
            outcome.sqrt_price_x96 < state.sqrt_price_x96,
            "zfo swap must drop the price below the start value"
        );
        assert_eq!(
            outcome.liquidity, state.liquidity,
            "liquidity unchanged (no crossing)"
        );
        assert!(
            (-60..60).contains(&outcome.tick),
            "tick stays within the active range (no crossing): got {}",
            outcome.tick
        );
        // A crossing swap (large zfo) must move the state off the start values.
        let big = I256::try_from(100_000_000_000_000_000u128).unwrap();
        let crossed = v3_simulate_swap(
            &state,
            identity.fee,
            identity.tick_spacing,
            true,
            big,
            V3PoolState::default_sqrt_price_limit(true),
        )
        .expect("computes");
        assert_ne!(
            crossed.sqrt_price_x96, state.sqrt_price_x96,
            "a crossing swap must move the price off the start value"
        );
    }

    /// Word-of parity: the Rust sparse miss-detection model (`word_of`, used by
    /// both V3 + V4 `vX_simulate_swap` to decide whether the current tick's
    /// bitmap word is "known") must match the Python companion's bitmap word
    /// computation (`position(tick // tick_spacing)[0]` in
    /// `v3_libraries/tick_bitmap.py`). Both use floored division (`div_euclid`
    /// == Python `//`) + arithmetic right-shift (`>> 8`), so they agree for
    /// negative non-multiple current ticks — the regime a crossing swap's
    /// post-step price lives in. This test locks that equivalence so the V4
    /// crossing-swap divergence under the fetch seam (slice 4) is NOT
    /// mis-attributed to the miss-detection model. See the slice-3 diagnosis
    /// recorded on ergo task `2ZG6XO`: the models match, so V4 routing's fork
    /// divergence lives elsewhere (fee accounting / boundary-tick walk / fetch
    /// merge semantics) and must be fork-validated.
    #[test]
    fn word_of_matches_python_bitmap_word_position_for_edge_ticks() {
        // (tick, tick_spacing) covering: positive + negative, multiples +
        // non-multiples of spacing, and cross-word boundaries.
        let cases: &[(i32, i32)] = &[
            (0, 60),
            (60, 60),
            (-60, 60),
            (-10, 60), // negative non-multiple: div_euclid floors to -1
            (-1, 60),
            (59, 60),
            (-61, 60),
            (-255, 1),
            (-256, 1), // word boundary (-1 → -2)
            (-257, 1),
            (255, 1),
            (256, 1), // word boundary (0 → 1)
            (887_272, 60),
            (-887_272, 60),
            (887_270, 60), // negative mirror of a non-multiple
            (-887_270, 60),
        ];
        for &(tick, spacing) in cases {
            let rust_word = V3PoolState::word_of(tick, spacing);
            // Python: compressed = tick // spacing (floored); word = compressed >> 8.
            let compressed = tick.div_euclid(spacing);
            let py_word = compressed >> 8;
            assert_eq!(
                rust_word, py_word,
                "word_of({tick}, {spacing}) = {rust_word} != python {py_word}"
            );
        }
    }
}
