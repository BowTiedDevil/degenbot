"""The feature-gated test lane (`scripts/feature_lane_inventory.py`).

The gate's contract is two-sided and mechanical:

* the inventory is derived from cargo metadata plus a module walk, never from a
  hand list (``test_the_committed_inventory_reconciles_with_the_live_tree``);
* the nextest filter is generated from the dispositions, so the recipe cannot
  run a selection the gate did not reconcile (``test_the_generated_filter_...``);
* a capture must select every offline unit and no excluded one, and the
  UNFILTERED capture must FAIL the same reconciliation -- the negative probe
  (``test_the_negative_probe_must_fail``);
* the fail-closed directions: an ignored item without an ``ignored`` row, a row
  for a unit the tree no longer has, a live-gated binary with an unnamed item,
  and a gate on an unmodeled predicate all refuse or fail.

Synthetic cases drive a throwaway workspace laid out like ``rust/`` plus a
metadata dump (so cargo never runs) and hand-written nextest captures; the
live-tree cases drive the committed workspace.
"""

from __future__ import annotations

import json
import shutil
from typing import TYPE_CHECKING

import pytest

from tests.repo_script import load_script

if TYPE_CHECKING:
    from pathlib import Path

_REPO_ROOT = __import__("pathlib").Path(__file__).resolve().parents[1]
_SCRIPT = _REPO_ROOT / "scripts" / "feature_lane_inventory.py"
_FIXTURES = _REPO_ROOT / "tests" / "fixtures" / "feature_lane"

# The lane's feature set: the canonical fixed features the nextest lane carries
# plus the observability family this lane adds. The justfile recipe and ci.yml
# pass the same list.
LANE_FEATURES = (
    "degenbot-pools/capture-fixtures,"
    "degenbot-pools/cache-lab,"
    "degenbot-bot/executor-ab-probe,"
    "degenbot-simulation/test-harness,"
    "degenbot-bot/otel,"
    "degenbot_rs/otel,"
    "degenbot-arbitrage/otel-replay"
)
_FIXTURE_FEATURES = "demo/obs"


@pytest.fixture
def lane_module():
    """Load the script as a module the way running it would."""
    return load_script("feature_lane_inventory", _SCRIPT)


@pytest.fixture
def fixture_workspace(tmp_path: Path) -> tuple[Path, Path]:
    """Copy the synthetic crate under a ``rust/`` root and render metadata."""
    root = tmp_path
    crate = root / "rust" / "crate"
    shutil.copytree(_FIXTURES / "crate", crate)
    metadata = (_FIXTURES / "metadata.json").read_text(encoding="utf-8")
    metadata_path = root / "metadata.json"
    metadata_path.write_text(metadata.replace("__CRATE_DIR__", str(crate)), encoding="utf-8")
    dispositions = root / "dispositions.txt"
    shutil.copyfile(_FIXTURES / "dispositions.txt", dispositions)
    return root, dispositions


def _run(module, argv: list[str], capsys) -> tuple[int, str, str]:
    exit_code = module.main(argv)
    captured = capsys.readouterr()
    return exit_code, captured.out, captured.err


def _args(root: Path, dispositions: Path, *rest: str) -> list[str]:
    return [
        "--root",
        str(root),
        "--metadata",
        str(root / "metadata.json"),
        "--dispositions",
        str(dispositions),
        "--features",
        _FIXTURE_FEATURES,
        *rest,
    ]


# --- the committed tree ------------------------------------------------------


def test_the_committed_inventory_reconciles_with_the_live_tree(lane_module, capsys):
    exit_code, out, err = _run(
        lane_module,
        ["--features", LANE_FEATURES, "check"],
        capsys,
    )
    assert exit_code == 0, err
    assert "in-lane units reconciled" in out
    assert "out of lane" in out


def test_the_generated_filter_excludes_the_live_probe_and_the_abort_repro(lane_module, capsys):
    exit_code, out, err = _run(lane_module, ["--features", LANE_FEATURES, "filter"], capsys)
    assert exit_code == 0, err
    expression = out.strip()
    assert expression.startswith("not (")
    assert (
        "(binary_id(=degenbot-bot::otel_jaeger_e2e) and "
        "test(=span_reaches_real_jaeger_from_a_current_thread_runtime))"
    ) in expression
    assert (
        "(binary_id(=degenbot-bot::otel_jaeger_e2e) and "
        "test(=span_reaches_real_jaeger_from_a_multi_thread_runtime))"
    ) in expression
    # The pure formatter helper in the live-gated binary stays in the lane.
    assert "rfc3339_utc_formats_known_instants" not in expression
    # Wildcard exclusions use the exact binary id, never a bare target name or
    # a global test name.
    assert "binary(otel_jaeger_e2e)" not in expression
    assert "binary_id(=degenbot-bot::otel_tls_repro)" in expression


