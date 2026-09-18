"""Balancer V2 token amount scaling and rate normalization."""

from typing import cast

from degenbot.balancer.libraries.constants import ONE
from degenbot.erc20 import Erc20Token

# To simplify Pool logic, all token balances and amounts are normalized to behave as if the token
# had 18 decimals. e.g. When comparing DAI (18 decimals) and USDC (6 decimals), 1 USDC and 1 DAI
# would both be represented as 1e18, whereas without scaling 1 USDC would be represented as 1e6.
# This allows us to not consider differences in token decimals in the internal Pool maths,
# simplifying it greatly.


def _compute_scaling_factor(token: Erc20Token) -> int:
    # Tokens that don't implement the `decimals` method are not supported.
    token_decimals = token.decimals

    # Tokens with more than 18 decimals are not supported.
    decimals_difference = 18 - token_decimals
    return cast("int", ONE * 10**decimals_difference)
