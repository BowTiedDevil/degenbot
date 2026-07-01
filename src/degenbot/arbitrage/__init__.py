"""Arbitrage path construction, swap encoding, and solver integration.

The engine-facing orchestrator (:mod:`.engine_registry`), hop descriptors
(:mod:`.hop_info`), and path policies (:mod:`.policy`) are intentionally NOT
re-exported here: they import concrete pool classes that, during
``import degenbot``, are first pulled in *before* this package finishes
initializing (pool modules import :mod:`degenbot.arbitrage.types` at load
time, creating a cycle). Import them directly from their submodules instead:

    from degenbot.arbitrage.engine_registry import EngineRegistry
    from degenbot.arbitrage.hop_info import HopInfo, PathInfo, V2HopInfo, ...
    from degenbot.arbitrage.policy import PathPolicy, ...
"""

from .encoding import (
    ApprovalStrategy,
    EncodedCall,
    FlatComposer,
    NoApprovals,
    PayloadComposer,
    generate_payloads,
)
from .types import ArbitrageCalculationResult, V4PoolKey

__all__ = (
    "ApprovalStrategy",
    "ArbitrageCalculationResult",
    "EncodedCall",
    "FlatComposer",
    "NoApprovals",
    "PayloadComposer",
    "V4PoolKey",
    "generate_payloads",
)
