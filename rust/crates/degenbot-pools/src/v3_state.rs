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

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use alloy::primitives::{Address, B256, I256, U160, U256};

use crate::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};
use crate::state_history::{ReorgJournal, ScalarPriors, TickBefore, V3BlockDelta};
use crate::tick_bitmap::{compute_tick_ranges, gen_ticks, V3TickRangeForSolver};
use crate::tick_fetch::TickWordFetcher;
use crate::TickInfo;
use degenbot_cl_math::cl_lib::functions::tick_position;
use degenbot_cl_math::cl_lib::swap_math::compute_swap_step_v3;
use degenbot_cl_math::cl_lib::tick_math::{
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
    pub tick_priors: Vec<(i32, TickInfo)>,
}

// ---------------------------------------------------------------------------
// V3 pool state
// ---------------------------------------------------------------------------

/// Cached tick ranges for a single pool, keyed by direction.
#[derive(Clone, Debug, Default)]
pub struct TickRangeCache {
    zfo: Option<Arc<[V3TickRangeForSolver]>>,
    ofz: Option<Arc<[V3TickRangeForSolver]>>,
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
    pub update_block: u64,

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

    /// Merge a fetched tick word's ticks into this state's `tick_data` +
    /// mark the word as known + invalidate the cached tick ranges. Used by
    /// the fetch+retry override loop (merges into the TRANSIENT override
    /// state, not registered `BotState`). Mirrors `BotState::merge_tick_word`.
    pub fn merge_tick_word(&mut self, fetched: &crate::tick_fetch::FetchedTickWord) {
        for (tick, info) in &fetched.ticks {
            self.tick_data.insert(*tick, info.clone());
        }
        self.known_bitmap_words.insert(fetched.word);
        self.invalidate_tick_range_cache();
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

    /// Apply a V3 Swap event to this pool's mutable `slot0` scalars + `tick_data`,
    /// capturing reverse-apply priors into the reorg journal (ADR-014 D1 —
    /// relocated from `BotState::apply_v3_swap_by_pool_id`).
    ///
    /// Journal-capture policy (ADR-014 D2 / Q2): the delta carries
    /// `scalar_priors: Some(..)` (a Swap changes the `slot0` head on every
    /// event) plus the per-tick priors for any ticks this event mutates. The
    /// tick-priors loop and the scalar-priors capture are inline here — no
    /// shared helper, because `apply_swap` *writes* the passed tick info into
    /// `tick_data` (it carries new tick values from the event), whereas
    /// [`Self::apply_liquidity_update`] *reads* the current tick priors before
    /// mutating (the delta is applied to existing boundary ticks). Same loop
    /// shape, different work — kept separate to stay honest about the cycle.
    ///
    /// `tick_priors`: ticks the event reports as changed, with their new
    /// `TickInfo`. A tick with no prior entry is journaled with
    /// `liquidity_gross_before: None` (on rollback, delete it).
    pub fn apply_swap(
        &mut self,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)],
    ) {
        // Capture priors for any ticks being mutated by this event, so reorg
        // rollback can reverse-apply them. A tick that had no prior entry gets
        // `liquidity_gross_before: None` (on rollback, delete it).
        let mut journaled_priors: Vec<(i32, TickBefore)> = Vec::with_capacity(tick_priors.len());
        for &(tick_index, ref new_info) in tick_priors {
            let prior = self.tick_data.get(&tick_index).cloned();
            journaled_priors.push((
                tick_index,
                TickBefore {
                    liquidity_gross_before: prior.as_ref().map(|p| p.liquidity_gross),
                    liquidity_net_before: prior
                        .as_ref()
                        .map_or(alloy::primitives::I256::ZERO, |p| p.liquidity_net),
                },
            ));
            self.tick_data.insert(tick_index, new_info.clone());
        }

        // Journal scalar priors (swap scalars change on every Swap).
        self.journal.push_delta(V3BlockDelta {
            block: block_number,
            scalar_priors: Some(ScalarPriors {
                sqrt_price_x96_before: self.sqrt_price_x96,
                liquidity_before: self.liquidity,
                tick_before: self.tick,
            }),
            tick_priors: journaled_priors,
        });

        self.sqrt_price_x96 = sqrt_price_x96;
        self.liquidity = liquidity;
        self.tick = tick;
        self.update_block = block_number;
        self.invalidate_tick_range_cache();
    }

    /// Apply a V3 liquidity update (Mint/Burn) to this pool's `tick_data`,
    /// capturing reverse-apply priors into the reorg journal (ADR-014 D1 —
    /// relocated from `BotState::apply_v3_liquidity_update_by_pool_id`).
    ///
    /// Applies the delta via [`apply_liquidity_to_tick_range`] (matching
    /// Solidity `Tick.update`: `liquidity_gross += delta` at both boundaries;
    /// `liquidity_net` `+=` at lower, `-=` at upper), advances `update_block`,
    /// invalidates the tick-range cache.
    ///
    /// Journal-capture policy (ADR-004 / ADR-014 Q2): the delta carries
    /// `scalar_priors: None` — `Mint`/`Burn` mutate `tick_data` only, NOT the
    /// active `liquidity` scalar, so restore skips the scalar write-back for
    /// these deltas. Only the two boundary-tick priors (captured BEFORE
    /// mutation) are reverse-applied on rollback.
    pub fn apply_liquidity_update(
        &mut self,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) {
        // Capture tick priors before mutation so reorg rollback can reverse-
        // apply. A tick that had no prior entry (newly initialized by this
        // Mint) gets `liquidity_gross_before: None` (on rollback, delete it).
        let mut journaled_priors: Vec<(i32, TickBefore)> = Vec::with_capacity(2);
        for &tick_idx in &[tick_lower, tick_upper] {
            let prior = self.tick_data.get(&tick_idx).cloned();
            journaled_priors.push((
                tick_idx,
                TickBefore {
                    liquidity_gross_before: prior.as_ref().map(|p| p.liquidity_gross),
                    liquidity_net_before: prior
                        .as_ref()
                        .map_or(alloy::primitives::I256::ZERO, |p| p.liquidity_net),
                },
            ));
        }

        crate::tick_bitmap::apply_liquidity_to_tick_range(
            &mut self.tick_data,
            tick_lower,
            tick_upper,
            liquidity_delta,
            block_number,
        );

        // Journal: Mint/Burn mutate tick_data only, NOT the active `liquidity`
        // scalar — so the journal carries no scalar priors for this tick-only
        // event (scalar_priors: None). Only the two tick priors are reverse-
        // applied on rollback. See ADR-004.
        self.journal.push_delta(V3BlockDelta {
            block: block_number,
            scalar_priors: None,
            tick_priors: journaled_priors,
        });

        self.update_block = block_number;
        self.invalidate_tick_range_cache();
    }

    /// Invalidate the cached tick ranges (call after any state mutation).
    pub fn invalidate_tick_range_cache(&self) {
        let mut cache = self.cached_tick_ranges.lock();
        cache.zfo = None;
        cache.ofz = None;
    }

    /// Get cached tick ranges for the given direction, computing and caching
    /// if absent. Uses `max_ranges=15` so all callers can slice the result.
    fn get_cached_tick_ranges(
        &self,
        tick_spacing: i32,
        zero_for_one: bool,
    ) -> Option<Arc<[V3TickRangeForSolver]>> {
        {
            let cache = self.cached_tick_ranges.lock();
            let slot = if zero_for_one { &cache.zfo } else { &cache.ofz };
            if let Some(ranges) = slot {
                return Some(Arc::clone(ranges));
            }
        }

        // Not cached — compute and store
        let ranges = compute_tick_ranges(
            &self.tick_data,
            self.tick,
            tick_spacing,
            self.liquidity,
            zero_for_one,
            15,
        )
        .map(|(ranges, _)| Arc::<[V3TickRangeForSolver]>::from(ranges));

        if let Some(ref r) = ranges {
            let mut cache = self.cached_tick_ranges.lock();
            if zero_for_one {
                cache.zfo = Some(Arc::clone(r));
            } else {
                cache.ofz = Some(Arc::clone(r));
            }
        }

        ranges
    }

    /// Build an integer V3 tick range sequence with up to `max_ranges` ranges,
    /// using original U256 sqrt prices and i128→u128 liquidity (no f64 conversion).
    ///
    /// Produces an [`IntV3TickRangeSequence`] suitable for the integer-exact
    /// V3-V3 solver, preserving full precision. Returns `None` if insufficient
    /// tick data.
    #[must_use]
    pub fn build_int_v3_sequence(
        &self,
        tick_spacing: i32,
        fee: u32,
        zero_for_one: bool,
        max_ranges: usize,
    ) -> Option<IntV3TickRangeSequence> {
        let ranges = self.get_cached_tick_ranges(tick_spacing, zero_for_one)?;
        let use_ranges = ranges.get(..ranges.len().min(max_ranges))?;

        let gamma_numer = u64::from(1_000_000 - fee);
        let fee_denom = 1_000_000u64;

        let mut int_ranges = Vec::with_capacity(use_ranges.len());
        for (i, r) in use_ranges.iter().enumerate() {
            let sqrt_price_x96 = if i == 0 {
                self.sqrt_price_x96
            } else if zero_for_one {
                use_ranges[i - 1].sqrt_price_upper
            } else {
                use_ranges[i - 1].sqrt_price_lower
            };

            let range_liquidity = if i == 0 {
                self.liquidity
            } else {
                let mut l = self.liquidity.cast_signed();
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
            };

            int_ranges.push(IntV3TickRangeHop {
                liquidity: range_liquidity,
                sqrt_price_x96,
                sqrt_price_lower_x96: r.sqrt_price_lower,
                sqrt_price_upper_x96: r.sqrt_price_upper,
                gamma_numer,
                fee_denom,
                zero_for_one,
            });
        }

        IntV3TickRangeSequence::new(int_ranges).ok()
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
#[allow(unused_assignments)]
// `tick` tracks the contract's post-step tick; kept faithful to the V3
// `_calculate_swap` loop even though this pure simulator returns only amounts.
#[allow(clippy::too_many_lines)] // faithful port of V3's `_calculate_swap`; splitting would obscure the loop.
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
    let ticks = gen_ticks(
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
#[cfg(test)]
mod apply_inherent_tests {
    #![allow(unused_imports)]
    use super::*;
    use crate::state_history::{ReorgJournal, ScalarPriors, TickBefore, V3BlockDelta};
    use alloy::primitives::{I256, U128, U256};
    use std::collections::{HashMap, HashSet};

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
            assert!(cache.zfo.is_none(), "zfo cache must be invalidated");
            assert!(cache.ofz.is_none(), "ofz cache must be invalidated");
        }
    }

    #[test]
    fn apply_liquidity_update_mutates_ticks_advances_block_journals_tick_priors_only() {
        // What: apply_liquidity_update must (1) apply the delta to BOTH
        // boundary ticks per Solidity Tick.update (gross += at both, net += at
        // lower, net -= at upper), (2) advance update_block, (3) push a
        // V3BlockDelta with scalar_priors: None (Mint/Burn is tick-only — the
        // slot0 head is untouched), (4) journal the two tick priors captured
        // BEFORE mutation, (5) NOT change sqrt_price / liquidity / tick.
        // Why: ADR-004 — tick-only events carry no scalar priors; restore
        // skips the scalar write-back for these deltas.
        let liq = 1_000_000u128;
        let mut state = state_with_position(liq);

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
        // (2) update_block advanced.
        assert_eq!(state.update_block, 9);
        // (5) slot0 scalars UNCHANGED.
        assert_eq!(state.sqrt_price_x96, sp_before);
        assert_eq!(state.liquidity, liq_before);
        assert_eq!(state.tick, tick_before);
        // (3)/(4) journal gained exactly one delta at block 9.
        assert_eq!(state.journal.len(), before_len + 1);
        assert_eq!(state.journal.newest_block(), Some(9));
    }
}
