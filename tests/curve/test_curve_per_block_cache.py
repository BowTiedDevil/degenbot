"""Per-block cache behavior of ``CurveStableswapPool``.

Exercises the pool's inlined ``_get_cached_block_timestamp`` accessor (the
try-cache→call-provider→store→return pattern). The metapool base-cache
freshness rule is not a Python concern: the Rust core resolves the metapool
virtual price at calc time.
"""

from __future__ import annotations

import pytest

from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.exceptions.pool import MissingCurveData
from tests.fakes.curve_data_provider import FakeCurveDataProvider
from tests.helpers.curve_pool_factory import make_curve_pool
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.pool_bot import new_pool_bot

_PLAIN = "0x" + "aa" * 20
_BASE = "0x" + "bb" * 20
_META = "0x" + "cc" * 20
_META_A = "0x" + "10" * 20
_META_B = "0x" + "11" * 20
_BASE_A = "0x" + "20" * 20
_BASE_B = "0x" + "21" * 20
_BASE_C = "0x" + "22" * 20
_E18 = 10**18
_E21 = 10**21


def _token(bot, address: str, symbol: str):
    return make_erc20(bot, address, name=symbol, symbol=symbol, decimals=18, chain_id=1)


def _plain_pool(*, data_provider: FakeCurveDataProvider | None = None) -> CurveStableswapPool:
    """Build a 2-coin plain pool with an optional data provider."""
    bot = new_pool_bot()
    tokens = [_token(bot, _META_A, "MA"), _token(bot, _META_B, "MB")]
    return make_curve_pool(
        _PLAIN,
        tokens=tokens,
        a_coefficient=100,
        fee=4_000_000,
        admin_fee=0,
        balances=[_E21, _E21],
        state_block=0,
        data_provider=data_provider,
        py_bot=bot,
    )


def _metapool(
    *,
    data_provider: FakeCurveDataProvider | None = None,
    state_block: int = 0,
) -> CurveStableswapPool:
    """Build a metapool over a 3-coin base pool, sharing one local ``Bot``."""
    bot = new_pool_bot()
    base_tokens = [
        _token(bot, _BASE_A, "BA"),
        _token(bot, _BASE_B, "BB"),
        _token(bot, _BASE_C, "BC"),
    ]
    base = make_curve_pool(
        _BASE,
        tokens=base_tokens,
        a_coefficient=100,
        fee=4_000_000,
        admin_fee=0,
        balances=[_E21, _E21, _E21],
        state_block=0,
        data_provider=FakeCurveDataProvider(virtual_price=_E18, block_timestamp=0),
        py_bot=bot,
    )
    meta_tokens = [_token(bot, _META_A, "MA"), _token(bot, _META_B, "MB")]
    return make_curve_pool(
        _META,
        tokens=meta_tokens,
        base_pool=base,
        tokens_underlying=base_tokens,
        a_coefficient=100,
        fee=4_000_000,
        admin_fee=0,
        balances=[_E21, _E21],
        state_block=state_block,
        data_provider=data_provider,
        py_bot=bot,
    )


class TestCacheMissCallsProvider:
    """On cache miss, the accessor calls the data provider and stores the result."""

    def test_block_timestamp(self) -> None:
        """Block timestamp is fetched from the provider on miss, then cached."""
        lp = _plain_pool(data_provider=FakeCurveDataProvider(block_timestamp=12345))
        assert lp._get_cached_block_timestamp(100) == 12345
        assert lp._get_cached_block_timestamp(100) == 12345


class TestCacheMissingProviderRaises:
    """On cache miss with no provider, the accessor raises MissingCurveData."""

    def test_block_timestamp(self) -> None:
        """Missing provider raises MissingCurveData for block_timestamp."""
        lp = _plain_pool(data_provider=None)
        with pytest.raises(MissingCurveData):
            lp._get_cached_block_timestamp(100)


class TestProviderlessMetapoolConstruction:
    """A metapool built with NO stored data provider is legal."""

    def test_construction_succeeds(self) -> None:
        """Construction reads no provider: the Rust core resolves state at calc time."""
        lp = _metapool(data_provider=None, state_block=12_345)
        assert isinstance(lp, CurveStableswapPool)
        assert lp.update_block == 12_345
