//! Aave V3 position analysis — pure math for liquidation-risk assessment.
//!
//! Verbatim port of `src/degenbot/aave/analysis/core.py`. No I/O, no `pyo3`.
//! Consumes the record structs [`degenbot_db::aave`] already produces
//! ([`AaveUserRecord`], [`AaveCollateralPositionRecord`],
//! [`AaveDebtPositionRecord`]) + builds the derived [`CollateralPositionData`]
//! / [`DebtPositionData`] / [`UserPositionSummary`] the CLI risk display
//! reads. The scaled→actual balance math routes through
//! [`crate::wad_ray_math`] (`ray_mul_floor` for collateral,
//! `ray_mul_ceil` for debt) — the §4.2 byte-identical Rust oracle.
//!
//! # Float discipline
//!
//! Index arithmetic is integer-exact (`U256`); the final health-factor +
//! max-LTV ratios are `f64` (mirrors the Python `int / int → float`
//! division). [`u256_to_f64`] converts via a lossy limb sum — exact for
//! values ≤ `2^53` and within `rel-1e-6` for all realistic magnitudes (the
//! tolerance the Python `test_core.py` suite uses).

use std::collections::HashMap;

use crate::{ray_mul_ceil, ray_mul_floor, WadRayError};
use alloy::primitives::U256;
use degenbot_db::aave::{AaveCollateralPositionRecord, AaveDebtPositionRecord, AaveUserRecord};

/// Basis points: `10_000` = 100% (mirrors Python `BASIS_POINTS`).
pub const BASIS_POINTS: i64 = 10_000;

/// A position is "at risk" when its health factor drops below this buffer.
pub const HEALTH_FACTOR_AT_RISK_THRESHOLD: f64 = 1.1;

/// A position is liquidatable when its health factor drops below 1.0.
pub const HEALTH_FACTOR_LIQUIDATABLE_THRESHOLD: f64 = 1.0;

// =========================================================================
// Derived position data (built from the flat records)
// =========================================================================

/// Collateral position with calculated values for risk analysis.
/// Mirrors Python `CollateralPositionData`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollateralPositionData {
    pub asset_address: String,
    pub asset_symbol: Option<String>,
    pub scaled_balance: U256,
    pub actual_balance: U256,
    pub liquidation_threshold: i64,
    pub ltv: i64,
    pub is_enabled_as_collateral: bool,
    pub in_emode: bool,
    pub emode_category_id: Option<i64>,
    pub price: Option<U256>,
}

impl CollateralPositionData {
    /// Liquidation threshold used for health-factor calculation.
    /// Returns `0` if collateral is disabled by user preference.
    #[must_use]
    pub fn effective_liquidation_threshold(&self) -> i64 {
        if self.is_enabled_as_collateral {
            self.liquidation_threshold
        } else {
            0
        }
    }

    /// Balance adjusted by price, in oracle base currency units.
    /// Returns `actual_balance * price`; if price is `None`, returns
    /// `actual_balance` (assumes price = 1).
    #[must_use]
    pub fn price_adjusted_balance(&self) -> U256 {
        match self.price {
            Some(price) => self.actual_balance * price,
            None => self.actual_balance,
        }
    }
}

/// Debt position with calculated values for risk analysis.
/// Mirrors Python `DebtPositionData`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebtPositionData {
    pub asset_address: String,
    pub asset_symbol: Option<String>,
    pub scaled_balance: U256,
    pub actual_balance: U256,
    pub stable_debt: bool,
    pub in_emode: bool,
    pub emode_category_id: Option<i64>,
    pub price: Option<U256>,
}

impl DebtPositionData {
    /// Balance adjusted by price, in oracle base currency units.
    /// Returns `actual_balance * price`; if price is `None`, returns
    /// `actual_balance` (assumes price = 1).
    #[must_use]
    pub fn price_adjusted_balance(&self) -> U256 {
        match self.price {
            Some(price) => self.actual_balance * price,
            None => self.actual_balance,
        }
    }
}

/// Summary of a user's Aave V3 position for liquidation risk assessment.
/// Mirrors Python `UserPositionSummary`.
///
/// `total_collateral_value` / `total_debt_value` are the (integer) sums of
/// `price_adjusted_balance` (stored as `U256` to preserve byte-exact parity
/// with the Python runtime, where the float-typed field holds an `int`).
#[derive(Debug, Clone, PartialEq)]
pub struct UserPositionSummary {
    pub user_address: String,
    pub market_id: i64,
    pub emode_category_id: Option<i64>,
    pub is_isolation_mode: bool,
    pub collateral_positions: Vec<CollateralPositionData>,
    pub debt_positions: Vec<DebtPositionData>,
    pub total_collateral_value: U256,
    pub total_debt_value: U256,
    pub health_factor: Option<f64>,
    pub max_ltv_ratio: Option<f64>,
}

impl UserPositionSummary {
    /// `True` if position is near liquidation (HF < at-risk threshold).
    #[must_use]
    pub fn is_at_risk(&self) -> bool {
        match self.health_factor {
            Some(hf) => hf < HEALTH_FACTOR_AT_RISK_THRESHOLD,
            None => false,
        }
    }

