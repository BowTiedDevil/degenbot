//! The pre-sim gas floor: a measured per-hop execution-cost model that
//! admission prices a solved candidate against.
//!
//! A candidate's execution cost is not a constant — it is dominated by how far
//! the swap walk travelled through each pool's tick map, which the solver
//! already knows. The coefficients here are measured on evm2 over archive
//! mainnet state (`docs/gas-estimation.md` carries the rigs, the fits, and the
//! error distributions); this module is the data and the pure arithmetic, so a
//! consumer (the delivery policy's profit window) can floor a candidate before
//! the simulation without re-deriving any cost.
//!
//! # Shape
//!
//! ```text
//! gas(path) = settlement_fixed
//!           + Σ_hops ( leg_gas(hop) + transfer_warm )
//!
//! leg_gas(V2)        = v2_fixed
//! leg_gas(V3)        = min(v3_fixed, concentrated_intercept_p10)
//!                    + v3_per_crossing · initialized_ticks_crossed
//!                    + v3_per_word      · bitmap_words_walked
//! leg_gas(V4)        = min(v4_fixed, concentrated_intercept_p10)
//!                    + v4_per_crossing · initialized_ticks_crossed
//!                    + v4_per_word      · bitmap_words_walked
//! ```
//!
//! # Every choice is the under-predicting one
//!
//! A candidate that clears the floor and fails verification costs one wasted
//! simulation; a candidate suppressed wrongly is lost alpha. So:
//!
//! - the concentrated-liquidity fixed term is capped at the p10 of the
//!   corpus's per-pool intercepts (`concentrated_intercept_p10`) — the family
//!   median over-predicts the cheap pools, and a pool's intercept is a pool
//!   property (measured 96.9k…151.1k across the corpus), not a family
//!   constant;
//! - a per-hop transfer is charged at the WARM marginal (the cheap side): a
//!   settling hop pushes its output token into a recipient that already holds
//!   a balance, and the cold first-touch is charged once per path instead;
//! - a census that ran out of modelled range (`SwapWalkCensus::bounded_tail`)
//!   reports lower bounds, and the floor takes them as given — the true cost is
//!   higher.
//!
//! # Units
//!
//! The floor is gas, and [`GasFloorModel::floor_wei`] prices it at the block's
//! base fee, so the result is ETH wei. It is comparable to a solver result's
//! `profit` only when that profit is denominated in an 18-decimal
//! ether-equivalent token (the settlement engine's WETH-terminated paths).
//! A path whose profit token is not comparable must scale the floor itself;
//! this module has no price oracle and invents none.

use alloy::primitives::U256;

use super::SolvePathResult;
use crate::state::int_v3_hop::SwapWalkCensus;

/// One hop's projection into the floor's cost model: the family (which selects
/// the coefficient row) plus the walk's own census for the hop.
///
/// The census is the walk's fact, not a re-derivation: the solver records it at
/// the evaluation that produced the result (`degenbot_pools::solve::
/// SolvePathResult`'s `gas_hops`), so admission reads it without walking again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HopGasProfile {
    /// Uniswap V2 constant-product: one flat leg cost, no tick map, no census.
    V2,
    /// Uniswap V3 concentrated liquidity. `census` is `None` when the hop's
    /// sequence carries no tick map (nothing to project) or the result did not
    /// come from the walk.
    V3 { census: Option<SwapWalkCensus> },
    /// Uniswap V4 concentrated liquidity (same swap math as V3, a different
    /// settlement frame, hence its own coefficient row).
    V4 { census: Option<SwapWalkCensus> },
    /// A family with no measured coefficient row: the floor cannot price it,
    /// so it reports no floor rather than a guessed one (no floor suppresses
    /// nothing).
    #[default]
    Unmodeled,
}

/// The measured coefficient table and the settlement terms, as data.
///
/// Every number is a gas count from `docs/gas-estimation.md`'s decomposition;
/// the fields are `pub` so a test, a rig, or an operator report can read the
/// table back without re-typing it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GasFloorModel {
    /// V2's flat leg cost (the router frame's marginal over the 21,000
    /// intrinsic; flat across 10^15…10^21 input).
    pub v2_fixed: u64,
    /// V3's fitted family intercept — the median pool, NOT the floor's term
    /// (see `concentrated_intercept_p10`).
    pub v3_fixed: u64,
    /// V3's marginal per initialized tick crossed (`Tick.cross`).
    pub v3_per_crossing: u64,
    /// V3's marginal per bitmap word walked.
    pub v3_per_word: u64,
    /// V4's fitted family intercept.
    pub v4_fixed: u64,
    /// V4's marginal per initialized tick crossed.
    pub v4_per_crossing: u64,
    /// V4's marginal per bitmap word walked (77% of V3's rate: the manager's
    /// storage layout makes a V4 word cheaper).
    pub v4_per_word: u64,
    /// The p10 of the corpus's measured per-pool intercepts — the cap on every
    /// concentrated-liquidity leg's fixed term.
    pub concentrated_intercept_p10: u64,
    /// One ERC-20 transfer into a recipient with a zero balance (cold SSTORE).
    pub transfer_cold: u64,
    /// One ERC-20 transfer into a recipient that already holds a balance.
    pub transfer_warm: u64,
    /// The settlement frame's per-path fixed cost: the transaction intrinsic
    /// plus the measured caller plumbing a batch frame carries once.
    pub settlement_fixed: u64,
}

