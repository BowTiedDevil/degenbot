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
//! One deliberate exception to that byte-identical rule: the Solidly and
//! Balancer weighted arms verified their optimum with an ascending
//! `center-3..=center+3` scan, while the Balancer stable and Curve arms walked
//! outward `center±delta` from a moving best. Both orders were measured against
//! each other on the golden-parity matrix and on each arm's corpus here —
//! profits were identical to within one wei, the ascending scan was never worse
//! (and a wei better on a handful of paths), and its worst-case residual matched
//! the walk's — so the four arms were unified on the ascending scan. That moved
//! the stable and Curve arms, whose snapshots are therefore re-pinned to the
//! ascending scan's answers (their input shifts are moves between equal-profit
//! candidates on a flat plateau). The re-pin carries the same measurement
//! provenance as the original capture.
//!
//! The captured mixed corpora (`heavy_mixed_solve_captures`,
//! `live_gatebursts_mixed`) carry no Solidly/Balancer/Curve legs, so this
//! file is the drift net for these four arms.

pub mod common;

use alloy::primitives::U256;
use common::{
    curve_hop, faithful_base, path, solidly_hop, stable_hop, v2_hop, weighted_hop, ONE_E18,
};
use degenbot_solvers::mixed::{solve_path, ResolvedMixedPath};
use degenbot_solvers::profit_envelope::GateDeps;

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

/// The pinned golden snapshots for the Balancer stable arm, re-pinned when the
/// arm moved from the outward `center±delta` walk to the shared ascending scan.
/// Only the integer-verification order changed: the profit delta is 0 wei on 23
/// of 24 paths and +1 wei on path 20 (the ascending scan's answer), and the
/// optimal inputs shift only between equal-profit candidates on the arm's flat
/// plateau. Measured against the walk on this corpus and on the golden-parity
/// matrix; see the module doc.
const STABLE_EXPECTED: &[Option<(u128, u128)>] = &[
    Some((3_467_115_877_927_057, 12_044_527_480_637)),
    Some((3_466_961_885_313_256, 12_043_990_805_362)),
    Some((23_106_217_732_623_064, 534_951_517_924_796)),
    Some((23_100_684_970_108_987, 534_821_649_274_428)),
    Some((47_135_627_381_325_444, 2_226_199_815_291_539)),
    Some((47_112_265_888_738_137, 2_225_104_382_359_182)),
    Some((2_974_213_894_443_474, 8_854_409_041_009)),
    Some((2_971_132_388_840_121, 8_845_292_733_609)),
    Some((22_623_020_750_132_932, 512_304_026_855_671)),
    Some((22_595_292_374_800_823, 511_675_234_118_257)),
    Some((46_664_341_015_805_935, 2_179_745_390_833_363)),
    Some((46_595_258_855_222_349, 2_176_527_797_317_921)),
    Some((13_868_463_511_708_239, 48_178_109_923_553)),
    Some((13_867_847_541_253_040, 48_175_963_224_451)),
    Some((92_424_870_930_492_264, 2_139_806_071_701_231)),
    Some((92_402_739_880_435_959, 2_139_286_597_097_715)),
    Some((188_542_509_525_301_782, 8_904_799_261_169_297)),
    Some((188_449_063_554_952_558, 8_900_417_529_436_737)),
    Some((11_896_750_024_904_737, 35_417_636_167_941)),
    Some((11_884_529_555_360_499, 35_381_170_935_442)),
    Some((90_492_083_000_531_750, 2_049_216_107_425_746)),
    Some((90_381_169_499_203_306, 2_046_700_936_475_073)),
    Some((186_657_364_063_223_745, 8_718_981_563_334_503)),
    Some((186_381_035_420_889_414, 8_706_111_189_274_829)),
];

/// The pinned golden snapshots for the Curve arm, re-pinned when the arm moved
/// from the outward `center±delta` walk to the shared ascending scan. The
/// profit is unchanged on all 24 paths (0 wei delta); only the reported optimal
/// input shifts, between equal-profit candidates on the arm's flat plateau.
/// Measured against the walk on this corpus and on the golden-parity matrix;
/// see the module doc.
const CURVE_EXPECTED: &[Option<(u128, u128)>] = &[
    Some((3_014_439_580_318_608, 9_882_023_458_030)),
    Some((3_011_577_348_966_945, 9_872_650_705_390)),
    Some((21_104_038_481_748_421, 484_398_698_166_047)),
    Some((21_013_927_610_940_449, 482_331_546_406_552)),
    Some((43_229_555_233_415_258, 2_033_143_493_427_019)),
    Some((42_865_553_892_645_073, 2_016_059_480_420_860)),
    Some((3_151_988_469_507_733, 10_807_479_767_247)),
    Some((3_149_873_292_543_216, 10_800_198_822_282)),
    Some((21_239_206_511_896_986, 490_764_016_129_577)),
    Some((21_154_471_180_318_740, 488_804_074_937_565)),
    Some((43_362_060_378_258_289, 2_046_186_850_610_620)),
    Some((43_008_756_266_569_138, 2_029_550_824_236_458)),
    Some((12_057_758_321_274_439, 39_528_093_832_122)),
    Some((12_046_309_395_867_792, 39_490_602_821_564)),
    Some((84_416_153_926_993_696, 1_937_594_792_664_191)),
    Some((84_055_710_443_761_815, 1_929_326_185_626_208)),
    Some((172_918_220_933_661_038, 8_132_573_973_708_076)),
    Some((171_462_215_570_580_302, 8_064_237_921_683_440)),
    Some((12_607_953_878_030_948, 43_229_919_068_991)),
    Some((12_599_493_170_172_883, 43_200_795_289_130)),
    Some((84_956_826_047_587_957, 1_963_056_064_518_312)),
    Some((84_617_884_721_274_968, 1_955_216_299_750_262)),
    Some((173_448_241_513_033_173, 8_184_747_402_442_483)),
    Some((172_035_025_066_276_561, 8_118_203_296_945_833)),
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
