"""Unit tests for PerBlockCache — the per-block on-chain cache for CurveStableswapPool."""

from __future__ import annotations

import pytest

from degenbot.curve.per_block_cache import PerBlockCache
from degenbot.exceptions.pool import MissingCurveData
from tests.fakes.curve_data_provider import FakeCurveDataProvider


def _make_cache(
    *,
    data_provider: FakeCurveDataProvider | None = None,
    base_pool_is_set: bool = False,
    state_cache_depth: int = 8,
) -> PerBlockCache:
    """Create a PerBlockCache with sensible defaults for testing.

    Returns:
        A PerBlockCache instance configured for testing.

    """
    return PerBlockCache(
        data_provider=data_provider,
        address="0x" + "0" * 40,
        base_pool_is_set=base_pool_is_set,
        state_cache_depth=state_cache_depth,
    )


class TestCacheMissCallsProvider:
    """On cache miss, PerBlockCache calls the data provider and stores the result."""

    def test_block_timestamp(self) -> None:
        """Block timestamp is fetched from provider on miss, then cached."""
        fake = FakeCurveDataProvider(block_timestamp=12345)
        cache = _make_cache(data_provider=fake)
        result = cache.get_cached_block_timestamp(100)
        assert result == 12345
        assert cache.get_cached_block_timestamp(100) == 12345

    def test_contract_d(self) -> None:
        """Contract D is fetched from provider on miss, then cached."""
        fake = FakeCurveDataProvider(d=10**18)
        cache = _make_cache(data_provider=fake)
        result = cache.get_cached_contract_d(100)
        assert result == 10**18
        assert cache.get_cached_contract_d(100) == 10**18

    def test_gamma(self) -> None:
        """Gamma is fetched from provider on miss, then cached."""
        fake = FakeCurveDataProvider(gamma=10**10)
        cache = _make_cache(data_provider=fake)
        result = cache.get_cached_gamma(100)
        assert result == 10**10
        assert cache.get_cached_gamma(100) == 10**10

    def test_price_scale(self) -> None:
        """Price scale is fetched from provider on miss, then cached."""
        fake = FakeCurveDataProvider(price_scale=(10**18, 10**18))
        cache = _make_cache(data_provider=fake)
        result = cache.get_cached_price_scale(100)
        assert result == (10**18, 10**18)
        assert cache.get_cached_price_scale(100) == (10**18, 10**18)

    def test_admin_balances(self) -> None:
        """Admin balances are fetched from provider on miss, then cached."""
        fake = FakeCurveDataProvider(admin_balances=(100, 200))
        cache = _make_cache(data_provider=fake)
        result = cache.get_cached_admin_balances(100)
        assert result == (100, 200)
        assert cache.get_cached_admin_balances(100) == (100, 200)

    def test_scaled_redemption_price(self) -> None:
        """Scaled redemption price is fetched from provider on miss, then cached."""
        fake = FakeCurveDataProvider(redemption_price=10**18)
        cache = _make_cache(data_provider=fake)
        result = cache.get_cached_scaled_redemption_price(100)
        assert result == 10**18
        assert cache.get_cached_scaled_redemption_price(100) == 10**18


