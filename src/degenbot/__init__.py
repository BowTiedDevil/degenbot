"""
degenbot package initializer with lazy loading and static-analysis friendly discovery.

This module builds a consolidated public API for the degenbot package without eagerly
importing subpackages. It discovers immediate child subpackages and top-level modules,
parses their exported symbols, and exposes both module names and selected symbols at
package import time. Actual imports occur lazily on first attribute access.

Key behaviors:
    - Discovery: Uses pkgutil.iter_modules over this package directory to find immediate
    child subpackages and top-level modules.
    - Export parsing: For subpackages that define __all__ in their __init__.py, this module
    parses the __all__ value via AST (no code execution) and records those symbol names.
    Only literal containers of string literals (and their concatenations) are supported;
    dynamic constructions are intentionally ignored.
    - Top-level modules: Files located directly under the package directory (e.g. anvil_fork.py)
    are treated as modules and can be accessed by name, and their public attributes may be
    resolved during fallback lookup.
    - Lazy access: Module-level __getattr__ (PEP 562) performs lazy imports on demand and
    resolves attributes from the appropriate submodule/module only when first accessed.
    - Public API: __all__ contains both discovered module names and aggregated exported symbols
    for a friendly interactive experience (dir(), tab-completion).
    - TYPE_CHECKING: When TYPE_CHECKING is true, this module eagerly imports discovered
    subpackages/modules and surfaces exported attributes to aid static analyzers and IDEs.
    - Duplicate names: If multiple subpackages export the same symbol, the first discovered
    one is kept and a RuntimeWarning is emitted.
"""

import ast
import contextlib
import pkgutil
import warnings
from importlib import import_module
from importlib.metadata import version
from pathlib import Path
from types import ModuleType
from typing import TYPE_CHECKING

__version__: str = version(__package__)

_PACKAGE_DIR = Path(__file__).parent

# Maps export name -> (subpackage name, fully qualified module name)
_EXPORTS: dict[str, tuple[str, str]] = {}

# Maps subpackage name -> fully qualified module name (e.g., 'uniswap' -> 'degenbot.uniswap')
_SUBPKGS: dict[str, str] = {}

# Cache of imported submodules: fully qualified module name -> ModuleType
_IMPORTED: dict[str, ModuleType] = {}


def _get_exported_symbols_from_init_file(init_path: Path) -> list[str]:
    """
    Parse exported symbol names from a subpackage __init__.py by statically analyzing __all__.

    This function reads the target file and parses its AST to extract string names assigned to
    __all__. It supports:
      - Literal containers (list/tuple/set) of string literals
      - Concatenation of literal containers using +
      - Starred elements within literal containers

    It does not execute any code and will ignore/dismiss dynamic constructions. Unexpected AST
    shapes raise TypeError to signal unsupported patterns.

    Parameters:
        init_path: Filesystem path to the subpackage's __init__.py.

    Returns:
        A list of unique symbol names exported by the subpackage (order not guaranteed).
    """

    source = init_path.read_text(encoding="utf-8")
    tree = ast.parse(source, filename=str(init_path))

    def extract_strs(node: ast.AST) -> set[str]:
        out: set[str] = set()

        def handle(n: ast.AST) -> None:
            if isinstance(n, (ast.List, ast.Tuple, ast.Set)):
                for elt in n.elts:
                    handle(elt)
            elif isinstance(n, ast.Constant) and isinstance(n.value, str):
                out.add(n.value)
            elif isinstance(n, ast.BinOp) and isinstance(n.op, ast.Add):
                handle(n.left)
                handle(n.right)
            elif isinstance(n, ast.Starred):
                handle(n.value)
            else:
                msg = "Unexpected value in AST"
                raise TypeError(msg)

        handle(node)
        return out

    exports: set[str] = set()
    for node in tree.body:
        if isinstance(node, ast.Assign):
            for target in node.targets:
                if isinstance(target, ast.Name) and target.id == "__all__":
                    exports = extract_strs(node.value)
        elif isinstance(node, ast.AnnAssign):
            target = node.target
            if isinstance(target, ast.Name) and target.id == "__all__" and node.value is not None:
                exports = extract_strs(node.value)

    return list(exports)


