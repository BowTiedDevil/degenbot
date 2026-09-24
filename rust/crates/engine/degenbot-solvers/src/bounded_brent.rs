//! Bounded scalar minimizer — faithful port of SciPy 1.17's
//! `_minimize_scalar_bounded` (also exposed as `fminbound`), the
//! golden-section + parabolic-interpolation search over a shrinking bracket
//! behind `scipy.optimize.minimize_scalar(method='bounded')`.
//!
//! Transcribed expression-by-expression from SciPy 1.17
//! (`scipy/optimize/_optimize.py`, `_minimize_scalar_bounded`). `bracket` is
//! NOT an input: the bounded method ignores it and derives everything from
//! `bounds`.
//!
//! # Divergences from SciPy
//!
//! - Invalid bounds (`a > b`, or a non-finite bound) make SciPy raise
//!   `ValueError` before any objective evaluation. This value-only core has no
//!   error channel in its contract, so [`minimize_scalar_bounded`] returns a
//!   failed [`BrentMinimize`] (`nfev == 0`) carrying SciPy's exact message
//!   text; the caller decides whether that is an error.
//! - `np.maximum` propagates NaN (NaN if either operand is NaN) while Rust's
//!   `f64::max` returns the non-NaN operand. [`numpy_maximum`] reproduces the
//!   NumPy behavior so a NaN `xatol` follows the same path.
//!
//! # Non-finite objective returns
//!
//! Handled exactly as SciPy does: the search keeps comparing floats (a NaN
//! `fu` fails every `<=`, so the bracket update skips that point) and the
//! final `isnan(xf) | isnan(fx) | isnan(fu)` check yields the "NaN result
//! encountered." status.

#![expect(clippy::many_single_char_names)]

/// SciPy's default `maxiter` for the bounded method; `maxfun = maxiter`.
pub const DEFAULT_MAXFUN: usize = 500;

/// Result of [`minimize_scalar_bounded`].
#[derive(Clone, Debug)]
pub struct BrentMinimize {
    /// Estimated position of the minimum.
    pub x: f64,
    /// Objective value at [`BrentMinimize::x`].
    pub fun: f64,
    /// Number of objective evaluations performed.
    pub nfev: usize,
    /// Whether the search terminated on its tolerance criterion.
    pub success: bool,
    /// SciPy's status message for the terminating condition.
    pub message: String,
}

