from threading import Lock
from typing import Any, ClassVar, Self
from weakref import WeakValueDictionary

from eth_typing import ChecksumAddress

from degenbot.checksum_cache import get_checksum_address
from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool
from degenbot.types.aliases import ChainId


class AbstractPoolManager[Pool: AbstractLiquidityPool]:
    """
    Base class for liquidity pool managers. The class instance dict and get_instance method are
    mechanisms for implementing a singleton strategy so only one pool manager is created for a given
    DEX factory.
    """

    # Class variables
    instances: ClassVar[
        WeakValueDictionary[
            tuple[ChainId, ChecksumAddress],
            Any,
        ]
    ] = WeakValueDictionary()
    pool_factory: type[Pool]  # this class attribute is set by __init_subclass__ at import time

    # Instance variables
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

    @classmethod
    def get_instance(cls, factory_address: str, chain_id: ChainId) -> Self | None:
        return cls.instances.get((chain_id, get_checksum_address(factory_address)))

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
