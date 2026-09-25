"""Mechanical retirement gate for database imports in test-owned surfaces."""

from __future__ import annotations

import ast
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SOURCES = (*(REPO_ROOT / "tests").rglob("*.py"), *(REPO_ROOT / "rust/crates/foundation/degenbot-db/tests/fixtures").glob("*.py"))
FORBIDDEN_IMPORTS = {
    "degenbot.database." + "models",
    "sql" + "alchemy",
    "degenbot.database." + "session_manager",
}
FORBIDDEN_SYMBOLS = {
    "Database" + "SessionManager",
    "get_scoped_" + "sqlite_session",
}


def _imported_names(path: Path) -> set[str]:
    tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    names: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            names.update(alias.name for alias in node.names)
        elif isinstance(node, ast.ImportFrom) and node.module is not None:
            names.add(node.module)
            names.update(f"{node.module}.{alias.name}" for alias in node.names)
    return names


def test_database_test_and_fixture_imports_do_not_depend_on_retired_orm() -> None:
    violations: list[str] = []
    for path in SOURCES:
        imported = _imported_names(path)
        forbidden = {
            name
            for name in imported
            if name in FORBIDDEN_IMPORTS
            or name.startswith("degenbot.database.")
            or name.rsplit(".", 1)[-1] in FORBIDDEN_SYMBOLS
        }
        if forbidden:
            violations.append(f"{path.relative_to(REPO_ROOT)}: {', '.join(sorted(forbidden))}")

    assert not violations, "retired database imports remain:\n" + "\n".join(violations)
