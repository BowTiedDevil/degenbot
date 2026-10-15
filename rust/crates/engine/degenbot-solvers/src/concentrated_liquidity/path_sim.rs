//! Public, byte-exact path simulator over precomputed CL hop tables.
//!
//! External harnesses (benchmarks, offline replays) need to evaluate path
//! profit through the SAME integer oracle the active-set walk uses, without
//! reaching into the walk's private machinery. [`ConcentratedLiquidityPathSim`] mirrors
//! [`crate::concentrated_liquidity::active_set::simulate_walk_path`] — the self-determined-crossing
//! forward simulator — over a path of CL sequences, deriving (or reusing) each
//! hop's crossing table once at construction so per-candidate evaluation is
//! pure integer arithmetic.

use std::sync::Arc;

use alloy::primitives::U256;

use super::entries::PreparedConcentratedLiquidityHop;
use super::hop_sim::simulate_v3_range_swap;
use super::tally::WalkTally;
use super::IntV3TickRangeSequence;

/// The forward simulation of a CL path at one input: per-hop outputs, the
/// ending-range index landed in per hop, and the final output.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConcentratedLiquidityPathOutcome {
    /// Output after the last hop.
    pub final_output: U256,
    /// `hop_outputs[i]` = output after hop `i`.
    pub hop_outputs: Vec<U256>,
    /// Ending-range index landed in per hop.
    pub landed: Vec<usize>,
}

/// A reusable, byte-exact CL path simulator.
///
/// Construction derives each hop's crossing table and word profiles once (or
/// O(1)-clones them from supplied prepared hops); [`ConcentratedLiquidityPathSim::output`] then
/// reproduces the active-set walk's forward oracle for any input.
pub struct ConcentratedLiquidityPathSim {
    hops: Vec<PreparedConcentratedLiquidityHop>,
}

impl ConcentratedLiquidityPathSim {
    /// Build a simulator for `sequences` in path order. `prepared[k]`, when
    /// present, supplies hop `k`'s already-derived tables (the projection's
    /// `Arc`-shared crossing + profile tables); otherwise they are built here.
    #[must_use]
    pub fn new(
        sequences: &[&IntV3TickRangeSequence],
        prepared: Option<&[PreparedConcentratedLiquidityHop]>,
    ) -> Self {
        let hops = sequences
            .iter()
            .enumerate()
            .map(|(i, seq)| {
                if let Some(prepared) = prepared.and_then(|p| p.get(i)) {
                    // The projection's prepared value: `Arc`-shared sequence +
                    // tables, nothing rebuilt.
                    prepared.clone()
                } else {
                    let crossings = Arc::new(
                        super::crossings::build_concentrated_liquidity_crossing_table(seq),
                    );
                    let mut tally = WalkTally::default();
                    let profiles = Arc::new(super::crossings::build_word_profiles(
                        crossings.as_slice(),
                        &mut tally,
                    ));
                    PreparedConcentratedLiquidityHop::from_parts(
                        Arc::new((*seq).clone()),
                        crossings,
                        profiles,
                    )
                }
            })
            .collect();
        Self { hops }
    }

    /// Evaluate the path at `amount_in`.
    #[must_use]
    pub fn output(&self, amount_in: U256) -> ConcentratedLiquidityPathOutcome {
        let n_hops = self.hops.len();
        let mut hop_outputs = Vec::with_capacity(n_hops);
        let mut landed = Vec::with_capacity(n_hops);
        let mut current = amount_in;
        // The helper has no collector; the tally is a throwaway so the
        // byte-exact forward oracle keeps one shape for every caller.
        let mut tally = WalkTally::default();
        for hop in &self.hops {
            if current.is_zero() {
                hop_outputs.push(U256::ZERO);
                landed.push(0);
                continue;
            }
            let k = super::crossings::landed_ending_range_index(hop.crossings.as_slice(), current);
            let crossing = &hop.crossings[k];
            let remaining = current - crossing.crossing_gross_input;
            let ending_output = match hop.profiles.get(k).and_then(Option::as_deref) {
                Some(profile) => profile.swap(remaining, &mut tally).output,
                None => {
                    simulate_v3_range_swap(remaining, &crossing.ending_range, &mut tally).output
                }
            };
            let out = crossing.crossing_output.saturating_add(ending_output);
            landed.push(k);
            hop_outputs.push(out);
            current = out;
        }
        ConcentratedLiquidityPathOutcome {
            final_output: hop_outputs.last().copied().unwrap_or(U256::ZERO),
            hop_outputs,
            landed,
        }
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::concentrated_liquidity::active_set::{simulate_walk_path, PieceView};
    use crate::concentrated_liquidity::crossings::{
        build_concentrated_liquidity_crossing_table, concentrated_liquidity_walk_hop,
    };
    use crate::concentrated_liquidity::entries::{
        derive_and_solve_concentrated_liquidity_piecewise, solve_concentrated_liquidity_piecewise,
        PreparedConcentratedLiquidityHop,
    };
    use crate::concentrated_liquidity::WalkTally;
    use crate::runtime::SolveRuntimeConfig;
    use degenbot_math::concentrated_liquidity::tick_math::get_sqrt_ratio_at_tick_internal;
    use degenbot_pools::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};

