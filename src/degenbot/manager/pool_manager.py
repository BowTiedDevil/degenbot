from typing import Dict, Optional, Union
from warnings import warn

from eth_typing import ChecksumAddress
from eth_utils.address import to_checksum_address

from ..baseclasses import PoolHelper

# Internal state dictionary that maintains a keyed dictionary of all
# pool helper objects. The top level dict is keyed by chain ID, and
# sub-dicts are keyed by the checksummed pool address.
_all_pools: Dict[
    int,
    Dict[ChecksumAddress, PoolHelper],
] = {}


class AllPools:
    def __init__(self, chain_id):
        try:
            _all_pools[chain_id]
        except KeyError:
            _all_pools[chain_id] = {}
        finally:
            self.pools = _all_pools[chain_id]

    def __delitem__(self, pool: Union[PoolHelper, ChecksumAddress, str]):
        if isinstance(pool, PoolHelper):
            _pool_address = pool.address
        else:
            _pool_address = to_checksum_address(pool)

        del self.pools[_pool_address]

    def __getitem__(self, pool_address: Union[ChecksumAddress, str]):
        _pool_address = to_checksum_address(pool_address)
        return self.pools[_pool_address]

    def __setitem__(
        self,
        pool_address: Union[ChecksumAddress, str],
        pool_helper: PoolHelper,
    ):
        _pool_address = to_checksum_address(pool_address)

        if self.pools.get(_pool_address):
            warn(
                f"A pool helper with address {_pool_address} already exists! It has been overwritten."
            )
        self.pools[_pool_address] = pool_helper

    def __len__(self):
        return len(self.pools)

    def get(self, pool_address: Union[ChecksumAddress, str]) -> Optional[PoolHelper]:
        _pool_address = to_checksum_address(pool_address)
        return self.pools.get(_pool_address)
