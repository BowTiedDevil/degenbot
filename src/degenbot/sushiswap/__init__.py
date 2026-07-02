"""SushiSwap V3 pool type registration and exports.

Deployment data (chain_id, factory → deployer / init_hash / variant /
dex_identity) for the SushiSwap V2/V3 factories is loaded from the
shipped ``deployments.json`` by the top-level ``degenbot`` package init via
``register_from_deployments(load_deployments())`` (ADR-005).

C8b: the hollow ``SushiswapV3Pool`` is collapsed — the canonical
``UniswapV3Pool`` is registered for the SushiSwap V3 factory (the SushiSwap
V3 fork has a byte-identical slot0/tick layout, so no ABI override is
needed). The ``sushiswap`` DB-kind variant is carried in ``deployments.json``.
"""

from . import (
    abi as abi,
)  # excluded from __all__ so it doesn't bubble back up to the top level package namespace
from .trackers import SushiswapV3PoolTracker

__all__ = ("SushiswapV3PoolTracker",)
