//! The merged journal-verb test suite (ADR-080 D4). These are the former
//! V3/V4 twin batteries (`apply_inherent_tests` + `exact_rewind_tests` in
//! `v3_state.rs` / `v4_state.rs`) converging at the shared interface: each
//! behavior is written ONCE against the [`ClStateFixture`] seam and run on
//! BOTH adapters. Only the panic probes get per-adapter wrappers (both
//! adapters must be exercised; `#[should_panic]` stops at the first).

#![expect(clippy::unwrap_used, clippy::expect_used)]

use alloy::primitives::{U128, U256};

use crate::families::concentrated_liquidity::state_fixture::{
    apply_script, assert_same_state, probe_rewind, replay_before, script_from_kinds,
    ClStateFixture, Ev, LIQ, REG,
};
use crate::families::concentrated_liquidity::state_journal::JournalHost;
use crate::families::concentrated_liquidity::state_seeding::SeedingHost;
use crate::families::concentrated_liquidity::state_walk::{
    get_cached_tick_ranges, CachedTickRanges, WalkHost,
};
use crate::state::state_history::{JournalError, ReorgPoolState, V3BlockDelta};
use crate::state::v3_state::V3PoolState;
use crate::state::v4_state::V4PoolState;
use crate::TickInfo;
use proptest::prelude::*;

// ------------------------------------------------------------------
// apply_liquidity_update — the journal-capture contract shared by the
// two adapters' trait impls (`ConcentratedLiquidityPoolMut` in
// `registry.rs`): tick-map mutation per Solidity `Tick.update`, the
// in-range scalar adjust, the two-stamp clock rule, and the delta's
// scalar_priors shape.
// ------------------------------------------------------------------

fn liquidity_update_out_of_range_is_tick_only_and_journals_no_scalars<F: ClStateFixture>(
    tag: &str,
) {
    // What: an OUT-OF-RANGE Mint/Burn (the applied [lower, upper) region
    // does NOT straddle the current tick) must (1) apply the delta to BOTH
    // boundary ticks per Solidity Tick.update (gross += at both, net += at
    // lower, net -= at upper), (2) advance only the LIQUIDITY clock, (3)
    // push a V3BlockDelta with scalar_priors: None (no active-scalar
    // change), (4) journal the two tick priors captured BEFORE mutation,
    // (5) NOT change sqrt_price / liquidity / tick.
    // Why: only IN-RANGE events adjust the active `liquidity` scalar;
    // out-of-range events are tick-only (ADR-004 — restore skips the
    // scalar write-back for these deltas).
    let liq = 1_000_000u128;
    let mut state = F::with_position(liq);
    // Move the current tick outside [-60, 60) so the applied range is
    // out-of-range and cannot touch the active scalar.
    state.set_tick(500);

    let sp_before = WalkHost::sqrt_price_x96(&state);
    let liq_before = WalkHost::liquidity(&state);
    let tick_before = WalkHost::tick(&state);

    // Capture the pre-mutation tick info at -60 / +60 so we can assert the
    // journal captured these "before" values.
    let prior_lower = SeedingHost::tick_data(&state).get(&-60).cloned().unwrap();
    let prior_upper = SeedingHost::tick_data(&state).get(&60).cloned().unwrap();

    let delta = 123_456i128;
    let before_len = JournalHost::journal(&state).len();

    state.apply_liquidity_update(-60, 60, delta, 9);

    // (1) tick_data mutated per Tick.update.
    let after_lower = SeedingHost::tick_data(&state).get(&-60).unwrap();
    let after_upper = SeedingHost::tick_data(&state).get(&60).unwrap();
    // gross += at both boundaries.
    assert_eq!(
        after_lower.liquidity_gross,
        prior_lower.liquidity_gross + U256::from(u128::try_from(delta).unwrap()).to::<U128>(),
        "{tag}: lower gross incremented"
    );
    assert_eq!(
        after_upper.liquidity_gross,
        prior_upper.liquidity_gross + U256::from(u128::try_from(delta).unwrap()).to::<U128>(),
        "{tag}: upper gross incremented"
    );
    // net += at lower, net -= at upper.
    assert_eq!(
        after_lower.liquidity_net,
        prior_lower.liquidity_net + delta,
        "{tag}: lower net"
    );
    assert_eq!(
        after_upper.liquidity_net,
        prior_upper.liquidity_net - delta,
        "{tag}: upper net"
    );
    // The mutating helper advances the tick's `block` field too.
    assert_eq!(after_lower.block, 9, "{tag}: lower row stamp");
    assert_eq!(after_upper.block, 9, "{tag}: upper row stamp");
    // (2) Two-stamp rule: out-of-range → only the LIQUIDITY clock advances;
    // the price clock does NOT move (slot0 head byte-identical).
    assert_eq!(
        WalkHost::tick_data_block(&state),
        9,
        "{tag}: liquidity clock advances"
    );
    assert_eq!(
        WalkHost::update_block(&state),
        0,
        "{tag}: price clock untouched"
    );
    // (5) slot0 scalars UNCHANGED (out-of-range → no active adjust).
    assert_eq!(
        WalkHost::sqrt_price_x96(&state),
        sp_before,
        "{tag}: sqrt unchanged"
    );
    assert_eq!(
        WalkHost::liquidity(&state),
        liq_before,
        "{tag}: liquidity unchanged"
    );
    assert_eq!(WalkHost::tick(&state), tick_before, "{tag}: tick unchanged");
    // (3)/(4) journal gained exactly one delta at block 9.
    assert_eq!(
        JournalHost::journal(&state).len(),
        before_len + 1,
        "{tag}: one delta pushed"
    );
    assert_eq!(
        JournalHost::journal(&state).newest_block(),
        Some(9),
        "{tag}: delta stamped at the event block"
    );
}