# --- the synthetic workspace ------------------------------------------------


def test_a_fixture_workspace_reconciles_against_its_dispositions(
    lane_module,
    fixture_workspace,
    capsys,
):
    root, dispositions = fixture_workspace
    exit_code, out, err = _run(lane_module, _args(root, dispositions, "filter"), capsys)
    assert exit_code == 0, err
    assert out.strip() == (
        "not (binary_id(=demo::abort_repro) "
        "or (binary_id(=demo) and test(=gated::ignored_item)) "
        "or (binary_id(=demo) and test(=gated::timing_probe)) "
        "or (binary_id(=demo::live_probe) and test(=shared_measurement_name)) "
        "or (binary_id(=demo::live_probe) and test(=span_reaches_real_service)))"
    )


def test_the_filter_scopes_every_exclusion_to_its_binary(lane_module, fixture_workspace, capsys):
    root, dispositions = fixture_workspace
    exit_code, out, err = _run(lane_module, _args(root, dispositions, "filter"), capsys)
    assert exit_code == 0, err
    expression = out.strip()
    # The live probe's name is filtered inside its own binary only; the offline
    # binary that reuses the name must stay selected.
    assert "(binary_id(=demo::live_probe) and test(=shared_measurement_name))" in expression
    assert "binary_id(=demo::offline_twin)" not in expression
    assert "or test(=shared_measurement_name)" not in expression
    assert "not (test(" not in expression


def test_a_measurement_unit_is_classified_and_excluded(lane_module, fixture_workspace, capsys):
    root, dispositions = fixture_workspace
    exit_code, out, err = _run(lane_module, _args(root, dispositions, "inventory"), capsys)
    assert exit_code == 0, err
    assert "measurement\tdemo\tgated::timing_probe\tobs" in out


def test_an_ignored_item_without_an_ignored_disposition_fails(
    lane_module,
    fixture_workspace,
    capsys,
):
    root, dispositions = fixture_workspace
    lines = [
        line
        for line in dispositions.read_text(encoding="utf-8").splitlines()
        if "gated::ignored_item" not in line
    ]
    dispositions.write_text("\n".join(lines) + "\n", encoding="utf-8")
    exit_code, _, err = _run(lane_module, _args(root, dispositions, "check"), capsys)
    assert exit_code == 1
    assert "mechanically #[ignore]d but classified 'offline'" in err


def test_a_disposition_for_a_vanished_unit_fails(lane_module, fixture_workspace, capsys):
    root, dispositions = fixture_workspace
    with dispositions.open("a", encoding="utf-8") as handle:
        handle.write("demo|gated::retired_item|offline|ADR-043|gone\n")
    exit_code, _, err = _run(lane_module, _args(root, dispositions, "check"), capsys)
    assert exit_code == 1
    assert "disposition names unknown unit" in err


def test_a_live_gated_binary_must_name_every_item(lane_module, fixture_workspace, capsys):
    root, dispositions = fixture_workspace
    lines = [
        line
        for line in dispositions.read_text(encoding="utf-8").splitlines()
        if "pure_helper" not in line
    ]
    dispositions.write_text("\n".join(lines) + "\n", encoding="utf-8")
    exit_code, _, err = _run(lane_module, _args(root, dispositions, "check"), capsys)
    assert exit_code == 1
    assert "every item needs an explicit disposition" in err
    assert "pure_helper" in err


def test_a_positive_unmodeled_gate_predicate_refuses_to_certify(
    lane_module,
    fixture_workspace,
    capsys,
):
    root, dispositions = fixture_workspace
    lib = root / "rust" / "crate" / "src" / "lib.rs"
    # A POSITIVE unknown predicate evaluates false, so without the check a unit
    # would silently leave the lane instead of failing the gate.
    lib.write_text(
        lib.read_text(encoding="utf-8")
        + '\n#[cfg(all(test, feature = "obs", has_std))]\n'
        + "mod positive_probe {\n    #[test]\n    fn decided_elsewhere() {}\n}\n",
        encoding="utf-8",
    )
    exit_code, _, err = _run(lane_module, _args(root, dispositions, "check"), capsys)
    assert exit_code == 1
    assert "unmodeled predicate" in err
    assert "has_std" in err


def test_an_unmodeled_gate_predicate_refuses_to_certify(lane_module, fixture_workspace, capsys):
    root, dispositions = fixture_workspace
    lib = root / "rust" / "crate" / "src" / "lib.rs"
    lib.write_text(
        lib.read_text(encoding="utf-8")
        + '\n#[cfg(all(test, feature = "obs", not(has_std)))]\n'
        + "mod mysterious {\n    #[test]\n    fn decided_elsewhere() {}\n}\n",
        encoding="utf-8",
    )
    exit_code, _, err = _run(lane_module, _args(root, dispositions, "check"), capsys)
    assert exit_code == 1
    assert "unmodeled predicate" in err
    assert "has_std" in err


