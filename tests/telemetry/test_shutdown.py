"""Telemetry shutdown ordering.

The ADR-043 section-6 contract (flush providers BEFORE the runtime-backed
telemetry machinery is torn down) is owned by degenbot.telemetry.shutdown_telemetry
— the one teardown sequence, registered atexit at import, so every exit path
(SIGINT without runner cleanup, driver crash, plain script end) flushes the
OTLP tail instead of losing it to runtime teardown.
"""

from __future__ import annotations

import atexit
import contextlib
import importlib
import logging
from typing import Any

import pytest  # ruff:ignore[typing-only-third-party-import] — runtime marks

import degenbot.telemetry as telemetry_mod
from tests.fakes.telemetry import FakeTelemetryTeardown

_FLUSH_FAIL = RuntimeError("exporter down")


class TestShutdownTelemetry:
    """The single teardown sequence: flush first, drainer stop second."""

    def test_flushes_before_stopping_drainer(self) -> None:
        fake = FakeTelemetryTeardown()

        telemetry_mod.shutdown_telemetry(fake)

        assert fake.calls == ["flush", "drainer"]

    def test_flush_error_still_stops_drainer(
        self,
        caplog: pytest.LogCaptureFixture,
    ) -> None:
        """A failed flush must not skip the teardown step (order survives)."""
        fake = FakeTelemetryTeardown()
        fake.flush_error = _FLUSH_FAIL

        with caplog.at_level(logging.DEBUG):
            telemetry_mod.shutdown_telemetry(fake)

        # The teardown survived the failed flush (drainer stopped either way).
        assert fake.calls == ["flush", "drainer"]

    def test_idempotent(self) -> None:
        fake = FakeTelemetryTeardown()
        telemetry_mod.shutdown_telemetry(fake)
        telemetry_mod.shutdown_telemetry(fake)
        assert fake.calls == ["flush", "drainer", "flush", "drainer"]


class TestAtexitRegistration:
    """Importing the module arms the process-exit flush."""

    def test_import_registers_atexit_handler(self, monkeypatch: pytest.MonkeyPatch) -> None:
        registered: list[Any] = []

        def capture(handler: Any) -> None:
            registered.append(handler)

        orig_register = atexit.register
        monkeypatch.setattr(atexit, "register", capture)
        try:
            importlib.reload(telemetry_mod)
            assert registered, "atexit.register was not called at import"
        finally:
            # Restore genuine registration state (the handler is idempotent).
            monkeypatch.undo()
            orig_register(telemetry_mod._at_exit)
            with contextlib.suppress(ValueError):  # handler re-registration on reload
                importlib.reload(telemetry_mod)

    def test_atexit_handler_swallows_errors(self) -> None:
        """Atexit semantics: the shutdown path can never raise out."""
        fake = FakeTelemetryTeardown()
        fake.flush_error = RuntimeError("no runtime")
        fake.drainer_error = RuntimeError("gone")

        telemetry_mod._at_exit(fake)  # must not raise

        # The teardown ran to its end despite both steps failing.
        assert fake.calls == ["flush", "drainer"]
