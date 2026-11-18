"""Detector 3 of the comment-hygiene gate (``scripts/hooks/comment-hygiene.sh``).

# live-invariant gate (ADR-087 D2): watches the live comment surface of the working
# tree against scripts/hooks/comment-hygiene-citation-census.txt; not tripwire precedent.

Detector 3 is a census ratchet over the tracker-citation shapes that Detector 1's
anchor words and Detector 2's uppercase tokens cannot see. A ratchet whose matcher
has gone blind reports the same green as a swept tree, so its red path is evidence
in its own right -- the GLOSSARY's negative-probe idiom applied to the gate:

  * ``test_a_synthetic_citation_fails_the_gate`` puts one citation in a throwaway
    tree and requires the nonzero exit plus the census row the gate proposes.
  * ``test_every_shape_fires_on_its_own_red_source`` drives every shape, so one
    that stops matching cannot hide behind the others.
  * ``test_a_listed_row_grandfathers_the_citation`` and
    ``test_a_stale_row_fails_the_gate`` prove the reconciliation runs both ways.
  * ``test_a_missing_census_fails_closed`` proves an absent census never certifies.
  * ``test_bare_phase_prose_is_out_of_scope`` pins the deliberate exclusion: the
    engine's algorithm phases are not plan phases.
  * ``test_the_committed_census_matches_the_tree`` runs the real gate over the real
    tree; a finding there is either a citation to strip or a census row to delete.

Every citation below is spelled by concatenation. This module lives under ``tests/``,
which the same gate scans, so a literal banned band here would fail the very gate it
probes.
"""

from __future__ import annotations

import os
import shutil
import subprocess  # ruff: ignore[suspicious-subprocess-import]
from pathlib import Path

import pytest

_REPO_ROOT = Path(__file__).resolve().parents[1]
_HOOK = _REPO_ROOT / "scripts" / "hooks" / "comment-hygiene.sh"
_BASH = shutil.which("bash")
assert _BASH is not None, "bash is not installed on PATH"

# The shapes, assembled rather than spelled: see the module docstring.
_TRACKER_HEX = "TO" + "DO-" + "e30504ed"
_TRACKER_WORD = "TO" + "DO(extract-per-family-arms)"
_LABEL_EPIC = "Ep" + "ic A1"
_LABEL_SLICE = "sli" + "ce C4"
_LABEL_PHASE = "Pha" + "se B1"
_PRE = "pr" + "e-epic"
_POST = "pos" + "t-epic"

_SHAPES = [
    pytest.param(_TRACKER_HEX, id="tracker-id-hex"),
    pytest.param(_TRACKER_WORD, id="tracker-id-word"),
    pytest.param(_LABEL_EPIC, id="epic-label"),
    pytest.param(_LABEL_SLICE, id="slice-label"),
    pytest.param(_LABEL_PHASE, id="phase-label"),
    pytest.param(_PRE, id="prefixed-label"),
    pytest.param(_POST, id="suffixed-label"),
]


def _fixture(tmp_path: Path, body: str) -> Path:
    """A throwaway tree holding `body` as one Python module."""
    tree = tmp_path / "tree"
    tree.mkdir()
    (tree / "mod.py").write_text(body, encoding="utf-8")
    return tree


def _census(tmp_path: Path, rows: list[str]) -> str:
    """A census file holding `rows`, standing in for the committed one."""
    path = tmp_path / "citation-census.txt"
    path.write_text("".join(f"{row}\n" for row in rows), encoding="utf-8")
    return str(path)


def _run(tree: Path, census: str) -> subprocess.CompletedProcess[str]:
    """Run the real hook over `tree` with the documented test overrides.

    HCR points the scan at the fixture; WAF and CHF empty the two pre-existing
    exemptions so only Detector 3 can be the reason for a failure; CCF is the
    census under test.
    """
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [_BASH, str(_HOOK)],
        env={**os.environ, "HCR": str(tree), "WAF": "/dev/null", "CHF": "/dev/null", "CCF": census},
        check=False,
        capture_output=True,
        text=True,
    )


# --- the negative probe: the red path, demonstrated --------------------------


def test_a_synthetic_citation_fails_the_gate(tmp_path: Path) -> None:
    """RED PATH: one citation with an empty census fails, naming the row to add."""
    tree = _fixture(tmp_path, f"# {_TRACKER_HEX}\n")

    result = _run(tree, "/dev/null")

    assert result.returncode == 1, result.stdout + result.stderr
    assert "Detector 3" in result.stderr, result.stderr
    assert f"census row: mod.py:{_TRACKER_HEX}" in result.stderr, result.stderr


