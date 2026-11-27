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
  * ``test_a_planted_bare_spawn_fails_and_names_it`` and
    ``test_the_bare_probe_is_green_once_its_row_lands_or_the_function_registers``
    are the same two runs for the bare ``thread::spawn`` shape: the extended
    token names the file and line, and both sanctuaries (a reviewed row, a
    registering function) are green.
  * ``test_every_spawn_shape_is_a_site`` drives the four shapes one at a time, so
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
  * ``test_the_scope_is_location_not_shape`` is the production-scope positive
    control: one identical bare spawn is reported under ``src/`` and silent under
    ``tests/`` and ``benches/``, so the exclusion is the location, not the shape.
  * ``test_a_cfg_test_module_region_is_out_of_scope`` and
    ``test_a_cfg_all_test_feature_region_is_out_of_scope`` pin the inline
    regions, each with a production site in the same source that must still be
    named -- a matcher that went blind would pass the exclusion silently.
  * ``test_a_feature_only_module_stays_a_production_site`` is the reverse
    control: ``cfg(feature = "auto-initialize")`` ships in a binary, so its
    spawn is a site.
  * ``test_a_cfg_test_external_module_file_is_out_of_scope`` resolves
    ``#[cfg(test)] mod tests;`` to ``tests.rs`` and ``tests/mod.rs`` and on
    through the modules those declare.
  * ``test_a_tree_of_only_test_spawns_fails_closed`` proves the scope cannot
    swallow the whole tree and still report green.
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
_BARE_STD_SITE = """pub fn spawn_helper() {
    let _ = std::thread::spawn(|| {});
}
"""
_REGISTERING_BARE_FILE = """pub fn spawn_helper() {
    degenbot_core::worker_census::register(row());
    let _ = std::thread::spawn(|| {});
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
_FIXTURE_MANIFEST = '[package]\nname = "fixture-crate"\n'
_TEST_TARGET_FILE = "rust/crates/fixture-crate/tests/spawn_site.rs"
_BENCH_TARGET_FILE = "rust/crates/fixture-crate/benches/spawn_site.rs"
_CFG_TEST_MODULE_SITE = """pub fn production_helper() {
    let _ = std::thread::spawn(|| {});
}

#[cfg(test)]
mod tests {
    #[test]
    fn spawn_helper() {
        let _ = std::thread::spawn(|| {});
    }
}
"""
_CFG_ALL_TEST_SITE = """#[cfg(all(test, feature = "auto-initialize"))]
mod probes {
    pub fn spawn_helper() {
        let _ = std::thread::spawn(|| {});
    }
}

pub fn production_helper() {
    let _ = std::thread::spawn(|| {});
}
"""
_FEATURE_ONLY_SITE = """#[cfg(feature = "auto-initialize")]
pub mod probes {
    pub fn spawn_helper() {
        let _ = std::thread::spawn(|| {});
    }
}
"""
_CFG_TEST_NESTED_MODULES = """mod reorg;
mod helpers;

pub fn fixture_spawn() {
    let _ = std::thread::spawn(|| {});
}
"""
_CFG_TEST_EXTERNAL_DECL = """#[cfg(test)]
mod tests;

