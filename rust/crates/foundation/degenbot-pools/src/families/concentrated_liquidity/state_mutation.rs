//! The concentrated liquidity family's state-mutation verbs (ADR-080 D1, step 4)
//! — the write half of the family tree, beside the read door.
//!
//! The V3 and V4 state structs (`crate::v3_state` / `crate::v4_state`) carry the
//! same reorg journal, the same initialized-tick map, the same slot0 scalars,
//! the same two-stamp clocks, the same per-mutation nonce, and the same lazy
//! tick-range cache, so the five write verbs — `replace_tick_data`,
//! `merge_tick_word`, `mark_bitmap_words_known`, `apply_swap`,
//! `apply_liquidity_update` — are written once here, data-generically over the
//! [`StateMutationHost`] field seam, and the per-design impls keep only thin
//! forwarders. What genuinely differs stays per-adapter: the registration
//! seeding scope, the hook-keyed V4 identity, and the event shapes that reach
//! these verbs.
//!
//! # The nonce contract
//!
//! Every verb that changes observable state bumps `state_nonce` exactly once.
//! The nonce is the dispatch seam's staleness gate (`Observe::StateNonce`): a
//! solver candidate computed against pre-mutation state is rejected only if the
//! nonce moved. Writing these verbs twice already cost exactly that — V4 bumped
//! on a tick-map replace and a word merge while V3 did not, so a V3 candidate
//! solved against a superseded tick map survived the gate. `state_mutation`'s
//! merged battery (`tests.rs`) pins the bump per verb on BOTH adapters, which
//! makes that drift unrepresentable rather than merely untested.
//!
//! The one deliberate exception is `mark_bitmap_words_known`: the known-word set
//! is a miss-detection fact the solver never reads, so it neither bumps the
//! nonce nor invalidates the tick-range cache.

use alloy::primitives::{I256, U256};
use hashbrown::HashMap;

use super::state_journal::JournalHost;
use super::state_seeding::{self, SeedingHost};
use super::state_walk::{self, WalkHost};
use crate::sim::registry::ConcentratedLiquidityPool;
use crate::sim::tick_fetch::FetchedTickWord;
use crate::state::state_history::{ScalarPriors, TickBefore, V3BlockDelta};
use crate::state::v3_state::{PoolTickCoverage, V3PoolState};
use crate::state::v4_state::V4PoolState;
use crate::TickInfo;

/// The field seam the shared mutation verbs read and write through —
/// implemented by each design's state struct (`V3PoolState`, `V4PoolState`),
/// whose tick map, scalars, clocks, journal, nonce, and cache are
/// shape-identical. It composes the three sibling seams the verbs already need
/// ([`WalkHost`] for the clocks and the cache, [`SeedingHost`] for the tick map
/// and the known-word set, [`JournalHost`] for the reorg journal) and adds only
/// the writes those seams do not expose.
pub(crate) trait StateMutationHost: WalkHost + SeedingHost + JournalHost {
    /// The frozen registration/seed block — the historical-replay guard's
    /// anchor, never advanced by a mutation.
    fn initial_state_block(&self) -> u64;

    /// Bump the per-mutation nonce the dispatch seam's staleness gate reads
    /// (wrapping — a nonce is a different-from-before token, not a counter).
    fn bump_state_nonce(&mut self);

    /// Write the slot0 price scalar.
    fn set_sqrt_price_x96(&mut self, sqrt_price_x96: U256);

    /// Write the active-liquidity scalar.
    fn set_active_liquidity(&mut self, liquidity: u128);

    /// Write the current tick.
    fn set_current_tick(&mut self, tick: i32);
}

/// The journal prior for one tick: its full pre-mutation row, with `None`
/// marking a tick the mutation is the first to initialize (a rollback deletes
/// it).
fn tick_before(prior: Option<&TickInfo>) -> TickBefore {
    TickBefore {
        liquidity_gross_before: prior.map(|p| p.liquidity_gross),
        liquidity_net_before: prior.map_or(0, |p| p.liquidity_net),
        block_before: prior.map_or(0, |p| p.block),
    }
}

