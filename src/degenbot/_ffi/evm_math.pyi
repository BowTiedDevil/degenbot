"""Stub for the dynamically-created ``degenbot._ffi.evm_math`` submodule.

Created at runtime by ``add_evm_math_module`` in the PyO3 wrapper crate
(``degenbot-python/src/evm_math/mod.rs``). Thin PyO3 wrapper over the pure-Rust
``degenbot-evm-math`` core crate — the single source of truth for EIP-1559 math,
so the Python driver does NOT re-implement ``next_base_fee``.
"""

def next_base_fee(
    parent_base_fee: int,
    parent_gas_used: int,
    parent_gas_limit: int,
    min_base_fee: int | None = None,
    base_fee_max_change_denominator: int = 8,
    elasticity_multiplier: int = 2,
) -> int:
    """Compute the EIP-1559 next-block base fee (wei).

    Args:
        parent_base_fee: The parent block's ``base_fee_per_gas`` (wei).
        parent_gas_used: The parent block's ``gas_used``.
        parent_gas_limit: The parent block's ``gas_limit``.
        min_base_fee: Optional floor applied to the result.
        base_fee_max_change_denominator: EIP-1559 denominator (default 8).
        elasticity_multiplier: EIP-1559 gas-target divisor (default 2).

    Returns:
        The computed next base fee in wei.
    """
    ...
