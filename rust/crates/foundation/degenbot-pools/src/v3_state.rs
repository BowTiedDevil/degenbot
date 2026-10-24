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

use crate::families::concentrated_liquidity::state_journal;
use crate::families::concentrated_liquidity::state_seeding;
use crate::families::concentrated_liquidity::state_walk;
use crate::int_v3_hop::IntV3TickRangeSequence;
use crate::state_history::{
    JournalError, ReorgJournal, ReorgPoolState, ScalarPriors, V3BlockDelta,
};
use crate::tick_bitmap::gen_ticks_iter;
use crate::tick_fetch::TickWordFetcher;
use crate::TickInfo;
use degenbot_math::concentrated_liquidity::swap_math::compute_swap_step_v3;
use degenbot_math::concentrated_liquidity::tick_math::{
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

/// The per-pool registration lifecycle for V3/V4 concentrated liquidity pools. Controls whether
/// the live pump applies events directly (`Live`) or defers them to the pump
/// buffer (`Quarantined`).
///
/// `Quarantined` is set at the start of `register_v3/v4_pool` (before the
/// first RPC await in the two-step verify) so a live `Swap`/`Mint`/`Burn`
/// landing during the drain+pin+verify window cannot advance `update_block`
/// past the pump's `last_complete_block` — which would desync the pinned
/// `(tick_data, update_block)` pair (the quarantine race; the live direct-apply
/// gap the `drain_pump_completed` buffer gate does NOT cover).
///
/// Defaults to `Live`: pools registered outside the two-step verify path
/// (test/standalone construction) keep the existing direct-apply behavior.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RegistrationLifecycle {
    /// Live events apply directly to pool state (the steady-state contract).
    #[default]
    Live,
    /// Live events are deferred to the pump buffer until `mark_registration_live`
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
/// or because the pool is `Quarantined` (quarantine deferral). Carries the scalar
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
/// `Mint` at logIdx 120). The quarantine deferral routes BOTH variants
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

/// The EVM storage-slot layout a concentrated-liquidity (V3-style) pool
/// contract uses. The `PancakeSwap` V3 fork diverges from canonical Uniswap
/// V3: `slot0` spans TWO words, shifting every following slot by
/// one (liquidity@5, ticks@6). Reads of a fork pool through the Uniswap
/// indices misread the contract (the false [sim-divergence] on pool
/// 0x1ac1A8FE, VERIFY2 T4).
///
/// The layout is immutable per pool → carried on [`V3PoolIdentity`].
/// Deliberately NO `Default`: a fork slot layout must always be chosen
/// explicitly by the caller (roster row, registration params, fixture) — an
/// implicit Uniswap assumption is exactly how the Pancake V3 replay bug
/// shipped. Slot constants single-source the fork table in
/// [`crate::v3_pancakeswap_storage_slots`].
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum ConcentratedLiquiditySlotLayout {
    /// Canonical Uniswap V3 — one-word slot0, liquidity@4, ticks@5.
    UniswapV3,
    /// `PancakeSwap` V3 fork — two-word slot0, liquidity@5, ticks@6.
    PancakeV3,
}

impl ConcentratedLiquiditySlotLayout {
    /// The `liquidity` (uint128) storage slot number.
    #[must_use]
    pub const fn liquidity_slot(self) -> u64 {
        match self {
            Self::UniswapV3 => 4,
            Self::PancakeV3 => crate::v3_pancakeswap_storage_slots::PANCAKE_V3_LIQUIDITY_SLOT,
        }
    }

    /// The `ticks(int24)` mapping base slot number.
    #[must_use]
    pub const fn ticks_mapping_slot(self) -> u64 {
        match self {
            Self::UniswapV3 => crate::v3_storage_slots::V3_TICKS_MAPPING_SLOT,
            Self::PancakeV3 => crate::v3_pancakeswap_storage_slots::PANCAKE_V3_TICKS_MAPPING_SLOT,
        }
    }

    /// The `tickBitmap(int16)` mapping base slot number.
    #[must_use]
    pub const fn tick_bitmap_mapping_slot(self) -> u64 {
        match self {
            Self::UniswapV3 => crate::v3_storage_slots::V3_TICK_BITMAP_MAPPING_SLOT,
            Self::PancakeV3 => {
                crate::v3_pancakeswap_storage_slots::PANCAKE_V3_TICK_BITMAP_MAPPING_SLOT
            }
        }
    }
}

/// Parameters for registering a V3 pool with `BotState`.
///
/// Bundles all fields to satisfy `clippy::too_many_arguments`.
///
/// The struct-update default serves sim/fixture plumbing only: every
/// PRODUCTION registration either flows through [`RegisterV3PoolParams`]
/// built from a roster edge or a CREATE2-verified builder — both of which
/// set `slot_layout` from the fork table — or through
/// `degenbot_substrate::planning::ExplicitPoolState::V3`, whose
/// `slot_layout` field is mandatory. A fixture defaulting to
/// `ConcentratedLiquiditySlotLayout::UniswapV3` must never reach a real fork pool.
#[derive(Clone, Debug)]
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
    /// The **liquidity** clock seed (two-stamp rule) — the block the
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

    /// Checked tick-bitmap WORD positions the caller looked up on-chain at
    /// registration and found EMPTY (Sparse pools only). Seeded inline with
    /// the registration — one write lock, mirroring the `tick_data` inline
    /// seed — so a known word is never re-fetched, without a post-register
    /// `update_tick_data` door. Combined with the words derived from the
    /// `tick_data` keys. Ignored for `Tracked` pools: their bitmap is
    /// complete, so the known set would be dead state (mirrors
    /// `mark_bitmap_words_known`'s no-op).
    pub known_bitmap_words: Vec<i32>,
    /// Sparse-tick backfill fetcher (stored on `V3PoolState` at
    /// registration; `None` for `Tracked` pools or when no Python fetcher
    /// was supplied). ADR-006 I/O trait object — pyo3-free trait, the
    /// `PyTickWordFetcher` adapter in `degenbot-python` wraps a Python
    /// callable.
    pub fetcher: Option<Arc<dyn TickWordFetcher>>,
    /// The CREATE2 deployer the Rust builder verified this pool's address
    /// against (Fork A). Equals the JSON row's `deployer`, or `factory`
    /// when the row had `null` (the `None -> factory` convention). For non-JSON
    /// pools, the factory (no lookup). Stored on the identity so a Python
    /// companion reads the verified deployer off the handle (no `chain_id`
    /// plumbing needed).
    pub deployer: Address,
    /// The CREATE2 init code hash the Rust builder verified this pool's
    /// address against (Fork A). The JSON row's `init_hash` when the
    /// `(chain, factory)` shipped, else the [`degenbot_uniswap::deployments`]
    /// `UNISWAP_V3_MAINNET_INIT_HASH` fallback (the retired Python `ClassVar`'s
    /// documented default for non-JSON V3 pools).
    pub init_hash: B256,
    /// The pool contract's storage-slot layout family (VERIFY2 T4). The
    /// struct-update default is for fixtures only — production
    /// registrations set it from the fork table
    /// (`V3_VARIANTS` in `degenbot-bot::connector_index`).
    pub slot_layout: ConcentratedLiquiditySlotLayout,
}