#[test]
fn liquidity_update_out_of_range_is_tick_only_and_journals_no_scalars_on_both_adapters() {
    liquidity_update_out_of_range_is_tick_only_and_journals_no_scalars::<V3PoolState>("V3");
    liquidity_update_out_of_range_is_tick_only_and_journals_no_scalars::<V4PoolState>("V4");
}

fn liquidity_update_in_range_mint_adjusts_active_liquidity_and_restores<F: ClStateFixture>(
    tag: &str,
) {
    // What: an IN-RANGE Mint (tick_lower <= current tick < tick_upper)
    // adds the delta to the ACTIVE `liquidity` scalar on top of the
    // boundary-tick map mutation (parity with the on-chain `liquidity`
    // field and the pure reference
    // `degenbot-concentrated-liquidity-math::apply_liquidity_mapping_update`).
    // The journal delta carries `scalar_priors: Some(..)` so a reorg
    // restore rolls the scalar back to its pre-event value.
    // Why: without the adjust the solver's active-liquidity scalar drifts
    // from on-chain even though sqrt_price/tick stay identical — an
    // in-range Mint leaves it too low, an in-range Burn too high.
    let liq = 1_000_000u128;
    let mut state = F::with_position(liq); // current tick 0 ∈ [-60, 60)
    let pre_liq = WalkHost::liquidity(&state);
    let pre_sqrt = WalkHost::sqrt_price_x96(&state);
    let delta = 123_456i128;

    state.apply_liquidity_update(-60, 60, delta, 9);

    assert_eq!(
        WalkHost::liquidity(&state),
        pre_liq + 123_456,
        "{tag}: in-range mint adds to active liquidity"
    );
    // A liquidity event does not move the price head.
    assert_eq!(
        WalkHost::sqrt_price_x96(&state),
        pre_sqrt,
        "{tag}: price head untouched"
    );
    assert_eq!(WalkHost::tick(&state), 0, "{tag}: tick unchanged");
    assert_eq!(
        WalkHost::update_block(&state),
        9,
        "{tag}: price clock advanced"
    );

    // Reorg restore rolls the in-range scalar adjust back.
    let res: Result<(), JournalError> = state.restore_before_block(9);
    assert!(res.is_ok(), "{tag}: restore is legal");
    assert_eq!(
        WalkHost::liquidity(&state),
        pre_liq,
        "{tag}: restore undoes the in-range scalar adjust"
    );
    assert_eq!(
        WalkHost::sqrt_price_x96(&state),
        pre_sqrt,
        "{tag}: price head restored"
    );
    assert_eq!(WalkHost::tick(&state), 0, "{tag}: tick restored");
}

#[test]
fn liquidity_update_in_range_mint_adjusts_active_liquidity_and_restores_on_both_adapters() {
    liquidity_update_in_range_mint_adjusts_active_liquidity_and_restores::<V3PoolState>("V3");
    liquidity_update_in_range_mint_adjusts_active_liquidity_and_restores::<V4PoolState>("V4");
}

fn liquidity_update_in_range_burn_reduces_active_liquidity<F: ClStateFixture>(tag: &str) {
    // What: an IN-RANGE Burn (negative delta) removes the magnitude from
    // the ACTIVE `liquidity` scalar — the exact case that historically
    // left the solver's scalar too high on mainnet (desync manifesting as
    // an on-chain-vs-solver liquidity mismatch with byte-identical
    // sqrt_price/tick).
    let liq = 1_000_000u128;
    let mut state = F::with_position(liq); // current tick 0 ∈ [-60, 60)
    let pre_liq = WalkHost::liquidity(&state);

    state.apply_liquidity_update(-60, 60, -250_000i128, 9);

    assert_eq!(
        WalkHost::liquidity(&state),
        pre_liq - 250_000,
        "{tag}: in-range burn removes active liquidity"
    );
}

