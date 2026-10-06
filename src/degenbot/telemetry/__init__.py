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
from typing import Protocol, override

from degenbot._ffi import flush_telemetry, shutdown_log_drainer

_AT_EXIT_LOGGER = "degenbot.telemetry"


class Teardown(Protocol):
    """The two teardown steps, in the order :func:`shutdown_telemetry` runs them.

    The flush executes while the runtime behind the providers is still up;
    the drainer stop ends the machinery. Tests install a recording double
    here instead of patching the module's FFI imports.

    """

    def flush(self) -> None: ...

    def stop_drainer(self) -> None: ...


class _FfiTeardown(Teardown):
    """The default :class:`Teardown` binding: the real FFI entry points."""

    @override
    def flush(self) -> None:
        flush_telemetry()

    @override
    def stop_drainer(self) -> None:
        shutdown_log_drainer()


def shutdown_telemetry(teardown: Teardown | None = None) -> None:
    """Run the one ordered telemetry teardown: flush, then stop the machinery.

    Idempotent and none-safe (telemetry off → no-op). Runs on live-callable
    state only: the flush executes while the runtime behind the providers is
    still up, so the final OTLP batch actually exports.

    A flush failure is logged and does NOT skip the teardown step — the
    ordering survives partial-startup and degraded-exporter states.

    Args:
        teardown: The teardown steps to run; the FFI entry points when
            omitted (the process-exit and runner-shutdown paths).

    """
    steps = _FfiTeardown() if teardown is None else teardown
    try:
        steps.flush()
    except Exception as exc:  # ruff:ignore[blind-except] — must not stop on a degraded exporter
        logging.getLogger(_AT_EXIT_LOGGER).debug("telemetry flush failed at shutdown: %r", exc)
    steps.stop_drainer()


def _at_exit(teardown: Teardown | None = None) -> None:
    """Process-exit handler: run the teardown, swallowing any error."""
    with contextlib.suppress(Exception):
        shutdown_telemetry(teardown)


atexit.register(_at_exit)

__all__ = ["flush_telemetry", "shutdown_telemetry"]
