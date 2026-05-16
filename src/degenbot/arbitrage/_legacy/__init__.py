"""Legacy arbitrage cycle classes.

These classes are deprecated and will be removed in a future release.
Refer to the migration guide at docs/migration-guides/legacy-cycles-to-arbitrage-path.md
for transitioning to ArbitragePath + ArbSolver.
"""

import importlib
import warnings

_DEPRECATED_CLASS_NAMES: dict[str, str] = {
    "UniswapLpCycle": ("degenbot.arbitrage._legacy._uniswap_lp_cycle:_UniswapLpCycle"),
    "UniswapCurveCycle": ("degenbot.arbitrage._legacy._uniswap_curve_cycle:_UniswapCurveCycle"),
    "_UniswapMultiPoolCycleTesting": (
        "degenbot.arbitrage._legacy._uniswap_multipool_cycle_testing:_UniswapMultiPoolCycleTesting"
    ),
    "_UniswapTwoPoolCycleTesting": (
        "degenbot.arbitrage._legacy._uniswap_2pool_cycle_testing:_UniswapTwoPoolCycleTesting"
    ),
    "_UniswapLpCycle": ("degenbot.arbitrage._legacy._uniswap_lp_cycle:_UniswapLpCycle"),
    "_UniswapCurveCycle": ("degenbot.arbitrage._legacy._uniswap_curve_cycle:_UniswapCurveCycle"),
}

_DEPRECATE_WARN_NAMES = {"UniswapLpCycle", "UniswapCurveCycle"}


def __getattr__(name: str) -> object:
    if name in _DEPRECATED_CLASS_NAMES:
        if name in _DEPRECATE_WARN_NAMES:
            warnings.warn(
                f"{name} is deprecated. Use ArbitragePath + ArbSolver instead. "
                "See docs/migration-guides/legacy-cycles-to-arbitrage-path.md",
                DeprecationWarning,
                stacklevel=2,
            )
        module_path, attr = _DEPRECATED_CLASS_NAMES[name].rsplit(":", 1)
        return getattr(importlib.import_module(module_path), attr)
    msg = f"module {__name__!r} has no attribute {name!r}"
    raise AttributeError(msg)