#[test]
fn liquidity_update_in_range_burn_reduces_active_liquidity_on_both_adapters() {
    liquidity_update_in_range_burn_reduces_active_liquidity::<V3PoolState>("V3");
    liquidity_update_in_range_burn_reduces_active_liquidity::<V4PoolState>("V4");
}

fn liquidity_update_replay_at_or_before_seed_block_does_not_adjust_scalar<F: ClStateFixture>(
    tag: &str,
) {
    // (historical-replay guard, Python `_initial_state_block` twin):
    // a pool seeded against head already reflects in its `liquidity` scalar
    // every on-chain in-range Mint/Burn at or before the seed block.
    // Replaying one such event after seed (e.g. a backfilled Burn applied
    // after registration) must NOT adjust the active-liquidity scalar — it
    // would subtract a removal the seed already contains, leaving the
    // solver exactly one in-range position-net BELOW on-chain with
    // identical sqrt/tick (the observed mainnet desync).
    let liq = 1_000_000u128;
    // Seed against head at block 100: scalar already reflects events <= 100.
    let mut state = F::with_position(liq); // tick 0 ∈ [-60, 60)
    state.set_initial_state_block(100);
    let pre_liq = WalkHost::liquidity(&state);

    // Historical replay of the in-range Burn at block 50 (<= seed 100).
    state.apply_liquidity_update(-60, 60, -250_000i128, 50);

    assert_eq!(
        WalkHost::liquidity(&state),
        pre_liq,
        "{tag}: replay at or before the seed block must NOT re-adjust the active liquidity (already in the seed)"
    );
    // Two-stamp rule: the replay mutates the TICK MAP (liquidity clock
    // advances to 50) but does NOT advance the PRICE clock (update_block
    // stays 0 — the replay leaves the slot0 head byte-identical).
    assert_eq!(
        WalkHost::tick_data_block(&state),
        50,
        "{tag}: the tick mutation advances the liquidity clock (tick map is replayed)"
    );
    assert_eq!(
        WalkHost::update_block(&state),
        0,
        "{tag}: an at-or-before-seed replay must NOT advance the price clock (slot0 head untouched)"
    );
}

#[test]
fn liquidity_update_replay_at_or_before_seed_block_does_not_adjust_scalar_on_both_adapters() {
    liquidity_update_replay_at_or_before_seed_block_does_not_adjust_scalar::<V3PoolState>("V3");
    liquidity_update_replay_at_or_before_seed_block_does_not_adjust_scalar::<V4PoolState>("V4");
}

fn liquidity_update_post_seed_in_range_adjusts_scalar<F: ClStateFixture>(tag: &str) {
    // A genuinely post-seed in-range event (block > seed block) DOES adjust
    // the active-liquidity scalar — the historical guard must not swallow
    // real forward liquidity events.
    let liq = 1_000_000u128;
    let mut state = F::with_position(liq);
    state.set_initial_state_block(100);
    let pre_liq = WalkHost::liquidity(&state);

    state.apply_liquidity_update(-60, 60, -250_000i128, 150);

    assert_eq!(
        WalkHost::liquidity(&state),
        pre_liq - 250_000,
        "{tag}: post-seed (block > initial_state_block) in-range burn adjusts the active liquidity"
    );
}

#[test]
fn liquidity_update_post_seed_in_range_adjusts_scalar_on_both_adapters() {
    liquidity_update_post_seed_in_range_adjusts_scalar::<V3PoolState>("V3");
    liquidity_update_post_seed_in_range_adjusts_scalar::<V4PoolState>("V4");
}

// ------------------------------------------------------------------
// ReorgPoolState trait (ADR-016 D4 — concentrated liquidity family adopts the trait).
// `restore_before_block` returns `()`; the landed-at state lives in the
// struct's own fields (read-after-restore). The shared verb in this
// module carries the whole restore contract; these probes run it on both
// adapters.
// ------------------------------------------------------------------

fn restore_writes_landed_at_scalars_and_restore_point_block<F: ClStateFixture>(tag: &str) {
    // apply_swap captures scalar priors; restore_before_block pops the
    // delta and writes the pre-swap scalars back. Two-stamp rule:
    // restore rewinds BOTH clocks to their exact pre-swap values (the
    // delta's `update_block_before`/`tick_data_block_before`), NOT to the
    // restore-point block.
    let liq = 1_000_000u128;
    let mut state = F::with_position(liq);
    let pre_sqrt = WalkHost::sqrt_price_x96(&state);
    let pre_liq = WalkHost::liquidity(&state);
    let pre_tick = WalkHost::tick(&state);

    let new_sqrt = U256::from(2u128) << 96;
    state.apply_swap(new_sqrt, liq + 1, 1, 7, &[]);
    assert_eq!(
        WalkHost::update_block(&state),
        7,
        "{tag}: price clock advanced"
    );
    assert_eq!(
        WalkHost::tick_data_block(&state),
        7,
        "{tag}: liquidity clock advanced"
    );

    let result: Result<(), JournalError> = state.restore_before_block(7);
    assert!(result.is_ok(), "{tag}: restore is legal");
    assert_eq!(
        WalkHost::sqrt_price_x96(&state),
        pre_sqrt,
        "{tag}: sqrt restored"
    );
    assert_eq!(
        WalkHost::liquidity(&state),
        pre_liq,
        "{tag}: liquidity restored"
    );
    assert_eq!(WalkHost::tick(&state), pre_tick, "{tag}: tick restored");
    assert_eq!(
        WalkHost::update_block(&state),
        0,
        "{tag}: the price clock rewinds to its exact pre-swap value (both clocks start at 0)"
    );
    assert_eq!(
        WalkHost::tick_data_block(&state),
        0,
        "{tag}: the liquidity clock rewinds to 0"
    );
    assert_eq!(state.journal_len(), 0, "{tag}: swap delta popped");
}

