"""FF-T5 (NT7HJC): the runtime fleet status - budget, plan, census.

"degenbot.runtime_status()" is the operator's live-process view. These
tests pin the FF-T5 contract, PARAMETRIZED over the fleet profile (the CI
matrix in miniature: "auto" - the real host path, whatever the quota
resolves to - and forced "pinned" - the operator override, marked
oversubscribed on sub-floor hosts):

- the status names the boot's profile, the resolved binding (the FF-T2
  tier table), and the oversubscription mark;
- the projected budget carries the seat/share table;
- the census rows carry the lane-to-thread binding per resource;
- the tier-refusal string names the pinned floor a serial host fell from.

Determinism (the FF-T5 addendum): every leg runs in a SUBPROCESS with its
own env - the parametrization never leaks a profile into the session, and
the parent never depends on which in-process test constructed an engine
first (the pytest-randomly/xdist ordering wobble the addendum names).
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import pytest


def _cgroup_v2_quota() -> float | None:
    """The tightest cgroup v2 cpu.max ratio on this process's path."""
    try:
        cgroup_text = Path("/proc/self/cgroup").read_text()
        mounts_text = Path("/proc/self/mounts").read_text()
    except OSError:
        return None
    rel = next(
        (
            line.removeprefix("0::").strip()
            for line in cgroup_text.splitlines()
            if line.startswith("0::")
        ),
        None,
    )
    if rel is None:
        return None
    root = next(
        (
            line.split()[1]
            for line in mounts_text.splitlines()
            if line.split()[1] != "/" and line.split()[2] == "cgroup2"
        ),
        None,
    )
    if root is None:
        return None
    start = Path(root) / rel.lstrip("/")
    tightest: float | None = None
    node = start if rel.strip("/") else Path(root)
    while True:
        try:
            parts = (node / "cpu.max").read_text().split()
        except OSError:
            parts = []
        if parts and parts[0] != "max":
            try:
                quota = float(parts[0])
                period = float(parts[1]) if len(parts) > 1 else 100_000.0
            except ValueError:
                period = 0.0
                quota = 0.0
            if period > 0 and quota > 0:
                ratio = quota / period
                tightest = ratio if tightest is None or ratio < tightest else tightest
        if node == Path(root) or Path(root) not in node.parents:
            break
        node = node.parent
    return tightest


def _fractional_quota_cpus() -> float:
    """The fleet quota mirror: min(tightest cgroup quota, affinity), >= 1."""
    cgroup = _cgroup_v2_quota()
    affinity = float(len(os.sched_getaffinity(0))) if hasattr(os, "sched_getaffinity") else None
    candidates = [q for q in (cgroup, affinity) if q is not None]
    return max(min(candidates), 1.0) if candidates else 1.0


_CHILD = """
import os
import sys

# Determinism (the FF-T5 addendum): the child controls its own env - pop
# the retired stance key, then apply THIS leg's profile.
os.environ.pop("DEGENBOT_FLEET", None)
if @SET_PROFILE@:
    os.environ["DEGENBOT_FLEET_PROFILE"] = @SET_PROFILE@
else:
    os.environ.pop("DEGENBOT_FLEET_PROFILE", None)

from degenbot._ffi import ArbitrageEngine, Bot

pre = Bot(1)
ArbitrageEngine(py_bot=pre)  # engine construction stamps the fleet boot

import degenbot

s = degenbot.runtime_status()
assert s["fleet_booted"] is True, f"an engine constructed: {s!r}"
assert s["profile"] == @PROFILE@, f"the boot's profile: {s['profile']!r}"

quota_floor = max(int(@QUOTA@ // 1), 1)
if @PROFILE@ == "pinned":
    expected_binding = "pinned"
else:
    # The FF-T2 tier table: pinned >= 6 cores, serial 2-5.
    expected_binding = "pinned" if quota_floor >= 6 else "serial"
assert s["binding"] == expected_binding, (
    f"the {s['profile']!r} host (quota_floor={quota_floor}) resolves "
    f"{expected_binding!r}, got {s['binding']!r}"
)
if @PROFILE@ == "pinned" and quota_floor < 6:
    assert s["oversubscribed"] is True, "a forced pinned sub-floor host runs MARKED"
    assert s["tier_refused"] and "QuotaTooSmallForPinnedRoles" in s["tier_refused"], (
        "the tier refusal names the pinned floor it fell from"
    )

budget = s["budget"]
for key in (
    "quota_cpus",
    "quota_floor",
    "reserve_cpus",
    "ambient_cpus",
    "resolve_cpus",
    "merge_cpus",
    "solver_cpus",
    "solver_pin_count",
    "sim_slot_cap",
    "pool_state_updater_slots",
):
    assert key in budget, f"the projected budget carries {key}"

assert s["census"], "the census rows are present"
for row in s["census"]:
    assert row["binding"] in ("pinned", "shared", "logical"), row
    assert row["resource"], row

print("STATUS-OK " + s["binding"])
"""


