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
"""

from __future__ import annotations

import pathlib
import re

import pytest

from tests.repo_script import load_script

_ROOT = pathlib.Path(__file__).resolve().parents[1]


# `load_script` registers the module before executing it: a dataclass resolves
# its own namespace through `sys.modules`, so an unregistered spec load runs on
# 3.15 and raises on the CI floor.
gate_registry = load_script("gate_registry", _ROOT / "scripts" / "gate_registry.py")


def _q(value: str) -> str:
    return '"' + value + '"'


def _gate(
    gate_id: str, *, stage: str = "pre-commit", entry: str | None = None, **extra: str
) -> str:
    fields = [
        ("id", _q(gate_id)),
        ("name", _q(gate_id.title())),
        ("stage", _q(stage)),
        ("entry", _q(entry or f"just {gate_id}")),
        *((key, _q(value)) for key, value in extra.items()),
    ]
    return "[[gate]]\n" + "\n".join(f"{key} = {value}" for key, value in fields)


def _registry(*blocks: str) -> str:
    return "version = 1\n\n" + "\n\n".join(blocks) + "\n"


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
    (tmp_path / "justfile").write_text(
        "alpha:\n    true\nbeta:\n    true\ngamma:\n    true\ndelta:\n    true\n",
        encoding="utf-8",
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
    """A throwaway tree holding the three files the renderer reads."""
    (tmp_path / "justfile").write_text("alpha:\n    true\nbeta:\n    true\n", encoding="utf-8")
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
