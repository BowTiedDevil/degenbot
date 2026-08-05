//! Pure-Rust Curve swap `get_dy` + calculator layer (task `YY64IT`, epic
//! `TV72EG`).
//!
//! Ports the Python `curve/get_dy` orchestration into `degenbot-curve-math`:
//! the per-`SwapStyle` calculators (`StandardDyCalculator`,
//! `LiveAdminDynamicDyCalculator`, `CryptoDyCalculator`), the two metapool
//! calculators (`MetapoolDyCalculator`, `MetapoolUnderlyingDyCalculator`),
//! the dynamic-fee helper, and the A-ramping resolution — all as pure
//! functions over a pre-resolved [`DyCalculationInputs`] snapshot (mirroring
//! the Python `curve/calculators/*` + `_a()` + `_resolve_calculation_inputs`).
//! It reuses the already-ported D / Y / YD invariant primitives in
//! [`crate::stableswap`]; no I/O and no `pyo3`, so a standalone `cargo add
//! degenbot` consumer can actually calculate a swap.
//!
//! The only external surface a calc path touches is the **base pool** (the
//! metapool-underlying path delegates `calc_token_amount` / `get_dy` /
//! `calc_withdraw_one_coin` to it). That dependency is expressed as the
//! sync [`CurveBasePoolPort`] trait (the Rust twin of the Python
//! `BasePoolPort` Protocol); the bot's `CurvePoolsState` implements it, and
//! tests use a canned stub.

use alloy::primitives::{Address, U256};

use crate::stableswap::{
    stableswap_get_y, stableswap_newton_y, stableswap_reduction_coefficient, CurveMathError,
    DVariant,
};

// ---------------------------------------------------------------------------
// Style enums (canonical discriminant table; see degenbot-pools/curve_strategies)
// ---------------------------------------------------------------------------

/// Which swap computation path `get_dy` uses. Mirrors `SwapStyle` (`auto()`,
/// 1-based). `CYTOKEN` maps to the same arithmetic as `STANDARD`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapStyle {
    Standard = 1,
    RateAdjusted = 2,
    RawBalance = 3,
    Crypto = 4,
    LiveAdmin = 5,
    LiveAdminDynamic = 6,
    LiveAdminDynamicPrecision = 7,
    LiveAdminOracle = 8,
    NoOneFeeRate = 9,
    CyToken = 10,
    RateAdjustedNoOne = 11,
}

impl SwapStyle {
    /// Map the stored `u8` discriminant (from
    /// `RegisterCurvePoolParams.swap_style`) to the calculator style.
    #[must_use]
    pub fn try_from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Standard),
            2 => Some(Self::RateAdjusted),
            3 => Some(Self::RawBalance),
            4 => Some(Self::Crypto),
            5 => Some(Self::LiveAdmin),
            6 => Some(Self::LiveAdminDynamic),
            7 => Some(Self::LiveAdminDynamicPrecision),
            8 => Some(Self::LiveAdminOracle),
            9 => Some(Self::NoOneFeeRate),
            10 => Some(Self::CyToken),
            11 => Some(Self::RateAdjustedNoOne),
            _ => None,
        }
    }
}

/// A-ramping resolution outcome. The pool stores a fixed `a_coefficient`
/// (from the `A()` read) plus, for ramping pools, the initial/future ramp
/// endpoints and times. `resolve_ramping_a` returns the *raw* amplified A
/// (the Python `_a()`); the caller divides by `A_PRECISION` for `YVariant0`
/// pools (see [`resolve_amp`]).
#[derive(Clone, Copy, Debug)]
pub struct ARampingParams {
    pub a_coefficient: u128,
    pub initial_a_coefficient: Option<u128>,
    pub future_a_coefficient: Option<u128>,
    pub initial_a_coefficient_time: Option<u64>,
    pub future_a_coefficient_time: Option<u64>,
    pub create_timestamp: Option<u64>,
    pub a_precision: u32,
}

