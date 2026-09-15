"""Smoke tests for the Rust-owned console reached through the Python seam
(ADR-051 D3).

The Python console script (``degenbot``, from ``[project.scripts]``) and
``python -m degenbot`` are both five-line passthroughs over
``degenbot._ffi.cli_main``; the argv vocabulary, semantics, prompts, rendering
and exit codes all live in the ``degenbot-cli-core`` / ``degenbot-cli`` pair.
These tests prove the seam is wired: the canonical invocation is the same
command ``uv run degenbot --help`` resolves to.
"""

from __future__ import annotations

import shutil
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import sys

import pytest


def _console_executable() -> list[str]:
    """The installed console script (what ``uv run degenbot`` runs)."""
    exe = shutil.which("degenbot")
    assert exe is not None, "the 'degenbot' console script is not installed on PATH"
    return [exe]


def _run(argv: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        argv,
        capture_output=True,
        text=True,
        check=False,
        timeout=120,
    )


def test_console_script_help_lists_command_groups() -> None:
    """``degenbot --help`` exits 0 and lists every command group."""
    proc = _run([*_console_executable(), "--help"])
    assert proc.returncode == 0, proc.stderr
    for group in ("database", "exchange", "pool", "aave", "fleet", "path"):
        assert group in proc.stdout, f"missing group {group!r} in help output"


def test_python_m_degenbot_uses_the_same_seam() -> None:
    """``python -m degenbot --help`` is the same passthrough, same output."""
    proc = _run([sys.executable, "-m", "degenbot", "--help"])
    assert proc.returncode == 0, proc.stderr
    assert "database" in proc.stdout


def test_database_upgrade_is_retired() -> None:
    """ADR-052 D4: ``database upgrade`` is a dead subcommand in the Rust
    console — it renders the pointed retirement error and exits 1."""
    proc = _run([*_console_executable(), "database", "upgrade"])
    assert proc.returncode == 1, (proc.stdout, proc.stderr)
    assert "the database upgrades itself at open" in proc.stderr
    assert "degenbot database heal" in proc.stderr


@pytest.mark.skip(
    reason=(
        "manual gate: no reachable boot-refusal path exists on this console yet "
        "(degenbot-cli-core defines CliError::BootRefused and maps it to "
        "EX_CONFIG 78 but no arm constructs it). Until a cheap fleet "
        "fixture lands, the mapping is pinned by the Rust unit test "
        "degenbot_rs::cli::tests::boot_refusal_maps_to_ex_config."
    ),
)
def test_boot_refused_exits_78() -> None:
    """A fleet boot refusal must exit 78 (sysexits EX_CONFIG)."""
    proc = _run([*_console_executable(), "fleet", "status"])
    assert proc.returncode == 78
    assert "[fleet-boot] REFUSED" in proc.stderr
