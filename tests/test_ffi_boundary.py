"""Enforce ADR-013's private Rust FFI seam.

Every ordinary ``_ffi`` import belongs in a stable ``degenbot.<domain>``
package barrier. Outside those barriers, only the permanent engine-handle
exception and the Rust-owned console passthrough are allowed.

The scan covers three trees. ``src/degenbot`` is the shipped package, whose
barriers are ``degenbot.<domain>`` modules. ``tests`` drives that package and
needs the same Rust names — engine handles to drive, pure math leaves to
cross-check, config verdicts to compare against the Python oracle — so it
reaches them through one door, ``tests/helpers/seam.py``, which re-exports the
suite's ``_ffi`` symbol set. The door is what keeps a seam rename a one-file
edit instead of a sweep across every test that hand-imported a Rust symbol, and
its import list is the census of what the suite is allowed to touch.
``examples`` is driver-facing template code, so it may import neither the
private seam nor the retired ``degenbot_rs`` extension name: it demonstrates the
supported import surface, which is ``degenbot.*``.
"""

from __future__ import annotations

import ast
import importlib
import importlib.util
from pathlib import Path

import pytest

import degenbot
from tests.source_scan import iter_text_files

REPO_ROOT = Path(__file__).resolve().parents[1]
TESTS_ROOT = REPO_ROOT / "tests"

#: ``(repo-relative root, importable package)`` for every scanned tree. All are
#: walked by ``tests.source_scan`` and scanned by the same rules; only the
#: allowlist a violation may fall through to differs.
SCAN_SCOPE: tuple[tuple[str, str], ...] = (
    ("src/degenbot", "degenbot"),
    ("tests", "tests"),
    ("examples", "examples"),
)

#: The retired spelling of the extension module, banned where driver-facing
#: template code is written so a partial revert cannot re-teach the old import.
RETIRED_EXTENSION_ROOT = "degenbot_rs"

# Driver-facing templates import the seam's own SUBMODULES for their adapter and
# diagnostics symbols: an exact (path, module, names) triple, so a new symbol or
# a new template is a reviewed row rather than an open door. The root module
# stays off-limits to a driver — no engine-handle or console exception reaches
# an example — because an example teaches the import surface a reader copies.
EXAMPLES_LEAF_FFI_IMPORTS: dict[str, dict[str, frozenset[str]]] = {
    "examples/eth_settlement_arbitrage_v2_v3_v4_rust.py": {
        "degenbot._ffi.diagnostics": frozenset({"mark_progress", "start_gil_probe"}),
    },
    "examples/execution_adapter_template.py": {
        "degenbot._ffi.execution": frozenset({
            "PayloadAdapter",
            "UnsupportedPath",
            "abi_encode_call",
        }),
    },
    "examples/execution_strategy_foreign.py": {
        "degenbot._ffi.execution": frozenset({"PayloadAdapter", "abi_encode_call"}),
    },
}


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
    },
)
_ENGINE_HANDLE_MODULES = frozenset({_FFI_ROOT, f"{_FFI_ROOT}.db"})

# ADR-051 D3 makes the console a deliberate passthrough rather than a Python
# command tree. It has no domain home; admit only its Rust-owned entrypoint in
# the exact process-entry module (module imports and other symbols still fail).
ALLOWED_LEAF_FFI_IMPORTS: dict[str, frozenset[str]] = {
    "src/degenbot/_cli.py": frozenset({"cli_main"}),
    # ADR-085 D2: the construction request is the seam's one input — a resolved
    # identity carried as a value, with no Python twin to translate to. This
    # module is its domain home; every caller imports it from here.
    "src/degenbot/builders/construction.py": frozenset({"PoolConstructionRequest"}),
}

# The suite's one door onto the seam. It is allowlisted for the whole ``_ffi``
# tree — root and ``add_submodule`` children — because re-exporting that tree is
# exactly its job; every other test module reaches ``_ffi`` names through it.
SEAM_DOOR = "tests/helpers/seam.py"
SEAM_MODULE = "tests.helpers.seam"

