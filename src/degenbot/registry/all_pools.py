from typing import TYPE_CHECKING, Self

from hexbytes import HexBytes

from degenbot.cache import get_checksum_address

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

from degenbot.exceptions import DegenbotValueError, RegistryAlreadyInitialized
from degenbot.types import AbstractLiquidityPool, AbstractRegistry

PoolId = bytes | str
Address = bytes | str


class _UniswapV4PoolManagerRegistry(AbstractRegistry):
    """
    The Uniswap V4 singleton design breaks the fundamental assumption of the PoolRegistry: that each
    liquidity pool can be uniquely identified by a chain ID and contract address. This private class
    is used to represent Uniswap V4 PoolManager singleton contracts. `PoolRegistry` and similar high
    level registries may defer to this class to track V4 pools by their pool ID and PoolManager
    address.
    """

    def __init__(self) -> None:
        self._all_v4_pools: dict[
            tuple[
                int,  # Chain ID
                ChecksumAddress,  # PoolManager contract address
                HexBytes,  # Pool id
            ],
            AbstractLiquidityPool,
        ] = {}

    def get(
        self,
        chain_id: int,
        pool_manager_address: Address,
        pool_id: PoolId,
    ) -> AbstractLiquidityPool | None:
        return self._all_v4_pools.get(
            (
                chain_id,
                get_checksum_address(pool_manager_address),
                HexBytes(pool_id),
            )
        )

    def add(
        self,
        pool: AbstractLiquidityPool,
        chain_id: int,
        pool_manager_address: Address,
        pool_id: PoolId,
    ) -> None:
        _pool_manager_address = get_checksum_address(pool_manager_address)
        _pool_id = HexBytes(pool_id)

        if self.get(
            chain_id=chain_id,
            pool_manager_address=_pool_manager_address,
            pool_id=_pool_id,
        ):
            raise DegenbotValueError(message="Pool is already registered")

        self._all_v4_pools[
            (
                chain_id,
                _pool_manager_address,
                _pool_id,
            )
        ] = pool

    def remove(
        self,
        pool_manager_address: Address,
        chain_id: int,
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


class PoolRegistry(AbstractRegistry):
    instance: Self | None = None

    @classmethod
    def get_instance(cls) -> Self | None:
        return cls.instance

    def __init__(self) -> None:
        if type(self).instance is not None:
            raise RegistryAlreadyInitialized(
                message="A registry has already been initialized. Access it using the pool_registry.get_instance() class method"  # noqa:E501
            )
        type(self).instance = self

        self._all_pools: dict[
            tuple[
                int,  # chain ID
                ChecksumAddress,  # pool address
            ],
            AbstractLiquidityPool,
        ] = {}
        self._v4_pool_registry = _UniswapV4PoolManagerRegistry()

    def get(
        self,
        chain_id: int,
        pool_address: Address,
        pool_id: PoolId | None = None,
    ) -> AbstractLiquidityPool | None:
        if pool_id is not None:
            return self._v4_pool_registry.get(
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
        pool: AbstractLiquidityPool,
        chain_id: int,
        pool_address: Address,
        pool_id: PoolId | None = None,
    ) -> None:
        if pool_id is not None:
            self._v4_pool_registry.add(
                pool,
                chain_id=chain_id,
                pool_manager_address=get_checksum_address(pool_address),
                pool_id=pool_id,
            )
        elif self.get(
            chain_id=chain_id,
            pool_address=get_checksum_address(pool_address),
        ):
            raise DegenbotValueError(message="Pool is already registered")

        self._all_pools[(chain_id, get_checksum_address(pool_address))] = pool

    def remove(
        self,
        chain_id: int,
        pool_address: Address,
        pool_id: PoolId | None = None,
    ) -> None:
        if pool_id is not None:
            self._v4_pool_registry.get(
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


pool_registry = PoolRegistry()
