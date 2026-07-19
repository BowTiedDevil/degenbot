"""Arbitrage path construction, swap encoding, and solver integration.

The Rust-backed ``ArbitrageEngine`` pyclass and the
``solve_balancer_weighted_basket`` pyfunction are re-exported here (bridged
from ``_ffi``) so consumers import ``from degenbot.arbitrage import
ArbitrageEngine`` / ``solve_balancer_weighted_basket`` rather than reaching
into ``_ffi`` (ADR-013: the Pydantic barrier — ``_ffi`` only in
``__init__.py``).

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

from degenbot._ffi import ArbitrageEngine, solve_balancer_weighted_basket

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
    "ArbitrageEngine",
    "EncodedCall",
    "FlatComposer",
    "NoApprovals",
    "PayloadComposer",
    "V4PoolKey",
    "generate_payloads",
    "solve_balancer_weighted_basket",
)
