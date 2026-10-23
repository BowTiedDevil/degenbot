"""The queued console handler's level is the ceiling on DEBUG visibility.

``set_log_level`` moves logger levels, but a handler's own level drops every
record below it at ``callHandlers`` before the handler runs. A logger-only
level change therefore looks like a working DEBUG path while no DEBUG record
ever reaches the queue — the state the suite's session logging fixture was
silently in.
"""

import logging
import time
from collections.abc import Generator
from contextlib import contextmanager

import degenbot.logging as dl
from tests.deadlock_guard import DEADLOCK_GUARD_S

_LOGGER_NAME = "degenbot.logging_ceiling_probe"
_LOGGER = logging.getLogger(_LOGGER_NAME)


class _CaptureHandler(logging.Handler):
    def __init__(self) -> None:
        super().__init__(level=logging.DEBUG)
        self.records: list[logging.LogRecord] = []

    def emit(self, record: logging.LogRecord) -> None:
        self.records.append(record)


@contextmanager
def _listener_capture() -> Generator[_CaptureHandler, None, None]:
    """Attach a listener destination for the duration of the block."""
    capture = _CaptureHandler()
    listener = dl._LOG_LISTENER
    listener.handlers = (*listener.handlers, capture)
    try:
        yield capture
    finally:
        listener.handlers = tuple(h for h in listener.handlers if h is not capture)


def _await_delivery(capture: _CaptureHandler, message: str) -> None:
    """Turn until the listener thread hands ``message`` to the capture handler.

    The queue listener is a background thread with its own queue, so the test
    cannot own a completion event for it: the deadline below is a failure guard
    only (see :mod:`tests.deadlock_guard`), never an ordering assertion. The
    ordering this file asserts — that a record dropped at the queued handler's
    level was never enqueued at all — rides the queue's FIFO position against a
    later marker record, not this budget.
    """
    deadline = time.monotonic() + DEADLOCK_GUARD_S
    while time.monotonic() < deadline:
        if any(r.getMessage() == message for r in capture.records):
            return
        time.sleep(0.01)


def test_suite_debug_posture_reaches_the_console_queue() -> None:
    """Under the suite's logging configuration a DEBUG record is visible.

    This is the guarantee the session logging fixture claims. It fails whenever
    the queued handler stays above DEBUG, whatever level the loggers carry.
    """
    probe = "degenbot.logging_ceiling_suite_probe"
    with _listener_capture() as capture:
        _LOGGER.debug(probe)
        _await_delivery(capture, probe)
        seen = {r.getMessage() for r in capture.records}
    assert probe in seen, (
        f"DEBUG record {probe!r} never reached the queue listener; queued handler "
        f"level is {logging.getLevelName(dl._QUEUED_HANDLER.level)} — logger levels "
        "alone do not make DEBUG visible"
    )


def test_queued_handler_level_gates_debug_visibility() -> None:
    """DEBUG is visible at handler DEBUG and suppressed at handler INFO.

    The suppression is proved by queue position, not by elapsed time: the
    blocked record is emitted while the queued handler sits at INFO (so it is
    dropped on the emitting thread and never enqueued), then the handler drops
    to DEBUG and a marker record is emitted. The marker can only reach the
    capture handler after the blocked record's queue position, so once the
    marker lands, a blocked record that was merely slow would already be there.
    """
    blocked = "degenbot.logging_ceiling_blocked"
    marker = "degenbot.logging_ceiling_marker"
    original = dl._QUEUED_HANDLER.level
    try:
        with _listener_capture() as capture:
            dl._QUEUED_HANDLER.setLevel(logging.INFO)
            _LOGGER.debug(blocked)
            dl._QUEUED_HANDLER.setLevel(logging.DEBUG)
            _LOGGER.debug(marker)
            _await_delivery(capture, marker)
            seen = {r.getMessage() for r in capture.records}
        assert marker in seen, (
            "a DEBUG record did not reach the queue listener while the queued handler was at DEBUG"
        )
        assert blocked not in seen, (
            "a DEBUG record reached the queue listener while the queued handler was at INFO"
        )
    finally:
        dl._QUEUED_HANDLER.setLevel(original)
