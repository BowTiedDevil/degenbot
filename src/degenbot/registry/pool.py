from typing import TYPE_CHECKING

from hexbytes import HexBytes

import degenbot.exceptions
from degenbot.checksum_cache import get_checksum_address
from degenbot.types.abstract import AbstractLiquidityPool, AbstractRegistry
from degenbot.types.aliases import ChainId

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress


PoolId = bytes | str
Address = bytes | str


class ManagedPoolRegistry(AbstractRegistry):
    """
    Registry for Uniswap V4 pools, which are identified by
    ``(chain_id, pool_manager_address, pool_id)`` rather than a simple address.
    """


    def __init__(self) -> None:
        self._all_v4_pools: dict[
            tuple[
                ChainId,
                ChecksumAddress,  # PoolManager contract address
                HexBytes,  # Pool id
            ],
            AbstractLiquidityPool,
        ] = {}

    def get(
        self,
        chain_id: ChainId,
        pool_manager_address: Address,
        pool_id: PoolId,
    ) -> "AbstractLiquidityPool | None":
        return self._all_v4_pools.get((
            chain_id,
            get_checksum_address(pool_manager_address),
            HexBytes(pool_id),
        ))

    def add(
        self,
        pool: "AbstractLiquidityPool",
        chain_id: ChainId,
        pool_manager_address: Address,
        pool_id: PoolId,
    ) -> None:
        pool_manager_address = get_checksum_address(pool_manager_address)
        pool_id = HexBytes(pool_id)

        if self.get(
            chain_id=chain_id,
            pool_manager_address=pool_manager_address,
            pool_id=pool_id,
        ):
            raise degenbot.exceptions.DegenbotValueError(message="Pool is already registered")

        self._all_v4_pools[
            chain_id,
            pool_manager_address,
            pool_id,
        ] = pool

    def remove(
        self,
        pool_manager_address: Address,
        chain_id: ChainId,
        pool_id: PoolId,
    ) -> None:
        self._all_v4_pools.pop(
            (
                chain_id,
                get_checksum_address(pool_manager_address),
                HexBytes(pool_id),
            ),
            None,
        )

    def _reset(self) -> None:
        self._all_v4_pools.clear()


class PoolRegistry(AbstractRegistry):
    def __init__(
        self,
        managed_pool_registry: ManagedPoolRegistry | None = None,
    ) -> None:
        self._all_pools: dict[
            tuple[
                ChainId,
                ChecksumAddress,  # pool address
            ],
            AbstractLiquidityPool,
        ] = {}
        self._managed_pool_registry = managed_pool_registry or ManagedPoolRegistry()

    def _reset(self) -> None:
        self._all_pools.clear()
        self._managed_pool_registry._reset()

    def get(
        self,
        chain_id: ChainId,
        pool_address: Address,
        pool_id: PoolId | None = None,
    ) -> "AbstractLiquidityPool | None":
        if pool_id is not None:
            return self._managed_pool_registry.get(
                chain_id=chain_id,
                pool_manager_address=get_checksum_address(pool_address),
                pool_id=pool_id,
            )

        return self._all_pools.get(
            (
                chain_id,
                get_checksum_address(pool_address),
            ),
        )

    def add(
        self,
        pool: "AbstractLiquidityPool",
        chain_id: ChainId,
        pool_address: Address,
        pool_id: PoolId | None = None,
    ) -> None:
        if pool_id is not None:
            self._managed_pool_registry.add(
                pool,
                chain_id=chain_id,
                pool_manager_address=get_checksum_address(pool_address),
                pool_id=pool_id,
            )
        elif self.get(
            chain_id=chain_id,
            pool_address=get_checksum_address(pool_address),
        ):
            raise degenbot.exceptions.DegenbotValueError(message="Pool is already registered")

        self._all_pools[chain_id, get_checksum_address(pool_address)] = pool

    def remove(
        self,
        chain_id: ChainId,
        pool_address: Address,
        pool_id: PoolId | None = None,
    ) -> None:
        if pool_id is not None:
            self._managed_pool_registry.remove(
                chain_id=chain_id,
                pool_manager_address=get_checksum_address(pool_address),
                pool_id=pool_id,
            )
        else:
            self._all_pools.pop(
                (
                    chain_id,
                    get_checksum_address(pool_address),
                ),
                None,
            )
