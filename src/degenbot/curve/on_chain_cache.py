"""
Consolidated on-chain data cache for Curve StableSwap pools.

Owns all per-block `BoundedCache` instances and provides accessor methods
that encapsulate the try-cache → call-provider → store → return pattern.

This module is an internal implementation detail of `CurveStableswapPool`.
The public seam is `CurveDataProvider` for I/O and `DyCalculationInputs`
for calculations.
"""

from __future__ import annotations

import contextlib
from typing import TYPE_CHECKING, Any, TypeVar

from degenbot.exceptions.pool import MissingCurveData
from degenbot.types.aliases import BlockNumber  # noqa: TC001
from degenbot.types.concrete import BoundedCache

if TYPE_CHECKING:
    from degenbot.curve.types import CurveDataProvider

_T = TypeVar("_T")


class CurveOnChainCache:
    """
    Per-block cache for on-chain Curve pool data.

    Replaces 10 individual `BoundedCache` fields in `CurveStableswapPool`
    with a single object that owns all caches and provides accessor methods
    with the try-cache → call-provider → store → return pattern.

    Attributes
    ----------
    base_cache_updated : int | None
        The block number when the base pool's cache was last updated.
        Used by `_get_virtual_price` to determine if the cached
        `base_virtual_price` is still valid.
    base_virtual_price : int
        The most recently fetched base pool virtual price. Updated
        whenever `_get_virtual_price` fetches from the provider.
    """

    def __init__(
        self,
        *,
        data_provider: CurveDataProvider | None,
        pool_address: str,
        max_items: int,
    ) -> None:
        self._data_provider = data_provider
        self._pool_address = pool_address
        self._max_items = max_items

        # Instance-level latest-value mirrors (for base_cache_updated / base_virtual_price expiry)
        self.base_cache_updated: int | None = None
        self.base_virtual_price: int = 0

        # Per-block caches
        self._rates: BoundedCache[BlockNumber, tuple[int, ...]] = BoundedCache(max_items=max_items)
        self._scaled_redemption_price: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=max_items
        )
        self._virtual_price: BoundedCache[BlockNumber, int] = BoundedCache(max_items=max_items)
        self._admin_balances: BoundedCache[BlockNumber, tuple[int, ...]] = BoundedCache(
            max_items=max_items
        )
        self._base_cache_updated: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=max_items
        )
        self._base_virtual_price: BoundedCache[BlockNumber, int] = BoundedCache(
            max_items=max_items
        )
        self._price_scale: BoundedCache[BlockNumber, tuple[int, ...]] = BoundedCache(
            max_items=max_items
        )
        self._contract_D: BoundedCache[BlockNumber, int] = BoundedCache(max_items=max_items)
        self._gamma: BoundedCache[BlockNumber, int] = BoundedCache(max_items=max_items)

        # Block timestamp cache (not per-provider, shared across pools)
        self._block_timestamps: dict[BlockNumber, int] = {}

    # ── Pickle support ──

    def __getstate__(self) -> dict[str, Any]:
        # Drop the data_provider reference — it may contain un-pickleable closures
        state = self.__dict__.copy()
        state["_data_provider"] = None
        return state

    def __setstate__(self, state: dict[str, Any]) -> None:
        self.__dict__ = state

    # ── Generic helper ──

    def _get_or_fetch(
        self,
        cache: BoundedCache[BlockNumber, _T],
        block_number: BlockNumber,
        provider_method_name: str,
        *args: object,
    ) -> _T:
        """Try cache → call provider → store → return.

        Parameters
        ----------
        cache : BoundedCache[BlockNumber, T]
            The per-block cache to check.
        block_number : BlockNumber
            Block number key.
        provider_method_name : str
            Method name on `self._data_provider` to call on miss.
        *args : object
            Additional positional arguments passed to the provider method.

 Returns
        -------
        _T
            Cached or freshly fetched value.

        Raises
        ------
        MissingCurveData
            If the data provider is not available.
        """
        with contextlib.suppress(KeyError):
            return cache[block_number]

        if self._data_provider is None:
            provider_attr = provider_method_name
            msg = (
                f"{provider_attr} requires a data_provider. "
                f"Provide one via Bot.build_pool()."
            )
            raise MissingCurveData(self._pool_address, provider_attr, msg)

        method = getattr(self._data_provider, provider_method_name)
        result = method(*args)
        cache[block_number] = result

        return result  # type: ignore[return-value]

    # ── Block timestamp ──

    def block_timestamp(self, block_number: BlockNumber) -> int:
        """Fetch or retrieve cached block timestamp."""
        if block_number in self._block_timestamps:
            return self._block_timestamps[block_number]

        if self._data_provider is None:
            raise MissingCurveData(
                self._pool_address,
                "block_timestamp",
                "Block timestamp requires a data_provider. Provide one via Bot.build_pool().",
            )
        result = self._data_provider.block_timestamp(block_number)
        self._block_timestamps[block_number] = result
        return result

    # ── Simple cached accessors ──

    def scaled_redemption_price(self, block_number: BlockNumber) -> int:
        """Fetch or retrieve cached scaled redemption price."""
        return self._get_or_fetch(
            self._scaled_redemption_price,
            block_number,
            "redemption_price",
            block_number,
        )

    def admin_balances(self, block_number: BlockNumber) -> tuple[int, ...]:
        """Fetch or retrieve cached admin balances."""
        return self._get_or_fetch(
            self._admin_balances,
            block_number,
            "admin_balances",
            block_number,
        )

    def contract_D(self, block_number: BlockNumber) -> int:  # noqa: N802
        """Fetch or retrieve cached contract D value (crypto pools)."""
        return self._get_or_fetch(
            self._contract_D,
            block_number,
            "D",
            block_number,
        )

    def gamma(self, block_number: BlockNumber) -> int:
        """Fetch or retrieve cached gamma value (crypto pools)."""
        return self._get_or_fetch(
            self._gamma,
            block_number,
            "gamma",
            block_number,
        )

    def price_scale(self, block_number: BlockNumber) -> tuple[int, ...]:
        """Fetch or retrieve cached price scale (crypto pools)."""
        return self._get_or_fetch(
            self._price_scale,
            block_number,
            "price_scale",
            block_number,
        )

    # ── Base pool accessors (with side-effect callbacks) ──

    def get_base_cache_updated(self, block_number: BlockNumber) -> int:
        """Fetch or retrieve cached base_cache_updated block number.

        Updates `self.base_cache_updated` as a side effect.
        """
        result = self._get_or_fetch(
            self._base_cache_updated,
            block_number,
            "base_cache_updated",
            block_number,
        )
        self.base_cache_updated = result
        return result

    def get_base_virtual_price(self, block_number: BlockNumber) -> int:
        """Fetch or retrieve cached base pool virtual price."""
        return self._get_or_fetch(
            self._base_virtual_price,
            block_number,
            "base_virtual_price",
            block_number,
        )

    # ── Virtual price (with base-cache-expiry logic) ──

    def virtual_price(self, block_number: BlockNumber, base_cache_expires: int) -> int:
        """Fetch or retrieve cached virtual price.

        For metapools, uses base_cache_updated expiry logic:
        - If base cache has expired or is unset, fetches live virtual_price
        - If base cache is still valid, uses cached base_virtual_price

        Parameters
        ----------
        block_number : BlockNumber
            Block number for cache lookup.
        base_cache_expires : int
            Number of seconds after which the base cache is considered expired.

        Returns
        -------
        int
            Virtual price value.
        """
        with contextlib.suppress(KeyError):
            return self._virtual_price[block_number]

        # Determine virtual price from base pool cache expiry
        base_virtual_price: int
        if (
            self.base_cache_updated is None
            or self._block_timestamps.get(block_number, 0)
            > self.base_cache_updated + base_cache_expires
        ):
            # Cache is not set or has expired — fetch live virtual price
            if self._data_provider is None:
                raise MissingCurveData(
                    self._pool_address,
                    "virtual_price",
                    "Virtual price requires a data_provider. Provide one via Bot.build_pool().",
                )
            base_virtual_price = self._data_provider.virtual_price(block_number)
        else:
            # Cache is still valid — use the cached base_virtual_price
            base_virtual_price = self.base_virtual_price

        self._virtual_price[block_number] = base_virtual_price
        self.base_virtual_price = base_virtual_price
        return base_virtual_price
