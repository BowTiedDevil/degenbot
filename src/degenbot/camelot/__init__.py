"""Camelot V2 liquidity pools and trackers."""

from . import (
    abi as abi,
)  # excluded from __all__ so it doesn't bubble back up to the top level package namespace
from .pools import CamelotLiquidityPool

__all__ = ("CamelotLiquidityPool",)

# Register with the unified pool type registry
import eth_typing

from degenbot.degenbot_rs import dex_identity
from degenbot.registry.pool_type import pool_type_registry

_factory_address = "0x6EcCab422D763aC031210895C81787E87B43A652"
_chain_id = eth_typing.ChainId.ARB1
# Camelot factories host both volatile + stable pools; the factory-default
# registration carries the volatile preset (the common case). The builder
# resolves the per-pool stable flag on-chain (``stableSwap()``) and — in a
# follow-up step — switches to the ``camelot-v2-stable`` preset when stable.
camelot_v2_identity = dex_identity("camelot-v2-volatile")
assert camelot_v2_identity is not None, "camelot-v2-volatile preset must resolve"
pool_type_registry.register(
    CamelotLiquidityPool,
    chain_id=_chain_id,
    factory_address=_factory_address,
    pool_init_hash="0xa856464ae65f7619087bc369daaf7e387dae1e5af69cfa7935850ebf754b04c1",
    deployer=None,
    dex_identity=camelot_v2_identity,
)
