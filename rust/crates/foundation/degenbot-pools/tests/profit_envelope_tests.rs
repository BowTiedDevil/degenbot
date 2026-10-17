//! Profit-envelope soundness tests .
//!
//! The bound is load-bearing for skips: every test here exists to catch an
//! envelope that ever dips BELOW a true output curve. The oracle is an
//! independent step-by-step walk built directly on `compute_swap_step_v3`
//! (NOT the solver's own `simulate_v3_range_swap`, so implementation bugs
//! cannot cancel between the two).

#![expect(
    clippy::doc_markdown,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::sync::LazyLock;

use alloy::primitives::{I256, U256, U512};
use degenbot_math::concentrated_liquidity::swap_math::compute_swap_step_v3;
use degenbot_math::concentrated_liquidity::tick_math::get_sqrt_ratio_at_tick_internal;
use degenbot_math::v2::IntHopState;
use degenbot_pools::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};
use degenbot_pools::solve::profit_envelope::{
    path_output_bound_at, path_profit_bound, ConcentratedLiquidityHop, Envelope, GateDeps,
    GateSkipCause, HopMath,
};
use degenbot_pools::solve::runtime::SolveRuntimeConfig;
use std::borrow::Cow;

/// The gate through its one entry at offline deps, unwrapped to the bound
/// (legacy Option shape) for soundness-only assertions.
fn bound(hops: &[Option<HopMath<'_>>]) -> Option<U256> {
    match path_profit_bound(hops, &GateDeps::offline()) {
        Envelope::Bound(b) => Some(b),
        Envelope::Unsupported(_) => None,
    }
}

/// ConcentratedLiquidity-carrying view helper: sequence + a borrowed table (the carried shape).
fn concentrated_liquidity_carried<'a>(
    seq: &'a IntV3TickRangeSequence,
    table: &'a Vec<degenbot_pools::int_v3_hop::IntTickRangeCrossing>,
) -> HopMath<'a> {
    HopMath::ConcentratedLiquidity(ConcentratedLiquidityHop {
        seq,
        crossings: Cow::Borrowed(table),
    })
}

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
    IntV3TickRangeSequence { ranges }
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
    let view = [Some(HopMath::concentrated_liquidity_derived(seq))];
    for &x in &SWEEP {
        let x = U256::from(x);
        let bound = path_output_bound_at(
            &view,
            &x,
            &degenbot_pools::solve::runtime::SolveRuntimeConfig::default(),
        )
        .expect("derives");
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
        let bound = path_output_bound_at(
            &view,
            &x,
            &degenbot_pools::solve::runtime::SolveRuntimeConfig::default(),
        )
        .expect("derives");
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
fn mixed_v2_concentrated_liquidity_path_pointwise_dominance() {
    let v2 = IntHopState::new(
        U256::from(50_000_000_000_000u128),
        U256::from(40_000_000_000_000u128),
        997_000,
        1_000_000,
    );
    let concentrated_liquidity = mk_seq(vec![mk_range(
        6_000_000_000_000u128,
        p_at_tick(0),
        p_at_tick(-1200),
        p_at_tick(0),
    )]);
    let views = [
        Some(HopMath::V2(&v2)),
        Some(HopMath::concentrated_liquidity_derived(
            &concentrated_liquidity,
        )),
        Some(HopMath::V2(&v2)),
    ];
    for &x in &SWEEP {
        let x = U256::from(x);
        let bound = path_output_bound_at(
            &views,
            &x,
            &degenbot_pools::solve::runtime::SolveRuntimeConfig::default(),
        )
        .expect("derives");
        // Chain the oracles: V2 → concentrated liquidity → V2.
        let y1 = mobius(v2.reserve_in, v2.reserve_out, 997_000, 1_000_000, x);
        let y1_u = narrow512(y1).expect("V2 output within U256");
        let mid = ref_walk(&concentrated_liquidity, y1_u).expect("walks");
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
    // Type-enforced: an unmapped hop is Unsupported(UnmappedHop) — solved
    // unscreened, never skip-eligible.
    assert!(matches!(
        path_profit_bound(&[None], &GateDeps::offline()),
        Envelope::Unsupported(GateSkipCause::UnmappedHop)
    ));
    assert!(path_output_bound_at(
        &[None],
        &U256::from(1u64),
        &degenbot_pools::solve::runtime::SolveRuntimeConfig::default()
    )
    .is_none());
}

#[test]
fn m6776w_overflow_paths_compose_without_poisoning() {
    // Multi-hop paths with 1e24+ reserves previously overflowed I512 during
    // compose, returning None (unsupported). The sound-reduction fix must
    // return Some WITHOUT under-cutting the true curve.
    use degenbot_pools::solve::profit_envelope::{path_output_bound_at, HopMath};
    // 4-hop V2 path with 1e24-scale reserves: coefficient product ~1e96.
    let big = U256::from_limbs([0, 0, 0x0DE0_B6B3_A764_0000u64, 0]); // ~1e24
    let v2 = HopMath::V2(&IntHopState::new(big, big, 997_000, 1_000_000));
    let views = vec![Some(v2); 4];
    // Must return Some (not None = unsupported).
    let bound = bound(&views).expect("reduced compose must not overflow");
    // Soundness: the bound must still dominate the real output at any x.
    let x = U256::from(1_000_000u64);
    let out_bound = path_output_bound_at(
        &views,
        &x,
        &degenbot_pools::solve::runtime::SolveRuntimeConfig::default(),
    )
    .expect("bound at x");
    // True output of 4 identical V2 hops: r_out*(gamma*x/(r_in+gamma*x))^4.
    // True CHAINED output of 4 identical V2 hops (each hop's output feeds
    // the next — NOT the fourth power of a single hop).
    let gamma = U512::from(997_000u64);
    let fee_den = U512::from(1_000_000u64);
    let mut current = U512::from(1_000_000u64);
    for _ in 0..4 {
        let aif = current * gamma / fee_den;
        current = aif * U512::from(big) / (U512::from(big) + aif);
    }
    let out_bound_u512 = U512::from(out_bound);
    assert!(
        out_bound_u512 >= current,
        "reduced bound {out_bound} under-cuts true chained output {current} on 4-hop big-reserve path"
    );
    let _ = bound;
}

#[test]
fn m6776w_balancer_weighted_3hop_no_overflow() {
    // The real prod overflow: 3-hop Balancer weighted with 1e24-scale balances
    // and 1e18 weights. The slope coefficients are ~1e54 per hop; chaining
    // 3 gives ~1e162 which overflows I512 (max ~6.7e153).
    use degenbot_pools::solve::profit_envelope::{path_output_bound_at, HopMath};
    let one = U256::from(1_000_000_000_000_000_000u64); // 1e18
    let half = one / U256::from(2u64);
    let bal = U256::from(1_000_000u64) * one; // 1e24 (upscaled)
    let hop = HopMath::Weighted {
        balance_in: bal,
        balance_out: bal,
        weight_in: half,
        weight_out: half,
        scaling_in: U256::from(1u64),
        scaling_out: U256::from(1u64),
    };
    let views = vec![Some(hop); 3];
    // Must return Some (not None = unsupported — the overflow case).
    let bound = path_output_bound_at(
        &views,
        &U256::from(100u64),
        &degenbot_pools::solve::runtime::SolveRuntimeConfig::default(),
    )
    .expect("reduced 3-hop weighted compose must not overflow");
    // The weighted pool with equal weights/balances is symmetric — output
    // is always <= input (fee-only). Bound must be >= true output (which is
    // less than input after fees). A bound of at least 99 suffices.
    assert!(
        bound >= U256::from(99u64),
        "3-hop weighted bound {bound} should be near input 100 (fee eats ~3%)"
    );
}

#[test]
fn m6776w_none_cause_classifies_each_exit() {
    // The typed verdict + per-cause counters classify WHY the gate returned
    // Unsupported — used to diagnose the prod soak's unsupported rate. The
    // counters now ride the gate's returned value, not a thread-local.
    use degenbot_pools::solve::profit_envelope::path_profit_bound_with_stats;
    // Hop unmapped: a None slot (a hop family the gate doesn't map OR a
    // degenerate hop_state the caller couldn't build).
    let (env, s) = path_profit_bound_with_stats(&[None], &GateDeps::offline());
    assert!(matches!(
        env,
        Envelope::Unsupported(GateSkipCause::UnmappedHop)
    ));
    assert_eq!(s.none_hop_unmapped, 1);
    assert_eq!(s.none_degenerate, 0);
    assert_eq!(s.none_overflow, 0);
    // Degenerate: a HopMath slot whose hops_lines_and_cap rejects
    // (zero reserves — a V2 hop with both reserves 0).
    let v2_zero = degenbot_pools::solve::profit_envelope::HopMath::V2(&IntHopState::new(
        U256::ZERO,
        U256::ZERO,
        0,
        1,
    ));
    let (env, s) = path_profit_bound_with_stats(&[Some(v2_zero)], &GateDeps::offline());
    assert!(matches!(
        env,
        Envelope::Unsupported(GateSkipCause::DegenerateHop)
    ));
    assert_eq!(s.none_hop_unmapped, 0);
    assert_eq!(s.none_degenerate, 1);
    assert_eq!(s.none_overflow, 0);
}

// ---------------------------------------------------------------------------
// Golden-capture soundness: the bound must dominate the recorded golden
// profit of every heavy-concentrated-liquidity capture path.
// ---------------------------------------------------------------------------

static CAPTURES: LazyLock<String> = LazyLock::new(|| {
    degenbot_pools::solve::capture_fixture::read_fixture(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/heavy_concentrated_liquidity_solve_captures.jsonl")
            .as_path(),
    )
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
fn precomputed_crossings_match_self_derived_bound() {
    // BZSOJ7 parity gate: feeding the caller-carried crossing tables must
    // produce the byte-identical bound as deriving them inside the envelope.
    for line in CAPTURES.lines().filter(|l| !l.trim().is_empty()).take(24) {
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
        let derived: Vec<Option<HopMath<'_>>> = owned_seqs
            .iter()
            .map(|s| Some(HopMath::concentrated_liquidity_derived(s)))
            .collect();
        let crossing_owned: Vec<Vec<degenbot_pools::int_v3_hop::IntTickRangeCrossing>> = owned_seqs
            .iter()
            .map(degenbot_pools::int_v3_hop::IntV3TickRangeSequence::crossings)
            .collect();
        let carried: Vec<Option<HopMath<'_>>> = owned_seqs
            .iter()
            .zip(crossing_owned.iter())
            .map(|(s, t)| Some(concentrated_liquidity_carried(s, t)))
            .collect();
        let derived_bound = path_profit_bound(&derived, &GateDeps::offline());
        let carried_bound = path_profit_bound(&carried, &GateDeps::offline());
        assert_eq!(
            derived_bound, carried_bound,
            "caller-carried crossing tables must not change the envelope"
        );
    }
}

#[test]
fn prefix_cache_reuse_is_byte_identical_and_counts_hits() {
    let line = CAPTURES
        .lines()
        .find(|l| !l.trim().is_empty())
        .expect("capture present");
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
    let views: Vec<Option<HopMath<'_>>> = owned_seqs
        .iter()
        .map(|s| Some(HopMath::concentrated_liquidity_derived(s)))
        .collect();

    // Same epoch both calls: populate the (content-keyed) cache, then reuse.
    let store = degenbot_pools::solve::profit_envelope::PrefixCache::new();
    let deps = degenbot_pools::solve::profit_envelope::GateDeps::per_block(
        1,
        &store,
        None,
        None,
        SolveRuntimeConfig::default(),
    );
    let (first, _gs1) =
        degenbot_pools::solve::profit_envelope::path_profit_bound_with_stats(&views, &deps);
    let (second, gs2) =
        degenbot_pools::solve::profit_envelope::path_profit_bound_with_stats(&views, &deps);
    assert_eq!(first, second, "prefix reuse must be byte-identical");
    assert!(
        gs2.prefix_hits > 0,
        "second same-table solve must hit the prefix cache"
    );
    assert!(
        gs2.boundaries_composed < gs2.prefix_hits + 1,
        "prefix reuse should cut composed boundaries"
    );
    // No cache reset exists — a new epoch in GateDeps drops older entries.
    let (dropped, gs3) = degenbot_pools::solve::profit_envelope::path_profit_bound_with_stats(
        &views,
        &GateDeps::per_block(2, &store, None, None, SolveRuntimeConfig::default()),
    );
    assert_eq!(dropped, first, "new-epoch solve must be byte-identical");
    assert_eq!(
        gs3.prefix_hits, 0,
        "entries from an older epoch must not survive"
    );
}

#[test]
fn prefix_cache_cross_domain_reuse_stays_exact_on_these_shapes() {
    // Loop-16 T3: two paths share the identical concentrated-liquidity-prefix crossing tables
    // but differ in the V2 tail's reserve (r_out = the hop cap, so the
    // x-domain differs). Cross-domain reuse is SOUND by construction
    // (every stored line globally dominates the true output; see the
    // cache-key doc in profit_envelope.rs) but may shift the bound's
    // tightness. This sentinel pins that these two shapes reuse
    // value-identically (the observed behavior for tangent-stack
    // prefixes: line takeovers stay below the prefix capacity, so the
    // domain never actually bites).
    let p_entry = p_at_tick(-10);
    let p_lo = p_at_tick(-40);
    let p_hi = p_at_tick(30);
    let seq1 = mk_seq(vec![mk_range(10_u128 << 100, p_entry, p_lo, p_hi)]);
    let p_entry2 = p_at_tick(20);
    let seq2 = mk_seq(vec![mk_range(
        10_u128 << 100,
        p_entry2,
        p_at_tick(-30),
        p_at_tick(60),
    )]);
    let crossings1: Vec<degenbot_pools::int_v3_hop::IntTickRangeCrossing> = seq1.crossings();
    let crossings2: Vec<degenbot_pools::int_v3_hop::IntTickRangeCrossing> = seq2.crossings();
    let small_tail = IntHopState::new(U256::from(1_000_u64), U256::from(40_000_u64), 997, 1000);
    let large_tail = IntHopState::new(
        U256::from(1_000_u64) << 140,
        U256::from(40_000_u64) << 140,
        997,
        1000,
    );
    let small_views = vec![
        Some(concentrated_liquidity_carried(&seq1, &crossings1)),
        Some(concentrated_liquidity_carried(&seq2, &crossings2)),
        Some(HopMath::V2(&small_tail)),
    ];
    let large_views = vec![
        Some(concentrated_liquidity_carried(&seq1, &crossings1)),
        Some(concentrated_liquidity_carried(&seq2, &crossings2)),
        Some(HopMath::V2(&large_tail)),
    ];

    let expected_small = path_profit_bound(&small_views, &GateDeps::offline());
    let expected_large = path_profit_bound(&large_views, &GateDeps::offline());
    // Populate the cache under the SMALL domain first.
    let store = degenbot_pools::solve::profit_envelope::PrefixCache::new();
    let _ = path_profit_bound(
        &small_views,
        &GateDeps::per_block(3, &store, None, None, SolveRuntimeConfig::default()),
    );
    // Now solve the large-domain path with the cache on (same epoch).
    let cached = path_profit_bound(
        &large_views,
        &GateDeps::per_block(3, &store, None, None, SolveRuntimeConfig::default()),
    );
    assert_eq!(
        cached, expected_large,
        "prefix entry composed under a smaller domain leaked into a larger-domain path"
    );
    let _ = expected_small;

    // Ladder topology: takeovers spread across magnitudes, so the small
    // domain's prune genuinely drops lines the large domain needs.
    let mut ranges = Vec::new();
    for i in 0..8i32 {
        ranges.push(mk_range(
            1_000_000_000_u128 << (i.cast_unsigned() * 16),
            p_at_tick(i * 10 - 40),
            p_at_tick(i * 10 - 45),
            p_at_tick(i * 10 + 5),
        ));
    }
    let ladder = mk_seq(ranges);
    let ladder_crossings: Vec<degenbot_pools::int_v3_hop::IntTickRangeCrossing> =
        ladder.crossings();
    let tiny_tail = IntHopState::new(U256::from(100u64), U256::from(200u64), 997, 1000);
    let huge_tail = IntHopState::new(
        U256::from(100u64) << 180,
        U256::from(200u64) << 180,
        997,
        1000,
    );
    let tiny_views = vec![
        Some(concentrated_liquidity_carried(&ladder, &ladder_crossings)),
        Some(HopMath::V2(&tiny_tail)),
    ];
    let huge_views = vec![
        Some(concentrated_liquidity_carried(&ladder, &ladder_crossings)),
        Some(HopMath::V2(&huge_tail)),
    ];
    let expected_tiny = path_profit_bound(&tiny_views, &GateDeps::offline());
    let expected_huge = path_profit_bound(&huge_views, &GateDeps::offline());
    let store = degenbot_pools::solve::profit_envelope::PrefixCache::new();
    let _ = path_profit_bound(
        &tiny_views,
        &GateDeps::per_block(4, &store, None, None, SolveRuntimeConfig::default()),
    );
    let cached_huge = path_profit_bound(
        &huge_views,
        &GateDeps::per_block(4, &store, None, None, SolveRuntimeConfig::default()),
    );
    assert_eq!(
        cached_huge, expected_huge,
        "ladder prefix composed under a tiny domain leaked into a huge-domain path"
    );
    let _ = expected_tiny;
}

#[test]
fn prefix_cache_chains_through_v2_hops() {
    // Loop-16 T3: Möbius-family (V2) hops no longer break the prefix
    // chain — a mixed [V2, concentrated liquidity, concentrated liquidity] path must reuse composed boundaries on
    // re-solve and stay byte-identical to the cacheless solve.
    let v2 = IntHopState::new(
        U256::from(1_000_000_u64) << 96,
        U256::from(1_100_000_u64) << 96,
        997,
        1000,
    );
    let ladder1 = mk_seq(vec![mk_range(
        1e12 as u128,
        p_at_tick(-10),
        p_at_tick(-40),
        p_at_tick(30),
    )]);
    let ladder2 = mk_seq(vec![mk_range(
        1e12 as u128,
        p_at_tick(20),
        p_at_tick(-30),
        p_at_tick(60),
    )]);
    let c1: Vec<degenbot_pools::int_v3_hop::IntTickRangeCrossing> = ladder1.crossings();
    let c2: Vec<degenbot_pools::int_v3_hop::IntTickRangeCrossing> = ladder2.crossings();
    let views = vec![
        Some(HopMath::V2(&v2)),
        Some(concentrated_liquidity_carried(&ladder1, &c1)),
        Some(concentrated_liquidity_carried(&ladder2, &c2)),
    ];
    let expected = path_profit_bound(&views, &GateDeps::offline());
    // C4: the cache is an owner-scoped value this test holds — populate once,
    // then the warm solve must hit DETERMINISTICALLY (no spin; C4 retired the
    // process-global store that made cross-test interference possible).
    let store = degenbot_pools::solve::profit_envelope::PrefixCache::new();
    let cold = path_profit_bound(
        &views,
        &GateDeps::per_block(5, &store, None, None, SolveRuntimeConfig::default()),
    );
    assert_eq!(cold, expected, "cold populate must be byte-identical");
    let (warm, gs) = degenbot_pools::solve::profit_envelope::path_profit_bound_with_stats(
        &views,
        &GateDeps::per_block(5, &store, None, None, SolveRuntimeConfig::default()),
    );
    assert_eq!(warm, expected, "mixed-prefix reuse must be byte-identical");
    assert!(
        gs.prefix_hits > 0,
        "mixed [V2, concentrated liquidity] prefixes must produce cache hits (V2 no longer breaks the chain)"
    );
}

#[test]
fn bound_dominates_golden_profit_on_heavy_concentrated_liquidity_captures() {
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
        let views: Vec<Option<HopMath<'_>>> = owned_seqs
            .iter()
            .map(|s| Some(HopMath::concentrated_liquidity_derived(s)))
            .collect();
        let golden = doc.get("golden").cloned().unwrap_or_default();
        if golden.is_null() {
            continue;
        }
        // Bound-check the first N full goldens only. The restored corpus has
        // 180 profitable heavy paths and each deep envelope derivation is
        // quadratic in its surviving composed line count; checking them all
        // turns the gate test into minutes of per-path composition work.
        if n_checked >= 12 {
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

        let bound = bound(&views).expect("all-concentrated-liquidity capture derives");
        let doc_pid = doc.get("path_id").cloned().unwrap_or_default();
        let doc_block = doc.get("block").cloned().unwrap_or_default();
        if bound < golden_profit {
            for (ih, ys) in gh.iter().enumerate() {
                let so: U256 = ys.as_str().expect("hop output str").parse().unwrap();
                let sb = path_output_bound_at(
                    &views[..=ih],
                    &go_in,
                    &degenbot_pools::solve::runtime::SolveRuntimeConfig::default(),
                )
                .unwrap();
                eprintln!(
                    "hop{ih} truth={so} bound_at={sb} (def {})",
                    sb.saturating_sub(so)
                );
            }
            eprintln!(
                "go_in={go_in} last={go_last} bound={bound} (block {doc_block} path {doc_pid})"
            );
        }
        assert!(
            bound >= golden_profit,
            "BOUND UNDER-CUTS GOLDEN: bound {bound} < profit {golden_profit} (block {doc_block} path {doc_pid})"
        );
        n_checked += 1;
    }
    // Most fixture lines record golden=null (regenerated post-revert); only
    // the fully-recorded ones are checkable here. Regenerating harnesses rebuild a
    // full golden set for gate A/B.
    assert!(
        n_checked >= 2,
        "expected ≥2 full golden captures, checked {n_checked}"
    );
}

// ---------------------------------------------------------------------------
// M6776W degenerate-path golden capture harness tests
// ---------------------------------------------------------------------------

/// In-memory [`degenbot_pools::solve::profit_envelope::DegeneratePathSink`] — the
/// test stand-in for the engine's file-writing adapter. The solver performs no
/// I/O, so a harness collects the serialized JSONL rows in memory and asserts
/// on them directly (no temp file).
#[derive(Debug)]
struct InMemoryCaptureSink {
    max_paths: u64,
    reserved: std::sync::atomic::AtomicU64,
    rows: std::sync::Mutex<Vec<String>>,
}

impl InMemoryCaptureSink {
    fn new(max_paths: u64) -> Self {
        Self {
            max_paths,
            reserved: std::sync::atomic::AtomicU64::new(0),
            rows: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn rows(&self) -> Vec<String> {
        self.rows.lock().expect("capture rows lock").clone()
    }
}

impl degenbot_pools::solve::profit_envelope::DegeneratePathSink for InMemoryCaptureSink {
    fn try_reserve(&self) -> bool {
        self.reserved
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            < self.max_paths
    }

    fn capture(&self, event: &degenbot_pools::solve::profit_envelope::DegeneratePathEvent<'_>) {
        self.rows
            .lock()
            .expect("capture rows lock")
            .push(event.to_jsonl_row());
    }
}

/// The capture harness must serialize a degenerate concentrated liquidity path's full range state
/// to a JSONL row through the caller-supplied sink so it can be replayed
/// offline for fix experimentation. The row is pinned byte-for-byte against the
/// historical schema.
#[test]
fn m6776w_capture_harness_writes_jsonl_for_zero_liq_rejection() {
    // The gate keeps no thread-local counters now; the capture cap is driven
    // by the sink alone.

    // A concentrated liquidity path with a zero-liquidity crossing range (the 40% bucket).
    let good_price = p_at_tick(0);
    let range = mk_range(0, good_price, p_at_tick(-10), p_at_tick(10));
    let seq = mk_seq(vec![range]);
    let views: Vec<Option<HopMath<'_>>> = vec![Some(HopMath::concentrated_liquidity_derived(&seq))];

    // path_profit_bound returns Unsupported(DegenerateHop) and triggers the
    // capture through the sink the harness supplies (the gate reads no env).
    let sink = InMemoryCaptureSink::new(10);
    // The constructor requires the store; the degenerate reject fires before
    // any prefix lookup, so the cache stays empty and the outcome is identical.
    let store = degenbot_pools::solve::profit_envelope::PrefixCache::new();
    let deps = degenbot_pools::solve::profit_envelope::GateDeps::per_block(
        0,
        &store,
        Some(&sink),
        None,
        SolveRuntimeConfig::default(),
    );
    assert!(matches!(
        path_profit_bound(&views, &deps),
        degenbot_pools::solve::profit_envelope::Envelope::Unsupported(
            degenbot_pools::solve::profit_envelope::GateSkipCause::DegenerateHop
        )
    ));

    // The sink must hold exactly one row (one degenerate path -> one row).
    let rows = sink.rows();
    assert_eq!(rows.len(), 1, "one degenerate path must yield one row");
    let row = &rows[0];
    assert!(
        !row.contains('\n'),
        "a JSONL row must not contain interior newlines"
    );

    // Byte-for-byte: the row equals the historical schema, field for field.
    let expected = serde_json::json!({
        "reject_hop": 0,
        "reject_reason": "reject=all_zero_liq",
        "n_hops": 1,
        "hops": [{
            "hop_index": 0,
            "family": "concentrated_liquidity",
            "ranges": [{
                "liquidity": "0",
                "sqrt_price_x96": p_at_tick(0).to_string(),
                "sqrt_price_lower_x96": p_at_tick(-10).to_string(),
                "sqrt_price_upper_x96": p_at_tick(10).to_string(),
                "gamma_numer": 997_000,
                "fee_denom": 1_000_000,
                "zero_for_one": true,
                "word_boundary_prices": [],
            }],
        }],
    });
    assert_eq!(row, &expected.to_string(), "capture row format drifted");

    // And it must parse back as the documented fields.
    let doc: serde_json::Value = serde_json::from_str(row.trim()).expect("valid JSONL line");
    assert_eq!(doc["n_hops"], 1);
    assert_eq!(doc["reject_hop"], 0);
    let reason = doc["reject_reason"].as_str().expect("reject_reason str");
    assert!(
        reason.contains("zero_liq"),
        "reason should classify zero_liq, got: {reason}"
    );
    // The hop's ranges must be serialized with the 8 primitive fields.
    let ranges = doc["hops"][0]["ranges"].as_array().expect("ranges array");
    assert_eq!(ranges.len(), 1);
    assert!(
        ranges[0]["liquidity"].as_str() == Some("0"),
        "zero liquidity must be serialized"
    );
    assert!(
        ranges[0]["gamma_numer"].as_u64() == Some(997_000),
        "gamma_numer must be serialized"
    );
}

/// The envelope must NOT reject a concentrated liquidity hop with an extreme entry price.
/// A pool pushed far from fair value by a misrouted swap IS a real arbitrage
/// opportunity — the extreme price is not evidence of infeasibility, and for
/// zero_for_one it is exactly the recovery direction (price moving DOWN from
/// the misrouted extreme). The gate skips ranges whose I512 coefficients
/// overflow, but keeps the lines that DO fit and lets the solver decide.
#[test]
fn extreme_entry_price_is_not_rejected() {
    // sqrt_price_x96 = 2^130 — far above the old P_ENTRY_LIMIT = 2^128.
    // This represents a pool at an extreme tick (misrouted swap recovery).
    let extreme_price = U256::from(1u128) << 130;
    let range = mk_range(
        1_000_000,
        extreme_price,
        extreme_price / U256::from(2u64),
        extreme_price * U256::from(2u64),
    );
    let seq = mk_seq(vec![range]);
    let views: Vec<Option<HopMath<'_>>> = vec![Some(HopMath::concentrated_liquidity_derived(&seq))];

    // The envelope must succeed (return Some bound), not reject the hop.
    // A single range with real liquidity and an extreme price is a perfectly
    // valid concave piece — the tangent line just has extreme coefficients,
    // but P^2 = 2^260 fits in I512 (max 2^511).
    let bound = bound(&views);
    assert!(
        bound.is_some(),
        "extreme entry price should not be rejected (real arbitrage opportunity)"
    );
}

// -----------------------------------------------------------------
// Cap-tail overflow probe: synthetic concentrated liquidity sequences where the LAST range
// has deep liquidity at an extreme price (or extreme liquidity), to
// confirm the U512-based cap-tail computation accepts these without
// rejecting the whole hop. The old code path
//   let liq = i128::try_from(er.liquidity).ok()?;
//   let step = compute_swap_step_v3(er.sqrt_price_x96, exit, liq,
//                                    I256::MAX, ...).ok()?;
//   let cap = acc_out.checked_add(step.amount_out)?;
// rejected the hop on ANY of these failures — in practice only the i128
// cast fires (for liquidity >= 2^127, type-legal on-chain uint128).
// -----------------------------------------------------------------

/// Wide position from tick 0 to tick 887272 (one_for_zero, extreme high).
#[test]
fn probe_cap_tail_extreme_ofz() {
    let p0 = p_at_tick(0);
    let p_max = p_at_tick(887_272);
    let liq: u128 = 1_000_000_000_000_000;
    let r0 = IntV3TickRangeHop {
        zero_for_one: false,
        ..mk_range(liq, p0, p0, p_at_tick(100))
    };
    let r1 = IntV3TickRangeHop {
        zero_for_one: false,
        ..mk_range(liq, p_at_tick(100), p_at_tick(100), p_max)
    };
    let seq = mk_seq(vec![r0, r1]);
    let views: Vec<Option<HopMath>> = vec![Some(HopMath::concentrated_liquidity_derived(&seq))];
    assert!(
        bound(&views).is_some(),
        "OFZ extreme should not be rejected"
    );
}

/// Wide position from tick 0 to tick -887272 (zero_for_one, extreme low).
#[test]
fn probe_cap_tail_extreme_zfo() {
    let p0 = p_at_tick(0);
    let p_min = p_at_tick(-887_272);
    let liq: u128 = 1_000_000_000_000_000;
    let r0 = mk_range(liq, p0, p_at_tick(-100), p0);
    let r1 = mk_range(liq, p_at_tick(-100), p_min, p_at_tick(-100));
    let seq = mk_seq(vec![r0, r1]);
    let views: Vec<Option<HopMath>> = vec![Some(HopMath::concentrated_liquidity_derived(&seq))];
    assert!(
        bound(&views).is_some(),
        "ZFO extreme should not be rejected"
    );
}

/// u128::MAX liquidity (> i128::MAX — the top bit is set). This is the
/// REAL trigger for `i128::try_from(er.liquidity).ok()?` returning None
/// and rejecting the whole hop. On-chain uint128 CAN hold this value.
/// The pool has deep liquidity at a normal price (tick 0 -> 100).
#[test]
fn probe_cap_tail_u128_max_liquidity_rejects() {
    let p0 = p_at_tick(0);
    let p100 = p_at_tick(100);
    let liq = u128::MAX; // 2^128 - 1 — exceeds i128::MAX

    // Sanity: u128::MAX genuinely doesn't fit in i128.
    assert!(
        i128::try_from(liq).is_err(),
        "u128::MAX must not fit in i128"
    );

    // Single range, normal price, extreme liquidity:
    let r = mk_range(liq, p0, p0, p100);
    let seq = mk_seq(vec![r]);
    let views: Vec<Option<HopMath>> = vec![Some(HopMath::concentrated_liquidity_derived(&seq))];
    // u128::MAX liquidity exceeds i128::MAX, so the old cap-tail's
    // `i128::try_from(er.liquidity).ok()?` rejected the whole hop. The
    // U512-based cap-tail computes the same formula directly, accepting
    // the on-chain uint128 liquidity type without truncation.
    assert!(
        bound(&views).is_some(),
        "u128::MAX liquidity should not be rejected (on-chain uint128 type)"
    );
}

/// Same u128::MAX liquidity but at EXTREME price (tick 887272).
#[test]
fn probe_cap_tail_u128_max_liquidity_extreme_price() {
    let p0 = p_at_tick(0);
    let p_max = p_at_tick(887_272);
    let liq = u128::MAX;
    let r = IntV3TickRangeHop {
        zero_for_one: false,
        ..mk_range(liq, p0, p0, p_max)
    };
    let seq = mk_seq(vec![r]);
    let views: Vec<Option<HopMath>> = vec![Some(HopMath::concentrated_liquidity_derived(&seq))];
    assert!(
        bound(&views).is_some(),
        "u128::MAX liquidity at extreme price should not be rejected"
    );
}

// ===========================================================================
// Solidly/Aerodrome/Camelot STABLE entry-tangent soundness.
//
// The gate's Solidly-stable rise line is the tangent at zero input on the
// invariant's CONCAVE output curve, plus an additive allowance for the leaf's
// integer rounding. The oracle is the REAL leaf
// (`calc_exact_in_stable_solidly` / `calc_exact_in_stable_camelot`), never a
// reimplementation: a bound that ever dips below the leaf would skip a
// profitable path.
// ===========================================================================

use degenbot_math::solidly::{calc_exact_in_stable_camelot, calc_exact_in_stable_solidly};
use proptest::prelude::*;

/// One generated Solidly-stable shape (a randomized-matrix row).
#[derive(Clone, Copy, Debug)]
struct SolidlyShape {
    reserves_0: U256,
    reserves_1: U256,
    decimals_0: U256,
    decimals_1: U256,
    token_in: u8,
    fee_numer: U256,
    fee_denom: U256,
    camelot: bool,
}

impl SolidlyShape {
    fn hop(self) -> HopMath<'static> {
        HopMath::SolidlyStable {
            reserves_0: self.reserves_0,
            reserves_1: self.reserves_1,
            decimals_0: self.decimals_0,
            decimals_1: self.decimals_1,
            token_in: self.token_in,
            fee_numer: self.fee_numer,
            fee_denom: self.fee_denom,
        }
    }

    /// The scaled input-side reserve (`reserve * ONE / decimals`), the leaf's
    /// own `x0` floor.
    fn scaled_in_reserve(self) -> U256 {
        if self.token_in == 0 {
            self.reserves_0
                .saturating_mul(U256::from(1_000_000_000_000_000_000u64))
                / self.decimals_0
        } else {
            self.reserves_1
                .saturating_mul(U256::from(1_000_000_000_000_000_000u64))
                / self.decimals_1
        }
    }

    /// The REAL family leaf — the oracle.
    fn leaf_output(self, x: U256) -> Option<U256> {
        let r = if self.camelot {
            calc_exact_in_stable_camelot(
                x,
                self.token_in,
                self.reserves_0,
                self.reserves_1,
                self.decimals_0,
                self.decimals_1,
                self.fee_numer,
                self.fee_denom,
            )
        } else {
            calc_exact_in_stable_solidly(
                x,
                self.token_in,
                self.reserves_0,
                self.reserves_1,
                self.decimals_0,
                self.decimals_1,
                self.fee_numer,
                self.fee_denom,
            )
        };
        r.ok()
    }

    fn native_out_reserve(self) -> U256 {
        if self.token_in == 0 {
            self.reserves_1
        } else {
            self.reserves_0
        }
    }
}

// ===========================================================================
// Generic point-wise dominance harness.
//
// ONE harness owns the shape strategy, the dense log-grid sweep, the
// point-wise dominance assertion against the REAL family leaf, the counters,
// the non-vacuity asserts, and the telemetry. A family supplies only its shape
// strategy, its hop view, and its leaf oracle.
//
// The old harnesses drew a 400-shape matrix from a hand-rolled LCG inside a
// single `proptest!` case keyed by `any::<u64>()`, so proptest saw only the
// 64-bit seed and a failure could not shrink. The case now draws ONE shape
// through a real `Strategy`, so proptest shrinks a failure to a minimal
// offending shape.
// ===========================================================================

/// The counters a dominance run accumulates across every proptest case. Each
/// is a non-vacuity witness: a strategy that quietly stopped drawing the wide
/// matrices would zero one of them and fail the run's lower-bound asserts.
#[derive(Default)]
struct DominanceCounters {
    /// Shapes whose output reserve was non-zero (a usable matrix row).
    shapes: u32,
    /// Point-wise dominance assertions actually evaluated.
    asserts: u32,
    /// Assertions where the tangent rise was the active (tighter) line.
    rise_active: u32,
    /// Shapes whose rise was dropped (guard fired), leaving the flat cap alone.
    guard_fired: u32,
    /// Probes where the leaf reported more than the pool holds (wrapped).
    leaf_wrapped: u32,
    /// Probes where the family's leaf was undefined.
    leaf_undefined: u32,
    /// Smallest bound-minus-leaf margin seen (telemetry only).
    min_margin: U256,
}

/// One hop family under the generic dominance harness.
trait DominanceFamily {
    /// The generated shape (one randomized-matrix row). `Debug` so a shrunken
    /// counterexample names the minimal offending shape.
    type Shape: std::fmt::Debug;

    /// The family name for telemetry.
    const NAME: &'static str;

    /// The shape strategy — `proptest` shrinks a failure to a minimal draw.
    fn strategy() -> BoxedStrategy<Self::Shape>;

    /// The gate's hop view for a shape.
    fn hop(shape: &Self::Shape) -> HopMath<'_>;

    /// The NATIVE output reserve (the cap the bound approaches).
    fn native_out_reserve(shape: &Self::Shape) -> U256;

    /// The REAL family leaf — the oracle — or `None` where it is undefined.
    fn leaf_output(shape: &Self::Shape, x: U256) -> Option<U256>;

    /// Whether this shape's leaf is undefined for EVERY input (a shape-level
    /// domain restriction, counted before the leaf is called).
    fn shape_leaf_undefined(_shape: &Self::Shape) -> bool {
        false
    }
}

/// `10^exp` as a `U256` (saturating) — the matrices' decade scale.
fn pow10_u256(exp: u32) -> U256 {
    let mut v = U256::ONE;
    for _ in 0..exp {
        v = v.saturating_mul(U256::from(10u64));
    }
    v
}

/// The number of strategy-driven cases per family run. The old hand-rolled
/// harness drew 400 shapes inside each of 24 `proptest!` cases (~9,600 shapes
/// total). One strategy case now draws ONE shape, so the case count is raised
/// to keep the total generated-shape volume — and therefore the counters'
/// non-vacuity guarantees — at least as strong as before.
const DOMINANCE_CASES: u32 = 9_600;

/// The old hand-rolled harness's case count: 24 `proptest!` cases, each
/// asserting per-case floors of 300 shapes / 2,000 assertions / 200
/// rise-active probes. The harness asserts those floors scaled by this count,
/// so the strategy-driven run's coverage breadth is at least as strong as
/// before.
const DOMINANCE_OLD_CASES: u32 = 24;

/// Sweep one shape on the dense log grid and assert the bound dominates the
/// real leaf point-wise. `Err` (via `prop_assert!`) carries the shape into
/// proptest's shrinker.
fn dominance_case<F: DominanceFamily>(
    shape: &F::Shape,
    cfg: &SolveRuntimeConfig,
    c: &mut DominanceCounters,
) -> Result<(), TestCaseError> {
    let out_reserve = F::native_out_reserve(shape);
    if out_reserve.is_zero() {
        return Ok(());
    }
    c.shapes += 1;

    let views = [Some(F::hop(shape))];

    // A shape whose rise was dropped (the curvature test failed or the
    // faithful-arithmetic window guard fired) bounds to the flat cap ALONE, so
    // its unit-input bound is exactly the output reserve.
    let unit_bound = path_output_bound_at(&views, &U256::ONE, cfg)
        .expect("the entry-tangent bound always derives");
    if unit_bound >= out_reserve {
        c.guard_fired += 1;
        prop_assert_eq!(
            unit_bound,
            out_reserve,
            "a cap-only shape must bound to the flat cap"
        );
    }

    // Dense log grid from the reserve cap down ~8 decades.
    let mut x = out_reserve;
    for _ in 0..40 {
        if x.is_zero() {
            break;
        }
        for probe in [x, x.saturating_add(U256::ONE)] {
            if F::shape_leaf_undefined(shape) {
                c.leaf_undefined += 1;
                continue;
            }
            let Some(leaf) = F::leaf_output(shape, probe) else {
                c.leaf_undefined += 1;
                continue;
            };
            // A leaf reporting more than the pool holds has wrapped its own
            // arithmetic; that is not the invariant curve, so no
            // invariant-based bound is defined there.
            if leaf > out_reserve {
                c.leaf_wrapped += 1;
                continue;
            }
            let bound = path_output_bound_at(&views, &probe, cfg)
                .expect("the entry-tangent bound always derives");
            prop_assert!(
                bound >= leaf,
                "{} bound {bound} < leaf {leaf} at x={probe} ({shape:#?})",
                F::NAME
            );
            let margin = bound - leaf;
            if margin < c.min_margin {
                c.min_margin = margin;
            }
            c.asserts += 1;
            if bound < out_reserve {
                c.rise_active += 1;
            }
        }
        x = x.saturating_mul(U256::from(464u64)) / U256::from(1000u64);
    }

    Ok(())
}

/// Run the dominance harness for one family and assert the non-vacuity floors.
///
/// The counters accumulate across the WHOLE strategy-driven run (not per case),
/// so the lower bounds stay the ones the hand-rolled matrix asserted — the same
/// coverage breadth, now with shrinking.
// The harness must surface proptest's shrunken minimal shape AS the test
// failure; a `#[test]` fn has no non-panicking way to report one.
#[expect(clippy::panic)]
fn run_dominance_suite<F: DominanceFamily>() {
    let cfg = SolveRuntimeConfig::default();
    // `TestRunner::run` takes an `Fn` closure, so the run-wide counters live
    // behind a `RefCell` (interior mutability) rather than a captured `&mut`.
    let counters = std::cell::RefCell::new(DominanceCounters {
        min_margin: U256::MAX,
        ..DominanceCounters::default()
    });
    let mut runner = prop::test_runner::TestRunner::new(ProptestConfig {
        cases: DOMINANCE_CASES,
        // The Balancer strategy discards draws whose invariant does not
        // converge; the default global-reject cap (1024) is far too low for a
        // ~9,600-case run.
        max_global_rejects: 1_000_000,
        // No on-disk regression persistence: a strategy-driven harness is
        // reproducible from its seed, and this one must not write into the
        // shared workspace.
        failure_persistence: None,
        ..ProptestConfig::default()
    });

    if let Err(err) = runner.run(&F::strategy(), |shape| {
        dominance_case::<F>(&shape, &cfg, &mut counters.borrow_mut())
    }) {
        match err {
            prop::test_runner::TestError::Fail(reason, shape) => {
                panic!(
                    "{} dominance failed: {reason}\nminimal failing shape: {shape:#?}",
                    F::NAME
                );
            }
            prop::test_runner::TestError::Abort(reason) => {
                panic!("{} dominance run aborted: {reason}", F::NAME);
            }
        }
    }

    let counters = counters.into_inner();

    eprintln!(
        "{} proptest: shapes={} asserts={} rise_active={} guard_fired_cap_only={} \
         leaf_wrapped={} leaf_undefined={} min_margin={}",
        F::NAME,
        counters.shapes,
        counters.asserts,
        counters.rise_active,
        counters.guard_fired,
        counters.leaf_wrapped,
        counters.leaf_undefined,
        counters.min_margin,
    );

    // These quantitative floors are the old harness's PER-CASE floors scaled by
    // its case count (see `DOMINANCE_OLD_CASES`), so the strategy-driven run's
    // coverage breadth is at least as strong as before; `guard_fired > 0`
    // stays the qualitative "the wide matrix reaches the guard at all" witness.
    assert!(
        counters.shapes >= 300 * DOMINANCE_OLD_CASES,
        "matrix too small: {} shapes",
        counters.shapes
    );
    assert!(
        counters.asserts >= 2_000 * DOMINANCE_OLD_CASES,
        "vacuous matrix: {} assertions",
        counters.asserts
    );
    assert!(
        counters.rise_active >= 200 * DOMINANCE_OLD_CASES,
        "the tangent rise was never the active line: {} probes",
        counters.rise_active
    );
    assert!(
        counters.guard_fired > 0,
        "the wide matrix never exercised the guard: {} cap-only shapes",
        counters.guard_fired
    );
}

/// The four decimals pairs the Solidly matrix draws (18/18, 6/18, 18/6, 6/6).
const SOLIDLY_DECIMALS: [(u64, u64); 4] = [
    (1_000_000_000_000_000_000, 1_000_000_000_000_000_000),
    (1_000_000, 1_000_000_000_000_000_000),
    (1_000_000_000_000_000_000, 1_000_000),
    (1_000_000, 1_000_000),
];

/// The three fee pairs the Solidly matrix draws (0 %, 0.03 %, 0.3 %).
const SOLIDLY_FEES: [(u64, u64); 3] = [(0, 1), (3, 10_000), (30, 10_000)];

prop_compose! {
    /// Draw one Solidly matrix row: reserves log-uniform 1e6..1e30, imbalance
    /// ratio up to 1e12 in either direction, the four decimals pairs, the three
    /// fee pairs, `token_in` in {0, 1}, and the Aerodrome/Camelot leaf choice.
    /// The wide ratio cap (12) is what drives the mixed-decimals post-trade
    /// window past the leaf's `calc_d` wrap threshold, exercising the
    /// faithful-arithmetic guard.
    fn solidly_shape_strategy()
        (
            e0 in 6u32..=30,
            m0 in 100u32..=90_099,
            ratio_exp in 0u32..=12,
            up in any::<bool>(),
            m1 in 100u32..=90_099,
            dec_idx in 0usize..4,
            fee_idx in 0usize..3,
            token_in in 0u8..=1,
            camelot in any::<bool>(),
        )
        -> SolidlyShape
    {
        let r0 = pow10_u256(e0) * U256::from(m0) / U256::from(100u64);
        let e1 = if up {
            e0 + ratio_exp
        } else {
            e0.saturating_sub(ratio_exp)
        }
        .clamp(1, 31);
        let r1 = pow10_u256(e1) * U256::from(m1) / U256::from(100u64);
        let (d0, d1) = SOLIDLY_DECIMALS[dec_idx];
        let (fee_numer, fee_denom) = SOLIDLY_FEES[fee_idx];
        SolidlyShape {
            reserves_0: r0,
            reserves_1: r1,
            decimals_0: U256::from(d0),
            decimals_1: U256::from(d1),
            token_in,
            fee_numer: U256::from(fee_numer),
            fee_denom: U256::from(fee_denom),
            camelot,
        }
    }
}

/// The Solidly-stable family under the generic dominance harness.
struct SolidlyFamily;

impl DominanceFamily for SolidlyFamily {
    type Shape = SolidlyShape;
    const NAME: &'static str = "solidly_stable";

    fn strategy() -> BoxedStrategy<SolidlyShape> {
        solidly_shape_strategy().boxed()
    }

    fn hop(shape: &SolidlyShape) -> HopMath<'_> {
        shape.hop()
    }

    fn native_out_reserve(shape: &SolidlyShape) -> U256 {
        shape.native_out_reserve()
    }

    fn leaf_output(shape: &SolidlyShape, x: U256) -> Option<U256> {
        shape.leaf_output(x)
    }

    fn shape_leaf_undefined(shape: &SolidlyShape) -> bool {
        // The Solidly leaf's Newton step divides by `calc_d`, which floors to
        // zero when the scaled input reserve is below `ONE^(2/3)` — the leaf
        // reverts there, so its output is undefined (Camelot guards the same
        // division).
        !shape.camelot && shape.scaled_in_reserve() < U256::from(1_000_000_000_000u64)
    }
}

#[test]
fn solidly_stable_entry_tangent_dominates_real_leaves() {
    run_dominance_suite::<SolidlyFamily>();
}

/// Tightening: on an imbalanced pool the composed bound with the tangent is
/// strictly BELOW the reserve-cap-only bound.
#[test]
fn solidly_stable_tangent_tightens_below_reserve_cap() {
    let cfg = SolveRuntimeConfig::default();
    let shape = SolidlyShape {
        reserves_0: U256::from(1_000_000_000_000_000_000_000_000u128),
        reserves_1: U256::from(10_000_000_000_000_000_000_000u128),
        decimals_0: U256::from(1_000_000_000_000_000_000u64),
        decimals_1: U256::from(1_000_000_000_000_000_000u64),
        token_in: 0,
        fee_numer: U256::from(3u64),
        fee_denom: U256::from(10_000u64),
        camelot: false,
    };
    let out_reserve = shape.native_out_reserve();
    let x = out_reserve / U256::from(2u64);
    let tangent = path_output_bound_at(&[Some(shape.hop())], &x, &cfg).expect("derives");
    let cap_only = path_output_bound_at(
        &[Some(HopMath::ReserveCap {
            reserve_out: out_reserve,
        })],
        &x,
        &cfg,
    )
    .expect("derives");
    assert!(
        tangent < cap_only,
        "tangent {tangent} must be strictly tighter than the reserve cap {cap_only}"
    );
}

/// Before/after skip-count delta on a Solidly-bearing mixed corpus, reported
/// through the existing `GateStats` counters. Neither committed mixed corpus
/// carries a Solidly leg (both are V2/concentrated liquidity only), so the Solidly corpus is
/// synthesized here: healthy pools whose true max profit is far below the
/// floor while their reserve caps sit far above it.
#[test]
fn solidly_stable_tangent_raises_skip_count_on_synthesized_corpus() {
    use degenbot_pools::solve::mixed::solve_path_with_min_profit;
    use degenbot_pools::solve::mixed::{ResolvedHop, ResolvedMixedPath, SolidlyHopState};
    use degenbot_pools::solve::profit_envelope::{path_profit_bound_with_floor, GateDeps};
    use degenbot_uniswap::dex_identity::DexVariant;

    // The committed mixed corpus has no Solidly leg.
    let committed = degenbot_pools::solve::capture_fixture::load_rows(
        degenbot_pools::solve::capture_fixture::fixture_path("heavy_mixed_solve_captures.jsonl"),
    );
    let solidly_rows = committed
        .iter()
        .filter(|row| {
            row.get("hops")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|hops| {
                    hops.iter().any(|h| {
                        h.get("kind")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|k| k.to_ascii_lowercase().contains("solidly"))
                    })
                })
        })
        .count();
    assert_eq!(
        solidly_rows, 0,
        "the committed mixed corpus unexpectedly carries Solidly legs"
    );

    let floor = U256::from(1_000_000_000_000_000_000u64);
    let cfg = SolveRuntimeConfig::default();
    let mut corpus: Vec<ResolvedMixedPath> = Vec::new();
    for i in 0..64u64 {
        // Healthy, slightly imbalanced pools: scaled reserves >= 2^60 and the
        // leaf's `calc_k` product well inside u256.
        let r0 =
            U256::from(1_000_000_000_000_000_000_000_000u128).saturating_mul(U256::from(1 + i % 5));
        let r1 =
            U256::from(100_000_000_000_000_000_000_000u128).saturating_mul(U256::from(1 + i % 3));
        let state = SolidlyHopState {
            reserves_0: r0,
            reserves_1: r1,
            decimals_0: U256::from(1_000_000_000_000_000_000u64),
            decimals_1: U256::from(1_000_000_000_000_000_000u64),
            token_in: u8::from(!i.is_multiple_of(2)),
            fee_numer: U256::from(3u64),
            fee_denom: U256::from(10_000u64),
            stable: true,
            variant: DexVariant::AerodromeV2Stable,
        };
        corpus.push(ResolvedMixedPath {
            hops: vec![ResolvedHop::SolidlyStable { state }],
            valid: true,
            state_nonces: vec![i],
            max_update_block: 0,
        });
    }

    let mut old_skips = 0u64;
    let mut new_skips = 0u64;
    for path in &corpus {
        let state = path
            .hops
            .first()
            .and_then(ResolvedHop::as_solidly_state)
            .expect("solidly hop");
        let out_reserve = if state.token_in == 0 {
            state.reserves_1
        } else {
            state.reserves_0
        };
        // Today's mapping: flat reserve cap only.
        let (old_env, _) = path_profit_bound_with_floor(
            &[Some(HopMath::ReserveCap {
                reserve_out: out_reserve,
            })],
            &GateDeps::offline_with_runtime(cfg),
            floor,
        );
        if matches!(old_env, Envelope::Bound(b) if b <= floor) {
            old_skips += 1;
        }
        // The new mapping, through the real entry.
        new_skips += solve_path_with_min_profit(path, floor, &GateDeps::offline())
            .gate_stats
            .skipped;
    }

    eprintln!(
        "solidly_stable corpus: {} paths, skip count {old_skips} (reserve cap) -> \
         {new_skips} (entry tangent + cap)",
        corpus.len()
    );
    assert!(
        new_skips > old_skips,
        "the entry tangent must raise the skip count ({old_skips} -> {new_skips})"
    );
}

// ===========================================================================
// Balancer V2 STABLE entry-tangent soundness + Curve stableswap non-concavity.
//
// The gate maps a Balancer-stable hop to `HopMath::BalancerStable` (entry
// tangent beside the native reserve cap) wherever the closed-form curvature
// test holds, and a Curve stableswap hop to `HopMath::CurveStableswap` (native
// reserve cap ALONE). The oracle for both is the REAL leaf, never a
// reimplementation: `balancer::stable_math::calc_out_given_in` (with
// `calculate_invariant` / `calculate_invariant_deployed` for D) and
// `curve::stableswap::stableswap_get_y`. A bound that ever dips below the leaf
// would skip a profitable path.
// ===========================================================================

use degenbot_math::balancer::fixed_point::mul_up;
use degenbot_math::balancer::stable_math::{
    calc_out_given_in, calculate_invariant, calculate_invariant_deployed,
};
use degenbot_math::curve::stableswap::{stableswap_get_d, stableswap_get_y, DVariant, YVariant};

const BAL_ONE: u64 = 1_000_000_000_000_000_000;

/// One generated Balancer-stable shape (a randomized-matrix row).
#[derive(Clone, Debug)]
struct BalancerShape {
    amp: U256,
    balances: Vec<U256>,
    token_index_in: usize,
    token_index_out: usize,
    invariant: U256,
    swap_fee: U256,
    scaling_factor_in: U256,
    scaling_factor_out: U256,
}

impl BalancerShape {
    fn hop(&self) -> HopMath<'_> {
        HopMath::BalancerStable {
            amp: self.amp,
            balances: self.balances.as_slice(),
            token_index_in: self.token_index_in,
            token_index_out: self.token_index_out,
            invariant: self.invariant,
            swap_fee: self.swap_fee,
            scaling_factor_in: self.scaling_factor_in,
            scaling_factor_out: self.scaling_factor_out,
        }
    }

    /// The NATIVE output-token reserve (`balance_out / scaling_factor_out`).
    fn native_out_reserve(&self) -> U256 {
        self.balances[self.token_index_out] / self.scaling_factor_out
    }

    /// The REAL Balancer leaf, driven exactly as `simulate_balancer_stable_hop`.
    fn leaf_output(&self, x_native: U256) -> Option<U256> {
        let upscaled = x_native.saturating_mul(self.scaling_factor_in);
        let fee = mul_up(upscaled, self.swap_fee).ok()?;
        let after_fee = upscaled.saturating_sub(fee);
        let out_up = calc_out_given_in(
            self.amp,
            &self.balances,
            self.token_index_in,
            self.token_index_out,
            after_fee,
            self.invariant,
        )
        .ok()?;
        if self.scaling_factor_out.is_zero() {
            return None;
        }
        Some(out_up / self.scaling_factor_out)
    }
}

/// The four realistic swap fees the Balancer matrix draws.
const BALANCER_FEES: [u64; 4] = [
    0,
    1_000_000_000_000_000,
    10_000_000_000_000_000,
    100_000_000_000_000_000,
];

/// The three scaling-factor pairs the Balancer matrix draws — never both
/// large, so a unit-input probe stays strictly below the native reserve.
const BALANCER_SCALING: [(u64, u64); 3] = [(1, 1), (1_000_000_000_000, 1), (1, 1_000_000_000_000)];

/// Draw one Balancer matrix row: token count 2..=4, amplification
/// `amp = A·1000` for `A` in 10..=5000, upscaled balances log-uniform over
/// `10^18..10^24` with an imbalance ratio up to `1e4` in either direction, the
/// four realistic swap fees, the three scaling-factor pairs, a token index
/// pair, and either invariant entry point (`calculate_invariant` /
/// `calculate_invariant_deployed`). The row is returned with the entry point so
/// the caller can derive the invariant (which can fail to converge).
fn balancer_shape_strategy() -> impl Strategy<Value = (BalancerShape, bool)> {
    (2usize..=4, 18u32..=24, 0u32..=4, any::<bool>()).prop_flat_map(
        |(n, base_exp, ratio_exp, up)| {
            (
                prop::collection::vec(1u32..=9_999, n),
                (0usize..n, 1usize..n),
                10u32..=5_000,
                0usize..4,
                0usize..3,
                any::<bool>(),
            )
                .prop_map(
                    move |(
                        mantissas,
                        (token_index_in, token_index_out_off),
                        amp_a,
                        fee_idx,
                        sf_idx,
                        deployed,
                    )| {
                        let balances = (0..n)
                            .map(|k| {
                                let e_k = if k == 0 {
                                    base_exp
                                } else if up {
                                    base_exp + ratio_exp
                                } else {
                                    base_exp.saturating_sub(ratio_exp)
                                }
                                .clamp(1, 30);
                                pow10_u256(e_k) * U256::from(mantissas[k]) / U256::from(1_000u64)
                            })
                            .collect::<Vec<_>>();
                        let (sf_in, sf_out) = BALANCER_SCALING[sf_idx];
                        let shape = BalancerShape {
                            amp: U256::from(amp_a) * U256::from(1_000u64),
                            balances,
                            token_index_in,
                            // An offset in 1..n maps each input index to a
                            // DISTINCT output index, uniformly over the
                            // distinct pairs and with no reject-based filter.
                            token_index_out: (token_index_in + token_index_out_off) % n,
                            invariant: U256::ZERO,
                            swap_fee: U256::from(BALANCER_FEES[fee_idx]),
                            scaling_factor_in: U256::from(sf_in),
                            scaling_factor_out: U256::from(sf_out),
                        };
                        (shape, deployed)
                    },
                )
        },
    )
}

/// The Balancer-stable family under the generic dominance harness.
struct BalancerFamily;

impl DominanceFamily for BalancerFamily {
    type Shape = BalancerShape;
    const NAME: &'static str = "balancer_stable";

    fn strategy() -> BoxedStrategy<BalancerShape> {
        balancer_shape_strategy()
            .prop_filter_map("the invariant converges", |(mut shape, deployed)| {
                let invariant = if deployed {
                    calculate_invariant_deployed(shape.amp, &shape.balances, true)
                } else {
                    calculate_invariant(shape.amp, &shape.balances)
                }
                .ok()?;
                if invariant.is_zero() {
                    return None;
                }
                shape.invariant = invariant;
                Some(shape)
            })
            .boxed()
    }

    fn hop(shape: &BalancerShape) -> HopMath<'_> {
        shape.hop()
    }

    fn native_out_reserve(shape: &BalancerShape) -> U256 {
        shape.native_out_reserve()
    }

    fn leaf_output(shape: &BalancerShape, x: U256) -> Option<U256> {
        shape.leaf_output(x)
    }
}

