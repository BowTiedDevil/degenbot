"""Committed SQLite fixtures must match the production schema.

A committed special-purpose DB stamped with an older
``_degenbot_db_schema_version`` than the running core's is drift: every consumer
reads it against the current schema. There is no in-place migration for a
rust-stamped fixture — ``db_upgrade_database`` / ``db_heal_database`` are no-ops
on a ``rust_owned`` file — so a stale fixture must be regenerated via its own
generator.

A stale fixture is a hard failure: the fix is to bring it forward, not to
tolerate it. ``alembic_revs/*`` fixtures are intentionally-historical migration
inputs and are excluded.
"""

from __future__ import annotations

import sqlite3
from pathlib import Path

from degenbot.db import db_schema_version

_REPO_ROOT = Path(__file__).resolve().parents[2]
_STAMP_TABLE = "_degenbot_db_schema_version"


def _committed_fixtures() -> list[Path]:
    return sorted([
        *(_REPO_ROOT / "tests").rglob("*.db"),
        *(_REPO_ROOT / "rust" / "crates").rglob("*.db"),
    ])


def _schema_version(path: Path) -> int | None:
    connection = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    try:
        tables = {
            row[0]
            for row in connection.execute("SELECT name FROM sqlite_master WHERE type='table'")
        }
        if _STAMP_TABLE not in tables:
            return None
        row = connection.execute(
            "SELECT schema_version FROM _degenbot_db_schema_version"
        ).fetchone()
        return int(row[0]) if row else None
    finally:
        connection.close()


def test_committed_db_fixtures_are_at_the_production_schema() -> None:
    current = db_schema_version()
    stale: list[str] = []
    for path in _committed_fixtures():
        relative = path.relative_to(_REPO_ROOT).as_posix()
        if "alembic_revs/" in relative:
            continue
        version = _schema_version(path)
        if version is not None and version != current:
            stale.append(f"{relative} (schema {version}, current {current})")
    assert not stale, "committed DB fixtures drifted from the production schema: " + "; ".join(
        stale
    )
