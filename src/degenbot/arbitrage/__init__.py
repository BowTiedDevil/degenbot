import importlib
import warnings

from .encoding import (
    ApprovalStrategy,
    EncodedCall,
    FlatComposer,
    NoApprovals,
    PayloadComposer,
    generate_payloads,
)
from .types import ArbitrageCalculationResult, V4PoolKey

_DEPRECATED_NAMES = {
    "UniswapLpCycle": "degenbot.arbitrage._legacy._uniswap_lp_cycle:_UniswapLpCycle",
    "UniswapCurveCycle": "degenbot.arbitrage._legacy._uniswap_curve_cycle:_UniswapCurveCycle",
}


def __getattr__(name: str) -> object:
    if name in _DEPRECATED_NAMES:
        warnings.warn(
            f"{name} is deprecated. Use ArbitragePath + ArbSolver instead. "
            "See docs/migration-guides/legacy-cycles-to-arbitrage-path.md",
            DeprecationWarning,
            stacklevel=2,
        )
        module_path, attr = _DEPRECATED_NAMES[name].rsplit(":", 1)
        return getattr(importlib.import_module(module_path), attr)
    msg = f"module {__name__!r} has no attribute {name!r}"
    raise AttributeError(msg)


__all__ = (
    "ApprovalStrategy",
    "ArbitrageCalculationResult",
    "EncodedCall",
    "FlatComposer",
    "NoApprovals",
    "PayloadComposer",
    "UniswapCurveCycle",  # noqa: F822
    "UniswapLpCycle",  # noqa: F822
    "V4PoolKey",
    "generate_payloads",
)
