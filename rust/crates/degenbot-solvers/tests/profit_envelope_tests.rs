//! Profit-envelope soundness tests (epic SU7MAE task 5N65UE).
//!
//! The bound is load-bearing for skips: every test here exists to catch an
//! envelope that ever dips BELOW a true output curve. The oracle is an
//! independent step-by-step walk built directly on `compute_swap_step_v3`
//! (NOT the solver's own `int_simulate_v3_swap`, so implementation bugs
//! cannot cancel between the two).

#![expect(clippy::doc_markdown, clippy::unwrap_used, clippy::expect_used)]

use std::sync::LazyLock;

use alloy::primitives::{I256, U256, U512};
use degenbot_math::cl::swap_math::compute_swap_step_v3;
use degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal;
use degenbot_math::v2::IntHopState;
use degenbot_pools::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};
use degenbot_solvers::profit_envelope::{path_output_bound_at, path_profit_bound, HopMath};

fn p_at_tick(tick: i32) -> U256 {
    U256::from(get_sqrt_ratio_at_tick_internal(tick).unwrap_or_default())
}

/// One zfo range at entry price `p_entry` spanning [p_lo, p_hi].
fn mk_range(liq: u128, p_entry: U256, p_lo: U256, p_hi: U256) -> IntV3TickRangeHop {
    IntV3TickRangeHop {
        liquidity: liq,
        sqrt_price_x96: p_entry,
        sqrt_price_lower_x96: p_lo,
        sqrt_price_upper_x96: p_hi,
        gamma_numer: 997_000, // 0.3% fee tier
        fee_denom: 1_000_000,
        zero_for_one: true,
        word_boundary_prices: Vec::new(),
    }
}

fn mk_seq(ranges: Vec<IntV3TickRangeHop>) -> IntV3TickRangeSequence {
    IntV3TickRangeSequence {
        ranges,
        truncated: false,
    }
}

/// Independent exact-in walk oracle over the sequence's ranges.
fn ref_walk(seq: &IntV3TickRangeSequence, x: U256) -> Option<U256> {
    let mut remaining = x;
    let mut out = U256::ZERO;
    for r in &seq.ranges {
        if remaining.is_zero() {
            break;
        }
        let exit = if r.zero_for_one {
            r.sqrt_price_lower_x96
        } else {
            r.sqrt_price_upper_x96
        };
        let fee = U256::from(r.fee_denom.saturating_sub(r.gamma_numer));
        let liq = i128::try_from(r.liquidity).ok()?;
        let rem = I256::try_from(remaining).ok()?;
        let step = compute_swap_step_v3(r.sqrt_price_x96, exit, liq, rem, fee).ok()?;
        out = out.checked_add(step.amount_out)?;
        let consumed = step.amount_in.saturating_add(step.fee_amount);
        if consumed >= remaining {
            break;
        }
        remaining -= consumed;
    }
    Some(out)
}

const SWEEP: [u64; 22] = [
    0,
    1,
    2,
    7,
    100,
    1_000,
    10_005,
    100_000,
    1_000_003,
    10_000_000,
    500_000_017,
    1_u64 << 30,
    (1_u64 << 40) + 7,
    (1_u64 << 48) + 13,
    (1_u64 << 54) + 3,
    (1_u64 << 58) + 11,
    (1_u64 << 60) + 5,
    (1_u64 << 62) + 9,
    u64::MAX / 4,
    u64::MAX / 2,
    u64::MAX - 1,
    u64::MAX,
];

fn assert_dominates(seq: &IntV3TickRangeSequence) {
    let view = [Some(HopMath::Cl(seq))];
    for &x in &SWEEP {
        let x = U256::from(x);
        let bound = path_output_bound_at(&view, &x).expect("derives");
        let truth = ref_walk(seq, x).expect("oracle walks");
        assert!(
            bound >= truth,
            "ENVELOPE VIOLATION at x={x}: bound {bound} < true {truth}"
        );
    }
}