/// `ConcentratedLiquidityPoolMut::replace_tick_data` — wholesale-replace the
/// `tick_data` map, advance the **liquidity** clock (`tick_data_block`) if
/// newer (monotonic — a backward stamp panics), re-seed `known_bitmap_words`
/// from the new keys' word positions, invalidate the cached tick ranges, and
/// bump the nonce. Scalars (`sqrt_price_x96`/`liquidity`/`tick`) ARE untouched
/// and the **price** clock (`update_block`) is NOT advanced — a tick-map
/// replace never claims the price is fresh (two-stamp rule).
/// `tick_data_block` is the block the new tick map is from.
///
/// No journal delta — a wholesale replace has undefined rollback semantics;
/// the pump is the authority for event-derived ticks (mirrors
/// `sync_v3_pool_state`).
pub(crate) fn replace_tick_data<H: StateMutationHost>(
    host: &mut H,
    tick_data: HashMap<i32, TickInfo>,
    tick_data_block: u64,
    tick_spacing: i32,
) {
    *host.tick_data_mut() = tick_data;
    state_walk::advance_tick_data_block(host, tick_data_block);
    host.bump_state_nonce();
    state_seeding::seed_known_bitmap_words(host, tick_spacing);
    state_walk::invalidate_tick_range_cache(host);
}

/// `ConcentratedLiquidityPoolMut::merge_tick_word` — merge a fetched
/// tick-bitmap word's ticks into this state's `tick_data`, mark the word as
/// known in `known_bitmap_words`, invalidate the cached tick ranges, and bump
/// the nonce. Scalars (`sqrt_price_x96`/`liquidity`/`tick`) are untouched. Used
/// by the fetch+retry miss path ([`crate::sim::tick_fetch::TickWordSource`]) and
/// the `simulate_swap_with_override` transient-state merge loop.
///
/// The insert is two-stamp-disciplined: a fetched word can predate an event the
/// pump applied while its staged fetch was in flight (the word re-fetches at its
/// context block after a Raced retry), so a fetched tick with an OLDER stamp
/// than a resident tick never regresses the resident value.
///
/// No journal delta — a fetched-word merge mutates `tick_data` only; reorg
/// rollback covers it via the surrounding event's delta, not a merge delta
/// (mirrors the no-journal contract of [`replace_tick_data`]).
pub(crate) fn merge_tick_word<H: StateMutationHost>(host: &mut H, fetched: &FetchedTickWord) {
    for (tick, info) in &fetched.ticks {
        let resident_is_fresher = WalkHost::tick_data(host)
            .get(tick)
            .is_some_and(|resident| resident.block > info.block);
        if !resident_is_fresher {
            host.tick_data_mut().insert(*tick, info.clone());
        }
    }
    host.known_bitmap_words_mut().insert(fetched.word);
    host.bump_state_nonce();
    state_walk::invalidate_tick_range_cache(host);
}

/// `ConcentratedLiquidityPoolMut::mark_bitmap_words_known` — record `words` as
/// known, **Sparse pools only**: a Tracked pool's
/// bitmap is complete, so the known set is never written (writing it would be
/// dead state). The word discipline: a word is known only when its WHOLE tick
/// set is established (a fetched word, a full-sync replace, or checked words
/// passed at the FFI `update_tick_data` boundary) — an apply event NEVER marks
/// its touched word.
///
/// No journal delta (the known set is a miss-detection fact, not a tick-map
/// mutation), no cache invalidation (the known set does not feed the cached
/// tick ranges), and no nonce bump (nothing the solver reads changed).
pub(crate) fn mark_bitmap_words_known<H: StateMutationHost>(host: &mut H, words: &[i32]) {
    if host.coverage() == PoolTickCoverage::Sparse {
        host.known_bitmap_words_mut().extend(words.iter().copied());
    }
}

