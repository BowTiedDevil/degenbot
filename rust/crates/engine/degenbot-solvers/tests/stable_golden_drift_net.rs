#![expect(clippy::expect_used)]

//! Drift net for the shared stable-family golden-section template.
//!
//! The four stable-family solve arms (Solidly, Balancer weighted, Balancer
//! stable, Curve stableswap) each run the same search: a V2-equivalent
//! Möbius precheck, a ±5x bracket around the model optimum, a 25-iteration
//! phi-approximated golden loop, and a ±3 integer verification. When that
//! search is extracted into one shared helper it must reproduce the probe
//! sequence bit-for-bit, so every arm is pinned here against a synthetic
//! corpus: the exact `(optimal_input, profit)` the arm returns per path,
//! captured from the pre-extraction code and asserted byte-identical after.
//! Any drift — even 1 wei — is a defect.
//!
//! The captured mixed corpora (`heavy_mixed_solve_captures`,
//! `live_gatebursts_mixed`) carry no Solidly/Balancer/Curve legs, so this
//! file is the drift net for these four arms.

use alloy::primitives::U256;
use degenbot_math::balancer::constants::PowVersion;
use degenbot_math::curve::stableswap::{DVariant, YVariant};
use degenbot_math::v2::IntHopState;
use degenbot_solvers::mixed::{
    solve_path, BalancerStableHopState, BalancerWeightedHopState, CurveStableswapHopState,
    ResolvedHop, ResolvedMixedPath, SolidlyHopState,
};
use degenbot_solvers::profit_envelope::GateDeps;
use degenbot_uniswap::dex_identity::DexVariant;

/// 18-decimal scale: the token decimals scale for Solidly hops and the
/// precision/rate unit for the Curve hop.
const ONE_E18: u128 = 1_000_000_000_000_000_000;

/// One corpus entry: the path plus a human-readable tag quoted in failure
/// messages so a regression names the shape that broke.
struct Case {
    tag: String,
    path: ResolvedMixedPath,
}

fn path(hops: Vec<ResolvedHop>) -> ResolvedMixedPath {
    let nonces = vec![1u64; hops.len()];
    ResolvedMixedPath {
        hops,
        valid: true,
        state_nonces: nonces,
        max_update_block: 0,
    }
}

/// A constant-product hop: reserves are already oriented (in, out).
fn v2_hop(reserve_in: u128, reserve_out: u128, gamma_numer: u64, fee_denom: u64) -> ResolvedHop {
    ResolvedHop::V2 {
        state: IntHopState::new(
            U256::from(reserve_in),
            U256::from(reserve_out),
            gamma_numer,
            fee_denom,
        ),
    }
}

/// A Solidly/Aerodrome stable hop. `scale_*` are the token decimal scales
/// (`10^decimals`); `token_in` is the leaf's un-oriented direction flag.
fn solidly_hop(
    reserves_0: u128,
    reserves_1: u128,
    scale_0: u128,
    scale_1: u128,
    token_in: u8,
    fee_numer: u64,
    fee_denom: u64,
) -> ResolvedHop {
    ResolvedHop::SolidlyStable {
        state: SolidlyHopState {
            reserves_0: U256::from(reserves_0),
            reserves_1: U256::from(reserves_1),
            decimals_0: U256::from(scale_0),
            decimals_1: U256::from(scale_1),
            token_in,
            fee_numer: U256::from(fee_numer),
            fee_denom: U256::from(fee_denom),
            stable: true,
            variant: DexVariant::AerodromeV2Stable,
        },
    }
}

/// The smallest native reserve whose scaled value clears the Solidly leaf's
/// faithful-arithmetic floor, so the corpus measures the solver rather than
/// integer `get_y` noise.
fn faithful_base(scale: u128) -> u128 {
    let scaled_floor = 4u128 << 60;
    (scaled_floor.saturating_mul(scale) / ONE_E18).max(2)
}

/// A balanced 50/50 Balancer weighted hop. `reverse` traverses it output→input;
/// equal balances make the direction numerically irrelevant.
fn weighted_hop(base: u128, swap_fee: u128, reverse: bool) -> ResolvedHop {
    let half = U256::from(ONE_E18) / U256::from(2u64);
    let _ = reverse;
    ResolvedHop::BalancerWeighted {
        state: BalancerWeightedHopState {
            balance_in: U256::from(base),
            balance_out: U256::from(base),
            weight_in: half,
            weight_out: half,
            swap_fee: U256::from(swap_fee),
            pow_version: PowVersion::V2,
            scaling_factor_in: U256::from(1u64),
            scaling_factor_out: U256::from(1u64),
        },
    }
}