#[test]
fn balancer_stable_entry_tangent_dominates_real_leaf() {
    run_dominance_suite::<BalancerFamily>();
}

// ---------------------------------------------------------------------------
// Curve stableswap non-concavity counterexamples (PINNED, deterministic).
//
// The src docs claim the Curve out-curve is non-concave against the real leaf,
// so an entry tangent would under-cut it and the family stays on the reserve
// cap. These cases demonstrate it: build the hop, derive the entry-tangent line
// the Solidly/Balancer arms would derive (same closed-form marginal rate, same
// fee/scaling conversion, ceil-rounded like `Line::eval`), and assert the REAL
// `stableswap_get_y` leaf output EXCEEDS it at some input in the sweep.
// ---------------------------------------------------------------------------

/// One deterministic Curve case (three coins; the co-coin is index 2).
#[derive(Clone, Copy, Debug)]
struct CurveCase {
    xp: [U256; 3],
    token_index_in: usize,
    token_index_out: usize,
    amp: U256,
    a_precision: U256,
    n_coins: U256,
    fee: U256,
    fee_denom: U256,
    precision: U256,
    rate_multiplier_in: U256,
    rate_multiplier_out: U256,
    y_variant: YVariant,
    d_variant: DVariant,
}

impl CurveCase {
    /// The REAL Curve leaf output, driven exactly as `simulate_curve_hop`.
    fn leaf_output(&self, x_native: U256) -> Option<U256> {
        let x = self.xp[self.token_index_in]
            .saturating_add(x_native.saturating_mul(self.rate_multiplier_in) / self.precision);
        let y = stableswap_get_y(
            self.token_index_in,
            self.token_index_out,
            x,
            &self.xp,
            self.amp,
            self.n_coins,
            self.a_precision,
            self.y_variant,
            self.d_variant,
        )
        .ok()?;
        let dy_raw = self.xp[self.token_index_out]
            .saturating_sub(y)
            .saturating_sub(U256::ONE);
        let fee = dy_raw.saturating_mul(self.fee) / self.fee_denom;
        Some(dy_raw.saturating_sub(fee).saturating_mul(self.precision) / self.rate_multiplier_out)
    }