/// `ConcentratedLiquidityPoolMut::apply_swap` — apply a Swap event to the
/// mutable `slot0` scalars + `tick_data`, capturing reverse-apply priors into
/// the reorg journal (ADR-014 D1). The delta carries `scalar_priors: Some(..)`
/// (a Swap changes the slot0 head on every event) plus per-tick priors for any
/// ticks this event mutates. A tick with no prior entry is journaled with
/// `liquidity_gross_before: None` (on rollback, delete it).
///
/// `tick_priors`: ticks the event reports as changed, with their new
/// `TickInfo`. The verb *writes* the passed tick info into `tick_data` (it
/// carries new tick values from the event), whereas [`apply_liquidity_update`]
/// *reads* the current tick priors before mutating — same loop shape, different
/// work.
pub(crate) fn apply_swap<H: StateMutationHost>(
    host: &mut H,
    sqrt_price_x96: U256,
    liquidity: u128,
    tick: i32,
    block_number: u64,
    tick_priors: &[(i32, TickInfo)],
) {
    let mut journaled_priors: Vec<(i32, TickBefore)> = Vec::with_capacity(tick_priors.len());
    for (tick_index, new_info) in tick_priors {
        let prior = WalkHost::tick_data(host).get(tick_index).cloned();
        journaled_priors.push((*tick_index, tick_before(prior.as_ref())));
        host.tick_data_mut().insert(*tick_index, new_info.clone());
    }

    // A Swap rewrites the slot0 head AND crosses ticks: it advances BOTH
    // clocks, and records both pre-event clock values for reorg restore
    // (two-stamp rule).
    let tick_before = WalkHost::tick(host);
    let delta = V3BlockDelta {
        block: block_number,
        scalar_priors: Some(ScalarPriors {
            sqrt_price_x96_before: WalkHost::sqrt_price_x96(host),
            liquidity_before: WalkHost::liquidity(host),
            tick_before: WalkHost::tick(host),
        }),
        update_block_before: Some(WalkHost::update_block(host)),
        tick_data_block_before: Some(WalkHost::tick_data_block(host)),
        tick_priors: journaled_priors,
    };
    JournalHost::journal_mut(host).push_delta(delta);

    host.set_sqrt_price_x96(sqrt_price_x96);
    host.set_active_liquidity(liquidity);
    host.set_current_tick(tick);
    // A Swap arrives LIVE (block > seed) in the steady state or as a backfill
    // replay (block <= seed). Live applies are STRICT monotonic (a backward
    // stamp outside a reorg panics); backfill replays drain older events and
    // are a sanctioned monotonic no-op on clocks they already cover (never
    // rewind a head-fresh pool below seed).
    if block_number > host.initial_state_block() {
        state_walk::advance_update_block(host, block_number);
        state_walk::advance_tick_data_block(host, block_number);
    } else {
        if block_number > WalkHost::update_block(host) {
            host.set_update_block(block_number);
        }
        if block_number > WalkHost::tick_data_block(host) {
            host.set_tick_data_block(block_number);
        }
    }
    host.bump_state_nonce();
    // Only invalidate the tick-range cache when the swap actually changes the
    // walk inputs (tick_data via tick_priors, or current tick). An in-range
    // swap that stays at the same tick without crossing any initialized ticks
    // leaves the walk result identical, so the cache is preserved (the
    // dominant case for active pools — they swap within their current range
    // every block).
    if tick != tick_before || !tick_priors.is_empty() {
        state_walk::invalidate_tick_range_cache(host);
    }
}