impl Default for RegisterV3PoolParams {
    /// The fixture/struct-update base: fixture geometry is Uniswap-shaped.
    /// PRODUCTION registrations must not rely on this — `slot_layout` is set
    /// from the fork table by the roster edge or the CREATE2-verified
    /// builder (see the type's doc).
    fn default() -> Self {
        Self {
            address: Address::ZERO,
            token0: Address::ZERO,
            token1: Address::ZERO,
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 0,
            tick: 0,
            tick_data: hashbrown::HashMap::new(),
            update_block: 0,
            tick_data_block: None,
            coverage: PoolTickCoverage::Sparse,
            known_bitmap_words: Vec::new(),
            fetcher: None,
            deployer: Address::ZERO,
            init_hash: B256::ZERO,
            slot_layout: ConcentratedLiquiditySlotLayout::UniswapV3,
        }
    }
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

// The lazy tick-range cache types live once in the concentrated liquidity family state module
// (`families/concentrated_liquidity/state_walk.rs`); re-exported here for
// the out-of-crate suites that construct `TickRangeCache` directly.
pub use crate::families::concentrated_liquidity::state_walk::{CachedTickRanges, TickRangeCache};

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
    /// The CREATE2 deployer the pool's address was verified against (Fork A).
    /// The JSON row's `deployer` (or `factory` for null), or the factory
    /// itself for non-JSON pools. Stored on the identity so the companion
    /// reads it off the handle.
    pub deployer: Address,
    /// The CREATE2 init code hash (Fork A). The JSON row's `init_hash`
    /// when shipped, else the Uniswap V3 mainnet fallback const. Off the
    /// handle, not the retired Python `ClassVar`.
    pub init_hash: B256,
    /// The pool contract's storage-slot layout family (VERIFY2 T4) — drives
    /// every slot-index consumer (divergence probe, sim-anchor projection,
    /// any direct slot serve). Immutable per pool.
    pub slot_layout: ConcentratedLiquiditySlotLayout,
}

