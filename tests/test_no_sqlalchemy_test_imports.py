"""Permanent no-SQLAlchemy retirement gate for Python and Rust fixture surfaces."""

from __future__ import annotations

import ast
import re
import tomllib
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
PYTHON_ROOTS = (
    REPO_ROOT / "src",
    REPO_ROOT / "tests",
    REPO_ROOT / "rust" / "crates",
)
FORBIDDEN_PACKAGE = "sql" + "alchemy"
FORBIDDEN_DATABASE_PACKAGE = "degenbot." + "database"
FORBIDDEN_SYMBOLS = {
    "Database" + "SessionManager",
    "scoped_" + "session",
    "get_scoped_" + "sqlite_session",
}


def _python_sources() -> list[Path]:
    return sorted(
        path
        for root in PYTHON_ROOTS
        if root.exists()
        for path in root.rglob("*.py")
    )


def _dotted_name(node: ast.AST) -> str | None:
    if isinstance(node, ast.Name):
        return node.id
    if isinstance(node, ast.Attribute):
        parent = _dotted_name(node.value)
        return f"{parent}.{node.attr}" if parent else None
    return None


def _forbidden_names(source: str) -> set[str]:
    tree = ast.parse(source)
    names: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            names.update(alias.name for alias in node.names)
        elif isinstance(node, ast.ImportFrom):
            module = node.module or ""
            if module:
                names.add(module)
                names.update(f"{module}.{alias.name}" for alias in node.names)
        elif isinstance(node, ast.Name):
            names.add(node.id)
        elif isinstance(node, ast.Attribute):
            dotted = _dotted_name(node)
            if dotted:
                names.add(dotted)
            names.add(node.attr)
    return names


def _is_forbidden_name(name: str) -> bool:
    normalized = name.lower()
    return (
        normalized in (FORBIDDEN_PACKAGE, FORBIDDEN_DATABASE_PACKAGE)
        or normalized.startswith((f"{FORBIDDEN_PACKAGE}.", f"{FORBIDDEN_DATABASE_PACKAGE}."))
        or name in FORBIDDEN_SYMBOLS
        or name.rsplit(".", 1)[-1] in FORBIDDEN_SYMBOLS
    )


def _violations_for_source(path: Path) -> list[str]:
    names = _forbidden_names(path.read_text(encoding="utf-8"))
    forbidden = sorted(name for name in names if _is_forbidden_name(name))
    return [f"{path.relative_to(REPO_ROOT)}: {name}" for name in forbidden]


def _requirement_name(requirement: str | dict[str, str]) -> str:
    if isinstance(requirement, dict):
        requirement = requirement.get("name", "")
    match = re.match(r"[A-Za-z0-9][A-Za-z0-9._-]*", requirement)
    return match.group(0).lower() if match else ""


def _requirements(value: object):
    if isinstance(value, str):
        yield value
    elif isinstance(value, list):
        for item in value:
            yield from _requirements(item)
    elif isinstance(value, dict):
        for item in value.values():
            yield from _requirements(item)


def test_no_sqlalchemy_imports_or_orm_symbols() -> None:
    violations = [
        violation
        for path in _python_sources()
        for violation in _violations_for_source(path)
    ]
    assert not violations, "retired SQLAlchemy/ORM references remain:\n" + "\n".join(violations)


def test_no_sqlalchemy_root_dependency_declarations() -> None:
    pyproject = tomllib.loads((REPO_ROOT / "pyproject.toml").read_text(encoding="utf-8"))
    project = pyproject["project"]
    dependency_values = [
        project.get("dependencies", []),
        project.get("optional-dependencies", {}),
        pyproject.get("dependency-groups", {}),
    ]
    direct_requirements = [
        requirement
        for value in dependency_values
        for requirement in _requirements(value)
    ]
    assert not any(
        _requirement_name(requirement) == FORBIDDEN_PACKAGE for requirement in direct_requirements
    )

    lock = tomllib.loads((REPO_ROOT / "uv.lock").read_text(encoding="utf-8"))
    packages = lock.get("package", [])
    assert not any(package.get("name", "").lower() == FORBIDDEN_PACKAGE for package in packages)
    root_package = next(package for package in packages if package.get("name") == "degenbot")
    root_requirements = root_package.get("metadata", {}).get("requires-dist", [])
    assert not any(
        _requirement_name(requirement) == FORBIDDEN_PACKAGE for requirement in root_requirements
    )


def test_no_sqlalchemy_gate_rejects_synthetic_import() -> None:
    synthetic = "import " + FORBIDDEN_PACKAGE + "\n"
    assert FORBIDDEN_PACKAGE in _forbidden_names(synthetic)
