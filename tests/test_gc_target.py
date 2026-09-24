"""Behavioral tests for target-cache housekeeping preview mode."""

from __future__ import annotations

import os
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import time
from pathlib import Path

REPO_ROOT = Path(__file__).parents[1]
SCRIPT = REPO_ROOT / "scripts" / "gc-target.sh"
OLD_MTIME = time.time() - 10 * 24 * 60 * 60


def _write(path: Path, content: str, *, old: bool = False) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    if old:
        os.utime(path, (OLD_MTIME, OLD_MTIME))
        os.utime(path.parent, (OLD_MTIME, OLD_MTIME))


def _apparent_bytes(path: Path) -> int:
    if path.is_symlink() or not path.is_dir():
        return path.lstat().st_size
    return path.lstat().st_size + sum(_apparent_bytes(child) for child in path.iterdir())


def _snapshot(root: Path) -> dict[Path, bytes | None]:
    return {
        path.relative_to(root): None if path.is_dir() else path.read_bytes()
        for path in sorted(root.rglob("*"))
    }


def _fields(output: str, key: str) -> list[str]:
    return [line.split("=", 1)[1] for line in output.splitlines() if line.startswith(f"{key}=")]


def test_dry_run_reports_complete_families_without_deleting(tmp_path: Path) -> None:
    repo = tmp_path / "repo"
    target = repo / "rust" / "target"
    selected = [
        target / "debug" / "incremental",
        target / "debug" / "deps" / "stale",
        target / "rust-analyzer" / "debug" / "incremental",
        target / "llvm-cov-target" / "debug",
        target / "llvm-cov-target" / "rust-1.profraw",
        target / "coverage" / "pyo3-build",
        target / "criterion" / "solver",
        target / "wheels" / "degenbot.whl",
        target / "doc" / "degenbot",
        target / "lcov.info",
    ]
    for path in selected:
        _write(path, f"artifact:{path.name}\n", old=True)

    _write(repo / ".build-number", "42 deadbeef\n")
    _write(target / "maturin" / "libdegenbot_rs.so", "warm extension\n")
    _write(target / "debug" / "fresh", "keep me\n")
    before = _snapshot(repo)

    result = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [str(SCRIPT)],
        cwd=REPO_ROOT,
        env={**os.environ, "AGE": "7", "DRY_RUN": "1", "GC_TARGET_DIR": str(target)},
        check=True,
        capture_output=True,
        text=True,
    )

    assert _snapshot(repo) == before
    assert _fields(result.stdout, "action") == ["dry-run"]
    assert _fields(result.stdout, "age-days") == ["7"]
    assert _fields(result.stdout, "reclaimable-bytes") == [
        str(sum(_apparent_bytes(path) for path in selected))
    ]
    assert _fields(result.stdout, "target-size-before-bytes") == _fields(
        result.stdout, "target-size-after-bytes"
    )
    assert _fields(result.stdout, "deleted-paths") == ["0"]

    families = {
        fields[0].split("=", 1)[1]: dict(field.split("=", 1) for field in fields[1:])
        for line in result.stdout.splitlines()
        if line.startswith("family=")
        for fields in [line.split()]
    }
    assert set(families) == {
        "normal-cargo",
        "maturin",
        "coverage",
        "llvm-coverage",
        "criterion",
        "wheels",
        "documentation",
    }
    assert families["maturin"]["policy"] == "protected"
    assert int(families["maturin"]["selected-paths"]) == 0
    assert int(families["normal-cargo"]["reclaimable-bytes"]) > 0
    for family in ("coverage", "llvm-coverage", "criterion", "wheels", "documentation"):
        assert int(families[family]["size-bytes"]) > 0
        assert int(families[family]["reclaimable-bytes"]) > 0

    protected = _fields(result.stdout, "protected")
    assert any(path.endswith("/.build-number") for path in protected)
    assert any(path.endswith("/rust/target/maturin") for path in protected)
