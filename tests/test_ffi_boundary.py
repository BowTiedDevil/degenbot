"""Enforce ADR-013's private Rust FFI seam.

Every ordinary ``_ffi`` import belongs in a stable ``degenbot.<domain>``
package barrier. Outside those barriers, only the permanent engine-handle
exception and the Rust-owned console passthrough are allowed.
"""

from __future__ import annotations

import ast
import importlib
import importlib.util
from pathlib import Path

import pytest

import degenbot

REPO_ROOT = Path(__file__).resolve().parents[1]
SCAN_DIR = REPO_ROOT / "src" / "degenbot"
_FFI_ROOT = "degenbot._ffi"

# ADR-032 fork (module-path disambiguation): these engine-handle pyclasses
# may be imported directly from _ffi by first-party code. Their clean names
# collide with same-named Python driver/model classes, and the module path
# (degenbot._ffi vs the domain home) is the disambiguator.
ENGINE_HANDLES: frozenset[str] = frozenset(
    {
        "Bot",
        "BotIo",
        "Erc20Token",
        "DatabasePositionQuery",
        "DatabaseSnapshot",
    },
)
_ENGINE_HANDLE_MODULES = frozenset({_FFI_ROOT, f"{_FFI_ROOT}.db"})

# ADR-051 D3 makes the console a deliberate passthrough rather than a Python
# command tree. It has no domain home; admit only its Rust-owned entrypoint in
# the exact process-entry module (module imports and other symbols still fail).
ALLOWED_LEAF_FFI_IMPORTS: dict[str, frozenset[str]] = {
    "src/degenbot/_cli.py": frozenset({"cli_main"}),
}


def _iter_python_files() -> list[Path]:
    """Yield every .py file under src/degenbot/ (excluding __pycache__)."""
    return [f for f in SCAN_DIR.rglob("*.py") if "__pycache__" not in f.parts]


def _module_package(path: Path) -> str:
    parts = [SCAN_DIR.name, *path.relative_to(SCAN_DIR).with_suffix("").parts]
    parts.pop()
    return ".".join(parts)


def _resolved_from_module(node: ast.ImportFrom, package: str) -> str | None:
    if node.level == 0:
        return node.module
    relative_name = "." * node.level + (node.module or "")
    try:
        return importlib.util.resolve_name(relative_name, package)
    except ImportError:
        return None


def _is_ffi_module(module: str | None) -> bool:
    return module == _FFI_ROOT or (module is not None and module.startswith(f"{_FFI_ROOT}."))


def _find_ffi_import_violations(source: str, path: Path) -> list[str]:
    """Find runtime imports of the private extension through any spelling."""
    tree = ast.parse(source, filename=str(path))
    package = _module_package(path)
    relative_path = path.relative_to(REPO_ROOT).as_posix()
    lines = source.splitlines()
    violations: list[str] = []

    class RuntimeVisitor(ast.NodeVisitor):
        def visit_If(self, node: ast.If) -> None:
            if "TYPE_CHECKING" in ast.unparse(node.test):
                for statement in node.orelse:
                    self.visit(statement)
                return
            self.generic_visit(node)

        def visit_Import(self, node: ast.Import) -> None:
            if any(_is_ffi_module(alias.name) for alias in node.names):
                violations.append(
                    f"{relative_path}:{node.lineno}: {lines[node.lineno - 1].strip()}"
                )

        def visit_ImportFrom(self, node: ast.ImportFrom) -> None:
            module = _resolved_from_module(node, package)
            names = {alias.name for alias in node.names}
            imports_ffi_module = _is_ffi_module(module)
            imports_ffi_attribute = module == "degenbot" and "_ffi" in names
            if not imports_ffi_module and not imports_ffi_attribute:
                return

            if imports_ffi_module and module in _ENGINE_HANDLE_MODULES and names <= ENGINE_HANDLES:
                return

            allowed_names = ALLOWED_LEAF_FFI_IMPORTS.get(relative_path, frozenset())
            if imports_ffi_module and module == _FFI_ROOT and names <= allowed_names:
                return

            violations.append(f"{relative_path}:{node.lineno}: {lines[node.lineno - 1].strip()}")

    RuntimeVisitor().visit(tree)
    return violations


@pytest.mark.parametrize(
    "source",
    [
        "from degenbot._ffi import runtime_status",
        "import degenbot._ffi",
        "from ._ffi import runtime_status",
        "from degenbot import _ffi",
    ],
)
def test_private_ffi_import_forms_are_violations(source: str) -> None:
    """Relative and top-level spellings cannot bypass the private seam."""
    assert _find_ffi_import_violations(source, SCAN_DIR / "synthetic_leaf.py")


def test_engine_handles_remain_an_explicit_exception() -> None:
    """Only the ADR-032 handle set may cross the boundary by module path."""
    assert not _find_ffi_import_violations(
        "from degenbot._ffi import Bot, BotIo", SCAN_DIR / "synthetic_leaf.py"
    )
    assert _find_ffi_import_violations(
        "from degenbot._ffi import Bot, runtime_status", SCAN_DIR / "synthetic_leaf.py"
    )


def test_console_passthrough_is_an_explicit_exception() -> None:
    """ADR-051 permits only ``cli_main`` in the exact console entry module."""
    source = "from degenbot._ffi import cli_main"
    assert not _find_ffi_import_violations(source, SCAN_DIR / "_cli.py")
    assert _find_ffi_import_violations(source, SCAN_DIR / "other_leaf.py")


def test_runtime_status_is_public_package_barrier() -> None:
    """The public call remains available from a package-owned FFI barrier."""
    runtime_status_module = importlib.import_module("degenbot.runtime_status")
    assert hasattr(runtime_status_module, "__path__")

    assert {"fleet_booted", "profile"} <= degenbot.runtime_status().keys()


def test_no_ffi_imports_outside_barriers_and_explicit_seams() -> None:
    """Fail on every private FFI import outside its approved seam."""
    violations: list[str] = []
    for path in _iter_python_files():
        if path.name == "__init__.py":
            continue
        violations.extend(_find_ffi_import_violations(path.read_text(), path))

    if violations:
        message = (
            f"\nFound {len(violations)} private `_ffi` import(s) outside approved seams:\n\n"
            + "\n".join(f"  - {violation}" for violation in violations)
            + "\n\nImport Rust symbols from a stable `degenbot.<domain>` package "
            "barrier instead. ENGINE_HANDLES and ALLOWED_LEAF_FFI_IMPORTS above "
            "are the only documented leaf exceptions."
        )
        pytest.fail(message)
