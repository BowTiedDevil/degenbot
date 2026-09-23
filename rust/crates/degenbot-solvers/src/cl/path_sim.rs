//! Public, byte-exact path simulator over precomputed CL hop tables.
//!
//! External harnesses (benchmarks, offline replays) need to evaluate path
//! profit through the SAME integer oracle the active-set walk uses, without
//! reaching into the walk's private machinery. [`ClPathSim`] mirrors
//! [`crate::cl::active_set::simulate_walk_path`] — the self-determined-crossing
//! forward simulator — over a path of CL sequences, deriving (or reusing) each
//! hop's crossing table once at construction so per-candidate evaluation is
//! pure integer arithmetic.

use std::sync::Arc;

use alloy::primitives::U256;

use super::entries::ClSolveTables;
use super::hop_sim::simulate_v3_range_swap;
use super::{ClCrossingTable, ClProfileTable, IntV3TickRangeSequence};

/// One CL hop's precomputed tables, reusing the projection's `Arc`-shared
/// tables when supplied.
struct ClPathSimHop {
    crossings: Arc<ClCrossingTable>,
    profiles: Arc<ClProfileTable>,
}

/// The forward simulation of a CL path at one input: per-hop outputs, the
/// ending-range index landed in per hop, and the final output.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClPathOutcome {
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
/// clones them from supplied [`ClSolveTables`]); [`ClPathSim::output`] then
/// reproduces the active-set walk's forward oracle for any input.
pub struct ClPathSim {
    hops: Vec<ClPathSimHop>,
}

impl ClPathSim {
    /// Build a simulator for `sequences` in path order. `prepared[k]`, when
    /// present, supplies hop `k`'s already-derived tables; otherwise they are
    /// built here.
    #[must_use]
    pub fn new(sequences: &[&IntV3TickRangeSequence], prepared: Option<&[ClSolveTables]>) -> Self {
        let hops = sequences
            .iter()
            .enumerate()
            .map(|(i, seq)| {
                let (crossings, profiles) = if let Some(tables) = prepared.and_then(|p| p.get(i)) {
                    (Arc::clone(&tables.crossings), Arc::clone(&tables.profiles))
                } else {
                    let crossings = Arc::new(super::crossings::build_cl_crossing_table(seq));
                    let profiles =
                        Arc::new(super::crossings::build_word_profiles(crossings.as_slice()));
                    (crossings, profiles)
                };
                ClPathSimHop {
                    crossings,
                    profiles,
                }
            })
            .collect();
        Self { hops }
    }

    /// Evaluate the path at `amount_in`.
    #[must_use]
    pub fn output(&self, amount_in: U256) -> ClPathOutcome {
        let n_hops = self.hops.len();
        let mut hop_outputs = Vec::with_capacity(n_hops);
        let mut landed = Vec::with_capacity(n_hops);
        let mut current = amount_in;
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
                Some(profile) => profile.swap(remaining).output,
                None => simulate_v3_range_swap(remaining, &crossing.ending_range).output,
            };
            let out = crossing.crossing_output.saturating_add(ending_output);
            landed.push(k);
            hop_outputs.push(out);
            current = out;
        }
        ClPathOutcome {
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
    use crate::cl::active_set::{simulate_walk_path, PieceView};
    use crate::cl::crossings::{build_cl_crossing_table, cl_walk_hop};
    use crate::cl::entries::{derive_and_solve_cl_piecewise, solve_cl_piecewise};
    use crate::runtime::SolveRuntimeConfig;
    use degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal;
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
        let prepared: Vec<ClSolveTables> =
            sequences.iter().map(|s| ClSolveTables::derive(s)).collect();
        let via_prepared = solve_cl_piecewise(sequences, &prepared, None, &cfg, None);
        let via_derived = derive_and_solve_cl_piecewise(sequences, &cfg);
        assert_eq!(
            via_prepared.result, via_derived.result,
            "{label}: prepared and derived solve entries agree"
        );

        let hops: Vec<PieceView> = sequences.iter().map(|s| cl_walk_hop(s, None)).collect();
        let helper = ClPathSim::new(sequences, Some(&prepared));
        let helper_derived = ClPathSim::new(sequences, None);

        let mut probes: Vec<U256> = vec![U256::ZERO, U256::ONE];
        for seq in sequences {
            for crossing in build_cl_crossing_table(seq) {
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
            let walk = simulate_walk_path(*x_star, &hops);
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
            let walk = simulate_walk_path(x, &hops);
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
        let outcome = derive_and_solve_cl_piecewise(&[&a, &b, &c], &cfg);
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
        let sim = ClPathSim::new(&[&a, &b], None);
        let out = sim.output(U256::ZERO);
        assert_eq!(out.final_output, U256::ZERO);
        assert_eq!(out.hop_outputs, vec![U256::ZERO, U256::ZERO]);
        assert_eq!(out.landed, vec![0, 0]);
    }
}