/// Pre-resolved snapshot the calculators read exclusively (the Rust twin of
/// `DyCalculationInputs`). All I/O, cache lookups, and rate resolution happen
/// before this object is built. Optional fields are populated per the swap
/// style (crypto fills `d`/`gamma`/`price_scale`; live-admin fills the
/// balance triples; metapool fills `virtual_price`/`scaled_redemption_price`).
#[derive(Clone, Debug)]
pub struct DyCalculationInputs {
    pub precision: U256,       // 1e18
    pub fee_denominator: U256, // 1e10
    pub fee: U256,
    pub n_coins: usize,
    pub balances: Vec<U256>,
    pub rate_multipliers: Vec<U256>,
    pub precision_multipliers: Vec<U256>,
    pub offpeg_fee_multiplier: U256,
    pub fee_gamma: U256,
    pub mid_fee: U256,
    pub out_fee: U256,
    pub address: Address,
    pub resolved_rates: Vec<U256>,
    pub xp: Vec<U256>,
    pub block_number: u64,
    pub block_timestamp: u64,
    pub amp: U256,
    pub d_variant: DVariant,
    pub y_variant: crate::stableswap::YVariant,
    pub a_precision: U256,
    pub swap_style: u8,
    /// Whether this pool is a metapool (has a base pool). When `true`,
    /// `calculate_dy` dispatches the `MetapoolDyCalculator` fast-path by
    /// `metapool_rate_style`; `calculate_dy_underlying` delegates base-pool
    /// ops through the `CurveBasePoolPort`.
    pub metapool: bool,
    pub metapool_rate_style: u8,
    pub metapool_underlying_style: u8,
    // crypto I/O
    pub d: Option<U256>,
    pub gamma: Option<U256>,
    pub price_scale: Option<Vec<U256>>,
    // live-admin I/O
    pub live_balances: Option<Vec<U256>>,
    pub admin_balances: Option<Vec<U256>>,
    pub effective_balances: Option<Vec<U256>>,
    // metapool I/O
    pub virtual_price: Option<U256>,
    pub scaled_redemption_price: Option<U256>,
}

/// The slice of the base-pool surface metapool calc paths need (the Rust twin
/// of the Python `BasePoolPort` Protocol; ADR-005 `BQM2OA` sibling). Sync —
/// matches the provider/calc call discipline. The bot's `CurvePoolsState`
/// implements it; tests use a canned stub.
pub trait CurveBasePoolPort {
    /// Number of base-pool coins.
    fn token_count(&self) -> usize;
    /// Base-pool swap fee (`FEE_DENOMINATOR` units).
    fn fee(&self) -> U256;
    /// Deposit token amount for `amounts` (slippage-adjusted).
    fn calc_token_amount(&self, amounts: &[U256], block: u64) -> Result<U256, CurveSwapError>;
    /// Output `dy` swapping `dx` of base coin `i` → base coin `j`.
    fn get_dy(&self, i: usize, j: usize, dx: U256, block: u64) -> Result<U256, CurveSwapError>;
    /// Single-coin withdrawal amount for `_token_amount` of base coin `i`.
    fn calc_withdraw_one_coin(
        &self,
        token_amount: U256,
        i: usize,
        block: u64,
    ) -> Result<U256, CurveSwapError>;
}

/// Recoverable swap-calculation failure. Mirrors the Python calculator
/// raising `EVMRevertError` (non-convergence / safety) or an assertion error
/// (index / args).
#[derive(Debug, PartialEq, Eq)]
pub enum CurveSwapError {
    /// The invariant solver rejected the inputs (non-convergence, overflow,
    /// out-of-range index, unsafe value).
    Invariant(CurveMathError),
    /// Unknown `swap_style` discriminant.
    UnknownStyle(u8),
    /// `calculate_dy_underlying` called without a base pool.
    NotMetapool,
    /// A required optional input was missing for the active style.
    MissingValue(&'static str),
    /// The base pool delegated operation failed.
    BasePool(Box<CurveSwapError>),
    /// Coin index out of range / `i == j`.
    IndexOutOfBounds,
    /// `dx == 0` (the crypto contract reverts on zero-input swaps).
    ZeroInput,
    /// A `uint256` arithmetic operation overflowed.
    Overflow,
    /// A division by an unexpected zero denominator.
    DivisionByZero,
}

impl From<CurveMathError> for CurveSwapError {
    fn from(e: CurveMathError) -> Self {
        Self::Invariant(e)
    }
}

impl std::fmt::Display for CurveSwapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invariant(e) => write!(f, "invariant: {e:?}"),
            Self::UnknownStyle(s) => write!(f, "unknown swap_style {s}"),
            Self::NotMetapool => write!(f, "underlying calc requires a base pool"),
            Self::MissingValue(v) => write!(f, "missing required input: {v}"),
            Self::BasePool(e) => write!(f, "base-pool op failed: {e}"),
            Self::IndexOutOfBounds => write!(f, "coin index out of range or i == j"),
            Self::ZeroInput => write!(f, "do not exchange 0 coins"),
            Self::Overflow => write!(f, "uint256 overflow"),
            Self::DivisionByZero => write!(f, "division by zero"),
        }
    }
}

