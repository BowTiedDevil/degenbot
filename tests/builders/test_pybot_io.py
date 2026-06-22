"""Tracer-bullet tests for PyBotIo (ADR-005 slice 14a).

`PyBotIo` is the Rust `#[pyclass]` I/O façade that builders will receive in
place of the Python `SyncPoolIO` adapter. It holds a Python provider (the
`ProviderAdapter` the `Bot` was constructed with) + an optional DB handle, and
exposes the 7-method `PoolIO` surface (`get_block_number`, `get_block`,
`get_block_timestamp`, `get_code`, `get_balance`, `call`, `call_raw`) by
delegating to the held provider.

These tests pin the *seam* — that delegating through the Rust pyclass yields
the same observable result as calling the provider directly. They do NOT yet
route a real builder through `PyBotIo`; that's the 14a follow-on (one builder's
`build()` via `PyBotIo`), and 14b extends it to all families.
"""

from __future__ import annotations

from typing import Any

import pytest
from hexbytes import HexBytes

from degenbot.degenbot_rs import PyBotIo


class _FakeProvider:
    """A minimal ``ProviderAdapter``-shaped double for the tracer.

    Only the 7 ``PoolIO`` methods are exercised; the rest of the
    ``ProviderAdapter`` surface is irrelevant to ``PoolIO`` conformance.
    """

    def __init__(self, *, block_number: int = 18_000_000) -> None:
        self._block_number = block_number
        self.calls: list[tuple[str, str]] = []  # (to, data_hex) audit trail

    def get_block_number(self) -> int:
        return self._block_number

    def get_block(self, block_identifier: int | str) -> dict[str, Any] | None:
        return {"number": int(block_identifier), "timestamp": 1_700_000_000}

    def get_block_timestamp(self, block: int | None = None) -> int:
        return 1_700_000_000

    def get_code(self, address: str, block: int | None = None) -> HexBytes:
        return HexBytes(b"\x60\x80\x60\x40")  # plausible bytecode prefix

    def get_balance(self, address: str, block: int | None = None) -> int:
        return 10**18

    def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        self.calls.append((to, data.hex()))
        return HexBytes(b"\x00" * 32)

    def call_raw(self, tx: Any, block: int | None = None) -> HexBytes:
        return self.call(tx["to"], tx["data"], block)


class _FakeDb:
    """A ``DatabaseSessionManager``-shaped double (cannot be called; presence only)."""


def test_pybot_io_delegates_get_block_number():
    """get_block_number delegates to the held provider verbatim."""
    provider = _FakeProvider(block_number=12_345_678)
    io = PyBotIo(provider=provider)
    assert io.get_block_number() == 12_345_678


def test_pybot_io_delegates_call_records_on_provider():
    """call(to, data, block) delegates to provider.call and returns its HexBytes."""
    provider = _FakeProvider()
    io = PyBotIo(provider=provider)
    result = io.call(to="0x" + "ab" * 20, data=b"\x12\x34\x56\x78", block=None)
    assert result == HexBytes(b"\x00" * 32)
    assert provider.calls == [("0x" + "ab" * 20, "12345678")]


def test_pybot_io_delegates_get_code():
    """get_code delegates and returns HexBytes."""
    provider = _FakeProvider()
    io = PyBotIo(provider=provider)
    code = io.get_code("0x" + "cd" * 20)
    assert code == HexBytes(b"\x60\x80\x60\x40")


def test_pybot_io_delegates_get_balance():
    """get_balance delegates and returns int (not wrapped)."""
    provider = _FakeProvider()
    io = PyBotIo(provider=provider)
    assert io.get_balance("0x" + "ee" * 20) == 10**18


def test_pybot_io_holds_optional_db_handle():
    """PyBotIo stores the DB handle and exposes it back (held, not called yet)."""
    db = _FakeDb()
    io = PyBotIo(provider=_FakeProvider(), db=db)
    # The held handle round-trips through the pyclass.
    assert io.db is db


@pytest.mark.parametrize(
    "method",
    [
        "get_block_number",
        "get_block",
        "get_block_timestamp",
        "get_code",
        "get_balance",
        "call",
        "call_raw",
    ],
)
def test_pybot_io_satisfies_pool_io_protocol(method: str):
    """PyBotIo exposes the full 7-method PoolIO surface (runtime protocol check).

    This is the acceptance criterion for 14a: every method a builder may call
    on its ``io: PoolIO`` parameter is reachable on ``PyBotIo``.
    """
    io = PyBotIo(provider=_FakeProvider())
    assert hasattr(io, method), f"PyBotIo missing PoolIO method {method!r}"