    /// The entry-tangent value the Balancer/Solidly arms would derive for this
    /// hop at native input `x`, as an exact `U512` with a CEIL (matching
    /// `Line::eval`'s `ceil_div`, so the tangent is never rounded down in the
    /// leaf's favour). `None` when the closed-form marginal rate is undefined.
    fn entry_tangent_at(&self, x_native: U256) -> Option<U512> {
        let invariant = stableswap_get_d(
            &self.xp,
            self.amp,
            self.n_coins,
            self.a_precision,
            self.d_variant,
        )
        .ok()?;
        // `amp·n / a_precision` — the invariant's amplification coefficient.
        let amp_n_eff = self
            .amp
            .checked_mul(self.n_coins)?
            .checked_div(self.a_precision)?;
        let total = self
            .xp
            .iter()
            .copied()
            .fold(U256::ZERO, U256::saturating_add);
        // `P = D + m·(Σx − D)` — the invariant's product term.
        let product =
            invariant.checked_add(amp_n_eff.checked_mul(total.checked_sub(invariant)?)?)?;
        let cross = self.xp[self.token_index_in].checked_mul(self.xp[self.token_index_out])?;
        let rate_num = amp_n_eff
            .checked_mul(cross)?
            .checked_add(product.checked_mul(self.xp[self.token_index_out])?)?;
        let rate_den = amp_n_eff
            .checked_mul(cross)?
            .checked_add(product.checked_mul(self.xp[self.token_index_in])?)?;
        let retained = self.fee_denom.checked_sub(self.fee)?;
        let numer = U512::from(rate_num)
            .checked_mul(U512::from(self.rate_multiplier_in))?
            .checked_mul(U512::from(retained))?;
        let denom = U512::from(rate_den)
            .checked_mul(U512::from(self.rate_multiplier_out))?
            .checked_mul(U512::from(self.fee_denom))?;
        if denom.is_zero() {
            return None;
        }
        let prod = numer.checked_mul(U512::from(x_native))?;
        Some(prod.checked_add(denom)?.checked_sub(U512::ONE)? / denom)
    }

