#![expect(clippy::expect_used)]

//! Drift net for the shared stable-family golden-section template.
//!
//! The four stable-family solve arms (Solidly, Balancer weighted, Balancer
//! stable, Curve stableswap) each run the same search: a V2-equivalent Möbius
//! bracket hint, a ±5x bracket around the model optimum, a phi-approximated
//! golden loop (25 iterations when anchored, width-sized when unanchored), and
//! one evaluation of the loop's closing midpoint — the returned input, with no
//! integer re-scan around it. Every arm is pinned here against a synthetic
//! corpus: the exact `(optimal_input, profit)` the arm returns per path. Any
//! drift — even 1 wei — is a defect.
//!
//! The corpora are synthetic because the captured mixed ones
//! (`heavy_mixed_solve_captures`, `live_gatebursts_mixed`) carry no
//! Solidly/Balancer/Curve legs: this file is the drift net for those four arms.
//!
//! # Re-pin provenance
//!
//! This file's protocol for a deliberate, non-behavior-identical change is to
//! pin the intended delta explicitly instead of the pre-change answers. This
//! re-pin records the deletion of the ±3-candidate integer sweep the template
//! ran after the loop, which returned the best of the seven candidates
//! straddling the midpoint. The midpoint alone captures no more than the leaf's
//! floor-division jog: measured against a swept-best reference over 1230 paths
//! (these corpora plus thin-margin grids), ≤ 1 wei for Solidly and Curve, ≤ 4
//! wei for Balancer weighted at 4e18 balances, and ≤ 1014 wei for a
//! 2:1-imbalanced Balancer-stable hop at 1e18 balances — the stable leaf's
//! floor jitter at its largest observed scale, ~12 orders of magnitude below gas
//! and never near an executable profit signal. Expected move pattern: the
//! reported input lands where the loop settled, and each path's profit may drop
//! by at most its family's jog. Any larger move means the loop, not the sweep,
//! moved.

pub mod common;

use alloy::primitives::U256;
use common::{
    curve_hop, faithful_base, path, solidly_hop, stable_hop, v2_hop, weighted_hop, ONE_E18,
};
use degenbot_math::curve::stableswap::{DVariant, YVariant};
use degenbot_pools::solve::gate::GateDeps;
use degenbot_pools::solve::{
    solve_path, BalancerStableHopState, BalancerWeightedHopState, CurveStableswapHopState,
    ResolvedHop, ResolvedMixedPath,
};

/// One corpus entry: the path plus a human-readable tag quoted in failure
/// messages so a regression names the shape that broke.
struct Case {
    tag: String,
    path: ResolvedMixedPath,
}

/// The Solidly corpus: two decimal scales x two Solidly fees x three
/// imbalances x two shapes (Solidly first, Solidly last) = 24 paths.
fn solidly_corpus() -> Vec<Case> {
    let scales = [1_000_000u128, ONE_E18];
    let fees = [(3u64, 1_000u64), (1, 1_000)];
    let imbalances = [101u128, 105, 110];
    let mut out = Vec::new();
    for &scale in &scales {
        let base = faithful_base(scale);
        for &(sfn, sfd) in &fees {
            for &b in &imbalances {
                let heavier = base.saturating_mul(b) / 100;
                out.push(Case {
                    tag: format!("solidly-first scale={scale} fee={sfn}/{sfd} imb={b}"),
                    path: path(vec![
                        solidly_hop(base, base, scale, scale, 0, sfn, sfd),
                        v2_hop(base, heavier, 997, 1_000),
                    ]),
                });
                out.push(Case {
                    tag: format!("solidly-last scale={scale} fee={sfn}/{sfd} imb={b}"),
                    path: path(vec![
                        v2_hop(base, heavier, 997, 1_000),
                        solidly_hop(base, base, scale, scale, 1, sfn, sfd),
                    ]),
                });
            }
        }
    }
    out
}

/// The Balancer weighted corpus: two bases x two swap fees x three imbalances
/// x two shapes = 24 paths.
fn weighted_corpus() -> Vec<Case> {
    let bases = [ONE_E18, 4 * ONE_E18];
    let fees = [3_000_000_000_000_000u128, 1_000_000_000_000_000];
    let imbalances = [101u128, 105, 110];
    let mut out = Vec::new();
    for &base in &bases {
        for &fee in &fees {
            for &b in &imbalances {
                let heavier = base.saturating_mul(b) / 100;
                out.push(Case {
                    tag: format!("weighted-first base={base} fee={fee} imb={b}"),
                    path: path(vec![
                        weighted_hop(base, fee, false),
                        v2_hop(base, heavier, 997, 1_000),
                    ]),
                });
                out.push(Case {
                    tag: format!("weighted-last base={base} fee={fee} imb={b}"),
                    path: path(vec![
                        v2_hop(base, heavier, 997, 1_000),
                        weighted_hop(base, fee, true),
                    ]),
                });
            }
        }
    }
    out
}

