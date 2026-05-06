"""
Tests verifying that Pool ABCs cannot be instantiated directly and that
concrete pool classes satisfy their ABC's abstract property requirements.
"""

import pytest

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.balancer.pools import BalancerV2Pool
from degenbot.camelot.pools import CamelotLiquidityPool
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.pancakeswap.pools import PancakeswapV2Pool, PancakeswapV3Pool
from degenbot.sushiswap.pools import SushiswapV2Pool, SushiswapV3Pool
from degenbot.swapbased.pools import SwapbasedV2Pool
from degenbot.types.abstract import (
    AbstractAerodromeV2Pool,
    AbstractConcentratedLiquidityPool,
    AbstractLiquidityPool,
    AbstractUniswapV2Pool,
)
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool


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

        assert issubclass(UniswapV2Pool, AbstractUniswapV2Pool)

    def test_v3_pool_satisfies_abc(self):

        assert issubclass(UniswapV3Pool, AbstractConcentratedLiquidityPool)

    def test_v4_pool_satisfies_abc(self):

        assert issubclass(UniswapV4Pool, AbstractConcentratedLiquidityPool)

    def test_aerodrome_v2_pool_satisfies_abc(self):

        assert issubclass(AerodromeV2Pool, AbstractAerodromeV2Pool)

    def test_camelot_pool_satisfies_v2_abc(self):

        assert issubclass(CamelotLiquidityPool, AbstractUniswapV2Pool)

    def test_pancakeswap_v2_pool_satisfies_v2_abc(self):

        assert issubclass(PancakeswapV2Pool, AbstractUniswapV2Pool)

    def test_pancakeswap_v3_pool_satisfies_cl_abc(self):

        assert issubclass(PancakeswapV3Pool, AbstractConcentratedLiquidityPool)

    def test_sushiswap_v2_pool_satisfies_v2_abc(self):

        assert issubclass(SushiswapV2Pool, AbstractUniswapV2Pool)

    def test_sushiswap_v3_pool_satisfies_cl_abc(self):

        assert issubclass(SushiswapV3Pool, AbstractConcentratedLiquidityPool)

    def test_swapbased_v2_pool_satisfies_v2_abc(self):

        assert issubclass(SwapbasedV2Pool, AbstractUniswapV2Pool)

    def test_curve_pool_satisfies_base_abc(self):

        assert issubclass(CurveStableswapPool, AbstractLiquidityPool)

    def test_balancer_pool_satisfies_base_abc(self):

        assert issubclass(BalancerV2Pool, AbstractLiquidityPool)