@pytest.mark.parametrize("profile", ["auto", "pinned"])
def test_runtime_status_reports_the_plan_budget_and_census(profile: str) -> None:
    """The status dict: profile, tier binding, budget, census (per leg)."""
    child = (
        _CHILD
        .replace("@PROFILE@", repr(profile))
        .replace("@SET_PROFILE@", repr(None if profile == "auto" else profile))
        .replace("@QUOTA@", repr(_fractional_quota_cpus()))
    )
    proc = subprocess.run(
        [sys.executable, "-c", child],
        capture_output=True,
        text=True,
        cwd=str(Path(__file__).parents[2]),
        timeout=120,
        check=False,
    )
    assert "STATUS-OK" in proc.stdout, f"stdout={proc.stdout!r} stderr_tail={proc.stderr[-1500:]!r}"


def test_runtime_status_before_any_engine_is_the_default_projection() -> None:
    """Pre-construction: fleet_booted False + the live default projection."""
    child = """
import os
os.environ.pop("DEGENBOT_FLEET", None)
os.environ.pop("DEGENBOT_FLEET_PROFILE", None)
import degenbot
s = degenbot.runtime_status()
assert s["fleet_booted"] is False, s
assert s["profile"] == "auto", s
assert s["binding"] in ("pinned", "serial"), s
# Import installs no runtimes or threads, so the census is EMPTY until
# driver_boot() — that emptiness IS the default projection.
assert s["census"] == [], s
print("PRE-OK")
"""
    proc = subprocess.run(
        [sys.executable, "-c", child],
        capture_output=True,
        text=True,
        cwd=str(Path(__file__).parents[2]),
        timeout=120,
        check=False,
    )
    assert "PRE-OK" in proc.stdout, f"stdout={proc.stdout!r} stderr_tail={proc.stderr[-1500:]!r}"


def test_runtime_status_after_driver_boot_populates_the_census() -> None:
    """driver_boot() installs the shared runtime; the census row appears."""
    child = """
import os
os.environ.pop("DEGENBOT_FLEET", None)
os.environ.pop("DEGENBOT_FLEET_PROFILE", None)
import degenbot
from degenbot.bot import driver_boot
before = degenbot.runtime_status()["census"]
driver_boot()
after = degenbot.runtime_status()["census"]
assert before == [], before
assert any(row["resource"] == "io_runtime_workers" for row in after), after
print("BOOT-OK")
"""
    proc = subprocess.run(

        [sys.executable, "-c", child],

        capture_output=True,

        text=True,

        cwd=str(Path(__file__).parents[2]),

        timeout=120,

        check=False,

    )

    assert "BOOT-OK" in proc.stdout, f"stdout={proc.stdout!r} stderr_tail={proc.stderr[-1500:]!r}"
