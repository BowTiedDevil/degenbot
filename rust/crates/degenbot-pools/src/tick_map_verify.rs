//! The tick-map verification seam (ADR-021 D3 slice 2, X6I3LN).
//!
//! One pure per-tick comparison shared by every consumer that asks
//! "does the stored CL tick-map state match the chain at block B?"
//! (registration lifecycle verify, the Python engine verify, the
//! solve-time tripwire's tick-map fidelity probe). Consumers supply
//! their own tick COVERAGE (which ticks to read) and keep their own
//! REACTION (typed registration error / bridge error / trip + exit);
//! the seam converges the verdict, not the reaction.
//!
//! The provider-bound batch reads stay in `degenbot-bot`'s
//! `bot_core::liquidity_verifier` (ADR-004 state crate stays pyo3-free
//! and read-free).

use alloy::primitives::U256;
use std::collections::HashMap;

/// One divergent tick between a stored tick map and the observed
/// on-chain tick set. A side is `None` when that side holds no entry
/// for the tick (stored tick never observed on-chain, or an on-chain
/// tick the engine does not hold).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickDivergence {
    /// The tick index.
    pub tick: i32,
    /// `(liquidity_gross, liquidity_net)` as stored; `None` when the
    /// tick exists only on the observed side.
    pub stored: Option<(u128, i128)>,
    /// `(liquidity_gross, liquidity_net)` as observed on-chain (or in
    /// the sim); `None` when the tick exists only in the stored map.
    pub on_chain: Option<(u128, i128)>,
}

/// The CL slot0 head scalars — the same fact family the tripwire's
/// `SolverHopScalarState` pins and the `[sim-revert-swap]` diagnostic
/// records as fix-enablers. Shared so the two log surfaces speak one
/// type language.
/// Compare a stored tick map against an observed on-chain (or post-sim)
/// tick set. Pure: no reads, no env, no logging — the caller supplies the
/// coverage (which tick set to read) and owns the reaction to the verdict.
///
/// A tick diverges when it is present on exactly one side, or present on
/// both with a different `(liquidity_gross, liquidity_net)`. The output is
/// in ascending tick order regardless of `HashMap` iteration order, so a
/// consumer's "first divergence" is deterministic.
#[must_use]
pub fn compare_tick_maps<S: std::hash::BuildHasher>(
    stored: &HashMap<i32, (u128, i128), S>,
    observed: &HashMap<i32, (u128, i128), S>,
) -> Vec<TickDivergence> {
    let mut ticks: Vec<i32> = stored.keys().chain(observed.keys()).copied().collect();
    ticks.sort_unstable();
    ticks.dedup();
    let mut out = Vec::new();
    for tick in ticks {
        let s = stored.get(&tick).copied();
        let o = observed.get(&tick).copied();
        if s == o {
            continue;
        }
        out.push(TickDivergence {
            tick,
            stored: s,
            on_chain: o,
        });
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Slot0HeadScalars {
    /// Post-swap (or at-block) `sqrtPriceX96`.
    pub sqrt_price_x96: U256,
    /// Active tick at the observed moment.
    pub tick: i32,
    /// Active liquidity at the observed moment.
    pub liquidity: U256,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hm(pairs: &[(i32, (u128, i128))]) -> HashMap<i32, (u128, i128)> {
        pairs.iter().copied().collect()
    }

    #[test]
    fn identical_maps_produce_no_divergence() {
        let a = hm(&[(100, (10, 1)), (200, (20, -1)), (300, (0, 0))]);
        let b = hm(&[(100, (10, 1)), (200, (20, -1)), (300, (0, 0))]);
        assert_eq!(compare_tick_maps(&a, &b), Vec::<TickDivergence>::new());
    }

    #[test]
    fn gross_difference_diverges_with_both_sides() {
        let a = hm(&[(100, (10, 1))]);
        let b = hm(&[(100, (11, 1))]);
        assert_eq!(
            compare_tick_maps(&a, &b),
            vec![TickDivergence {
                tick: 100,
                stored: Some((10, 1)),
                on_chain: Some((11, 1))
            }]
        );
    }

    #[test]
    fn net_only_difference_diverges() {
        let a = hm(&[(100, (10, 1))]);
        let b = hm(&[(100, (10, 2))]);
        assert_eq!(
            compare_tick_maps(&a, &b),
            vec![TickDivergence {
                tick: 100,
                stored: Some((10, 1)),
                on_chain: Some((10, 2))
            }]
        );
    }

    #[test]
    fn stored_tick_absent_from_observed_side() {
        let a = hm(&[(100, (10, 1))]);
        let b = hm(&[]);
        assert_eq!(
            compare_tick_maps(&a, &b),
            vec![TickDivergence {
                tick: 100,
                stored: Some((10, 1)),
                on_chain: None
            }]
        );
    }

    #[test]
    fn observed_only_tick_diverges() {
        let a = hm(&[]);
        let b = hm(&[(100, (7, -7))]);
        assert_eq!(
            compare_tick_maps(&a, &b),
            vec![TickDivergence {
                tick: 100,
                stored: None,
                on_chain: Some((7, -7))
            }]
        );
    }

    #[test]
    fn divergences_sorted_ascending_regardless_of_input_order() {
        let a = hm(&[(500, (1, 1)), (300, (1, 1)), (100, (1, 1))]);
        let b = hm(&[(500, (2, 2)), (300, (2, 2)), (100, (2, 2))]);
        let d = compare_tick_maps(&a, &b);
        assert_eq!(
            d.iter().map(|x| x.tick).collect::<Vec<_>>(),
            vec![100, 300, 500]
        );
    }

    #[test]
    fn equal_ticks_skipped_among_divergent_ones() {
        let a = hm(&[(100, (1, 1)), (200, (5, 5))]);
        let b = hm(&[(100, (1, 1)), (200, (6, 5))]);
        assert_eq!(
            compare_tick_maps(&a, &b),
            vec![TickDivergence {
                tick: 200,
                stored: Some((5, 5)),
                on_chain: Some((6, 5))
            }]
        );
    }

    #[test]
    fn slot0_head_scalars_default_is_all_zero() {
        let s = Slot0HeadScalars::default();
        assert_eq!(s.sqrt_price_x96, U256::ZERO);
        assert_eq!(s.tick, 0);
        assert_eq!(s.liquidity, U256::ZERO);
    }

    #[test]
    fn slot0_head_scalars_construct_and_compare() {
        let s = Slot0HeadScalars {
            sqrt_price_x96: U256::from(3u64),
            tick: -42,
            liquidity: U256::from(7u64),
        };
        assert_eq!(
            s,
            Slot0HeadScalars {
                sqrt_price_x96: U256::from(3u64),
                tick: -42,
                liquidity: U256::from(7u64),
            }
        );
        assert_ne!(s, Slot0HeadScalars::default());
    }
}
