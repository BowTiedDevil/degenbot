"""The publish-blocked census (``scripts/publish_blocked.py``).

A census gate that has gone blind reports the same green as a fully covered
publish chain, so this file is the gate's own evidence: the GLOSSARY's
negative-probe idiom applied to the publish-blocked census. Every case drives
the real gate with ``--root`` pointed at a throwaway workspace, so no assertion
here can be satisfied by a re-typed copy of the gate's logic.

  * ``test_a_fresh_blocked_member_fails_and_names_the_row`` -- the red path: a
    member the manifests withhold with no census row fails and the message
    names the row to add.
  * ``test_a_listed_row_grandfathers_the_withheld_member`` -- the census is
    what keeps a recorded member from blocking the gate.
  * ``test_a_stale_row_fails_the_gate`` -- the other direction: a member that
    packages again deletes its row in the same change.
  * ``test_a_tree_reason_the_census_omits_fails`` and
    ``test_a_census_reason_the_tree_dropped_fails`` -- the reason mirror, both
    ways, so a row cannot describe a population the manifests no longer imply.
  * ``test_a_missing_census_fails_closed``,
    ``test_a_row_without_a_reason_fails_closed`` and
    ``test_a_tree_without_a_publishable_member_fails_closed`` -- the
    fail-closed edges.
  * ``test_a_versionless_dev_dep_is_stripped_not_blocking`` -- rule 1: the one
    dependency shape cargo deletes at packaging.
  * ``test_a_versionless_git_dep_is_blocking`` -- rule 2: cargo refuses to
    package it at all, so it is withheld like any other unpackageable member.
  * ``test_a_versioned_dev_dep_still_requires_its_target`` -- the rule the
    degenbot-submission manifest states in prose: a ``version`` key turns even a
    dev-dependency into a crates.io requirement.
  * ``test_a_blocked_member_blocks_its_dependents`` -- rule 4, the closure.
  * ``test_the_blocked_list_is_the_withheld_population`` -- ``--blocked``
    prints exactly the rows' members.
  * ``test_a_normal_dev_cycle_fails_with_the_cycle_named`` -- a cycle formed
    by registry-retained normal + dev dependencies among publishable members
    fails the gate and the message names the cycle; no publish order exists.
  * ``test_a_stripped_versionless_dev_dep_does_not_close_a_cycle`` -- the
    dependency shape cargo strips at packaging is not a requirement and
    cannot close a cycle.
  * ``test_an_acyclic_retained_dev_dep_passes`` -- a retained dev edge is
    only an ordering constraint; without a cycle nothing is withheld.
  * ``test_the_committed_census_matches_the_tree`` -- the real gate over the
    real tree.
"""

from __future__ import annotations

import subprocess  # ruff: ignore[suspicious-subprocess-import]
import sys
from pathlib import Path

_REPO_ROOT = Path(__file__).resolve().parents[1]
_GATE = _REPO_ROOT / "scripts" / "publish_blocked.py"
_CENSUS_PARTS = ("scripts", "publish-blocked-census.txt")

_GIT_DEP = 'evm2 = {{ git = "https://example.invalid/evm2.git"{version} }}'
_GIT_DEP_JIT = 'evm2_jit = { git = "https://example.invalid/evm2.git", version = "0.1.0" }'
_VERSIONED = ', version = "0.1.0"'


def _tree(
    tmp_path: Path, members: dict[str, str], publishable: tuple[str, ...] | None = None
) -> Path:
    """A throwaway workspace whose members carry `members`' dependency tables.

    `publishable` narrows the members that declare no `publish = false`; every
    listed member gets a real lib target so `cargo metadata` will enumerate it.
    """
    root = tmp_path / "tree"
    names = list(members)
    publishable = names if publishable is None else list(publishable)
    workspace = root / "rust" / "Cargo.toml"
    workspace.parent.mkdir(parents=True)
    quoted = ", ".join(f'"crates/{name}"' for name in names)
    workspace.write_text(f'[workspace]\nmembers = [{quoted}]\nresolver = "2"\n', encoding="utf-8")
    for name, dependency_table in members.items():
        crate = root / "rust" / "crates" / name
        (crate / "src").mkdir(parents=True)
        (crate / "src" / "lib.rs").write_text("", encoding="utf-8")
        publish = "" if name in publishable else "\npublish = false\n"
        manifest = (
            f'[package]\nname = "{name}"\nversion = "0.1.0"\nedition = "2021"\n'
            f"{publish}\n{dependency_table}"
        )
        (crate / "Cargo.toml").write_text(manifest, encoding="utf-8")
    return root


