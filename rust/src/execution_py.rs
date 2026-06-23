//! `PyO3` bindings for executor calldata construction.

use crate::alloy_py::extract_python_u256;
use crate::execution::{
    encode_compose_four_leg_calldata, encode_match_internal_calldata, encode_native_arb_calldata,
    ComposeParams, MatchParams, NativeArbParams, SwapStep,
};
use crate::simulation::curve::CurveSnapshot;
use crate::simulation::curve_optimize::optimal_input_2pool_curve;
use crate::simulation::uniswap_v3_math::v3_mid_price_x96;
use crate::simulation::v2_optimize::{
    apply_gap_to_price_x96, optimal_input_2pool, optimal_v2_frontrun_amount,
    synthetic_victim_amount_in, v2_mid_price_x96, v2_optimal_sandwich_size, v2_sandwich_max_size,
};
use crate::simulation::v3::V3Snapshot;
use crate::simulation::v3_optimize::{
    optimal_input_2pool_v3, v3_optimal_sandwich_size, v3_sandwich_max_size,
};
use alloy::hex;
use alloy::primitives::{Address, Bytes, U256};
use pyo3::exceptions::{PyKeyError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
use std::str::FromStr;

/// Encode calldata for `executeNativeArb`.
#[pyfunction]
#[pyo3(name = "encode_native_arb_calldata")]
#[allow(clippy::too_many_arguments)]
pub fn encode_native_arb_calldata_py(
    py: Python<'_>,
    flash_lender: &str,
    flash_protocol: &str,
    flash_token: &str,
    flash_amount: &Bound<'_, PyAny>,
    swaps: &Bound<'_, PyList>,
    min_profit: &Bound<'_, PyAny>,
    deadline: &Bound<'_, PyAny>,
) -> PyResult<Py<PyAny>> {
    let params = NativeArbParams {
        flash_lender: Address::from_str(flash_lender)
            .map_err(|e| PyValueError::new_err(e.to_string()))?,
        flash_protocol: flash_protocol.parse().map_err(PyValueError::new_err)?,
        flash_token: Address::from_str(flash_token)
            .map_err(|e| PyValueError::new_err(e.to_string()))?,
        flash_amount: extract_python_u256(flash_amount)?,
        swaps: parse_swap_steps(swaps)?,
        min_profit: extract_python_u256(min_profit)?,
        deadline: extract_python_u256(deadline)?,
    };

    let calldata = py
        .detach(|| encode_native_arb_calldata(&params))
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

    Ok(PyBytes::new(py, calldata.as_ref()).into_any().unbind())
}

/// Encode calldata for `matchInternal`.
#[pyfunction]
#[pyo3(name = "encode_match_internal_calldata")]
#[allow(clippy::too_many_arguments)]
pub fn encode_match_internal_calldata_py(
    py: Python<'_>,
    cow_settlement_calldata: &Bound<'_, PyAny>,
    uniswapx_batch_calldata: &Bound<'_, PyAny>,
    expected_token_inflows: &Bound<'_, PyList>,
    expected_token_inflow_min: &Bound<'_, PyList>,
    flash_lender: &Bound<'_, PyAny>,
    flash_protocol: &str,
    flash_token: &Bound<'_, PyAny>,
    flash_amount: &Bound<'_, PyAny>,
    min_profit: &Bound<'_, PyAny>,
    deadline: &Bound<'_, PyAny>,
    settlement_funding_token: &Bound<'_, PyAny>,
    settlement_funding_max: &Bound<'_, PyAny>,
) -> PyResult<Py<PyAny>> {
    let params = MatchParams {
        cow_settlement_calldata: extract_bytes(cow_settlement_calldata)?.into(),
        uniswapx_batch_calldata: extract_bytes(uniswapx_batch_calldata)?.into(),
        expected_token_inflows: parse_address_list(expected_token_inflows)?,
        expected_token_inflow_min: parse_u256_list(expected_token_inflow_min)?,
        flash_lender: extract_address(flash_lender)?,
        flash_protocol: flash_protocol.parse().map_err(PyValueError::new_err)?,
        flash_token: extract_address(flash_token)?,
        flash_amount: extract_python_u256(flash_amount)?,
        min_profit: extract_python_u256(min_profit)?,
        deadline: extract_python_u256(deadline)?,
        settlement_funding_token: extract_address(settlement_funding_token)?,
        settlement_funding_max: extract_python_u256(settlement_funding_max)?,
    };

    let calldata = py
        .detach(|| encode_match_internal_calldata(&params))
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

    Ok(PyBytes::new(py, calldata.as_ref()).into_any().unbind())
}

/// Encode calldata for `composeFourLeg`.
#[pyfunction]
#[pyo3(name = "encode_compose_four_leg_calldata")]
#[allow(clippy::too_many_arguments)]
pub fn encode_compose_four_leg_calldata_py(
    py: Python<'_>,
    across_fill_calldata: &Bound<'_, PyAny>,
    arb_swaps: &Bound<'_, PyList>,
    cow_fill_calldata: &Bound<'_, PyAny>,
    uniswapx_rebalance_calldata: &Bound<'_, PyAny>,
    flash_lender: &Bound<'_, PyAny>,
    flash_protocol: &str,
    flash_token: &Bound<'_, PyAny>,
    flash_amount: &Bound<'_, PyAny>,
    min_profit: &Bound<'_, PyAny>,
    deadline: &Bound<'_, PyAny>,
    settlement_funding_token: &Bound<'_, PyAny>,
    settlement_funding_max: &Bound<'_, PyAny>,
) -> PyResult<Py<PyAny>> {
    let params = ComposeParams {
        across_fill_calldata: extract_bytes(across_fill_calldata)?.into(),
        arb_swaps: parse_swap_steps(arb_swaps)?,
        cow_fill_calldata: extract_bytes(cow_fill_calldata)?.into(),
        uniswapx_rebalance_calldata: extract_bytes(uniswapx_rebalance_calldata)?.into(),
        flash_lender: extract_address(flash_lender)?,
        flash_protocol: flash_protocol.parse().map_err(PyValueError::new_err)?,
        flash_token: extract_address(flash_token)?,
        flash_amount: extract_python_u256(flash_amount)?,
        min_profit: extract_python_u256(min_profit)?,
        deadline: extract_python_u256(deadline)?,
        settlement_funding_token: extract_address(settlement_funding_token)?,
        settlement_funding_max: extract_python_u256(settlement_funding_max)?,
    };

    let calldata = py
        .detach(|| encode_compose_four_leg_calldata(&params))
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

    Ok(PyBytes::new(py, calldata.as_ref()).into_any().unbind())
}

/// Calculate the mid-price in Q64.96 for a V2 pool.
#[pyfunction]
#[pyo3(name = "v2_mid_price_x96")]
pub fn v2_mid_price_x96_py(
    reserve_in: &Bound<'_, PyAny>,
    reserve_out: &Bound<'_, PyAny>,
) -> PyResult<String> {
    let result = v2_mid_price_x96(
        extract_python_u256(reserve_in)?,
        extract_python_u256(reserve_out)?,
    );
    Ok(result.to_string())
}

/// Calculate the mid-price in Q64.96 for a V3 pool.
#[pyfunction]
#[pyo3(name = "v3_mid_price_x96")]
pub fn v3_mid_price_x96_py(sqrt_price_x96: &Bound<'_, PyAny>) -> PyResult<String> {
    let result = v3_mid_price_x96(extract_python_u256(sqrt_price_x96)?);
    Ok(result.to_string())
}

/// Apply the gap to a price.
#[pyfunction]
#[pyo3(name = "apply_gap_to_price_x96")]
pub fn apply_gap_to_price_x96_py(price_x96: &Bound<'_, PyAny>, gap_bps: i32) -> PyResult<String> {
    let result = apply_gap_to_price_x96(extract_python_u256(price_x96)?, gap_bps);
    Ok(result.to_string())
}

/// Synthetic victim swap size.
#[pyfunction]
#[pyo3(name = "synthetic_victim_amount_in")]
pub fn synthetic_victim_amount_in_py(
    gap_bps: i32,
    reserve_in: &Bound<'_, PyAny>,
) -> PyResult<String> {
    let result = synthetic_victim_amount_in(gap_bps, extract_python_u256(reserve_in)?);
    Ok(result.to_string())
}

/// Calculate the optimal frontrun amount for a sandwich attack.
#[pyfunction]
#[pyo3(name = "optimal_v2_frontrun_amount")]
pub fn optimal_v2_frontrun_amount_py(
    victim_amount_in: &Bound<'_, PyAny>,
    victim_min_out: &Bound<'_, PyAny>,
    reserve_in: &Bound<'_, PyAny>,
    reserve_out: &Bound<'_, PyAny>,
    fee_bps: u32,
    margin_bps: u32,
) -> PyResult<String> {
    let result = optimal_v2_frontrun_amount(
        extract_python_u256(victim_amount_in)?,
        extract_python_u256(victim_min_out)?,
        extract_python_u256(reserve_in)?,
        extract_python_u256(reserve_out)?,
        fee_bps,
        margin_bps,
    )
    .map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(result.to_string())
}

/// Largest frontrun size `a` such that the victim's post-distortion
/// fill is exactly `victim_min_out`.
#[pyfunction]
#[pyo3(name = "v2_sandwich_max_size")]
pub fn v2_sandwich_max_size_py(
    victim_amount_in: &Bound<'_, PyAny>,
    victim_min_out: &Bound<'_, PyAny>,
    reserve_in: &Bound<'_, PyAny>,
    reserve_out: &Bound<'_, PyAny>,
    fee_bps: u32,
) -> PyResult<String> {
    let result = v2_sandwich_max_size(
        extract_python_u256(victim_amount_in)?,
        extract_python_u256(victim_min_out)?,
        extract_python_u256(reserve_in)?,
        extract_python_u256(reserve_out)?,
        fee_bps,
    )
    .map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(result.to_string())
}

/// Find the unconstrained optimal frontrun size using golden-section search.
#[pyfunction]
#[pyo3(name = "v2_optimal_sandwich_size")]
pub fn v2_optimal_sandwich_size_py(
    victim_amount_in: &Bound<'_, PyAny>,
    reserve_in: &Bound<'_, PyAny>,
    reserve_out: &Bound<'_, PyAny>,
    fee_bps: u32,
    a_max: &Bound<'_, PyAny>,
) -> PyResult<String> {
    let result = v2_optimal_sandwich_size(
        extract_python_u256(victim_amount_in)?,
        extract_python_u256(reserve_in)?,
        extract_python_u256(reserve_out)?,
        fee_bps,
        extract_python_u256(a_max)?,
    )
    .map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(result.to_string())
}

/// Largest frontrun size `a` for a V3 sandwich.
#[pyfunction]
#[pyo3(name = "v3_sandwich_max_size")]
pub fn v3_sandwich_max_size_py(
    pool_json: String,
    victim_amount_in: &Bound<'_, PyAny>,
    victim_min_out: &Bound<'_, PyAny>,
    zero_for_one: bool,
) -> PyResult<String> {
    let pool: V3Snapshot = serde_json::from_str(&pool_json)
        .map_err(|e| PyValueError::new_err(format!("invalid pool json: {e}")))?;

    let result = v3_sandwich_max_size(
        &pool,
        extract_python_u256(victim_amount_in)?,
        extract_python_u256(victim_min_out)?,
        zero_for_one,
    )
    .map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(result.to_string())
}

/// Find the optimal frontrun size for a V3 sandwich.
#[pyfunction]
#[pyo3(name = "v3_optimal_sandwich_size")]
pub fn v3_optimal_sandwich_size_py(
    pool_json: String,
    victim_amount_in: &Bound<'_, PyAny>,
    zero_for_one: bool,
    a_max: &Bound<'_, PyAny>,
) -> PyResult<String> {
    let pool: V3Snapshot = serde_json::from_str(&pool_json)
        .map_err(|e| PyValueError::new_err(format!("invalid pool json: {e}")))?;

    let result = v3_optimal_sandwich_size(
        &pool,
        extract_python_u256(victim_amount_in)?,
        zero_for_one,
        extract_python_u256(a_max)?,
    )
    .map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(result.to_string())
}

/// Calculate the optimal input amount for a 2-pool Uniswap V2 arbitrage cycle.
#[pyfunction]
#[pyo3(name = "optimal_input_2pool")]
pub fn optimal_input_2pool_py(
    r_a1: &Bound<'_, PyAny>,
    r_b1: &Bound<'_, PyAny>,
    fee_bps1: u32,
    r_b2: &Bound<'_, PyAny>,
    r_a2: &Bound<'_, PyAny>,
    fee_bps2: u32,
) -> PyResult<String> {
    let result = optimal_input_2pool(
        extract_python_u256(r_a1)?,
        extract_python_u256(r_b1)?,
        fee_bps1,
        extract_python_u256(r_b2)?,
        extract_python_u256(r_a2)?,
        fee_bps2,
    )
    .map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(result.to_string())
}

/// Calculate the optimal input amount for a 2-pool V3 arbitrage cycle.
#[pyfunction]
#[pyo3(name = "optimal_input_2pool_v3")]
pub fn optimal_input_2pool_v3_py(
    pool1_v3_json: String,
    pool1_zero_for_one: bool,
    pool2_v3_json: Option<String>,
    pool2_v2_data: Option<(String, String, u32)>,
) -> PyResult<String> {
    let p1: V3Snapshot = serde_json::from_str(&pool1_v3_json)
        .map_err(|e| PyValueError::new_err(format!("invalid p1 json: {e}")))?;

    let p2v3: Option<V3Snapshot> = if let Some(json) = pool2_v3_json {
        Some(
            serde_json::from_str(&json)
                .map_err(|e| PyValueError::new_err(format!("invalid p2 json: {e}")))?,
        )
    } else {
        None
    };

    let p2v2: Option<(U256, U256, u32)> = if let Some(data) = pool2_v2_data {
        Some((
            U256::from_str_radix(&data.0, 10).map_err(|e| PyValueError::new_err(e.to_string()))?,
            U256::from_str_radix(&data.1, 10).map_err(|e| PyValueError::new_err(e.to_string()))?,
            data.2,
        ))
    } else {
        None
    };

    let result = optimal_input_2pool_v3(&p1, pool1_zero_for_one, p2v3.as_ref(), p2v2)
        .map_err(|e| PyValueError::new_err(e.to_string()))?;

    Ok(result.to_string())
}

/// Calculate the optimal input amount for a 2-pool Curve arbitrage cycle.
#[pyfunction]
#[pyo3(name = "optimal_input_2pool_curve")]
pub fn optimal_input_2pool_curve_py(
    pool_curve_json: String,
    i: usize,
    j: usize,
    pool2_v2_data: Option<(String, String, u32)>,
) -> PyResult<String> {
    let p_curve: CurveSnapshot = serde_json::from_str(&pool_curve_json)
        .map_err(|e| PyValueError::new_err(format!("invalid curve json: {e}")))?;

    let p2v2: Option<(U256, U256, u32)> = if let Some(data) = pool2_v2_data {
        Some((
            U256::from_str_radix(&data.0, 10).map_err(|e| PyValueError::new_err(e.to_string()))?,
            U256::from_str_radix(&data.1, 10).map_err(|e| PyValueError::new_err(e.to_string()))?,
            data.2,
        ))
    } else {
        None
    };

    let result = optimal_input_2pool_curve(&p_curve, i, j, p2v2)
        .map_err(|e| PyValueError::new_err(e.to_string()))?;

    Ok(result.to_string())
}

/// Add executor module to Python module.
pub fn add_execution_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(encode_native_arb_calldata_py, m)?)?;
    m.add_function(wrap_pyfunction!(encode_match_internal_calldata_py, m)?)?;
    m.add_function(wrap_pyfunction!(encode_compose_four_leg_calldata_py, m)?)?;
    m.add_function(wrap_pyfunction!(optimal_input_2pool_py, m)?)?;
    m.add_function(wrap_pyfunction!(optimal_input_2pool_v3_py, m)?)?;
    m.add_function(wrap_pyfunction!(optimal_input_2pool_curve_py, m)?)?;
    m.add_function(wrap_pyfunction!(optimal_v2_frontrun_amount_py, m)?)?;
    m.add_function(wrap_pyfunction!(v2_mid_price_x96_py, m)?)?;
    m.add_function(wrap_pyfunction!(v3_mid_price_x96_py, m)?)?;
    m.add_function(wrap_pyfunction!(apply_gap_to_price_x96_py, m)?)?;
    m.add_function(wrap_pyfunction!(synthetic_victim_amount_in_py, m)?)?;
    m.add_function(wrap_pyfunction!(v2_sandwich_max_size_py, m)?)?;
    m.add_function(wrap_pyfunction!(v2_optimal_sandwich_size_py, m)?)?;
    m.add_function(wrap_pyfunction!(v3_sandwich_max_size_py, m)?)?;
    m.add_function(wrap_pyfunction!(v3_optimal_sandwich_size_py, m)?)?;
    Ok(())
}

