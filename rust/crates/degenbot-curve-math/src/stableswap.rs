//! Curve StableSwap invariant solvers + step functions.
//!
//! Direct Rust ports of `src/degenbot/calculations/stableswap.py` (themselves
//! direct ports of the Vyper contract logic). All arithmetic is EVM-exact
//! integer math over [`U256`]; overflow (a Vyper revert) surfaces as
//! [`CurveMathError::Overflow`]; non-convergence within 255 iterations surfaces
//! as [`CurveMathError::NotConverged`].
//!
//! The variant enumerations ([`DVariant`], [`YVariant`], [`YDVariant`]) mirror
//! the Python `degenbot.curve.types` enums: the discriminants are 1-based
//! `auto()` values so `MyVariant::try_from_u8(python_enum.value)` round-trips.

use alloy::primitives::U256;
// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors from the Curve StableSwap math solvers.
///
/// Mirrors the Vyper contract's revert conditions: the Python oracle raises
/// `ValueError` (non-convergence) or `AssertionError` (index / safety
/// violations); the Rust port surfaces these as recoverable values so the
/// eventual Rust `get_dy` (ADR-005 slice 11c follow-on) can map them to typed
/// Python exceptions without catching a panic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurveMathError {
    /// A Newton iteration did not converge within 255 steps (Vyper revert).
    NotConverged,
    /// A uint256 arithmetic operation overflowed (Vyper checked-arithmetic revert).
    Overflow,
    /// An index argument was out of range (`i == j`, `j >= n_coins`, …).
    IndexOutOfBounds,
    /// A safety assertion failed (the `stableswap_newton_y` A/gamma/D/frac
    /// bounds — Vyper reverts these to guard against adversarial inputs).
    UnsafeValue,
}

type Result<T> = std::result::Result<T, CurveMathError>;

// ---------------------------------------------------------------------------
// Checked-arithmetic helpers (Vyper uses checked uint256)
// ---------------------------------------------------------------------------

#[inline]
fn checked_mul(a: U256, b: U256) -> Result<U256> {
    a.checked_mul(b).ok_or(CurveMathError::Overflow)
}

#[inline]
fn checked_add(a: U256, b: U256) -> Result<U256> {
    a.checked_add(b).ok_or(CurveMathError::Overflow)
}

#[inline]
fn checked_sub(a: U256, b: U256) -> Result<U256> {
    a.checked_sub(b).ok_or(CurveMathError::Overflow)
}

/// Vyper `//` — integer division. Vyper reverts on divide-by-zero; the Rust
/// port surfaces it as `Overflow` (a recoverable error, not a panic) so a
/// degenerate input can't bring down the process.
#[inline]
fn div(a: U256, b: U256) -> Result<U256> {
    if b.is_zero() {
        return Err(CurveMathError::Overflow);
    }
    Ok(a / b)
}

// ---------------------------------------------------------------------------
// Variant enums (mirror degenbot.curve.types: auto() 1-based)
// ---------------------------------------------------------------------------

/// Which D-step / D'-step variant to use. Mirrors `DVariant`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DVariant {
    /// `STANDARD` — calc_d + calc_dp.
    Standard = 1,
    /// `VARIANT_ALPHA` — calc_d_variant_alpha.
    VariantAlpha = 2,
    /// `VARIANT_ALPHA_DP_ALPHA` — calc_d_variant_alpha + calc_dp_variant_alpha.
    VariantAlphaDpAlpha = 3,
    /// `VARIANT_DP_ALPHA` — calc_dp_variant_alpha.
    VariantDpAlpha = 4,
    /// `VARIANT_BETA_DP` — calc_dp_variant_beta.
    VariantBetaDp = 5,
    /// `VARIANT_GAMMA_DP` — calc_dp_variant_gamma.
    VariantGammaDp = 6,
}

