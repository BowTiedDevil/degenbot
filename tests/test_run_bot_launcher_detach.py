"""A captured ``run_bot.sh start`` returns when the launcher returns.

``start`` hands the launch to a ``setsid`` supervisor so the launching shell can
exit without the bot dying on SIGHUP. Every descriptor such a supervisor
inherits outlives the launcher, so a caller that captures the launcher's own
output -- ``subprocess.run(capture_output=True)``, which the bankroll runner does
-- would otherwise stay blocked until the bot itself exited. The supervisor
therefore redirects its own stdout/stderr onto the run log at the ``setsid``
invocation, beside the driver redirection inside the detach wrapper: the log
stays the single authoritative sink, and the detached pair holds no descriptor
the caller is waiting on.

These probes drive a COPY of the launcher whose log/pidfile paths and both
driver-name patterns are rewritten to a unique per-test token, and whose driver
command is a fake long-lived script. No real bot, log, or pidfile is touched, and
the copy's ``stop`` can only reach processes the copy itself started.
"""

from __future__ import annotations

import contextlib
import os
import re
import shlex
import shutil
import signal
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import time
import uuid
from dataclasses import dataclass
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
LAUNCHER = REPO_ROOT / "run_bot.sh"

#: The production driver names. The copy must carry neither, so its pkill/pgrep
#: patterns and pidfile stay confined to the fakes below.
REAL_PY_DRIVER = "eth_settlement_arbitrage_v2_v3_v4"
REAL_RUST_DRIVER = "degenbot-settlement-bot-example"

#: The launcher's own work is a few hundred milliseconds (the pidfile handshake
#: poll plus one fake ``uv`` probe). The bound exists to prove the captured start
#: returns while the fake driver is still running: a descriptor inherited by the
#: detached supervisor parks the caller for the driver's whole lifetime.
START_RETURN_BUDGET_S = 20.0
STOP_RETURN_BUDGET_S = 30.0
EXIT_STATUS_BUDGET_S = 10.0

#: The fake bot driver: long-lived, so any descriptor the detached supervisor
#: inherited from the launcher stays open for as long as a bot would live. It
#: prints one line (the driver's stdout reaches the log by redirection) and
#: records a clean TERM exit status.
_FAKE_DRIVER = """#!/usr/bin/env bash
trap 'exit 143' TERM
printf '[fake-driver] up\n'
while :; do
    sleep 0.2
done
"""

#: The fake ``uv``: the launcher only reaches for it to resolve chain endpoints,
#: and a probe that answers nothing keeps the copy off the extension build and
#: off every network endpoint.
_FAKE_UV = """#!/usr/bin/env bash
exit 1
"""


@dataclass(frozen=True)
class LauncherCopy:
    """A temp-controlled copy of the launcher, plus the fakes it drives."""

    script: Path
    log: Path
    pidfile: Path
    python_driver: str
    rust_driver: str
    env: dict[str, str]


def _write_executable(path: Path, body: str) -> Path:
    """Write ``body`` to ``path`` and mark it executable."""
    path.write_text(body, encoding="utf-8")
    path.chmod(0o755)
    return path


def _bash() -> str:
    """Absolute path to bash (never launch a bare name off PATH)."""
    exe = shutil.which("bash")
    assert exe is not None, "bash is not installed on PATH"
    return exe


def _require_launcher_tools() -> None:
    """Fail with a legible message when the launcher's shell tools are absent."""
    for tool in ("bash", "setsid", "pgrep", "pkill", "ps", "tee", "date"):
        assert shutil.which(tool) is not None, f"missing launcher dependency {tool!r}"


