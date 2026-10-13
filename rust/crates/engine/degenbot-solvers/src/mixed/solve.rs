//! Pure solve + simulate family for mixed multi-hop paths (ADR-015).
//!
//! Receiver-free: `solve_path(&ResolvedMixedPath) -> Option<SolvePathResult>`
//! is the dispatcher; the `solve_*_path_int` arms are golden-section / mixed
//! Möbius searches over the math leaves; the `simulate_*_hop` leaves are the
//! per-hop swap functions the searches call. Relocated from
//! `degenbot-bot/src/solvers/arb_engine/solver_dispatch.rs` (where they were
//! `impl ArbitrageEngine` associated fns with `#[allow(unused_self)]`).
//!
//! The CL hop uses `crate::cl::*` (same crate); the V2 + CL
//! Möbius arms call `crate::mobius_int::*` / `crate::mobius_int_exact::*`.

use std::collections::BTreeSet;

use alloy::primitives::{U256, U512};

use crate::mixed::families::cl::{cl_hop_lines, cl_to_math, solve_walkable_path_int};
use crate::mixed::families::solidly::{solidly_hop_lines, solidly_to_math, solve_solidly_entry};
use crate::profit_envelope::{
    balancer_stable_hop_lines, curve_hop_lines, path_profit_bound_with_floor, refuse_lines,
    reserve_cap_hop_lines, v2_hop_lines, weighted_hop_lines, Envelope, GateDeps, GateStats,
    HopMath, Line, PathBoundLines, PrefixCache,
};
use crate::runtime::SolveRuntimeConfig;

use crate::mixed::{
    BalancerStableHopState, BalancerWeightedHopState, CurveStableswapHopState, HopType,
    ResolvedHop, ResolvedMixedPath, SolvePathResult, INT128_MAX,
};

/// Dispatches based on path composition:
/// - V2-V2: integer-exact Möbius solver (closed-form U512 isqrt)
/// - V3-V3 / V4-V4 / V3-V4 / V4-V3: integer piecewise-Möbius (CL × CL)
/// - V2-V3 / V3-V2 / V2-V4 / V4-V2: mixed integer-exact solver
///
/// ADR-005 slice 15b-1: signature dropped the `&self` receiver (it was
/// `#[allow(clippy::unused_self)]` — body is pure dispatch to freestanding
/// math helpers). The static form lets `rebuild_and_solve_affected` and
/// `solve_all` invoke `solve_path` from a rayon `par_iter` closure without
/// borrowing `self` (which would conflict with the `&mut self` write to
/// `self.results` that follows the solve).
/// The mixed solve entry's whole return: result + typed envelope verdict +
/// returned walk telemetry (no frozen thread-locals on the
/// read-back path).
#[derive(Debug)]
pub struct SolveOutcome {
    pub result: Option<SolvePathResult>,
    /// The gate's typed fact, carried independently of walk telemetry.
    /// Planning maps `Bound` at or below its floor to the no-envelope-profit
    /// rejection; `Unsupported` remains solve-unscreened.
    pub envelope: Envelope,
    pub stats: crate::cl::WalkStats,
    /// The gate phase counters this solve produced — carried out on the
    /// outcome (the asymmetric twin of `stats`; no thread-local read-back).
    pub gate_stats: GateStats,
}

#[must_use]
pub fn solve_path(resolved: &ResolvedMixedPath, gate: &GateDeps<'_>) -> SolveOutcome {
    solve_path_with_min_profit(resolved, U256::ZERO, gate)
}

/// [`solve_path`] with the profit-envelope gate active at floor `min_profit`:
/// when the rigorous upper bound on path profit falls below the floor, the
/// path is provably unprofitable and returns `None` WITHOUT running any
/// walk. See GLOSSARY.md → "Profit envelope gate". The gate's epoch / cache /
/// capture stance rides [`GateDeps`] — `GateDeps::per_block` in the engine,
/// `GateDeps::offline` in tests and replays.
#[must_use]
pub fn solve_path_with_min_profit(
    resolved: &ResolvedMixedPath,
    min_profit: U256,
    gate: &GateDeps<'_>,
) -> SolveOutcome {
    // Hard cutover : the gate is unconditional.
    let views: Vec<Option<crate::profit_envelope::HopMath<'_>>> =
        resolved.hops.iter().map(hop_math_for).collect();
    let (envelope, mut gate_stats) = path_profit_bound_with_floor(&views, gate, min_profit);
    match envelope {
        // Unsupported families are SOLVED unscreened, never skipped — the
        // gate counts its own verdicts (evaluated / unsupported / cause).
        Envelope::Unsupported(_) => {}
        Envelope::Bound(bound) => {
            // `<=`: a path whose rigorous max profit cannot STRICTLY exceed the
            // floor is unexecutable at that floor (dispatch discards zero-profit
            // results downstream), so skipping is outcome-identical and saves
            // the walk.
            if bound <= min_profit {
                gate_stats.skipped += 1;
                return SolveOutcome {
                    result: None,
                    envelope,
                    stats: crate::cl::WalkStats::default(),
                    gate_stats,
                };
            }
        }
    }
    // Loop-21 `envelope_pruned_refine`: hand the walk the composed bound so
    // its refine windows skip the inputs the bound disproves. This is the
    // SECOND query against the gate's chain-composition path — it shares the
    // gate's prefix store, so the CL tangent fans and composed prefixes are
    // reused rather than re-derived. Its derive / prune / exact-compose
    // counters merge into the same returned `GateStats` the gate call
    // produced.
    let walk_env = if gate.runtime.envelope_pruned_refine {
        crate::profit_envelope::path_bound_lines_with_stats(
            &views,
            &gate.runtime,
            gate.fan_cache(),
            &mut gate_stats,
        )
    } else {
        None
    };
    let (result, stats) = solve_path_inner(resolved, gate, walk_env.as_ref());
    SolveOutcome {
        result,
        envelope,
        stats,
        gate_stats,
    }
}

/// Map one resolved hop to the gate's hop view, or `None` when the family has
/// no rigorous bound (an unmapped hop POISONS the whole path — no skip without
/// a proof). Dispatch is a row lookup on the hop's family plus a one-line
/// delegation to that row's `to_math` projection (ADR-071 D3). Each family's
/// projection lives in its own fn so the two stableswap arms can BORROW the
/// leaf's state slices instead of cloning a `Vec` per hop per candidate path.
fn hop_math_for(hop: &ResolvedHop) -> Option<crate::profit_envelope::HopMath<'_>> {
    (family_row_for(hop.hop_type()).to_math)(hop)
}

/// The V2 family's gate projection: the resolved constant-product state.
fn v2_to_math(hop: &ResolvedHop) -> Option<HopMath<'_>> {
    match hop {
        ResolvedHop::V2 { state } => Some(HopMath::V2(state)),
        _ => None,
    }
}

/// The Balancer-weighted family's gate projection: the concave weighted-pool
/// curve → entry-rate tangent dominates globally (sound) + the reserve
/// asymptote caps it.
fn weighted_to_math(hop: &ResolvedHop) -> Option<HopMath<'_>> {
    match hop {
        ResolvedHop::BalancerWeighted { state } => Some(HopMath::Weighted {
            balance_in: state.balance_in,
            balance_out: state.balance_out,
            weight_in: state.weight_in,
            weight_out: state.weight_out,
            scaling_in: state.scaling_factor_in,
            scaling_out: state.scaling_factor_out,
        }),
        _ => None,
    }
}

