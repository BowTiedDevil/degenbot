from unittest.mock import MagicMock

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.arbitrage.path.pool_adapter import get_adapter
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool


class TestAdapterRegistry:
    def test_v2_adapter_registered(self):
        pool = MagicMock(spec=UniswapV2Pool)
        adapter = get_adapter(pool)
        assert adapter is not None
        assert adapter.__class__.__name__ == "UniswapV2PoolAdapter"

    def test_v3_adapter_registered(self):
        pool = MagicMock(spec=UniswapV3Pool)
        adapter = get_adapter(pool)
        assert adapter is not None
        assert adapter.__class__.__name__ == "ConcentratedLiquidityAdapter"

    def test_v4_adapter_registered(self):
        pool = MagicMock(spec=UniswapV4Pool)
        adapter = get_adapter(pool)
        assert adapter is not None
        assert adapter.__class__.__name__ == "ConcentratedLiquidityAdapter"

    def test_aerodrome_adapter_registered(self):
        pool = MagicMock(spec=AerodromeV2Pool)
        pool.stable = False
        adapter = get_adapter(pool)
        assert adapter is not None
        assert adapter.__class__.__name__ == "AerodromeV2PoolAdapter"

    def test_unknown_pool_returns_none(self):
        assert get_adapter(MagicMock()) is None

    def test_v3_and_v4_share_adapter(self):
        v3 = MagicMock(spec=UniswapV3Pool)
        v4 = MagicMock(spec=UniswapV4Pool)
        assert get_adapter(v3) is get_adapter(v4)