def _copied_launcher(tmp_path: Path) -> LauncherCopy:
    """Write the temp-controlled launcher copy and its fake driver.

    Only constants and patterns are rewritten: the log directory (so the copy
    owns its log and pidfile), the producing driver names (so the copy's
    ``stop``/``status`` can never match a real bot), and the driver command.
    """
    token = uuid.uuid4().hex[:12]
    root = tmp_path / "launcher"
    logdir = root / "logs"
    bin_dir = root / "bin"
    logdir.mkdir(parents=True)
    bin_dir.mkdir()

    python_driver = f"fake-bot-driver-py-{token}"
    rust_driver = f"fake-bot-driver-rs-{token}"
    driver = _write_executable(root / python_driver, _FAKE_DRIVER)
    _write_executable(bin_dir / "uv", _FAKE_UV)

    text = LAUNCHER.read_text(encoding="utf-8")
    # The rewrites below must be real substitutions of real occurrences: a copy
    # that silently kept a production path or driver name would point at the
    # live bot's log, pidfile, or process.
    assert token not in text
    assert text.count(REAL_PY_DRIVER) > 0, "the launcher no longer names the python driver"
    assert text.count(REAL_RUST_DRIVER) > 0, "the launcher no longer names the rust driver"
    text = text.replace(REAL_PY_DRIVER, python_driver)
    text = text.replace(REAL_RUST_DRIVER, rust_driver)
    text, logdir_hits = re.subn(
        r"^LOGDIR=.*$",
        f"LOGDIR={logdir}",
        text,
        count=1,
        flags=re.MULTILINE,
    )
    text, chdir_hits = re.subn(
        r"^cd .*$",
        f"cd {shlex.quote(str(REPO_ROOT))}",
        text,
        count=1,
        flags=re.MULTILINE,
    )
    text, cmd_hits = re.subn(
        r"^PY_BOT_CMD=\(.*\)$",
        f'PY_BOT_CMD=("{driver}")',
        text,
        count=1,
        flags=re.MULTILINE,
    )
    assert (logdir_hits, chdir_hits, cmd_hits) == (1, 1, 1), (
        "launcher shape changed: the copy could not be retargeted"
    )
    assert REAL_PY_DRIVER not in text
    assert REAL_RUST_DRIVER not in text

    script = _write_executable(root / "run_bot_copy.sh", text)
    env = {
        **os.environ,
        "PATH": f"{bin_dir}{os.pathsep}{os.environ.get('PATH', '')}",
    }
    return LauncherCopy(
        script=script,
        log=logdir / "bot_run.log",
        pidfile=logdir / "bot_run.pid",
        python_driver=python_driver,
        rust_driver=rust_driver,
        env=env,
    )


def _launcher(
    copy: LauncherCopy, action: str, *, timeout: float
) -> subprocess.CompletedProcess[str]:
    """Run one launcher action through the copy, capturing its console."""
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [_bash(), str(copy.script), action],
        env=copy.env,
        capture_output=True,
        text=True,
        check=False,
        timeout=timeout,
    )


def _alive(pid: int) -> bool:
    """Return True while ``pid`` names a live process."""
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def _cmdline(pid: int) -> str:
    """Return ``pid``'s command line (NUL separators flattened)."""
    return Path(f"/proc/{pid}/cmdline").read_text(encoding="utf-8").replace("\0", " ")


def _await_log(log: Path, pattern: str, budget: float) -> str:
    """Return the log text once it matches ``pattern``, within ``budget``."""
    deadline = time.monotonic() + budget
    text = ""
    while time.monotonic() < deadline:
        text = log.read_text(encoding="utf-8") if log.exists() else ""
        if re.search(pattern, text, re.MULTILINE):
            return text
        time.sleep(0.05)
    pytest.fail(f"{log} never carried {pattern!r}: {text!r}")
    return text  # unreachable; pytest.fail raises


def _kill_fakes(copy: LauncherCopy) -> None:
    """Kill every process the copy started, keyed on its unique driver names."""
    if copy.pidfile.exists():
        pid = copy.pidfile.read_text(encoding="utf-8").strip()
        if pid.isdigit():
            with contextlib.suppress(ProcessLookupError):
                os.kill(int(pid), signal.SIGKILL)
    pkill = shutil.which("pkill")
    if pkill is None:  # pragma: no cover - util-linux owns the shell tools
        return
    for name in (copy.python_driver, copy.rust_driver):
        subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
            [pkill, "-9", "-f", name],
            check=False,
            capture_output=True,
        )


def test_captured_start_returns_while_the_detached_driver_stays_alive(
    tmp_path: Path,
) -> None:
    """``start`` returns at the launcher's exit; ``stop`` still owns the bot."""
    _require_launcher_tools()
    copy = _copied_launcher(tmp_path)
    try:
        started = _launcher(copy, "start", timeout=START_RETURN_BUDGET_S)
        assert started.returncode == 0, started.stderr
        assert "[runner] started bot pid" in f"{started.stdout}{started.stderr}"
        pid = int(copy.pidfile.read_text(encoding="utf-8").strip())
        assert _alive(pid), "the detached driver died with the launcher"
        assert copy.python_driver in _cmdline(pid)
        # The driver's own stdout reaches the log, and the exit-status line is
        # the supervisor's verdict on a driver that is still running.
        assert "[runner] bot exited" not in _await_log(
            copy.log,
            r"\[fake-driver\] up",
            EXIT_STATUS_BUDGET_S,
        )

        stopped = _launcher(copy, "stop", timeout=STOP_RETURN_BUDGET_S)
        assert stopped.returncode == 0, stopped.stderr
        assert "[runner] stopped" in stopped.stdout
        assert not copy.pidfile.exists()
        assert not _alive(pid)

        status = _launcher(copy, "status", timeout=STOP_RETURN_BUDGET_S)
        assert status.returncode == 0, status.stderr
        assert "[runner] not running" in status.stdout

        log = _await_log(
            copy.log,
            r"^\[runner\] bot exited rc=(137|143) ",
            EXIT_STATUS_BUDGET_S,
        )
        assert "[fake-driver] up" in log
    finally:
        _kill_fakes(copy)