impl std::error::Error for CurveSwapError {}

// ---------------------------------------------------------------------------
// Checked-arithmetic helpers
// ---------------------------------------------------------------------------

fn cmul(a: U256, b: U256) -> Result<U256, CurveSwapError> {
    a.checked_mul(b).ok_or(CurveSwapError::Overflow)
}
fn cadd(a: U256, b: U256) -> Result<U256, CurveSwapError> {
    a.checked_add(b).ok_or(CurveSwapError::Overflow)
}
fn csub(a: U256, b: U256) -> Result<U256, CurveSwapError> {
    a.checked_sub(b).ok_or(CurveSwapError::Overflow)
}
fn cdiv(a: U256, b: U256) -> Result<U256, CurveSwapError> {
    if b.is_zero() {
        return Err(CurveSwapError::DivisionByZero);
    }
    Ok(a / b)
}

// ---------------------------------------------------------------------------
// A-ramping resolution
// ---------------------------------------------------------------------------

/// Resolve the raw amplified A at `timestamp`, mirroring
/// `CurveStableswapPool._a(timestamp)`.
///
/// - No ramp endpoints → `a_coefficient * a_precision`.
/// - Ramp finished (`create_timestamp >= future_a_time`) → `future_a`.
/// - `timestamp >= future_a_time` → `future_a`.
/// - Otherwise linear interpolation between `initial_a` and `future_a`.
pub fn resolve_ramping_a(p: ARampingParams, timestamp: u64) -> Result<U256, CurveSwapError> {
    let (Some(initial_a), Some(future_a)) = (p.initial_a_coefficient, p.future_a_coefficient)
    else {
        return cmul(U256::from(p.a_coefficient), U256::from(p.a_precision));
    };
    let (Some(t0), Some(t1), Some(create)) = (
        p.initial_a_coefficient_time,
        p.future_a_coefficient_time,
        p.create_timestamp,
    ) else {
        // Ramp endpoints without times is degenerate; fall back to the fixed A.
        return cmul(U256::from(p.a_coefficient), U256::from(p.a_precision));
    };
    if create >= t1 {
        return Ok(U256::from(future_a));
    }

    let a1 = U256::from(future_a);
    let a0 = U256::from(initial_a);
    let scaled = if timestamp < t1 {
        if a1 > a0 {
            a0 + (a1 - a0) * U256::from(timestamp.saturating_sub(t0))
                / U256::from(t1.saturating_sub(t0))
        } else {
            a0 - (a0 - a1) * U256::from(timestamp.saturating_sub(t0))
                / U256::from(t1.saturating_sub(t0))
        }
    } else {
        a1
    };
    Ok(scaled)
}

/// Apply the `YVariant0` A_PRECISION divisor to a raw amp (mirrors the
/// `y_variant == VARIANT_0` branch in `_resolve_calculation_inputs_via_io`).
#[must_use]
pub fn resolve_amp(raw_a: U256, a_precision: U256, y_variant: crate::stableswap::YVariant) -> U256 {
    if y_variant.omits_a_precision() {
        raw_a / a_precision
    } else {
        raw_a
    }
}

// ---------------------------------------------------------------------------
// StandardDyCalculator axes
// ---------------------------------------------------------------------------

/// Whether the standard calculator builds XP from rate-adjusted balances or
/// the raw reserve balances.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BalanceSource {
    RateAdjustedXp,
    RawBalances,
}

