"""
Analytical derivatives for Uniswap V2 pools.

Provides closed-form derivatives for swap calculations to enable
gradient-based optimization. These derivatives are exact for the
constant product formula used by V2 pools.

The constant product formula is:
    x * y = k

For a swap with input amount Δx and fee f:
    (x + Δx * (1-f)) * (y - Δy) = k

Solving for output Δy:
    Δy = y * Δx * (1-f) / (x + Δx * (1-f))

The derivative d(Δy)/d(Δx):
    d(Δy)/d(Δx) = y * (1-f) * x / (x + Δx * (1-f))²

Example:
    >>> pool = UniswapV2Pool(...)
    >>> derivative = calculate_swap_derivative(
    ...     pool, token_in, amount_in=10**18
    ... )
    >>> print(f"Marginal rate: {derivative}")
"""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from degenbot.erc20.erc20 import Erc20Token
    from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
    from degenbot.uniswap.v2_types import UniswapV2PoolState


def calculate_swap_derivative(
    pool: "UniswapV2Pool",
    token_in: "Erc20Token",
    amount_in: int,
    override_state: "UniswapV2PoolState | None" = None,
) -> Fraction:
    """
    Calculate the analytical derivative of swap output with respect to input.

    For a V2 pool with constant product x*y=k, the derivative at input
    amount Δx is:

        d(output)/d(input) = y * (1-f) * x / (x + Δx * (1-f))²

    where:
        x = input token reserves
        y = output token reserves
        f = trading fee (e.g., 0.003 for 0.3%)
        Δx = input amount

    This derivative represents the instantaneous rate of exchange at the
    given input amount. It is used by gradient-based optimizers to find
    optimal arbitrage amounts more efficiently.

    Args:
        pool: The V2 liquidity pool
        token_in: The input token
        amount_in: The input amount to evaluate derivative at
        override_state: Optional state override for simulation

    Returns:
        Fraction representing the derivative (output per unit input)

    Raises:
        ValueError: If token_in is not in the pool
        ValueError: If amount_in is negative

    Example:
        >>> pool = UniswapV2Pool(...)
        >>> derivative = calculate_swap_derivative(
        ...     pool, WETH, amount_in=10**18
        ... )
        >>> # derivative ≈ 2000.0 for ETH/USDC pool
    """
    if token_in not in pool.tokens:
        msg = f"Token {token_in} not in pool {pool.address}"
        raise ValueError(msg)

    if amount_in < 0:
        msg = f"Input amount must be non-negative, got {amount_in}"
        raise ValueError(msg)

    # Get reserves
    state = pool.state if override_state is None else override_state

    if token_in == pool.token0:
        reserves_in = state.reserves_token0
        reserves_out = state.reserves_token1
        fee = pool.fee_token0
    else:
        reserves_in = state.reserves_token1
        reserves_out = state.reserves_token0
        fee = pool.fee_token1

    # Handle edge case: zero reserves
    if reserves_in == 0 or reserves_out == 0:
        return Fraction(0)

    # Compute fee multiplier
    fee_multiplier = Fraction(1) - fee

    # At zero input, derivative is spot price: (1-f) * y/x
    if amount_in == 0:
        if reserves_in == 0:
            return Fraction(0)
        return fee_multiplier * Fraction(reserves_out, reserves_in)

    # Full derivative formula:
    # d(output)/d(input) = y * (1-f) * x / (x + Δx * (1-f))²
    numerator = Fraction(reserves_out) * fee_multiplier * Fraction(reserves_in)
    denominator = Fraction(reserves_in) + Fraction(amount_in) * fee_multiplier
    denominator = denominator * denominator

    return numerator / denominator


