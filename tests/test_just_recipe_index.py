"""The justfile recipe index (``scripts/just_recipe_index.py``).

`just` is the authority on whether a name resolves to an invocable recipe, so
this seam asks it instead of parsing justfile text: ``just --show <name>``
answers existence (private and parameterized recipes included) while
``just --summary`` runs only to validate that the justfile parses. The cases
here pin the three answers and the exit codes the CLI adapter returns for each:

  * ``test_a_normal_recipe_exists`` / ``test_a_private_recipe_exists`` /
    ``test_a_parameterized_recipe_exists`` -- names `just` resolves.
  * ``test_a_variable_is_not_a_recipe`` / ``test_a_set_directive_is_not_a_recipe``
    / ``test_an_unknown_name_is_just_unknown`` -- names it does not; the caller
    keeps the missing-recipe finding.
  * ``test_a_malformed_justfile_refuses`` / ``test_a_missing_just_refuses`` --
    the refusal exits, distinct from a missing-recipe finding.
  * ``test_an_empty_justfile_yields_no_names`` / ``test_lookups_are_cached`` --
    the population signal the catalog's empty-input refusal reads, and the
    per-process caching contract.
"""

from __future__ import annotations

import os
import pathlib
import shutil
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import sys

import pytest

from tests.repo_script import load_script

_ROOT = pathlib.Path(__file__).resolve().parents[1]
_MODULE = _ROOT / "scripts" / "just_recipe_index.py"

just_recipe_index = load_script("just_recipe_index", _MODULE)

JUSTFILE = """\
# fixture
py_run := "uv run --no-sync"
set shell := ["bash", "-cu"]

build:
    echo build

_hidden:
    echo hidden

_param amount:
    echo {{amount}}
"""


@pytest.fixture
def justfile(tmp_path: pathlib.Path) -> pathlib.Path:
    path = tmp_path / "justfile"
    path.write_text(JUSTFILE, encoding="utf-8")
    return path


def _index(path: pathlib.Path):
    return just_recipe_index.RecipeIndex(path)


def _run_cli(path: pathlib.Path, name: str, **overrides: str) -> subprocess.CompletedProcess[str]:
    env = dict(os.environ)
    env.update(overrides)
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [sys.executable, str(_MODULE), "--justfile", str(path), name],
        check=False,
        capture_output=True,
        text=True,
        env=env,
    )


def test_a_normal_recipe_exists(justfile: pathlib.Path) -> None:
    assert _index(justfile).exists("build") is True

    result = _run_cli(justfile, "build")

    assert result.returncode == 0, result.stdout + result.stderr


def test_a_private_recipe_exists(justfile: pathlib.Path) -> None:
    assert _index(justfile).exists("_hidden") is True

    result = _run_cli(justfile, "_hidden")

    assert result.returncode == 0, result.stdout + result.stderr
    # A private recipe stays invocable, not merely indexed.
    just = shutil.which("just")
    assert just is not None, "just is not installed on PATH"
    ran = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [just, "--justfile", str(justfile), "_hidden"],
        check=False,
        capture_output=True,
        text=True,
    )
    assert ran.returncode == 0, ran.stdout + ran.stderr
    assert "hidden" in ran.stdout


def test_a_parameterized_recipe_exists(justfile: pathlib.Path) -> None:
    assert _index(justfile).exists("_param") is True

    result = _run_cli(justfile, "_param")

    assert result.returncode == 0, result.stdout + result.stderr


def test_a_variable_is_not_a_recipe(justfile: pathlib.Path) -> None:
    assert _index(justfile).exists("py_run") is False

    result = _run_cli(justfile, "py_run")

    assert result.returncode == 1, result.stdout + result.stderr


def test_a_set_directive_is_not_a_recipe(justfile: pathlib.Path) -> None:
    assert _index(justfile).exists("set") is False

    result = _run_cli(justfile, "set")

    assert result.returncode == 1, result.stdout + result.stderr


def test_an_unknown_name_is_just_unknown(justfile: pathlib.Path) -> None:
    assert _index(justfile).exists("nope") is False

    result = _run_cli(justfile, "nope")

    assert result.returncode == 1, result.stdout + result.stderr
    # The missing-recipe finding is the caller's to report; the adapter is silent.
    assert not result.stdout
    assert not result.stderr


def test_a_malformed_justfile_refuses(tmp_path: pathlib.Path) -> None:
    bad = tmp_path / "justfile"
    bad.write_text("this is : not valid\n:::\n", encoding="utf-8")

    with pytest.raises(just_recipe_index.RecipeIndexRefusal, match="refusing to index"):
        _index(bad).exists("build")

    result = _run_cli(bad, "build")

    assert result.returncode == 2, result.stdout + result.stderr
    assert "refusing to index" in result.stderr


