"""Arbitrage path construction, swap encoding, and solver integration."""

import importlib

from .encoding import (
    ApprovalStrategy,
    EncodedCall,
    FlatComposer,
    NoApprovals,
    PayloadComposer,
    generate_payloads,
)
from .types import ArbitrageCalculationResult, V4PoolKey

# Engine-facing orchestrator + hop descriptors (Plan 102): lazily imported so
# `import degenbot` (which eagerly pulls in `.arbitrage` before `Bot` is
# defined) doesn't trigger their top-level `from degenbot import Bot` — and so
# the heavy snapshot/builder modules they pull in aren't loaded for callers
# that only want `ApprovalStrategy` etc. Resolved on first attribute access
# via `__getattr__` below.
_LAZY = {
    "EngineRegistry": ".engine_registry",
    "HopInfo": ".hop_info",
    "NoOpPathPredicate": ".policy",
    "PathCompositionPredicate": ".policy",
    "PathInfo": ".hop_info",
    "PathPolicy": ".policy",
    "V2HopInfo": ".hop_info",
    "V3HopInfo": ".hop_info",
    "V4HopInfo": ".hop_info",
    "build_hops_from_pools": ".hop_info",
    "touched_tokens": ".policy",
}


def __getattr__(name: str) -> object:
    if name in _LAZY:
        module = importlib.import_module(_LAZY[name], package=__name__)
        return getattr(module, name)
    msg = f"module {__name__!r} has no attribute {name!r}"
    raise AttributeError(msg)


__all__ = (
    "ApprovalStrategy",
    "ArbitrageCalculationResult",
    "EncodedCall",
    "EngineRegistry",  # noqa: F822
    "FlatComposer",
    "HopInfo",  # noqa: F822
    "NoApprovals",
    "NoOpPathPredicate",  # noqa: F822
    "PathCompositionPredicate",  # noqa: F822
    "PathInfo",  # noqa: F822
    "PathPolicy",  # noqa: F822
    "PayloadComposer",
    "V2HopInfo",  # noqa: F822
    "V3HopInfo",  # noqa: F822
    "V4HopInfo",  # noqa: F822
    "V4PoolKey",
    "build_hops_from_pools",  # noqa: F822
    "generate_payloads",
    "touched_tokens",  # noqa: F822
)