def _census(tree: Path, rows: list[str]) -> None:
    """Write the census `tree` implies."""
    path = tree.joinpath(*_CENSUS_PARTS)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("".join(f"{row}\n" for row in rows), encoding="utf-8")


def _run(tree: Path, *args: str) -> subprocess.CompletedProcess[str]:
    """Run the real gate over `tree`."""
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [sys.executable, str(_GATE), "--root", str(tree), *args],
        check=False,
        capture_output=True,
        text=True,
    )


def _deps(body: str) -> str:
    return f"[dependencies]\n{body}\n" if body else ""


def test_a_fresh_blocked_member_fails_and_names_the_row(tmp_path: Path) -> None:
    tree = _tree(tmp_path, {"b": _deps(_GIT_DEP.format(version=_VERSIONED))})
    _census(tree, [])
    result = _run(tree)
    assert result.returncode == 1
    assert "b\tgit dep evm2@^0.1.0" in result.stdout


def test_a_listed_row_grandfathers_the_withheld_member(tmp_path: Path) -> None:
    tree = _tree(tmp_path, {"b": _deps(_GIT_DEP.format(version=_VERSIONED))})
    _census(tree, ["b\tgit dep evm2@^0.1.0"])
    result = _run(tree)
    assert result.returncode == 0
    assert "withheld from publish-dry-run: b" in result.stdout


def test_a_stale_row_fails_the_gate(tmp_path: Path) -> None:
    tree = _tree(tmp_path, {"b": _deps("")})
    _census(tree, ["b\tgit dep evm2@^0.1.0"])
    result = _run(tree)
    assert result.returncode == 1
    assert "census row with no publish-blocked member" in result.stdout
    assert "b\tgit dep evm2@^0.1.0" in result.stdout


def test_a_tree_reason_the_census_omits_fails(tmp_path: Path) -> None:
    table = f"{_GIT_DEP.format(version=_VERSIONED)}\n{_GIT_DEP_JIT}"
    tree = _tree(tmp_path, {"b": _deps(table)})
    _census(tree, ["b\tgit dep evm2@^0.1.0"])
    result = _run(tree)
    assert result.returncode == 1
    assert "reason drift for b" in result.stdout
    assert "evm2_jit@^0.1.0" in result.stdout


def test_a_census_reason_the_tree_dropped_fails(tmp_path: Path) -> None:
    tree = _tree(tmp_path, {"b": _deps(_GIT_DEP.format(version=_VERSIONED))})
    _census(tree, ["b\tgit dep evm2@^0.1.0, git dep evm2_jit@^0.1.0"])
    result = _run(tree)
    assert result.returncode == 1
    assert "reason drift for b" in result.stdout
    assert "manifests: git dep evm2@^0.1.0" in result.stdout
    assert "census:    git dep evm2@^0.1.0, git dep evm2_jit@^0.1.0" in result.stdout


def test_a_missing_census_fails_closed(tmp_path: Path) -> None:
    tree = _tree(tmp_path, {"b": _deps(_GIT_DEP.format(version=_VERSIONED))})
    result = _run(tree)
    assert result.returncode == 2
    assert "missing census" in result.stderr


def test_a_row_without_a_reason_fails_closed(tmp_path: Path) -> None:
    tree = _tree(tmp_path, {"b": _deps(_GIT_DEP.format(version=_VERSIONED))})
    path = tree.joinpath(*_CENSUS_PARTS)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("b\n", encoding="utf-8")
    result = _run(tree)
    assert result.returncode == 2
    assert "row is not '<member>TAB<reason>'" in result.stderr


def test_a_tree_without_a_publishable_member_fails_closed(tmp_path: Path) -> None:
    tree = _tree(tmp_path, {"b": _deps("")}, publishable=())
    _census(tree, [])
    result = _run(tree)
    assert result.returncode == 2
    assert "no publishable member" in result.stderr


def test_a_versionless_dev_dep_is_stripped_not_blocking(tmp_path: Path) -> None:
    table = '[dev-dependencies]\nb = { path = "../b" }'
    tree = _tree(tmp_path, {"b": _deps(_GIT_DEP.format(version=_VERSIONED)), "a": table})
    _census(tree, ["b\tgit dep evm2@^0.1.0"])
    result = _run(tree, "--rows")
    assert result.returncode == 0
    assert "a\t" not in result.stdout