/// `ConcentratedLiquidityPoolMut::apply_liquidity_update` — apply a liquidity
/// update (V3 Mint/Burn, V4 `ModifyLiquidity`) to `tick_data`, capturing
/// reverse-apply priors into the reorg journal (ADR-014 D1). Applies the delta
/// via [`crate::state::tick_bitmap::apply_liquidity_to_tick_range`] (matching
/// Solidity `Tick.update`), advances the liquidity clock, invalidates the
/// tick-range cache, and bumps the nonce.
///
/// Journal-capture policy: when the `[tick_lower, tick_upper)` region straddles
/// the pool's current `tick`, the event ALSO changes the on-chain active
/// `liquidity` scalar by `delta` (a Mint adds range liquidity to the active
/// pool; a Burn removes it). In that case the delta carries
/// `scalar_priors: Some(..)` so restore rolls the scalar back. Out-of-range
/// events mutate `tick_data` only and carry `scalar_priors: None` (ADR-004 —
/// restore skips the scalar write-back). The two boundary-tick priors (captured
/// BEFORE mutation) are reverse-applied in both cases.
pub(crate) fn apply_liquidity_update<H: StateMutationHost>(
    host: &mut H,
    tick_lower: i32,
    tick_upper: i32,
    liquidity_delta: i128,
    block_number: u64,
) {
    // Historical-replay guard (the Python `_initial_state_block` guard): only a
    // genuinely in-range event STRICTLY AFTER the seed block adjusts the
    // active-liquidity scalar. The seed's `liquidity` already reflects every
    // on-chain event <= initial_state_block, so replaying one (e.g. a
    // backfilled Burn applied after registration against head) would
    // double-count it. Such a replay still mutates the tick map, but the scalar
    // write-back + journal stay tick-only (scalar_priors: None).
    let in_range = tick_lower <= WalkHost::tick(host)
        && WalkHost::tick(host) < tick_upper
        && block_number > host.initial_state_block();

    let mut journaled_priors: Vec<(i32, TickBefore)> = Vec::with_capacity(2);
    for tick_idx in [tick_lower, tick_upper] {
        let prior = WalkHost::tick_data(host).get(&tick_idx).cloned();
        journaled_priors.push((tick_idx, tick_before(prior.as_ref())));
    }

    crate::state::tick_bitmap::apply_liquidity_to_tick_range(
        host.tick_data_mut(),
        tick_lower,
        tick_upper,
        liquidity_delta,
        block_number,
    );

    // A liquidity event ALWAYS mutates the tick map (advance the liquidity
    // clock); it advances the PRICE clock only when in-range post-seed (the
    // scalar adjust). An out-of-range or historical-replay event leaves the
    // slot0 head byte-identical, so the price clock must NOT move (the
    // two-stamp rule). The price-clock prior is `None` in that case so a reorg
    // restore leaves `update_block` untouched.
    let delta = V3BlockDelta {
        block: block_number,
        scalar_priors: in_range.then(|| ScalarPriors {
            sqrt_price_x96_before: WalkHost::sqrt_price_x96(host),
            liquidity_before: WalkHost::liquidity(host),
            tick_before: WalkHost::tick(host),
        }),
        update_block_before: in_range.then(|| WalkHost::update_block(host)),
        tick_data_block_before: Some(WalkHost::tick_data_block(host)),
        tick_priors: journaled_priors,
    };
    JournalHost::journal_mut(host).push_delta(delta);

    if in_range {
        // Invariant-guarded: the in-range branch implies the range straddles
        // the current tick, so `Some` is guaranteed (loud panic on a real
        // break).
        #[expect(clippy::expect_used)]
        let liquidity = in_range_active_liquidity(
            WalkHost::liquidity(host),
            WalkHost::tick(host),
            tick_lower,
            tick_upper,
            liquidity_delta,
        )
        .expect("in-range branch implies the range straddles the current tick");
        host.set_active_liquidity(liquidity);
    }

    if block_number > host.initial_state_block() {
        state_walk::advance_tick_data_block(host, block_number);
        if in_range {
            state_walk::advance_update_block(host, block_number);
        }
    } else {
        // Backfill replay (block <= seed): sanctioned monotonic no-op — mutate
        // the tick map but never rewind the liquidity clock it already covers.
        if block_number > WalkHost::tick_data_block(host) {
            host.set_tick_data_block(block_number);
        }
    }
    host.bump_state_nonce();
    state_walk::invalidate_tick_range_cache(host);
}

