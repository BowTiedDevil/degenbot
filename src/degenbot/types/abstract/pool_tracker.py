from __future__ import annotations

from threading import Lock
from typing import TYPE_CHECKING, Any

from eth_typing import ChecksumAddress

from degenbot.checksum_cache import get_checksum_address
from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool
from degenbot.types.aliases import ChainId

if TYPE_CHECKING:
    from degenbot.bot import Bot


class AbstractPoolTracker[Pool: AbstractLiquidityPool]:
    """
    Base class for liquidity trackers.
    """

    # Class variables
    pool_factory: type[Pool]  # this class attribute is set by __init_subclass__ at import time

    # Instance variables
    _bot: Bot
    _chain_id: ChainId
    _deployer_address: ChecksumAddress
    _factory_address: ChecksumAddress
    _tracked_pools: dict[ChecksumAddress, Pool]
    _untracked_pools: set[ChecksumAddress]
    _lock: Lock

    def __init_subclass__(cls, *, pool_factory: type[Pool] | None = None, **kwargs: Any) -> None:
        if pool_factory is not None:
            cls.pool_factory = pool_factory
        super().__init_subclass__(**kwargs)

    def _add_tracked_pool(self, pool_helper: Pool) -> None:
        with self._lock:
            self._tracked_pools[pool_helper.address] = pool_helper

    def remove(self, pool_address: ChecksumAddress | str) -> None:
        pool_address = get_checksum_address(pool_address)
        self._tracked_pools.pop(pool_address, None)
        self._untracked_pools.discard(pool_address)

    @property
    def chain_id(self) -> ChainId:
        return self._chain_id

    @property
    def deployer_address(self) -> ChecksumAddress:
        return self._deployer_address

    @property
    def factory_address(self) -> ChecksumAddress:
        return self._factory_address
