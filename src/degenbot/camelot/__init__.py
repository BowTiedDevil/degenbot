"""Camelot V2 liquidity pools and trackers.

ADR-005 slice 7 step 4b: the hollow ``CamelotLiquidityPool`` subclass is
deleted — the canonical ``UniswapV2Pool`` is registered for the Camelot
factory, keyed on the ``camelot-v2-volatile`` DexIdentity preset (variant
tag, reserves ABI shape, default fees) + the ``variant="camelot"`` override
(preserves the ``camelot_v2`` DB kind). Camelot's solidly-stable calc was
folded into ``UniswapV2Pool`` in step 4a.

The builder (``V2PoolBuilder.build``, slice 7 step 4b) resolves the per-pool
``stableSwap()`` flag on-chain + switches to the ``camelot-v2-stable`` preset
when stable.

Deployment data (chain_id, factory → deployer / init_hash / variant /
dex_identity) is loaded from the shipped ``deployments.json`` by the
top-level ``degenbot`` package init via
``register_from_deployments(load_deployments())`` (ADR-005).
"""

from . import (
    abi as abi,
)  # excluded from __all__ so it doesn't bubble back up to the top level package namespace
