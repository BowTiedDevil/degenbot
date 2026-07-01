"""SushiSwap V3 pool type registration and exports.

Deployment data (chain_id, factory → deployer / init_hash / variant /
dex_identity) for the SushiSwap V2/V3 factories is loaded from the
shipped ``deployments.json`` by the top-level ``degenbot`` package init via
``register_from_deployments(load_deployments())`` (ADR-005).
"""

from . import (
    abi as abi,
)  # excluded from __all__ so it doesn't bubble back up to the top level package namespace
from .pools import SushiswapV3Pool
from .trackers import SushiswapV3PoolTracker

__all__ = (
    "SushiswapV3Pool",
    "SushiswapV3PoolTracker",
)
