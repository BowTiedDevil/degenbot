"""Companion→FFI alias-identity contract for the whole companion surface.

ADR-005 puts degenbot in three layers: a Rust engine (exposed through the
PyO3 wrapper ``degenbot._ffi``), thin companion packages that re-export the
FFI symbols under stable, seam-agnostic names, and the Python driver.

The load-bearing invariant of the middle layer: every companion re-export
must be a **direct alias** (``from degenbot._ffi import PyX as X``), not a
subclass or Python wrapper. The Rust engine constructs and consumes these
pyclasses/pyfunctions directly and driver code passes the instances back to
Rust, so a subclass/wrapper re-export would break type identity at the Rust
FFI boundary. For exceptions, the same is true for ``except``: matching —
Rust raises the exact ``#[pyclass]`` type, so a subclass re-export would
silently break the catch in policy/driver code.

This module replaces the per-feature reexport-identity test family with one
parametrized table. Each row asserts the companion attr ``is`` the FFI attr
named by ``(companion_module, companion_attr, ffi_module, ffi_attr)``.
Rows that formerly compared a name imported from a single module against
itself (true tautologies) were deleted, not migrated; the rows below were
each verified to compare symbols from two different modules.

Pinned by this table:
- ``degenbot.dispatch`` (dispatch/sim/submission seams)
- ``degenbot.updater`` (updater/cancel/db seams)
- ``degenbot.exceptions`` (verification/arbitrage exception seams)
- ``degenbot.types`` (dex-identity seam)
- ``degenbot.chainlink`` / ``degenbot.aave`` (price seam)
- ``degenbot.uniswap.math`` / ``degenbot.aerodrome.math`` (v2-math re-export seam)
"""

from __future__ import annotations

import importlib

import pytest

import degenbot.dispatch

# Module aliases keep each table row on a single line.
_DISPATCH = "degenbot.dispatch"
_SIM = "degenbot._ffi.simulation"
_SUBMIT = "degenbot._ffi.submission"
_UPDATER = "degenbot.updater"
_CANCEL = "degenbot._ffi.cancel"
_DB = "degenbot._ffi.db"
_EXCEPTIONS = "degenbot.exceptions"
_VERIF = "degenbot.exceptions.verification"
_ARBITRAGE = "degenbot.exceptions.arbitrage"
_TYPES = "degenbot.types"
_DEX_ID = "degenbot._ffi.dex_identity"
_CHAINLINK = "degenbot.chainlink"
_AAVE = "degenbot.aave"
_PRICE = "degenbot._ffi.price"
_UNI_MATH = "degenbot.uniswap.math"
_AERO_MATH = "degenbot.aerodrome.math"
_V2_MATH = "degenbot._ffi.v2_math"