#[test]
fn restore_writes_landed_at_scalars_and_restore_point_block_on_both_adapters() {
    restore_writes_landed_at_scalars_and_restore_point_block::<V3PoolState>("V3");
    restore_writes_landed_at_scalars_and_restore_point_block::<V4PoolState>("V4");
}

fn restore_no_op_when_newest_delta_before_target<F: ClStateFixture>(tag: &str) {
    let liq = 1_000_000u128;
    let mut state = F::with_position(liq);
    let new_sqrt = U256::from(2u128) << 96;
    state.apply_swap(new_sqrt, liq + 1, 1, 7, &[]);
    let post_sqrt = WalkHost::sqrt_price_x96(&state);

    let result: Result<(), JournalError> = state.restore_before_block(8);
    assert!(result.is_ok(), "{tag}: restore is legal");
    assert_eq!(
        WalkHost::sqrt_price_x96(&state),
        post_sqrt,
        "{tag}: no rollback when newest delta is before the target"
    );
    assert_eq!(state.journal_len(), 1, "{tag}: delta not popped");
}

#[test]
fn restore_no_op_when_newest_delta_before_target_on_both_adapters() {
    restore_no_op_when_newest_delta_before_target::<V3PoolState>("V3");
    restore_no_op_when_newest_delta_before_target::<V4PoolState>("V4");
}

fn restore_removes_newly_initialized_tick_on_rollback<F: ClStateFixture>(tag: &str) {
    // A swap that seeds a brand-new tick (no prior entry) records
    // `liquidity_gross_before: None`; restore must remove that tick.
    let liq = 1_000_000u128;
    let mut state = F::with_position(liq);
    let new_tick = TickInfo {
        liquidity_gross: U256::from(500u64).to::<U128>(),
        liquidity_net: 500i128,
        block: 7,
    };
    let new_sqrt = U256::from(2u128) << 96;
    state.apply_swap(new_sqrt, liq + 1, 1, 7, &[(100, new_tick)]);
    assert!(
        SeedingHost::tick_data(&state).contains_key(&100),
        "{tag}: new tick seeded by swap"
    );

    let result: Result<(), JournalError> = state.restore_before_block(7);
    assert!(result.is_ok(), "{tag}: restore is legal");
    assert!(
        !SeedingHost::tick_data(&state).contains_key(&100),
        "{tag}: newly-initialized tick removed on rollback"
    );
}

#[test]
fn restore_removes_newly_initialized_tick_on_rollback_on_both_adapters() {
    restore_removes_newly_initialized_tick_on_rollback::<V3PoolState>("V3");
    restore_removes_newly_initialized_tick_on_rollback::<V4PoolState>("V4");
}

fn restore_stamps_restored_ticks_with_the_liquidity_clock<F: ClStateFixture>(tag: &str) {
    // Two liquidity events (blocks 5 then 9); restoring before block 9 pops
    // only the block-9 delta, so the restored map is exact at the block-5
    // clock. The restored rows must carry that block — not the rolled-back
    // event's block (9) and not a hardcoded 0.
    let liq = 1_000_000u128;
    let mut state = F::with_position(liq);
    state.apply_liquidity_update(-60, 60, 100, 5);
    state.apply_liquidity_update(-60, 60, 200, 9);
    assert_eq!(
        WalkHost::tick_data_block(&state),
        9,
        "{tag}: liquidity clock at 9"
    );

    let result: Result<(), JournalError> = state.restore_before_block(9);
    assert!(result.is_ok(), "{tag}: restore is legal");
    assert_eq!(
        WalkHost::tick_data_block(&state),
        5,
        "{tag}: the liquidity clock rewinds to its pre-target value"
    );
    assert_eq!(
        SeedingHost::tick_data(&state)[&-60].block,
        5,
        "{tag}: restored tick carries the restored liquidity clock, not a hardcoded 0"
    );
    assert_eq!(
        SeedingHost::tick_data(&state)[&60].block,
        5,
        "{tag}: upper row stamp"
    );
}

