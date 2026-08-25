//! Pure solve + simulate family for mixed multi-hop paths (ADR-015).
//!
//! Receiver-free: `solve_path(&ResolvedMixedPath) -> Option<SolvePathResult>`
//! is the dispatcher; the `solve_*_path_int` arms are golden-section / mixed
//! Möbius searches over the math leaves; the `simulate_*_hop` leaves are the
//! per-hop swap functions the searches call. Relocated from
//! `degenbot-bot/src/solvers/arb_engine/solver_dispatch.rs` (where they were
//! `impl ArbitrageEngine` associated fns with `#[allow(unused_self)]`).
//!
//! The CL hop uses `crate::mobius_v3_int::*` (same crate); the V2 + CL
//! Möbius arms call `crate::mobius_int::*` / `crate::mobius_int_exact::*`.

use std::sync::OnceLock;

use alloy::primitives::{U256, U512};

use crate::profit_envelope::{path_profit_bound, HopMath};
use crate::profit_envelope::{GATE_EVALUATED, GATE_SKIPPED, GATE_UNSUPPORTED};

use crate::mixed::{
    BalancerStableHopState, BalancerWeightedHopState, CurveStableswapHopState, ResolvedHop,
    ResolvedMixedPath, SolidlyHopState, SolvePathResult, INT128_MAX,
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
#[must_use]
pub fn solve_path(resolved: &ResolvedMixedPath) -> Option<SolvePathResult> {
    solve_path_with_min_profit(resolved, U256::ZERO)
}

/// [`solve_path`] with the profit-envelope gate active at floor `min_profit`:
/// when the rigorous upper bound on path profit falls below the floor, the
/// path is provably unprofitable and returns `None` WITHOUT running any
/// walk. See CONTEXT.md → "Profit envelope gate".
#[must_use]
pub fn solve_path_with_min_profit(
    resolved: &ResolvedMixedPath,
    min_profit: U256,
) -> Option<SolvePathResult> {
    solve_path_gated(resolved, min_profit, gate_enabled())
}

/// Gate logic split out for direct testing (env flags are process-global and
/// untestable in a shared test binary).
fn solve_path_gated(
    resolved: &ResolvedMixedPath,
    min_profit: U256,
    enabled: bool,
) -> Option<SolvePathResult> {
    if !enabled {
        return solve_path_inner(resolved);
    }
    let views: Vec<Option<crate::profit_envelope::HopMath<'_>>> = resolved
        .hops
        .iter()
        .map(|h| match h {
            ResolvedHop::V2 { state } => Some(HopMath::V2(state)),
            ResolvedHop::V3 { int_seq, .. } | ResolvedHop::V4 { int_seq, .. } => {
                Some(HopMath::Cl(int_seq))
            }
            _ => None,
        })
        .collect();
    match path_profit_bound(&views) {
        // Unsupported families are SOLVED unscreened, never skipped.
        None => GATE_UNSUPPORTED.with(|c| c.set(c.get() + 1)),
        Some(bound) => {
            GATE_EVALUATED.with(|c| c.set(c.get() + 1));
            // `<=`: a path whose rigorous max profit cannot STRICTLY exceed the
            // floor is unexecutable at that floor (dispatch discards zero-profit
            // results downstream), so skipping is outcome-identical and saves
            // the walk.
            if bound <= min_profit {
                GATE_SKIPPED.with(|c| c.set(c.get() + 1));
                return None;
            }
        }
    }
    solve_path_inner(resolved)
}

fn gate_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED
        .get_or_init(|| std::env::var_os("DEGENBOT_SOLVER_PROFIT_GATE").is_some_and(|v| v == "1"))
}

