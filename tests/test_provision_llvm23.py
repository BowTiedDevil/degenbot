"""Behavioural tests for the static-archive check in `scripts/provision-llvm23.sh`.

The script hands the LLVM static link a zstd archive, and that archive is
linked into the extension's *shared object*. A non-PIC x86_64 archive cannot be
placed there, so the script proves every archive it accepts before the linker
sees it. `--check-static-lib PATH` is that gate; these are its probes.
"""

from __future__ import annotations

import platform
import shutil
import subprocess  # ruff: ignore[suspicious-subprocess-import]
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).parents[1]
SCRIPT = REPO_ROOT / "scripts" / "provision-llvm23.sh"

x86_64_only = pytest.mark.skipif(
    platform.machine() not in {"x86_64", "amd64"},
    reason="the relocation kinds the guard reads are x86_64's",
)

# Absolute 32-bit addressing of a datum is exactly what a shared object may not
# carry; the RIP-relative form is the assembler's PIC spelling of the same load.
NON_PIC_SOURCE = """\
    .text
    .globl probe
probe:
    movl $scratch, %eax
    ret
    .data
    .globl scratch
scratch:
    .long 0
"""

PIC_SOURCE = """\
    .text
    .globl probe
probe:
    leaq scratch(%rip), %rax
    ret
    .data
    .globl scratch
scratch:
    .long 0
"""


def _bash() -> str:
    """Absolute path to bash (S607: never launch a bare name off PATH)."""
    exe = shutil.which("bash")
    assert exe is not None, "bash is not installed on PATH"
    return exe


def _assemble(tmp_path: Path, source: str) -> Path:
    assembler = shutil.which("as")
    assert assembler is not None, "binutils (as) is not installed on PATH"
    asm = tmp_path / "probe.s"
    asm.write_text(source, encoding="utf-8")
    obj = tmp_path / "probe.o"
    subprocess.run([assembler, str(asm), "-o", str(obj)], check=True)  # ruff: ignore[subprocess-without-shell-equals-true]
    return obj


def _check(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [_bash(), str(SCRIPT), *args], capture_output=True, text=True, check=False
    )


@x86_64_only
def test_a_non_pic_archive_is_refused(tmp_path: Path) -> None:
    result = _check("--check-static-lib", str(_assemble(tmp_path, NON_PIC_SOURCE)))

    assert result.returncode != 0, result.stderr
    assert "-fPIC" in result.stderr


@x86_64_only
def test_a_pic_archive_is_accepted(tmp_path: Path) -> None:
    result = _check("--check-static-lib", str(_assemble(tmp_path, PIC_SOURCE)))

    assert result.returncode == 0, result.stderr
    assert "position-independent" in result.stderr


def test_the_check_needs_a_path() -> None:
    result = _check("--check-static-lib")

    assert result.returncode == 2, result.stderr
    assert "needs a path" in result.stderr


def test_a_missing_archive_is_refused(tmp_path: Path) -> None:
    result = _check("--check-static-lib", str(tmp_path / "absent.a"))

    assert result.returncode != 0, result.stderr
    assert "is not a file" in result.stderr
