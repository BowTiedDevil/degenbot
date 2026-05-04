"""Test CurveStableswapPool integration with ArbitragePath and solvers."""

from fractions import Fraction

import pytest

from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.types.hop_types import CurveStableswapHop, PoolInvariant


def test_curve_hop_dataclass_with_swap_fn():
    """Test that CurveStableswapHop can be created with swap_fn."""
    def mock_swap(dx: int) -> int:
        return dx * 995 // 1000  # 0.5% fee

    hop = CurveStableswapHop(
        reserve_in=1_000_000_000_000,
        reserve_out=1_000_000_000_000,
        fee=Fraction(5, 10000),  # 0.05%
        curve_a=1000,
        curve_n_coins=2,
        curve_d=0,
        token_index_in=0,
        token_index_out=1,
        precisions=(10**18, 10**18),
        swap_fn=mock_swap,
    )
    assert hop.swap_fn is not None
    assert hop.swap_fn(10000) == 9950
    assert hop.invariant == PoolInvariant.CURVE_STABLESWAP


def test_simulate_path_with_curve_swap_fn():
    """Test that _simulate_path respects Curve swap_fn."""
    from degenbot.arbitrage.optimizers.solver import _simulate_path

    def curve_swap(dx: int) -> int:
        # Mock Curve swap: exact 1:1 minus 0.3% fee
        return dx * 997 // 1000

    curve_hop = CurveStableswapHop(
        reserve_in=1_000_000_000_000,
        reserve_out=1_000_000_000_000,
        fee=Fraction(3, 10000),
        curve_a=1000,
        curve_n_coins=2,
        curve_d=0,
        token_index_in=0,
        token_index_out=1,
        precisions=(10**18, 10**18),
        swap_fn=curve_swap,
    )

    # Single Curve hop
    result = _simulate_path(100000, (curve_hop,))
    assert result == 100000 * 0.997


def test_simulate_path_with_mixed_hops():
    """Test _simulate_path with Curve and V2 hops mixed."""
    from degenbot.arbitrage.optimizers.solver import _simulate_path
    from degenbot.types.hop_types import ConstantProductHop

    def curve_swap(dx: int) -> int:
        return dx * 997 // 1000

    curve_hop = CurveStableswapHop(
        reserve_in=10_000_000_000,
        reserve_out=10_000_000_000,
        fee=Fraction(3, 10000),
        curve_a=1000,
        curve_n_coins=2,
        curve_d=0,
        token_index_in=0,
        token_index_out=1,
        precisions=(10**18, 10**18),
        swap_fn=curve_swap,
    )

    v2_hop = ConstantProductHop(
        reserve_in=10_000_000_000,
        reserve_out=10_000_000_000,
        fee=Fraction(3, 10000),
    )

    # Path: Curve -> V2
    result = _simulate_path(100000, (curve_hop, v2_hop))

    # First hop: 100000 * 0.997 = 99700
    # Second hop: 99700 * 0.997 * 10B / (10B + 99700 * 0.997)
    expected_after_curve = 100000 * 0.997
    g = 1 - 0.0003  # gamma
    expected_after_v2 = expected_after_curve * g * 10_000_000_000 / (10_000_000_000 + expected_after_curve * g)
    assert pytest.approx(result, rel=1e-6) == expected_after_v2


def test_curve_hop_to_hop_state_signature():
    """Test that CurveStableswapPool.to_hop_state accepts the expected signature."""
    # We can't easily construct a CurveStableswapPool without a fork,
    # but we can verify the method signature
    import inspect
    sig = inspect.signature(CurveStableswapPool.to_hop_state)
    params = list(sig.parameters.keys())
    assert "zero_for_one" in params
    assert "state_override" in params
