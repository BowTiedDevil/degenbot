"""Aerodrome V2/V3 liquidity pools and trackers."""

from . import (
    abi as abi,
)  # excluded from __all__ so it doesn't bubble back up to the top level package namespace
from .pools import AerodromeV2Pool, AerodromeV3Pool
from .trackers import AerodromeV2PoolTracker, AerodromeV3PoolTracker
from .types import AerodromeV2PoolState, AerodromeV3PoolState

# Deployment data (chain_id, factory → deployer / init_hash / variant /
# dex_identity) for the Aerodrome V2/V3 factories is loaded from the shipped
# deployments.json by the top-level degenbot package init via
# register_from_deployments(load_deployments()) (ADR-005).

__all__ = (
    "AerodromeV2Pool",
    "AerodromeV2PoolState",
    "AerodromeV2PoolTracker",
    "AerodromeV3Pool",
    "AerodromeV3PoolState",
    "AerodromeV3PoolTracker",
)
