"""SMOZG3: operator ERC6909 vault-capture toggle reaches the Rust seam.

``_dispatch_profitable`` must project the driver's
``ERC6909_PROFIT`` operator knob (``driver_constants``, env-gated default
off) into the ``DispatchCandidate(erc6909_profit=...)`` kwarg so the Rust
strategy's ``resolve_axes`` / ``config_for_options`` axis chain (→
``check_mode=2`` + the pure-V4 ``V4_MINT_COMPACT`` stream) is reachable in
production. The seam's kwarg acceptance itself is pinned by
``tests/rust/test_simulation_seam_classes.py``; this test pins the driver's
projection of the knob into it.
"""

from __future__ import annotations

import pytest

from degenbot.runner import dispatch as d
from degenbot.runner.dispatch import ERC6909_PROFIT


class _RecordingCandidate:
    """Stand-in for the FFI ``DispatchCandidate`` — records constructor kwargs."""

    def __init__(self, **kwargs) -> None:
        self.kwargs = kwargs


class _EngineRegistry:
    engine = object()


def test_erc6909_default_is_off() -> None:
    # Custody capture (the long-standing production behavior) stays the
    # default: the knob is off unless the operator opts in via the env var.
    assert ERC6909_PROFIT is False


async def test_dispatch_profitable_projects_erc6909_toggle(monkeypatch) -> None:
    recorded: list[dict] = []

    class _Rec(_RecordingCandidate):
        def __init__(self, **kwargs) -> None:  # noqa: D107
            super().__init__(**kwargs)
            recorded.append(kwargs)

    monkeypatch.setattr(d, "DispatchCandidate", _Rec)

    # One solved result; ``sim_ctx=None`` raises AFTER candidate construction
    # (the RuntimeError is the tripwire that the constructor really ran).
    results = [(1, 100, 5, (105,), (100,), 10, (0,))]
    with pytest.raises(RuntimeError, match="SimulateContext is required"):
        await d._dispatch_profitable(
            results=results,  # type: ignore[arg-type]
            engine_registry=_EngineRegistry(),  # type: ignore[arg-type]
            async_w3=None,  # type: ignore[arg-type]
            sim_ctx=None,
            operator_private_key="0x" + "0" * 32,
            operator_nonce=0,
            dispatcher=d.Dispatcher.for_block(0),
            current_block=10,
            block_timestamp=1_700_000_000,
            base_fee_next=1_000_000_000,
            dry_run=True,
        )

    assert len(recorded) == 1, "one candidate must be constructed"
    assert recorded[0]["erc6909_profit"] is ERC6909_PROFIT, (
        "the operator knob must be projected into the candidate kwarg"
    )
