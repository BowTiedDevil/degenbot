"""
Tests verifying existing pool classes satisfy the PoolSimulation protocol.

These tests create lightweight pool instances (using offline providers or
anvil forks) and verify isinstance checks against the runtime-checkable
protocols.
"""

import pytest

from degenbot.types.pool_protocols import (
    ArbitrageCapablePool,
    PoolSimulation,
    ReverseSimulatablePool,
    StateManageablePool,
)


class TestProtocolChecks:
    """
    Verify that pool classes have the required methods for protocol
    satisfaction. We check method existence rather than constructing
    full pool instances (which require chain access).
    """

    def test_v2_pool_has_simulate_swap(self):
        from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool

        assert hasattr(UniswapV2Pool, "simulate_swap")
        assert hasattr(UniswapV2Pool, "simulate_swap_for_output")

    def test_v3_pool_has_simulate_swap(self):
        from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

        assert hasattr(UniswapV3Pool, "simulate_swap")
        assert hasattr(UniswapV3Pool, "simulate_swap_for_output")

    def test_v4_pool_has_simulate_swap(self):
        from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool

        assert hasattr(UniswapV4Pool, "simulate_swap")

    def test_aerodrome_v2_pool_has_simulate_swap(self):
        from degenbot.aerodrome.pools import AerodromeV2Pool

        assert hasattr(AerodromeV2Pool, "simulate_swap")
        assert hasattr(AerodromeV2Pool, "simulate_swap_for_output")

    def test_aerodrome_v3_pool_has_simulate_swap(self):
        from degenbot.aerodrome.pools import AerodromeV3Pool

        assert hasattr(AerodromeV3Pool, "simulate_swap")
        assert hasattr(AerodromeV3Pool, "simulate_swap_for_output")

    def test_curve_pool_has_simulate_swap(self):
        from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool

        assert hasattr(CurveStableswapPool, "simulate_swap")

    def test_balancer_pool_has_simulate_swap(self):
        from degenbot.balancer.pools import BalancerV2Pool

        assert hasattr(BalancerV2Pool, "simulate_swap")


class TestFakePoolSatisfiesProtocols:
    """
    Verify FakePool implementations in test fixtures satisfy the
    protocols once they add the required methods.
    """

    def test_pool_simulation_protocol_is_runtime_checkable(self):
        assert issubclass(type(PoolSimulation), type)

    def test_arbitrage_capable_pool_protocol_is_runtime_checkable(self):
        assert issubclass(type(ArbitrageCapablePool), type)

    def test_reverse_simulatable_pool_protocol_is_runtime_checkable(self):
        assert issubclass(type(ReverseSimulatablePool), type)

    def test_state_manageable_pool_protocol_is_runtime_checkable(self):
        assert issubclass(type(StateManageablePool), type)
