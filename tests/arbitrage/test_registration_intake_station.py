"""PRG-3 (IRUMXD): the fleet registration intake station.

Under the fleet stance (`fleet.stance=fleet`) the crawl's pool-build
consumers ride the fleet's duty-counted `PoolStateUpdater` seats (census
row `fleet_pool_state_updater_slots`, Deferrable cordon class) instead of
a private `ThreadPoolExecutor`. These tests prove the surfaces:

- the FFI station end to end (subprocess with the stance env — the boot
  descriptor installs at module init + engine construction, so the
  process-wide stance cannot be flipped in-process): callables execute on
  named `work-fleet-poolupd-{n}` seats and results ride the receipt;
- the legacy stance keeps the incumbent worker pool (`submit_registration_unit`
  refuses loudly);
- the pipeline refuses to construct at all under a legacy stance (PRG-5:
  the hard cutover — fleet-hosted only, actionable refusal).
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest

from degenbot._ffi import Bot


@pytest.fixture(autouse=True)
def _no_leaked_fleet_stance_env(monkeypatch: pytest.MonkeyPatch) -> None:
    """Kill the retired DEGENBOT_FLEET stance env before any child boots.

    Pre-approved determinism fix (FF-T5 addendum, folded at FF-T1's xfail
    gating): the key is RETIRED and fails the config load loudly if
    exported, but the suite can still leak it across test orderings under
    pytest-randomly/xdist — the child subprocesses here inherit whatever
    the worker carries. Pop it so the fleet-station family is deterministic
    regardless of which test ran before it in this process.
    """
    monkeypatch.delenv("DEGENBOT_FLEET", raising=False)


def _cgroup_v2_quota() -> float | None:
    """The tightest cgroup v2 cpu.max ratio on this process's path, or None."""
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
    root = next(
        (
            line.split()[1]
            for line in mounts_text.splitlines()
            if len(line.split()) > 2 and line.split()[2] == "cgroup2"
        ),
        None,
    )
    if rel is None or root is None:
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
    """Mirror of degenbot-workers quota.rs fractional_cpu_budget (read-only).

    The fleet budget's sole sizing authority: min(tightest cgroup quota,
    affinity), floored at 1.0. Used only to predict, from the parent,
    whether the pinned-role floor would refuse this host — the child
    subprocess stays the authority for what actually boots.
    """
    cgroup = _cgroup_v2_quota()
    affinity = float(len(os.sched_getaffinity(0))) if hasattr(os, "sched_getaffinity") else None
    candidates = [q for q in (cgroup, affinity) if q is not None]
    return max(min(candidates), 1.0) if candidates else 1.0


