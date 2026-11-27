"""The gate registry (``scripts/gate_registry.py``, ADR-102).

`scripts/gates.toml` is the one declaration of every hook's facts, and
`prek.toml`'s hook lines are rendered from it. These cases prove the rendering
contract rather than a copy of it: the committed pair agrees, a declaration the
tree cannot satisfy is rejected before anything renders, and prose stays with
its gate when the registry reorders the hooks.

  * ``test_the_committed_registry_renders_the_committed_hook_lines`` -- the
    drift assertion the `gates-check` hook runs, in process.
  * ``test_every_hook_line_stays_on_one_line`` -- `ci-parity` extracts entries
    with a line-anchored sed, so a wrapped hook line would silently leave the
    push-gate manifest.
  * ``test_a_hook_missing_from_the_registry_fails`` -- the direction that
    catches a hook added by hand and never declared.
  * ``test_a_new_declaration_renders_without_prose`` -- the other direction: a
    registry edit lands before its rationale does.
  * ``test_regen_retires_the_line_and_prose_of_a_gate_the_registry_dropped``
    and ``test_check_refuses_where_regen_retires`` -- a retirement is a registry
    deletion, so the drift gate refuses the orphaned line while `gates-regen`
    removes it and the prose it owned; the two commands, run end to end.
  * ``test_the_stage_anchor_and_intro_survive_retiring_its_first_hook`` -- the
    stage separator and its intro belong to the stage, not to whichever hook
    happens to follow them.
  * ``test_prose_left_behind_by_a_deleted_hook_line_fails_closed`` and
    ``test_a_stage_anchor_without_a_gate_fails_closed`` -- prose no hook owns is
    a rendering ambiguity, never something a regeneration deletes silently.

Every hook also declares the facts its identity needs beyond the wiring: its
``scope``, its ``data`` files, the ``docs/gates.md`` heading that documents
it, its probe tests, and exactly one CI coverage form. The coverage cases below
are the four forms, each with a negative and a positive control:

  * ``direct`` -- the CI job runs the hook's entry command verbatim;
  * ``contains`` -- the job runs an entry whose `just` invocation graph
    (``just --dump`` dependencies and body calls) reaches the hook's recipe,
    arguments included;
  * ``equivalent`` -- the job's parallel implementation is pinned by a named
    probe test;
  * ``ci_probe`` -- a committed-tree pytest node is the CI execution.

``test_every_ci_probe_is_in_the_default_suite`` closes the last hole: a
committed-tree probe that the default pytest selection excludes certifies
nothing, so the registry's ``ci_probe` names must lack an excluded marker.
"""

from __future__ import annotations

import ast
import pathlib
import re
import tomllib

import pytest

from tests.repo_script import load_script

_ROOT = pathlib.Path(__file__).resolve().parents[1]


# `load_script` registers the module before executing it: a dataclass resolves
# its own namespace through `sys.modules`, so an unregistered spec load runs on
# 3.15 and raises on the CI floor.
gate_registry = load_script("gate_registry", _ROOT / "scripts" / "gate_registry.py")


def _q(value: str) -> str:
    return '"' + value + '"'


def _array(values: list[str]) -> str:
    return "[" + ", ".join(_q(value) for value in values) + "]"


def _render_value(value: object) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, list):
        return _array(value)
    return _q(str(value))


def _gate(
    gate_id: str,
    *,
    stage: str = "pre-commit",
    entry: str | None = None,
    scope: str = "index",
    docs: str = "The gate registry",
    data: list[str] | None = None,
    probe: list[str] | None = None,
    coverage: dict[str, str] | None = None,
    **extra: object,
) -> str:
    fields: list[tuple[str, str]] = [
        ("id", _q(gate_id)),
        ("name", _q(gate_id.title())),
        ("stage", _q(stage)),
        ("entry", _q(entry or f"just {gate_id}")),
        ("scope", _q(scope)),
        ("docs", _q(docs)),
        ("probe", _array(probe or ["tests/probe.py::test_probe"])),
        ("data", _array(data or [])),
    ]
    fields.extend(
        (key, _render_value(value))
        for key, value in (
            coverage or {"ci_job": "probe-job", "ci_probe": "tests/probe.py::test_probe"}
        ).items()
    )
    fields.extend((key, _render_value(value)) for key, value in extra.items())
    return "[[gate]]\n" + "\n".join(f"{key} = {value}" for key, value in fields)


def _registry(*blocks: str) -> str:
    return "version = 1\n\n" + "\n\n".join(blocks) + "\n"


