"""Swap-amount multiplier matrices for the pool swap-calculation tests.

Each multiplier is a fraction of the pool's max token reserve and determines
one swap amount as ``(multiplier * max_reserve)``. The Balancer and Uniswap
matrices are distinct value sets probing different reserve scales; they are
preserved as found, not unified.
"""

BALANCER_TOKEN_AMOUNT_MULTIPLIERS = [
    0.0000001,
    0.000001,
    0.00001,
    0.0001,
    0.001,
    0.01,
    0.1,
    0.125,
    0.25,
]

UNISWAP_TOKEN_AMOUNT_MULTIPLIERS = [
    0.000000001,
    0.00000001,
    0.0000001,
    0.000001,
    0.00001,
    0.0001,
    0.001,
    0.01,
    0.1,
]
