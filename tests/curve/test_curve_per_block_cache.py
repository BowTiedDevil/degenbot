"""Per-block cache behavior of ``CurveStableswapPool``.

Exercises the pool's inlined per-block accessors — ``_get_cached_block_timestamp``
and ``_get_cached_virtual_price`` — including the metapool base-cache expiry the
contract's ``_vp_rate_ro()`` performs.
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

    def test_virtual_price(self) -> None:
        """Missing provider raises MissingCurveData for virtual_price."""
        lp = _plain_pool(data_provider=None)
        with pytest.raises(MissingCurveData):
            lp._get_cached_virtual_price(100)


class TestVirtualPriceNonMetapool:
    """A plain pool fetches virtual_price directly from the provider."""

    def test_fetches_directly(self) -> None:
        """Non-metapool gets virtual_price directly, then caches it."""
        lp = _plain_pool(data_provider=FakeCurveDataProvider(virtual_price=_E18))
        assert lp._get_cached_virtual_price(100) == _E18
        assert lp._get_cached_virtual_price(100) == _E18


class TestVirtualPriceExpiryLogic:
    """Metapool virtual_price expiry and base-cache-validity logic."""

    def test_base_cache_valid_uses_base_virtual_price(self) -> None:
        """When the base cache has not expired, the base virtual price is used."""
        lp = _metapool(
            data_provider=FakeCurveDataProvider(
                base_cache_updated=1_700_000_000,
                base_virtual_price=_E18 + 42,
                virtual_price=_E18 + 999,
                block_timestamp=1_700_000_100,
            ),
        )
        assert lp._get_cached_virtual_price(100) == _E18 + 42

    def test_base_cache_expired_fetches_live_virtual_price(self) -> None:
        """When the base cache has expired, the live virtual price is fetched."""
        lp = _metapool(
            data_provider=FakeCurveDataProvider(
                base_cache_updated=1_700_000_000,
                base_virtual_price=_E18 + 42,
                virtual_price=_E18 + 999,
                block_timestamp=1_700_000_700,
            ),
        )
        assert lp._get_cached_virtual_price(100) == _E18 + 999


class TestConstructionPrefetchOutcome:
    """The construction-time prefetch is a typed, deliberately-ignored fact."""

    def test_missing_provider_at_construction_is_typed_and_ignored(self) -> None:
        """Fault-injected: a metapool built with NO stored data provider.

        Construction succeeds — the prefetch's typed ``MissingCurveData`` is a
        deliberately-ignored fact (a provider-less pool is legal; the Rust core
        resolves the virtual price at calc time). Asking for the value directly
        surfaces that typed fact.
        """
        lp = _metapool(data_provider=None, state_block=12_345)
        assert isinstance(lp, CurveStableswapPool)
        with pytest.raises(MissingCurveData):
            lp._get_cached_virtual_price(12_345)