/// Compute the new active (in-range) `liquidity` scalar when a
/// Mint/Burn/`ModifyLiquidity` event's `[tick_lower, tick_upper)` region
/// straddles the pool's current `tick`.
///
/// Parity with the pure reference
/// `degenbot-concentrated-liquidity-math::liquidity_mapping::apply_liquidity_mapping_update`
/// and the Python companions (`UniswapV3Pool`/`UniswapV4Pool.update_liquidity_map`):
/// an in-range liquidity event changes the pool's on-chain `liquidity` field by
/// `delta` in addition to the boundary-tick map mutation. Without this the
/// solver's active-liquidity scalar drifts from on-chain even though
/// `sqrt_price_x96` / `tick` stay byte-identical — an in-range Burn leaves it
/// too high (capable of overdrawing / wrong-crossing estimates), an in-range
/// Mint leaves it too low.
///
/// Computed in wide `I256` so the intermediate never overflows for valid
/// inputs. Returns `Some(new_liquidity)` when the current tick is in range,
/// else `None` (out-of-range events leave the scalar untouched).
///
/// # Panics
///
/// Panics if the in-range result would be negative (an invariant violation —
/// on-chain liquidity can never be negative) or exceeds `u128::MAX` (real
/// surprise overflow). Out-of-range events never reach the arithmetic.
#[expect(clippy::expect_used)] // invariant-guarded conversions panic loudly on a true break
fn in_range_active_liquidity(
    current_liquidity: u128,
    current_tick: i32,
    tick_lower: i32,
    tick_upper: i32,
    liquidity_delta: i128,
) -> Option<u128> {
    if !(tick_lower <= current_tick && current_tick < tick_upper) {
        return None;
    }
    let base = I256::try_from(U256::from(current_liquidity)).expect("u128 fits I256");
    let delta = I256::try_from(liquidity_delta).expect("i128 fits I256");
    let new_active = base + delta;
    assert!(
        new_active >= I256::ZERO,
        "in-range liquidity adjustment violated invariant: {base} + {delta} < 0"
    );
    let narrowed =
        u128::try_from(new_active).expect("non-negative in-range reachable liquidity fits u128");
    Some(narrowed)
}

/// Mutable concentrated-liquidity-family trait — the write twin of
/// [`ConcentratedLiquidityPool`] (ADR-014 D2b). Carries the concentrated
/// liquidity mutators the dispatch row reaches through
/// `&mut dyn ConcentratedLiquidityPoolMut`, so a caller holding a resolved key
/// never names `V3PoolState` or `V4PoolState`.
///
/// The behavior lives ONCE in this module's data-generic verbs; the two impls
/// below are the thin per-design forwarders (`tick_spacing` is passed in
/// because it lives on the identity slice, not the state struct — the caller
/// reads it off `V3PoolIdentity.tick_spacing` /
/// `V4PoolIdentity.pool_key.tick_spacing` before dispatching, so the trait
/// stays identity-agnostic and the two state-struct impls are byte-identical).
pub trait ConcentratedLiquidityPoolMut: ConcentratedLiquidityPool {
    /// Wholesale-replace the `tick_data` map — see
    /// [`replace_tick_data`] for the full contract.
    ///
    /// Returns `true` (the concentrated liquidity mutator always succeeds; the
    /// dispatch site's `false` return is reserved for V2 / non-concentrated-
    /// liquidity / unregistered pools).
    fn replace_tick_data(
        &mut self,
        tick_data: HashMap<i32, TickInfo>,
        tick_data_block: u64,
        tick_spacing: i32,
    ) -> bool;

    /// Merge a fetched tick-bitmap word — see [`merge_tick_word`].
    ///
    /// Returns `true` (the concentrated liquidity mutator always succeeds; the
    /// dispatch site's `false` return is reserved for non-concentrated-liquidity
    /// / unregistered pools — same convention as [`Self::replace_tick_data`]).
    fn merge_tick_word(&mut self, fetched: &FetchedTickWord) -> bool;