/// The Balancer-stable family's gate projection: the leaf's out-curve is
/// concave in the upscaled input wherever the derive arm's closed-form
/// curvature test holds, so its entry tangent is a sound rise beside the
/// reserve cap.
fn stable_to_math(hop: &ResolvedHop) -> Option<HopMath<'_>> {
    match hop {
        ResolvedHop::BalancerStable { state } => Some(HopMath::BalancerStable {
            amp: state.amp,
            balances: state.balances.as_slice(),
            token_index_in: state.token_index_in,
            token_index_out: state.token_index_out,
            invariant: state.invariant,
            swap_fee: state.swap_fee,
            scaling_factor_in: state.scaling_factor_in,
            scaling_factor_out: state.scaling_factor_out,
        }),
        _ => None,
    }
}

/// The Curve family's gate projection: the entry tangent was MEASURED
/// non-concave against the real leaf (see `curve_stableswap_lines`), so the
/// family keeps the sound native reserve cap — carried through its own hop
/// view.
fn curve_to_math(hop: &ResolvedHop) -> Option<HopMath<'_>> {
    match hop {
        ResolvedHop::CurveStableswap { state } => {
            if state.rate_multiplier_out.is_zero() {
                return None;
            }
            // Only the fields the cap derivation and the family label read:
            // the rest of the resolved state stays on `CurveStableswapHopState`
            // for `simulate_curve_hop`, but the gate view does not carry it.
            Some(HopMath::CurveStableswap {
                amp: state.amp,
                xp: state.xp.as_slice(),
                token_index_in: state.token_index_in,
                token_index_out: state.token_index_out,
                precision: state.precision,
                rate_multiplier_out: state.rate_multiplier_out,
            })
        }
        _ => None,
    }
}

/// The refusal rows' projection: no hop view (an unmapped hop poisons the
/// path). Never reached through a family row's `to_math`.
fn refuse_to_math(_hop: &ResolvedHop) -> Option<HopMath<'_>> {
    None
}

/// The family's path-level solve entry (ADR-071 D3): maps a resolved path to
/// its result plus walk telemetry. Dispatch runs ONCE per solve, never per
/// candidate input (ADR-059 D6), so a static fn pointer costs nothing in the
/// hot loop.
pub(crate) type SolveFn = fn(
    &ResolvedMixedPath,
    &GateDeps<'_>,
    Option<&PathBoundLines>,
) -> (Option<SolvePathResult>, crate::cl::WalkStats);

/// The family's `ResolvedHop -> Option<HopMath>` gate projection.
pub(crate) type ToMathFn = fn(&ResolvedHop) -> Option<HopMath<'_>>;

/// The family's envelope line builder — the per-family arm of the former
/// `hop_lines_and_cap_cached` match.
pub(crate) type LinesFn = fn(
    &HopMath<'_>,
    &SolveRuntimeConfig,
    Option<(&PrefixCache, u64)>,
    &mut GateStats,
) -> Option<(Vec<Line>, U256)>;

/// One row of [`FAMILY_DISPATCH`]: the family set a path may present, the
/// family it must present, and the family's entry surface. The row IS the
/// registration (ADR-071 D3): adding a family adds a row, not a central
/// `match` arm.
pub(crate) struct FamilyRow {
    /// Families the path may contain for this row to match (`None` = any).
    allowed: Option<&'static [HopType]>,
    /// A family the path must contain for this row to match.
    required: Option<HopType>,
    /// The family's path-level solve entry.
    pub(crate) solve: SolveFn,
    /// The family's `ResolvedHop -> Option<HopMath>` projection.
    pub(crate) to_math: ToMathFn,
    /// The family's envelope line builder.
    pub(crate) lines: LinesFn,
    /// The family's label token (the `HopMath::family_label` family name).
    pub(crate) label: &'static str,
}

/// The ordered family-set dispatch table. The FIRST row whose `allowed` set
/// contains every present family (and whose `required` family, when set, is
/// present) selects the row. A path matching no row falls to [`WALKABLE_ROW`].
///
/// The order is load-bearing, not cosmetic: a V2+Solidly path must reach the
/// Solidly row before the CL/mixed walk, and a Solidly+CL pairing (or a
/// Solidly hop beside a weighted/stable/Curve hop) must be refused rather
/// than walked. Each refusal row therefore requires its family with no
/// allowed-set constraint — it fires only once the narrower rows above it have
/// all failed.
static FAMILY_DISPATCH: &[FamilyRow] = &[
    // ExactV2: every hop V2 → the integer-exact Möbius solve.
    FamilyRow {
        allowed: Some(&[HopType::V2]),
        required: None,
        solve: solve_exact_v2_entry,
        to_math: v2_to_math,
        lines: v2_hop_lines,
        label: "v2",
    },
    // Concentrated liquidity: every hop V3/V4 → the all-CL walk.
    FamilyRow {
        allowed: Some(&[HopType::V3, HopType::V4]),
        required: None,
        solve: solve_walkable_path_int,
        to_math: cl_to_math,
        lines: cl_hop_lines,
        label: "cl",
    },
    // Solidly accept row: V2 ∪ Solidly with ≥1 Solidly hop. A lone Solidly
    // path is the arm's own len<2 concern, not the dispatch's.
    FamilyRow {
        allowed: Some(&[HopType::V2, HopType::SolidlyStable]),
        required: Some(HopType::SolidlyStable),
        solve: solve_solidly_entry,
        to_math: solidly_to_math,
        lines: solidly_hop_lines,
        label: "solidly",
    },
    // Solidly refusal row: Solidly beside a CL hop (or a second non-V2 family).
    FamilyRow {
        allowed: None,
        required: Some(HopType::SolidlyStable),
        solve: refuse_path,
        to_math: refuse_to_math,
        lines: refuse_lines,
        label: "unsupported",
    },
    // Weighted accept row, then its refusal row beside a CL hop.
    FamilyRow {
        allowed: Some(&[HopType::V2, HopType::BalancerWeighted]),
        required: Some(HopType::BalancerWeighted),
        solve: solve_balancer_weighted_entry,
        to_math: weighted_to_math,
        lines: weighted_hop_lines,
        label: "weighted",
    },
    FamilyRow {
        allowed: None,
        required: Some(HopType::BalancerWeighted),
        solve: refuse_path,
        to_math: refuse_to_math,
        lines: refuse_lines,
        label: "unsupported",
    },
    // Stable accept row, then its refusal row beside a CL hop / second family.
    FamilyRow {
        allowed: Some(&[HopType::V2, HopType::BalancerStable]),
        required: Some(HopType::BalancerStable),
        solve: solve_balancer_stable_entry,
        to_math: stable_to_math,
        lines: balancer_stable_hop_lines,
        label: "balancer_stable",
    },
    FamilyRow {
        allowed: None,
        required: Some(HopType::BalancerStable),
        solve: refuse_path,
        to_math: refuse_to_math,
        lines: refuse_lines,
        label: "unsupported",
    },
    // Curve accept row, then its refusal row beside a CL hop.
    FamilyRow {
        allowed: Some(&[HopType::V2, HopType::CurveStableswap]),
        required: Some(HopType::CurveStableswap),
        solve: solve_curve_entry,
        to_math: curve_to_math,
        lines: curve_hop_lines,
        label: "curve_stableswap",
    },
    FamilyRow {
        allowed: None,
        required: Some(HopType::CurveStableswap),
        solve: refuse_path,
        to_math: refuse_to_math,
        lines: refuse_lines,
        label: "unsupported",
    },
];

/// The mixed V2+CL walk — the fallback for a path matching no row above, and
/// the ONLY cross-family composition the walk supports.
static WALKABLE_ROW: FamilyRow = FamilyRow {
    allowed: None,
    required: None,
    solve: solve_walkable_path_int,
    to_math: refuse_to_math,
    lines: refuse_lines,
    label: "walkable",
};

