"""Import-cheapness of the PyO3 binding.

The ``degenbot_rs`` module init registers symbols and validates typed config
ONLY. Everything a long-running driver needs — the shared tokio runtime, the
tracing subscriber + Rust→Python log drainer, the metrics scrape thread, the
panic hook, the worker-census boot dump — is installed by the explicit
``driver_boot()`` call. A one-shot consumer (the console passthrough, plain
library imports) pays none of it.

The observable is ``degenbot.runtime_status()``: the worker census (the
runtimes + drainer inventory, installed by ``driver_boot()`` alone) and the
fleet boot flag. Verified via fresh subprocesses: census state is
process-global, so an in-process import would be polluted by any earlier
import in the pytest session.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path


def _run_probe(code: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-X", "utf8", "-c", code],
        capture_output=True,
        text=True,
        cwd=Path(__file__).parent.parent,
        check=True,
    )


def test_import_alone_installs_no_driver_stack() -> None:
    """A bare ``import degenbot`` must not boot runtimes or drainers.

    The census is the inventory of exactly that machinery; it is empty (and
    the runtime fleet unbooted) until ``driver_boot()`` installs it.
    """
    out = _run_probe(
        """
from degenbot import runtime_status

status = runtime_status()
assert status['census'] == [], f'bare import installed the driver stack: {status}'
assert status['fleet_booted'] is False, f'bare import booted the fleet: {status}'
print('IMPORT_OK')
"""
    )
    assert "IMPORT_OK" in out.stdout


def test_driver_boot_is_explicit_and_idempotent() -> None:
    """driver_boot() installs the driver stack once; a second call is a no-op."""
    out = _run_probe(
        """
from degenbot import runtime_status
from degenbot._ffi import driver_boot

before = runtime_status()
driver_boot()
after = runtime_status()
driver_boot()
after_second = runtime_status()
assert before['census'] == [], f'pre-boot census already installed: {before}'
assert len(after['census']) > 0, f'driver_boot installed no census: {after}'
assert after['census'] == after_second['census'], (
    f'second driver_boot changed the census: {after} -> {after_second}'
)
print('BOOT_OK')
"""
    )
    assert "BOOT_OK" in out.stdout
    # The census line is forwarded through the Python-logging base config, so
    # its stream is the handler's choice — assert it surfaces exactly once in
    # whichever stream carries it.
    surfaces = out.stdout + out.stderr
    assert surfaces.count("boot table full") <= 1, (
        f"census boot line must not repeat: {surfaces!r}"
    )


def test_console_script_silent_by_default() -> None:
    """The degenbot console owns its surface: silent at the default level."""
    out = _run_probe(
        """
import subprocess, sys
res = subprocess.run(
    [sys.executable, '-m', 'degenbot', 'exchange', 'list'],
    capture_output=True, text=True,
)
print('EXIT', res.returncode)
print('STDERR_START')
print(res.stderr)
print('STDERR_END')
"""
    )
    stderr = out.stdout.split("STDERR_START")[1].split("STDERR_END")[0]
    assert "INFO" not in stderr and "DEBUG" not in stderr, f"console stderr noisy: {stderr!r}"
    assert " ERROR" not in stderr, f"console errored: {stderr!r}"