/// The Balancer stable corpus: two bases x two swap fees x three imbalances x
/// two shapes = 24 paths.
fn stable_corpus() -> Vec<Case> {
    let bases = [ONE_E18, 4 * ONE_E18];
    let fees = [10_000_000_000_000u128, 1_000_000_000_000_000];
    let imbalances = [101u128, 105, 110];
    let mut out = Vec::new();
    for &base in &bases {
        for &fee in &fees {
            for &b in &imbalances {
                let heavier = base.saturating_mul(b) / 100;
                out.push(Case {
                    tag: format!("stable-first base={base} fee={fee} imb={b}"),
                    path: path(vec![
                        stable_hop(base, fee, false),
                        v2_hop(base, heavier, 997, 1_000),
                    ]),
                });
                out.push(Case {
                    tag: format!("stable-last base={base} fee={fee} imb={b}"),
                    path: path(vec![
                        v2_hop(base, heavier, 997, 1_000),
                        stable_hop(base, fee, true),
                    ]),
                });
            }
        }
    }
    out
}

/// The Curve corpus: two bases x two fees x three imbalances x two shapes = 24
/// paths.
fn curve_corpus() -> Vec<Case> {
    let bases = [ONE_E18, 4 * ONE_E18];
    let fees = [4_000_000u128, 1_000_000];
    let imbalances = [101u128, 105, 110];
    let mut out = Vec::new();
    for &base in &bases {
        for &fee in &fees {
            for &b in &imbalances {
                let heavier = base.saturating_mul(b) / 100;
                out.push(Case {
                    tag: format!("curve-first base={base} fee={fee} imb={b}"),
                    path: path(vec![
                        curve_hop(base, fee, false),
                        v2_hop(base, heavier, 997, 1_000),
                    ]),
                });
                out.push(Case {
                    tag: format!("curve-last base={base} fee={fee} imb={b}"),
                    path: path(vec![
                        v2_hop(base, heavier, 997, 1_000),
                        curve_hop(base, fee, true),
                    ]),
                });
            }
        }
    }
    out
}

/// The pinned golden snapshots for the Solidly arm: the arm's
/// `(optimal_input, profit)` per corpus path, `None` when it returns no
/// result.
const SOLIDLY_EXPECTED: &[Option<(u128, u128)>] = &[
    Some((8361, 16)),
    Some((8335, 16)),
    Some((99_755, 2167)),
    Some((98_044, 2160)),
    Some((210_888, 9663)),
    Some((210_247, 9633)),
    Some((13_972, 40)),
    Some((12_373, 39)),
    Some((103_841, 2373)),
    Some((103_097, 2370)),
    Some((214_291, 10_091)),
    Some((214_065, 10_078)),
    Some((9_151_808_400_264_083, 18_052_874_650_956)),
    Some((9_124_337_589_652_207, 17_998_715_747_367)),
    Some((100_262_186_149_207_222, 2_168_294_551_014_954)),
    Some((99_957_509_334_501_468, 2_161_744_189_689_375)),
    Some((211_397_125_329_384_378, 9_663_448_719_038_813)),
    Some((210_686_926_241_788_163, 9_632_490_294_034_219)),
    Some((13_784_351_250_696_695, 41_037_336_960_061)),
    Some((13_770_532_517_292_623, 40_996_297_430_169)),
    Some((104_796_857_622_656_380, 2_373_767_140_600_914)),
    Some((104_687_237_949_363_410, 2_371_335_976_520_272)),
    Some((215_796_112_030_083_984, 10_091_500_070_090_259)),
    Some((215_496_745_625_485_923, 10_079_201_387_695_375)),
];