/// The gate-only `ReserveCap` pseudo-family's row. `ReserveCap` is not a hop
/// family (no [`HopType`], never selected by the path ladder) but it is a
/// [`HopMath`] variant, so `HopMath::family_row` needs a registration for its
/// line builder and label. Its `solve` / `to_math` are never reached.
pub(crate) static RESERVE_CAP_ROW: FamilyRow = FamilyRow {
    allowed: None,
    required: None,
    solve: refuse_path,
    to_math: refuse_to_math,
    lines: reserve_cap_hop_lines,
    label: "reserve_cap",
};

/// Classify a path by the SET of hop families it contains — the dispatch key.
/// The table is consulted in order and the first matching row wins.
fn family_row(families: &BTreeSet<HopType>) -> &'static FamilyRow {
    FAMILY_DISPATCH
        .iter()
        .find(|row| {
            row.required.is_none_or(|f| families.contains(&f))
                && row
                    .allowed
                    .is_none_or(|allowed| families.iter().all(|f| allowed.contains(f)))
        })
        .unwrap_or(&WALKABLE_ROW)
}

/// The row a SINGLE hop family selects — the per-hop analogue of
/// [`family_row`], used by the gate projection and by `HopMath`'s line/label
/// sites. The narrow accept rows sit above the refusal rows, so a singleton
/// set always reaches the family's accept row.
pub(crate) fn family_row_for(ht: HopType) -> &'static FamilyRow {
    FAMILY_DISPATCH
        .iter()
        .find(|row| {
            row.required.is_none_or(|f| f == ht)
                && row.allowed.is_none_or(|allowed| allowed.contains(&ht))
        })
        .unwrap_or(&WALKABLE_ROW)
}

/// The refusal rows' path entry: no engine for the pairing → no result.
fn refuse_path(
    _resolved: &ResolvedMixedPath,
    _gate: &GateDeps<'_>,
    _walk_env: Option<&PathBoundLines>,
) -> (Option<SolvePathResult>, crate::cl::WalkStats) {
    (None, crate::cl::WalkStats::default())
}

/// The V2 family's path entry: the integer-exact Möbius solve.
fn solve_exact_v2_entry(
    resolved: &ResolvedMixedPath,
    _gate: &GateDeps<'_>,
    _walk_env: Option<&PathBoundLines>,
) -> (Option<SolvePathResult>, crate::cl::WalkStats) {
    let int_hops: Vec<_> = resolved
        .hops
        .iter()
        .filter_map(ResolvedHop::as_v2_state)
        .cloned()
        .collect();
    if int_hops.len() == resolved.hops.len() {
        (
            crate::mobius_int_exact::exact_mobius_solve(&int_hops)
                .ok()
                .and_then(|r| {
                    if r.is_profitable && !r.optimal_input.is_zero() && !r.profit.is_zero() {
                        // V2 constant-product pools: each hop's consumed input
                        // is the previous hop's output (hop_outputs[i-1]),
                        // with hop 0 consuming optimal_input.
                        let mut consumed_inputs = Vec::with_capacity(r.hop_outputs.len());
                        consumed_inputs.push(r.optimal_input);
                        for i in 1..r.hop_outputs.len() {
                            consumed_inputs.push(r.hop_outputs[i - 1]);
                        }
                        Some(SolvePathResult {
                            optimal_input: r.optimal_input,
                            profit: r.profit,
                            hop_outputs: r.hop_outputs,
                            consumed_inputs,
                            ..Default::default()
                        })
                    } else {
                        None
                    }
                }),
            crate::cl::WalkStats::default(),
        )
    } else {
        (None, crate::cl::WalkStats::default())
    }
}

/// [`SolveFn`] adapter for the Balancer-weighted family's path entry.
fn solve_balancer_weighted_entry(
    resolved: &ResolvedMixedPath,
    _gate: &GateDeps<'_>,
    _walk_env: Option<&PathBoundLines>,
) -> (Option<SolvePathResult>, crate::cl::WalkStats) {
    (
        solve_balancer_weighted_path_int(resolved),
        crate::cl::WalkStats::default(),
    )
}

/// [`SolveFn`] adapter for the Balancer-stable family's path entry.
fn solve_balancer_stable_entry(
    resolved: &ResolvedMixedPath,
    _gate: &GateDeps<'_>,
    _walk_env: Option<&PathBoundLines>,
) -> (Option<SolvePathResult>, crate::cl::WalkStats) {
    (
        solve_balancer_stable_path_int(resolved),
        crate::cl::WalkStats::default(),
    )
}

/// [`SolveFn`] adapter for the Curve family's path entry.
fn solve_curve_entry(
    resolved: &ResolvedMixedPath,
    _gate: &GateDeps<'_>,
    _walk_env: Option<&PathBoundLines>,
) -> (Option<SolvePathResult>, crate::cl::WalkStats) {
    (
        solve_curve_path_int(resolved),
        crate::cl::WalkStats::default(),
    )
}

#[must_use]
#[hotpath::measure(label = "mixed.solve_path_inner")]
pub fn solve_path_inner(
    resolved: &ResolvedMixedPath,
    gate: &GateDeps<'_>,
    walk_env: Option<&crate::profit_envelope::PathBoundLines>,
) -> (Option<SolvePathResult>, crate::cl::WalkStats) {
    // An invalid (partially-resolved) path has hops missing — don't solve.
    if !resolved.valid {
        return (None, crate::cl::WalkStats::default());
    }
    // The distinct hop families present in this path — the dispatch key,
    // computed ONCE. [`family_row`] is a set-membership test against it; see
    // [`FAMILY_DISPATCH`] for the ordered row selection.
    let families: BTreeSet<HopType> = resolved.hops.iter().map(ResolvedHop::hop_type).collect();

    // The row lookup IS the dispatch (ADR-071 D3): the selected family's path
    // entry is a fn pointer on the row, so no central `match` arm names it.
    let (result, walk_stats) = (family_row(&families).solve)(resolved, gate, walk_env);

    // V4 int128 guard: reject paths where any V4 hop's consumed input or
    // output exceeds int128_max. V4's toBalanceDelta() calls toInt128() on
    // swap amounts — if either doesn't fit, V4 reverts with SafeCastOverflow.
    if let Some(ref r) = result {
        for (i, hop) in resolved.hops.iter().enumerate() {
            if matches!(hop, ResolvedHop::V4 { .. }) {
                let consumed = r.consumed_inputs.get(i).copied().unwrap_or(U256::ZERO);
                let output = r.hop_outputs.get(i).copied().unwrap_or(U256::ZERO);
                if consumed > INT128_MAX || output > INT128_MAX {
                    return (None, walk_stats);
                }
            }
        }
    }

    let result = result.map(|mut r| {
        r.state_nonces.clone_from(&resolved.state_nonces);
        // The pool-state descriptions are consumed ONLY by the
        // [solver-st] debug sites in the bot - formatting U256s for every
        // solved path every cycle is pure waste unless a debug subscriber
        // is active (RUST_LOG=degenbot::solver=debug turns them back on).
        if !tracing::enabled!(
            target: "degenbot::solver",
            tracing::Level::DEBUG
        ) {
            return r;
        }
        // Capture solver pool state for diagnostic cross-referencing.
        let pool_states: Vec<String> = resolved
            .hops
            .iter()
            .map(|hop| match hop {
                ResolvedHop::V3 { prepared, .. } | ResolvedHop::V4 { prepared, .. } => {
                    if let Some(range) = prepared.int_seq.ranges.first() {
                        let family = if matches!(hop, ResolvedHop::V3 { .. }) {
                            "V3"
                        } else {
                            "V4"
                        };
                        #[expect(clippy::cast_possible_truncation)]
                        let fee_bps = 10000u64.saturating_sub(range.gamma_numer / 100) as u16;
                        format!(
                            "{family}:sq={},liq={},fee={fee_bps},zfo={}",
                            range.sqrt_price_x96, range.liquidity, range.zero_for_one
                        )
                    } else {
                        String::new()
                    }
                }
                ResolvedHop::V2 { state } => {
                    format!("V2:rin={},rout={}", state.reserve_in, state.reserve_out)
                }
                ResolvedHop::SolidlyStable { state } => {
                    format!("Solidly:r0={},r1={}", state.reserves_0, state.reserves_1)
                }
                _ => String::new(),
            })
            .collect();
        r.solver_pool_states = pool_states;
        r
    });
    (result, walk_stats)
}