def _discover_subpackages_and_exports() -> None:
    """
    Discover immediate child subpackages and top-level modules, then collect exports.

    For each immediate child:
      - If it is a subpackage and not private (name does not start with "_"), record its
        fully-qualified module name and parse its __all__ (if present) to collect exported
        symbol names.
      - If it is a top-level module (a .py file in this directory), record it so it can be
        imported on-demand and used during attribute resolution.

    Duplicate exported symbol names across subpackages are resolved by keeping the first
    discovered entry and emitting a RuntimeWarning.
    """

    for _, name, ispkg in pkgutil.iter_modules([str(_PACKAGE_DIR)]):
        module_fqn = f"{__name__}.{name}"

        if ispkg:
            if name.startswith("_"):
                continue
            _SUBPKGS[name] = module_fqn

            init_path = _PACKAGE_DIR / name / "__init__.py"
            if init_path.is_file():
                exports = _get_exported_symbols_from_init_file(init_path)
                for symbol in exports:
                    if symbol in _EXPORTS:
                        prev_pkg, _ = _EXPORTS[symbol]
                        warnings.warn(
                            f"{__package__} lazy loader: duplicate export name '{symbol}' found in "
                            f"subpackages '{prev_pkg}' and '{name}'. "
                            f"Keeping the first ({prev_pkg}).",
                            RuntimeWarning,
                            stacklevel=2,
                        )
                        continue
                    _EXPORTS[symbol] = (name, module_fqn)
        else:
            # Also treat top-level modules (files like anvil_fork.py) as direct exports
            _SUBPKGS.setdefault(name, module_fqn)


# Initialize discovery exactly once at import time
_discover_subpackages_and_exports()

# Public API listing: include subpackage names and aggregated symbol names
__all__ = tuple(
    sorted(
        list(_SUBPKGS.keys()) + list(_EXPORTS.keys()),
    )
)


def _import_submodule(module_fqn: str) -> ModuleType:
    mod = _IMPORTED.get(module_fqn)
    if mod is not None:
        return mod
    mod = import_module(module_fqn)
    _IMPORTED[module_fqn] = mod
    return mod


def __getattr__(name: str) -> object:
    """
    Lazy attribute access for package-level names (PEP 562).

    Resolution order:
        - If 'name' matches a discovered subpackage or top-level module, import and return that
        module.
        - If 'name' matches a symbol exported by a subpackage (collected from its __all__), import
        the subpackage and return the attribute.
        - Fallback: import each discovered subpackage/module (best-effort) and return the first
        module that exposes an attribute with this name. This helps in cases where __all__ was
        dynamic or where a top-level module defines a public symbol without listing it.

    Raises:
        AttributeError: If no module or exported attribute with the requested name can be found.
    """
    # 1) Accessing a subpackage or top-level module by its module name
    subpkg_fqn = _SUBPKGS.get(name)
    if subpkg_fqn:
        return _import_submodule(subpkg_fqn)

    # 2) Accessing a symbol exported by a subpackage (collected from __all__)
    target = _EXPORTS.get(name)
    if target:
        _, module_fqn = target
        mod = _import_submodule(module_fqn)
        try:
            return getattr(mod, name)
        except AttributeError as exc:
            msg = (
                f"Module '{__name__}' lazily imported '{module_fqn}' "
                f"but it does not expose '{name}'"
            )
            raise AttributeError(msg) from exc

    # 3) Fallback: scan subpackages/modules for an attribute with this name
    # This addresses cases where a subpackage exports a symbol via its __all__,
    # but our AST parse could not discover it (e.g., dynamic __all__), or where
    # a top-level module defines a public symbol.
    for module_fqn in _SUBPKGS.values():
        with contextlib.suppress(ImportError):
            mod = _import_submodule(module_fqn)
            if hasattr(mod, name):
                return getattr(mod, name)

    msg = f"module '{__name__}' has no attribute '{name}'"
    raise AttributeError(msg)


def __dir__() -> list[str]:
    """
    Provide an enhanced directory listing for interactive use.

    The returned names include:
      - Existing globals of this package module
      - Discovered subpackage and top-level module names
      - Exported symbols collected from subpackages' __all__
      - Best-effort inclusion of public attributes from already-imported known modules

    Returns:
        Sorted list of attribute names visible on this package.
    """
    base = set(globals().keys())
    # Show module names (subpackages and top-level modules)
    base.update(_SUBPKGS.keys())
    # Show exported symbols that we parsed from subpackages
    base.update(_EXPORTS.keys())

    # Best-effort: include public attributes from already-imported known modules
    for module_fqn in list(_IMPORTED.keys()):
        with contextlib.suppress(ImportError):
            mod = _IMPORTED[module_fqn]
            for attr in getattr(mod, "__all__", []) or []:
                base.add(attr)

    return sorted(base)


# Static typing: eagerly import to surface symbols to type checkers
if TYPE_CHECKING:
    for _subpkg, _fqn in list(_SUBPKGS.items()):
        with contextlib.suppress(ImportError):
            globals()[_subpkg] = import_module(_fqn)
    for _name, (_sp, _fqn) in list(_EXPORTS.items()):
        with contextlib.suppress(ImportError):
            _m = import_module(_fqn)
            if hasattr(_m, _name):
                globals()[_name] = getattr(_m, _name)

    # Also surface common top-level classes/functions defined in modules without __all__
    # by importing those modules and copying attributes into globals for type checking.
    for _subpkg, _fqn in list(_SUBPKGS.items()):
        with contextlib.suppress(ImportError):
            _m = import_module(_fqn)
            for _attr in getattr(_m, "__all__", []) or []:
                globals()[_attr] = getattr(_m, _attr)