/// Which rate tuple the standard calculator reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RateSource {
    ResolvedRates,
    RateMultipliers,
}

/// Order of fee and rate-conversion on the raw `dy`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConversionStyle {
    FeeThenRate,
    RateThenFee,
    FeeOnly,
}

/// The four independent axes parameterizing the standard calculator.
#[derive(Clone, Copy, Debug)]
struct StandardAxes {
    balance_source: BalanceSource,
    rate_source: RateSource,
    subtract_one: bool,
    conversion_style: ConversionStyle,
}

impl Default for StandardAxes {
    fn default() -> Self {
        Self {
            balance_source: BalanceSource::RateAdjustedXp,
            rate_source: RateSource::ResolvedRates,
            subtract_one: true,
            conversion_style: ConversionStyle::FeeThenRate,
        }
    }
}

/// Map a `SwapStyle` to its standard-calculator axes (mirrors
/// `make_swap_style_calculator`). Live-admin styles use the effective/xp
/// values the caller pre-computed into `inputs`.
fn standard_axes(style: SwapStyle) -> StandardAxes {
    let mut a = StandardAxes::default();
    match style {
        SwapStyle::RateAdjusted | SwapStyle::RateAdjustedNoOne => {
            a.conversion_style = ConversionStyle::RateThenFee;
            if style == SwapStyle::RateAdjustedNoOne {
                a.subtract_one = false;
            }
        }
        SwapStyle::RawBalance => {
            a.balance_source = BalanceSource::RawBalances;
            a.conversion_style = ConversionStyle::FeeOnly;
        }
        SwapStyle::NoOneFeeRate => {
            a.subtract_one = false;
        }
        SwapStyle::LiveAdmin => {
            a.rate_source = RateSource::RateMultipliers;
        }
        // STANDARD / CYTOKEN / LIVE_ADMIN_ORACLE use all-default axes; the
        // dynamic/crypto styles are dispatched before this function is called.
        _ => {}
    }
    a
}

/// The `StandardDyCalculator` core (mirrors `StandardDyCalculator.calculate`).
fn calculate_standard_dy(
    i: usize,
    j: usize,
    dx: U256,
    inputs: &DyCalculationInputs,
    axes: StandardAxes,
) -> Result<U256, CurveSwapError> {
    if i == j || i >= inputs.n_coins || j >= inputs.n_coins {
        return Err(CurveSwapError::IndexOutOfBounds);
    }

    let (xp, rates, x): (Vec<U256>, Vec<U256>, U256) =
        if axes.balance_source == BalanceSource::RawBalances {
            let xp = inputs.balances.clone();
            let x = xp[i] + dx;
            let rates = vec![inputs.precision; inputs.n_coins]; // identity rates
            (xp, rates, x)
        } else {
            let rates = if axes.rate_source == RateSource::RateMultipliers {
                inputs.rate_multipliers.clone()
            } else {
                inputs.resolved_rates.clone()
            };
            let xp = inputs.xp.clone();
            let x = cadd(xp[i], cdiv(cmul(dx, rates[i])?, inputs.precision)?)?;
            (xp, rates, x)
        };

    let y = stableswap_get_y(
        i,
        j,
        x,
        &xp,
        inputs.amp,
        U256::from(inputs.n_coins),
        inputs.a_precision,
        inputs.y_variant,
        inputs.d_variant,
    )?;

    let raw_dy = csub(
        csub(xp[j], y)?,
        if axes.subtract_one {
            U256::from(1u8)
        } else {
            U256::ZERO
        },
    )?;

    match axes.conversion_style {
        ConversionStyle::FeeThenRate => {
            let fee = cdiv(cmul(inputs.fee, raw_dy)?, inputs.fee_denominator)?;
            cdiv(cmul(csub(raw_dy, fee)?, inputs.precision)?, rates[j])
        }
        ConversionStyle::RateThenFee => {
            let converted = cdiv(cmul(raw_dy, inputs.precision)?, rates[j])?;
            let fee = cdiv(cmul(inputs.fee, converted)?, inputs.fee_denominator)?;
            csub(converted, fee)
        }
        ConversionStyle::FeeOnly => {
            let fee = cdiv(cmul(inputs.fee, raw_dy)?, inputs.fee_denominator)?;
            csub(raw_dy, fee)
        }
    }
}

