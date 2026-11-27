"""The environment surface, read from Python source as a syntax tree.

Two gate questions share one implementation in ``scripts/env_surface.py``: which
environment names the Python companion reads, and which ``DEGENBOT_*`` names
exist anywhere in the Rust tree. The Rust test tier reaches the same module
through thin adapters (``degenbot-config``'s ``no_stray_env_reads`` and
``schema_completeness``), so this file is the pytest half of one scan, not a
second scanner.

The read scan is a syntax tree, not a token walk, for the two shapes a lexer
cannot see: a module or name imported under an alias, and a source that does not
parse at all. A lexer answers both with silence -- an unterminated literal leaves
it stuck in string mode and every later read is prose -- so the silence of a
lexer's green run is a claim it cannot make. Here a source that does not parse is
a refusal, and an alias resolves to the name it binds.
"""

from __future__ import annotations

import os
import subprocess  # ruff: ignore[suspicious-subprocess-import] -- the CLI seam is the adapter's contract
import sys
from pathlib import Path

import pytest

from tests.repo_script import load_script

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "env_surface.py"

env_surface = load_script("env_surface", SCRIPT)


def _names(source: str) -> list[str]:
    """The environment names one source reads, in report order."""
    return [read.name for read in env_surface.scan_source(source, "probe.py")]


def _lines(*lines: str) -> str:
    return "".join(f"{line}\n" for line in lines)


# --- every spelling a reader can write, and the prose it must not mistake ----

_SHAPES: tuple[tuple[str, str, list[str]], ...] = (
    (
        "the mapping call",
        _lines('raw = os.environ.get("DEGENBOT_GATE_CANARY")'),
        ["DEGENBOT_GATE_CANARY"],
    ),
    (
        "the getenv function",
        _lines('raw = os.getenv("DEGENBOT_GATE_CANARY")'),
        ["DEGENBOT_GATE_CANARY"],
    ),
    (
        "the subscript",
        _lines('raw = os.environ["DEGENBOT_GATE_CANARY"]'),
        ["DEGENBOT_GATE_CANARY"],
    ),
    (
        "the membership test",
        _lines('if "DEGENBOT_GATE_CANARY" in os.environ:', "    pass"),
        ["DEGENBOT_GATE_CANARY"],
    ),
    (
        "the bytes subscript",
        _lines('raw = os.environb[b"DEGENBOT_GATE_CANARY"]'),
        ["DEGENBOT_GATE_CANARY"],
    ),
    (
        "the bytes membership test",
        _lines('if "DEGENBOT_GATE_CANARY" in os.environb:', "    pass"),
        ["DEGENBOT_GATE_CANARY"],
    ),
    (
        "a loop over a single-name call",
        _lines('for b in os.environ.get("DEGENBOT_GATE_CANARY", "").split(","):', "    pass"),
        ["DEGENBOT_GATE_CANARY"],
    ),
    (
        "a computed call name",
        _lines("raw = os.environ.get(name)"),
        ["name"],
    ),
    (
        "a retired-knob presence sweep",
        _lines(
            "for name in _RETIRED:",
            "    if name in os.environ:",
            "        raise ValueError",
        ),
        ["name"],
    ),
    (
        "the whole mapping",
        _lines("env = os.environ"),
        ["<computed>"],
    ),
    (
        "an f-string name",
        _lines('raw = os.environ.get(f"DEGENBOT_GATE_{SUFFIX}")'),
        ["<computed>"],
    ),
    (
        "the mapping in an f-string hole",
        _lines('msg = f"{os.environ}"'),
        ["<computed>"],
    ),
    (
        "a line comment",
        _lines('# os.environ.get("DEGENBOT_GATE_CANARY")'),
        [],
    ),
    (
        "a trailing comment",
        _lines('raw = value  # not os.environ.get("DEGENBOT_GATE_CANARY")'),
        [],
    ),
    (
        "a docstring",
        _lines('"""The example\'s raw os.environ.get is retired."""'),
        [],
    ),
    (
        "a read interpolated into a multi-line f-string",
        _lines(
            'msg = f"""',
            '    {os.environ.get("DEGENBOT_GATE_CANARY")}',
            '"""',
        ),
        ["DEGENBOT_GATE_CANARY"],
    ),
    (
        "prose about a read, then a read",
        _lines(
            'note = """',
            "    it isn't a read, and it isn't os.environ either",
            '"""',
            'raw = os.environ.get("DEGENBOT_GATE_CANARY")',
        ),
        ["DEGENBOT_GATE_CANARY"],
    ),
    (
        "a doubled brace in an f-string",
        _lines('msg = f"{{ {SUFFIX} }}"'),
        [],
    ),
    (
        "a longer attribute that only starts like the mapping",
        _lines('raw = os.environx.get("DEGENBOT_GATE_CANARY")'),
        [],
    ),
)


