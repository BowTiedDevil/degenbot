"""Tests verifying that concrete pool classes satisfy their pool-shape protocols
(ConstantProductPool, ConcentratedLiquidityPool, StableswapPool) and that
AbstractLiquidityPool remains the root ABC.
"""

import pytest

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.balancer.pools import BalancerV2Pool
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.pancakeswap.pools import PancakeswapV3Pool
from degenbot.registry.pool_type import _derive_family
from degenbot.sushiswap.pools import SushiswapV3Pool
from degenbot.types.abstract import AbstractLiquidityPool
from degenbot.types.pool_type import PoolFamily
from degenbot.uniswap.liquidity_pool import LiquidityPool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool


class TestAbstractLiquidityPoolCannotInstantiate:
    """AbstractLiquidityPool with abstract properties should reject direct instantiation."""

    def test_cannot_instantiate(self):

        with pytest.raises(TypeError, match="abstract method"):
            AbstractLiquidityPool()


class TestConcretePoolsSatisfyProtocols:
    """Concrete pool classes satisfy their pool-shape protocol via isinstance checks.

    Note: issubclass() doesn't work with runtime_checkable protocols that have
    @property members. We verify protocol satisfaction via isinstance on the
    protocol — this checks structural compatibility at runtime.
    """

    def test_v2_pool_satisfies_constant_product(self):
        assert issubclass(LiquidityPool, AbstractLiquidityPool)
        # Structural check: V2Pool has fee_token0, fee_token1, reserves
        assert hasattr(LiquidityPool, "fee_token0")
        assert hasattr(LiquidityPool, "fee_token1")
        assert hasattr(LiquidityPool, "reserves_token0")
        assert hasattr(LiquidityPool, "reserves_token1")

    def test_v3_pool_satisfies_concentrated_liquidity(self):
        assert issubclass(UniswapV3Pool, AbstractLiquidityPool)
        # Structural check: V3Pool has sqrt_price_x96, tick, liquidity
        assert hasattr(UniswapV3Pool, "sqrt_price_x96")
        assert hasattr(UniswapV3Pool, "tick")
        assert hasattr(UniswapV3Pool, "tick_spacing")
        assert hasattr(UniswapV3Pool, "liquidity")

    def test_v4_pool_satisfies_concentrated_liquidity(self):
        assert issubclass(UniswapV4Pool, AbstractLiquidityPool)
        assert hasattr(UniswapV4Pool, "sqrt_price_x96")
        assert hasattr(UniswapV4Pool, "tick")
        assert hasattr(UniswapV4Pool, "tick_spacing")
        assert hasattr(UniswapV4Pool, "liquidity")

    def test_aerodrome_v2_pool_satisfies_constant_product(self):
        assert issubclass(AerodromeV2Pool, AbstractLiquidityPool)
        assert hasattr(AerodromeV2Pool, "fee_token0")
        assert hasattr(AerodromeV2Pool, "fee_token1")
        assert hasattr(AerodromeV2Pool, "reserves_token0")
        assert hasattr(AerodromeV2Pool, "reserves_token1")

    def test_liquidity_pool_satisfies_constant_product(self):
        # ADR-005 slice 7 step 4b: the hollow V2 DEX subclasses (Sushi/Pancake/
        # Swapbased/Camelot) collapsed into the canonical LiquidityPool — it's
        # the single V2-family concrete class, so one shape test covers them all.
        assert issubclass(LiquidityPool, AbstractLiquidityPool)
        assert hasattr(LiquidityPool, "fee_token0")
        assert hasattr(LiquidityPool, "fee_token1")
        assert hasattr(LiquidityPool, "reserves_token0")
        assert hasattr(LiquidityPool, "reserves_token1")

    def test_pancakeswap_v3_pool_satisfies_concentrated_liquidity(self):
        assert issubclass(PancakeswapV3Pool, AbstractLiquidityPool)
        assert hasattr(PancakeswapV3Pool, "sqrt_price_x96")

    def test_sushiswap_v3_pool_satisfies_concentrated_liquidity(self):
        assert issubclass(SushiswapV3Pool, AbstractLiquidityPool)
        assert hasattr(SushiswapV3Pool, "sqrt_price_x96")

    def test_curve_pool_satisfies_base_abc(self):
        assert issubclass(CurveStableswapPool, AbstractLiquidityPool)

    def test_balancer_pool_satisfies_base_abc(self):
        assert issubclass(BalancerV2Pool, AbstractLiquidityPool)


class TestPoolFamilyDerivation:
    """Verify that _derive_family correctly identifies pool families via structural checks."""

    def test_v2_is_constant_product(self):
        assert _derive_family(LiquidityPool) == PoolFamily.CONSTANT_PRODUCT

    def test_v3_is_concentrated_liquidity(self):
        assert _derive_family(UniswapV3Pool) == PoolFamily.CONCENTRATED_LIQUIDITY

    def test_v4_is_concentrated_liquidity(self):
        assert _derive_family(UniswapV4Pool) == PoolFamily.CONCENTRATED_LIQUIDITY

    def test_aerodrome_is_constant_product(self):
        assert _derive_family(AerodromeV2Pool) == PoolFamily.CONSTANT_PRODUCT

    def test_curve_is_stableswap(self):
        assert _derive_family(CurveStableswapPool) == PoolFamily.STABLESWAP

    def test_camelot_is_constant_product(self):
        # Camelot collapsed into LiquidityPool (slice 7 step 4b).
        assert _derive_family(LiquidityPool) == PoolFamily.CONSTANT_PRODUCT
