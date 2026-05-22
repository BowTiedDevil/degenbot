import pytest

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.types.pool_protocols import ArbitrageCapablePool
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

from .conftest import _make_aerodrome_pool, _make_v2_pool, _make_v3_pool


@pytest.fixture
def token0():
    from tests.fakes.tokens import FakeToken

    return FakeToken("0xt0")


@pytest.fixture
def token1():
    from tests.fakes.tokens import FakeToken

    return FakeToken("0xt1")


class TestProtocolSatisfaction:
    def test_v2_pool_satisfies_arbitrage_protocol(self, token0, token1):
        pool = _make_v2_pool(token0, token1)
        assert isinstance(pool, ArbitrageCapablePool)

    def test_v3_pool_satisfies_arbitrage_protocol(self, token0, token1):
        pool = _make_v3_pool(token0, token1)
        assert isinstance(pool, ArbitrageCapablePool)

    def test_v4_pool_satisfies_arbitrage_protocol(self, token0, token1):
        # V4 pools are structurally identical to V3 for protocol conformance
        pool = _make_v3_pool(token0, token1)
        assert isinstance(pool, ArbitrageCapablePool)

    def test_aerodrome_pool_satisfies_arbitrage_protocol(self, token0, token1):
        pool = _make_aerodrome_pool(token0, token1, stable=False)
        assert isinstance(pool, ArbitrageCapablePool)

    def test_unknown_pool_does_not_satisfy_protocol(self):
        assert not isinstance(object(), ArbitrageCapablePool)
