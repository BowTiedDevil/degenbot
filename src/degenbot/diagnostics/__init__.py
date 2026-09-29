"""Runtime diagnostics (GIL-probe stuck-watchdog) — stable ADR-013 home.

The settlement-arbitrage bot installs a GIL-acquire-latency probe + main-loop stuck-watchdog
(``start_gil_probe`` / ``mark_progress``). These pyfunctions are dynamically
created in ``degenbot._ffi.diagnostics`` by the PyO3 wrapper; this ``__init__.py``
is the stable ``degenbot.<domain>`` home leaf modules must import from (ADR-013:
the Pydantic barrier — the ``_ffi`` seam is private to ``__init__.py`` files).
"""

from enum import StrEnum

from degenbot._ffi import diagnostics as _diagnostics
from degenbot.exceptions.base import DegenbotValueError


class FailureAction(StrEnum):
    """The closed per-bucket failure-reaction vocabulary (ADR-040).

    The Rust ``failure_policy`` matrix is the single source of truth; its
    ``failure_action`` pyfunction answers one of these spellings. This enum is
    the ONE wire-string → member conversion — consumers compare members, never
    raw strings.
    """

    OBSERVE = "observe"
    EVENT = "event"
    QUARANTINE = "quarantine"
    EXIT = "exit"


def failure_action(kind: str, reason: str | None = None) -> FailureAction:
    """Consult the Rust failure-policy matrix for one bucket's action.

    Args:
        kind: The failure bucket (e.g. ``"sim_failure"``).
        reason: The optional reason sub-split.

    Returns:
        The bucket's action as a :class:`FailureAction` member.

    Raises:
        DegenbotValueError: On a wire string outside the FailureAction
            vocabulary — Rust/Python matrix drift, loud.

    """
    raw = _diagnostics.failure_action(kind, reason)
    try:
        return FailureAction(raw)
    except ValueError as e:
        msg = f"Unrecognized failure action {raw!r} from the Rust failure_policy matrix."
        raise DegenbotValueError(message=msg) from e


mark_progress = _diagnostics.mark_progress
start_gil_probe = _diagnostics.start_gil_probe

__all__ = ["FailureAction", "failure_action", "mark_progress", "start_gil_probe"]
