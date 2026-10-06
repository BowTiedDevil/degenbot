"""The telemetry teardown fake: call-order recording without the FFI.

``shutdown_telemetry`` takes its two steps through the ``Teardown`` seam, so
a test installs this double instead of patching the module's FFI imports.
The recorded call order is the flush-before-drainer contract; either step
can be armed to raise, so the degraded-exporter paths run the same code the
process-exit path runs.
"""

from __future__ import annotations


class FakeTelemetryTeardown:
    """In-memory ``Teardown`` double: records call order, never touches the FFI.

    ``flush_error`` / ``drainer_error`` arm the corresponding step to raise
    (a degraded exporter, a gone drainer) on top of recording the call.

    """

    def __init__(self) -> None:
        self.calls: list[str] = []
        self.flush_error: Exception | None = None
        self.drainer_error: Exception | None = None

    def flush(self) -> None:
        self.calls.append("flush")
        if self.flush_error is not None:
            raise self.flush_error

    def stop_drainer(self) -> None:
        self.calls.append("drainer")
        if self.drainer_error is not None:
            raise self.drainer_error