impl DVariant {
    /// Map a Python `DVariant.value` (1-based `auto()`) → the Rust enum.
    /// Returns `Err` for an unknown discriminant (a wiring error).
    #[must_use]
    pub fn try_from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Standard),
            2 => Some(Self::VariantAlpha),
            3 => Some(Self::VariantAlphaDpAlpha),
            4 => Some(Self::VariantDpAlpha),
            5 => Some(Self::VariantBetaDp),
            6 => Some(Self::VariantGammaDp),
            _ => None,
        }
    }
}

/// Which Y-step variant to use. Mirrors `YVariant`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YVariant {
    /// `STANDARD` — amp WITH A_PRECISION divisor + standard c/b.
    Standard = 1,
    /// `VARIANT_0` — amp WITHOUT A_PRECISION divisor.
    Variant0 = 2,
    /// `VARIANT_1` — amp WITHOUT A_PRECISION divisor (alternate).
    Variant1 = 3,
}

impl YVariant {
    /// Map a Python `YVariant.value` (1-based `auto()`) → the Rust enum.
    #[must_use]
    pub fn try_from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Standard),
            2 => Some(Self::Variant0),
            3 => Some(Self::Variant1),
            _ => None,
        }
    }

    /// Whether this variant omits the A_PRECISION divisor (matches the
    /// Python `y_variant in {VARIANT_0, VARIANT_1}` check).
    #[must_use]
    fn omits_a_precision(self) -> bool {
        matches!(self, Self::Variant0 | Self::Variant1)
    }
}

/// Which Y_D-step variant to use. Mirrors `YDVariant`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YDVariant {
    /// `STANDARD`.
    Standard = 1,
    /// `VARIANT_0`.
    Variant0 = 2,
}

impl YDVariant {
    /// Map a Python `YDVariant.value` (1-based `auto()`) → the Rust enum.
    #[must_use]
    pub fn try_from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Standard),
            2 => Some(Self::Variant0),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// D-step functions — single Newton step: d_new = d_func(d, d_p, s, a_nn, …)
// ---------------------------------------------------------------------------

/// Standard D step (Vyper): `(a_nn*s//a_prec + d_p*n) * d // ((a_nn-a_prec)*d//a_prec + (n+1)*d_p)`.
pub fn calc_d(
    a_nn: U256,
    s: U256,
    d: U256,
    d_p: U256,
    n_coins: U256,
    a_precision: U256,
) -> Result<U256> {
    let lhs = checked_add(
        checked_mul(a_nn, div(s, a_precision)?)?,
        checked_mul(d_p, n_coins)?,
    )?;
    let rhs_inner = div(
        checked_mul(checked_sub(a_nn, a_precision)?, d)?,
        a_precision,
    )?;
    let rhs = checked_add(
        rhs_inner,
        checked_mul(checked_add(n_coins, U256::from(1u64))?, d_p)?,
    )?;
    checked_mul(lhs, d).and_then(|v| div(v, rhs))
}

/// Variant-alpha D step: omits `a_precision` from the formula entirely.
/// `(a_nn*s + d_p*n) * d // ((a_nn-1)*d + (n+1)*d_p)`.
pub fn calc_d_variant_alpha(
    a_nn: U256,
    s: U256,
    d: U256,
    d_p: U256,
    n_coins: U256,
    _a_precision: U256,
) -> Result<U256> {
    let lhs = checked_add(checked_mul(a_nn, s)?, checked_mul(d_p, n_coins)?)?;
    let rhs = checked_add(
        checked_mul(checked_sub(a_nn, U256::from(1u64))?, d)?,
        checked_mul(checked_add(n_coins, U256::from(1u64))?, d_p)?,
    )?;
    checked_mul(lhs, d).and_then(|v| div(v, rhs))
}

// ---------------------------------------------------------------------------
// D'-step functions — d_p from the current D and xp values
// ---------------------------------------------------------------------------

/// Standard D' step: `d_p = d_p * d // (x * n_coins)` for each x in xp.
pub fn calc_dp(d: U256, mut d_p: U256, xp: &[U256], n_coins: U256) -> Result<U256> {
    for &x in xp {
        d_p = div(checked_mul(d_p, d)?, checked_mul(x, n_coins)?)?;
    }
    Ok(d_p)
}

