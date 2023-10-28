from typing import Optional


def next_base_fee(
    parent_base_fee: int,
    parent_gas_used: int,
    parent_gas_limit: int,
    min_base_fee: Optional[int] = None,
    base_fee_max_change_denominator: int = 8,  # limits the maximum base fee increase per block to 1/8 (12.5%)
    elasticity_multiplier: int = 2,
) -> int:
    """
    Calculate next base fee for an EIP-1559 compatible blockchain. The
    formula is taken from the example code in the EIP-1559 proposal (ref:
    https://eips.ethereum.org/EIPS/eip-1559).

    The default values for `base_fee_max_change_denominator` and
    `elasticity_multiplier` are taken from EIP-1559.

    Enforces `min_base_fee` if provided.
    """

    last_gas_target = parent_gas_limit // elasticity_multiplier

    if parent_gas_used == last_gas_target:
        next_base_fee = parent_base_fee
    elif parent_gas_used > last_gas_target:
        gas_used_delta = parent_gas_used - last_gas_target
        base_fee_delta = max(
            parent_base_fee
            * gas_used_delta
            // last_gas_target
            // base_fee_max_change_denominator,
            1,
        )
        next_base_fee = parent_base_fee + base_fee_delta
    else:
        gas_used_delta = last_gas_target - parent_gas_used
        base_fee_delta = (
            parent_base_fee
            * gas_used_delta
            // last_gas_target
            // base_fee_max_change_denominator
        )
        next_base_fee = parent_base_fee - base_fee_delta

    return max(min_base_fee, next_base_fee) if min_base_fee else next_base_fee
