from __future__ import annotations

import json
import os
import subprocess  # ruff: ignore[suspicious-subprocess-import] -- fixtures drive fixed git argv
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
GATE = ROOT / "scripts" / "executor_release_ratchet.py"
BASE_ENV = "DEGENBOT_RELEASE_RATCHET_BASE"
RELEASES = Path("rust/crates/foundation/degenbot-executor/releases")
INDEX = RELEASES / "index.json"
GIT_IDENTITY = (
    "-c",
    "user.name=release-ratchet",
    "-c",
    "user.email=ratchet@example.invalid",
    "-c",
    "commit.gpgsign=false",
)


def gate_env(base: str | None = None) -> dict[str, str]:
    env = {key: value for key, value in os.environ.items() if key != BASE_ENV}
    if base is not None:
        env[BASE_ENV] = base
    return env


def run_gate(
    root: Path,
    base_index: Path | None = None,
    *,
    base: str | None = None,
) -> subprocess.CompletedProcess[str]:
    argv = [sys.executable, str(GATE), "check", "--root", str(root)]
    if base_index is not None:
        argv += ["--base-index", str(base_index)]
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true] -- fixed argv, no shell
        argv,
        cwd=ROOT,
        text=True,
        capture_output=True,
        check=False,
        env=gate_env(base),
    )


def generate(root: Path) -> None:
    subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true] -- fixed argv, no shell
        [sys.executable, str(GATE), "generate", "--root", str(root)],
        cwd=ROOT,
        text=True,
        capture_output=True,
        check=True,
        env=gate_env(),
    )


def add_package(root: Path, ordinal: int) -> None:
    package = root / RELEASES / f"v{ordinal}"
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


def frozen_contract(root: Path, ordinal: int = 2) -> Path:
    return root / RELEASES / f"v{ordinal}" / "contract.vy"


def run_git(root: Path, *args: str) -> str:
    result = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true] -- fixed argv, no shell
        ["git", *GIT_IDENTITY, *args],  # ruff: ignore[start-process-with-partial-path] -- git PATH lookup is intended
        cwd=root,
        text=True,
        capture_output=True,
        check=False,
    )
    assert result.returncode == 0, f"git {' '.join(args)} failed: {result.stderr}"
    return result.stdout.strip()


def new_repository(tmp_path: Path) -> Path:
    root = tmp_path / "repository"
    root.mkdir()
    run_git(root, "init", "-q", "-b", "main", ".")
    return root


def commit(root: Path, message: str) -> str:
    run_git(root, "add", "-A")
    run_git(root, "commit", "-q", "-m", message)
    return run_git(root, "rev-parse", "HEAD")


def released_repository(tmp_path: Path) -> tuple[Path, str]:
    root = new_repository(tmp_path)
    add_package(root, 2)
    generate(root)
    return root, commit(root, "release v2")


def track_upstream(root: Path, commit_sha: str, branch: str = "ratchet-base") -> None:
    run_git(root, "branch", branch, commit_sha)
    run_git(root, "branch", "--set-upstream-to", branch, "main")


def publish_origin_main(root: Path, commit_sha: str) -> None:
    run_git(root, "update-ref", "refs/remotes/origin/main", commit_sha)


def tamper(root: Path) -> str:
    frozen_contract(root).write_text("changed runtime behavior\n")
    generate(root)
    return commit(root, "tamper")


def test_frozen_file_edit_cannot_be_laundered_by_regeneration(tmp_path: Path) -> None:
    add_package(tmp_path, 2)
    generate(tmp_path)
    base_index = tmp_path / "base-index.json"
    base_index.write_bytes((tmp_path / INDEX).read_bytes())

    frozen_contract(tmp_path).write_text("changed runtime behavior\n")
    generate(tmp_path)

    result = run_gate(tmp_path, base_index)

    assert result.returncode == 1, result.stderr + result.stdout
    assert "cmd_executor/2" in result.stderr
    assert "contract.vy" in result.stderr


def test_explicit_base_index_needs_no_repository(tmp_path: Path) -> None:
    root = tmp_path / "plain"
    root.mkdir()
    add_package(root, 2)
    generate(root)
    base_index = tmp_path / "base-index.json"
    base_index.write_bytes((root / INDEX).read_bytes())

    result = run_gate(root, base_index)

    assert result.returncode == 0, result.stderr + result.stdout
    assert "clean" in result.stdout


def test_committed_release_index_is_clean() -> None:
    result = run_gate(ROOT, ROOT / INDEX)

    assert result.returncode == 0, result.stderr + result.stdout


def test_unregenerated_file_edit_fails_drift_check(tmp_path: Path) -> None:
    add_package(tmp_path, 2)
    generate(tmp_path)
    base_index = tmp_path / "base-index.json"
    base_index.write_bytes((tmp_path / INDEX).read_bytes())

    frozen_contract(tmp_path).write_text("changed runtime behavior\n")
    result = run_gate(tmp_path, base_index)

    assert result.returncode == 1, result.stderr + result.stdout
    assert "generated index drift" in result.stderr


