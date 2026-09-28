"""Fleet posture — the stable mirror home for the operator re-tune channel.

The operator re-tune channel (Part B): an operator re-tunes the LIVE fleet cordon
thresholds (the six typed `DEGENBOT_FLEET_CORDON_*` keys) on a running bot
without touching its process. This home is the ADR-013 Pydantic barrier for
the `degenbot._ffi.fleet` seam — the FIRST Python consumer mints the home,
and no leaf module imports `degenbot._ffi` directly.

Two public functions:

- :func:`set_posture_thresholds` — apply a partial patch over the six
  typed thresholds to the live process posture policy and return the
  effective policy (all six fields + the current `Nominal|Cordoned`
  posture). Every change emits one loud `tracing::warn!` line listing
  old -> new per changed key (Rust side). Validation lives ONCE in the Rust
  core (`PosturePolicyPatch::validate`): unknown keys, an empty patch,
  wrong value types, and out-of-range thresholds raise the typed
  :class:`PostureRetuneError` — rejected, never clamped.
- :func:`current_posture` — the read side: the effective policy + current
  posture with no write.

Boot config stays the default source: `FleetBoot::from_config` projects
the typed keys into the process owner at boot; this surface only re-tunes
the live policy on top of it.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from collections.abc import Mapping

from degenbot._ffi.fleet import PostureRetuneError
from degenbot._ffi.fleet import current_posture_policy as _ffi_current_posture_policy
from degenbot._ffi.fleet import set_posture_policy as _ffi_set_posture_policy

__all__ = (
    "POSTURE_THRESHOLD_KEYS",
    "PostureRetuneError",
    "current_posture",
    "set_posture_thresholds",
)

#: The six typed cordon-threshold key names a patch may carry (the
#: degenbot-config `fleet` section; env `DEGENBOT_FLEET_CORDON_*`).
#: `cordon_sim_intake_floor` is `opt usize`: an explicit `None` value
#: clears the override (back to half the slot cap).
POSTURE_THRESHOLD_KEYS = (
    "cordon_enter_events",
    "cordon_enter_window_ms",
    "cordon_duty_percent",
    "cordon_duty_window_ms",
    "cordon_exit_clean_ms",
    "cordon_sim_intake_floor",
)


def set_posture_thresholds(patch: Mapping[str, int | float | None]) -> dict[str, Any]:
    """Re-tune the LIVE fleet posture thresholds with a partial patch.

    Args:
        patch: A mapping over :data:`POSTURE_THRESHOLD_KEYS` — every key is
            optional (absent = keep the current value) and at least one is
            required. `cordon_sim_intake_floor` accepts an int or `None`
            (`None` = clear the override; the cordon intake cap returns to
            half the slot cap).

    Returns:
        The effective policy as a dict: all six threshold fields plus
        `posture` (`"Nominal"` | `"Cordoned"`).

    The Rust validator refuses (never clamps) an unknown key, an empty
    patch, a wrong-typed value, or an out-of-range threshold (windows > 0
    ms; duty percent in (0.0, 100.0]; enter events >= 1; floor >= 1 when
    set) by raising :class:`PostureRetuneError` — the live policy is
    untouched on any refusal.

    """
    return _ffi_set_posture_policy(dict(patch))


def current_posture() -> dict[str, Any]:
    """Return the live effective policy + current posture (no write).

    Returns:
        The same dict shape :func:`set_posture_thresholds` returns.

    """
    return _ffi_current_posture_policy()