    /// Record `words` as known in `known_bitmap_words` — see
    /// [`mark_bitmap_words_known`] for the sparse-only, no-nonce contract.
    fn mark_bitmap_words_known(&mut self, words: &[i32]);

    /// Apply a Swap event — see [`apply_swap`].
    fn apply_swap(
        &mut self,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)],
    );

    /// Apply a liquidity update (V3 Mint/Burn, V4 `ModifyLiquidity`) — see
    /// [`apply_liquidity_update`].
    fn apply_liquidity_update(
        &mut self,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    );

    /// Push the registration/seed genesis anchor: a `before == after` journal
    /// delta at `block`, advancing NO clock.
    ///
    /// The delta records the CURRENT scalars + both clocks as its “before”
    /// priors, so a restore pops to the exact seeded state, and the non-empty
    /// journal keeps `has_state_prior_to` true from registration — a
    /// mid-window reorg restores to the seeded state instead of the graceful
    /// `NoStatePriorToBlock` pump shutdown an empty journal triggers.
    ///
    /// Advancing no clock is the point, not an omission: registration can seed
    /// price at HEAD past the DB map block, so an `apply_swap`-shaped genesis
    /// would backward-panic the price clock and falsely claim the tick map
    /// reaches head. Because `before == after` it is safe; call once at
    /// registration, mirroring the V2/Curve genesis-anchor pattern.
    fn seed_genesis(&mut self, block: u64);
}

impl ConcentratedLiquidityPoolMut for V3PoolState {
    fn replace_tick_data(
        &mut self,
        tick_data: HashMap<i32, TickInfo>,
        tick_data_block: u64,
        tick_spacing: i32,
    ) -> bool {
        replace_tick_data(self, tick_data, tick_data_block, tick_spacing);
        true
    }

    fn merge_tick_word(&mut self, fetched: &FetchedTickWord) -> bool {
        merge_tick_word(self, fetched);
        true
    }

    fn mark_bitmap_words_known(&mut self, words: &[i32]) {
        mark_bitmap_words_known(self, words);
    }

    fn apply_swap(
        &mut self,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)],
    ) {
        apply_swap(
            self,
            sqrt_price_x96,
            liquidity,
            tick,
            block_number,
            tick_priors,
        );
    }

    fn apply_liquidity_update(
        &mut self,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) {
        apply_liquidity_update(self, tick_lower, tick_upper, liquidity_delta, block_number);
    }

    fn seed_genesis(&mut self, block: u64) {
        state_seeding::seed_genesis(self, block);
    }
}

impl ConcentratedLiquidityPoolMut for V4PoolState {
    fn replace_tick_data(
        &mut self,
        tick_data: HashMap<i32, TickInfo>,
        tick_data_block: u64,
        tick_spacing: i32,
    ) -> bool {
        replace_tick_data(self, tick_data, tick_data_block, tick_spacing);
        true
    }

    fn merge_tick_word(&mut self, fetched: &FetchedTickWord) -> bool {
        merge_tick_word(self, fetched);
        true
    }

    fn mark_bitmap_words_known(&mut self, words: &[i32]) {
        mark_bitmap_words_known(self, words);
    }

    fn apply_swap(
        &mut self,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block_number: u64,
        tick_priors: &[(i32, TickInfo)],
    ) {
        apply_swap(
            self,
            sqrt_price_x96,
            liquidity,
            tick,
            block_number,
            tick_priors,
        );
    }

    fn apply_liquidity_update(
        &mut self,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        block_number: u64,
    ) {
        apply_liquidity_update(self, tick_lower, tick_upper, liquidity_delta, block_number);
    }

    fn seed_genesis(&mut self, block: u64) {
        state_seeding::seed_genesis(self, block);
    }
}

/// The merged mutation-verb test suite (ADR-080 D4) — one battery over both
/// designs, the merge-bumps-nonce contract included.
#[cfg(test)]
mod tests;
