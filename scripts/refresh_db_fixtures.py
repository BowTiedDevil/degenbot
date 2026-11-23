#!/usr/bin/env python3
"""Bring every committed DB fixture to the production schema, preserving rows.

The canonical forward path is ``db_upgrade_database`` (the Rust-owned D2
version-lock over ``RUST_MIGRATIONS``): a rust-stamped fixture behind the
running core's schema is migrated in place by the same registry the open path
applies, so its rows and ids survive and the migrated file carries the head DDL.

Run it after a ``RUST_SCHEMA_VERSION`` bump; ``tests/database/
test_committed_db_schema.py`` fails the tree until every stamped fixture is at
the production schema. ``alembic_revs/*`` fixtures are intentionally-historical
migration inputs and are left alone.

Every pre-existing table must keep its row count and its exact id set; a
violation exits non-zero instead of leaving a silently rewritten fixture.
"""

from __future__ import annotations

import sqlite3
import sys
import tempfile
from pathlib import Path

from degenbot.db import db_schema_version, db_upgrade_database

_REPO_ROOT = Path(__file__).resolve().parents[1]
_STAMP_TABLE = "_degenbot_db_schema_version"
_HISTORICAL = "alembic_revs/"

# The projection family this schema step owns: their stored DDL is re-checked
# against a freshly-created head DB on every run, so a fixture cannot diverge
# from the head silently after it was migrated.
_PARITY_TABLES = (
    "aave_v3_block_anchors",
    "aave_v3_checkpoint_generations",
    "aave_v3_checkpoint_positions",
    "aave_v3_delta_journal",
    "aave_v3_generations",
    "aave_v3_projection_checkpoints",
    "aave_v3_projection_cursors",
    "aave_v3_transaction_anchors",
)

TableSnapshot = tuple[int, list[int] | None]


def _fixtures() -> list[Path]:
    candidates = [
        *(_REPO_ROOT / "tests").rglob("*.db"),
        *(_REPO_ROOT / "rust" / "crates").rglob("*.db"),
    ]
    fixtures = []
    for path in sorted(set(candidates)):
        relative = path.relative_to(_REPO_ROOT).as_posix()
        if _HISTORICAL in relative:
            continue
        if _stamp(path) is not None:
            fixtures.append(path)
    return fixtures


def _read(path: Path) -> sqlite3.Connection:
    return sqlite3.connect(f"file:{path}?mode=ro", uri=True)


def _stamp(path: Path) -> int | None:
    connection = _read(path)
    try:
        tables = {
            row[0]
            for row in connection.execute("SELECT name FROM sqlite_master WHERE type='table'")
        }
        if _STAMP_TABLE not in tables:
            return None
        # S608: the identifier is the module's own stamp-table constant.
        row = connection.execute(f"SELECT schema_version FROM {_STAMP_TABLE}").fetchone()  # ruff: ignore[hardcoded-sql-expression]
        return int(row[0]) if row else None
    finally:
        connection.close()


def _snapshot(path: Path) -> dict[str, TableSnapshot]:
    connection = _read(path)
    try:
        tables = [
            row[0]
            for row in connection.execute(
                "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"
            )
        ]
        snapshot: dict[str, TableSnapshot] = {}
        for table in tables:
            columns = {row[1] for row in connection.execute(f'PRAGMA table_info("{table}")')}
            # S608: every identifier comes from `sqlite_master`, not from input.
            rows = int(connection.execute(f'SELECT COUNT(*) FROM "{table}"').fetchone()[0])  # ruff: ignore[hardcoded-sql-expression]
            ids = None
            if "id" in columns:
                ids = [
                    int(row[0])
                    for row in connection.execute(
                        f'SELECT id FROM "{table}" ORDER BY id'  # ruff: ignore[hardcoded-sql-expression]
                    )
                ]
            snapshot[table] = (rows, ids)
        return snapshot
    finally:
        connection.close()


def _ddl(connection: sqlite3.Connection) -> dict[str, str]:
    return {
        str(row[0]): str(row[1])
        for row in connection.execute(
            "SELECT name, sql FROM sqlite_master WHERE type='table' AND sql IS NOT NULL"
        )
    }


def _head_ddl() -> dict[str, str]:
    with tempfile.TemporaryDirectory() as directory:
        fresh = Path(directory) / "fresh.db"
        db_upgrade_database(str(fresh))
        connection = _read(fresh)
        try:
            return _ddl(connection)
        finally:
            connection.close()


def _ddl_parity(path: Path, added: list[str], head: dict[str, str]) -> tuple[list[str], int]:
    connection = _read(path)
    try:
        declared = _ddl(connection)
    finally:
        connection.close()
    problems = []
    checked = 0
    for table in sorted({*added, *_PARITY_TABLES}):
        if table not in declared and table not in added:
            continue
        checked += 1
        if table not in head:
            problems.append(f"{table}: absent from the fresh head DDL")
        elif declared.get(table) != head[table]:
            problems.append(f"{table}: DDL differs from the fresh head")
    return problems, checked


def _diff(before: dict[str, TableSnapshot], after: dict[str, TableSnapshot]) -> list[str]:
    problems = []
    for table, (rows, ids) in before.items():
        if rows == 0 and ids is None and table not in after:
            continue
        if table not in after:
            problems.append(f"{table}: table vanished")
            continue
        new_rows, new_ids = after[table]
        if new_rows != rows:
            problems.append(f"{table}: {rows} -> {new_rows} rows")
        if ids is not None and new_ids != ids:
            problems.append(f"{table}: id set changed")
    return problems


def main() -> int:
    current = db_schema_version()
    head = _head_ddl()
    failures: list[str] = []
    for path in _fixtures():
        relative = path.relative_to(_REPO_ROOT).as_posix()
        before_stamp = _stamp(path)
        before = _snapshot(path)
        outcome = db_upgrade_database(str(path))
        after_stamp = _stamp(path)
        after = _snapshot(path)
        added = sorted(set(after) - set(before))
        problems = _diff(before, after)
        parity_problems, checked = _ddl_parity(path, added, head)
        problems.extend(parity_problems)
        print(
            f"{relative}: schema {before_stamp} -> {after_stamp} ({outcome}), "
            f"tables +{len(added)}{(' ' + ', '.join(added)) if added else ''}, "
            f"head DDL identical for {checked} projection tables"
        )
        if after_stamp != current:
            problems.append(f"stamp {after_stamp} != production {current}")
        failures.extend(f"{relative}: {problem}" for problem in problems)

    if failures:
        print("FAILED:\n  " + "\n  ".join(failures), file=sys.stderr)
        return 1
    print(f"all committed fixtures at production schema {current}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
