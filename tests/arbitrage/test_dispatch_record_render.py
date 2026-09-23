"""Log-level contract for the submit-leaf record renderer.

A default-INFO live run must surface WHY a submission did not happen. The
BROADCAST_FAILED skip reason was historically logged at DEBUG, so a relay
outage or an underpriced replacement could silence a live bot for hours.
"""

from __future__ import annotations

import types

import pytest

from degenbot.runner import _dispatch as dispatch_module
from degenbot.runner._relay_posture import RelayPosture


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

    def error(self, msg: str) -> None:
        self._cap("error", msg)


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
            chain_id=1,
            dry_run=False,
            inject_executor_code=False,
        ),
        dispatcher=types.SimpleNamespace(current_block=1),
        relay_posture=RelayPosture(relay_urls=["http://offline-test.relay"]),
        submission_smoke=dispatch_module.SubmissionSmoke(),
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


def _live_session(cfg_overrides: dict) -> types.SimpleNamespace:
    cfg = types.SimpleNamespace(
        operator_private_key="0x" + "a" * 64,
        chain_id=1,
        dry_run=False,
        inject_executor_code=False,
        **cfg_overrides,
    )
    return types.SimpleNamespace(
        async_w3=types.SimpleNamespace(as_async_alloy=lambda: object()),
        cfg=cfg,
        dispatcher=types.SimpleNamespace(current_block=1),
        relay_posture=RelayPosture(relay_urls=["http://offline-test.relay"]),
        submission_smoke=dispatch_module.SubmissionSmoke(),
    )


def _candidate() -> types.SimpleNamespace:
    return types.SimpleNamespace(
        path_id=7, solve_block=1, net_profit=0, gas_used=0, execute_calldata=None
    )


@pytest.mark.asyncio
async def test_silent_veto_streak_warns_once(monkeypatch: pytest.MonkeyPatch) -> None:
    """Live-armed + gate-clearing candidates + zero submissions, repeatedly,
    must produce exactly one throttled WARN naming the skip reasons."""
    captured = _CapturingLogger()
    monkeypatch.setattr(dispatch_module, "bot_logger", captured)

    skip = dispatch_module.SkippedRecord(
        path_id=7, reason=dispatch_module.SubmitSkipReason.POOLS_CLAIMED
    )

    async def all_skipped(**kwargs):  # noqa: ANN003, ANN202
        return [skip]

    monkeypatch.setattr(dispatch_module, "dispatch_and_submit", all_skipped)
    session = _live_session({})
    outcome = types.SimpleNamespace(gas_profitable=[_candidate()])

    for _ in range(4):
        await dispatch_module._submit_batch_records(session, outcome, operator_nonce=3)

    stall_warnings = [m for lvl, m in captured.calls if lvl == "warning" and "no submissions" in m]
    assert len(stall_warnings) == 1, f"expected one throttled stall warn, got {captured.calls}"

    real_submitted = dispatch_module.SubmittedRecord(path_id=7, tx_hash="0xabc", nonce=3)

    async def one_submitted(**kwargs):  # noqa: ANN003, ANN202
        return [real_submitted]

    monkeypatch.setattr(dispatch_module, "dispatch_and_submit", one_submitted)
    await dispatch_module._submit_batch_records(session, outcome, operator_nonce=3)
    assert session.submission_smoke.streak == 0


class TestSubmissionSmokeFsm:
    """The per-session smoke FSM's `Quiet | Streak | Warn` transitions."""

    def test_first_warn_fires_on_any_monotonic_clock_origin(self) -> None:
        """A fresh boot's monotonic clock starts near zero, so `now` can sit
        INSIDE the 300s throttle window; the never-warned state must not be
        conflated with `last_warn=0.0` - the first full-veto streak WARNs on
        the third veto no matter what the clock reads."""
        smoke = dispatch_module.SubmissionSmoke()
        verdict = dispatch_module.SubmissionSmokeVerdict

        assert smoke.observe(vetoed=False, now=10.0) is verdict.QUIET
        assert smoke.observe(vetoed=True, now=110.0) is verdict.STREAK
        assert smoke.observe(vetoed=True, now=210.0) is verdict.STREAK
        assert smoke.observe(vetoed=True, now=260.0) is verdict.WARN

        # Once warned, the 300s throttle anchors at that warning.
        assert smoke.observe(vetoed=True, now=261.0) is verdict.STREAK
        assert smoke.observe(vetoed=True, now=561.0) is verdict.WARN

    def test_quiet_streak_warn_and_throttle(self) -> None:
        smoke = dispatch_module.SubmissionSmoke()
        verdict = dispatch_module.SubmissionSmokeVerdict

        # A mature monotonic clock (host up for hours): the first WARN fires
        # on the third consecutive veto; `last_warn=0.0` is out of the
        # window because the clock is already past the interval.
        assert smoke.observe(vetoed=False, now=10_000.0) is verdict.QUIET
        assert smoke.streak == 0
        assert smoke.observe(vetoed=True, now=10_000.0) is verdict.STREAK
        assert smoke.observe(vetoed=True, now=10_000.0) is verdict.STREAK
        assert smoke.observe(vetoed=True, now=10_000.0) is verdict.WARN
        assert smoke.streak == 3
        # A second veto inside the throttle window stays STREAK.
        assert smoke.observe(vetoed=True, now=10_001.0) is verdict.STREAK
        # Re-arms once the window elapses.
        assert smoke.observe(vetoed=True, now=10_301.0) is verdict.WARN
        # A batch that submits (not vetoed) resets the FSM to Quiet.
        assert smoke.observe(vetoed=False, now=10_302.0) is verdict.QUIET
        assert smoke.streak == 0
