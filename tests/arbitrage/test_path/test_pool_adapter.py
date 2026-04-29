from degenbot.types.pool_protocols import ArbitrageCapablePool

from .conftest import (
    FakeAerodromeV2Pool,
    FakeConcentratedLiquidityPool,
    FakeUniswapV2Pool,
    _make_token,
)


class TestProtocolSatisfaction:
    def test_v2_pool_satisfies_arbitrage_protocol(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        pool = FakeUniswapV2Pool(t0, t1)
        assert isinstance(pool, ArbitrageCapablePool)

    def test_v3_pool_satisfies_arbitrage_protocol(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        pool = FakeConcentratedLiquidityPool(t0, t1)
        assert isinstance(pool, ArbitrageCapablePool)

    def test_v4_pool_satisfies_arbitrage_protocol(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        pool = FakeConcentratedLiquidityPool(t0, t1)
        assert isinstance(pool, ArbitrageCapablePool)

    def test_aerodrome_pool_satisfies_arbitrage_protocol(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        pool = FakeAerodromeV2Pool(t0, t1, stable=False)
        assert isinstance(pool, ArbitrageCapablePool)

    def test_unknown_pool_does_not_satisfy_protocol(self):
        assert not isinstance(object(), ArbitrageCapablePool)