#[test]
fn single_range_envelope_dominates_exact_walk() {
    let seq = mk_seq(vec![mk_range(
        10_000_000_000_000u128,
        p_at_tick(0),
        p_at_tick(-600),
        p_at_tick(600),
    )]);
    assert_dominates(&seq);
}

#[test]
fn multi_range_envelope_dominates_exact_walk() {
    // Adjacent descending bands (zfo): entries at 0, -1200, -2400 ticks.
    let seq = mk_seq(vec![
        mk_range(
            8_000_000_000_000u128,
            p_at_tick(0),
            p_at_tick(-1200),
            p_at_tick(0),
        ),
        mk_range(
            9_000_000_000_000u128,
            p_at_tick(-1200),
            p_at_tick(-2400),
            p_at_tick(-1200),
        ),
    ]);
    assert_dominates(&seq);
}

#[test]
fn adversarial_deep_later_ranges_envelope_still_dominates() {
    // The first-piece-extension trap: shallow entry band, DEEP later band.
    // A naive Möbius extension under-estimates past the boundary; the
    // entry-slope envelope must not.
    let seq = mk_seq(vec![
        mk_range(1_000_000u128, p_at_tick(0), p_at_tick(-600), p_at_tick(0)),
        mk_range(
            400_000_000_000_000_000u128,
            p_at_tick(-600),
            p_at_tick(-1800),
            p_at_tick(-600),
        ),
        mk_range(
            900_000_000_000_000_000u128,
            p_at_tick(-1800),
            p_at_tick(-3000),
            p_at_tick(-1800),
        ),
    ]);
    assert_dominates(&seq);
}

#[test]
fn v2_envelope_dominates_true_mobius() {
    let hop = IntHopState::new(
        U256::from(1_000_000_000_000u128),
        U256::from(800_000_000_000u128),
        997_000,
        1_000_000,
    );
    let view = [Some(HopMath::V2(&hop))];
    let (r_in, r_out) = (U512::from(hop.reserve_in), U512::from(hop.reserve_out));
    for &x in &SWEEP {
        let x = U256::from(x);
        let bound = path_output_bound_at(&view, &x).expect("derives");
        // True Möbius: γ·r_out·x / (r_in + γ·x)
        let num = U512::from(x) * U512::from(997_000u64) * r_out;
        let den = r_in * U512::from(1_000_000u64) + U512::from(x) * U512::from(997_000u64);
        let truth = num / den;
        assert!(
            U512::from(bound) >= truth,
            "V2 ENVELOPE VIOLATION at x={x}: {bound} < {truth}"
        );
    }
}

#[test]
fn mixed_v2_cl_path_pointwise_dominance() {
    let v2 = IntHopState::new(
        U256::from(50_000_000_000_000u128),
        U256::from(40_000_000_000_000u128),
        997_000,
        1_000_000,
    );
    let cl = mk_seq(vec![mk_range(
        6_000_000_000_000u128,
        p_at_tick(0),
        p_at_tick(-1200),
        p_at_tick(0),
    )]);
    let views = [
        Some(HopMath::V2(&v2)),
        Some(HopMath::Cl(&cl)),
        Some(HopMath::V2(&v2)),
    ];
    for &x in &SWEEP {
        let x = U256::from(x);
        let bound = path_output_bound_at(&views, &x).expect("derives");
        // Chain the oracles: V2 → CL → V2.
        let y1 = mobius(v2.reserve_in, v2.reserve_out, 997_000, 1_000_000, x);
        let y1_u = narrow512(y1).expect("V2 output within U256");
        let mid = ref_walk(&cl, y1_u).expect("walks");
        let truth = mobius(v2.reserve_in, v2.reserve_out, 997_000, 1_000_000, mid);
        assert!(
            U512::from(bound) >= truth,
            "MIXED ENVELOPE VIOLATION at x={x}: {bound} < {truth}"
        );
    }
}

