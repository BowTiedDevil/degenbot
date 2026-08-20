//! Balancer V2 `StableMath` — StableSwap invariant + swap math. Direct port
//! of `src/degenbot/balancer/libraries/stable_math.py`.
//!
//! Plain integer math (NOT 18-decimal fixed point) — the Solidity
//! `Math.mul`/`Math.divDown`/`Math.divUp` map to plain `*` / `/` here with
//! zero-division checks. FixedPoint operations (`mul_down`/`mul_up` etc.) are
//! used ONLY by the BPT/join/exit functions, which are out of scope for this
//! slice (the swap math + the two `_calculate_invariant` variants cover the
//! slice 12e plan + the oracle snapshot).
//!
//! Two `_calculate_invariant` implementations:
//!
//! 1. [`calculate_invariant`] (monorepo version, V1 / `INVARIANT_V1`):
//!    always-roundDown with D_P accumulation `D^(n+1) / (n^n * P)`. Robust
//!    for all balance configurations.
//! 2. [`calculate_invariant_deployed`] (deployed contract, V2 /
//!    `INVARIANT_V2`): takes a `round_up` parameter and uses P_D
//!    accumulation `n^n * P / D^(n-1)`. Matches the deployed MetaStablePool
//!    and ComposableStablePool exactly. Called with `round_up = true` for
//!    swaps (per the deployed contract), `round_up = false` for joins/exits.

use alloy::primitives::U256;

use crate::balancer::BalancerMathError;

/// `Result` alias for the StableMath port.
pub type Result<T> = std::result::Result<T, BalancerMathError>;

/// `AMP_PRECISION = 1000` — the amplification parameter is stored scaled by
/// this value.
const AMP_PRECISION: U256 = U256::from_limbs([1_000, 0, 0, 0]);

// `_calculate_invariant` (monorepo V1) — always-roundDown D_P accumulation.
///
/// # Errors
///
/// Returns [`BalancerMathError::TokenCountOverflow`] if `balances.len()`
/// exceeds `u64::MAX` (unreachable on a 64-bit platform).
pub fn calculate_invariant(amp: U256, balances: &[U256]) -> Result<U256> {
    let num_tokens = U256::from(
        u64::try_from(balances.len()).map_err(|_| BalancerMathError::TokenCountOverflow)?,
    );
    let mut sum_ = U256::ZERO;
    for i in 0..balances.len() {
        sum_ = sum_
            .checked_add(balances[i])
            .ok_or(BalancerMathError::AddOverflow)?;
    }
    if sum_.is_zero() {
        return Ok(U256::ZERO);
    }

    let mut prev_invariant;
    let mut invariant = sum_;
    let amp_times_total = amp
        .checked_mul(num_tokens)
        .ok_or(BalancerMathError::MulOverflow)?;

    for _ in 0..255 {
        // D_P accumulation: D^(n+1) / (n^n * P), built as
        //   D_P *= D / (balance * n).
        let mut d_p = invariant;
        for j in 0..balances.len() {
            let denom = balances[j]
                .checked_mul(num_tokens)
                .ok_or(BalancerMathError::MulOverflow)?;
            d_p = math_div_down(
                d_p.checked_mul(invariant)
                    .ok_or(BalancerMathError::MulOverflow)?,
                denom,
            )?;
        }

        prev_invariant = invariant;
        // numerator = ((A_tot * sum) / A_prec + D_P * n) * D
        let amp_term = math_div_down(
            amp_times_total
                .checked_mul(sum_)
                .ok_or(BalancerMathError::MulOverflow)?,
            AMP_PRECISION,
        )?;
        let d_p_n = d_p
            .checked_mul(num_tokens)
            .ok_or(BalancerMathError::MulOverflow)?;
        let num = (amp_term
            .checked_add(d_p_n)
            .ok_or(BalancerMathError::AddOverflow)?)
        .checked_mul(invariant)
        .ok_or(BalancerMathError::MulOverflow)?;
        // denominator = (A_tot - A_prec) * D / A_prec + (n + 1) * D_P
        // Canonical `StableMath._calculateInvariant` computes
        // `divDown((ampTimesTotal - _AMP_PRECISION) * invariant, _AMP_PRECISION)`
        // — MULTIPLY FIRST, then divide. The earlier divide-then-multiply
        // ordering dropped up to `AMP_PRECISION - 1` wei and diverged from
        // the deployed bytecode (caught by the Tier-3 balancer oracle).
        let amp_minus_prec = amp_times_total
            .checked_sub(AMP_PRECISION)
            .ok_or(BalancerMathError::SubOverflow)?;
        let denom_lhs = math_div_down(
            amp_minus_prec
                .checked_mul(invariant)
                .ok_or(BalancerMathError::MulOverflow)?,
            AMP_PRECISION,
        )?;
        let n_plus_1 = num_tokens + U256::from(1u64);
        let denom_rhs = n_plus_1
            .checked_mul(d_p)
            .ok_or(BalancerMathError::MulOverflow)?;
        let denom = denom_lhs
            .checked_add(denom_rhs)
            .ok_or(BalancerMathError::AddOverflow)?;
        invariant = math_div_down(num, denom)?;
        if invariant > prev_invariant {
            if invariant - prev_invariant <= U256::from(1u64) {
                return Ok(invariant);
            }
        } else if prev_invariant - invariant <= U256::from(1u64) {
            return Ok(invariant);
        }
    }
    Err(BalancerMathError::StableInvariantDidntConverge)
}

