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

_FLUSH_FAIL = RuntimeError("exporter down")


class _Recorder:
    """Record call order across the FFI stand-ins."""

    def __init__(self) -> None:
        self.calls: list[str] = []

    def flush(self) -> None:
        self.calls.append("flush")

    def stop_drainer(self) -> None:
        self.calls.append("drainer")


def _install_fakes(monkeypatch: pytest.MonkeyPatch, recorder: _Recorder) -> None:
    monkeypatch.setattr(telemetry_mod, "flush_telemetry", recorder.flush)
    monkeypatch.setattr(telemetry_mod, "shutdown_log_drainer", recorder.stop_drainer)


class TestShutdownTelemetry:
    """The single teardown sequence: flush first, drainer stop second."""

    def test_flushes_before_stopping_drainer(self, monkeypatch: pytest.MonkeyPatch) -> None:
        recorder = _Recorder()
        _install_fakes(monkeypatch, recorder)

        telemetry_mod.shutdown_telemetry()

        assert recorder.calls == ["flush", "drainer"]

    def test_flush_error_still_stops_drainer(
        self,
        monkeypatch: pytest.MonkeyPatch,
        caplog: pytest.LogCaptureFixture,
    ) -> None:
        """A failed flush must not skip the teardown step (order survives)."""
        recorder = _Recorder()

        def boom() -> None:
            raise _FLUSH_FAIL

        monkeypatch.setattr(telemetry_mod, "flush_telemetry", boom)
        _install_fakes(monkeypatch, recorder)
        with caplog.at_level(logging.DEBUG):
            telemetry_mod.shutdown_telemetry()

        # The teardown survived the failed flush (drainer stopped either way).
        assert recorder.calls[-1] == "drainer"

    def test_idempotent(self, monkeypatch: pytest.MonkeyPatch) -> None:
        recorder = _Recorder()
        _install_fakes(monkeypatch, recorder)
        telemetry_mod.shutdown_telemetry()
        telemetry_mod.shutdown_telemetry()
        assert recorder.calls == ["flush", "drainer", "flush", "drainer"]


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

    def test_atexit_handler_swallows_errors(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """Atexit semantics: the shutdown path can never raise out."""

        flush_fail = RuntimeError("no runtime")
        stop_fail = RuntimeError("gone")

        def raise_flush() -> None:
            raise flush_fail

        def raise_stop() -> None:
            raise stop_fail

        monkeypatch.setattr(telemetry_mod, "flush_telemetry", raise_flush)
        monkeypatch.setattr(telemetry_mod, "shutdown_log_drainer", raise_stop)
        telemetry_mod._at_exit()  # must not raise