/// Number of golden-section refinement iterations (mirrors Python's
/// `SolidlyStableSolver.GOLDEN_SECTION_ITERATIONS`).
const GOLDEN_SECTION_ITERATIONS: usize = 25;

/// Shared golden-section solve template for the stable-family arms (Solidly,
/// Balancer weighted, Balancer stable, Curve stableswap).
///
/// Owns the whole search: the V2-equivalent Möbius profitability precheck, the
/// ±5x bracket around the Möbius optimum, the 25-iteration phi-approximated
/// golden loop, and the ±3 integer verification. The family-specific parts
/// enter as closures: `simulate` evaluates one candidate input against the real
/// math leaf, and `simulate_outputs` returns the per-hop outputs for the
/// winning input. All four arms share one integer-verification order (see
/// [`ascending_sweep`]).
pub(crate) fn solve_golden_section_path_int(
    v2_equiv: &[degenbot_math::v2::IntHopState],
    simulate: &impl Fn(U256) -> U256,
    simulate_outputs: &impl Fn(U256) -> Vec<U256>,
) -> Option<SolvePathResult> {
    use crate::mobius_int::compute_int_mobius_coefficients;
    use crate::mobius_int_exact::compute_mobius_model_optimal_input;

    // --- Profitability precheck (V2-equivalent Möbius) ---
    let coeffs = compute_int_mobius_coefficients(v2_equiv).ok()?;
    if !coeffs.is_profitable {
        return None;
    }
    let x_mobius = compute_mobius_model_optimal_input(&coeffs);

    // --- Bracket: [1, max_reserve], narrowed around the Möbius optimum ---
    let one = U256::from(1u64);
    let five = U256::from(5u64);
    let max_reserve = v2_equiv
        .iter()
        .map(|h| h.reserve_in)
        .max()
        .unwrap_or(U256::ZERO);
    if max_reserve.is_zero() {
        return None;
    }
    let mut x_low = one;
    let mut x_high = max_reserve;
    if !x_mobius.is_zero() {
        let x_center = x_mobius.min(x_high);
        x_low = x_low.max(x_center / five);
        x_high = x_high.min(x_center.saturating_mul(five));
    }
    if x_high <= x_low {
        // Degenerate bracket — fall back to a single-point verification.
        return verify_best(simulate, simulate_outputs, x_low);
    }

    // --- Golden-section search (25 iterations) ---
    // `phi = (sqrt(5) - 1) / 2` ≈ 0.6180339887498949, approximated as the
    // U256 fraction `phi_num / phi_den` and applied to the bracket span in
    // U512 (the span may approach U256 magnitude; the product does not fit
    // in U256 so the multiply is done in U512 then truncated back).
    let phi_num = U256::from(6_180_339_887_498_949u64); // phi * 1e16
    let phi_den = U256::from(10_000_000_000_000_000u64); // 1e16
    let phi_span = |lo: U256, hi: U256| -> U256 {
        let d = U512::from(hi - lo);
        let scaled = d * U512::from(phi_num) / U512::from(phi_den);
        crate::mobius_int_exact::narrow_u512_to_u256(scaled)
    };
    let mut x1 = x_high - phi_span(x_low, x_high);
    let mut x2 = x_low + phi_span(x_low, x_high);
    let mut p1 = simulate(x1).saturating_sub(x1);
    let mut p2 = simulate(x2).saturating_sub(x2);

    for _ in 0..GOLDEN_SECTION_ITERATIONS {
        if p1 < p2 {
            x_low = x1;
            x1 = x2;
            p1 = p2;
            x2 = x_low + phi_span(x_low, x_high);
            p2 = simulate(x2).saturating_sub(x2);
        } else {
            x_high = x2;
            x2 = x1;
            p2 = p1;
            x1 = x_high - phi_span(x_low, x_high);
            p1 = simulate(x1).saturating_sub(x1);
        }
    }

    let x_opt = (x_low + x_high) / U256::from(2u64);
    verify_best(simulate, simulate_outputs, x_opt)
}

/// Integer verification + result construction for the golden-section template:
/// resolve `center` to the max-profit input with the shared ascending ±3 sweep
/// ([`ascending_sweep`]), then build the [`SolvePathResult`]. Returns `None`
/// when no candidate is profitable.
fn verify_best(
    simulate: &impl Fn(U256) -> U256,
    simulate_outputs: &impl Fn(U256) -> Vec<U256>,
    center: U256,
) -> Option<SolvePathResult> {
    let (best_input, best_profit) = ascending_sweep(simulate, center)?;

    let hop_outputs = simulate_outputs(best_input);
    let mut consumed_inputs = Vec::with_capacity(hop_outputs.len());
    consumed_inputs.push(best_input);
    for i in 1..hop_outputs.len() {
        consumed_inputs.push(hop_outputs[i - 1]);
    }
    Some(SolvePathResult {
        optimal_input: best_input,
        profit: best_profit,
        hop_outputs,
        consumed_inputs,
        ..Default::default()
    })
}

/// The shared integer-verification order: an ascending `center-3 ..= center+3`
/// scan (candidate set clamped at 1) where the earliest max-profit candidate
/// wins ties. Returns the input and its profit, or `None` when no candidate is
/// profitable.
///
/// All four stable arms use this one order. The outward `center±delta` walk it
/// replaces was measured against it on the golden-parity matrix and each arm's
/// drift-net corpus: profits were identical to within a single wei, this scan
/// was never worse (and a wei better on a few paths), and its worst-case
/// residual matched the walk's — so the simpler scan is the survivor.
fn ascending_sweep(simulate: &impl Fn(U256) -> U256, center: U256) -> Option<(U256, U256)> {
    const SEARCH_RADIUS: u64 = 3;
    let one = U256::from(1u64);
    let start = center.saturating_sub(U256::from(SEARCH_RADIUS)).max(one);
    let end = center + U256::from(SEARCH_RADIUS) + one;

    let mut best_input = U256::ZERO;
    let mut best_profit = U256::ZERO;
    let mut cand = start;
    while cand <= end {
        let output = simulate(cand);
        let profit = output.saturating_sub(cand);
        if profit > best_profit {
            best_profit = profit;
            best_input = cand;
        }
        if cand == U256::MAX {
            break;
        }
        cand += one;
    }

    if best_profit.is_zero() {
        None
    } else {
        Some((best_input, best_profit))
    }
}

