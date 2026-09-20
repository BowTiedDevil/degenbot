"""FF-T1 (BPHR6F) — boot refusal is a typed error, never a process abort.

Red-first: at HEAD, booting the fleet on a host below the pinned-role floor
aborts the host process ([fleet-reg] unrecoverable — aborting, the
2026-09-11 CI failures on 4-vCPU runners). This test pins the FF-T1
contract: the child process must survive the refusal and surface a typed
error instead.

The child shrinks its own affinity to 1 CPU — the fleet quota is
min(cgroup quota, affinity), floored at 1.0 — simulating the
sub-SERIAL-floor condition (FF-T4, Z6XTDX: the 2-5-core tier now boots the
serial binding, so the typed refusal fires only below the tier floor).
The intake station boot is lazy: the first submit is what
triggers the fleet budget check.
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.skipif(
    not hasattr(os, "sched_setaffinity"), reason="Linux-only affinity simulation"
)

_CHILD = """
import os
import sys

# The fleet-posture holder is first-wins at module init: drop any inherited
# fleet env before importing degenbot (the delenv-fixture discipline, applied
# at the child seam).
os.environ.pop("DEGENBOT_FLEET", None)

# Simulate a sub-SERIAL-floor host (FF-T4, Z6XTDX): the 2-5-core tier
# now boots the serial binding, so the typed refusal fires only below
# the tier floor — one CPU floors the quota at 1.0 (< HOST_FLOOR_CORES).
os.sched_setaffinity(0, {0})

from degenbot._ffi import ArbitrageEngine, Bot

try:
    pre = Bot(1)
    ArbitrageEngine(py_bot=pre)  # engine construction installs the intake boot
    probe = Bot(1)
    if not probe.registration_fleet_hosted():
        print("RETUNE-NOT-INSTALLED", file=sys.stderr)
        sys.exit(4)
    # The registration intake station boots lazily: the first submit runs
    # the fleet budget check — the sub-floor refusal fires here.
    receipt = probe.submit_registration_unit(lambda: 1)
    receipt.wait(timeout=30.0)
except Exception as exc:
    print(f"BOOT-REFUSED {type(exc).__name__}: {exc}", file=sys.stderr)
    sys.exit(3)

print("FLEET-OK")
sys.exit(0)
"""


def test_boot_refusal_is_typed_never_abort() -> None:
    """A sub-floor boot raises a typed error; the process must survive."""
    proc = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true] — trusted binary, args list, no shell
        [sys.executable, "-c", _CHILD],
        capture_output=True,
        text=True,
        cwd=str(Path(__file__).parents[2]),
        timeout=120,
        check=False,
    )
    assert proc.returncode == 3, (
        "boot refusal must exit cleanly with the typed error; "
        f"returncode={proc.returncode} (a negative value means the process "
        "was killed by a signal — the library aborted instead of raising) "
        f"stdout={proc.stdout!r} stderr_tail={proc.stderr[-1500:]!r}"
    )
    assert "BOOT-REFUSED" in proc.stderr
    assert "BootRefused" in proc.stderr, (
        "the refusal must surface as the typed BootRefused error, carrying "
        "budget, floor, and an operator hint"
    )