/// Variant-alpha D' step: `d_p = d_p * d // (x * n_coins + 1)`.
pub fn calc_dp_variant_alpha(d: U256, mut d_p: U256, xp: &[U256], n_coins: U256) -> Result<U256> {
    for &x in xp {
        let denom = checked_add(checked_mul(x, n_coins)?, U256::from(1u64))?;
        d_p = div(checked_mul(d_p, d)?, denom)?;
    }
    Ok(d_p)
}

/// Variant-beta D' step: first two coins only, `d*d//x0 * d//x1 // n^2`.
pub fn calc_dp_variant_beta(d: U256, _d_p: U256, xp: &[U256], n_coins: U256) -> Result<U256> {
    let n_sq = checked_mul(n_coins, n_coins)?;
    let v = div(checked_mul(d, d)?, xp[0])?;
    let v = div(checked_mul(v, d)?, xp[1])?;
    div(v, n_sq)
}

/// Variant-gamma D' step: like beta but `n^n` instead of `n^2`.
pub fn calc_dp_variant_gamma(d: U256, _d_p: U256, xp: &[U256], n_coins: U256) -> Result<U256> {
    let n_to_n = n_coins.pow(n_coins);
    let v = div(checked_mul(d, d)?, xp[0])?;
    let v = div(checked_mul(v, d)?, xp[1])?;
    div(v, n_to_n)
}

// ---------------------------------------------------------------------------
// Iterative solvers
// ---------------------------------------------------------------------------

/// Solve for the Curve StableSwap invariant D using modified Newton's method.
///
/// Direct port of `stableswap_get_d`. Iterates until convergence (Δ ≤ 1) or
/// 255 steps ([`CurveMathError::NotConverged`]).
pub fn stableswap_get_d(
    xp: &[U256],
    amp: U256,
    n_coins: U256,
    a_precision: U256,
    d_variant: DVariant,
) -> Result<U256> {
    let mut d = xp.iter().copied().fold(U256::ZERO, U256::saturating_add);
    let s = d;
    if s.is_zero() {
        return Ok(U256::ZERO);
    }
    let a_nn = checked_mul(amp, n_coins)?;

    let n_plus_1 = checked_add(n_coins, U256::from(1u64))?;
    for _ in 0..255 {
        let d_prev = d;
        let mut d_p = d;
        d_p = dp_step(d_variant, d, d_p, xp, n_coins, n_plus_1)?;
        d = d_step(d_variant, a_nn, s, d, d_p, n_coins, a_precision)?;
        if d >= d_prev {
            if d - d_prev <= U256::from(1u64) {
                return Ok(d);
            }
        } else if d_prev - d <= U256::from(1u64) {
            return Ok(d);
        }
    }
    Err(CurveMathError::NotConverged)
}

/// Compute x[j] if one makes x[i] = x in a Curve StableSwap pool.
///
/// Direct port of `stableswap_get_y`. Solves the quadratic iteratively.
///
/// # Errors
///
/// Returns [`CurveMathError::IndexOutOfBounds`] if `i == j` or an index is out
/// of range; [`CurveMathError::NotConverged`] after 255 steps.
#[allow(clippy::too_many_arguments)] // mirrors the Vyper contract's 9-arg signature
pub fn stableswap_get_y(
    i: usize,
    j: usize,
    x: U256,
    xp: &[U256],
    amp: U256,
    n_coins: U256,
    a_precision: U256,
    y_variant: YVariant,
    d_variant: DVariant,
) -> Result<U256> {
    if i == j {
        return Err(CurveMathError::IndexOutOfBounds);
    }
    let n = n_coins.as_limbs()[0] as usize;
    if j >= n || i >= n {
        return Err(CurveMathError::IndexOutOfBounds);
    }

    let d = stableswap_get_d(xp, amp, n_coins, a_precision, d_variant)?;
    let mut c = d;
    let mut s = U256::ZERO;

    for coin_index in 0..n {
        let x_ = if coin_index == i {
            x
        } else if coin_index != j {
            xp[coin_index]
        } else {
            continue;
        };
        s = checked_add(s, x_)?;
        c = div(checked_mul(c, d)?, checked_mul(x_, n_coins)?)?;
    }

    let a_nn = checked_mul(amp, n_coins)?;
    let (c, b) = if y_variant.omits_a_precision() {
        let c = div(checked_mul(c, d)?, checked_mul(a_nn, n_coins)?)?;
        let b = checked_add(s, div(d, a_nn)?)?;
        (c, b)
    } else {
        let c = div(
            checked_mul(checked_mul(c, d)?, a_precision)?,
            checked_mul(a_nn, n_coins)?,
        )?;
        let b = checked_add(s, div(checked_mul(d, a_precision)?, a_nn)?)?;
        (c, b)
    };

    let mut y = d;
    for _ in 0..255 {
        let y_prev = y;
        y = div(
            checked_add(checked_mul(y, y)?, c)?,
            checked_sub(checked_add(checked_mul(U256::from(2u64), y)?, b)?, d)?,
        )?;
        if y > y_prev {
            if y - y_prev <= U256::from(1u64) {
                return Ok(y);
            }
        } else if y_prev - y <= U256::from(1u64) {
            return Ok(y);
        }
    }
    Err(CurveMathError::NotConverged)
}

