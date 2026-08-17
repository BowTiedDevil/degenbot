//! `PyO3` wrappers for the concentrated-liquidity math library.
//!
//! Thin binding layer that extracts Python arguments, calls the pure Rust
//! core, and converts results back to Python types.
//!
//! The GIL is held (not released) during computation because CL math
//! operations take ~20ns — far less than the ~200ns GIL release/reacquire
//! overhead.

use crate::prelude::*;
use alloy::primitives::{aliases::I256, U128, U256};
use pyo3::{exceptions::PyValueError, types::PyAny, PyTypeInfo};

// PyObject alias for pyo3 0.28+
type PyObject = pyo3::Py<pyo3::PyAny>;

use crate::conversion::alloy as alloy_py;
use degenbot_concentrated_liquidity_math::bit_math;
use degenbot_concentrated_liquidity_math::full_math;
use degenbot_concentrated_liquidity_math::liquidity_mapping::{
    self, BitmapAtWord, LiquidityAtTick,
};
use degenbot_concentrated_liquidity_math::liquidity_math;
use degenbot_concentrated_liquidity_math::sqrt_price_math;
use degenbot_concentrated_liquidity_math::swap_math;
use degenbot_concentrated_liquidity_math::tick_math;
use degenbot_concentrated_liquidity_math::unsafe_math;

/// Convert a Python int/bytes to U256.
fn extract_u256(obj: &Bound<'_, PyAny>) -> PyResult<U256> {
    if let Ok(v) = obj.extract::<u64>() {
        return Ok(U256::from(v));
    }
    if let Ok(v) = obj.extract::<u128>() {
        return Ok(U256::from(v));
    }
    // For arbitrary Python ints, convert via bytes
    let py = obj.py();
    if obj.is_instance(&pyo3::types::PyInt::type_object(py))? {
        let kwargs = pyo3::types::PyDict::new(py);
        kwargs.set_item("signed", false)?;
        let bytes_obj = obj.call_method("to_bytes", (32, "big"), Some(&kwargs))?;
        let bytes: &[u8] = bytes_obj.extract()?;
        Ok(U256::try_from_be_slice(bytes).unwrap_or(U256::ZERO))
    } else {
        Err(PyErr::new::<PyValueError, _>("Expected int"))
    }
}

/// Convert a U256 to a Python int (owned `PyObject`).
fn u256_to_py_obj(py: Python<'_>, v: U256) -> PyResult<PyObject> {
    let bound = alloy_py::u256_to_py(py, &v)?;
    Ok(bound.unbind())
}

