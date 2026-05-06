"""
Tests verifying that Pool ABCs cannot be instantiated directly and that
concrete pool classes satisfy their ABC's abstract property requirements.
"""


import pytest

from degenbot.types.abstract import (
    AbstractAerodromeV2Pool,
    AbstractConcentratedLiquidityPool,
    AbstractLiquidityPool,
    AbstractUniswapV2Pool,
)


class TestAbstractLiquidityPoolCannotInstantiate:
    """AbstractLiquidityPool with abstract properties should reject direct instantiation."""

    def test_cannot_instantiate(self):
        with pytest.raises(TypeError, match="abstract method"):
            AbstractLiquidityPool()


class TestAbstractUniswapV2PoolCannotInstantiate:
    def test_cannot_instantiate(self):
        with pytest.raises(TypeError, match="abstract method"):
            AbstractUniswapV2Pool()


class TestAbstractConcentratedLiquidityPoolCannotInstantiate:
    def test_cannot_instantiate(self):
        with pytest.raises(TypeError, match="abstract method"):
            AbstractConcentratedLiquidityPool()


class TestAbstractAerodromeV2PoolCannotInstantiate:
    def test_cannot_instantiate(self):
        with pytest.raises(TypeError, match="abstract method"):
            AbstractAerodromeV2Pool()


class TestConcretePoolsSatisfyABCs:
    """Concrete pool classes define all abstract properties required by their ABC."""

    def test_v2_pool_satisfies_abc(self):
        from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool

        assert issubclass(UniswapV2Pool, AbstractUniswapV2Pool)

    def test_v3_pool_satisfies_abc(self):
        from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

        assert issubclass(UniswapV3Pool, AbstractConcentratedLiquidityPool)

    def test_v4_pool_satisfies_abc(self):
        from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool

        assert issubclass(UniswapV4Pool, AbstractConcentratedLiquidityPool)

    def test_aerodrome_v2_pool_satisfies_abc(self):
        from degenbot.aerodrome.pools import AerodromeV2Pool

        assert issubclass(AerodromeV2Pool, AbstractAerodromeV2Pool)

    def test_camelot_pool_satisfies_v2_abc(self):
        from degenbot.camelot.pools import CamelotLiquidityPool

        assert issubclass(CamelotLiquidityPool, AbstractUniswapV2Pool)

    def test_pancakeswap_v2_pool_satisfies_v2_abc(self):
        from degenbot.pancakeswap.pools import PancakeswapV2Pool

        assert issubclass(PancakeswapV2Pool, AbstractUniswapV2Pool)

    def test_pancakeswap_v3_pool_satisfies_cl_abc(self):
        from degenbot.pancakeswap.pools import PancakeswapV3Pool

        assert issubclass(PancakeswapV3Pool, AbstractConcentratedLiquidityPool)

    def test_sushiswap_v2_pool_satisfies_v2_abc(self):
        from degenbot.sushiswap.pools import SushiswapV2Pool

        assert issubclass(SushiswapV2Pool, AbstractUniswapV2Pool)

    def test_sushiswap_v3_pool_satisfies_cl_abc(self):
        from degenbot.sushiswap.pools import SushiswapV3Pool

        assert issubclass(SushiswapV3Pool, AbstractConcentratedLiquidityPool)

    def test_swapbased_v2_pool_satisfies_v2_abc(self):
        from degenbot.swapbased.pools import SwapbasedV2Pool

        assert issubclass(SwapbasedV2Pool, AbstractUniswapV2Pool)

    def test_curve_pool_satisfies_base_abc(self):
        from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool

        assert issubclass(CurveStableswapPool, AbstractLiquidityPool)

    def test_balancer_pool_satisfies_base_abc(self):
        from degenbot.balancer.pools import BalancerV2Pool

        assert issubclass(BalancerV2Pool, AbstractLiquidityPool)
