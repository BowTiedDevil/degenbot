"""CI-parity brake (`scripts/hooks/ci-parity.sh`).

Every ci.yml job declares how it is reproduced locally in
`scripts/hooks/ci-parity-manifest.txt`, whose lines are
`job|kind|push_gates|notes`. The brake fails when a job has no manifest line,
when a note names a `just` recipe that does not exist, and when a `push_gates`
entry is not a `stages = ["pre-push"]` entry in prek.toml -- the push-gate
manifest `just pre-push` delegates to (ADR-074).

These are the gate's negative probes: each mutation is applied to a throwaway
copy of the source and must produce an observed nonzero exit. The env overrides
(`MANIFEST`, `CI`, `JF`, `PREK`) exist for exactly that.
"""

from __future__ import annotations

import os
import shutil
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import tomllib
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "hooks" / "ci-parity.sh"
MANIFEST = REPO_ROOT / "scripts" / "hooks" / "ci-parity-manifest.txt"
PREK_TOML = REPO_ROOT / "prek.toml"
CI_YML = REPO_ROOT / ".github" / "workflows" / "ci.yml"

# The sed extraction ci-parity.sh uses to read prek.toml's pre-push entry
# strings. Kept byte-identical to the script's copy; the parser backstop below
# asserts it appears verbatim in the script, so the two cannot drift silently.
SED_PATTERN = r's/^[[:space:]]*{ id = .*entry = "\([^"]*\)".*stages = \["pre-push"\].*/\1/p'


def _sh() -> str:
    """Absolute path to bash (S607: never launch a bare name off PATH)."""
    exe = shutil.which("bash")
    assert exe is not None, "bash is not installed on PATH"
    return exe


def _sed() -> str:
    """Absolute path to sed (S607: never launch a bare name off PATH)."""
    exe = shutil.which("sed")
    assert exe is not None, "sed is not installed on PATH"
    return exe


def _run_ci_parity(**overrides: str) -> subprocess.CompletedProcess[str]:
    env = dict(os.environ)
    for var in ("MANIFEST", "CI", "JF", "PREK", "TOOLS"):
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


def _push_gate_row() -> tuple[str, str]:
    """The first manifest row that declares push gates: its line and the entry."""
    for line in MANIFEST.read_text(encoding="utf-8").splitlines():
        fields = line.split("|")
        if len(fields) >= 3 and fields[2].strip():
            return line, fields[2].strip()
    msg = "no manifest row declares a push gate; the probes below would be vacuous"
    raise AssertionError(msg)


def _hook_line_for(entry: str) -> str:
    """The prek.toml hook line running `entry`, so a probe never pins wiring text."""
    for line in PREK_TOML.read_text(encoding="utf-8").splitlines():
        if line.lstrip().startswith("{ id = ") and f'entry = "{entry}"' in line:
            return line
    msg = f"no hook in prek.toml runs {entry!r}"
    raise AssertionError(msg)


def test_parity_holds_on_the_committed_tree() -> None:
    result = _run_ci_parity()

    assert result.returncode == 0, result.stdout + result.stderr
    assert "jobs covered" in result.stdout
    assert "FAILED" not in result.stderr


def test_the_manifest_is_a_file_the_script_reads() -> None:
    script = SCRIPT.read_text(encoding="utf-8")

    assert "ci-parity-manifest.txt" in script, "the script must read the promoted manifest"
    assert MANIFEST.is_file()
    assert MANIFEST.read_text(encoding="utf-8").strip()


