"""Pre-push object-database GC gate (`scripts/hooks/object-gc.sh`).

Background: a run staged `rust/target` build output and never committed it,
leaving 139,329 loose objects / 44.5 GiB in `.git/objects`, all unreachable.
`git gc --auto` (fired by that `git push`) then ran `git repack` ->
`pack-objects` over the pile: 663% CPU, 21.4 GB RSS, delta-compressing 100 MB
ELF binaries, and it never terminated.

The gate is the brake: it notices the pathological object DB *before* the push
and removes the unreachable pile with `git prune --expire=now` (the cheap,
reflog-respecting operation — 40 s for that 44.5 GiB, vs. an unbounded repack).
It must never invoke the repack/pack-objects path that blew up, and it must
never touch reachable or reflog-protected objects.
"""

from __future__ import annotations

import os
import shutil
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import tomllib
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "hooks" / "object-gc.sh"

# Thresholds are env-tunable precisely so the tests can force the gate's
# decision branches on tiny repos. Defaults are exercised by test 5.
DEFAULTS = {
    "DEGENBOT_OBJECT_GC_MAX_COUNT": "2000",
    "DEGENBOT_OBJECT_GC_MAX_KIB": "204800",
    "DEGENBOT_OBJECT_GC_HARD_MAX_COUNT": "50000",
    "DEGENBOT_OBJECT_GC_HARD_MAX_KIB": "2097152",
}


def _git_executable() -> str:
    """Absolute path to git (S607: never launch a bare name off PATH)."""
    exe = shutil.which("git")
    assert exe is not None, "git is not installed on PATH"
    return exe


def _git(repo: Path, *args: str) -> str:
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [_git_executable(), "-C", str(repo), *args],
        check=True,
        capture_output=True,
        text=True,
    ).stdout


def _make_repo(tmp_path: Path) -> Path:
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "-q")
    _git(repo, "config", "user.email", "gc-gate@example.invalid")
    _git(repo, "config", "user.name", "gc gate")
    (repo / "tracked.txt").write_text("hello\n")
    _git(repo, "add", "tracked.txt")
    _git(repo, "commit", "-q", "-m", "test: seed")
    return repo


def _count_objects(repo: Path) -> dict[str, int]:
    stats: dict[str, int] = {}
    for line in _git(repo, "count-objects", "-v").splitlines():
        key, _, value = line.partition(":")
        if key in {"count", "size", "garbage"}:
            stats[key] = int(value.strip())
    return stats


def _loose_object_files(repo: Path) -> set[str]:
    objects = repo / ".git" / "objects"
    return {
        entry.name
        for fanout in objects.iterdir()
        if fanout.is_dir() and len(fanout.name) == 2
        for entry in fanout.iterdir()
    }


def _add_unreachable_blobs(repo: Path, tmp_path: Path, count: int) -> None:
    """Write loose blobs that no ref, index, or reflog can reach."""
    blob = tmp_path / "junk.bin"
    for index in range(count):
        blob.write_bytes(b"unreachable-junk-" + str(index).encode() * 128)
        _git(repo, "hash-object", "-w", str(blob))


def _run_gate(
    repo: Path,
    *,
    use_defaults: bool = True,
    **overrides: str,
) -> subprocess.CompletedProcess[str]:
    env = {
        key: value for key, value in os.environ.items() if not key.startswith("DEGENBOT_OBJECT_GC_")
    }
    if use_defaults:
        env.update(DEFAULTS)
    env.update(overrides)
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [str(SCRIPT)],
        cwd=repo,
        env=env,
        check=False,
        capture_output=True,
        text=True,
    )


def test_healthy_repo_is_left_untouched(tmp_path: Path) -> None:
    repo = _make_repo(tmp_path)
    before = _loose_object_files(repo)

    # No overrides: this is the script's own built-in threshold set, which is
    # what an actual push runs (a healthy repo must never be touched by it).
    result = _run_gate(repo, use_defaults=False)

    assert result.returncode == 0, result.stderr
    assert _loose_object_files(repo) == before
    assert "prun" not in result.stdout.lower()


def test_prunes_unreachable_objects_above_soft_threshold(tmp_path: Path) -> None:
    repo = _make_repo(tmp_path)
    _add_unreachable_blobs(repo, tmp_path, 25)
    assert _count_objects(repo)["count"] > 20

    result = _run_gate(repo, DEGENBOT_OBJECT_GC_MAX_COUNT="5")

    assert result.returncode == 0, result.stderr
    assert _count_objects(repo)["count"] <= 5
    # The cleanup is surgical: history, refs, and the work tree are untouched.
    assert "test: seed" in _git(repo, "log", "--oneline")
    _git(repo, "cat-file", "-e", "HEAD^{commit}")
    assert not _git(repo, "status", "--porcelain")


def test_reachable_objects_survive_a_forced_prune(tmp_path: Path) -> None:
    repo = _make_repo(tmp_path)
    tracked = repo / "tracked.txt"
    tracked.write_text("updated\n")
    _git(repo, "add", "tracked.txt")
    _git(repo, "commit", "-q", "-m", "test: second")
    head = _git(repo, "rev-parse", "HEAD").strip()

    # Thresholds of 0 force the prune branch; every remaining object is
    # reachable, so the gate must leave all of them alone.
    result = _run_gate(repo, DEGENBOT_OBJECT_GC_MAX_COUNT="0", DEGENBOT_OBJECT_GC_MAX_KIB="0")

    assert result.returncode == 0, result.stderr
    assert _git(repo, "rev-parse", "HEAD").strip() == head
    _git(repo, "cat-file", "-e", f"{head}^{{tree}}")
    assert not _git(repo, "status", "--porcelain")
    assert tracked.read_text() == "updated\n"


def test_hard_threshold_blocks_the_push_when_prune_cannot_help(tmp_path: Path) -> None:
    repo = _make_repo(tmp_path)

    result = _run_gate(
        repo,
        DEGENBOT_OBJECT_GC_MAX_COUNT="0",
        DEGENBOT_OBJECT_GC_HARD_MAX_COUNT="0",
    )

    assert result.returncode == 1
    output = (result.stdout + result.stderr).lower()
    assert "blocked" in output
    assert "prune" in output or "repack" in output


def test_gate_is_wired_into_the_pre_push_stage() -> None:
    with (REPO_ROOT / "prek.toml").open("rb") as handle:
        config = tomllib.load(handle)

    hooks = [
        hook
        for entry in config["repos"]
        for hook in entry.get("hooks", [])
        if hook["id"] == "object-gc"
    ]
    assert len(hooks) == 1, "object-gc must be registered exactly once in prek.toml"
    assert "pre-push" in hooks[0]["stages"]
    assert hooks[0]["always_run"] is True

    justfile = (REPO_ROOT / "justfile").read_text()
    assert "scripts/hooks/object-gc.sh" in justfile, "just pre-push must mirror the gate"