// `_calculate_invariant_deployed` — round_up-parameter P_D accumulation.
///
/// # Errors
///
/// Returns [`BalancerMathError::TokenCountOverflow`] if `balances.len()`
/// exceeds `u64::MAX` (unreachable on a 64-bit platform).
pub fn calculate_invariant_deployed(amp: U256, balances: &[U256], round_up: bool) -> Result<U256> {
    let num_tokens = U256::from(
        u64::try_from(balances.len()).map_err(|_| BalancerMathError::TokenCountOverflow)?,
    );
    let mut sum_ = U256::ZERO;
    for i in 0..balances.len() {
        sum_ = sum_
            .checked_add(balances[i])
            .ok_or(BalancerMathError::AddOverflow)?;
    }
    if sum_.is_zero() {
        return Ok(U256::ZERO);
    }

    let mut prev_invariant;
    let mut invariant = sum_;
    let amp_times_total = amp
        .checked_mul(num_tokens)
        .ok_or(BalancerMathError::MulOverflow)?;

    for _ in 0..255 {
        // P_D accumulation: n^n * P / D^(n-1), built as
        //   P_D *= balance * n / D with round_up rounding.
        let mut p_d = balances[0]
            .checked_mul(num_tokens)
            .ok_or(BalancerMathError::MulOverflow)?;
        for j in 1..balances.len() {
            let prod = p_d
                .checked_mul(balances[j])
                .ok_or(BalancerMathError::MulOverflow)?
                .checked_mul(num_tokens)
                .ok_or(BalancerMathError::MulOverflow)?;
            p_d = math_div(prod, invariant, round_up)?;
        }
        prev_invariant = invariant;
        // numerator = n * D * D + (A_tot * sum) * P_D / A_prec
        let n_d_d = num_tokens
            .checked_mul(invariant)
            .ok_or(BalancerMathError::MulOverflow)?
            .checked_mul(invariant)
            .ok_or(BalancerMathError::MulOverflow)?;
        let amp_times_sum = amp_times_total
            .checked_mul(sum_)
            .ok_or(BalancerMathError::MulOverflow)?;
        let lhs = math_div(
            amp_times_sum
                .checked_mul(p_d)
                .ok_or(BalancerMathError::MulOverflow)?,
            AMP_PRECISION,
            round_up,
        )?;
        let num = n_d_d
            .checked_add(lhs)
            .ok_or(BalancerMathError::AddOverflow)?;
        // denominator = (n + 1) * D + (A_tot - A_prec) * P_D / A_prec
        //               (the p_d term rounds the OPPOSITE way)
        let n_plus_1_d = (num_tokens + U256::from(1u64))
            .checked_mul(invariant)
            .ok_or(BalancerMathError::MulOverflow)?;
        let amp_minus_prec = amp_times_total
            .checked_sub(AMP_PRECISION)
            .ok_or(BalancerMathError::SubOverflow)?;
        let rhs = math_div(
            amp_minus_prec
                .checked_mul(p_d)
                .ok_or(BalancerMathError::MulOverflow)?,
            AMP_PRECISION,
            !round_up, // deployed contract: p_d term rounds the other way
        )?;
        let denom = n_plus_1_d
            .checked_add(rhs)
            .ok_or(BalancerMathError::AddOverflow)?;
        invariant = math_div(num, denom, round_up)?;
        if invariant > prev_invariant {
            if invariant - prev_invariant <= U256::from(1u64) {
                return Ok(invariant);
            }
        } else if prev_invariant - invariant <= U256::from(1u64) {
            return Ok(invariant);
        }
    }
    Err(BalancerMathError::StableInvariantDidntConverge)
}