@pytest.mark.parametrize(
    ("shape", "source", "expected"),
    _SHAPES,
    ids=[shape for shape, _, _ in _SHAPES],
)
def test_every_read_shape_is_classified(shape: str, source: str, expected: list[str]) -> None:
    assert _names(source) == expected, shape


# --- import aliases: the spelling a token walk cannot see --------------------

_ALIASES: tuple[tuple[str, str, list[str]], ...] = (
    (
        "the module under another name",
        _lines("import os as o", 'raw = o.environ.get("DEGENBOT_GATE_CANARY")'),
        ["DEGENBOT_GATE_CANARY"],
    ),
    (
        "the mapping imported by name",
        _lines("from os import environ", 'raw = environ["DEGENBOT_GATE_CANARY"]'),
        ["DEGENBOT_GATE_CANARY"],
    ),
    (
        "the accessor imported under another name",
        _lines("from os import getenv as g", 'raw = g("DEGENBOT_GATE_CANARY")'),
        ["DEGENBOT_GATE_CANARY"],
    ),
    (
        "the mapping imported under another name, tested for a name",
        _lines("from os import environ as ENV", 'if "DEGENBOT_GATE_CANARY" in ENV:', "    pass"),
        ["DEGENBOT_GATE_CANARY"],
    ),
    (
        "the bytes mapping imported by name",
        _lines("from os import environb as eb", 'raw = eb[b"DEGENBOT_GATE_CANARY"]'),
        ["DEGENBOT_GATE_CANARY"],
    ),
    (
        "a star import of the os surface",
        _lines("from os import *", 'raw = getenv("DEGENBOT_GATE_CANARY")'),
        ["DEGENBOT_GATE_CANARY"],
    ),
    (
        "a star import's mapping",
        _lines("from os import *", 'raw = environ["DEGENBOT_GATE_CANARY"]'),
        ["DEGENBOT_GATE_CANARY"],
    ),
    (
        "an alias that binds nothing of interest",
        _lines("import os.path as p", 'raw = p.join("a", "b")'),
        [],
    ),
)


@pytest.mark.parametrize(
    ("shape", "source", "expected"), _ALIASES, ids=[shape for shape, _, _ in _ALIASES]
)
def test_an_import_alias_resolves_to_the_name_it_binds(
    shape: str, source: str, expected: list[str]
) -> None:
    assert _names(source) == expected, shape


# --- refusals: a scan that cannot see must not report silence --------------

_BROKEN = _lines(
    'note = """',
    "    the literal never closes",
    'raw = os.environ.get("DEGENBOT_GATE_CANARY")',
)


def test_an_unparseable_source_is_refused_not_silenced() -> None:
    with pytest.raises(env_surface.NamedRefusal, match=r"probe\.py:1"):
        env_surface.scan_source(_BROKEN, "probe.py")


def test_an_unparseable_source_in_a_tree_is_refused(tmp_path: Path) -> None:
    broken = tmp_path / "src" / "degenbot" / "broken.py"
    broken.parent.mkdir(parents=True)
    broken.write_text(_BROKEN, encoding="utf-8")

    with pytest.raises(env_surface.NamedRefusal, match=r"broken\.py"):
        env_surface.scan_tree(["src/degenbot"], root=tmp_path)


def test_an_empty_surface_is_refused(tmp_path: Path) -> None:
    empty = tmp_path / "empty"
    empty.mkdir()

    with pytest.raises(env_surface.NamedRefusal, match="no Python source"):
        env_surface.scan_tree(["empty"], root=tmp_path)