#[test]
fn restore_stamps_restored_ticks_with_the_liquidity_clock_on_both_adapters() {
    restore_stamps_restored_ticks_with_the_liquidity_clock::<V3PoolState>("V3");
    restore_stamps_restored_ticks_with_the_liquidity_clock::<V4PoolState>("V4");
}

fn discard_drops_old_deltas_without_mutating_live_state<F: ClStateFixture>(tag: &str) {
    let liq = 1_000_000u128;
    let mut state = F::with_position(liq);
    // Genesis anchor (tick-only, block 0) so discard has an old delta to
    // drop without emptying the journal entirely.
    JournalHost::journal_mut(&mut state).push_delta(V3BlockDelta {
        block: 0,
        scalar_priors: None,
        update_block_before: None,
        tick_data_block_before: None,
        tick_priors: Vec::new(),
    });
    let new_sqrt = U256::from(2u128) << 96;
    state.apply_swap(new_sqrt, liq + 1, 1, 7, &[]);
    assert_eq!(state.journal_len(), 2, "{tag}: anchor + swap delta");
    let post_sqrt = WalkHost::sqrt_price_x96(&state);

    let result: Result<(), JournalError> = state.discard_before_block(7);
    assert!(result.is_ok(), "{tag}: discard is legal");
    assert_eq!(state.journal_len(), 1, "{tag}: genesis dropped, swap kept");
    assert_eq!(
        WalkHost::sqrt_price_x96(&state),
        post_sqrt,
        "{tag}: discard trims history only — live state untouched"
    );
}

#[test]
fn discard_drops_old_deltas_without_mutating_live_state_on_both_adapters() {
    discard_drops_old_deltas_without_mutating_live_state::<V3PoolState>("V3");
    discard_drops_old_deltas_without_mutating_live_state::<V4PoolState>("V4");
}

#[test]
fn journal_len_reports_delta_count_on_both_adapters() {
    fn journal_len_reports_delta_count<F: ClStateFixture>(tag: &str) {
        let liq = 1_000_000u128;
        let mut state = F::with_position(liq);
        assert_eq!(state.journal_len(), 0, "{tag}: fresh journal");
        state.apply_swap(U256::from(2u128) << 96, liq + 1, 1, 7, &[]);
        assert_eq!(state.journal_len(), 1, "{tag}: one delta after the swap");
    }
    journal_len_reports_delta_count::<V3PoolState>("V3");
    journal_len_reports_delta_count::<V4PoolState>("V4");
}

// ------------------------------------------------------------------
// Exact rewind — the product: `restore_before_block(target)` leaves the
// state bit-identical to a fresh replay of every event strictly before
// `target` (tick rows INCLUDING their `block` stamps, the scalars, and
// both clocks). The battery below pins each way a rewind can go wrong.
// ------------------------------------------------------------------

#[test]
fn multi_block_popped_range_rewinds_to_the_pre_target_state_on_both_adapters() {
    fn multi_block_popped_range_rewinds_to_the_pre_target_state<F: ClStateFixture>(tag: &str) {
        // (a) A popped range spanning several blocks (events at 97..100,
        // rewind to 98) lands exactly on the block-97 state.
        let script = vec![
            (
                97,
                Ev::Liq {
                    lower: -60,
                    upper: 60,
                    delta: 7_000,
                },
            ),
            (
                98,
                Ev::Swap {
                    sqrt: 1u128 << 96,
                    liq: LIQ + 5,
                    tick: 0,
                    new_tick: None,
                },
            ),
            (
                99,
                Ev::Liq {
                    lower: -60,
                    upper: 60,
                    delta: 9_000,
                },
            ),
            (
                100,
                Ev::Swap {
                    sqrt: 1u128 << 96,
                    liq: LIQ + 7,
                    tick: 0,
                    new_tick: None,
                },
            ),
        ];
        let mut state = F::seeded_state();
        apply_script(&mut state, &script);
        state.restore_before_block(98).expect("legal");
        assert_same_state(
            "multi-block",
            &state,
            &replay_before(&script, 98),
            "target=98",
        );
        assert_eq!(
            SeedingHost::tick_data(&state)[&-60].block,
            97,
            "{tag}: the restored row carries its own last-mutation stamp"
        );
        assert_eq!(
            SeedingHost::tick_data(&state)[&60].block,
            97,
            "{tag}: upper row stamp"
        );
        assert_eq!(
            WalkHost::tick_data_block(&state),
            97,
            "{tag}: liquidity clock"
        );
        assert_eq!(WalkHost::update_block(&state), 97, "{tag}: price clock");
    }
    multi_block_popped_range_rewinds_to_the_pre_target_state::<V3PoolState>("V3");
    multi_block_popped_range_rewinds_to_the_pre_target_state::<V4PoolState>("V4");
}