/// Minimize a scalar function on `bounds = (a, b)` with SciPy's bounded
/// Brent search.
///
/// `xatol` is the absolute tolerance on `x`; `maxfun` mirrors SciPy's
/// `maxfun` (`maxiter`) evaluation budget. See the module docs for the
/// invalid-bounds and non-finite-return stances.
#[expect(
    clippy::float_cmp,
    clippy::manual_midpoint,
    clippy::similar_names,
    clippy::too_many_lines,
    reason = "SciPy's expressions are transcribed literally: the exact `==` \
              float guards and `0.5 * (a + b)` midpoints are load-bearing for \
              bit-parity (`f64::midpoint` rounds differently), and the SciPy \
              names `fulc`/`nfc` are kept for side-by-side diffability."
)]
#[must_use]
pub fn minimize_scalar_bounded(
    f: &mut dyn FnMut(f64) -> f64,
    bounds: (f64, f64),
    xatol: f64,
    maxfun: usize,
) -> BrentMinimize {
    let (x1, x2) = bounds;

    if !(x1.is_finite() && x2.is_finite()) {
        return finalize(
            x1,
            f64::NAN,
            0,
            2,
            "Optimization bounds must be finite scalars.",
        );
    }
    if x1 > x2 {
        return finalize(
            x1,
            f64::NAN,
            0,
            2,
            "The lower bound exceeds the upper bound.",
        );
    }

    let sqrt_eps = 2.2e-16_f64.sqrt();
    let golden_mean = 0.5 * (3.0 - 5.0_f64.sqrt());

    let (mut a, mut b) = (x1, x2);
    let mut fulc = a + golden_mean * (b - a);
    let mut nfc = fulc;
    let mut xf = fulc;
    let mut rat = 0.0_f64;
    let mut e = 0.0_f64;
    let mut x;
    let mut fx = f(xf);
    let mut num: usize = 1;
    let mut fu = f64::INFINITY;
    let mut ffulc = fx;
    let mut fnfc = fx;
    let mut xm = 0.5 * (a + b);
    let mut tol1 = sqrt_eps * xf.abs() + xatol / 3.0;
    let mut tol2 = 2.0 * tol1;

    let mut flag = 0_u8;
    while (xf - xm).abs() > (tol2 - 0.5 * (b - a)) {
        let mut golden = true;

        if e.abs() > tol1 {
            golden = false;
            let r = (xf - nfc) * (fx - ffulc);
            let q = (xf - fulc) * (fx - fnfc);
            let mut p = (xf - fulc) * q - (xf - nfc) * r;
            let q = 2.0 * (q - r);
            if q > 0.0 {
                p = -p;
            }
            let q = q.abs();
            let r = e;
            e = rat;

            if p.abs() < (0.5 * q * r).abs() && p > q * (a - xf) && p < q * (b - xf) {
                rat = p / q;
                x = xf + rat;

                if (x - a) < tol2 || (b - x) < tol2 {
                    let si = scipy_sign(xm - xf) + f64::from(xm == xf);
                    rat = tol1 * si;
                }
            } else {
                golden = true;
            }
        }

        if golden {
            e = if xf >= xm { a - xf } else { b - xf };
            rat = golden_mean * e;
        }

        let si = scipy_sign(rat) + f64::from(rat == 0.0);
        x = xf + si * numpy_maximum(rat.abs(), tol1);
        fu = f(x);
        num += 1;

        if fu <= fx {
            if x >= xf {
                a = xf;
            } else {
                b = xf;
            }
            fulc = nfc;
            ffulc = fnfc;
            nfc = xf;
            fnfc = fx;
            xf = x;
            fx = fu;
        } else {
            if x < xf {
                a = x;
            } else {
                b = x;
            }
            if fu <= fnfc || nfc == xf {
                fulc = nfc;
                ffulc = fnfc;
                nfc = x;
                fnfc = fu;
            } else if fu <= ffulc || fulc == xf || fulc == nfc {
                fulc = x;
                ffulc = fu;
            }
        }

        xm = 0.5 * (a + b);
        tol1 = sqrt_eps * xf.abs() + xatol / 3.0;
        tol2 = 2.0 * tol1;

        if num >= maxfun {
            flag = 1;
            break;
        }
    }

    if xf.is_nan() || fx.is_nan() || fu.is_nan() {
        flag = 2;
    }

    finalize(xf, fx, num, flag, scipy_message(flag))
}

/// `np.sign` semantics: `-1`, `0`, `1`, and NaN for NaN input.
fn scipy_sign(value: f64) -> f64 {
    if value > 0.0 {
        1.0
    } else if value < 0.0 {
        -1.0
    } else if value == 0.0 {
        0.0
    } else {
        f64::NAN
    }
}

/// `np.maximum(a, b)` semantics: NaN propagates instead of being dropped.
fn numpy_maximum(lhs: f64, rhs: f64) -> f64 {
    if lhs.is_nan() || rhs.is_nan() {
        f64::NAN
    } else if lhs >= rhs {
        lhs
    } else {
        rhs
    }
}

/// SciPy's `_status_message` text for the bounded method's terminal flags.
fn scipy_message(flag: u8) -> &'static str {
    match flag {
        0 => "Solution found.",
        1 => "Maximum number of function calls reached.",
        _ => "NaN result encountered.",
    }
}

fn finalize(x: f64, fun: f64, nfev: usize, flag: u8, message: &str) -> BrentMinimize {
    BrentMinimize {
        x,
        fun,
        nfev,
        success: flag == 0,
        message: message.to_owned(),
    }
}