@pytest.mark.parametrize("band", _SHAPES)
def test_every_shape_fires_on_its_own_red_source(tmp_path: Path, band: str) -> None:
    """Each shape is caught on its own, so a blind shape cannot hide."""
    tree = _fixture(tmp_path, f"# {band}\n")

    result = _run(tree, "/dev/null")

    assert result.returncode == 1, result.stdout + result.stderr
    assert f"census row: mod.py:{band}" in result.stderr, result.stderr


# --- the ratchet: both directions -------------------------------------------


def test_a_listed_row_grandfathers_the_citation(tmp_path: Path) -> None:
    """A listed row is a green: the census is what keeps a site from blocking."""
    tree = _fixture(tmp_path, f"# {_LABEL_EPIC}\n")
    census = _census(tmp_path, [f"mod.py:{_LABEL_EPIC}"])

    result = _run(tree, census)

    assert result.returncode == 0, result.stdout + result.stderr


def test_a_stale_row_fails_the_gate(tmp_path: Path) -> None:
    """A row whose citation was swept fails, so the census can only shrink."""
    tree = _fixture(tmp_path, "# nothing to see here\n")
    gone = "ep" + "ic ZZ"
    census = _census(tmp_path, [f"mod.py:{gone}"])

    result = _run(tree, census)

    assert result.returncode == 1, result.stdout + result.stderr
    assert f"stale: mod.py:{gone}" in result.stderr, result.stderr


def test_a_missing_census_fails_closed(tmp_path: Path) -> None:
    """An absent census refuses to run instead of passing over nothing."""
    tree = _fixture(tmp_path, "# nothing to see here\n")

    result = _run(tree, str(tmp_path / "absent-census.txt"))

    assert result.returncode == 1, result.stdout + result.stderr
    assert "citation census missing" in result.stderr, result.stderr


# --- scope ------------------------------------------------------------------


def test_bare_phase_prose_is_out_of_scope(tmp_path: Path) -> None:
    """A numbered algorithm phase is prose about the code, not a work item."""
    tree = _fixture(tmp_path, "# the eager phase 2 ignores it, per the boundary tail\n")

    result = _run(tree, "/dev/null")

    assert result.returncode == 0, result.stdout + result.stderr


# --- the committed census ---------------------------------------------------


def test_the_committed_census_matches_the_tree() -> None:
    """The real gate over the real tree: citations and rows are in step."""
    result = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [_BASH, str(_HOOK)],
        cwd=_REPO_ROOT,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert "stale:" not in result.stderr, result.stderr


def _git_fixture(tmp_path: Path) -> Path:
    """A throwaway git checkout the gate can certify in its real (non-HCR) mode.

    Index scope only exists when the scan runs inside a git work tree, so this
    fixture commits the three census files (empty is enough: the tree states no
    bare token) plus one clean source file, and returns the checkout.
    """
    repo = tmp_path / "repo"
    (repo / "scripts" / "hooks").mkdir(parents=True)
    (repo / "src").mkdir()
    for name in (
        "comment-hygiene-waivers.txt",
        "comment-hygiene-census.txt",
        "comment-hygiene-citation-census.txt",
    ):
        (repo / "scripts" / "hooks" / name).write_text("")
    (repo / "src" / "lib.rs").write_text("// nothing to see here\n")
    subprocess.run(["git", "init", "-q", str(repo)], check=True)  # ruff: ignore[subprocess-without-shell-equals-true]
    subprocess.run(["git", "-C", str(repo), "add", "-A"], check=True)
    subprocess.run(
        [
            "git",
            "-C",
            str(repo),
            "-c",
            "user.email=probe@example.com",
            "-c",
            "user.name=probe",
            "commit",
            "-qm",
            "probe fixture",
        ],
        check=True,
    )
    return repo


def _run_in_repo(repo: Path) -> subprocess.CompletedProcess[str]:
    """Run the hook with HCR unset, so it takes the index-scope path."""
    env = {k: v for k, v in os.environ.items() if k not in {"HCR", "WAF", "CHF", "CCF"}}
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [_BASH, str(_HOOK)],
        cwd=repo,
        env=env,
        check=False,
        capture_output=True,
        text=True,
    )


def test_the_scan_scope_is_the_index_not_the_working_tree(tmp_path: Path) -> None:
    """A concurrent lane's untracked work cannot fail this commit; staging it can."""
    repo = _git_fixture(tmp_path)
    violating = repo / "src" / "in_progress.rs"
    violating.write_text("// " + _TRACKER_HEX + "\n")

    untracked = _run_in_repo(repo)
    assert untracked.returncode == 0, untracked.stderr

    subprocess.run(["git", "-C", str(repo), "add", str(violating)], check=True)
    staged = _run_in_repo(repo)
    assert staged.returncode != 0
    assert "comment-hygiene" in staged.stderr
