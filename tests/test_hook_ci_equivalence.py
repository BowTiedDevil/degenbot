"""Hook <-> CI equivalence probes (ADR-102).

A hook's CI coverage form is one of four, and three of them are checked here
after the registry's static pass. `equivalent` means CI runs a parallel
implementation -- the whole-tree sweep, the event-range lint, the whole-workspace
test lane -- so the registry names a probe, and the probe is what makes the claim
checkable: it reads the same `just --dump` the registry validates against and
pins the property that makes the two implementations one verdict at two
populations. A registry that names a probe whose assertions no longer cover the
pair is drift this suite catches.

Two gates have no failure fixture of their own -- the toolchain tier is a
precondition of other recipes, and the clippy form is the CI lint verdict -- so
their present behavior is pinned here rather than assumed.
"""

from __future__ import annotations

import os
import pathlib
import re
import shutil
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import tomllib

import pytest

from tests.repo_script import load_script

_ROOT = pathlib.Path(__file__).resolve().parents[1]
JUSTFILE = _ROOT / "justfile"
CI_YML = _ROOT / ".github" / "workflows" / "ci.yml"
CARGO_TOML = _ROOT / "rust" / "Cargo.toml"
STAGED_RUST_FMT = _ROOT / "scripts" / "hooks" / "rust-fmt-staged.sh"
STAGED_PY_FMT = _ROOT / "scripts" / "hooks" / "python-fmt-staged.sh"

gate_registry = load_script("gate_registry", _ROOT / "scripts" / "gate_registry.py")
just_recipe_index = load_script("just_recipe_index", _ROOT / "scripts" / "just_recipe_index.py")


@pytest.fixture(scope="module")
def index():
    return just_recipe_index.RecipeIndex(JUSTFILE)


@pytest.fixture(scope="module")
def gates() -> dict[str, object]:
    return {gate.id: gate for gate in gate_registry.load(_ROOT)}


def _hook(gates: dict[str, object], gate_id: str):
    gate = gates[gate_id]
    assert gate is not None, f"the registry declares no {gate_id!r} hook"
    return gate


def _bash() -> str:
    """Absolute path to bash (S607: never launch a bare name off PATH)."""
    exe = shutil.which("bash")
    assert exe is not None, "bash is not installed on PATH"
    return exe


def _workspace_edition() -> str:
    """The edition rustfmt must use for a workspace file: cargo fmt's source."""
    manifest = tomllib.loads(CARGO_TOML.read_text(encoding="utf-8"))
    return manifest["workspace"]["package"]["edition"]


def _run_staged_rustfmt(
    tmp_path: pathlib.Path, *files: str
) -> tuple[list[str], subprocess.CompletedProcess[str]]:
    """Run the staged hook with rustfmt replaced by a recording stub."""
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    argv_file = tmp_path / "rustfmt-argv.txt"
    stub = "#!/bin/sh\n" + 'printf "%s\\n" "$@" >> ' + str(argv_file) + "\n"
    rustfmt = bin_dir / "rustfmt"
    rustfmt.write_text(stub, encoding="utf-8")
    rustfmt.chmod(0o755)
    env = dict(os.environ)
    env["PATH"] = str(bin_dir) + os.pathsep + env["PATH"]
    result = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [_bash(), str(STAGED_RUST_FMT), *files],
        cwd=_ROOT,
        env=env,
        check=False,
        capture_output=True,
        text=True,
    )
    recorded = argv_file.read_text(encoding="utf-8").splitlines() if argv_file.exists() else []
    return recorded, result


def _ci_job(name: str) -> str:
    """The named job's block: its key through the line before the next job key."""
    block: list[str] = []
    key = f"  {name}:"
    for line in CI_YML.read_text(encoding="utf-8").splitlines():
        if re.fullmatch(r"  [A-Za-z0-9_-]+:", line):
            if block:
                break
            if line != key:
                continue
        if block or line == key:
            block.append(line)
    assert block, f"{CI_YML} declares no job {name!r}"
    return "\n".join(block)


def test_rust_fmt_staged_slices_the_whole_tree_fmt_check(
    tmp_path: pathlib.Path, index, gates
) -> None:
    """Same formatter, same edition, same non-mutating check; the list is sliced.

    The whole-tree sweep is `cargo fmt --all -- --check`; the commit-time hook
    is rustfmt over the staged files. They are one verdict only while the hook
    carries the workspace edition and cannot recurse into an unstaged sibling,
    so both are read here instead of being described in prose.
    """
    whole_tree = "\n".join(index.body("fmt-check"))
    assert "cargo fmt" in whole_tree
    assert "--all" in whole_tree
    assert "-- --check" in whole_tree

    hook = _hook(gates, "rust-fmt-staged")
    assert hook.triggers["types"] == ["rust"]
    assert "exclude" not in hook.triggers, (
        "a staged rust file the hook excludes is a file the push sweep alone judges"
    )

    edition = _workspace_edition()
    recorded, result = _run_staged_rustfmt(tmp_path, "lib.rs", "child/mod.rs")

    assert result.returncode == 0, result.stdout + result.stderr
    assert recorded == [
        "--check",
        "--edition",
        edition,
        "--config",
        "skip_children=true",
        "--",
        "lib.rs",
        "child/mod.rs",
    ], recorded