/// V3 concentrated-liquidity pool state owned by [`crate::BotState`].
///
/// Carries authoritative mutable state plus a per-pool reorg journal. Swap
/// calculations read current mutable fields directly (never touch the journal);
/// `apply_swap`/`apply_liquidity_update` push reverse-apply deltas. Immutable
/// identity lives on [`V3PoolIdentity`]; look it up via the
/// typed `BotState::pool_slice` lens.
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
    /// (two-stamp pool state).
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
    /// count a removal/addition the seed already contains (solver
    /// desync: a pre-seed in-range Burn subtracted its net twice). Frozen —
    /// never advanced by `apply_swap`/`apply_liquidity_update`/`update_block`.
    pub initial_state_block: u64,

    /// The per-pool registration lifecycle: `Quarantined` during
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
    /// candidate (the block-N solve used pool@N-1 while on-chain@N
    /// has pool@N after the user swap).
    pub state_nonce: u64,

    /// Initialized ticks: tick index → (`liquidity_gross`, `liquidity_net`).
    pub tick_data: HashMap<i32, TickInfo>,

    /// The pinned snapshot seed: a copy of the `tick_data` supplied
    /// at registration, NEVER mutated by `apply_liquidity_update` /
    /// `apply_swap`. Retained so step-1 verify can compare the **seed**
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
    /// direction with its CURRENT state? The O(1) extremes verb lives once
    /// in the concentrated liquidity family state module
    /// (`families/concentrated_liquidity/state_walk.rs`).
    #[must_use]
    pub fn swap_is_viable(&self, zero_for_one: bool) -> bool {
        state_walk::swap_is_viable(self, zero_for_one)
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
    /// the given tick. Shared by V3/V4 sparse miss-detection; the verb lives
    /// once in the concentrated liquidity family state module
    /// (`families/concentrated_liquidity/state_seeding.rs`).
    #[must_use]
    pub fn word_of(tick: i32, tick_spacing: i32) -> i32 {
        state_seeding::word_of(tick, tick_spacing)
    }

    /// The first boundary bitmap word of a liquidity event whose sparse region
    /// this pool has not checked yet; `None` when every boundary word is known
    /// or the pool is `Tracked`. The predicate of the strict apply gate
    /// (`pump.strict_sparse_apply`) — see
    /// [`state_seeding::unknown_boundary_word`].
    #[must_use]
    pub fn unknown_boundary_word(
        &self,
        tick_lower: i32,
        tick_upper: i32,
        tick_spacing: i32,
    ) -> Option<i32> {
        state_seeding::unknown_boundary_word(self, tick_lower, tick_upper, tick_spacing)
    }

    /// Seed `known_bitmap_words` from the current `tick_data` keys' word
    /// positions. Called at registration for Sparse pools (the keys a partial
    /// snapshot carries are known); a no-op for Tracked pools (detection is
    /// bypassed when `coverage != Sparse`).
    pub fn seed_known_bitmap_words(&mut self, tick_spacing: i32) {
        state_seeding::seed_known_bitmap_words(self, tick_spacing);
    }

    /// Monotonic advance of the **price** clock (`update_block`) — the verb
    /// lives once in the concentrated liquidity family state module
    /// (`families/concentrated_liquidity/state_walk.rs`); a backward stamp
    /// outside a reorg panics (two-stamp rule).
    pub fn advance_update_block(&mut self, block: u64) {
        state_walk::advance_update_block(self, block);
    }

    /// Monotonic advance of the **liquidity** clock (`tick_data_block`) —
    /// same contract as [`Self::advance_update_block`] for the tick-map clock.
    pub fn advance_tick_data_block(&mut self, block: u64) {
        state_walk::advance_tick_data_block(self, block);
    }

    // `merge_tick_word` lives on the `ConcentratedLiquidityPoolMut` trait —
    // one body shared with `V4PoolState`. See
    // `impl ConcentratedLiquidityPoolMut for V3PoolState` in `registry.rs`.

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
            // Two-stamp rule: the price clock seeds at `update_block`; the
            // liquidity clock seeds at `tick_data_block` when the caller
            // split them (fresh-read builder), else falls back to the same
            // seed block. The historical-replay guard always keys on the
            // PRICE seed block (`update_block` — the block the SLOT0 scalar,
            // including active `liquidity`, reflects).
            tick_data_block: params.tick_data_block.unwrap_or(params.update_block),
            initial_state_block: params.update_block,
            state_nonce: 0,
            // ADR-close of the rolling-start direct-apply gap: a
            // freshly-registered `Tracked` pool starts `Quarantined` so no live
            // event can direct-apply before the two-step verify; `mark_registration_live`
            // (post-verify) is the sole transition to `Live`. `Sparse` pools
            // (no complete liquidity map → no pin, no step-2 verify) stay
            // `Live`/direct-apply — quarantining them would defer events with
            // nothing to protect and re-raise the retained-tail flush hazard at
            // `set_live`.
            registration_lifecycle: state_seeding::registration_lifecycle_for(params.coverage),
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
            slot_layout: params.slot_layout,
        };
        // pin the snapshot seed for Tracked pools so step-1 verify
        // compares the seed (not pump-mutated `tick_data`) against
        // on-chain@snapshot_block. Sparse pools have no complete seed. Computed
        // after the struct literal because `tick_data` is moved into `state` above.
        state.snapshot_seed = state_seeding::snapshot_seed_for(&state.tick_data, params.coverage);
        // A partial snapshot's tick_data keys are known regions.
        if params.coverage == PoolTickCoverage::Sparse {
            state.seed_known_bitmap_words(params.tick_spacing);
        }
        // Registration-supplied checked words (a caller's on-chain
        // checked-empty words) land in the same construction as the seed.
        state_seeding::seed_checked_bitmap_words(
            &mut state,
            &params.known_bitmap_words,
            params.coverage,
        );
        (identity, state)
    }

    // `apply_swap` + `apply_liquidity_update` live on the
    // `ConcentratedLiquidityPoolMut` trait — one body shared by V3 and V4.
    // See `impl ConcentratedLiquidityPoolMut for V3PoolState` in `registry.rs`.
    /// Invalidate the cached tick ranges (call after any state mutation) —
    /// the verb lives once in the concentrated liquidity family state module
    /// (`families/concentrated_liquidity/state_walk.rs`).
    pub fn invalidate_tick_range_cache(&self) {
        state_walk::invalidate_tick_range_cache(self);
    }

    /// Build an integer V3 tick range sequence using original U256 sqrt prices
    /// and i128→u128 liquidity (no f64 conversion) — the walk verb lives once
    /// in the concentrated liquidity family state module
    /// (`families/concentrated_liquidity/state_walk.rs`).
    ///
    /// Returns `None` if insufficient tick data.
    #[must_use]
    pub fn build_int_v3_sequence(
        &self,
        tick_spacing: i32,
        fee: u32,
        zero_for_one: bool,
    ) -> Option<IntV3TickRangeSequence> {
        state_walk::build_int_v3_sequence(self, tick_spacing, fee, zero_for_one)
    }
}