/// The pinned golden snapshots for the Balancer weighted arm: the arm's
/// `(optimal_input, profit)` per corpus path, `None` when it returns no
/// result.
const WEIGHTED_EXPECTED: &[Option<(u128, u128)>] = &[
    Some((990_749_347_312_219, 1_954_360_072_910)),
    Some((985_827_614_056_263, 1_944_651_422_592)),
    Some((10_859_266_612_684_715, 234_789_128_350_231)),
    Some((10_594_794_648_133_169, 229_070_957_478_767)),
    Some((22_934_212_507_455_894, 1_047_236_224_788_676)),
    Some((21_843_669_755_992_473, 997_439_185_817_230)),
    Some((1_492_263_706_016_718, 4_442_612_912_430)),
    Some((1_486_333_260_127_358, 4_424_957_403_301)),
    Some((11_350_897_626_630_916, 257_044_156_468_027)),
    Some((11_085_294_644_186_131, 251_029_504_867_457)),
    Some((23_413_750_432_845_157, 1_093_677_534_956_870)),
    Some((22_321_718_767_612_277, 1_042_667_744_656_410)),
    Some((3_962_997_389_248_876, 7_817_440_291_640)),
    Some((3_943_310_456_225_058, 7_778_605_690_374)),
    Some((43_437_066_450_738_860, 939_156_513_400_929)),
    Some((42_379_178_592_532_674, 916_283_829_915_066)),
    Some((91_736_850_029_823_573, 4_188_944_899_154_706)),
    Some((87_374_679_023_969_893, 3_989_756_743_268_923)),
    Some((5_969_054_824_066_875, 17_770_451_649_723)),
    Some((5_945_333_040_509_433, 17_699_829_613_207)),
    Some((45_403_590_506_523_666, 1_028_176_625_872_106)),
    Some((44_341_178_576_744_523, 1_004_118_019_469_829)),
    Some((93_655_001_731_380_624, 4_374_710_139_827_484)),
    Some((89_286_875_070_449_112, 4_170_670_978_625_640)),
];

/// The pinned golden snapshots for the Balancer stable arm: the arm's
/// `(optimal_input, profit)` per corpus path, `None` when it returns no
/// result.
const STABLE_EXPECTED: &[Option<(u128, u128)>] = &[
    Some((3_467_115_877_927_060, 12_044_527_480_637)),
    Some((3_466_961_885_313_259, 12_043_990_805_362)),
    Some((23_106_217_732_623_067, 534_951_517_924_796)),
    Some((23_100_684_970_108_990, 534_821_649_274_428)),
    Some((47_135_627_381_325_447, 2_226_199_815_291_539)),
    Some((47_112_265_888_738_140, 2_225_104_382_359_182)),
    Some((2_974_213_894_443_477, 8_854_409_041_009)),
    Some((2_971_132_388_840_124, 8_845_292_733_609)),
    Some((22_623_020_750_132_935, 512_304_026_855_671)),
    Some((22_595_292_374_800_826, 511_675_234_118_257)),
    Some((46_664_341_015_805_938, 2_179_745_390_833_363)),
    Some((46_595_258_855_222_352, 2_176_527_797_317_921)),
    Some((13_868_463_511_708_242, 48_178_109_923_553)),
    Some((13_867_847_541_253_043, 48_175_963_224_451)),
    Some((92_424_870_930_492_267, 2_139_806_071_701_231)),
    Some((92_402_739_880_435_962, 2_139_286_597_097_715)),
    Some((188_542_509_525_301_785, 8_904_799_261_169_297)),
    Some((188_449_063_554_952_561, 8_900_417_529_436_737)),
    Some((11_896_750_024_904_740, 35_417_636_167_941)),
    Some((11_884_529_555_360_502, 35_381_170_935_442)),
    Some((90_492_083_000_531_746, 2_049_216_107_425_745)),
    Some((90_381_169_499_203_309, 2_046_700_936_475_073)),
    Some((186_657_364_063_223_748, 8_718_981_563_334_503)),
    Some((186_381_035_420_889_417, 8_706_111_189_274_829)),
];

