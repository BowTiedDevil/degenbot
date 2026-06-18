"""Camelot V2 liquidity pools and trackers.

ADR-005 slice 7 step 4b: the hollow ``CamelotLiquidityPool`` subclass is
deleted — the canonical ``LiquidityPool`` is registered for the Camelot
factory, keyed on the ``camelot-v2-volatile`` DexIdentity preset (variant
tag, reserves ABI shape, default fees) + the ``variant="camelot"`` override
(preserves the ``camelot_v2`` DB kind). Camelot's solidly-stable calc +
stable ``to_hop_state`` branch were folded into ``LiquidityPool`` in step 4a.

The builder (``V2PoolBuilder.build``, slice 7 step 4b) resolves the per-pool
``stableSwap()`` flag on-chain + switches to the ``camelot-v2-stable`` preset
when stable.
"""

import eth_typing

from degenbot.degenbot_rs import dex_identity
from degenbot.registry.pool_type import pool_type_registry
from degenbot.uniswap.liquidity_pool import LiquidityPool

from . import (
    abi as abi,
)  # excluded from __all__ so it doesn't bubble back up to the top level package namespace

# Register with the unified pool type registry.
# Camelot factories host both volatile + stable pools; the factory-default
# registration carries the volatile preset (the common case). The builder
# resolves the per-pool stable flag on-chain (``stableSwap()``) + switches to
# the ``camelot-v2-stable`` preset when stable.
_factory_address = "0x6EcCab422D763aC031210895C81787E87B43A652"
_chain_id = eth_typing.ChainId.ARB1
camelot_v2_identity = dex_identity("camelot-v2-volatile")
assert camelot_v2_identity is not None, "camelot-v2-volatile preset must resolve"
pool_type_registry.register(
    LiquidityPool,
    chain_id=_chain_id,
    factory_address=_factory_address,
    pool_init_hash="0xa856464ae65f7619087bc369daaf7e387dae1e5af69cfa7935850ebf754b04c1",
    deployer=None,
    variant="camelot",
    dex_identity=camelot_v2_identity,
)