// ---------------------------------------------------------------------------
// Dynamic offpeg fee + LiveAdminDynamicDyCalculator
// ---------------------------------------------------------------------------

/// Compute the dynamic offpeg fee (mirrors `_dynamic_fee`). Returns the plain
/// `_fee` when `_feemul <= fee_denominator`.
fn dynamic_fee(
    xpi: U256,
    xpj: U256,
    fee: U256,
    feemul: U256,
    fee_denominator: U256,
) -> Result<U256, CurveSwapError> {
    if feemul <= fee_denominator {
        return Ok(fee);
    }
    let xps2 = cmul(cadd(xpi, xpj)?, cadd(xpi, xpj)?)?;
    let numerator = cmul(feemul, fee)?;
    let term = cdiv(
        cmul(
            cmul(csub(feemul, fee_denominator)?, U256::from(4u8))?,
            cmul(xpi, xpj)?,
        )?,
        xps2,
    )?;
    let denominator = cadd(term, fee_denominator)?;
    cdiv(numerator, denominator)
}

/// Whether the live-admin dynamic calculator applies precision multipliers to
/// the effective balances (mirrors `PrecisionMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrecisionMode {
    None,
    PrecisionMultipliers,
}

/// Live-admin dynamic-fee core (mirrors `LiveAdminDynamicDyCalculator.calculate`).
fn calculate_live_admin_dynamic_dy(
    i: usize,
    j: usize,
    dx: U256,
    inputs: &DyCalculationInputs,
    mode: PrecisionMode,
) -> Result<U256, CurveSwapError> {
    if i == j || i >= inputs.n_coins || j >= inputs.n_coins {
        return Err(CurveSwapError::IndexOutOfBounds);
    }
    let effective = inputs
        .effective_balances
        .as_deref()
        .ok_or(CurveSwapError::MissingValue("effective_balances"))?;

    let (xp, x): (Vec<U256>, U256) = if mode == PrecisionMode::PrecisionMultipliers {
        let xp = effective
            .iter()
            .zip(inputs.precision_multipliers.iter())
            .map(|(b, r)| cmul(*b, *r))
            .collect::<Result<Vec<_>, _>>()?;
        let x = cadd(xp[i], cmul(dx, inputs.precision_multipliers[i])?)?;
        (xp, x)
    } else {
        let xp = effective.to_vec();
        let x = cadd(xp[i], dx)?;
        (xp, x)
    };

    let y = stableswap_get_y(
        i,
        j,
        x,
        &xp,
        inputs.amp,
        U256::from(inputs.n_coins),
        inputs.a_precision,
        inputs.y_variant,
        inputs.d_variant,
    )?;

    let dy = if mode == PrecisionMode::PrecisionMultipliers {
        cdiv(csub(xp[j], y)?, inputs.precision_multipliers[j])?
    } else {
        csub(xp[j], y)?
    };

    let dynamic = dynamic_fee(
        cdiv(cadd(xp[i], x)?, U256::from(2u8))?,
        cdiv(cadd(xp[j], y)?, U256::from(2u8))?,
        inputs.fee,
        inputs.offpeg_fee_multiplier,
        inputs.fee_denominator,
    )?;
    let fee_ = cdiv(cmul(dynamic, dy)?, inputs.fee_denominator)?;
    csub(dy, fee_)
}

// ---------------------------------------------------------------------------
// CryptoDyCalculator
// ---------------------------------------------------------------------------

/// Tricrypto precision multipliers (hard-coded in the contract), mirrored
/// verbatim from the Python.
const TRICRYPTO_PRECISIONS: [U256; 3] = [
    U256::from_limbs([1_000_000_000_000, 0, 0, 0]), // 1e12
    U256::from_limbs([10_000_000_000, 0, 0, 0]),    // 1e10
    U256::from_limbs([1, 0, 0, 0]),                 // 1
];

fn one_e18() -> U256 {
    U256::from(10u64).pow(U256::from(18u64))
}

