"""SwapBased DEX pool type registration.

ADR-005 slice 7 step 4b: the hollow ``SwapbasedV2Pool`` subclass is deleted
— the canonical ``LiquidityPool`` is registered for the SwapBased factory,
keyed on the ``swapbased-v2`` DexIdentity preset (variant tag, reserves ABI
shape, default fees) + the ``variant="swapbased"`` override (preserves the
``swapbased_v2`` DB kind for backward compatibility).
"""

from degenbot.degenbot_rs import dex_identity
from degenbot.registry.pool_type import pool_type_registry
from degenbot.uniswap.liquidity_pool import LiquidityPool

# Register Swapbased V2 factory.
# Deployment data previously sourced from FACTORY_DEPLOYMENTS — now hardcoded
# directly; pool_type_registry is the sole source of truth.
_factory_address = "0x04C9f118d21e8B767D2e50C946f0cC9F6C367300"
_chain_id = 8453
swapbased_v2_identity = dex_identity("swapbased-v2")
assert swapbased_v2_identity is not None, "swapbased-v2 preset must resolve"
pool_type_registry.register(
    LiquidityPool,
    chain_id=_chain_id,
    factory_address=_factory_address,
    pool_init_hash="0xb64118b4e99d4a4163453838112a1695032df46c09f7f09064d4777d2767f8ea",
    deployer=None,
    variant="swapbased",
    dex_identity=swapbased_v2_identity,
)
