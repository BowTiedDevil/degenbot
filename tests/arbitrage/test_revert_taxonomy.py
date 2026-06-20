"""Tests for simulation revert taxonomy (Plan: exotic-path diagnosis).

The permutation runner historically bucketed every failed simulation into a
single ``N failed`` count, hiding whether reverts were ``CurrencyNotSettled``,
``ERC20: transfer amount exceeds balance``, ``Panic``, or genuine RPC errors.

``_classify_revert`` maps raw revert return-data to a short canonical label
so the ``[sim]`` summary can break failures down by root cause. The labels are
stable strings suitable for tallying and for the issue tracker; they are NOT
the verbose per-fail diagnostic string (that stays in ``[sim-fail]`` lines).
"""

from __future__ import annotations

import eth_abi.abi as eth_abi

from examples.eth_backrun_helpers import (
    _EXECUTOR_REVERT_SELECTORS,
    _V4_REVERT_SELECTORS,
    _classify_revert,
    _format_failure_breakdown,
)

# ── Fixtures ─────────────────────────────────────────────────────────────


def _error_string_sel() -> bytes:
    """Selector for ``Error(string)`` — ``0x08c379a0``."""
    return bytes.fromhex("08c379a0")


def _panic_sel() -> bytes:
    """Selector for ``Panic(uint256)`` — ``0x4e487b71``."""
    return bytes.fromhex("4e487b71")


def _encode_error_string(message: str) -> bytes:
    return _error_string_sel() + eth_abi.encode(["string"], [message])


def _encode_panic(code: int) -> bytes:
    return _panic_sel() + eth_abi.encode(["uint256"], [code])


# ── _classify_revert ────────────────────────────────────────────────────


def test_classify_v4_currency_not_settled() -> None:
    """CurrencyNotSettled() is the dominant V4 unlock residual-delta revert."""
    data = bytes.fromhex("5212cba1")
    assert _classify_revert(data) == "CurrencyNotSettled"


def test_classify_executor_custom_error_strips_params() -> None:
    """Custom-error selectors map to their name, dropping ABI params.

    InsufficientProfit(actual, expected) → bucket ``InsufficientProfit`` so all
    param variations tally together rather than fragmenting per amount.
    """
    data = bytes.fromhex("4e88422a") + eth_abi.encode(["uint256", "uint256"], [123, 456])
    assert _classify_revert(data) == "InsufficientProfit"


def test_classify_invalid_callback() -> None:
    # 32-byte address arg (contents don't matter — params are dropped on classify)
    data = bytes.fromhex("b028a63a") + b"\x00" * 31 + b"\xff"
    assert _classify_revert(data) == "InvalidCallback"


def test_classify_error_string_message() -> None:
    """``Error(string)`` reverts (e.g. ERC20 transfer-exceeds-balance) bucket by message."""
    data = _encode_error_string("ERC20: transfer amount exceeds balance")
    assert _classify_revert(data) == "ERC20: transfer amount exceeds balance"


def test_classify_panic_includes_code() -> None:
    """Panic reverts include the panic code so 0x11 / 0x32 tally separately."""
    assert _classify_revert(_encode_panic(0x11)) == "Panic(0x11)"
    assert _classify_revert(_encode_panic(0x32)) == "Panic(0x32)"


def test_classify_numeric_revert() -> None:
    """A bare 32-byte numeric revert (leading zeros) is its own bucket."""
    # 0x00...00 followed by a small number — Vyper numeric revert shape
    data = bytes(31) + b"\x01"
    assert _classify_revert(data) == "numeric-revert"


def test_classify_unknown_selector() -> None:
    """Unknown selectors fall back to ``unknown:0x........`` rather than dropping."""
    data = bytes.fromhex("deadbeef") + b"\x00" * 32
    assert _classify_revert(data) == "unknown:0xdeadbeef"


def test_classify_empty_and_short() -> None:
    """Empty revert data and malformed (<4 byte) data get distinct labels."""
    assert _classify_revert(b"") == "empty"
    assert _classify_revert(b"\x01\x02") == "short:0102"


def test_classify_invalid_callback_matches_driver_selector_set() -> None:
    """The helper's selector maps must stay in sync with the driver's.

    Guard against drift: every selector the driver decodes must be classifiable.
    """
    assert "b028a63a" in _EXECUTOR_REVERT_SELECTORS
    assert "5212cba1" in _V4_REVERT_SELECTORS
    # Every mapped selector classifies to a non-empty label without raising.
    for sel, name in _V4_REVERT_SELECTORS.items():
        assert _classify_revert(bytes.fromhex(sel)) == name.split("(")[0]
    for sel, name in _EXECUTOR_REVERT_SELECTORS.items():
        assert _classify_revert(bytes.fromhex(sel)) == name.split("(")[0]


# ── _format_failure_breakdown ────────────────────────────────────────────


def test_format_breakdown_sorts_by_count_desc_then_name() -> None:
    """Breakdown lists the most common root cause first for at-a-glance reading."""
    buckets = {
        "CurrencyNotSettled": 9,
        "no-profit": 5,
        "ERC20: transfer amount exceeds balance": 2,
        "rpc-failed": 9,  # tie with CurrencyNotSettled → name breaks tie
    }
    breakdown = _format_failure_breakdown(buckets)
    # Ties (9==9) broken by name → "CurrencyNotSettled" before "rpc-failed".
    # Remaining entries ordered by count desc → no-profit(5) before ERC20…(2).
    assert breakdown == (
        "CurrencyNotSettled=9 rpc-failed=9 no-profit=5 ERC20: transfer amount exceeds balance=2"
    )


def test_format_breakdown_empty() -> None:
    """An empty tally yields the empty string (caller skips the suffix)."""
    result = _format_failure_breakdown({})
    assert isinstance(result, str)
    assert len(result) == 0