def calculate_arbitrage_derivative_2pool(
    pool_hi: "UniswapV2Pool",
    pool_lo: "UniswapV2Pool",
    token_start: "Erc20Token",
    amount_start: int,
    pool_hi_state: "UniswapV2PoolState | None" = None,
    pool_lo_state: "UniswapV2PoolState | None" = None,
) -> Fraction:
    """
    Calculate the derivative of 2-pool arbitrage profit with respect to input.

    For a 2-pool arbitrage WETH -> Pool A -> Token -> Pool B -> WETH,
    the profit is:
        profit = output_B - input_start

    The derivative is:
        d(profit)/d(input) = d(output_B)/d(input) - 1

    This can be computed using the chain rule through both pools.

    Args:
        pool_hi: Pool where we sell the starting token (higher price)
        pool_lo: Pool where we buy the starting token (lower price)
        token_start: The starting/arbitrage token (e.g., WETH)
        amount_start: Input amount to evaluate at
        pool_hi_state: Optional state override for pool_hi
        pool_lo_state: Optional state override for pool_lo

    Returns:
        Fraction representing the derivative of profit

    Example:
        >>> derivative = calculate_arbitrage_derivative_2pool(
        ...     pool_a, pool_b, WETH, amount_in=10**18
        ... )
        >>> # derivative > 0 means profitable at margin
    """
    # Identify the intermediate token
    if token_start == pool_hi.token0:
        intermediate_token = pool_hi.token1
    else:
        intermediate_token = pool_hi.token0

    # For now, compute numerical derivative as fallback
    # In production, this would use the full chain rule
    delta = max(1, amount_start // 1000)

    # Calculate profit at amount ± delta
    def profit_at(amount):
        # Through pool_hi: token_start -> intermediate
        if token_start == pool_hi.token0:
            out = pool_hi.calculate_tokens_out_from_tokens_in(
                token_in=token_start,
                token_in_quantity=amount,
                override_state=pool_hi_state,
            )
        else:
            out = pool_hi.calculate_tokens_out_from_tokens_in(
                token_in=token_start,
                token_in_quantity=amount,
                override_state=pool_hi_state,
            )

        # Through pool_lo: intermediate -> token_start
        if intermediate_token == pool_lo.token0:
            final = pool_lo.calculate_tokens_out_from_tokens_in(
                token_in=intermediate_token,
                token_in_quantity=out,
                override_state=pool_lo_state,
            )
        else:
            final = pool_lo.calculate_tokens_out_from_tokens_in(
                token_in=intermediate_token,
                token_in_quantity=out,
                override_state=pool_lo_state,
            )

        return final - amount

    # Numerical derivative
    profit_plus = profit_at(amount_start + delta)
    profit_minus = profit_at(amount_start - delta) if amount_start > delta else profit_at(0)

    if amount_start > delta:
        return Fraction(profit_plus - profit_minus, 2 * delta)
    else:
        # Forward difference at boundary
        profit_zero = profit_at(0)
        return Fraction(profit_plus - profit_zero, delta)


def verify_derivative_numerically(
    pool: "UniswapV2Pool",
    token_in: "Erc20Token",
    amount_in: int,
    override_state: "UniswapV2PoolState | None" = None,
    epsilon: int | None = None,
) -> tuple[Fraction, Fraction, float]:
    """
    Verify analytical derivative against numerical differentiation.

    Computes both the analytical derivative and a numerical approximation
    using finite differences, returning both for comparison.

    Args:
        pool: The V2 liquidity pool
        token_in: The input token
        amount_in: The input amount to evaluate at
        override_state: Optional state override
        epsilon: Step size for numerical derivative (default: amount_in / 1000)

    Returns:
        Tuple of (analytical_derivative, numerical_derivative, percent_error)

    Example:
        >>> analytical, numerical, error = verify_derivative_numerically(
        ...     pool, WETH, 10**18
        ... )
        >>> assert error < 1.0  # Within 1%
    """
    # Compute analytical derivative
    analytical = calculate_swap_derivative(pool, token_in, amount_in, override_state)

    # Compute numerical derivative using central differences
    if epsilon is None:
        epsilon = max(1, amount_in // 1000) if amount_in > 0 else 10**15

    # Output at amount + epsilon
    amount_plus = amount_in + epsilon
    out_plus = pool.calculate_tokens_out_from_tokens_in(
        token_in=token_in,
        token_in_quantity=amount_plus,
        override_state=override_state,
    )

    # Output at amount - epsilon (or use forward difference if at boundary)
    if amount_in > epsilon:
        # Central difference
        amount_minus = amount_in - epsilon
        out_minus = pool.calculate_tokens_out_from_tokens_in(
            token_in=token_in,
            token_in_quantity=amount_minus,
            override_state=override_state,
        )
        numerical = Fraction(out_plus - out_minus, 2 * epsilon)
    else:
        # At or near zero, use forward difference from a small positive amount
        # Can't use 0 as input since calculate_tokens_out_from_tokens_in requires positive
        small_amount = max(1, epsilon // 100)
        out_small = pool.calculate_tokens_out_from_tokens_in(
            token_in=token_in,
            token_in_quantity=small_amount,
            override_state=override_state,
        )
        # Approximate derivative as (f(epsilon) - f(small)) / (epsilon - small)
        numerical = Fraction(out_plus - out_small, amount_plus - small_amount)

    # Calculate percent error
    if numerical == 0:
        error = 100.0 if analytical != 0 else 0.0
    else:
        diff = abs(float(analytical) - float(numerical))
        avg = (abs(float(analytical)) + abs(float(numerical))) / 2
        error = (diff / avg) * 100 if avg > 0 else 0.0

    return (analytical, numerical, error)


def calculate_swap_hessian(
    pool: "UniswapV2Pool",
    token_in: "Erc20Token",
    amount_in: int,
    override_state: "UniswapV2PoolState | None" = None,
) -> Fraction:
    """
    Calculate the analytical second derivative of swap output.

    For a V2 pool with constant product x*y=k, the second derivative at input
    amount Δx is:

        d²(output)/d(input)² = -2 * y * (1-f)² * x / (x + Δx * (1-f))³

    where:
        x = input token reserves
        y = output token reserves
        f = trading fee (e.g., 0.003 for 0.3%)
        Δx = input amount

    This second derivative (Hessian) is used by Newton's method for quadratic
    convergence. It represents the rate of change of the marginal rate.

    Args:
        pool: The V2 liquidity pool
        token_in: The input token
        amount_in: The input amount to evaluate Hessian at
        override_state: Optional state override for simulation

    Returns:
        Fraction representing the second derivative (always negative for V2)

    Raises:
        ValueError: If token_in is not in the pool
        ValueError: If amount_in is negative

    Example:
        >>> pool = UniswapV2Pool(...)
        >>> hessian = calculate_swap_hessian(pool, WETH, amount_in=10**18)
        >>> # hessian < 0 (concave function)
    """
    if token_in not in pool.tokens:
        msg = f"Token {token_in} not in pool {pool.address}"
        raise ValueError(msg)

    if amount_in < 0:
        msg = f"Input amount must be non-negative, got {amount_in}"
        raise ValueError(msg)

    # Get reserves
    state = pool.state if override_state is None else override_state

    if token_in == pool.token0:
        reserves_in = state.reserves_token0
        reserves_out = state.reserves_token1
        fee = pool.fee_token0
    else:
        reserves_in = state.reserves_token1
        reserves_out = state.reserves_token0
        fee = pool.fee_token1

    # Handle edge case: zero reserves
    if reserves_in == 0 or reserves_out == 0:
        return Fraction(0)

    # Compute fee multiplier
    fee_multiplier = Fraction(1) - fee

    # At zero input, use limit as amount_in -> 0
    # d²y/dx² = -2 * y * (1-f)² * x / x³ = -2 * y * (1-f)² / x²
    if amount_in == 0:
        if reserves_in == 0:
            return Fraction(0)
        numerator = -2 * Fraction(reserves_out) * fee_multiplier * fee_multiplier
        denominator = Fraction(reserves_in) * Fraction(reserves_in)
        return numerator / denominator

    # Full Hessian formula:
    # d²(output)/d(input)² = -2 * y * (1-f)² * x / (x + Δx * (1-f))³
    numerator = (
        -2 * Fraction(reserves_out) * fee_multiplier * fee_multiplier * Fraction(reserves_in)
    )
    denominator = Fraction(reserves_in) + Fraction(amount_in) * fee_multiplier
    denominator = denominator * denominator * denominator

    return numerator / denominator


def calculate_arbitrage_derivatives_2pool(
    pool_hi: "UniswapV2Pool",
    pool_lo: "UniswapV2Pool",
    token_start: "Erc20Token",
    amount_start: int,
    pool_hi_state: "UniswapV2PoolState | None" = None,
    pool_lo_state: "UniswapV2PoolState | None" = None,
) -> tuple[Fraction, Fraction]:
    """
    Calculate first and second derivatives of 2-pool arbitrage profit.

    For a 2-pool arbitrage WETH -> Pool A -> Token -> Pool B -> WETH,
    the profit is:
        profit = output_B - input_start

    The derivatives are computed using the chain rule through both pools.

    Args:
        pool_hi: Pool where we sell the starting token (higher price)
        pool_lo: Pool where we buy the starting token (lower price)
        token_start: The starting/arbitrage token (e.g., WETH)
        amount_start: Input amount to evaluate at
        pool_hi_state: Optional state override for pool_hi
        pool_lo_state: Optional state override for pool_lo

    Returns:
        Tuple of (first_derivative, second_derivative) as Fractions

    Note:
        This is an analytical approximation using the product rule.
        For exact derivatives, numerical differentiation may be preferred
        for complex arbitrage paths.

    Example:
        >>> d1, d2 = calculate_arbitrage_derivatives_2pool(
        ...     pool_a, pool_b, WETH, amount_in=10**18
        ... )
        >>> # d1 > 0 means profitable at margin
        >>> # d2 < 0 means profit function is concave
    """
    # Get intermediate amount through first pool
    if token_start == pool_hi.token0:
        intermediate_token = pool_hi.token1
        intermediate_amount = pool_hi.calculate_tokens_out_from_tokens_in(
            token_in=token_start,
            token_in_quantity=amount_start,
            override_state=pool_hi_state,
        )
    else:
        intermediate_token = pool_hi.token0
        intermediate_amount = pool_hi.calculate_tokens_out_from_tokens_in(
            token_in=token_start,
            token_in_quantity=amount_start,
            override_state=pool_hi_state,
        )

    # Get first derivatives for both pools
    d1_pool_hi = calculate_swap_derivative(pool_hi, token_start, amount_start, pool_hi_state)

    d1_pool_lo = calculate_swap_derivative(
        pool_lo, intermediate_token, intermediate_amount, pool_lo_state
    )

    # Chain rule: d(profit)/d(input) = d(pool_lo)/d(intermediate) * d(pool_hi)/d(input) - 1
    first_derivative = d1_pool_lo * d1_pool_hi - Fraction(1)

    # Second derivative using product rule
    # d²(profit)/d(input)² = d²(pool_lo)/d(intermediate)² * (d(pool_hi)/d(input))²
    #                       + d(pool_lo)/d(intermediate) * d²(pool_hi)/d(input)²
    d2_pool_hi = calculate_swap_hessian(pool_hi, token_start, amount_start, pool_hi_state)

    d2_pool_lo = calculate_swap_hessian(
        pool_lo, intermediate_token, intermediate_amount, pool_lo_state
    )

    # Product rule for second derivative
    # Let f = pool_hi output, g = pool_lo output
    # profit = g(f(x)) - x
    # d(profit)/dx = g'(f(x)) * f'(x) - 1
    # d²(profit)/dx² = g''(f(x)) * (f'(x))² + g'(f(x)) * f''(x)
    second_derivative = d2_pool_lo * d1_pool_hi * d1_pool_hi + d1_pool_lo * d2_pool_hi

    return (first_derivative, second_derivative)