def _pinned_floor_refused() -> bool:
    """Would the DEFAULT-override pinned-role floor refuse this host?

    Mirrors budget.rs derive_table with default overrides: H=1,
    A=max(1, (floor(Q)-H)//4), R=1, M=1, and the 2-core Solver minimum —
    refused iff base + 2 > floor(Q). True on sub-floor hosts (the 4-vCPU
    CI runners); False on the 8-core devcontainers. The station test
    xfails under this condition (strict) until the serial arm lands.
    """
    quota_floor = max(int(_fractional_quota_cpus() // 1), 1)
    reserve = 1
    ambient = max(1, (quota_floor - reserve) // 4)
    base = reserve + ambient + 1 + 1
    return base + 2 > quota_floor


_FLEET_DRIVER = """
import threading

from degenbot._ffi import ArbitrageEngine, Bot

# The stance is read at module init (first-wins holder): this process boots
# WITH the fleet stance, and the ENGINE construction installs the intake
# boot descriptor (same typed config).
pre = Bot(1)
assert pre.registration_fleet_hosted() is False, 'pre-engine'
ArbitrageEngine(py_bot=pre)  # engine construction installs the intake boot
probe = Bot(1)
assert probe.registration_fleet_hosted() is True, 'post-engine'

names: set[str] = set()
lock = threading.Lock()

def work(i: int) -> int:
    with lock:
        names.add(threading.current_thread().name)
    return i * 2

receipts = [probe.submit_registration_unit(lambda i=i: work(i)) for i in range(16)]
assert [r.wait(timeout=30.0) for r in receipts] == [i * 2 for i in range(16)]
assert names, 'units executed'
# FF-T4 (Z6XTDX): the seat names are BINDING-dependent — the pinned
# binding runs the pooled PoolStateUpdater seats (work-fleet-poolupd-{n});
# the serial binding (2-5-core hosts) runs the ONE named cycle seat
# (work-fleet-serial-0). Both are fleet seats: the units must never run
# anywhere else.
assert all(
    n.startswith('work-fleet-poolupd-') or n == 'work-fleet-serial-0'
    for n in names
), names
# Receipt probes: done() flips before result() delivers.
assert all(r.done() for r in receipts)
print('FLEET-OK')
"""


# FF-T1 (BPHR6F) kept this strict-xfail while the serial arm was pending:
# on a sub-floor host the station subprocess surfaced the TYPED
# BootRefused refusal instead of SIGABRT-ing the worker. FF-T4 (Z6XTDX)
# LANDED the serial binding — the 2-5-core tier now boots (the ONE named
# cycle seat above), so the strict mark is RETIRED: the station runs (and
# must pass) on every tier the plan admits. FF-T5 folds the
# profile-parametrized rewrite over this same shape.
def test_fleet_station_executes_callables_on_named_fleet_seats() -> None:
    """End-to-end: stance env -> boot install -> named-seat receipts."""
    proc = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true] — trusted binary, args list, no shell
        [sys.executable, "-c", _FLEET_DRIVER],
        capture_output=True,
        text=True,
        cwd=str(Path(__file__).parents[2]),
        # PRG-3 test originally injected DEGENBOT_FLEET=fleet; that env var was
        # retired loudly by the CQLMM2 stance cutover (config cutover JLFE2F,
        # commit 2729b52bf) — the worker fleet is now the ONLY behavior, so the
        # subprocess just inherits the env and boots fleet-hosted by default.
        timeout=120,
        check=False,
    )
    assert "FLEET-OK" in proc.stdout, f"stdout={proc.stdout!r} stderr={proc.stderr!r}"


_LEGACY_CHILD = """
import os
import sys

# FF-T5 determinism (the addendum): a FRESH process - no engine
# constructed, no stance env - so the legacy-stance view is what the
# child sees, regardless of which in-process test ran first under
# pytest-randomly/xdist (the pass/skip wobble is dead by construction).
os.environ.pop("DEGENBOT_FLEET", None)
os.environ.pop("DEGENBOT_FLEET_PROFILE", None)

from degenbot._ffi import Bot

bot = Bot(1)
assert bot.registration_fleet_hosted() is False, "a fresh process boots legacy"
try:
    bot.submit_registration_unit(lambda: 1)
except RuntimeError as exc:
    assert "not fleet-hosted" in str(exc), exc
else:
    raise AssertionError("the legacy stance must refuse the fleet intake")

from types import SimpleNamespace
from degenbot.runner.build_paths import PathRegistrationPipeline

ctx = SimpleNamespace(
    bot=bot,
    chain_id=1,
    db=None,
    uniswap_v3_tracker=None,
    sushiswap_v3_tracker=None,
    pancakeswap_v3_tracker=None,
    weth=None,
)
try:
    PathRegistrationPipeline(context=ctx, engine_registry=None)
except RuntimeError as exc:
    assert "fleet-hosted only" in str(exc), exc
else:
    raise AssertionError("PRG-5: a pipeline over a legacy bot must refuse")

print("LEGACY-OK")
"""


def test_legacy_stance_keeps_the_incumbent_pool() -> None:
    """No engine stance installed (a fresh process): intake refuses."""
    # FF-T5 (the addendum determinism fix): the legacy-stance assertions
    # run in a SUBPROCESS - a fresh process never carries another test's
    # in-process engine construction, so the skip condition (and the
    # pass/skip wobble under pytest-randomly/xdist) is gone: the test is
    # deterministic on every host and every ordering.
    proc = subprocess.run(
        [sys.executable, "-c", _LEGACY_CHILD],
        capture_output=True,
        text=True,
        cwd=str(Path(__file__).parents[2]),
        timeout=120,
        check=False,
    )
    assert "LEGACY-OK" in proc.stdout, f"stdout={proc.stdout!r} stderr_tail={proc.stderr[-1500:]!r}"


def test_legacy_stance_pipeline_construction_refuses() -> None:
    """PRG-5 hard cutover: a pipeline over a legacy-stance bot refuses."""
    # The subprocess child asserts BOTH legacy arms (the intake refusal
    # and the pipeline construction refusal): one fresh process, both
    # contracts, deterministic under every test ordering.
    proc = subprocess.run(
        [sys.executable, "-c", _LEGACY_CHILD],
        capture_output=True,
        text=True,
        cwd=str(Path(__file__).parents[2]),
        timeout=120,
        check=False,
    )
    assert "LEGACY-OK" in proc.stdout, f"stdout={proc.stdout!r} stderr_tail={proc.stderr[-1500:]!r}"