/// Convert a Python int to I256 (signed 256-bit).
fn extract_i256(obj: &Bound<'_, PyAny>) -> PyResult<I256> {
    let py = obj.py();
    if obj.is_instance(&pyo3::types::PyInt::type_object(py))? {
        // Get 32-byte signed representation directly
        let kwargs = pyo3::types::PyDict::new(py);
        kwargs.set_item("signed", true)?;
        let bytes_obj = obj.call_method("to_bytes", (32, "big"), Some(&kwargs))?;
        let bytes: &[u8] = bytes_obj.extract()?;
        if bytes.len() != 32 {
            return Err(PyErr::new::<PyValueError, _>(
                "to_bytes returned unexpected length",
            ));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(bytes);
        Ok(I256::from_be_bytes::<32>(arr))
    } else {
        Err(PyErr::new::<PyValueError, _>("Expected int"))
    }
}

// extract_round_up not needed — we use Option<bool> directly in the signature

/// Extract a Python int as i128 (for liquidity values).
fn extract_i128(obj: &Bound<'_, PyAny>) -> PyResult<i128> {
    let i256 = extract_i256(obj)?;
    let bytes = i256.to_be_bytes::<32>();
    Ok(i128::from_be_bytes(
        bytes[16..32].try_into().unwrap_or([0u8; 16]),
    ))
}
// ─── BitMath ───────────────────────────────────────────────────────────

/// Find the index of the most significant bit set in `x`.
///
/// # Errors
///
/// Returns `PyValueError` if `x` is zero.
#[pyfunction(signature = (x))]
pub fn most_significant_bit(x: &Bound<'_, PyAny>) -> PyResult<u8> {
    let v = extract_u256(x)?;
    bit_math::most_significant_bit(v).map_err(|e| PyValueError::new_err(e.to_string()))
}

/// Find the index of the least significant bit set in `x`.
///
/// # Errors
///
/// Returns `PyValueError` if `x` is zero.
#[pyfunction(signature = (x))]
pub fn least_significant_bit(x: &Bound<'_, PyAny>) -> PyResult<u8> {
    let v = extract_u256(x)?;
    bit_math::least_significant_bit(v).map_err(|e| PyValueError::new_err(e.to_string()))
}

// ─── FullMath ──────────────────────────────────────────────────────────

/// Compute `floor(a * b / denominator)` with full 512-bit precision.
///
/// # Errors
///
/// Returns `PyValueError` on division by zero or overflow.
#[pyfunction(signature = (a, b, denominator))]
pub fn muldiv(
    a: &Bound<'_, PyAny>,
    b: &Bound<'_, PyAny>,
    denominator: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = a.py();
    let result = full_math::muldiv(
        extract_u256(a)?,
        extract_u256(b)?,
        extract_u256(denominator)?,
    )?;
    u256_to_py_obj(py, result)
}

/// Compute `ceil(a * b / denominator)` with full 512-bit precision.
///
/// # Errors
///
/// Returns `PyValueError` on division by zero or overflow.
#[pyfunction(signature = (a, b, denominator))]
pub fn muldiv_rounding_up(
    a: &Bound<'_, PyAny>,
    b: &Bound<'_, PyAny>,
    denominator: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = a.py();
    let result = full_math::muldiv_rounding_up(
        extract_u256(a)?,
        extract_u256(b)?,
        extract_u256(denominator)?,
    )?;
    u256_to_py_obj(py, result)
}

// ─── UnsafeMath ────────────────────────────────────────────────────────

/// Compute `ceil(x / y)` without overflow checking.
///
/// # Errors
///
/// Returns `PyValueError` if `y` is zero.
#[pyfunction(signature = (x, y))]
pub fn div_rounding_up(x: &Bound<'_, PyAny>, y: &Bound<'_, PyAny>) -> PyResult<PyObject> {
    let py = x.py();
    let result = unsafe_math::div_rounding_up(extract_u256(x)?, extract_u256(y)?);
    u256_to_py_obj(py, result)
}

/// Compute `(a * b) / denominator` without overflow checking.
///
/// # Errors
///
/// Returns `PyValueError` if `denominator` is zero.
#[pyfunction(signature = (a, b, denominator))]
pub fn simple_mul_div(
    a: &Bound<'_, PyAny>,
    b: &Bound<'_, PyAny>,
    denominator: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = a.py();
    let result = unsafe_math::simple_mul_div(
        extract_u256(a)?,
        extract_u256(b)?,
        extract_u256(denominator)?,
    );
    u256_to_py_obj(py, result)
}

// ─── LiquidityMath ─────────────────────────────────────────────────────

/// Add a signed delta `y` to `x`, checking that the result fits in `uint128`.
///
/// # Errors
///
/// Returns `PyValueError` if the result overflows or inputs are out of range.
#[pyfunction(signature = (x, y))]
pub fn add_delta(x: &Bound<'_, PyAny>, y: &Bound<'_, PyAny>) -> PyResult<PyObject> {
    let py = x.py();
    // Extract x as u128 via U256 — validate range first
    let x_u256 = extract_u256(x)?;
    let x_u128: u128 = match x_u256.try_into() {
        Ok(v) => v,
        Err(_) => return Err(PyValueError::new_err("x exceeds u128")),
    };
    // Extract y as i128 via I256 — validate range first
    let y_i256 = extract_i256(y)?;
    let y_bytes = y_i256.to_be_bytes::<32>();
    let y_i128 = i128::from_be_bytes(y_bytes[16..32].try_into().unwrap_or([0u8; 16]));
    let result = liquidity_math::add_delta(x_u128, y_i128)
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(pyo3::types::PyInt::new(py, result).into_any().unbind())
}

// ─── SqrtPriceMath ─────────────────────────────────────────────────────

/// Get the amount0 delta between two prices for a given liquidity.
///
/// # Errors
///
/// Returns `PyValueError` on invalid input (zero price, overflow, etc.).
#[pyfunction(signature = (sqrt_price_a, sqrt_price_b, liquidity, round_up=None))]
pub fn get_amount0_delta(
    sqrt_price_a: &Bound<'_, PyAny>,
    sqrt_price_b: &Bound<'_, PyAny>,
    liquidity: &Bound<'_, PyAny>,
    round_up: Option<bool>,
) -> PyResult<PyObject> {
    let py = sqrt_price_a.py();
    let result = sqrt_price_math::get_amount0_delta(
        extract_u256(sqrt_price_a)?,
        extract_u256(sqrt_price_b)?,
        extract_i128(liquidity)?,
        round_up,
    )?;
    u256_to_py_obj(py, result)
}

/// Get the amount1 delta between two prices for a given liquidity.
///
/// # Errors
///
/// Returns `PyValueError` on invalid input (negative liquidity, overflow, etc.).
#[pyfunction(signature = (sqrt_price_a, sqrt_price_b, liquidity, round_up=None))]
pub fn get_amount1_delta(
    sqrt_price_a: &Bound<'_, PyAny>,
    sqrt_price_b: &Bound<'_, PyAny>,
    liquidity: &Bound<'_, PyAny>,
    round_up: Option<bool>,
) -> PyResult<PyObject> {
    let py = sqrt_price_a.py();
    let result = sqrt_price_math::get_amount1_delta(
        extract_u256(sqrt_price_a)?,
        extract_u256(sqrt_price_b)?,
        extract_i128(liquidity)?,
        round_up,
    )?;
    u256_to_py_obj(py, result)
}

/// Get the next sqrt price given a delta of token0, rounding up.
///
/// # Errors
///
/// Returns `PyValueError` on overflow or insufficient liquidity.
#[pyfunction(signature = (sqrt_price_x96, liquidity, amount, add))]
pub fn get_next_sqrt_price_from_amount0_rounding_up(
    sqrt_price_x96: &Bound<'_, PyAny>,
    liquidity: &Bound<'_, PyAny>,
    amount: &Bound<'_, PyAny>,
    add: bool,
) -> PyResult<PyObject> {
    let py = sqrt_price_x96.py();
    let result = sqrt_price_math::get_next_sqrt_price_from_amount0_rounding_up(
        extract_u256(sqrt_price_x96)?,
        extract_i128(liquidity)?,
        extract_u256(amount)?,
        add,
    )?;
    u256_to_py_obj(py, result)
}

/// Get the next sqrt price given a delta of token1, rounding down.
///
/// # Errors
///
/// Returns `PyValueError` on overflow or insufficient liquidity.
#[pyfunction(signature = (sqrt_price_x96, liquidity, amount, add))]
pub fn get_next_sqrt_price_from_amount1_rounding_down(
    sqrt_price_x96: &Bound<'_, PyAny>,
    liquidity: &Bound<'_, PyAny>,
    amount: &Bound<'_, PyAny>,
    add: bool,
) -> PyResult<PyObject> {
    let py = sqrt_price_x96.py();
    let result = sqrt_price_math::get_next_sqrt_price_from_amount1_rounding_down(
        extract_u256(sqrt_price_x96)?,
        extract_i128(liquidity)?,
        extract_u256(amount)?,
        add,
    )?;
    u256_to_py_obj(py, result)
}

/// Get the next sqrt price given an input amount.
///
/// # Errors
///
/// Returns `PyValueError` on invalid price/liquidity or overflow.
#[pyfunction(signature = (sqrt_price_x96, liquidity, amount_in, zero_for_one))]
pub fn get_next_sqrt_price_from_input(
    sqrt_price_x96: &Bound<'_, PyAny>,
    liquidity: &Bound<'_, PyAny>,
    amount_in: &Bound<'_, PyAny>,
    zero_for_one: bool,
) -> PyResult<PyObject> {
    let py = sqrt_price_x96.py();
    let result = sqrt_price_math::get_next_sqrt_price_from_input(
        extract_u256(sqrt_price_x96)?,
        extract_i128(liquidity)?,
        extract_u256(amount_in)?,
        zero_for_one,
    )?;
    u256_to_py_obj(py, result)
}

/// Get the next sqrt price given an output amount.
///
/// # Errors
///
/// Returns `PyValueError` on invalid price/liquidity or overflow.
#[pyfunction(signature = (sqrt_price_x96, liquidity, amount_out, zero_for_one))]
pub fn get_next_sqrt_price_from_output(
    sqrt_price_x96: &Bound<'_, PyAny>,
    liquidity: &Bound<'_, PyAny>,
    amount_out: &Bound<'_, PyAny>,
    zero_for_one: bool,
) -> PyResult<PyObject> {
    let py = sqrt_price_x96.py();
    let result = sqrt_price_math::get_next_sqrt_price_from_output(
        extract_u256(sqrt_price_x96)?,
        extract_i128(liquidity)?,
        extract_u256(amount_out)?,
        zero_for_one,
    )?;
    u256_to_py_obj(py, result)
}

// ─── SwapMath ──────────────────────────────────────────────────────────

/// Compute a V3-style swap step.
///
/// # Errors
///
/// Returns `PyValueError` on invalid input, overflow, or if liquidity exceeds int128.
#[expect(clippy::similar_names)]
#[pyfunction(signature = (sqrt_price_current, sqrt_price_target, liquidity, amount_remaining, fee_pips))]
pub fn compute_swap_step_v3(
    sqrt_price_current: &Bound<'_, PyAny>,
    sqrt_price_target: &Bound<'_, PyAny>,
    liquidity: &Bound<'_, PyAny>,
    amount_remaining: &Bound<'_, PyAny>,
    fee_pips: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = sqrt_price_current.py();
    let amount_i256 = extract_i256(amount_remaining)?;
    let liquidity_u256 = extract_u256(liquidity)?;
    let liquidity_u128: u128 = match liquidity_u256.try_into() {
        Ok(v) => v,
        Err(_) => return Err(PyValueError::new_err("liquidity exceeds u128")),
    };
    // Validate before cast to avoid using a wrapped value.
    if liquidity_u128 > i128::MAX as u128 {
        return Err(PyValueError::new_err("liquidity exceeds int128 range"));
    }
    // Safe: validated above that liquidity_u128 <= i128::MAX.
    let liquidity_i128 = liquidity_u128.cast_signed();

    let result = swap_math::compute_swap_step_v3(
        extract_u256(sqrt_price_current)?,
        extract_u256(sqrt_price_target)?,
        liquidity_i128,
        amount_i256,
        extract_u256(fee_pips)?,
    )?;

    let tuple = pyo3::types::PyTuple::new(
        py,
        [
            u256_to_py_obj(py, result.sqrt_price_next)?,
            u256_to_py_obj(py, result.amount_in)?,
            u256_to_py_obj(py, result.amount_out)?,
            u256_to_py_obj(py, result.fee_amount)?,
        ],
    )?;
    Ok(tuple.into_any().unbind())
}

/// Compute a V4-style swap step.
///
/// # Errors
///
/// Returns `PyValueError` on invalid input, overflow, or if liquidity exceeds int128.
#[expect(clippy::similar_names)]
#[pyfunction(signature = (sqrt_price_current, sqrt_price_target, liquidity, amount_remaining, fee_pips))]
pub fn compute_swap_step_v4(
    sqrt_price_current: &Bound<'_, PyAny>,
    sqrt_price_target: &Bound<'_, PyAny>,
    liquidity: &Bound<'_, PyAny>,
    amount_remaining: &Bound<'_, PyAny>,
    fee_pips: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = sqrt_price_current.py();
    let amount_i256 = extract_i256(amount_remaining)?;
    let liquidity_u256 = extract_u256(liquidity)?;
    let liquidity_u128: u128 = match liquidity_u256.try_into() {
        Ok(v) => v,
        Err(_) => return Err(PyValueError::new_err("liquidity exceeds u128")),
    };
    // V4's compute_swap_step takes i128 for liquidity, but V4 Solidity uses uint128.
    // Positive i128 values cover the full uint128 range (0..2^127-1). Values >= 2^127
    // are not expected in practice since max uint128 = 2^128-1 cannot fit in i128.
    // Validate before cast to avoid using a wrapped value.
    if liquidity_u128 > i128::MAX as u128 {
        return Err(PyValueError::new_err("liquidity exceeds int128 range"));
    }
    // Safe: validated above that liquidity_u128 <= i128::MAX.
    let liquidity_i128 = liquidity_u128.cast_signed();

    let result = swap_math::compute_swap_step_v4(
        extract_u256(sqrt_price_current)?,
        extract_u256(sqrt_price_target)?,
        liquidity_i128,
        amount_i256,
        extract_u256(fee_pips)?,
    )?;

    let tuple = pyo3::types::PyTuple::new(
        py,
        [
            u256_to_py_obj(py, result.sqrt_price_next)?,
            u256_to_py_obj(py, result.amount_in)?,
            u256_to_py_obj(py, result.amount_out)?,
            u256_to_py_obj(py, result.fee_amount)?,
        ],
    )?;
    Ok(tuple.into_any().unbind())
}

// ─── TickMath (additional helpers) ─────────────────────────────────────

/// Maximum usable tick for the given spacing (delegates to the pure-Rust core).
#[pyfunction(signature = (tick_spacing))]
#[must_use]
#[expect(clippy::missing_const_for_fn)]
pub fn max_usable_tick(tick_spacing: i32) -> i32 {
    tick_math::max_usable_tick(tick_spacing)
}

/// Minimum usable tick for the given spacing (delegates to the pure-Rust core).
#[pyfunction(signature = (tick_spacing))]
#[must_use]
#[expect(clippy::missing_const_for_fn)]
pub fn min_usable_tick(tick_spacing: i32) -> i32 {
    tick_math::min_usable_tick(tick_spacing)
}

// ─── LiquidityMapping (tick-bitmap + apply_liquidity_mapping_update) ────

/// Compute the tick word and bit position for a compressed tick.
///
/// Returns `(word, bit)` where `word` is the mapping key (`i32`) and `bit`
/// is in `0..=255` (`u8`). Mirrors `degenbot_concentrated_liquidity_math::get_tick_word_and_bit_position`.
#[pyfunction(signature = (tick, tick_spacing))]
#[must_use]
pub fn get_tick_word_and_bit_position(tick: i32, tick_spacing: i32) -> (i32, u8) {
    liquidity_mapping::get_tick_word_and_bit_position(tick, tick_spacing)
}

/// Retrieve a field from a Python mapping or pydantic model, by key or attr.
/// Accepts dicts (`obj[key]`) and attribute-bearing objects (`obj.attr`) so the
/// seam works whether the values are plain dicts or `BitmapAtWord`/`LiquidityAtTick`
/// pydantic models.
fn field<'py>(obj: &Bound<'py, PyAny>, name: &str) -> PyResult<Bound<'py, PyAny>> {
    obj.getattr(name).or_else(|_| obj.get_item(name))
}

