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
