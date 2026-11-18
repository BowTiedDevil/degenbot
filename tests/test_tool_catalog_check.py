"""Negative probes for the tool-catalog reconciliation gate.

``scripts/tool_catalog_check.py`` reconciles ``rust/tools/README.md`` against
``cargo metadata`` and the current justfile. Every case here drives the real
gate -- over the committed workspace, or over a throwaway workspace laid out
exactly like it (a ``--metadata`` JSON dump standing in for the cargo child,
so the synthetic cases never invoke cargo):

  * ``test_the_committed_catalog_reconciles_with_the_live_workspace`` -- the
    real gate over the real tree, real metadata included.
  * ``test_an_added_target_without_a_row_fails`` /
    ``test_a_row_whose_target_left_the_manifests_fails`` -- the two coverage
    directions over the lab population.
  * ``test_a_duplicated_row_fails`` -- one target cataloged twice.
  * ``test_a_malformed_row_fails`` / ``test_a_missing_source_link_fails`` --
    structural defects.
  * ``test_source_drift_fails`` -- a source link that resolves to a live file
    that is not the target's ``src_path``.
  * ``test_package_drift_fails`` -- a row under one tool package's section
    pointing at another package's ``[[bin]]``.
  * ``test_lane_drift_fails`` / ``test_a_secondary_build_shape_lane_passes`` --
    the documented Lane against ``required-features``, with the explicitly
    documented secondary shape ignored.
  * ``test_an_unknown_recipe_fails`` -- a ``just <recipe>`` the justfile does
    not define.
  * ``test_just_directives_are_not_recipes`` -- the justfile's top-level
    directives (``set``/``alias``/``export``) and ``:=`` variables are not
    recipe definitions: a row referencing them fails while a row referencing
    a real recipe passes.
  * ``test_a_prose_just_reference_is_not_a_recipe_reference`` -- ``just
    <recipe>`` extraction reads Markdown code spans only; prose 'just any
    word' is not a recipe reference.
  * ``test_a_nonlocal_source_link_fails`` -- a URL and a root escape.
  * ``test_a_declared_count_that_disagrees_with_the_table_fails`` -- a
    heading's row count against its table.
  * ``test_a_tool_package_without_a_lab_section_fails`` -- tool-package
    discovery is metadata-driven: a current tool package with no Lab section
    fails, and its ``[[bin]]``s are still reconciled.
  * ``test_a_duplicated_lab_section_fails`` -- a tool package with two Lab
    sections is ambiguous and fails.
  * ``test_an_integration_row_without_a_live_target_fails`` -- a selected
    current-source row whose live test target left the manifests fails.
  * ``test_a_deleted_current_integration_row_fails`` -- the integration
    population is reconciled in both directions against the live targets the
    current-source rule selects; a deleted row whose live target remains
    fails.
  * ``test_an_unknown_integration_row_fails`` -- an integration row outside
    the current-source rule is not part of this catalog surface and fails.
  * ``test_a_metadata_failure_refuses_to_certify`` /
    ``test_malformed_metadata_json_refuses_to_certify`` /
    ``test_a_package_list_that_is_not_a_list_refuses_to_certify`` /
    ``test_a_package_missing_manifest_path_refuses_to_certify`` /
    ``test_a_package_manifest_path_of_the_wrong_type_refuses_to_certify`` /
    ``test_a_target_missing_name_refuses_to_certify`` /
    ``test_a_missing_catalog_section_refuses_to_certify`` /
    ``test_an_unreadable_workspace_refuses_to_certify`` -- the fail-closed
    exits: malformed metadata of every shape leaves exit 2 with a concise
    diagnostic and no traceback.
  * ``test_a_fenced_fake_table_is_not_cataloged`` /
    ``test_a_table_under_a_non_lab_subsection_is_not_cataloged`` -- the
    parser ignores fenced code blocks and tables under non-Lab ``###``
    subsections; neither becomes a catalog row or a finding.
  * ``test_the_gate_is_registered_where_it_runs`` -- the wiring: the justfile
    recipe, the ``lint-rust-check`` dependency, the ``lint-python``/``ty``
    coverage, and the pre-commit entry.
"""

from __future__ import annotations

