"""Type stubs for the degenbot Rust fleet seam.

Python module: `degenbot._ffi.fleet`
Rust: `crates/shells/degenbot-python/src/fleet.rs` (feature = "simulation")

The JCI2FW Part B operator re-tune channel over the ONE process-level fleet
posture owner (`degenbot_workers::posture::process`). The Python mirror
home is `degenbot.fleet` (ADR-013 Pydantic barrier); the operator ops are
`set_fleet_posture` / `get_fleet_posture` on the operator channel.
"""

from typing import Any

class PostureRetuneError(ValueError):
    """The re-tune channel refused the posture patch.

    Raised for an unknown threshold key, a non-dict patch, an empty patch,
    a wrong-typed value, or a threshold outside its typed range (windows
    must be > 0 ms, duty percent in (0.0, 100.0], `cordon_enter_events >= 1`,
    `cordon_sim_intake_floor >= 1` when set). REJECT, never clamp: the live
    policy is untouched when this fires. Subclasses `ValueError` so broad
    handlers keep working; classify by type.
    """

def set_posture_policy(patch: dict[str, Any]) -> dict[str, Any]:
    """Apply a partial patch over the six typed cordon thresholds.

    `patch` maps threshold key names to values: `cordon_enter_events`
    (int), `cordon_enter_window_ms` (int ms), `cordon_duty_percent`
    (float), `cordon_duty_window_ms` (int ms), `cordon_exit_clean_ms`
    (int ms), and `cordon_sim_intake_floor` (int, or `None` to clear the
    override and return to half the slot cap). Absent keys keep the current
    value; an empty patch raises. Boot config stays the default source —
    this re-tunes the LIVE policy only.

    Returns the effective policy: all six fields plus the current
    `posture` ("Nominal" | "Cordoned"). Every change emits one loud
    `tracing::warn!` line listing old -> new per changed key.
    """

def current_posture_policy() -> dict[str, Any]:
    """Read the effective policy + current posture (no write)."""

__all__ = [
    "PostureRetuneError",
    "current_posture_policy",
    "set_posture_policy",
]