/// Crypto-dy core (mirrors `CryptoDyCalculator.calculate`): Newton's method,
/// dynamic mid/out fee via `reduction_coefficient`, `price_scale` adjustment.
fn calculate_crypto_dy(
    i: usize,
    j: usize,
    dx: U256,
    inputs: &DyCalculationInputs,
) -> Result<U256, CurveSwapError> {
    if i == j {
        return Err(CurveSwapError::IndexOutOfBounds);
    }
    if i >= inputs.n_coins || j >= inputs.n_coins {
        return Err(CurveSwapError::IndexOutOfBounds);
    }
    if dx.is_zero() {
        return Err(CurveSwapError::ZeroInput);
    }
    let d = inputs.d.ok_or(CurveSwapError::MissingValue("d"))?;
    let gamma = inputs.gamma.ok_or(CurveSwapError::MissingValue("gamma"))?;
    let price_scale = inputs
        .price_scale
        .as_deref()
        .ok_or(CurveSwapError::MissingValue("price_scale"))?;

    let n = inputs.n_coins;
    let mut xp = inputs.balances.clone();
    xp[i] = cadd(xp[i], dx)?;
    xp[0] = cmul(xp[0], TRICRYPTO_PRECISIONS[0])?;
    for k in 0..n.saturating_sub(1) {
        xp[k + 1] = cdiv(
            cmul(
                cmul(xp[k + 1], price_scale[k])?,
                TRICRYPTO_PRECISIONS[k + 1],
            )?,
            inputs.precision,
        )?;
    }

    let y = stableswap_newton_y(
        inputs.amp,
        gamma,
        &xp,
        d,
        j,
        U256::from(n),
        inputs.a_precision,
    )?;
    let mut dy = csub(csub(xp[j], y)?, U256::from(1u8))?;

    xp[j] = y;
    if j > 0 {
        dy = cdiv(cmul(dy, inputs.precision)?, price_scale[j - 1])?;
    }
    dy = cdiv(dy, TRICRYPTO_PRECISIONS[j])?;

    let f = stableswap_reduction_coefficient(&xp, inputs.fee_gamma, U256::from(n))?;
    let one = one_e18();
    let fee_calc = cdiv(
        cadd(
            cmul(inputs.mid_fee, f)?,
            cmul(inputs.out_fee, csub(one, f)?)?,
        )?,
        one,
    )?;
    let fee = cdiv(
        cmul(fee_calc, dy)?,
        U256::from(10u64).pow(U256::from(10u64)),
    )?;
    csub(dy, fee)
}

// ---------------------------------------------------------------------------
// MetapoolDyCalculator (get_dy metapool fast-path)
// ---------------------------------------------------------------------------

/// Metapool-dy core (mirrors `MetapoolDyCalculator.calculate`). `rate_style`
/// is the `MetapoolRateStyle` discriminant (1=STANDARD, 2=PRECISION_VP,
/// 3=REDEMPTION_VP).
fn calculate_metapool_dy(
    i: usize,
    j: usize,
    dx: U256,
    inputs: &DyCalculationInputs,
    rate_style: u8,
) -> Result<U256, CurveSwapError> {
    if i == j || i >= inputs.n_coins || j >= inputs.n_coins {
        return Err(CurveSwapError::IndexOutOfBounds);
    }
    let vp = inputs
        .virtual_price
        .ok_or(CurveSwapError::MissingValue("virtual_price"))?;
    let rates = match rate_style {
        1 => [inputs.rate_multipliers[0], vp],
        2 => [inputs.precision, vp],
        3 => [
            inputs
                .scaled_redemption_price
                .ok_or(CurveSwapError::MissingValue("scaled_redemption_price"))?,
            vp,
        ],
        _ => return Err(CurveSwapError::UnknownStyle(rate_style)),
    };

    let xp = inputs
        .balances
        .iter()
        .zip(rates)
        .map(|(b, r)| cdiv(cmul(r, *b)?, inputs.precision))
        .collect::<Result<Vec<_>, _>>()?;
    let x = cadd(xp[i], cdiv(cmul(dx, rates[i])?, inputs.precision)?)?;
    let y = stableswap_get_y(
        i,
        j,
        x,
        &xp,
        inputs.amp,
        U256::from(inputs.n_coins),
        inputs.a_precision,
        inputs.y_variant,
        inputs.d_variant,
    )?;
    let dy = csub(csub(xp[j], y)?, U256::from(1u8))?;
    let fee = cdiv(cmul(inputs.fee, dy)?, inputs.fee_denominator)?;
    cdiv(cmul(csub(dy, fee)?, inputs.precision)?, rates[j])
}

