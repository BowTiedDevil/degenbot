"""The worker-census spawn-site gate (``scripts/hooks/worker_census_spawn_gate.py``).

# live-invariant gate (ADR-087 D2): watches the live spawn sites of the crate tree

# against their registrations and the reviewed inventory; not tripwire precedent.

A ratchet whose matcher has gone blind reports the same green a clean tree does,
so its red path is evidence in its own right -- the negative-probe idiom applied
to the gate:
  * ``test_a_planted_spawn_site_fails_and_names_it`` is run one of the recorded
    two-run probe: a bare spawn site in a fixture tree fails, and the failure
    names the file, the line, the shape and the row to add.
  * ``test_the_two_run_probe_is_green_once_its_row_lands`` is run two: the same
    tree, the same site, admitted by one reviewed row, is green.
  * ``test_every_spawn_shape_is_a_site`` drives the three shapes one at a time, so
    a shape that stopped matching cannot hide behind the others.
  * ``test_a_registering_function_is_sanctioned`` pins the positive path: a
    function that registers its own site is never reported.
  * ``test_a_stale_row_fails`` and ``test_an_unreasoned_row_fails`` pin the two
    reverse directions of the reconciliation.
  * ``test_a_missing_inventory_fails_closed`` and
    ``test_a_tree_with_no_spawn_site_fails_closed`` pin the edges where the gate
    must refuse rather than certify.
  * ``test_the_committed_inventory_matches_the_tree`` runs the real gate over the
    real tree, so a finding here is a site to register or a row to delete.
"""

from __future__ import annotations

import shutil
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import sys
import tomllib
from pathlib import Path

_REPO_ROOT = Path(__file__).resolve().parents[1]
_GATE = _REPO_ROOT / "scripts" / "hooks" / "worker_census_spawn_gate.py"
_INVENTORY = _REPO_ROOT / "scripts" / "hooks" / "worker-census-spawn-inventory.txt"
_REGISTRY = _REPO_ROOT / "scripts" / "gates.toml"
_RECIPE = "check-worker-census-spawn"
_GATE_ID = "worker-census-spawn"
_SITE_FILE = "rust/crates/fixture-crate/src/spawn_site.rs"
_ROW = f"{_SITE_FILE}:spawn_helper"
_BARE_SITE = """pub fn spawn_helper() -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("gate-fixture".to_string())
        .spawn(|| {})
        .expect("fixture spawn")
}
"""
_REGISTERING_FILE = """pub fn spawn_helper() {
    degenbot_core::worker_census::register(row());
    let _ = std::thread::Builder::new().spawn(|| {});
}
pub fn spawn_listed_helper() {
    let _ = std::thread::Builder::new().spawn(|| {});
}
fn row() -> WorkerCensusEntry {
    todo!()
}
"""
_MULTI_SITE = """pub fn spawn_helper() {
    let _ = Builder::new_multi_thread().build();
}
"""
_CURRENT_SITE = """pub fn spawn_helper() {
    let _ = Builder::new_current_thread().build();
}
"""
_NO_SITE = """pub fn no_spawn_here() -> usize {
    3
}
"""


def _tree(tmp_path: Path, source: str) -> Path:
    """A throwaway tree holding `source` at the scanned path."""
    tree = tmp_path / "tree"
    path = tree / _SITE_FILE
    path.parent.mkdir(parents=True)
    path.write_text(source, encoding="utf-8")
    return tree


def _inventory(tmp_path: Path, body: str) -> Path:
    """An inventory file holding `body`, standing in for the committed one."""
    path = tmp_path / "inventory.txt"
    path.write_text(body, encoding="utf-8")
    return path


def _run(tree: Path | None, inventory: Path | str) -> subprocess.CompletedProcess[str]:
    """Run the real gate over `tree` (the repository when None) and `inventory`."""
    argv = [sys.executable, str(_GATE), "--inventory", str(inventory)]
    if tree is not None:
        argv += ["--root", str(tree)]
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        argv, cwd=_REPO_ROOT, check=False, capture_output=True, text=True
    )


# --- the recorded two-run probe ---------------------------------------------
def test_a_planted_spawn_site_fails_and_names_it(tmp_path: Path) -> None:
    """Run one: a bare spawn site with no inventory row fails, naming the row."""
    tree = _tree(tmp_path, _BARE_SITE)
    result = _run(tree, _inventory(tmp_path, "# no exceptions yet\n"))
    assert result.returncode == 1, result.stdout + result.stderr
    assert f"fresh: {_SITE_FILE}:2: spawn_helper (std thread)" in result.stderr, result.stderr
    assert f"inventory row: {_ROW}" in result.stderr, result.stderr
    assert "fix: register the worker at the spawn site" in result.stderr, result.stderr