class TestCacheMissingProviderRaises:
    """On cache miss with no provider, PerBlockCache raises MissingCurveData."""

    def test_block_timestamp(self) -> None:
        """Missing provider raises MissingCurveData for block_timestamp."""
        cache = _make_cache(data_provider=None)
        with pytest.raises(MissingCurveData):
            cache.get_cached_block_timestamp(100)

    def test_contract_d(self) -> None:
        """Missing provider raises MissingCurveData for contract D."""
        cache = _make_cache(data_provider=None)
        with pytest.raises(MissingCurveData):
            cache.get_cached_contract_d(100)

    def test_gamma(self) -> None:
        """Missing provider raises MissingCurveData for gamma."""
        cache = _make_cache(data_provider=None)
        with pytest.raises(MissingCurveData):
            cache.get_cached_gamma(100)

    def test_price_scale(self) -> None:
        """Missing provider raises MissingCurveData for price_scale."""
        cache = _make_cache(data_provider=None)
        with pytest.raises(MissingCurveData):
            cache.get_cached_price_scale(100)

    def test_admin_balances(self) -> None:
        """Missing provider raises MissingCurveData for admin_balances."""
        cache = _make_cache(data_provider=None)
        with pytest.raises(MissingCurveData):
            cache.get_cached_admin_balances(100)

    def test_scaled_redemption_price(self) -> None:
        """Missing provider raises MissingCurveData for scaled_redemption_price."""
        cache = _make_cache(data_provider=None)
        with pytest.raises(MissingCurveData):
            cache.get_cached_scaled_redemption_price(100)

    def test_base_cache_updated(self) -> None:
        """Missing provider raises MissingCurveData for base_cache_updated."""
        cache = _make_cache(data_provider=None)
        with pytest.raises(MissingCurveData):
            cache.get_cached_base_cache_updated(100)

    def test_base_virtual_price(self) -> None:
        """Missing provider raises MissingCurveData for base_virtual_price."""
        cache = _make_cache(data_provider=None)
        with pytest.raises(MissingCurveData):
            cache.get_cached_base_virtual_price(100)

    def test_virtual_price_non_metapool(self) -> None:
        """Missing provider raises MissingCurveData for virtual_price on non-metapool."""
        cache = _make_cache(data_provider=None, base_pool_is_set=False)
        with pytest.raises(MissingCurveData):
            cache.get_cached_virtual_price(100)


class TestVirtualPriceNonMetapool:
    """Non-metapool path fetches virtual_price directly from provider."""

    def test_fetches_directly(self) -> None:
        """Non-metapool gets virtual_price directly from provider, then caches it."""
        fake = FakeCurveDataProvider(virtual_price=10**18)
        cache = _make_cache(data_provider=fake, base_pool_is_set=False)
        result = cache.get_cached_virtual_price(100)
        assert result == 10**18
        assert cache.get_cached_virtual_price(100) == 10**18


class TestVirtualPriceExpiryLogic:
    """Metapool virtual_price expiry and base-cache-validity logic."""

    def test_base_cache_valid_uses_cached_base_virtual_price(self) -> None:
        """When base cache has not expired, virtual_price uses cached base_virtual_price."""
        fake = FakeCurveDataProvider(
            base_cache_updated=1_700_000_000,
            base_virtual_price=10**18 + 42,
            virtual_price=10**18 + 999,
            block_timestamp=1_700_000_100,
        )
        cache = _make_cache(data_provider=fake, base_pool_is_set=True)

        cache.get_cached_base_cache_updated(100)
        cache.get_cached_base_virtual_price(100)

        result = cache.get_cached_virtual_price(100)
        assert result == 10**18 + 42

    def test_base_cache_expired_fetches_live_virtual_price(self) -> None:
        """When base cache has expired, virtual_price fetches live from provider."""
        fake = FakeCurveDataProvider(
            base_cache_updated=1_700_000_000,
            base_virtual_price=10**18 + 42,
            virtual_price=10**18 + 999,
            block_timestamp=1_700_000_700,
        )
        cache = _make_cache(data_provider=fake, base_pool_is_set=True)

        cache.get_cached_base_cache_updated(100)

        result = cache.get_cached_virtual_price(100)
        assert result == 10**18 + 999

    def test_base_cache_never_set_fetches_live_virtual_price(self) -> None:
        """When base_cache_updated was never resolved, virtual_price fetches live."""
        fake = FakeCurveDataProvider(
            base_cache_updated=1_700_000_000,
            base_virtual_price=10**18 + 42,
            virtual_price=10**18 + 999,
            block_timestamp=1_700_000_100,
        )
        cache = _make_cache(data_provider=fake, base_pool_is_set=True)

        result = cache.get_cached_virtual_price(100)
        assert result == 10**18 + 42