/// A balanced Balancer stable hop with its invariant pre-computed at the
/// resolved-state convention.
fn stable_hop(base: u128, swap_fee: u128, reverse: bool) -> ResolvedHop {
    let balances = vec![U256::from(base), U256::from(base)];
    let amp = U256::from(200_000u64);
    let invariant =
        degenbot_math::balancer::stable_math::calculate_invariant_deployed(amp, &balances, true)
            .expect("balanced positive balances converge");
    ResolvedHop::BalancerStable {
        state: BalancerStableHopState {
            amp,
            balances,
            token_index_in: usize::from(reverse),
            token_index_out: usize::from(!reverse),
            invariant,
            swap_fee: U256::from(swap_fee),
            scaling_factor_in: U256::from(1u64),
            scaling_factor_out: U256::from(1u64),
        },
    }
}

/// A balanced two-coin Curve stableswap hop (identity rates, standard
/// variants).
fn curve_hop(base: u128, fee: u128, reverse: bool) -> ResolvedHop {
    let precision = U256::from(ONE_E18);
    ResolvedHop::CurveStableswap {
        state: CurveStableswapHopState {
            amp: U256::from(1_000u64),
            a_precision: U256::from(100u64),
            xp: vec![U256::from(base), U256::from(base)],
            token_index_in: usize::from(reverse),
            token_index_out: usize::from(!reverse),
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

/// The pinned golden snapshots: one entry per corpus path, `None` when the arm
/// returns no result. Captured from the pre-extraction arms.
const SOLIDLY_EXPECTED: &[Option<(u128, u128)>] = &[
    Some((8362, 17)),
    Some((8336, 17)),
    Some((99_756, 2168)),
    Some((98_045, 2161)),
    Some((210_889, 9664)),
    Some((210_247, 9633)),
    Some((13_973, 41)),
    Some((12_374, 40)),
    Some((103_842, 2374)),
    Some((103_098, 2371)),
    Some((214_291, 10_091)),
    Some((214_066, 10_079)),
    Some((9_151_808_400_264_080, 18_052_874_650_956)),
    Some((9_124_337_589_652_204, 17_998_715_747_367)),
    Some((100_262_186_149_207_220, 2_168_294_551_014_954)),
    Some((99_957_509_334_501_465, 2_161_744_189_689_375)),
    Some((211_397_125_329_384_375, 9_663_448_719_038_813)),
    Some((210_686_926_241_788_160, 9_632_490_294_034_219)),
    Some((13_784_351_250_696_692, 41_037_336_960_061)),
    Some((13_770_532_517_292_620, 40_996_297_430_169)),
    Some((104_796_857_622_656_377, 2_373_767_140_600_914)),
    Some((104_687_237_949_363_407, 2_371_335_976_520_272)),
    Some((215_796_112_030_083_982, 10_091_500_070_090_260)),
    Some((215_496_745_625_485_920, 10_079_201_387_695_375)),
];

const WEIGHTED_EXPECTED: &[Option<(u128, u128)>] = &[
    Some((990_749_347_312_216, 1_954_360_072_910)),
    Some((985_827_614_056_260, 1_944_651_422_592)),
    Some((10_859_266_612_684_712, 234_789_128_350_231)),
    Some((10_594_794_648_133_166, 229_070_957_478_767)),
    Some((22_934_212_507_455_891, 1_047_236_224_788_676)),
    Some((21_843_669_755_992_470, 997_439_185_817_230)),
    Some((1_492_263_706_016_715, 4_442_612_912_430)),
    Some((1_486_333_260_127_355, 4_424_957_403_301)),
    Some((11_350_897_626_630_913, 257_044_156_468_027)),
    Some((11_085_294_644_186_128, 251_029_504_867_457)),
    Some((23_413_750_432_845_156, 1_093_677_534_956_870)),
    Some((22_321_718_767_612_274, 1_042_667_744_656_411)),
    Some((3_962_997_389_248_873, 7_817_440_291_643)),
    Some((3_943_310_456_225_061, 7_778_605_690_375)),
    Some((43_437_066_450_738_858, 939_156_513_400_931)),
    Some((42_379_178_592_532_672, 916_283_829_915_068)),
    Some((91_736_850_029_823_571, 4_188_944_899_154_708)),
    Some((87_374_679_023_969_892, 3_989_756_743_268_924)),
    Some((5_969_054_824_066_874, 17_770_451_649_724)),
    Some((5_945_333_040_509_433, 17_699_829_613_207)),
    Some((45_403_590_506_523_664, 1_028_176_625_872_108)),
    Some((44_341_178_576_744_520, 1_004_118_019_469_832)),
    Some((93_655_001_731_380_623, 4_374_710_139_827_485)),
    Some((89_286_875_070_449_116, 4_170_670_978_625_644)),
];

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
