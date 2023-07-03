from typing import Dict, Union

from eth_typing import ChecksumAddress
from web3 import Web3

from degenbot.types import PoolHelper

# Internal state dictionary that maintains a keyed dictionary of all
# pool helper objects. The top level dict is keyed by chain ID, and
# sub-dicts are keyed by the checksummed pool address.
_all_pools: Dict[
    int,
    Dict[ChecksumAddress, PoolHelper],
] = {}


def to_checksum_address(address: Union[ChecksumAddress, str]):
    """
    Return the checksummed version of an address.
    """
    if type(address) == ChecksumAddress:
        return address
    elif isinstance(address, str):
        return Web3.toChecksumAddress(address)
    else:
        raise ValueError(
            f"Unexpected input. Expected str or ChecksumAddress, got {type(address)}"
        )


class AllPools:
    def __init__(self, chain_id):
        try:
            _all_pools[chain_id]
        except KeyError:
            _all_pools[chain_id] = {}
        finally:
            self.pools = _all_pools[chain_id]

    def __delitem__(self, pool_address: Union[ChecksumAddress, str]):
        _pool_address = to_checksum_address(pool_address)
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
        self.pools[_pool_address] = pool_helper

    def __len__(self):
        return len(self.pools)

    def get(self, pool_address: Union[ChecksumAddress, str]):
        _pool_address = to_checksum_address(pool_address)
        try:
            return self.pools[_pool_address]
        except KeyError:
            return None