    fn sp_at(tick: i32) -> U256 {
        U256::from(get_sqrt_ratio_at_tick_internal(tick).unwrap())
    }

    /// Build a multi-range CL sequence in swap order around `anchor_tick`
    /// (the construction pattern of `examples/synth_corpus_gen.rs`).
    fn multi_range_sequence(
        anchor_tick: i32,
        step: i32,
        zfo: bool,
        liquidities: &[u128],
    ) -> IntV3TickRangeSequence {
        let ranges: Vec<IntV3TickRangeHop> = liquidities
            .iter()
            .enumerate()
            .map(|(i, &liquidity)| {
                let i = i32::try_from(i).unwrap();
                let (tick_lo, tick_hi) = if zfo {
                    (anchor_tick - (i + 1) * step, anchor_tick - i * step)
                } else {
                    (anchor_tick + i * step, anchor_tick + (i + 1) * step)
                };
                let sqrt_price_x96 = if i == 0 {
                    sp_at(anchor_tick)
                } else if zfo {
                    sp_at(anchor_tick - i * step)
                } else {
                    sp_at(anchor_tick + i * step)
                };
                IntV3TickRangeHop {
                    liquidity,
                    sqrt_price_x96,
                    sqrt_price_lower_x96: sp_at(tick_lo),
                    sqrt_price_upper_x96: sp_at(tick_hi),
                    gamma_numer: 997_000,
                    fee_denom: 1_000_000,
                    zero_for_one: zfo,
                    word_boundary_prices: Vec::new(),
                }
            })
            .collect();
        IntV3TickRangeSequence::new(ranges).unwrap()
    }

    /// Assert the helper matches the walk byte-for-byte across a probe sweep
    /// (anchor, anchor±1..8, every crossing boundary ±1 and +37, plus 0/1).
    fn assert_parity(sequences: &[&IntV3TickRangeSequence], label: &str) {
        let cfg = SolveRuntimeConfig::default();
        // The projection's shape is one `PreparedConcentratedLiquidityHop` per CL hop.
        let prepared_hops: Vec<PreparedConcentratedLiquidityHop> = sequences
            .iter()
            .map(|s| {
                PreparedConcentratedLiquidityHop::derive(Arc::new(IntV3TickRangeSequence::clone(s)))
            })
            .collect();
        let via_prepared =
            solve_concentrated_liquidity_piecewise(&prepared_hops, None, None, &cfg, None);
        let via_derived = derive_and_solve_concentrated_liquidity_piecewise(sequences, &cfg);
        assert_eq!(
            via_prepared.result, via_derived.result,
            "{label}: prepared and derived solve entries agree"
        );

        let hops: Vec<PieceView> = sequences
            .iter()
            .map(|s| concentrated_liquidity_walk_hop(s, None))
            .collect();
        let helper = ConcentratedLiquidityPathSim::new(sequences, Some(&prepared_hops));
        let helper_derived = ConcentratedLiquidityPathSim::new(sequences, None);

        let mut probes: Vec<U256> = vec![U256::ZERO, U256::ONE];
        for seq in sequences {
            for crossing in build_concentrated_liquidity_crossing_table(seq) {
                let boundary = crossing.crossing_gross_input;
                probes.push(boundary);
                probes.push(boundary.saturating_add(U256::ONE));
                probes.push(boundary.saturating_sub(U256::ONE));
                probes.push(boundary.saturating_add(U256::from(37u64)));
            }
        }

        if let Some((x_star, profit, hop_outputs)) = via_prepared.result.as_ref() {
            probes.push(*x_star);
            for delta in 1..=8u64 {
                probes.push(x_star.saturating_add(U256::from(delta)));
                probes.push(x_star.saturating_sub(U256::from(delta)));
            }

            let at_optimum = helper.output(*x_star);
            assert_eq!(
                &at_optimum.hop_outputs, hop_outputs,
                "{label}: helper hop_outputs vs walk result"
            );
            assert_eq!(
                at_optimum.final_output - *x_star,
                *profit,
                "{label}: helper final_output - x vs walk profit"
            );
            let walk = simulate_walk_path(*x_star, &hops, &mut WalkTally::default());
            assert_eq!(
                at_optimum.landed, walk.landed,
                "{label}: helper landed vs walk landed"
            );
            assert_eq!(
                at_optimum.final_output, walk.final_output,
                "{label}: helper final_output vs walk final_output"
            );
        }

        for x in probes {
            let from_prepared = helper.output(x);
            let from_derived = helper_derived.output(x);
            let walk = simulate_walk_path(x, &hops, &mut WalkTally::default());
            assert_eq!(
                from_prepared.hop_outputs, walk.hop_outputs,
                "{label}: hop_outputs mismatch at x={x}"
            );
            assert_eq!(
                from_prepared.landed, walk.landed,
                "{label}: landed mismatch at x={x}"
            );
            assert_eq!(
                from_prepared.final_output, walk.final_output,
                "{label}: final_output mismatch at x={x}"
            );
            assert_eq!(
                from_derived.hop_outputs, from_prepared.hop_outputs,
                "{label}: tableless hop_outputs mismatch at x={x}"
            );
            assert_eq!(
                from_derived.landed, from_prepared.landed,
                "{label}: tableless landed mismatch at x={x}"
            );
            assert_eq!(
                from_derived.final_output, from_prepared.final_output,
                "{label}: tableless final_output mismatch at x={x}"
            );
        }
    }