    /// `True` if position can be liquidated (HF < 1).
    #[must_use]
    pub fn is_liquidatable(&self) -> bool {
        match self.health_factor {
            Some(hf) => hf < HEALTH_FACTOR_LIQUIDATABLE_THRESHOLD,
            None => false,
        }
    }

    /// `True` if the user has any debt positions.
    #[must_use]
    pub fn has_debt(&self) -> bool {
        !self.debt_positions.is_empty()
    }
}

/// Result of analyzing positions for liquidation risk.
/// Mirrors Python `PositionAnalysisResult`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PositionAnalysisResult {
    pub safe_users: Vec<UserPositionSummary>,
    pub at_risk_users: Vec<UserPositionSummary>,
    pub liquidatable_users: Vec<UserPositionSummary>,
}

impl PositionAnalysisResult {
    /// Total users across all buckets.
    #[must_use]
    pub fn total_users(&self) -> usize {
        self.safe_users.len() + self.at_risk_users.len() + self.liquidatable_users.len()
    }

    /// At-risk count.
    #[must_use]
    pub fn at_risk_count(&self) -> usize {
        self.at_risk_users.len()
    }

    /// Liquidatable count.
    #[must_use]
    pub fn liquidatable_count(&self) -> usize {
        self.liquidatable_users.len()
    }

    /// Categorize a user summary by health factor.
    pub fn categorize(&mut self, summary: UserPositionSummary, health_factor_threshold: f64) {
        match summary.health_factor {
            Some(hf) if hf < HEALTH_FACTOR_LIQUIDATABLE_THRESHOLD => {
                self.liquidatable_users.push(summary);
            }
            Some(hf) if hf < health_factor_threshold => {
                self.at_risk_users.push(summary);
            }
            _ => {
                self.safe_users.push(summary);
            }
        }
    }

