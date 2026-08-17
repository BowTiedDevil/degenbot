"""Runtime diagnostics (GIL-probe stuck-watchdog) — stable ADR-013 home.

The settlement-arbitrage bot installs a GIL-acquire-latency probe + main-loop stuck-watchdog
(``start_gil_probe`` / ``mark_progress``). These pyfunctions are dynamically
created in ``degenbot._ffi.diagnostics`` by the PyO3 wrapper; this ``__init__.py``
is the stable ``degenbot.<domain>`` home leaf modules must import from (ADR-013:
the Pydantic barrier — the ``_ffi`` seam is private to ``__init__.py`` files).
"""

from degenbot._ffi import diagnostics as _diagnostics

mark_progress = _diagnostics.mark_progress
start_gil_probe = _diagnostics.start_gil_probe

__all__ = ["mark_progress", "start_gil_probe"]
