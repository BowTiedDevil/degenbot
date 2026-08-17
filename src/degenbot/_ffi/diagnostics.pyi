def start_gil_probe(interval_ms: int, threshold_ms: int, stuck_ms: int) -> None: ...
def mark_progress() -> None: ...

__all__ = [
    "mark_progress",
    "start_gil_probe",
]
