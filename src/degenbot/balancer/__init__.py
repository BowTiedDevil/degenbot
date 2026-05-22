"""Balancer V2 weighted and stable pools with swap encoding."""

from degenbot.balancer.pools import BalancerV2Pool
from degenbot.balancer.stable_pools import INVARIANT_V1, INVARIANT_V2, BalancerV2StablePool
from degenbot.balancer.swap_amounts import BalancerV2SwapAmounts
from degenbot.balancer.types import (
    BalancerV2PoolStateUpdated,
    BalancerV2StablePoolExternalUpdate,
    BalancerV2WeightedPoolExternalUpdate,
)
from degenbot.registry.pool_type import pool_type_registry
from degenbot.types.pool_type import PoolFamily

__all__ = [
    "INVARIANT_V1",
    "INVARIANT_V2",
    "BalancerV2Pool",
    "BalancerV2PoolStateUpdated",
    "BalancerV2StablePool",
    "BalancerV2StablePoolExternalUpdate",
    "BalancerV2SwapAmounts",
    "BalancerV2WeightedPoolExternalUpdate",
]

# Self-register Balancer V2 factory addresses in the pool type registry.
# These registrations enable Bot.build_pool() to automatically resolve
# Balancer pools from factory addresses.

# Balancer V2 Weighted Pool Factory (v3) on Ethereum mainnet
pool_type_registry.register(
    BalancerV2Pool,
    chain_id=1,
    factory_address="0x8e9AA87E45e92BAD7D5F7F9dd794cEa12f21707B",
    family=PoolFamily.WEIGHTED,  # Override: _derive_family returns STABLESWAP
)

# Balancer V2 Stable Pool Factory (v1) on Ethereum mainnet
pool_type_registry.register(
    BalancerV2StablePool,
    chain_id=1,
    factory_address="0x8519f5a4A85678e0e03395586e2e223D70E9e09B",
    family=PoolFamily.STABLESWAP,
)

# ComposableStablePool Factory (v2) on Ethereum mainnet
pool_type_registry.register(
    BalancerV2StablePool,
    chain_id=1,
    factory_address="0xA8936f4824B2E6407fC0e94133909aEf7D48E876",
    family=PoolFamily.STABLESWAP,
)