# ``tests/helpers/seam.py`` also hands out the extension module object itself,
# for tests whose SUBJECT is the registration surface: the pyclass/stub walkers
# under ``tests/ffi/`` and the public-mirror identity checks, which must walk
# ``dir()``, ``__module__`` and submodule presence rather than call one symbol.
# This is not a second import door — those modules still import only from the
# seam — and the list is the whole permit.
SURFACE_WALK_TESTS: frozenset[str] = frozenset(
    {
        "tests/ffi/test_ffi_registration_surface.py",
        "tests/ffi/test_pyclass_module_annotation.py",
        "tests/pool_handle/test_structural_cutover.py",
        "tests/test_db_public_mirror.py",
    },
)
REGISTRATION_SURFACE_ATTR = "REGISTRATION_SURFACE"

#: This module is the rule's enforcement home: it names the door's attribute to
#: test the permit above, so it is exempt from its own scan.
_ENFORCEMENT_HOME = Path(__file__).resolve().relative_to(REPO_ROOT).as_posix()


def _scanned_sources() -> list[tuple[str, str]]:
    """Every ``(repo-relative path, source)`` under the scanned trees."""
    return list(iter_text_files([root for root, _package in SCAN_SCOPE], [".py"]))


def _scope_for(relative: str) -> tuple[str, str]:
    """Return the ``(root, package)`` whose tree owns ``relative``."""
    for root, package in SCAN_SCOPE:
        if relative == root or relative.startswith(f"{root}/"):
            return root, package
    msg = f"{relative} is outside the scanned trees {[root for root, _ in SCAN_SCOPE]}"
    raise AssertionError(msg)


def _module_package(relative: str) -> str:
    root, package = _scope_for(relative)
    parts = [package, *Path(relative).relative_to(root).with_suffix("").parts]
    parts.pop()
    return ".".join(parts)


def _is_retired_extension(module: str | None) -> bool:
    """Whether ``module`` names the retired extension root or one of its children."""
    return module == RETIRED_EXTENSION_ROOT or (
        module is not None and module.startswith(f"{RETIRED_EXTENSION_ROOT}.")
    )


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


def _find_ffi_import_violations(source: str, relative: str) -> list[str]:
    """Find runtime imports of the private extension through any spelling."""
    tree = ast.parse(source, filename=relative)
    package = _module_package(relative)
    relative_path = relative
    in_tests = relative_path == "tests" or relative_path.startswith("tests/")
    in_examples = _scope_for(relative_path)[0] == "examples"
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
            if in_examples and any(_is_retired_extension(alias.name) for alias in node.names):
                self._record(node)
                return
            if not any(_is_ffi_module(alias.name) for alias in node.names):
                return
            if in_tests and relative_path == SEAM_DOOR:
                return
            if in_examples:
                # `import degenbot._ffi.execution` binds the submodule itself;
                # the permit map is keyed by that dotted path, and the root
                # module is not in it.
                permitted = EXAMPLES_LEAF_FFI_IMPORTS.get(relative_path, {})
                if all(
                    _is_ffi_module(alias.name) and alias.name in permitted for alias in node.names
                ):
                    return
            self._record(node)

        def visit_ImportFrom(self, node: ast.ImportFrom) -> None:
            module = _resolved_from_module(node, package)
            names = {alias.name for alias in node.names}
            imports_ffi_module = _is_ffi_module(module)
            imports_ffi_attribute = module == "degenbot" and "_ffi" in names
            if in_examples and _is_retired_extension(module):
                self._record(node)
                return
            if not imports_ffi_module and not imports_ffi_attribute:
                return

            if in_tests:
                # The suite has ONE door; no engine-handle or console escape
                # hatch, since the door re-exports those names too.
                if relative_path != SEAM_DOOR:
                    self._record(node)
                return

            if in_examples:
                # An example's only seam permit is a submodule symbol it needs
                # (EXAMPLES_LEAF_FFI_IMPORTS): the root module has no exception
                # here, engine handles and the console included.
                permitted = EXAMPLES_LEAF_FFI_IMPORTS.get(relative_path, {}).get(
                    module or "", frozenset()
                )
                if imports_ffi_module and module != _FFI_ROOT and names <= permitted:
                    return
                self._record(node)
                return

            if imports_ffi_module and module in _ENGINE_HANDLE_MODULES and names <= ENGINE_HANDLES:
                return

            allowed_names = ALLOWED_LEAF_FFI_IMPORTS.get(relative_path, frozenset())
            if imports_ffi_module and module == _FFI_ROOT and names <= allowed_names:
                return

            self._record(node)

        def _record(self, node: ast.Import | ast.ImportFrom) -> None:
            violations.append(f"{relative_path}:{node.lineno}: {lines[node.lineno - 1].strip()}")

    RuntimeVisitor().visit(tree)
    return violations


