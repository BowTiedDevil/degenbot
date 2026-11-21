//! `degenbot._ffi.eip_1559` — thin `PyO3` wrappers over the pure-Rust
//! `degenbot-core::eip_1559` module.
//!
//! Exposes the EIP-1559 `next_base_fee` to the Python driver from the Rust core
//! (single source of truth): the `BotRunner` computes `base_fee_next` here (Rust
//! math) and pipes it into the Rust dispatch seam, so the driver path holds no
//! second implementation of the [EIP-1559] formula.
//!
//! [EIP-1559]: https://eips.ethereum.org/EIPS/eip-1559

use degenbot_core::eip_1559::{next_base_fee as next_base_fee_checked, BaseFeeParams};
use pyo3::prelude::*;

/// Compute the EIP-1559 next-block base fee from the pure-Rust
/// `degenbot_core::eip_1559::next_base_fee`.
///
/// Args: `parent_base_fee`, `parent_gas_used`, `parent_gas_limit`, optional
/// `min_base_fee`, `base_fee_max_change_denominator` (default 8) and
/// `elasticity_multiplier` (default 2). U256 intermediates avoid u128 overflow.
///
/// The numeric signature is this seam's job: Python has no non-zero integer
/// type, so the two divisors arrive as plain ints and the Rust core's refusals
/// leave here as `ValueError`.
///
/// # Errors
///
/// `ValueError` when either divisor is zero, or when `parent_gas_limit` is below
/// `elasticity_multiplier` while `parent_gas_used` is non-zero (a zero gas target
/// with no divisor for the delta).
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
    let params = BaseFeeParams::try_new(
        base_fee_max_change_denominator,
        elasticity_multiplier,
        min_base_fee,
    )?;
    Ok(next_base_fee_checked(
        parent_base_fee,
        parent_gas_used,
        parent_gas_limit,
        params,
    )?)
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
