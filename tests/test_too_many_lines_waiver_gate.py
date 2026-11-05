"""The `too_many_lines` waiver ratchet (``scripts/hooks/too_many_lines_waivers.py``).

A census ratchet whose matcher has gone blind reports the same green as a swept
tree, so this file is the gate's own evidence: the GLOSSARY's negative-probe idiom
applied to the waiver census. Every case drives the real gate with ``--root``
pointed at a throwaway tree, so no assertion here can be satisfied by a re-typed
copy of the gate's logic.

  * ``test_a_fresh_waiver_fails_and_names_the_row`` -- the red path: a waiver with
    no census row fails and the message names the row to add.
  * ``test_a_listed_row_grandfathers_the_waiver`` -- the census is what keeps a
    listed site from blocking.
  * ``test_a_stale_row_fails_the_gate`` -- the other direction: a row whose waiver
    was swept has to go in the same change.
  * ``test_a_stated_reason_the_census_omits_fails`` and
    ``test_a_census_reason_the_source_dropped_fails`` -- the reason mirror, both
    ways, so a justification cannot drift out of step with its code.
  * ``test_a_reasonless_waiver_above_the_ceiling_fails`` -- a new waiver with no
    reason cannot land even when its row is added.
  * ``test_a_census_without_its_ceiling_fails_closed``,
    ``test_a_missing_census_fails_closed`` and
    ``test_a_tree_without_rust_source_fails_closed`` -- the fail-closed edges.
  * ``test_an_inner_attribute_is_a_module_waiver``,
    ``test_a_waiver_stacked_above_another_attribute_names_its_function`` and
    ``test_a_multiline_attribute_records_its_reason`` -- the three attribute shapes
    the extractor has to read, each pinned on its own.
  * ``test_the_committed_census_matches_the_tree`` -- the real gate over the real
    tree.
"""

from __future__ import annotations

import subprocess  # ruff: ignore[suspicious-subprocess-import]
import sys
from pathlib import Path

_REPO_ROOT = Path(__file__).resolve().parents[1]
_GATE = _REPO_ROOT / "scripts" / "hooks" / "too_many_lines_waivers.py"
_CENSUS_PARTS = ("scripts", "hooks", "too-many-lines-waiver-census.txt")

_WAIVER = "#[expect(clippy::too_many_lines)]\nfn long_one() {\n    let _ = 1;\n}\n"


def _tree(tmp_path: Path, rust_source: str) -> Path:
    """A throwaway tree holding `rust_source` as one Rust module."""
    tree = tmp_path / "tree"
    target = tree / "rust" / "src" / "lib.rs"
    target.parent.mkdir(parents=True)
    target.write_text(rust_source, encoding="utf-8")
    return tree


def _census(tree: Path, rows: list[str], ceiling: int) -> None:
    """Write the census `tree` implies, with `ceiling` as its reasonless freeze."""
    path = tree.joinpath(*_CENSUS_PARTS)
    path.parent.mkdir(parents=True, exist_ok=True)
    body = "".join(f"{row}\n" for row in rows)
    path.write_text(f"# reasonless-high-water: {ceiling}\n{body}", encoding="utf-8")


def _run(tree: Path) -> subprocess.CompletedProcess[str]:
    """Run the real gate over `tree`."""
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [sys.executable, str(_GATE), "--root", str(tree)],
        check=False,
        capture_output=True,
        text=True,
    )


# --- the negative probe: the red path, demonstrated --------------------------


def test_a_fresh_waiver_fails_and_names_the_row(tmp_path: Path) -> None:
    """RED PATH: a waiver with no row fails, proposing the row it needs."""
    tree = _tree(tmp_path, _WAIVER)
    _census(tree, [], ceiling=0)

    result = _run(tree)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "fresh: rust/src/lib.rs:1: rust/src/lib.rs::long_one" in result.stderr, result.stderr


def test_a_listed_row_grandfathers_the_waiver(tmp_path: Path) -> None:
    """A listed row is a green: the census keeps the site from blocking."""
    tree = _tree(tmp_path, _WAIVER)
    _census(tree, ["rust/src/lib.rs::long_one"], ceiling=1)

    result = _run(tree)

    assert result.returncode == 0, result.stdout + result.stderr
    assert "clean (1 waiver sites" in result.stdout, result.stdout


def test_a_stale_row_fails_the_gate(tmp_path: Path) -> None:
    """A row whose waiver was swept fails, so the census can only shrink."""
    tree = _tree(tmp_path, _WAIVER)
    _census(tree, ["rust/src/lib.rs::long_one", "rust/src/lib.rs::swept_away"], ceiling=1)

    result = _run(tree)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "stale: rust/src/lib.rs::swept_away -> delete this row" in result.stderr, result.stderr


# --- the reason mirror, both directions --------------------------------------


def test_a_stated_reason_the_census_omits_fails(tmp_path: Path) -> None:
    """A waiver that states its reason must have that reason recorded."""
    tree = _tree(
        tmp_path,
        "#[expect(clippy::too_many_lines)] // two flows read best together\nfn long_one() {}\n",
    )
    _census(tree, ["rust/src/lib.rs::long_one"], ceiling=0)

    result = _run(tree)

    assert result.returncode == 1, result.stdout + result.stderr
    assert (
        'reason: rust/src/lib.rs:1: the source states "two flows read best together"'
        in result.stderr
    ), result.stderr


