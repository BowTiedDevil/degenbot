"""CI-parity brake (`scripts/hooks/ci-parity.sh`).

Every ci.yml job declares how it is reproduced locally in
`scripts/hooks/ci-parity-manifest.txt`. The brake fails when a job has no
manifest line, when a manifest line names a `just` recipe that does not exist,
and when a note claiming pre-push wiring names a recipe that is not a
`stages = ["pre-push"]` entry in prek.toml -- the push-gate manifest `just
pre-push` delegates to (ADR-074).

These are the gate's negative probes: each mutation is applied to a throwaway
copy of the source and must produce an observed nonzero exit. The env overrides
(`MANIFEST`, `CI`, `JF`, `PREK`) exist for exactly that.
"""

from __future__ import annotations

import os
import shutil
import subprocess  # ruff: ignore[suspicious-subprocess-import]
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "hooks" / "ci-parity.sh"
MANIFEST = REPO_ROOT / "scripts" / "hooks" / "ci-parity-manifest.txt"
PREK_TOML = REPO_ROOT / "prek.toml"
CI_YML = REPO_ROOT / ".github" / "workflows" / "ci.yml"


def _sh() -> str:
    """Absolute path to bash (S607: never launch a bare name off PATH)."""
    exe = shutil.which("bash")
    assert exe is not None, "bash is not installed on PATH"
    return exe


def _run_ci_parity(**overrides: str) -> subprocess.CompletedProcess[str]:
    env = dict(os.environ)
    for var in ("MANIFEST", "CI", "JF", "PREK"):
        env.pop(var, None)
    env.update(overrides)
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [_sh(), str(SCRIPT)],
        cwd=REPO_ROOT,
        env=env,
        check=False,
        capture_output=True,
        text=True,
    )


def _mutate(tmp_path: Path, source: Path, old: str, new: str) -> Path:
    """Write `source` with its first `old` replaced by `new`; return the copy."""
    text = source.read_text(encoding="utf-8")
    assert old in text, f"the mutation anchor is gone from {source}: {old!r}"
    mutated = tmp_path / source.name
    mutated.write_text(text.replace(old, new, 1), encoding="utf-8")
    return mutated


def test_parity_holds_on_the_committed_tree() -> None:
    result = _run_ci_parity()

    assert result.returncode == 0, result.stdout + result.stderr
    assert "jobs covered" in result.stdout
    assert "FAILED" not in result.stderr


def test_the_manifest_is_a_file_the_script_reads() -> None:
    script = SCRIPT.read_text(encoding="utf-8")

    assert "ci-parity-manifest.txt" in script, "the script must read the promoted manifest"
    assert "cat <<'EOF'" not in script, "the heredoc manifest is gone"
    assert "run_gate" not in script, "the wiring check must read prek.toml, not run_gate lines"
    assert MANIFEST.is_file()
    assert MANIFEST.read_text(encoding="utf-8").strip()


def test_a_note_naming_a_recipe_outside_the_push_gate_manifest_fails(tmp_path: Path) -> None:
    # `just test-fork` is a real recipe, so the existence check passes and the
    # failure can only come from the push-gate wiring check.
    mutated = _mutate(
        tmp_path,
        MANIFEST,
        "just ci-no-python-cli-gate (in just pre-push)",
        "just test-fork (in just pre-push)",
    )

    result = _run_ci_parity(MANIFEST=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr
    assert "test-fork" in result.stderr
    assert "does not exist" not in result.stderr, "the recipe exists; wiring is what fails"
    assert "pre-push" in result.stderr


def test_a_recipe_dropped_from_the_push_gate_manifest_fails(tmp_path: Path) -> None:
    dropped = '  { id = "stub-drift", name = "Stub drift gate", language = "system", '
    dropped += 'entry = "just lint-stub-drift", stages = ["pre-push"], always_run = true, '
    dropped += "pass_filenames = false, fail_fast = true },\n"
    mutated = _mutate(tmp_path, PREK_TOML, dropped, "")

    result = _run_ci_parity(PREK=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr
    assert "lint-stub-drift" in result.stderr


def test_a_ci_job_without_a_manifest_line_fails(tmp_path: Path) -> None:
    mutated = _mutate(
        tmp_path,
        CI_YML,
        "jobs:\n",
        "jobs:\n  bogus-job:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo hi\n",
    )

    result = _run_ci_parity(CI=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr
    assert "bogus-job" in result.stderr
    assert "has no manifest line" in result.stderr


def test_a_manifest_entry_naming_a_nonexistent_recipe_fails(tmp_path: Path) -> None:
    mutated = _mutate(
        tmp_path,
        MANIFEST,
        "markdownlint|local|just lint-markdown",
        "markdownlint|local|just no-such-recipe-xyz",
    )

    result = _run_ci_parity(MANIFEST=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr
    assert "no-such-recipe-xyz" in result.stderr
    assert "does not exist" in result.stderr


def test_a_missing_manifest_fails_closed(tmp_path: Path) -> None:
    result = _run_ci_parity(MANIFEST=str(tmp_path / "no-such-manifest.txt"))

    assert result.returncode == 2, result.stdout + result.stderr
    assert "refusing to pass" in result.stderr
    assert "jobs covered" not in result.stdout


def test_an_empty_push_gate_manifest_fails_closed(tmp_path: Path) -> None:
    empty = tmp_path / "prek.toml"
    empty.write_text("", encoding="utf-8")

    result = _run_ci_parity(PREK=str(empty))

    assert result.returncode == 2, result.stdout + result.stderr
    assert "refusing to pass" in result.stderr
    assert "jobs covered" not in result.stdout
