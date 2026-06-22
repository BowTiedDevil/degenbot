"""Typed verification exceptions exposed by the Rust engine (TODO-53b7453b).

Background — `build_paths` previously classified verification failures by
string-matching ``"tick data mismatch"`` inside a ``RuntimeError`` and
swallowed everything else. That conflated two distinct categories:

- a genuine snapshot/on-chain **mismatch** (fatal — the bot must not operate
  with stale tick data), and
- an **RPC / transport error** during verification (the bot could not reach
  the node to verify — also not safe to silently skip, but a different
  failure mode that may warrant retry/backoff rather than immediate abort).

The Rust seam now raises distinct Python exception *types* (both subclassing
``RuntimeError`` so broad ``except RuntimeError`` handlers still catch them)
so Python can classify by type, not by fragile string matching. These tests
pin the seam's public surface.
"""

from __future__ import annotations

from degenbot import degenbot_rs


def test_verification_mismatch_error_is_exposed() -> None:
    """``VerificationMismatchError`` is exported and is a ``RuntimeError``."""
    assert hasattr(degenbot_rs, "VerificationMismatchError")
    exc_type = degenbot_rs.VerificationMismatchError
    assert issubclass(exc_type, RuntimeError)


def test_verification_rpc_error_is_exposed() -> None:
    """``VerificationRpcError`` is exported and is a ``RuntimeError``."""
    assert hasattr(degenbot_rs, "VerificationRpcError")
    exc_type = degenbot_rs.VerificationRpcError
    assert issubclass(exc_type, RuntimeError)


def test_verification_errors_are_distinct_types() -> None:
    """The two failure categories are distinguishable by ``isinstance``.

    This is the contract ``build_paths`` relies on to stop string-matching:
    a mismatch (fatal) and an RPC failure (transport) must not be the same
    Python type.
    """
    mismatch = degenbot_rs.VerificationMismatchError
    rpc = degenbot_rs.VerificationRpcError
    assert mismatch is not rpc
    assert not issubclass(mismatch, rpc)
    assert not issubclass(rpc, mismatch)


def test_verification_mismatch_error_carries_message() -> None:
    """The exception preserves its message (``str(exc)`` round-trips)."""
    msg = "V3 pool 0x.. at snapshot block 1: tick data mismatch: ..."
    exc = degenbot_rs.VerificationMismatchError(msg)
    assert str(exc) == msg


def test_verification_rpc_error_carries_message() -> None:
    """The RPC-error exception preserves its message."""
    msg = "verify: 0x..: failed to create provider: connection refused"
    exc = degenbot_rs.VerificationRpcError(msg)
    assert str(exc) == msg


def test_both_exceptions_caught_by_runtime_error_handler() -> None:
    """Broad ``except RuntimeError`` still catches both (backward compat).

    Existing handlers that catch ``RuntimeError`` broadly keep working; the
    new types only *add* the ability to discriminate by type.
    """
    for exc_type in (
        degenbot_rs.VerificationMismatchError,
        degenbot_rs.VerificationRpcError,
    ):
        try:
            raise exc_type("boom")  # noqa: EM101
        except RuntimeError as exc:
            assert isinstance(exc, exc_type)


def test_rpc_transport_failure_message_is_not_a_mismatch() -> None:
    """VP42BP: a per-call RPC transport failure surfaces as
    ``VerificationRpcError`` — the type a retry/backoff policy would branch on
    — NOT as ``VerificationMismatchError`` (the fatal mismatch type).

    Pins the seam contract the Rust-side `LiquidityVerifyError::Rpc` →
    `VerifyError::Rpc` → `VerificationRpcError` mapping now satisfies. The
    Rust verifier's RPC-failure branches (``"... RPC call failed: ..."``)
    previously folded into ``VerificationMismatch``, surfaced as
    ``VerificationMismatchError``; they now surface as the retryable type.
    """
    transport_msg = (
        "V3 pool 0x.. at snapshot block 1: "
        "tickBitmap(0) RPC call failed: connection reset"
    )
    rpc_exc = degenbot_rs.VerificationRpcError(transport_msg)
    mismatch_exc = degenbot_rs.VerificationMismatchError(
        "V3 pool 0x.. at snapshot block 1: tick data mismatch"
    )
    # A transport failure raises the Rpc type, not the Mismatch type — the
    # two are distinguishable by isinstance (the contract build_paths relies
    # on, per VP42BP).
    assert isinstance(rpc_exc, degenbot_rs.VerificationRpcError)
    assert not isinstance(rpc_exc, degenbot_rs.VerificationMismatchError)
    assert isinstance(mismatch_exc, degenbot_rs.VerificationMismatchError)
    assert not isinstance(mismatch_exc, degenbot_rs.VerificationRpcError)
    assert str(rpc_exc) == transport_msg
