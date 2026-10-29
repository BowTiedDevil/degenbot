"""Log-level contract for the submit-leaf record renderer + the smoke FSM.

A default-INFO live run must surface WHY a submission did not happen. The
BROADCAST_FAILED skip reason was historically logged at DEBUG, so a relay
outage or an underpriced replacement could silence a live bot for hours.

The silent-veto smoke FSM's input is the resolved display policy
(``DisplayPolicy.vetoed``), so the veto predicate and the throttle live on
typed values rather than on a session the renderer would have to reach into.
"""

from __future__ import annotations

import dataclasses

from degenbot.runner import _dispatch as dispatch_module
from degenbot.runner._render import DisplayPolicy, MergedOutcome

#: A drained batch that carried nothing at all (the falsy render view).
_EMPTY_BATCH = MergedOutcome(
    gas_profitable=[],
    gas_unprofitable_count=0,
    exception_count=0,
    fail_count=0,
    candidate_count=0,
    suppressed_count=0,
    fail_buckets={},
    failures=[],
    path_infos={},
)


class _CapturingLogger:
    """A recording logger double rendering the standard lazy ``%`` args."""

    def __init__(self) -> None:
        self.calls: list[tuple[str, str]] = []

    def _cap(self, level: str, msg: str, *args: object) -> None:
        if args:
            msg = msg % args
        self.calls.append((level, msg))

    def info(self, msg: str, *args: object) -> None:
        self._cap("info", msg, *args)

    def warning(self, msg: str, *args: object) -> None:
        self._cap("warning", msg, *args)

    def debug(self, msg: str, *args: object) -> None:
        self._cap("debug", msg, *args)

    def error(self, msg: str, *args: object) -> None:
        self._cap("error", msg, *args)


def test_broadcast_failure_renders_at_warning() -> None:
    rec = dispatch_module.SkippedRecord(
        path_id=7,
        reason=dispatch_module.SubmitSkipReason.BROADCAST_FAILED,
        detail="relay unreachable",
    )

    captured = _CapturingLogger()

    # The drain's submit-record render (the executor publishes the raw lane
    # records; `render_submit_records` is its display leaf).
    dispatch_module.render_submit_records([rec], logger=captured)

    assert any(
        level == "warning" and "relay unreachable" in msg for level, msg in captured.calls
    ), f"expected a warning naming the failure detail, got {captured.calls}"


def _live_policy() -> DisplayPolicy:
    return DisplayPolicy(live=True, sim_exit_on_fail=False, exit_ignore_buckets="")


def _outcome_with_candidate() -> MergedOutcome:
    return dataclasses.replace(_EMPTY_BATCH, gas_profitable=[object()], candidate_count=1)


def test_silent_veto_streak_warns_once() -> None:
    """Live-armed + gate-clearing candidates + zero submissions, repeatedly,
    must produce exactly one throttled WARN naming the skip reasons."""
    captured = _CapturingLogger()

    skip = dispatch_module.SkippedRecord(
        path_id=7, reason=dispatch_module.SubmitSkipReason.POOLS_CLAIMED
    )
    smoke = dispatch_module.SubmissionSmoke()
    outcome = _outcome_with_candidate()
    policy = _live_policy()

    for _ in range(4):
        dispatch_module.track_submission_smoke(
            smoke, outcome, [skip], policy=policy, logger=captured
        )

    stall_warnings = [m for lvl, m in captured.calls if lvl == "warning" and "no submissions" in m]
    assert len(stall_warnings) == 1, f"expected one throttled stall warn, got {captured.calls}"

    real_submitted = dispatch_module.SubmittedRecord(path_id=7, tx_hash="0xabc", nonce=3)
    dispatch_module.track_submission_smoke(
        smoke, outcome, [real_submitted], policy=policy, logger=captured
    )
    assert smoke.streak == 0


class TestDisplayPolicyVeto:
    """The veto predicate: the live-armed disposition gate on the FSM input."""

    def test_a_live_batch_with_candidates_and_no_submission_is_vetoed(self) -> None:
        assert _live_policy().vetoed(_outcome_with_candidate(), 0) is True

    def test_a_submission_clears_the_veto(self) -> None:
        assert _live_policy().vetoed(_outcome_with_candidate(), 1) is False

    def test_a_candidate_free_batch_is_not_a_veto(self) -> None:
        assert _live_policy().vetoed(_EMPTY_BATCH, 0) is False

    def test_a_dry_run_batch_is_not_a_veto(self) -> None:
        dry_run = DisplayPolicy(live=False, sim_exit_on_fail=False, exit_ignore_buckets="")
        assert dry_run.vetoed(_outcome_with_candidate(), 0) is False


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
