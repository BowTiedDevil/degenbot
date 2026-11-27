"""The comment-hygiene gate (`scripts/hooks/comment-hygiene.sh`).

# live-invariant gate (ADR-087 D2): watches the live comment surface of the scanned
# file classes against docs/comments.md; not tripwire precedent.

One anchored detector with zero tolerance: a tracker citation, a migration-work-item
label, or a concrete incident label in a scanned source file fails the commit. No
census and no waiver stands behind it, so a matcher that has gone blind reports the
same green as a swept tree -- the GLOSSARY's negative-probe idiom is the gate's red
path:

  * `test_a_synthetic_citation_fails_the_gate` puts one citation in a throwaway tree
    and requires the nonzero exit that names the site.
  * `test_every_retained_shape_fires_on_its_own_red_source` drives every retained
    shape on its own source, so one that stops matching cannot hide behind the
    others.
  * `test_review_owned_narration_passes` and `test_review_owned_bare_tokens_pass`
    pin the classes review owns instead of the detector: broad past-voice narration
    and bare six-character tokens.
  * `test_legitimate_prose_passes` pins the shapes the patterns must not trip on.
  * `test_the_scan_scope_is_the_index_not_the_working_tree` proves the population.
  * `test_the_trigger_covers_every_scanned_file_class` reconciles the registry
    trigger, the rendered hook line and the script's own enumeration.
  * `test_the_committed_tree_is_clean` runs the real gate over the real tree.

Every banned band below is spelled by concatenation. This module lives under
`tests/`, which the same gate scans, so a literal banned band here would fail the
very gate it probes.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess  # ruff: ignore[suspicious-subprocess-import]
from pathlib import Path

import pytest

from tests.repo_script import load_script

_REPO_ROOT = Path(__file__).resolve().parents[1]
_HOOK = _REPO_ROOT / "scripts" / "hooks" / "comment-hygiene.sh"
_PREK = _REPO_ROOT / "prek.toml"
_BASH = shutil.which("bash")
assert _BASH is not None, "bash is not installed on PATH"

# The scanned file classes: one definition here, one in the hook's enumeration, one
# in the registry trigger (reconciled by the trigger probe below).
_SCANNED_CLASSES = ("py", "rs", "pyi", "toml", "yml", "yaml", "sh", "sol")

_ID = "ABC" + "123"

# Every shape the detector retains, one source line each. Assembled, never
# spelled: see the module docstring.
_SHAPES = [
    pytest.param("er" + f"go {_ID}", id="ergo-id"),
    pytest.param("er" + f"go task {_ID}", id="ergo-kind-id"),
    pytest.param("ep" + f"ic {_ID}", id="kind-id"),
    pytest.param("RE" + f"D {_ID}", id="red-before-id"),
    pytest.param(f"{_ID} G" + "REEN", id="green-after-id"),
    pytest.param("sli" + "ce 14j", id="migration-slice"),
    pytest.param("sli" + "ce-3b", id="migration-slice-hyphen"),
    pytest.param("Pl" + "an 102", id="migration-plan"),
    pytest.param("pl" + "an-101", id="migration-plan-hyphen"),
    pytest.param("TO" + "DO-e30504ed", id="todo-hex"),
    pytest.param("TO" + "DO(extract-per-family-arms)", id="todo-word"),
    pytest.param("Ep" + "ic A1", id="epic-token"),
    pytest.param("sli" + "ce C4", id="slice-token"),
    pytest.param("Pha" + "se B1", id="phase-letter-digit"),
    pytest.param("pr" + "e-epic", id="epic-prefixed"),
    pytest.param("pos" + "t-epic", id="epic-suffixed"),
    pytest.param("re" + "tired grab", id="incident-retired-artifact"),
    pytest.param("twin-" + "colla" + "pse", id="incident-repair-name"),
    pytest.param("gr" + "ep compat", id="incident-grep-compat"),
    pytest.param("cr" + "ash #42", id="incident-ordinal"),
    pytest.param("RE" + "D placeholders", id="incident-gate-placeholder"),
]

# Review-owned: the change's voice, which no anchor word separates from live
# present-tense prose. docs/comments.md says review catches these.
_PAST_VOICE = [
    "must " + "STAY",
    "the guard " + "STAYS",
    "the " + "prior constant",
    "deleted in a " + "later slice",
    "this chunk's " + "change",
    "surfaces " + "fixed",
    "was " + "removed",
    "before the " + "fix",
    "post-" + "fix",
    "pre-" + "fix",
    "used to " + "live",
]

# Review-owned: bare six-character tokens with no anchor word. A legit identifier,
# an acronym and a tracker citation share one surface form.
_BARE_TOKENS = ["SI" + "GINT", "WE" + "TH", "SELF" + "DESTRUCT", "ABC" + "XYZ"]

# Prose about the code: data slicing, the call and index forms, an algorithm phase.
_LEGITIMATE_PROSE = [
    "the two-" + "slice intake",
    "pl" + "an(1.5)",
    "pl" + "an[0]",
    "the eager " + "phase 2 ignores it",
]


def _fixture(tmp_path: Path, *lines: str) -> Path:
    """A throwaway tree holding `lines` as one Python module."""
    tree = tmp_path / "tree"
    tree.mkdir()
    body = "".join(f"# {line}\n" for line in lines)
    (tree / "mod.py").write_text(body, encoding="utf-8")
    return tree


def _environment(**overrides: str) -> dict[str, str]:
    """This process's environment with HCR removed, then `overrides` applied."""
    env = {key: value for key, value in os.environ.items() if key != "HCR"}
    env.update(overrides)
    return env