/// Solve an all-V2-or-Balancer-weighted (no CL hops, ≥1 weighted) path with
/// the shared golden-section template (`solve_golden_section_path_int`).
///
/// Per-hop simulation calls
/// `degenbot_math::balancer::weighted_math::calc_out_given_in`. The
/// V2-equivalent precheck treats each weighted hop as a constant-product
/// hop — exact for 50/50 pools, approximate for other weights (good enough
/// for bracket narrowing).
fn solve_balancer_weighted_path_int(resolved: &ResolvedMixedPath) -> Option<SolvePathResult> {
    use degenbot_math::v2::IntHopState;

    let hops = &resolved.hops;
    if hops.len() < 2 {
        return None;
    }

    // V2-equivalent IntHopState per hop: weighted hops use
    // balance_in/balance_out + the retained fee fraction; V2 hops pass
    // through unchanged.
    let v2_equiv: Vec<IntHopState> = hops
        .iter()
        .map(balancer_weighted_hop_v2_equiv)
        .collect::<Option<Vec<_>>>()?;

    solve_golden_section_path_int(
        &v2_equiv,
        &|x| simulate_balancer_weighted_path(x, hops),
        &|x| simulate_balancer_weighted_path_outputs(x, hops),
    )
}

/// Build the V2-equivalent [`IntHopState`] for a hop in a Balancer
/// weighted solve path. Weighted hops use the raw balances as reserves
/// and convert the fee (`swap_fee` as fraction of 1e18 → retained fraction
/// `gamma_numer/fee_denom`). V2 hops pass through unchanged. Returns `None`
/// for any non-V2/non-weighted hop.
fn balancer_weighted_hop_v2_equiv(hop: &ResolvedHop) -> Option<degenbot_math::v2::IntHopState> {
    use degenbot_math::v2::IntHopState;
    match hop {
        ResolvedHop::V2 { state } => Some(state.clone()),
        ResolvedHop::BalancerWeighted { state } => {
            // swap_fee is the fee fraction as parts of 1e18.
            // gamma = 1e18 - swap_fee; IntHopState wants gamma_numer/fee_denom.
            let one_e18 = U256::from(10u64).pow(U256::from(18u64));
            let gamma = one_e18.saturating_sub(state.swap_fee);
            let gn: u64 = gamma.try_into().ok()?;
            let fd: u64 = one_e18.try_into().ok()?;
            Some(IntHopState::new(
                state.balance_in,
                state.balance_out,
                gn,
                fd,
            ))
        }
        _ => None,
    }
}

/// V2-equivalent [`IntHopState`] for a Balancer stable hop. Used by the
/// Möbius precheck to narrow the golden-section bracket. `StableSwap` ≠
/// constant product, but at high amplification it's locally V2-like.
fn balancer_stable_hop_v2_equiv(hop: &ResolvedHop) -> Option<degenbot_math::v2::IntHopState> {
    use degenbot_math::v2::IntHopState;
    match hop {
        ResolvedHop::V2 { state } => Some(state.clone()),
        ResolvedHop::BalancerStable { state } => {
            let one_e18 = U256::from(10u64).pow(U256::from(18u64));
            let gamma = one_e18.saturating_sub(state.swap_fee);
            let gn: u64 = gamma.try_into().ok()?;
            let fd: u64 = one_e18.try_into().ok()?;
            Some(IntHopState::new(
                state.balances[state.token_index_in],
                state.balances[state.token_index_out],
                gn,
                fd,
            ))
        }
        _ => None,
    }
}

/// Simulate a Balancer-weighted-or-V2 path, returning only the final output.
#[must_use]
pub(crate) fn simulate_balancer_weighted_path(x: U256, hops: &[ResolvedHop]) -> U256 {
    simulate_balancer_weighted_path_outputs(x, hops)
        .last()
        .copied()
        .unwrap_or(U256::ZERO)
}

/// Simulate a Balancer-weighted-or-V2 path, returning per-hop outputs.
fn simulate_balancer_weighted_path_outputs(x: U256, hops: &[ResolvedHop]) -> Vec<U256> {
    let mut amount = x;
    let mut outputs = Vec::with_capacity(hops.len());
    for hop in hops {
        if amount.is_zero() {
            outputs.push(U256::ZERO);
            continue;
        }
        let out = match hop {
            ResolvedHop::V2 { state } => state.swap(amount).unwrap_or(U256::ZERO),
            ResolvedHop::BalancerWeighted { state } => {
                simulate_balancer_weighted_hop(amount, state)
            }
            _ => U256::ZERO,
        };
        outputs.push(out);
        amount = out;
    }
    outputs
}

/// Evaluate a single Balancer weighted hop's output via
/// `degenbot_math::balancer::weighted_math::calc_out_given_in`. The swap
/// fee is subtracted from the input first (Balancer convention:
/// `amount_in_after_fee = amount_in - mul_up(amount_in, swap_fee)`),
/// then the invariant math runs on the after-fee amount. Returns
/// `U256::ZERO` on overflow / error (matching the defensive fallback in
/// `mixed::families::solidly::simulate_solidly_hop`).
fn simulate_balancer_weighted_hop(amount_in: U256, hop: &BalancerWeightedHopState) -> U256 {
    use degenbot_math::balancer::fixed_point::mul_up;
    use degenbot_math::balancer::weighted_math::calc_out_given_in;
    if amount_in.is_zero() {
        return U256::ZERO;
    }
    // Upscale input to 18-decimal fixed-point (plain integer multiply,
    // NOT mul_down — scaling factors are integer multipliers, not FXP).
    let amount_in_upscaled = amount_in.saturating_mul(hop.scaling_factor_in);
    // Subtract swap fee: amount_in_after_fee = amount_in - mul_up(amount_in, swap_fee).
    let Ok(fee_amount) = mul_up(amount_in_upscaled, hop.swap_fee) else {
        return U256::ZERO;
    };
    let amount_in_after_fee = amount_in_upscaled.saturating_sub(fee_amount);
    let Ok(amount_out_upscaled) = calc_out_given_in(
        hop.balance_in,
        hop.weight_in,
        hop.balance_out,
        hop.weight_out,
        amount_in_after_fee,
        hop.pow_version,
    ) else {
        return U256::ZERO;
    };
    // Downscale output from 18-decimal to native decimals (plain integer
    // division, NOT div_down — matches Python's `amount // scaling_factor`).
    if hop.scaling_factor_out.is_zero() {
        return U256::ZERO;
    }
    amount_out_upscaled / hop.scaling_factor_out
}

/// Balancer stable solve — the shared golden-section template
/// (`solve_golden_section_path_int`) over the stable math leaf
/// (`calc_out_given_in` with a pre-computed invariant).
fn solve_balancer_stable_path_int(resolved: &ResolvedMixedPath) -> Option<SolvePathResult> {
    use degenbot_math::v2::IntHopState;

    let hops = &resolved.hops;

    // V2-equivalent IntHopState per hop: stable hops use
    // balances[idx_in]/balances[idx_out] + the retained fee fraction;
    // V2 hops pass through unchanged.
    let v2_equiv: Vec<IntHopState> = hops
        .iter()
        .map(balancer_stable_hop_v2_equiv)
        .collect::<Option<Vec<_>>>()?;

    solve_golden_section_path_int(
        &v2_equiv,
        &|x| simulate_balancer_stable_path(x, hops),
        &|x| simulate_balancer_stable_path_outputs(x, hops),
    )
}

/// Simulate the full path and return the final output.
fn simulate_balancer_stable_path(x: U256, hops: &[ResolvedHop]) -> U256 {
    let outputs = simulate_balancer_stable_path_outputs(x, hops);
    outputs.last().copied().unwrap_or(U256::ZERO)
}

/// Simulate the full path and return each hop's output.
fn simulate_balancer_stable_path_outputs(x: U256, hops: &[ResolvedHop]) -> Vec<U256> {
    let mut amount = x;
    let mut outputs = Vec::with_capacity(hops.len());
    for hop in hops {
        if amount.is_zero() {
            outputs.push(U256::ZERO);
            continue;
        }
        let out = match hop {
            ResolvedHop::V2 { state } => state.swap(amount).unwrap_or(U256::ZERO),
            ResolvedHop::BalancerStable { state } => simulate_balancer_stable_hop(amount, state),
            _ => U256::ZERO,
        };
        outputs.push(out);
        amount = out;
    }
    outputs
}