pub fn production_helper() {
    let _ = std::thread::spawn(|| {});
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


# --- the recorded two-run probe, for the bare shape the matcher grew ---------
def test_a_planted_bare_spawn_fails_and_names_it(tmp_path: Path) -> None:
    """Run one: a bare `thread::spawn` site fails, naming its file, line and row."""
    tree = _tree(tmp_path, _BARE_STD_SITE)
    result = _run(tree, _inventory(tmp_path, "# no exceptions yet\n"))
    assert result.returncode == 1, result.stdout + result.stderr
    assert f"fresh: {_SITE_FILE}:2: spawn_helper (bare std thread)" in result.stderr, result.stderr
    assert f"inventory row: {_ROW}" in result.stderr, result.stderr


def test_the_bare_probe_is_green_once_its_row_lands_or_the_function_registers(
    tmp_path: Path,
) -> None:
    """Run two: the bare site is green admitted by a row, and green registered."""
    admitted = _run(
        _tree(tmp_path / "admitted", _BARE_STD_SITE),
        _inventory(tmp_path, f"# fixture: a one-shot probe thread\n{_ROW}\n"),
    )
    assert admitted.returncode == 0, admitted.stdout + admitted.stderr
    assert "1 admitted by 1 reviewed inventory rows" in admitted.stdout, admitted.stdout
    registered = _run(
        _tree(tmp_path / "registered", _REGISTERING_BARE_FILE),
        _inventory(tmp_path, "# no exceptions yet\n"),
    )
    assert registered.returncode == 0, registered.stdout + registered.stderr
    assert "1 register in their own function" in registered.stdout, registered.stdout


# --- every shape, and the positive path --------------------------------------
def test_every_spawn_shape_is_a_site(tmp_path: Path) -> None:
    """A source with a bare spawn site fails in each of the four shapes."""
    shapes = (_BARE_SITE, _BARE_STD_SITE, _MULTI_SITE, _CURRENT_SITE)
    for index, source in enumerate(shapes):
        tree = _tree(tmp_path / f"shape-{index}", source)
        result = _run(tree, _inventory(tmp_path, "# no exceptions yet\n"))
        assert result.returncode == 1, result.stdout + result.stderr
        assert f"fresh: {_SITE_FILE}:2: spawn_helper" in result.stderr, result.stderr
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


# --- production scope: test targets and test-only modules ---------------------
def _files(tmp_path: Path, sources: dict[str, str]) -> Path:
    """A throwaway tree holding every given repo-relative path."""
    tree = tmp_path / "tree"
    for relative, source in sources.items():
        path = tree / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(source, encoding="utf-8")
    return tree


def test_the_scope_is_location_not_shape(tmp_path: Path) -> None:
    """The same spawn shape is a site under src and out of scope under tests/ and benches/."""
    tree = _files(
        tmp_path,
        {
            "rust/crates/fixture-crate/Cargo.toml": _FIXTURE_MANIFEST,
            _SITE_FILE: _BARE_STD_SITE,
            _TEST_TARGET_FILE: _BARE_STD_SITE,
            _BENCH_TARGET_FILE: _BARE_STD_SITE,
        },
    )
    result = _run(tree, _inventory(tmp_path, "# no exceptions yet\n"))
    assert result.returncode == 1, result.stdout + result.stderr
    assert result.stderr.count("  fresh: ") == 1, result.stderr
    assert f"fresh: {_SITE_FILE}:2: spawn_helper (bare std thread)" in result.stderr, result.stderr
    assert _TEST_TARGET_FILE not in result.stderr, result.stderr
    assert _BENCH_TARGET_FILE not in result.stderr, result.stderr


def test_the_summary_counts_the_sources_out_of_scope(tmp_path: Path) -> None:
    """Green with a production registration and one test target: the summary names the scope."""
    tree = _files(
        tmp_path,
        {
            "rust/crates/fixture-crate/Cargo.toml": _FIXTURE_MANIFEST,
            _SITE_FILE: _REGISTERING_BARE_FILE,
            _TEST_TARGET_FILE: _BARE_STD_SITE,
        },
    )
    result = _run(tree, _inventory(tmp_path, "# no exceptions yet\n"))
    assert result.returncode == 0, result.stdout + result.stderr
    assert "1 test-only sources out of scope" in result.stdout, result.stdout
    assert "1 register in their own function" in result.stdout, result.stdout


def test_a_cfg_test_module_region_is_out_of_scope(tmp_path: Path) -> None:
    """A spawn inside an inline `#[cfg(test)] mod` is not a site; its production sibling is."""
    tree = _tree(tmp_path, _CFG_TEST_MODULE_SITE)
    result = _run(tree, _inventory(tmp_path, "# no exceptions yet\n"))
    assert result.returncode == 1, result.stdout + result.stderr
    assert result.stderr.count("  fresh: ") == 1, result.stderr
    assert f"fresh: {_SITE_FILE}:2: production_helper" in result.stderr, result.stderr
    assert "spawn_helper" not in result.stderr, result.stderr


def test_a_cfg_all_test_feature_region_is_out_of_scope(tmp_path: Path) -> None:
    """`cfg(all(test, feature = ...))` is a test predicate too; only the production site fails."""
    tree = _tree(tmp_path, _CFG_ALL_TEST_SITE)
    result = _run(tree, _inventory(tmp_path, "# no exceptions yet\n"))
    assert result.returncode == 1, result.stdout + result.stderr
    assert result.stderr.count("  fresh: ") == 1, result.stderr
    assert f"inventory row: {_SITE_FILE}:production_helper" in result.stderr, result.stderr
    assert "spawn_helper" not in result.stderr, result.stderr


def test_a_feature_only_module_stays_a_production_site(tmp_path: Path) -> None:
    """`cfg(feature = "auto-initialize")` names no test predicate, so its spawn is a site."""
    tree = _tree(tmp_path, _FEATURE_ONLY_SITE)
    result = _run(tree, _inventory(tmp_path, "# no exceptions yet\n"))
    assert result.returncode == 1, result.stdout + result.stderr
    assert result.stderr.count("  fresh: ") == 1, result.stderr
    assert f"inventory row: {_SITE_FILE}:spawn_helper" in result.stderr, result.stderr


def test_a_cfg_test_external_module_file_is_out_of_scope(tmp_path: Path) -> None:
    """`#[cfg(test)] mod tests;` resolves its file (sibling and `mod.rs` forms) as test-only."""
    tree = _files(
        tmp_path,
        {
            "rust/crates/fixture-crate/src/spawn_site.rs": _CFG_TEST_EXTERNAL_DECL,
            "rust/crates/fixture-crate/src/spawn_site/tests.rs": _CFG_TEST_NESTED_MODULES,
            "rust/crates/fixture-crate/src/spawn_site/tests/reorg.rs": _BARE_STD_SITE,
            "rust/crates/fixture-crate/src/spawn_site/tests/helpers/mod.rs": _BARE_STD_SITE,
        },
    )
    result = _run(tree, _inventory(tmp_path, "# no exceptions yet\n"))
    assert result.returncode == 1, result.stdout + result.stderr
    assert result.stderr.count("  fresh: ") == 1, result.stderr
    assert f"inventory row: {_SITE_FILE}:production_helper" in result.stderr, result.stderr
    assert "fixture_spawn" not in result.stderr, result.stderr
    assert "tests.rs" not in result.stderr, result.stderr


def test_a_tree_of_only_test_spawns_fails_closed(tmp_path: Path) -> None:
    """A scope that excluded every spawn site refuses to certify instead of reporting green."""
    tree = _files(
        tmp_path,
        {
            "rust/crates/fixture-crate/Cargo.toml": _FIXTURE_MANIFEST,
            _TEST_TARGET_FILE: _BARE_STD_SITE,
        },
    )
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