/// Extract a Python int as `u64`, clamping large sentinels to `u64::MAX`.
///
/// `cli/pool.py` sets `initial_state_block = MAX_UINT256` to skip the in-range
/// liquidity adjustment (the `state_block > initial_state_block` guard). Since a
/// real uint256 cannot fit `u64`, sentinel values beyond `u64::MAX` are clamped
/// to `u64::MAX`, which preserves the skip behavior for any real `update_block`.
fn extract_u64_clamped(obj: &Bound<'_, PyAny>) -> PyResult<u64> {
    let big = extract_u256(obj)?;
    let max = U256::from(u64::MAX);
    if big > max {
        Ok(u64::MAX)
    } else {
        Ok(big.to::<u64>())
    }
}

/// Widen a `U128` big-endian bytes representation to a Python int via `U256`.
fn u128_to_py_obj(py: Python<'_>, v: U128) -> PyResult<PyObject> {
    let mut full = [0u8; 32];
    full[16..32].copy_from_slice(&v.to_be_bytes::<16>());
    u256_to_py_obj(py, U256::from_be_bytes::<32>(full))
}

/// Apply a liquidity-mapping update (mint/burn at `tick_lower`..`tick_upper`)
/// to the tick bitmap and tick data, returning the next state plus the updated
/// active liquidity.
///
/// Mirrors `degenbot.calculations.concentrated_liquidity.apply_liquidity_mapping_update`.
/// Inputs are dicts of `{bitmap, block}` / `{liquidity_net, liquidity_gross, block}`
/// (or pydantic `BitmapAtWord`/`LiquidityAtTick` models); the result is a plain
/// dict with `tick_bitmap`, `tick_data`, `liquidity` keys.
///
/// # Errors
///
/// Returns `PyValueError` on invalid input types, non-uint128 liquidity, or
/// non-i32 ticks.
#[expect(clippy::too_many_arguments)]
#[pyfunction]
pub fn apply_liquidity_mapping_update(
    tick_bitmap: &Bound<'_, PyAny>,
    tick_data: &Bound<'_, PyAny>,
    tick_spacing: i32,
    tick: i32,
    liquidity: &Bound<'_, PyAny>,
    initial_state_block: &Bound<'_, PyAny>,
    update_block: &Bound<'_, PyAny>,
    tick_lower: i32,
    tick_upper: i32,
    liquidity_delta: &Bound<'_, PyAny>,
) -> PyResult<PyObject> {
    let py = tick_bitmap.py();

    // Build the Rust bitmap map.
    let mut rb: std::collections::HashMap<i32, BitmapAtWord> = std::collections::HashMap::new();
    if !tick_bitmap.is_none() {
        for k in tick_bitmap.try_iter()? {
            let k = k?;
            let word: i32 = k.extract()?;
            let v = tick_bitmap.get_item(&k)?;
            rb.insert(
                word,
                BitmapAtWord {
                    bitmap: extract_u256(&field(&v, "bitmap")?)?,
                    block: extract_u64_clamped(&field(&v, "block")?)?,
                },
            );
        }
    }

    // Build the Rust tick_data map.
    let mut rd: std::collections::HashMap<i32, LiquidityAtTick> = std::collections::HashMap::new();
    if !tick_data.is_none() {
        for k in tick_data.try_iter()? {
            let k = k?;
            let t: i32 = k.extract()?;
            let v = tick_data.get_item(&k)?;
            let gross_u256 = extract_u256(&field(&v, "liquidity_gross")?)?;
            let gross_bytes = gross_u256.to_be_bytes::<32>();
            let low: [u8; 16] = gross_bytes[16..32]
                .try_into()
                .map_err(|_| PyValueError::new_err("liquidity_gross byte slice"))?;
            rd.insert(
                t,
                LiquidityAtTick {
                    liquidity_net: extract_i256(&field(&v, "liquidity_net")?)?,
                    liquidity_gross: U128::from_be_bytes(low),
                    block: extract_u64_clamped(&field(&v, "block")?)?,
                },
            );
        }
    }

    // Active liquidity: uint128.
    let liq_u256 = extract_u256(liquidity)?;
    let liq_bytes = liq_u256.to_be_bytes::<32>();
    let liq_low: [u8; 16] = liq_bytes[16..32]
        .try_into()
        .map_err(|_| PyValueError::new_err("liquidity byte slice"))?;
    let liq_u128 = U128::from_be_bytes(liq_low);

    let result = liquidity_mapping::apply_liquidity_mapping_update(
        rb,
        rd,
        tick_spacing,
        tick,
        liq_u128,
        extract_u64_clamped(initial_state_block)?,
        extract_u64_clamped(update_block)?,
        tick_lower,
        tick_upper,
        extract_i256(liquidity_delta)?,
    );

    // Convert the result back to plain Python dicts.
    let out_bitmap = pyo3::types::PyDict::new(py);
    for (word, bw) in &result.tick_bitmap {
        let inner = pyo3::types::PyDict::new(py);
        inner.set_item("bitmap", u256_to_py_obj(py, bw.bitmap)?)?;
        inner.set_item("block", bw.block)?;
        out_bitmap.set_item(*word, inner)?;
    }
    let out_data = pyo3::types::PyDict::new(py);
    for (t, lv) in &result.tick_data {
        let inner = pyo3::types::PyDict::new(py);
        inner.set_item(
            "liquidity_net",
            alloy_py::i256_to_py(py, &lv.liquidity_net)?,
        )?;
        inner.set_item("liquidity_gross", u128_to_py_obj(py, lv.liquidity_gross)?)?;
        inner.set_item("block", lv.block)?;
        out_data.set_item(*t, inner)?;
    }
    let out = pyo3::types::PyDict::new(py);
    out.set_item("tick_bitmap", out_bitmap)?;
    out.set_item("tick_data", out_data)?;
    out.set_item("liquidity", u128_to_py_obj(py, result.liquidity)?)?;
    Ok(out.into_any().unbind())
}

