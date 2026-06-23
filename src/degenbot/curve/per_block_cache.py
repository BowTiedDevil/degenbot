"""Per-block on-chain cache for CurveStableswapPool.

Owns all BoundedCache instances and their accessor methods, extracting the
cache subsystem from the pool class. Each accessor follows the pure
try/cache/store/return pattern — no accessor mutates state read by another
accessor. get_cached_virtual_price() resolves its own dependencies inline
(calling get_cached_base_cache_updated() and get_cached_base_virtual_price()),
eliminating the need for side-effect mirrors or call-ordering contracts.
"""

import contextlib

from eth_typing import ChecksumAddress

from degenbot.exceptions.pool import MissingCurveData
from degenbot.types.aliases import BlockNumber
from degenbot.types.concrete import BoundedCache

from .types import CurveDataProvider


class PerBlockCache:
    """Owns all per-block on-chain caches for a CurveStableswapPool.

    Each accessor implements the try-cache → call-provider → store → return
    pattern. No accessor mutates state read by another accessor.
    get_cached_virtual_price() resolves its own dependencies internally,
    eliminating the need for side-effect mirrors or call-ordering contracts.

    The pool holds one instance of this class instead of nine
    separate BoundedCache fields.
    """

    # Moved from CurveStableswapPool — only used by get_cached_virtual_price
    BASE_CACHE_EXPIRES: int = 10 * 60  # 10 minutes in seconds

    def __init__(
        self,
        data_provider: CurveDataProvider | None,
        address: ChecksumAddress,
        *,
        base_pool_is_set: bool,
        state_cache_depth: int,
    ) -> None:
        """Initialise PerBlockCache with a data provider and cache depth."""
        self._data_provider = data_provider
        self._address = address
        self._base_pool_is_set = base_pool_is_set

        # Cache fields
        self._cache_block_timestamps: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth,
        )
        self._cache_contract_D: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth,
        )
        self._cache_gamma: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth,
        )
        self._cache_price_scale: BoundedCache[BlockNumber, tuple[int, ...]] = BoundedCache(
            max_items=state_cache_depth,
        )
        self._cache_admin_balances: BoundedCache[BlockNumber, tuple[int, ...]] = BoundedCache(
            max_items=state_cache_depth,
        )
        self._cache_scaled_redemption_price: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth,
        )
        self._cache_virtual_price: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth,
        )
        self._cache_base_cache_updated: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth,
        )
        self._cache_base_virtual_price: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=state_cache_depth,
        )

    # ── Accessors (all follow the same try/cache/store/return pattern) ──

    def get_cached_block_timestamp(self, block_number: BlockNumber) -> int:
        """Fetch or retrieve cached block timestamp.

        Returns:
            The block timestamp as an integer.

        Raises:
            MissingCurveData: If the value is not cached and no provider is available.

        """
        with contextlib.suppress(KeyError):
            return self._cache_block_timestamps[block_number]
        if self._data_provider is None:
            msg = "block_timestamp requires a data_provider. Provide one via Bot.build_pool()."
            raise MissingCurveData(self._address, "block_timestamp", msg)
        result = self._data_provider.block_timestamp(block_number)
        self._cache_block_timestamps[block_number] = result
        return result

    def get_cached_contract_d(self, block_number: BlockNumber) -> int:
        """Fetch or retrieve cached contract D value (crypto pools).

        Returns:
            The D invariant value as an integer.

        Raises:
            MissingCurveData: If the value is not cached and no provider is available.

        """
        with contextlib.suppress(KeyError):
            return self._cache_contract_D[block_number]
        if self._data_provider is None:
            msg = "contract_D requires a data_provider. Provide one via Bot.build_pool()."
            raise MissingCurveData(self._address, "D", msg)
        result = self._data_provider.d(block_number)
        self._cache_contract_D[block_number] = result
        return result

    def get_cached_gamma(self, block_number: BlockNumber) -> int:
        """Fetch or retrieve cached gamma value (crypto pools).

        Returns:
            The gamma value as an integer.

        Raises:
            MissingCurveData: If the value is not cached and no provider is available.

        """
        with contextlib.suppress(KeyError):
            return self._cache_gamma[block_number]
        if self._data_provider is None:
            msg = "gamma requires a data_provider. Provide one via Bot.build_pool()."
            raise MissingCurveData(self._address, "gamma", msg)
        result = self._data_provider.gamma(block_number)
        self._cache_gamma[block_number] = result
        return result

    def get_cached_price_scale(self, block_number: BlockNumber) -> tuple[int, ...]:
        """Fetch or retrieve cached price scale (crypto pools).

        Returns:
            The price scale values as a tuple of integers.

        Raises:
            MissingCurveData: If the value is not cached and no provider is available.

        """
        with contextlib.suppress(KeyError):
            return self._cache_price_scale[block_number]
        if self._data_provider is None:
            msg = "price_scale requires a data_provider. Provide one via Bot.build_pool()."
            raise MissingCurveData(self._address, "price_scale", msg)
        result = self._data_provider.price_scale(block_number)
        self._cache_price_scale[block_number] = result
        return result

    def get_cached_admin_balances(self, block_number: BlockNumber) -> tuple[int, ...]:
        """Fetch or retrieve cached admin balances.

        Returns:
            The admin balances as a tuple of integers.

        Raises:
            MissingCurveData: If the value is not cached and no provider is available.

        """
        with contextlib.suppress(KeyError):
            return self._cache_admin_balances[block_number]
        if self._data_provider is None:
            msg = "admin_balances requires a data_provider. Provide one via Bot.build_pool()."
            raise MissingCurveData(self._address, "admin_balances", msg)
        result = self._data_provider.admin_balances(block_number)
        self._cache_admin_balances[block_number] = result
        return result

    def get_cached_scaled_redemption_price(self, block_number: BlockNumber) -> int:
        """Fetch or retrieve cached scaled redemption price.

        Returns:
            The scaled redemption price as an integer.

        Raises:
            MissingCurveData: If the value is not cached and no provider is available.

        """
        with contextlib.suppress(KeyError):
            return self._cache_scaled_redemption_price[block_number]
        if self._data_provider is None:
            msg = (
                "scaled_redemption_price requires a data_provider."
                " Provide one via Bot.build_pool()."
            )
            raise MissingCurveData(self._address, "redemption_price", msg)
        result = self._data_provider.redemption_price(block_number)
        self._cache_scaled_redemption_price[block_number] = result
        return result

    def get_cached_base_cache_updated(self, block_number: BlockNumber) -> int:
        """Fetch or retrieve cached base_cache_updated block number.

        Pure try/cache/store/return. No mirror side effect.

        Returns:
            The base_cache_updated timestamp as an integer.

        Raises:
            MissingCurveData: If the value is not cached and no provider is available.

        """
        with contextlib.suppress(KeyError):
            return self._cache_base_cache_updated[block_number]
        if self._data_provider is None:
            msg = "base_cache_updated requires a data_provider. Provide one via Bot.build_pool()."
            raise MissingCurveData(self._address, "base_cache_updated", msg)
        result = self._data_provider.base_cache_updated(block_number)
        self._cache_base_cache_updated[block_number] = result
        return result

    def get_cached_base_virtual_price(self, block_number: BlockNumber) -> int:
        """Fetch or retrieve cached base pool virtual price.

        Pure try/cache/store/return. No mirror side effect.

        Returns:
            The base pool virtual price as an integer.

        Raises:
            MissingCurveData: If the value is not cached and no provider is available.

        """
        with contextlib.suppress(KeyError):
            return self._cache_base_virtual_price[block_number]
        if self._data_provider is None:
            msg = "base_virtual_price requires a data_provider. Provide one via Bot.build_pool()."
            raise MissingCurveData(self._address, "base_virtual_price", msg)
        result = self._data_provider.base_virtual_price(block_number)
        self._cache_base_virtual_price[block_number] = result
        return result

    def get_cached_virtual_price(self, block_number: BlockNumber) -> int:
        """Resolve virtual price with self-contained expiry logic.

        For metapools, internally resolves base_cache_updated and
        base_virtual_price (calling their accessors) to determine
        whether to use the cached base_virtual_price or fetch live.
        No side-effect mirrors — all dependency resolution is inline.

        Returns:
            The virtual price as an integer.

        Raises:
            MissingCurveData: If the value is not cached and no provider is available.

        """
        with contextlib.suppress(KeyError):
            return self._cache_virtual_price[block_number]

        base_virtual_price: int
        if not self._base_pool_is_set:
            # Non-metapool: fetch directly
            if self._data_provider is None:
                msg = "virtual_price requires a data_provider. Provide one via Bot.build_pool()."
                raise MissingCurveData(self._address, "virtual_price", msg)
            base_virtual_price = self._data_provider.virtual_price(block_number)
        else:
            # Metapool expiry logic (matches contract's _vp_rate_ro())
            base_cache_updated = self.get_cached_base_cache_updated(block_number)
            block_timestamp = self.get_cached_block_timestamp(block_number)
            if block_timestamp > base_cache_updated + self.BASE_CACHE_EXPIRES:
                # Cache expired — fetch live virtual_price
                if self._data_provider is None:
                    msg = (
                        "virtual_price requires a data_provider. Provide one via Bot.build_pool()."
                    )
                    raise MissingCurveData(self._address, "virtual_price", msg)
                base_virtual_price = self._data_provider.virtual_price(block_number)
            else:
                # Cache valid — resolve base_virtual_price from its own cache
                base_virtual_price = self.get_cached_base_virtual_price(block_number)

        self._cache_virtual_price[block_number] = base_virtual_price
        return base_virtual_price
