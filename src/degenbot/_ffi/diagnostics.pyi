"""Stub for the dynamically-created ``degenbot._ffi.diagnostics`` submodule.

Created at runtime by ``add_diagnostics_module`` in the PyO3 wrapper crate
(``degenbot-python/src/diagnostics/gil_probe.rs``). Holds the GIL-probe
diagnostics pyfunctions the backrun bot uses to watchdog the main loop.
"""

def start_gil_probe(interval_ms: int, threshold_ms: int, stuck_ms: int) -> None: ...
def mark_progress() -> None: ...

__all__ = [
    "mark_progress",
    "start_gil_probe",
]