/// The pinned golden snapshots for the Curve arm: the arm's
/// `(optimal_input, profit)` per corpus path, `None` when it returns no
/// result.
const CURVE_EXPECTED: &[Option<(u128, u128)>] = &[
    Some((3_014_439_580_318_611, 9_882_023_458_030)),
    Some((3_011_577_348_966_948, 9_872_650_705_390)),
    Some((21_104_038_481_748_424, 484_398_698_166_047)),
    Some((21_013_927_610_940_452, 482_331_546_406_552)),
    Some((43_229_555_233_415_261, 2_033_143_493_427_019)),
    Some((42_865_553_892_645_076, 2_016_059_480_420_860)),
    Some((3_151_988_469_507_736, 10_807_479_767_247)),
    Some((3_149_873_292_543_219, 10_800_198_822_282)),
    Some((21_239_206_511_896_989, 490_764_016_129_577)),
    Some((21_154_471_180_318_743, 488_804_074_937_565)),
    Some((43_362_060_378_258_292, 2_046_186_850_610_620)),
    Some((43_008_756_266_569_141, 2_029_550_824_236_458)),
    Some((12_057_758_321_274_442, 39_528_093_832_122)),
    Some((12_046_309_395_867_795, 39_490_602_821_564)),
    Some((84_416_153_926_993_699, 1_937_594_792_664_191)),
    Some((84_055_710_443_761_818, 1_929_326_185_626_208)),
    Some((172_918_220_933_661_041, 8_132_573_973_708_076)),
    Some((171_462_215_570_580_305, 8_064_237_921_683_440)),
    Some((12_607_953_878_030_951, 43_229_919_068_991)),
    Some((12_599_493_170_172_886, 43_200_795_289_130)),
    Some((84_956_826_047_587_960, 1_963_056_064_518_312)),
    Some((84_617_884_721_274_971, 1_955_216_299_750_262)),
    Some((173_448_241_513_033_176, 8_184_747_402_442_483)),
    Some((172_035_025_066_276_564, 8_118_203_296_945_833)),
];

fn assert_arm(label: &str, corpus: &[Case], expected: &[Option<(u128, u128)>]) {
    assert_eq!(
        corpus.len(),
        expected.len(),
        "{label}: corpus size drifted from the pinned snapshot"
    );
    for (i, case) in corpus.iter().enumerate() {
        let got = solve_path(&case.path, &GateDeps::offline())
            .result
            .map(|r| (r.optimal_input, r.profit));
        let want = expected[i].map(|(a, b)| (U256::from(a), U256::from(b)));
        assert_eq!(got, want, "{label}: path {i} ({}) drifted", case.tag);
    }
}

#[test]
fn solidly_arm_matches_golden_snapshot() {
    assert_arm("solidly", &solidly_corpus(), SOLIDLY_EXPECTED);
}

#[test]
fn weighted_arm_matches_golden_snapshot() {
    assert_arm("weighted", &weighted_corpus(), WEIGHTED_EXPECTED);
}

#[test]
fn stable_arm_matches_golden_snapshot() {
    assert_arm("stable", &stable_corpus(), STABLE_EXPECTED);
}

#[test]
fn curve_arm_matches_golden_snapshot() {
    assert_arm("curve", &curve_corpus(), CURVE_EXPECTED);
}

// ---------------------------------------------------------------------------
// CP-proxy precheck regression
// ---------------------------------------------------------------------------

/// An 80/20 Balancer weighted hop (the shared `weighted_hop` builder is 50/50
/// only). With the input token at weight 0.8 the true entry marginal rate is
/// 4x the constant-product proxy rate (`w_in/w_out` = 4), which is exactly the
/// divergence the precheck's false rejection rode.
fn weighted_8020_hop(base: u128, swap_fee: u128) -> ResolvedHop {
    let eight = U256::from(ONE_E18) * U256::from(8u64) / U256::from(10u64);
    let two = U256::from(ONE_E18) * U256::from(2u64) / U256::from(10u64);
    ResolvedHop::BalancerWeighted {
        state: BalancerWeightedHopState {
            balance_in: U256::from(base),
            balance_out: U256::from(base),
            weight_in: eight,
            weight_out: two,
            swap_fee: U256::from(swap_fee),
            pow_version: degenbot_math::balancer::constants::PowVersion::V2,
            scaling_factor_in: U256::from(1u64),
            scaling_factor_out: U256::from(1u64),
        },
    }
}

/// Regression for the precheck's false rejection: the shared golden-section
/// template's V2-equivalent Möbius profitability precheck treats every hop as
/// constant-product. On this path the CP-proxy product is
/// `0.997 * 0.997 * 1.005 = 0.99898 < 1`, so the precheck declares the path
/// unprofitable — while the true weighted entry-rate product is
/// `(0.997 * 4) * (0.997 * 1.005) = 4.0`, so the path is profitable at real
/// weighted math. The precheck is a bracket hint; it must not reject. A
/// re-derived instance of the adversarial-review counterexample class.
#[test]
fn weighted_arm_does_not_reject_cp_proxy_unprofitable_but_profitable_path() {
    let base = ONE_E18;
    let fee = 3_000_000_000_000_000u128; // 0.3%
    for shape in ["weighted-first", "weighted-last"] {
        let path = if shape == "weighted-first" {
            path(vec![
                weighted_8020_hop(base, fee),
                v2_hop(base, base + base / 200, 997, 1_000),
            ])
        } else {
            path(vec![
                v2_hop(base, base + base / 200, 997, 1_000),
                weighted_8020_hop(base, fee),
            ])
        };
        let got = solve_path(&path, &GateDeps::offline()).result;
        assert!(
            got.is_some(),
            "{shape}: CP-proxy precheck hard-rejected a profitable weighted path"
        );
        let result = got.expect("presence asserted above");
        assert!(
            !result.profit.is_zero() && !result.optimal_input.is_zero(),
            "{shape}: expected a profitable solve, got {result:?}"
        );
    }
}