    /// Sort at-risk + liquidatable users by health factor (lowest first).
    ///
    /// Mirrors the Python `key=lambda x: x.health_factor or float("inf")`:
    /// `None` *and* `Some(0.0)` both sort as `+∞` (the Python `or` treats `0.0`
    /// as falsy). Faithful replication of the oracle's sort key.
    pub fn sort_by_risk(&mut self) {
        let key = |s: &UserPositionSummary| -> f64 {
            match s.health_factor {
                Some(hf) if hf != 0.0 => hf,
                _ => f64::INFINITY,
            }
        };
        self.at_risk_users.sort_by(|a, b| {
            key(a)
                .partial_cmp(&key(b))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        self.liquidatable_users.sort_by(|a, b| {
            key(a)
                .partial_cmp(&key(b))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
}

// =========================================================================
// Pure calculation functions
// =========================================================================

/// Calculate actual collateral balance from a scaled balance.
///
/// Uses floor rounding (`ray_mul_floor`) to match Aave V3 behavior — the
/// collateral balance is not over-accounted.
///
/// # Errors
///
/// Returns [`WadRayError`] on `ray_mul_floor` overflow.
pub fn calculate_actual_collateral_balance(
    scaled_balance: U256,
    liquidity_index: U256,
) -> Result<U256, WadRayError> {
    ray_mul_floor(scaled_balance, liquidity_index)
}

/// Calculate actual debt balance from a scaled balance.
///
/// Uses ceil rounding (`ray_mul_ceil`) to match Aave V3 behavior — the debt
/// is not under-accounted.
///
/// # Errors
///
/// Returns [`WadRayError`] on `ray_mul_ceil` overflow.
pub fn calculate_actual_debt_balance(
    scaled_balance: U256,
    borrow_index: U256,
) -> Result<U256, WadRayError> {
    ray_mul_ceil(scaled_balance, borrow_index)
}

/// Get the effective liquidation threshold for a collateral position.
///
/// If the user is in eMode and the asset is in the same eMode category, use
/// the eMode liquidation threshold; otherwise the standard threshold.
#[must_use]
pub fn get_liquidation_threshold(
    asset_lt: i64,
    emode_lt: Option<i64>,
    user_emode: i64,
    asset_emode_category_id: Option<i64>,
) -> i64 {
    if user_emode > 0 && asset_emode_category_id == Some(user_emode) {
        if let Some(lt) = emode_lt {
            return lt;
        }
    }
    asset_lt
}

/// Get the effective LTV for a collateral position.
///
/// If the user is in eMode and the asset is in the same eMode category, use
/// the eMode LTV; otherwise the standard LTV.
#[must_use]
pub fn get_ltv(
    asset_ltv: i64,
    emode_ltv: Option<i64>,
    user_emode: i64,
    asset_emode_category_id: Option<i64>,
) -> i64 {
    if user_emode > 0 && asset_emode_category_id == Some(user_emode) {
        if let Some(ltv) = emode_ltv {
            return ltv;
        }
    }
    asset_ltv
}

/// Build [`CollateralPositionData`] from a flat record.
///
/// # Errors
///
/// Returns [`WadRayError`] if the scaled-balance computation overflows.
pub fn build_collateral_position_data(
    record: &AaveCollateralPositionRecord,
    user_emode: i64,
    collateral_enabled: bool,
    price: Option<U256>,
) -> Result<CollateralPositionData, WadRayError> {
    let actual_balance =
        calculate_actual_collateral_balance(record.balance, record.liquidity_index)?;
    let lt = get_liquidation_threshold(
        record.asset_lt,
        record.emode_lt,
        user_emode,
        record.e_mode_category_id,
    );
    let ltv = get_ltv(
        record.asset_ltv,
        record.emode_ltv,
        user_emode,
        record.e_mode_category_id,
    );
    let in_emode = user_emode > 0 && record.e_mode_category_id == Some(user_emode);
    Ok(CollateralPositionData {
        asset_address: record.underlying_address.clone(),
        asset_symbol: record.underlying_symbol.clone(),
        scaled_balance: record.balance,
        actual_balance,
        liquidation_threshold: lt,
        ltv,
        is_enabled_as_collateral: collateral_enabled,
        in_emode,
        emode_category_id: record.e_mode_category_id,
        price,
    })
}

/// Build [`DebtPositionData`] from a flat record.
///
/// # Errors
///
/// Returns [`WadRayError`] if the scaled-balance computation overflows.
pub fn build_debt_position_data(
    record: &AaveDebtPositionRecord,
    user_emode: i64,
    price: Option<U256>,
) -> Result<DebtPositionData, WadRayError> {
    let actual_balance = calculate_actual_debt_balance(record.balance, record.borrow_index)?;
    let in_emode = user_emode > 0 && record.e_mode_category_id == Some(user_emode);
    Ok(DebtPositionData {
        asset_address: record.underlying_address.clone(),
        asset_symbol: record.underlying_symbol.clone(),
        scaled_balance: record.balance,
        actual_balance,
        stable_debt: false,
        in_emode,
        emode_category_id: record.e_mode_category_id,
        price,
    })
}

/// Calculate the health factor for a user position.
///
/// `HF = Σ(collateral · price · LT) / (Σ(debt · price) · BASIS_POINTS)`
///
/// Prices are in 8-decimal oracle format. Returns `None` when there is no
/// debt (or the effective debt is zero). For isolation mode, debt is capped
/// at the debt ceiling when `isolation_mode_debt > 0`.
///
/// # Panics
///
/// Panics if the `BASIS_POINTS` (`10_000`) constant fails its cast to `u64` —
/// it cannot, `BASIS_POINTS` is a compile-time `const` well within `u64` range.
#[must_use]
#[expect(clippy::cast_sign_loss)]
pub fn calculate_health_factor(
    collateral_positions: &[CollateralPositionData],
    debt_positions: &[DebtPositionData],
    isolation_mode_debt: U256,
    isolation_debt_ceiling: Option<U256>,
) -> Option<f64> {
    if debt_positions.is_empty() && isolation_mode_debt == U256::ZERO {
        return None;
    }

    let mut weighted_collateral = U256::ZERO;
    for collateral_pos in collateral_positions {
        if collateral_pos.is_enabled_as_collateral && collateral_pos.actual_balance > U256::ZERO {
            let weighted = collateral_pos.price_adjusted_balance()
                * U256::from(collateral_pos.effective_liquidation_threshold().max(0) as u64);
            weighted_collateral += weighted;
        }
    }

    let mut total_debt = U256::ZERO;
    for debt_pos in debt_positions {
        total_debt += debt_pos.price_adjusted_balance();
    }

    if isolation_mode_debt > U256::ZERO {
        if let Some(ceiling) = isolation_debt_ceiling {
            total_debt = total_debt.min(ceiling);
        }
    }

    if total_debt == U256::ZERO {
        return None;
    }

    #[expect(clippy::expect_used)] // BASIS_POINTS (10_000) always fits u64
    let denom = total_debt * U256::try_from(BASIS_POINTS).expect("BASIS_POINTS fits u64");
    Some(u256_to_f64(weighted_collateral) / u256_to_f64(denom))
}

/// Analyze a single user's position for liquidation risk.
///
/// # Errors
///
/// Returns [`WadRayError`] if any scaled-balance computation overflows.
///
/// # Panics
///
/// Panics if the `BASIS_POINTS` (`10_000`) constant fails its cast to `u64` —
/// it cannot, `BASIS_POINTS` is a compile-time `const` well within `u64` range.
#[expect(clippy::cast_sign_loss, clippy::implicit_hasher)]
pub fn analyze_user_position(
    user: &AaveUserRecord,
    collateral_positions: &[AaveCollateralPositionRecord],
    debt_positions: &[AaveDebtPositionRecord],
    collateral_config_map: &HashMap<i64, bool>,
    price_map: Option<&HashMap<String, U256>>,
) -> Result<UserPositionSummary, WadRayError> {
    let user_emode = user.e_mode;
    let empty_map = HashMap::new();
    let prices = price_map.unwrap_or(&empty_map);
    let default_enabled = true;

    let mut collateral_data = Vec::with_capacity(collateral_positions.len());
    for pos in collateral_positions {
        let collateral_enabled = *collateral_config_map
            .get(&pos.asset_id)
            .unwrap_or(&default_enabled);
        let price = prices.get(&pos.underlying_address).copied();
        collateral_data.push(build_collateral_position_data(
            pos,
            user_emode,
            collateral_enabled,
            price,
        )?);
    }

    let mut debt_data = Vec::with_capacity(debt_positions.len());
    for pos in debt_positions {
        let price = prices.get(&pos.underlying_address).copied();
        debt_data.push(build_debt_position_data(pos, user_emode, price)?);
    }

    let isolation_mode_debt = if user.is_isolation_mode {
        user.isolation_mode_debt
    } else {
        U256::ZERO
    };

    let health_factor = calculate_health_factor(
        &collateral_data,
        &debt_data,
        isolation_mode_debt,
        user.isolation_debt_ceiling,
    );

    let total_collateral_value = collateral_data
        .iter()
        .filter(|p| p.is_enabled_as_collateral)
        .map(CollateralPositionData::price_adjusted_balance)
        .sum::<U256>();

    let total_debt_value = debt_data
        .iter()
        .map(DebtPositionData::price_adjusted_balance)
        .sum::<U256>();

    let max_ltv_capacity = collateral_data
        .iter()
        .filter(|p| p.is_enabled_as_collateral)
        .map(|p| p.price_adjusted_balance() * U256::from(p.ltv.max(0) as u64))
        .sum::<U256>();
    let total_debt_raw = total_debt_value;

    let max_ltv_ratio = if total_debt_raw > U256::ZERO && max_ltv_capacity > U256::ZERO {
        #[expect(clippy::expect_used)] // BASIS_POINTS (10_000) always fits u64
        let numer = total_debt_raw * U256::try_from(BASIS_POINTS).expect("BASIS_POINTS fits u64");
        Some(u256_to_f64(numer) / u256_to_f64(max_ltv_capacity))
    } else {
        None
    };

    Ok(UserPositionSummary {
        user_address: user.address.clone(),
        market_id: user.market_id,
        emode_category_id: if user_emode > 0 {
            Some(user_emode)
        } else {
            None
        },
        is_isolation_mode: user.is_isolation_mode,
        collateral_positions: collateral_data,
        debt_positions: debt_data,
        total_collateral_value,
        total_debt_value,
        health_factor,
        max_ltv_ratio,
    })
}

// =========================================================================
// Internal helpers
// =========================================================================

/// Convert a `U256` to `f64` via a lossy limb sum.
///
/// Python computes `int / int` as a correctly-rounded `f64` of the exact
/// rational; this conversion rounds each limb individually, which is exact for
/// values ≤ `2^53` and within `rel-1e-6` for all realistic magnitudes (the
/// tolerance the Python `test_core.py` suite uses via `pytest.approx`).
#[expect(clippy::cast_precision_loss)]
fn u256_to_f64(v: U256) -> f64 {
    let limbs = v.into_limbs();
    (limbs[0] as f64)
        + (limbs[1] as f64) * 2f64.powi(64)
        + (limbs[2] as f64) * 2f64.powi(128)
        + (limbs[3] as f64) * 2f64.powi(192)
}

#[expect(clippy::unwrap_used)]
#[cfg(test)]
mod tests {
    //! 1:1 port of `tests/aave/analysis/test_core.py`. Each Python test class
    //! becomes a Rust test module; the int inputs + asserted outputs are
    //! verbatim. The Python suite is the spec.

    use super::*;
    use alloy::primitives::U256;
    use std::collections::HashMap;

    const ZERO_ADDR_HEX: &str = "0x0000000000000000000000000000000000000000";

    fn addr(n: u8) -> String {
        // Mirrors the Python `_addr(n)`: `0x{'0'*39}{n}` (a 40-hex-char
        // address with `n` as the last hex digit). Kept as a `String` (the
        // analysis records key on address strings) — the value itself isn't
        // checksum-validated, mirroring the test-only Python helper.
        format!("0x{zero39}{n:x}", zero39 = "0".repeat(39))
    }

    #[expect(clippy::too_many_arguments)]
    fn make_collateral_position_data(
        asset_address: &str,
        asset_symbol: &str,
        scaled_balance: u128,
        actual_balance: u128,
        liquidation_threshold: i64,
        ltv: i64,
        is_enabled_as_collateral: bool,
        in_emode: bool,
        price: Option<u128>,
    ) -> CollateralPositionData {
        CollateralPositionData {
            asset_address: asset_address.to_string(),
            asset_symbol: Some(asset_symbol.to_string()),
            scaled_balance: U256::from(scaled_balance),
            actual_balance: U256::from(actual_balance),
            liquidation_threshold,
            ltv,
            is_enabled_as_collateral,
            in_emode,
            emode_category_id: None,
            price: price.map(U256::from),
        }
    }

    fn make_debt_position_data(
        asset_address: &str,
        asset_symbol: &str,
        scaled_balance: u128,
        actual_balance: u128,
        price: Option<u128>,
    ) -> DebtPositionData {
        DebtPositionData {
            asset_address: asset_address.to_string(),
            asset_symbol: Some(asset_symbol.to_string()),
            scaled_balance: U256::from(scaled_balance),
            actual_balance: U256::from(actual_balance),
            stable_debt: false,
            in_emode: false,
            emode_category_id: None,
            price: price.map(U256::from),
        }
    }

    // ── TestHealthFactorPure ──────────────────────────────────────────────

    #[test]
    fn no_debt_returns_none() {
        let collateral = make_collateral_position_data(
            ZERO_ADDR_HEX,
            "WETH",
            1000,
            1000,
            8000,
            7500,
            true,
            false,
            None,
        );
        let result = calculate_health_factor(&[collateral], &[], U256::ZERO, None);
        assert!(result.is_none());
    }

    #[test]
    fn safe_position() {
        let collateral = make_collateral_position_data(
            ZERO_ADDR_HEX,
            "WETH",
            1000,
            1000,
            8000,
            7500,
            true,
            false,
            None,
        );
        let debt = make_debt_position_data(&addr(1), "USDC", 500, 500, None);
        let result = calculate_health_factor(&[collateral], &[debt], U256::ZERO, None);
        assert!(result.is_some());
        assert!(result.unwrap() > 1.0);
    }

    #[test]
    fn liquidatable_position() {
        let collateral = make_collateral_position_data(
            ZERO_ADDR_HEX,
            "WETH",
            100,
            100,
            8000,
            7500,
            true,
            false,
            None,
        );
        let debt = make_debt_position_data(&addr(1), "USDC", 100, 100, None);
        let result = calculate_health_factor(&[collateral], &[debt], U256::ZERO, None);
        assert!(result.is_some());
        assert!(result.unwrap() < 1.0);
    }

    #[test]
    fn disabled_collateral_zero_weight() {
        let collateral = make_collateral_position_data(
            ZERO_ADDR_HEX,
            "WETH",
            1000,
            1000,
            8000,
            7500,
            false,
            false,
            None,
        );
        let debt = make_debt_position_data(&addr(1), "USDC", 500, 500, None);
        let result = calculate_health_factor(&[collateral], &[debt], U256::ZERO, None);
        assert!(result.is_some());
        assert!((result.unwrap() - 0.0).abs() < 1e-9);
    }

    #[test]
    fn multiple_collateral_and_debt() {
        let collateral_a = make_collateral_position_data(
            &addr(1),
            "WETH",
            1000,
            1000,
            8000,
            7500,
            true,
            false,
            None,
        );
        let collateral_b = make_collateral_position_data(
            &addr(2),
            "WBTC",
            2000,
            2000,
            7000,
            6500,
            true,
            false,
            None,
        );
        let debt_a = make_debt_position_data(&addr(3), "USDC", 1000, 1000, None);
        let debt_b = make_debt_position_data(&addr(4), "DAI", 500, 500, None);
        let result = calculate_health_factor(
            &[collateral_a, collateral_b],
            &[debt_a, debt_b],
            U256::ZERO,
            None,
        );
        // Weighted collateral: (1000*8000) + (2000*7000) = 8M + 14M = 22M
        // Total debt: 1000 + 500 = 1500
        // HF = 22_000_000 / (1500 * 10000) = 22M / 15M ≈ 1.4667
        assert!(result.is_some());
        let got = result.unwrap();
        let want = 22_000_000.0 / 15_000_000.0;
        assert!((got - want).abs() <= want.abs() * 1e-6);
    }

    #[test]
    fn price_adjusted_collateral() {
        let collateral = make_collateral_position_data(
            ZERO_ADDR_HEX,
            "WETH",
            1,
            1,
            8000,
            7500,
            true,
            false,
            Some(2_000_000_000), // $2000 in 8 decimals
        );
        let debt = make_debt_position_data(&addr(1), "USDC", 1, 1, Some(1_000_000));
        let result = calculate_health_factor(&[collateral], &[debt], U256::ZERO, None);
        assert!(result.is_some());
        let got = result.unwrap();
        assert!((got - 1600.0).abs() <= 1600.0 * 1e-6);
    }

    #[test]
    fn isolation_mode_caps_debt() {
        let collateral = make_collateral_position_data(
            ZERO_ADDR_HEX,
            "WETH",
            10_000,
            10_000,
            8000,
            7500,
            true,
            false,
            None,
        );
        let debt = make_debt_position_data(&addr(1), "USDC", 5_000, 5_000, None);
        let result_no_isolation = calculate_health_factor(
            std::slice::from_ref(&collateral),
            std::slice::from_ref(&debt),
            U256::ZERO,
            None,
        );
        // With isolation ceiling of 1000: effective debt = min(5000, 1000) = 1000
        let result_isolation = calculate_health_factor(
            std::slice::from_ref(&collateral),
            std::slice::from_ref(&debt),
            U256::from(5_000u64),
            Some(U256::from(1_000u64)),
        );
        assert!(result_no_isolation.is_some());
        assert!(result_isolation.is_some());
        assert!(result_isolation.unwrap() > result_no_isolation.unwrap());
    }

    #[test]
    fn zero_balance_collateral_ignored() {
        let collateral = make_collateral_position_data(
            ZERO_ADDR_HEX,
            "WETH",
            0,
            0,
            8000,
            7500,
            true,
            false,
            None,
        );
        let debt = make_debt_position_data(&addr(1), "USDC", 100, 100, None);
        let result = calculate_health_factor(&[collateral], &[debt], U256::ZERO, None);
        assert!(result.is_some());
        assert!((result.unwrap() - 0.0).abs() < 1e-9);
    }

    // ── TestCollateralPositionDataProperties ──────────────────────────────

    fn base_collateral_data() -> CollateralPositionData {
        make_collateral_position_data(ZERO_ADDR_HEX, "TEST", 0, 0, 8000, 7500, true, false, None)
    }

    #[test]
    fn effective_lt_enabled() {
        let pos = base_collateral_data();
        assert_eq!(pos.effective_liquidation_threshold(), 8000);
    }

    #[test]
    fn effective_lt_disabled() {
        let mut pos = base_collateral_data();
        pos.is_enabled_as_collateral = false;
        assert_eq!(pos.effective_liquidation_threshold(), 0);
    }

    #[test]
    fn price_adjusted_balance_with_price() {
        let pos = make_collateral_position_data(
            ZERO_ADDR_HEX,
            "TEST",
            0,
            100,
            8000,
            7500,
            true,
            false,
            Some(2_000_000_000),
        );
        assert_eq!(
            pos.price_adjusted_balance(),
            U256::from(100u64) * U256::from(2_000_000_000u64)
        );
    }

    #[test]
    fn price_adjusted_balance_without_price() {
        let pos = base_collateral_data_with_balance(100);
        assert_eq!(pos.price_adjusted_balance(), U256::from(100u64));
    }

    fn base_collateral_data_with_balance(actual_balance: u128) -> CollateralPositionData {
        make_collateral_position_data(
            ZERO_ADDR_HEX,
            "TEST",
            0,
            actual_balance,
            8000,
            7500,
            true,
            false,
            None,
        )
    }

    // ── TestDebtPositionDataProperties ────────────────────────────────────

    #[test]
    fn debt_price_adjusted_balance_with_price() {
        let pos = make_debt_position_data(ZERO_ADDR_HEX, "USDC", 0, 500, Some(1_000_000));
        assert_eq!(
            pos.price_adjusted_balance(),
            U256::from(500u64) * U256::from(1_000_000u64)
        );
    }

    #[test]
    fn debt_price_adjusted_balance_without_price() {
        let pos = make_debt_position_data(ZERO_ADDR_HEX, "USDC", 0, 500, None);
        assert_eq!(pos.price_adjusted_balance(), U256::from(500u64));
    }

    // ── TestUserPositionSummaryProperties ─────────────────────────────────

    fn make_summary(health_factor: Option<f64>) -> UserPositionSummary {
        UserPositionSummary {
            user_address: ZERO_ADDR_HEX.to_string(),
            market_id: 1,
            emode_category_id: None,
            is_isolation_mode: false,
            collateral_positions: Vec::new(),
            debt_positions: Vec::new(),
            total_collateral_value: U256::ZERO,
            total_debt_value: U256::ZERO,
            health_factor,
            max_ltv_ratio: None,
        }
    }

    #[test]
    fn at_risk_below_threshold() {
        assert!(make_summary(Some(1.05)).is_at_risk());
    }

    #[test]
    fn at_risk_above_threshold() {
        assert!(!make_summary(Some(1.5)).is_at_risk());
    }

    #[test]
    fn liquidatable_below_one() {
        assert!(make_summary(Some(0.95)).is_liquidatable());
    }

    #[test]
    fn not_liquidatable_above_one() {
        assert!(!make_summary(Some(1.05)).is_liquidatable());
    }

    #[test]
    fn none_health_factor_is_safe() {
        let summary = make_summary(None);
        assert!(!summary.is_at_risk());
        assert!(!summary.is_liquidatable());
        assert!(!summary.has_debt());
    }

    #[test]
    fn has_debt() {
        let summary = UserPositionSummary {
            user_address: ZERO_ADDR_HEX.to_string(),
            market_id: 1,
            emode_category_id: None,
            is_isolation_mode: false,
            collateral_positions: Vec::new(),
            debt_positions: vec![make_debt_position_data(
                ZERO_ADDR_HEX,
                "USDC",
                100,
                100,
                None,
            )],
            total_collateral_value: U256::ZERO,
            total_debt_value: U256::ZERO,
            health_factor: None,
            max_ltv_ratio: None,
        };
        assert!(summary.has_debt());
    }

    // ── TestBuildCollateralPositionData ──────────────────────────────────

    #[expect(clippy::too_many_arguments)]
    fn make_collateral_record(
        asset_id: i64,
        balance: u128,
        underlying_address: &str,
        underlying_symbol: &str,
        e_mode_category_id: Option<i64>,
        asset_lt: i64,
        asset_ltv: i64,
        emode_lt: Option<i64>,
        emode_ltv: Option<i64>,
    ) -> AaveCollateralPositionRecord {
        AaveCollateralPositionRecord {
            asset_id,
            balance: U256::from(balance),
            underlying_address: underlying_address.to_string(),
            underlying_symbol: Some(underlying_symbol.to_string()),
            liquidity_index: U256::from(10u128).pow(U256::from(27u64)), // 1e27 (RAY)
            e_mode_category_id,
            asset_lt,
            asset_ltv,
            emode_lt,
            emode_ltv,
        }
    }

    #[test]
    fn build_collateral_from_record() {
        let record =
            make_collateral_record(1, 1000, &addr(1), "WETH", None, 8000, 7500, None, None);
        let result = build_collateral_position_data(&record, 0, true, None).unwrap();
        assert_eq!(result.asset_address, addr(1));
        assert_eq!(result.asset_symbol.as_deref(), Some("WETH"));
        assert_eq!(result.scaled_balance, U256::from(1000u64));
        assert!(result.is_enabled_as_collateral);
        assert_eq!(result.liquidation_threshold, 8000);
        assert_eq!(result.ltv, 7500);
    }

    #[test]
    fn build_collateral_with_emode() {
        let record = make_collateral_record(
            1,
            1000,
            &addr(1),
            "WETH",
            Some(1),
            8000,
            7500,
            Some(9500),
            Some(9000),
        );
        let result = build_collateral_position_data(&record, 1, true, None).unwrap();
        assert_eq!(result.liquidation_threshold, 9500); // eMode override
        assert_eq!(result.ltv, 9000);
        assert!(result.in_emode);
    }

    #[test]
    fn build_collateral_with_different_emode() {
        let record = make_collateral_record(
            1,
            1000,
            &addr(1),
            "WETH",
            Some(1),
            8000,
            7500,
            Some(9500),
            Some(9000),
        );
        let result = build_collateral_position_data(&record, 2, true, None).unwrap();
        assert_eq!(result.liquidation_threshold, 8000); // standard, not eMode
        assert_eq!(result.ltv, 7500);
        assert!(!result.in_emode);
    }

    #[test]
    fn build_collateral_disabled() {
        let record =
            make_collateral_record(1, 1000, &addr(1), "WETH", None, 8000, 7500, None, None);
        let result = build_collateral_position_data(&record, 0, false, None).unwrap();
        assert_eq!(result.effective_liquidation_threshold(), 0);
    }

    #[test]
    fn build_collateral_with_price() {
        let record = make_collateral_record(1, 1, &addr(1), "WETH", None, 8000, 7500, None, None);
        let result =
            build_collateral_position_data(&record, 0, true, Some(U256::from(2_000_000_000u64)))
                .unwrap();
        assert_eq!(result.price, Some(U256::from(2_000_000_000u64)));
    }

    // ── TestBuildDebtPositionData ────────────────────────────────────────

    fn make_debt_record(
        asset_id: i64,
        balance: u128,
        underlying_address: &str,
        underlying_symbol: &str,
        e_mode_category_id: Option<i64>,
    ) -> AaveDebtPositionRecord {
        AaveDebtPositionRecord {
            asset_id,
            balance: U256::from(balance),
            underlying_address: underlying_address.to_string(),
            underlying_symbol: Some(underlying_symbol.to_string()),
            borrow_index: U256::from(10u128).pow(U256::from(27u64)), // 1e27 (RAY)
            e_mode_category_id,
        }
    }

    #[test]
    fn build_debt_from_record() {
        let record = make_debt_record(1, 500, &addr(1), "USDC", None);
        let result = build_debt_position_data(&record, 0, None).unwrap();
        assert_eq!(result.asset_address, addr(1));
        assert_eq!(result.asset_symbol.as_deref(), Some("USDC"));
        assert_eq!(result.scaled_balance, U256::from(500u64));
        assert!(!result.in_emode);
    }

    #[test]
    fn build_debt_with_emode() {
        let record = make_debt_record(1, 500, &addr(1), "USDC", Some(1));
        let result = build_debt_position_data(&record, 1, None).unwrap();
        assert!(result.in_emode);
    }

    // ── TestAnalyzeUserPosition ──────────────────────────────────────────

    fn make_user(
        id: i64,
        address: &str,
        market_id: i64,
        e_mode: i64,
        is_isolation_mode: bool,
        isolation_mode_debt: u128,
        isolation_debt_ceiling: Option<u128>,
    ) -> AaveUserRecord {
        AaveUserRecord {
            id,
            address: address.to_string(),
            market_id,
            e_mode,
            is_isolation_mode,
            isolation_mode_debt: U256::from(isolation_mode_debt),
            isolation_debt_ceiling: isolation_debt_ceiling.map(U256::from),
        }
    }

    #[test]
    fn analyze_no_debt_no_health_factor() {
        let user = make_user(1, &addr(99), 1, 0, false, 0, None);
        let collateral = vec![make_collateral_record(
            1,
            1000,
            &addr(1),
            "WETH",
            None,
            8000,
            7500,
            None,
            None,
        )];
        let result = analyze_user_position(&user, &collateral, &[], &HashMap::new(), None).unwrap();
        assert!(result.health_factor.is_none());
    }

    #[test]
    fn analyze_safe_position() {
        let user = make_user(1, &addr(99), 1, 0, false, 0, None);
        let collateral = vec![make_collateral_record(
            1,
            1000,
            &addr(1),
            "WETH",
            None,
            8000,
            7500,
            None,
            None,
        )];
        let debt = vec![make_debt_record(2, 500, &addr(2), "USDC", None)];
        let mut cfg = HashMap::new();
        cfg.insert(1i64, true);
        let result = analyze_user_position(&user, &collateral, &debt, &cfg, None).unwrap();
        assert!(result.health_factor.is_some());
        assert!(result.health_factor.unwrap() > 1.0);
    }

    #[test]
    fn analyze_disabled_collateral_zero_hf() {
        let user = make_user(1, &addr(99), 1, 0, false, 0, None);
        let collateral = vec![make_collateral_record(
            1,
            1000,
            &addr(1),
            "WETH",
            None,
            8000,
            7500,
            None,
            None,
        )];
        let debt = vec![make_debt_record(2, 500, &addr(2), "USDC", None)];
        let mut cfg = HashMap::new();
        cfg.insert(1i64, false); // disabled
        let result = analyze_user_position(&user, &collateral, &debt, &cfg, None).unwrap();
        assert!(result.health_factor.is_some());
        assert!((result.health_factor.unwrap() - 0.0).abs() < 1e-9);
    }

    #[test]
    fn analyze_isolation_mode() {
        let user = make_user(1, &addr(99), 1, 0, true, 5000, Some(1000));
        let collateral = vec![make_collateral_record(
            1,
            1000,
            &addr(1),
            "WETH",
            None,
            8000,
            7500,
            None,
            None,
        )];
        let debt = vec![make_debt_record(2, 500, &addr(2), "USDC", None)];
        let result =
            analyze_user_position(&user, &collateral, &debt, &HashMap::new(), None).unwrap();
        assert!(result.is_isolation_mode);
        assert!(result.health_factor.is_some());
    }

    #[test]
    fn analyze_user_metadata_preserved() {
        let user = make_user(1, &addr(42), 7, 0, false, 0, None);
        let result = analyze_user_position(&user, &[], &[], &HashMap::new(), None).unwrap();
        assert_eq!(result.user_address, addr(42));
        assert_eq!(result.market_id, 7);
        assert_eq!(result.emode_category_id, None);
    }

    // ── PositionAnalysisResult categorize / sort ─────────────────────────

    #[test]
    fn categorize_liquidatable() {
        let mut result = PositionAnalysisResult::default();
        result.categorize(make_summary(Some(0.5)), HEALTH_FACTOR_AT_RISK_THRESHOLD);
        assert_eq!(result.liquidatable_count(), 1);
        assert_eq!(result.at_risk_count(), 0);
        assert_eq!(result.total_users(), 1);
    }

    #[test]
    fn categorize_at_risk() {
        let mut result = PositionAnalysisResult::default();
        result.categorize(make_summary(Some(1.05)), HEALTH_FACTOR_AT_RISK_THRESHOLD);
        assert_eq!(result.liquidatable_count(), 0);
        assert_eq!(result.at_risk_count(), 1);
    }

    #[test]
    fn categorize_safe() {
        let mut result = PositionAnalysisResult::default();
        result.categorize(make_summary(Some(1.5)), HEALTH_FACTOR_AT_RISK_THRESHOLD);
        assert_eq!(result.safe_users.len(), 1);
    }

    #[test]
    fn categorize_none_hf_is_safe() {
        let mut result = PositionAnalysisResult::default();
        result.categorize(make_summary(None), HEALTH_FACTOR_AT_RISK_THRESHOLD);
        assert_eq!(result.safe_users.len(), 1);
    }

    #[test]
    fn sort_by_risk_orders_lowest_first() {
        let mut result = PositionAnalysisResult::default();
        // Insert out of order; after sort, lower HF comes first.
        result.at_risk_users.push(make_summary(Some(1.08)));
        result.at_risk_users.push(make_summary(Some(1.02)));
        result.at_risk_users.push(make_summary(Some(1.05)));
        result.sort_by_risk();
        let hfs: Vec<Option<f64>> = result
            .at_risk_users
            .iter()
            .map(|s| s.health_factor)
            .collect();
        assert_eq!(hfs, vec![Some(1.02), Some(1.05), Some(1.08)]);
    }
}
