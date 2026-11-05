//! `degenbot._ffi.eip_1559` — thin `PyO3` wrappers over the pure-Rust
//! `degenbot-core::eip_1559` module.
//!
//! Exposes the EIP-1559 `next_base_fee` to the Python driver from the Rust core
//! (single source of truth): the `BotRunner` computes `base_fee_next` here (Rust
//! math) and pipes it into the Rust dispatch seam, so the driver path holds no
//! second implementation of the [EIP-1559] formula.
//!
//! [EIP-1559]: https://eips.ethereum.org/EIPS/eip-1559

use pyo3::exceptions::PyZeroDivisionError;
use pyo3::prelude::*;

/// Compute the EIP-1559 next-block base fee, mirroring the pure-Rust
/// `degenbot_core::eip_1559::next_base_fee`.
///
/// Args: `parent_base_fee`, `parent_gas_used`, `parent_gas_limit`, optional
/// `min_base_fee`, `base_fee_max_change_denominator` (default 8) and
/// `elasticity_multiplier` (default 2). U256 intermediates avoid u128 overflow.
#[pyfunction]
#[pyo3(signature = (parent_base_fee, parent_gas_used, parent_gas_limit, min_base_fee=None, base_fee_max_change_denominator=8, elasticity_multiplier=2))]
fn next_base_fee(
    parent_base_fee: u128,
    parent_gas_used: u128,
    parent_gas_limit: u128,
    min_base_fee: Option<u128>,
    base_fee_max_change_denominator: u128,
    elasticity_multiplier: u128,
) -> PyResult<u128> {
    // Only the Greater/Less branches divide by `last_gas_target`, so a zero
    // target raises ZeroDivisionError there and only there — the Equal branch
    // (e.g. gas_limit=0, gas_used=0) returns the parent fee without dividing.
    // This avoids leaking a Rust panic (`PanicException`) for the degenerate
    // non-realistic input.
    if elasticity_multiplier == 0 {
        return Err(PyZeroDivisionError::new_err("division by zero"));
    }
    let last_gas_target = parent_gas_limit / elasticity_multiplier;
    if last_gas_target == 0 && parent_gas_used != last_gas_target {
        return Err(PyZeroDivisionError::new_err("division by zero"));
    }
    Ok(degenbot_core::eip_1559::next_base_fee(
        parent_base_fee,
        parent_gas_used,
        parent_gas_limit,
        min_base_fee,
        base_fee_max_change_denominator,
        elasticity_multiplier,
    ))
}

/// The `degenbot._ffi.eip_1559` Python submodule (declarative `#[pymodule]`),
/// carrying the EIP-1559 `next_base_fee` seam. The parent module registers
/// the submodule itself and its `sys.modules` entry.
#[pymodule(submodule)]
#[pyo3(module = "degenbot._ffi")]
pub mod eip_1559 {
    #[pymodule_export]
    use super::next_base_fee;
}