/// A 2:1-imbalanced Balancer stable hop (the shared `stable_hop` builder is
/// balanced only). The invariant is pre-computed at the resolved-state
/// convention, as in `stable_hop`.
///
/// # Panics
///
/// Panics if `calculate_invariant_deployed` fails to converge; the positive
/// balances here always converge.
fn stable_imbalanced_hop(balance_in: u128, balance_out: u128, swap_fee: u128) -> ResolvedHop {
    let balances = vec![U256::from(balance_in), U256::from(balance_out)];
    let amp = U256::from(200_000u64);
    let invariant =
        degenbot_math::balancer::stable_math::calculate_invariant_deployed(amp, &balances, true)
            .expect("positive balances converge");
    ResolvedHop::BalancerStable {
        state: BalancerStableHopState {
            amp,
            balances,
            token_index_in: 0,
            token_index_out: 1,
            invariant,
            swap_fee: U256::from(swap_fee),
            scaling_factor_in: U256::from(1u64),
            scaling_factor_out: U256::from(1u64),
        },
    }
}

/// A 2:1-imbalanced two-coin Curve stableswap hop (the shared `curve_hop`
/// builder is balanced only), identity rates, standard variants.
fn curve_imbalanced_hop(balance_in: u128, balance_out: u128, fee: u128) -> ResolvedHop {
    let precision = U256::from(ONE_E18);
    ResolvedHop::CurveStableswap {
        state: CurveStableswapHopState {
            amp: U256::from(1_000u64),
            a_precision: U256::from(100u64),
            xp: vec![U256::from(balance_in), U256::from(balance_out)],
            token_index_in: 0,
            token_index_out: 1,
            n_coins: U256::from(2u64),
            fee: U256::from(fee),
            fee_denom: U256::from(10_000_000_000u64),
            precision,
            rate_multiplier_in: precision,
            rate_multiplier_out: precision,
            y_variant: YVariant::Standard,
            d_variant: DVariant::Standard,
        },
    }
}

/// The stable-family instances of the same false rejection: on a 2:1-imbalanced
/// stable pool the CP proxy's entry rate is `γ · (b_out/b_in) ≈ γ/2`, while
/// the true stableswap entry rate stays near the peg. The V2 hop's reserve
/// imbalance must clear each family's true entry discount: Balancer stable at
/// amp `200_000` sits within 2% of peg (2% imbalance suffices); Curve at A=10
/// and a 2:1 imbalance has a deeper true entry discount — measured: an 8% V2
/// imbalance is still unprofitable, 10% solves (needs 15% for a robust pin).
/// The proxy products are ≈ 0.5-0.7 (< 1, rejected by the old precheck)
/// while the true entry-rate products are > 1 (profitable).
#[test]
fn stable_arms_do_not_reject_cp_proxy_unprofitable_but_profitable_path() {
    let b_in = 2 * ONE_E18;
    let b_out = ONE_E18;
    let stable = path(vec![
        stable_imbalanced_hop(b_in, b_out, 1_000_000_000_000_000),
        v2_hop(ONE_E18, ONE_E18 + ONE_E18 / 50, 997, 1_000),
    ]);
    let curve = path(vec![
        curve_imbalanced_hop(b_in, b_out, 4_000_000),
        v2_hop(ONE_E18, ONE_E18 + ONE_E18 * 15 / 100, 997, 1_000),
    ]);
    for (label, path) in [("balancer-stable", stable), ("curve", curve)] {
        let got = solve_path(&path, &GateDeps::offline()).result;
        assert!(
            got.is_some(),
            "{label}: CP-proxy precheck hard-rejected a profitable path"
        );
        let result = got.expect("presence asserted above");
        assert!(
            !result.profit.is_zero() && !result.optimal_input.is_zero(),
            "{label}: expected a profitable solve, got {result:?}"
        );
    }
}
