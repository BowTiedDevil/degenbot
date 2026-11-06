"""Negative probes for the parity-ledger single-source gate.

``scripts/hooks/parity_ledger_single_source.py`` reconciles the settlement-bot
parity ledger's prose against the boot gate's oracle
(``tests/standalone_parity/fixtures/settlement_bot_boot.json``
``expected.ledger_rows``). Every case here drives the real gate over throwaway
copies, so no assertion can be satisfied by a re-typed copy of the gate's logic
(the GLOSSARY's negative-probe idiom applied to a consistency gate):

  * ``test_the_committed_prose_agrees_with_the_committed_oracle`` -- the real
    gate over the real files.
  * ``test_a_doc_status_edit_fails_and_names_the_row`` -- the red path: editing
    ONE place leaves the others disagreeing, and the gate names the row.
  * ``test_an_oracle_status_edit_fails_and_names_the_row`` -- the other
    direction: the oracle moved, the prose did not.
  * ``test_a_pinned_status_edit_fails`` / ``test_a_pinned_key_the_oracle_lacks_fails``
    -- the pinned decision rows table, status and key.
  * ``test_a_delta_row_that_drops_its_status_fails`` -- the designation deltas
    table must state the keyed row's oracle status.
  * ``test_a_status_restated_outside_the_tables_fails`` -- the duplication rule
    as a check: a status beside a row reference in a fourth place fails.
  * ``test_a_ledger_row_the_doc_omits_fails`` / ``test_an_oracle_row_the_prose_contradicts_fails``
    -- the two row-set directions.
  * ``test_a_missing_doc_fails_closed``, ``test_an_oracle_without_ledger_rows_fails_closed``
    and ``test_a_non_contiguous_oracle_series_fails_closed`` -- the fail-closed
    edges, so a blind extraction can never report a green.
  * ``test_the_gate_is_registered_where_it_runs`` -- the wiring the gate needs
    to run at all: the justfile recipe it names and the pre-commit entry.
"""

from __future__ import annotations

import json
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import sys
import tomllib
from pathlib import Path

_REPO_ROOT = Path(__file__).resolve().parents[1]
_GATE = _REPO_ROOT / "scripts" / "hooks" / "parity_ledger_single_source.py"
_DOC = _REPO_ROOT / "docs" / "architecture" / "rust-settlement-bot-parity.md"
_ORACLE = _REPO_ROOT / "tests" / "standalone_parity" / "fixtures" / "settlement_bot_boot.json"
_PREK = _REPO_ROOT / "prek.toml"
_JUSTFILE = _REPO_ROOT / "justfile"
_HOOK_ID = "parity-ledger-single-source"
_RECIPE = "check-parity-ledger-single-source"


def _oracle_rows() -> dict[str, str]:
    return json.loads(_ORACLE.read_text(encoding="utf-8"))["expected"]["ledger_rows"]


def _run(doc: Path | None = None, oracle: Path | None = None) -> subprocess.CompletedProcess[str]:
    argv = [sys.executable, str(_GATE)]
    if doc is not None:
        argv += ["--doc", str(doc)]
    if oracle is not None:
        argv += ["--oracle", str(oracle)]
    return subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true]
        argv, cwd=_REPO_ROOT, check=False, capture_output=True, text=True
    )


def _mutated(tmp_path: Path, source: Path, old: str, new: str) -> Path:
    """Write `source` with its first `old` replaced by `new`; return the copy."""
    text = source.read_text(encoding="utf-8")
    assert old in text, f"the mutation anchor is gone from {source}: {old!r}"
    assert old != new, "a mutation must change something"
    copy = tmp_path / source.name
    copy.write_text(text.replace(old, new, 1), encoding="utf-8")
    return copy


def _without_line(tmp_path: Path, source: Path, prefix: str) -> Path:
    """Write `source` without its first line starting with `prefix`."""
    lines = source.read_text(encoding="utf-8").splitlines(keepends=True)
    kept = [line for line in lines if not line.startswith(prefix)]
    assert len(kept) == len(lines) - 1, f"expected exactly one line starting with {prefix!r}"
    copy = tmp_path / source.name
    copy.write_text("".join(kept), encoding="utf-8")
    return copy


def _rewritten_oracle(tmp_path: Path, mutate) -> Path:
    """Write the oracle with `mutate` applied to a parsed copy."""
    payload = json.loads(_ORACLE.read_text(encoding="utf-8"))
    mutate(payload["expected"]["ledger_rows"])
    copy = tmp_path / _ORACLE.name
    copy.write_text(json.dumps(payload), encoding="utf-8")
    return copy


# --- the committed tree ------------------------------------------------------


def test_the_committed_prose_agrees_with_the_committed_oracle() -> None:
    result = _run()

    assert result.returncode == 0, result.stdout + result.stderr
    assert "restatements: none outside the three tables" in result.stdout, result.stdout
    assert "6 keys agree" in result.stdout, result.stdout


# --- the red path: one side edited, the others disagree ----------------------


def test_a_doc_status_edit_fails_and_names_the_row(tmp_path: Path) -> None:
    rows = _oracle_rows()
    replacement = rows["11-discovery-db-enumeration"]
    assert replacement != rows["13-path-policy"], "the probe needs two distinct statuses"
    doc = _mutated(
        tmp_path,
        _DOC,
        "| DRIVER-POLICY | example-implemented",
        f"| {replacement} | example-implemented",
    )

    result = _run(doc=doc)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "13-path-policy" in result.stderr, result.stderr
    assert "the ledger table states" in result.stderr, result.stderr