/// `_calc_out_given_in(amp, balances, i, j, token_amount_in, invariant)`.
///
/// Compute how many tokens can be taken out if `token_amount_in` of token
/// `token_index_in` are sent, given current balances + invariant. Rounds
/// down overall (amount out → favor the pool).
///
/// Mirrors the deployed contract's swap math: adds the input to the input
/// balance, computes the projected output balance (via Newton to invert the
/// invariant), and returns the difference (with a `-1` to compensate for the
/// round-up of `_get_token_balance_given_invariant_and_all_other_balances`).
pub fn calc_out_given_in(
    amp: U256,
    balances: &[U256],
    token_index_in: usize,
    token_index_out: usize,
    token_amount_in: U256,
    invariant: U256,
) -> Result<U256> {
    let mut b: Vec<U256> = balances.to_vec();
    b[token_index_in] = b[token_index_in]
        .checked_add(token_amount_in)
        .ok_or(BalancerMathError::AddOverflow)?;
    let final_balance_out = get_token_balance_given_invariant_and_all_other_balances(
        amp,
        &b,
        invariant,
        token_index_out,
    )?;
    // Restore (no checked arithmetic needed since we just added).
    // Compute the output with the deployed contract's `-1` compensation.
    let balance_out = balances[token_index_out];
    if final_balance_out >= balance_out {
        // Shouldn't happen outside of rounding errors; clamp to 0 to avoid underflow.
        return Ok(U256::ZERO);
    }
    Ok(balance_out - final_balance_out - U256::from(1u64))
}

/// `_calc_in_given_out(amp, balances, i, j, token_amount_out, invariant)`.
///
/// Compute how many tokens must be sent in to take `token_amount_out` of
/// token `token_index_out`. Rounds up overall (amount in → favor the pool).
pub fn calc_in_given_out(
    amp: U256,
    balances: &[U256],
    token_index_in: usize,
    token_index_out: usize,
    token_amount_out: U256,
    invariant: U256,
) -> Result<U256> {
    let mut b: Vec<U256> = balances.to_vec();
    if b[token_index_out] < token_amount_out {
        return Err(BalancerMathError::SubOverflow);
    }
    b[token_index_out] -= token_amount_out;
    let final_balance_in = get_token_balance_given_invariant_and_all_other_balances(
        amp,
        &b,
        invariant,
        token_index_in,
    )?;
    // Restore.
    // Add the `+1` compensation for the round-up of
    // `_get_token_balance_given_invariant_and_all_other_balances`.
    Ok(final_balance_in - balances[token_index_in] + U256::from(1u64))
}

