
from eth_typing import ChecksumAddress
from eth_utils.address import to_checksum_address

from ..baseclasses import AbstractLiquidityPool
from ..logging import logger

# Internal state dictionary that maintains a keyed dictionary of all pool objects. The top level
# dict is keyed by chain ID, and sub-dicts are keyed by the checksummed pool address.
_all_pools: dict[
    int,
    dict[ChecksumAddress, AbstractLiquidityPool],
] = {}


class AllPools:
    def __init__(self, chain_id: int) -> None:
        try:
            _all_pools[chain_id]
        except KeyError:
            _all_pools[chain_id] = {}
        finally:
            self.pools = _all_pools[chain_id]

    def __contains__(self, pool: AbstractLiquidityPool | str) -> bool:
        if isinstance(pool, AbstractLiquidityPool):
            _pool_address = pool.address
        else:
            _pool_address = to_checksum_address(pool)
        return _pool_address in self.pools

    def __delitem__(self, pool: AbstractLiquidityPool | str) -> None:
        if isinstance(pool, AbstractLiquidityPool):
            _pool_address = pool.address
        else:
            _pool_address = to_checksum_address(pool)
        del self.pools[_pool_address]

    def __getitem__(self, pool_address: str) -> AbstractLiquidityPool:
        return self.pools[to_checksum_address(pool_address)]

    def __setitem__(self, pool_address: str, pool_helper: AbstractLiquidityPool) -> None:
        _pool_address = to_checksum_address(pool_address)
        if _pool_address in self.pools:  # pragma: no cover
            logger.warning(
                f"Pool with address {_pool_address} already known. It has been overwritten."
            )
        self.pools[_pool_address] = pool_helper

    def __len__(self) -> int:  # pragma: no cover
        return len(self.pools)

    def get(self, pool_address: str) -> AbstractLiquidityPool | None:
        return self.pools.get(to_checksum_address(pool_address))
