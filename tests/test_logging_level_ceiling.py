"""The queued console handler's level is the ceiling on DEBUG visibility.

``set_log_level`` moves logger levels, but a handler's own level drops every
record below it at ``callHandlers`` before the handler runs. A logger-only
level change therefore looks like a working DEBUG path while no DEBUG record
ever reaches the queue — the state the suite's session logging fixture was
silently in.
"""

import logging
import time

import degenbot.logging as dl

_LOGGER_NAME = "degenbot.logging_ceiling_probe"


class _CaptureHandler(logging.Handler):
    def __init__(self) -> None:
        super().__init__(level=logging.DEBUG)
        self.records: list[logging.LogRecord] = []

    def emit(self, record: logging.LogRecord) -> None:
        self.records.append(record)


def _emit_and_wait(message: str, timeout: float) -> set[str]:
    """Attach a listener destination, emit one DEBUG record, wait for it.

    Returns every message the destination saw within ``timeout``.
    """
    capture = _CaptureHandler()
    listener = dl._LOG_LISTENER
    listener.handlers = (*listener.handlers, capture)
    try:
        logging.getLogger(_LOGGER_NAME).debug(message)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if any(r.getMessage() == message for r in capture.records):
                break
            time.sleep(0.01)
        return {r.getMessage() for r in capture.records}
    finally:
        listener.handlers = tuple(h for h in listener.handlers if h is not capture)


def test_suite_debug_posture_reaches_the_console_queue() -> None:
    """Under the suite's logging configuration a DEBUG record is visible.

    This is the guarantee the session logging fixture claims. It fails whenever
    the queued handler stays above DEBUG, whatever level the loggers carry.
    """
    probe = "degenbot.logging_ceiling_suite_probe"
    seen = _emit_and_wait(probe, timeout=5.0)
    assert probe in seen, (
        f"DEBUG record {probe!r} never reached the queue listener; queued handler "
        f"level is {logging.getLevelName(dl._QUEUED_HANDLER.level)} — logger levels "
        "alone do not make DEBUG visible"
    )


def test_queued_handler_level_gates_debug_visibility() -> None:
    """DEBUG is visible at handler DEBUG and suppressed at handler INFO."""
    blocked = "degenbot.logging_ceiling_blocked"
    allowed = "degenbot.logging_ceiling_allowed"
    original = dl._QUEUED_HANDLER.level
    try:
        dl._QUEUED_HANDLER.setLevel(logging.INFO)
        assert blocked not in _emit_and_wait(blocked, timeout=0.3), (
            "a DEBUG record reached the queue listener while the queued handler was at INFO"
        )

        dl._QUEUED_HANDLER.setLevel(logging.DEBUG)
        assert allowed in _emit_and_wait(allowed, timeout=5.0), (
            "a DEBUG record did not reach the queue listener while the queued handler was at DEBUG"
        )
    finally:
        dl._QUEUED_HANDLER.setLevel(original)
