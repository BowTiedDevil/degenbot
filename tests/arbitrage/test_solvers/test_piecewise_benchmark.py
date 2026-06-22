"""Benchmark tests for PiecewiseMobiusSolver performance.

Validates the ~10-20μs performance claim for multi-range V3 paths.
"""

import time
from fractions import Fraction

import pytest

from degenbot.arbitrage.solvers._v3_utils import _v3_virtual_reserves
from degenbot.arbitrage.solvers.hop_types import SolveInput
from degenbot.arbitrage.solvers.solver import PiecewiseMobiusSolver
from degenbot.types.hop_types import (
    BoundedProductHop,
    ConstantProductHop,
    V3TickRangeInfo,
)

Q96 = 2**96


def _make_profitable_v3_v2_path(
    liquidity: int,
    sqrt_price_x96: int,
    v2_rate_multiplier: float = 2.0,
) -> tuple[BoundedProductHop, ConstantProductHop]:
    """Build a profitable V3→V2 arbitrage path with consistent reserves.

    V3 reserves are computed from L/sqrt_price via _v3_virtual_reserves
    so they match the pool's actual swap function. V2 reserves are set to
    make the cycle profitable at the margin.

    Parameters
    ----------
    liquidity
        V3 pool liquidity.
    sqrt_price_x96
        V3 current sqrt price (X96).
    v2_rate_multiplier
        V2 marginal rate as a multiple of V3 inverse rate. Values > 1
        produce profitable arbitrage (token0→V3→token1→V2→token0).

    """
    ri, ro = _v3_virtual_reserves(
        liquidity=liquidity,
        sqrt_price_x96=sqrt_price_x96,
        zero_for_one=True,
    )
    v2_reserve_in = ro
    v2_reserve_out = round(float(ri) * v2_rate_multiplier)
    v3_hop = BoundedProductHop(
        reserve_in=ri,
        reserve_out=ro,
        fee=Fraction(3, 1000),
        liquidity=liquidity,
        sqrt_price=sqrt_price_x96,
        tick_lower=0,
        tick_upper=100,
        zero_for_one=True,
    )
    v2_hop = ConstantProductHop(
        reserve_in=v2_reserve_in,
        reserve_out=v2_reserve_out,
        fee=Fraction(3, 1000),
    )
    return v3_hop, v2_hop


def test_piecewise_performance_single_candidate():
    """Benchmark single-range V3 path (should be fast via delegation)."""
    solver = PiecewiseMobiusSolver()

    ranges = (
        V3TickRangeInfo(
            tick_lower=0,
            tick_upper=1000,
            liquidity=10_000_000_000_000,
            sqrt_price_lower=int(1.0 * Q96),
            sqrt_price_upper=int(2.0 * Q96),
        ),
    )

    v3_hop, v2_hop = _make_profitable_v3_v2_path(
        liquidity=10_000_000_000_000,
        sqrt_price_x96=int(1.5 * Q96),
    )
    v3_hop = BoundedProductHop(
        reserve_in=v3_hop.reserve_in,
        reserve_out=v3_hop.reserve_out,
        fee=v3_hop.fee,
        liquidity=v3_hop.liquidity,
        sqrt_price=v3_hop.sqrt_price,
        tick_lower=0,
        tick_upper=1000,
        tick_ranges=ranges,
        current_range_index=0,
        zero_for_one=True,
    )

    input_data = SolveInput(hops=(v3_hop, v2_hop))

    for _ in range(5):
        solver.solve(input_data)

    n_iterations = 100
    start = time.perf_counter_ns()
    for _ in range(n_iterations):
        result = solver.solve(input_data)
    elapsed_ns = time.perf_counter_ns() - start

    avg_time_us = elapsed_ns / n_iterations / 1000

    print(f"\nSingle-range V3: {avg_time_us:.2f}μs per solve")
    print(f"Result: method={result.method}")

    assert avg_time_us < 100.0, f"Too slow: {avg_time_us:.2f}μs"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
