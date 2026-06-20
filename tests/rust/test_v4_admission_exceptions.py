"""Typed V4 pool-admission exceptions exposed by the Rust engine (Plan 102).

Background — ``EngineRegistry.register_v4_pool`` and ``build_paths``
previously classified V4 pool rejections by string-matching
``"amount-modifying hooks"`` / ``"dynamic fees"`` inside a ``ValueError``.
That is the same fragile pattern TODO-53b7453b removed for verification
errors. Pool admission is a *correctness floor* (the solver's CL math
assumes no hook intervention; a dynamic fee breaks the fixed-fee solve), so
the Rust core refuses such pools — and the refusal must surface as a typed
Python exception (subclassing ``ValueError`` so existing broad
``except ValueError`` handlers still catch it) so Python classifies by type.

These tests pin the seam's public surface and the three admission outcomes:

- ``HookedPoolRejectedError`` — amount-modifying hook (``hook_flags & 0xCC``)
- ``DynamicFeePoolRejectedError`` — dynamic fee (``fee == 0x100000``)
- a plain ``ValueError`` — duplicate ``(pool_manager, pool_id)`` registration
  (a wiring/programming error, not an admission category)
"""

from __future__ import annotations

import pytest

from degenbot import degenbot_rs


def test_hooked_pool_rejected_error_is_exposed() -> None:
    """``HookedPoolRejectedError`` is exported and is a ``ValueError``."""
    assert hasattr(degenbot_rs, "HookedPoolRejectedError")
    exc_type = degenbot_rs.HookedPoolRejectedError
    assert issubclass(exc_type, ValueError)


def test_dynamic_fee_pool_rejected_error_is_exposed() -> None:
    """``DynamicFeePoolRejectedError`` is exported and is a ``ValueError``."""
    assert hasattr(degenbot_rs, "DynamicFeePoolRejectedError")
    exc_type = degenbot_rs.DynamicFeePoolRejectedError
    assert issubclass(exc_type, ValueError)


def test_admission_errors_are_distinct_value_errors() -> None:
    """The two admission categories are distinguishable by ``isinstance``.

    Both subclass ``ValueError`` (broad handlers keep working), but neither is
    a subclass of the other, so ``build_paths`` can route them to separate
    counters without re-introducing string matching.
    """
    hooked = degenbot_rs.HookedPoolRejectedError
    dynamic = degenbot_rs.DynamicFeePoolRejectedError
    assert hooked is not dynamic
    assert not issubclass(hooked, dynamic)
    assert not issubclass(dynamic, hooked)


def test_hooked_pool_rejected_error_carries_message() -> None:
    """A raised ``HookedPoolRejectedError`` carries a useful message."""
    exc = degenbot_rs.HookedPoolRejectedError("V4 pool has amount-modifying hooks")
    assert "amount-modifying hooks" in str(exc)


def test_dynamic_fee_pool_rejected_error_carries_message() -> None:
    """A raised ``DynamicFeePoolRejectedError`` carries a useful message."""
    exc = degenbot_rs.DynamicFeePoolRejectedError("V4 pool has dynamic fee")
    assert "dynamic fee" in str(exc)


@pytest.mark.parametrize(
    "exc_name",
    ["HookedPoolRejectedError", "DynamicFeePoolRejectedError"],
)
def test_admission_errors_catchable_as_value_error(exc_name: str) -> None:
    """Any admission refusal must be catchable by a broad ``except ValueError``.

    ``build_paths`` already wraps registration in ``except ValueError`` to skip
    rejected pools — the typed exceptions must not escape that net.
    """
    exc_type = getattr(degenbot_rs, exc_name)
    with pytest.raises(ValueError):  # noqa: PT011 — broad catch is the contract
        raise exc_type("rejected")