// The reorg-journal verbs live once in the concentrated liquidity state module
// (`families/concentrated_liquidity/state_journal.rs`); this impl forwards
// through the `JournalHost` field seam and carries no restore logic of its
// own.
impl ReorgPoolState for V3PoolState {
    fn restore_before_block(&mut self, block: u64) -> Result<(), JournalError> {
        state_journal::restore_before_block(self, block);
        Ok(())
    }

    fn discard_before_block(&mut self, block: u64) -> Result<(), JournalError> {
        state_journal::discard_before_block(self, block)
    }

    fn journal_len(&self) -> usize {
        state_journal::journal_len(self)
    }

    fn newest_block(&self) -> Option<u64> {
        state_journal::newest_block(self)
    }
}

impl state_journal::JournalHost for V3PoolState {
    fn journal(&self) -> &ReorgJournal<V3BlockDelta> {
        &self.journal
    }

    fn journal_mut(&mut self) -> &mut ReorgJournal<V3BlockDelta> {
        &mut self.journal
    }

    fn tick_data_mut(&mut self) -> &mut HashMap<i32, TickInfo> {
        &mut self.tick_data
    }

    fn apply_scalar_priors(&mut self, priors: &ScalarPriors) {
        self.sqrt_price_x96 = priors.sqrt_price_x96_before;
        self.liquidity = priors.liquidity_before;
        self.tick = priors.tick_before;
    }

