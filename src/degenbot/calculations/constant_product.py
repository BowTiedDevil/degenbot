"""Constant product (x*y=k) invariant calculations.

Pure functions implementing the Uniswap V2 constant-product invariant.
These are the mathematical core; fee application and swap encoding
remain in the pool class and builder.

All functions are pure: numeric inputs → numeric outputs, no self, no class references.
"""

from fractions import Fraction


def get_amount_out(
    amount_in: int,
    reserves_in: int,
    reserves_out: int,
    fee: Fraction,
) -> int:
    """Calculate amount out for an exact input in a constant-product pool.

    Formula: amount_out = (amount_in_after_fee * reserves_out) / (reserves_in + amount_in_after_fee)
    where amount_in_after_fee = amount_in * (1 - fee)
    """
    amount_in_after_fee = amount_in - amount_in * fee.numerator // fee.denominator
    return (amount_in_after_fee * reserves_out) // (reserves_in + amount_in_after_fee)
