"""Contract: the legacy one-shot ``UniswapArbEngine.start(rpc_url)`` is gone.

Plan 102, Slice 1. The canonical bot-startup path is the Python-orchestrated
two-phase ritual (``subscribe`` → ``backfill_from_snapshot`` → ``resume``),
lifted into ``degenbot.arbitrage.EngineRegistry`` in Slice 3. The competing
Rust one-shot ``start`` + ``BlockPump::spawn`` is dead surface — it had zero
Python callers and was never declared on the ``.pyi`` stub — so it is deleted,
not reorganized.

These tests guard the deletion: ``start`` must not be exposed, while the
canonical phase methods (``subscribe`` / ``backfill_from_snapshot`` / ``resume``
/ ``stop``) remain the surface every operator uses.
"""

from __future__ import annotations

import pytest

from degenbot._ffi import UniswapArbEngine


def test_start_is_not_exposed() -> None:
    """The legacy one-shot entry must not be on the Python surface."""
    assert not hasattr(UniswapArbEngine, "start"), (
        "UniswapArbEngine.start still exists — the dead one-shot "
        "(PyUniswapArbEngine::start / BlockPump::spawn) must be deleted "
        "(Plan 102, Slice 1). The canonical startup is subscribe→resume."
    )


@pytest.mark.parametrize(
    "method",
    [
        "subscribe",
        "resume",
    ],
)
def test_canonical_phase_methods_remain(method: str) -> None:
    """The two-phase surface every operator uses must stay intact.

    The pump lifecycle ends via the engine's ``__pyo3::traverse``/drop or by
    dropping the handle; the phase entry points are ``subscribe`` (observe)
    and ``resume`` (begin normal processing + auto-backfill, J3FMDO). The
    ``backfill_from_snapshot`` method is retired (2SM4Y7) — the snapshot→WS
    gap is closed automatically inside ``BlockPump::resume_from_subscribe``.
    """
    assert hasattr(UniswapArbEngine, method), (
        f"UniswapArbEngine.{method} missing — Slice 1 deletes only start(); "
        f"the canonical phase surface must be preserved."
    )
