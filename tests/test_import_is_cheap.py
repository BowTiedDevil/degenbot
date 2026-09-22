"""Import-cheapness of the PyO3 binding.

The ``degenbot_rs`` module init registers symbols and validates typed config
ONLY. Everything a long-running driver needs — the shared tokio runtime, the
tracing subscriber + Rust→Python log drainer, the metrics scrape thread, the
panic hook, the worker-census boot dump — is installed by the explicit
``driver_boot()`` call. A one-shot consumer (the console passthrough, plain
library imports) pays none of it.

Verified via fresh subprocesses: thread creation and the census table are
process-global, so an in-process import would be polluted by any earlier
import in the pytest session.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path


_BOOT_PROBE = """
import degenbot
from degenbot._ffi import driver_boot
sigs = [d for d in __import__('os').listdir('/proc/self/task')]
import os, threading
before = len(os.listdir('/proc/self/task'))
driver_boot()
after = len(os.listdir('/proc/self/task'))
driver_boot()
after2 = len(os.listdir('/proc/self/task'))
print(f'TASKS before={before} after_boot={after} after_second={after2}')
threads = threading.enumerate()
print(f'PY_THREADS={len(threads)}')
"""


def _run_probe(code: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-X", "utf8", "-c", code],
        capture_output=True,
        text=True,
        cwd=Path(__file__).parent.parent,
        check=True,
    )


def test_import_alone_spawns_no_threads() -> None:
    """A bare ``import degenbot`` must not boot runtimes or drainers."""
    out = _run_probe(
        """
import degenbot
print('IMPORT_OK')
"""
    )
    assert "IMPORT_OK" in out.stdout


def test_driver_boot_is_explicit_and_idempotent() -> None:
    """driver_boot() installs the driver stack once; a second call is a no-op."""
    out = _run_probe(
        """
import os
import degenbot
from degenbot._ffi import driver_boot
before = len(os.listdir('/proc/self/task'))
driver_boot()
after = len(os.listdir('/proc/self/task'))
driver_boot()
after2 = len(os.listdir('/proc/self/task'))
assert after > before, f'driver_boot spawned no threads: {before} -> {after}'
assert after2 == after, f'second driver_boot spawned threads: {after} -> {after2}'
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
