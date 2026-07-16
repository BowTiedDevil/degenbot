"""Contract: per-pool PyO3 snapshot ingestion is gone (DADWUP).

The DB→store snapshot transfer is owned by Rust: the DB path loads the whole
snapshot inside ``Bot::load_snapshot_from_db`` at ``PyBot`` construction (zero
tick dicts cross PyO3); the non-DB path crosses the boundary ONCE via
``load_v3_snapshot_from_py`` / ``load_v4_snapshot_from_py`` with the whole dict.

The retired surface — per-pool crossings that built the snapshot one pool at a
time via SQLAlchemy ``yield_per`` loops on the Python side — was the FFI bulk-
data overhead DADWUP removes:

- ``begin_v3_snapshot_stream`` / ``insert_v3_pool_snapshot`` / ``finish_v3_snapshot``
  (+ V4 twins) on the ``UniswapArbEngine`` pyo3 surface.
- ``stream_v3_snapshot_to_engine`` / ``stream_v4_snapshot_to_engine`` in
  ``degenbot.uniswap.snapshot_binary`` (the SQLAlchemy ``yield_per`` loops
  that drove ``insert_*_pool_snapshot`` per pool).

The whole-dict ``load_*_from_py`` methods stay (non-DB-only, single crossing).
"""

from __future__ import annotations

import pytest

from degenbot.arbitrage.engine_registry import UniswapArbEngine

_RETIRED_PYO3_METHODS = [
    "begin_v3_snapshot_stream",
    "insert_v3_pool_snapshot",
    "finish_v3_snapshot",
    "begin_v4_snapshot_stream",
    "insert_v4_pool_snapshot",
    "finish_v4_snapshot",
]


@pytest.mark.parametrize("method", _RETIRED_PYO3_METHODS)
def test_per_pool_snapshot_pyo3_method_removed(method: str) -> None:
    """Each per-pool ingestion crossing must be gone from the Python surface."""
    assert not hasattr(UniswapArbEngine, method), (
        f"UniswapArbEngine.{method} still exists — DADWUP retires the "
        f"per-pool PyO3 ingestion surface (insert_* / begin_* / finish_*). "
        f"The DB path loads inside Bot::load_snapshot_from_db; the non-DB "
        f"path crosses once via load_*_from_py."
    )


@pytest.mark.parametrize("method", ["load_v3_snapshot_from_py", "load_v4_snapshot_from_py"])
def test_whole_dict_load_methods_remain(method: str) -> None:
    """The single-crossing whole-dict loaders stay (non-DB-only path)."""
    assert hasattr(UniswapArbEngine, method), (
        f"UniswapArbEngine.{method} must stay — it's the non-DB snapshot "
        f"path's single whole-dict PyO3 crossing (DADWUP keeps it; only "
        f"the per-pool insert/begin/finish surface retires)."
    )


def test_stream_snapshot_to_engine_functions_removed() -> None:
    """The SQLAlchemy yield_per loops must not be importable."""
    import degenbot.uniswap.snapshot_binary as mod

    for name in ("stream_v3_snapshot_to_engine", "stream_v4_snapshot_to_engine"):
        assert not hasattr(mod, name), (
            f"degenbot.uniswap.snapshot_binary.{name} still exists — DADWUP "
            f"retires the SQLAlchemy yield_per loops (per-pool crossings). "
            f"The DB path is Rust-owned; the non-DB path uses load_*_from_py."
        )
