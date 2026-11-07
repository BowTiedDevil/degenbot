"""The staged-slice python format gate (``scripts/hooks/python-fmt-staged.sh``).

The gate judges this commit's python shape with ``ruff format --check``, so
its verdict must come from the one pinned ruff vintage the justfile owns
(``ruff_version``). A floating ``uvx ruff`` resolved whatever PyPI published
that minute: an upstream release could flip the shared gate's verdict -- or
rewrite a file under ``format`` -- with no diff on our side to review.

Three contracts, the middle one negative-probed:

1. the gate checks the passed files with the pinned vintage (the same one
   ``just format`` / ``fmt-check-python`` / the gen-stubs pipeline run);
2. an empty file list fails closed rather than certifying a list it never
   examined;
3. a formatter failure fails the gate.

The gate is driven as a subprocess with ``uvx`` replaced by a recording stub,
so the argv it actually runs is what the parity assertion reads.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess  # ruff: ignore[suspicious-subprocess-import]
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "hooks" / "python-fmt-staged.sh"
JUSTFILE = REPO_ROOT / "justfile"


def _bash() -> str:
    """Absolute path to bash (S607: never launch a bare name off PATH)."""
    exe = shutil.which("bash")
    assert exe is not None, "bash is not installed on PATH"
    return exe


def _stub_bin(tmp_path: Path, *, uvx_exit: int = 0) -> tuple[Path, Path]:
    """A PATH prefix carrying a recording uvx stub ahead of the system dirs."""
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    argv_file = tmp_path / "uvx-argv.txt"
    stub = "#!/bin/sh\n"
    stub += 'printf "%s\\n" "$@" >> ' + str(argv_file) + "\n"
    stub += f"exit {uvx_exit}\n"
    uvx = bin_dir / "uvx"
    uvx.write_text(stub, encoding="utf-8")
    uvx.chmod(0o755)
    return bin_dir, argv_file


def _run_gate(
    tmp_path: Path, *args: str, uvx_exit: int = 0
) -> tuple[subprocess.CompletedProcess[str], Path]:
    bin_dir, argv_file = _stub_bin(tmp_path, uvx_exit=uvx_exit)
    env = dict(os.environ)
    env["PATH"] = str(bin_dir) + os.pathsep + env["PATH"]
    result = subprocess.run(  # ruff: ignore[suspicious-subprocess-import]
        [_bash(), str(SCRIPT), *args],
        cwd=REPO_ROOT,
        env=env,
        check=False,
        capture_output=True,
        text=True,
    )
    return result, argv_file


def _pinned_ruff() -> str:
    """The `ruff_version` value the justfile owns."""
    match = re.search(r'^ruff_version := "(.*)"$', JUSTFILE.read_text(encoding="utf-8"), re.M)
    assert match is not None, "the justfile must own a ruff_version pin"
    return match.group(1)


def test_the_gate_checks_the_commit_with_the_pinned_ruff(tmp_path: Path) -> None:
    result, argv_file = _run_gate(tmp_path, "tests/test_python_fmt_staged_gate.py")

    assert result.returncode == 0, result.stdout + result.stderr
    argv = argv_file.read_text(encoding="utf-8").splitlines()
    assert "tests/test_python_fmt_staged_gate.py" in argv, (
        "the gate must check the files it was handed"
    )
    assert _pinned_ruff() in argv, (
        f"the gate must format-check with the justfile's pin, ran: {argv}"
    )


def test_an_empty_file_list_fails_closed(tmp_path: Path) -> None:
    result, argv_file = _run_gate(tmp_path)

    assert result.returncode != 0, "a list the gate never examined must not pass"
    assert not argv_file.exists(), "no format check may run for a vacuous invocation"


def test_a_format_failure_fails_the_gate(tmp_path: Path) -> None:
    result, _ = _run_gate(tmp_path, "tests/test_python_fmt_staged_gate.py", uvx_exit=1)

    assert result.returncode != 0, "an unformatted staged file must fail the gate"