/// Simulate a single Balancer stable hop (upscaling, fee, math,
/// downscaling).
fn simulate_balancer_stable_hop(amount_in: U256, hop: &BalancerStableHopState) -> U256 {
    use degenbot_math::balancer::fixed_point::mul_up;
    use degenbot_math::balancer::stable_math::calc_out_given_in;
    if amount_in.is_zero() {
        return U256::ZERO;
    }
    let amount_in_upscaled = amount_in.saturating_mul(hop.scaling_factor_in);
    let Ok(fee_amount) = mul_up(amount_in_upscaled, hop.swap_fee) else {
        return U256::ZERO;
    };
    let amount_in_after_fee = amount_in_upscaled.saturating_sub(fee_amount);
    let Ok(amount_out_upscaled) = calc_out_given_in(
        hop.amp,
        &hop.balances,
        hop.token_index_in,
        hop.token_index_out,
        amount_in_after_fee,
        hop.invariant,
    ) else {
        return U256::ZERO;
    };
    if hop.scaling_factor_out.is_zero() {
        return U256::ZERO;
    }
    amount_out_upscaled / hop.scaling_factor_out
}

/// Curve stableswap solve — the shared golden-section template
/// (`solve_golden_section_path_int`) over the Curve stableswap math leaf
/// (`stableswap_get_y` + fee/rate conversion).
fn solve_curve_path_int(resolved: &ResolvedMixedPath) -> Option<SolvePathResult> {
    use degenbot_math::v2::IntHopState;

    let hops = &resolved.hops;

    // V2-equivalent IntHopState per hop: Curve hops use xp[idx_in]/xp[idx_out]
    // + the retained fee fraction; V2 hops pass through unchanged.
    let v2_equiv: Vec<IntHopState> = hops
        .iter()
        .map(curve_hop_v2_equiv)
        .collect::<Option<Vec<_>>>()?;

    solve_golden_section_path_int(&v2_equiv, &|x| simulate_curve_path(x, hops), &|x| {
        simulate_curve_path_outputs(x, hops)
    })
}

/// V2-equivalent [`IntHopState`] for a Curve stableswap hop.
fn curve_hop_v2_equiv(hop: &ResolvedHop) -> Option<degenbot_math::v2::IntHopState> {
    use degenbot_math::v2::IntHopState;
    match hop {
        ResolvedHop::V2 { state } => Some(state.clone()),
        ResolvedHop::CurveStableswap { state } => {
            // Curve fee is fee/fee_denom; gamma = 1 - fee/fee_denom.
            // IntHopState wants gamma_numer/fee_denom.
            let gn: u64 = state.fee_denom.saturating_sub(state.fee).try_into().ok()?;
            let fd: u64 = state.fee_denom.try_into().ok()?;
            Some(IntHopState::new(
                state.xp[state.token_index_in],
                state.xp[state.token_index_out],
                gn,
                fd,
            ))
        }
        _ => None,
    }
}

/// Simulate the full path and return the final output.
fn simulate_curve_path(x: U256, hops: &[ResolvedHop]) -> U256 {
    let outputs = simulate_curve_path_outputs(x, hops);
    outputs.last().copied().unwrap_or(U256::ZERO)
}

/// Simulate the full path and return each hop's output.
fn simulate_curve_path_outputs(x: U256, hops: &[ResolvedHop]) -> Vec<U256> {
    let mut amount = x;
    let mut outputs = Vec::with_capacity(hops.len());
    for hop in hops {
        if amount.is_zero() {
            outputs.push(U256::ZERO);
            continue;
        }
        let out = match hop {
            ResolvedHop::V2 { state } => state.swap(amount).unwrap_or(U256::ZERO),
            ResolvedHop::CurveStableswap { state } => simulate_curve_hop(amount, state),
            _ => U256::ZERO,
        };
        outputs.push(out);
        amount = out;
    }
    outputs
}

/// Simulate a single Curve stableswap hop (rate-adjusted XP, `get_y`,
/// fee+rate conversion).
fn simulate_curve_hop(amount_in: U256, hop: &CurveStableswapHopState) -> U256 {
    use degenbot_math::curve::stableswap::stableswap_get_y;
    if amount_in.is_zero() {
        return U256::ZERO;
    }
    let i = hop.token_index_in;
    let j = hop.token_index_out;
    // Rate-adjust the input: x = xp[i] + dx * rate_in / PRECISION
    let x =
        hop.xp[i].saturating_add(amount_in.saturating_mul(hop.rate_multiplier_in) / hop.precision);
    let Ok(y) = stableswap_get_y(
        i,
        j,
        x,
        &hop.xp,
        hop.amp,
        hop.n_coins,
        hop.a_precision,
        hop.y_variant,
        hop.d_variant,
    ) else {
        return U256::ZERO;
    };
    // dy = xp[j] - y - 1
    let dy_raw = hop.xp[j].saturating_sub(y).saturating_sub(U256::from(1u64));
    // Fee on dy, then rate convert: (dy - fee) * PRECISION / rate_out
    let fee = dy_raw.saturating_mul(hop.fee) / hop.fee_denom;
    let dy_after_fee = dy_raw.saturating_sub(fee);
    // Rate-convert from XP-space to native token space
    dy_after_fee.saturating_mul(hop.precision) / hop.rate_multiplier_out
}

