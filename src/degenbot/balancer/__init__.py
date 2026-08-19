"""Balancer V2 weighted and stable pools with swap encoding."""

from degenbot.balancer.pools import BalancerV2Pool
from degenbot.balancer.stable_pools import INVARIANT_V1, INVARIANT_V2, BalancerV2StablePool
from degenbot.balancer.types import (
    BalancerV2StablePoolExternalUpdate,
    BalancerV2WeightedPoolExternalUpdate,
)

__all__ = [
    "INVARIANT_V1",
    "INVARIANT_V2",
    "BalancerV2Pool",
    "BalancerV2StablePool",
    "BalancerV2StablePoolExternalUpdate",
    "BalancerV2WeightedPoolExternalUpdate",
]

# Self-register Balancer V2 factory addresses in the pool type registry.
# These registrations enable Bot.build_pool() to automatically resolve
# Balancer pools from factory addresses.
#
# Deployment data (chain_id, factory → deployer / init_hash / family) is
# loaded from the shipped deployments.json by the top-level degenbot package
# init via register_from_deployments(load_deployments()) (ADR-005). The
# Balancer factories require a `family` override (their classes lack
# `fee_token0`, so _derive_family would misclassify) — carried as a JSON
# field rather than inline here.
