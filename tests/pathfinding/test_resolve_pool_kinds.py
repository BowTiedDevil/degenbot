"""Loud-abort contract for unknown pool-table classes at graph build (ADR-055 D4).

``_resolve_pool_kinds`` maps the driver-declared ``pool_types`` onto Rust
``pool_kind`` discriminants. An unknown family at this use site is an
infrastructure gap: the caller declared intent to use the pool, and the
graph builder cannot serve it. It must abort loudly, never silently omit.
"""

import warnings

import pytest

from degenbot.database.models.pools import (
    LiquidityPoolTable,
    UniswapV3PoolTable,
)
from degenbot.pathfinding._pathfinding import _resolve_pool_kinds


def _unknown_table() -> type:
    # An unmapped subclass: the test only exercises classification, so the
    # mapper-config noise at class creation is silenced deliberately.
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        return type("SomeFuturePoolTable", (LiquidityPoolTable,), {})


def test_known_families_resolve():
    assert _resolve_pool_kinds([UniswapV3PoolTable]) == {1}
    assert _resolve_pool_kinds([LiquidityPoolTable]) == {0, 1}


def test_unknown_family_aborts_loudly():
    with pytest.raises(ValueError, match="SomeFuturePoolTable"):
        _resolve_pool_kinds([_unknown_table()])
