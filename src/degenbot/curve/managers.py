from __future__ import annotations

import contextlib
from threading import Lock
from typing import TYPE_CHECKING

from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.exceptions.manager import PoolCreationFailed, PoolNotAssociated
from degenbot.types.abstract import AbstractPoolManager

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.bot import Bot
    from degenbot.types.aliases import ChainId


class CurveStableswapPoolManager(
    AbstractPoolManager["CurveStableswapPool"],
    pool_factory=None,  # type: ignore[assignment]
):
    """
    Manages Curve StableSwap pool instances.

    Tracks Curve pools by address, delegates construction to Bot.build_curve_pool(),
    and supports registry-based discovery.
    """

    def __init__(
        self,
        *,
        bot: Bot,
        chain_id: ChainId | None = None,
    ) -> None:
        self._bot = bot
        self._chain_id = chain_id or bot.connections.default_chain_id
        self._tracked_pools: dict[ChecksumAddress, CurveStableswapPool] = {}
        self._untracked_pools: set[ChecksumAddress] = set()
        self._lock = Lock()

    def get_pool(
        self,
        pool_address: ChecksumAddress | str,
        *,
        silent: bool = False,
    ) -> CurveStableswapPool:
        """
        Get a Curve pool from its address.

        If the pool is already tracked, that instance is returned.
        If the pool is in the bot's pool registry, it is tracked and returned.
        Otherwise, a new pool is built via Bot.build_curve_pool().
        """

        pool_address = get_checksum_address(pool_address)

        with contextlib.suppress(KeyError):
            return self._tracked_pools[pool_address]

        if pool_address in self._untracked_pools:
            raise PoolNotAssociated(pool_address)

        # Check if the pool registry already has this pool
        pool_from_registry = self._bot.pools.get(
            pool_address=pool_address,
            chain_id=self._chain_id,
        )

        if pool_from_registry is not None:
            if isinstance(pool_from_registry, CurveStableswapPool):
                self._add_tracked_pool(pool_from_registry)
                return pool_from_registry
            self._untracked_pools.add(pool_address)
            raise PoolNotAssociated(pool_address)

        try:
            new_pool = self._bot.build_curve_pool(
                address=pool_address,
                chain_id=self._chain_id,
                silent=silent,
            )
        except Exception as exc:
            raise PoolCreationFailed(
                message=f"Could not build Curve pool {pool_address}: {exc}"
            ) from exc
        else:
            self._add_tracked_pool(new_pool)
            return new_pool

    def get_pools_for_token(
        self,
        token_address: ChecksumAddress | str,
    ) -> list[CurveStableswapPool]:
        """Return all tracked pools that contain the given token."""
        token_address = get_checksum_address(token_address)
        return [
            pool
            for pool in self._tracked_pools.values()
            if any(
                token.address == token_address
                for token in pool.tokens
            )
        ]

    def __repr__(self) -> str:  # pragma: no cover
        return f"{self.__class__.__name__}(chain_id={self._chain_id}, pools={len(self._tracked_pools)})"