_CI_YAML = """\
name: CI

jobs:
  alpha-job:
    runs-on: ubuntu-latest
    steps:
      - run: just alpha
      - run: just beta
      - run: just lint-all
      - run: just checked

  probe-job:
    runs-on: ubuntu-latest
    steps:
      - run: just test-probes
"""

_DOCS = """\
# Gate ratchets

### The gate registry
"""

_PROBE = """\
def test_probe() -> None:
    assert True
"""


def _workspace_files(root: pathlib.Path, justfile: str) -> None:
    """The files the registry validates against, beyond the hook lines."""
    (root / "justfile").write_text(justfile, encoding="utf-8")
    (root / "docs").mkdir(exist_ok=True)
    (root / "docs" / "gates.md").write_text(_DOCS, encoding="utf-8")
    (root / "tests").mkdir(exist_ok=True)
    (root / "tests" / "probe.py").write_text(_PROBE, encoding="utf-8")
    (root / ".github" / "workflows").mkdir(parents=True, exist_ok=True)
    (root / ".github" / "workflows" / "ci.yml").write_text(_CI_YAML, encoding="utf-8")


def _hook(gate_id: str, stage: str = "pre-commit") -> str:
    """One hook line in the shape the renderer emits."""
    return (
        f'  {{ id = "{gate_id}", name = "{gate_id.title()}", language = "system", '
        f'entry = "just {gate_id}", stages = ["{stage}"] }},'
    )


_PRE_COMMIT = "pre-commit"
_PRE_PUSH = "pre-push"
_STAGES = {"alpha": _PRE_COMMIT, "beta": _PRE_COMMIT, "gamma": _PRE_PUSH, "delta": _PRE_PUSH}


def _declare(root: pathlib.Path, *gate_ids: str) -> None:
    """Write a registry declaring exactly `gate_ids`, in stage order."""
    ordered = [gate_id for gate_id in _STAGES if gate_id in gate_ids]
    (root / "scripts" / "gates.toml").write_text(
        _registry(*(_gate(gate_id, stage=_STAGES[gate_id]) for gate_id in ordered)),
        encoding="utf-8",
    )


@pytest.fixture
def staged_workspace(tmp_path: pathlib.Path) -> pathlib.Path:
    """A throwaway tree whose hooks sit under stage anchors, two per stage."""
    _workspace_files(
        tmp_path, "alpha:\n    true\nbeta:\n    true\ngamma:\n    true\ndelta:\n    true\n"
    )
    hooks = "\n".join([
        "  # --- pre-commit ---",
        "  # pre-commit stage intro",
        "",
        "  # alpha rationale",
        _hook("alpha"),
        "",
        "  # beta rationale",
        _hook("beta"),
        "",
        "  # --- pre-push ---",
        "  # pre-push stage intro",
        _hook("gamma", _PRE_PUSH),
        "",
        "  # delta rationale",
        _hook("delta", _PRE_PUSH),
    ])
    (tmp_path / "prek.toml").write_text(f"hooks = [\n{hooks}\n]\n", encoding="utf-8")
    (tmp_path / "scripts").mkdir()
    _declare(tmp_path, "alpha", "beta", "gamma", "delta")
    return tmp_path


@pytest.fixture
def workspace(tmp_path: pathlib.Path) -> pathlib.Path:
    """A throwaway tree holding every file the registry validates against."""
    _workspace_files(
        tmp_path,
        "alpha:\n    true\nbeta:\n    true\n"
        "lint-all:\n    just alpha\n"
        "checked:\n    just alpha --check\n"
        "test-probes:\n    true\n",
    )
    (tmp_path / "prek.toml").write_text(
        "hooks = [\n"
        "  # alpha rationale\n"
        '  { id = "alpha", name = "Alpha", language = "system", entry = "just alpha", '
        'stages = ["pre-commit"] },\n'
        "\n"
        "  # beta rationale\n"
        '  { id = "beta", name = "Beta", language = "system", entry = "just beta", '
        'stages = ["pre-push"] },\n'
        "]\n",
        encoding="utf-8",
    )
    (tmp_path / "scripts").mkdir()
    (tmp_path / "scripts" / "gates.toml").write_text(
        _registry(_gate("alpha"), _gate("beta", stage="pre-push")), encoding="utf-8"
    )
    return tmp_path


def test_the_committed_registry_renders_the_committed_hook_lines() -> None:
    gates = gate_registry.load(_ROOT)
    assert gates, "the registry declares no gates"
    expected = (_ROOT / "prek.toml").read_text(encoding="utf-8")
    assert gate_registry.render_prek(gates, _ROOT) == expected


