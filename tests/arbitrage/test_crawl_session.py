"""The engine crawl session is the trim-proof operator identity.

``BotRunner.run()`` trims main-loop Python state (``release_python_state()`` +
``drop_bot()``, so ``actors.bot`` becomes ``None``). The operator surface —
``enqueue_path`` / ``trigger_discovery`` — must keep working after that trim:
the identity it routes through is the engine's booted crawl session, parked on
the registration-owned :class:`~degenbot.runner.build_paths.ConstructionContext`
that the trim never severs.
"""

from __future__ import annotations

import importlib
from dataclasses import dataclass
from typing import Any

import pytest

from degenbot.runner.build_paths import ConstructionContext, _consume
from tests.helpers.session import session_for_test


@dataclass
class _FakeCrawlSession:
    """The engine session's operator surface, recorded."""

    enqueued: list[tuple[Any, list[bool] | None]] = None  # type: ignore[assignment]
    sweeps: list[int | None] = None  # type: ignore[assignment]

    def __post_init__(self) -> None:
        self.enqueued = []
        self.sweeps = []

    async def enqueue_path(self, path: Any, directions: list[bool] | None = None) -> int:
        self.enqueued.append((path, directions))
        return 7

    async def trigger_discovery(self, bound: int | None = None) -> int:
        self.sweeps.append(bound)
        return 3


def _context(session: Any) -> ConstructionContext:
    return ConstructionContext(
        bot=None,  # type: ignore[arg-type]  # the trim drops the Python bot ref
        chain_id=1,
        database_path=None,  # type: ignore[arg-type]
        construction_route=None,  # type: ignore[arg-type]
        session=session,
    )


@pytest.mark.asyncio
async def test_the_operator_surface_survives_the_main_loop_trim() -> None:
    """After the trim, enqueue_path / trigger_discovery still reach the engine."""
    crawl = _FakeCrawlSession()
    session = session_for_test()
    # The ritual drives the session into its running phase (the operator
    # surface's precondition); the crawl session is what build_paths parks.
    await session.run()
    session.registration_context = _context(crawl)

    # The trim: the Python-companion bot ref the actors carried is gone.
    session.drop_bot()
    assert session.actors is not None
    assert session.actors.bot is None

    steps = [("0x" + "11" * 20, 1, None)]
    await session.enqueue_path(steps, directions=[True])
    assert crawl.enqueued == [(steps, [True])]
    assert await session.trigger_discovery(bound=5) == 3
    assert crawl.sweeps == [5]


class _ScriptedCrawl:
    """A crawl session whose receipt stream, fold, and render are scripted.

    `advance` makes the fold keep up with the stream (one created registration per
    receipt); without it the fold is FROZEN — what a consumer sees when it is
    still draining a burst the engine has already finished folding.
    """

    def __init__(
        self,
        receipts: list[dict[str, Any]],
        summary: dict[str, Any],
        *,
        advance: bool = False,
    ) -> None:
        self._receipts = receipts
        self._summary = summary
        self._advance = advance
        self._registered = 0

    async def receipts(self) -> Any:
        for receipt in self._receipts:
            if self._advance and receipt["path_id"] is not None:
                self._registered += 1
            yield receipt

    def summary(self) -> dict[str, Any]:
        report = dict(self._summary)
        if self._advance:
            report["path_count"] = self._registered
        return report

    def progress_line(self) -> str:
        return "[build_paths] Progress: 1000 paths registered (terminal render)"

    def run_outcome(self) -> tuple[str, None]:
        return ("completed", None)


class _SkipMeter:
    """The skip-metric sink a real ``Bot`` exposes to ``_observe_skip``."""

    def __init__(self) -> None:
        self.reasons: list[str] = []

    def record_registration_skip(self, reason: str) -> None:
        self.reasons.append(reason)


class _MeteredBot:
    def __init__(self) -> None:
        self._py_bot = _SkipMeter()


def _registered(path_id: int | None) -> dict[str, Any]:
    return {"seq": 0, "kind": "registered", "tag": None, "path_id": path_id}


def _anchor_lines(lines: list[str]) -> list[str]:
    return [line for line in lines if line.endswith("duplicates")]


def _recording_bot() -> _MeteredBot:
    return _MeteredBot()


def _craft_receipts() -> list[dict[str, Any]]:
    receipts: list[dict[str, Any]] = []
    for index in range(2500):
        receipts.append(_registered(1000 + index))
        if index % 8 == 0:
            receipts.append(_registered(None))
    return receipts


@pytest.mark.asyncio
async def test_a_drained_burst_prints_the_anchor_once(monkeypatch: pytest.MonkeyPatch) -> None:
    """A lagging consumer drains a burst without reprinting the same line.

    The engine's fold has already absorbed every receipt it sent, so a burst this
    consumer is still draining reads ONE (unchanged) count for the whole burst.
    The anchor fires once per 1000-boundary the fold reaches — never once per
    receipt — and a duplicate (no path id) moves no counter: it stays a skip
    metric, as the retired pipeline recorded it.
    """
    receipts = _craft_receipts()
    # The frozen fold: exactly what a drained burst sees after the crawl resolved.
    session = _ScriptedCrawl(
        receipts,
        {
            "path_count": 1000,
            "skip_count": 0,
            "token_filter_count": 0,
            "engine_reject_count": 0,
            "dup_count": 0,
            "register_fail_count": 0,
        },
    )
    lines: list[str] = []
    bot = _recording_bot()
    monkeypatch.setattr(
        # The package re-exports the `build_paths` FUNCTION, which shadows the
        # submodule attribute: resolve the module itself.
        importlib.import_module("degenbot.runner.build_paths"),
        "bot_logger",
        type("_Recorder", (), {"info": lambda _self, line: lines.append(line)})(),
    )

    await _consume(session, bot=bot, progress_interval_s=30.0)

    assert len(_anchor_lines(lines)) == 1, "a frozen fold prints its boundary once"
    cadence = [line for line in lines if "(terminal render)" in line]
    assert len(cadence) == 1, "the time-throttled cadence does not repeat inside one burst"
    assert bot._py_bot.reasons.count("dup") == len(receipts) - 2500


@pytest.mark.asyncio
async def test_a_live_crawl_prints_one_anchor_per_thousand(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """While the fold keeps up, the anchor keeps its per-1000 cadence."""
    session = _ScriptedCrawl(
        _craft_receipts(),
        {
            "skip_count": 0,
            "token_filter_count": 0,
            "engine_reject_count": 0,
            "dup_count": 0,
            "register_fail_count": 0,
        },
        advance=True,
    )
    lines: list[str] = []
    monkeypatch.setattr(
        importlib.import_module("degenbot.runner.build_paths"),
        "bot_logger",
        type("_Recorder", (), {"info": lambda _self, line: lines.append(line)})(),
    )

    await _consume(session, bot=_recording_bot(), progress_interval_s=30.0)

    assert len(_anchor_lines(lines)) == 2, "2500 created registrations cross two boundaries"


@pytest.mark.asyncio
async def test_without_a_booted_session_the_operator_surface_refuses_loudly() -> None:
    """No session ⇒ a named refusal, never a silent drop."""
    session = session_for_test()
    await session.run()
    session.registration_context = _context(None)

    with pytest.raises(RuntimeError, match="no live crawl session"):
        await session.enqueue_path([("0x" + "11" * 20, 1, None)])
    with pytest.raises(RuntimeError, match="no live crawl session"):
        await session.trigger_discovery(bound=1)