import json
import subprocess  # ruff: ignore[suspicious-subprocess-import] -- the seam under test is a subprocess CLI
import sys
from pathlib import Path

_REPO_ROOT = Path(__file__).resolve().parents[1]
_CHECK = _REPO_ROOT / "scripts" / "tool_catalog_check.py"
_PREK = _REPO_ROOT / "prek.toml"
_JUSTFILE = _REPO_ROOT / "justfile"


README_TEMPLATE = """# Tools

## Public examples ({examples_declared} rows)

Consumer-facing starting points.

| Target | Source | Purpose | Consumer gate | Inputs | Behavior |
| --- | --- | --- | --- | --- | --- |
| `demo_consumer` | [`degenbot` examples](../crates/facade/degenbot/examples/demo_consumer.rs) | proof | `just build-demo` | none | stdout |

## Lab binaries ({lab_declared} rows)

Non-publishable tool packages.

### `degenbot-fixture-tools` (2 rows) — `fixture-tools/src/bin/`

Record / check workflows.

| Binary | Purpose | Current command | Lane | Inputs | Writes | Status |
| --- | --- | --- | --- | --- | --- | --- |
| [`alpha_gen`](fixture-tools/src/bin/alpha_gen/main.rs) | generate | `just check-alpha` | default | none | stdout | gate |
| [`beta_recorder`](fixture-tools/src/bin/beta_recorder/main.rs) | record | `just check-alpha` | {beta_lane} | none | stdout | gate |

## Owning-crate integration tests ({integration_declared} rows)

Boot-level smoke assertions owned by their product crates.

| Test | Source | Purpose | Command | Status |
| --- | --- | --- | --- | --- |
| `headless_boot` | [`degenbot-ingestion` tests](../crates/foundation/degenbot-ingestion/tests/headless_boot.rs) | boot | `just test-rust-nextest` | gate |
| `config_smoke_boot` | [`degenbot-bot` tests](../crates/engine/degenbot-bot/tests/config_smoke_boot.rs) | boot | `just test-rust-nextest` | gate |
"""


JUSTFILE = """# Recipes

py_run := "uv run --no-sync"

build-demo:
    echo demo

check-alpha:
    echo alpha

test-rust-nextest:
    echo nextest
"""


def _write(path: Path, text: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    return path


def _target(
    name: str, kind: str, src_path: Path, required_features: list[str] | None = None
) -> dict[str, object]:
    return {
        "name": name,
        "kind": [kind],
        "src_path": str(src_path),
        "required-features": required_features or [],
    }


def _package(name: str, manifest: Path, targets: list[dict[str, object]]) -> dict[str, object]:
    return {
        "name": name,
        "version": "0.1.0",
        "manifest_path": str(manifest),
        "targets": targets,
        "features": {},
    }


def _workspace(
    tmp_path: Path, *, beta_lane: str = "`sql-ledger`", beta_features: list[str] | None = None
) -> dict[str, Path]:
    """A throwaway workspace laid out like the real one.

    Returns the paths the tests need: the root, the README (also returned as
    ``readme_path``), and the metadata JSON dump path.
    """
    root = tmp_path
    rust = root / "rust"
    fixture_tools = rust / "tools" / "fixture-tools"
    alpha = _write(fixture_tools / "src" / "bin" / "alpha_gen" / "main.rs", "fn main() {}\n")
    beta = _write(fixture_tools / "src" / "bin" / "beta_recorder" / "main.rs", "fn main() {}\n")
    demo = _write(
        rust / "crates" / "facade" / "degenbot" / "examples" / "demo_consumer.rs",
        "fn main() {}\n",
    )
    headless = _write(
        rust / "crates" / "foundation" / "degenbot-ingestion" / "tests" / "headless_boot.rs",
        "#[test]\nfn boot() {}\n",
    )
    config_smoke = _write(
        rust / "crates" / "engine" / "degenbot-bot" / "tests" / "config_smoke_boot.rs",
        "#[test]\nfn boot() {}\n",
    )
    metadata = {
        "workspace_root": str(rust),
        "packages": [
            _package(
                "degenbot",
                rust / "crates" / "facade" / "degenbot" / "Cargo.toml",
                [_target("demo_consumer", "example", demo)],
            ),
            _package(
                "degenbot-fixture-tools",
                fixture_tools / "Cargo.toml",
                [
                    _target("degenbot_fixture_tools", "lib", fixture_tools / "src" / "lib.rs"),
                    _target("alpha_gen", "bin", alpha),
                    _target("beta_recorder", "bin", beta, beta_features or ["sql-ledger"]),
                ],
            ),
            _package(
                "degenbot-ingestion",
                rust / "crates" / "foundation" / "degenbot-ingestion" / "Cargo.toml",
                [_target("headless_boot", "test", headless)],
            ),
            _package(
                "degenbot-bot",
                rust / "crates" / "engine" / "degenbot-bot" / "Cargo.toml",
                [_target("config_smoke_boot", "test", config_smoke)],
            ),
        ],
    }
    metadata_path = _write(root / "metadata.json", json.dumps(metadata))
    readme_path = _write(
        rust / "tools" / "README.md",
        README_TEMPLATE.format(
            examples_declared=1, lab_declared=2, integration_declared=2, beta_lane=beta_lane
        ),
    )
    _write(root / "justfile", JUSTFILE)
    return {"root": root, "readme": readme_path, "metadata": metadata_path}


def _run(workspace: dict[str, Path]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true] -- a fixed literal argv built here
        [
            sys.executable,
            str(_CHECK),
            "--root",
            str(workspace["root"]),
            "--metadata",
            str(workspace["metadata"]),
        ],
        cwd=workspace["root"],
        check=False,
        capture_output=True,
        text=True,
    )