    /// Sweep the log grid and return `(max_excess, argmax, leaf_at_argmax)` —
    /// the largest amount by which the REAL leaf exceeds the entry tangent.
    fn max_excess(&self) -> (U512, U256, U256) {
        let reserve =
            self.xp[self.token_index_out].saturating_mul(self.precision) / self.rate_multiplier_out;
        let mut best = U512::ZERO;
        let mut arg = U256::ZERO;
        let mut leaf_at = U256::ZERO;
        let mut x = reserve;
        for _ in 0..120 {
            if x.is_zero() {
                break;
            }
            if let (Some(leaf), Some(tangent)) = (self.leaf_output(x), self.entry_tangent_at(x)) {
                let leaf512 = U512::from(leaf);
                if leaf512 > tangent && leaf512 - tangent > best {
                    best = leaf512 - tangent;
                    arg = x;
                    leaf_at = leaf;
                }
            }
            x = x.saturating_mul(U256::from(464u64)) / U256::from(1000u64);
        }
        (best, arg, leaf_at)
    }
}

fn curve_base(xp: [U256; 3], y_variant: YVariant, d_variant: DVariant) -> CurveCase {
    CurveCase {
        xp,
        token_index_in: 0,
        token_index_out: 1,
        amp: U256::from(10_000u64), // A = 100 (a_precision = 100)
        a_precision: U256::from(100u64),
        n_coins: U256::from(3u64),
        fee: U256::from(4_000_000u64), // 0.04 % of 1e10
        fee_denom: U256::from(10_000_000_000u64),
        precision: U256::from(BAL_ONE),
        rate_multiplier_in: U256::from(BAL_ONE),
        rate_multiplier_out: U256::from(BAL_ONE),
        y_variant,
        d_variant,
    }
}