def test_python_fmt_staged_slices_the_whole_tree_sweep(index, gates) -> None:
    """The hook's file list is the tracked python population minus `executor/`.

    `just fmt-check-python` enumerates `git ls-files '*.py'` and drops the
    vendored tree; prek hands the hook this commit's `.py` files with the same
    exclusion. Same detector, so a fully staged tree and the push sweep judge
    one population.
    """
    whole_tree = "\n".join(index.body("fmt-check-python"))
    assert "git ls-files -z '*.py'" in whole_tree
    assert "grep -zv '^executor/'" in whole_tree
    assert "ruff format --check" in whole_tree

    hook = _hook(gates, "python-fmt-staged")
    assert hook.triggers["types"] == ["python"]
    assert hook.triggers["exclude"] == "^executor/"

    assert _hook(gates, "python-fmt-check").scope == "staged", (
        "the pre-push sweep enumerates git's index (git ls-files), so its population "
        "is `staged`: another lane's untracked python is outside the run"
    )

    staged = STAGED_PY_FMT.read_text(encoding="utf-8")
    assert "ruff format --check" in staged


def test_the_executor_release_ci_entry_is_the_hook_minus_its_pytest_probes(index, gates) -> None:
    """CI runs the hook's body half; the hook's remainder is its pytest file.

    The hook spends most of its time in the pytest probes, which already run in
    the suite CI's python job executes. The live-tree half is therefore the
    hook's declared dependency, and the equivalence is that the two halves
    together are the whole hook -- not a second, shorter gate.
    """
    hook = _hook(gates, "executor-release-ratchet")
    assert hook.entry == "just check-executor-release-ratchet"
    assert hook.coverage.relation == "equivalent"
    assert hook.coverage.entry == "just check-executor-release-ratchet-index"
    assert hook.coverage.entry in _ci_job("policy-gates")

    dependencies = {
        dependency["recipe"]
        for dependency in index.dump()["recipes"]["check-executor-release-ratchet"]["dependencies"]
    }
    assert "check-executor-release-ratchet-index" in dependencies

    probe_files = {probe.split("::")[0] for probe in hook.probe}
    assert "tests/test_executor_release_ratchet.py" in probe_files
    assert "tests/test_executor_release_ratchet.py" in "\n".join(
        index.body("check-executor-release-ratchet")
    )


def test_the_whole_workspace_lane_is_the_ci_counterpart_of_the_affected_selection(
    index, gates
) -> None:
    """The push leg narrows the same nextest run whose default is the workspace.

    `test-rust-affected` turns the outgoing diff into `-p` selections for
    `test-rust-nextest`, whose own default selection is `--workspace`; CI's
    rust-test job runs the workspace suite. So the CI counterpart is the same
    invocation over a superset of packages -- never a different suite.
    """
    hook = _hook(gates, "rust-test-affected")
    assert hook.coverage.relation == "equivalent"
    assert hook.coverage.entry == "just test-rust-dev"
    assert hook.coverage.entry in _ci_job("rust-test")

    affected = "\n".join(index.body("test-rust-affected"))
    assert "scripts/rust_affected_packages.py" in affected
    assert "just test-rust-nextest $packages" in affected

    nextest = "\n".join(index.body("test-rust-nextest"))
    assert 'selection="--workspace"' in nextest
    assert "cargo nextest run" in nextest

    workspace = "\n".join(index.body("test-rust-dev"))
    assert "cargo test" in workspace
    assert "--workspace" in workspace


def test_the_whole_tree_gates_certify_the_toolchain_tier_first(index) -> None:
    """A gate that judges code the wrong toolchain built is not a verdict."""
    for recipe in ("fmt-check", "lint-rust-check", "test-rust-dev", "test-rust-affected"):
        assert index.reaches(recipe, "check-toolchain"), recipe


def test_the_clippy_recipe_denies_warnings_on_the_strict_tier(index) -> None:
    """CI's rust-lint job runs this form: the strict tier's deny-warnings clippy."""
    body = "\n".join(index.body("lint-rust-check"))

    assert "cargo clippy" in body
    assert "--workspace" in body
    assert "--deny warnings" in body
    assert "scripts/toolchain_tier.sh" in body
