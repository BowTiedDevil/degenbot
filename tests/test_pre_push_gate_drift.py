"""Pre-push gate drift brake (`scripts/hooks/pre-push-gate-drift.sh`).

`just pre-push` is the pre-push check AGENTS.md points every agent at before
`git push`, and its gate list is a hand-written transcription of the
`stages = ["pre-push"]` entries in prek.toml. A transcription is an artifact
that drifts, and this one already did: it ran four gates short and pointed two
of them at a different entrypoint than the hook, so `just pre-push` reported
green while `git push` was blocked by a gate the recipe never ran.

The brake compares the two lists and fails on any difference. It must compare
entries (the command each gate runs) in declaration order, must fail closed when
either list cannot be produced -- a green that examined nothing is exactly the
failure mode this exists to prevent -- and must never accept a partial match.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import tomllib
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "hooks" / "pre-push-gate-drift.sh"
PREK_TOML = REPO_ROOT / "prek.toml"
JUSTFILE = REPO_ROOT / "justfile"


def _sh() -> str:
    """Absolute path to bash (S607: never launch a bare name off PATH)."""
    exe = shutil.which("bash")
    assert exe is not None, "bash is not installed on PATH"
    return exe


def _run_gate(**overrides: str) -> subprocess.CompletedProcess[str]:
    env = dict(os.environ)
    env.pop("PREK", None)
    env.pop("JF", None)
    env.update(overrides)
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [_sh(), str(SCRIPT)],
        cwd=REPO_ROOT,
        env=env,
        check=False,
        capture_output=True,
        text=True,
    )


def _prek_entries(path: Path = PREK_TOML) -> list[str]:
    """The pre-push hook entries, read independently through a TOML parser.

    Deliberately not the shell script's own sed: the point of these tests is to
    catch an extraction that quietly stops seeing what it is supposed to see.
    """
    with path.open("rb") as handle:
        config = tomllib.load(handle)
    return [
        hook["entry"]
        for entry in config["repos"]
        for hook in entry.get("hooks", [])
        if "pre-push" in hook.get("stages", [])
    ]


def _recipe_gate_lines(path: Path = JUSTFILE) -> list[str]:
    """The `run_gate` call lines of the `pre-push` recipe, in declaration order."""
    lines: list[str] = []
    in_recipe = False
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.startswith("pre-push:"):
            in_recipe = True
            continue
        if in_recipe and line and not line[0].isspace() and not line.startswith("#"):
            break
        if in_recipe and re.match(r'^\s*run_gate "', line):
            lines.append(line)
    return lines


def _recipe_entries(path: Path = JUSTFILE) -> list[str]:
    """The `run_gate` commands of the `pre-push` recipe, in declaration order."""
    return [
        re.match(r'^\s*run_gate "[^"]*"\s*(.*)$', line).group(1).strip()
        for line in _recipe_gate_lines(path)
    ]


def test_the_recipe_transcribes_the_pre_push_stage() -> None:
    from_prek = _prek_entries()
    from_recipe = _recipe_entries()

    assert from_prek, "no pre-push hooks found in prek.toml"
    assert from_recipe, "no run_gate lines found in the just pre-push recipe"
    assert from_prek == from_recipe, (
        "the just pre-push recipe must list the same gates, in the same order, "
        "as the prek.toml pre-push stage"
    )

    result = _run_gate()
    assert result.returncode == 0, result.stdout + result.stderr
    assert "ok" in result.stdout


def test_a_gate_dropped_from_the_recipe_is_drift(tmp_path: Path) -> None:
    dropped = "just check-no-pyo3-in-cores"
    assert dropped in _recipe_entries()
    mutated = tmp_path / "justfile"
    mutated.write_text(
        "\n".join(
            line
            for line in JUSTFILE.read_text(encoding="utf-8").splitlines()
            if dropped not in line
        ),
        encoding="utf-8",
    )
    assert dropped not in _recipe_entries(mutated), "the mutation must actually drop the gate"

    result = _run_gate(JF=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr
    assert dropped in result.stderr, "the report must name the gate the recipe dropped"
    assert "false green" in result.stderr


def test_a_hook_added_to_prek_only_is_drift(tmp_path: Path) -> None:
    extra = "scripts/hooks/extra-gate.sh"
    anchor = '  { id = "commitlint-push"'
    added = (
        '  { id = "extra-gate", name = "Extra", language = "system", '
        f'entry = "{extra}", stages = ["pre-push"], always_run = true, '
        "pass_filenames = false, fail_fast = true },\n"
        "\n"
    )
    mutated = tmp_path / "prek.toml"
    mutated.write_text(
        PREK_TOML.read_text(encoding="utf-8").replace(anchor, added + anchor), encoding="utf-8"
    )
    assert extra in _prek_entries(mutated), "the mutation must actually add the hook"

    result = _run_gate(PREK=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr
    assert extra in result.stderr, "the report must name the hook the recipe never runs"


def test_a_reordered_gate_is_drift(tmp_path: Path) -> None:
    # Order is load-bearing (python-build must precede python-test), so a
    # transcription that reorders the gates is drift even with the same set.
    gate_lines = _recipe_gate_lines()
    assert len(gate_lines) > 2

    lines = JUSTFILE.read_text(encoding="utf-8").splitlines()
    positions = [i for i, line in enumerate(lines) if line in gate_lines]
    assert len(positions) == len(gate_lines)
    lines[positions[0]], lines[positions[1]] = lines[positions[1]], lines[positions[0]]
    mutated = tmp_path / "justfile"
    mutated.write_text("\n".join(lines), encoding="utf-8")

    before = _recipe_entries()
    after = _recipe_entries(mutated)
    assert after == [before[1], before[0], *before[2:]], "the mutation must actually reorder"

    result = _run_gate(JF=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr


def test_an_absent_source_fails_closed(tmp_path: Path) -> None:
    result = _run_gate(JF=str(tmp_path / "no-such-justfile"))

    assert result.returncode == 2, result.stdout + result.stderr
    assert "ok" not in result.stdout


def test_a_source_that_yields_no_gates_fails_closed(tmp_path: Path) -> None:
    empty = tmp_path / "justfile"
    empty.write_text("")

    result = _run_gate(JF=str(empty))

    assert result.returncode == 2, result.stdout + result.stderr
    assert "failing closed" in result.stderr
    assert "ok" not in result.stdout


def test_gate_is_wired_into_the_pre_push_stage() -> None:
    with PREK_TOML.open("rb") as handle:
        config = tomllib.load(handle)

    hooks = [
        hook
        for entry in config["repos"]
        for hook in entry.get("hooks", [])
        if hook["id"] == "pre-push-gate-drift"
    ]
    assert len(hooks) == 1, "pre-push-gate-drift must be registered exactly once in prek.toml"
    assert "pre-push" in hooks[0]["stages"]
    assert hooks[0]["always_run"] is True

    recipe = _recipe_entries()
    assert "scripts/hooks/pre-push-gate-drift.sh" in recipe, "just pre-push must run the gate"
    assert recipe.index("scripts/hooks/pre-push-gate-drift.sh") < recipe.index(
        "just build-rust-extension"
    ), "the drift check must run before the expensive gates it validates"