def test_a_missing_surface_is_refused(tmp_path: Path) -> None:
    with pytest.raises(env_surface.NamedRefusal, match="no such file"):
        env_surface.scan_tree(["nowhere"], root=tmp_path)


def test_an_empty_inventory_sweep_is_refused(tmp_path: Path) -> None:
    (tmp_path / "rust" / "crates").mkdir(parents=True)

    with pytest.raises(env_surface.NamedRefusal, match="no DEGENBOT_"):
        env_surface.inventory_names(tmp_path)


# --- the companion: the live tree, and a python-only diff -------------------


def test_the_companion_reads_only_sanctioned_names() -> None:
    violations = env_surface.scan_tree(["src/degenbot"])

    assert violations == [], "unsanctioned companion env reads:\n" + "\n".join(
        str(violation) for violation in violations
    )


def test_a_python_only_diff_is_scanned(tmp_path: Path) -> None:
    """The default suite scans the companion, so a Python-only change is judged."""
    canary = tmp_path / "src" / "degenbot" / "runner" / "gate_canary.py"
    canary.parent.mkdir(parents=True)
    canary.write_text(
        _lines("import os", "", 'DEGENBOT_GATE_CANARY = os.environ.get("DEGENBOT_GATE_CANARY")'),
        encoding="utf-8",
    )

    violations = env_surface.scan_tree(["src/degenbot"], root=tmp_path)

    assert [str(violation) for violation in violations] == [
        "src/degenbot/runner/gate_canary.py:3: DEGENBOT_GATE_CANARY"
    ]


def test_every_allowlist_entry_is_a_live_companion_sanction() -> None:
    """The allowlist is the review surface: a stale row sanctions nothing."""
    for path, names in env_surface.PYTHON_ALLOWLIST.items():
        sanctioned = ROOT / path
        assert path.startswith("src/degenbot/"), path
        assert sanctioned.is_file(), f"{path} is allowlisted but does not exist"
        assert names, f"{path} is allowlisted with no name"
        raw = {read.name for read in env_surface.scan_source(sanctioned.read_text())}
        assert raw == set(names), (
            f"{path}: the allowlist and the source disagree -- "
            f"unsanctioned {sorted(raw - set(names))}, unused {sorted(set(names) - raw)}"
        )


# --- the Rust-tree inventory -------------------------------------------------


def test_the_inventory_sweep_reaches_the_schema_keys() -> None:
    names = env_surface.inventory_names()

    assert names == sorted(set(names)), "the sweep is one sorted, deduplicated list"
    assert "DEGENBOT_CONFIG" in names, "the bootstrap key is part of the inventory"


def test_the_inventory_sweep_excludes_the_config_crates_own_probes() -> None:
    """The config crate's tests carry negative-probe literals, not keys."""
    assert "DEGENBOT_GATE_CANARY" not in env_surface.inventory_names()


# --- the CLI the Rust tier invokes ------------------------------------------


def _run(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true] -- fixed argv, no shell
        [sys.executable, str(SCRIPT), *args],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
        env={**os.environ, "PYTHONIOENCODING": "utf-8"},
    )


def test_the_cli_reports_a_violation_with_its_path_and_name(tmp_path: Path) -> None:
    canary = tmp_path / "gate_canary.py"
    canary.write_text(_lines('raw = os.environ.get("DEGENBOT_GATE_CANARY")'), encoding="utf-8")

    result = _run("reads", "--root", str(canary.parent))

    assert result.returncode == 1, result.stdout + result.stderr
    assert "DEGENBOT_GATE_CANARY" in result.stdout


def test_the_cli_refuses_an_empty_surface(tmp_path: Path) -> None:
    result = _run("reads", "--root", str(tmp_path))

    assert result.returncode == 2, result.stdout + result.stderr
    assert result.stderr.startswith("env_surface: ")


def test_the_cli_prints_the_inventory() -> None:
    result = _run("inventory")

    assert result.returncode == 0, result.stdout + result.stderr
    assert "DEGENBOT_CONFIG" in result.stdout.splitlines()