def test_an_oracle_status_edit_fails_and_names_the_row(tmp_path: Path) -> None:
    rows = _oracle_rows()
    replacement = rows["13-path-policy"]
    oracle = _rewritten_oracle(
        tmp_path,
        lambda ledger: ledger.__setitem__("06-engine-subscribe-resume", replacement),
    )

    result = _run(oracle=oracle)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "06-engine-subscribe-resume" in result.stderr, result.stderr


def test_a_pinned_status_edit_fails(tmp_path: Path) -> None:
    replacement = _oracle_rows()["13-path-policy"]
    doc = _mutated(
        tmp_path,
        _DOC,
        "| `15-dispatch-selection` | `REACHABLE` |",
        f"| `15-dispatch-selection` | `{replacement}` |",
    )

    result = _run(doc=doc)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "the pinned table pins" in result.stderr, result.stderr
    assert "15-dispatch-selection" in result.stderr, result.stderr


def test_a_pinned_key_the_oracle_lacks_fails(tmp_path: Path) -> None:
    doc = _mutated(
        tmp_path,
        _DOC,
        "| `15-dispatch-selection` | `REACHABLE` |",
        "| `15-dispatch-selection-typo` | `REACHABLE` |",
    )

    result = _run(doc=doc)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "which the oracle lacks" in result.stderr, result.stderr


def test_a_delta_row_that_drops_its_status_fails(tmp_path: Path) -> None:
    doc = _mutated(
        tmp_path,
        _DOC,
        "| `REACHABLE` — **LIFT landed** "
        "(`degenbot-submission::sim_pipeline`; **S9/S11**); cap value stays driver-side |",
        "| **LIFT landed** "
        "(`degenbot-submission::sim_pipeline`; **S9/S11**); cap value stays driver-side |",
    )

    result = _run(doc=doc)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "the After cell does not state" in result.stderr, result.stderr


# --- the duplication rule, as a check ---------------------------------------


def test_a_status_restated_outside_the_tables_fails(tmp_path: Path) -> None:
    status = _oracle_rows()["13-path-policy"]
    text = _DOC.read_text(encoding="utf-8")
    doc = tmp_path / _DOC.name
    restatement = f"The `13-path-policy` row keeps its {status} status."
    doc.write_text(f"{text}\n{restatement}\n", encoding="utf-8")

    result = _run(doc=doc)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "beside a row reference" in result.stderr, result.stderr


# --- the row-set directions -------------------------------------------------


def test_a_ledger_row_the_doc_omits_fails(tmp_path: Path) -> None:
    doc = _without_line(tmp_path, _DOC, "| 13 |")

    result = _run(doc=doc)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "13-path-policy: the ledger table has no row 13" in result.stderr, result.stderr


def test_an_oracle_row_the_prose_contradicts_fails(tmp_path: Path) -> None:
    status = _oracle_rows()["01-cli"]
    oracle = _rewritten_oracle(tmp_path, lambda ledger: ledger.__setitem__("21-extra-row", status))

    result = _run(oracle=oracle)

    assert result.returncode == 1, result.stdout + result.stderr
    assert "21-extra-row" in result.stderr, result.stderr


# --- fail-closed edges ------------------------------------------------------


def test_a_missing_doc_fails_closed(tmp_path: Path) -> None:
    result = _run(doc=tmp_path / "no-such-ledger.md")

    assert result.returncode == 2, result.stdout + result.stderr
    assert "refusing to pass" in result.stderr, result.stderr


def test_an_oracle_without_ledger_rows_fails_closed(tmp_path: Path) -> None:
    empty = tmp_path / _ORACLE.name
    empty.write_text(json.dumps({"expected": {}}), encoding="utf-8")

    result = _run(oracle=empty)

    assert result.returncode == 2, result.stdout + result.stderr
    assert "carries no expected.ledger_rows" in result.stderr, result.stderr


def test_a_non_contiguous_oracle_series_fails_closed(tmp_path: Path) -> None:
    oracle = _rewritten_oracle(tmp_path, lambda ledger: ledger.pop("13-path-policy"))

    result = _run(oracle=oracle)

    assert result.returncode == 2, result.stdout + result.stderr
    assert "contiguous" in result.stderr, result.stderr


# --- the wiring the gate depends on ----------------------------------------


def test_the_gate_is_registered_where_it_runs() -> None:
    """A gate nothing invokes is prose; the recipe and the hook are the wiring."""
    parsed = tomllib.loads(_PREK.read_text(encoding="utf-8"))
    hooks = {hook["id"]: hook for repo in parsed["repos"] for hook in repo.get("hooks", [])}

    assert _HOOK_ID in hooks, "the gate has no prek entry"
    hook = hooks[_HOOK_ID]
    assert hook["entry"] == f"just {_RECIPE}", hook["entry"]
    assert hook["stages"] == ["pre-commit"], hook["stages"]
    assert hook["pass_filenames"] is False, "the reconciliation needs whole files"
    assert _RECIPE in _JUSTFILE.read_text(encoding="utf-8"), "the recipe the hook names is gone"
