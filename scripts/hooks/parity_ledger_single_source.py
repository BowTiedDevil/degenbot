#!/usr/bin/env python3
"""Single-source consistency gate for the settlement-bot parity ledger.

The ledger's row statuses have exactly one machine-checked home:
``expected.ledger_rows`` in
``tests/standalone_parity/fixtures/settlement_bot_boot.json``, compared against
the boot report the parity example actually prints by
``rust/examples/settlement_bot/tests/boot_gate.rs``. The prose in
``docs/architecture/rust-settlement-bot-parity.md`` repeats those facts in three
places, and this gate reconciles every one of them against that oracle:

1. the ledger table -- one row per status, matched to the oracle's ``NN-slug``
   key by its number;
2. the fixture boot gate's pinned decision rows table -- keyed by the oracle
   key, with the pinned status in its own cell;
3. the ledger designation deltas table -- keyed by the oracle key, whose oracle
   status has to appear in the ``After`` cell.

Everywhere else a ledger row may be named by key or number, never by a status:
the fourth check fails a line that pairs a row reference with one of the
oracle's status values outside those three tables. That is the document's
duplication rule as a check rather than a habit -- a repeated fact is allowed
exactly where a gate compares it in the same run.

Both directions are checked: an oracle row the prose drops, a prose row in the
oracle's series the oracle dropped, and a status that disagrees in either file
all fail. Editing a status is therefore a two-file change whose disagreement
the gate reports by row.

Exit codes: 0 clean, 1 drift (each complaint on stderr), 2 the gate examined
nothing (a missing or unparseable side fails closed). ``--doc``/``--oracle``
point the gate at throwaway copies; the negative-probe suite
(``tests/test_parity_ledger_single_source.py``) is the caller that uses them.

live-invariant gate (ADR-087 D2): watches the live parity ledger prose against the boot-report oracle's ledger rows; not tripwire precedent.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Sequence

_REPO_ROOT = Path(__file__).resolve().parents[2]

_DEFAULT_DOC = "docs/architecture/rust-settlement-bot-parity.md"
_DEFAULT_ORACLE = "tests/standalone_parity/fixtures/settlement_bot_boot.json"

_LEDGER_HEADING = "## Ledger"
_PINNED_HEADER = ("Row", "Pinned status", "Decision contract")
_DELTA_HEADER = ("Ledger row", "Before", "After")

_NUMBERED_KEY = re.compile(r"^(\d+)-")
_ROW_REFERENCE = re.compile(r"\brows?\s+(\d+)(?:\s*[-\u2013]\s*(\d+))?", re.IGNORECASE)
_BACKTICKED = re.compile(r"`([^`]+)`")
_SEPARATOR_CELL = re.compile(r"^:?-+:?$")


class ExtractionError(Exception):
    """An extraction found nothing usable: fail closed instead of passing."""


def _resolve(path: str) -> Path:
    candidate = Path(path)
    return candidate if candidate.is_absolute() else _REPO_ROOT / candidate


def _display(path: Path) -> str:
    try:
        return str(path.relative_to(_REPO_ROOT))
    except ValueError:
        return str(path)


def _cells(line: str) -> list[str] | None:
    """The cell texts of a markdown table line, or None when it is not one."""
    stripped = line.strip()
    if not stripped.startswith("|") or not stripped.endswith("|"):
        return None
    return [cell.strip() for cell in stripped[1:-1].split("|")]


def _status(cell: str) -> str:
    """A status cell's value: emphasis and code-span markers stripped."""
    return cell.replace("`", "").strip("*_ ").strip()


def _doc_lines(doc_path: Path) -> list[str]:
    try:
        return doc_path.read_text(encoding="utf-8").splitlines()
    except OSError as error:
        raise ExtractionError(f"unreadable document {_display(doc_path)}: {error}") from error