/// `_get_token_balance_given_invariant_and_all_other_balances`.
///
/// Newton-Raphson iteration to find a single token's balance given the
/// invariant and all OTHER token balances. Rounds up overall (mirrors the
/// deployed contract, which the ±1 in the swap callers compensates for).
///
/// # Panics
///
/// # Errors
///
/// Returns [`BalancerMathError::TokenCountOverflow`] if `balances.len()`
/// exceeds `u64::MAX` (unreachable on a 64-bit platform).
pub fn get_token_balance_given_invariant_and_all_other_balances(
    amp: U256,
    balances: &[U256],
    invariant: U256,
    token_index: usize,
) -> Result<U256> {
    let n = U256::from(
        u64::try_from(balances.len()).map_err(|_| BalancerMathError::TokenCountOverflow)?,
    );
    let amp_times_total = amp.checked_mul(n).ok_or(BalancerMathError::MulOverflow)?;

    let mut sum_ = balances[0];
    let mut p_d = balances[0]
        .checked_mul(n)
        .ok_or(BalancerMathError::MulOverflow)?;
    for j in 1..balances.len() {
        p_d = math_div_down(
            p_d.checked_mul(balances[j])
                .ok_or(BalancerMathError::MulOverflow)?
                .checked_mul(n)
                .ok_or(BalancerMathError::MulOverflow)?,
            invariant,
        )?;
        sum_ = sum_
            .checked_add(balances[j])
            .ok_or(BalancerMathError::AddOverflow)?;
    }
    // No safe-math needed — sum >= balances[token_index].
    sum_ -= balances[token_index];

    let inv2 = invariant
        .checked_mul(invariant)
        .ok_or(BalancerMathError::MulOverflow)?;
    // Remove the balance from c by multiplying it.
    let c = math_div_up(
        inv2,
        amp_times_total
            .checked_mul(p_d)
            .ok_or(BalancerMathError::MulOverflow)?,
    )?
    .checked_mul(AMP_PRECISION)
    .ok_or(BalancerMathError::MulOverflow)?
    .checked_mul(balances[token_index])
    .ok_or(BalancerMathError::MulOverflow)?;
    let b = sum_
        .checked_add(
            math_div_down(invariant, amp_times_total)?
                .checked_mul(AMP_PRECISION)
                .ok_or(BalancerMathError::MulOverflow)?,
        )
        .ok_or(BalancerMathError::AddOverflow)?;

    let mut prev_token_balance;
    // First iteration outside the loop uses the invariant for initial
    // approximation: (D^2 + c) / (D + b).
    let mut token_balance = math_div_up(
        inv2.checked_add(c).ok_or(BalancerMathError::AddOverflow)?,
        invariant
            .checked_add(b)
            .ok_or(BalancerMathError::AddOverflow)?,
    )?;

    for _ in 0..255 {
        prev_token_balance = token_balance;
        // (token_balance^2 + c) / (2 * token_balance + b - D)
        let num = token_balance
            .checked_mul(token_balance)
            .ok_or(BalancerMathError::MulOverflow)?
            .checked_add(c)
            .ok_or(BalancerMathError::AddOverflow)?;
        let t2 = token_balance
            .checked_mul(U256::from(2u64))
            .ok_or(BalancerMathError::MulOverflow)?;
        let denom_inner = t2
            .checked_add(b)
            .ok_or(BalancerMathError::AddOverflow)?
            .checked_sub(invariant)
            .ok_or(BalancerMathError::SubOverflow)?;
        token_balance = math_div_up(num, denom_inner)?;
        if token_balance > prev_token_balance {
            if token_balance - prev_token_balance <= U256::from(1u64) {
                return Ok(token_balance);
            }
        } else if prev_token_balance - token_balance <= U256::from(1u64) {
            return Ok(token_balance);
        }
    }
    Err(BalancerMathError::StableGetBalanceDidntConverge)
}

// -------------------------------------------------------------------------
// Internal Math-library helpers (plain-integer, matching Solidity's Math)
// -------------------------------------------------------------------------

/// `Math.divDown(a, b) = a / b` (rounding down). Plain integer division —
/// not 18-decimal fixed-point.
fn math_div_down(a: U256, b: U256) -> Result<U256> {
    if b.is_zero() {
        return Err(BalancerMathError::ZeroDivision);
    }
    Ok(a / b)
}

/// `Math.divUp(a, b) = 1 + (a - 1) / b` when a > 0, else 0. Plain integer.
fn math_div_up(a: U256, b: U256) -> Result<U256> {
    if b.is_zero() {
        return Err(BalancerMathError::ZeroDivision);
    }
    if a.is_zero() {
        return Ok(U256::ZERO);
    }
    Ok(U256::from(1u64) + (a - U256::from(1u64)) / b)
}

