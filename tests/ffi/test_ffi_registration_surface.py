"""``degenbot._ffi`` registration-surface gate (ADR-032 naming; ADR-053 stubs).

The compiled extension registers its Python-facing surface on
``degenbot._ffi`` and its ``add_submodule`` children — the runtime
registration surface, whose GENERATED stubs under
``src/degenbot/_ffi/*.pyi`` are the surface of record for type checking
(ADR-013; generation + cutover per ADR-066 D2). ``mypy.stubtest`` owns the symbol-level stub-drift checks
(``just lint-stubtest``, allowlist ``tests/ffi/stubtest_allowlist.txt``);
the checks stubtest cannot perform live here, all pinned against the same
runtime registration surface:

- **Pyclass naming (ADR-032)** — every extension type is registered with its
  Python module, so the walk below sees the whole pyclass population; any
  grandfathered-prefix breakage fails.
- **R0 (submodule stub coverage)** — every runtime ``degenbot._ffi.*``
  submodule must have a stub file. stubtest's module walk cannot be relied
  on across PyO3's ``add_submodule`` registration, and the check is a
  dozen lines.

---

ADR-032 decision: new pyclass types must use clean Python-facing names; a
Py-prefixed Python-visible name marks a Rust-internal seam type and may
exist only on the grandfather list. This test walks the runtime
degenbot._ffi module tree (the registration surface — the same source of
truth the stubs are pinned against) and asserts that no registered class name
carries an origin prefix — the live naming rule, over whatever the tree
registers today. A NEW Py-prefixed registration fails outright: the prefix is
never extended.

PyO3 pyclasses report __module__ as the Python module they were registered
on (degenbot._ffi or a degenbot._ffi.* submodule), which is what makes the
walk precise: Python-side wrapper classes in the degenbot.* consumer
package never match.
"""

from __future__ import annotations

import importlib
import pathlib
import sys
from typing import TYPE_CHECKING

from tests.helpers.seam import REGISTRATION_SURFACE as ffi

if TYPE_CHECKING:
    from collections.abc import Mapping

# ADR-032 grandfather list — EMPTY (2026-08-17): all 27 census
# names were renamed to clean Python-facing names (the collision set took
# Rust-prefixed or role-specific clean names — see the ADR-032 post-adoption
# note). Any new Py-prefixed registration now fails test 1 outright.
GRANDFATHERED: frozenset[str] = frozenset()


def _registered_classes() -> Mapping[str, type]:
    """Class name -> class for every extension type registered on _ffi modules."""
    found: dict[str, type] = {}

    def scan(mod: object) -> None:
        for name in dir(mod):
            if name.startswith("_"):
                continue
            obj = getattr(mod, name)
            if isinstance(obj, type) and getattr(obj, "__module__", "").startswith("degenbot._ffi"):
                found[name] = obj

    scan(ffi)
    submods = [
        name
        for name in dir(ffi)
        if not name.startswith("_") and isinstance(getattr(ffi, name), type(ffi.abi))
    ]
    for sub in submods:
        scan(importlib.import_module(f"degenbot._ffi.{sub}"))
    return found


# live-invariant gate (ADR-087 D2): watches the live registered pyclass walk, which must
# carry no `Py` origin prefix; not tripwire precedent.
def test_no_new_prefixed_class_names() -> None:
    """ADR-032 D1/D2: every registered Py-prefixed class is grandfathered."""
    classes = _registered_classes()
    prefixed = {n for n in classes if n.startswith("Py") and len(n) > 2}
    new = sorted(prefixed - GRANDFATHERED)
    assert not new, (
        "new Py-prefixed pyclass names violate ADR-032 (use a clean name, or extend "
        f"the grandfather list with a justified ADR amendment): {new}"
    )


# ADR-032 fork: the five Rust-prefixed collision names were retired into
# pure clean names (module path is the disambiguator). No origin prefix -
# neither Py nor Rust - is permitted on a Python-visible pyclass.
GRANDFATHERED_RUST: frozenset[str] = frozenset()


# live-invariant gate (ADR-087 D2): watches the live registered pyclass walk, which must
# carry no `Rust` origin prefix; not tripwire precedent.
def test_no_rust_prefixed_class_names() -> None:
    """ADR-032 fork: no Rust-prefixed Python-visible pyclass may exist."""
    classes = _registered_classes()
    rust_prefixed = {n for n in classes if n.startswith("Rust") and len(n) > 4}
    new = sorted(rust_prefixed - GRANDFATHERED_RUST)
    assert not new, (
        "Rust-prefixed pyclass names violate ADR-032 (the module path is the"
        f" disambiguator; use the clean name): {new}"
    )


# ---------------------------------------------------------------------------
# R0 (ADR-053) — the checks stubtest cannot perform, kept alongside the
# registration walk they share a source of truth with. See module docstring.
# ---------------------------------------------------------------------------
_STUB_DIR = pathlib.Path(__file__).resolve().parents[2] / "src" / "degenbot" / "_ffi"


def test_every_runtime_submodule_has_a_stub() -> None:
    """R0: every runtime `degenbot._ffi.*` submodule has a stub file."""
    # The door's module-level import registers every runtime submodule, so the
    # ``sys.modules`` walk below sees the full set.
    stub_stems = {p.stem for p in _STUB_DIR.glob("*.pyi") if p.name != "__init__.pyi"}
    runtime_subs = {
        name.removeprefix("degenbot._ffi.")
        for name in sys.modules
        if name.startswith("degenbot._ffi.")
    }
    missing = sorted(runtime_subs - stub_stems)
    assert not missing, f"runtime submodules missing a stub: {missing}"