#[test]
fn tick_created_inside_the_popped_range_is_removed_on_rollback_on_both_adapters() {
    fn tick_created_inside_the_popped_range_is_removed_on_rollback<F: ClStateFixture>(tag: &str) {
        // (b) A tick created inside the popped range records a `None` prior
        // and is removed on rollback.
        let script = vec![(
            97,
            Ev::Swap {
                sqrt: 1u128 << 96,
                liq: LIQ + 1,
                tick: 60,
                new_tick: Some((120, 500)),
            },
        )];
        let mut state = F::seeded_state();
        apply_script(&mut state, &script);
        assert!(
            SeedingHost::tick_data(&state).contains_key(&120),
            "{tag}: tick seeded by the swap"
        );
        state.restore_before_block(97).expect("legal");
        assert!(
            !SeedingHost::tick_data(&state).contains_key(&120),
            "{tag}: a tick created inside the popped range has no prior row - it is removed"
        );
        assert_same_state(
            "created-tick",
            &state,
            &replay_before(&script, 97),
            "target=97",
        );
    }
    tick_created_inside_the_popped_range_is_removed_on_rollback::<V3PoolState>("V3");
    tick_created_inside_the_popped_range_is_removed_on_rollback::<V4PoolState>("V4");
}

#[test]
fn tick_touched_in_multiple_popped_blocks_restores_the_exact_pre_range_row_on_both_adapters() {
    fn tick_touched_in_multiple_popped_blocks_restores_the_exact_pre_range_row<
        F: ClStateFixture,
    >(
        tag: &str,
    ) {
        // (c) A tick touched in several popped blocks comes back as its
        // exact pre-range row (values AND stamp), not as the mid-range row
        // the later delta saw.
        let script = vec![
            (
                97,
                Ev::Liq {
                    lower: -60,
                    upper: 60,
                    delta: 7_000,
                },
            ),
            (
                98,
                Ev::Liq {
                    lower: -60,
                    upper: 60,
                    delta: 9_000,
                },
            ),
        ];
        let mut state = F::seeded_state();
        apply_script(&mut state, &script);
        state.restore_before_block(97).expect("legal");
        assert_eq!(
            SeedingHost::tick_data(&state)[&-60],
            TickInfo {
                liquidity_gross: U256::from(LIQ).to::<U128>(),
                liquidity_net: i128::try_from(LIQ).unwrap_or(0),
                block: 0,
            },
            "{tag}: the oldest popped prior wins: the seed row, seed stamp included"
        );
        assert_same_state(
            "same-tick-twice",
            &state,
            &replay_before(&script, 97),
            "target=97",
        );
    }
    tick_touched_in_multiple_popped_blocks_restores_the_exact_pre_range_row::<V3PoolState>("V3");
    tick_touched_in_multiple_popped_blocks_restores_the_exact_pre_range_row::<V4PoolState>("V4");
}

#[test]
fn tick_only_popped_range_skips_the_scalar_write_back_and_keeps_the_price_clock_on_both() {
    fn tick_only_popped_range_skips_the_scalar_write_back_and_keeps_the_price_clock<
        F: ClStateFixture,
    >(
        tag: &str,
    ) {
        // (d)+(e) A popped range holding only an out-of-range Mint/Burn is
        // tick-only: the scalar write-back is skipped (ADR-004) and the
        // price clock's prior is `None` (it never moved), so only the
        // liquidity clock rewinds.
        let script = vec![
            (
                97,
                Ev::Swap {
                    sqrt: (1u128 << 96) + 3,
                    liq: LIQ + 11,
                    tick: 500,
                    new_tick: None,
                },
            ),
            (
                98,
                Ev::Liq {
                    lower: 120,
                    upper: 240,
                    delta: 3_000,
                },
            ),
        ];
        let mut state = F::seeded_state();
        apply_script(&mut state, &script);
        assert_eq!(
            WalkHost::update_block(&state),
            97,
            "{tag}: price clock at 97"
        );
        state.restore_before_block(98).expect("legal");
        assert_same_state(
            "tick-only",
            &state,
            &replay_before(&script, 98),
            "target=98",
        );
        assert!(
            !SeedingHost::tick_data(&state).contains_key(&120),
            "{tag}: the tick created by the popped event is removed"
        );
        assert_eq!(
            WalkHost::sqrt_price_x96(&state),
            U256::from((1u128 << 96) + 3),
            "{tag}: tick-only rollback leaves the slot0 head untouched"
        );
        assert_eq!(
            WalkHost::liquidity(&state),
            LIQ + 11,
            "{tag}: active liquidity kept"
        );
        assert_eq!(WalkHost::tick(&state), 500, "{tag}: tick kept");
        assert_eq!(
            WalkHost::update_block(&state),
            97,
            "{tag}: the price clock's prior is None - it keeps its post-97 value"
        );
        assert_eq!(
            WalkHost::tick_data_block(&state),
            97,
            "{tag}: the liquidity clock rewinds to its pre-target value"
        );
    }
    tick_only_popped_range_skips_the_scalar_write_back_and_keeps_the_price_clock::<V3PoolState>(
        "V3",
    );
    tick_only_popped_range_skips_the_scalar_write_back_and_keeps_the_price_clock::<V4PoolState>(
        "V4",
    );
}

