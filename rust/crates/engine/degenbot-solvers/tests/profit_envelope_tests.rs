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
use degenbot_math::cl::swap_math::compute_swap_step_v3;
use degenbot_math::cl::tick_math::get_sqrt_ratio_at_tick_internal;
use degenbot_math::v2::IntHopState;
use degenbot_pools::int_v3_hop::{IntV3TickRangeHop, IntV3TickRangeSequence};
use degenbot_solvers::profit_envelope::{
    path_output_bound_at, path_profit_bound, ClHop, Envelope, GateDeps, GateSkipCause, HopMath,
};
use degenbot_solvers::runtime::SolveRuntimeConfig;
use std::borrow::Cow;

/// The gate through its one entry at offline deps, unwrapped to the bound
/// (legacy Option shape) for soundness-only assertions.
fn bound(hops: &[Option<HopMath<'_>>]) -> Option<U256> {
    match path_profit_bound(hops, &GateDeps::offline()) {
        Envelope::Bound(b) => Some(b),
        Envelope::Unsupported(_) => None,
    }
}

/// Cl-carrying view helper: sequence + a borrowed table (the carried shape).
fn cl_carried<'a>(
    seq: &'a IntV3TickRangeSequence,
    table: &'a Vec<degenbot_pools::int_v3_hop::IntTickRangeCrossing>,
) -> HopMath<'a> {
    HopMath::Cl(ClHop {
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
    let view = [Some(HopMath::cl_derived(seq))];
    for &x in &SWEEP {
        let x = U256::from(x);
        let bound = path_output_bound_at(
            &view,
            &x,
            &degenbot_solvers::runtime::SolveRuntimeConfig::default(),
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
            &degenbot_solvers::runtime::SolveRuntimeConfig::default(),
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
        Some(HopMath::cl_derived(&cl)),
        Some(HopMath::V2(&v2)),
    ];
    for &x in &SWEEP {
        let x = U256::from(x);
        let bound = path_output_bound_at(
            &views,
            &x,
            &degenbot_solvers::runtime::SolveRuntimeConfig::default(),
        )
        .expect("derives");
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
    // Type-enforced: an unmapped hop is Unsupported(UnmappedHop) — solved
    // unscreened, never skip-eligible.
    assert!(matches!(
        path_profit_bound(&[None], &GateDeps::offline()),
        Envelope::Unsupported(GateSkipCause::UnmappedHop)
    ));
    assert!(path_output_bound_at(
        &[None],
        &U256::from(1u64),
        &degenbot_solvers::runtime::SolveRuntimeConfig::default()
    )
    .is_none());
}

#[test]
fn m6776w_overflow_paths_compose_without_poisoning() {
    // Multi-hop paths with 1e24+ reserves previously overflowed I512 during
    // compose, returning None (unsupported). The sound-reduction fix must
    // return Some WITHOUT under-cutting the true curve.
    use degenbot_solvers::profit_envelope::{path_output_bound_at, HopMath};
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
        &degenbot_solvers::runtime::SolveRuntimeConfig::default(),
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
    use degenbot_solvers::profit_envelope::{path_output_bound_at, HopMath};
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
        &degenbot_solvers::runtime::SolveRuntimeConfig::default(),
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
    use degenbot_solvers::profit_envelope::path_profit_bound_with_stats;
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
    let v2_zero = degenbot_solvers::profit_envelope::HopMath::V2(&IntHopState::new(
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
// profit of every heavy-CL capture path.
// ---------------------------------------------------------------------------

static CAPTURES: LazyLock<String> = LazyLock::new(|| {
    degenbot_solvers::capture_fixture::read_fixture(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/heavy_cl_solve_captures.jsonl")
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
            .map(|s| Some(HopMath::cl_derived(s)))
            .collect();
        let crossing_owned: Vec<Vec<degenbot_pools::int_v3_hop::IntTickRangeCrossing>> = owned_seqs
            .iter()
            .map(degenbot_pools::int_v3_hop::IntV3TickRangeSequence::crossings)
            .collect();
        let carried: Vec<Option<HopMath<'_>>> = owned_seqs
            .iter()
            .zip(crossing_owned.iter())
            .map(|(s, t)| Some(cl_carried(s, t)))
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
        .map(|s| Some(HopMath::cl_derived(s)))
        .collect();

    // Same epoch both calls: populate the (content-keyed) cache, then reuse.
    let store = degenbot_solvers::profit_envelope::PrefixCache::new();
    let deps = degenbot_solvers::profit_envelope::GateDeps::per_block(
        1,
        &store,
        None,
        None,
        SolveRuntimeConfig::default(),
    );
    let (first, _gs1) =
        degenbot_solvers::profit_envelope::path_profit_bound_with_stats(&views, &deps);
    let (second, gs2) =
        degenbot_solvers::profit_envelope::path_profit_bound_with_stats(&views, &deps);
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
    let (dropped, gs3) = degenbot_solvers::profit_envelope::path_profit_bound_with_stats(
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
    // Loop-16 T3: two paths share the identical CL-prefix crossing tables
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
        Some(cl_carried(&seq1, &crossings1)),
        Some(cl_carried(&seq2, &crossings2)),
        Some(HopMath::V2(&small_tail)),
    ];
    let large_views = vec![
        Some(cl_carried(&seq1, &crossings1)),
        Some(cl_carried(&seq2, &crossings2)),
        Some(HopMath::V2(&large_tail)),
    ];

    let expected_small = path_profit_bound(&small_views, &GateDeps::offline());
    let expected_large = path_profit_bound(&large_views, &GateDeps::offline());
    // Populate the cache under the SMALL domain first.
    let store = degenbot_solvers::profit_envelope::PrefixCache::new();
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
        Some(cl_carried(&ladder, &ladder_crossings)),
        Some(HopMath::V2(&tiny_tail)),
    ];
    let huge_views = vec![
        Some(cl_carried(&ladder, &ladder_crossings)),
        Some(HopMath::V2(&huge_tail)),
    ];
    let expected_tiny = path_profit_bound(&tiny_views, &GateDeps::offline());
    let expected_huge = path_profit_bound(&huge_views, &GateDeps::offline());
    let store = degenbot_solvers::profit_envelope::PrefixCache::new();
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
    // chain — a mixed [V2, CL, CL] path must reuse composed boundaries on
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
        Some(cl_carried(&ladder1, &c1)),
        Some(cl_carried(&ladder2, &c2)),
    ];
    let expected = path_profit_bound(&views, &GateDeps::offline());
    // C4: the cache is an owner-scoped value this test holds — populate once,
    // then the warm solve must hit DETERMINISTICALLY (no spin; C4 retired the
    // process-global store that made cross-test interference possible).
    let store = degenbot_solvers::profit_envelope::PrefixCache::new();
    let cold = path_profit_bound(
        &views,
        &GateDeps::per_block(5, &store, None, None, SolveRuntimeConfig::default()),
    );
    assert_eq!(cold, expected, "cold populate must be byte-identical");
    let (warm, gs) = degenbot_solvers::profit_envelope::path_profit_bound_with_stats(
        &views,
        &GateDeps::per_block(5, &store, None, None, SolveRuntimeConfig::default()),
    );
    assert_eq!(warm, expected, "mixed-prefix reuse must be byte-identical");
    assert!(
        gs.prefix_hits > 0,
        "mixed [V2, CL] prefixes must produce cache hits (V2 no longer breaks the chain)"
    );
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
        let views: Vec<Option<HopMath<'_>>> = owned_seqs
            .iter()
            .map(|s| Some(HopMath::cl_derived(s)))
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

        let bound = bound(&views).expect("all-CL capture derives");
        let doc_pid = doc.get("path_id").cloned().unwrap_or_default();
        let doc_block = doc.get("block").cloned().unwrap_or_default();
        if bound < golden_profit {
            for (ih, ys) in gh.iter().enumerate() {
                let so: U256 = ys.as_str().expect("hop output str").parse().unwrap();
                let sb = path_output_bound_at(
                    &views[..=ih],
                    &go_in,
                    &degenbot_solvers::runtime::SolveRuntimeConfig::default(),
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

/// In-memory [`degenbot_solvers::profit_envelope::DegeneratePathSink`] — the
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

impl degenbot_solvers::profit_envelope::DegeneratePathSink for InMemoryCaptureSink {
    fn try_reserve(&self) -> bool {
        self.reserved
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            < self.max_paths
    }

    fn capture(&self, event: &degenbot_solvers::profit_envelope::DegeneratePathEvent<'_>) {
        self.rows
            .lock()
            .expect("capture rows lock")
            .push(event.to_jsonl_row());
    }
}

/// The capture harness must serialize a degenerate CL path's full range state
/// to a JSONL row through the caller-supplied sink so it can be replayed
/// offline for fix experimentation. The row is pinned byte-for-byte against the
/// historical schema.
#[test]
fn m6776w_capture_harness_writes_jsonl_for_zero_liq_rejection() {
    // The gate keeps no thread-local counters now; the capture cap is driven
    // by the sink alone.

    // A CL path with a zero-liquidity crossing range (the 40% bucket).
    let good_price = p_at_tick(0);
    let range = mk_range(0, good_price, p_at_tick(-10), p_at_tick(10));
    let seq = mk_seq(vec![range]);
    let views: Vec<Option<HopMath<'_>>> = vec![Some(HopMath::cl_derived(&seq))];

    // path_profit_bound returns Unsupported(DegenerateHop) and triggers the
    // capture through the sink the harness supplies (the gate reads no env).
    let sink = InMemoryCaptureSink::new(10);
    // The constructor requires the store; the degenerate reject fires before
    // any prefix lookup, so the cache stays empty and the outcome is identical.
    let store = degenbot_solvers::profit_envelope::PrefixCache::new();
    let deps = degenbot_solvers::profit_envelope::GateDeps::per_block(
        0,
        &store,
        Some(&sink),
        None,
        SolveRuntimeConfig::default(),
    );
    assert!(matches!(
        path_profit_bound(&views, &deps),
        degenbot_solvers::profit_envelope::Envelope::Unsupported(
            degenbot_solvers::profit_envelope::GateSkipCause::DegenerateHop
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
            "family": "cl",
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

/// The envelope must NOT reject a CL hop with an extreme entry price.
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
    let views: Vec<Option<HopMath<'_>>> = vec![Some(HopMath::cl_derived(&seq))];

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
// Cap-tail overflow probe: synthetic CL sequences where the LAST range
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
    let views: Vec<Option<HopMath>> = vec![Some(HopMath::cl_derived(&seq))];
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
    let views: Vec<Option<HopMath>> = vec![Some(HopMath::cl_derived(&seq))];
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
    let views: Vec<Option<HopMath>> = vec![Some(HopMath::cl_derived(&seq))];
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
    let views: Vec<Option<HopMath>> = vec![Some(HopMath::cl_derived(&seq))];
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

fn solidly_next(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *state
}

fn solidly_pow10(exp: u32) -> U256 {
    let mut v = U256::ONE;
    for _ in 0..exp {
        v = v.saturating_mul(U256::from(10u64));
    }
    v
}

/// Draw one matrix row: reserves log-uniform 1e6..1e30, imbalance ratio up to
/// 1e12 in either direction, the four decimals pairs, the three fee pairs,
/// `token_in` in {0, 1}, and the Aerodrome/Camelot leaf choice. The wide ratio
/// cap (12) is what drives the mixed-decimals post-trade window past the
/// leaf's `calc_d` wrap threshold, exercising the faithful-arithmetic guard.
fn solidly_shape(state: &mut u64) -> SolidlyShape {
    let e0 = 6 + solidly_next(state) % 25;
    let m0 = 100 + solidly_next(state) % 90_000;
    let r0 = solidly_pow10(u32::try_from(e0).unwrap_or(6)).saturating_mul(U256::from(m0))
        / U256::from(100u64);
    let ratio_exp = solidly_next(state) % 13;
    let up = solidly_next(state).is_multiple_of(2);
    let e1 = if up {
        e0 + ratio_exp
    } else {
        e0.saturating_sub(ratio_exp)
    }
    .clamp(1, 31);
    let m1 = 100 + solidly_next(state) % 90_000;
    let r1 = solidly_pow10(u32::try_from(e1).unwrap_or(1)).saturating_mul(U256::from(m1))
        / U256::from(100u64);
    let decs = [
        (
            U256::from(1_000_000_000_000_000_000u64),
            U256::from(1_000_000_000_000_000_000u64),
        ),
        (
            U256::from(1_000_000u64),
            U256::from(1_000_000_000_000_000_000u64),
        ),
        (
            U256::from(1_000_000_000_000_000_000u64),
            U256::from(1_000_000u64),
        ),
        (U256::from(1_000_000u64), U256::from(1_000_000u64)),
    ];
    let (decimals_0, decimals_1) = decs[(solidly_next(state) % 4) as usize];
    let fees = [
        (U256::ZERO, U256::ONE),
        (U256::from(3u64), U256::from(10_000u64)),
        (U256::from(30u64), U256::from(10_000u64)),
    ];
    let (fee_numer, fee_denom) = fees[(solidly_next(state) % 3) as usize];
    SolidlyShape {
        reserves_0: r0,
        reserves_1: r1,
        decimals_0,
        decimals_1,
        token_in: (solidly_next(state) % 2) as u8,
        fee_numer,
        fee_denom,
        camelot: solidly_next(state).is_multiple_of(2),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// Point-wise dominance of the Solidly-stable entry-tangent bound over the
    /// REAL leaves on a dense log grid up to the reserve cap.
    #[test]
    fn solidly_stable_entry_tangent_dominates_real_leaves(seed in any::<u64>()) {
        let cfg = SolveRuntimeConfig::default();
        let mut rng = seed | 1;
        let mut n_shapes = 0u32;
        let mut n_asserts = 0u32;
        let mut n_rise_active = 0u32;
        let mut n_leaf_wrapped = 0u32;
        let mut n_leaf_undefined = 0u32;
        let mut n_guard_fired = 0u32;

        for _ in 0..400 {
            let shape = solidly_shape(&mut rng);
            let out_reserve = shape.native_out_reserve();
            if out_reserve.is_zero() {
                continue;
            }
            n_shapes += 1;
            let views = [Some(shape.hop())];

            // The faithful-arithmetic guard drops the rise on shapes whose
            // post-trade window would wrap the leaf's `calc_d` products; those
            // fall back to the flat cap ALONE, so the bound at unit input is
            // exactly the output reserve. The rise, when kept, anchors at the
            // tiny additive allowance `a << reserve_out` there.
            let unit_bound = path_output_bound_at(&views, &U256::ONE, &cfg)
                .expect("the Solidly-stable bound always derives");
            if unit_bound >= out_reserve {
                n_guard_fired += 1;
                prop_assert_eq!(
                    unit_bound, out_reserve,
                    "a guard-fired (cap-only) shape must bound to the flat cap"
                );
            }

            // Dense log grid from the reserve cap down ~8 decades.
            let mut x = out_reserve;
            for _ in 0..40 {
                if x.is_zero() {
                    break;
                }
                for probe in [x, x.saturating_add(U256::ONE)] {
                    // The Solidly leaf's Newton step divides by `calc_d`,
                    // which floors to zero when the scaled input reserve is
                    // below `ONE^(2/3)` — the leaf reverts there, so its
                    // output is undefined (Camelot guards the same division).
                    if !shape.camelot
                        && shape.scaled_in_reserve() < U256::from(1_000_000_000_000u64)
                    {
                        n_leaf_undefined += 1;
                        continue;
                    }
                    let Some(leaf) = shape.leaf_output(probe) else {
                        continue;
                    };
                    // A leaf reporting more than the pool holds has wrapped its
                    // own `Rb - y` arithmetic; that is not the invariant curve,
                    // so no invariant-based bound is defined there.
                    if leaf > out_reserve {
                        n_leaf_wrapped += 1;
                        continue;
                    }
                    let bound = path_output_bound_at(&views, &probe, &cfg)
                        .expect("the Solidly-stable bound always derives");
                    prop_assert!(
                        bound >= leaf,
                        "Solidly-stable bound {bound} < leaf {leaf} at x={probe} \
                         (r0={}, r1={}, d0={}, d1={}, fee={}/{}, token_in={}, camelot={})",
                        shape.reserves_0, shape.reserves_1, shape.decimals_0,
                        shape.decimals_1, shape.fee_numer, shape.fee_denom,
                        shape.token_in, shape.camelot
                    );
                    n_asserts += 1;
                    if bound < out_reserve {
                        n_rise_active += 1;
                    }
                }
                x = x.saturating_mul(U256::from(464u64)) / U256::from(1000u64);
            }
        }

        eprintln!(
            "solidly_stable proptest: shapes={n_shapes} asserts={n_asserts} \
             rise_active={n_rise_active} guard_fired_cap_only={n_guard_fired} \
             leaf_wrapped={n_leaf_wrapped} leaf_undefined={n_leaf_undefined}"
        );

        prop_assert!(n_shapes >= 300, "matrix too small: {n_shapes} shapes");
        prop_assert!(n_asserts >= 2_000, "vacuous matrix: {n_asserts} assertions");
        prop_assert!(
            n_rise_active >= 200,
            "the tangent rise was never the active line: {n_rise_active} probes \
             ({n_leaf_wrapped} leaf-wrapped and {n_leaf_undefined} \
             leaf-undefined probes skipped)"
        );
        prop_assert!(
            n_guard_fired > 0,
            "the wide-ratio matrix never exercised the faithful-arithmetic guard: \
             {n_guard_fired} cap-only shapes"
        );
    }
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
/// carries a Solidly leg (both are V2/CL only), so the Solidly corpus is
/// synthesized here: healthy pools whose true max profit is far below the
/// floor while their reserve caps sit far above it.
#[test]
fn solidly_stable_tangent_raises_skip_count_on_synthesized_corpus() {
    use degenbot_solvers::mixed::solve_path_with_min_profit;
    use degenbot_solvers::mixed::{ResolvedHop, ResolvedMixedPath, SolidlyHopState};
    use degenbot_solvers::profit_envelope::{path_profit_bound_with_floor, GateDeps};
    use degenbot_uniswap::dex_identity::DexVariant;

    // The committed mixed corpus has no Solidly leg.
    let committed = degenbot_solvers::capture_fixture::load_rows(
        degenbot_solvers::capture_fixture::fixture_path("heavy_mixed_solve_captures.jsonl"),
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
