"""Telemetry control surface — the stable ADR-013 home for provider teardown.

The ADR-043 section-6 contract — flush every provider (spans + metrics)
BEFORE the tokio runtime behind them is torn down, else the final OTLP batch
exports nothing — is owned HERE, not left to call-site prose:

- :func:`flush_telemetry` — the mid-run-safe flush (runner shutdown, any
  exit path that already knows it is torn down next).
- :func:`shutdown_telemetry` — the full teardown sequence: flush, then stop
  the runtime-backed log drainer + OTel provider. The one ordered teardown.
- :func:`_at_exit` — registered atexit at import, so every exit path
  (SIGINT without runner cleanup, driver crash, plain script end) flushes.

Leaf modules import from here, never from degenbot._ffi (ADR-013: the
Pydantic barrier — the _ffi seam is private to __init__.py files).
"""

from __future__ import annotations

import atexit
import contextlib
import logging

from degenbot._ffi import flush_telemetry, shutdown_log_drainer

_AT_EXIT_LOGGER = "degenbot.telemetry"


def shutdown_telemetry() -> None:
    """Run the one ordered telemetry teardown: flush, then stop the machinery.

    Idempotent and none-safe (telemetry off → no-op). Runs on live-callable
    state only: the flush executes while the runtime behind the providers is
    still up, so the final OTLP batch actually exports.

    A flush failure is logged and does NOT skip the teardown step — the
    ordering survives partial-startup and degraded-exporter states.

    """
    try:
        flush_telemetry()
    except Exception as exc:  # ruff:ignore[blind-except] — must not stop on a degraded exporter
        logging.getLogger(_AT_EXIT_LOGGER).debug("telemetry flush failed at shutdown: %r", exc)
    shutdown_log_drainer()


def _at_exit() -> None:
    """Process-exit handler: run the teardown, swallowing any error."""
    with contextlib.suppress(Exception):
        shutdown_telemetry()


atexit.register(_at_exit)

__all__ = ["flush_telemetry", "shutdown_telemetry"]
