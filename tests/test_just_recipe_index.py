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