/// `Math.div(a, b, round_up)` — div with explicit direction (deployed
/// contract's variant parameter).
fn math_div(a: U256, b: U256, round_up: bool) -> Result<U256> {
    if b.is_zero() {
        return Err(BalancerMathError::ZeroDivision);
    }
    if round_up {
        if a.is_zero() {
            return Ok(U256::ZERO);
        }
        Ok(U256::from(1u64) + (a - U256::from(1u64)) / b)
    } else {
        Ok(a / b)
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::balancer::ONE;

    #[test]
    fn div_by_zero_returns_err() {
        assert_eq!(
            math_div_down(ONE, U256::ZERO),
            Err(BalancerMathError::ZeroDivision)
        );
        assert_eq!(
            math_div_up(ONE, U256::ZERO),
            Err(BalancerMathError::ZeroDivision)
        );
    }

    #[test]
    fn div_up_rounds_up_for_nonzero_remainder() {
        // 7 / 2 = 3.5 → div_up = 4, div_down = 3.
        assert_eq!(
            math_div_up(U256::from(7u64), U256::from(2u64)).unwrap(),
            U256::from(4u64)
        );
        assert_eq!(
            math_div_down(U256::from(7u64), U256::from(2u64)).unwrap(),
            U256::from(3u64)
        );
    }

    #[test]
    fn div_up_zero_is_zero() {
        assert_eq!(
            math_div_up(U256::ZERO, U256::from(7u64)).unwrap(),
            U256::ZERO
        );
    }

    /// Regression (Tier-3 balancer oracle): the V1 `_calculateInvariant`
    /// denominator must compute `(ampTimesTotal - AMP_PRECISION) * invariant /
    /// AMP_PRECISION` — MULTIPLY first, then divide (matching canonical
    /// `StableMath._calculateInvariant`). The earlier divide-then-multiply
    /// ordering leaked `AMP_PRECISION - 1` wei and diverged from the deployed
    /// bytecode. A fixture whose `(amp*2 - AMP) / AMP` is non-exact selects the
    /// divergent branch.
    #[test]
    fn v1_invariant_denominator_multiply_before_divide_matches_canonical() {
        let amp = U256::from(100_111u64); // (amp*2 - 1000) % 1000 != 0
        let balances = [ONE * U256::from(3u64), ONE * U256::from(7u64)];

        let got = calculate_invariant(amp, &balances).expect("converges");

        // Reference loops with the canonical (multiply-before-divide) and the
        // buggy (divide-before-multiply) denominator — a mini fingerprint of
        // the Newton iteration for this 2-token fixture.
        let n = U256::from(2u64);
        let amp_times_total = amp * n;
        let sum_ = balances[0] + balances[1];
        let mut canonical = sum_;
        let mut buggy = sum_;
        for _ in 0..255 {
            let d_p_c = math_div_down(canonical * canonical, balances[0] * n).unwrap();
            let d_p_c = math_div_down(d_p_c * canonical, balances[1] * n).unwrap();
            let d_p_b = math_div_down(buggy * buggy, balances[0] * n).unwrap();
            let d_p_b = math_div_down(d_p_b * buggy, balances[1] * n).unwrap();
            let amp_minus = amp_times_total - U256::from(1000u64);
            let denom_c = math_div_down(amp_minus * canonical, U256::from(1000u64)).unwrap()
                + (n + U256::from(1u64)) * d_p_c;
            let denom_b = math_div_down(amp_minus, U256::from(1000u64)).unwrap() * buggy
                + (n + U256::from(1u64)) * d_p_b;
            let num_c = (math_div_down(amp_times_total * sum_, U256::from(1000u64)).unwrap()
                + d_p_c * n)
                * canonical;
            let num_b = (math_div_down(amp_times_total * sum_, U256::from(1000u64)).unwrap()
                + d_p_b * n)
                * buggy;
            canonical = math_div_down(num_c, denom_c).unwrap();
            buggy = math_div_down(num_b, denom_b).unwrap();
        }

        // The buggy ordering must actually diverge on this fixture (guards
        // against the regression test silently becoming a no-op).
        assert_ne!(canonical, buggy, "fixture must select the divergent branch");
        // And the fixed function took the canonical ordering.
        assert_eq!(got, canonical);
    }
}