/// Coherent Standard/Standard 3-coin pool whose co-coin is nearly empty and
/// whose amplification is at the top of the realistic range — the leaf's
/// Standard y-iteration overshoots the entry tangent by a measured 1–2 native
/// units here (the smallest soundness breach that still makes a tangent
/// unsound).
fn curve_standard_nearly_empty_co_coin() -> CurveCase {
    let mut case = curve_base(
        [pow10_u256(24), pow10_u256(24), pow10_u256(19)],
        YVariant::Standard,
        DVariant::Standard,
    );
    case.amp = U256::from(1_000_000u64); // A = 10_000
    case
}

/// A `Variant1` y-iteration paired with a `Standard` D-step — `project_curve`
/// reads the two discriminators INDEPENDENTLY from the pool identity, so the
/// pairing is reachable — on a pool whose co-coin is nearly empty. The
/// `Variant1` y-step omits the `A_PRECISION` divisor, so its root is not the
/// invariant's and the leaf overshoots the entry tangent by ~1e24 native units
/// (essentially the pool's whole output reserve).
fn curve_variant1_standard_nearly_empty_co_coin() -> CurveCase {
    curve_base(
        [pow10_u256(24), pow10_u256(24), U256::ONE],
        YVariant::Variant1,
        DVariant::Standard,
    )
}

