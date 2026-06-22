"""Shared CVXPY 2-pool problem builder for test code.

Provides a factory function that constructs a fresh cvxpy Problem for 2-pool
constant-product arbitrage, parameterized by token decimal counts. Each call
returns a new Problem — no shared mutable state between tests.
"""

from fractions import Fraction

import cvxpy
import numpy as np
from cvxpy.atoms.affine.binary_operators import multiply as cvxpy_multiply
from cvxpy.atoms.affine.bmat import bmat as cvxpy_bmat
from cvxpy.atoms.geo_mean import geo_mean


def build_2pool_cvxpy_problem(
    *,
    reserves0_a: int,
    reserves1_a: int,
    reserves0_b: int,
    reserves1_b: int,
    decimals0: int,
    decimals1: int,
    fee: Fraction,
) -> cvxpy.Problem:
    """Build a CVXPY 2-pool constant-product arbitrage problem.

    Constructs the problem with per-token compression factors so floats stay
    in a numerically stable [0, 1] range regardless of decimal asymmetry.

    Args:
        reserves0_a: Pool A reserves for token0 (raw, un-scaled)
        reserves1_a: Pool A reserves for token1 (raw, un-scaled)
        reserves0_b: Pool B reserves for token0 (raw, un-scaled)
        reserves1_b: Pool B reserves for token1 (raw, un-scaled)
        decimals0: Decimal count for token0
        decimals1: Decimal count for token1
        fee: Swap fee as a Fraction

    Returns:
        A solved cvxpy Problem. The caller can inspect problem.value,
        problem.status, and variable .value attributes.

    """
    # Per-token compression: divide each token's raw reserve by the largest
    # value of that token across both pools, then by 10**decimals
    compression_factor_0 = max(
        Fraction(reserves0_a, 10**decimals0),
        Fraction(reserves0_b, 10**decimals0),
    )
    compression_factor_1 = max(
        Fraction(reserves1_a, 10**decimals1),
        Fraction(reserves1_b, 10**decimals1),
    )

    compressed_reserves_a = (
        Fraction(reserves0_a, 10**decimals0) / compression_factor_0,
        Fraction(reserves1_a, 10**decimals1) / compression_factor_1,
    )
    compressed_reserves_b = (
        Fraction(reserves0_b, 10**decimals0) / compression_factor_0,
        Fraction(reserves1_b, 10**decimals1) / compression_factor_1,
    )

    # Parameters
    compressed_reserves_pre_swap = cvxpy.Parameter(
        name="compressed_reserves_pre_swap",
        shape=(2, 2),
        value=np.array(
            (compressed_reserves_a, compressed_reserves_b),
            dtype=np.float64,
        ),
    )

    pool_a_pre_swap_k = cvxpy.Parameter(
        name="pool_a_pre_swap_k",
        value=geo_mean(compressed_reserves_pre_swap[0]).value,
    )
    pool_b_pre_swap_k = cvxpy.Parameter(
        name="pool_b_pre_swap_k",
        value=geo_mean(compressed_reserves_pre_swap[1]).value,
    )

    # Variables
    forward_token_amount = cvxpy.Variable(name="forward_token_amount", nonneg=True)
    profit_token_in = cvxpy.Variable(name="profit_token_in", nonneg=True)
    profit_token_out = cvxpy.Variable(name="profit_token_out", nonneg=True)

    # Fee multiplier
    fee_float = float(fee)
    fee_multiplier = cvxpy_bmat((
        (1 - fee_float, 1 - fee_float),
        (1 - fee_float, 1 - fee_float),
    ))

    # Deposits and withdrawals
    # Pool A: deposit token0, withdraw token1
    # Pool B: deposit token1, withdraw token0
    deposits = cvxpy_bmat(((forward_token_amount, 0), (0, profit_token_in)))
    withdrawals = cvxpy_bmat(((0, profit_token_out), (forward_token_amount, 0)))

    fees_removed = cvxpy_multiply(fee_multiplier, deposits)

    compressed_reserves_post_swap = (
        compressed_reserves_pre_swap + deposits - withdrawals - fees_removed
    )

    pool_a_post_swap_k = geo_mean(compressed_reserves_post_swap[0])
    pool_b_post_swap_k = geo_mean(compressed_reserves_post_swap[1])

    # Objective: maximize profit (withdrawals - deposits)
    objective = cvxpy.Maximize(profit_token_out - profit_token_in)

    # Constraints
    constraints = [
        pool_a_post_swap_k >= pool_a_pre_swap_k,
        pool_b_post_swap_k >= pool_b_pre_swap_k,
        profit_token_out <= compressed_reserves_pre_swap[0, 1],
        forward_token_amount <= compressed_reserves_pre_swap[1, 0],
    ]

    problem = cvxpy.Problem(objective, constraints)
    problem.solve(solver=cvxpy.CLARABEL)
    return problem