def test_a_census_reason_the_source_dropped_fails(tmp_path: Path) -> None:
    """A row cannot keep quoting a reason the source no longer states."""
    tree = _tree(tmp_path, _WAIVER)
    _census(tree, ["rust/src/lib.rs::long_one\ttwo flows read best together"], ceiling=1)

    result = _run(tree)

    assert result.returncode == 1, result.stdout + result.stderr
    assert (
        'the row records "two flows read best together" but the source states none' in result.stderr
    ), result.stderr


def test_a_ceiling_ignores_sites_that_state_a_reason(tmp_path: Path) -> None:
    """The freeze counts unexplained waivers only."""
    tree = _tree(
        tmp_path, "#[expect(clippy::too_many_lines)] // one arm per family\nfn long_one() {}\n"
    )
    _census(tree, ["rust/src/lib.rs::long_one\tone arm per family"], ceiling=0)

    result = _run(tree)

    assert result.returncode == 0, result.stdout + result.stderr


# --- the ceiling: a new reasonless waiver cannot land ------------------------


def test_a_reasonless_waiver_above_the_ceiling_fails(tmp_path: Path) -> None:
    """A second unexplained waiver fails even though both rows are listed."""
    two = _WAIVER + "\n#[expect(clippy::too_many_lines)]\nfn long_two() {}\n"
    tree = _tree(tmp_path, two)
    _census(tree, ["rust/src/lib.rs::long_one", "rust/src/lib.rs::long_two"], ceiling=1)

    result = _run(tree)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "2 waiver sites state no reason, the census ceiling is 1" in result.stderr, result.stderr


# --- fail-closed edges -------------------------------------------------------


def test_a_missing_census_fails_closed(tmp_path: Path) -> None:
    """An absent census refuses to run instead of passing over nothing."""
    tree = _tree(tmp_path, _WAIVER)

    result = _run(tree)

    assert result.returncode == 2, result.stdout + result.stderr
    assert "census missing" in result.stderr, result.stderr


def test_a_census_without_its_ceiling_fails_closed(tmp_path: Path) -> None:
    """A census with rows but no freeze number certifies nothing."""
    tree = _tree(tmp_path, _WAIVER)
    path = tree.joinpath(*_CENSUS_PARTS)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("rust/src/lib.rs::long_one\n", encoding="utf-8")

    result = _run(tree)

    assert result.returncode == 2, result.stdout + result.stderr
    assert "carries no `# reasonless-high-water: <n>` header" in result.stderr, result.stderr


def test_a_tree_without_rust_source_fails_closed(tmp_path: Path) -> None:
    """A scan that examined no Rust file refuses to report a green."""
    tree = tmp_path / "empty"
    tree.mkdir()
    _census(tree, [], ceiling=0)

    result = _run(tree)

    assert result.returncode == 2, result.stdout + result.stderr
    assert "examined nothing" in result.stderr, result.stderr


# --- the three attribute shapes ----------------------------------------------


def test_an_inner_attribute_is_a_module_waiver(tmp_path: Path) -> None:
    """A file-level `#![expect(...)]` is one site, scoped to the module."""
    tree = _tree(tmp_path, "#![expect(clippy::too_many_lines)]\n\nfn one() {}\n")
    _census(tree, ["rust/src/lib.rs::!module"], ceiling=1)

    result = _run(tree)

    assert result.returncode == 0, result.stdout + result.stderr


def test_a_waiver_stacked_above_another_attribute_names_its_function(tmp_path: Path) -> None:
    """The item is the declaration after every attribute, not the next attribute."""
    tree = _tree(
        tmp_path, "#[expect(clippy::too_many_lines)]\n#[must_use]\nfn stacked() -> u8 {\n    1\n}\n"
    )
    _census(tree, ["rust/src/lib.rs::stacked"], ceiling=1)

    result = _run(tree)

    assert result.returncode == 0, result.stdout + result.stderr


def test_a_multiline_attribute_records_its_reason(tmp_path: Path) -> None:
    """A `reason = "..."` argument spanning lines is the row's reason text."""
    source = (
        "#[expect(\n"
        "    clippy::unwrap_used,\n"
        "    clippy::too_many_lines,\n"
        '    reason = "the stage sweep reads top-down, one arm per stage"\n'
        ")]\n"
        "fn multiline() {}\n"
    )
    tree = _tree(tmp_path, source)
    _census(
        tree,
        ["rust/src/lib.rs::multiline\tthe stage sweep reads top-down, one arm per stage"],
        ceiling=0,
    )

    result = _run(tree)

    assert result.returncode == 0, result.stdout + result.stderr


# --- the committed census ----------------------------------------------------


def test_the_committed_census_matches_the_tree() -> None:
    """The real gate over the real tree: waiver sites and rows are in step."""
    result = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [sys.executable, str(_GATE)],
        cwd=_REPO_ROOT,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert "reasonless at the ceiling" in result.stdout, result.stdout
