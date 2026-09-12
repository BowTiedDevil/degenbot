"""Telemetry control surface - the stable ADR-013 home for the provider flush.

ADR-043 section 6: every telemetry provider (spans + metrics) must be flushed
BEFORE the tokio runtime behind it is torn down, otherwise the final OTLP batch
exports nothing and the tail of a run is silently lost. Leaf modules import
``flush_telemetry`` from here, never from ``degenbot._ffi`` (ADR-013: the
Pydantic barrier - the ``_ffi`` seam is private to ``__init__.py`` files).
"""

from degenbot._ffi import flush_telemetry

__all__ = ["flush_telemetry"]
