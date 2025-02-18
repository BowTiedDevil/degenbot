import contextlib
import sys
from typing import TYPE_CHECKING

from eth_utils.address import to_checksum_address

if sys.version_info >= (3, 11):
    from typing import Self
else:
    from typing_extensions import Self


from degenbot.exceptions import DegenbotValueError, RegistryAlreadyInitialized
from degenbot.types import AbstractLiquidityPool, AbstractRegistry

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress


class PoolRegistry(AbstractRegistry):
    instance: Self | None = None

    @classmethod
    def get_instance(cls) -> Self | None:
        return cls.instance

    def __init__(self) -> None:
        if self.__class__.instance is not None:
            raise RegistryAlreadyInitialized(
                message="A registry has already been initialized. Access it using the get_instance() class method"  # noqa:E501
            )
        self.__class__.instance = self

        self._all_pools: dict[
            tuple[
                int,  # chain ID
                ChecksumAddress,  # pool address
            ],
            AbstractLiquidityPool,
        ] = {}

    def get(self, pool_address: str, chain_id: int) -> AbstractLiquidityPool | None:
        return self._all_pools.get(
            (chain_id, to_checksum_address(pool_address)),
        )

    def add(self, pool_address: str, chain_id: int, pool: AbstractLiquidityPool) -> None:
        pool_address = to_checksum_address(pool_address)
        if self.get(pool_address=pool_address, chain_id=chain_id):
            raise DegenbotValueError(message="Pool is already registered")
        self._all_pools[(chain_id, pool_address)] = pool

    def remove(self, pool_address: str, chain_id: int) -> None:
        pool_address = to_checksum_address(pool_address)

        with contextlib.suppress(KeyError):
            del self._all_pools[(chain_id, pool_address)]


pool_registry = PoolRegistry()
