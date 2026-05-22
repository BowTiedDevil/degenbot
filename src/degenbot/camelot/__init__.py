"""Camelot V2 liquidity pools and trackers."""

from . import (
    abi as abi,
)  # excluded from __all__ so it doesn't bubble back up to the top level package namespace
from .pools import CamelotLiquidityPool

__all__ = ("CamelotLiquidityPool",)

# Register with the unified pool type registry
import eth_typing

from degenbot.registry.pool_type import pool_type_registry

_factory_address = "0x6EcCab422D763aC031210895C81787E87B43A652"
_chain_id = eth_typing.ChainId.ARB1
pool_type_registry.register(
    CamelotLiquidityPool,
    chain_id=_chain_id,
    factory_address=_factory_address,
    pool_init_hash="0xa856464ae65f7619087bc369daaf7e387dae1e5af69cfa7935850ebf754b04c1",
    deployer=None,
)