def _run(tree: Path) -> subprocess.CompletedProcess[str]:
    """Run the real hook over `tree`, its documented fixture-root override."""
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [_BASH, str(_HOOK)],
        env=_environment(HCR=str(tree)),
        check=False,
        capture_output=True,
        text=True,
    )


# --- the negative probe: the red path, demonstrated --------------------------


def test_a_synthetic_citation_fails_the_gate(tmp_path: Path) -> None:
    """RED PATH: one citation fails the gate, naming the file and the line."""
    tree = _fixture(tmp_path, "er" + f"go {_ID}")

    result = _run(tree)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "comment-hygiene" in result.stderr, result.stderr
    assert "mod.py:1:" in result.stderr, result.stderr


@pytest.mark.parametrize("band", _SHAPES)
def test_every_retained_shape_fires_on_its_own_red_source(tmp_path: Path, band: str) -> None:
    """Each shape is caught on its own, so a blind shape cannot hide."""
    tree = _fixture(tmp_path, band)

    result = _run(tree)

    assert result.returncode == 1, f"{band!r} went unseen: " + result.stdout + result.stderr
    assert band in result.stderr, result.stderr


# --- the classes review owns ------------------------------------------------


def test_review_owned_narration_passes(tmp_path: Path) -> None:
    """Past-voice change narration is review's, not the detector's."""
    tree = _fixture(tmp_path, *_PAST_VOICE)

    result = _run(tree)

    assert result.returncode == 0, result.stderr


def test_review_owned_bare_tokens_pass(tmp_path: Path) -> None:
    """A bare six-character token has no anchor word to judge it by."""
    tree = _fixture(tmp_path, *_BARE_TOKENS)

    result = _run(tree)

    assert result.returncode == 0, result.stderr


def test_legitimate_prose_passes(tmp_path: Path) -> None:
    """The narrow shapes leave data slicing, call forms and phases alone."""
    tree = _fixture(tmp_path, *_LEGITIMATE_PROSE)

    result = _run(tree)

    assert result.returncode == 0, result.stderr


def test_an_unlabelled_epic_word_is_prose(tmp_path: Path) -> None:
    """`epic <word>` with no digit is prose; the labelled work item still fails."""
    phrase = "an " + "ep" + "ic feature"
    prose = _fixture(tmp_path, phrase)

    prose_result = _run(prose)

    assert prose_result.returncode == 0, prose_result.stderr

    (tmp_path / "labelled").mkdir()
    band = "Ep" + "ic A1"
    labelled = _fixture(tmp_path / "labelled", band)

    labelled_result = _run(labelled)

    assert labelled_result.returncode == 1, labelled_result.stderr
    assert band in labelled_result.stderr, labelled_result.stderr


