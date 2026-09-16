"""Log-level contract for the submit-leaf record renderer.

A default-INFO live run must surface WHY a submission did not happen. The
BROADCAST_FAILED skip reason was historically logged at DEBUG, so a relay
outage or an underpriced replacement could silence a live bot for hours.
"""

from __future__ import annotations

import types

import pytest

from degenbot.runner import _dispatch as dispatch_module


class _CapturingLogger:
    def __init__(self) -> None:
        self.calls: list[tuple[str, str]] = []

    def _cap(self, level: str, msg: str) -> None:
        self.calls.append((level, msg))

    def info(self, msg: str) -> None:
        self._cap("info", msg)

    def warning(self, msg: str) -> None:
        self._cap("warning", msg)

    def debug(self, msg: str) -> None:
        self._cap("debug", msg)


@pytest.mark.asyncio
async def test_broadcast_failure_renders_at_warning(monkeypatch: pytest.MonkeyPatch) -> None:
    rec = dispatch_module.SkippedRecord(
        path_id=7,
        reason=dispatch_module.SubmitSkipReason.BROADCAST_FAILED,
        detail="relay unreachable",
    )

    captured = _CapturingLogger()
    monkeypatch.setattr(dispatch_module, "bot_logger", captured)

    async def fake_dispatch_and_submit(**kwargs):  # noqa: ANN003, ANN202
        return [rec]

    monkeypatch.setattr(dispatch_module, "dispatch_and_submit", fake_dispatch_and_submit)

    session = types.SimpleNamespace(
        async_w3=types.SimpleNamespace(as_async_alloy=lambda: object()),
        cfg=types.SimpleNamespace(
            operator_private_key="0x" + "a" * 64,
            dry_run=False,
            inject_executor_code=False,
        ),
        dispatcher=types.SimpleNamespace(current_block=1),
    )
    candidate = types.SimpleNamespace(
        path_id=7,
        solve_block=1,
        net_profit=0,
        gas_used=0,
        execute_calldata=None,
    )
    outcome = types.SimpleNamespace(gas_profitable=[candidate])

    await dispatch_module._submit_batch_records(session, outcome, operator_nonce=3)

    assert any(
        level == "warning" and "relay unreachable" in msg for level, msg in captured.calls
    ), f"expected a warning naming the failure detail, got {captured.calls}"
