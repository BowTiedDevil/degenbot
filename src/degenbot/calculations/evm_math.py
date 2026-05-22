"""EVM arithmetic, gas math, and value validation."""

from degenbot.constants import MAX_UINT256, MIN_UINT256
from degenbot.exceptions.pool import InvalidUint256


def evm_divide(numerator: int, denominator: int) -> int:
    """Perform integer division, rounding towards zero to match the EVM behavior."""
    return -(-numerator // denominator) if numerator < 0 else numerator // denominator


def next_base_fee(
    *,
    parent_base_fee: int,
    parent_gas_used: int,
    parent_gas_limit: int,
    min_base_fee: int | None = None,
    base_fee_max_change_denominator: int = 8,
    elasticity_multiplier: int = 2,
) -> int:
    """Calculate next base fee for an EIP-1559 compatible blockchain. The.

    formula is taken from the example code in the EIP-1559 proposal (ref:
    https://eips.ethereum.org/EIPS/eip-1559).

    The default values for `base_fee_max_change_denominator` and
    `elasticity_multiplier` are taken from EIP-1559.

    Enforces `min_base_fee` if provided.
    """
    last_gas_target = parent_gas_limit // elasticity_multiplier

    if parent_gas_used == last_gas_target:
        working_base_fee = parent_base_fee
    elif parent_gas_used > last_gas_target:
        gas_used_delta = parent_gas_used - last_gas_target
        base_fee_delta = max(
            parent_base_fee * gas_used_delta // last_gas_target // base_fee_max_change_denominator,
            1,
        )
        working_base_fee = parent_base_fee + base_fee_delta
    else:
        gas_used_delta = last_gas_target - parent_gas_used
        base_fee_delta = (
            parent_base_fee * gas_used_delta // last_gas_target // base_fee_max_change_denominator
        )
        working_base_fee = parent_base_fee - base_fee_delta

    return max(min_base_fee, working_base_fee) if min_base_fee else working_base_fee


def raise_if_invalid_uint256(number: int) -> None:
    """Perform raise if invalid uint256."""
    if (MIN_UINT256 <= number <= MAX_UINT256) is False:
        raise InvalidUint256