#[test]
fn rollback_then_new_events_apply_cleanly_and_move_stamps_forward_on_both_adapters() {
    fn rollback_then_new_events_apply_cleanly_and_move_stamps_forward<F: ClStateFixture>(
        tag: &str,
    ) {
        // (f) After a rollback the chain keeps moving: new events at 99/100
        // apply cleanly and move the stamps forward again.
        let original = vec![
            (
                97,
                Ev::Liq {
                    lower: -60,
                    upper: 60,
                    delta: 7_000,
                },
            ),
            (
                98,
                Ev::Liq {
                    lower: -60,
                    upper: 60,
                    delta: 9_000,
                },
            ),
        ];
        let mut state = F::seeded_state();
        apply_script(&mut state, &original);
        state.restore_before_block(98).expect("legal");

        let replacement = vec![
            (
                99,
                Ev::Liq {
                    lower: -60,
                    upper: 60,
                    delta: 4_000,
                },
            ),
            (
                100,
                Ev::Swap {
                    sqrt: (1u128 << 96) + 9,
                    liq: LIQ + 3,
                    tick: 60,
                    new_tick: Some((120, 700)),
                },
            ),
        ];
        apply_script(&mut state, &replacement);
        assert_eq!(
            SeedingHost::tick_data(&state)[&-60].block,
            99,
            "{tag}: lower restamped"
        );
        assert_eq!(
            SeedingHost::tick_data(&state)[&120].block,
            100,
            "{tag}: new tick stamped"
        );
        assert_eq!(
            WalkHost::tick_data_block(&state),
            100,
            "{tag}: liquidity clock"
        );
        assert_eq!(WalkHost::update_block(&state), 100, "{tag}: price clock");

        let mut oracle = replay_before(&original, 98);
        apply_script(&mut oracle, &replacement);
        assert_same_state("rollback-then-apply", &state, &oracle, "98 then 99/100");
    }
    rollback_then_new_events_apply_cleanly_and_move_stamps_forward::<V3PoolState>("V3");
    rollback_then_new_events_apply_cleanly_and_move_stamps_forward::<V4PoolState>("V4");
}

#[test]
fn double_rollback_equals_a_single_rollback_to_the_earlier_target_on_both_adapters() {
    fn double_rollback_equals_a_single_rollback_to_the_earlier_target<F: ClStateFixture>(
        tag: &str,
    ) {
        // (g) Rewinding twice (99 then 97) equals a single rewind to the
        // earlier target (97).
        let script = vec![
            (
                97,
                Ev::Liq {
                    lower: -60,
                    upper: 60,
                    delta: 7_000,
                },
            ),
            (
                98,
                Ev::Swap {
                    sqrt: (1u128 << 96) + 3,
                    liq: LIQ + 11,
                    tick: 500,
                    new_tick: Some((120, 500)),
                },
            ),
            (
                99,
                Ev::Liq {
                    lower: 120,
                    upper: 240,
                    delta: 3_000,
                },
            ),
        ];
        let mut twice = F::seeded_state();
        apply_script(&mut twice, &script);
        twice.restore_before_block(99).expect("legal");
        twice.restore_before_block(97).expect("legal");

        let mut once = F::seeded_state();
        apply_script(&mut once, &script);
        once.restore_before_block(97).expect("legal");

        assert_same_state(
            &format!("{tag}: double-rollback"),
            &twice,
            &once,
            "99 then 97 vs 97",
        );
        assert_same_state(
            &format!("{tag}: double-rollback-oracle"),
            &once,
            &replay_before(&script, 97),
            "target=97",
        );
    }
    double_rollback_equals_a_single_rollback_to_the_earlier_target::<V3PoolState>("V3");
    double_rollback_equals_a_single_rollback_to_the_earlier_target::<V4PoolState>("V4");
}

/// (h) An empty journal is the registration boundary: the coordinator's
/// `has_state_prior_to` predicate is false there and the rewind maps to
/// `NoStatePriorToBlock` (the pool-level surface is the journal's
/// empty-journal refusal). Per-adapter `#[should_panic]` probes — a shared
/// test body would stop at the first adapter's panic.
#[test]
#[should_panic(expected = "No pool state known prior to block")]
fn rewind_below_the_registration_boundary_is_a_hard_error_v3() {
    let mut state = V3PoolState::pool_state();
    assert!(
        JournalHost::journal(&state).is_empty(),
        "no anchor - nothing to rewind to"
    );
    state
        .restore_before_block(REG)
        .expect("unreachable: refuses");
}