def _oracle_rows(oracle_path: Path) -> dict[str, str]:
    try:
        payload = json.loads(oracle_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ExtractionError(f"unreadable oracle {_display(oracle_path)}: {error}") from error
    rows = payload.get("expected", {}).get("ledger_rows")
    if not isinstance(rows, dict) or not rows:
        raise ExtractionError(f"{_display(oracle_path)} carries no expected.ledger_rows")
    for key, value in rows.items():
        if not isinstance(value, str) or not _status(value):
            raise ExtractionError(f"{_display(oracle_path)}: ledger_rows[{key}] is not a status")
    return rows


def _numbered_keys(oracle: dict[str, str]) -> dict[int, str]:
    """The oracle's keys as number -> key, requiring a contiguous 01..NN series."""
    numbers: dict[int, str] = {}
    for key in oracle:
        match = _NUMBERED_KEY.match(key)
        if match is None:
            raise ExtractionError(f"oracle key {key!r} carries no NN- prefix")
        number = int(match.group(1))
        if number in numbers:
            raise ExtractionError(f"oracle keys {numbers[number]!r} and {key!r} share a number")
        numbers[number] = key
    if sorted(numbers) != list(range(1, len(numbers) + 1)):
        raise ExtractionError("the oracle's keys are not a contiguous 01..NN series")
    return numbers


def _rows_after(lines: Sequence[str], header_index: int, table_name: str):
    separator = _cells(lines[header_index + 1]) if header_index + 1 < len(lines) else None
    if separator is None or not all(_SEPARATOR_CELL.match(cell) for cell in separator):
        raise ExtractionError(f"the {table_name} header is not followed by a separator row")
    rows: list[list[str]] = []
    last = header_index + 1
    for offset in range(header_index + 2, len(lines)):
        cells = _cells(lines[offset])
        if cells is None:
            break
        rows.append(cells)
        last = offset
    if not rows:
        raise ExtractionError(f"the {table_name} holds no rows")
    return (header_index, rows, last)


def _table_by_header(lines: Sequence[str], header: tuple[str, ...], table_name: str):
    for index, line in enumerate(lines):
        cells = _cells(line)
        if cells is not None and tuple(cells) == header:
            return _rows_after(lines, index, table_name)
    raise ExtractionError(f"no {table_name} with header {' | '.join(header)}")


def _ledger_table(lines: Sequence[str]):
    """The first table after the ledger heading -- the document's own row list."""
    try:
        heading = list(lines).index(_LEDGER_HEADING)
    except ValueError as error:
        raise ExtractionError(f"no {_LEDGER_HEADING!r} heading in the document") from error
    for index in range(heading + 1, len(lines)):
        if _cells(lines[index]) is not None:
            return _rows_after(lines, index, "ledger table")
    raise ExtractionError("no table follows the ledger heading")


def _keyed(cell: str) -> str:
    """The oracle key a cell names, or the empty string when it names none."""
    match = _BACKTICKED.search(cell)
    return match.group(1) if match is not None else ""


def _check_ledger(rows: Sequence[Sequence[str]], oracle: dict[str, str], numbers: dict[int, str]):
    """The document's ledger table against the oracle, both directions."""
    complaints: list[str] = []
    stated: dict[int, str] = {}
    for cells in rows:
        if len(cells) < 4 or not cells[0].isdigit():
            complaints.append(f"ledger row {cells[0]!r} is not a numbered four-cell row")
            continue
        stated[int(cells[0])] = _status(cells[2])
    for number, key in sorted(numbers.items()):
        expected = oracle[key]
        if number not in stated:
            complaints.append(f"{key}: the ledger table has no row {number}")
        elif stated[number] != expected:
            complaints.append(
                f"{key}: the ledger table states {stated[number]!r}, the oracle states {expected!r}"
            )
    return complaints


def _check_pinned(rows: Sequence[Sequence[str]], oracle: dict[str, str]):
    """The pinned decision rows table against the oracle, by key."""
    complaints: list[str] = []
    for cells in rows:
        if len(cells) < 2:
            complaints.append(f"pinned row {cells!r} is not a three-cell row")
            continue
        key = _keyed(cells[0])
        if key not in oracle:
            complaints.append(f"the pinned table names {key or cells[0]!r}, which the oracle lacks")
            continue
        if _status(cells[1]) != oracle[key]:
            complaints.append(
                f"{key}: the pinned table pins {_status(cells[1])!r}, the oracle states "
                f"{oracle[key]!r}"
            )
    return complaints


def _check_deltas(rows: Sequence[Sequence[str]], oracle: dict[str, str]):
    """The designation deltas table: each keyed row states its oracle status."""
    complaints: list[str] = []
    for cells in rows:
        if len(cells) < 3:
            complaints.append(f"delta row {cells!r} is not a three-cell row")
            continue
        key = _keyed(cells[0])
        if key not in oracle:
            complaints.append(f"the deltas table names {key or cells[0]!r}, which the oracle lacks")
            continue
        if oracle[key] not in _BACKTICKED.findall(cells[2]):
            complaints.append(
                f"{key}: the After cell does not state the oracle's {oracle[key]!r} "
                f"(it states {_BACKTICKED.findall(cells[2]) or 'none'})"
            )
    return complaints


def _covered(*tables) -> set[int]:
    """The 1-based line numbers the checked tables own."""
    covered: set[int] = set()
    for header_index, _rows, last in tables:
        covered.update(range(header_index + 1, last + 2))
    return covered


def _status_pattern(oracle: dict[str, str]) -> re.Pattern[str]:
    tokens = sorted(set(oracle.values()), key=len, reverse=True)
    return re.compile(
        "|".join(rf"(?<![A-Za-z0-9_-]){re.escape(token)}(?![A-Za-z0-9_-])" for token in tokens)
    )


def _check_restatements(lines: Sequence[str], covered: set[int], oracle: dict[str, str]):
    """Any status restated beside a row reference outside the three tables.

    A table line is read cell by cell, so a sweep decision that happens to
    share a status word with a ledger status (a row whose rationale mentions a
    ledger row elsewhere in the same row) stays clean: what the rule forbids is
    a status and a row reference in the SAME statement.
    """
    keys = set(oracle)
    pattern = _status_pattern(oracle)
    complaints: list[str] = []
    for number, line in enumerate(lines, start=1):
        if number in covered:
            continue
        cells = _cells(line)
        for segment in cells if cells is not None else [line]:
            names_a_row = any(token in keys for token in _BACKTICKED.findall(segment))
            if not names_a_row and not _ROW_REFERENCE.search(segment):
                continue
            hit = pattern.search(segment)
            if hit is not None:
                complaints.append(
                    f"line {number}: restates {hit.group(0)!r} beside a row reference; a status "
                    f"belongs only in the three gate-checked places"
                )
    return complaints


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0] if __doc__ else None)
    parser.add_argument("--doc", default=_DEFAULT_DOC)
    parser.add_argument("--oracle", default=_DEFAULT_ORACLE)
    args = parser.parse_args(argv)

    doc_path = _resolve(args.doc)
    oracle_path = _resolve(args.oracle)
    try:
        lines = _doc_lines(doc_path)
        oracle = _oracle_rows(oracle_path)
        numbers = _numbered_keys(oracle)
        ledger = _ledger_table(lines)
        pinned = _table_by_header(lines, _PINNED_HEADER, "pinned decision rows table")
        deltas = _table_by_header(lines, _DELTA_HEADER, "designation deltas table")
    except ExtractionError as error:
        print(f"parity-ledger single source: {error}", file=sys.stderr)
        print(
            "parity-ledger single source: refusing to pass over an extraction that found nothing",
            file=sys.stderr,
        )
        return 2

    complaints = [
        *_check_ledger(ledger[1], oracle, numbers),
        *_check_pinned(pinned[1], oracle),
        *_check_deltas(deltas[1], oracle),
        *_check_restatements(lines, _covered(ledger, pinned, deltas), oracle),
    ]

    if complaints:
        print(f"parity-ledger single source: {len(complaints)} disagreement(s)", file=sys.stderr)
        for complaint in complaints:
            print(f"  {complaint}", file=sys.stderr)
        return 1

    ledger_span = f"{ledger[0] + 1}-{ledger[2] + 1}"
    print(
        f"parity-ledger single source: {len(numbers)} row statuses in "
        f"{_display(oracle_path)}, reconciled against {_display(doc_path)}"
    )
    print(f"  ledger table ({ledger_span}): {len(numbers)} keyed rows agree")
    print(f"  pinned decision rows: {len(pinned[1])} keys agree")
    print(f"  designation deltas: {len(deltas[1])} keys agree")
    print("  restatements: none outside the three tables")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
