"""Regression guard: every exported pyclass reports ``__module__`` under ``degenbot._ffi``.

This locks in the ``module = "..."`` annotation on every ``#[pyclass]`` and
``create_exception!`` in the PyO3 binding crate
(``rust/crates/degenbot-python``). Without that annotation pyo3 bakes the
internal cdylib crate name (``degenbot_rs``) or ``builtins`` into the type
object's ``__module__``, which leaks the Rust crate name across the FFI
boundary into ``repr(type)`` / pickle / IDE introspection.

A new ``#[pyclass]`` added without ``module=`` will regress to
``degenbot_rs`` and trip this test.
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


def _iter_exported_classes() -> list[tuple[str, str, type]]:
    """Yield ``(source_path, class_name, class)`` for every public class on
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


def test_all_pyclass_module_under_degenbot_ffi() -> None:
    """Every exported pyclass must report ``__module__`` under ``degenbot._ffi``.

    A class that was registered on the module surface but whose
    ``#[pyclass]``/``create_exception!`` lacks ``module=`` will report
    ``degenbot_rs`` (the cdylib crate name) or ``builtins`` and fail here.
    """
    bad: list[str] = []
    checked = 0
    for source_path, name, cls in _iter_exported_classes():
        module = getattr(cls, "__module__", "")
        checked += 1
        if not module.startswith("degenbot._ffi"):
            bad.append(
                f"{source_path}.{name}: __module__={module!r} "
                f"(expected to start with 'degenbot._ffi')"
            )
    assert not bad, (
        f"{len(bad)} of {checked} exported pyclasses leak the internal "
        f"crate name in __module__ (missing module= annotation on "
        f"#[pyclass] / create_exception!):\n  " + "\n  ".join(bad)
    )


def test_db_classes_on_db_submodule() -> None:
    """The db-row/event/snapshot classes live on ``degenbot._ffi.db`` and
    must report ``__module__ == 'degenbot._ffi.db'`` (not root)."""
    db = ffi.db
    expected = {
        "LiquidityPoolRow",
        "ExchangeRow",
        "PoolManagerRow",
        "LiquidityUpdateEvent",
        "V2PoolRowInput",
        "V3PoolRowInput",
        "V4PoolRowInput",
        "DatabaseSnapshot",
        "DatabasePositionQuery",
        "DatabaseSchemaStale",
    }
    missing = [n for n in expected if not hasattr(db, n)]
    assert not missing, f"db submodule missing expected classes: {missing}"
    for name in expected:
        cls = getattr(db, name)
        assert cls.__module__ == "degenbot._ffi.db", (
            f"db.{name}.__module__={cls.__module__!r}, expected 'degenbot._ffi.db'"
        )
