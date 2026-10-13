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

use crate::mixed::families::balancer_stable::{
    balancer_stable_hop_lines, solve_balancer_stable_entry, stable_to_math,
};
use crate::mixed::families::balancer_weighted::{
    solve_balancer_weighted_entry, weighted_hop_lines, weighted_to_math,
};
use crate::mixed::families::cl::{cl_hop_lines, cl_to_math, solve_walkable_path_int};
use crate::mixed::families::curve_stableswap::{curve_hop_lines, curve_to_math, solve_curve_entry};
use crate::mixed::families::solidly::{solidly_hop_lines, solidly_to_math, solve_solidly_entry};
use crate::profit_envelope::{
    path_profit_bound_with_floor, refuse_lines, reserve_cap_hop_lines, v2_hop_lines, Envelope,
    GateDeps, GateStats, HopMath, Line, PathBoundLines, PrefixCache,
};
use crate::runtime::SolveRuntimeConfig;

use crate::mixed::{
    HopType, ResolvedHop, ResolvedMixedPath, SolveArmOutcome, SolvePathResult, INT128_MAX,
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
    /// Set only when a solve surface declined without walking (see
    /// [`crate::cl::WalkRefusal`]); `None` on every walked outcome,
    /// profitable or not. The envelope gate's rejection rides [`Self::envelope`].
    pub refusal: Option<crate::cl::WalkRefusal>,
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
                    refusal: None,
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
    let arm = solve_path_inner(resolved, gate, walk_env.as_ref());
    SolveOutcome {
        result: arm.result,
        refusal: arm.refusal,
        envelope,
        stats: arm.stats,
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

/// The refusal rows' projection: no hop view (an unmapped hop poisons the
/// path). Never reached through a family row's `to_math`.
fn refuse_to_math(_hop: &ResolvedHop) -> Option<HopMath<'_>> {
    None
}

/// The family's path-level solve entry (ADR-071 D3): maps a resolved path to
/// its result plus walk telemetry. Dispatch runs ONCE per solve, never per
/// candidate input (ADR-059 D6), so a static fn pointer costs nothing in the
/// hot loop.
pub(crate) type SolveFn =
    fn(&ResolvedMixedPath, &GateDeps<'_>, Option<&PathBoundLines>) -> SolveArmOutcome;

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

/// The refusal rows' path entry: no engine for the pairing → the named
/// refusal, never a silent empty result.
fn refuse_path(
    _resolved: &ResolvedMixedPath,
    _gate: &GateDeps<'_>,
    _walk_env: Option<&PathBoundLines>,
) -> SolveArmOutcome {
    SolveArmOutcome::refused(crate::cl::WalkRefusal::NotWalkable)
}

/// The V2 family's path entry: the integer-exact Möbius solve.
fn solve_exact_v2_entry(
    resolved: &ResolvedMixedPath,
    _gate: &GateDeps<'_>,
    _walk_env: Option<&PathBoundLines>,
) -> SolveArmOutcome {
    let int_hops: Vec<_> = resolved
        .hops
        .iter()
        .filter_map(ResolvedHop::as_v2_state)
        .cloned()
        .collect();
    if int_hops.len() == resolved.hops.len() {
        SolveArmOutcome::walked(
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
        SolveArmOutcome::refused(crate::cl::WalkRefusal::NotWalkable)
    }
}

#[must_use]
#[hotpath::measure(label = "mixed.solve_path_inner")]
pub fn solve_path_inner(
    resolved: &ResolvedMixedPath,
    gate: &GateDeps<'_>,
    walk_env: Option<&crate::profit_envelope::PathBoundLines>,
) -> SolveArmOutcome {
    // An invalid (partially-resolved) path has hops missing — don't solve.
    if !resolved.valid {
        return SolveArmOutcome::refused(crate::cl::WalkRefusal::Unresolved);
    }
    // The distinct hop families present in this path — the dispatch key,
    // computed ONCE. [`family_row`] is a set-membership test against it; see
    // [`FAMILY_DISPATCH`] for the ordered row selection.
    let families: BTreeSet<HopType> = resolved.hops.iter().map(ResolvedHop::hop_type).collect();

    // The row lookup IS the dispatch (ADR-071 D3): the selected family's path
    // entry is a fn pointer on the row, so no central `match` arm names it.
    let arm = (family_row(&families).solve)(resolved, gate, walk_env);

    // V4 int128 guard: reject paths where any V4 hop's consumed input or
    // output exceeds int128_max. V4's toBalanceDelta() calls toInt128() on
    // swap amounts — if either doesn't fit, V4 reverts with SafeCastOverflow.
    if let Some(ref r) = arm.result {
        for (i, hop) in resolved.hops.iter().enumerate() {
            if matches!(hop, ResolvedHop::V4 { .. }) {
                let consumed = r.consumed_inputs.get(i).copied().unwrap_or(U256::ZERO);
                let output = r.hop_outputs.get(i).copied().unwrap_or(U256::ZERO);
                if consumed > INT128_MAX || output > INT128_MAX {
                    // A hard reject AFTER the walk (not a refusal): the
                    // stats show the walk ran and its value is unexecutable
                    // on-chain.
                    return SolveArmOutcome::walked(None, arm.stats);
                }
            }
        }
    }

    let result = arm.result.map(|mut r| {
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
    SolveArmOutcome {
        result,
        refusal: arm.refusal,
        stats: arm.stats,
    }
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
            solve_path_inner(&p, &GateDeps::offline(), None).result,
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
