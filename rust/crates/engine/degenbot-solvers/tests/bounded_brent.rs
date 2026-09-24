//! Golden-trace parity against SciPy 1.17's `_minimize_scalar_bounded`, plus
//! analytic convergence / budget / degenerate-bracket behavior.
//!
//! The golden constants below are raw `x`, `fun`, `nfev`, `success` outputs of
//! the real SciPy bounded method (`options={'xatol': 1.0}`) over a
//! deterministic objective set. Regenerate them by running
//! `tests/fixtures/generate_bounded_brent_goldens.py` with
//! `uv run --with scipy==1.17 python ...`; the values are inlined here so a
//! regression fails at the exact comparison rather than in a fixture diff.
//!
//! Parity bar: `nfev` exactly, `success` equal, `x` bit-for-bit or within
//! `1e-8` relative, `fun` bit-for-bit or within `1e-8` scaled absolute. All
//! four pinned traces currently match SciPy bit-for-bit in both `x` and `fun`.

#![expect(clippy::doc_markdown, clippy::float_cmp, clippy::unreadable_literal)]

use degenbot_solvers::bounded_brent::{minimize_scalar_bounded, BrentMinimize};

struct Golden {
    name: &'static str,
    objective: fn(f64) -> f64,
    bounds: (f64, f64),
    x: f64,
    fun: f64,
    nfev: usize,
    success: bool,
}

const GOLDENS: &[Golden] = &[
    Golden {
        name: "SQUARE_SHIFTED",
        objective: |x| (x - 12345.678) * (x - 12345.678),
        bounds: (1.0, 20000.0),
        x: 12345.678,
        fun: 0.0,
        nfev: 6,
        success: true,
    },
    Golden {
        name: "SCALED_1E18_PLUS_LINEAR",
        objective: |x| 1e18 * (x / 12345.678 - 1.0) * (x / 12345.678 - 1.0) + 7.5e-3 * x,
        bounds: (1.0, 30000.0),
        x: 12345.678,
        fun: 92.592585,
        nfev: 6,
        success: true,
    },
    Golden {
        name: "QUARTIC_FLAT_TOP",
        objective: |x| {
            (x - 5000.0) * (x - 5000.0) * (x - 5000.0) * (x - 5000.0)
                + 1e-3 * (x - 5000.0) * (x - 5000.0)
        },
        bounds: (1.0, 10000.0),
        x: 4999.9998205100555,
        fun: 3.221767807329575e-11,
        nfev: 7,
        success: true,
    },
    Golden {
        name: "SHIFTED_LEFT_MIN",
        objective: |x| (x - 11111.25) * (x - 11111.25),
        bounds: (1.0, 200000.0),
        x: 11111.250000000007,
        fun: 5.293955920339377e-23,
        nfev: 6,
        success: true,
    },
];

fn relative_gap(got: f64, want: f64) -> f64 {
    if got.to_bits() == want.to_bits() {
        return 0.0;
    }
    (got - want).abs() / want.abs()
}

fn assert_x_close(got: f64, want: f64, name: &str) {
    let gap = relative_gap(got, want);
    assert!(
        gap <= 1e-8,
        "{name}: x diverged from SciPy: got {got:?} want {want:?} rel_gap={gap:e}"
    );
}

fn assert_fun_close(got: f64, want: f64, name: &str) {
    if got.to_bits() == want.to_bits() {
        return;
    }
    let tolerance = 1e-8 * want.abs().max(1.0);
    assert!(
        (got - want).abs() <= tolerance,
        "{name}: fun diverged from SciPy: got {got:?} want {want:?}"
    );
}

#[test]
fn golden_traces_match_scipy() {
    for golden in GOLDENS {
        let mut objective = golden.objective;
        let result: BrentMinimize =
            minimize_scalar_bounded(&mut objective, golden.bounds, 1.0, 500);
        assert_eq!(
            result.nfev, golden.nfev,
            "{}: nfev mismatch (SciPy nfev={})",
            golden.name, golden.nfev
        );
        assert_eq!(result.success, golden.success, "{}: success", golden.name);
        assert_x_close(result.x, golden.x, golden.name);
        assert_fun_close(result.fun, golden.fun, golden.name);
    }
}

#[test]
fn convex_quadratic_converges_within_xatol() {
    let mut objective = |x: f64| (x - 3.0) * (x - 3.0);
    let result = minimize_scalar_bounded(&mut objective, (-10.0, 10.0), 1e-8, 500);
    assert!(result.success);
    assert!(
        (result.x - 3.0).abs() <= 1e-4,
        "argmin off by {}",
        (result.x - 3.0).abs()
    );
}

#[test]
fn evaluation_budget_is_an_upper_bound() {
    let mut objective = |x: f64| (x - 3.0) * (x - 3.0);
    let result = minimize_scalar_bounded(&mut objective, (-1e6, 1e6), 1e-12, 3);
    assert!(result.nfev <= 3, "nfev {} exceeded budget 3", result.nfev);
    assert!(!result.success);
    assert_eq!(result.message, "Maximum number of function calls reached.");
}

#[test]
fn degenerate_bracket_matches_scipy() {
    let mut objective = |x: f64| (x - 5.0) * (x - 5.0);
    let result = minimize_scalar_bounded(&mut objective, (5.0, 5.0), 1.0, 500);
    assert_eq!(result.x, 5.0);
    assert_eq!(result.fun, 0.0);
    assert_eq!(result.nfev, 1);
    assert!(result.success);
    assert_eq!(result.message, "Solution found.");
}

#[test]
fn invalid_lower_above_upper_reports_failure() {
    let mut objective = |x: f64| x;
    let result = minimize_scalar_bounded(&mut objective, (5.0, 1.0), 1.0, 500);
    assert!(!result.success);
    assert_eq!(result.nfev, 0);
    assert_eq!(result.message, "The lower bound exceeds the upper bound.");
}

#[test]
fn non_finite_bounds_report_failure() {
    let mut objective = |x: f64| x;
    let result = minimize_scalar_bounded(&mut objective, (f64::INFINITY, 1.0), 1.0, 500);
    assert!(!result.success);
    assert_eq!(result.nfev, 0);
    assert_eq!(
        result.message,
        "Optimization bounds must be finite scalars."
    );
}

#[test]
fn nan_objective_reports_nan_result() {
    let mut objective = |_: f64| f64::NAN;
    let result = minimize_scalar_bounded(&mut objective, (0.0, 10.0), 1.0, 500);
    assert!(!result.success);
    assert_eq!(result.message, "NaN result encountered.");
}