fn narrow512(v: U512) -> Option<U256> {
    let l = v.as_limbs();
    if l[4] != 0 || l[5] != 0 || l[6] != 0 || l[7] != 0 {
        return None;
    }
    Some(U256::from_limbs([l[0], l[1], l[2], l[3]]))
}

fn mobius(r_in: U256, r_out: U256, gamma_num: u64, fee_den: u64, x: U256) -> U512 {
    let num = U512::from(x) * U512::from(gamma_num) * U512::from(r_out);
    let den = U512::from(r_in) * U512::from(fee_den) + U512::from(x) * U512::from(gamma_num);
    num / den
}

#[test]
fn unsupported_family_poisons_bound() {
    assert!(path_profit_bound(&[None]).is_none());
    assert!(path_output_bound_at(&[None], &U256::from(1u64)).is_none());
}

// ---------------------------------------------------------------------------
// Golden-capture soundness: the bound must dominate the recorded golden
// profit of every heavy-CL capture path.
// ---------------------------------------------------------------------------

static CAPTURES: LazyLock<String> = LazyLock::new(|| {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/heavy_cl_solve_captures.jsonl"),
    )
    .expect("heavy-CL capture fixture present")
});

fn parse_range(v: &serde_json::Value) -> Option<IntV3TickRangeHop> {
    Some(IntV3TickRangeHop {
        liquidity: v.get("liquidity")?.as_str()?.parse().ok()?,
        sqrt_price_x96: v.get("sqrt_price_x96")?.as_str()?.parse().ok()?,
        sqrt_price_lower_x96: v.get("sqrt_price_lower_x96")?.as_str()?.parse().ok()?,
        sqrt_price_upper_x96: v.get("sqrt_price_upper_x96")?.as_str()?.parse().ok()?,
        gamma_numer: v.get("gamma_numer")?.as_u64()?,
        fee_denom: v.get("fee_denom")?.as_u64()?,
        zero_for_one: v.get("zero_for_one")?.as_bool()?,
        word_boundary_prices: Vec::new(),
    })
}

#[test]
fn bound_dominates_golden_profit_on_heavy_cl_captures() {
    let mut n_checked = 0u32;
    for line in CAPTURES.lines().filter(|l| !l.trim().is_empty()) {
        let doc: serde_json::Value = serde_json::from_str(line).expect("valid capture JSONL");
        let hops_v = doc
            .get("hops")
            .and_then(serde_json::Value::as_array)
            .expect("hops array");
        let owned_seqs: Vec<IntV3TickRangeSequence> = hops_v
            .iter()
            .map(|hop| {
                let ra = hop.as_array().expect("hop is array of ranges");
                let ranges: Vec<_> = ra
                    .iter()
                    .map(parse_range)
                    .collect::<Option<Vec<_>>>()
                    .expect("range fields");
                mk_seq(ranges)
            })
            .collect();
        let views: Vec<Option<HopMath<'_>>> =
            owned_seqs.iter().map(|s| Some(HopMath::Cl(s))).collect();
        let golden = doc.get("golden").cloned().unwrap_or_default();
        if golden.is_null() {
            continue;
        }
        let go_in: U256 = golden["optimal_input"]
            .as_str()
            .expect("optimal_input")
            .parse()
            .unwrap();
        let gh = golden["hop_outputs"].as_array().expect("hop_outputs");
        let go_last: U256 = gh
            .last()
            .and_then(|v| v.as_str())
            .expect("hop output str")
            .parse()
            .unwrap();
        let golden_profit = go_last.saturating_sub(go_in);

        let bound = path_profit_bound(&views).expect("all-CL capture derives");
        assert!(
            bound >= golden_profit,
            "BOUND UNDER-CUTS GOLDEN: bound {bound} < profit {golden_profit}"
        );
        n_checked += 1;
    }
    // Most fixture lines record golden=null (regenerated post-revert); only
    // the fully-recorded ones are checkable here. Task N6NBUY regenerates a
    // full golden set for gate A/B.
    assert!(
        n_checked >= 2,
        "expected ≥2 full golden captures, checked {n_checked}"
    );
}