// ---------------------------------------------------------------------------
// MetapoolUnderlyingDyCalculator (get_dy_underlying)
// ---------------------------------------------------------------------------

/// Metapool-underlying core (mirrors
/// `MetapoolUnderlyingDyCalculator.calculate`). `underlying_style` is the
/// `MetapoolUnderlyingStyle` discriminant (1=STANDARD, 2=REDEMPTION,
/// 3=PRECISION_VP).
fn calculate_metapool_underlying_dy(
    i: usize,
    j: usize,
    dx: U256,
    inputs: &DyCalculationInputs,
    underlying_style: u8,
    base: &dyn CurveBasePoolPort,
) -> Result<U256, CurveSwapError> {
    // NB: underlying indices span BOTH the meta coins and the base-pool coins,
    // so `j` may exceed `n_coins` — only the meta-vs-base split (base_i/base_j)
    // bounds it. Keep the `i == j` guard only.
    if i == j {
        return Err(CurveSwapError::IndexOutOfBounds);
    }
    let vp = inputs
        .virtual_price
        .ok_or(CurveSwapError::MissingValue("virtual_price"))?;
    let base_n = base.token_count();
    let max_coin = inputs.n_coins.saturating_sub(1);

    let rates = match underlying_style {
        1 => [inputs.rate_multipliers[0], vp],
        2 => [
            inputs
                .scaled_redemption_price
                .ok_or(CurveSwapError::MissingValue("scaled_redemption_price"))?,
            vp,
        ],
        3 => [inputs.precision, vp],
        _ => return Err(CurveSwapError::UnknownStyle(underlying_style)),
    };

    let xp = inputs
        .balances
        .iter()
        .zip(rates)
        .map(|(b, r)| cdiv(cmul(r, *b)?, inputs.precision))
        .collect::<Result<Vec<_>, _>>()?;

    // `checked_sub(max_coin)` is `None` exactly when the index is a meta coin
    // (i < max_coin), `Some(k)` when it is the k-th base coin.
    let base_i = i.checked_sub(max_coin);
    let base_j = j.checked_sub(max_coin);
    let mut meta_i = max_coin;
    let mut meta_j = max_coin;
    if base_i.is_none() {
        meta_i = i;
    }
    if base_j.is_none() {
        meta_j = j;
    }

    let x = match base_i {
        None => match underlying_style {
            2 => {
                let rp = inputs
                    .scaled_redemption_price
                    .ok_or(CurveSwapError::MissingValue("scaled_redemption_price"))?;
                cadd(xp[i], cdiv(cmul(dx, rp)?, inputs.precision)?)?
            }
            3 => cadd(xp[i], dx)?,
            1 => cadd(xp[i], cmul(dx, inputs.precision_multipliers[i])?)?,
            _ => return Err(CurveSwapError::UnknownStyle(underlying_style)),
        },
        Some(bi) => match base_j {
            // i is a base coin, j is the meta coin → deposit into the base pool.
            None => {
                if bi >= base_n {
                    return Err(CurveSwapError::IndexOutOfBounds);
                }
                let mut base_inputs = vec![U256::ZERO; base_n];
                base_inputs[bi] = dx;
                let token_amt = base.calc_token_amount(&base_inputs, inputs.block_number)?;
                let mut x = cdiv(cmul(token_amt, vp)?, inputs.precision)?;
                x = csub(
                    x,
                    cdiv(
                        cmul(x, base.fee())?,
                        cmul(U256::from(2u8), inputs.fee_denominator)?,
                    )?,
                )?;
                cadd(x, xp[max_coin])?
            }
            // Both are base coins → delegate to the base pool's own get_dy.
            Some(bj) => return base.get_dy(bi, bj, dx, inputs.block_number),
        },
    };

    let y = stableswap_get_y(
        meta_i,
        meta_j,
        x,
        &xp,
        inputs.amp,
        U256::from(inputs.n_coins),
        inputs.a_precision,
        inputs.y_variant,
        inputs.d_variant,
    )?;
    let mut dy = csub(csub(xp[meta_j], y)?, U256::from(1u8))?;
    dy = csub(dy, cdiv(cmul(inputs.fee, dy)?, inputs.fee_denominator)?)?;

    match underlying_style {
        2 => {
            if j == 0 {
                let rp = inputs
                    .scaled_redemption_price
                    .ok_or(CurveSwapError::MissingValue("scaled_redemption_price"))?;
                dy = cdiv(cmul(dy, inputs.precision)?, rp)?;
            }
            if let Some(bj) = base_j {
                dy = base.calc_withdraw_one_coin(
                    cdiv(cmul(dy, inputs.precision)?, vp)?,
                    bj,
                    inputs.block_number,
                )?;
            }
        }
        3 => {
            // (PRECISION/VP conversion of coin 0 is the identity `//1e18`)
            if let Some(bj) = base_j {
                dy = base.calc_withdraw_one_coin(
                    cdiv(cmul(dy, inputs.precision)?, rates[1])?,
                    bj,
                    inputs.block_number,
                )?;
            }
        }
        1 => {
            if base_j.is_none() {
                dy = cdiv(dy, inputs.precision_multipliers[meta_j])?;
            } else if let Some(bj) = base_j {
                dy = base.calc_withdraw_one_coin(
                    cdiv(cmul(dy, inputs.precision)?, vp)?,
                    bj,
                    inputs.block_number,
                )?;
            }
        }
        _ => return Err(CurveSwapError::UnknownStyle(underlying_style)),
    }
    Ok(dy)
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Calculate the output `dy` swapping `dx` of coin `i` → coin `j` (the Rust
/// twin of `CurveStableswapPool.get_dy`).
///
/// When `inputs.metapool` is set the `MetapoolDyCalculator` fast-path is
/// dispatched on `inputs.metapool_rate_style`; otherwise the `SwapStyle`
/// calculator path is used. Pure — no I/O; `calculate_dy_underlying` (which
/// needs base-pool delegation) is separate.
pub fn calculate_dy(
    i: usize,
    j: usize,
    dx: U256,
    inputs: &DyCalculationInputs,
) -> Result<U256, CurveSwapError> {
    if inputs.metapool {
        return calculate_metapool_dy(i, j, dx, inputs, inputs.metapool_rate_style);
    }
    let style = SwapStyle::try_from_u8(inputs.swap_style)
        .ok_or(CurveSwapError::UnknownStyle(inputs.swap_style))?;
    match style {
        SwapStyle::LiveAdminDynamic => {
            calculate_live_admin_dynamic_dy(i, j, dx, inputs, PrecisionMode::None)
        }
        SwapStyle::LiveAdminDynamicPrecision => {
            calculate_live_admin_dynamic_dy(i, j, dx, inputs, PrecisionMode::PrecisionMultipliers)
        }
        SwapStyle::Crypto => calculate_crypto_dy(i, j, dx, inputs),
        SwapStyle::Standard
        | SwapStyle::RateAdjusted
        | SwapStyle::RawBalance
        | SwapStyle::LiveAdmin
        | SwapStyle::LiveAdminOracle
        | SwapStyle::NoOneFeeRate
        | SwapStyle::CyToken
        | SwapStyle::RateAdjustedNoOne => {
            calculate_standard_dy(i, j, dx, inputs, standard_axes(style))
        }
    }
}

/// Calculate the output `dy` for a metapool swap into/out of an underlying
/// base-pool coin (the Rust twin of `_get_dy_underlying`). Requires a base
/// pool; dispatches on `inputs.metapool_underlying_style`.
pub fn calculate_dy_underlying(
    i: usize,
    j: usize,
    dx: U256,
    inputs: &DyCalculationInputs,
    base: &dyn CurveBasePoolPort,
) -> Result<U256, CurveSwapError> {
    calculate_metapool_underlying_dy(i, j, dx, inputs, inputs.metapool_underlying_style, base)
}

#[cfg(test)]
mod tests;
