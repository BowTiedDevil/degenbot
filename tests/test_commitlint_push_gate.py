"""The push-range commitlint gate (``scripts/hooks/commitlint-push.sh``).

Two contracts, both negative-probed here:

1. The gate never certifies a push range it did not lint. The script is the
   safety net for ``git commit --no-verify``, so exiting green with a
   "skipping" note when ``npx`` is absent punches exactly the hole it exists
   to close. Missing tooling must fail loud.

2. It lints with the rule set CI lints with. CI's commitlint job pins
   ``@commitlint/cli``; a floating local resolution can flip the shared gate's
   verdict with no diff to review. The four invocation surfaces -- this gate,
   ``just lint-commits``, prek.toml's commit-msg entry, and ci.yml's job --
   must name one version.

The gate is driven as a subprocess inside a throwaway two-commit repository,
with ``git`` on PATH and ``npx`` replaced by a recording stub, so the argv the
gate actually runs is what the parity assertion reads.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess  # ruff: ignore[suspicious-subprocess-import]
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "hooks" / "commitlint-push.sh"
CI_YML = REPO_ROOT / ".github" / "workflows" / "ci.yml"
JUSTFILE = REPO_ROOT / "justfile"
PREK_TOML = REPO_ROOT / "prek.toml"


def _bash() -> str:
    """Absolute path to bash (S607: never launch a bare name off PATH)."""
    exe = shutil.which("bash")
    assert exe is not None, "bash is not installed on PATH"
    return exe


def _git() -> str:
    """Absolute path to git (S607: never launch a bare name off PATH)."""
    exe = shutil.which("git")
    assert exe is not None, "git is not installed on PATH"
    return exe


def _throwaway_repo(tmp_path: Path) -> Path:
    """A two-commit repository with no upstream, so the gate lints `HEAD^..HEAD`."""
    repo = tmp_path / "repo"
    repo.mkdir()
    git = [_git(), "-C", str(repo)]
    identity = ["-c", "user.name=gate", "-c", "user.email=gate@example.invalid"]
    subprocess.run([*git, "init", "--quiet"], check=True)  # ruff: ignore[suspicious-subprocess-import]
    for subject in ("feat: one", "feat: two"):
        (repo / "message.txt").write_text(subject, encoding="utf-8")
        subprocess.run([*git, "add", "message.txt"], check=True)  # ruff: ignore[suspicious-subprocess-import]
        subprocess.run([*git, *identity, "commit", "--quiet", "-m", subject], check=True)  # ruff: ignore[suspicious-subprocess-import]
    return repo


def _stub_bin(tmp_path: Path, *, npx_exit: int = 0) -> tuple[Path, Path]:
    """A PATH directory carrying real git and a recording npx stub.

    The stub appends its argv to a file and exits `npx_exit`, so a test can
    assert both what the gate invoked and how its verdict propagated.
    """
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    (bin_dir / "git").symlink_to(_git())
    argv_file = tmp_path / "npx-argv.txt"
    npx = bin_dir / "npx"
    stub = "#!/bin/sh\n"
    stub += 'printf "%s\\n" "$@" >> ' + str(argv_file) + "\n"
    stub += f"exit {npx_exit}\n"
    npx.write_text(stub, encoding="utf-8")
    npx.chmod(0o755)
    return bin_dir, argv_file


def _run_gate(
    tmp_path: Path, *, npx_exit: int = 0, with_npx: bool = True
) -> tuple[subprocess.CompletedProcess[str], Path]:
    repo = _throwaway_repo(tmp_path)
    bin_dir, argv_file = _stub_bin(tmp_path, npx_exit=npx_exit)
    if not with_npx:
        (bin_dir / "npx").unlink()
    env = dict(os.environ)
    env["PATH"] = str(bin_dir)
    result = subprocess.run(  # ruff: ignore[suspicious-subprocess-import]
        [_bash(), str(SCRIPT)],
        cwd=repo,
        env=env,
        check=False,
        capture_output=True,
        text=True,
    )
    return result, argv_file


def test_a_clean_lint_passes_the_range(tmp_path: Path) -> None:
    result, argv_file = _run_gate(tmp_path)

    assert result.returncode == 0, result.stdout + result.stderr
    assert "--from" in argv_file.read_text(encoding="utf-8"), (
        "the gate must lint the outgoing range, not exit before invoking lint"
    )


def test_a_lint_failure_blocks_the_push(tmp_path: Path) -> None:
    result, _ = _run_gate(tmp_path, npx_exit=1)

    assert result.returncode != 0, "a failed lint must block the push"
    assert "Push blocked" in result.stderr


def test_the_gate_fails_loud_when_npx_is_absent(tmp_path: Path) -> None:
    result, _ = _run_gate(tmp_path, with_npx=False)

    assert result.returncode != 0, (
        "a range the gate never linted must not be certified by a silent skip"
    )
    assert "npx" in result.stderr


def test_the_gate_lints_with_the_rule_set_ci_pins(tmp_path: Path) -> None:
    result, argv_file = _run_gate(tmp_path)

    assert result.returncode == 0, result.stdout + result.stderr
    match = re.search(r"@commitlint/cli@(\S+)", argv_file.read_text(encoding="utf-8"))
    assert match is not None, "the gate must name a pinned @commitlint/cli"
    version = match.group(1)

    for surface in (CI_YML, JUSTFILE, PREK_TOML):
        pinned = set(
            re.findall(
                r"@commitlint/cli@([0-9][0-9A-Za-z.\-]*)", surface.read_text(encoding="utf-8")
            )
        )
        assert pinned == {version}, (
            f"{surface.name} pins {sorted(pinned)} while the gate lints with {version}"
        )