    fn apply_clock_priors(
        &mut self,
        update_block_before: Option<u64>,
        tick_data_block_before: Option<u64>,
    ) {
        if let Some(b) = update_block_before {
            self.update_block = b;
        }
        if let Some(b) = tick_data_block_before {
            self.tick_data_block = b;
        }
    }

    fn bump_state_nonce_and_invalidate_cache(&mut self) {
        self.state_nonce = self.state_nonce.wrapping_add(1);
        self.invalidate_tick_range_cache();
    }
}

impl state_seeding::SeedingHost for V3PoolState {
    fn tick_data(&self) -> &HashMap<i32, TickInfo> {
        &self.tick_data
    }

    fn known_bitmap_words_mut(&mut self) -> &mut HashSet<i32> {
        &mut self.known_bitmap_words
    }

    fn known_bitmap_words(&self) -> &HashSet<i32> {
        &self.known_bitmap_words
    }

    fn coverage(&self) -> PoolTickCoverage {
        self.coverage
    }

    fn sqrt_price_x96(&self) -> U256 {
        self.sqrt_price_x96
    }

    fn liquidity(&self) -> u128 {
        self.liquidity
    }

    fn tick(&self) -> i32 {
        self.tick
    }

    fn update_block(&self) -> u64 {
        self.update_block
    }

    fn tick_data_block(&self) -> u64 {
        self.tick_data_block
    }

    fn journal_mut(&mut self) -> &mut ReorgJournal<V3BlockDelta> {
        &mut self.journal
    }
}

// The walk/clock/cache verbs live once in the concentrated liquidity state module
// (`families/concentrated_liquidity/state_walk.rs`); this impl is the
// family-specific field access those verbs go through.
impl state_walk::WalkHost for V3PoolState {
    fn sqrt_price_x96(&self) -> U256 {
        self.sqrt_price_x96
    }

    fn liquidity(&self) -> u128 {
        self.liquidity
    }

    fn tick(&self) -> i32 {
        self.tick
    }

    fn tick_data(&self) -> &HashMap<i32, TickInfo> {
        &self.tick_data
    }

    fn tick_range_cache(&self) -> &parking_lot::Mutex<TickRangeCache> {
        &self.cached_tick_ranges
    }

    fn update_block(&self) -> u64 {
        self.update_block
    }

    fn tick_data_block(&self) -> u64 {
        self.tick_data_block
    }

    fn set_update_block(&mut self, block: u64) {
        self.update_block = block;
    }