fn parse_swap_steps(list: &Bound<'_, PyList>) -> PyResult<Vec<SwapStep>> {
    list.iter().map(|item| parse_swap_step(&item)).collect()
}

fn parse_swap_step(item: &Bound<'_, PyAny>) -> PyResult<SwapStep> {
    let dict = item.cast::<PyDict>()?;
    let dex_kind = required_item(dict, "dex_kind")?.extract::<String>()?;
    let router = Address::from_str(&required_item(dict, "router")?.extract::<String>()?)
        .map_err(|e| PyValueError::new_err(format!("invalid router address: {} - {}", e, item)))?;
    let call_data = Bytes::from(extract_bytes(&required_item(dict, "call_data")?)?);
    let token_in = Address::from_str(&required_item(dict, "token_in")?.extract::<String>()?)
        .map_err(|e| PyValueError::new_err(format!("invalid token_in address: {}", e)))?;
    let token_out = Address::from_str(&required_item(dict, "token_out")?.extract::<String>()?)
        .map_err(|e| PyValueError::new_err(format!("invalid token_out address: {}", e)))?;
    let amount_in = extract_python_u256(&required_item(dict, "amount_in")?)?;
    let amount_out_min = extract_python_u256(&required_item(dict, "amount_out_min")?)?;

    Ok(SwapStep {
        dex_kind: dex_kind.parse().map_err(PyValueError::new_err)?,
        router,
        call_data,
        token_in,
        token_out,
        amount_in,
        amount_out_min,
    })
}

