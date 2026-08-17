"""Pyclass Python-facing naming gate (ADR-032; ergo LEY2OD).

ADR-032 decision: new pyclass types must use clean Python-facing names; a
Py-prefixed Python-visible name marks a Rust-internal seam type and may
exist only on the grandfather list. This test walks the runtime
degenbot._ffi module tree (the registration surface — the same source of
truth the drift gate pins against the stubs) and asserts, in both
directions, that the set of registered Py-prefixed class names equals the
grandfather list:

- a NEW Py-prefixed registration fails the test (the prefix is never
  extended);
- a DEAD list entry (a name no longer registered — e.g. after the VD5MD5
  renames retire it without updating the list) also fails the test, keeping
  the list honest as the retirement proceeds.

PyO3 pyclasses report __module__ as the Python module they were registered
on (degenbot._ffi or a degenbot._ffi.* submodule), which is what makes the
walk precise: Python-side wrapper classes in the degenbot.* consumer
package never match.
"""

from __future__ import annotations

import importlib
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Mapping

# ADR-032 grandfather list — EMPTY as of VD5MD5 (2026-08-17): all 27 census
# names were renamed to clean Python-facing names (the collision set took
# Rust-prefixed or role-specific clean names — see the ADR-032 post-adoption
# note). Any new Py-prefixed registration now fails test 1 outright.
GRANDFATHERED: frozenset[str] = frozenset()


def _registered_classes() -> Mapping[str, type]:
    """Class name -> class for every extension type registered on _ffi modules."""
    import degenbot._ffi as ffi

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


def test_no_new_prefixed_class_names() -> None:
    """ADR-032 D1/D2: every registered Py-prefixed class is grandfathered."""
    classes = _registered_classes()
    prefixed = {n for n in classes if n.startswith("Py") and len(n) > 2}
    new = sorted(prefixed - GRANDFATHERED)
    assert not new, (
        "new Py-prefixed pyclass names violate ADR-032 (use a clean name, or extend "
        f"the grandfather list with a justified ADR amendment): {new}"
    )


def test_grandfather_list_has_no_dead_names() -> None:
    """ADR-032 D4: the list tracks runtime truth (no un-retired entries)."""
    classes = _registered_classes()
    dead = sorted(GRANDFATHERED - set(classes))
    assert not dead, (
        "grandfathered names no longer registered — remove from both the ADR-032 "
        f"list and GRANDFATHERED: {dead}"
    )


def test_prefixed_census_is_complete() -> None:
    """Sanity: the walk actually sees the grandfather population (guards the probe)."""
    classes = _registered_classes()
    visible = set(GRANDFATHERED) & set(classes)
    assert len(visible) == len(GRANDFATHERED), (
        f"walk missed {len(GRANDFATHERED) - len(visible)} grandfathered names — "
        "check the __module__ predicate in _registered_classes"
    )
