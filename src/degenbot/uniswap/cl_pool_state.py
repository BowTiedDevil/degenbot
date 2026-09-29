"""Shared state surface for concentrated-liquidity (CL) pools.

``V3PoolState`` and ``V4PoolState`` were copy-paste twins for the reads Rust
owns generically (survey C4): the sparse-liquidity-map probe lives here once,
reading the typed ``PoolTickCoverage`` enum off the CL handle (survey C7).
"""

from __future__ import annotations

from typing import Any

from degenbot.uniswap import PoolTickCoverage


class ConcentratedLiquidityPoolState:
    """State surface shared by every CL pool state mixin (V3/V4)."""

    # The CL handle (set by the companion's from_handle); the
    # sparse_liquidity_map property reads Rust coverage through it.
    _py_pool: Any

    @property
    def sparse_liquidity_map(self) -> bool:
        """Determine sparse liquidity map.

        Rust ``coverage`` is the fact (the double-tracked Python flag is
        retired; the shared CL state owns the read of the typed enum).

        """
        return self._py_pool.concentrated_liquidity().coverage == PoolTickCoverage.Sparse