    fn set_tick_data_block(&mut self, block: u64) {
        self.tick_data_block = block;
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
    /// Final `sqrtPriceX96` after the swap walk — the companion's
    /// `simulate_exact_input_swap` builds `final_state` from it.
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
    /// solver must clamp its concentrated-liquidity-hop input to this value
    /// (with a rounding margin) so the exact-in loop terminates on
    /// `amountRemaining == 0` at the last funded tick instead of marching
    /// empty bitmap words to the price limit
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
    //! concentrated-liquidity-hop input clamp that prevents over-feeding a pool past its capacity
    //! (the path-5000 20M-gas EMPTY-HALT class, ).
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
// Faithful port of V3's `_calculate_swap`; splitting would obscure the loop.
#[expect(clippy::too_many_lines)]
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
                    // The net is the on-chain int128 — no narrowing
                    // conversion needed at the crossing site anymore.
                    let liquidity_net = info.liquidity_net;
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
            // V3 mirror of the V4 ELSE-branch miss check: an
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
// Tests for the relocated apply methods (ADR-014 D1 — concentrated liquidity half of Q1).
// These exercise `V3PoolState::apply_swap` / `apply_liquidity_update` directly
// against a constructed state, with no `BotState` registry, no buffer, no
// address index — the unit layer for the concentrated-liquidity-family apply contract. The
// integration coverage that previously lived on `BotState` (exercise through
// the registry dispatch) stays in `bot_core/v3_state.rs` / `bot_core/mod.rs`
// as the dispatch regression net.
// ===========================================================================
#[expect(clippy::expect_used, clippy::print_stderr, unused_imports)]
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
                liquidity_net: i128::try_from(liq).unwrap_or(0),
                block: 0,
            },
        );
        tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: liq_u128,
                liquidity_net: -i128::try_from(liq).unwrap_or(0),
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
        let _ = state_walk::get_cached_tick_ranges(&state, 60, true);
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
        let _ = state_walk::get_cached_tick_ranges(&state, 60, true);
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
            liquidity_net: 500i128,
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
        // a Swap rewrites the slot0 head AND crosses ticks, so it
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
    #[should_panic(expected = "monotonicity violated: tick_data_block")]
    fn replace_tick_data_backward_block_panics() {
        // Two-stamp monotonicity on the liquidity clock: replacing the tick map
        // with data from a block BELOW the current liquidity clock is a
        // backward stamp → panic loudly.
        let mut state = state_with_position(1_000_000u128);
        state.tick_data_block = 10;
        let empty: HashMap<i32, TickInfo> = HashMap::new();
        state.replace_tick_data(empty, 5, 60);
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
                liquidity_net: 7i128,
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
        // (2) Two-stamp rule: the tick-map replace advances only the LIQUIDITY clock;
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
        // replace_tick_data advances only the LIQUIDITY clock
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
    // ReorgPoolState trait (ADR-016 D4 — concentrated liquidity family adopts the trait).
    // `restore_before_block` returns `()`; the landed-at state lives in the
    // struct's own fields (read-after-restore). `V3RestoreResult` stays
    // internal to the impl and never escapes.
    // ------------------------------------------------------------------

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
    /// `no-profit` trap; see `logs/fixtures/v2_v3_v3_solver_divergence_25641093.md`,
    /// a gitignored logs/ artifact since removed).
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
                    liquidity_net: net,
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
        eprintln!("range liquidities: {liqs:?}");

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
        eprintln!("ofz r0 liquidity: {ofz_r0}");
        assert_eq!(
            ofz_r0, 5_407_362_545_736_161_987,
            "ofz r0 must NOT drain (gt exclusive)"
        );
    }
}

// ---------------------------------------------------------------------------
// Tests relocated from the `bot_core::v3_state` shim: the shim
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
                liquidity_net: i128::try_from(liq).unwrap_or(0),
                block: 0,
            },
        );
        tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: liq_u128,
                liquidity_net: -i128::try_from(liq).unwrap_or(0),
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
                slot_layout: ConcentratedLiquiditySlotLayout::UniswapV3,
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
        // V3 mirror of the V4 ELSE-branch miss check. A swap
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
        // The companion's simulate_exact_input_swap builds final_state from
        // the outcome, so v3_simulate_swap must return the
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
}

/// The V3 adapter's fixture seam for the merged concentrated liquidity state test suites
/// (ADR-080 D4).
#[cfg(test)]
mod fixture_impl;

/// A wholesale V3 pool-state overwrite from one source snapshot.
///
/// The V3 mirror of [`V4StateSync`](crate::v4_state::V4StateSync): a full-sync
/// replaces the scalars AND the tick map from one snapshot, so both clocks
/// (`update_block` and `tick_data_block`) move to the snapshot's block together
/// — a sanctioned reset, not the incremental backward stamp the two-stamp rule
/// forbids.
#[derive(Clone, Debug)]
pub struct V3StateSync {
    /// New sqrt price (Q64.96).
    pub sqrt_price_x96: U256,
    /// New active in-range liquidity.
    pub liquidity: u128,
    /// Current tick.
    pub tick: i32,
    /// Full tick-data overwrite (`liquidity_gross`/`liquidity_net`).
    pub tick_data: HashMap<i32, TickInfo>,
    /// Block number of this state update.
    pub update_block: u64,
}
