"""The staged-slice python format gate (``scripts/hooks/python-fmt-staged.sh``).

The gate judges this commit's python shape with ``ruff format --check``, run out
of the project's own environment: ruff is a ``dev`` dependency, so its vintage is
the one ``uv.lock`` records and every other python gate in the repo uses. Three
contracts, the middle one negative-probed:

1. the gate format-checks the files it was handed with the project venv's ruff,
   without resolving or reinstalling that environment (a commit-time gate must
   not mutate the venv underneath the caller);
2. an empty file list fails closed rather than certifying a list it never
   examined;
3. a formatter failure fails the gate.

The gate is driven as a subprocess with ``uv`` replaced by a recording stub, so
the argv it actually runs is what the assertions read.
"""

from __future__ import annotations

import os
import shutil
import subprocess  # ruff: ignore[suspicious-subprocess-import]
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "hooks" / "python-fmt-staged.sh"


def _bash() -> str:
    """Absolute path to bash (S607: never launch a bare name off PATH)."""
    exe = shutil.which("bash")
    assert exe is not None, "bash is not installed on PATH"
    return exe


def _stub_bin(tmp_path: Path, *, uv_exit: int = 0) -> tuple[Path, Path]:
    """A PATH prefix carrying a recording uv stub ahead of the system dirs."""
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    argv_file = tmp_path / "uv-argv.txt"
    stub = "#!/bin/sh\n"
    stub += 'printf "%s\\n" "$@" >> ' + str(argv_file) + "\n"
    stub += f"exit {uv_exit}\n"
    uv = bin_dir / "uv"
    uv.write_text(stub, encoding="utf-8")
    uv.chmod(0o755)
    return bin_dir, argv_file


def _run_gate(
    tmp_path: Path, *args: str, uv_exit: int = 0
) -> tuple[subprocess.CompletedProcess[str], Path]:
    bin_dir, argv_file = _stub_bin(tmp_path, uv_exit=uv_exit)
    env = dict(os.environ)
    env["PATH"] = str(bin_dir) + os.pathsep + env["PATH"]
    result = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true] - one explicit argv, never a shell
        [_bash(), str(SCRIPT), *args],
        cwd=REPO_ROOT,
        env=env,
        check=False,
        capture_output=True,
        text=True,
    )
    return result, argv_file


def test_the_gate_checks_the_commit_with_the_project_ruff(tmp_path: Path) -> None:
    target = "tests/test_python_fmt_staged_gate.py"
    result, argv_file = _run_gate(tmp_path, target)

    assert result.returncode == 0, result.stdout + result.stderr
    argv = argv_file.read_text(encoding="utf-8").splitlines()
    assert argv == ["run", "--no-sync", "ruff", "format", "--check", "--", target], (
        f"the gate must format-check the handed files with the venv ruff, ran: {argv}"
    )


def test_an_empty_file_list_fails_closed(tmp_path: Path) -> None:
    result, argv_file = _run_gate(tmp_path)

    assert result.returncode != 0, "a list the gate never examined must not pass"
    assert not argv_file.exists(), "no format check may run for a vacuous invocation"


def test_a_format_failure_fails_the_gate(tmp_path: Path) -> None:
    result, _ = _run_gate(tmp_path, "tests/test_python_fmt_staged_gate.py", uv_exit=1)

    assert result.returncode != 0, "an unformatted staged file must fail the gate"