    #[test]
    fn parity_single_range_hops() {
        let a = multi_range_sequence(0, 60, true, &[10_000_000_000_000u128]);
        let b = multi_range_sequence(0, 60, false, &[10_000_000_000_000u128]);
        let c = multi_range_sequence(100, 60, true, &[8_000_000_000_000u128]);
        assert_parity(&[&a, &b, &c], "single_range_hops");
    }

    #[test]
    fn parity_multi_range_walk() {
        let a = multi_range_sequence(-100, 60, true, &[5_000_000_000_000u128; 8]);
        let b = multi_range_sequence(0, 60, false, &[10_000_000_000_000u128; 6]);
        let c = multi_range_sequence(100, 60, true, &[7_000_000_000_000u128; 5]);
        assert_parity(&[&a, &b, &c], "multi_range_walk");
    }

    /// Family-W shape (synth corpus `path 27817`): thin bars with one deep
    /// late-liquidity range pushing the optimal landing index past the legacy
    /// prefix cap.
    #[test]
    fn parity_family_w_deep_late_liquidity() {
        let hop1 = multi_range_sequence(750, 1300, true, &[1_000_000_000_000_000u128]);
        let mut thin = vec![1_000_000_000u128; 60];
        thin.push(1_000_000_000_000_000u128);
        let hop2 = multi_range_sequence(0, 60, false, &thin);
        let hop3 = multi_range_sequence(
            -200,
            10,
            true,
            &[
                5_000_000_000_000u128,
                8_000_000_000_000u128,
                3_000_000_000_000u128,
            ],
        );
        assert_parity(&[&hop1, &hop2, &hop3], "family_w_deep_late_liquidity");
    }

    #[test]
    fn parity_unprofitable_path() {
        let a = multi_range_sequence(0, 60, true, &[10_000_000_000_000u128]);
        let b = multi_range_sequence(0, 60, false, &[10_000_000_000_000u128]);
        let c = multi_range_sequence(0, 60, true, &[10_000_000_000_000u128]);
        let cfg = SolveRuntimeConfig::default();
        let outcome = derive_and_solve_concentrated_liquidity_piecewise(&[&a, &b, &c], &cfg);
        assert!(
            outcome.result.is_none(),
            "same-product 3-hop must be unprofitable"
        );
        assert_parity(&[&a, &b, &c], "unprofitable_path");
    }

    #[test]
    fn output_zero_input_is_zero() {
        let a = multi_range_sequence(0, 60, true, &[10_000_000_000_000u128]);
        let b = multi_range_sequence(0, 60, false, &[10_000_000_000_000u128]);
        let sim = ConcentratedLiquidityPathSim::new(&[&a, &b], None);
        let out = sim.output(U256::ZERO);
        assert_eq!(out.final_output, U256::ZERO);
        assert_eq!(out.hop_outputs, vec![U256::ZERO, U256::ZERO]);
        assert_eq!(out.landed, vec![0, 0]);
    }
}
