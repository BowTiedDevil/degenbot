"""Legacy arbitrage cycle classes.

These classes are deprecated and will be removed in a future release.
Refer to the migration guide at docs/migration-guides/legacy-cycles-to-arbitrage-path.md
for transitioning to ArbitragePath + ArbSolver.
"""

import warnings

from degenbot.arbitrage._legacy._uniswap_2pool_cycle_testing import _UniswapTwoPoolCycleTesting
from degenbot.arbitrage._legacy._uniswap_curve_cycle import _UniswapCurveCycle
from degenbot.arbitrage._legacy._uniswap_lp_cycle import _UniswapLpCycle
from degenbot.arbitrage._legacy._uniswap_multipool_cycle_testing import _UniswapMultiPoolCycleTesting

_DEPRECATED_CLASS_NAMES = {
    "UniswapLpCycle": _UniswapLpCycle,
    "UniswapCurveCycle": _UniswapCurveCycle,
}


def __getattr__(name: str) -> object:
    if name in _DEPRECATED_CLASS_NAMES:
        warnings.warn(
            f"{name} is deprecated. Use ArbitragePath + ArbSolver instead. "
            "See docs/migration-guides/legacy-cycles-to-arbitrage-path.md",
            DeprecationWarning,
            stacklevel=2,
        )
        return _DEPRECATED_CLASS_NAMES[name]
    msg = f"module {__name__!r} has no attribute {name!r}"
    raise AttributeError(msg)


__all__ = [
    "_UniswapCurveCycle",
    "_UniswapLpCycle",
    "_UniswapMultiPoolCycleTesting",
    "_UniswapTwoPoolCycleTesting",
]