def test_the_two_run_probe_is_green_once_its_row_lands(tmp_path: Path) -> None:
    """Run two: the same tree, its site admitted by one reviewed row, is green."""
    tree = _tree(tmp_path, _BARE_SITE)
    result = _run(
        tree,
        _inventory(tmp_path, f"# fixture: a probe thread, none spawned in production\n{_ROW}\n"),
    )
    assert result.returncode == 0, result.stdout + result.stderr
    assert f"{_ROW}" not in result.stderr, result.stderr
    assert "1 admitted by 1 reviewed inventory rows" in result.stdout, result.stdout


# --- every shape, and the positive path --------------------------------------
def test_every_spawn_shape_is_a_site(tmp_path: Path) -> None:
    """A source with a bare spawn site fails in each of the three shapes."""
    for index, source in enumerate((_BARE_SITE, _MULTI_SITE, _CURRENT_SITE)):
        tree = _tree(tmp_path / f"shape-{index}", source)
        result = _run(tree, _inventory(tmp_path, "# no exceptions yet\n"))
        assert result.returncode == 1, result.stdout + result.stderr
        assert f"inventory row: {_ROW}" in result.stderr, result.stderr


def test_a_registering_function_is_sanctioned(tmp_path: Path) -> None:
    """A site whose function registers is not reported; the bare sibling still is listed."""
    tree = _tree(tmp_path, _REGISTERING_FILE)
    listed = f"{_SITE_FILE}:spawn_listed_helper"
    result = _run(
        tree, _inventory(tmp_path, f"# fixture: the second helper is the listed one\n{listed}\n")
    )
    assert result.returncode == 0, result.stdout + result.stderr
    assert "1 register in their own function" in result.stdout, result.stdout


# --- the ratchet, both directions -------------------------------------------
def test_a_stale_row_fails(tmp_path: Path) -> None:
    """A row with no live site fails: a swept site's row goes in the same change."""
    tree = _tree(tmp_path, _BARE_SITE)
    removed = f"{_SITE_FILE}:spawn_removed"
    body = f"# fixture: live site\n{_ROW}\n\n# fixture: a site that is gone\n{removed}\n"
    result = _run(tree, _inventory(tmp_path, body))
    assert result.returncode == 1, result.stdout + result.stderr
    assert f"stale: {removed}" in result.stderr, result.stderr
    assert "delete this row" in result.stderr, result.stderr


def test_an_unreasoned_row_fails(tmp_path: Path) -> None:
    """A row with no reason written above it fails: the inventory is a reviewed list."""
    tree = _tree(tmp_path, _BARE_SITE)
    result = _run(tree, _inventory(tmp_path, f"{_ROW}\n"))
    assert result.returncode == 1, result.stdout + result.stderr
    assert f"unreasoned: {_ROW}" in result.stderr, result.stderr


# --- the fail-closed edges --------------------------------------------------
def test_a_missing_inventory_fails_closed(tmp_path: Path) -> None:
    """An absent inventory never certifies: the gate refuses with its second exit code."""
    tree = _tree(tmp_path, _BARE_SITE)
    result = _run(tree, tmp_path / "absent-inventory.txt")
    assert result.returncode == 2, result.stdout + result.stderr
    assert "inventory missing" in result.stderr, result.stderr


def test_a_tree_with_no_spawn_site_fails_closed(tmp_path: Path) -> None:
    """A scan that found no site at all refuses to certify a matcher it cannot trust."""
    tree = _tree(tmp_path, _NO_SITE)
    result = _run(tree, _inventory(tmp_path, "# no exceptions yet\n"))
    assert result.returncode == 2, result.stdout + result.stderr
    assert "gone blind" in result.stderr, result.stderr


# --- the committed tree and the wiring --------------------------------------
def test_the_committed_inventory_matches_the_tree() -> None:
    """The real gate over the real tree: green, or there is a row to add or delete."""
    result = _run(None, _INVENTORY)
    assert result.returncode == 0, result.stdout + result.stderr
    assert "register in their own function" in result.stdout, result.stdout


def test_the_gate_is_registered_where_it_runs() -> None:
    """The recipe resolves under just, and the registry declares the hook."""
    just = shutil.which("just")
    assert just is not None, "just is not installed on PATH"
    listed = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        [just, "--list"], cwd=_REPO_ROOT, check=False, capture_output=True, text=True
    )
    assert listed.returncode == 0, listed.stderr
    assert _RECIPE in {line.split()[0] for line in listed.stdout.splitlines() if line.split()}
    registry = tomllib.loads(_REGISTRY.read_text(encoding="utf-8"))
    entries = {gate["id"]: gate for gate in registry["gate"]}
    assert _GATE_ID in entries, sorted(entries)
    assert entries[_GATE_ID]["entry"] == f"just {_RECIPE}", entries[_GATE_ID]
