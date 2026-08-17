"""Registration↔stub drift gate (ergo DSWX6Z).

The PyO3 registration surface (``rust/crates/degenbot-python/src/c_api.rs`` +
the per-domain ``add_*_module`` fns) is the source of truth for what the
``degenbot._ffi`` extension exposes at runtime. The type stubs in
``src/degenbot/_ffi/*.pyi`` are hand-maintained, and nothing mechanical used
to catch drift between the two (the extension module wins over the
namespace-package stub dir at import time, so the stubs are invisible to the
runtime). This test pins both directions:

- **R1 (coverage)**: every public runtime symbol of a stubbed module must
  appear in its stub — defined (class / function / assignment) or bound via a
  top-level import.
- **R2 (honest ``__all__``)**: every ``__all__`` entry of a stub must exist at
  runtime — phantom entries would break ``import *``.
- **R3 (no phantom definitions)**: every class/function/constant a stub
  DEFINES at top level must exist at runtime. Annotation-only type imports
  are exempt: they document parameter types, not the module surface.
- **R0 (submodule coverage)**: every runtime ``degenbot._ffi.*`` submodule
  must have a stub file.

Surface-changing tasks must update the affected stub in the same commit so
this gate stays green (epic C7D2CH guardrail).
"""

from __future__ import annotations

import ast
import importlib
import pathlib
import sys
from typing import TYPE_CHECKING

import pytest

if TYPE_CHECKING:
    from collections.abc import Iterator

_STUB_DIR = pathlib.Path(__file__).resolve().parents[2] / "src" / "degenbot" / "_ffi"


def _stub_modules() -> dict[str, pathlib.Path]:
    """Importable module name -> stub file, for the root + every submodule stub."""
    mods: dict[str, pathlib.Path] = {"degenbot._ffi": _STUB_DIR / "__init__.pyi"}
    for path in sorted(_STUB_DIR.glob("*.pyi")):
        if path.name == "__init__.pyi":
            continue
        mods[f"degenbot._ffi.{path.stem}"] = path
    return mods


def _runtime_public_names(module_name: str) -> set[str]:
    obj = importlib.import_module(module_name)
    return {n for n in dir(obj) if not n.startswith("_")}


def _top_level_nodes(stub_path: pathlib.Path) -> Iterator[ast.stmt]:
    return ast.iter_child_nodes(ast.parse(stub_path.read_text(encoding="utf-8")))


def _stub_defined_names(stub_path: pathlib.Path) -> set[str]:
    """Top-level classes / functions / assignments the stub DEFINES."""
    names: set[str] = set()
    for node in _top_level_nodes(stub_path):
        if isinstance(node, (ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)):
            names.add(node.name)
        elif isinstance(node, ast.Assign):
            for target in node.targets:
                if isinstance(target, ast.Name) and target.id != "__all__":
                    names.add(target.id)
        elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
            names.add(node.target.id)
    return names


def _stub_bound_names(stub_path: pathlib.Path) -> set[str]:
    """Defined names + names bound by top-level imports (R1 coverage set)."""
    names = _stub_defined_names(stub_path)
    for node in _top_level_nodes(stub_path):
        if isinstance(node, (ast.Import, ast.ImportFrom)):
            for alias in node.names:
                names.add((alias.asname or alias.name).split(".")[0])
    return names


def _stub_all_names(stub_path: pathlib.Path) -> set[str] | None:
    for node in _top_level_nodes(stub_path):
        if isinstance(node, ast.Assign):
            for target in node.targets:
                if isinstance(target, ast.Name) and target.id == "__all__":
                    value = node.value
                    if isinstance(value, (ast.List, ast.Tuple, ast.Set)):
                        return {el.value for el in value.elts if isinstance(el, ast.Constant)}
    return None


@pytest.mark.parametrize("module_name", sorted(_stub_modules()))
def test_runtime_symbols_are_stubbed(module_name: str) -> None:
    """R1: every public runtime symbol is documented in the stub."""
    stub = _stub_modules()[module_name]
    runtime = _runtime_public_names(module_name)
    stubbed = _stub_bound_names(stub)
    missing = sorted(runtime - stubbed)
    assert not missing, f"{module_name}: runtime symbols missing from {stub.name}: {missing}"


@pytest.mark.parametrize("module_name", sorted(_stub_modules()))
def test_stub_all_is_honest(module_name: str) -> None:
    """R2: every ``__all__`` entry exists at runtime (no phantom promises)."""
    stub = _stub_modules()[module_name]
    all_names = _stub_all_names(stub)
    if all_names is None:
        pytest.skip(f"{stub.name} declares no __all__")
    runtime = _runtime_public_names(module_name)
    phantom = sorted(all_names - runtime)
    assert not phantom, f"{module_name}: __all__ entries absent at runtime: {phantom}"


@pytest.mark.parametrize("module_name", sorted(_stub_modules()))
def test_stub_definitions_exist_at_runtime(module_name: str) -> None:
    """R3: classes/functions/constants the stub defines exist at runtime."""
    stub = _stub_modules()[module_name]
    runtime = _runtime_public_names(module_name)
    phantom = sorted(_stub_defined_names(stub) - runtime)
    assert not phantom, (
        f"{module_name}: stub defines names absent at runtime (phantom surface): {phantom}"
    )


def test_every_runtime_submodule_has_a_stub() -> None:
    """R0: every runtime `degenbot._ffi.*` submodule has a stub file."""
    import degenbot._ffi  # ruff: ignore[unused-import]  (registers the submodules)

    stub_stems = {p.stem for p in _STUB_DIR.glob("*.pyi") if p.name != "__init__.pyi"}
    runtime_subs = {
        name.removeprefix("degenbot._ffi.")
        for name in sys.modules
        if name.startswith("degenbot._ffi.")
    }
    missing = sorted(runtime_subs - stub_stems)
    assert not missing, f"runtime submodules missing a stub: {missing}"