def test_a_versionless_git_dep_is_blocking(tmp_path: Path) -> None:
    tree = _tree(tmp_path, {"b": _deps(_GIT_DEP.format(version=""))})
    _census(tree, [])
    result = _run(tree, "--rows")
    assert result.returncode == 0
    assert "b\tgit dep evm2 (version-less)" in result.stdout


def test_a_versioned_dev_dep_still_requires_its_target(tmp_path: Path) -> None:
    table = '[dev-dependencies]\nb = { path = "../b", version = "0.1.0" }'
    tree = _tree(tmp_path, {"b": _deps(_GIT_DEP.format(version=_VERSIONED)), "a": table})
    _census(tree, ["b\tgit dep evm2@^0.1.0"])
    result = _run(tree, "--rows")
    assert result.returncode == 0
    assert "a\tdepends on blocked b" in result.stdout


def test_a_blocked_member_blocks_its_dependents(tmp_path: Path) -> None:
    table = 'b = { path = "../b", version = "0.1.0" }'
    tree = _tree(tmp_path, {"b": _deps(_GIT_DEP.format(version=_VERSIONED)), "a": _deps(table)})
    _census(tree, ["b\tgit dep evm2@^0.1.0"])
    result = _run(tree, "--rows")
    assert result.returncode == 0
    assert "a\tdepends on blocked b" in result.stdout


def test_the_blocked_list_is_the_withheld_population(tmp_path: Path) -> None:
    table = 'b = { path = "../b", version = "0.1.0" }'
    tree = _tree(tmp_path, {"b": _deps(_GIT_DEP.format(version=_VERSIONED)), "a": _deps(table)})
    _census(tree, ["a\tdepends on blocked b", "b\tgit dep evm2@^0.1.0"])
    result = _run(tree, "--blocked")
    assert result.returncode == 0
    assert result.stdout.split() == ["a", "b"]


def test_a_normal_dev_cycle_fails_with_the_cycle_named(tmp_path: Path) -> None:
    """A retained normal+dev cycle among publishable members cannot be ordered.

    `a --normal--> b --retained dev--> a` leaves `cargo publish --workspace`
    no upload order: publishing `a` requires `b` at this version, and
    publishing `b` requires `a` too -- cargo resolves `b`'s retained
    versioned dev-dependency while preparing `b`'s package, even under
    `--no-verify`, so the fresh sibling version must already be resolvable.
    The gate must name the cycle, not withhold a census row for
    it -- the cycle is a release-graph defect, not a withheld member.
    """
    tree = _tree(
        tmp_path,
        {
            "a": _deps('b = { path = "../b", version = "0.1.0" }'),
            "b": '[dev-dependencies]\na = { path = "../a", version = "0.1.0" }\n',
        },
    )
    _census(tree, [])
    result = _run(tree)
    assert result.returncode == 1
    assert "publishable dependency cycle" in result.stdout
    assert "a -> b -> a" in result.stdout


def test_a_stripped_versionless_dev_dep_does_not_close_a_cycle(tmp_path: Path) -> None:
    """The one shape cargo strips from the published manifest breaks no cycle.

    `b`'s version-less path dev-dependency leaves its published manifest at
    packaging, so it is not a requirement and cannot order `b` after `a`:
    the graph is the acyclic `a -> b` and the gate stays green.
    """
    tree = _tree(
        tmp_path,
        {
            "a": _deps('b = { path = "../b", version = "0.1.0" }'),
            "b": '[dev-dependencies]\na = { path = "../a" }\n',
        },
    )
    _census(tree, [])
    result = _run(tree)
    assert result.returncode == 0
    assert "publishable dependency cycle" not in result.stdout


def test_an_acyclic_retained_dev_dep_passes(tmp_path: Path) -> None:
    """A retained dev edge is legal; only the cycle it might close is not.

    `b`'s versioned dev-dependency on leaf `c` is a real requirement (rule 3
    keeps it in the graph) but names no cycle, so nothing is withheld.
    """
    tree = _tree(
        tmp_path,
        {
            "b": '[dev-dependencies]\nc = { path = "../c", version = "0.1.0" }\n',
            "c": _deps(""),
        },
    )
    _census(tree, [])
    result = _run(tree)
    assert result.returncode == 0
    assert result.stdout.strip().endswith("withheld from publish-dry-run: none")


def test_the_committed_census_matches_the_tree() -> None:
    result = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [sys.executable, str(_GATE)],
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, result.stdout + result.stderr