/// Calculate y given A, xp, and D in a Curve StableSwap pool.
///
/// Direct port of `stableswap_get_y_d`. Used by `calc_token_amount` /
/// `calc_withdraw_one_coin`.
///
/// # Errors
///
/// Returns [`CurveMathError::IndexOutOfBounds`] if `i >= n_coins`;
/// [`CurveMathError::NotConverged`] after 255 steps.
pub fn stableswap_get_y_d(
    a: U256,
    i: usize,
    xp: &[U256],
    d: U256,
    n_coins: U256,
    a_precision: U256,
    yd_variant: YDVariant,
) -> Result<U256> {
    let n = n_coins.as_limbs()[0] as usize;
    if i >= n {
        return Err(CurveMathError::IndexOutOfBounds);
    }

    let mut c = d;
    let mut s = U256::ZERO;
    for coin_index in 0..n {
        if coin_index != i {
            let x = xp[coin_index];
            s = checked_add(s, x)?;
            c = div(checked_mul(c, d)?, checked_mul(x, n_coins)?)?;
        }
    }

    let a_nn = checked_mul(a, n_coins)?;
    let (b, c) = if yd_variant == YDVariant::Variant0 {
        let b = checked_add(s, div(checked_mul(d, a_precision)?, a_nn)?)?;
        let c = div(
            checked_mul(checked_mul(c, d)?, a_precision)?,
            checked_mul(a_nn, n_coins)?,
        )?;
        (b, c)
    } else {
        let b = checked_add(s, div(d, a_nn)?)?;
        let c = div(checked_mul(c, d)?, checked_mul(a_nn, n_coins)?)?;
        (b, c)
    };

    let mut y = d;
    for _ in 0..255 {
        let y_prev = y;
        y = div(
            checked_add(checked_mul(y, y)?, c)?,
            checked_sub(checked_add(checked_mul(U256::from(2u64), y)?, b)?, d)?,
        )?;
        if y > y_prev {
            if y - y_prev <= U256::from(1u64) {
                return Ok(y);
            }
        } else if y_prev - y <= U256::from(1u64) {
            return Ok(y);
        }
    }
    Err(CurveMathError::NotConverged)
}