def test_a_missing_just_refuses(tmp_path: pathlib.Path, justfile: pathlib.Path) -> None:
    with pytest.raises(just_recipe_index.RecipeIndexRefusal, match="refusing to answer"):
        just_recipe_index.RecipeIndex(justfile, just=str(tmp_path / "no-such-just")).exists("build")

    empty_bin = tmp_path / "empty-bin"
    empty_bin.mkdir()
    result = _run_cli(justfile, "build", PATH=str(empty_bin))

    assert result.returncode == 2, result.stdout + result.stderr
    assert "refusing" in result.stderr


def test_an_empty_justfile_yields_no_names(tmp_path: pathlib.Path) -> None:
    empty = tmp_path / "justfile"
    empty.write_text("", encoding="utf-8")

    index = _index(empty)

    assert index.names() == ()
    # It parses, so a name is unknown, not a refusal.
    assert index.exists("build") is False


GRAPH_JUSTFILE = """\
check-alpha:
    echo alpha

check-beta mode="":
    echo beta {{mode}}

check-gamma:
    echo gamma

check-delta:
    echo delta

lint-all: check-alpha
    just check-beta --strict

with-dependency-argument: (check-beta "loud")
    echo dependency argument

chain-a: chain-b
    echo a

chain-b:
    just chain-c

chain-c:
    echo c

echoes:
    echo "run just check-beta"
    # just check-gamma
    just "check-delta"

cycle-a:
    just cycle-b

cycle-b:
    just cycle-a
"""


@pytest.fixture
def graph(tmp_path: pathlib.Path):
    path = tmp_path / "justfile"
    path.write_text(GRAPH_JUSTFILE, encoding="utf-8")
    return _index(path)


def test_a_body_call_reaches_its_recipe(graph) -> None:
    assert graph.reaches("lint-all", "check-beta") is True


def test_a_body_call_carries_its_arguments(graph) -> None:
    assert graph.reaches("lint-all", "check-beta", arguments="--strict") is True


def test_a_body_call_with_other_arguments_does_not_satisfy_the_entry(graph) -> None:
    """`just check-beta --loose` is not `just check-beta --strict`."""
    assert graph.reaches("lint-all", "check-beta", arguments="--loose") is False


def test_a_declared_dependency_reaches_its_recipe(graph) -> None:
    assert graph.reaches("lint-all", "check-alpha") is True


def test_a_dependency_argument_is_rendered(graph) -> None:
    """`(check-beta "loud")` is an invocation of check-beta with `loud`."""
    assert graph.reaches("with-dependency-argument", "check-beta", arguments="loud") is True


def test_a_body_is_rendered_with_its_variable_splice(graph) -> None:
    assert "echo beta {{mode}}" in graph.body("check-beta")


def test_a_multi_step_chain_reaches_its_transitive_body_call(graph) -> None:
    assert graph.reaches("chain-a", "chain-c") is True


def test_a_prose_mention_is_not_an_invocation(graph) -> None:
    """`echo "run just check-beta"` certifies nothing: only a command is a call."""
    assert graph.reaches("echoes", "check-beta") is False


def test_a_comment_is_not_an_invocation(graph) -> None:
    assert graph.reaches("echoes", "check-gamma") is False


def test_a_quoted_recipe_name_is_an_invocation(graph) -> None:
    assert graph.reaches("echoes", "check-delta") is True


def test_an_unreached_recipe_is_not_reachable(graph) -> None:
    assert graph.reaches("check-alpha", "check-delta") is False


def test_an_unknown_target_is_not_reachable(graph) -> None:
    assert graph.reaches("lint-all", "no-such-recipe") is False


def test_a_cycle_terminates(graph) -> None:
    assert graph.reaches("cycle-a", "cycle-b") is True
    assert graph.reaches("cycle-a", "check-alpha") is False


def test_a_malformed_justfile_refuses_the_graph(tmp_path: pathlib.Path) -> None:
    bad = tmp_path / "justfile"
    bad.write_text("this is : not valid\n:::\n", encoding="utf-8")

    with pytest.raises(just_recipe_index.RecipeIndexRefusal, match="refusing"):
        _index(bad).reaches("build", "other")


def test_a_missing_just_refuses_the_graph(tmp_path: pathlib.Path, justfile: pathlib.Path) -> None:
    index = just_recipe_index.RecipeIndex(justfile, just=str(tmp_path / "no-such-just"))

    with pytest.raises(just_recipe_index.RecipeIndexRefusal, match="refusing"):
        index.reaches("build", "other")


def test_lookups_are_cached(justfile: pathlib.Path, monkeypatch: pytest.MonkeyPatch) -> None:
    calls: list[object] = []
    real_run = just_recipe_index.subprocess.run

    def spy(command: object, **kwargs: object) -> object:
        calls.append(command)
        return real_run(command, **kwargs)

    monkeypatch.setattr(just_recipe_index.subprocess, "run", spy)
    index = _index(justfile)

    assert index.exists("build") is True
    assert index.exists("build") is True
    assert index.exists("nope") is False
    assert index.exists("nope") is False

    # One parse validation plus one lookup per unique name: the justfile is
    # parsed once per process and each name is asked once.
    assert len(calls) == 3