def test_a_malformed_disposition_line_refuses(lane_module, fixture_workspace, capsys):
    root, dispositions = fixture_workspace
    with dispositions.open("a", encoding="utf-8") as handle:
        handle.write("demo|gated::gated_item|offline\n")
    exit_code, _, err = _run(lane_module, _args(root, dispositions, "check"), capsys)
    assert exit_code == 2
    assert "expected binary|item|class|ADR|rationale" in err


def test_an_unknown_class_refuses(lane_module, fixture_workspace, capsys):
    root, dispositions = fixture_workspace
    dispositions.write_text("demo|gated::gated_item|flaky|ADR-043|nope\n", encoding="utf-8")
    exit_code, _, err = _run(lane_module, _args(root, dispositions, "check"), capsys)
    assert exit_code == 2
    assert "unknown class" in err


# --- the two-sided capture reconciliation -----------------------------------


def test_verify_selects_every_offline_unit_and_excludes_the_rest(
    lane_module,
    fixture_workspace,
    capsys,
):
    root, dispositions = fixture_workspace
    exit_code, out, err = _run(
        lane_module,
        _args(
            root,
            dispositions,
            "verify",
            "--list",
            str(_FIXTURES / "capture-filtered.json"),
            "--negative-probe",
            str(_FIXTURES / "capture-unfiltered.json"),
        ),
        capsys,
    )
    assert exit_code == 0, err
    assert "negative probe failed as required" in out


def test_verify_fails_when_an_excluded_unit_is_selected(
    lane_module,
    fixture_workspace,
    capsys,
):
    root, dispositions = fixture_workspace
    exit_code, _, err = _run(
        lane_module,
        _args(root, dispositions, "verify", "--list", str(_FIXTURES / "capture-unfiltered.json")),
        capsys,
    )
    assert exit_code == 1
    assert "excluded unit is selected" in err


def test_the_negative_probe_must_fail(lane_module, fixture_workspace, capsys):
    root, dispositions = fixture_workspace
    # A probe that never proves selection (every excluded case present but
    # already mismatched) reconciles clean, so the probe itself must fail.
    probe = json.loads((_FIXTURES / "capture-filtered.json").read_text(encoding="utf-8"))
    probe["rust-suites"]["demo::abort_repro"] = {
        "package-name": "demo",
        "binary-id": "demo::abort_repro",
        "kind": "test",
        "status": "listed",
        "testcases": {
            "aborts_by_design": {
                "kind": "test",
                "ignored": True,
                "filter-match": {"status": "mismatch", "reason": "default-filter"},
            },
            "child_leaks_guard": {
                "kind": "test",
                "ignored": False,
                "filter-match": {"status": "mismatch", "reason": "expression"},
            },
        },
    }
    probe_path = root / "probe.json"
    probe_path.write_text(json.dumps(probe), encoding="utf-8")
    exit_code, _, err = _run(
        lane_module,
        _args(
            root,
            dispositions,
            "verify",
            "--list",
            str(_FIXTURES / "capture-filtered.json"),
            "--negative-probe",
            str(probe_path),
        ),
        capsys,
    )
    assert exit_code == 1
    assert "also reconciles clean" in err


def test_verify_fails_when_an_offline_unit_is_not_selected(
    lane_module,
    fixture_workspace,
    capsys,
):
    root, dispositions = fixture_workspace
    capture = json.loads((_FIXTURES / "capture-filtered.json").read_text(encoding="utf-8"))
    capture["rust-suites"]["demo"]["testcases"]["gated::gated_item"]["filter-match"]["status"] = (
        "mismatch"
    )
    mutated = root / "capture.json"
    mutated.write_text(json.dumps(capture), encoding="utf-8")
    exit_code, _, err = _run(
        lane_module,
        _args(root, dispositions, "verify", "--list", str(mutated)),
        capsys,
    )
    assert exit_code == 1
    assert "offline unit is not selected: demo::gated::gated_item" in err


# --- wiring -----------------------------------------------------------------


def test_the_lane_is_wired_into_the_justfile_and_ci() -> None:
    justfile = (_REPO_ROOT / "justfile").read_text(encoding="utf-8")
    ci = (_REPO_ROOT / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")
    assert "test-rust-feature-lanes" in justfile
    assert "just test-rust-feature-lanes" in ci
    assert "just check-dev-tools fixture-sql-ledger" in ci