def _find_surface_walk_violations(source: str, relative: str) -> list[str]:
    """Find module-object use from the door outside the registration-surface tests."""
    relative_path = relative
    if relative_path in SURFACE_WALK_TESTS | {SEAM_DOOR, _ENFORCEMENT_HOME}:
        return []

    tree = ast.parse(source, filename=relative)
    lines = source.splitlines()
    violations: list[str] = []
    for node in ast.walk(tree):
        reaches_surface = (
            isinstance(node, ast.ImportFrom)
            and node.module == SEAM_MODULE
            and any(alias.name == REGISTRATION_SURFACE_ATTR for alias in node.names)
        ) or (isinstance(node, ast.Attribute) and node.attr == REGISTRATION_SURFACE_ATTR)
        if reaches_surface:
            violations.append(f"{relative_path}:{node.lineno}: {lines[node.lineno - 1].strip()}")
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
    assert _find_ffi_import_violations(source, "src/degenbot/synthetic_leaf.py")


def test_engine_handles_remain_an_explicit_exception() -> None:
    """Only the ADR-032 handle set may cross the boundary by module path."""
    assert not _find_ffi_import_violations(
        "from degenbot._ffi import Bot, BotIo", "src/degenbot/synthetic_leaf.py"
    )
    assert _find_ffi_import_violations(
        "from degenbot._ffi import Bot, runtime_status", "src/degenbot/synthetic_leaf.py"
    )


def test_console_passthrough_is_an_explicit_exception() -> None:
    """ADR-051 permits only ``cli_main`` in the exact console entry module."""
    source = "from degenbot._ffi import cli_main"
    assert not _find_ffi_import_violations(source, "src/degenbot/_cli.py")
    assert _find_ffi_import_violations(source, "src/degenbot/other_leaf.py")


def test_examples_may_not_reach_the_seam_or_the_retired_extension_name() -> None:
    """Driver-facing template code demonstrates the supported surface only.

    An example that imports the private seam (by any spelling, engine handles and
    the console included) or the retired ``degenbot_rs`` name teaches a reader an
    import the drivers must never make.
    """
    for source in (
        "from degenbot._ffi import Bot",
        "from degenbot._ffi import cli_main",
        "import degenbot._ffi",
        "from degenbot import _ffi",
        "from degenbot_rs import Bot",
        "import degenbot_rs",
    ):
        assert _find_ffi_import_violations(source, "examples/showcase_bot.py"), source

    assert not _find_ffi_import_violations(
        "from degenbot.provider import AlloyProvider", "examples/showcase_bot.py"
    )


# live-invariant gate (ADR-087 D2): watches the live `tests/` modules, which must
# reach the private seam only through `tests/helpers/seam.py`; not tripwire
# precedent.
def test_the_suite_has_one_door_and_no_handle_exception() -> None:
    """In ``tests/`` only the door may import the seam — ``Bot`` included.

    The engine-handle exception exists to disambiguate module paths inside the
    shipped package; in the suite the door already carries those handles, so
    there is nothing to disambiguate and no reason for a second importer.
    """
    forms = (
        "from degenbot._ffi import Bot",
        "from degenbot._ffi import runtime_status",
        "from degenbot._ffi.provider import AlloyProvider",
        "import degenbot._ffi",
        "from degenbot import _ffi",
    )
    for source in forms:
        assert _find_ffi_import_violations(source, "tests/synthetic_leaf.py"), source
        assert not _find_ffi_import_violations(source, SEAM_DOOR), source


def test_runtime_status_is_public_package_barrier() -> None:
    """The public call remains available from a package-owned FFI barrier."""
    runtime_status_module = importlib.import_module("degenbot.runtime_status")
    assert hasattr(runtime_status_module, "__path__")

    assert {"fleet_booted", "profile"} <= degenbot.runtime_status().keys()


def test_surface_walk_is_scoped_to_the_registration_tests() -> None:
    """The door's module object is not a general accessor for the seam."""
    from_door = f"from {SEAM_MODULE} import {REGISTRATION_SURFACE_ATTR}"
    through_door = f"from {SEAM_MODULE} import {REGISTRATION_SURFACE_ATTR} as ffi\nffi.db"

    for source in (from_door, through_door):
        assert _find_surface_walk_violations(source, "tests/synthetic_leaf.py")
        assert not _find_surface_walk_violations(
            source, "tests/ffi/test_ffi_registration_surface.py"
        )

    assert not _find_surface_walk_violations(
        "from tests.helpers.seam import Bot", "tests/synthetic_leaf.py"
    )


