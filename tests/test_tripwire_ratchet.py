"""The retirement-tripwire ratchet (`scripts/hooks/tripwire_ratchet.py`).

# live-invariant gate (ADR-087 D2): watches the present test/gate surface (rust test
# files, tests/, justfile, prek.toml, scripts/, docs/) for the retirement-tripwire
# signatures ADR-087 D1 bans; not tripwire precedent.

ADR-087 D4's ratchet is a source scan, so its own failure branch is the thing that
needs evidence: a scanner that has gone blind reports the same green as a clean tree.
These tests are that evidence, and they are the GLOSSARY's negative-probe idiom
applied to the gate itself --

  * `test_a_synthetic_tripwire_fails_the_gate` builds a throwaway tree holding exactly
    one tombstone assertion (`assert not hasattr(bot, "removed_thing")  # removed X
    must not exist`), runs the real script, and REQUIRES the nonzero exit and the
    reported `file:line`.
  * `test_the_same_fixture_passes_once_it_carries_the_d2_marker` runs the same tree
    with the D2 marker added and requires the green: the exemption is what separates
    the two species, and it is proven to work rather than assumed.
  * `test_every_signature_fires_on_its_own_red_source` drives each rule in the
    scanner's table with a minimal red source, so a rule that stops matching cannot
    hide behind the others.

The committed tree is asserted separately (`test_the_committed_tree_is_clean`); a
finding there is either a leftover tripwire to delete or a false positive to tighten,
and the failure message says so.
"""

from __future__ import annotations

import shutil
import subprocess  # ruff: ignore[suspicious-subprocess-import]
from typing import TYPE_CHECKING

import pytest

from scripts.hooks import tripwire_ratchet as ratchet

if TYPE_CHECKING:
    from pathlib import Path

_GIT = shutil.which("git")
_PYTHON = shutil.which("python3")
assert _GIT is not None, "git is not installed on PATH"
assert _PYTHON is not None, "python3 is not installed on PATH"

# The ADR-087 D2 exemption marker, spelled once here so the fixture cannot drift
# from the scanner's own constant.
_MARKER = ratchet.MARKER

# A tree with one tombstone assertion in it, and nothing else. `_scratch` keeps the
# fixture legible: a real tripwire names a retired symbol and says so in its message.
_TOMBSTONE = (
    "def test_removed_thing_is_gone(bot):\n"
    '    assert not hasattr(bot, "removed_thing")  # removed X must not exist\n'
)


def _git(repo: Path, *args: str) -> None:
    subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [_GIT, "-C", str(repo), *args],
        check=True,
        capture_output=True,
        text=True,
    )


def _fixture_tree(tmp_path: Path, body: str) -> Path:
    """A throwaway git repo whose `tests/` holds `body` as one test module."""
    repo = tmp_path / "tree"
    tests = repo / "tests"
    tests.mkdir(parents=True)
    _git(repo, "init", "--quiet")
    (tests / "test_fixture.py").write_text(body, encoding="utf-8")
    return repo


def _run(repo: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [_PYTHON, str(ratchet.__file__), "--root", str(repo)],
        check=False,
        capture_output=True,
        text=True,
    )


# --- the negative probe: the red path, demonstrated --------------------------


def test_a_synthetic_tripwire_fails_the_gate(tmp_path: Path) -> None:
    """RED PATH: a tree holding one tombstone assertion fails, naming file:line."""
    repo = _fixture_tree(tmp_path, _TOMBSTONE)

    result = _run(repo)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "test_fixture.py:2" in result.stderr, result.stderr
    assert "absent-api assertion" in result.stderr
    assert "retirement tripwire" in result.stderr


def test_the_same_fixture_passes_once_it_carries_the_d2_marker(tmp_path: Path) -> None:
    """The D2 marker is the exemption: same assertion, labeled, is a green."""
    labeled = (
        f"# {_MARKER} the live Pool handle's read-only surface; not tripwire precedent.\n"
        + _TOMBSTONE
    )
    repo = _fixture_tree(tmp_path, labeled)

    result = _run(repo)

    assert result.returncode == 0, result.stdout + result.stderr
    assert "clean" in result.stdout


def test_a_tree_with_no_surface_files_fails_closed(tmp_path: Path) -> None:
    """An empty enumeration never certifies: 0 scanned files is exit 2, not green."""
    repo = tmp_path / "empty"
    repo.mkdir()
    _git(repo, "init", "--quiet")

    result = _run(repo)

    assert result.returncode == 2, result.stdout + result.stderr
    assert "examined nothing" in result.stderr


# --- per-signature coverage: every rule fires on its own red source ----------

# One minimal red source per signature in the scanner's table. Each entry names the
# label the finding must carry, so a rule that silently stopped firing fails here.
_RED_SOURCES = {
    "absent-api assertion": [
        'assert not hasattr(bot, "retired_thing")\n',
        "assert not any(hasattr(obj, name) for name in retired)\n",
        'assert "gone" not in vars(registry)\n',
        'assert "gone" not in module.__all__\n',
    ],
    "retired-name grep": [
        'assert "retired_name" not in source\n',
        'assert not re.search(r"RETIRED_LITERAL: u64 = \\d+", source)\n',
        'assert "retired" not in field_names\n',
        'assert "retired" not in param_names\n',
    ],
    "regrowth assertion": [
        "assert not reintroduced\n",
        "assert regrown == []\n",
    ],
}


