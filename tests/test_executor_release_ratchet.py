from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
GATE = ROOT / "scripts" / "executor_release_ratchet.py"


def run_gate(root: Path, base_index: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [
            sys.executable,
            str(GATE),
            "check",
            "--root",
            str(root),
            "--base-index",
            str(base_index),
        ],
        cwd=ROOT,
        text=True,
        capture_output=True,
        check=False,
    )


def generate(root: Path) -> None:
    subprocess.run(
        [sys.executable, str(GATE), "generate", "--root", str(root)],
        cwd=ROOT,
        text=True,
        capture_output=True,
        check=True,
    )


def add_package(root: Path, ordinal: int) -> None:
    package = root / "rust/crates/foundation/degenbot-executor/releases" / f"v{ordinal}"
    package.mkdir(parents=True)
    (package / "contract.vy").write_text(f"contract {ordinal}\n")
    (package / "manifest.json").write_text(json.dumps({"release_id": f"cmd_executor/{ordinal}"}))
    (package / "package.json").write_text(
        json.dumps({
            "release_id": f"cmd_executor/{ordinal}",
            "ordinal": ordinal,
            "files": [
                f"rust/crates/foundation/degenbot-executor/releases/v{ordinal}/contract.vy",
                f"rust/crates/foundation/degenbot-executor/releases/v{ordinal}/manifest.json",
            ],
        })
    )


def test_frozen_file_edit_cannot_be_laundered_by_regeneration(tmp_path: Path) -> None:
    add_package(tmp_path, 2)
    generate(tmp_path)
    base_index = tmp_path / "base-index.json"
    index = tmp_path / "rust/crates/foundation/degenbot-executor/releases/index.json"
    base_index.write_bytes(index.read_bytes())

    contract = tmp_path / "rust/crates/foundation/degenbot-executor/releases/v2/contract.vy"
    contract.write_text("changed runtime behavior\n")
    generate(tmp_path)

    result = run_gate(tmp_path, base_index)

    assert result.returncode == 1, result.stderr + result.stdout
    assert "cmd_executor/2" in result.stderr
    assert "contract.vy" in result.stderr


def test_committed_release_index_is_clean() -> None:
    result = run_gate(ROOT, ROOT / "rust/crates/foundation/degenbot-executor/releases/index.json")

    assert result.returncode == 0, result.stderr + result.stdout


def test_unregenerated_file_edit_fails_drift_check(tmp_path: Path) -> None:
    add_package(tmp_path, 2)
    generate(tmp_path)
    base_index = tmp_path / "base-index.json"
    index = tmp_path / "rust/crates/foundation/degenbot-executor/releases/index.json"
    base_index.write_bytes(index.read_bytes())

    contract = tmp_path / "rust/crates/foundation/degenbot-executor/releases/v2/contract.vy"
    contract.write_text("changed runtime behavior\n")
    result = run_gate(tmp_path, base_index)

    assert result.returncode == 1, result.stderr + result.stdout
    assert "generated index drift" in result.stderr


def test_next_sequential_package_preserves_the_base_release(tmp_path: Path) -> None:
    add_package(tmp_path, 2)
    generate(tmp_path)
    base_index = tmp_path / "base-index.json"
    index = tmp_path / "rust/crates/foundation/degenbot-executor/releases/index.json"
    base_index.write_bytes(index.read_bytes())

    add_package(tmp_path, 3)
    generate(tmp_path)
    result = run_gate(tmp_path, base_index)

    assert result.returncode == 0, result.stderr + result.stdout