def test_door_reexports_the_suite_symbol_set() -> None:
    """The door is importable on its own and forwards the module's own symbols."""
    seam = importlib.import_module(SEAM_MODULE)
    ffi = importlib.import_module(_FFI_ROOT)

    assert seam.REGISTRATION_SURFACE is ffi

    # Every name the door advertises resolves, and a root-owned symbol is the
    # extension's own object rather than a copy: the door forwards, it does not
    # shadow.
    for name in seam.__all__:
        assert getattr(seam, name) is not None, name
    for name in ("Bot", "BotIo", "Pool", "SwapRefused", "keccak256", "resolved_config"):
        assert getattr(seam, name) is getattr(ffi, name), name

    # Symbols registered on an ``add_submodule`` child are forwarded from that
    # child, which is why the door's import list names the module per symbol.
    liquidity_math = importlib.import_module(f"{_FFI_ROOT}.concentrated_liquidity_math")
    assert seam.MAX_SQRT_RATIO is liquidity_math.MAX_SQRT_RATIO


# live-invariant gate (ADR-087 D2): watches the live `tests/helpers/seam.py`
# re-export wall, whose top-level statements must stay imports and bindings; not
# tripwire precedent.
def test_door_stays_a_thin_reexport() -> None:
    """The door is a re-export wall, never a second implementation home.

    A door that grows logic recreates the flat root one tier down: every
    top-level statement is an import, the module docstring, or a simple name
    binding (``__all__``, the forwarded-module aliases).
    """
    seam_path = TESTS_ROOT / "helpers" / "seam.py"
    tree = ast.parse(seam_path.read_text())

    def is_typing_conditional(node: ast.stmt) -> bool:
        return (
            isinstance(node, ast.If)
            and isinstance(node.test, ast.Name)
            and node.test.id == "TYPE_CHECKING"
            and all(isinstance(inner, ast.Import | ast.ImportFrom) for inner in node.body)
        )

    for node in tree.body:
        assert isinstance(
            node, ast.Import | ast.ImportFrom | ast.Assign | ast.Expr
        ) or is_typing_conditional(node), (
            f"seam.py grew a {type(node).__name__} at line {node.lineno} — "
            "logic belongs in degenbot._ffi or the test, not the door"
        )
        if isinstance(node, ast.Expr):
            assert isinstance(node.value, ast.Constant), (
                f"seam.py grew an expression at line {node.lineno}"
            )


# live-invariant gate (ADR-087 D2): watches the live `src/degenbot` and `tests`
# sources against the `ALLOWED_LEAF_FFI_IMPORTS` barrier list; not tripwire
# precedent.
def test_no_ffi_imports_outside_barriers_and_explicit_seams() -> None:
    """Fail on every private FFI import outside its approved seam.

    The scan reads statements, so the fresh-interpreter probe programs embedded
    in some test modules as source strings are out of scope: those children
    observe the private module at process level (boot posture, import cost),
    which is exactly the claim a door import would change the subject of.
    """
    violations: list[str] = []
    for relative, source in _scanned_sources():
        # ``__init__.py`` barriers are the shipped package's own facade modules;
        # no test package's ``__init__`` may import the seam, so the suite side
        # is scanned in full.
        if Path(relative).name == "__init__.py" and relative.startswith("src/degenbot/"):
            continue
        violations.extend(_find_ffi_import_violations(source, relative))
        violations.extend(_find_surface_walk_violations(source, relative))

    if violations:
        message = (
            f"\nFound {len(violations)} private `_ffi` import(s) outside approved seams:\n\n"
            + "\n".join(f"  - {violation}" for violation in violations)
            + "\n\nIn `src/`, import Rust symbols from a stable `degenbot.<domain>` "
            "package barrier instead; ENGINE_HANDLES and ALLOWED_LEAF_FFI_IMPORTS "
            "above are the only documented leaf exceptions. In `tests/`, import "
            f"them from `{SEAM_MODULE}` — `{SEAM_DOOR}` is the suite's only "
            "_ffi importer, and it is where a seam rename is absorbed. In "
            "`examples/`, import from `degenbot.*` only."
        )
        pytest.fail(message)
