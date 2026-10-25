//! The shared test fixture for the concentrated liquidity state modules' merged test suites
//! (ADR-080 D4). The former V3/V4 twin batteries converge at the state
//! modules' interface: every shared behavior is written ONCE, generic over
//! [`ClStateFixture`], in the three state modules' `tests` mods. Each adapter
//! supplies only its construction + event-application fixture — the
//! `impl ClStateFixture for V3PoolState` / `for V4PoolState` blocks live in
//! the per-design files (`v3_state/fixture_impl.rs`, `v4_state/fixture_impl.rs`),
//! where the private fields are visible. What stays genuinely per-adapter
//! (slot0-vs-state-view storage shapes, hook-keyed identity, per-design event
//! application detail) keeps its thin tests on the adapters.

#![expect(clippy::expect_used, clippy::panic)]

use alloy::primitives::{U128, U256};

use crate::families::concentrated_liquidity::state_journal::JournalHost;
use crate::families::concentrated_liquidity::state_seeding::SeedingHost;
use crate::families::concentrated_liquidity::state_walk::{CachedTickRanges, WalkHost};
use crate::state::int_v3_hop::IntV3TickRangeSequence;
use crate::state::state_history::ReorgPoolState;
use crate::state::v3_state::PoolTickCoverage;
use crate::TickInfo;

/// Registration block of the seeded pool: the genesis anchor lands here and
/// every scripted event lands strictly after it.
pub(crate) const REG: u64 = 50;

/// Active liquidity of the rewind battery's registration-state pool.
pub(crate) const LIQ: u128 = 1_000_000;

/// The per-adapter fixture seam the merged state-module test suites drive —
/// construction, event application, and the few field probes/mutations the
/// shared batteries need that the host traits (`WalkHost`, `SeedingHost`,
/// `JournalHost`) do not already expose. Implemented by `V3PoolState` and
/// `V4PoolState` in their own files.
pub(crate) trait ClStateFixture:
    WalkHost + SeedingHost + JournalHost + ReorgPoolState + Sized
{
    /// Minimal state at tick 0, 1:1 price, liquidity `liq`, with a
    /// [-60, +60] position (so `tick_data` has ticks -60 and +60
    /// initialized). The journal is fresh (depth 8); snapshot/pinned fields
    /// are `None`.
    fn with_position(liq: u128) -> Self;

    /// The rewind battery's registration-state pool: liquidity `LIQ`, one
    /// [-60, +60) position, empty depth-16 journal.
    fn pool_state() -> Self;

    /// Push the registration anchor (`seed_genesis`: a `before == after`
    /// delta that advances no clock).
    fn seed_genesis(&mut self, block: u64);

    /// `pool_state` plus the registration anchor at `REG`.
    fn seeded_state() -> Self {
        let mut state = Self::pool_state();
        Self::seed_genesis(&mut state, REG);
        state
    }

    /// Apply a Swap event the way an event replay would.
    fn apply_swap(
        &mut self,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        block: u64,
        tick_priors: &[(i32, TickInfo)],
    );

    /// Apply a Mint/Burn over `[lower, upper)`.
    fn apply_liquidity_update(&mut self, lower: i32, upper: i32, delta: i128, block: u64);

    /// The adapter's integer tick-range walk — `build_int_v3_sequence` on
    /// V3, `build_int_v4_sequence` (with the state's own packed protocol
    /// fee) on V4.
    fn build_int_sequence(
        &self,
        tick_spacing: i32,
        fee: u32,
        zero_for_one: bool,
    ) -> Option<IntV3TickRangeSequence>;

    /// The per-mutation nonce.
    fn state_nonce(&self) -> u64;

    /// A snapshot of one direction's tick-range cache slot.
    fn cached_slot(&self, zero_for_one: bool) -> CachedTickRanges;

    /// Overwrite the current tick (fixture setup).
    fn set_tick(&mut self, tick: i32);

    /// Overwrite the slot0 price scalar (fixture setup).
    fn set_sqrt_price(&mut self, sqrt_price_x96: U256);

    /// Overwrite the seed block (fixture setup for the historical-replay
    /// guard tests).
    fn set_initial_state_block(&mut self, block: u64);

    /// Overwrite the tick-map coverage (fixture setup for the
    /// coverage-gated registration-seeding tests).
    fn set_coverage(&mut self, coverage: PoolTickCoverage);
}

/// One event of a scripted sequence.
#[derive(Clone, Debug)]
pub(crate) enum Ev {
    /// Slot0 head rewrite; advances BOTH clocks. `new_tick` seeds
    /// `tick_priors` (a tick mutation — a creation when the tick is absent
    /// from the map).
    Swap {
        sqrt: u128,
        liq: u128,
        tick: i32,
        new_tick: Option<(i32, i128)>,
    },
    /// Mint/Burn over `[lower, upper)`.
    Liq { lower: i32, upper: i32, delta: i128 },
}

