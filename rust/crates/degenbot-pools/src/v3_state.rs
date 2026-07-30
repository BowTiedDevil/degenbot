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
use crate::state_history::{JournalError, ReorgJournal, ReorgPoolState, V3BlockDelta};
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

    // `merge_tick_word` lives on the `ConcentratedLiquidityPoolMut` trait
    // (ADR-017 slice 1) — the body was the byte-identical twin of
    // `V4PoolState::merge_tick_word`; the trait dedups the two. See
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
            // T47PPB: the active-set walk has no tuple budget, so feed depth is
            // bounded by data, not solvability. 24 visible ranges ≈ 2× the
            // word-boundary ring around the current tick for dense inserted
            // liquidity (block-25641093 pool 0xDcA4…65c9 needs ≥17 to see the
            // -22900 liquidity activation past the mid-spine flank pairs).
            24,
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
        self.update_block = result.block;
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
    use crate::registry::ConcentratedLiquidityPoolMut;
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
        let mut new_data = std::collections::HashMap::new();
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
        // (2) update_block advanced to 5.
        assert_eq!(state.update_block, 5);
        // (3) known_bitmap_words seeded from the new keys (word of tick 120
        // at spacing 60 = 120.div_euclid(60) >> 8 = 2 >> 8 = 0).
        assert!(state
            .known_bitmap_words
            .contains(&V3PoolState::word_of(120, 60)));
        // (4) cache invalidated.
        {
            let cache = state.cached_tick_ranges.lock();
            assert!(cache.zfo.is_none());
            assert!(cache.ofz.is_none());
        }
        // Scalars untouched.
        assert_eq!(state.sqrt_price_x96, sp_before);
    }

    #[test]
    fn replace_tick_data_does_not_rewind_block() {
        // update_block must NOT rewind when the supplied block is older
        // (monotonic — mirrors the sync_v3_pool_state contract).
        let liq = 1_000_000u128;
        let mut state = state_with_position(liq);
        state.update_block = 10;
        let empty: std::collections::HashMap<i32, TickInfo> = std::collections::HashMap::new();
        state.replace_tick_data(empty, 3, 60);
        assert_eq!(
            state.update_block, 10,
            "update_block must not rewind to an older block"
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
        // delta and writes the pre-swap scalars back. `update_block` is set
        // to `result.block` (the oldest popped delta's block = the restore
        // point) — absorbing the existing `BotState::v3_restore_before_block`
        // behavior verbatim.
        let liq = 1_000_000u128;
        let mut state = state_with_position(liq);
        let pre_sqrt = state.sqrt_price_x96;
        let pre_liq = state.liquidity;
        let pre_tick = state.tick;

        let new_sqrt = U256::from(2u128) << 96;
        state.apply_swap(new_sqrt, liq + 1, 1, 7, &[]);
        assert_eq!(state.update_block, 7);

        let result: Result<(), JournalError> = state.restore_before_block(7);
        assert!(result.is_ok());
        assert_eq!(state.sqrt_price_x96, pre_sqrt);
        assert_eq!(state.liquidity, pre_liq);
        assert_eq!(state.tick, pre_tick);
        assert_eq!(
            state.update_block, 7,
            "update_block set to the restore-point block (result.block)"
        );
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
}