def test_next_sequential_package_preserves_the_base_release(tmp_path: Path) -> None:
    add_package(tmp_path, 2)
    generate(tmp_path)
    base_index = tmp_path / "base-index.json"
    base_index.write_bytes((tmp_path / INDEX).read_bytes())

    add_package(tmp_path, 3)
    generate(tmp_path)
    result = run_gate(tmp_path, base_index)

    assert result.returncode == 0, result.stderr + result.stdout


def test_upstream_merge_base_detects_a_frozen_edit(tmp_path: Path) -> None:
    root, released = released_repository(tmp_path)
    track_upstream(root, released)
    tamper(root)

    result = run_gate(root)

    assert result.returncode == 1, result.stderr + result.stdout
    assert "frozen file changed" in result.stderr
    assert "contract.vy" in result.stderr


def test_origin_main_merge_base_detects_a_frozen_edit(tmp_path: Path) -> None:
    root, released = released_repository(tmp_path)
    publish_origin_main(root, released)
    tamper(root)

    result = run_gate(root)

    assert result.returncode == 1, result.stderr + result.stdout
    assert "frozen file changed" in result.stderr
    assert "contract.vy" in result.stderr


def test_explicit_base_environment_detects_a_frozen_edit(tmp_path: Path) -> None:
    root, released = released_repository(tmp_path)
    tamper(root)

    result = run_gate(root, base=released)

    assert result.returncode == 1, result.stderr + result.stdout
    assert "frozen file changed" in result.stderr


def test_missing_base_refuses_to_skip_preservation(tmp_path: Path) -> None:
    root, _ = released_repository(tmp_path)

    result = run_gate(root)

    assert result.returncode == 2, result.stderr + result.stdout
    assert "no independent release base" in result.stderr


def test_base_without_a_release_index_refuses(tmp_path: Path) -> None:
    root = new_repository(tmp_path)
    (root / "README.md").write_text("base without releases\n")
    base = commit(root, "base")
    add_package(root, 2)
    generate(root)
    commit(root, "release v2")

    result = run_gate(root, base=base)

    assert result.returncode == 2, result.stderr + result.stdout
    assert "index.json" in result.stderr


def test_base_with_an_unreadable_release_index_refuses(tmp_path: Path) -> None:
    root = new_repository(tmp_path)
    (root / INDEX).parent.mkdir(parents=True)
    (root / INDEX).write_text("{ not json\n")
    base = commit(root, "unreadable base")
    add_package(root, 2)
    generate(root)
    commit(root, "release v2")

    result = run_gate(root, base=base)

    assert result.returncode == 2, result.stderr + result.stdout
    assert "not valid JSON" in result.stderr


def test_explicit_head_base_advances_to_parent_and_catches_committed_tamper(
    tmp_path: Path,
) -> None:
    root, _ = released_repository(tmp_path)
    tamper(root)

    result = run_gate(root, base="HEAD")

    assert result.returncode == 1, result.stderr + result.stdout
    assert "frozen file changed" in result.stderr
    assert "contract.vy" in result.stderr


def test_inferred_head_base_advances_to_parent_and_catches_committed_tamper(
    tmp_path: Path,
) -> None:
    root, _ = released_repository(tmp_path)
    head = tamper(root)
    track_upstream(root, head, branch="mirror")

    result = run_gate(root)

    assert result.returncode == 1, result.stderr + result.stdout
    assert "frozen file changed" in result.stderr
    assert "contract.vy" in result.stderr


def test_head_without_a_parent_refuses(tmp_path: Path) -> None:
    root, _ = released_repository(tmp_path)

    result = run_gate(root, base="HEAD")

    assert result.returncode == 2, result.stderr + result.stdout
    assert "HEAD has no parent" in result.stderr


def test_inferred_head_without_a_parent_refuses(tmp_path: Path) -> None:
    root, released = released_repository(tmp_path)
    track_upstream(root, released)

    result = run_gate(root)

    assert result.returncode == 2, result.stderr + result.stdout
    assert "HEAD has no parent" in result.stderr


def test_clean_repository_with_an_inferred_base_is_clean(tmp_path: Path) -> None:
    root, released = released_repository(tmp_path)
    track_upstream(root, released)
    (root / "README.md").write_text("documentation after the release\n")
    commit(root, "docs")

    result = run_gate(root)

    assert result.returncode == 0, result.stderr + result.stdout
    assert "clean" in result.stdout


def test_sequential_release_addition_preserves_an_inferred_base(tmp_path: Path) -> None:
    root, released = released_repository(tmp_path)
    track_upstream(root, released)
    add_package(root, 3)
    generate(root)
    commit(root, "release v3")

    result = run_gate(root)

    assert result.returncode == 0, result.stderr + result.stdout


def test_regeneration_drift_runs_without_a_base(tmp_path: Path) -> None:
    root, _ = released_repository(tmp_path)
    frozen_contract(root).write_text("changed runtime behavior\n")

    result = run_gate(root)

    assert result.returncode == 2, result.stderr + result.stdout
    assert "generated index drift" in result.stderr
    assert "no independent release base" in result.stderr
