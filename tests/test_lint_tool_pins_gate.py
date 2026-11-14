"""The lint/format tool-pin gate (``scripts/hooks/lint-tool-pins.sh``).

The pins (ruff, ty, markdownlint-cli2, the commitlint pair) are exact so a gate
verdict cannot move unreviewed. This gate owns the other half of that deal: an
exact pin nobody refreshes ages out. ``--report`` names every pin that is behind
what the repo can install (the pre-push ``lint-tools`` hook runs it) and
``--bump`` refreshes them in place for ``just bump-lint-tools``.

Four contracts, three negative-probed:

1. every pin current -> ``--report`` exits 0;
2. a pin behind the installable version -> ``--report`` exits 1 and names the
   one-command remedy;
3. an unreachable registry is a WARNING, never a red (offline must not break a
   push), while a vanished pin FAILS CLOSED instead of reading as current;
4. ``--bump`` rewrites only the version token, leaving the rest of the line
   byte-identical, and reports what it changed.

The probes are stubbed (``uvx`` / ``uv`` / ``curl`` first on PATH), so the
verdicts are the gate's own and never the network's.
"""

from __future__ import annotations

import os
import shutil
import subprocess  # ruff: ignore[suspicious-subprocess-import]
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "hooks" / "lint-tool-pins.sh"


def _bash() -> str:
    """Absolute path to bash (S607: never launch a bare name off PATH)."""
    exe = shutil.which("bash")
    assert exe is not None, "bash is not installed on PATH"
    return exe


def _stub(bin_dir: Path, name: str, body: str) -> None:
    """Write one executable probe stub."""
    path = bin_dir / name
    path.write_text(body, encoding="utf-8")
    path.chmod(0o755)


def _pins_root(
    tmp_path: Path,
    *,
    ruff: str,
    ty: str,
    markdownlint: str = "0.24.1",
    commitlint: str = "21.2.4",
) -> Path:
    """The minimal pin root the gate reads: a justfile and a prek.toml."""
    root = tmp_path / "root"
    root.mkdir()
    (root / "justfile").write_text(
        f'ty_version := "ty=={ty}"\n'
        f'ruff_version := "ruff=={ruff}"\n'
        f'    npx --yes markdownlint-cli2@{markdownlint} "**/*.md"\n'
        f'    npx --yes @commitlint/cli@{commitlint} --from "$from" --to "$to"\n',
        encoding="utf-8",
    )
    (root / "prek.toml").write_text(
        f'  {{ id = "commitlint", entry = "npx --yes @commitlint/cli@{commitlint} --edit" }},\n',
        encoding="utf-8",
    )
    return root


def _probes(
    tmp_path: Path,
    *,
    ruff: str | None,
    ty: str | None,
    markdownlint: str = "0.24.1",
    commitlint: str = "21.2.4",
) -> Path:
    """Probe stubs answering with the versions the gate should compare against.

    ``None`` for ``ruff`` / ``ty`` stubs the resolver as unreachable (exit 1, no
    output), which is how an offline push looks to the gate.
    """
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    ruff_body = f'echo "ruff {ruff}"' if ruff else "exit 1"
    ty_body = f'echo "ty {ty}"' if ty else "exit 1"
    _stub(bin_dir, "uvx", f"#!/bin/sh\n{ruff_body}\n")
    _stub(bin_dir, "uv", f"#!/bin/sh\n{ty_body}\n")
    _stub(
        bin_dir,
        "curl",
        "#!/bin/sh\n"
        'case "$*" in\n'
        f'  *markdownlint-cli2*) echo \'{{"version": "{markdownlint}"}}\' ;;\n'
        f'  *) echo \'{{"version": "{commitlint}"}}\' ;;\n'
        "esac\n",
    )
    return bin_dir


def _run(root: Path, bin_dir: Path, *args: str) -> subprocess.CompletedProcess[str]:
    """Run the gate against ``root`` with the probe stubs first on PATH."""
    env = dict(os.environ)
    env["LINT_TOOL_PINS_ROOT"] = str(root)
    env["PATH"] = f"{bin_dir}{os.pathsep}{env['PATH']}"
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true] - fixed argv
        [_bash(), str(SCRIPT), *args],
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )


def test_report_passes_when_every_pin_is_current(tmp_path: Path) -> None:
    """Contract 1: pins at the installable version are a green gate."""
    root = _pins_root(tmp_path, ruff="0.17.5", ty="0.0.95")
    result = _run(root, _probes(tmp_path, ruff="0.17.5", ty="0.0.95"))
    assert result.returncode == 0, result.stderr
    assert "BEHIND" not in result.stdout
    assert "every pin is at the newest version" in result.stdout


def test_report_fails_when_a_pin_is_behind(tmp_path: Path) -> None:
    """Contract 2: a stale pin is red, and names the one-command remedy."""
    root = _pins_root(tmp_path, ruff="0.17.4", ty="0.0.95")
    result = _run(root, _probes(tmp_path, ruff="0.17.5", ty="0.0.95"))
    assert result.returncode == 1
    assert "BEHIND" in result.stdout
    assert "just bump-lint-tools" in result.stderr


def test_unreachable_registry_warns_but_stays_green(tmp_path: Path) -> None:
    """Contract 3a: offline is a warning -- freshness never breaks a push."""
    root = _pins_root(tmp_path, ruff="0.17.5", ty="0.0.95")
    result = _run(root, _probes(tmp_path, ruff=None, ty=None))
    assert result.returncode == 0, result.stderr
    assert "unknown (no registry answer)" in result.stdout


def test_vanished_pin_fails_closed(tmp_path: Path) -> None:
    """Contract 3b: a missing pin must not read as current."""
    root = _pins_root(tmp_path, ruff="0.17.5", ty="0.0.95")
    (root / "justfile").write_text('ruff_version := "ruff==0.17.5"\n', encoding="utf-8")
    result = _run(root, _probes(tmp_path, ruff="0.17.5", ty="0.0.95"))
    assert result.returncode == 2
    assert "no line matches" in result.stderr


def test_bump_rewrites_only_the_version_token(tmp_path: Path) -> None:
    """Contract 4: the bump leaves every other byte of the line alone."""
    root = _pins_root(tmp_path, ruff="0.17.4", ty="0.0.91", markdownlint="0.24.0")
    result = _run(
        root,
        _probes(tmp_path, ruff="0.17.5", ty="0.0.95", markdownlint="0.24.1"),
        "--bump",
    )
    assert result.returncode == 0, result.stderr
    assert "ruff: 0.17.4 -> 0.17.5" in result.stdout
    assert "ty: 0.0.91 -> 0.0.95" in result.stdout
    assert "markdownlint-cli2: 0.24.0 -> 0.24.1" in result.stdout
    text = (root / "justfile").read_text(encoding="utf-8")
    assert 'ty_version := "ty==0.0.95"' in text
    assert 'ruff_version := "ruff==0.17.5"' in text
    # the mid-line npm pin keeps its surrounding recipe text
    assert 'npx --yes markdownlint-cli2@0.24.1 "**/*.md"' in text


def test_bump_reports_nothing_to_do_when_current(tmp_path: Path) -> None:
    """A current pin set produces no false "bumped" line."""
    root = _pins_root(tmp_path, ruff="0.17.5", ty="0.0.95")
    result = _run(root, _probes(tmp_path, ruff="0.17.5", ty="0.0.95"), "--bump")
    assert result.returncode == 0, result.stderr
    assert "nothing to bump" in result.stdout