def test_a_push_gates_entry_outside_the_push_gate_manifest_fails(tmp_path: Path) -> None:
    # `just test-fork` is a real recipe, so the notes' existence check passes
    # and the failure can only come from the push_gates wiring check. The row is
    # the first that declares one, so the probe follows the manifest rather than
    # pinning a line that a re-wiring would leave stale.
    row, gate = _push_gate_row()
    mutated = _mutate(tmp_path, MANIFEST, row, row.replace(gate, "just test-fork", 1))

    result = _run_ci_parity(MANIFEST=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr
    assert "test-fork" in result.stderr
    assert "does not exist" not in result.stderr, "the recipe exists; wiring is what fails"
    assert "pre-push" in result.stderr


def test_an_old_format_three_field_line_fails_closed(tmp_path: Path) -> None:
    # A pre-typing 3-field line lands with an empty notes field (fail closed).
    mutated = _mutate(
        tmp_path,
        MANIFEST,
        "markdownlint|local||just lint-markdown",
        "markdownlint|local|just lint-markdown",
    )

    result = _run_ci_parity(MANIFEST=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr
    assert "markdownlint" in result.stderr
    assert "needs kind AND notes" in result.stderr


def test_a_recipe_dropped_from_the_push_gate_manifest_fails(tmp_path: Path) -> None:
    # Whichever hook the manifest's push-gate row names is the one to drop, so
    # retiring or re-pointing a gate cannot leave this probe asserting a line
    # that no longer exists.
    _, gate = _push_gate_row()
    mutated = _mutate(tmp_path, PREK_TOML, _hook_line_for(gate) + "\n", "")

    result = _run_ci_parity(PREK=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr
    assert gate in result.stderr


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
        "markdownlint|local||just lint-markdown",
        "markdownlint|local||just no-such-recipe-xyz",
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


def test_sed_extraction_matches_a_real_toml_parser() -> None:
    """The sed extraction is the gate's blind spot; a TOML parser backstops it.

    ci-parity.sh derives prek.toml's pre-push entry strings with a sed anchored
    on single-line `{ id = ... }` tables, so a multi-line hook table would be
    invisible to it. Re-deriving the same list through a real TOML parser makes
    that divergence a red test instead of a comment.
    """
    script = SCRIPT.read_text(encoding="utf-8")
    assert SED_PATTERN in script, (
        "the test's sed pattern must stay byte-identical to the script's copy"
    )

    parsed = tomllib.loads(PREK_TOML.read_text(encoding="utf-8"))
    toml_entries = [
        hook["entry"]
        for repo in parsed.get("repos", [])
        for hook in repo.get("hooks", [])
        if "pre-push" in hook.get("stages", [])
    ]
    assert toml_entries, 'prek.toml declares no stages = ["pre-push"] entries'

    result = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [_sed(), "-n", SED_PATTERN, str(PREK_TOML)],
        cwd=REPO_ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, result.stderr
    sed_entries = [line for line in result.stdout.splitlines() if line]

    assert sed_entries == toml_entries


TOOLS = REPO_ROOT / "scripts" / "hooks" / "ci-recipe-tools.txt"


def test_the_requirement_table_provisions_every_recipe_toolchain() -> None:
    """The committed tree passes the toolchain reconciliation too."""
    result = _run_ci_parity()

    assert result.returncode == 0, result.stdout + result.stderr
    assert "recipe toolchains provisioned" in result.stdout


def test_a_job_missing_a_tool_its_recipe_needs_fails(tmp_path: Path) -> None:
    """The tier-3 job without its uv step cannot run the vyper compile."""
    mutated = _mutate(
        tmp_path,
        CI_YML,
        "      - name: Install uv (executor vyper toolchain)\n"
        "        uses: astral-sh/setup-uv@37802adc94f370d6bfd71619e3f0bf239e1f3b78 # v7",
        "      - name: Provision nothing\n        run: echo skipped",
    )

    result = _run_ci_parity(CI=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr
    assert "tier3-oracle" in result.stderr
    assert "verify-tier3-executor-artifact" in result.stderr
    assert "uv" in result.stderr


def test_a_wrong_install_action_tool_fails(tmp_path: Path) -> None:
    """`taiki-e/install-action` counts only when it installs nextest itself."""
    mutated = _mutate(
        tmp_path,
        CI_YML,
        "          tool: nextest@0.9.152",
        "          tool: cargo-binstall",
    )

    result = _run_ci_parity(CI=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr
    assert "cargo-nextest" in result.stderr


def test_a_recipe_a_job_runs_without_a_row_fails(tmp_path: Path) -> None:
    """A new `just` invocation in a job has to declare its toolchain."""
    # `just test-fork` is a real recipe, so the name-existence check passes and
    # this failure can only come from the missing requirement row.
    mutated = _mutate(tmp_path, CI_YML, "run: just check-uv-lock", "run: just test-fork")

    result = _run_ci_parity(CI=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr
    assert "test-fork" in result.stderr
    assert "no row" in result.stderr


def test_a_row_no_job_runs_fails(tmp_path: Path) -> None:
    """A requirement row that outlives its CI caller is a stale claim."""
    mutated = _mutate(
        tmp_path,
        TOOLS,
        "fmt-check-python||",
        "fmt-check-python||\norphan-recipe|cargo-nextest|",
    )

    result = _run_ci_parity(CI=str(CI_YML), TOOLS=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr
    assert "orphan-recipe" in result.stderr
    assert "no ci.yml job runs" in result.stderr


def test_a_job_without_the_llvm_toolchain_fails(tmp_path: Path) -> None:
    """A job that compiles default features needs the LLVM 23 composite action."""
    mutated = _mutate(
        tmp_path,
        CI_YML,
        "      - uses: ./.github/actions/llvm23\n\n      - name: Compile workspace at MSRV",
        "\n      - name: Compile workspace at MSRV",
    )

    result = _run_ci_parity(CI=str(mutated))

    assert result.returncode == 1, result.stdout + result.stderr
    assert "msrv" in result.stderr
    assert "llvm23" in result.stderr
