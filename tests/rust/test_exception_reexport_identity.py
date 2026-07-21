"""Companion-layer re-exports of Rust-raised exceptions are identity aliases.

The three-layer architecture (ADR-005) requires that driver and policy code
import exception types from the companion package ``degenbot.exceptions`` —
not from the PyO3 wrapper ``degenbot_rs``. The companion re-exports must be
**direct aliases** (``from degenbot._ffi import X as Y``), not Python
subclasses: these exceptions are *raised by Rust* ``#[pyclass]`` code, so
Python-side ``except SomeError:`` / ``isinstance(exc, SomeError)`` matching
must hit the exact type the Rust side raises. A subclass re-export would
silently break that catch — the Rust-raised instance would not be an
instance of the Python subclass.

These tests pin the identity contract.
"""

from __future__ import annotations

from degenbot.exceptions import (
    DynamicFeePoolRejectedError,
    HookedPoolRejectedError,
    VerificationMismatchError,
    VerificationRpcError,
)
from degenbot.exceptions import DynamicFeePoolRejectedError as RsDynamicFeePoolRejectedError
from degenbot.exceptions import HookedPoolRejectedError as RsHookedPoolRejectedError
from degenbot.exceptions import VerificationMismatchError as RsVerificationMismatchError
from degenbot.exceptions import VerificationRpcError as RsVerificationRpcError
from degenbot.exceptions.arbitrage import (
    DynamicFeePoolRejectedError as ArbDynamicFeePoolRejectedError,
)
from degenbot.exceptions.arbitrage import (
    HookedPoolRejectedError as ArbHookedPoolRejectedError,
)
from degenbot.exceptions.verification import (
    VerificationMismatchError as VerifVerificationMismatchError,
)
from degenbot.exceptions.verification import (
    VerificationRpcError as VerifVerificationRpcError,
)


def test_verification_rpc_error_is_identity_alias() -> None:
    """``degenbot.exceptions.VerificationRpcError`` is the Rust type itself."""
    assert VerificationRpcError is RsVerificationRpcError
    assert VerifVerificationRpcError is RsVerificationRpcError


def test_verification_mismatch_error_is_identity_alias() -> None:
    """``degenbot.exceptions.VerificationMismatchError`` is the Rust type."""
    assert VerificationMismatchError is RsVerificationMismatchError
    assert VerifVerificationMismatchError is RsVerificationMismatchError


def test_hooked_pool_rejected_error_is_identity_alias() -> None:
    """``degenbot.exceptions.HookedPoolRejectedError`` is the Rust type."""
    assert HookedPoolRejectedError is RsHookedPoolRejectedError
    assert ArbHookedPoolRejectedError is RsHookedPoolRejectedError


def test_dynamic_fee_pool_rejected_error_is_identity_alias() -> None:
    """``degenbot.exceptions.DynamicFeePoolRejectedError`` is the Rust type."""
    assert DynamicFeePoolRejectedError is RsDynamicFeePoolRejectedError
    assert ArbDynamicFeePoolRejectedError is RsDynamicFeePoolRejectedError


def test_verification_rpc_error_caught_from_rust_raised_instance() -> None:
    """An instance the Rust pyclass would raise is catchable by the alias.

    This is the load-bearing contract: ``verification_retry.py`` catches
    ``VerificationRpcError`` from the companion, and Rust code raises the
    ``degenbot._ffi.VerificationRpcError`` pyclass. The alias makes those the
    same type so ``except`` matches.
    """
    try:
        raise RsVerificationRpcError("transport blip")
    except VerificationRpcError as exc:
        assert isinstance(exc, VerificationRpcError)
        assert "transport blip" in str(exc)


def test_verification_mismatch_error_caught_from_rust_raised_instance() -> None:
    """An instance the Rust pyclass would raise is catchable by the alias."""
    try:
        raise RsVerificationMismatchError("tick data mismatch")
    except VerificationMismatchError as exc:
        assert isinstance(exc, VerificationMismatchError)