// ─── Register all CL math functions ────────────────────────────────────

/// Register the math functions on the concentrated-liquidity submodule.
///
/// Called by `crate::concentrated_liquidity_math::add_concentrated_liquidity_math_module` (the single entry point that
/// also registers the `tick_math.rs` entry points + boundary constants and
/// wires up `sys.modules`). This helper registers only the `lib.rs` fns
/// (`BitMath` / `FullMath` / `UnsafeMath` / `LiquidityMath` / `SqrtPriceMath` /
/// `SwapMath` / `TickMath` helpers / `LiquidityMapping`), un-prefixed.
///
/// # Errors
///
/// Returns `PyErr` if any function fails to register.
pub fn add_lib_functions(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // BitMath
    m.add_function(wrap_pyfunction!(most_significant_bit, m)?)?;
    m.add_function(wrap_pyfunction!(least_significant_bit, m)?)?;

    // FullMath
    m.add_function(wrap_pyfunction!(muldiv, m)?)?;
    m.add_function(wrap_pyfunction!(muldiv_rounding_up, m)?)?;

    // UnsafeMath
    m.add_function(wrap_pyfunction!(div_rounding_up, m)?)?;
    m.add_function(wrap_pyfunction!(simple_mul_div, m)?)?;

    // LiquidityMath
    m.add_function(wrap_pyfunction!(add_delta, m)?)?;

    // SqrtPriceMath
    m.add_function(wrap_pyfunction!(get_amount0_delta, m)?)?;
    m.add_function(wrap_pyfunction!(get_amount1_delta, m)?)?;
    m.add_function(wrap_pyfunction!(
        get_next_sqrt_price_from_amount0_rounding_up,
        m
    )?)?;
    m.add_function(wrap_pyfunction!(
        get_next_sqrt_price_from_amount1_rounding_down,
        m
    )?)?;
    m.add_function(wrap_pyfunction!(get_next_sqrt_price_from_input, m)?)?;
    m.add_function(wrap_pyfunction!(get_next_sqrt_price_from_output, m)?)?;

    // SwapMath
    m.add_function(wrap_pyfunction!(compute_swap_step_v3, m)?)?;
    m.add_function(wrap_pyfunction!(compute_swap_step_v4, m)?)?;

    // TickMath (additional helpers beyond the existing get_sqrt_ratio/tick functions)
    m.add_function(wrap_pyfunction!(max_usable_tick, m)?)?;
    m.add_function(wrap_pyfunction!(min_usable_tick, m)?)?;

    // LiquidityMapping
    m.add_function(wrap_pyfunction!(get_tick_word_and_bit_position, m)?)?;
    m.add_function(wrap_pyfunction!(apply_liquidity_mapping_update, m)?)?;

    Ok(())
}
