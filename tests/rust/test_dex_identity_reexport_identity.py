"""Companion-layer re-export of the Rust DexIdentity FFI pyclass.

The three-layer architecture (ADR-005) requires that driver / test code
import the DEX-identity pyclass from the companion package ``degenbot.types``
— not from the PyO3 wrapper ``degenbot._ffi``. The ``Py*`` prefix is the FFI
seam naming itself; it should never appear in driver code.

This re-export must be a **direct alias** (``from degenbot._ffi import
PyDexIdentity as DexIdentity``), not a Python subclass — the Rust engine
constructs and consumes this pyclass directly; subclassing would break type
identity at the FFI boundary.
"""

from __future__ import annotations

from degenbot._ffi import PyDexIdentity


def test_dex_identity_alias_identity() -> None:
    """The stable companion name is the exact FFI pyclass."""
    from degenbot.types import DexIdentity

    assert DexIdentity is PyDexIdentity


def test_dex_identity_factory_alias_identity() -> None:
    """The stable companion ``dex_identity`` factory is the exact FFI function."""
    from degenbot._ffi import dex_identity as ffi_dex_identity
    from degenbot.types import dex_identity

    assert dex_identity is ffi_dex_identity
