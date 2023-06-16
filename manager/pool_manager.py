from typing import Dict
from degenbot.types import PoolHelper

_all_pools: Dict[
    int,
    Dict[str, PoolHelper],
] = {}


class AllPools:
    def __init__(self, chain_id):
        try:
            _all_pools[chain_id]
        except KeyError:
            _all_pools[chain_id] = {}
        finally:
            self.pools = _all_pools[chain_id]

    def __delitem__(self, pool_address: str):
        del self.pools[pool_address]

    def __getitem__(self, pool_address: str):
        return self.pools[pool_address]

    def __setitem__(
        self,
        pool_address: str,
        pool_helper: PoolHelper,
    ):
        self.pools[pool_address] = pool_helper

    def __len__(self):
        return len(self.pools)

    def get(self, pool_address: str):
        try:
            return self.pools[pool_address]
        except KeyError:
            return None
