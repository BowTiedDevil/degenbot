from degenbot.arbitrage.path.pool_adapter import get_adapter

from .conftest import (
    FakeAerodromeV2Pool,
    FakeConcentratedLiquidityPool,
    FakeUniswapV2Pool,
    _make_token,
)


class TestAdapterRegistry:
    def test_v2_adapter_registered(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        pool = FakeUniswapV2Pool(t0, t1)
        adapter = get_adapter(pool)
        assert adapter is not None
        assert adapter.__class__.__name__ == "UniswapV2PoolAdapter"

    def test_v3_adapter_registered(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        pool = FakeConcentratedLiquidityPool(t0, t1)
        adapter = get_adapter(pool)
        assert adapter is not None
        assert adapter.__class__.__name__ == "ConcentratedLiquidityAdapter"

    def test_v4_adapter_registered(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        pool = FakeConcentratedLiquidityPool(t0, t1)
        adapter = get_adapter(pool)
        assert adapter is not None
        assert adapter.__class__.__name__ == "ConcentratedLiquidityAdapter"

    def test_aerodrome_adapter_registered(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        pool = FakeAerodromeV2Pool(t0, t1, stable=False)
        adapter = get_adapter(pool)
        assert adapter is not None
        assert adapter.__class__.__name__ == "AerodromeV2PoolAdapter"

    def test_unknown_pool_returns_none(self):
        assert get_adapter(object()) is None

    def test_v3_and_v4_share_adapter(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        v3 = FakeConcentratedLiquidityPool(t0, t1)
        v4 = FakeConcentratedLiquidityPool(t0, t1)
        assert get_adapter(v3) is get_adapter(v4)
