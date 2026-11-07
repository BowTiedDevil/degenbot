"""The console pipeline's arming is owned in one place by ``degenbot.logging``.

Two drivers stop the one ``QueueListener``: pytest's ``pytest_sessionfinish``
drains it while the capture streams are still open, and ``atexit`` drains it
again at interpreter exit. The listener cannot arbitrate that itself — stdlib
``stop()`` joins ``self._thread`` unguarded through 3.12 (upstream guards it
from 3.13, gh-114706) — so a stop-after-stop raises inside an ``atexit``
callback, where the exception is unraisable: it prints as ``Exception ignored in
atexit callback`` and leaves the exit status untouched.
"""

from __future__ import annotations

import logging
import logging.handlers
import queue

import degenbot.logging as dl
from tests.helpers.verdict_probe import run


class _Recorder(logging.Handler):
    """Collect the records the listener forwards to it."""

    def __init__(self) -> None:
        super().__init__(level=logging.DEBUG)
        self.records: list[logging.LogRecord] = []

    def emit(self, record: logging.LogRecord) -> None:
        """Keep one forwarded record."""
        self.records.append(record)


def test_second_stop_is_a_noop_and_start_rearms_delivery() -> None:
    """A stop after the session stop is inert, and the re-armed pair forwards.

    That second stop is exactly what ``atexit`` performs: the session end
    already stopped this listener, through the listener's own unguarded stdlib
    ``stop`` on 3.12. The cycles run on an injected listener over its own queue
    and handler, so nothing about the console the rest of the suite logs
    through is touched.
    """
    probe = "degenbot.console_pipeline_probe"
    recorder = _Recorder()
    log_queue = queue.SimpleQueue()
    listener = logging.handlers.QueueListener(log_queue, recorder, respect_handler_level=True)
    pipeline = dl._ConsolePipeline(listener_factory=lambda: listener)
    # Never registered (`logging.Logger`, not `getLogger`): the mutations
    # below must stay off the global logging registry other tests use.
    probe_logger = logging.Logger(  # ruff: ignore[direct-logger-instantiation]
        "tests.console_log_pipeline",
    )
    probe_logger.setLevel(logging.DEBUG)
    probe_logger.propagate = False
    queued = logging.handlers.QueueHandler(log_queue)
    probe_logger.addHandler(queued)
    try:
        pipeline.start()
        pipeline.stop()
        pipeline.stop()  # the atexit call, after the session stop
        pipeline.start()
        probe_logger.info(probe)
        pipeline.stop()  # the drain stop performs inside flush_console_log forwards it
        assert probe in {record.getMessage() for record in recorder.records}
    finally:
        pipeline.stop()
        probe_logger.removeHandler(queued)


def test_exit_after_a_session_stop_prints_nothing_unraisable() -> None:
    """Stopping the pipeline, then exiting, leaves no ``Exception ignored`` line."""
    finished = run(
        """
import degenbot.logging as dl

dl._console_pipeline.stop()  # the stop pytest_sessionfinish performs
print("SESSION_DONE")
""",
    )
    assert finished.returncode == 0, finished.stderr
    assert "SESSION_DONE" in finished.stdout
    assert "Exception ignored" not in finished.stderr, finished.stderr
