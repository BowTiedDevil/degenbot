"""Regression guard: exported pyclasses must not leak the Rust crate name.

``repr(type)``, IDE introspection and pickle-by-reference all surface
``type.__module__``. When a ``#[pyclass]`` or ``create_exception!`` in the
binding crate (``rust/crates/shells/degenbot-python``) does not name the
Python module it registers on, PyO3 bakes the cdylib crate name
(``degenbot_rs``) — or ``builtins`` — into the type object's ``__module__``,
leaking the internal crate name across the FFI boundary.

The binding crate holds ``__module__`` at the registration surface with an
explicit ``module = "..."`` on every registration. This guard asserts the
behaviour that produces — the observed ``__module__`` value — rather than
the annotation itself: a PyO3 release that derives the registration module
by default keeps passing here, while any regression to ``degenbot_rs`` /
``builtins`` fails with the leaking type named.
"""

from __future__ import annotations

import inspect

import degenbot._ffi as ffi

#: The submodules created by ``add_*_module`` in the PyO3 binding crate.
_SUBMODULES = (
    "abi",
    "balancer_math",
    "concentrated_liquidity_math",
    "curve_math",
    "solidly_math",
    "db",
)

#: The internal cdylib crate name. Its appearance in any Python-visible
#: ``__module__`` is the leak this guard exists for.
_CRATE_NAME = "degenbot_rs"


def _iter_exported_classes() -> list[tuple[str, str, type]]:
    """Yield ``(module_path, class_name, class)`` for every public class on
    the root module + each registered submodule."""
    seen: set[int] = set()
    out: list[tuple[str, str, type]] = []
    sources: list[tuple[str, object]] = [("degenbot._ffi", ffi)]
    for sub in _SUBMODULES:
        full = f"degenbot._ffi.{sub}"
        mod = getattr(ffi, sub, None)
        if inspect.ismodule(mod):
            sources.append((full, mod))
    for source_path, mod in sources:
        for name in sorted(dir(mod)):
            if name.startswith("_"):
                continue
            obj = getattr(mod, name)
            if not inspect.isclass(obj):
                continue
            if id(obj) in seen:
                continue
            seen.add(id(obj))
            out.append((source_path, name, obj))
    return out


def test_no_crate_name_in_exported_type_modules() -> None:
    """No exported class leaks ``degenbot_rs`` (or ``builtins``) through
    ``__module__``.

    The crate name in a type's ``__module__`` is visible to ``repr(type)``,
    IDE introspection and pickle-by-reference regardless of which PyO3
    mechanism produced it, so the assertion is on the observed value.
    """
    bad: list[str] = []
    checked = 0
    for source_path, name, cls in _iter_exported_classes():
        module = getattr(cls, "__module__", "")
        checked += 1
        if _CRATE_NAME in module or module == "builtins":
            bad.append(f"{source_path}.{name}: __module__={module!r}")
    assert not bad, (
        f"{len(bad)} of {checked} exported pyclasses leak the internal "
        f"crate name through __module__ (repr / pickle / IDE introspection "
        f"see it; the registration must name its Python module):\n  "
        + "\n  ".join(bad)
    )


def test_exported_classes_report_their_exporting_module() -> None:
    """Every exported class reports the Python module that exports it.

    The expectation is derived from the runtime registration surface — the
    module the class is actually exported from — not from a spelled-out
    PyO3 attribute, so a future PyO3 that defaults the module to the
    registration surface stays green. Only an observed mismatch (a class
    reporting another crate's namespace) fails.
    """
    wrong: list[str] = []
    checked = 0
    for source_path, name, cls in _iter_exported_classes():
        module = getattr(cls, "__module__", "")
        checked += 1
        if module != source_path:
            wrong.append(f"{source_path}.{name}: __module__={module!r}")
    assert not wrong, (
        f"{len(wrong)} of {checked} exported pyclasses report a module "
        f"other than the one they are exported from:\n  "
        + "\n  ".join(wrong)
    )


def test_db_row_types_report_the_db_submodule() -> None:
    """The db-row/event/input handles live on ``degenbot._ffi.db`` and must
    report that module, not root: the module path, not a name prefix,
    disambiguates them from the same-named Python shells in
    ``uniswap.{v3,v4}_snapshot`` and ``aave.analysis.orchestrator``
    (ADR-013 amendment; ADR-032 fork)."""
    db = ffi.db
    expected = (
        "LiquidityPoolRow",
        "ExchangeRow",
        "PoolManagerRow",
        "LiquidityUpdateEvent",
        "V2PoolRowInput",
        "V3PoolRowInput",
        "V4PoolRowInput",
        "DatabaseSnapshot",
        "DatabasePositionQuery",
    )
    missing = [n for n in expected if not hasattr(db, n)]
    assert not missing, f"db submodule missing expected classes: {missing}"
    for name in expected:
        cls = getattr(db, name)
        assert cls.__module__ == "degenbot._ffi.db", (
            f"db.{name}.__module__={cls.__module__!r}, expected 'degenbot._ffi.db'"
        )
