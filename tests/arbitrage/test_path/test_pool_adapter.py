import pytest

from degenbot.types.pool_protocols import ArbitrageCapablePool

from .conftest import (
    FakeAerodromeV2Pool,
    FakeConcentratedLiquidityPool,
    FakeToken,
    FakeUniswapV2Pool,
)


@pytest.fixture
def token0():
    return FakeToken("0xt0")


@pytest.fixture
def token1():
    return FakeToken("0xt1")


class TestProtocolSatisfaction:
    def test_v2_pool_satisfies_arbitrage_protocol(self, token0, token1):
        pool = FakeUniswapV2Pool(token0, token1)
        assert isinstance(pool, ArbitrageCapablePool)

    def test_v3_pool_satisfies_arbitrage_protocol(self, token0, token1):
        pool = FakeConcentratedLiquidityPool(token0, token1)
        assert isinstance(pool, ArbitrageCapablePool)

    def test_v4_pool_satisfies_arbitrage_protocol(self, token0, token1):
        pool = FakeConcentratedLiquidityPool(token0, token1)
        assert isinstance(pool, ArbitrageCapablePool)

    def test_aerodrome_pool_satisfies_arbitrage_protocol(self, token0, token1):
        pool = FakeAerodromeV2Pool(token0, token1, stable=False)
        assert isinstance(pool, ArbitrageCapablePool)

    def test_unknown_pool_does_not_satisfy_protocol(self):
        assert not isinstance(object(), ArbitrageCapablePool)