/// The measured table (`docs/gas-estimation.md`) at the anchor block the fits
/// were taken at. Numbers drift with the anchor; the shape does not.
pub const MEASURED_GAS_FLOOR: GasFloorModel = GasFloorModel {
    v2_fixed: 30_862,
    v3_fixed: 118_594,
    v3_per_crossing: 24_824,
    v3_per_word: 7_373,
    v4_fixed: 72_232,
    v4_per_crossing: 14_254,
    v4_per_word: 5_684,
    concentrated_intercept_p10: 81_947,
    transfer_cold: 41_056,
    transfer_warm: 23_956,
    // 21,000 (a transaction's intrinsic) + 4,672 (the measured caller
    // plumbing a batch frame carries once).
    settlement_fixed: 25_672,
};

impl GasFloorModel {
    /// The fixed term a concentrated-liquidity leg pays: the family's measured
    /// intercept, capped by the corpus's p10 intercept (the cheap-pool side).
    const fn concentrated_fixed(&self, family_fixed: u64) -> u64 {
        if family_fixed < self.concentrated_intercept_p10 {
            family_fixed
        } else {
            self.concentrated_intercept_p10
        }
    }

    /// One hop's quote-frame leg cost — the pool's own swap loop, no
    /// settlement. `None` for a hop the model has no row for.
    ///
    /// A missing census contributes its fixed term only: the leg ran, and its
    /// crossings are unknown, which is the under-predicting direction.
    #[must_use]
    pub fn leg_gas(&self, hop: HopGasProfile) -> Option<u64> {
        match hop {
            HopGasProfile::V2 => Some(self.v2_fixed),
            HopGasProfile::V3 { census } => {
                let (crossings, words) = census_counts(census);
                Some(
                    self.concentrated_fixed(self.v3_fixed)
                        .saturating_add(self.v3_per_crossing.saturating_mul(crossings))
                        .saturating_add(self.v3_per_word.saturating_mul(words)),
                )
            }
            HopGasProfile::V4 { census } => {
                let (crossings, words) = census_counts(census);
                Some(
                    self.concentrated_fixed(self.v4_fixed)
                        .saturating_add(self.v4_per_crossing.saturating_mul(crossings))
                        .saturating_add(self.v4_per_word.saturating_mul(words)),
                )
            }
            HopGasProfile::Unmodeled => None,
        }
    }

    /// The legs' gas alone — the sum of [`Self::leg_gas`], with no per-hop
    /// transfers and no settlement frame. This is the quantity the corpus's
    /// measured quote-frame gas is the truth set for (`docs/gas-estimation.md`);
    /// the settlement terms are measured separately and are NOT in this figure.
    /// `None` when any hop is unmodeled.
    #[must_use]
    pub fn legs_gas(&self, hops: &[HopGasProfile]) -> Option<u64> {
        let mut total = 0u64;
        for &hop in hops {
            total = total.saturating_add(self.leg_gas(hop)?);
        }
        Some(total)
    }

    /// The settling path's total gas: every leg, one warm transfer per hop, and
    /// the settlement frame's fixed cost once. `None` when any hop is
    /// unmodeled (an unknown leg makes the whole sum unknown).
    #[must_use]
    pub fn path_gas(&self, hops: &[HopGasProfile]) -> Option<u64> {
        let mut total = self.settlement_fixed.saturating_add(self.transfer_cold);
        for &hop in hops {
            total = total
                .saturating_add(self.leg_gas(hop)?)
                .saturating_add(self.transfer_warm);
        }
        Some(total)
    }

    /// The floor in wei at `base_fee_per_gas` — the settling path's gas priced
    /// at the block's base fee. `None` when the path cannot be priced.
    #[must_use]
    pub fn floor_wei(&self, hops: &[HopGasProfile], base_fee_per_gas: u128) -> Option<U256> {
        self.path_gas(hops)
            .map(|gas| U256::from(gas).saturating_mul(U256::from(base_fee_per_gas)))
    }
}