def test_every_hook_line_stays_on_one_line() -> None:
    text = (_ROOT / "prek.toml").read_text(encoding="utf-8")
    lines = [line for line in text.split("\n") if line.strip().startswith("{ id = ")]
    assert len(lines) == len(gate_registry.load(_ROOT))
    for line in lines:
        assert re.match(r'^  \{ id = "[^"]+".*\},$', line), line


def test_the_markdown_hook_lints_with_the_justfile_pin() -> None:
    """A gate that runs a floating CLI is a rule set nobody reviewed.

    The hook, the two `just lint-markdown*` recipes, and CI's markdown job all
    lint one CLI: the justfile's `markdownlint_cli` literal. A hook naming the
    package without that version could lint a different rule set than the job
    that blocks the merge. This is the registry's `ci_equivalent_probe` for the
    hook: the CI job runs the check-only recipe, and every surface reaches the
    pin through that one literal instead of restating a version.
    """
    justfile = (_ROOT / "justfile").read_text(encoding="utf-8")
    pin = re.search(r'^markdownlint_cli := "([^"]+)"$', justfile, re.MULTILINE)
    assert pin is not None, "the justfile must declare the markdownlint CLI pin"

    gates = {gate.id: gate for gate in gate_registry.load(_ROOT)}

    assert gates["markdownlint"].entry == f"npx --yes {pin.group(1)}"
    assert gates["markdownlint"].coverage.entry == "just lint-markdown-check"

    ci = (_ROOT / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")
    assert "run: just lint-markdown-check" in ci, (
        "the markdown job must run the check-only recipe, or the hook's rule set "
        "is not the one that blocks the merge"
    )

    spliced = [
        line.strip()
        for line in justfile.splitlines()
        if line.strip().startswith("npx --yes {{ markdownlint_cli }}")
    ]
    assert len(spliced) == 2, spliced


def test_a_hook_missing_from_the_registry_fails(workspace: pathlib.Path) -> None:
    (workspace / "scripts" / "gates.toml").write_text(_registry(_gate("alpha")), encoding="utf-8")
    gates = gate_registry.load(workspace)
    with pytest.raises(gate_registry.RegistryError, match=r"absent from scripts/gates\.toml"):
        gate_registry.render_prek(gates, workspace)


def test_a_new_declaration_renders_without_prose(workspace: pathlib.Path) -> None:
    (workspace / "scripts" / "hooks").mkdir(parents=True)
    (workspace / "scripts" / "hooks" / "alpha2.sh").write_text("#!/bin/sh\n", encoding="utf-8")
    (workspace / "scripts" / "gates.toml").write_text(
        _registry(
            _gate("alpha"),
            _gate("alpha2", entry="scripts/hooks/alpha2.sh"),
            _gate("beta", stage="pre-push"),
        ),
        encoding="utf-8",
    )
    rendered = gate_registry.render_prek(gate_registry.load(workspace), workspace)
    assert 'id = "alpha2"' in rendered
    assert rendered.count("rationale") == 2


def test_duplicate_ids_are_rejected(workspace: pathlib.Path) -> None:
    (workspace / "scripts" / "gates.toml").write_text(
        _registry(_gate("alpha"), _gate("alpha")), encoding="utf-8"
    )
    with pytest.raises(gate_registry.RegistryError, match="duplicate id"):
        gate_registry.load(workspace)


def test_an_unknown_stage_is_rejected(workspace: pathlib.Path) -> None:
    (workspace / "scripts" / "gates.toml").write_text(
        _registry(_gate("alpha"), _gate("beta", stage="post-merge")), encoding="utf-8"
    )
    with pytest.raises(gate_registry.RegistryError, match="not one of"):
        gate_registry.load(workspace)


def test_an_entry_naming_a_missing_recipe_is_rejected(workspace: pathlib.Path) -> None:
    (workspace / "scripts" / "gates.toml").write_text(
        _registry(_gate("alpha"), _gate("beta", stage="pre-push", entry="just gamma")),
        encoding="utf-8",
    )
    with pytest.raises(gate_registry.RegistryError, match="does not define"):
        gate_registry.load(workspace)


def test_an_entry_naming_a_private_recipe_validates(workspace: pathlib.Path) -> None:
    """Private recipes count: `just` resolves them, so a gate may name one."""
    (workspace / "justfile").write_text(
        "_internal:\n    true\nalpha:\n    true\nbeta:\n    true\n", encoding="utf-8"
    )
    (workspace / "scripts" / "gates.toml").write_text(
        _registry(
            _gate("alpha", entry="just _internal"),
            _gate("beta", stage="pre-push"),
        ),
        encoding="utf-8",
    )

    gates = gate_registry.load(workspace)

    assert {gate.id for gate in gates} == {"alpha", "beta"}


def test_an_entry_naming_a_variable_is_rejected(workspace: pathlib.Path) -> None:
    """`just py_run` can never validate as a gate entry: `py_run` is a variable."""
    (workspace / "justfile").write_text(
        'py_run := "uv run --no-sync"\nalpha:\n    true\nbeta:\n    true\n', encoding="utf-8"
    )
    (workspace / "scripts" / "gates.toml").write_text(
        _registry(
            _gate("alpha", entry="just py_run"),
            _gate("beta", stage="pre-push"),
        ),
        encoding="utf-8",
    )

    with pytest.raises(gate_registry.RegistryError, match="does not define"):
        gate_registry.load(workspace)


def test_an_entry_naming_a_directive_is_rejected(workspace: pathlib.Path) -> None:
    """`just set` can never validate as a gate entry: `set` is a directive."""
    (workspace / "justfile").write_text(
        'set shell := ["bash", "-cu"]\nalpha:\n    true\nbeta:\n    true\n', encoding="utf-8"
    )
    (workspace / "scripts" / "gates.toml").write_text(
        _registry(
            _gate("alpha", entry="just set"),
            _gate("beta", stage="pre-push"),
        ),
        encoding="utf-8",
    )

    with pytest.raises(gate_registry.RegistryError, match="does not define"):
        gate_registry.load(workspace)


def test_a_malformed_justfile_refuses_validation(workspace: pathlib.Path) -> None:
    """A justfile that will not parse is a refusal, not a missing-recipe finding."""
    (workspace / "justfile").write_text("this is : not valid\n:::\n", encoding="utf-8")
    (workspace / "scripts" / "gates.toml").write_text(
        _registry(_gate("alpha"), _gate("beta", stage="pre-push")), encoding="utf-8"
    )

    with pytest.raises(
        gate_registry.just_recipe_index.RecipeIndexRefusal, match="refusing to index"
    ):
        gate_registry.load(workspace)


def test_an_entry_naming_a_missing_script_is_rejected(workspace: pathlib.Path) -> None:
    (workspace / "scripts" / "gates.toml").write_text(
        _registry(_gate("alpha"), _gate("beta", stage="pre-push", entry="scripts/hooks/gone.sh")),
        encoding="utf-8",
    )
    with pytest.raises(gate_registry.RegistryError, match="does not exist"):
        gate_registry.load(workspace)


def test_gates_must_stay_grouped_by_stage(workspace: pathlib.Path) -> None:
    (workspace / "scripts" / "gates.toml").write_text(
        _registry(_gate("alpha", stage="pre-push"), _gate("beta")), encoding="utf-8"
    )
    with pytest.raises(gate_registry.RegistryError, match="grouped by stage"):
        gate_registry.load(workspace)


def test_prose_follows_its_gate_when_the_registry_reorders(workspace: pathlib.Path) -> None:
    (workspace / "prek.toml").write_text(
        (workspace / "prek.toml")
        .read_text(encoding="utf-8")
        .replace(
            'entry = "just beta", stages = ["pre-push"]',
            'entry = "just beta", stages = ["pre-commit"]',
        ),
        encoding="utf-8",
    )
    (workspace / "scripts" / "gates.toml").write_text(
        _registry(_gate("beta"), _gate("alpha")), encoding="utf-8"
    )
    rendered = gate_registry.render_prek(gate_registry.load(workspace), workspace)
    assert rendered.index("beta rationale") < rendered.index('id = "beta"')
    assert rendered.index("beta rationale") < rendered.index("# alpha rationale")


def test_regen_retires_the_line_and_prose_of_a_gate_the_registry_dropped(
    staged_workspace: pathlib.Path,
) -> None:
    """Retiring a gate deletes its declaration, and the two commands disagree on purpose."""
    _declare(staged_workspace, "alpha", "gamma", "delta")
    gates = gate_registry.load(staged_workspace)

    with pytest.raises(gate_registry.RegistryError, match=r"hook\(s\) absent"):
        gate_registry.render_prek(gates, staged_workspace)

    rendered = gate_registry.render_prek(
        gates, staged_workspace, undeclared=gate_registry.UndeclaredHook.REGEN
    )
    assert 'id = "beta"' not in rendered
    assert "beta rationale" not in rendered
    assert 'id = "alpha"' in rendered
    assert "pre-commit stage intro" in rendered


def test_check_refuses_where_regen_retires(staged_workspace: pathlib.Path) -> None:
    """The two commands the hook and the fix hint run, end to end."""
    _declare(staged_workspace, "alpha", "gamma", "delta")

    assert gate_registry.main(["check"], staged_workspace) == 1
    assert gate_registry.main(["regen"], staged_workspace) == 0
    rewritten = (staged_workspace / "prek.toml").read_text(encoding="utf-8")
    assert 'id = "beta"' not in rewritten
    assert "beta rationale" not in rewritten
    assert "pre-commit stage intro" in rewritten
    assert gate_registry.main(["check"], staged_workspace) == 0

    settled = (staged_workspace / "prek.toml").read_text(encoding="utf-8")
    assert gate_registry.main(["regen"], staged_workspace) == 0
    assert (staged_workspace / "prek.toml").read_text(encoding="utf-8") == settled


@pytest.mark.parametrize(
    ("retired", "anchor", "intro", "survivor"),
    [
        ("alpha", "# --- pre-commit ---", "pre-commit stage intro", "beta"),
        ("gamma", "# --- pre-push ---", "pre-push stage intro", "delta"),
    ],
)
def test_the_stage_anchor_and_intro_survive_retiring_its_first_hook(
    staged_workspace: pathlib.Path,
    retired: str,
    anchor: str,
    intro: str,
    survivor: str,
) -> None:
    declared = [gate_id for gate_id in _STAGES if gate_id != retired]
    _declare(staged_workspace, *declared)
    rendered = gate_registry.render_prek(
        gate_registry.load(staged_workspace),
        staged_workspace,
        undeclared=gate_registry.UndeclaredHook.REGEN,
    )

    assert anchor in rendered
    assert intro in rendered
    assert f"{retired} rationale" not in rendered
    assert rendered.index(intro) < rendered.index(f'id = "{survivor}"')


def test_prose_left_behind_by_a_deleted_hook_line_fails_closed(
    staged_workspace: pathlib.Path,
) -> None:
    (staged_workspace / "prek.toml").write_text(
        (staged_workspace / "prek.toml")
        .read_text(encoding="utf-8")
        .replace(_hook("delta", _PRE_PUSH), ""),
        encoding="utf-8",
    )
    gates = gate_registry.load(staged_workspace)

    with pytest.raises(gate_registry.RegistryError, match="explains no gate"):
        gate_registry.render_prek(
            gates, staged_workspace, undeclared=gate_registry.UndeclaredHook.REGEN
        )


def test_a_stage_anchor_without_a_gate_fails_closed(staged_workspace: pathlib.Path) -> None:
    _declare(staged_workspace, "alpha", "beta")
    gates = gate_registry.load(staged_workspace)

    with pytest.raises(gate_registry.RegistryError, match="stage anchor"):
        gate_registry.render_prek(
            gates, staged_workspace, undeclared=gate_registry.UndeclaredHook.REGEN
        )


def test_a_registry_without_a_version_is_rejected(workspace: pathlib.Path) -> None:
    (workspace / "scripts" / "gates.toml").write_text(_gate("alpha") + "\n", encoding="utf-8")
    with pytest.raises(gate_registry.RegistryError, match="version"):
        gate_registry.load(workspace)


def test_an_unknown_key_is_rejected(workspace: pathlib.Path) -> None:
    (workspace / "scripts" / "gates.toml").write_text(
        _registry(_gate("alpha", colour="red")), encoding="utf-8"
    )
    with pytest.raises(gate_registry.RegistryError, match="unknown key"):
        gate_registry.load(workspace)


# --- identity facts ---------------------------------------------------------
# A hook's identity is more than its wiring line: the population it judges, the
# data it reconciles, the docs that explain it, the probes that keep it honest.


def _load_with(workspace: pathlib.Path, *blocks: str) -> list:
    (workspace / "scripts" / "gates.toml").write_text(_registry(*blocks), encoding="utf-8")
    return gate_registry.load(workspace)


def test_a_tree_scope_that_takes_the_file_list_fails(workspace: pathlib.Path) -> None:
    """A tree sweep certifies the whole tree, so it cannot also be handed a slice."""
    with pytest.raises(gate_registry.RegistryError, match="pass_filenames"):
        _load_with(workspace, _gate("alpha", scope="tree"), _gate("beta", stage="pre-push"))


def test_an_index_scope_that_refuses_the_file_list_fails(workspace: pathlib.Path) -> None:
    with pytest.raises(gate_registry.RegistryError, match="pass_filenames"):
        _load_with(
            workspace,
            _gate("alpha", pass_filenames=False),
            _gate("beta", stage="pre-push"),
        )


def test_a_staged_scope_that_takes_the_file_list_fails(workspace: pathlib.Path) -> None:
    with pytest.raises(gate_registry.RegistryError, match="pass_filenames"):
        _load_with(workspace, _gate("alpha", scope="staged"), _gate("beta", stage="pre-push"))


def test_an_unknown_scope_is_rejected(workspace: pathlib.Path) -> None:
    with pytest.raises(gate_registry.RegistryError, match="not one of"):
        _load_with(workspace, _gate("alpha", scope="workspace"), _gate("beta"))


def test_a_data_path_that_does_not_exist_fails(workspace: pathlib.Path) -> None:
    with pytest.raises(gate_registry.RegistryError, match="data"):
        _load_with(workspace, _gate("alpha", data=["gone.txt"]), _gate("beta"))


def test_a_data_path_that_exists_validates(workspace: pathlib.Path) -> None:
    gates = _load_with(workspace, _gate("alpha", data=["justfile"]), _gate("beta"))

    assert gates[0].data == ("justfile",)


def test_a_docs_anchor_that_does_not_exist_fails(workspace: pathlib.Path) -> None:
    with pytest.raises(gate_registry.RegistryError, match="docs"):
        _load_with(workspace, _gate("alpha", docs="No such heading"), _gate("beta"))


def test_a_probe_that_names_no_test_fails(workspace: pathlib.Path) -> None:
    with pytest.raises(gate_registry.RegistryError, match="test"):
        _load_with(
            workspace,
            _gate("alpha", probe=["tests/probe.py::test_gone"]),
            _gate("beta"),
        )


def test_a_probe_file_that_does_not_exist_fails(workspace: pathlib.Path) -> None:
    with pytest.raises(gate_registry.RegistryError, match="test"):
        _load_with(
            workspace,
            _gate("alpha", probe=["tests/gone.py::test_probe"]),
            _gate("beta"),
        )


def test_a_ci_entry_that_is_only_mentioned_in_a_comment_fails(workspace: pathlib.Path) -> None:
    """A comment is prose: it never executes the gate the registry claims."""
    ci = workspace / ".github" / "workflows" / "ci.yml"
    ci.write_text(
        ci.read_text(encoding="utf-8").replace(
            "      - run: just lint-all", "      # run: just lint-all"
        ),
        encoding="utf-8",
    )
    with pytest.raises(gate_registry.RegistryError, match="never runs"):
        _load_with(
            workspace,
            _gate(
                "alpha",
                coverage={
                    "ci_job": "alpha-job",
                    "ci_entry": "just lint-all",
                    "ci_relation": "contains",
                },
                scope="tree",
                pass_filenames=False,
            ),
            _gate("beta"),
        )


def test_a_ci_entry_does_not_match_a_longer_recipe_name(workspace: pathlib.Path) -> None:
    """`just lint-all-extended` is not `just lint-all`."""
    ci = workspace / ".github" / "workflows" / "ci.yml"
    ci.write_text(
        ci.read_text(encoding="utf-8").replace(
            "      - run: just lint-all", "      - run: just lint-all-extended"
        ),
        encoding="utf-8",
    )
    with pytest.raises(gate_registry.RegistryError, match="never runs"):
        _load_with(
            workspace,
            _gate(
                "alpha",
                coverage={
                    "ci_job": "alpha-job",
                    "ci_entry": "just lint-all",
                    "ci_relation": "contains",
                },
                scope="tree",
                pass_filenames=False,
            ),
            _gate("beta"),
        )


# --- CI coverage forms ------------------------------------------------------
# Exactly one form per hook. Presence in ci.yml is never sufficient: `direct`
# is verbatim, `contains` is graph reachability, `equivalent` names its pinning
# probe, and `ci_probe` names the committed-tree node that runs in CI.


def test_a_gate_without_a_ci_form_fails(workspace: pathlib.Path) -> None:
    block = "\n".join(
        line for line in _gate("alpha").splitlines() if not line.startswith(("ci_job", "ci_probe"))
    )
    with pytest.raises(gate_registry.RegistryError, match="exactly one CI coverage form"):
        _load_with(workspace, block, _gate("beta"))


def test_a_gate_with_two_ci_forms_fails(workspace: pathlib.Path) -> None:
    block = _gate(
        "alpha",
        coverage={
            "ci_job": "probe-job",
            "ci_probe": "tests/probe.py::test_probe",
            "ci_entry": "just test-probes",
            "ci_relation": "contains",
        },
    )
    with pytest.raises(gate_registry.RegistryError, match="exactly one CI coverage form"):
        _load_with(workspace, block, _gate("beta"))


def test_a_ci_job_that_the_workflow_lacks_fails(workspace: pathlib.Path) -> None:
    with pytest.raises(gate_registry.RegistryError, match="is no job"):
        _load_with(
            workspace,
            _gate(
                "alpha",
                coverage={"ci_job": "ghost-job", "ci_probe": "tests/probe.py::test_probe"},
            ),
            _gate("beta"),
        )


def test_a_ci_entry_its_job_never_runs_fails(workspace: pathlib.Path) -> None:
    with pytest.raises(gate_registry.RegistryError, match="never runs"):
        _load_with(
            workspace,
            _gate(
                "alpha",
                coverage={
                    "ci_job": "probe-job",
                    "ci_entry": "just lint-all",
                    "ci_relation": "contains",
                },
            ),
            _gate("beta"),
        )


def test_direct_coverage_must_name_the_hook_entry_verbatim(workspace: pathlib.Path) -> None:
    with pytest.raises(gate_registry.RegistryError, match="direct"):
        _load_with(
            workspace,
            _gate(
                "alpha",
                coverage={
                    "ci_job": "alpha-job",
                    "ci_entry": "just lint-all",
                    "ci_relation": "direct",
                },
            ),
            _gate("beta"),
        )


def test_direct_coverage_accepts_the_entry_its_job_runs(workspace: pathlib.Path) -> None:
    gates = _load_with(
        workspace,
        _gate(
            "alpha",
            coverage={"ci_job": "alpha-job", "ci_entry": "just alpha", "ci_relation": "direct"},
            scope="tree",
            pass_filenames=False,
        ),
        _gate("beta"),
    )

    assert gates[0].coverage == gate_registry.Coverage(
        job="alpha-job", entry="just alpha", relation="direct"
    )


def test_contains_coverage_accepts_a_body_call_chain(workspace: pathlib.Path) -> None:
    gates = _load_with(
        workspace,
        _gate(
            "alpha",
            coverage={
                "ci_job": "alpha-job",
                "ci_entry": "just lint-all",
                "ci_relation": "contains",
            },
            scope="tree",
            pass_filenames=False,
        ),
        _gate("beta"),
    )

    assert gates[0].coverage.relation == "contains"


def test_contains_coverage_rejects_an_unreachable_recipe(workspace: pathlib.Path) -> None:
    with pytest.raises(gate_registry.RegistryError, match="does not reach"):
        _load_with(
            workspace,
            _gate(
                "alpha",
                coverage={
                    "ci_job": "alpha-job",
                    "ci_entry": "just beta",
                    "ci_relation": "contains",
                },
                scope="tree",
                pass_filenames=False,
            ),
            _gate("beta"),
        )


def test_contains_coverage_requires_the_entry_arguments(workspace: pathlib.Path) -> None:
    """`just lint-all` runs `just alpha`, not `just alpha --strict`."""
    with pytest.raises(gate_registry.RegistryError, match="does not reach"):
        _load_with(
            workspace,
            _gate(
                "alpha --strict",
                coverage={
                    "ci_job": "alpha-job",
                    "ci_entry": "just lint-all",
                    "ci_relation": "contains",
                },
                scope="tree",
                pass_filenames=False,
            ),
            _gate("beta"),
        )


def test_contains_coverage_matches_the_entry_arguments(workspace: pathlib.Path) -> None:
    gates = _load_with(
        workspace,
        _gate(
            "alpha --check",
            coverage={"ci_job": "alpha-job", "ci_entry": "just checked", "ci_relation": "contains"},
            scope="tree",
            pass_filenames=False,
        ),
        _gate("beta"),
    )

    assert gates[0].coverage.entry == "just checked"


def test_equivalent_coverage_without_a_named_probe_fails(workspace: pathlib.Path) -> None:
    with pytest.raises(gate_registry.RegistryError, match="ci_equivalent_probe"):
        _load_with(
            workspace,
            _gate(
                "alpha",
                coverage={
                    "ci_job": "probe-job",
                    "ci_entry": "just test-probes",
                    "ci_relation": "equivalent",
                },
                scope="tree",
                pass_filenames=False,
            ),
            _gate("beta"),
        )


def test_equivalent_coverage_names_a_real_test(workspace: pathlib.Path) -> None:
    gates = _load_with(
        workspace,
        _gate(
            "alpha",
            coverage={
                "ci_job": "probe-job",
                "ci_entry": "just test-probes",
                "ci_relation": "equivalent",
                "ci_equivalent_probe": "tests/probe.py::test_probe",
            },
            scope="tree",
            pass_filenames=False,
        ),
        _gate("beta"),
    )

    assert gates[0].coverage.equivalent_probe == "tests/probe.py::test_probe"


def test_an_equivalent_probe_naming_no_test_fails(workspace: pathlib.Path) -> None:
    with pytest.raises(gate_registry.RegistryError, match="test"):
        _load_with(
            workspace,
            _gate(
                "alpha",
                coverage={
                    "ci_job": "probe-job",
                    "ci_entry": "just test-probes",
                    "ci_relation": "equivalent",
                    "ci_equivalent_probe": "tests/probe.py::test_gone",
                },
                scope="tree",
                pass_filenames=False,
            ),
            _gate("beta"),
        )


def test_ci_probe_naming_no_test_function_fails(workspace: pathlib.Path) -> None:
    with pytest.raises(gate_registry.RegistryError, match="test"):
        _load_with(
            workspace,
            _gate(
                "alpha", coverage={"ci_job": "probe-job", "ci_probe": "tests/probe.py::test_gone"}
            ),
            _gate("beta"),
        )


def test_ci_probe_naming_a_missing_file_fails(workspace: pathlib.Path) -> None:
    with pytest.raises(gate_registry.RegistryError, match="test"):
        _load_with(
            workspace,
            _gate(
                "alpha", coverage={"ci_job": "probe-job", "ci_probe": "tests/gone.py::test_probe"}
            ),
            _gate("beta"),
        )


# --- the committed registry -------------------------------------------------


def test_the_committed_registry_declares_every_hook_with_one_ci_form() -> None:
    gates = gate_registry.load(_ROOT)
    hook_ids = [
        line.split('"')[1]
        for line in (_ROOT / "prek.toml").read_text(encoding="utf-8").splitlines()
        if line.strip().startswith("{ id = ")
    ]

    assert sorted(gate.id for gate in gates) == sorted(hook_ids)
    assert len(gates) == 21
    for gate in gates:
        assert gate.coverage is not None, gate.id
        assert gate.probe, f"{gate.id} declares no probe"
        assert gate.docs in {
            line.lstrip("# ").strip()
            for line in (_ROOT / "docs" / "gates.md").read_text(encoding="utf-8").splitlines()
            if line.startswith("#")
        }, gate.id


def test_every_ci_probe_is_in_the_default_suite() -> None:
    """A ci_probe the default selection excludes is not a CI execution at all.

    `gates-check` validates the node statically (path plus function presence);
    this is the pytest-side half: the file is under `testpaths` and neither it
    nor the test carries a marker the default `-m` filter deselects, so a
    normal CI run really executes it.
    """
    config = tomllib.loads((_ROOT / "pyproject.toml").read_text(encoding="utf-8"))["tool"][
        "pytest"
    ]["ini_options"]
    testpaths = config["testpaths"]
    excluded = set(re.findall(r"not ([a-z_]+)", " ".join(config["addopts"])))
    assert excluded, "the default selection must exclude something, or this probe is vacuous"

    probes = [gate for gate in gate_registry.load(_ROOT) if gate.coverage.probe]
    assert probes, "no gate declares a ci_probe"

    for gate in probes:
        path_text, _, function = gate.coverage.probe.partition("::")
        assert any(path_text.startswith(f"{root}/") for root in testpaths), (
            f"{gate.id}: {path_text} is outside the default testpaths {testpaths}"
        )
        markers = _markers(_ROOT / path_text, function)
        assert not markers & excluded, (
            f"{gate.id}: {gate.coverage.probe} carries {sorted(markers & excluded)}, "
            "which the default pytest selection deselects"
        )


def _markers(path: pathlib.Path, function: str) -> set[str]:
    """The pytest markers on a test function and its module."""
    tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    found: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Assign) and any(
            isinstance(target, ast.Name) and target.id == "pytestmark" for target in node.targets
        ):
            found |= _mark_names(node.value)
        if isinstance(node, ast.FunctionDef | ast.AsyncFunctionDef) and node.name == function:
            for decorator in node.decorator_list:
                found |= _mark_names(decorator)
    return found


def _mark_names(node: ast.AST) -> set[str]:
    """The marker names a `pytest.mark.<name>` expression (or list) carries."""
    names: set[str] = set()
    for child in ast.walk(node):
        if (
            isinstance(child, ast.Attribute)
            and isinstance(child.value, ast.Attribute)
            and isinstance(child.value.value, ast.Name)
            and child.value.value.id == "pytest"
            and child.value.attr == "mark"
        ):
            names.add(child.attr)
    return names