fn parse_address_list(list: &Bound<'_, PyList>) -> PyResult<Vec<Address>> {
    list.iter()
        .map(|item| {
            let s = item.extract::<String>()?;
            Address::from_str(&s).map_err(|e| PyValueError::new_err(e.to_string()))
        })
        .collect()
}

fn parse_u256_list(list: &Bound<'_, PyList>) -> PyResult<Vec<U256>> {
    list.iter().map(|item| extract_python_u256(&item)).collect()
}

fn extract_address(item: &Bound<'_, PyAny>) -> PyResult<Address> {
    let s = item.extract::<String>()?;
    Address::from_str(&s).map_err(|e| PyValueError::new_err(e.to_string()))
}

fn extract_bytes(item: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
    if let Ok(b) = item.cast::<PyBytes>() {
        Ok(b.as_bytes().to_vec())
    } else if let Ok(s) = item.extract::<String>() {
        let s = s.strip_prefix("0x").or(s.strip_prefix("0X")).unwrap_or(&s);
        hex::decode(s).map_err(|e| PyValueError::new_err(e.to_string()))
    } else {
        Err(PyValueError::new_err("Value must be an integer or bytes"))
    }
}

fn required_item<'py>(dict: &Bound<'py, PyDict>, key: &str) -> PyResult<Bound<'py, PyAny>> {
    dict.get_item(key)?
        .ok_or_else(|| PyKeyError::new_err(key.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::FlashProtocol;
    use crate::with_python_for_tests as with_python;
    use pyo3::types::{PyInt, PyString};

    fn sample_swap_step() -> SwapStep {
        SwapStep {
            dex_kind: crate::execution::DexKind::UniV3,
            router: Address::repeat_byte(0x11),
            call_data: Bytes::from_static(b"data"),
            token_in: Address::repeat_byte(0x22),
            token_out: Address::repeat_byte(0x33),
            amount_in: U256::from(100u64),
            amount_out_min: U256::from(90u64),
        }
    }

    #[test]
    fn native_arb_py_binding_matches_core() {
        with_python(|py| {
            let params = NativeArbParams {
                flash_lender: Address::repeat_byte(0x44),
                flash_protocol: FlashProtocol::Aave,
                flash_token: Address::repeat_byte(0x55),
                flash_amount: U256::from(1234u64),
                swaps: vec![sample_swap_step()],
                min_profit: U256::from(42u64),
                deadline: U256::from(999_999u64),
            };

            let swaps = PyList::empty(py);
            let swap = PyDict::new(py);
            swap.set_item("dex_kind", "univ3").unwrap();
            swap.set_item("router", format!("{:#x}", params.swaps[0].router))
                .unwrap();
            swap.set_item("call_data", PyBytes::new(py, b"data"))
                .unwrap();
            swap.set_item("token_in", format!("{:#x}", params.swaps[0].token_in))
                .unwrap();
            swap.set_item("token_out", format!("{:#x}", params.swaps[0].token_out))
                .unwrap();
            swap.set_item("amount_in", 100u64).unwrap();
            swap.set_item("amount_out_min", 90u64).unwrap();
            swaps.append(swap).unwrap();

            let flash_amount = PyInt::new(py, 1234).into_any();
            let min_profit = PyInt::new(py, 42).into_any();
            let deadline = PyInt::new(py, 999_999).into_any();

            let calldata = encode_native_arb_calldata_py(
                py,
                &format!("{:#x}", params.flash_lender),
                "aave",
                &format!("{:#x}", params.flash_token),
                &flash_amount,
                &swaps,
                &min_profit,
                &deadline,
            )
            .unwrap();

            let expected = encode_native_arb_calldata(&params).unwrap();
            let actual: &[u8] = calldata.bind(py).extract().unwrap();
            assert_eq!(actual, expected.as_ref());
        });
    }

    #[test]
    fn match_internal_py_binding_matches_core() {
        with_python(|py| {
            let params = MatchParams {
                cow_settlement_calldata: Bytes::from_static(b"cow"),
                uniswapx_batch_calldata: Bytes::from_static(b"uni"),
                expected_token_inflows: vec![Address::repeat_byte(0xaa)],
                expected_token_inflow_min: vec![U256::from(1u64)],
                flash_lender: Address::repeat_byte(0x44),
                flash_protocol: FlashProtocol::Morpho,
                flash_token: Address::repeat_byte(0x55),
                flash_amount: U256::from(1234u64),
                min_profit: U256::from(42u64),
                deadline: U256::from(999_999u64),
                settlement_funding_token: Address::ZERO,
                settlement_funding_max: U256::ZERO,
            };

            let expected_token_inflows = PyList::empty(py);
            expected_token_inflows
                .append(PyString::new(
                    py,
                    &format!("{:#x}", params.expected_token_inflows[0]),
                ))
                .unwrap();

            let expected_token_inflow_min = PyList::empty(py);
            expected_token_inflow_min.append(PyInt::new(py, 1)).unwrap();

            let cow_settlement_calldata = PyBytes::new(py, b"cow").into_any();
            let uniswapx_batch_calldata = PyBytes::new(py, b"uni").into_any();
            let flash_lender = PyString::new(py, &format!("{:#x}", params.flash_lender)).into_any();
            let flash_token = PyString::new(py, &format!("{:#x}", params.flash_token)).into_any();
            let flash_amount = PyInt::new(py, 1234).into_any();
            let min_profit = PyInt::new(py, 42).into_any();
            let deadline = PyInt::new(py, 999_999).into_any();
            let settlement_funding_token =
                PyString::new(py, &format!("{:#x}", params.settlement_funding_token)).into_any();
            let settlement_funding_max = PyInt::new(py, 0).into_any();

            let calldata = encode_match_internal_calldata_py(
                py,
                &cow_settlement_calldata,
                &uniswapx_batch_calldata,
                &expected_token_inflows,
                &expected_token_inflow_min,
                &flash_lender,
                "morpho",
                &flash_token,
                &flash_amount,
                &min_profit,
                &deadline,
                &settlement_funding_token,
                &settlement_funding_max,
            )
            .unwrap();

            let expected = encode_match_internal_calldata(&params).unwrap();
            let actual: &[u8] = calldata.bind(py).extract().unwrap();
            assert_eq!(actual, expected.as_ref());
        });
    }

    #[test]
    fn compose_four_leg_py_binding_matches_core() {
        with_python(|py| {
            let params = ComposeParams {
                across_fill_calldata: Bytes::from_static(b"across"),
                arb_swaps: vec![sample_swap_step()],
                cow_fill_calldata: Bytes::from_static(b"cow"),
                uniswapx_rebalance_calldata: Bytes::from_static(b"uni"),
                flash_lender: Address::repeat_byte(0x44),
                flash_protocol: FlashProtocol::ERC3156,
                flash_token: Address::repeat_byte(0x55),
                flash_amount: U256::from(1234u64),
                min_profit: U256::from(42u64),
                deadline: U256::from(999_999u64),
                settlement_funding_token: Address::ZERO,
                settlement_funding_max: U256::ZERO,
            };

            let arb_swaps = PyList::empty(py);
            let swap = PyDict::new(py);
            swap.set_item("dex_kind", "univ3").unwrap();
            swap.set_item("router", format!("{:#x}", params.arb_swaps[0].router))
                .unwrap();
            swap.set_item("call_data", PyBytes::new(py, b"data"))
                .unwrap();
            swap.set_item("token_in", format!("{:#x}", params.arb_swaps[0].token_in))
                .unwrap();
            swap.set_item("token_out", format!("{:#x}", params.arb_swaps[0].token_out))
                .unwrap();
            swap.set_item("amount_in", 100u64).unwrap();
            swap.set_item("amount_out_min", 90u64).unwrap();
            arb_swaps.append(swap).unwrap();

            let across_fill_calldata = PyBytes::new(py, b"across").into_any();
            let cow_fill_calldata = PyBytes::new(py, b"cow").into_any();
            let uniswapx_rebalance_calldata = PyBytes::new(py, b"uni").into_any();
            let flash_lender = PyString::new(py, &format!("{:#x}", params.flash_lender)).into_any();
            let flash_token = PyString::new(py, &format!("{:#x}", params.flash_token)).into_any();
            let flash_amount = PyInt::new(py, 1234).into_any();
            let min_profit = PyInt::new(py, 42).into_any();
            let deadline = PyInt::new(py, 999_999).into_any();
            let settlement_funding_token =
                PyString::new(py, &format!("{:#x}", params.settlement_funding_token)).into_any();
            let settlement_funding_max = PyInt::new(py, 0).into_any();

            let calldata = encode_compose_four_leg_calldata_py(
                py,
                &across_fill_calldata,
                &arb_swaps,
                &cow_fill_calldata,
                &uniswapx_rebalance_calldata,
                &flash_lender,
                "erc3156",
                &flash_token,
                &flash_amount,
                &min_profit,
                &deadline,
                &settlement_funding_token,
                &settlement_funding_max,
            )
            .unwrap();

            let expected = encode_compose_four_leg_calldata(&params).unwrap();
            let actual: &[u8] = calldata.bind(py).extract().unwrap();
            assert_eq!(actual, expected.as_ref());
        });
    }
}
