"""Shared state surface for concentrated-liquidity pools.

``V3PoolState`` and ``V4PoolState`` were copy-paste twins for the reads Rust
owns generically (survey C4): the sparse-liquidity-map probe lives here once,
reading the typed ``PoolTickCoverage`` enum off the concentrated liquidity
handle (survey C7).
"""

from __future__ import annotations

from typing import Any, cast

from degenbot.uniswap import PoolTickCoverage


class ConcentratedLiquidityPoolState:
    """State surface shared by every V3/V4 concentrated liquidity state mixin."""

    # The concentrated liquidity handle (set by the companion's from_handle); the
    # sparse_liquidity_map property reads Rust coverage through it.
    _py_pool: Any

    @property
    def sparse_liquidity_map(self) -> bool:
        """Determine sparse liquidity map.

        Rust ``coverage`` is the fact (the double-tracked Python flag is
        retired; the shared concentrated liquidity state owns the read of
        the typed enum).

        """
        return cast(
            "bool",
            self._py_pool.concentrated_liquidity().coverage == PoolTickCoverage.Sparse,
        )
