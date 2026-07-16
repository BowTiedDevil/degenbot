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

F2EVV6 reparented the V4-specific admission names under a unified
``PoolRegistrationError`` hierarchy, shared across V2/V3/V4:

.. code-block:: text

    ValueError
    └─ PoolRegistrationError                       (F2EVV6 base)
       ├─ HookedPoolRejectedError                    (V4 admission —
       │                                              amount-modifying hook)
       ├─ DynamicFeePoolRejectedError                (V4 admission — dynamic fee)
       ├─ PoolAlreadyRegisteredError                (V2/V3/V4 — duplicate
       │                                              address at registration)
       └─ SpecViolationError                        (V2/V3/V4 — out-of-spec
                                                      field)

The ``PoolAlreadyRegisteredError`` upgrade replaces the previous
"plain ``ValueError``" duplicate-registration behavior across the family
(V2/V3/V4). These tests pin the V4 admission variants' public surface and
the three admission outcomes documented in the original Plan 102 work:

- ``HookedPoolRejectedError`` — amount-modifying hook (``hook_flags & 0xCC``)
- ``DynamicFeePoolRejectedError`` — dynamic fee (``fee == 0x100000``)
- a ``PoolAlreadyRegisteredError`` — duplicate ``(pool_manager, pool_id)``
  registration (now a typed admission category, not a plain
  ``ValueError``).

The seam-triggered companions across V2/V3/V4 live in
``tests/rust/test_pybot_admission_exceptions.py``.
"""

from __future__ import annotations

import pytest

import degenbot.exceptions
from degenbot.exceptions import (
    DynamicFeePoolRejectedError,
    HookedPoolRejectedError,
    PoolRegistrationError,
)


def test_hooked_pool_rejected_error_is_exposed() -> None:
    """``HookedPoolRejectedError`` is exported and is a ``ValueError``."""
    assert hasattr(degenbot.exceptions, "HookedPoolRejectedError")
    exc_type = HookedPoolRejectedError
    assert issubclass(exc_type, ValueError)


def test_dynamic_fee_pool_rejected_error_is_exposed() -> None:
    """``DynamicFeePoolRejectedError`` is exported and is a ``ValueError``."""
    assert hasattr(degenbot.exceptions, "DynamicFeePoolRejectedError")
    exc_type = DynamicFeePoolRejectedError
    assert issubclass(exc_type, ValueError)


def test_admission_errors_are_distinct_value_errors() -> None:
    """The two admission categories are distinguishable by ``isinstance``.

    Both subclass ``ValueError`` (broad handlers keep working), but neither is
    a subclass of the other, so ``build_paths`` can route them to separate
    counters without re-introducing string matching. F2EVV6 reparented them
    under ``PoolRegistrationError``; they stay distinct from each other.
    """
    hooked = HookedPoolRejectedError
    dynamic = DynamicFeePoolRejectedError
    assert hooked is not dynamic
    assert not issubclass(hooked, dynamic)
    assert not issubclass(dynamic, hooked)


def test_v4_admission_errors_are_pool_registration_errors() -> None:
    """F2EVV6: the V4 admission variants now subclass ``PoolRegistrationError``.

    ``build_paths`` can scope its broad ``except PoolRegistrationError:`` (or
    narrow to the V4-specific subclasses). Reparenting is the unified
    hierarchy; the V4 origins stay distinguishable by ``isinstance``.
    """
    base = PoolRegistrationError
    assert issubclass(HookedPoolRejectedError, base)
    assert issubclass(DynamicFeePoolRejectedError, base)


def test_hooked_pool_rejected_error_carries_message() -> None:
    """A raised ``HookedPoolRejectedError`` carries a useful message."""
    exc = HookedPoolRejectedError("V4 pool has amount-modifying hooks")
    assert "amount-modifying hooks" in str(exc)


def test_dynamic_fee_pool_rejected_error_carries_message() -> None:
    """A raised ``DynamicFeePoolRejectedError`` carries a useful message."""
    exc = DynamicFeePoolRejectedError("V4 pool has dynamic fee")
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
    exc_type = getattr(degenbot.exceptions, exc_name)
    msg = "rejected"
    with pytest.raises(ValueError):  # noqa: PT011 — broad catch is the contract
        raise exc_type(msg)