@pytest.mark.parametrize(
    ("label", "body"),
    [(label, body) for label, bodies in _RED_SOURCES.items() for body in bodies],
    ids=[f"{label}-{i}" for label, bodies in _RED_SOURCES.items() for i in range(len(bodies))],
)
def test_every_signature_fires_on_its_own_red_source(tmp_path: Path, label: str, body: str) -> None:
    repo = _fixture_tree(tmp_path, body)

    result = _run(repo)

    assert result.returncode == 1, result.stdout + result.stderr
    assert label in result.stderr, result.stderr


def test_the_rust_source_scan_signature_fires(tmp_path: Path) -> None:
    """The Rust `include_str!` spelling of a retired-name grep."""
    body = (
        'const SRC: &str = include_str!("../src/x.rs");\n'
        'fn t() { assert!(!SRC.contains("retired_switch")); }\n'
    )
    repo = tmp_path / "tree"
    (repo / "rust" / "crate" / "tests").mkdir(parents=True)
    _git(repo, "init", "--quiet")
    (repo / "rust" / "crate" / "tests" / "pin.rs").write_text(body, encoding="utf-8")

    result = _run(repo)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "retired-name grep" in result.stderr


# --- the prose signature ----------------------------------------------------


def test_a_prose_revival_ban_in_docs_fails(tmp_path: Path) -> None:
    body = "The old registry was retired. Do not revive it.\n"
    repo = tmp_path / "tree"
    (repo / "docs").mkdir(parents=True)
    _git(repo, "init", "--quiet")
    (repo / "docs" / "note.md").write_text(body, encoding="utf-8")

    result = _run(repo)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "revival prohibition" in result.stderr


def test_a_prose_ban_without_a_retirement_nearby_is_a_design_note(tmp_path: Path) -> None:
    """A prohibition about a live design is not a guard on a deleted thing.

    This is the false-positive class the paragraph scope exists for: the words match,
    but nothing in scope was retired, so the sentence argues about present code.
    """
    body = "The additive proof must not rebuild the matrix; that explosion is the problem.\n"
    repo = tmp_path / "tree"
    (repo / "docs").mkdir(parents=True)
    _git(repo, "init", "--quiet")
    (repo / "docs" / "note.md").write_text(body, encoding="utf-8")

    result = _run(repo)

    assert result.returncode == 0, result.stdout + result.stderr


def test_a_quoted_prohibition_is_naming_the_rule_not_stating_it(tmp_path: Path) -> None:
    """Quoting is how a document names a prohibition it defines or repeals."""
    body = 'The retired guard said "do not revive this term"; ADR-087 deleted it.\n'
    repo = tmp_path / "tree"
    (repo / "docs").mkdir(parents=True)
    _git(repo, "init", "--quiet")
    (repo / "docs" / "note.md").write_text(body, encoding="utf-8")

    result = _run(repo)

    assert result.returncode == 0, result.stdout + result.stderr


# --- scope ------------------------------------------------------------------


def test_a_present_behavior_assertion_is_not_flagged(tmp_path: Path) -> None:
    """Tests of present code are not tripwires (ADR-087 D1 clarification 1)."""
    body = (
        "def test_build_pool_returns_a_pool(bot):\n"
        "    pool = bot.build_pool(address)\n"
        "    assert pool.address == address\n"
        '    assert hasattr(pool, "reserve_pair")\n'
        "    with pytest.raises(TypeError):\n"
        "        bot.build_pool(address, tick_bitmap={})\n"
    )
    repo = _fixture_tree(tmp_path, body)

    result = _run(repo)

    assert result.returncode == 0, result.stdout + result.stderr


def test_a_retired_identifier_outside_the_surface_is_out_of_scope(tmp_path: Path) -> None:
    """Production code is not the D4 surface; the compiler and review own it.

    The tree still has to hold a surface file for the run to be a green at all, so the
    fixture carries one alongside the production module this test is about.
    """
    repo = tmp_path / "tree"
    (repo / "src" / "pkg").mkdir(parents=True)
    (repo / "tests").mkdir(parents=True)
    _git(repo, "init", "--quiet")
    (repo / "src" / "pkg" / "mod.py").write_text(
        'def helper(bot):\n    assert not hasattr(bot, "gone")\n', encoding="utf-8"
    )
    (repo / "tests" / "test_live.py").write_text(
        "def test_live(bot):\n    assert bot.address\n", encoding="utf-8"
    )

    result = _run(repo)

    assert result.returncode == 0, result.stdout + result.stderr
    assert "1 files in the D4 surface" in result.stdout


def test_a_rust_test_file_carrying_a_test_module_is_in_scope(tmp_path: Path) -> None:
    """The census found tripwires inside production modules' inline test modules."""
    body = (
        "fn live() -> u32 { 1 }\n"
        "#[cfg(test)]\n"
        "mod tests {\n"
        '    const SRC: &str = include_str!("x.rs");\n'
        "    #[test]\n"
        '    fn gone() { assert!(!SRC.contains("retired_switch")); }\n'
        "}\n"
    )
    repo = tmp_path / "tree"
    (repo / "rust" / "crate" / "src").mkdir(parents=True)
    _git(repo, "init", "--quiet")
    (repo / "rust" / "crate" / "src" / "lib.rs").write_text(body, encoding="utf-8")

    result = _run(repo)

    assert result.returncode == 1, result.stdout + result.stderr


# --- the landing tree is NOT asserted here ----------------------------------
# The tree's own verdict is the gate's, not this module's: `just lint-tripwire-ratchet`
# and the pre-push hook run the scan over the real surface, and a duplicate assertion
# here would report the same findings twice while reddening the suite for a reason the
# gate already states better. What this module owns is the scanner's LOGIC -- that its
# rules fire (above), that the marker excuses, and that a blind scan fails closed.