#[test]
#[should_panic(expected = "No pool state known prior to block")]
fn rewind_below_the_registration_boundary_is_a_hard_error_v4() {
    let mut state = V4PoolState::pool_state();
    assert!(
        JournalHost::journal(&state).is_empty(),
        "no anchor - nothing to rewind to"
    );
    state
        .restore_before_block(REG)
        .expect("unreachable: refuses");
}

#[test]
fn rewind_at_registration_and_one_past_it_are_legal_on_both_adapters() {
    fn rewind_at_registration_and_one_past_it_are_legal<F: ClStateFixture>(tag: &str) {
        // (h) The legal half: with the registration anchor in place, a
        // rewind to the first event's block lands on the registration state
        // (stamps included), and a rewind AT the boundary pops the anchor
        // and lands there too.
        let script = vec![(
            REG + 1,
            Ev::Swap {
                sqrt: (1u128 << 96) + 1,
                liq: LIQ + 1,
                tick: 60,
                new_tick: Some((120, 500)),
            },
        )];
        let mut boundary = F::seeded_state();
        apply_script(&mut boundary, &script);
        boundary.restore_before_block(REG + 1).expect("legal");
        assert_same_state(
            "registration+1",
            &boundary,
            &replay_before(&script, REG + 1),
            "target=REG+1",
        );
        assert_eq!(
            SeedingHost::tick_data(&boundary)[&-60].block,
            0,
            "{tag}: registration rows keep the seed stamp"
        );
        assert_eq!(WalkHost::update_block(&boundary), 0, "{tag}: price clock");
        assert_eq!(
            WalkHost::tick_data_block(&boundary),
            0,
            "{tag}: liquidity clock"
        );
        assert_eq!(
            boundary.journal_len(),
            1,
            "{tag}: the anchor survives; only the event delta was popped"
        );

        boundary
            .restore_before_block(REG)
            .expect("legal at the boundary");
        assert_eq!(boundary.journal_len(), 0, "{tag}: the anchor is popped");
        assert_eq!(WalkHost::update_block(&boundary), 0, "{tag}: price clock");
        assert_eq!(
            WalkHost::tick_data_block(&boundary),
            0,
            "{tag}: liquidity clock"
        );
    }
    rewind_at_registration_and_one_past_it_are_legal::<V3PoolState>("V3");
    rewind_at_registration_and_one_past_it_are_legal::<V4PoolState>("V4");
}

#[test]
fn rollback_bumps_nonce_and_invalidates_the_tick_range_cache_on_both_adapters() {
    fn rollback_bumps_nonce_and_invalidates_the_tick_range_cache<F: ClStateFixture>(tag: &str) {
        // (j) A rollback still bumps `state_nonce` exactly once and
        // invalidates both directions of the tick-range cache.
        let mut state = F::seeded_state();
        state.apply_liquidity_update(-60, 60, 7_000, 97);
        let _ = get_cached_tick_ranges(&state, 60, true);
        assert!(
            matches!(state.cached_slot(true), CachedTickRanges::Hit(_)),
            "{tag}: cache warm before the rollback"
        );
        let nonce_before = state.state_nonce();
        state.restore_before_block(97).expect("legal");
        assert_eq!(
            state.state_nonce(),
            nonce_before.wrapping_add(1),
            "{tag}: exactly one nonce bump per rollback"
        );
        assert!(
            matches!(state.cached_slot(true), CachedTickRanges::StillEmpty),
            "{tag}: zfo cache invalidated"
        );
        assert!(
            matches!(state.cached_slot(false), CachedTickRanges::StillEmpty),
            "{tag}: ofz cache invalidated"
        );
    }
    rollback_bumps_nonce_and_invalidates_the_tick_range_cache::<V3PoolState>("V3");
    rollback_bumps_nonce_and_invalidates_the_tick_range_cache::<V4PoolState>("V4");
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    /// The product: for EVERY legal rewind target, the rolled-back state is
    /// bit-identical to a fresh replay of the events strictly before it —
    /// on BOTH adapters.
    #[test]
    fn rewind_equals_replay_on_both_adapters(kinds in prop::collection::vec(0u8..6, 1..6)) {
        fn rewind_equals_replay<F: ClStateFixture>(kinds: &[u8]) {
            let script = script_from_kinds(kinds);
            let newest = script.last().map_or(REG, |(block, _)| *block);
            let mut targets: Vec<u64> = vec![REG, REG + 1];
            for (block, _) in &script {
                targets.push(*block);
                targets.push(*block + 1);
            }
            targets.push(newest + 1);
            targets.sort_unstable();
            targets.dedup();
            for target in targets {
                probe_rewind::<F>(&script, target);
            }
        }
        rewind_equals_replay::<V3PoolState>(&kinds);
        rewind_equals_replay::<V4PoolState>(&kinds);
    }
}