/// Zip a kind sequence into a script on strictly-increasing blocks
/// (`REG + 1`, `REG + 2`, ...) so a live apply never trips the monotonic
/// clock guard.
pub(crate) fn script_from_kinds(kinds: &[u8]) -> Vec<(u64, Ev)> {
    kinds
        .iter()
        .enumerate()
        .map(|(i, k)| {
            let block = REG + 1 + i as u64;
            let ev = match k % 6 {
                0 => Ev::Swap {
                    sqrt: 1u128 << 96,
                    liq: LIQ,
                    tick: 0,
                    new_tick: None,
                },
                1 => Ev::Swap {
                    sqrt: (1u128 << 96) + 1,
                    liq: LIQ + 1,
                    tick: 60,
                    new_tick: Some((120, 500)),
                },
                2 => Ev::Swap {
                    sqrt: (1u128 << 96) - 1,
                    liq: LIQ - 2,
                    tick: 500,
                    new_tick: Some((180, 250)),
                },
                3 => Ev::Liq {
                    lower: -60,
                    upper: 60,
                    delta: 7_000,
                },
                4 => Ev::Liq {
                    lower: 120,
                    upper: 240,
                    delta: 3_000,
                },
                _ => Ev::Liq {
                    lower: 120,
                    upper: 240,
                    delta: -3_000,
                },
            };
            (block, ev)
        })
        .collect()
}

/// Apply one script entry the way an event replay would.
pub(crate) fn apply_ev<F: ClStateFixture>(state: &mut F, block: u64, ev: &Ev) {
    match ev {
        Ev::Swap {
            sqrt,
            liq,
            tick,
            new_tick,
        } => {
            let priors: Vec<(i32, TickInfo)> = new_tick
                .iter()
                .map(|(t, net)| {
                    (
                        *t,
                        TickInfo {
                            liquidity_gross: U128::from(u128::try_from(*net).unwrap_or(0)),
                            liquidity_net: *net,
                            block,
                        },
                    )
                })
                .collect();
            state.apply_swap(U256::from(*sqrt), *liq, *tick, block, &priors);
        }
        Ev::Liq {
            lower,
            upper,
            delta,
        } => {
            state.apply_liquidity_update(*lower, *upper, *delta, block);
        }
    }
}

/// Apply a whole script in order.
pub(crate) fn apply_script<F: ClStateFixture>(state: &mut F, script: &[(u64, Ev)]) {
    for (block, ev) in script {
        apply_ev(state, *block, ev);
    }
}

/// Replay only the events strictly before `target` onto a fresh seed — the
/// oracle `restore_before_block(target)` must reproduce exactly.
pub(crate) fn replay_before<F: ClStateFixture>(script: &[(u64, Ev)], target: u64) -> F {
    let mut state = F::seeded_state();
    for (block, ev) in script {
        if *block < target {
            apply_ev(&mut state, *block, ev);
        }
    }
    state
}

/// Field-by-field comparison that names the FIRST divergence — stamps first,
/// since a stamp is what an approximated rewind gets wrong while its values
/// still match.
pub(crate) fn assert_same_state<F: ClStateFixture>(tag: &str, got: &F, want: &F, detail: &str) {
    let got_ticks = WalkHost::tick_data(got);
    let want_ticks = WalkHost::tick_data(want);
    let mut keys: Vec<i32> = got_ticks.keys().chain(want_ticks.keys()).copied().collect();
    keys.sort_unstable();
    keys.dedup();
    for t in keys {
        match (got_ticks.get(&t), want_ticks.get(&t)) {
            (Some(a), Some(b)) => {
                assert!(
                    a.block == b.block,
                    "{tag}: tick {t} STAMP mismatch - rewind says {} but replay says {} ({detail})",
                    a.block,
                    b.block
                );
                assert!(
                    a.liquidity_gross == b.liquidity_gross && a.liquidity_net == b.liquidity_net,
                    "{tag}: tick {t} VALUE mismatch - rewind {a:?} vs replay {b:?} ({detail})"
                );
            }
            (Some(a), None) => panic!(
                "{tag}: tick {t} present after rewind ({a:?}) but absent in replay ({detail})"
            ),
            (None, Some(b)) => panic!(
                "{tag}: tick {t} absent after rewind but present in replay ({b:?}) ({detail})"
            ),
            (None, None) => {}
        }
    }
    assert_eq!(
        WalkHost::sqrt_price_x96(got),
        WalkHost::sqrt_price_x96(want),
        "{tag}: sqrt_price_x96 ({detail})"
    );
    assert_eq!(
        WalkHost::liquidity(got),
        WalkHost::liquidity(want),
        "{tag}: active liquidity scalar ({detail})"
    );
    assert_eq!(
        WalkHost::tick(got),
        WalkHost::tick(want),
        "{tag}: current tick ({detail})"
    );
    assert_eq!(
        WalkHost::update_block(got),
        WalkHost::update_block(want),
        "{tag}: price clock ({detail})"
    );
    assert_eq!(
        WalkHost::tick_data_block(got),
        WalkHost::tick_data_block(want),
        "{tag}: liquidity clock ({detail})"
    );
}

/// Rewind a fully-applied pool to `target` and compare against a fresh
/// replay of the events before it.
pub(crate) fn probe_rewind<F: ClStateFixture>(script: &[(u64, Ev)], target: u64) {
    let mut live = F::seeded_state();
    apply_script(&mut live, script);
    live.restore_before_block(target).expect("rewind is legal");
    assert_same_state(
        "rewind==replay",
        &live,
        &replay_before(script, target),
        &format!("target={target} script={script:?}"),
    );
}