/// The Standard/Standard case: the real leaf overshoots the entry tangent — the
/// measured reason Curve stays on the cap. The overshoot is small (a 1–2
/// native-unit rounding excess) but a single unit is already an unsound skip.
#[test]
fn curve_standard_nearly_empty_co_coin_is_non_concave() {
    let case = curve_standard_nearly_empty_co_coin();
    let (excess, arg, leaf) = case.max_excess();
    eprintln!(
        "curve Standard/Standard counterexample: excess={excess} native units \
         at x={arg} (leaf={leaf}, tangent={})",
        case.entry_tangent_at(arg).unwrap_or_default()
    );
    assert!(
        excess >= U512::from(1u64),
        "expected the Standard/Standard leaf to overshoot the entry tangent, \
         got {excess}"
    );
}

/// The Variant1/Standard case: the overshoot reaches the pool's whole output
/// reserve (`~1e24` native units, far past the `1e5+` scale).
#[test]
fn curve_variant1_standard_nearly_empty_co_coin_is_non_concave() {
    let case = curve_variant1_standard_nearly_empty_co_coin();
    let (excess, arg, leaf) = case.max_excess();
    eprintln!(
        "curve Variant1/Standard counterexample: excess={excess} native \
         units at x={arg} (leaf={leaf}, tangent={})",
        case.entry_tangent_at(arg).unwrap_or_default()
    );
    assert!(
        excess >= U512::from(100_000u64),
        "expected the Variant1/Standard leaf to overshoot the entry tangent by \
         far more than 1e5 native units, got {excess}"
    );
}