#[must_use]
#[expect(clippy::too_many_lines)]
pub fn solve_path_inner(resolved: &ResolvedMixedPath) -> Option<SolvePathResult> {
    // An invalid (partially-resolved) path has hops missing — don't solve.
    if !resolved.valid {
        return None;
    }
    let all_v2 = resolved
        .hops
        .iter()
        .all(|h| matches!(h, ResolvedHop::V2 { .. }));
    let all_cl = resolved.hops.iter().all(|h| h.as_int_sequence().is_some());
    let has_solidly = resolved
        .hops
        .iter()
        .any(|h| matches!(h, ResolvedHop::SolidlyStable { .. }));
    let all_v2_or_solidly = resolved.hops.iter().all(|h| {
        matches!(
            h,
            ResolvedHop::V2 { .. } | ResolvedHop::SolidlyStable { .. }
        )
    });
    let has_balancer_weighted = resolved
        .hops
        .iter()
        .any(|h| matches!(h, ResolvedHop::BalancerWeighted { .. }));
    let all_v2_or_weighted = resolved.hops.iter().all(|h| {
        matches!(
            h,
            ResolvedHop::V2 { .. } | ResolvedHop::BalancerWeighted { .. }
        )
    });
    let has_balancer_stable = resolved
        .hops
        .iter()
        .any(|h| matches!(h, ResolvedHop::BalancerStable { .. }));
    let all_v2_or_stable = resolved.hops.iter().all(|h| {
        matches!(
            h,
            ResolvedHop::V2 { .. } | ResolvedHop::BalancerStable { .. }
        )
    });
    let has_curve = resolved
        .hops
        .iter()
        .any(|h| matches!(h, ResolvedHop::CurveStableswap { .. }));
    let all_v2_or_curve = resolved.hops.iter().all(|h| {
        matches!(
            h,
            ResolvedHop::V2 { .. } | ResolvedHop::CurveStableswap { .. }
        )
    });

    let result = if all_v2 {
        let int_hops: Vec<_> = resolved
            .hops
            .iter()
            .filter_map(ResolvedHop::as_v2_state)
            .cloned()
            .collect();
        if int_hops.len() == resolved.hops.len() {
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
                })
        } else {
            None
        }
    } else if all_cl {
        // V3-V3, V4-V4, V3-V4, V4-V3, V3-V3-V3, etc: all concentrated-liquidity
        let int_sequences: Vec<_> = resolved
            .hops
            .iter()
            .filter_map(ResolvedHop::as_int_sequence)
            .collect();
        let cl_profiles: Vec<_> = resolved
            .hops
            .iter()
            .filter_map(ResolvedHop::as_word_profiles)
            .collect();
        if int_sequences.len() >= 2 {
            crate::mobius_v3_int::int_solve_cl_path_with_profiles(&int_sequences, &cl_profiles).map(
                |(optimal_input, _profit, hop_outputs)| {
                    // consumed_inputs[0] = optimal_input (first hop always consumes
                    // its full input for single-range paths; no partial fill).
                    // consumed_inputs[i>0] = hop_outputs[i-1] (the previous hop's
                    // output becomes this hop's input — matching the pipeline:
                    // V3 output flows into V4 as amountSpecified).
                    let mut consumed_inputs = Vec::with_capacity(hop_outputs.len());
                    consumed_inputs.push(optimal_input);
                    for i in 1..hop_outputs.len() {
                        consumed_inputs.push(hop_outputs[i - 1]);
                    }
                    let profit = hop_outputs
                        .last()
                        .copied()
                        .unwrap_or(U256::ZERO)
                        .saturating_sub(consumed_inputs[0]);
                    SolvePathResult {
                        optimal_input,
                        profit,
                        hop_outputs,
                        consumed_inputs,
                        ..Default::default()
                    }
                },
            )
        } else {
            None
        }
    } else if all_v2_or_solidly && has_solidly {
        // All-V2-or-Solidly with ≥1 Solidly hop — the two-stage Möbius
        // precheck + golden-section solve (task DMPSNG). Scope (p):
        // Solidly mixed with CL is rejected below.
        solve_solidly_path_int(resolved)
    } else if has_solidly {
        // A Solidly hop alongside a CL hop — out of scope (p). The
        // Solidly solve is a per-hop `swap_fn` walk incompatible with
        // CL tick-range enumeration; Python rejects these too.
        None
    } else if all_v2_or_weighted && has_balancer_weighted {
        // All-V2-or-Balancer-weighted with ≥1 weighted hop — the
        // Möbius precheck + golden-section solve over the Balancer
        // weighted math leaf. Scope: weighted mixed with CL is
        // rejected below (same scope rule as Solidly+CL).
        solve_balancer_weighted_path_int(resolved)
    } else if has_balancer_weighted {
        // A weighted hop alongside a CL hop — out of scope (same
        // rationale as Solidly+CL).
        None
    } else if all_v2_or_stable && has_balancer_stable {
        // All-V2-or-Balancer-stable with ≥1 stable hop — the
        // Möbius precheck + golden-section solve over the Balancer
        // stable math leaf. Scope: stable mixed with CL or weighted is
        // rejected below.
        solve_balancer_stable_path_int(resolved)
    } else if has_balancer_stable {
        // A stable hop alongside a CL or weighted hop — out of scope.
        None
    } else if all_v2_or_curve && has_curve {
        // All-V2-or-Curve with ≥1 Curve hop — the Möbius precheck +
        // golden-section solve over the Curve stableswap math leaf.
        solve_curve_path_int(resolved)
    } else if has_curve {
        // A Curve hop alongside a CL or Balancer hop — out of scope.
        None
    } else {
        // Mixed V2 + CL (V3 or V4)
        solve_mixed_path_int(resolved)
    };

    // V4 int128 guard: reject paths where any V4 hop's consumed input or
    // output exceeds int128_max. V4's toBalanceDelta() calls toInt128() on
    // swap amounts — if either doesn't fit, V4 reverts with SafeCastOverflow.
    if let Some(ref r) = result {
        for (i, hop) in resolved.hops.iter().enumerate() {
            if matches!(hop, ResolvedHop::V4 { .. }) {
                let consumed = r.consumed_inputs.get(i).copied().unwrap_or(U256::ZERO);
                let output = r.hop_outputs.get(i).copied().unwrap_or(U256::ZERO);
                if consumed > INT128_MAX || output > INT128_MAX {
                    return None;
                }
            }
        }
    }

    result.map(|mut r| {
        r.state_nonces.clone_from(&resolved.state_nonces);
        // Capture solver pool state for diagnostic cross-referencing.
        let pool_states: Vec<String> = resolved
            .hops
            .iter()
            .map(|hop| match hop {
                ResolvedHop::V3 { int_seq, .. } | ResolvedHop::V4 { int_seq, .. } => {
                    if let Some(range) = int_seq.ranges.first() {
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
    })
}

/// Solve a mixed V2 + CL (V3 or V4) path using integer-exact Möbius solver.
///
/// Uses the pre-built `IntV3TickRangeSequence` from `resolve_path`,
/// which was constructed directly from U256 values (no f64 conversion).
/// V3 and V4 hops produce the same type — `IntV3TickRangeSequence`.
///
/// The sequence-based solver enumerates CL ending ranges and computes
/// the optimal input for each piece, validating with crossing-aware
/// simulation. This eliminates false positives from single-range
/// approximation when swaps exceed the current tick range capacity.
fn solve_mixed_path_int(resolved: &ResolvedMixedPath) -> Option<SolvePathResult> {
    if resolved.hops.len() < 2 {
        return None;
    }

    // Check that this is actually a mixed path (both V2 and CL hops)
    let has_v2 = resolved
        .hops
        .iter()
        .any(|h| matches!(h, ResolvedHop::V2 { .. }));
    let has_cl = resolved.hops.iter().any(|h| h.as_int_sequence().is_some());
    if !has_v2 || !has_cl {
        return None; // not a mixed path — should be handled by other dispatches
    }

    // Build hop_order and adapter arrays from the enum
    let hop_order: Vec<bool> = resolved
        .hops
        .iter()
        .map(|h| matches!(h, ResolvedHop::V2 { .. }))
        .collect();
    let v2_hops: Vec<Option<degenbot_math::v2::IntHopState>> = resolved
        .hops
        .iter()
        .map(|h| h.as_v2_state().cloned())
        .collect();
    let int_v3_sequences: Vec<Option<crate::mobius_v3_int::IntV3TickRangeSequence>> = resolved
        .hops
        .iter()
        .map(|h| h.as_int_sequence().cloned())
        .collect();

    crate::mobius_v3_int::exact_solve_mixed_path_n(&v2_hops, &int_v3_sequences, &hop_order).map(
        |(optimal_input, profit, hop_outputs)| {
            // consumed_inputs[0] = optimal_input (first hop consumes full input).
            // consumed_inputs[i>0] = hop_outputs[i-1] (previous hop's output
            // becomes this hop's input).
            let mut consumed_inputs = Vec::with_capacity(hop_outputs.len());
            consumed_inputs.push(optimal_input);
            for i in 1..hop_outputs.len() {
                consumed_inputs.push(hop_outputs[i - 1]);
            }
            SolvePathResult {
                optimal_input,
                profit,
                hop_outputs,
                consumed_inputs,
                ..Default::default()
            }
        },
    )
}

/// Solve an all-V2-or-Solidly (no CL hops, ≥1 Solidly) path using the
/// two-stage Solidly solve: a V2-equivalent Möbius precheck narrows the
/// golden-section bracket around the Möbius optimum (±5x), then 25
/// golden-section iterations refine the optimum, then integer
/// verification scans ±3 candidates and picks the max-profit input.
///
/// Faithful port of `arbitrage.solvers.solidly_stable.SolidlyStableSolver`
/// (the `_solve_golden_section` branch with `swap_fn` set). The Möbius
/// precheck early-outs unprofitable paths before the expensive search.
fn solve_solidly_path_int(resolved: &ResolvedMixedPath) -> Option<SolvePathResult> {
    use crate::mobius_int::compute_int_mobius_coefficients;
    use crate::mobius_int_exact::compute_mobius_model_optimal_input;
    use degenbot_math::v2::IntHopState;

    let hops = &resolved.hops;
    if hops.len() < 2 {
        return None;
    }

    // V2-equivalent IntHopState per hop: Solidly hops orient reserves by
    // `token_in` and convert the fee (SolidlyHopState stores the fee
    // fraction `fee_numer/fee_denom`; IntHopState wants the retained
    // fraction `gamma_numer/fee_denom`, so `gamma_numer = fee_denom -
    // fee_numer`). V2 hops pass through unchanged.
    let v2_equiv: Vec<IntHopState> = hops
        .iter()
        .map(solidly_hop_v2_equiv)
        .collect::<Option<Vec<_>>>()?;

    // --- Profitability precheck (V2-equivalent Möbius) ---
    let coeffs = compute_int_mobius_coefficients(&v2_equiv).ok()?;
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
        return solidly_brute_force_best(hops, x_low);
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
        crate::mobius_int_exact::u512_to_u256_internal(scaled)
    };
    let mut x1 = x_high - phi_span(x_low, x_high);
    let mut x2 = x_low + phi_span(x_low, x_high);
    let mut p1 = simulate_solidly_path(x1, hops).saturating_sub(x1);
    let mut p2 = simulate_solidly_path(x2, hops).saturating_sub(x2);

    for _ in 0..SOLIDLY_GOLDEN_SECTION_ITERATIONS {
        if p1 < p2 {
            x_low = x1;
            x1 = x2;
            p1 = p2;
            x2 = x_low + phi_span(x_low, x_high);
            p2 = simulate_solidly_path(x2, hops).saturating_sub(x2);
        } else {
            x_high = x2;
            x2 = x1;
            p2 = p1;
            x1 = x_high - phi_span(x_low, x_high);
            p1 = simulate_solidly_path(x1, hops).saturating_sub(x1);
        }
    }

    let x_opt = (x_low + x_high) / U256::from(2u64);
    solidly_brute_force_best(hops, x_opt)
}

/// Number of golden-section refinement iterations (mirrors Python's
/// `SolidlyStableSolver.GOLDEN_SECTION_ITERATIONS`).
const SOLIDLY_GOLDEN_SECTION_ITERATIONS: usize = 25;

/// Integer verification: scan `±SOLIDLY_INTEGER_SEARCH_RADIUS` candidates
/// around `center` (plus one past the upper edge) and pick the max-profit
/// input. Mirrors Python's `search_radius = 3` sweep. Returns `None` when
/// no candidate is profitable.
fn solidly_brute_force_best(hops: &[ResolvedHop], center: U256) -> Option<SolvePathResult> {
    const SEARCH_RADIUS: u64 = 3;
    let one = U256::from(1u64);
    let start = center.saturating_sub(U256::from(SEARCH_RADIUS)).max(one);
    let end = center + U256::from(SEARCH_RADIUS) + one;

    let mut best_input = U256::ZERO;
    let mut best_profit = U256::ZERO;
    let mut cand = start;
    while cand <= end {
        let output = simulate_solidly_path(cand, hops);
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
        return None;
    }

    // Re-simulate at best_input to record per-hop outputs.
    let hop_outputs = simulate_solidly_path_outputs(best_input, hops);
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

/// Build the V2-equivalent [`IntHopState`] for a hop in a Solidly solve
/// path. Solidly hops orient reserves by `token_in` and convert the fee
/// (fee fraction → retained fraction); V2 hops pass through unchanged.
/// Returns `None` for any non-V2/non-Solidly hop or when the Solidly fee
/// pair overflows `u64` (the path is then unsolvable here).
fn solidly_hop_v2_equiv(hop: &ResolvedHop) -> Option<degenbot_math::v2::IntHopState> {
    use degenbot_math::v2::IntHopState;
    match hop {
        ResolvedHop::V2 { state } => Some(state.clone()),
        ResolvedHop::SolidlyStable { state } => {
            let (reserve_in, reserve_out) = if state.token_in == 0 {
                (state.reserves_0, state.reserves_1)
            } else {
                (state.reserves_1, state.reserves_0)
            };
            // SolidlyHopState fee = fee fraction (fee_numer/fee_denom).
            // IntHopState fee = retained fraction (gamma_numer/fee_denom).
            let gamma_numer = state.fee_denom.saturating_sub(state.fee_numer);
            let gn: u64 = gamma_numer.try_into().ok()?;
            let fd: u64 = state.fee_denom.try_into().ok()?;
            Some(IntHopState::new(reserve_in, reserve_out, gn, fd))
        }
        _ => None,
    }
}

/// Simulate a Solidly-or-V2 path, returning only the final output.
/// Per-hop dispatch: Solidly hops call the `degenbot-solidly-math` leaf
/// selected by `(variant, stable)`; V2 hops reuse `IntHopState::swap`.
#[must_use]
pub fn simulate_solidly_path(x: U256, hops: &[ResolvedHop]) -> U256 {
    simulate_solidly_path_outputs(x, hops)
        .last()
        .copied()
        .unwrap_or(U256::ZERO)
}

/// Simulate a Solidly-or-V2 path, returning per-hop outputs.
/// `hop_outputs[i]` = output of hop `i` given `hop_outputs[i-1]` as input
/// (or `x` for `i==0`). Returns `U256::ZERO` per hop once the chain breaks
/// at a zero amount (matching the Python `_simulate_mixed_path_int`'s
/// early-out).
#[must_use]
fn simulate_solidly_path_outputs(x: U256, hops: &[ResolvedHop]) -> Vec<U256> {
    let mut amount = x;
    let mut outputs = Vec::with_capacity(hops.len());
    for hop in hops {
        if amount.is_zero() {
            outputs.push(U256::ZERO);
            continue;
        }
        let out = match hop {
            ResolvedHop::V2 { state } => state.swap(amount).unwrap_or(U256::ZERO),
            ResolvedHop::SolidlyStable { state } => simulate_solidly_hop(amount, state),
            // Non-V2/Solidly hops can't reach here — the solve dispatch
            // rejects Solidly+CL paths before calling this.
            _ => U256::ZERO,
        };
        outputs.push(out);
        amount = out;
    }
    outputs
}

/// Evaluate a single Solidly hop's output via the `degenbot-solidly-math`
/// leaf, selected by `(variant, stable)`. Returns `U256::ZERO` on
/// overflow / invalid `token_in` (the leaf's `Err` variants), matching the
/// Python `_simulate_mixed_path_int`'s defensive fallback.
fn simulate_solidly_hop(amount_in: U256, hop: &SolidlyHopState) -> U256 {
    use degenbot_math::solidly::{
        calc_exact_in_stable_camelot, calc_exact_in_stable_solidly, calc_exact_in_volatile,
    };
    use degenbot_uniswap::dex_identity::DexVariant;
    if amount_in.is_zero() {
        return U256::ZERO;
    }
    let out = match (hop.variant, hop.stable) {
        (DexVariant::AerodromeV2Stable | DexVariant::AerodromeV2Volatile, true) => {
            calc_exact_in_stable_solidly(
                amount_in,
                hop.token_in,
                hop.reserves_0,
                hop.reserves_1,
                hop.decimals_0,
                hop.decimals_1,
                hop.fee_numer,
                hop.fee_denom,
            )
        }
        (DexVariant::CamelotV2Stable | DexVariant::CamelotV2Volatile, true) => {
            calc_exact_in_stable_camelot(
                amount_in,
                hop.token_in,
                hop.reserves_0,
                hop.reserves_1,
                hop.decimals_0,
                hop.decimals_1,
                hop.fee_numer,
                hop.fee_denom,
            )
        }
        (_, false) => calc_exact_in_volatile(
            amount_in,
            hop.token_in,
            hop.reserves_0,
            hop.reserves_1,
            hop.fee_numer,
            hop.fee_denom,
        ),
        // A non-Solidly variant with `stable=true` is impossible — these
        // DEXes have no stable pool family. Falls back to zero output.
        _ => return U256::ZERO,
    };
    out.unwrap_or(U256::ZERO)
}

/// Resolve a path's pool refs into hop states and tick-range sequences.
/// Solve an all-V2-or-Balancer-weighted (no CL hops, ≥1 weighted) path
/// using the same two-stage template as the Solidly solve: a V2-equivalent
/// Möbius precheck narrows the golden-section bracket around the Möbius
/// optimum (±5x), then 25 golden-section iterations refine the optimum,
/// then integer verification scans ±3 candidates and picks the max-profit
/// input.
///
/// Per-hop simulation calls
/// `degenbot_math::balancer::weighted_math::calc_out_given_in`. The
/// V2-equivalent precheck treats each weighted hop as a constant-product
/// hop — exact for 50/50 pools, approximate for other weights (good enough
/// for bracket narrowing).
fn solve_balancer_weighted_path_int(resolved: &ResolvedMixedPath) -> Option<SolvePathResult> {
    use crate::mobius_int::compute_int_mobius_coefficients;
    use crate::mobius_int_exact::compute_mobius_model_optimal_input;
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

    // --- Profitability precheck (V2-equivalent Möbius) ---
    let coeffs = compute_int_mobius_coefficients(&v2_equiv).ok()?;
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
        return balancer_weighted_brute_force_best(hops, x_low);
    }

    // --- Golden-section search (25 iterations) ---
    let phi_num = U256::from(6_180_339_887_498_949u64); // phi * 1e16
    let phi_den = U256::from(10_000_000_000_000_000u64); // 1e16
    let phi_span = |lo: U256, hi: U256| -> U256 {
        let d = U512::from(hi - lo);
        let scaled = d * U512::from(phi_num) / U512::from(phi_den);
        crate::mobius_int_exact::u512_to_u256_internal(scaled)
    };
    let mut x1 = x_high - phi_span(x_low, x_high);
    let mut x2 = x_low + phi_span(x_low, x_high);
    let mut p1 = simulate_balancer_weighted_path(x1, hops).saturating_sub(x1);
    let mut p2 = simulate_balancer_weighted_path(x2, hops).saturating_sub(x2);

    for _ in 0..SOLIDLY_GOLDEN_SECTION_ITERATIONS {
        if p1 < p2 {
            x_low = x1;
            x1 = x2;
            p1 = p2;
            x2 = x_low + phi_span(x_low, x_high);
            p2 = simulate_balancer_weighted_path(x2, hops).saturating_sub(x2);
        } else {
            x_high = x2;
            x2 = x1;
            p2 = p1;
            x1 = x_high - phi_span(x_low, x_high);
            p1 = simulate_balancer_weighted_path(x1, hops).saturating_sub(x1);
        }
    }

    let x_opt = (x_low + x_high) / U256::from(2u64);
    balancer_weighted_brute_force_best(hops, x_opt)
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

/// Integer verification: scan `±SOLIDLY_INTEGER_SEARCH_RADIUS` candidates
/// around `center` and pick the max-profit input. Mirrors
/// `solidly_brute_force_best`'s sweep.
fn balancer_weighted_brute_force_best(
    hops: &[ResolvedHop],
    center: U256,
) -> Option<SolvePathResult> {
    const SEARCH_RADIUS: u64 = 3;
    let one = U256::from(1u64);
    let start = center.saturating_sub(U256::from(SEARCH_RADIUS)).max(one);
    let end = center + U256::from(SEARCH_RADIUS) + one;

    let mut best_input = U256::ZERO;
    let mut best_profit = U256::ZERO;
    let mut cand = start;
    while cand <= end {
        let output = simulate_balancer_weighted_path(cand, hops);
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
        return None;
    }

    let hop_outputs = simulate_balancer_weighted_path_outputs(best_input, hops);
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
/// `simulate_solidly_hop`).
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

/// Balancer stable solve — Möbius precheck + golden-section search,
/// mirroring `solve_balancer_weighted_path_int` with the stable math
/// leaf (`calc_out_given_in` with pre-computed invariant).
#[expect(clippy::too_many_lines)]
fn solve_balancer_stable_path_int(resolved: &ResolvedMixedPath) -> Option<SolvePathResult> {
    use crate::mobius_int::compute_int_mobius_coefficients;
    use crate::mobius_int_exact::compute_mobius_model_optimal_input;
    use degenbot_math::v2::IntHopState;

    let hops = &resolved.hops;

    // V2-equivalent IntHopState per hop: stable hops use
    // balances[idx_in]/balances[idx_out] + the retained fee fraction;
    // V2 hops pass through unchanged.
    let v2_equiv: Vec<IntHopState> = hops
        .iter()
        .map(balancer_stable_hop_v2_equiv)
        .collect::<Option<Vec<_>>>()?;

    let coeffs = compute_int_mobius_coefficients(&v2_equiv).ok()?;
    if !coeffs.is_profitable {
        return None;
    }
    let x_mobius = compute_mobius_model_optimal_input(&coeffs);

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
        let best_input = balancer_stable_brute_force_best(hops, x_low)?;
        let best_profit =
            simulate_balancer_stable_path(best_input, hops).saturating_sub(best_input);
        if best_profit.is_zero() {
            return None;
        }
        let hop_outputs = simulate_balancer_stable_path_outputs(best_input, hops);
        let consumed_inputs: Vec<U256> = std::iter::once(best_input)
            .chain(
                hop_outputs
                    .iter()
                    .take(hop_outputs.len().saturating_sub(1))
                    .copied(),
            )
            .collect();
        return Some(SolvePathResult {
            optimal_input: best_input,
            profit: best_profit,
            hop_outputs,
            consumed_inputs,
            ..Default::default()
        });
    }

    let phi_num = U256::from(6_180_339_887_498_949u64);
    let phi_den = U256::from(10_000_000_000_000_000u64);
    let phi_span = |lo: U256, hi: U256| -> U256 {
        let d = U512::from(hi - lo);
        let scaled = d * U512::from(phi_num) / U512::from(phi_den);
        crate::mobius_int_exact::u512_to_u256_internal(scaled)
    };
    let mut x1 = x_high - phi_span(x_low, x_high);
    let mut x2 = x_low + phi_span(x_low, x_high);
    let mut p1 = simulate_balancer_stable_path(x1, hops).saturating_sub(x1);
    let mut p2 = simulate_balancer_stable_path(x2, hops).saturating_sub(x2);

    for _ in 0..SOLIDLY_GOLDEN_SECTION_ITERATIONS {
        if p1 < p2 {
            x_low = x1;
            x1 = x2;
            p1 = p2;
            x2 = x_low + phi_span(x_low, x_high);
            p2 = simulate_balancer_stable_path(x2, hops).saturating_sub(x2);
        } else {
            x_high = x2;
            x2 = x1;
            p2 = p1;
            x1 = x_high - phi_span(x_low, x_high);
            p1 = simulate_balancer_stable_path(x1, hops).saturating_sub(x1);
        }
    }

    let x_opt = (x_low + x_high) / U256::from(2u64);
    let best_input = balancer_stable_brute_force_best(hops, x_opt)?;
    let best_profit = simulate_balancer_stable_path(best_input, hops).saturating_sub(best_input);
    if best_profit.is_zero() {
        return None;
    }
    let hop_outputs = simulate_balancer_stable_path_outputs(best_input, hops);
    let consumed_inputs: Vec<U256> = std::iter::once(best_input)
        .chain(
            hop_outputs
                .iter()
                .take(hop_outputs.len().saturating_sub(1))
                .copied(),
        )
        .collect();
    Some(SolvePathResult {
        optimal_input: best_input,
        profit: best_profit,
        hop_outputs,
        consumed_inputs,
        ..Default::default()
    })
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

/// ±3 integer brute-force verify sweep (matches the Solidly/weighted
/// template — ensures the golden-section result is a local max).
/// Returns `Some(input)` only if profit > 0.
fn balancer_stable_brute_force_best(hops: &[ResolvedHop], start: U256) -> Option<U256> {
    let mut best_profit = simulate_balancer_stable_path(start, hops).saturating_sub(start);
    let mut best_input = start;
    for delta in 1u64..=3 {
        let cand_up = best_input.saturating_add(U256::from(delta));
        let profit_up = simulate_balancer_stable_path(cand_up, hops).saturating_sub(cand_up);
        if profit_up > best_profit {
            best_profit = profit_up;
            best_input = cand_up;
        }
        if best_input >= U256::from(delta) {
            let cand_dn = best_input - U256::from(delta);
            let profit_dn = simulate_balancer_stable_path(cand_dn, hops).saturating_sub(cand_dn);
            if profit_dn > best_profit {
                best_profit = profit_dn;
                best_input = cand_dn;
            }
        }
    }
    if best_profit.is_zero() {
        None
    } else {
        Some(best_input)
    }
}

/// Curve stableswap solve — Möbius precheck + golden-section search,
/// mirroring the Balancer stable solve with the Curve stableswap math
/// leaf (`stableswap_get_y` + fee/rate conversion).
fn solve_curve_path_int(resolved: &ResolvedMixedPath) -> Option<SolvePathResult> {
    use crate::mobius_int::compute_int_mobius_coefficients;
    use crate::mobius_int_exact::compute_mobius_model_optimal_input;
    use degenbot_math::v2::IntHopState;

    let hops = &resolved.hops;

    // V2-equivalent IntHopState per hop: Curve hops use xp[idx_in]/xp[idx_out]
    // + the retained fee fraction; V2 hops pass through unchanged.
    let v2_equiv: Vec<IntHopState> = hops
        .iter()
        .map(curve_hop_v2_equiv)
        .collect::<Option<Vec<_>>>()?;

    let coeffs = compute_int_mobius_coefficients(&v2_equiv).ok()?;
    if !coeffs.is_profitable {
        return None;
    }
    let x_mobius = compute_mobius_model_optimal_input(&coeffs);

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
        let best_input = curve_brute_force_best(hops, x_low)?;
        return curve_build_result(hops, best_input);
    }

    let phi_num = U256::from(6_180_339_887_498_949u64);
    let phi_den = U256::from(10_000_000_000_000_000u64);
    let phi_span = |lo: U256, hi: U256| -> U256 {
        let d = U512::from(hi - lo);
        let scaled = d * U512::from(phi_num) / U512::from(phi_den);
        crate::mobius_int_exact::u512_to_u256_internal(scaled)
    };
    let mut x1 = x_high - phi_span(x_low, x_high);
    let mut x2 = x_low + phi_span(x_low, x_high);
    let mut p1 = simulate_curve_path(x1, hops).saturating_sub(x1);
    let mut p2 = simulate_curve_path(x2, hops).saturating_sub(x2);

    for _ in 0..SOLIDLY_GOLDEN_SECTION_ITERATIONS {
        if p1 < p2 {
            x_low = x1;
            x1 = x2;
            p1 = p2;
            x2 = x_low + phi_span(x_low, x_high);
            p2 = simulate_curve_path(x2, hops).saturating_sub(x2);
        } else {
            x_high = x2;
            x2 = x1;
            p2 = p1;
            x1 = x_high - phi_span(x_low, x_high);
            p1 = simulate_curve_path(x1, hops).saturating_sub(x1);
        }
    }

    let x_opt = (x_low + x_high) / U256::from(2u64);
    let best_input = curve_brute_force_best(hops, x_opt)?;
    curve_build_result(hops, best_input)
}

/// Build the full `SolvePathResult` from the best input.
fn curve_build_result(hops: &[ResolvedHop], best_input: U256) -> Option<SolvePathResult> {
    let best_profit = simulate_curve_path(best_input, hops).saturating_sub(best_input);
    if best_profit.is_zero() {
        return None;
    }
    let hop_outputs = simulate_curve_path_outputs(best_input, hops);
    let consumed_inputs: Vec<U256> = std::iter::once(best_input)
        .chain(
            hop_outputs
                .iter()
                .take(hop_outputs.len().saturating_sub(1))
                .copied(),
        )
        .collect();
    Some(SolvePathResult {
        optimal_input: best_input,
        profit: best_profit,
        hop_outputs,
        consumed_inputs,
        ..Default::default()
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

/// ±3 integer brute-force verify sweep.
fn curve_brute_force_best(hops: &[ResolvedHop], start: U256) -> Option<U256> {
    let mut best_profit = simulate_curve_path(start, hops).saturating_sub(start);
    let mut best_input = start;
    for delta in 1u64..=3 {
        let cand_up = best_input.saturating_add(U256::from(delta));
        let profit_up = simulate_curve_path(cand_up, hops).saturating_sub(cand_up);
        if profit_up > best_profit {
            best_profit = profit_up;
            best_input = cand_up;
        }
        if best_input >= U256::from(delta) {
            let cand_dn = best_input - U256::from(delta);
            let profit_dn = simulate_curve_path(cand_dn, hops).saturating_sub(cand_dn);
            if profit_dn > best_profit {
                best_profit = profit_dn;
                best_input = cand_dn;
            }
        }
    }
    if best_profit.is_zero() {
        None
    } else {
        Some(best_input)
    }
}

// ---------------------------------------------------------------------------
// Profit-envelope gate wiring tests (SU7MAE WNQB3V)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod gate_tests {
    use super::*;
    use crate::mixed::ResolvedMixedPath;
    use crate::profit_envelope::take_last_gate_stats;
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
            solve_path_gated(&p, U256::from(u64::MAX), false),
            solve_path_inner(&p),
            "flag OFF must be bit-for-bit identical"
        );
    }

    #[test]
    fn gate_huge_floor_skips_and_counts() {
        let p = profitable_path();
        let _ = take_last_gate_stats(); // clear thread counters
        let r = solve_path_gated(&p, U256::MAX, true);
        assert!(r.is_none(), "infinite floor skips every screened path");
        let stats = take_last_gate_stats();
        assert_eq!(stats.skipped, 1, "skip must be counted");
        assert_eq!(stats.evaluated, 1);
        assert_eq!(stats.unsupported, 0);
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
        let _ = take_last_gate_stats();
        let r = solve_path_gated(&p, U256::ZERO, true);
        assert!(
            r.is_none(),
            "fee-dominated round trip must be skipped at floor 0"
        );
        let stats = take_last_gate_stats();
        assert_eq!(stats.skipped, 1);
        assert_eq!(stats.evaluated, 1);
    }

    #[test]
    fn unsupported_family_counts_unsupported_and_still_solves() {
        // Solidly hops have no envelope yet; the path must still be SOLVED
        // (never skipped) and counted as unsupported.
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
        let _ = take_last_gate_stats();
        let _ = solve_path_gated(&p, U256::ZERO, true);
        let stats = take_last_gate_stats();
        assert_eq!(stats.unsupported, 1);
    }
}