/// Calculate xp[i] given other balances and invariant D, using Newton's method.
///
/// Direct port of `stableswap_newton_y`. Used by crypto (volatile) Curve pools.
///
/// # Errors
///
/// Returns [`CurveMathError::UnsafeValue`] if any safety assertion fails
/// (A/gamma/D/frac bounds); [`CurveMathError::NotConverged`] after 255 steps.
pub fn stableswap_newton_y(
    ann: U256,
    gamma: U256,
    xp: &[U256],
    d: U256,
    token_index: usize,
    n_coins: U256,
    a_multiplier: U256,
) -> Result<U256> {
    let n = n_coins.as_limbs()[0] as usize;
    let n_to_n = n_coins.pow(n_coins);

    // Safety checks (Vyper asserts).
    let lo = checked_sub(checked_mul(n_to_n, a_multiplier)?, U256::from(1u64))?;
    let hi = checked_add(
        checked_mul(checked_mul(U256::from(10_000u64), n_to_n)?, a_multiplier)?,
        U256::from(1u64),
    )?;
    if !(ann > lo && ann < hi) {
        return Err(CurveMathError::UnsafeValue);
    }
    if !(gamma > pow10(10) - U256::from(1u64) && gamma < pow10(16) + U256::from(1u64)) {
        return Err(CurveMathError::UnsafeValue);
    }
    if !(d > pow10(17) - U256::from(1u64) && d < pow10(15) * pow10(18) + U256::from(1u64)) {
        return Err(CurveMathError::UnsafeValue);
    }

    for index in 0..n {
        if index != token_index {
            let frac = div(checked_mul(xp[index], pow10(18))?, d)?;
            if !(frac > pow10(16) - U256::from(1u64) && frac < pow10(20) + U256::from(1u64)) {
                return Err(CurveMathError::UnsafeValue);
            }
        }
    }

    let exp18 = pow10(18);
    let exp14 = pow10(14);
    let exp16 = pow10(16);
    let exp20 = pow10(20);

    let mut y = div(d, n_coins)?;
    let mut k_0_i = exp18;
    let mut s_i = U256::ZERO;

    // Sort x with the token_index zeroed (high → low).
    let mut x_sorted: Vec<U256> = xp.to_vec();
    x_sorted[token_index] = U256::ZERO;
    x_sorted.sort_by(|a, b| b.cmp(a));

    let convergence_limit = x_sorted[0]
        .max(d)
        .checked_div(exp14)
        .unwrap_or(U256::ZERO)
        .max(U256::from(100u64));

    for j_ in 2..=n {
        let x_ = x_sorted[n - j_];
        y = div(checked_mul(y, d)?, checked_mul(x_, n_coins)?)?; // small _x first
        s_i = checked_add(s_i, x_)?;
    }

    for k_ in 0..(n.saturating_sub(1)) {
        k_0_i = div(checked_mul(checked_mul(k_0_i, x_sorted[k_])?, n_coins)?, d)?;
        // large _x first
    }

    for _ in 0..255 {
        let y_prev = y;

        let k_0 = div(checked_mul(checked_mul(k_0_i, y)?, n_coins)?, d)?;
        let s = checked_add(s_i, y)?;

        let mut g1k0 = checked_add(gamma, exp18)?;
        g1k0 = if g1k0 > k_0 {
            checked_add(checked_sub(g1k0, k_0)?, U256::from(1u64))?
        } else {
            checked_add(checked_sub(k_0, g1k0)?, U256::from(1u64))?
        };

        // mul1 = 10**18 * d // gamma * g1k0 // gamma * g1k0 * a_multiplier // ann
        // (left-associative: (((((10**18 * d) // gamma) * g1k0) // gamma) * g1k0) * a_multiplier) // ann)
        let t1 = div(checked_mul(exp18, d)?, gamma)?;
        let t2 = checked_mul(t1, g1k0)?;
        let t3 = div(t2, gamma)?;
        let t4 = checked_mul(t3, g1k0)?;
        let t5 = checked_mul(t4, a_multiplier)?;
        let mul1 = div(t5, ann)?;
        // mul2 = 10**18 + (2 * 10**18) * k_0 // g1k0
        //   = 10**18 + (((2*10**18) * k_0) // g1k0)
        let two_e18 = checked_mul(U256::from(2u64), exp18)?;
        let mul2 = checked_add(exp18, div(checked_mul(two_e18, k_0)?, g1k0)?)?;

        let yfprime = checked_add(
            checked_add(checked_mul(exp18, y)?, checked_mul(s, mul2)?)?,
            mul1,
        )?;
        let dyfprime = checked_mul(d, mul2)?;

        if yfprime < dyfprime {
            y = div(y_prev, U256::from(2u64))?;
            continue;
        }

        let yfprime = checked_sub(yfprime, dyfprime)?;
        let fprime = div(yfprime, y)?;

        let mut y_minus = div(mul1, fprime)?;
        // y_plus = (yfprime + 10**18 * d) // fprime + y_minus * 10**18 // k_0
        let y_plus = checked_add(
            div(checked_add(yfprime, checked_mul(exp18, d)?)?, fprime)?,
            div(checked_mul(y_minus, exp18)?, k_0)?,
        )?;
        y_minus = checked_add(y_minus, div(checked_mul(exp18, s)?, fprime)?)?;

        y = if y_plus < y_minus {
            div(y_prev, U256::from(2u64))?
        } else {
            checked_sub(y_plus, y_minus)?
        };
        let diff = if y > y_prev { y - y_prev } else { y_prev - y };

        let limit = convergence_limit.max(div(y, exp14)?);
        if diff < limit {
            let frac = div(checked_mul(y, exp18)?, d)?;
            if !(frac > exp16 - U256::from(1u64) && frac < exp20 + U256::from(1u64)) {
                return Err(CurveMathError::UnsafeValue);
            }
            return Ok(y);
        }
    }
    Err(CurveMathError::NotConverged)
}

