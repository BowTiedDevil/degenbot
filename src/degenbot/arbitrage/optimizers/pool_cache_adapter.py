"""
Auto-registration adapter that syncs pool state to the Rust solver cache.

When a pool's state changes, the adapter receives the notification and
updates the solver's Rust pool cache, eliminating the need for manual
cache management.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.logging import logger
from degenbot.types.concrete import AbstractPublisherMessage, Publisher, Subscriber
from degenbot.types.pool_protocols import CacheablePool

if TYPE_CHECKING:
    from degenbot.arbitrage.optimizers.solver import ArbSolver
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool


class ArbPoolCacheAdapter(Subscriber):
    """
    Subscribes to pool state updates and auto-registers them in the.

    ArbSolver's Rust pool cache.

    Each pool is registered in both reserve orientations (token0→token1
    and token1→token0), since the solver's cache stores direction-specific
    reserve pairs. The adapter returns the primary pool_id; the reverse
    orientation's ID is ``primary_id + 1``.

    Requires the pool to implement ``CacheablePool`` (has
    ``reserves_for_cache()`` and ``fee_for_cache()`` methods).
    """

    def __init__(self, *, solver: ArbSolver) -> None:
        """Initialize the instance."""
        self._solver = solver
        self._pool_to_ids: dict[int, tuple[int, int]] = {}  # id(pool) → (forward_id, reverse_id)

    def register(self, pool: AbstractLiquidityPool) -> int:
        """
        Register a pool for auto-updates.

        Subscribes to the pool's state notifications and registers both
        reserve orientations in the solver's cache.

        Returns the primary (forward) pool ID.
        """
        if not isinstance(pool, CacheablePool):
            msg = (
                f"Pool {pool.address} does not implement CacheablePool "
                f"(missing reserves_for_cache() or fee_for_cache()). "
                f"Cannot register in Rust solver cache."
            )
            raise TypeError(msg)

        pool.subscribe(self)

        # Extract current state to register immediately
        reserve0, reserve1 = pool.reserves_for_cache()
        fee = pool.fee_for_cache()

        # Register forward orientation
        forward_id = self._solver.register_pool(
            reserve_in=reserve0,
            reserve_out=reserve1,
            fee=fee,
        )

        # Register reverse orientation
        reverse_id = self._solver.register_pool(
            reserve_in=reserve1,
            reserve_out=reserve0,
            fee=fee,
        )

        self._pool_to_ids[id(pool)] = (forward_id, reverse_id)
        return forward_id

    def notify(
        self,
        publisher: Publisher,
        message: AbstractPublisherMessage,  # ruff: ignore[ARG002]
    ) -> None:
        """
        Handle a pool state update.

        Updates both reserve orientations in the Rust cache.
        """
        pool = publisher
        ids = self._pool_to_ids.get(id(pool))
        if ids is None:
            return  # Pool not registered with this adapter

        if not isinstance(pool, CacheablePool):
            logger.warning(
                f"Pool {id(pool)} no longer implements CacheablePool; skipping cache update."
            )
            return

        reserve0, reserve1 = pool.reserves_for_cache()
        fee = pool.fee_for_cache()
        forward_id, reverse_id = ids

        # Update both orientations
        self._solver.update_pool(forward_id, reserve0, reserve1, fee)
        self._solver.update_pool(reverse_id, reserve1, reserve0, fee)

    def get_pool_ids(self, pool: AbstractLiquidityPool) -> tuple[int, int] | None:
        """Return (forward_id, reverse_id) for a registered pool, or None."""
        return self._pool_to_ids.get(id(pool))