// ---------------------------------------------------------------------------
// Profit-envelope gate wiring tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod gate_tests {
    use super::*;

    use crate::mixed::families::cl::{cl_hop_lines, cl_to_math, solve_walkable_path_int};
    use crate::mixed::ResolvedMixedPath;
    use degenbot_math::v2::IntHopState;
    use degenbot_uniswap::dex_identity::DexVariant;

    fn v2_hop(r_in: u64, r_out: u64) -> ResolvedHop {
        ResolvedHop::V2 {
            state: IntHopState::new(U256::from(r_in), U256::from(r_out), 997_000, 1_000_000),
        }
    }

    /// A trivially profitable V2 round trip: 1000 A→B then B→A with the
    /// second pool's price shifted so the round trip nets positive output.
    fn profitable_path() -> ResolvedMixedPath {
        ResolvedMixedPath {
            hops: vec![v2_hop(1_000_000, 1_000_000), v2_hop(1_010_000, 990_000)],
            valid: true,
            state_nonces: vec![1, 2],
            max_update_block: 0,
        }
    }

    #[test]
    fn gate_disabled_matches_ungated_result() {
        let p = profitable_path();
        assert_eq!(
            solve_path_with_min_profit(&p, U256::from(u64::MAX), &GateDeps::offline()).result,
            solve_path_inner(&p, &GateDeps::offline(), None).0,
            "offline deps (no prefix cache) must be bit-for-bit identical"
        );
    }

    #[test]
    fn gate_huge_floor_skips_and_counts() {
        let p = profitable_path();
        let r = solve_path_with_min_profit(&p, U256::MAX, &GateDeps::offline());
        assert!(
            r.result.is_none(),
            "infinite floor skips every screened path"
        );
        let stats = r.gate_stats;
        assert_eq!(stats.skipped, 1, "skip must be counted");
        assert_eq!(stats.evaluated, 1);
        assert_eq!(stats.unsupported, 0);
        assert!(matches!(r.envelope, Envelope::Bound(_)));
        assert_eq!(r.stats.sims, 0, "gate skip carries no walk simulations");
    }

    #[test]
    fn gate_zero_floor_skips_provably_unprofitable_path() {
        // Identical V2 pools both ways: fees guarantee the round trip can
        // never net positive, so the envelope bound must be ZERO and the
        // path must be skipped even at floor 0.
        let p = ResolvedMixedPath {
            hops: vec![v2_hop(1_000_000, 1_000_000), v2_hop(1_000_000, 1_000_000)],
            valid: true,
            state_nonces: vec![1, 2],
            max_update_block: 0,
        };
        let r = solve_path_with_min_profit(&p, U256::ZERO, &GateDeps::offline());
        assert!(
            r.result.is_none(),
            "fee-dominated round trip must be skipped at floor 0"
        );
        let stats = r.gate_stats;
        assert_eq!(stats.skipped, 1);
        assert_eq!(stats.evaluated, 1);
    }

    // -----------------------------------------------------------------
    // Soundness proptests: the new bounds must point-wise dominate
    // the REAL family math leaves (independent oracle = the same
    // `simulate_*_hop` the solver walks). A bound that ever dips BELOW the
    // true output would skip a profitable path -> WRONG. proptest over
    // random reserves/weights/amounts exercises the slope + cap lines.
    // -----------------------------------------------------------------
    use crate::profit_envelope::{path_output_bound_at, HopMath};
    use degenbot_math::balancer::constants::ONE;
    use proptest::prelude::*;

    proptest! {
        /// Curve stableswap: the reserve cap (`xp[out]·PRECISION/rate_out` = the
        /// native output-token reserve) must dominate the real Curve leaf output
        /// for every amount — guards the rate-multiplier unit conversion.
        #[test]
        fn m6776w_curve_reserve_cap_dominates_real_leaf(
            bal_in in 1_000_000u64..1_000_000_000_000u64,
            bal_out in 1_000_000u64..1_000_000_000_000u64,
            x in 0u64..1_000_000_000_000u64,
            token_in in 0u8..2u8,
        ) {
            use degenbot_math::curve::stableswap::{DVariant, YVariant};
            let precision = U256::from(1_000_000_000_000_000_000u64); // 1e18
            // rate multiplier for a 6-decimal token = 10^30 (10^(18+12)).
            let rate = U256::from(1_000_000_000u64)
                * U256::from(1_000_000_000u64)
                * U256::from(1_000_000_000_000u64); // 1e9 * 1e9 * 1e12 = 1e30
            let (i, j) = if token_in == 0 { (0usize, 1usize) } else { (1, 0) };
            let balances = [
                if i == 0 { bal_in } else { bal_out },
                if i == 1 { bal_in } else { bal_out },
            ];
            // xp[k] = balance[k] * rate / precision
            let xp: Vec<U256> = balances
                .iter()
                .map(|&b| U256::from(b).saturating_mul(rate) / precision)
                .collect();
            // Sanity: the caller-side cap formula xp[j]*precision/rate_out
            // equals the native reserve (exact: 1e30/1e18=1e12, no rounding).
            let caller_cap = xp[j].saturating_mul(precision) / rate;
            prop_assert!(
                caller_cap == U256::from(balances[j]),
                "curve cap unit mismatch: caller {caller_cap} != reserve {}",
                U256::from(balances[j])
            );
            let hop = HopMath::ReserveCap {
                reserve_out: U256::from(balances[j]),
            };
            let bound = path_output_bound_at(&[Some(hop)], &U256::from(x), &crate::runtime::SolveRuntimeConfig::default())
                .unwrap_or(U256::ZERO);
            let state = crate::mixed::CurveStableswapHopState {
                amp: U256::from(1000u64),
                a_precision: U256::from(100u64),
                xp: xp.clone(),
                token_index_in: i,
                token_index_out: j,
                n_coins: U256::from(2u64),
                fee: U256::from(4_000_000u64),
                fee_denom: U256::from(10_000_000_000u64),
                precision,
                rate_multiplier_in: rate,
                rate_multiplier_out: rate,
                y_variant: YVariant::Standard,
                d_variant: DVariant::Standard,
            };
            let true_out = simulate_curve_hop(U256::from(x), &state);
            if x > 0 && true_out.is_zero() {
                return Ok(()); // leaf rejected the input; not soundness-relevant
            }
            prop_assert!(
                bound >= true_out,
                "curve cap {bound} < true {true_out} (bin={bal_in},bout={bal_out},x={x},t={token_in})"
            );
        }

        /// Balancer weighted: the concave tangent (entry marginal rate) + the
        /// reserve asymptote must dominate `calc_out_given_in` for every amount.
        #[test]
        fn m6776w_balancer_weighted_bound_dominates_real_leaf(
            b_in in 1u64..1_000_000_000u64,
            b_out in 1u64..1_000_000_000u64,
            w_in_pct in 10u32..90u32,
            x in 0u64..1_000_000_000u64,
        ) {
            let weight_in = U256::from(w_in_pct) * ONE / U256::from(100u32);
            let weight_out = ONE - weight_in;
            let state = crate::mixed::BalancerWeightedHopState {
                balance_in: U256::from(b_in),
                balance_out: U256::from(b_out),
                weight_in,
                weight_out,
                swap_fee: U256::from(3u64) * ONE / U256::from(1000u64),
                pow_version: degenbot_math::balancer::constants::PowVersion::V2,
                scaling_factor_in: U256::from(1u64),
                scaling_factor_out: U256::from(1u64),
            };
            let hop = HopMath::Weighted {
                balance_in: state.balance_in,
                balance_out: state.balance_out,
                weight_in: state.weight_in,
                weight_out: state.weight_out,
                scaling_in: state.scaling_factor_in,
                scaling_out: state.scaling_factor_out,
            };
            let bound = path_output_bound_at(&[Some(hop)], &U256::from(x), &crate::runtime::SolveRuntimeConfig::default())
                .unwrap_or(U256::ZERO);
            let true_out = simulate_balancer_weighted_hop(U256::from(x), &state);
            // Skip cases the math leaf itself rejects (returns 0 with x>0):
            // those are not soundness-relevant.
            if x > 0 && true_out.is_zero() {
                return Ok(());
            }
            prop_assert!(
                bound >= true_out,
                "weighted bound {bound} < true {true_out} (bin={b_in},bout={b_out},w={w_in_pct},x={x})"
            );
        }
    }

    #[test]
    fn solidly_and_stableswap_hops_are_now_screened_not_unsupported() {
        // Solidly/Curve/Balancer hops now carry rigorous (or sound
        // reserve-cap) bounds, so the path is EVALUATED by the gate rather
        // than SOLVED unscreened. A single Solidly-stable hop yields a
        // `Some` bound (reserve cap) → `evaluated`, NOT `unsupported`.
        let solidly = ResolvedHop::SolidlyStable {
            state: crate::mixed::SolidlyHopState {
                reserves_0: U256::from(1_000_000u64),
                reserves_1: U256::from(1_000_000u64),
                decimals_0: U256::from(1_000_000_000_000_000_000u64),
                decimals_1: U256::from(1_000_000_000_000_000_000u64),
                token_in: 0,
                fee_numer: U256::from(3u64),
                fee_denom: U256::from(1000u64),
                stable: true,
                variant: DexVariant::AerodromeV2Stable,
            },
        };
        let p = ResolvedMixedPath {
            hops: vec![solidly],
            valid: false, // invalid anyway; only counter behavior matters
            state_nonces: vec![],
            max_update_block: 0,
        };
        let r = solve_path_with_min_profit(&p, U256::ZERO, &GateDeps::offline());
        let stats = r.gate_stats;
        assert_eq!(stats.unsupported, 0);
        assert_eq!(stats.evaluated, 1);
    }

    /// The EXACT entry surface each [`HopType`]'s row must register: the
    /// family's gate projection, envelope line builder, and path solve.
    ///
    /// ADR-071 D3 moved family exhaustiveness from the compiler into the
    /// `family_dispatch_table_...` test, because the dispatch table is DATA.
    /// This `match` gives the compiler its nag back for the one fact the
    /// table cannot carry: it has NO wildcard arm, so adding a `HopType`
    /// variant fails compilation of the test until the new variant's
    /// registration is named here.
    fn expected_entry(ht: HopType) -> (&'static str, ToMathFn, LinesFn, SolveFn) {
        match ht {
            HopType::V2 => ("v2", v2_to_math, v2_hop_lines, solve_exact_v2_entry),
            HopType::V3 | HopType::V4 => ("cl", cl_to_math, cl_hop_lines, solve_walkable_path_int),
            HopType::SolidlyStable => (
                "solidly",
                solidly_to_math,
                solidly_hop_lines,
                solve_solidly_entry,
            ),
            HopType::BalancerWeighted => (
                "weighted",
                weighted_to_math,
                weighted_hop_lines,
                solve_balancer_weighted_entry,
            ),
            HopType::BalancerStable => (
                "balancer_stable",
                stable_to_math,
                balancer_stable_hop_lines,
                solve_balancer_stable_entry,
            ),
            HopType::CurveStableswap => (
                "curve_stableswap",
                curve_to_math,
                curve_hop_lines,
                solve_curve_entry,
            ),
        }
    }

    /// Guard that [`HopType::ALL`] lists every variant exactly once.
    ///
    /// The or-pattern names every variant, so a new variant fails compilation
    /// of THIS match; the count assertion then catches a variant that is
    /// named in the enum but left out of (or duplicated in) `HopType::ALL`.
    fn assert_all_variants_listed() {
        let mut covered = 0usize;
        for &ht in HopType::ALL {
            match ht {
                HopType::V2
                | HopType::V3
                | HopType::V4
                | HopType::SolidlyStable
                | HopType::BalancerWeighted
                | HopType::BalancerStable
                | HopType::CurveStableswap => covered += 1,
            }
        }
        assert_eq!(
            covered, 7,
            "HopType::ALL must list every HopType variant exactly once"
        );
    }

    /// Pin [`family_row`]'s row selection directly — every row of the accepted
    /// [`FAMILY_DISPATCH`] table plus the `WALKABLE_ROW` default — so a future
    /// row reorder or predicate edit cannot silently change the selection.
    ///
    /// WHY the FOUR refusal rows and the default exist: a path that pairs a
    /// Solidly, Balancer, or Curve hop with a CL hop — or with a second such
    /// family — has no single engine. The per-hop `swap_fn` solves are
    /// incompatible with CL tick-range enumeration (Python rejects these too),
    /// so such a path MUST be refused, not silently composed. The rows above
    /// each refusal row are the narrower accept rows; the refusal rows are the
    /// `allowed: None` catch-alls that fire only once those fail. `WALKABLE_ROW`
    /// is the fallback — the mixed V2+CL case, the ONLY cross-family
    /// composition the walk supports — and a refusal that fell through to it
    /// would walk an unsupported path. The empty-set case is defensive: an
    /// empty family set is vacuously `⊆ {V2}`, so it selects the V2 row exactly
    /// as the old vacuous `all_v2` did.
    #[test]
    fn family_dispatch_table_selects_the_row_the_old_ladder_selected() {
        let set =
            |families: &[HopType]| -> BTreeSet<HopType> { families.iter().copied().collect() };

        // Every case names the SELECTED ROW by its `label` token: the row IS
        // the registration (ADR-071 D3), so the label is the row's identity.
        let cases: &[(&[HopType], &str)] = &[
            // V2 row: every hop V2 (a SET, so a duplicate V2 collapses).
            (&[HopType::V2], "v2"),
            (&[HopType::V2, HopType::V2], "v2"),
            // CL row: every hop CL (V3/V4).
            (&[HopType::V3], "cl"),
            (&[HopType::V4], "cl"),
            (&[HopType::V3, HopType::V4], "cl"),
            // Solidly accept row: V2 ∪ Solidly with ≥1 Solidly hop. A lone
            // Solidly path is the family's own len<2 concern, not dispatch's.
            (&[HopType::V2, HopType::SolidlyStable], "solidly"),
            (&[HopType::SolidlyStable], "solidly"),
            // Solidly refusal row: Solidly beside a CL hop (or both CLs).
            (&[HopType::SolidlyStable, HopType::V3], "unsupported"),
            (
                &[HopType::SolidlyStable, HopType::V3, HopType::V4],
                "unsupported",
            ),
            // Weighted accept row, then its refusal row beside a CL hop.
            (&[HopType::V2, HopType::BalancerWeighted], "weighted"),
            (&[HopType::BalancerWeighted, HopType::V3], "unsupported"),
            // Stable accept row, then its refusal row beside a CL hop and
            // beside a second non-V2 family.
            (&[HopType::V2, HopType::BalancerStable], "balancer_stable"),
            (
                &[HopType::BalancerStable, HopType::BalancerWeighted],
                "unsupported",
            ),
            (
                &[
                    HopType::V2,
                    HopType::BalancerStable,
                    HopType::BalancerWeighted,
                ],
                "unsupported",
            ),
            // Curve accept row, then its refusal row beside a CL hop.
            (&[HopType::V2, HopType::CurveStableswap], "curve_stableswap"),
            (&[HopType::CurveStableswap, HopType::V3], "unsupported"),
            // Walkable default: mixed V2+CL is the only cross-family walk; the
            // set is unordered, so the hop order must not matter.
            (&[HopType::V2, HopType::V3], "walkable"),
            (&[HopType::V2, HopType::V3, HopType::V4], "walkable"),
            (&[HopType::V3, HopType::V2], "walkable"),
            // Defensive empty set: vacuously ⊆ {V2} → the V2 row.
            (&[], "v2"),
        ];

        for (families, expected) in cases {
            assert_eq!(
                family_row(&set(families)).label,
                *expected,
                "family_row({families:?}) selected the wrong row"
            );
        }

        // Exhaustiveness moves from the compiler to this named assertion
        // (ADR-071 D3): EVERY family's entry surface — the gate projection,
        // the envelope line builder, and the path solve its row registers —
        // must appear in the table, registering the EXACT fn the family owns.
        // A dropped row, a refusal stub, or a CROSS-WIRED live builder (e.g.
        // the CL row pointing at `weighted_hop_lines`) fails here: the check
        // is POSITIVE identity, not merely "not the refusal stub".
        //
        // The compiler's nag is restored by `expected_entry`, whose exhaustive
        // `match` fails compilation on a new `HopType` variant until its
        // registration is named; `assert_all_variants_listed` guards that
        // `HopType::ALL` carries every variant.
        assert_all_variants_listed();
        for &hop_type in HopType::ALL {
            let (label, to_math, lines, solve) = expected_entry(hop_type);
            let row = family_row_for(hop_type);
            assert_eq!(row.label, label, "{hop_type:?} entry row label");
            assert!(
                std::ptr::fn_addr_eq(row.to_math, to_math),
                "{hop_type:?} entry row must register the family's projection, not a \
                 cross-wired or refusal pointer"
            );
            assert!(
                std::ptr::fn_addr_eq(row.lines, lines),
                "{hop_type:?} entry row must register the family's line builder, not a \
                 cross-wired or refusal pointer"
            );
            assert!(
                std::ptr::fn_addr_eq(row.solve, solve),
                "{hop_type:?} entry row must register the family's path solve, not a \
                 cross-wired or refusal pointer"
            );
        }
    }
}
