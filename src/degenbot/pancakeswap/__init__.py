"""PancakeSwap V3 pool type registration and exports.

Deployment data (chain_id, factory → deployer / init_hash / variant /
dex_identity) for the PancakeSwap V2/V3 factories is loaded from the
shipped ``deployments.json`` by the top-level ``degenbot`` package init via
``register_from_deployments(load_deployments())`` (ADR-005).
"""

from .pools import PancakeswapV3Pool
from .trackers import PancakeswapV3PoolTracker

__all__ = (
    "PancakeswapV3Pool",
    "PancakeswapV3PoolTracker",
)