/// `fee_gamma / (fee_gamma + (1 - K))` where `K = prod(x) / (sum(x)/N)**N`.
///
/// Direct port of `stableswap_reduction_coefficient`. Used by crypto pools for
/// dynamic fee calculation. All values normalized to 1e18.
pub fn stableswap_reduction_coefficient(
    x: &[U256],
    fee_gamma: U256,
    n_coins: U256,
) -> Result<U256> {
    let mut k = pow10(18);
    let mut s = U256::ZERO;
    for &x_i in x {
        s = checked_add(s, x_i)?;
    }
    for &x_i in x {
        k = div(checked_mul(checked_mul(k, n_coins)?, x_i)?, s)?;
    }
    if !fee_gamma.is_zero() {
        k = div(
            checked_mul(fee_gamma, pow10(18))?,
            checked_sub(checked_add(fee_gamma, exp18())?, k)?,
        )?;
    }
    Ok(k)
}

fn exp18() -> U256 {
    pow10(18)
}

/// 10**n as a U256 (avoids `u64::pow` overflow for n ≥ 20). Mirrors the Vyper
/// `10**N` literals.
#[allow(clippy::excessive_precision)]
fn pow10(n: u32) -> U256 {
    U256::from(10u64).pow(U256::from(n))
}

// ---------------------------------------------------------------------------
// Internal dispatch helpers (variant → step function)
// ---------------------------------------------------------------------------

fn d_step(
    d_variant: DVariant,
    a_nn: U256,
    s: U256,
    d: U256,
    d_p: U256,
    n_coins: U256,
    a_precision: U256,
) -> Result<U256> {
    match d_variant {
        DVariant::Standard
        | DVariant::VariantDpAlpha
        | DVariant::VariantBetaDp
        | DVariant::VariantGammaDp => calc_d(a_nn, s, d, d_p, n_coins, a_precision),
        DVariant::VariantAlpha | DVariant::VariantAlphaDpAlpha => {
            calc_d_variant_alpha(a_nn, s, d, d_p, n_coins, a_precision)
        }
    }
}

fn dp_step(
    d_variant: DVariant,
    d: U256,
    d_p: U256,
    xp: &[U256],
    n_coins: U256,
    _n_plus_1: U256,
) -> Result<U256> {
    match d_variant {
        DVariant::Standard | DVariant::VariantAlpha => calc_dp(d, d_p, xp, n_coins),
        DVariant::VariantAlphaDpAlpha | DVariant::VariantDpAlpha => {
            calc_dp_variant_alpha(d, d_p, xp, n_coins)
        }
        DVariant::VariantBetaDp => calc_dp_variant_beta(d, d_p, xp, n_coins),
        DVariant::VariantGammaDp => calc_dp_variant_gamma(d, d_p, xp, n_coins),
    }
}
