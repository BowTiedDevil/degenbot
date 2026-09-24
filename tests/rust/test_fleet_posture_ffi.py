"""Fleet posture re-tune verb through the compiled FFI (JCI2FW Part B).

The validation rules live ONCE in the Rust core
(`PosturePolicyPatch::validate`, unit-tested in
`rust/crates/engine/degenbot-workers/src/posture.rs`); this file exercises them
through the compiled `degenbot._ffi.fleet` verb — the Python surface of the
same rules (typed `PostureRetuneError`, partial-patch merge, the tri-state
sim-intake floor, and the effective-policy echo shape).

Every test snapshots + restores the process-global live policy (the ONE
owner), so ordering cannot leak state.
"""

from __future__ import annotations

import pytest

from degenbot._ffi import fleet

_ECHO_FIELDS = (
    "cordon_enter_events",
    "cordon_enter_window_ms",
    "cordon_duty_percent",
    "cordon_duty_window_ms",
    "cordon_exit_clean_ms",
    "cordon_sim_intake_floor",
)


@pytest.fixture(autouse=True)
def _restore_live_posture_policy():
    """Snapshot + restore the process-global policy around each test."""
    before = fleet.current_posture_policy()
    yield
    patch = {key: before[key] for key in _ECHO_FIELDS}
    fleet.set_posture_policy(patch)


def test_effective_echo_carries_all_six_fields_plus_posture() -> None:
    """The echo shape: six typed fields + the Nominal|Cordoned posture."""
    effective = fleet.current_posture_policy()
    assert tuple(effective) == (*_ECHO_FIELDS, "posture")
    assert effective["posture"] in ("Nominal", "Cordoned")


def test_partial_patch_merges_over_the_live_policy() -> None:
    """Absent keys keep the live value; supplied keys land."""
    before = fleet.current_posture_policy()
    effective = fleet.set_posture_policy({"cordon_enter_events": 9})
    assert effective["cordon_enter_events"] == 9
    for key in _ECHO_FIELDS:
        if key != "cordon_enter_events":
            assert effective[key] == before[key], key


def test_set_then_clear_the_sim_intake_floor() -> None:
    """The floor key's tri-state: set an override, clear it back to None."""
    effective = fleet.set_posture_policy({"cordon_sim_intake_floor": 3})
    assert effective["cordon_sim_intake_floor"] == 3
    # An explicit None value CLEARS the override (the key's typed type is
    # opt usize: None = half the slot cap).
    effective = fleet.set_posture_policy({"cordon_sim_intake_floor": None})
    assert effective["cordon_sim_intake_floor"] is None
    # An absent key keeps the (cleared) value.
    effective = fleet.set_posture_policy({"cordon_enter_events": 1})
    assert effective["cordon_sim_intake_floor"] is None


def test_an_empty_patch_is_refused() -> None:
    """No key supplied: the typed EmptyPatch refusal."""
    with pytest.raises(
        fleet.PostureRetuneError, match="at least one cordon threshold key"
    ) as exc_info:
        fleet.set_posture_policy({})
    assert isinstance(exc_info.value, ValueError)


def test_an_unknown_key_is_refused() -> None:
    """A non-threshold key never reaches the core policy."""
    with pytest.raises(fleet.PostureRetuneError, match="unknown fleet-posture threshold"):
        fleet.set_posture_policy({"cordon_bogus": 1})


def test_the_posture_key_is_read_only() -> None:
    """The echoed posture label is never a patchable key."""
    with pytest.raises(fleet.PostureRetuneError, match="read-only"):
        fleet.set_posture_policy({"posture": "Cordoned"})


@pytest.mark.parametrize(
    ("key", "value", "message"),
    [
        ("cordon_enter_events", 0, "cordon_enter_events must be >= 1"),
        ("cordon_enter_window_ms", 0, "cordon_enter_window_ms must be > 0 ms"),
        ("cordon_duty_window_ms", 0, "cordon_duty_window_ms must be > 0 ms"),
        ("cordon_exit_clean_ms", 0, "cordon_exit_clean_ms must be > 0 ms"),
        ("cordon_duty_percent", 0.0, "cordon_duty_percent must be in"),
        ("cordon_duty_percent", -0.5, "cordon_duty_percent must be in"),
        ("cordon_duty_percent", 100.5, "cordon_duty_percent must be in"),
        ("cordon_sim_intake_floor", 0, "cordon_sim_intake_floor must be >= 1"),
    ],
)
def test_range_rules_refuse_with_typed_messages(key, value, message) -> None:
    """Every threshold rule rejects with its typed message (never clamps)."""
    with pytest.raises(fleet.PostureRetuneError, match=message):
        fleet.set_posture_policy({key: value})


def test_duty_percent_inclusive_ceiling_is_admitted() -> None:
    """100.0 is the inclusive duty ceiling (a legal threshold)."""
    effective = fleet.set_posture_policy({"cordon_duty_percent": 100.0})
    assert effective["cordon_duty_percent"] == pytest.approx(100.0)


@pytest.mark.parametrize(
    ("key", "value"),
    [
        ("cordon_enter_events", True),
        ("cordon_enter_events", "7"),
        ("cordon_enter_events", 7.5),
        ("cordon_enter_window_ms", True),
        ("cordon_duty_percent", True),
        ("cordon_duty_percent", "2"),
        ("cordon_sim_intake_floor", True),
        ("cordon_sim_intake_floor", 2.5),
    ],
)
def test_wrong_types_are_refused_not_coerced(key, value) -> None:
    """REJECT, never coerce: bools/strs/floats-for-ints never land."""
    with pytest.raises(fleet.PostureRetuneError, match="expected"):
        fleet.set_posture_policy({key: value})


def test_a_refused_patch_never_lands() -> None:
    """A refusal leaves the live policy byte-identical."""
    before = fleet.current_posture_policy()
    with pytest.raises(fleet.PostureRetuneError):
        fleet.set_posture_policy({"cordon_enter_events": 0, "cordon_duty_percent": 5.0})
    assert fleet.current_posture_policy() == before


def test_a_multi_key_patch_applies_together() -> None:
    """Several keys in one patch merge in one atomic swap."""
    before = fleet.current_posture_policy()
    effective = fleet.set_posture_policy({
        "cordon_enter_events": 4,
        "cordon_enter_window_ms": before["cordon_enter_window_ms"],
        "cordon_duty_percent": 1.5,
        "cordon_duty_window_ms": before["cordon_duty_window_ms"],
        "cordon_exit_clean_ms": before["cordon_exit_clean_ms"],
        "cordon_sim_intake_floor": before["cordon_sim_intake_floor"],
    })
    assert effective["cordon_enter_events"] == 4
    assert effective["cordon_duty_percent"] == pytest.approx(1.5)
    assert fleet.current_posture_policy() == effective