# Each row: companion_module, companion_attr, ffi_module, ffi_attr
ALIAS_TABLE: list[tuple[str, str, str, str]] = [
    # degenbot.dispatch — simulation seam
    (_DISPATCH, "DispatchCandidate", _SIM, "DispatchCandidate"),
    (_DISPATCH, "DispatchOutcome", _SIM, "DispatchOutcome"),
    (_DISPATCH, "SimulateContext", _SIM, "SimulateContext"),
    (_DISPATCH, "PayloadOutcome", _SIM, "PayloadOutcome"),
    (_DISPATCH, "PayloadVerdict", _SIM, "PayloadVerdict"),
    (_DISPATCH, "dispatch_profitable", _SIM, "dispatch_profitable_py"),
    (_DISPATCH, "merge_payload_results", _SIM, "merge_payload_results_py"),
    # degenbot.dispatch — submission seam
    # dispatch_and_submit is deliberately NOT pinned here: it is the one
    # non-alias on the seam, a thin async wrapper decoding the leaf's record
    # dicts into the typed records of degenbot.dispatch.records (behavior is
    # pinned by tests/dispatch/test_submit_records.py).
    (_DISPATCH, "Dispatcher", _SUBMIT, "Dispatcher"),
    (_DISPATCH, "TxSigner", _SUBMIT, "TxSigner"),
    (_DISPATCH, "fetch_fee_history", _SUBMIT, "fetch_fee_history_py"),
    # degenbot.updater
    (_UPDATER, "CancelHandle", _CANCEL, "CancelHandle"),
    (_UPDATER, "LiquidityUpdateEvent", _DB, "LiquidityUpdateEvent"),
    (_UPDATER, "V2PoolRowInput", _DB, "V2PoolRowInput"),
    (_UPDATER, "V3PoolRowInput", _DB, "V3PoolRowInput"),
    (_UPDATER, "V4PoolRowInput", _DB, "V4PoolRowInput"),
    # degenbot.exceptions — the companion root must alias the seam submodules
    (_EXCEPTIONS, "VerificationRpcError", _VERIF, "VerificationRpcError"),
    (_EXCEPTIONS, "VerificationMismatchError", _VERIF, "VerificationMismatchError"),
    (_EXCEPTIONS, "HookedPoolRejectedError", _ARBITRAGE, "HookedPoolRejectedError"),
    (_EXCEPTIONS, "DynamicFeePoolRejectedError", _ARBITRAGE, "DynamicFeePoolRejectedError"),
    # degenbot.types — dex identity
    (_TYPES, "DexIdentity", _DEX_ID, "DexIdentity"),
    (_TYPES, "dex_identity", _DEX_ID, "dex_identity"),
    # price seam
    (_CHAINLINK, "ChainlinkPriceFeed", _PRICE, "ChainlinkPriceFeed"),
    (_AAVE, "AavePriceOracle", _PRICE, "AavePriceOracle"),
    # v2-math reexports
    (_UNI_MATH, "calc_exact_in_v2", _V2_MATH, "calc_exact_in_v2"),
    (_UNI_MATH, "calc_exact_out_v2", _V2_MATH, "calc_exact_out_v2"),
    (_AERO_MATH, "calc_exact_out_v2", _V2_MATH, "calc_exact_out_v2"),
]


@pytest.mark.parametrize(
    ("companion_module", "companion_attr", "ffi_module", "ffi_attr"),
    ALIAS_TABLE,
)
def test_companion_alias_is_ffi_symbol(
    companion_module: str,
    companion_attr: str,
    ffi_module: str,
    ffi_attr: str,
) -> None:
    """The companion re-export is the exact FFI symbol, not a subclass."""

    companion = getattr(importlib.import_module(companion_module), companion_attr)
    ffi = getattr(importlib.import_module(ffi_module), ffi_attr)

    assert companion is ffi, (
        f"{companion_module}.{companion_attr} is not the exact FFI symbol "
        f"{ffi_module}.{ffi_attr} — a subclass/wrapper re-export breaks type "
        "identity at the Rust FFI boundary"
    )


def test_dispatch_all_pins_public_surface() -> None:
    """Every stable dispatch name is exported from ``degenbot.dispatch``.

    ``SubmitCandidate`` joined the surface with the inline-sim seam
    (SIMPIPE2 T3): the runner builds submit records from payload batches,
    so it is a public name alongside the FFI leaf wrappers. NUUJFA added
    the payload seam (``merge_payload_results`` + the two pyclasses) when
    the payload arm started routing through the same Rust sim join.
    """

    d = degenbot.dispatch
    expected = {
        "DispatchCandidate",
        "DispatchOutcome",
        "Dispatcher",
        "PayloadOutcome",
        "PayloadVerdict",
        "SimulateContext",
        "SkippedRecord",
        "SubmitCandidate",
        "SubmitContext",
        "SubmitRecord",
        "SubmitSkipReason",
        "SubmittedRecord",
        "TxSigner",
        "dispatch_and_submit",
        "dispatch_profitable",
        "fetch_fee_history",
        "merge_payload_results",
        "typed_submit_record",
    }
    assert expected.issubset(set(dir(d)))
    assert expected == set(d.__all__)


def test_rust_raised_exception_is_caught_by_companion_alias() -> None:
    """An instance the Rust pyclass raises is catchable by the companion name.

    ``verification_retry.py`` catches ``VerificationRpcError`` from the
    companion package, while Rust code raises the
    ``degenbot._ffi.VerificationRpcError`` pyclass. The alias makes those
    the same type so ``except`` matches; the raise here goes through the
    FFI symbol and the catch through the companion symbol to pin the
    cross-module contract.
    """
    from degenbot._ffi import VerificationRpcError as FfiVerificationRpcError
    from degenbot.exceptions import VerificationRpcError

    msg = "transport blip"
    with pytest.raises(VerificationRpcError) as caught:
        raise FfiVerificationRpcError(msg)

    assert "transport blip" in str(caught.value)