def _rewritten_readme(workspace: dict[str, Path], old: str, new: str) -> None:
    readme: Path = workspace["readme"]
    text = readme.read_text(encoding="utf-8")
    assert old in text, f"the mutation anchor is gone from the README: {old!r}"
    readme.write_text(text.replace(old, new, 1), encoding="utf-8")


# --- the committed workspace -------------------------------------------------


def test_the_committed_catalog_reconciles_with_the_live_workspace() -> None:
    result = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true] -- the real gate over the real tree
        [sys.executable, str(_CHECK)],
        cwd=_REPO_ROOT,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stdout + result.stderr
    assert "reconcile against cargo metadata and the justfile" in result.stdout


# --- coverage, both directions ----------------------------------------------


def test_an_added_target_without_a_row_fails(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    added = _write(
        workspace["root"]
        / "rust"
        / "tools"
        / "fixture-tools"
        / "src"
        / "bin"
        / "gamma_probe"
        / "main.rs",
        "fn main() {}\n",
    )
    metadata = json.loads(workspace["metadata"].read_text(encoding="utf-8"))
    for pkg in metadata["packages"]:
        if pkg["name"] == "degenbot-fixture-tools":
            pkg["targets"].append(_target("gamma_probe", "bin", added))
    workspace["metadata"].write_text(json.dumps(metadata), encoding="utf-8")

    result = _run(workspace)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "gamma_probe" in result.stderr
    assert "has no catalog row" in result.stderr


def test_a_row_whose_target_left_the_manifests_fails(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    metadata = json.loads(workspace["metadata"].read_text(encoding="utf-8"))
    for pkg in metadata["packages"]:
        if pkg["name"] == "degenbot-fixture-tools":
            pkg["targets"] = [
                target for target in pkg["targets"] if target["name"] != "beta_recorder"
            ]
    workspace["metadata"].write_text(json.dumps(metadata), encoding="utf-8")

    result = _run(workspace)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "beta_recorder" in result.stderr
    assert "no live [[bin]]" in result.stderr


def test_a_duplicated_row_fails(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    duplicated = (
        "| [`alpha_gen`](fixture-tools/src/bin/alpha_gen/main.rs) | generate | "
        "`just check-alpha` | default | none | stdout | gate |\n"
    )
    _rewritten_readme(workspace, duplicated, duplicated + duplicated)
    _rewritten_readme(
        workspace, "### `degenbot-fixture-tools` (2 rows)", "### `degenbot-fixture-tools` (3 rows)"
    )

    result = _run(workspace)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "already cataloged" in result.stderr


# --- structure ---------------------------------------------------------------


def test_a_malformed_row_fails(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    _rewritten_readme(
        workspace,
        "| [`alpha_gen`](fixture-tools/src/bin/alpha_gen/main.rs) | generate | `just check-alpha` | default | none | stdout | gate |",
        "| [`alpha_gen`](fixture-tools/src/bin/alpha_gen/main.rs) | generate | `just check-alpha` | default | stdout | gate |",
    )

    result = _run(workspace)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "the header declares" in result.stderr


def test_a_missing_source_link_fails(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    _rewritten_readme(
        workspace, "[`alpha_gen`](fixture-tools/src/bin/alpha_gen/main.rs)", "`alpha_gen`"
    )

    result = _run(workspace)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "carries no source link" in result.stderr


def test_source_drift_fails(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    decoy = _write(
        workspace["root"]
        / "rust"
        / "tools"
        / "fixture-tools"
        / "src"
        / "bin"
        / "alpha_gen"
        / "moved.rs",
        "fn main() {}\n",
    )
    assert decoy.is_file()
    _rewritten_readme(
        workspace,
        "fixture-tools/src/bin/alpha_gen/main.rs",
        "fixture-tools/src/bin/alpha_gen/moved.rs",
    )

    result = _run(workspace)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "no live [[bin]]" in result.stderr


def test_package_drift_fails(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    foreign = _write(
        workspace["root"]
        / "rust"
        / "tools"
        / "solver-lab"
        / "src"
        / "bin"
        / "lab_tool"
        / "main.rs",
        "fn main() {}\n",
    )
    metadata = json.loads(workspace["metadata"].read_text(encoding="utf-8"))
    metadata["packages"].append(
        _package(
            "degenbot-solver-lab",
            workspace["root"] / "rust" / "tools" / "solver-lab" / "Cargo.toml",
            [_target("lab_tool", "bin", foreign)],
        )
    )
    workspace["metadata"].write_text(json.dumps(metadata), encoding="utf-8")
    _rewritten_readme(
        workspace,
        "fixture-tools/src/bin/beta_recorder/main.rs",
        "solver-lab/src/bin/lab_tool/main.rs",
    )

    result = _run(workspace)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "no live [[bin]] of degenbot-fixture-tools" in result.stderr
    assert "it is a [[bin]] of degenbot-solver-lab" in result.stderr


def test_lane_drift_fails(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path, beta_lane="`default`")

    result = _run(workspace)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "does not match" in result.stderr
    assert "'sql-ledger'" in result.stderr


def test_a_secondary_build_shape_lane_passes(tmp_path: Path) -> None:
    workspace = _workspace(
        tmp_path,
        beta_lane="`sql-ledger` (+ `telemetry` second build shape: another lane)",
    )

    result = _run(workspace)

    assert result.returncode == 0, result.stdout + result.stderr


def test_an_unknown_recipe_fails(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    _rewritten_readme(workspace, "`just check-alpha` | default", "`just missing-recipe` | default")

    result = _run(workspace)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "missing-recipe" in result.stderr
    assert "does not define" in result.stderr


def test_just_directives_are_not_recipes(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    directives = (
        'set shell := ["bash", "-cu"]\n'
        "alias d := build-demo\n"
        'export FLAG := "1"\n'
        "unexport FLAG\n"
        'witness := "a:b"\n'
    )
    justfile: Path = workspace["root"] / "justfile"
    justfile.write_text(directives + justfile.read_text(encoding="utf-8"), encoding="utf-8")
    _rewritten_readme(
        workspace,
        "`just check-alpha` | default | none | stdout | gate |",
        "`just set`, `just alias`, `just export`, `just witness`, `just check-alpha`"
        " | default | none | stdout | gate |",
    )

    result = _run(workspace)

    assert result.returncode == 1, result.stdout + result.stderr
    findings = [line for line in result.stderr.splitlines() if "does not define" in line]
    assert len(findings) == 4, result.stderr
    for pseudo in ("set", "alias", "export", "witness"):
        assert any(
            f"`just {pseudo}`, which the justfile does not define" in line for line in findings
        )
    assert all("check-alpha" not in line for line in findings)


def test_a_prose_just_reference_is_not_a_recipe_reference(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    _rewritten_readme(workspace, "| proof |", "| run just any word to build |")

    result = _run(workspace)

    assert result.returncode == 0, result.stdout + result.stderr


def test_a_nonlocal_source_link_fails(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    _rewritten_readme(
        workspace,
        "(../crates/facade/degenbot/examples/demo_consumer.rs)",
        "(https://example.com/demo_consumer.rs)",
    )

    result = _run(workspace)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "not a repository-relative path" in result.stderr


def test_a_root_escaping_source_link_fails(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    _rewritten_readme(
        workspace,
        "(../crates/facade/degenbot/examples/demo_consumer.rs)",
        "(../../../outside/demo_consumer.rs)",
    )

    result = _run(workspace)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "escapes the repository root" in result.stderr


def test_a_declared_count_that_disagrees_with_the_table_fails(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    _rewritten_readme(
        workspace, "### `degenbot-fixture-tools` (2 rows)", "### `degenbot-fixture-tools` (3 rows)"
    )

    result = _run(workspace)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "declares 3 rows, its table holds 2" in result.stderr


# --- the integration rows ----------------------------------------------------


def test_an_integration_row_without_a_live_target_fails(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    metadata = json.loads(workspace["metadata"].read_text(encoding="utf-8"))
    for pkg in metadata["packages"]:
        if pkg["name"] == "degenbot-ingestion":
            pkg["targets"] = []
    workspace["metadata"].write_text(json.dumps(metadata), encoding="utf-8")

    result = _run(workspace)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "no live integration-test target" in result.stderr


def test_a_tool_package_without_a_lab_section_fails(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    scribe = _write(
        workspace["root"]
        / "rust"
        / "tools"
        / "scribe-tools"
        / "src"
        / "bin"
        / "scribe"
        / "main.rs",
        "fn main() {}\n",
    )
    metadata = json.loads(workspace["metadata"].read_text(encoding="utf-8"))
    metadata["packages"].append(
        _package(
            "degenbot-scribe-tools",
            workspace["root"] / "rust" / "tools" / "scribe-tools" / "Cargo.toml",
            [_target("scribe", "bin", scribe)],
        )
    )
    workspace["metadata"].write_text(json.dumps(metadata), encoding="utf-8")

    result = _run(workspace)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "degenbot-scribe-tools" in result.stderr
    assert "no 'Lab binaries' section" in result.stderr
    assert "scribe" in result.stderr
    assert "has no catalog row" in result.stderr


def test_a_duplicated_lab_section_fails(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    text = workspace["readme"].read_text(encoding="utf-8")
    start = text.index("### `degenbot-fixture-tools` (2 rows)")
    end = text.index("## Owning-crate integration tests")
    workspace["readme"].write_text(text[:end] + text[start:end] + text[end:], encoding="utf-8")

    result = _run(workspace)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "appears twice" in result.stderr


def test_a_deleted_current_integration_row_fails(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    _rewritten_readme(
        workspace,
        "| `config_smoke_boot` | [`degenbot-bot` tests]"
        "(../crates/engine/degenbot-bot/tests/config_smoke_boot.rs)"
        " | boot | `just test-rust-nextest` | gate |\n",
        "",
    )
    _rewritten_readme(
        workspace,
        "## Owning-crate integration tests (2 rows)",
        "## Owning-crate integration tests (1 row)",
    )

    result = _run(workspace)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "config_smoke_boot" in result.stderr
    assert "has no catalog row" in result.stderr


def test_an_unknown_integration_row_fails(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    other = _write(
        workspace["root"]
        / "rust"
        / "crates"
        / "foundation"
        / "degenbot-ingestion"
        / "tests"
        / "other_boot.rs",
        "#[test]\nfn boot() {}\n",
    )
    _rewritten_readme(
        workspace,
        "## Owning-crate integration tests (2 rows)",
        "## Owning-crate integration tests (3 rows)",
    )
    _rewritten_readme(
        workspace,
        "| `headless_boot` | [`degenbot-ingestion` tests]"
        "(../crates/foundation/degenbot-ingestion/tests/headless_boot.rs)"
        " | boot | `just test-rust-nextest` | gate |",
        "| `boot_smoke` | [other](../crates/foundation/degenbot-ingestion/tests/other_boot.rs)"
        " | boot | `just test-rust-nextest` | gate |\n"
        "| `headless_boot` | [`degenbot-ingestion` tests]"
        "(../crates/foundation/degenbot-ingestion/tests/headless_boot.rs)"
        " | boot | `just test-rust-nextest` | gate |",
    )
    assert other.is_file()

    result = _run(workspace)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "boot_smoke" in result.stderr
    assert "not part of this catalog surface" in result.stderr


# --- fail-closed exits -------------------------------------------------------


def test_a_metadata_failure_refuses_to_certify(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    workspace["metadata"].unlink()

    result = _run(workspace)

    assert result.returncode == 2, result.stdout + result.stderr
    assert "refusing to certify" in result.stderr


def test_malformed_metadata_json_refuses_to_certify(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    workspace["metadata"].write_text('{"packages": [', encoding="utf-8")

    result = _run(workspace)

    assert result.returncode == 2, result.stdout + result.stderr
    assert "unparseable" in result.stderr
    assert "Traceback" not in result.stderr


def test_a_package_list_that_is_not_a_list_refuses_to_certify(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    metadata = json.loads(workspace["metadata"].read_text(encoding="utf-8"))
    metadata["packages"] = "degenbot-fixture-tools"
    workspace["metadata"].write_text(json.dumps(metadata), encoding="utf-8")

    result = _run(workspace)

    assert result.returncode == 2, result.stdout + result.stderr
    assert "no package list" in result.stderr
    assert "Traceback" not in result.stderr


def test_a_package_missing_manifest_path_refuses_to_certify(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    metadata = json.loads(workspace["metadata"].read_text(encoding="utf-8"))
    for pkg in metadata["packages"]:
        if pkg["name"] == "degenbot-fixture-tools":
            del pkg["manifest_path"]
    workspace["metadata"].write_text(json.dumps(metadata), encoding="utf-8")

    result = _run(workspace)

    assert result.returncode == 2, result.stdout + result.stderr
    assert "manifest_path" in result.stderr
    assert "degenbot-fixture-tools" in result.stderr
    assert "Traceback" not in result.stderr


def test_a_package_manifest_path_of_the_wrong_type_refuses_to_certify(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    metadata = json.loads(workspace["metadata"].read_text(encoding="utf-8"))
    metadata["packages"][0]["manifest_path"] = 5
    workspace["metadata"].write_text(json.dumps(metadata), encoding="utf-8")

    result = _run(workspace)

    assert result.returncode == 2, result.stdout + result.stderr
    assert "manifest_path" in result.stderr
    assert "Traceback" not in result.stderr


def test_a_target_missing_name_refuses_to_certify(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    metadata = json.loads(workspace["metadata"].read_text(encoding="utf-8"))
    for pkg in metadata["packages"]:
        if pkg["name"] == "degenbot-fixture-tools":
            for target in pkg["targets"]:
                if target["name"] == "beta_recorder":
                    del target["name"]
    workspace["metadata"].write_text(json.dumps(metadata), encoding="utf-8")

    result = _run(workspace)

    assert result.returncode == 2, result.stdout + result.stderr
    assert "'name'" in result.stderr
    assert "Traceback" not in result.stderr


def test_a_garbage_metadata_payload_refuses_to_certify(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    workspace["metadata"].write_text("[]", encoding="utf-8")

    result = _run(workspace)

    assert result.returncode == 2, result.stdout + result.stderr
    assert "refusing to certify" in result.stderr


def test_a_missing_catalog_section_refuses_to_certify(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    text = workspace["readme"].read_text(encoding="utf-8")
    head, _, _ = text.partition("## Lab binaries")
    tail_start = text.index("## Owning-crate integration tests")
    workspace["readme"].write_text(head + text[tail_start:], encoding="utf-8")

    result = _run(workspace)

    assert result.returncode == 2, result.stdout + result.stderr
    assert "no 'Lab binaries' section" in result.stderr


def test_an_unreadable_workspace_refuses_to_certify(tmp_path: Path) -> None:
    result = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true] -- a fixed literal argv built here
        [sys.executable, str(_CHECK), "--root", str(tmp_path / "absent")],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 2, result.stdout + result.stderr
    assert "refusing to certify" in result.stderr


# --- the markdown the parser must ignore -------------------------------------


def test_a_fenced_fake_table_is_not_cataloged(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    fence = (
        "```\n"
        "## Lab binaries (1 row)\n"
        "\n"
        "| Binary | Purpose | Current command | Lane | Inputs | Writes | Status |\n"
        "| --- | --- | --- | --- | --- | --- | --- |\n"
        "| [`fake`](fixture-tools/src/bin/fake/main.rs) | fake | `just missing-recipe` "
        "| default | none | stdout | gate |\n"
        "```\n"
    )
    _rewritten_readme(workspace, "## Lab binaries (2 rows)", fence + "## Lab binaries (2 rows)")

    result = _run(workspace)

    assert result.returncode == 0, result.stdout + result.stderr


def test_a_table_under_a_non_lab_subsection_is_not_cataloged(tmp_path: Path) -> None:
    workspace = _workspace(tmp_path)
    subsection = (
        "### Retired rows\n"
        "\n"
        "| Target | Source | Purpose | Consumer gate | Inputs | Behavior |\n"
        "| --- | --- | --- | --- | --- | --- |\n"
        "| `ghost_target` | [gone](../crates/facade/degenbot/examples/ghost.rs) "
        "| prose just any word | `just missing-recipe` | none | stdout |\n"
        "\n"
    )
    _rewritten_readme(
        workspace, "## Lab binaries (2 rows)", subsection + "## Lab binaries (2 rows)"
    )

    result = _run(workspace)

    assert result.returncode == 0, result.stdout + result.stderr


# --- the wiring --------------------------------------------------------------


def test_the_gate_is_registered_where_it_runs() -> None:
    justfile = _JUSTFILE.read_text(encoding="utf-8")
    prek = _PREK.read_text(encoding="utf-8")

    assert "check-tool-catalog:" in justfile
    assert "python3 scripts/tool_catalog_check.py" in justfile
    assert (
        "lint-rust-check: check-toolchain check-publish-blocked "
        "check-vendor-tracked check-tool-catalog" in justfile
    )
    assert "scripts/tool_catalog_check.py" in justfile  # lint-python / ty coverage
    assert "tests/test_tool_catalog_check.py" in justfile
    assert 'id = "tool-catalog"' in prek
    assert 'entry = "just check-tool-catalog"' in prek
    tool_catalog_line = next(line for line in prek.splitlines() if 'id = "tool-catalog"' in line)
    # every rust manifest triggers (manifest-only example drift), and the
    # rust/examples example crates trigger on manifests and sources alike
    assert "rust/.*Cargo\\.toml$" in tool_catalog_line
    assert "^rust/examples/" in tool_catalog_line


def test_an_untracked_target_is_outside_the_catalog_surface(tmp_path: Path) -> None:
    """A peer lane's new, uncommitted binary cannot fail this run.

    The catalog reconciles against the index, so a target whose source is on disk
    but not in the commit under test is outside the surface this run certifies.
    Staging it brings it back in, and then it needs its row.
    """
    workspace = _workspace(tmp_path)
    root: Path = workspace["root"]
    beta_row = (
        "| [`beta_recorder`](fixture-tools/src/bin/beta_recorder/main.rs) | record "
        "| `just check-alpha` | `sql-ledger` | none | stdout | gate |\n"
    )
    _rewritten_readme(workspace, "## Lab binaries (2 rows)", "## Lab binaries (1 rows)")
    _rewritten_readme(
        workspace, "`degenbot-fixture-tools` (2 rows)", "`degenbot-fixture-tools` (1 rows)"
    )
    _rewritten_readme(workspace, beta_row, "")

    beta = "rust/tools/fixture-tools/src/bin/beta_recorder/main.rs"
    subprocess.run(["git", "init", "-q", str(root)], check=True)
    subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)
    subprocess.run(["git", "-C", str(root), "rm", "--cached", "-q", beta], check=True)

    untracked = _run(workspace)
    assert untracked.returncode == 0, untracked.stdout + untracked.stderr

    subprocess.run(["git", "-C", str(root), "add", beta], check=True)
    staged = _run(workspace)
    assert staged.returncode != 0
    assert "has no catalog row" in staged.stdout + staged.stderr
