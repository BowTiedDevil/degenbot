from collections.abc import Sequence
from typing import cast

from degenbot.balancer.libraries.constants import ONE
from degenbot.balancer.libraries.fixed_point import div_down, div_up, mul_down
from degenbot.erc20_token import Erc20Token

# To simplify Pool logic, all token balances and amounts are normalized to behave as if the token
# had 18 decimals. e.g. When comparing DAI (18 decimals) and USDC (6 decimals), 1 USDC and 1 DAI
# would both be represented as 1e18, whereas without scaling 1 USDC would be represented as 1e6.
# This allows us to not consider differences in token decimals in the internal Pool maths,
# simplifying it greatly.


def _upscale(
    amount: int,
    scaling_factor: int,
) -> int:
    # Upscale rounding wouldn't necessarily always go in the same direction: in a swap for example
    # the balance of token in should be rounded up, and that of token out rounded down. This is the
    # only place where we round in the same direction for all amounts, as the impact of this
    # rounding is expected to be minimal.

    return mul_down(amount, scaling_factor)


def _downscale_down(
    amount: int,
    scaling_factor: int,
) -> int:
    """
    Reverses the `scaling_factor` applied to `amount`, resulting in a smaller or equal value
    depending on whether it needed scaling or not. The result is rounded down.
    """

    return div_down(amount, scaling_factor)


def _downscale_up(
    amount: int,
    scaling_factor: int,
) -> int:
    """
    Reverses the `scaling_factor` applied to `amount`, resulting in a smaller or equal value
    depending on whether it needed scaling or not. The result is rounded up.
    """

    return div_up(amount, scaling_factor)


def _upscale_array(amounts: list[int], scaling_factors: Sequence[int]) -> None:
    """
    Same as `_upscale`, but for an entire array. This function does not return anything, but instead
    *mutates* the `amounts` array.
    """

    amounts = [
        mul_down(amount, factor) for amount, factor in zip(amounts, scaling_factors, strict=True)
    ]


def _downscale_down_array(amounts: list[int], scaling_factors: list[int]) -> None:
    """
    Same as `_downscale_down`, but for an entire array. This function does not return anything, but
    instead *mutates* the `amounts` array.
    """

    amounts = [
        div_down(amount, factor) for amount, factor in zip(amounts, scaling_factors, strict=True)
    ]


def _downscale_up_array(amounts: list[int], scaling_factors: list[int]) -> None:
    """
    Same as `_downscale_up`, but for an entire array. This function does not return anything, but
    instead *mutates* the `amounts` array.
    """

    amounts = [
        div_up(amount, factor) for amount, factor in zip(amounts, scaling_factors, strict=True)
    ]


def _compute_scaling_factor(token: Erc20Token) -> int:
    # Tokens that don't implement the `decimals` method are not supported.
    token_decimals = token.decimals

    # Tokens with more than 18 decimals are not supported.
    decimals_difference = 18 - token_decimals
    return cast("int", ONE * 10**decimals_difference)