/// The two census counts as `u64`s; zeros when the census is absent.
const fn census_counts(census: Option<SwapWalkCensus>) -> (u64, u64) {
    match census {
        Some(census) => (
            census.initialized_ticks_crossed as u64,
            census.bitmap_words_walked as u64,
        ),
        None => (0, 0),
    }
}

/// The admission floor for one solved path at the block's base fee, from the
/// census the solve carried (`SolvePathResult::gas_hops`).
///
/// `None` when the path is unpriced: any hop outside the measured families, or
/// a result whose solve surface reported no hop projections at all. A `None`
/// floor admits on the static `min_profit` alone.
#[must_use]
pub fn path_gas_floor_wei(result: &SolvePathResult, base_fee_per_gas: u128) -> Option<U256> {
    // An empty projection is "the solve surface reported nothing", not "a path
    // with no hops": a real path carries at least two, so pricing the empty
    // list would report the settlement frame alone as if it were a floor.
    if result.gas_hops.is_empty() {
        return None;
    }
    MEASURED_GAS_FLOOR.floor_wei(&result.gas_hops, base_fee_per_gas)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used)]

    use super::*;

    fn census(initialized_ticks_crossed: usize, bitmap_words_walked: usize) -> SwapWalkCensus {
        SwapWalkCensus {
            initialized_ticks_crossed,
            bitmap_words_walked,
            bounded_tail: false,
        }
    }

    /// The table the module ships IS the documented table: a coefficient edit
    /// that also edits this test is a deliberate measurement change, not a
    /// silent drift.
    #[test]
    fn the_measured_table_is_the_documented_table() {
        let m = MEASURED_GAS_FLOOR;
        assert_eq!(m.v2_fixed, 30_862, "V2 flat leg");
        assert_eq!(m.v3_fixed, 118_594, "V3 family intercept");
        assert_eq!(m.v3_per_crossing, 24_824, "V3 per initialized crossing");
        assert_eq!(m.v3_per_word, 7_373, "V3 per bitmap word");
        assert_eq!(m.v4_fixed, 72_232, "V4 fixed");
        assert_eq!(m.v4_per_crossing, 14_254, "V4 per crossing");
        assert_eq!(m.v4_per_word, 5_684, "V4 per word (not V3's 7,373)");
        assert_eq!(m.concentrated_intercept_p10, 81_947, "p10 intercept");
        assert_eq!(m.transfer_cold, 41_056, "cold-recipient transfer");
        assert_eq!(m.transfer_warm, 23_956, "warm-recipient transfer");
        assert_eq!(m.settlement_fixed, 25_672, "intrinsic + caller plumbing");
    }

    /// A V2 leg is its constant: no crossing axis exists, and a supplied
    /// census (which a V2 hop never has) is not consulted.
    #[test]
    fn a_v2_leg_is_its_flat_cost() {
        let m = MEASURED_GAS_FLOOR;
        assert_eq!(m.leg_gas(HopGasProfile::V2), Some(30_862));
    }

    /// Concentrated liquidity legs: the fixed term is the p10 intercept (the
    /// family median would over-predict every cheap pool), and the marginals
    /// are per family — V4's word is cheaper than V3's.
    #[test]
    fn concentrated_liquidity_legs_price_their_census() {
        let m = MEASURED_GAS_FLOOR;

        // Zero crossings, zero words: the capped fixed term only.
        assert_eq!(
            m.leg_gas(HopGasProfile::V3 {
                census: Some(census(0, 0))
            }),
            Some(81_947),
            "V3's fixed term is capped at the p10 intercept, not the 118,594 median"
        );
        assert_eq!(
            m.leg_gas(HopGasProfile::V4 {
                census: Some(census(0, 0))
            }),
            Some(72_232),
            "V4's own fixed cost sits below the cap, so it is used as measured"
        );

        // V3: 2 crossings + 3 words.
        assert_eq!(
            m.leg_gas(HopGasProfile::V3 {
                census: Some(census(2, 3))
            }),
            Some(81_947 + 2 * 24_824 + 3 * 7_373)
        );

        // V4: 1 crossing + 1 word, at V4's own marginals.
        assert_eq!(
            m.leg_gas(HopGasProfile::V4 {
                census: Some(census(1, 1))
            }),
            Some(72_232 + 14_254 + 5_684)
        );

        // The two families' word terms must not be interchangeable: the same
        // census prices higher on V3.
        let v3 = m
            .leg_gas(HopGasProfile::V3 {
                census: Some(census(0, 10)),
            })
            .unwrap();
        let v4 = m
            .leg_gas(HopGasProfile::V4 {
                census: Some(census(0, 10)),
            })
            .unwrap();
        assert!(v3 > v4, "V3 word rate (7,373) exceeds V4's (5,684)");
    }

    /// A missing census prices the fixed term only — the leg ran, its travel is
    /// unknown, and guessing upward would suppress.
    #[test]
    fn a_hop_without_a_census_prices_its_fixed_term() {
        let m = MEASURED_GAS_FLOOR;
        assert_eq!(m.leg_gas(HopGasProfile::V3 { census: None }), Some(81_947));
        assert_eq!(m.leg_gas(HopGasProfile::V4 { census: None }), Some(72_232));
    }

    /// A path pays every leg, a warm transfer per hop, one cold transfer for
    /// its own settlement, and the settlement frame's fixed cost once.
    #[test]
    fn a_path_pays_legs_plus_transfers_plus_the_settlement_frame() {
        let m = MEASURED_GAS_FLOOR;
        let hops = [
            HopGasProfile::V2,
            HopGasProfile::V3 {
                census: Some(census(1, 0)),
            },
            HopGasProfile::V4 {
                census: Some(census(0, 2)),
            },
        ];
        let legs = 30_862 + (81_947 + 24_824) + (72_232 + 2 * 5_684);
        let expected = m.settlement_fixed + m.transfer_cold + legs + 3 * m.transfer_warm;
        assert_eq!(m.path_gas(&hops), Some(expected));
        assert_eq!(
            m.legs_gas(&hops),
            Some(legs),
            "the leg sum is path_gas without the settlement terms"
        );
    }

    /// An unmodeled family makes the whole path unpriced: no floor is better
    /// than an invented one.
    #[test]
    fn an_unmodeled_hop_leaves_the_path_unpriced() {
        let m = MEASURED_GAS_FLOOR;
        assert_eq!(m.leg_gas(HopGasProfile::Unmodeled), None);
        assert_eq!(
            m.path_gas(&[HopGasProfile::V2, HopGasProfile::Unmodeled]),
            None
        );
        assert_eq!(
            m.floor_wei(&[HopGasProfile::V2, HopGasProfile::Unmodeled], 10),
            None
        );
        // A result that carried no hop projections at all is unpriced too.
        assert_eq!(path_gas_floor_wei(&SolvePathResult::default(), 10), None);
    }

    /// The floor is DATA: the same path at a higher base fee floors a larger
    /// margin, and the multiplication is the block's own base fee.
    #[test]
    fn the_floor_prices_the_block_base_fee() {
        let m = MEASURED_GAS_FLOOR;
        let hops = [HopGasProfile::V2, HopGasProfile::V2];
        let gas = m.path_gas(&hops).unwrap();
        assert_eq!(
            m.floor_wei(&hops, 0),
            Some(U256::ZERO),
            "a zero base fee has no gas cost to recover"
        );
        assert_eq!(
            m.floor_wei(&hops, 7_000_000_000),
            Some(U256::from(gas) * U256::from(7_000_000_000u64))
        );
        let low = m.floor_wei(&hops, 5_000_000_000).unwrap();
        let high = m.floor_wei(&hops, 20_000_000_000).unwrap();
        assert_eq!(high, low * U256::from(4u64));
    }

    /// The floor is a pure function of its inputs — no engine, no clock.
    #[test]
    fn the_floor_is_pure_over_profiles_and_base_fee() {
        let hops = [HopGasProfile::V3 {
            census: Some(census(4, 9)),
        }];
        let first = MEASURED_GAS_FLOOR.floor_wei(&hops, 12_000_000_000);
        let second = MEASURED_GAS_FLOOR.floor_wei(&hops, 12_000_000_000);
        assert_eq!(first, second);
    }

    /// A bounded-tail census is a LOWER bound on travel, so the floor
    /// under-predicts by construction — it takes the counts as reported.
    #[test]
    fn a_bounded_tail_census_is_taken_as_the_lower_bound() {
        let m = MEASURED_GAS_FLOOR;
        let open = HopGasProfile::V3 {
            census: Some(SwapWalkCensus {
                initialized_ticks_crossed: 2,
                bitmap_words_walked: 5,
                bounded_tail: false,
            }),
        };
        let bounded = HopGasProfile::V3 {
            census: Some(SwapWalkCensus {
                initialized_ticks_crossed: 2,
                bitmap_words_walked: 5,
                bounded_tail: true,
            }),
        };
        assert_eq!(m.leg_gas(open), m.leg_gas(bounded));
    }
}