# --- the committed tree -----------------------------------------------------


def test_the_committed_tree_is_clean() -> None:
    """The real gate over the real tree: the swept surface stays swept."""
    result = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [_BASH, str(_HOOK)],
        cwd=_REPO_ROOT,
        env=_environment(),
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr


# --- scope and trigger ------------------------------------------------------


def _git_fixture(tmp_path: Path) -> Path:
    """A throwaway git checkout the gate certifies in its real (non-HCR) mode.

    Index scope only exists inside a git work tree, so this fixture commits one
    clean source file and returns the checkout.
    """
    repo = tmp_path / "repo"
    (repo / "src").mkdir(parents=True)
    (repo / "src" / "lib.rs").write_text("// nothing to see here\n", encoding="utf-8")
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
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [_BASH, str(_HOOK)],
        cwd=repo,
        env=_environment(),
        check=False,
        capture_output=True,
        text=True,
    )


def test_the_scan_scope_is_the_index_not_the_working_tree(tmp_path: Path) -> None:
    """A concurrent lane's untracked work cannot fail this commit; staging it can."""
    repo = _git_fixture(tmp_path)
    violating = repo / "src" / "in_progress.rs"
    violating.write_text("// er" + f"go {_ID}\n", encoding="utf-8")

    untracked = _run_in_repo(repo)
    assert untracked.returncode == 0, untracked.stderr

    subprocess.run(["git", "-C", str(repo), "add", str(violating)], check=True)
    staged = _run_in_repo(repo)
    assert staged.returncode == 1, staged.stderr
    assert "in_progress.rs" in staged.stderr, staged.stderr

    # The registry's declaration has to be the population this test just
    # demonstrated: the scan enumerates git's index, so it is "staged" -- a "tree"
    # declaration would promise untracked files are examined.
    gate = _registry_gate("comment-hygiene")
    assert gate.scope == "staged", (
        f"the gate scans git's index; its registered population is not {gate.scope!r}"
    )
    assert gate.pass_filenames is False
    assert gate.data == ()
    assert "git ls-files -z --cached" in _HOOK.read_text(encoding="utf-8")


def _scanned_classes() -> tuple[str, ...]:
    """The class list the hook's own enumeration is built from."""
    script = _HOOK.read_text(encoding="utf-8")
    match = re.search(r"^exts=\((?P<classes>[^)]*)\)$", script, re.MULTILINE)
    assert match is not None, "the hook declares its scanned classes as one `exts=(...)` list"
    return tuple(match.group("classes").split())


def test_the_trigger_covers_every_scanned_file_class() -> None:
    """The pre-commit trigger fires on every class the scan enumerates."""
    gate = _registry_gate("comment-hygiene")
    assert "types_or" not in gate.triggers, gate.triggers
    assert "files" in gate.triggers, gate.triggers
    trigger = re.compile(gate.triggers["files"])

    classes = _scanned_classes()
    assert classes == _SCANNED_CLASSES, classes
    for extension in classes:
        assert trigger.search(f"src/degenbot/probe.{extension}"), f"the trigger skips *.{extension}"
    assert trigger.search("justfile"), "the trigger skips the root justfile"
    assert not trigger.search("docs/gates.md"), "markdown is out of scope"

    # The rendered hook line git actually loads carries the same trigger: a
    # registry the renderer never applied is a trigger that does not fire.
    line = next(
        line
        for line in _PREK.read_text(encoding="utf-8").splitlines()
        if '{ id = "comment-hygiene"' in line
    )
    assert "types_or" not in line, line
    assert gate.triggers["files"] in line, line


def _registry_gate(gate_id: str):
    """The committed registry's (validated) declaration for `gate_id`."""
    gate_registry = load_script("gate_registry", _REPO_ROOT / "scripts" / "gate_registry.py")
    return {gate.id: gate for gate in gate_registry.load(_REPO_ROOT)}[gate_id]
